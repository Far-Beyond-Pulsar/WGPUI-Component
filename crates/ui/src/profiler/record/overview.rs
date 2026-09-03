//! The overview strip (§2 of `.agents/PROFILER_UI_SPEC.md`): a ruler-labeled
//! bar chart over the *whole* capture window, with a drag-to-select gesture
//! that sets the active range. Deliberately never zooms itself — it's the
//! navigator, not the thing being navigated; the detail flame chart is what
//! zooms, via the selection this produces. GPU-instanced via
//! [`crate::profiler::BarInstance`]/[`FlameLaneGpu`], the same approach the
//! detail flame chart's lanes use — not one `div()` per bucket (up to
//! [`super::data::OVERVIEW_BUCKET_COUNT`]-ish rectangles, redrawn on every
//! pan/selection change).
//!
//! # v1 note, honestly
//!
//! Dominant-category solid-color bars, not yet the reference's true
//! **stacked** area (`OverviewBucket::category_ns` already carries the full
//! per-category breakdown needed for that — see its field doc — only the
//! rendering hasn't been upgraded to draw multiple stacked segments per bar
//! yet), and no Frames/Network/Timings/Interactions rows below it (§2 rows
//! 3-7; `.agents/PROFILER_UI_SPEC.md`'s own build-order note already flags
//! the ruler+CPU graph as the highest-value, lowest-cost piece to land
//! first). Long-task hatching (§5, `OverviewBucket::has_long_task`) is real.

use std::rc::Rc;

use gpui::{
    canvas, div, prelude::FluentBuilder as _, px, AnyElement, AppContext as _, Bounds, ClickEvent,
    Context, DragMoveEvent, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    Render, StatefulInteractiveElement as _, Styled, Window,
};

use crate::{v_flex, ActiveTheme};

use crate::profiler::{category_color, BarInstance, FlameBarPipeline, FlameLaneGpu};

use super::{data::RecordOverview, ProfilerPanel};

/// Drag marker for the overview's range-select gesture. GPUI's `on_drag`
/// mechanism only cares about matching `TypeId`s, and the Record tab has no
/// other draggable view mounted at the same time, so a single marker type
/// is safe.
#[derive(Clone)]
struct RangeSelectDrag;

impl Render for RangeSelectDrag {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

#[derive(Default)]
pub(crate) struct OverviewState {
    /// Measured screen bounds of the strip, captured via a `canvas()`
    /// overlay each render — used to turn drag x-coordinates into
    /// nanosecond instants.
    bounds: Bounds<Pixels>,
    /// `start_ns` of an in-progress drag-select; `None` when no selection
    /// drag is active. The end is always "wherever the pointer currently
    /// is", so only the anchor needs to be state.
    drag_anchor_ns: Option<u64>,
    gpu: Option<FlameLaneGpu>,
    pipeline: Option<Rc<FlameBarPipeline>>,
    /// Latches once `Window::create_wgpu_surface` ever returns `None`
    /// (headless test platform, or a backend/platform that doesn't support
    /// it), so the "GPU unavailable" notice doesn't flicker if creation
    /// transiently fails on exactly one render.
    gpu_unavailable: bool,
}

pub(crate) fn render(
    state: &mut OverviewState,
    overview_data: Option<&RecordOverview>,
    selection: Option<(u64, u64)>,
    _frame_durations_ms: &[f32],
    window: &mut Window,
    cx: &mut Context<ProfilerPanel>,
) -> AnyElement {
    let Some(overview) = overview_data else {
        return div().into_any_element();
    };
    const OVERVIEW_HEIGHT: f32 = 64.0;
    const DEFAULT_WIDTH: f32 = 900.0;

    let measured_width = f32::from(state.bounds.size.width);
    let chart_width = if measured_width > 1.0 {
        measured_width
    } else {
        DEFAULT_WIDTH
    };

    let domain_start_ns = overview.domain_start_ns;
    let domain_span_ns = (overview.domain_end_ns - domain_start_ns).max(1) as f64;
    let bucket_width_px = (chart_width as f64 / overview.buckets.len().max(1) as f64) as f32;
    let max_ns = overview.max_bucket_ns.max(1);

    let gpu_available = !state.gpu_unavailable;
    let scale = window.scale_factor();
    let mut instances: Vec<BarInstance> = Vec::new();
    if gpu_available {
        for (index, bucket) in overview.buckets.iter().enumerate() {
            if bucket.total_ns == 0 {
                continue;
            }
            let height = ((bucket.total_ns as f32 / max_ns as f32) * OVERVIEW_HEIGHT).max(1.0);
            let color = bucket
                .dominant_category()
                .map(|c| category_color(c, cx))
                .unwrap_or(cx.theme().muted_foreground);
            let rgba = color.to_rgb();
            let x = index as f32 * bucket_width_px;
            let width = (bucket_width_px - 0.5).max(1.0);
            let top = OVERVIEW_HEIGHT - height;
            instances.push(BarInstance {
                rect_min: [x * scale, top * scale],
                rect_max: [(x + width) * scale, OVERVIEW_HEIGHT * scale],
                color: [rgba.r, rgba.g, rgba.b, rgba.a],
                corner_radius: 0.0,
                highlight: 0.0,
                _pad: [0.0, 0.0],
            });
            // Long-task warning hatch: a thin red bar along the very top
            // edge, an orthogonal channel from category color (§5's own
            // "never conflate what-kind with is-a-problem" rule).
            if bucket.has_long_task {
                instances.push(BarInstance {
                    rect_min: [x * scale, 0.0],
                    rect_max: [(x + width) * scale, 3.0 * scale],
                    color: [0.85, 0.2, 0.2, 0.9],
                    corner_radius: 0.0,
                    highlight: 0.0,
                    _pad: [0.0, 0.0],
                });
            }
        }
    }
    let surface_handle = if gpu_available {
        paint_gpu(state, window, &instances)
    } else {
        None
    };

    const RULER_TICKS: usize = 6;
    let mut ruler_elements: Vec<AnyElement> = Vec::with_capacity(RULER_TICKS);
    for tick in 0..=RULER_TICKS {
        let fraction = tick as f64 / RULER_TICKS as f64;
        let ns = domain_start_ns + (fraction * domain_span_ns) as u64;
        let ms = (ns.saturating_sub(domain_start_ns)) as f64 / 1.0e6;
        ruler_elements.push(
            div()
                .absolute()
                .top(px(0.))
                .left(px(fraction as f32 * chart_width))
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format!("{ms:.0} ms"))
                .into_any_element(),
        );
    }

    let selection_element = selection.map(|(start, end)| {
        let x0 =
            ((start.saturating_sub(domain_start_ns)) as f64 / domain_span_ns) as f32 * chart_width;
        let x1 =
            ((end.saturating_sub(domain_start_ns)) as f64 / domain_span_ns) as f32 * chart_width;
        div()
            .absolute()
            .top(px(0.))
            .bottom(px(0.))
            .left(px(x0.min(x1)))
            .w(px((x0 - x1).abs().max(1.0)))
            .bg(cx.theme().selection.opacity(0.35))
            .border_1()
            .border_color(cx.theme().selection)
            .into_any_element()
    });

    let panel_entity = cx.entity().clone();

    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Drag to select a range \u{00B7} double-click to clear"),
        )
        .child(
            div()
                .id("record-overview")
                .relative()
                .h(px(OVERVIEW_HEIGHT + 16.0))
                .overflow_hidden()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .child(
                    canvas(
                        {
                            let panel_entity = panel_entity.clone();
                            move |bounds, _window, cx| {
                                panel_entity.update(cx, |panel, _cx| {
                                    panel.record.overview.bounds = bounds;
                                });
                            }
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
                .children(ruler_elements)
                .when_some(surface_handle, |el, handle| {
                    el.child(
                        div()
                            .absolute()
                            .bottom(px(0.))
                            .left(px(0.))
                            .w(px(chart_width))
                            .h(px(OVERVIEW_HEIGHT))
                            .child(
                                gpui::wgpu_surface(handle)
                                    .absolute()
                                    .inset_0()
                                    .defer_resize_until_mouse_up(true),
                            ),
                    )
                })
                .when(!gpu_available, |el| {
                    el.child(
                        div()
                            .absolute()
                            .bottom(px(0.))
                            .left(px(0.))
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .child("GPU overview bars unavailable on this platform/build"),
                    )
                })
                .children(selection_element)
                .on_drag(RangeSelectDrag, {
                    let panel_entity = panel_entity.clone();
                    move |_, start_position, _window, cx| {
                        panel_entity.update(cx, |panel, _cx| {
                            let ns = overview_x_to_ns(&panel.record.overview, panel.record.overview_data(), start_position.x);
                            panel.record.overview.drag_anchor_ns = ns;
                            if ns.is_some() {
                                panel.record.selection = None;
                            }
                        });
                        cx.new(|_| RangeSelectDrag)
                    }
                })
                .on_drag_move(cx.listener(
                    |panel, event: &DragMoveEvent<RangeSelectDrag>, _window, cx| {
                        let Some(anchor_ns) = panel.record.overview.drag_anchor_ns else {
                            return;
                        };
                        let Some(current_ns) = overview_x_to_ns(
                            &panel.record.overview,
                            panel.record.overview_data(),
                            event.event.position.x,
                        ) else {
                            return;
                        };
                        panel.record.selection = Some((
                            anchor_ns.min(current_ns),
                            anchor_ns.max(current_ns).max(anchor_ns + 1),
                        ));
                        cx.notify();
                    },
                ))
                .on_click(cx.listener(|panel, event: &ClickEvent, _window, cx| {
                    if event.click_count() >= 2 {
                        panel.record.selection = None;
                        cx.notify();
                    }
                })),
        )
        .into_any_element()
}

/// Converts a window-absolute x coordinate into a nanosecond instant on the
/// overview's domain axis, or `None` if the overview hasn't measured its
/// bounds yet. The overview never zooms, so unlike the detail flame chart's
/// own coordinate math this is a direct linear map against the *whole*
/// domain, not a zoomed visible window.
fn overview_x_to_ns(
    state: &OverviewState,
    overview: Option<&RecordOverview>,
    absolute_x: Pixels,
) -> Option<u64> {
    let overview = overview?;
    let width = f32::from(state.bounds.size.width);
    if width <= 1.0 {
        return None;
    }
    let local_x = f32::from(absolute_x) - f32::from(state.bounds.origin.x);
    let fraction = (local_x / width).clamp(0.0, 1.0) as f64;
    let domain_span_ns = (overview.domain_end_ns - overview.domain_start_ns) as f64;
    Some(overview.domain_start_ns + (fraction * domain_span_ns) as u64)
}

fn paint_gpu(
    state: &mut OverviewState,
    window: &Window,
    instances: &[BarInstance],
) -> Option<gpui::WgpuSurfaceHandle> {
    if state.gpu.is_none() {
        let Some(surface) = window.create_wgpu_surface(1, 1, wgpu::TextureFormat::Rgba8UnormSrgb)
        else {
            state.gpu_unavailable = true;
            return None;
        };
        let device = surface.device().clone();
        let pipeline = Rc::new(FlameBarPipeline::new(&device, surface.format()));
        state.gpu = Some(FlameLaneGpu::new(&device, surface, &pipeline));
        state.pipeline = Some(pipeline);
    }
    let pipeline = state.pipeline.clone()?;
    let gpu = state.gpu.as_mut()?;
    let handle = gpu.surface.clone();
    let Some((view, (width, height))) = handle.back_view_with_size() else {
        return Some(handle);
    };
    gpu.render(&pipeline, instances, &view, width, height);
    drop(view);
    handle.swap_buffers();
    Some(handle)
}

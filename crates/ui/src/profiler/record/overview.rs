//! The overview strip (§2 of `.agents/PROFILER_UI_SPEC.md`): a ruler-labeled
//! **stacked** CPU-activity area graph plus a per-frame "Frames" row over the
//! *whole* capture window, with a drag-to-select gesture that sets the
//! active range. Deliberately never zooms itself — it's the navigator, not
//! the thing being navigated; the detail flame chart is what zooms, via the
//! selection this produces. GPU-instanced via
//! [`crate::profiler::BarInstance`]/[`FlameLaneGpu`], the same approach the
//! detail flame chart's lanes use — not one `div()` per bucket/frame (up to
//! [`super::data::OVERVIEW_BUCKET_COUNT`] buckets times up to
//! [`super::data::OVERVIEW_CATEGORIES`]`.len()` stacked segments, plus one
//! rectangle per recorded frame, redrawn on every pan/selection change).
//! Both rows share a single `wgpu_surface`/instance buffer (one draw call for
//! the whole strip) rather than one surface per row — nothing here needs
//! independent GPU resources, and halving the surface count halves the
//! per-render `back_view_with_size`/`swap_buffers` bookkeeping.
//!
//! # What's here vs. still deferred
//!
//! The CPU graph renders a true stacked area (every category in
//! [`super::data::OVERVIEW_CATEGORIES`] gets its own vertically-stacked
//! segment per bucket, using [`super::data::OverviewBucket::category_ns`]'s
//! full breakdown — not just [`super::data::OverviewBucket::dominant_category`]
//! collapsed to one solid color), the Frames row is real (one color-coded
//! rectangle per entry in `frame_durations_ms`, Chrome's own 16.7ms/33ms
//! thresholds), and the long-task hatch (§5,
//! [`super::data::OverviewBucket::has_long_task`]) is preserved from v1.
//! Still not here, deliberately (`.agents/PROFILER_UI_SPEC.md`'s own
//! build-order note flags these as lower value than the CPU graph + Frames
//! row): a Network row (this profiler doesn't capture network activity), a
//! Timings/Interactions row (no marker/interaction data model yet), and the
//! screenshot filmstrip (no frame-image capture in this codebase at all —
//! see the task boundary that shipped this file). The Frames row also has to
//! *place* each frame along the time axis from `frame_durations_ms` alone
//! (durations only, no per-frame start times cross this function's
//! contract — see [`frame_x_range`]'s doc for the back-to-back-frames
//! assumption that follows from that).

use std::rc::Rc;

use gpui::{
    canvas, div, prelude::FluentBuilder as _, px, AnyElement, AppContext as _, Bounds, ClickEvent,
    Context, DragMoveEvent, Entity, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, Render, StatefulInteractiveElement as _, Styled, Window,
};

use crate::{v_flex, ActiveTheme};

use crate::profiler::{category_color, BarInstance, FlameBarPipeline, FlameLaneGpu};

use super::{
    data::{OverviewBucket, RecordOverview, OVERVIEW_CATEGORIES},
    ProfilerPanel,
};

/// Drag marker for the overview's range-select gesture (drawing a brand
/// new selection from scratch). GPUI's `on_drag` mechanism only cares about
/// matching `TypeId`s, and the Record tab has no other draggable view
/// mounted at the same time, so a single marker type is safe.
#[derive(Clone)]
struct RangeSelectDrag;

impl Render for RangeSelectDrag {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// Drag marker for resizing an *existing* selection's left edge only — a
/// distinct type from [`RangeSelectDrag`] so its own `on_drag`/
/// `on_drag_move` pair can hold the right edge fixed instead of drawing a
/// new selection from scratch. See [`left_edge_grip`].
#[derive(Clone)]
struct LeftEdgeDrag;

impl Render for LeftEdgeDrag {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// The right-edge counterpart to [`LeftEdgeDrag`] — see [`right_edge_grip`].
#[derive(Clone)]
struct RightEdgeDrag;

impl Render for RightEdgeDrag {
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
    /// `start_ns` of an in-progress drag-select (drawing a brand new
    /// selection); `None` when no such drag is active. The end is always
    /// "wherever the pointer currently is", so only the anchor needs to be
    /// state.
    drag_anchor_ns: Option<u64>,
    /// The *opposite* edge's ns value, held fixed while the user drags one
    /// of the two selection-edge grips ([`left_edge_grip`]/
    /// [`right_edge_grip`]) specifically, as opposed to `drag_anchor_ns`
    /// above (which anchors a brand-new selection instead of resizing an
    /// existing one). `Some` only while an edge-resize drag is in progress.
    edge_fixed_ns: Option<u64>,
    gpu: Option<FlameLaneGpu>,
    pipeline: Option<Rc<FlameBarPipeline>>,
    /// Latches once `Window::create_wgpu_surface` ever returns `None`
    /// (headless test platform, or a backend/platform that doesn't support
    /// it), so the "GPU unavailable" notice doesn't flicker if creation
    /// transiently fails on exactly one render.
    gpu_unavailable: bool,
}

// ── Layout constants ─────────────────────────────────────────────────────
//
// Local (unscaled, logical-pixel) geometry of the strip, top to bottom:
// ruler labels, then the CPU graph, a thin gap, then the Frames row. Kept as
// named constants (not magic numbers scattered through the instance
// builders) because the GPU surface's own local coordinate space starts at
// y=0 = the top of the CPU graph (right below the ruler, which is a plain
// text overlay outside the GPU surface, not drawn by it) — every builder
// below needs the same numbers to agree on where each row lives.

/// Height of the ruler-label text row above the GPU-drawn area.
const RULER_HEIGHT: f32 = 16.0;
/// Height of the stacked CPU-activity area graph.
const CPU_GRAPH_HEIGHT: f32 = 44.0;
/// Vertical breathing room between the CPU graph and the Frames row —
/// Chrome's own overview keeps its rows visually separate rather than
/// butted directly together.
const FRAME_ROW_GAP: f32 = 3.0;
/// Height of the Frames row. Deliberately thin relative to the CPU graph
/// (matches the reference screenshots — Frames is a secondary signal, not
/// a second chart of equal visual weight).
const FRAME_ROW_HEIGHT: f32 = 10.0;
/// Height of the region the shared GPU surface actually draws into (CPU
/// graph + gap + Frames row); the ruler sits above this, drawn as plain text
/// elements, not GPU instances.
const GRAPH_AREA_HEIGHT: f32 = CPU_GRAPH_HEIGHT + FRAME_ROW_GAP + FRAME_ROW_HEIGHT;
/// Total strip height: ruler + graph area.
const STRIP_HEIGHT: f32 = RULER_HEIGHT + GRAPH_AREA_HEIGHT;
/// Height of the red long-task hatch mark drawn along the very top edge of
/// the CPU graph — an orthogonal "is-a-problem" channel from category color
/// (§5's own "never conflate what-kind with is-a-problem" rule).
const LONG_TASK_HATCH_HEIGHT: f32 = 3.0;

const DEFAULT_WIDTH: f32 = 900.0;
const RULER_TICKS: usize = 6;
/// Width of each edge grip's *hit target* (the actual draggable area) —
/// wider than the visible pill mark itself so the edge is easy to grab
/// without needing pixel-perfect precision, same padding idea
/// `resizable::resize_handle`'s own `HANDLE_PADDING` uses for panel-split
/// handles.
const GRIP_HIT_WIDTH: f32 = 10.0;

pub(crate) fn render(
    state: &mut OverviewState,
    overview_data: Option<&RecordOverview>,
    selection: Option<(u64, u64)>,
    frame_durations_ms: &[f32],
    window: &mut Window,
    cx: &mut Context<ProfilerPanel>,
) -> AnyElement {
    let Some(overview) = overview_data else {
        return div().into_any_element();
    };

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
        push_ruler_gridlines(&mut instances, chart_width, scale, cx);
        for (index, bucket) in overview.buckets.iter().enumerate() {
            let x = index as f32 * bucket_width_px;
            let width = (bucket_width_px - 0.5).max(1.0);
            push_cpu_stack_segments(&mut instances, bucket, x, width, max_ns, scale, cx);
        }
        push_frame_row_segments(
            &mut instances,
            frame_durations_ms,
            domain_span_ns,
            chart_width,
            scale,
            cx,
        );
    }
    let surface_handle = if gpu_available {
        paint_gpu(state, window, &instances)
    } else {
        None
    };

    let mut ruler_elements: Vec<AnyElement> = Vec::with_capacity(RULER_TICKS);
    for tick in 0..=RULER_TICKS {
        let fraction = tick as f64 / RULER_TICKS as f64;
        let ns = domain_start_ns + (fraction * domain_span_ns) as u64;
        let ms = (ns.saturating_sub(domain_start_ns)) / 1_000_000;
        ruler_elements.push(
            div()
                .absolute()
                .top(px(0.))
                .left(px(fraction as f32 * chart_width + 3.0))
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format!("{} ms", format_ms_with_commas(ms)))
                .into_any_element(),
        );
    }

    let panel_entity = cx.entity().clone();

    let selection_element = selection.map(|(start, end)| {
        render_selection_overlay(
            start,
            end,
            domain_start_ns,
            domain_span_ns,
            chart_width,
            panel_entity.clone(),
            cx,
        )
    });

    v_flex()
        .w_full()
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
                .w_full()
                .h(px(STRIP_HEIGHT))
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
                            .top(px(RULER_HEIGHT))
                            .left(px(0.))
                            .w(px(chart_width))
                            .h(px(GRAPH_AREA_HEIGHT))
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
                            .top(px(RULER_HEIGHT))
                            .left(px(0.))
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .child("GPU overview graph unavailable on this platform/build"),
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

/// Renders the translucent selection box: a tinted fill + border spanning
/// the *whole* strip height (ruler included — matches the reference
/// screenshots, where the selection's drag handles run from the very top of
/// the ruler down through the Frames row), plus two independently
/// draggable edge grips so either bound can be moved on its own, holding
/// the other bound fixed — not just a whole-selection redraw gesture.
fn render_selection_overlay(
    start: u64,
    end: u64,
    domain_start_ns: u64,
    domain_span_ns: f64,
    chart_width: f32,
    panel_entity: Entity<ProfilerPanel>,
    cx: &Context<ProfilerPanel>,
) -> AnyElement {
    let x0 = ((start.saturating_sub(domain_start_ns)) as f64 / domain_span_ns) as f32 * chart_width;
    let x1 = ((end.saturating_sub(domain_start_ns)) as f64 / domain_span_ns) as f32 * chart_width;
    let left = x0.min(x1);
    let width = (x0 - x1).abs().max(1.0);
    let right = left + width;

    // A single `inset_0()` wrapper so the fill/border box and the two edge
    // grips all position `left`/`top` against the *same* coordinate space
    // (the whole strip) rather than nesting an absolutely-positioned grip
    // inside an already-absolutely-positioned, explicitly-widthed fill box
    // — that would make the grip's `left` relative to the fill box's edge
    // in one case and the wrapper's collapsed auto-width in the other,
    // two different origins for what should be one consistent x axis.
    div()
        .absolute()
        .inset_0()
        .child(
            div()
                .absolute()
                .top(px(0.))
                .bottom(px(0.))
                .left(px(left))
                .w(px(width))
                .bg(cx.theme().selection.opacity(0.2))
                .border_1()
                .border_color(cx.theme().selection),
        )
        .child(left_edge_grip(left, end, panel_entity.clone()))
        .child(right_edge_grip(right, start, panel_entity))
        .into_any_element()
}

/// The small visible pill mark inside an edge grip's (wider, invisible)
/// drag-hit-target — the affordance cue for "you can drag this edge",
/// matching the reference's own selection handles. `local_x` is relative to
/// the grip's own hit-target box, not the strip.
fn selection_grip_mark(local_x: f32) -> AnyElement {
    div()
        .absolute()
        .top(px(STRIP_HEIGHT / 2.0 - 7.0))
        .left(px(local_x - 2.0))
        .w(px(4.0))
        .h(px(14.0))
        .rounded_full()
        .bg(gpui::black().opacity(0.35))
        .into_any_element()
}

/// One selection edge's drag-to-resize hit target: a small `.occlude()`d
/// box (matching `resizable::resize_handle`'s own "small drag handle on top
/// of a larger interactive area" pattern — occlusion is what stops the
/// strip's own whole-selection `RangeSelectDrag` from *also* firing for the
/// same mouse-down, since GPUI hit-tests the topmost occluding element
/// first) that moves *only* this edge, holding `other_edge_ns` fixed for
/// the duration of the drag. `x` is in strip-local coordinates, same axis
/// [`render_selection_overlay`]'s fill box uses.
fn left_edge_grip(x: f32, other_edge_ns: u64, panel_entity: Entity<ProfilerPanel>) -> AnyElement {
    div()
        .id("record-overview-grip-left")
        .occlude()
        .cursor_col_resize()
        .absolute()
        .top(px(0.))
        .left(px(x - GRIP_HIT_WIDTH / 2.0))
        .w(px(GRIP_HIT_WIDTH))
        .h(px(STRIP_HEIGHT))
        .child(selection_grip_mark(GRIP_HIT_WIDTH / 2.0))
        .on_drag(LeftEdgeDrag, {
            let panel_entity = panel_entity.clone();
            move |_, _start_position, _window, cx| {
                panel_entity.update(cx, |panel, _cx| {
                    panel.record.overview.edge_fixed_ns = Some(other_edge_ns);
                });
                cx.new(|_| LeftEdgeDrag)
            }
        })
        .on_drag_move(move |event: &DragMoveEvent<LeftEdgeDrag>, _window, cx| {
            panel_entity.update(cx, |panel, cx| {
                let Some(fixed_ns) = panel.record.overview.edge_fixed_ns else {
                    return;
                };
                let Some(current_ns) = overview_x_to_ns(
                    &panel.record.overview,
                    panel.record.overview_data(),
                    event.event.position.x,
                ) else {
                    return;
                };
                panel.record.selection =
                    Some((fixed_ns.min(current_ns), fixed_ns.max(current_ns).max(fixed_ns + 1)));
                cx.notify();
            });
        })
        .into_any_element()
}

/// The right-edge counterpart to [`left_edge_grip`] — identical shape,
/// [`RightEdgeDrag`] marker instead so the two grips' gestures can never be
/// confused with one another.
fn right_edge_grip(x: f32, other_edge_ns: u64, panel_entity: Entity<ProfilerPanel>) -> AnyElement {
    div()
        .id("record-overview-grip-right")
        .occlude()
        .cursor_col_resize()
        .absolute()
        .top(px(0.))
        .left(px(x - GRIP_HIT_WIDTH / 2.0))
        .w(px(GRIP_HIT_WIDTH))
        .h(px(STRIP_HEIGHT))
        .child(selection_grip_mark(GRIP_HIT_WIDTH / 2.0))
        .on_drag(RightEdgeDrag, {
            let panel_entity = panel_entity.clone();
            move |_, _start_position, _window, cx| {
                panel_entity.update(cx, |panel, _cx| {
                    panel.record.overview.edge_fixed_ns = Some(other_edge_ns);
                });
                cx.new(|_| RightEdgeDrag)
            }
        })
        .on_drag_move(move |event: &DragMoveEvent<RightEdgeDrag>, _window, cx| {
            panel_entity.update(cx, |panel, cx| {
                let Some(fixed_ns) = panel.record.overview.edge_fixed_ns else {
                    return;
                };
                let Some(current_ns) = overview_x_to_ns(
                    &panel.record.overview,
                    panel.record.overview_data(),
                    event.event.position.x,
                ) else {
                    return;
                };
                panel.record.selection =
                    Some((fixed_ns.min(current_ns), fixed_ns.max(current_ns).max(fixed_ns + 1)));
                cx.notify();
            });
        })
        .into_any_element()
}

/// Appends the stacked-area segments for one CPU-graph bucket: one
/// [`BarInstance`] per non-empty category in [`OVERVIEW_CATEGORIES`] order
/// (bottom of the graph upward), plus the long-task hatch when this bucket
/// has one — matches Chrome's own layered/stacked CPU graph rather than
/// collapsing each bucket to its single dominant category's color.
fn push_cpu_stack_segments(
    instances: &mut Vec<BarInstance>,
    bucket: &OverviewBucket,
    x: f32,
    width: f32,
    max_ns: u64,
    scale: f32,
    cx: &Context<ProfilerPanel>,
) {
    if bucket.total_ns == 0 {
        return;
    }
    let mut cumulative_ns: u64 = 0;
    for (category_index, category) in OVERVIEW_CATEGORIES.iter().enumerate() {
        let segment_ns = bucket.category_ns[category_index];
        if segment_ns == 0 {
            continue;
        }
        let cumulative_after = cumulative_ns + segment_ns;
        let (top, bottom) = stack_segment_y(cumulative_ns, cumulative_after, max_ns, CPU_GRAPH_HEIGHT);
        cumulative_ns = cumulative_after;

        let rgba = category_color(*category, cx).to_rgb();
        instances.push(BarInstance {
            rect_min: [x * scale, top * scale],
            rect_max: [(x + width) * scale, bottom * scale],
            color: [rgba.r, rgba.g, rgba.b, rgba.a],
            corner_radius: 0.0,
            highlight: 0.0,
            _pad: [0.0, 0.0],
        });
    }

    if bucket.has_long_task {
        instances.push(BarInstance {
            rect_min: [x * scale, 0.0],
            rect_max: [(x + width) * scale, LONG_TASK_HATCH_HEIGHT * scale],
            color: [0.85, 0.2, 0.2, 0.9],
            corner_radius: 0.0,
            highlight: 0.0,
            _pad: [0.0, 0.0],
        });
    }
}

/// Computes one stacked category segment's `(top_y, bottom_y)` in local
/// graph pixels, stacked from the bottom of a `graph_height`-tall area
/// upward given the running nanosecond totals before/after this segment
/// (out of `max_ns`). Pulled out of [`push_cpu_stack_segments`] so the
/// stacking math has exactly one implementation to unit-test (see
/// `tests::stack_segment_*` below) instead of re-deriving the two fractions
/// inline for every category of every bucket.
fn stack_segment_y(cumulative_before: u64, cumulative_after: u64, max_ns: u64, graph_height: f32) -> (f32, f32) {
    let max_ns = max_ns.max(1) as f32;
    let bottom = graph_height - (cumulative_before as f32 / max_ns) * graph_height;
    let top = graph_height - (cumulative_after as f32 / max_ns) * graph_height;
    (top, bottom)
}

/// Appends one [`BarInstance`] per entry in `frame_durations_ms`, colored by
/// Chrome's own three-tier frame-time classification (see [`frame_class`]),
/// laid out left-to-right along the Frames row using [`frame_x_range`].
fn push_frame_row_segments(
    instances: &mut Vec<BarInstance>,
    frame_durations_ms: &[f32],
    domain_span_ns: f64,
    chart_width: f32,
    scale: f32,
    cx: &Context<ProfilerPanel>,
) {
    if frame_durations_ms.is_empty() {
        return;
    }
    let row_top = CPU_GRAPH_HEIGHT + FRAME_ROW_GAP;
    let row_bottom = GRAPH_AREA_HEIGHT;
    let mut cumulative_ns_before: f64 = 0.0;
    for duration_ms in frame_durations_ms {
        let duration_ns = (*duration_ms as f64) * 1.0e6;
        let (x0, x1) = frame_x_range(cumulative_ns_before, duration_ns, domain_span_ns, chart_width);
        cumulative_ns_before += duration_ns;

        let rgba = frame_class(*duration_ms).color(cx).to_rgb();
        // A hairline gap between adjacent frame rectangles when they're wide
        // enough to show one (sparse frames); at real frame-rate density
        // (hundreds of frames across a few hundred pixels) this rounds away
        // to nothing and the row reads as one continuous strip, matching the
        // reference screenshots' own dense-frame appearance.
        let x1 = (x1 - 0.5).max(x0 + 0.5);
        instances.push(BarInstance {
            rect_min: [x0 * scale, row_top * scale],
            rect_max: [x1 * scale, row_bottom * scale],
            color: [rgba.r, rgba.g, rgba.b, rgba.a],
            corner_radius: 0.0,
            highlight: 0.0,
            _pad: [0.0, 0.0],
        });
    }
}

/// Maps one frame's `[cumulative_ns_before, cumulative_ns_before +
/// duration_ns)` onto `[0, chart_width)` in the overview's whole-domain
/// axis. Assumes frames sit back-to-back with no gap, starting at the
/// domain origin — the only assumption possible given this function's
/// inputs: `render`'s contract passes `frame_durations_ms: &[f32]` (plain
/// durations, computed in `profiler/mod.rs` as `frame_end_ns -
/// frame_start_ns`), not each frame's absolute `start_ns`, so there is no
/// per-frame timestamp to place it from directly. In practice frames
/// *are* recorded back-to-back (`domain_start_ns`/`domain_end_ns` in
/// `data::build_overview` are exactly the first/last frame's own bounds),
/// so this reproduces the true layout whenever there's no idle gap between
/// frames; a real gap would only ever compress the tail of the row inward
/// (last frame ends slightly left of the CPU graph's own right edge), never
/// misplace a frame outside the strip.
fn frame_x_range(cumulative_ns_before: f64, duration_ns: f64, domain_span_ns: f64, chart_width: f32) -> (f32, f32) {
    let domain_span_ns = domain_span_ns.max(1.0);
    let x0 = (cumulative_ns_before / domain_span_ns) as f32 * chart_width;
    let x1 = ((cumulative_ns_before + duration_ns) / domain_span_ns) as f32 * chart_width;
    (x0, x1.max(x0 + 0.5))
}

/// Chrome's own three-tier frame-time classification: comfortably inside a
/// 60fps budget, inside a 30fps budget, or worse than that (a dropped/janky
/// frame). Named cut points (rather than the comparisons inlined at the one
/// call site) so `insights.rs`'s own "janky frame" count (`> 16.7`) and this
/// row's coloring can't silently drift apart if either is edited later.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FrameClass {
    Good,
    Warn,
    Bad,
}

impl FrameClass {
    fn color(self, cx: &Context<ProfilerPanel>) -> Hsla {
        let theme = cx.theme();
        match self {
            FrameClass::Good => theme.success,
            FrameClass::Warn => theme.warning,
            FrameClass::Bad => theme.danger,
        }
    }
}

/// `<= 16.7ms` (60fps) is [`FrameClass::Good`], `<= 33ms` (30fps) is
/// [`FrameClass::Warn`], anything slower is [`FrameClass::Bad`] — Chrome's
/// own Frames-row thresholds.
fn frame_class(duration_ms: f32) -> FrameClass {
    if duration_ms <= 16.7 {
        FrameClass::Good
    } else if duration_ms <= 33.0 {
        FrameClass::Warn
    } else {
        FrameClass::Bad
    }
}

/// Appends one thin, low-opacity vertical gridline per ruler tick, spanning
/// the whole graph area (CPU graph + Frames row) — the reference
/// screenshots carry faint vertical guides under the ruler labels, tying
/// the label to the exact column below it rather than leaving the labels to
/// float unanchored over the graph.
fn push_ruler_gridlines(
    instances: &mut Vec<BarInstance>,
    chart_width: f32,
    scale: f32,
    cx: &Context<ProfilerPanel>,
) {
    let rgba = cx.theme().border.opacity(0.5).to_rgb();
    for tick in 0..=RULER_TICKS {
        let fraction = tick as f32 / RULER_TICKS as f32;
        let x = fraction * chart_width;
        instances.push(BarInstance {
            rect_min: [x * scale, 0.0],
            rect_max: [(x + 1.0) * scale, GRAPH_AREA_HEIGHT * scale],
            color: [rgba.r, rgba.g, rgba.b, rgba.a],
            corner_radius: 0.0,
            highlight: 0.0,
            _pad: [0.0, 0.0],
        });
    }
}

/// Formats a nonnegative millisecond count with thousands separators
/// (`12345` -> `"12,345"`), matching Chrome's own ruler label style
/// (`1,000 ms`, not `1000 ms`) — plain `format!("{ms}")` doesn't group
/// digits.
fn format_ms_with_commas(ms: u64) -> String {
    let digits = ms.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let len = digits.len();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_segment_y_first_segment_sits_at_the_bottom() {
        // A category with no predecessors (cumulative_before == 0) that is
        // exactly half of max_ns should occupy the bottom half of the graph.
        let (top, bottom) = stack_segment_y(0, 50, 100, 40.0);
        assert_eq!(bottom, 40.0);
        assert_eq!(top, 20.0);
    }

    #[test]
    fn stack_segment_y_stacks_on_top_of_prior_segments() {
        // A second segment stacked after a first that already consumed half
        // the bucket should occupy the *upper* half, not overlap it.
        let (top, bottom) = stack_segment_y(50, 100, 100, 40.0);
        assert_eq!(bottom, 20.0);
        assert_eq!(top, 0.0);
    }

    #[test]
    fn stack_segment_y_full_bucket_spans_the_whole_graph_height() {
        let (top, bottom) = stack_segment_y(0, 100, 100, 40.0);
        assert_eq!(bottom, 40.0);
        assert_eq!(top, 0.0);
    }

    #[test]
    fn frame_x_range_first_frame_starts_at_zero() {
        let (x0, x1) = frame_x_range(0.0, 16_700_000.0, 100_000_000.0, 900.0);
        assert_eq!(x0, 0.0);
        assert!((x1 - 150.3).abs() < 1.0);
    }

    #[test]
    fn frame_x_range_never_produces_a_zero_width_rect() {
        let (x0, x1) = frame_x_range(0.0, 0.0, 100_000_000.0, 900.0);
        assert!(x1 > x0);
    }

    #[test]
    fn frame_class_thresholds_match_chrome() {
        assert_eq!(frame_class(16.7), FrameClass::Good);
        assert_eq!(frame_class(16.8), FrameClass::Warn);
        assert_eq!(frame_class(33.0), FrameClass::Warn);
        assert_eq!(frame_class(33.1), FrameClass::Bad);
    }

    #[test]
    fn format_ms_with_commas_groups_thousands() {
        assert_eq!(format_ms_with_commas(0), "0");
        assert_eq!(format_ms_with_commas(999), "999");
        assert_eq!(format_ms_with_commas(1000), "1,000");
        assert_eq!(format_ms_with_commas(1234567), "1,234,567");
    }
}

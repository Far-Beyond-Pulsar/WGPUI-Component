//! The memory track (§3 of `.agents/PROFILER_UI_SPEC.md`, shown when the
//! toolbar's `☑ Memory` toggle is on): one chart with several **overlaid
//! step lines**, each independently toggleable via a checkbox-legend row
//! above it — the same shape as Chrome's own memory graph
//! (`☑ JS heap [81.1 MB – 82.5 MB]  ☑ Documents [3 – 3]  ☑ Nodes [...]  ...`
//! sitting above a chart of flat-then-jump lines).
//!
//! # v2 note, honestly
//!
//! The *metrics* are still not Chrome's — no JS heap equivalent exists in a
//! native UI framework, and `ProfilerPanel::memory_cpu`/`memory_gpu` are
//! one-shot on-demand snapshots (`gpui::MemorySnapshot`/`GpuMemorySnapshot`),
//! not a per-frame time series, so they can't back a chart like this without
//! new instrumentation (`.agents/PROFILER_UI_SPEC.md`'s own build-order note
//! already flagged this as needing new counters, not just a new view). What
//! *is* real and per-frame today, and what this still renders: each
//! [`gpui::FrameCapture`]'s own span/diagnostic counts (CPU spans,
//! background-task spans, GPU spans, diagnostic events) — real counts,
//! honestly labeled, standing in for JS heap/Documents/Nodes/Listeners/GPU
//! memory until it's worth adding real per-frame memory counters.
//!
//! What *did* change from v1: the **rendering**. v1 drew each series as its
//! own separate sparkline-bar row; this draws every checked series as one
//! overlaid step-line polyline in a single chart area, behind a real
//! checkbox legend, matching the reference's visual shape. A few concrete
//! choices, since the reference doesn't literally give a spec for a
//! from-scratch rebuild:
//!
//! - **X-axis**: each frame's `frame_start_ns`, mapped onto `[start_ns,
//!   end_ns]` — the exact same domain the overview strip and detail flame
//!   chart above this pane use — so the memory track's time axis lines up
//!   with theirs, the same way Chrome's memory graph sits directly under its
//!   Main track with a shared timeline. Not index-evenly-spaced like v1's
//!   bars were.
//! - **Y-axis**: each series is normalized to the *full chart height*
//!   against its **own** `(min, max)`, independently of every other series.
//!   This isn't a simplification of Chrome's real per-series-auto-scaling
//!   behavior — it *is* that behavior (it's exactly why Chrome can overlay a
//!   ~megabyte-scale JS-heap line against a low-hundreds Nodes line and both
//!   still read clearly). It's also why unchecking one series never moves
//!   any other series' line or rescales the chart: each line's `y` values
//!   never depended on any other series' data to begin with.
//! - **Line rendering**: still instanced *rectangles* via
//!   `wgpu_surface`/[`crate::profiler::FlameBarPipeline`]/[`crate::profiler::BarInstance`]/
//!   [`crate::profiler::FlameLaneGpu`] (see [`super::overview`], which does
//!   the same GPU setup end-to-end) — deliberately, not an oversight: this
//!   crate *does* have a real path/curve primitive now (`crate::plot`,
//!   backed by `gpui::PathBuilder`; see `super::overview::build_stacked_area_bands`
//!   for the CPU-activity graph's own move to it), but a step line is
//!   flat-then-jump *by definition* — smoothly interpolating between two
//!   discrete per-frame counts would misrepresent them as continuously
//!   changing, which they aren't. So a step-line polyline is approximated
//!   as a sequence of thin filled rectangles — a flat horizontal rect held
//!   at each sample's value until the next sample's x, plus a thin vertical
//!   rect at each jump (see [`step_polyline_rects`]) — one instanced draw
//!   call for the whole chart, not one `div()` per line segment.

use std::rc::Rc;

use gpui::{
    canvas, div, prelude::FluentBuilder as _, px, AnyElement, AppContext as _, Bounds, Context,
    Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    StatefulInteractiveElement as _, Styled, Window,
};

use crate::{h_flex, v_flex, ActiveTheme};

use crate::profiler::{BarInstance, FlameBarPipeline, FlameLaneGpu};

use super::ProfilerPanel;

const SERIES_COUNT: usize = 4;

const SERIES_LABELS: [&str; SERIES_COUNT] = [
    "CPU spans / frame",
    "Background spans / frame",
    "GPU spans / frame",
    "Diagnostics / frame",
];

/// Thin-rectangle line thickness, in logical (pre-scale-factor) pixels.
const LINE_THICKNESS: f32 = 2.0;

/// Fraction of the chart height reserved as top/bottom margin so a
/// perfectly flat series (`min == max`, e.g. a `Documents`-style count that
/// never changes) doesn't paint its line flush against the chart's edge.
const Y_MARGIN_FRAC: f32 = 0.08;

const DEFAULT_WIDTH: f32 = 900.0;
const DEFAULT_HEIGHT: f32 = 140.0;

pub(crate) struct MemoryState {
    /// Measured screen bounds of the chart area (not the whole pane — just
    /// the plot below the legend row) — used both to size the GPU surface
    /// and to convert into pixel coordinates, mirroring
    /// `overview::OverviewState::bounds`.
    bounds: Bounds<Pixels>,
    /// Per-series checkbox state, indexed the same as [`SERIES_LABELS`].
    /// Defaults to all-checked so a freshly opened memory track matches the
    /// reference's default (everything shown).
    visible: [bool; SERIES_COUNT],
    gpu: Option<FlameLaneGpu>,
    pipeline: Option<Rc<FlameBarPipeline>>,
    /// Latches once `Window::create_wgpu_surface` ever returns `None` — see
    /// `overview::OverviewState::gpu_unavailable`'s field doc for why this
    /// is sticky rather than re-checked every render.
    gpu_unavailable: bool,
}

impl Default for MemoryState {
    fn default() -> Self {
        Self {
            bounds: Bounds::default(),
            visible: [true; SERIES_COUNT],
            gpu: None,
            pipeline: None,
            gpu_unavailable: false,
        }
    }
}

pub(crate) fn render(
    state: &mut MemoryState,
    capture: &gpui::Capture,
    start_ns: u64,
    end_ns: u64,
    // The one authoritative panel width every panel in the resizable group
    // shares -- see `RecordState::panels_bounds`'s field doc. `<= 1.0`
    // means "not measured yet".
    panels_width: f32,
    window: &mut Window,
    cx: &mut Context<ProfilerPanel>,
) -> AnyElement {
    let frames: Vec<&gpui::FrameCapture> = capture
        .frames()
        .filter(|f| f.frame_end_ns >= start_ns && f.frame_start_ns <= end_ns)
        .collect();
    if frames.is_empty() {
        // Bare `profiler_empty_state` has no explicit sizing of its own
        // (by design -- it's used inline in plenty of already-`flex_1`
        // contexts too), which given no other content of its own to
        // stretch against would leave this pane exactly as wide as its
        // own message text. Same explicit-width treatment as the real
        // chart below, for the same reason.
        return div()
            .flex_1()
            .when(panels_width > 1.0, |el| el.w(px(panels_width)))
            .child(super::super::profiler_empty_state(
                "No frames in the selected range.",
                cx,
            ))
            .into_any_element();
    }

    let series_values: [Vec<u32>; SERIES_COUNT] = [
        frames.iter().map(|f| f.cpu_spans.len() as u32).collect(),
        frames
            .iter()
            .map(|f| f.background_spans.len() as u32)
            .collect(),
        frames.iter().map(|f| f.gpu_spans.len() as u32).collect(),
        frames.iter().map(|f| f.diagnostics.len() as u32).collect(),
    ];
    let colors: [Hsla; SERIES_COUNT] = [
        cx.theme().chart_1,
        cx.theme().chart_2,
        cx.theme().chart_3,
        cx.theme().chart_4,
    ];

    let legend = render_legend(&state.visible, &series_values, &colors, cx);
    let chart = render_chart(
        state,
        &frames,
        &series_values,
        &colors,
        start_ns,
        end_ns,
        window,
        cx,
    );

    v_flex()
        .id("record-memory-chart")
        // `flex_1().size_full()` is only this frame's fallback (before
        // `panels_width` is measured); `.when(..)` below overrides it with
        // an explicit pixel width shared by every panel in the resizable
        // group -- see `RecordState::panels_bounds`'s field doc.
        .flex_1()
        .size_full()
        .when(panels_width > 1.0, |el| el.w(px(panels_width)))
        .gap_1()
        .p_2()
        .child(legend)
        .child(chart)
        .into_any_element()
}

/// The `☑ label [min – max]` row, one item per series, each tinted with
/// that series' own arbitrary chart color (not the category palette used
/// elsewhere in this app — these lines have nothing to do with span
/// categories) exactly like the reference's legend.
fn render_legend(
    visible: &[bool; SERIES_COUNT],
    series_values: &[Vec<u32>; SERIES_COUNT],
    colors: &[Hsla; SERIES_COUNT],
    cx: &mut Context<ProfilerPanel>,
) -> AnyElement {
    h_flex()
        .id("record-memory-legend")
        .gap_4()
        .flex_wrap()
        .items_center()
        .children((0..SERIES_COUNT).map(|index| {
            let (min, max) = min_max(&series_values[index]);
            let checked = visible[index];
            let color = colors[index];
            h_flex()
                .id(format!("record-memory-legend-{index}"))
                .gap_1p5()
                .items_center()
                .cursor_pointer()
                .child(
                    div()
                        .size(px(10.))
                        .rounded_sm()
                        .border_1()
                        .border_color(color)
                        .when(checked, |d| d.bg(color))
                        .when(!checked, |d| d.bg(cx.theme().background)),
                )
                .child(
                    div()
                        .text_xs()
                        .when(checked, |d| d.text_color(cx.theme().foreground))
                        .when(!checked, |d| d.text_color(cx.theme().muted_foreground))
                        .child(SERIES_LABELS[index]),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("[{min} \u{2013} {max}]")),
                )
                .on_click(cx.listener(move |panel, _ev, _window, cx| {
                    panel.record.memory.visible[index] = !panel.record.memory.visible[index];
                    cx.notify();
                }))
        }))
        .into_any_element()
}

/// The chart area itself: a bounds-measuring `canvas()` overlay (same
/// pattern as `overview::render`) plus, once bounds are known, a
/// GPU-instanced-rectangle surface with one thin step-line polyline per
/// checked series.
fn render_chart(
    state: &mut MemoryState,
    frames: &[&gpui::FrameCapture],
    series_values: &[Vec<u32>; SERIES_COUNT],
    colors: &[Hsla; SERIES_COUNT],
    start_ns: u64,
    end_ns: u64,
    window: &mut Window,
    cx: &mut Context<ProfilerPanel>,
) -> AnyElement {
    let measured_width = f32::from(state.bounds.size.width);
    let chart_width = if measured_width > 1.0 {
        measured_width
    } else {
        DEFAULT_WIDTH
    };
    let measured_height = f32::from(state.bounds.size.height);
    let chart_height = if measured_height > 1.0 {
        measured_height
    } else {
        DEFAULT_HEIGHT
    };

    let gpu_available = !state.gpu_unavailable;
    let scale = window.scale_factor();
    let mut instances: Vec<BarInstance> = Vec::new();
    if gpu_available {
        for index in 0..SERIES_COUNT {
            if !state.visible[index] {
                continue;
            }
            let (ys, _min, _max) = normalize_to_height(&series_values[index], chart_height);
            let points: Vec<(f32, f32)> = frames
                .iter()
                .zip(ys.iter())
                .map(|(frame, &y)| {
                    let x = time_fraction_x(frame.frame_start_ns, start_ns, end_ns, chart_width);
                    (x, y)
                })
                .collect();
            let rgba = colors[index].to_rgb();
            for (x0, y0, x1, y1) in step_polyline_rects(&points, chart_width, LINE_THICKNESS) {
                instances.push(BarInstance {
                    rect_min: [x0 * scale, y0 * scale],
                    rect_max: [x1 * scale, y1 * scale],
                    color: [rgba.r, rgba.g, rgba.b, rgba.a],
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

    // A few faint horizontal guide lines, the same understated grid Chrome's
    // own memory graph has, purely a reading aid -- they carry no data (each
    // series has its own independent scale, so a labeled gridline would be
    // honest for at most one series at a time).
    const GRID_LINES: usize = 3;
    let grid = (1..GRID_LINES).map(|i| {
        let fraction = i as f32 / GRID_LINES as f32;
        div()
            .absolute()
            .left(px(0.))
            .right(px(0.))
            .top(px(fraction * chart_height))
            .h(px(1.))
            .bg(cx.theme().border.opacity(0.4))
            .into_any_element()
    });

    let panel_entity = cx.entity().clone();

    div()
        .id("record-memory-chart-area")
        .relative()
        .flex_1()
        .min_h(px(48.))
        .w_full()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .overflow_hidden()
        .child(
            canvas(
                move |bounds, _window, cx| {
                    panel_entity.update(cx, |panel, cx| {
                        // See `crate::profiler::update_measured_bounds`'s doc
                        // comment: without this notify-on-real-change, a pure
                        // resize (window or split-pane handle) leaves the
                        // fixed-pixel surface below stuck at its pre-resize
                        // size inside this now-correctly-resized container.
                        if crate::profiler::update_measured_bounds(
                            &mut panel.record.memory.bounds,
                            bounds,
                        ) {
                            cx.notify();
                        }
                    });
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
        .children(grid)
        .when_some(surface_handle, |el, handle| {
            el.child(
                div()
                    .absolute()
                    .top(px(0.))
                    .left(px(0.))
                    .w(px(chart_width))
                    .h(px(chart_height))
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
                    .bottom(px(4.))
                    .left(px(4.))
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child("GPU memory chart unavailable on this platform/build"),
            )
        })
        .into_any_element()
}

/// Lazily creates the chart's `wgpu_surface`/pipeline/instance-buffer state
/// on first use, then renders `instances` into it -- mirrors
/// `overview::paint_gpu` exactly (see its doc for why: one lane's worth of
/// GPU state, reused and grown across renders, not recreated per frame).
fn paint_gpu(
    state: &mut MemoryState,
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

/// Maps raw per-frame counts to `y` pixel positions inside `[0, height]`,
/// scaled independently to this one series' own `(min, max)` -- see the
/// module doc's "Y-axis" note for why that's not a simplification of
/// Chrome's real behavior but a match for it.
fn normalize_to_height(values: &[u32], height: f32) -> (Vec<f32>, u32, u32) {
    let (min, max) = min_max(values);
    let margin = height * Y_MARGIN_FRAC;
    let usable = (height - margin * 2.0).max(1.0);
    let span = (max - min) as f32;
    let ys = values
        .iter()
        .map(|&v| {
            let t = if span > 0.0 {
                (v - min) as f32 / span
            } else {
                0.5
            };
            // Screen-space y grows downward, so a bigger value sits nearer
            // the top (smaller y).
            height - margin - t * usable
        })
        .collect();
    (ys, min, max)
}

fn min_max(values: &[u32]) -> (u32, u32) {
    let min = values.iter().copied().min().unwrap_or(0);
    let max = values.iter().copied().max().unwrap_or(0).max(min);
    (min, max)
}

/// Maps a nanosecond instant onto `[0, chart_width]` against the
/// `[start_ns, end_ns]` domain -- the same domain the overview strip and
/// detail flame chart use, so this chart's x-axis lines up with theirs.
fn time_fraction_x(ns: u64, start_ns: u64, end_ns: u64, chart_width: f32) -> f32 {
    let domain_span_ns = end_ns.saturating_sub(start_ns).max(1) as f64;
    let fraction = (ns.saturating_sub(start_ns) as f64 / domain_span_ns).clamp(0.0, 1.0);
    fraction as f32 * chart_width
}

/// Converts a sequence of ascending-`x` `(x, y)` step samples into the thin
/// filled rectangles that approximate a step-line polyline when drawn with
/// the shared bar-instancing GPU primitive (see the module doc's "Line
/// rendering" note). Each sample's value is drawn flat until the next
/// sample's `x` ("steps-post" -- the value is "whatever was last observed",
/// not an interpolation between two readings nobody actually took), then
/// jumps vertically. The final sample's value is held flat out to
/// `chart_width` (the chart's right/"now" edge), matching Chrome's own
/// memory graphs holding the last known reading out to the present.
///
/// Returns `(min_x, min_y, max_x, max_y)` rectangle corners in the same
/// pixel space as the input points.
fn step_polyline_rects(
    points: &[(f32, f32)],
    chart_width: f32,
    thickness: f32,
) -> Vec<(f32, f32, f32, f32)> {
    if points.is_empty() {
        return Vec::new();
    }
    let half = thickness / 2.0;
    let mut rects = Vec::with_capacity(points.len() * 2);
    for (index, &(x, y)) in points.iter().enumerate() {
        let next = points.get(index + 1).copied();
        let next_x = next.map(|(nx, _)| nx).unwrap_or(chart_width);

        // Flat hold segment from this sample out to the next sample's x (or
        // the chart's right edge, for the last sample).
        let x0 = x.min(next_x);
        let x1 = x.max(next_x).max(x0 + thickness);
        rects.push((x0, y - half, x1, y + half));

        // Vertical jump into the next sample's value, if there is one.
        if let Some((_, next_y)) = next {
            let y0 = y.min(next_y);
            let y1 = y.max(next_y).max(y0 + thickness);
            rects.push((next_x - half, y0, next_x + half, y1));
        }
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_max_handles_empty_and_flat() {
        assert_eq!(min_max(&[]), (0, 0));
        assert_eq!(min_max(&[7, 7, 7]), (7, 7));
        assert_eq!(min_max(&[3, 1, 4, 1, 5]), (1, 5));
    }

    #[test]
    fn normalize_to_height_places_max_near_top_min_near_bottom() {
        let (ys, min, max) = normalize_to_height(&[0, 10], 100.0);
        assert_eq!((min, max), (0, 10));
        // Bigger value (index 1, v=10) should have a smaller y (nearer the
        // top) than the smaller value (index 0, v=0).
        assert!(ys[1] < ys[0]);
        // Both stay within the margin-inset range, never flush with the
        // chart edges.
        let margin = 100.0 * Y_MARGIN_FRAC;
        for &y in &ys {
            assert!(y >= margin - 0.01 && y <= 100.0 - margin + 0.01);
        }
    }

    #[test]
    fn normalize_to_height_flat_series_sits_at_the_midline() {
        let (ys, min, max) = normalize_to_height(&[4, 4, 4], 100.0);
        assert_eq!((min, max), (4, 4));
        for &y in &ys {
            assert!((y - 50.0).abs() < 0.01);
        }
    }

    #[test]
    fn time_fraction_x_maps_domain_endpoints_to_chart_edges() {
        assert_eq!(time_fraction_x(0, 0, 1000, 200.0), 0.0);
        assert_eq!(time_fraction_x(1000, 0, 1000, 200.0), 200.0);
        assert_eq!(time_fraction_x(500, 0, 1000, 200.0), 100.0);
        // Clamped even if a frame straddles the selection boundary.
        assert_eq!(time_fraction_x(2000, 0, 1000, 200.0), 200.0);
    }

    #[test]
    fn step_polyline_rects_empty_input_yields_no_rects() {
        assert!(step_polyline_rects(&[], 100.0, 2.0).is_empty());
    }

    #[test]
    fn step_polyline_rects_single_point_holds_flat_to_chart_width() {
        let rects = step_polyline_rects(&[(10.0, 20.0)], 100.0, 2.0);
        // One flat hold segment, no jump (nothing to jump to).
        assert_eq!(rects.len(), 1);
        let (x0, y0, x1, y1) = rects[0];
        assert_eq!(x0, 10.0);
        assert_eq!(x1, 100.0);
        assert_eq!(y0, 19.0);
        assert_eq!(y1, 21.0);
    }

    #[test]
    fn step_polyline_rects_two_points_produce_hold_then_jump() {
        let rects = step_polyline_rects(&[(0.0, 10.0), (50.0, 40.0)], 100.0, 2.0);
        // Flat hold at y=10 from x=0..50, then a vertical jump at x=50 from
        // y=10 to y=40, then a flat hold at y=40 from x=50..100 (chart
        // width, since index 1 is the last point).
        assert_eq!(rects.len(), 3);

        let (x0, y0, x1, y1) = rects[0];
        assert_eq!((x0, x1), (0.0, 50.0));
        assert_eq!((y0, y1), (9.0, 11.0));

        let (x0, y0, x1, y1) = rects[1];
        assert_eq!((x0, x1), (49.0, 51.0));
        assert_eq!((y0, y1), (10.0, 40.0));

        let (x0, y0, x1, y1) = rects[2];
        assert_eq!((x0, x1), (50.0, 100.0));
        assert_eq!((y0, y1), (39.0, 41.0));
    }
}

//! The detail flame chart (§3 of `.agents/PROFILER_UI_SPEC.md`): the
//! zoomed-in, interactive view of whichever range is currently selected on
//! the overview strip.
//!
//! # What this renders, and why it looks the way it does
//!
//! This is a from-scratch renderer built to match Chrome DevTools'
//! Performance panel flame chart, not the plain Flame Chart tab's single-
//! frame view reused verbatim -- see the git history of this file for the
//! v1 stub that just forwarded to `ProfilerPanel::render_flame_lanes_body`.
//! It still leans hard on that tab's own low-level building blocks
//! (`BarInstance`/`FlameLaneGpu`/`FlameBarPipeline` via
//! `ProfilerPanel::paint_flame_lane`, `bar_screen_rect`,
//! `ProfilerPanel::update_flame_hover`/`select_flame_bar_at`, `RangeZoom`
//! via `ProfilerPanel::flame_zoom`) rather than reinventing GPU-instanced
//! rect rendering or hit-testing -- those are already correct, tested, and
//! deliberately shared with the plain Flame Chart tab (the two sections
//! render mutually exclusively, see `ProfilerPanel::render`, so nothing
//! fights over the shared pan/zoom/hover/selection state at once). What's
//! new here is everything about *how a bar looks*:
//!
//! - **Depth-0 "Task" styling.** Chrome's own Task row is a flat neutral
//!   gray no matter what ran inside it -- the color story starts one level
//!   down, at `Layout`/`Commit`/etc. [`bar_fill_color`] mirrors that: depth
//!   0 always renders as `theme.secondary`, and `category_color`/
//!   `gpu_pass_color` (unchanged, shared with the plain tab) drive every
//!   bar below it.
//! - **The red long-task hatch.** A depth-0 bar at or above
//!   `super::data::LONG_TASK_NS` (50ms -- the same constant the overview
//!   strip's own hatching already uses, so the two views can never
//!   disagree about what counts as "long") gets a translucent red hatch
//!   band along its top edge, via [`long_task_hatch_rects`]. The shared bar
//!   shader (`profiler_flame_shader.wgsl`, out of scope for this file) only
//!   knows how to draw axis-aligned rects with no rotation, so a literal
//!   45-degree stripe isn't available without touching the shader; this
//!   approximates one as a repeating diagonal *staircase* of small squares,
//!   which reads as a hatch at the ~5px band height it renders at. The
//!   hatch rects are extra `BarInstance`s appended to the same lane's
//!   instance buffer right after the bar they belong to, relying on the one
//!   GPU-instanced draw call rasterizing/blending instances in push order
//!   (true on every backend this renders through) so the hatch composites
//!   on top of the solid fill rather than under it.
//! - **In-place text labels.** Chrome labels the handful of big blocks near
//!   the top of the stack (`Task`, `Layout`, `Commit`, ...) but never the
//!   dense unlabeled "sawtooth" of tiny calls a few rows down -- there just
//!   isn't room, and it isn't the point (you zoom in for that). This
//!   crate's original perf fix for the flame chart was specifically about
//!   *not* creating a GPUI element per bar (see the "Flame chart GPU
//!   rendering" section in `profiler/mod.rs`), so [`should_label_bar`]
//!   keeps this file's on-bar label divs bounded the same way Chrome's own
//!   rendering effectively is: only bars shallow enough (`depth <=
//!   LABEL_MAX_DEPTH`) *and* wide enough (`>= LABEL_MIN_WIDTH_PX`) get one,
//!   which naturally excludes the sawtooth mass regardless of how many
//!   total bars a lane has.
//! - **A left-edge lane gutter.** A narrow fixed column showing a small
//!   icon per lane (CPU / background thread / GPU, via [`lane_icon`]),
//!   matching the reference's row-type gutter. Bar screen-space math
//!   (`bar_screen_rect`) already assumes a `chart_width` that starts at
//!   x=0 for the bar area, so the gutter is carved out of the *measured*
//!   container width once, up front (`chart_width = measured - GUTTER_WIDTH`),
//!   rather than threaded through every downstream calculation.
//! - **A ruler + gridlines that share the bars' own coordinate space.** A
//!   thin time-ruler sits above the lane stack (fixed -- it never scrolls,
//!   see below) and faint vertical gridlines run down through every lane,
//!   scrolling with them -- the reference's "one shared coordinate space"
//!   feel called out for this view, along the time axis. Both are built
//!   from [`pick_tick_interval_ns`]/[`ruler_ticks`] over the exact same
//!   `visible_start_ns`/`visible_span_ns`/`chart_width` the bars themselves
//!   use, so a gridline is never off by even a pixel from the bar edges
//!   it's meant to line up with.
//! - **Vertical scrolling, with real virtualization underneath it.** A
//!   deeply-recursive capture can produce a lane hundreds of rows tall --
//!   the lane stack sits in its own scrollable region (below the fixed
//!   ruler) so you can actually reach the bottom of one, and, more
//!   importantly, this file never *builds* GPU instances, hit-test bars, or
//!   labels for a depth row that isn't within (or near) the current
//!   viewport in the first place. `render` tracks each lane's vertical
//!   position as it lays the stack out; a lane entirely outside the
//!   viewport (± [`VIRTUALIZATION_OVERSCAN_ROWS`] rows of slack) contributes
//!   only an empty, correctly-sized spacer -- no `wgpu_surface`, no per-bar
//!   loop at all -- and a lane that's *partially* visible only processes the
//!   depth rows inside that window, not its full height. The per-lane
//!   `wgpu_surface`'s own div is sized/positioned to just that visible
//!   window rather than the lane's full height, so the GPU texture itself
//!   shrinks along with the CPU-side work. This is the actual fix for "the
//!   flame chart is slow" at real stack depths -- the merge step
//!   ([`merge_tiny_adjacent_bars`]) bounds work *along* the time axis, this
//!   bounds it *along* the depth axis, and together the per-render cost
//!   stops scaling with total stack size and starts scaling with viewport
//!   size instead.
//!
//! # Known gaps vs. the reference
//!
//! - The long-task hatch is a staircase approximation, not a literal
//!   45-degree stripe (see above) -- correct at the scale it renders, but
//!   not pixel-identical to Chrome's.
//! - No call-tree-aware label text (`Task`/`Layout`/`Commit` are Chrome's
//!   *own* fixed vocabulary for specific browser lifecycle phases); labels
//!   here use each bar's actual span name, which is this profiler's
//!   equivalent information but won't literally read "Layout".
//! - The scroll-wheel zoom-anchor and right-drag-pan fraction math (both
//!   copied from `ProfilerPanel::render_flame_lanes_body` essentially
//!   verbatim, see [`render`]) measures its denominator against the whole
//!   container width, gutter included -- a few pixels of anchor drift if
//!   the gesture starts directly over the gutter, not worth threading
//!   `GUTTER_WIDTH` through shared math for.

use std::rc::Rc;

use gpui::{
    canvas, div, hsla, prelude::FluentBuilder as _, px, wgpu_surface, AnyElement, AppContext as _,
    Bounds, ClickEvent, Context, DragMoveEvent, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement as _, Styled, Window,
};

use crate::{
    alert::Alert,
    h_flex,
    profiler::{
        bar_screen_rect, category_color, contains_ignore_ascii_case, flame_bar_tooltip,
        gpu_pass_color, ns_to_x, zoom_factor_for_wheel_delta, BarInstance, FlameBar,
        FlameBarPipeline, FlameLane, FlameLaneGpu, HitBar, ProfilerPanDrag, ProfilerPanel,
        FLAME_ROW_HEIGHT,
    },
    v_flex, ActiveTheme, Icon, IconName, Sizable as _,
};

use super::data::{self, LONG_TASK_NS};

/// All state this file owns directly. Empty: pan/zoom (`flame_zoom`), hover/
/// selection (`hovered_bar`/`selected_span`), search (`flame_search`), and
/// the per-lane GPU surfaces (`flame_lane_gpu`/`flame_bar_pipeline`) all stay
/// on `ProfilerPanel` itself rather than forking a second copy here --
/// they're already correct and, per `ProfilerPanel::render`, safe to share
/// with the plain Flame Chart tab since the two never render at once. If a
/// future feature genuinely needs Record-only flame-chart state (e.g. a
/// label-density toggle), it belongs here.
#[derive(Default)]
pub(crate) struct FlameState {
    /// Tracks the lane stack's scroll position (see [`render`]'s "Vertical
    /// virtualization" section) — the other half of the visible-band
    /// computation alongside `ProfilerPanel::flame_chart_bounds`, which
    /// already measures the surrounding viewport's height for the
    /// scroll-wheel-zoom-anchor math this file already had.
    scroll_handle: gpui::ScrollHandle,
    /// GPU surface for the ruler gridlines — a single surface spanning the
    /// *whole* lane stack (unlike the per-lane bar surfaces, which are each
    /// windowed to their own visible depth range), since a gridline is one
    /// continuous vertical run across every lane rather than something that
    /// makes sense to split per lane. Reuses `ProfilerPanel::flame_bar_pipeline`
    /// (the same compiled shader every bar surface in this profiler already
    /// shares) rather than compiling a second copy of it.
    gridline_gpu: Option<FlameLaneGpu>,
    /// Latches once creating [`Self::gridline_gpu`]'s surface ever fails, so
    /// the gridlines just silently stop appearing rather than retrying (and
    /// failing) every render — same pattern `ProfilerPanel::flame_gpu_unavailable`
    /// already uses for the bar surfaces, kept separate from that flag since
    /// a gridline-surface failure shouldn't disable the bars themselves.
    gridline_gpu_unavailable: bool,
}

/// Width of the left-edge lane-type gutter (§3's icon column). Subtracted
/// from the measured container width once, up front, so every downstream
/// bar/ruler/gridline x-coordinate is computed in one consistent "bar area
/// starts at 0" space -- see this file's module doc.
const GUTTER_WIDTH: f32 = 22.0;

/// Height of the time ruler row above the lane stack.
const RULER_HEIGHT: f32 = 18.0;

/// Approximates one lane's "`{label} ({n} spans)`" caption row height, for
/// sizing the gridline overlay's total height only -- not used for any
/// hit-testing math, same caveat `render_flame_lanes_body`'s own analogous
/// constant carries.
const LANE_LABEL_HEIGHT: f32 = 22.0;

/// Approximates the `v_flex().gap_1()` spacing between lane blocks, for the
/// same total-height estimate as [`LANE_LABEL_HEIGHT`].
const LANE_GAP: f32 = 4.0;

/// Extra depth rows built beyond the strictly-visible window on each side,
/// so a small scroll delta never has to wait a frame for freshly-visible
/// rows to appear — see [`render`]'s "Vertical virtualization" section.
const VIRTUALIZATION_OVERSCAN_ROWS: u16 = 6;

pub(crate) fn render(
    panel: &mut ProfilerPanel,
    lanes: &[FlameLane],
    window: &mut Window,
    cx: &mut Context<ProfilerPanel>,
) -> AnyElement {
    // `record::recompute_for_selection` already called `flame_zoom.set_domain`
    // for this exact selection before `render` was invoked, so this just
    // reads the (possibly zoomed/panned) visible window back out -- same
    // ordering `render_flame_lanes_body` relies on for the plain tab.
    let visible_start_ns = panel.flame_zoom.visible_start();
    let visible_end_ns = panel.flame_zoom.visible_end();
    let visible_span_ns = panel.flame_zoom.visible_span();
    let domain_start_ns = panel
        .record
        .overview_data()
        .map(|o| o.domain_start_ns as f64)
        .unwrap_or(visible_start_ns);

    const DEFAULT_CHART_WIDTH: f32 = 900.0;
    // Same one-frame-of-lag tradeoff `render_flame_lanes_body` documents:
    // the canvas below measures real width only after the first paint.
    let measured_width = f32::from(panel.flame_chart_bounds.size.width);
    let full_width = if measured_width > 1.0 {
        measured_width
    } else {
        DEFAULT_CHART_WIDTH
    };
    let chart_width = (full_width - GUTTER_WIDTH).max(1.0);

    let search = panel.flame_search.to_lowercase();
    let selected_key = panel
        .selected_span
        .as_ref()
        .map(|s| (s.name.clone(), s.depth, s.duration_ns));

    panel.flame_lane_gpu.truncate(lanes.len());
    panel
        .flame_lane_bounds
        .resize(lanes.len(), Bounds::default());

    let gpu_available = !panel.flame_gpu_unavailable;
    let scale = window.scale_factor();

    let mut lane_elements: Vec<AnyElement> = Vec::new();
    let mut total_lanes_height: f32 = 0.0;

    // Vertical virtualization (see this file's module doc): the visible
    // content-space band, in the lane stack's own local coordinates (0 =
    // top of the first lane's caption), plus a row-based overscan margin so
    // a small scroll delta never has to wait a frame for newly-visible rows
    // to appear. `flame_chart_bounds` measures the *whole*
    // `#flame-chart-canvas` container (fixed ruler band included), so its
    // height minus the ruler's own fixed height is the scrollable
    // viewport's actual height.
    let scroll_offset_y = f32::from(panel.record.flame.scroll_handle.offset().y);
    let viewport_height = (f32::from(panel.flame_chart_bounds.size.height) - RULER_HEIGHT).max(0.0);
    let overscan_px = VIRTUALIZATION_OVERSCAN_ROWS as f32 * FLAME_ROW_HEIGHT;
    let viewport_top = -scroll_offset_y - overscan_px;
    let viewport_bottom = -scroll_offset_y + viewport_height + overscan_px;

    for (lane_index, lane) in lanes.iter().enumerate() {
        lane_elements.push(
            div()
                .id(SharedString::from(format!("flame-lane-caption-{lane_index}")))
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .mt(px(6.))
                .child(format!("{} ({} spans)", lane.label, lane.bars.len()))
                .into_any_element(),
        );
        total_lanes_height += LANE_LABEL_HEIGHT;

        let lane_height = (lane.max_depth as f32 + 1.0) * FLAME_ROW_HEIGHT;
        // `total_lanes_height` at this exact point already includes this
        // lane's own caption but not yet its body -- exactly the body's
        // content-space top.
        let lane_body_top = total_lanes_height;
        let lane_body_bottom = lane_body_top + lane_height;
        total_lanes_height += lane_height + LANE_GAP;

        if !lane_intersects_viewport(lane_body_top, lane_body_bottom, viewport_top, viewport_bottom) {
            // Entirely outside the viewport (± overscan): a correctly-sized
            // empty spacer keeps scroll extent and every later lane's own
            // position correct, with none of the per-bar work below.
            lane_elements.push(
                div()
                    .id(SharedString::from(format!("flame-lane-spacer-{lane_index}")))
                    .w(px(chart_width + GUTTER_WIDTH))
                    .h(px(lane_height))
                    .into_any_element(),
            );
            continue;
        }

        // The depth-row window actually worth building: this lane's own
        // local row range intersecting the viewport (± overscan), clamped
        // to the rows that actually exist. A lane that fits entirely inside
        // the viewport gets `depth_start == 0`/`depth_end == max_depth`
        // here, i.e. no windowing overhead beyond this computation itself.
        let (depth_start, depth_end) =
            visible_depth_window(viewport_top, viewport_bottom, lane_body_top, lane.max_depth);
        let window_top_px = depth_start as f32 * FLAME_ROW_HEIGHT;
        let window_height_px = (depth_end - depth_start + 1) as f32 * FLAME_ROW_HEIGHT;

        let mut instances: Vec<BarInstance> = if gpu_available {
            Vec::with_capacity(lane.bars.len())
        } else {
            Vec::new()
        };
        let mut hit_bars: Vec<HitBar> = Vec::with_capacity(lane.bars.len());
        let mut label_elements: Vec<AnyElement> = Vec::new();

        // Collapse tiny, tightly-packed same-depth bars into fewer,
        // synthesized ones before this loop ever sees them -- see
        // `merge_tiny_adjacent_bars`'s own doc comment. Everything below
        // this line is unaware merging happened at all; it just sees fewer,
        // wider `FlameBar`s.
        let merged_bars =
            merge_tiny_adjacent_bars(&lane.bars, visible_start_ns, visible_span_ns, chart_width);

        for bar in &merged_bars {
            // The other half of virtualization, alongside the whole-lane
            // skip above: a bar whose row falls outside this lane's
            // currently-relevant depth window never gets an instance, a
            // `HitBar`, or a label element built for it at all.
            if bar.depth < depth_start || bar.depth > depth_end {
                continue;
            }
            let bar_start_ns = bar.start_ns as f64;
            let bar_end_ns = bar_start_ns + bar.duration_ns as f64;
            // Cull spans outside the zoomed/panned window, same as
            // `render_flame_lanes_body` -- keeps a deeply-zoomed-in view
            // from building thousands of off-screen instances/hit-bars.
            if bar_end_ns < visible_start_ns || bar_start_ns > visible_end_ns {
                continue;
            }
            let (x, width) = bar_screen_rect(bar, visible_start_ns, visible_span_ns, chart_width);
            // `lane_top` is this bar's position within the *lane's own*
            // full-height `bar_area` -- what hit-testing and labels use,
            // since both are plain GPUI elements laid out against that
            // whole area. `window_top` is the same position re-based to the
            // *windowed* `wgpu_surface`'s own smaller texture -- since that
            // surface is itself positioned at `window_top_px` within
            // `bar_area` (see below), `window_top_px + window_top` always
            // equals `lane_top`, so the two coordinate spaces agree on
            // screen even though only one of them is what the GPU instance
            // buffer is built in.
            let lane_top = bar.depth as f32 * FLAME_ROW_HEIGHT;
            let window_top = lane_top - window_top_px;
            let long_task = is_long_task(bar);
            let base_color = bar_fill_color(bar, cx);

            if gpu_available {
                let matches_search =
                    search.is_empty() || contains_ignore_ascii_case(&bar.label, &search);
                let is_selected = selected_key.as_ref().is_some_and(|(name, depth, dur)| {
                    *name == bar.label && *depth == bar.depth && *dur == bar.duration_ns
                });
                let is_hovered = panel.hovered_bar.as_ref().is_some_and(|h| {
                    h.lane_index == lane_index && h.start_ns == bar.start_ns && h.depth == bar.depth
                });

                let mut rgba = base_color.to_rgb();
                if !matches_search {
                    rgba.a *= 0.25;
                }

                instances.push(BarInstance {
                    rect_min: [x * scale, window_top * scale],
                    rect_max: [
                        (x + width) * scale,
                        (window_top + (FLAME_ROW_HEIGHT - 2.0)) * scale,
                    ],
                    color: [rgba.r, rgba.g, rgba.b, rgba.a],
                    corner_radius: 3.0 * scale,
                    highlight: if is_selected || is_hovered { 1.0 } else { 0.0 },
                    _pad: [0.0, 0.0],
                });

                // Drawn *after* the bar it belongs to, in the same
                // instance buffer -- see this file's module doc for why
                // that ordering is what makes the hatch composite on top.
                if long_task && matches_search {
                    let danger = cx.theme().danger.to_rgb();
                    for (rx0, ry0, rx1, ry1) in long_task_hatch_rects(x, width, window_top) {
                        instances.push(BarInstance {
                            rect_min: [rx0 * scale, ry0 * scale],
                            rect_max: [rx1 * scale, ry1 * scale],
                            color: [danger.r, danger.g, danger.b, 0.55],
                            corner_radius: 0.0,
                            highlight: 0.0,
                            _pad: [0.0, 0.0],
                        });
                    }
                }
            }

            hit_bars.push(HitBar {
                bar: bar.clone(),
                x,
                width,
                top: lane_top,
            });

            if should_label_bar(bar.depth, width) {
                let text_color = contrasting_label_color(base_color);
                label_elements.push(bar_label_element(
                    bar.label.clone(),
                    x,
                    lane_top,
                    width,
                    text_color,
                ));
            }
        }

        let surface_handle = panel.paint_flame_lane(lane_index, window, &instances);

        let mut bar_area = div()
            .id(SharedString::from(format!("flame-lane-{lane_index}")))
            .relative()
            .w(px(chart_width))
            .h(px(lane_height));

        if let Some(handle) = surface_handle {
            // Positioned/sized to just the visible depth window
            // (`window_top_px`/`window_height_px`), not `inset_0()` across
            // the whole (potentially much taller) `bar_area` -- the other
            // half of vertical virtualization: the GPU texture itself
            // shrinks along with the CPU-side instance count, rather than
            // always being allocated at the lane's full height. Deferred-
            // resize, same rationale as `render_flame_lanes_body`: without
            // it, every sidebar/window resize (and now every scroll tick)
            // would force a real GPU texture reallocation on top of the
            // instance rebuild this loop already does.
            bar_area = bar_area.child(
                wgpu_surface(handle)
                    .absolute()
                    .top(px(window_top_px))
                    .left(px(0.))
                    .w(px(chart_width))
                    .h(px(window_height_px))
                    .defer_resize_until_mouse_up(true),
            );
        }

        let entity = cx.entity().clone();
        bar_area = bar_area.child(
            canvas(
                {
                    let entity = entity.clone();
                    move |bounds, _window, cx| {
                        entity.update(cx, |state, _cx| {
                            if let Some(slot) = state.flame_lane_bounds.get_mut(lane_index) {
                                *slot = bounds;
                            }
                        });
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        );

        // One hit-test overlay per lane, not one interactive element per
        // bar -- see this file's module doc and `profiler/mod.rs`'s "Flame
        // chart GPU rendering" section for why that distinction matters at
        // real span counts.
        let hit_bars_for_move = Rc::new(hit_bars);
        let hit_bars_for_click = hit_bars_for_move.clone();
        let lane_label_for_click = lane.label.clone();
        bar_area = bar_area.child(
            div()
                .id(SharedString::from(format!("flame-lane-hit-{lane_index}")))
                .absolute()
                .inset_0()
                .cursor_pointer()
                .on_mouse_move(cx.listener(move |state, event: &MouseMoveEvent, _window, cx| {
                    state.update_flame_hover(lane_index, &hit_bars_for_move, event.position, cx);
                }))
                .on_click(cx.listener(move |state, event: &ClickEvent, _window, cx| {
                    state.select_flame_bar_at(
                        lane_index,
                        &hit_bars_for_click,
                        lane_label_for_click.clone(),
                        event.position(),
                        cx,
                    );
                })),
        );

        bar_area = bar_area.children(label_elements);

        if let Some(hover) = panel
            .hovered_bar
            .as_ref()
            .filter(|h| h.lane_index == lane_index)
        {
            bar_area = bar_area.child(flame_bar_tooltip(hover, cx));
        }

        let row = h_flex()
            .id(SharedString::from(format!("flame-lane-row-{lane_index}")))
            .items_start()
            .child(lane_gutter(lane, lane_height, cx))
            .child(bar_area);

        lane_elements.push(row.into_any_element());
    }

    if panel.flame_gpu_unavailable {
        lane_elements.insert(
            0,
            Alert::warning(
                "flame-gpu-unavailable",
                "GPU-accelerated flame bars are unavailable on this platform/build; lane rows \
                 still lay out and remain interactive, but bars aren't drawn.",
            )
            .into_any_element(),
        );
    }

    // Ruler ticks + gridlines share the exact `visible_start_ns`/
    // `visible_span_ns`/`chart_width` the bars above were built from, so a
    // gridline can never disagree with a bar edge -- see this file's module
    // doc.
    let tick_interval_ns = pick_tick_interval_ns(visible_span_ns, chart_width);
    let ticks = ruler_ticks(visible_start_ns, visible_end_ns, tick_interval_ns);

    // GPU-instanced now (see `paint_gridlines_gpu`), not one `div()` per
    // tick -- "everything that isn't text/a popup renders via the shader"
    // applies here the same as the bars, the overview strip's own CPU
    // graph/Frames row/selection, and everything else in this profiler's
    // Record tab. Only the tick *labels* (`ruler_children`, real text)
    // stay as UI-framework elements.
    let mut ruler_children: Vec<AnyElement> = Vec::new();
    let mut gridline_instances: Vec<BarInstance> = Vec::new();
    if !lanes.is_empty() {
        for (tick_index, tick_ns) in ticks.iter().enumerate() {
            let Some(x) = ns_to_x(tick_ns.max(0.0) as u64, visible_start_ns, visible_span_ns, chart_width)
            else {
                continue;
            };
            ruler_children.push(
                div()
                    .id(SharedString::from(format!("flame-ruler-tick-{tick_index}")))
                    .absolute()
                    .left(px(GUTTER_WIDTH + x + 3.0))
                    .top(px(2.0))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format_tick_label(*tick_ns, domain_start_ns, tick_interval_ns))
                    .into_any_element(),
            );
            let gridline_x = GUTTER_WIDTH + x;
            let rgba = cx.theme().border.opacity(0.35).to_rgb();
            gridline_instances.push(BarInstance {
                rect_min: [gridline_x * scale, 0.0],
                rect_max: [(gridline_x + 1.0) * scale, total_lanes_height * scale],
                color: [rgba.r, rgba.g, rgba.b, rgba.a],
                corner_radius: 0.0,
                highlight: 0.0,
                _pad: [0.0, 0.0],
            });
        }
    }

    let gridline_row_width = GUTTER_WIDTH + chart_width;
    let gridline_surface = if !gridline_instances.is_empty() {
        paint_gridlines_gpu(
            &mut panel.record.flame,
            &mut panel.flame_bar_pipeline,
            window,
            &gridline_instances,
        )
    } else {
        None
    };

    let entity = cx.entity().clone();
    let scroll_handle = panel.record.flame.scroll_handle.clone();

    div()
        .id("flame-chart-canvas")
        .relative()
        .overflow_hidden()
        .w_full()
        .h_full()
        .child(
            // Zero-footprint overlay capturing this container's screen
            // bounds each render, so the scroll-wheel handler below can
            // turn a cursor position into a time-axis zoom anchor.
            canvas(
                {
                    let entity = entity.clone();
                    move |bounds, _window, cx| {
                        entity.update(cx, |state, _cx| state.flame_chart_bounds = bounds);
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
        .on_scroll_wheel(cx.listener(|state, event: &ScrollWheelEvent, window, cx| {
            if !(event.modifiers.control || event.modifiers.platform) {
                return;
            }
            cx.stop_propagation();
            let width = f32::from(state.flame_chart_bounds.size.width);
            if width <= 1.0 {
                return;
            }
            let cursor_fraction = ((f32::from(event.position.x)
                - f32::from(state.flame_chart_bounds.origin.x))
                / width)
                .clamp(0.0, 1.0);
            let delta_y = f32::from(event.delta.pixel_delta(window.line_height()).y);
            state
                .flame_zoom
                .zoom_at(cursor_fraction, zoom_factor_for_wheel_delta(delta_y));
            cx.notify();
        }))
        .on_click(cx.listener(|state, event: &ClickEvent, _window, cx| {
            if event.click_count() >= 2 {
                state.flame_zoom.reset();
                cx.notify();
            }
        }))
        .on_drag(ProfilerPanDrag, {
            let entity = entity.clone();
            move |_, _start_position, _window, cx| {
                entity.update(cx, |state, _cx| state.flame_pan_last_x = None);
                cx.new(|_| ProfilerPanDrag)
            }
        })
        .on_drag_move(cx.listener(
            |state, event: &DragMoveEvent<ProfilerPanDrag>, _window, cx| {
                let width = f32::from(event.bounds.size.width).max(1.0);
                let x = f32::from(event.event.position.x);
                if let Some(last_x) = state.flame_pan_last_x {
                    state.flame_zoom.pan_by_fraction(-(x - last_x) / width);
                }
                state.flame_pan_last_x = Some(x);
                cx.notify();
            },
        ))
        // Right-click+drag pans the time axis too -- the per-lane hit-test
        // overlays use left-click for select-a-span, so right-drag is the
        // conflict-free complement for panning without a modifier key.
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(|state, _event: &MouseDownEvent, _window, _cx| {
                state.flame_pan_last_x = None;
            }),
        )
        .on_mouse_move(cx.listener(|state, event: &MouseMoveEvent, _window, cx| {
            if event.pressed_button != Some(MouseButton::Right) {
                return;
            }
            let width = f32::from(state.flame_chart_bounds.size.width).max(1.0);
            let x = f32::from(event.position.x);
            if let Some(last_x) = state.flame_pan_last_x {
                state.flame_zoom.pan_by_fraction(-(x - last_x) / width);
                cx.notify();
            }
            state.flame_pan_last_x = Some(x);
        }))
        .on_mouse_up(
            MouseButton::Right,
            cx.listener(|state, _event: &MouseUpEvent, _window, cx| {
                state.flame_pan_last_x = None;
            }),
        )
        .child(
            div()
                .id("flame-ruler-band")
                .h(px(RULER_HEIGHT))
                .w_full()
                .flex_shrink_0()
                .border_b_1()
                .border_color(cx.theme().border),
        )
        // The scrollable lane stack, separated from the fixed ruler band
        // above it -- see this file's module doc's "Vertical scrolling"
        // section. Explicit height (the same `viewport_height` the
        // virtualization math above already computed) rather than a flex
        // fill, since `#flame-chart-canvas` is a plain block container the
        // way every other top-level `div()` in this file already is, not a
        // flex column.
        .child(
            div()
                .id("flame-chart-scroll-area")
                .relative()
                .w_full()
                .h(px(viewport_height))
                .overflow_y_scroll()
                .track_scroll(&scroll_handle)
                .child(
                    v_flex()
                        .id("flame-chart-lanes")
                        .gap_1()
                        .children(lane_elements),
                )
                .when_some(gridline_surface, |el, handle| {
                    el.child(
                        div()
                            .absolute()
                            .top(px(0.))
                            .left(px(0.))
                            .w(px(gridline_row_width))
                            .h(px(total_lanes_height))
                            .child(
                                // NOT `.defer_resize_until_mouse_up(true)`:
                                // this is a single shared surface (like the
                                // overview strip's), not the many per-lane
                                // ones that flag exists for -- deferring its
                                // resize while a pane-resize drag is in
                                // progress (also a left-button drag) would
                                // reproduce the exact "content briefly goes
                                // stale relative to the box" bug already
                                // fixed for the overview strip's own shared
                                // surface. See that fix's commit message for
                                // the full mechanism.
                                gpui::wgpu_surface(handle).absolute().inset_0(),
                            ),
                    )
                }),
        )
        .children(ruler_children)
        .into_any_element()
}

/// Paints the ruler-gridline instances into [`FlameState::gridline_gpu`]'s
/// single shared surface, creating it (and, if needed, the shared
/// `panel_pipeline`) on first use — same lazy-init/render/swap shape
/// [`ProfilerPanel::paint_flame_lane`] already uses per bar lane, just
/// against this file's own dedicated surface instead of one indexed by
/// lane. Takes `panel_pipeline` by `&mut` rather than reading it off
/// `panel` directly so this function doesn't need a `&mut ProfilerPanel`
/// just to read/lazily-fill one of its fields.
fn paint_gridlines_gpu(
    state: &mut FlameState,
    panel_pipeline: &mut Option<Rc<FlameBarPipeline>>,
    window: &Window,
    instances: &[BarInstance],
) -> Option<gpui::WgpuSurfaceHandle> {
    if state.gridline_gpu_unavailable {
        return None;
    }
    if state.gridline_gpu.is_none() {
        let Some(surface) = window.create_wgpu_surface(1, 1, wgpu::TextureFormat::Rgba8UnormSrgb)
        else {
            state.gridline_gpu_unavailable = true;
            return None;
        };
        let device = surface.device().clone();
        if panel_pipeline.is_none() {
            *panel_pipeline = Some(Rc::new(FlameBarPipeline::new(&device, surface.format())));
        }
        let pipeline = panel_pipeline.clone().unwrap();
        state.gridline_gpu = Some(FlameLaneGpu::new(&device, surface, &pipeline));
    }
    let pipeline = panel_pipeline.clone()?;
    let gpu = state.gridline_gpu.as_mut()?;
    let handle = gpu.surface.clone();
    let Some((view, (width, height))) = handle.back_view_with_size() else {
        return Some(handle);
    };
    gpu.render(&pipeline, instances, &view, width, height);
    drop(view);
    handle.swap_buffers();
    Some(handle)
}

/// Whether a lane's content-space body range `[lane_body_top,
/// lane_body_bottom)` overlaps the currently visible band `[viewport_top,
/// viewport_bottom)` at all (the band already includes
/// [`VIRTUALIZATION_OVERSCAN_ROWS`] of slack — see [`render`]) — a lane that
/// doesn't gets skipped entirely, per this file's module doc.
fn lane_intersects_viewport(
    lane_body_top: f32,
    lane_body_bottom: f32,
    viewport_top: f32,
    viewport_bottom: f32,
) -> bool {
    lane_body_bottom >= viewport_top && lane_body_top <= viewport_bottom
}

/// For a lane known to intersect the viewport (see
/// [`lane_intersects_viewport`]), the inclusive `[depth_start, depth_end]`
/// row range — in that lane's own local row coordinates — actually worth
/// building bars for, clamped to the rows that exist (`0..=max_depth`). A
/// lane that fits entirely inside the viewport resolves to
/// `(0, max_depth)`, i.e. every row, matching the pre-virtualization
/// behavior exactly for a lane short enough that windowing wouldn't help
/// anyway.
fn visible_depth_window(
    viewport_top: f32,
    viewport_bottom: f32,
    lane_body_top: f32,
    max_depth: u16,
) -> (u16, u16) {
    let depth_start = ((viewport_top - lane_body_top) / FLAME_ROW_HEIGHT)
        .floor()
        .max(0.0) as u16;
    let depth_end = (((viewport_bottom - lane_body_top) / FLAME_ROW_HEIGHT)
        .ceil()
        .max(0.0) as u16)
        .min(max_depth)
        .max(depth_start.min(max_depth));
    (depth_start.min(max_depth), depth_end)
}

/// A bar this narrow on screen is a merge *candidate*. Below this width you
/// genuinely can't distinguish it from its neighbors -- see
/// [`merge_tiny_adjacent_bars`].
const MERGE_MAX_BAR_WIDTH_PX: f32 = 2.0;

/// Two consecutive merge-candidate bars (same depth) merge if the gap
/// between them is at most this many pixels -- close enough that the gap
/// itself wouldn't register either. A bar that's already individually wide
/// enough to read never merges just because a tiny neighbor happens to sit
/// this close to it; only runs of bars that are *both* tiny stack up.
const MERGE_MAX_GAP_PX: f32 = 1.5;

/// Collapses runs of tiny, tightly-packed same-depth bars into single
/// synthesized `FlameBar`s -- this is exactly the reference's own "sawtooth"
/// texture (`.agents/PROFILER_UI_SPEC.md` §3: "a dense unlabeled sawtooth
/// texture of hundreds of tiny same-color bars ... rendered as texture, not
/// as individually hit-testable elements"), and the practical reason a
/// multi-frame Record selection with tens of thousands of leaf-level spans
/// doesn't build tens of thousands of GPU instances (plus that many
/// `HitBar`s) every single render.
///
/// Grouped by depth (merging across depths would visually overlap two
/// unrelated rows), then walked in `start_ns` order within each depth. A
/// merged bar's `label` becomes `"{n} spans"`, and its `category`/
/// `gpu_pass_kind` become whichever the run's *total duration* was mostly
/// spent in -- the same "dominant" principle `OverviewBucket::dominant_category`
/// already uses for the overview strip's stacked bars, reused here via
/// `data::category_index`/`data::OVERVIEW_CATEGORIES` so the two views can
/// never disagree about category ordering. Its `start_ns`/`duration_ns` span
/// the run's full extent, so `bar_screen_rect` (called again, completely
/// unmodified, by `render`'s own per-bar loop right after this returns)
/// reproduces exactly the merged rect this function measured while deciding
/// to merge -- no separate/divergent geometry math to keep in sync.
///
/// A bar that never became part of any merge (individually wide enough, or
/// too far from any other candidate) passes through byte-for-byte unchanged
/// via a plain clone -- this never *drops* a bar, only ever combines
/// adjacent tiny ones, so nothing goes missing from the chart, it just stops
/// being individually resolvable at this zoom level (exactly like the
/// reference).
fn merge_tiny_adjacent_bars(
    bars: &[FlameBar],
    visible_start_ns: f64,
    visible_span_ns: f64,
    chart_width: f32,
) -> Vec<FlameBar> {
    if bars.is_empty() {
        return Vec::new();
    }
    let max_depth = bars.iter().map(|b| b.depth).max().unwrap_or(0);
    let mut by_depth: Vec<Vec<&FlameBar>> = vec![Vec::new(); max_depth as usize + 1];
    for bar in bars {
        by_depth[bar.depth as usize].push(bar);
    }

    let mut merged: Vec<FlameBar> = Vec::with_capacity(bars.len());
    for depth_bars in &mut by_depth {
        depth_bars.sort_by_key(|b| b.start_ns);

        let mut run: Option<MergeRun> = None;
        for bar in depth_bars.iter().copied() {
            let (x, width) = bar_screen_rect(bar, visible_start_ns, visible_span_ns, chart_width);
            let bar_end_ns = bar.start_ns.saturating_add(bar.duration_ns as u64);

            if width > MERGE_MAX_BAR_WIDTH_PX {
                if let Some(r) = run.take() {
                    merged.push(r.into_bar());
                }
                merged.push(bar.clone());
                continue;
            }

            let extends_run = run.as_ref().is_some_and(|r| x - r.end_x <= MERGE_MAX_GAP_PX);
            if extends_run {
                run.as_mut().unwrap().extend(bar, x + width, bar_end_ns);
            } else {
                if let Some(r) = run.take() {
                    merged.push(r.into_bar());
                }
                run = Some(MergeRun::new(bar, x + width, bar_end_ns));
            }
        }
        if let Some(r) = run.take() {
            merged.push(r.into_bar());
        }
    }
    merged
}

/// [`merge_tiny_adjacent_bars`]'s in-progress merge-group accumulator.
/// `end_x` is screen-space bookkeeping used only to decide whether the
/// *next* candidate bar is close enough to extend this run -- it never
/// appears in the synthesized [`FlameBar`] this becomes, since that bar's
/// `start_ns`/`duration_ns` alone are enough for `bar_screen_rect` to
/// reproduce the same screen rect later.
struct MergeRun {
    first_bar: FlameBar,
    end_ns: u64,
    end_x: f32,
    count: u32,
    category_ns: [u64; data::OVERVIEW_CATEGORIES.len()],
}

impl MergeRun {
    fn new(bar: &FlameBar, end_x: f32, end_ns: u64) -> Self {
        let mut category_ns = [0u64; data::OVERVIEW_CATEGORIES.len()];
        if let Some(category) = bar.category {
            category_ns[data::category_index(category)] += bar.duration_ns as u64;
        }
        Self {
            first_bar: bar.clone(),
            end_ns,
            end_x,
            count: 1,
            category_ns,
        }
    }

    fn extend(&mut self, bar: &FlameBar, end_x: f32, end_ns: u64) {
        self.end_x = end_x;
        self.end_ns = self.end_ns.max(end_ns);
        self.count += 1;
        if let Some(category) = bar.category {
            self.category_ns[data::category_index(category)] += bar.duration_ns as u64;
        }
    }

    /// A run of exactly one bar (a lone candidate with no mergeable
    /// neighbor) resolves to that original bar completely unchanged -- only
    /// a run of 2+ actually gets synthesized into an aggregate.
    fn into_bar(self) -> FlameBar {
        if self.count <= 1 {
            return self.first_bar;
        }
        let dominant_category = self
            .category_ns
            .iter()
            .enumerate()
            .max_by_key(|(_, ns)| **ns)
            .filter(|(_, ns)| **ns > 0)
            .map(|(index, _)| data::OVERVIEW_CATEGORIES[index]);
        let duration_ns = self
            .end_ns
            .saturating_sub(self.first_bar.start_ns)
            .min(u32::MAX as u64) as u32;
        FlameBar {
            label: SharedString::from(format!("{} spans", self.count)),
            depth: self.first_bar.depth,
            start_ns: self.first_bar.start_ns,
            duration_ns,
            category: dominant_category.or(self.first_bar.category),
            gpu_pass_kind: if dominant_category.is_some() {
                None
            } else {
                self.first_bar.gpu_pass_kind
            },
            element_type: None,
            element_source: None,
        }
    }
}

/// The narrow left-edge column identifying a lane's row type, mirroring the
/// reference's icon gutter. Purely decorative (never hit-tested), so it's a
/// handful of plain, non-interactive elements -- one per lane, nothing like
/// the per-bar element count the GPU-instanced bars themselves specifically
/// avoid.
fn lane_gutter(lane: &FlameLane, height: f32, cx: &Context<ProfilerPanel>) -> AnyElement {
    div()
        .id(SharedString::from(format!("flame-lane-gutter-{}", lane.label)))
        .w(px(GUTTER_WIDTH))
        .h(px(height))
        .flex_shrink_0()
        .flex()
        .items_start()
        .justify_center()
        .pt(px(3.))
        .border_r_1()
        .border_color(cx.theme().border)
        .child(
            Icon::new(lane_icon(&lane.label))
                .xsmall()
                .text_color(cx.theme().muted_foreground),
        )
        .into_any_element()
}

/// Picks a gutter icon from a lane's label -- see `data::build_flame_lanes_for_range`
/// for the exact label strings this matches against ("Main Thread (CPU)",
/// "Background Thread N", "GPU").
fn lane_icon(label: &str) -> IconName {
    if label.starts_with("GPU") {
        IconName::ElectronicsChip
    } else if label.starts_with("Background") {
        IconName::Server
    } else {
        IconName::Cpu
    }
}

/// A depth-0 bar is this profiler's equivalent of Chrome's per-frame `Task`
/// entry (see this file's module doc); one running at or beyond
/// [`LONG_TASK_NS`] gets the red hatch. Only depth 0 is eligible -- deeper
/// bars are real named work, not the coarse per-frame unit Chrome's own
/// long-task warning targets.
fn is_long_task(bar: &FlameBar) -> bool {
    bar.depth == 0 && bar.duration_ns as u64 >= LONG_TASK_NS
}

/// Depth 0 renders as a flat neutral "Task" block regardless of category,
/// same as Chrome's own Task row -- see this file's module doc. Every bar
/// below it keeps the existing category/GPU-pass coloring
/// (`category_color`/`gpu_pass_color`), shared verbatim with the plain
/// Flame Chart tab.
fn bar_fill_color(bar: &FlameBar, cx: &Context<ProfilerPanel>) -> Hsla {
    if bar.depth == 0 {
        cx.theme().secondary
    } else {
        match (bar.category, bar.gpu_pass_kind) {
            (Some(cat), _) => category_color(cat, cx),
            (None, Some(kind)) => gpu_pass_color(kind, cx),
            _ => cx.theme().chart_1,
        }
    }
}

/// Picks readable label text over a given fill color -- near-black on a
/// light fill, near-white on a dark one. Simple lightness threshold rather
/// than a full contrast-ratio computation: flame bar fills are themselves
/// simple, roughly-saturated theme colors, not photographic content, so this
/// doesn't need to be more precise than that to stay legible.
fn contrasting_label_color(fill: Hsla) -> Hsla {
    if fill.l > 0.6 {
        hsla(0.0, 0.0, 0.08, 0.92)
    } else {
        hsla(0.0, 0.0, 0.98, 0.92)
    }
}

/// Only wide-enough, shallow bars get an in-place text label -- see this
/// file's module doc for why bounding this by depth (not just width) is
/// what keeps the label element count small regardless of how many total
/// bars a lane has.
const LABEL_MAX_DEPTH: u16 = 2;
const LABEL_MIN_WIDTH_PX: f32 = 28.0;

fn should_label_bar(depth: u16, width_px: f32) -> bool {
    depth <= LABEL_MAX_DEPTH && width_px >= LABEL_MIN_WIDTH_PX
}

fn bar_label_element(label: SharedString, x: f32, top: f32, width: f32, text_color: Hsla) -> AnyElement {
    // Positioned relative to the lane's own bar-area div (`bar_area` in
    // `render`), which already starts at x=0 for the bar area -- unlike the
    // ruler ticks/gridlines below, which are children of the *outer*
    // container and do need the `GUTTER_WIDTH` offset added back in.
    div()
        .absolute()
        .left(px(x + 4.0))
        .top(px(top + 2.0))
        .w(px((width - 6.0).max(0.0)))
        .h(px((FLAME_ROW_HEIGHT - 4.0).max(0.0)))
        .overflow_hidden()
        .truncate()
        .text_xs()
        .text_color(text_color)
        .child(label)
        .into_any_element()
}

/// Diagonal-hatch geometry for a long-task block's warning band, in logical
/// (unscaled) pixels in the same "bar area starts at x=0" space
/// `bar_screen_rect` uses -- callers translate to screen space and turn each
/// rect into a `BarInstance` themselves. See this file's module doc for why
/// this is a staircase approximation of a true 45-degree stripe rather than
/// a literal one.
const HATCH_BAND_HEIGHT: f32 = 5.0;
const HATCH_PERIOD_PX: f32 = 6.0;
const HATCH_DASH_PX: f32 = 1.6;

fn long_task_hatch_rects(x: f32, width: f32, top: f32) -> Vec<(f32, f32, f32, f32)> {
    if width <= 0.0 {
        return Vec::new();
    }
    let steps = (HATCH_BAND_HEIGHT / HATCH_DASH_PX).ceil() as i32;
    let mut rects = Vec::new();
    let mut period_start = 0.0f32;
    // Defensive cap: guards against a pathologically large `width` (an
    // extreme zoom-out on a very long task) turning this into an unbounded
    // loop -- the hatch is cosmetic, so silently truncating it past a few
    // thousand periods is preferable to stalling a render.
    for _ in 0..4000 {
        if period_start >= width {
            break;
        }
        for step in 0..steps {
            let dx = period_start + step as f32 * HATCH_DASH_PX;
            if dx >= width {
                break;
            }
            let rx0 = x + dx;
            let rx1 = (rx0 + HATCH_DASH_PX).min(x + width);
            let ry0 = top + step as f32 * HATCH_DASH_PX;
            let ry1 = (ry0 + HATCH_DASH_PX).min(top + HATCH_BAND_HEIGHT);
            rects.push((rx0, ry0, rx1, ry1));
        }
        period_start += HATCH_PERIOD_PX;
    }
    rects
}

/// Picks a "nice" (1/2/5 × 10^n) tick spacing in nanoseconds so the ruler's
/// gridlines land on round millisecond-ish values instead of an arbitrary
/// fraction of the visible window -- the standard "nice number" approach
/// most charting libraries use for axis ticks.
const TARGET_TICK_PX: f32 = 90.0;

fn pick_tick_interval_ns(visible_span_ns: f64, chart_width: f32) -> f64 {
    let target_ticks = (chart_width / TARGET_TICK_PX).max(1.0) as f64;
    let raw_interval = (visible_span_ns / target_ticks).max(1.0);
    // The tiny epsilon nudge before `floor()` guards against `log10()` of an
    // exact power of ten landing a hair under its true integer value (e.g.
    // `7.999999999999998` instead of `8.0`) on some libm implementations,
    // which would otherwise pick a magnitude 10x too small.
    let magnitude = 10f64.powf((raw_interval.log10() + 1e-9).floor());
    let residual = raw_interval / magnitude;
    let nice = if residual < 1.5 {
        1.0
    } else if residual < 3.5 {
        2.0
    } else if residual < 7.5 {
        5.0
    } else {
        10.0
    };
    nice * magnitude
}

/// Every tick that falls inside `[visible_start_ns, visible_end_ns]`,
/// starting from the first multiple of `interval_ns` at or after
/// `visible_start_ns`.
fn ruler_ticks(visible_start_ns: f64, visible_end_ns: f64, interval_ns: f64) -> Vec<f64> {
    if interval_ns <= 0.0 || !interval_ns.is_finite() {
        return Vec::new();
    }
    let first = (visible_start_ns / interval_ns).ceil() * interval_ns;
    let mut ticks = Vec::new();
    let mut t = first;
    // Defensive cap, same rationale as `long_task_hatch_rects`'s.
    for _ in 0..2000 {
        if t > visible_end_ns {
            break;
        }
        ticks.push(t);
        t += interval_ns;
    }
    ticks
}

/// Formats a tick as milliseconds since the capture domain's own start
/// (`domain_start_ns`), not since the Unix epoch or the current zoom
/// window -- matching Chrome's ruler, whose labels stay anchored to
/// recording start regardless of how far you've zoomed/panned. Precision
/// scales with the tick spacing so zoomed-in views (sub-ms intervals) don't
/// round every label to the same integer millisecond.
fn format_tick_label(tick_ns: f64, domain_start_ns: f64, interval_ns: f64) -> String {
    let relative_ms = (tick_ns - domain_start_ns) / 1.0e6;
    if interval_ns < 1.0e6 {
        format!("{relative_ms:.2}ms")
    } else if interval_ns < 1.0e7 {
        format!("{relative_ms:.1}ms")
    } else {
        format!("{relative_ms:.0}ms")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::SpanCategory;

    fn sample_bar(depth: u16, duration_ns: u32) -> FlameBar {
        FlameBar {
            label: "test".into(),
            depth,
            start_ns: 0,
            duration_ns,
            category: Some(SpanCategory::ElementPaint),
            gpu_pass_kind: None,
            element_type: None,
            element_source: None,
        }
    }

    #[test]
    fn is_long_task_requires_depth_zero() {
        let deep = sample_bar(1, LONG_TASK_NS as u32 + 1);
        assert!(!is_long_task(&deep));
    }

    #[test]
    fn is_long_task_requires_the_duration_threshold() {
        let short = sample_bar(0, LONG_TASK_NS as u32 - 1);
        let long = sample_bar(0, LONG_TASK_NS as u32 + 1);
        assert!(!is_long_task(&short));
        assert!(is_long_task(&long));
    }

    #[test]
    fn should_label_bar_requires_both_shallow_and_wide() {
        assert!(should_label_bar(0, LABEL_MIN_WIDTH_PX));
        assert!(!should_label_bar(LABEL_MAX_DEPTH + 1, LABEL_MIN_WIDTH_PX));
        assert!(!should_label_bar(0, LABEL_MIN_WIDTH_PX - 1.0));
    }

    #[test]
    fn contrasting_label_color_picks_dark_text_on_light_fills() {
        let light = hsla(0.0, 0.0, 0.9, 1.0);
        let dark = hsla(0.0, 0.0, 0.1, 1.0);
        assert!(contrasting_label_color(light).l < 0.5);
        assert!(contrasting_label_color(dark).l > 0.5);
    }

    #[test]
    fn long_task_hatch_rects_stays_within_the_bar_bounds() {
        let rects = long_task_hatch_rects(10.0, 40.0, 100.0);
        assert!(!rects.is_empty());
        for (rx0, ry0, rx1, ry1) in &rects {
            assert!(*rx0 >= 10.0 - f32::EPSILON);
            assert!(*rx1 <= 10.0 + 40.0 + f32::EPSILON);
            assert!(*ry0 >= 100.0 - f32::EPSILON);
            assert!(*ry1 <= 100.0 + HATCH_BAND_HEIGHT + f32::EPSILON);
        }
    }

    #[test]
    fn long_task_hatch_rects_is_empty_for_a_zero_width_bar() {
        assert!(long_task_hatch_rects(0.0, 0.0, 0.0).is_empty());
    }

    #[test]
    fn pick_tick_interval_ns_lands_on_a_round_number() {
        // ~10 ticks across a 900px chart for a 1-second window should land
        // on a clean 100ms (1e8 ns) interval, not an arbitrary fraction.
        let interval = pick_tick_interval_ns(1.0e9, 900.0);
        assert_eq!(interval, 1.0e8);
    }

    #[test]
    fn ruler_ticks_starts_at_or_after_the_visible_window() {
        let ticks = ruler_ticks(1_250.0, 5_250.0, 1_000.0);
        assert_eq!(ticks, vec![2_000.0, 3_000.0, 4_000.0, 5_000.0]);
    }

    #[test]
    fn ruler_ticks_is_empty_for_a_non_positive_interval() {
        assert!(ruler_ticks(0.0, 1_000.0, 0.0).is_empty());
    }

    #[test]
    fn format_tick_label_is_relative_to_the_domain_start() {
        assert_eq!(format_tick_label(5_000_000.0, 1_000_000.0, 1.0e7), "4ms");
    }

    /// At `visible_span_ns = 100_000.0` / `chart_width = 100.0` (1px per
    /// 1000ns), a duration this small always renders at `bar_screen_rect`'s
    /// own 1.5px floor -- i.e. always a merge candidate by construction,
    /// regardless of the exact duration passed.
    fn tiny_bar(start_ns: u64, depth: u16, category: SpanCategory) -> FlameBar {
        FlameBar {
            label: "leaf".into(),
            depth,
            start_ns,
            duration_ns: 10,
            category: Some(category),
            gpu_pass_kind: None,
            element_type: None,
            element_source: None,
        }
    }

    const MERGE_TEST_SPAN_NS: f64 = 100_000.0;
    const MERGE_TEST_CHART_WIDTH: f32 = 100.0;

    #[test]
    fn merge_tiny_adjacent_bars_merges_close_tiny_bars_at_the_same_depth() {
        let bars = vec![
            tiny_bar(0, 0, SpanCategory::ElementPaint),
            tiny_bar(50, 0, SpanCategory::ElementPaint),
            tiny_bar(100, 0, SpanCategory::ElementPaint),
        ];
        let merged =
            merge_tiny_adjacent_bars(&bars, 0.0, MERGE_TEST_SPAN_NS, MERGE_TEST_CHART_WIDTH);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].label.as_ref(), "3 spans");
    }

    #[test]
    fn merge_tiny_adjacent_bars_keeps_a_lone_bar_unchanged() {
        let bars = vec![tiny_bar(0, 0, SpanCategory::ElementPaint)];
        let merged =
            merge_tiny_adjacent_bars(&bars, 0.0, MERGE_TEST_SPAN_NS, MERGE_TEST_CHART_WIDTH);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].label.as_ref(), "leaf");
    }

    #[test]
    fn merge_tiny_adjacent_bars_does_not_merge_across_a_wide_gap() {
        let bars = vec![
            tiny_bar(0, 0, SpanCategory::ElementPaint),
            // 50px away at this zoom -- well past `MERGE_MAX_GAP_PX`.
            tiny_bar(50_000, 0, SpanCategory::ElementPaint),
        ];
        let merged =
            merge_tiny_adjacent_bars(&bars, 0.0, MERGE_TEST_SPAN_NS, MERGE_TEST_CHART_WIDTH);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merge_tiny_adjacent_bars_never_merges_a_bar_wide_enough_to_read() {
        let bars = vec![
            FlameBar {
                label: "big".into(),
                depth: 0,
                start_ns: 0,
                // 20px at this zoom -- well over `MERGE_MAX_BAR_WIDTH_PX`.
                duration_ns: 20_000,
                category: Some(SpanCategory::ElementPaint),
                gpu_pass_kind: None,
                element_type: None,
                element_source: None,
            },
            tiny_bar(20_010, 0, SpanCategory::ElementPaint),
        ];
        let merged =
            merge_tiny_adjacent_bars(&bars, 0.0, MERGE_TEST_SPAN_NS, MERGE_TEST_CHART_WIDTH);
        // The wide bar never joins a run at all, and the lone tiny bar next
        // to it has no *candidate* neighbor to merge with -- two bars out,
        // both untouched.
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].label.as_ref(), "big");
        assert_eq!(merged[1].label.as_ref(), "leaf");
    }

    #[test]
    fn merge_tiny_adjacent_bars_keeps_different_depths_separate() {
        let bars = vec![
            tiny_bar(0, 0, SpanCategory::ElementPaint),
            tiny_bar(50, 1, SpanCategory::ElementPaint),
        ];
        let merged =
            merge_tiny_adjacent_bars(&bars, 0.0, MERGE_TEST_SPAN_NS, MERGE_TEST_CHART_WIDTH);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merge_tiny_adjacent_bars_picks_the_category_with_the_most_total_duration() {
        let bars = vec![
            FlameBar {
                label: "a".into(),
                depth: 0,
                start_ns: 0,
                duration_ns: 5,
                category: Some(SpanCategory::ElementPaint),
                gpu_pass_kind: None,
                element_type: None,
                element_source: None,
            },
            FlameBar {
                label: "b".into(),
                depth: 0,
                start_ns: 10,
                // Dominates the merged category despite being only one of
                // two merged bars -- summed *duration* decides, not count.
                duration_ns: 50,
                category: Some(SpanCategory::ElementRequestLayout),
                gpu_pass_kind: None,
                element_type: None,
                element_source: None,
            },
        ];
        let merged =
            merge_tiny_adjacent_bars(&bars, 0.0, MERGE_TEST_SPAN_NS, MERGE_TEST_CHART_WIDTH);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].category, Some(SpanCategory::ElementRequestLayout));
    }

    #[test]
    fn merge_tiny_adjacent_bars_returns_nothing_for_an_empty_lane() {
        assert!(merge_tiny_adjacent_bars(&[], 0.0, MERGE_TEST_SPAN_NS, MERGE_TEST_CHART_WIDTH)
            .is_empty());
    }

    #[test]
    fn lane_intersects_viewport_true_when_ranges_overlap() {
        assert!(lane_intersects_viewport(100.0, 200.0, 150.0, 300.0));
        // Touching exactly at an edge still counts as intersecting.
        assert!(lane_intersects_viewport(100.0, 200.0, 200.0, 300.0));
    }

    #[test]
    fn lane_intersects_viewport_false_when_entirely_above_or_below() {
        assert!(!lane_intersects_viewport(0.0, 50.0, 100.0, 200.0));
        assert!(!lane_intersects_viewport(300.0, 400.0, 100.0, 200.0));
    }

    #[test]
    fn visible_depth_window_covers_the_whole_lane_when_it_fits_in_the_viewport() {
        // A short lane (5 rows) fully inside a generous viewport should
        // window to every row, not a subset -- no virtualization overhead
        // for a lane that's already small.
        let (start, end) = visible_depth_window(-1000.0, 1000.0, 0.0, 4);
        assert_eq!((start, end), (0, 4));
    }

    #[test]
    fn visible_depth_window_clamps_to_a_partial_range_for_a_tall_lane() {
        // Viewport covers content-space rows [2*ROW, 5*ROW) of a lane whose
        // body starts at content-space 0 -- only rows 1..=5 (with the floor/
        // ceil rounding) should be in the window, not the full 0..=200.
        let (start, end) =
            visible_depth_window(2.0 * FLAME_ROW_HEIGHT, 5.0 * FLAME_ROW_HEIGHT, 0.0, 200);
        assert_eq!(start, 2);
        assert_eq!(end, 5);
        assert!(end < 200);
    }

    #[test]
    fn visible_depth_window_never_produces_an_inverted_range() {
        let (start, end) = visible_depth_window(-10.0, -5.0, 0.0, 50);
        assert!(end >= start);
    }
}

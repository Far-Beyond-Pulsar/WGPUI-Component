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
//!
//! The screenshot filmstrip (§2.3) is real too, backed by `gpui::Capture`'s
//! own periodic thumbnail capture (opt-in via the toolbar's `☑ Screenshots`
//! toggle — see `record::toolbar`) — see [`render_filmstrip_row`] and
//! [`render_hover_preview`]. It's the one visual element in this whole
//! strip that isn't GPU-instanced: a thumbnail is decoded bitmap content,
//! not a solid-color rectangle a shader can synthesize from a handful of
//! numbers, so it goes through this crate's ordinary `img()` element
//! (backed by the UI framework's own already-GPU-accelerated sprite atlas)
//! instead of another [`BarInstance`]. Hovering — or dragging any of this
//! strip's own gestures — anywhere on the strip live-scrubs a larger
//! preview of the nearest sample, Chrome's own "drag to see a live replay"
//! filmstrip behavior.
//!
//! Still not here, deliberately (`.agents/PROFILER_UI_SPEC.md`'s own
//! build-order note flags these as lower value than the CPU graph/Frames
//! row/filmstrip): a Network row (this profiler doesn't capture network
//! activity) and a Timings/Interactions row (no marker/interaction data
//! model yet). The Frames row also has to *place* each frame along the
//! time axis from `frame_durations_ms` alone (durations only, no per-frame
//! start times cross this function's contract — see [`frame_x_range`]'s
//! doc for the back-to-back-frames assumption that follows from that).

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    canvas, div, img, prelude::FluentBuilder as _, px, AnyElement, AppContext as _, Bounds,
    ClickEvent, Context, DragMoveEvent, Entity, Hsla, ImageSource, InteractiveElement as _,
    IntoElement, MouseMoveEvent, ParentElement as _, Pixels, Render, RenderImage,
    StatefulInteractiveElement as _, Styled, Thumbnail, Window,
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

/// Drag marker for panning an *existing* selection's whole body across the
/// domain, holding its width fixed — the third and last of this file's drag
/// gestures, alongside [`RangeSelectDrag`] (draw new) and
/// [`LeftEdgeDrag`]/[`RightEdgeDrag`] (resize one edge). See
/// [`selection_body`].
#[derive(Clone)]
struct PanSelectionDrag;

impl Render for PanSelectionDrag {
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
    /// `Some((anchor_ns, original_selection))` while the user is dragging
    /// the selection's *body* to pan it (see [`selection_body`]) —
    /// `anchor_ns` is the domain position under the cursor when the drag
    /// started, `original_selection` is what the selection was at that
    /// instant. Keeping both means the pan is one consistent shift computed
    /// fresh from the drag's start every move, rather than an accumulation
    /// of per-frame deltas that could drift.
    pan_drag: Option<(u64, (u64, u64))>,
    gpu: Option<FlameLaneGpu>,
    pipeline: Option<Rc<FlameBarPipeline>>,
    /// Latches once `Window::create_wgpu_surface` ever returns `None`
    /// (headless test platform, or a backend/platform that doesn't support
    /// it), so the "GPU unavailable" notice doesn't flicker if creation
    /// transiently fails on exactly one render.
    gpu_unavailable: bool,
    /// The domain position currently under the cursor, live during idle
    /// hover *and* any of this strip's four drag gestures — the single
    /// source of truth [`render_hover_preview`] reads to decide what to
    /// show, so "hover to preview" and "drag to scrub the live preview"
    /// are the same code path rather than two separate ones. `None` once
    /// the pointer leaves the strip (see `render`'s `.on_hover`).
    hover_ns: Option<u64>,
    /// Every captured thumbnail this session, decoded into a GPUI-
    /// displayable image exactly once and cached here — converting a
    /// `Thumbnail`'s raw RGBA8 bytes into an `Arc<RenderImage>` on every
    /// render (which can happen on every mouse-move tick while scrubbing)
    /// would redo real decode/allocation work for image data that never
    /// changes once a capture has stopped. Rebuilt only when
    /// `capture_generation` changes (see [`render`]'s cache-hit check),
    /// the same "computed once when the capture stops" treatment
    /// `ProfilerPanel::counter_summary`/`record::data::RecordOverview`
    /// already get. `None` before any thumbnails exist for the current
    /// capture (screenshots weren't enabled, or none have landed yet).
    thumbnail_images: Option<ThumbnailImageCache>,
}

/// [`OverviewState::thumbnail_images`]'s cached contents: every captured
/// thumbnail, timestamp-ordered (same order `gpui::Capture::thumbnails()`
/// already yields them in), each paired with its one-time-decoded image.
struct ThumbnailImageCache {
    capture_generation: u64,
    images: Vec<(u64, Arc<RenderImage>)>,
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
/// Vertical breathing room between the CPU graph and the filmstrip row below
/// it — same spirit as `FRAME_ROW_GAP`.
const FILMSTRIP_GAP: f32 = 3.0;
/// Height of the filmstrip row (§2.3) — real `img()` elements, not GPU
/// instances (see [`render_filmstrip_row`]'s doc comment for why bitmap
/// thumbnails are the one thing in this strip that isn't shader-rendered),
/// sized to leave a `FILMSTRIP_GAP`-tall gap of true-transparent, undrawn
/// space in the shared GPU surface for them to sit over.
const FILMSTRIP_HEIGHT: f32 = 28.0;
/// Vertical breathing room between the filmstrip row and the Frames row —
/// Chrome's own overview keeps its rows visually separate rather than
/// butted directly together.
const FRAME_ROW_GAP: f32 = 3.0;
/// Height of the Frames row. Deliberately thin relative to the CPU graph
/// (matches the reference screenshots — Frames is a secondary signal, not
/// a second chart of equal visual weight).
const FRAME_ROW_HEIGHT: f32 = 10.0;
/// Height of the region the shared GPU surface actually draws into (CPU
/// graph + gap + filmstrip-sized gap + gap + Frames row -- the filmstrip
/// row's own height counts toward this total even though the surface draws
/// nothing there itself, so the Frames row below it lands in the right
/// place); the ruler sits above this, drawn as plain text elements, not GPU
/// instances.
const GRAPH_AREA_HEIGHT: f32 =
    CPU_GRAPH_HEIGHT + FILMSTRIP_GAP + FILMSTRIP_HEIGHT + FRAME_ROW_GAP + FRAME_ROW_HEIGHT;
/// Top edge of the filmstrip row, in the same local-to-the-graph-area
/// coordinate space every `push_*` function in this file uses.
const FILMSTRIP_TOP: f32 = CPU_GRAPH_HEIGHT + FILMSTRIP_GAP;
/// Total strip height: ruler + graph area.
const STRIP_HEIGHT: f32 = RULER_HEIGHT + GRAPH_AREA_HEIGHT;
/// Aspect ratio (`width / height`) every thumbnail is captured at — see
/// `gpui::THUMBNAIL_WIDTH`/`THUMBNAIL_HEIGHT`. Used to size filmstrip slots
/// and the hover preview without distorting the source image.
const THUMBNAIL_ASPECT: f32 = gpui::THUMBNAIL_WIDTH as f32 / gpui::THUMBNAIL_HEIGHT as f32;
/// Width of one filmstrip slot, derived from [`FILMSTRIP_HEIGHT`] and the
/// thumbnail's own aspect ratio so slots are never stretched/squashed —
/// Chrome's own filmstrip fills the strip's width edge-to-edge with
/// however many same-size slots fit, rather than a fixed slot count, so
/// this file does the same (see [`render_filmstrip_row`]).
const FILMSTRIP_SLOT_WIDTH: f32 = FILMSTRIP_HEIGHT * THUMBNAIL_ASPECT;
/// The hover/scrub preview popup's size — larger than one filmstrip slot
/// (matching the reference screenshots' own "small filmstrip, bigger
/// hover-preview" size relationship) so it's actually useful to look at,
/// not just a magnified version of the same tiny thumbnail.
const PREVIEW_HEIGHT: f32 = 150.0;
const PREVIEW_WIDTH: f32 = PREVIEW_HEIGHT * THUMBNAIL_ASPECT;
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
    // The detail flame chart's own current visible window (only `Some` when
    // `selection` is, and generally narrower than it once the user zooms in
    // down there) -- what actually gets *drawn and dragged* as this strip's
    // selection box. `selection` itself never gets read for that; it stays
    // purely "what a direct drag on the overview last set", read only by
    // `record::recompute_for_selection` to (re)establish `flame_zoom`'s
    // domain. Deliberately two separate values rather than one, so that
    // detail-view zooming can flow back into this strip's display without
    // ever writing to `selection` -- if it did, the next render's
    // `set_domain` call would see a "changed" domain and reset the zoom
    // right back out to fit it, undoing the very zoom being reflected.
    detail_visible: Option<(u64, u64)>,
    frame_durations_ms: &[f32],
    // The active/stopped capture, purely for its `.thumbnails()` — every
    // other value this function needs (the overview buckets, the frame
    // durations) is already precomputed and passed in separately above.
    // `None` before any capture has ever completed, same as everywhere
    // else in `record` that threads `Capture` through for one specific
    // query rather than owning it.
    capture: Option<&gpui::Capture>,
    capture_generation: u64,
    window: &mut Window,
    cx: &mut Context<ProfilerPanel>,
) -> AnyElement {
    let Some(overview) = overview_data else {
        return div().into_any_element();
    };
    // Falls back to `selection` only as a defensive default (`mod.rs`
    // always passes both `Some` or both `None` together); this is the
    // range this strip actually shows and drags, per the doc comment above.
    let displayed_range = detail_visible.or(selection);

    // Rebuild the decoded-thumbnail cache only when this is a genuinely
    // different capture than the one it was last built from -- see
    // `OverviewState::thumbnail_images`'s field doc for why this is a
    // cache at all, not per-render decode work.
    let cache_hit = state
        .thumbnail_images
        .as_ref()
        .is_some_and(|cache| cache.capture_generation == capture_generation);
    if !cache_hit {
        state.thumbnail_images = capture.map(|capture| ThumbnailImageCache {
            capture_generation,
            images: capture
                .thumbnails()
                .filter_map(|(ns, thumbnail)| Some((*ns, thumbnail_to_render_image(thumbnail)?)))
                .collect(),
        });
    }

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
        push_selection_segments(
            &mut instances,
            displayed_range,
            domain_start_ns,
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

    let selection_element = displayed_range.map(|(start, end)| {
        render_selection_overlay(
            start,
            end,
            domain_start_ns,
            domain_span_ns,
            chart_width,
            panel_entity.clone(),
        )
    });

    let filmstrip_element = state
        .thumbnail_images
        .as_ref()
        .and_then(|cache| render_filmstrip_row(&cache.images, domain_start_ns, domain_span_ns, chart_width));

    // `deferred(..)` delays this popup's *painting* until after
    // `#record-overview`'s own subtree finishes -- its layout/positioning
    // still resolves against the same coordinate space a normal child
    // would, but painting later is what lets it draw outside that
    // container's `.overflow_hidden()` clip (the same escape-the-clip
    // mechanism this crate's own popovers/context menus already use for
    // exactly this "float above a scroll/clip container" need), since the
    // preview intentionally sits *above* the strip's own top edge.
    let hover_preview_element = render_hover_preview(state, domain_start_ns, domain_span_ns, chart_width, cx)
        .map(|preview| gpui::deferred(preview).with_priority(1).into_any_element());

    v_flex()
        // `resizable_panel()`'s own wrapping div is a `display:flex` *row*
        // (see `resizable::panel::ResizablePanel::render` -- it never sets
        // `flex_direction`, so it inherits the row default) that this
        // element is the sole non-absolute child of. `w_full()` alone
        // (`width: 100%`) asks the *row* to resolve this item's width from
        // a percentage of its own box, which the flex algorithm doesn't
        // reliably do for a flex-basis: auto item ahead of knowing that
        // box's own final size; `flex_1()` (grow: 1, shrink: 1, basis: 0)
        // sidesteps that entirely by asking for "all available main-axis
        // space" directly, the same way this file's sibling panes in
        // `record::mod`'s `main_column` already size themselves.
        .flex_1()
        .w_full()
        .min_w(px(0.))
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
                                // NOT `.defer_resize_until_mouse_up(true)`:
                                // that flag defers the *texture* reallocation
                                // while any left mouse button is held OR the
                                // window itself is resizing (see
                                // `WgpuSurfaceElement::prepaint`) -- meant for
                                // the flame chart's many per-lane surfaces,
                                // where reallocating dozens of textures on
                                // every pixel of a drag is real cost. This
                                // strip has exactly one shared surface, so
                                // that protection isn't worth what it costs
                                // here: while dragging a pane-resize handle
                                // (also a left-button drag) the texture stays
                                // at its *old* size and only actually
                                // reallocates on mouse-up, but the CPU-side
                                // `chart_width`/instances this file computes
                                // every render already tracks the *current*
                                // measured width throughout the drag -- so
                                // the strip visibly resized correctly while
                                // dragging, then front-loaded a mismatch
                                // (new box, stale-width content) the instant
                                // the deferred texture resize finally landed
                                // without a fresh instance rebuild to go with
                                // it. Resizing immediately keeps the texture
                                // and the CPU-side math in lockstep always.
                                gpui::wgpu_surface(handle).absolute().inset_0(),
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
                .children(filmstrip_element)
                .children(selection_element)
                .children(hover_preview_element)
                .on_mouse_move(cx.listener(|panel, event: &MouseMoveEvent, _window, cx| {
                    // Idle-hover half of `hover_ns`'s live-scrub contract
                    // (see `OverviewState::hover_ns`'s field doc) -- the
                    // four drag handlers below cover the other half.
                    let ns = overview_x_to_ns(
                        &panel.record.overview,
                        panel.record.overview_data(),
                        event.position.x,
                    );
                    if panel.record.overview.hover_ns != ns {
                        panel.record.overview.hover_ns = ns;
                        cx.notify();
                    }
                }))
                .on_hover(cx.listener(|panel, hovered: &bool, _window, cx| {
                    if !hovered && panel.record.overview.hover_ns.is_some() {
                        panel.record.overview.hover_ns = None;
                        cx.notify();
                    }
                }))
                .on_drag(RangeSelectDrag, {
                    let panel_entity = panel_entity.clone();
                    move |_, start_position, _window, cx| {
                        panel_entity.update(cx, |panel, _cx| {
                            let ns = overview_x_to_ns(&panel.record.overview, panel.record.overview_data(), start_position.x);
                            panel.record.overview.drag_anchor_ns = ns;
                            panel.record.overview.hover_ns = ns;
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
                        panel.record.overview.hover_ns = Some(current_ns);
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

/// Composes the selection's three drag gestures — pan the whole body,
/// resize the left edge, resize the right edge — into one `inset_0()`
/// coordinate space. Purely interactive: every one of these is a fully
/// transparent hit-target div, `.occlude()`d so a drag starting on any of
/// them doesn't *also* trigger the strip's own from-scratch
/// `RangeSelectDrag`. The actual selection *pixels* (fill, border, grip
/// marks) are GPU-instanced alongside the CPU graph/Frames row —
/// see [`push_selection_segments`] — not drawn by anything in this
/// function or its children.
fn render_selection_overlay(
    start: u64,
    end: u64,
    domain_start_ns: u64,
    domain_span_ns: f64,
    chart_width: f32,
    panel_entity: Entity<ProfilerPanel>,
) -> AnyElement {
    let x0 = ((start.saturating_sub(domain_start_ns)) as f64 / domain_span_ns) as f32 * chart_width;
    let x1 = ((end.saturating_sub(domain_start_ns)) as f64 / domain_span_ns) as f32 * chart_width;
    let left = x0.min(x1);
    let width = (x0 - x1).abs().max(1.0);
    let right = left + width;

    // A single `inset_0()` wrapper so the three hit-targets all position
    // `left`/`top` against the *same* coordinate space (the whole strip)
    // rather than nesting one absolutely-positioned grip inside another —
    // that would make the inner one's `left` relative to the outer one's
    // edge instead of a consistent shared x axis.
    div()
        .absolute()
        .inset_0()
        .child(selection_body(left, width, start, end, panel_entity.clone()))
        .child(left_edge_grip(left, end, panel_entity.clone()))
        .child(right_edge_grip(right, start, panel_entity))
        .into_any_element()
}

/// The selection's *hit target* for dragging the whole body to pan the
/// range across the domain (holding its width fixed) rather than redraw
/// it -- Chrome's own overview selection works the same way: it's a window
/// into the data, not just a highlight. Fully transparent -- the fill/
/// border pixels this box visually corresponds to are drawn by
/// [`push_selection_segments`] instead. `.occlude()`d for the same reason
/// the edge grips are (see [`left_edge_grip`]'s doc comment): without it, a
/// drag starting inside the selection would *also* trigger the strip's own
/// `RangeSelectDrag`, which draws a brand-new selection from scratch
/// instead of panning this one.
fn selection_body(
    left: f32,
    width: f32,
    start: u64,
    end: u64,
    panel_entity: Entity<ProfilerPanel>,
) -> AnyElement {
    div()
        .id("record-overview-selection-body")
        .occlude()
        .cursor_grab()
        .absolute()
        .top(px(0.))
        .bottom(px(0.))
        .left(px(left))
        .w(px(width))
        .on_drag(PanSelectionDrag, {
            let panel_entity = panel_entity.clone();
            move |_, start_position, _window, cx| {
                panel_entity.update(cx, |panel, _cx| {
                    if let Some(anchor_ns) = overview_x_to_ns(
                        &panel.record.overview,
                        panel.record.overview_data(),
                        start_position.x,
                    ) {
                        panel.record.overview.pan_drag = Some((anchor_ns, (start, end)));
                    }
                });
                cx.new(|_| PanSelectionDrag)
            }
        })
        .on_drag_move(move |event: &DragMoveEvent<PanSelectionDrag>, _window, cx| {
            panel_entity.update(cx, |panel, cx| {
                let Some((anchor_ns, (orig_start, orig_end))) = panel.record.overview.pan_drag
                else {
                    return;
                };
                let Some(current_ns) = overview_x_to_ns(
                    &panel.record.overview,
                    panel.record.overview_data(),
                    event.event.position.x,
                ) else {
                    return;
                };
                let Some((domain_start, domain_end)) = panel
                    .record
                    .overview_data()
                    .map(|o| (o.domain_start_ns, o.domain_end_ns))
                else {
                    return;
                };
                panel.record.selection = Some(pan_selection(
                    (orig_start, orig_end),
                    anchor_ns,
                    current_ns,
                    (domain_start, domain_end),
                ));
                panel.record.overview.hover_ns = Some(current_ns);
                cx.notify();
            });
        })
        .into_any_element()
}

/// Shifts `(orig_start, orig_end)` by however far the pointer moved from
/// `anchor_ns` to `current_ns`, then slides the result back inside
/// `(domain_start, domain_end)` (without changing its width) if that shift
/// would have pushed either edge outside the domain — the same "clamp by
/// sliding, not by clipping the width" behavior [`super::data`]'s
/// `RangeZoom::clamp_to_domain` uses elsewhere in this profiler, kept as a
/// free function here since this operates on plain `u64` nanoseconds
/// rather than that type's `f64` fractional domain.
fn pan_selection(
    (orig_start, orig_end): (u64, u64),
    anchor_ns: u64,
    current_ns: u64,
    (domain_start, domain_end): (u64, u64),
) -> (u64, u64) {
    let delta = current_ns as i64 - anchor_ns as i64;
    let width = orig_end as i64 - orig_start as i64;
    let mut new_start = orig_start as i64 + delta;
    let mut new_end = orig_end as i64 + delta;

    if new_start < domain_start as i64 {
        let shift = domain_start as i64 - new_start;
        new_start += shift;
        new_end += shift;
    }
    if new_end > domain_end as i64 {
        let shift = new_end - domain_end as i64;
        new_start -= shift;
        new_end -= shift;
    }
    // A selection wider than the domain itself (only possible if the
    // domain is smaller than the selection to begin with) still has to
    // land somewhere sane after both clamps above fight each other.
    new_start = new_start.max(domain_start as i64);
    new_end = new_end.max(new_start + width.max(1));

    (new_start as u64, new_end as u64)
}

/// One selection edge's drag-to-resize hit target: a small, fully
/// transparent `.occlude()`d box (matching `resizable::resize_handle`'s own
/// "small drag handle on top of a larger interactive area" pattern —
/// occlusion is what stops the strip's own whole-selection
/// `RangeSelectDrag` from *also* firing for the same mouse-down, since
/// GPUI hit-tests the topmost occluding element first) that moves *only*
/// this edge, holding `other_edge_ns` fixed for the duration of the drag.
/// `x` is in strip-local coordinates, same axis [`render_selection_overlay`]
/// uses. The visible grip pill this hit-target sits over is GPU-instanced
/// by [`push_selection_segments`], not drawn here.
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
                panel.record.overview.hover_ns = Some(current_ns);
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
                panel.record.overview.hover_ns = Some(current_ns);
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
    let row_top = FILMSTRIP_TOP + FILMSTRIP_HEIGHT + FRAME_ROW_GAP;
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

/// Decodes one captured [`Thumbnail`]'s raw RGBA8 bytes into a GPUI-
/// displayable image, the same `image::RgbaImage::from_raw` ->
/// `image::Frame` -> `RenderImage` pipeline
/// `ProfilerPanel::render_deep_capture_preview` already uses for its own
/// on-demand GPU-replay preview -- not a new image-decoding path, reusing
/// the one this crate already has. Returns `None` only if `thumbnail`'s
/// byte buffer doesn't actually match `width * height * 4` (would indicate
/// a corrupt/mismatched sample from the capture engine; degrade to "no
/// image for this sample" rather than panicking).
fn thumbnail_to_render_image(thumbnail: &Thumbnail) -> Option<Arc<RenderImage>> {
    let rgba = image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, thumbnail.rgba.clone())?;
    let frame = image::Frame::new(rgba);
    Some(Arc::new(RenderImage::new(smallvec::smallvec![frame])))
}

/// The cached image whose timestamp is at-or-before `ns`, falling back to
/// the earliest cached image when `ns` precedes every sample — the exact
/// same "at-or-before, fall back to earliest" rule
/// `gpui::Capture::thumbnail_near` uses over raw `Thumbnail`s, reimplemented
/// here over the already-decoded `(timestamp, image)` cache instead so a
/// filmstrip-slot or hover-preview lookup never has to re-decode an image
/// `render`'s own cache-rebuild step already produced. `images` is
/// timestamp-ordered (same order `Capture::thumbnails()` yields), so this
/// is a binary search, not a scan.
fn nearest_cached_image(images: &[(u64, Arc<RenderImage>)], ns: u64) -> Option<Arc<RenderImage>> {
    if images.is_empty() {
        return None;
    }
    // `partition_point` finds the first index whose timestamp is *after*
    // `ns`; the sample at-or-before `ns` is one slot to the left of that,
    // unless `ns` precedes every sample (index 0), which is exactly the
    // "fall back to earliest" case.
    let split = images.partition_point(|(timestamp, _)| *timestamp <= ns);
    let index = split.saturating_sub(1).min(images.len() - 1);
    Some(images[index].1.clone())
}

/// The filmstrip row (§2.3): a strip of small screenshots sampled across
/// the whole capture, matching Chrome's own comic-strip-of-thumbnails
/// overview row. The one part of this whole strip that *isn't*
/// GPU-instanced — a thumbnail is real bitmap content decoded from a
/// capture, not a solid-color rectangle a shader can synthesize from a
/// handful of numbers, so it goes through this crate's ordinary `img()`
/// element (backed by the UI framework's own sprite atlas, which is
/// already GPU-accelerated for exactly this — image content, not shader-
/// friendly instanced geometry) instead of another `BarInstance`. Slots are
/// sized to [`FILMSTRIP_SLOT_WIDTH`] (thumbnail-aspect-correct, not
/// stretched) and fill the strip edge-to-edge with however many fit, each
/// showing [`nearest_cached_image`] for that slot's own timestamp — so a
/// wide strip shows more, smaller-interval samples, not the same handful
/// stretched wider.
///
/// Returns `None` (rendering nothing) when there's no capture, no
/// thumbnails were ever sampled (`☑ Screenshots` was off), or the cache
/// hasn't been decoded yet — `render`'s own "no filmstrip row at all"
/// fallback for a capture that simply doesn't have this data, matching how
/// the rest of this file omits rows it has no honest content for rather
/// than rendering an empty placeholder band.
fn render_filmstrip_row(
    images: &[(u64, Arc<RenderImage>)],
    domain_start_ns: u64,
    domain_span_ns: f64,
    chart_width: f32,
) -> Option<AnyElement> {
    if images.is_empty() {
        return None;
    }
    let slot_count = ((chart_width / FILMSTRIP_SLOT_WIDTH).floor() as usize).max(1);

    let mut slots: Vec<AnyElement> = Vec::with_capacity(slot_count);
    for slot in 0..slot_count {
        // The slot's *center* timestamp -- sampling at the center rather
        // than the leading edge means the very first/last slot's thumbnail
        // is representative of the middle of its own span, not biased
        // toward the strip's outer edges.
        let fraction = (slot as f64 + 0.5) / slot_count as f64;
        let ns = domain_start_ns + (fraction * domain_span_ns) as u64;
        let Some(image) = nearest_cached_image(images, ns) else {
            continue;
        };
        let x = slot as f32 * FILMSTRIP_SLOT_WIDTH;
        slots.push(
            img(ImageSource::Render(image))
                .id(("record-overview-filmstrip-slot", slot))
                .absolute()
                .top(px(0.))
                .left(px(x))
                .w(px(FILMSTRIP_SLOT_WIDTH))
                .h(px(FILMSTRIP_HEIGHT))
                .into_any_element(),
        );
    }

    Some(
        div()
            .absolute()
            .top(px(FILMSTRIP_TOP))
            .left(px(0.))
            .w(px(chart_width))
            .h(px(FILMSTRIP_HEIGHT))
            .overflow_hidden()
            .children(slots)
            .into_any_element(),
    )
}

/// The hover/scrub preview popup (§2.3/§6): a larger re-showing of whatever
/// thumbnail is nearest the cursor, labeled with the exact timestamp under
/// it — Chrome's own "10459 ms 🔍" hover label. Live during *both* idle
/// hover and any of this strip's drag gestures (`state.hover_ns` is updated
/// from all of them, see `render`'s `.on_mouse_move`/`.on_drag_move`
/// wiring), which is what makes dragging across the overview double as a
/// live filmstrip scrub instead of only a selection-editing gesture.
///
/// Positioned just *above* the strip near the hovered x (not directly under
/// the cursor, which would put the popup under the finger/pointer doing the
/// hovering) and clamped so it never runs off either edge of the strip.
fn render_hover_preview(
    state: &OverviewState,
    domain_start_ns: u64,
    domain_span_ns: f64,
    chart_width: f32,
    cx: &Context<ProfilerPanel>,
) -> Option<AnyElement> {
    let hover_ns = state.hover_ns?;
    let images = &state.thumbnail_images.as_ref()?.images;
    let image = nearest_cached_image(images, hover_ns)?;

    let fraction = ((hover_ns.saturating_sub(domain_start_ns)) as f64 / domain_span_ns) as f32;
    let raw_x = fraction * chart_width;
    let left = (raw_x - PREVIEW_WIDTH / 2.0).clamp(0.0, (chart_width - PREVIEW_WIDTH).max(0.0));
    let relative_ms = (hover_ns.saturating_sub(domain_start_ns)) / 1_000_000;

    Some(
        div()
            .absolute()
            .top(px(-(PREVIEW_HEIGHT + RULER_HEIGHT + 6.0)))
            .left(px(left))
            .flex()
            .flex_col()
            .bg(cx.theme().popover)
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .overflow_hidden()
            .shadow_lg()
            .child(
                img(ImageSource::Render(image))
                    .w(px(PREVIEW_WIDTH))
                    .h(px(PREVIEW_HEIGHT)),
            )
            .child(
                div()
                    .px_1()
                    .py_0p5()
                    .text_xs()
                    .text_color(cx.theme().popover_foreground)
                    .child(format!("{} ms \u{1F50D}", format_ms_with_commas(relative_ms))),
            )
            .into_any_element(),
    )
}

/// Selection box fill/border + edge-grip marks, appended to the same
/// instance buffer as the CPU graph/Frames row -- moved here from plain
/// `div()` styling so the "everything except text/popups renders via the
/// shader" rule applies to the selection the same as everything else in
/// this strip. Only the *pixels* live here; the draggable hit-target divs
/// ([`selection_body`]/[`left_edge_grip`]/[`right_edge_grip`]) stay in the
/// UI framework layer, fully transparent, purely for interactivity — same
/// split this crate already uses for the flame chart's own bars vs. their
/// hit-test overlay.
///
/// Deliberately confined to the graph area's own local coordinate space
/// (`0..GRAPH_AREA_HEIGHT`, the same space every other `push_*` function in
/// this file uses), not the reference's full-strip-height handles (ruler
/// band included) -- the shared surface this draws into only covers the
/// graph area; the ruler above it is a separate plain-text overlay, and
/// extending the surface upward just to cover a few extra pixels of
/// selection chrome would mean re-deriving every other `push_*`
/// function's Y math against a taller area for a purely cosmetic gain.
fn push_selection_segments(
    instances: &mut Vec<BarInstance>,
    displayed_range: Option<(u64, u64)>,
    domain_start_ns: u64,
    domain_span_ns: f64,
    chart_width: f32,
    scale: f32,
    cx: &Context<ProfilerPanel>,
) {
    let Some((start, end)) = displayed_range else {
        return;
    };
    let x0 = ((start.saturating_sub(domain_start_ns)) as f64 / domain_span_ns) as f32 * chart_width;
    let x1 = ((end.saturating_sub(domain_start_ns)) as f64 / domain_span_ns) as f32 * chart_width;
    let left = x0.min(x1);
    let width = (x0 - x1).abs().max(1.0);
    let right = left + width;

    const BORDER_PX: f32 = 1.0;
    const GRIP_HALF_WIDTH: f32 = 2.0;
    const GRIP_HEIGHT: f32 = 14.0;

    let fill = cx.theme().selection.opacity(0.2).to_rgb();
    instances.push(BarInstance {
        rect_min: [left * scale, 0.0],
        rect_max: [right * scale, GRAPH_AREA_HEIGHT * scale],
        color: [fill.r, fill.g, fill.b, fill.a],
        corner_radius: 0.0,
        highlight: 0.0,
        _pad: [0.0, 0.0],
    });

    let border = cx.theme().selection.to_rgb();
    let border_rects = [
        (left, 0.0, left + BORDER_PX, GRAPH_AREA_HEIGHT),
        (right - BORDER_PX, 0.0, right, GRAPH_AREA_HEIGHT),
        (left, 0.0, right, BORDER_PX),
        (left, GRAPH_AREA_HEIGHT - BORDER_PX, right, GRAPH_AREA_HEIGHT),
    ];
    for (bx0, by0, bx1, by1) in border_rects {
        instances.push(BarInstance {
            rect_min: [bx0 * scale, by0 * scale],
            rect_max: [bx1 * scale, by1 * scale],
            color: [border.r, border.g, border.b, border.a],
            corner_radius: 0.0,
            highlight: 0.0,
            _pad: [0.0, 0.0],
        });
    }

    // Edge-grip pills: the affordance cue for "you can drag this edge",
    // matching the reference's own selection handles -- same visual as
    // before, just GPU-instanced instead of a `div().rounded_full()`.
    for grip_x in [left, right] {
        instances.push(BarInstance {
            rect_min: [
                (grip_x - GRIP_HALF_WIDTH) * scale,
                (GRAPH_AREA_HEIGHT / 2.0 - GRIP_HEIGHT / 2.0) * scale,
            ],
            rect_max: [
                (grip_x + GRIP_HALF_WIDTH) * scale,
                (GRAPH_AREA_HEIGHT / 2.0 + GRIP_HEIGHT / 2.0) * scale,
            ],
            color: [0.0, 0.0, 0.0, 0.35],
            corner_radius: GRIP_HALF_WIDTH * scale,
            highlight: 0.0,
            _pad: [0.0, 0.0],
        });
    }
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

    #[test]
    fn pan_selection_shifts_by_the_pointer_delta_when_inside_the_domain() {
        let result = pan_selection((100, 200), 500, 550, (0, 1_000));
        assert_eq!(result, (150, 250));
    }

    #[test]
    fn pan_selection_slides_back_at_the_domain_start_without_shrinking() {
        // Dragging far enough left that a raw shift would push start < 0
        // should clamp by sliding the whole window back into the domain,
        // not by clipping its width.
        let result = pan_selection((100, 200), 500, 0, (0, 1_000));
        assert_eq!(result, (0, 100));
    }

    #[test]
    fn pan_selection_slides_back_at_the_domain_end_without_shrinking() {
        let result = pan_selection((800, 900), 500, 1_500, (0, 1_000));
        assert_eq!(result, (900, 1_000));
    }

    #[test]
    fn pan_selection_preserves_width_regardless_of_direction() {
        let (start, end) = pan_selection((300, 450), 1_000, 700, (0, 10_000));
        assert_eq!(end - start, 150);
    }

    fn sample_thumbnail() -> Thumbnail {
        Thumbnail {
            width: 1,
            height: 1,
            rgba: vec![10, 20, 30, 255],
        }
    }

    #[test]
    fn thumbnail_to_render_image_decodes_a_well_formed_sample() {
        assert!(thumbnail_to_render_image(&sample_thumbnail()).is_some());
    }

    #[test]
    fn thumbnail_to_render_image_declines_a_mismatched_buffer() {
        // 1x1 RGBA8 needs exactly 4 bytes; this has 3.
        let malformed = Thumbnail {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3],
        };
        assert!(thumbnail_to_render_image(&malformed).is_none());
    }

    fn sample_images(timestamps: &[u64]) -> Vec<(u64, Arc<RenderImage>)> {
        let image = thumbnail_to_render_image(&sample_thumbnail()).unwrap();
        timestamps.iter().map(|ts| (*ts, image.clone())).collect()
    }

    #[test]
    fn nearest_cached_image_returns_none_for_an_empty_cache() {
        assert!(nearest_cached_image(&[], 1_000).is_none());
    }

    #[test]
    fn nearest_cached_image_falls_back_to_earliest_before_the_first_sample() {
        let images = sample_images(&[1_000, 2_000, 3_000]);
        // Querying before the first sample can't find an at-or-before match,
        // so it should fall back to the earliest one rather than `None`.
        let found = nearest_cached_image(&images, 0).unwrap();
        assert!(Arc::ptr_eq(&found, &images[0].1));
    }

    #[test]
    fn nearest_cached_image_picks_the_at_or_before_sample() {
        let images = sample_images(&[1_000, 2_000, 3_000]);
        let found = nearest_cached_image(&images, 2_500).unwrap();
        assert!(Arc::ptr_eq(&found, &images[1].1));
    }

    #[test]
    fn nearest_cached_image_matches_an_exact_timestamp() {
        let images = sample_images(&[1_000, 2_000, 3_000]);
        let found = nearest_cached_image(&images, 2_000).unwrap();
        assert!(Arc::ptr_eq(&found, &images[1].1));
    }

    #[test]
    fn nearest_cached_image_returns_the_last_sample_past_the_end() {
        let images = sample_images(&[1_000, 2_000, 3_000]);
        let found = nearest_cached_image(&images, 999_999).unwrap();
        assert!(Arc::ptr_eq(&found, &images[2].1));
    }
}

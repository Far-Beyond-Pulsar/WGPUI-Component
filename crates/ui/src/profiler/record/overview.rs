//! The overview strip (§2 of `.agents/PROFILER_UI_SPEC.md`): a ruler-labeled
//! **stacked** CPU-activity area graph plus a per-frame "Frames" row over the
//! *whole* capture window, with a drag-to-select gesture that sets the
//! active range. Deliberately never zooms itself — it's the navigator, not
//! the thing being navigated; the detail flame chart is what zooms, via the
//! selection this produces.
//!
//! Two rendering paths, not one: the CPU-activity graph itself is a smooth
//! curve — [`build_stacked_area_bands`]/[`paint_stacked_area_bands`], backed
//! by `gpui::PathBuilder`, one tessellated fill per category (at most
//! [`super::data::OVERVIEW_CATEGORIES`]`.len()` = 8 draw calls total,
//! matching the reference's own smoothly-shaped CPU graph instead of a
//! blocky one flat-topped rectangle per bucket per category). Everything
//! else here — ruler gridlines, the long-task hatch, the Frames row, the
//! drag-selection overlay — is still GPU-instanced via
//! [`crate::profiler::BarInstance`]/[`FlameLaneGpu`], the same approach the
//! detail flame chart's lanes use: those are genuinely axis-aligned
//! rectangles with no benefit from a smooth curve, so instancing them into
//! one shared `wgpu_surface`/instance buffer (one draw call for all of
//! them) stays the cheaper, simpler choice. The curve paints *underneath*
//! that shared surface (see [`render`]'s own doc comment at the relevant
//! `.child(..)` call for the exact ordering reasoning), so the selection
//! overlay -- always the last thing pushed into that shared buffer -- still
//! reads on top of the graph exactly as before.
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
//! strip that's neither GPU-instanced nor a tessellated path: a thumbnail
//! is decoded bitmap content, not something a shader or a fill can
//! synthesize from a handful of numbers, so it goes through this crate's
//! ordinary `img()` element (backed by the UI framework's own
//! already-GPU-accelerated sprite atlas) instead. Hovering — or dragging
//! any of this strip's own gestures — anywhere on the strip live-scrubs a larger
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
    canvas, div, img, point, prelude::FluentBuilder as _, px, AnyElement, AppContext as _, Bounds,
    ClickEvent, Context, DragMoveEvent, Entity, Hsla, ImageSource, InteractiveElement as _,
    IntoElement, MouseButton, MouseMoveEvent, ParentElement as _, PathBuilder, Pixels, Point,
    Render, RenderImage, StatefulInteractiveElement as _, Styled, Thumbnail, Window,
};

use crate::{v_flex, ActiveTheme};

use crate::profiler::{category_color, BarInstance, FlameBarPipeline, FlameLaneGpu};

use super::{
    data::{OverviewBucket, RecordOverview, OVERVIEW_BUCKET_COUNT, OVERVIEW_CATEGORIES},
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

impl OverviewState {
    /// Whether any of this strip's three selection-editing drags (draw a
    /// new selection, resize an edge, pan the body) is currently in
    /// progress. `record::recompute_for_selection`/
    /// `recompute_bottom_up_for_selection` read this to skip their
    /// expensive per-range rebuild while `true` — see those functions' own
    /// doc comments for why a drag firing dozens of these rebuilds per
    /// second (once per mouse-move tick, each one a fresh walk of every
    /// span in the *current, still-changing* selection) was the actual
    /// source of the reported lag while dragging a large selection, not
    /// the flame chart's own GPU-instanced rendering.
    pub(crate) fn is_dragging_selection(&self) -> bool {
        self.drag_anchor_ns.is_some() || self.edge_fixed_ns.is_some() || self.pan_drag.is_some()
    }

    /// Clears all three drag-in-progress flags at once — called from every
    /// one of this strip's `on_mouse_up` handlers regardless of *which*
    /// gesture just ended, since exactly one of the three is ever `Some` at
    /// a time and clearing the other two (already `None`) is a no-op.
    /// Without this, none of them was ever reset back to `None` anywhere,
    /// which would otherwise leave `is_dragging_selection` stuck `true` —
    /// and the expensive rebuild it gates permanently skipped — from the
    /// very first drag onward.
    fn end_drag(&mut self) {
        self.drag_anchor_ns = None;
        self.edge_fixed_ns = None;
        self.pan_drag = None;
    }
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
/// The hover/scrub preview popup's size — much larger than one filmstrip
/// slot (matching the reference screenshots' own "small filmstrip, bigger
/// hover-preview" size relationship) so it's actually useful to look at,
/// not just a slightly-magnified version of the same tiny thumbnail. Sized
/// well past a single thumbnail's own native resolution (§ `Thumbnail`'s
/// doc comment: 160x100) since legibility, not pixel-density, is the goal
/// here — this is a quick "what was on screen" glance, not a lossless
/// zoom.
const PREVIEW_HEIGHT: f32 = 320.0;
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
    // The one authoritative panel width every panel in the resizable group
    // shares -- see `RecordState::panels_bounds`'s field doc. `<= 1.0`
    // means "not measured yet"; `#record-overview` below falls back to its
    // ordinary flex-based sizing for that one frame instead.
    panels_width: f32,
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

    // The stacked CPU-activity graph itself: a smooth curve, not one flat
    // rectangle per bucket per category (see `build_stacked_area_bands`'s
    // own doc comment for the full reasoning -- fewer draw calls *and* the
    // curved look, from the same change). Built here, in `App`-context,
    // because `category_color` needs `Context<ProfilerPanel>`, not the
    // bare `&mut App` the painting canvas below only has; the bands
    // themselves are plain data with no GPU/paint dependency, so they're
    // just captured into that canvas's closure once built.
    let cpu_area_bands = build_stacked_area_bands(&overview.buckets, max_ns, bucket_width_px);
    let cpu_area_colors: Vec<Hsla> = OVERVIEW_CATEGORIES
        .iter()
        .map(|c| category_color(*c, cx))
        .collect();

    let gpu_available = !state.gpu_unavailable;
    let scale = window.scale_factor();
    let mut instances: Vec<BarInstance> = Vec::new();
    if gpu_available {
        push_ruler_gridlines(&mut instances, chart_width, scale, cx);
        for (index, bucket) in overview.buckets.iter().enumerate() {
            let x = index as f32 * bucket_width_px;
            let width = (bucket_width_px - 0.5).max(1.0);
            push_long_task_hatch(&mut instances, bucket, x, width, scale);
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
                // Explicit pixel width from `panels_width` -- the *one*
                // number every panel in the resizable group below reads
                // (see `RecordState::panels_bounds`'s field doc) -- rather
                // than any flex/percentage resolution. This `div` sits
                // inside a plain column, so ordinarily its width would just
                // be the cross axis, resolved for free by the column's
                // default stretch with no percentage math involved at all
                // -- and that *is* what the `.when` below falls back to
                // for the one frame before `panels_width` is measured.
                // But stretch-based sizing here turned out to still be
                // reachable by the exact same "row flex-item resolving a
                // percentage of a box whose own size isn't pinned down
                // yet" failure this whole module tree already documents
                // (`record::flame::render`'s own outer container has the
                // matching fix) one level further up the tree, where a
                // *sibling* panel's own content could perturb the result.
                // An explicit pixel value measured upstream of the entire
                // resizable group sidesteps every one of those layers at
                // once, unconditionally.
                .when(panels_width > 1.0, |el| el.w(px(panels_width)))
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
                                panel_entity.update(cx, |panel, cx| {
                                    // See `crate::profiler::update_measured_bounds`'s doc
                                    // comment: a resize (window or split-pane handle)
                                    // settles this `w_full()` container's own layout
                                    // immediately, but the shared GPU surface below is
                                    // sized to a fixed `chart_width` derived from this
                                    // measurement -- without an explicit notify here on
                                    // a real change, that surface (and the whole CPU
                                    // graph/filmstrip/Frames row painted onto it) stays
                                    // frozen at its pre-resize size, adrift small and
                                    // left-aligned in the now-correctly-resized
                                    // container, until something unrelated causes
                                    // another render of this panel.
                                    if crate::profiler::update_measured_bounds(
                                        &mut panel.record.overview.bounds,
                                        bounds,
                                    ) {
                                        cx.notify();
                                    }
                                });
                            }
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
                .children(ruler_elements)
                // The smooth stacked CPU-activity curve -- painted here,
                // *before* (so visually underneath) the GPU-instanced
                // gridlines/Frames-row/selection-overlay surface below, so
                // the selection highlight and its border/grips stay
                // visible over the graph exactly as before (that overlay
                // was always the last thing painted into the shared
                // surface, i.e. already on top; this curve just joins
                // everything else that already sits under it). Gridlines
                // painting on top of the curve instead of being masked by
                // it wherever the old flat-topped bars used to cover them
                // is the one visible ordering change, and if anything
                // reads as more correct: a reference line is now always
                // visible, never hidden behind the data it's a reference
                // for.
                .child(
                    canvas(
                        move |_bounds, _window, _cx| (cpu_area_bands, cpu_area_colors),
                        |bounds, (bands, colors), window, _cx| {
                            paint_stacked_area_bands(&bands, bounds.origin, &colors, window);
                        },
                    )
                    .absolute()
                    .top(px(RULER_HEIGHT))
                    .left(px(0.))
                    .w(px(chart_width))
                    .h(px(CPU_GRAPH_HEIGHT)),
                )
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
                // Ends the drag-select gesture and forces one fresh render
                // so the (until now, deliberately skipped -- see
                // `OverviewState::is_dragging_selection`'s doc comment)
                // expensive lane/bottom-up rebuild finally runs exactly
                // once, against the range the user actually settled on.
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|panel, _, _window, cx| {
                        panel.record.overview.end_drag();
                        cx.notify();
                    }),
                )
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
        .on_drag_move({
            let panel_entity = panel_entity.clone();
            move |event: &DragMoveEvent<PanSelectionDrag>, _window, cx| {
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
            }
        })
        .on_mouse_up(
            MouseButton::Left,
            move |_, _window, cx| {
                panel_entity.update(cx, |panel, cx| {
                    panel.record.overview.end_drag();
                    cx.notify();
                });
            },
        )
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
        .on_drag_move({
            let panel_entity = panel_entity.clone();
            move |event: &DragMoveEvent<LeftEdgeDrag>, _window, cx| {
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
                    panel.record.selection = Some((
                        fixed_ns.min(current_ns),
                        fixed_ns.max(current_ns).max(fixed_ns + 1),
                    ));
                    panel.record.overview.hover_ns = Some(current_ns);
                    cx.notify();
                });
            }
        })
        .on_mouse_up(
            MouseButton::Left,
            move |_, _window, cx| {
                panel_entity.update(cx, |panel, cx| {
                    panel.record.overview.end_drag();
                    cx.notify();
                });
            },
        )
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
        .on_drag_move({
            let panel_entity = panel_entity.clone();
            move |event: &DragMoveEvent<RightEdgeDrag>, _window, cx| {
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
                    panel.record.selection = Some((
                        fixed_ns.min(current_ns),
                        fixed_ns.max(current_ns).max(fixed_ns + 1),
                    ));
                    panel.record.overview.hover_ns = Some(current_ns);
                    cx.notify();
                });
            }
        })
        .on_mouse_up(
            MouseButton::Left,
            move |_, _window, cx| {
                panel_entity.update(cx, |panel, cx| {
                    panel.record.overview.end_drag();
                    cx.notify();
                });
            },
        )
        .into_any_element()
}

/// Appends this bucket's long-task hatch instance, if it has one — the one
/// piece of the old `push_cpu_stack_segments` (see its git history) that's
/// still a plain instanced rect: a thin warning stripe along the top edge
/// reads fine blocky, unlike the stacked category fill itself, which moved
/// to [`build_stacked_area_bands`]'s smooth curve (see that function's own
/// doc comment for why).
fn push_long_task_hatch(instances: &mut Vec<BarInstance>, bucket: &OverviewBucket, x: f32, width: f32, scale: f32) {
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
/// (out of `max_ns`). Pulled out into its own function so the stacking math
/// has exactly one implementation to unit-test (see `tests::stack_segment_*`
/// below) instead of re-deriving the two fractions inline for every
/// category of every bucket -- shared by [`build_stacked_area_bands`]
/// (sampled once per bucket *center* into a curve) the same way it used to
/// be shared by the old per-bucket flat-rectangle approach.
fn stack_segment_y(cumulative_before: u64, cumulative_after: u64, max_ns: u64, graph_height: f32) -> (f32, f32) {
    let max_ns = max_ns.max(1) as f32;
    let bottom = graph_height - (cumulative_before as f32 / max_ns) * graph_height;
    let top = graph_height - (cumulative_after as f32 / max_ns) * graph_height;
    (top, bottom)
}

/// Builds one smooth, closed "band" shape per [`OVERVIEW_CATEGORIES`] entry
/// that has any nonzero contribution across `buckets` — the stacked CPU
/// activity graph itself. Each band is the region between two natural
/// (Catmull-Rom-to-cubic-bezier — matching `crate::plot::shape::{Line,
/// Area}`'s own "Natural" interpolation, see [`trace_natural_curve`])
/// curves: this category's cumulative top edge, and its cumulative bottom
/// edge (== the previous category's top edge) — the exact same stacking
/// math [`stack_segment_y`] already computed for the old per-bucket flat
/// rectangles, just sampled once per bucket *center* into a curve instead
/// of turned into one flat-topped `BarInstance` per bucket.
///
/// Replaces what used to be up to `OVERVIEW_BUCKET_COUNT *
/// OVERVIEW_CATEGORIES.len()` (400 × 8 = 3,200) GPU instances for this one
/// chart with at most `OVERVIEW_CATEGORIES.len()` = 8 tessellated paths,
/// each a single draw call — both the smoother curve look and a real
/// reduction in per-frame GPU work behind it, not a purely visual change.
/// Returned as plain point data (no path tessellation, no color, no
/// `Window`/`App` dependency) so the caller can build this from
/// `Context<ProfilerPanel>` (needed for `category_color`) and hand it off
/// to a plain `canvas()` paint closure (which only has a bare `&mut App`)
/// to actually turn into `Path`s and paint — see [`paint_stacked_area_bands`].
fn build_stacked_area_bands(
    buckets: &[OverviewBucket],
    max_ns: u64,
    bucket_width_px: f32,
) -> Vec<(usize, Vec<Point<Pixels>>, Vec<Point<Pixels>>)> {
    let n = buckets.len();
    if n == 0 {
        return Vec::new();
    }

    let mut cumulative_ns = vec![0u64; n];
    let mut bands = Vec::with_capacity(OVERVIEW_CATEGORIES.len());

    for category_index in 0..OVERVIEW_CATEGORIES.len() {
        let mut top_points = Vec::with_capacity(n);
        let mut bottom_points = Vec::with_capacity(n);
        let mut any_nonzero = false;

        for (i, bucket) in buckets.iter().enumerate() {
            // Sampled at each bucket's *center*, not its left edge -- a
            // curve through left edges would visually shift the whole
            // graph half a bucket earlier than where the data actually is.
            let x = (i as f32 + 0.5) * bucket_width_px;
            let cumulative_before = cumulative_ns[i];
            let segment_ns = bucket.category_ns[category_index];
            let cumulative_after = cumulative_before + segment_ns;
            if segment_ns > 0 {
                any_nonzero = true;
            }
            let (top, bottom) =
                stack_segment_y(cumulative_before, cumulative_after, max_ns, CPU_GRAPH_HEIGHT);
            top_points.push(point(px(x), px(top)));
            bottom_points.push(point(px(x), px(bottom)));
            cumulative_ns[i] = cumulative_after;
        }

        if any_nonzero {
            bands.push((category_index, top_points, bottom_points));
        }
    }

    bands
}

/// Traces a smooth curve through `points` via the exact Catmull-Rom-to-
/// cubic-bezier construction `crate::plot::shape::{Line, Area}` already use
/// for their own "Natural" stroke style — duplicated rather than reused
/// because this file needs the curve as one edge of a larger closed band
/// `Path` (see [`paint_stacked_area_bands`]), not a whole standalone
/// stroked/filled `Path` the way `Line`/`Area` build for themselves. Traces
/// from wherever the builder's pen currently is through every point in
/// `points` in order; call `builder.move_to(points[0])` yourself first if
/// this is the start of a new subpath rather than a continuation.
fn trace_natural_curve(builder: &mut PathBuilder, points: &[Point<Pixels>]) {
    let n = points.len();
    if n < 2 {
        return;
    }
    for i in 0..n - 1 {
        let p0 = if i == 0 { points[0] } else { points[i - 1] };
        let p1 = points[i];
        let p2 = points[i + 1];
        let p3 = if i + 2 < n { points[i + 2] } else { points[n - 1] };

        // Catmull-Rom to Bezier.
        let c1 = Point::new(p1.x + (p2.x - p0.x) / 6.0, p1.y + (p2.y - p0.y) / 6.0);
        let c2 = Point::new(p2.x - (p3.x - p1.x) / 6.0, p2.y - (p3.y - p1.y) / 6.0);
        builder.cubic_bezier_to(p2, c1, c2);
    }
}

/// Turns [`build_stacked_area_bands`]'s plain point data into actual
/// `Path`s and paints them, one per band, back to front in
/// [`OVERVIEW_CATEGORIES`] order (depth-0 first) — called from inside a
/// `canvas()` paint closure, the one place in this render pass that
/// actually has paint access. `origin` is the painting canvas element's own
/// screen-space top-left (its `Bounds::origin`); `bands`' own points are in
/// local, canvas-relative space, matching every other builder in this file.
fn paint_stacked_area_bands(
    bands: &[(usize, Vec<Point<Pixels>>, Vec<Point<Pixels>>)],
    origin: Point<Pixels>,
    colors: &[Hsla],
    window: &mut Window,
) {
    let offset = |p: &Point<Pixels>| point(p.x + origin.x, p.y + origin.y);

    for (category_index, top_points, bottom_points) in bands {
        if top_points.is_empty() {
            continue;
        }
        let Some(&color) = colors.get(*category_index) else {
            continue;
        };

        let top: Vec<Point<Pixels>> = top_points.iter().map(offset).collect();
        let bottom_rev: Vec<Point<Pixels>> = bottom_points.iter().rev().map(offset).collect();

        let mut builder = PathBuilder::fill();
        builder.move_to(top[0]);
        trace_natural_curve(&mut builder, &top);
        // Close the band: a straight connector down to this category's
        // bottom edge at the same x (the right edge of the last bucket),
        // the bottom curve traced backward, then a straight connector back
        // up to the top curve's own start (the left edge of the first
        // bucket) -- `top_points`/`bottom_points` share the same x per
        // index by construction, so both connectors are simple verticals.
        builder.line_to(bottom_rev[0]);
        trace_natural_curve(&mut builder, &bottom_rev);
        builder.line_to(top[0]);
        builder.close();

        if let Ok(path) = builder.build() {
            window.paint_path(path, color);
        }
    }
}

/// Above this many recorded frames, [`push_frame_row_segments`] switches
/// from one rectangle per frame to grouping consecutive frames into this
/// many buckets instead — a resolution limit, not a length limit, matching
/// how [`super::data::OVERVIEW_BUCKET_COUNT`] already caps the CPU graph
/// regardless of the capture's own length (see that constant's own doc
/// comment). Without this, the Frames row was the one part of this strip
/// that scaled with capture length rather than screen resolution: a
/// ten-minute capture at 60fps is 36,000 frames, which used to mean 36,000
/// `BarInstance`s rebuilt from scratch on every render (this function isn't
/// cached — see `overview::render`'s own doc comment on why the strip as a
/// whole doesn't need to be) — density the viewer can never actually
/// resolve past a few hundred pixels wide, and paid for anyway. Set equal
/// to the CPU graph's own bucket budget so neither row is the odd one out.
const FRAME_ROW_MAX_SEGMENTS: usize = OVERVIEW_BUCKET_COUNT;

/// How many consecutive frames [`push_frame_row_segments`] groups into one
/// rendered segment, given `frame_count` recorded frames total — always `1`
/// (one segment per frame, today's exact behavior) at or under
/// [`FRAME_ROW_MAX_SEGMENTS`] frames, growing only once there are more
/// frames than the resolution budget has room for.
fn frame_row_bucket_size(frame_count: usize) -> usize {
    ((frame_count + FRAME_ROW_MAX_SEGMENTS - 1) / FRAME_ROW_MAX_SEGMENTS).max(1)
}

/// One rendered Frames-row segment: `(start_ns, duration_ns, worst_ms)` for
/// either a single frame or (above [`FRAME_ROW_MAX_SEGMENTS`] frames) a
/// bucket of consecutive ones grouped by [`frame_row_bucket_size`] — see
/// [`push_frame_row_segments`]'s own doc comment for why this exists at all.
/// A pure function over `frame_durations_ms` (no `Context`/GPU dependency)
/// specifically so the bucketing math itself — not just its resulting
/// pixels — has something to unit-test.
fn bucket_frame_durations(frame_durations_ms: &[f32]) -> Vec<(f64, f64, f32)> {
    let bucket_size = frame_row_bucket_size(frame_durations_ms.len());
    let mut cumulative_ns_before: f64 = 0.0;
    frame_durations_ms
        .chunks(bucket_size)
        .map(|bucket| {
            let bucket_start_ns = cumulative_ns_before;
            let mut bucket_duration_ns: f64 = 0.0;
            // The *worst* frame in the bucket drives its color, not the
            // average -- a profiler's whole point is surfacing jank, and
            // averaging one slow frame in among many fast ones would smooth
            // away exactly the thing worth still seeing when zoomed out.
            let mut worst_ms: f32 = 0.0;
            for &duration_ms in bucket {
                bucket_duration_ns += (duration_ms as f64) * 1.0e6;
                worst_ms = worst_ms.max(duration_ms);
            }
            cumulative_ns_before += bucket_duration_ns;
            (bucket_start_ns, bucket_duration_ns, worst_ms)
        })
        .collect()
}

/// Appends one [`BarInstance`] per entry in `frame_durations_ms` (or, above
/// [`FRAME_ROW_MAX_SEGMENTS`] frames, one per fixed-size bucket of
/// consecutive frames — see [`bucket_frame_durations`]), colored by
/// Chrome's own three-tier frame-time classification (see [`frame_class`]),
/// laid out left-to-right along the Frames row using [`frame_x_range`].
///
/// The bucketing is a resolution limit, not a length limit, matching how
/// [`super::data::OVERVIEW_BUCKET_COUNT`] already caps the CPU graph
/// regardless of the capture's own length (see that constant's own doc
/// comment). Without it, the Frames row was the one part of this strip that
/// scaled with capture length rather than screen resolution: a ten-minute
/// capture at 60fps is 36,000 frames, which used to mean 36,000
/// `BarInstance`s rebuilt from scratch on every render (this function isn't
/// cached — see `overview::render`'s own doc comment on why the strip as a
/// whole doesn't need to be) — density the viewer can never actually
/// resolve past a few hundred pixels wide, and paid for anyway.
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

    for (bucket_start_ns, bucket_duration_ns, worst_ms) in bucket_frame_durations(frame_durations_ms) {
        let (x0, x1) =
            frame_x_range(bucket_start_ns, bucket_duration_ns, domain_span_ns, chart_width);
        let rgba = frame_class(worst_ms).color(cx).to_rgb();
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
/// Positioned just *below* the strip near the hovered x (not directly under
/// the cursor, which would put the popup under the finger/pointer doing the
/// hovering) and clamped so it never runs off either edge of the strip.
/// Below rather than above because there's reliably open space there —
/// above the strip risks running into whatever's docked above the Record
/// tab (or the window's own top edge when the strip is scrolled to the
/// top), while the space below the strip is always free.
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
            .top(px(STRIP_HEIGHT + 6.0))
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

    fn bucket_with(category_ns: [u64; OVERVIEW_CATEGORIES.len()]) -> OverviewBucket {
        OverviewBucket {
            total_ns: category_ns.iter().sum(),
            category_ns,
            has_long_task: false,
        }
    }

    #[test]
    fn build_stacked_area_bands_is_empty_for_no_buckets() {
        assert!(build_stacked_area_bands(&[], 100, 10.0).is_empty());
    }

    #[test]
    fn build_stacked_area_bands_skips_categories_with_no_contribution() {
        let mut category_ns = [0u64; OVERVIEW_CATEGORIES.len()];
        category_ns[0] = 50;
        let buckets = vec![bucket_with(category_ns)];

        let bands = build_stacked_area_bands(&buckets, 100, 10.0);

        assert_eq!(bands.len(), 1);
        assert_eq!(bands[0].0, 0);
    }

    #[test]
    fn build_stacked_area_bands_samples_one_point_per_bucket_at_its_center() {
        let mut category_ns = [0u64; OVERVIEW_CATEGORIES.len()];
        category_ns[0] = 25;
        let buckets = vec![bucket_with(category_ns), bucket_with(category_ns)];

        let bands = build_stacked_area_bands(&buckets, 100, 10.0);
        let (_, top_points, bottom_points) = &bands[0];

        assert_eq!(top_points.len(), 2);
        assert_eq!(bottom_points.len(), 2);
        // Bucket 0 spans [0, 10), so its sample sits at its center, x = 5;
        // bucket 1 spans [10, 20), center x = 15.
        assert_eq!(top_points[0].x, px(5.0));
        assert_eq!(top_points[1].x, px(15.0));
    }

    #[test]
    fn build_stacked_area_bands_stacks_later_categories_above_earlier_ones() {
        let mut category_ns = [0u64; OVERVIEW_CATEGORIES.len()];
        category_ns[0] = 50;
        category_ns[1] = 50;
        let buckets = vec![bucket_with(category_ns)];

        let bands = build_stacked_area_bands(&buckets, 100, 10.0);

        assert_eq!(bands.len(), 2);
        // Category 0 (pushed first, bottom of the stack) sits at the very
        // bottom of a fully-utilized (50 + 50 == max_ns) graph.
        assert_eq!(bands[0].2[0].y, px(CPU_GRAPH_HEIGHT));
        // Category 1 stacks directly on top of it, reaching the very top.
        assert_eq!(bands[1].1[0].y, px(0.0));
        // And the two categories share a seam: category 0's top edge is
        // exactly category 1's bottom edge, with no gap or overlap.
        assert_eq!(bands[0].1[0].y, bands[1].2[0].y);
    }

    #[test]
    fn trace_natural_curve_handles_fewer_than_two_points_without_panicking() {
        let mut builder = PathBuilder::fill();
        builder.move_to(point(px(0.0), px(0.0)));
        trace_natural_curve(&mut builder, &[]);
        trace_natural_curve(&mut builder, &[point(px(1.0), px(1.0))]);
        // No assertion beyond "didn't panic" -- there's no meaningful curve
        // to trace through 0 or 1 points, and callers (this file's own
        // `paint_stacked_area_bands`) already guard the real degenerate
        // case (an empty band) before ever reaching this function.
    }

    #[test]
    fn frame_row_bucket_size_is_one_at_or_under_the_budget() {
        assert_eq!(frame_row_bucket_size(0), 1);
        assert_eq!(frame_row_bucket_size(1), 1);
        assert_eq!(frame_row_bucket_size(FRAME_ROW_MAX_SEGMENTS), 1);
    }

    #[test]
    fn frame_row_bucket_size_grows_only_once_over_the_budget() {
        // One more frame than the budget still needs *some* bucket wider
        // than one frame to fit within it.
        assert!(frame_row_bucket_size(FRAME_ROW_MAX_SEGMENTS + 1) >= 2);
        // An order of magnitude over budget: still capped to (about) the
        // budget's own bucket count, not left to grow one-bucket-per-frame.
        let huge = FRAME_ROW_MAX_SEGMENTS * 20;
        let bucket_size = frame_row_bucket_size(huge);
        let bucket_count = (huge + bucket_size - 1) / bucket_size;
        assert!(bucket_count <= FRAME_ROW_MAX_SEGMENTS);
    }

    #[test]
    fn bucket_frame_durations_is_one_bucket_per_frame_under_budget() {
        let durations = vec![16.0_f32, 8.0, 33.0];
        let buckets = bucket_frame_durations(&durations);
        assert_eq!(buckets.len(), durations.len());
        // Each single-frame bucket's "worst" is just that frame's own ms.
        for (bucket, &expected_ms) in buckets.iter().zip(&durations) {
            assert_eq!(bucket.2, expected_ms);
        }
    }

    #[test]
    fn bucket_frame_durations_never_produces_more_buckets_than_the_budget() {
        let durations = vec![16.0_f32; FRAME_ROW_MAX_SEGMENTS * 5];
        let buckets = bucket_frame_durations(&durations);
        assert!(buckets.len() <= FRAME_ROW_MAX_SEGMENTS);
    }

    #[test]
    fn bucket_frame_durations_keeps_the_worst_frame_in_a_bucket_not_the_average() {
        // Well over budget, so frames actually get grouped -- one severe
        // spike among many fast frames in the same bucket. Averaging would
        // report a merely-Warn-looking duration for that bucket and hide
        // the spike entirely; taking the worst frame doesn't.
        let mut durations = vec![1.0_f32; FRAME_ROW_MAX_SEGMENTS * 3];
        durations[1] = 200.0;
        let buckets = bucket_frame_durations(&durations);
        let bucket_size = frame_row_bucket_size(durations.len());
        assert!(bucket_size > 1, "test needs bucketing to actually kick in");
        // Frame index 1 falls in the very first bucket.
        assert_eq!(buckets[0].2, 200.0);
    }

    #[test]
    fn bucket_frame_durations_sums_to_the_total_duration() {
        let durations = vec![16.0_f32; FRAME_ROW_MAX_SEGMENTS * 3];
        let expected_total_ns: f64 = durations.iter().map(|d| *d as f64 * 1.0e6).sum();
        let total_ns: f64 = bucket_frame_durations(&durations)
            .iter()
            .map(|(_, duration_ns, _)| duration_ns)
            .sum();
        assert!((total_ns - expected_total_ns).abs() < 1.0);
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

//! Profiler tab for the Inspector devtools panel (Phase 7 of the profiling
//! epic, `Far-Beyond-Pulsar/WGPUI#64`): a viewer built directly on top of
//! `gpui-ce`'s `flamegraph`/`flamegraph_replay`/`flamegraph_ui_capture`
//! capture engine. Sibling to `inspector.rs` rather than folded into it,
//! matching how `code_editor.rs`/`diagnostics.rs` already sit alongside it
//! as their own files.
//!
//! Gated behind `feature = "flamegraph"` on top of the Inspector panel's own
//! `any(feature = "inspector", debug_assertions)` gate (see `lib.rs`), since
//! this module depends on `gpui-ce`'s `flamegraph`-gated
//! `InspectorTab::Profiler` variant and the whole capture/replay API.
//!
//! # State ownership
//!
//! `gpui-ce`'s `Inspector` type owns no Profiler-specific state (it can't:
//! it lives in a different crate and predates this tab). [`ProfilerPanel`]
//! is this crate's own `Entity`, created lazily on first render of the
//! Profiler tab and cached as an `App`-level [`Global`] (mirroring the
//! `OnceCell`-cached `DivInspector` singleton `inspector.rs::init` already
//! uses for style-editor state) so capture sessions, replay cursors, and
//! poll tasks survive across re-renders of the outer `Inspector` panel.
//!
//! # Documented deferrals
//!
//! - **UI tree → Elements/Styles/Layout cross-wiring.** The per-request
//!   ask was to wire "click-to-inspect" from a captured UI-tree node toward
//!   the existing tabs where reasonable, and to cut it cleanly if fiddly.
//!   It's a hard blocker, not just fiddly: `Inspector::set_active_element_id`
//!   needs a real `InspectorElementId` (a `Rc<InspectorElementPath>` built
//!   from a live `GlobalElementId` observed during element construction).
//!   [`gpui::UiElementNode`] only carries a `global_id_hash` (a one-way hash
//!   of that id, captured Phase 5's own module doc notes is for joining
//!   *between* flamegraph captures, not for reconstructing a live id to
//!   feed back into the picking/selection system). There is no path from a
//!   hash back to a `GlobalElementId`, so this tab's UI-tree view is a
//!   clean, standalone tree instead.
//! - **Flame chart zoom/pan.** Explicitly called out as optional
//!   ("if you have time") in the request; the chart already renders full
//!   nested CPU+GPU spans, is searchable, and scrolls horizontally, which
//!   covers the stated minimum. Left for a follow-up.
//! - **GPU deep-capture live preview beyond `Quads`.** Matches
//!   `flamegraph_replay::render_deep_capture_step`'s own documented scope:
//!   only `Quads` draw calls get a real re-rendered pixel preview this
//!   round; `MonoSprites`/`PolySprites`/`Surfaces` degrade to a generated
//!   checkerboard placeholder, and everything else reports "no replay
//!   pipeline wired up yet" rather than erroring. This tab surfaces exactly
//!   that distinction rather than working around it.

use std::{
    collections::HashSet,
    ops::Range,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui::{
    canvas, div, img, point, prelude::FluentBuilder as _, px, uniform_list, AnyElement, App,
    AppContext as _, Bounds, ClickEvent, Context, DeepCaptureDrawCall, DeepCaptureReplay,
    DragMoveEvent, DrawCallResourceStatus, Empty, Entity, FontWeight, Global, ImageSource,
    Inspector, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, Pixels, Point, Render, RenderImage, ScrollWheelEvent,
    SharedString, StatefulInteractiveElement as _, Styled, Subscription, Task, Timer,
    UiElementNode, UiTreeReplay, Window, wgpu_surface,
};
// `wgpu` and `bytemuck` are used fully-qualified below (both are direct,
// `flamegraph`-gated dependencies of this crate; see `Cargo.toml`), matching
// how `wgpu::Device`/`wgpu::Queue` are already referenced elsewhere in this
// file for the GPU deep-capture preview.
use wgpu::util::DeviceExt as _;

use crate::{
    alert::Alert,
    button::{Button, ButtonVariants as _},
    description_list::DescriptionList,
    h_flex,
    input::{InputEvent, InputState, TextInput},
    spinner::Spinner,
    styled::Disableable as _,
    tab::{Tab, TabBar},
    tooltip::Tooltip,
    v_flex, ActiveTheme, IconName, Sizable as _,
};

/// How many 50ms polls to wait for an on-demand capture (`DeepCapture`/
/// `UiTreeCapture`) to complete before giving up and surfacing a timeout.
/// Both complete on "the very next drawn frame", so 3s is generous slack for
/// a window that's momentarily idle/occluded rather than never drawing again.
const CAPTURE_POLL_TIMEOUT_ATTEMPTS: u32 = 60;
const CAPTURE_POLL_INTERVAL: Duration = Duration::from_millis(50);

// ── Zoom / pan math ──────────────────────────────────────────────────────
//
// Pan+zoom interaction (flame chart time axis, counters sparkline frame-index
// axis, and the GPU deep-capture preview's 2D image axes) is unified behind
// one pure, unit-testable primitive: `RangeZoom`, a `0.0..=1.0`-independent
// "visible window into a domain" that `zoom_at`/`pan_by_fraction` remap. Each
// view owns its own `RangeZoom` instance (they cover different domains and
// reset independently when their underlying data changes), but all three
// share this exact math rather than re-deriving it. The 2D preview composes
// two `RangeZoom`s (one per axis) via `ImageViewport` instead of duplicating
// the logic for two dimensions.

/// A zoomable/pannable "visible window" into a 1D domain (e.g. nanoseconds
/// within a captured frame, or frame indices across a capture). Rendering
/// code maps `visible_start()..visible_end()` onto whatever pixel span it has
/// available; this type only tracks *which* sub-range of the domain is
/// currently shown, independent of any pixel measurements.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RangeZoom {
    domain_start: f64,
    domain_end: f64,
    visible_start: f64,
    visible_end: f64,
}

impl RangeZoom {
    /// Zooming in stops once the visible window would be smaller than this
    /// fraction of the full domain — a generous ~500x maximum zoom that still
    /// keeps the window numerically well-behaved.
    const MIN_VISIBLE_FRACTION: f64 = 0.002;

    fn full(domain_start: f64, domain_end: f64) -> Self {
        let domain_end = domain_end.max(domain_start + 1.0);
        Self { domain_start, domain_end, visible_start: domain_start, visible_end: domain_end }
    }

    /// Updates the underlying domain (e.g. a newly selected capture frame's
    /// time range, or a capture with a different frame count). If the domain
    /// actually changed, the visible window resets to "fit all" — switching
    /// to different data shouldn't leave a stale zoom window pointed at the
    /// wrong range. Re-rendering the *same* domain preserves the current
    /// zoom/pan untouched.
    fn set_domain(&mut self, domain_start: f64, domain_end: f64) {
        let domain_end = domain_end.max(domain_start + 1.0);
        if (domain_start - self.domain_start).abs() > f64::EPSILON
            || (domain_end - self.domain_end).abs() > f64::EPSILON
        {
            self.domain_start = domain_start;
            self.domain_end = domain_end;
            self.visible_start = domain_start;
            self.visible_end = domain_end;
        }
    }

    fn domain_span(&self) -> f64 {
        (self.domain_end - self.domain_start).max(1.0)
    }

    fn visible_span(&self) -> f64 {
        (self.visible_end - self.visible_start).max(1e-6)
    }

    fn visible_start(&self) -> f64 {
        self.visible_start
    }

    fn visible_end(&self) -> f64 {
        self.visible_end
    }

    fn is_zoomed(&self) -> bool {
        self.visible_start > self.domain_start + 1e-6 || self.visible_end < self.domain_end - 1e-6
    }

    fn reset(&mut self) {
        self.visible_start = self.domain_start;
        self.visible_end = self.domain_end;
    }

    /// Zooms around a point expressed as a `0.0..=1.0` fraction across the
    /// *current* visible window (`0.0` = its left edge, `1.0` = its right
    /// edge). `factor` scales the visible span: `< 1.0` zooms in, `> 1.0`
    /// zooms out. The result is clamped so the window never extends past the
    /// domain and never shrinks past `MIN_VISIBLE_FRACTION` of it.
    fn zoom_at(&mut self, cursor_fraction: f32, factor: f32) {
        let cursor_fraction = cursor_fraction.clamp(0.0, 1.0) as f64;
        let factor = (factor as f64).max(0.01);
        let span = self.visible_span();
        let anchor = self.visible_start + cursor_fraction * span;
        let min_span = self.domain_span() * Self::MIN_VISIBLE_FRACTION;
        let new_span = (span * factor).clamp(min_span, self.domain_span());

        let mut new_start = anchor - cursor_fraction * new_span;
        let mut new_end = new_start + new_span;
        self.clamp_to_domain(&mut new_start, &mut new_end);
        self.visible_start = new_start;
        self.visible_end = new_end;
    }

    /// Pans by a delta expressed as a fraction of the current visible span
    /// (positive moves the window later/right, negative earlier/left),
    /// clamped to the domain.
    fn pan_by_fraction(&mut self, delta_fraction: f32) {
        let delta = delta_fraction as f64 * self.visible_span();
        let mut new_start = self.visible_start + delta;
        let mut new_end = self.visible_end + delta;
        self.clamp_to_domain(&mut new_start, &mut new_end);
        self.visible_start = new_start;
        self.visible_end = new_end;
    }

    /// Slides `[start, end]` back inside `[domain_start, domain_end]` without
    /// changing its span, then hard-clamps as a final guard against a span
    /// that (only possible via a degenerate domain) exceeds the domain size.
    fn clamp_to_domain(&self, start: &mut f64, end: &mut f64) {
        if *start < self.domain_start {
            let shift = self.domain_start - *start;
            *start += shift;
            *end += shift;
        }
        if *end > self.domain_end {
            let shift = *end - self.domain_end;
            *start -= shift;
            *end -= shift;
        }
        *start = start.max(self.domain_start);
        *end = end.min(self.domain_end);
    }

    /// Maps an absolute domain value to a `0.0..=1.0`-ish fraction of the
    /// current visible window (used to position elements along the axis;
    /// values outside the visible window map outside `0.0..=1.0`, which
    /// callers use to cull off-screen elements).
    fn value_to_fraction(&self, value: f64) -> f32 {
        ((value - self.visible_start) / self.visible_span()) as f32
    }
}

/// Converts a raw scroll-wheel vertical delta (in pixels) into a
/// multiplicative span factor for [`RangeZoom::zoom_at`]/[`ImageViewport::zoom_at`]:
/// `< 1.0` zooms in, `> 1.0` zooms out. Scrolling up/away (negative delta)
/// zooms in; scrolling down/toward the viewer (positive delta) zooms out.
/// The input is clamped so a single large trackpad flick can't jump the zoom
/// level by an extreme amount in one event.
fn zoom_factor_for_wheel_delta(delta_y: f32) -> f32 {
    let clamped = delta_y.clamp(-160.0, 160.0);
    (1.0 + clamped / 400.0).clamp(0.6, 1.4)
}

/// A zoomable/pannable 2D viewport into a source image, expressed as a pair
/// of independent [`RangeZoom`]s (one per axis) each over the normalized
/// `0.0..=1.0` domain of the image. Reuses `RangeZoom`'s math for both axes
/// instead of a bespoke 2D implementation.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ImageViewport {
    x: RangeZoom,
    y: RangeZoom,
}

impl ImageViewport {
    fn new() -> Self {
        Self { x: RangeZoom::full(0.0, 1.0), y: RangeZoom::full(0.0, 1.0) }
    }

    fn is_zoomed(&self) -> bool {
        self.x.is_zoomed() || self.y.is_zoomed()
    }

    fn reset(&mut self) {
        self.x.reset();
        self.y.reset();
    }

    fn zoom_at(&mut self, cursor_fraction_x: f32, cursor_fraction_y: f32, factor: f32) {
        self.x.zoom_at(cursor_fraction_x, factor);
        self.y.zoom_at(cursor_fraction_y, factor);
    }

    fn pan_by_fraction(&mut self, delta_fraction_x: f32, delta_fraction_y: f32) {
        self.x.pan_by_fraction(delta_fraction_x);
        self.y.pan_by_fraction(delta_fraction_y);
    }

    /// The visible crop window as `(x0, y0, x1, y1)` fractions of the full
    /// source image, all within `0.0..=1.0`.
    fn crop_fractions(&self) -> (f32, f32, f32, f32) {
        (
            self.x.visible_start() as f32,
            self.y.visible_start() as f32,
            self.x.visible_end() as f32,
            self.y.visible_end() as f32,
        )
    }
}

/// Marker payload for the pan-via-drag gesture shared by the flame chart, the
/// counters sparkline, and the GPU deep-capture preview. GPUI's `on_drag` /
/// `on_drag_move` mechanism only cares about matching `TypeId`s, not which
/// element started the drag, so one marker is safe to reuse across all three:
/// the Profiler tab's sections render mutually exclusively (see
/// `ProfilerPanel::render`), so at most one of these views is ever mounted —
/// and thus draggable — at a time.
#[derive(Clone)]
struct ProfilerPanDrag;

impl Render for ProfilerPanDrag {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

// ── Entry point ──────────────────────────────────────────────────────────

/// Render the Profiler tab's content. Called from `inspector.rs`'s
/// `render_tab_content` match arm.
pub(crate) fn render_profiler_tab(window: &mut Window, cx: &mut Context<Inspector>) -> AnyElement {
    let panel = profiler_panel(window, cx);
    panel.update(cx, |state, cx| state.render(window, cx))
}

struct ProfilerGlobal(Entity<ProfilerPanel>);
impl Global for ProfilerGlobal {}

fn profiler_panel(window: &mut Window, cx: &mut App) -> Entity<ProfilerPanel> {
    if let Some(global) = cx.try_global::<ProfilerGlobal>() {
        return global.0.clone();
    }
    let entity = cx.new(|cx| ProfilerPanel::new(window, cx));
    cx.set_global(ProfilerGlobal(entity.clone()));
    entity
}

// ── Sections ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProfilerSection {
    FlameChart,
    Counters,
    Memory,
    UiTree,
    DeepCapture,
}

impl ProfilerSection {
    const ALL: [ProfilerSection; 5] = [
        ProfilerSection::FlameChart,
        ProfilerSection::Counters,
        ProfilerSection::Memory,
        ProfilerSection::UiTree,
        ProfilerSection::DeepCapture,
    ];

    fn label(self) -> &'static str {
        match self {
            ProfilerSection::FlameChart => "Flame Chart",
            ProfilerSection::Counters => "Counters",
            ProfilerSection::Memory => "Memory",
            ProfilerSection::UiTree => "UI Tree",
            ProfilerSection::DeepCapture => "GPU Deep Capture",
        }
    }
}

// ── Panel state ──────────────────────────────────────────────────────────

#[derive(Clone)]
struct SelectedSpan {
    name: SharedString,
    lane: SharedString,
    category_label: SharedString,
    depth: u16,
    duration_ns: u32,
    element_type: Option<SharedString>,
    element_source: Option<SharedString>,
}

enum DeepCapturePreview {
    Image {
        image: Arc<RenderImage>,
        width: u32,
        height: u32,
        texture_unavailable: bool,
    },
    Unavailable(SharedString),
}

/// Cached [`build_flame_lanes`] output for one `(capture_generation,
/// frame_index)` pair (see [`ProfilerPanel::flame_lane_cache`]).
struct FlameLaneCache {
    capture_generation: u64,
    frame_index: usize,
    lanes: Rc<Vec<FlameLane>>,
}

pub struct ProfilerPanel {
    section: ProfilerSection,

    // Flame chart / capture session (Phase 1-2: `Capture`/`CounterSummary`).
    capture_handle: Option<gpui::CaptureHandle>,
    capture: Option<gpui::Capture>,
    capture_error: Option<SharedString>,
    selected_frame: usize,
    selected_span: Option<SelectedSpan>,
    search_input: Entity<InputState>,
    flame_search: SharedString,

    // Bumped every time `self.capture` is replaced by a freshly stopped
    // capture (see `toggle_capture`). Keys `flame_lane_cache` below
    // alongside `selected_frame`, so a cache entry can never be mistaken for
    // the current capture's data even if a new capture happens to reuse the
    // same frame index as the previous one.
    capture_generation: u64,

    // Cached `build_flame_lanes` output for the current
    // `(capture_generation, selected_frame)` pair. Perf: `build_flame_lanes`
    // walks every span in the frame, allocating a `FlameBar` (and a
    // `SharedString` for its source location) per span -- rebuilding that on
    // every render, including renders triggered by hovering a bar or
    // panning/zooming the time axis that don't change which frame is
    // selected, was pure waste at real span counts. Cleared implicitly by
    // the generation bump above whenever a new capture replaces this one.
    flame_lane_cache: Option<FlameLaneCache>,

    // Counters (Phase 2): computed once in `toggle_capture` when a capture
    // stops, rather than re-aggregated from every recorded frame on every
    // render of the Counters tab -- panning/zooming/hovering the sparkline
    // below re-renders this whole section without the underlying capture
    // ever changing.
    counter_summary: Option<gpui::CounterSummary>,
    frame_durations_ms: Vec<f32>,
    frame_durations_max_ms: f32,

    // Flame chart time-axis pan/zoom. `flame_chart_bounds` is the last
    // measured screen bounds of the chart canvas (captured via a `canvas()`
    // overlay each render), used to turn scroll-wheel cursor positions into
    // zoom anchors. `flame_pan_last_x` tracks the previous pointer x during
    // an in-progress drag so each `on_drag_move` tick only needs to apply the
    // incremental delta.
    flame_zoom: RangeZoom,
    flame_chart_bounds: Bounds<Pixels>,
    flame_pan_last_x: Option<f32>,

    // GPU-rendered flame bars (perf fix, `Far-Beyond-Pulsar/WGPUI-Component`
    // flame-chart perf bug): one `wgpu_surface` + instanced draw call per
    // lane instead of one GPUI `div()` per span bar. `flame_bar_pipeline` is
    // compiled once and shared across every lane's own `FlameLaneGpu` (a
    // `wgpu::RenderPipeline` only depends on the shader + target format, not
    // on any lane's instance data). `flame_lane_gpu`/`flame_lane_bounds` are
    // indexed by the lane's position in `build_flame_lanes`'s output for the
    // currently selected frame; both are resized to match on every render.
    // `flame_gpu_unavailable` latches once `Window::create_wgpu_surface`
    // ever returns `None` (headless test platform, or a backend/platform
    // that doesn't support it), so the "GPU unavailable" notice doesn't
    // flicker if creation transiently fails on exactly one render.
    flame_bar_pipeline: Option<Rc<FlameBarPipeline>>,
    flame_lane_gpu: Vec<Option<FlameLaneGpu>>,
    flame_lane_bounds: Vec<Bounds<Pixels>>,
    flame_gpu_unavailable: bool,
    hovered_bar: Option<HoveredBar>,

    // Counters sparkline pan/zoom over the frame-index axis (same math as
    // `flame_zoom`, independent state since it's a different domain).
    counters_zoom: RangeZoom,
    counters_chart_bounds: Bounds<Pixels>,
    counters_pan_last_x: Option<f32>,

    // Memory (Phase 3: on-demand `MemorySnapshot`/`GpuMemorySnapshot`).
    memory_cpu: Option<gpui::MemorySnapshot>,
    memory_gpu: Option<gpui::GpuMemorySnapshot>,
    memory_error: Option<SharedString>,

    // UI tree (Phase 5/6: `UiTreeCapture` + `UiTreeReplay`).
    ui_tree_replay: Option<UiTreeReplay>,
    ui_tree_pending: bool,
    ui_tree_error: Option<SharedString>,
    ui_tree_selected: Option<usize>,
    ui_tree_collapsed: HashSet<usize>,
    ui_tree_poll_task: Option<Task<()>>,

    // GPU deep capture (Phase 4/6: `DeepCapture` + `DeepCaptureReplay`).
    deep_capture_replay: Option<DeepCaptureReplay>,
    deep_capture_pending: bool,
    deep_capture_error: Option<SharedString>,
    deep_capture_preview: Option<DeepCapturePreview>,
    deep_capture_poll_task: Option<Task<()>>,

    // GPU deep-capture preview 2D pan/zoom (image space), same shape as the
    // flame chart's fields above but for a 2D `ImageViewport`.
    deep_capture_view: ImageViewport,
    deep_capture_preview_bounds: Bounds<Pixels>,
    deep_capture_pan_last: Option<(f32, f32)>,
    // Measured bounds of the preview panel's own outer container (*not* the
    // fixed-width viewport div inside it -- that div's width is derived
    // *from* this measurement, so capturing bounds on it would be
    // circular). Lets the displayed preview image scale to the Inspector
    // sidebar's actual, user-resizable width instead of a hardcoded
    // constant.
    deep_capture_panel_bounds: Bounds<Pixels>,

    _subscriptions: Vec<Subscription>,
}

impl ProfilerPanel {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search spans by name..."));

        let _subscriptions = vec![cx.subscribe_in(
            &search_input,
            window,
            |this: &mut ProfilerPanel, state, event: &InputEvent, _window, cx| {
                if let InputEvent::Change = event {
                    this.flame_search = state.read(cx).value();
                    cx.notify();
                }
            },
        )];

        Self {
            section: ProfilerSection::FlameChart,

            capture_handle: None,
            capture: None,
            capture_error: None,
            selected_frame: 0,
            selected_span: None,
            search_input,
            flame_search: SharedString::default(),

            capture_generation: 0,
            flame_lane_cache: None,

            counter_summary: None,
            frame_durations_ms: Vec::new(),
            frame_durations_max_ms: 1.0,

            flame_zoom: RangeZoom::full(0.0, 1.0),
            flame_chart_bounds: Bounds::default(),
            flame_pan_last_x: None,

            flame_bar_pipeline: None,
            flame_lane_gpu: Vec::new(),
            flame_lane_bounds: Vec::new(),
            flame_gpu_unavailable: false,
            hovered_bar: None,

            counters_zoom: RangeZoom::full(0.0, 1.0),
            counters_chart_bounds: Bounds::default(),
            counters_pan_last_x: None,

            memory_cpu: None,
            memory_gpu: None,
            memory_error: None,

            ui_tree_replay: None,
            ui_tree_pending: false,
            ui_tree_error: None,
            ui_tree_selected: None,
            ui_tree_collapsed: HashSet::default(),
            ui_tree_poll_task: None,

            deep_capture_replay: None,
            deep_capture_pending: false,
            deep_capture_error: None,
            deep_capture_preview: None,
            deep_capture_poll_task: None,

            deep_capture_view: ImageViewport::new(),
            deep_capture_preview_bounds: Bounds::default(),
            deep_capture_pan_last: None,
            deep_capture_panel_bounds: Bounds::default(),

            _subscriptions,
        }
    }

    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .id("profiler-panel")
            .size_full()
            .child(self.render_section_tabs(cx))
            .child(
                div()
                    .id("profiler-section-body")
                    .flex_1()
                    .min_h(px(0.))
                    .when(self.section != ProfilerSection::FlameChart, |d| d.overflow_y_scroll())
                    .child(match self.section {
                        ProfilerSection::FlameChart => self.render_flame_chart_section(window, cx),
                        ProfilerSection::Counters => self.render_counters_section(cx),
                        ProfilerSection::Memory => self.render_memory_section(window, cx),
                        ProfilerSection::UiTree => self.render_ui_tree_section(window, cx),
                        ProfilerSection::DeepCapture => self.render_deep_capture_section(window, cx),
                    }),
            )
            .into_any_element()
    }

    fn render_section_tabs(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let sections = ProfilerSection::ALL;
        let active_idx = sections.iter().position(|s| *s == self.section).unwrap_or(0);
        let tab_labels: Vec<(Option<SharedString>, bool)> = sections
            .iter()
            .map(|s| (Some(SharedString::from(s.label())), false))
            .collect();

        TabBar::new("profiler-sections")
            .underline()
            .small()
            .selected_index(active_idx)
            .on_click({
                let entity = cx.entity().clone();
                move |idx, _, cx: &mut App| {
                    let idx = *idx;
                    entity.update(cx, |state, cx| {
                        state.section = ProfilerSection::ALL[idx];
                        cx.notify();
                    });
                }
            })
            .build_tabs(sections.len(), tab_labels, move |ix, _, _| {
                Tab::new(ProfilerSection::ALL[ix].label())
            })
            .into_any_element()
    }

    // ── Flame chart ──────────────────────────────────────────────────

    fn toggle_capture(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.capture_handle.take() {
            let capture = handle.stop();
            // The last handful of frames in a stopped capture trail off:
            // whatever frame was still open (mid-`Window::draw`) at the
            // exact instant `stop()` was called gets force-closed into the
            // buffer by `finalize_open_frames` rather than dropped, but it
            // only recorded however many spans happened before the
            // interruption -- often one or two, not zero, so a bare
            // "non-empty" check still lands on a near-empty frame that
            // *looks* broken. Pick the last frame whose total span count is
            // at least a fifth of the healthiest recent frame's, which
            // reliably skips the trailing partial frames while still
            // landing near the actual end of the capture.
            let frames: Vec<_> = capture.frames().collect();
            let span_count = |frame: &&gpui::FrameCapture| {
                frame.cpu_spans.len() + frame.background_spans.len() + frame.gpu_spans.len()
            };
            let recent_max = frames.iter().rev().take(10).map(span_count).max().unwrap_or(0);
            let threshold = (recent_max / 5).max(1);
            let healthy_last = frames
                .iter()
                .enumerate()
                .rev()
                .find(|(_, frame)| span_count(frame) >= threshold)
                .map(|(index, _)| index);

            // Counters/sparkline data computed once here, right as the
            // capture stops, rather than re-aggregated from every recorded
            // frame on every render of the Counters tab (see
            // `counter_summary`/`frame_durations_ms`'s field docs).
            let frame_durations_ms: Vec<f32> = frames
                .iter()
                .map(|f| (f.frame_end_ns.saturating_sub(f.frame_start_ns)) as f32 / 1.0e6)
                .collect();
            let frame_durations_max_ms = frame_durations_ms.iter().copied().fold(0.0f32, f32::max).max(1.0);
            let counter_summary = capture.counter_summary();

            self.selected_frame = healthy_last.unwrap_or_else(|| capture.frame_count().saturating_sub(1));
            self.capture = Some(capture);
            self.counter_summary = Some(counter_summary);
            self.frame_durations_ms = frame_durations_ms;
            self.frame_durations_max_ms = frame_durations_max_ms;
            self.selected_span = None;
            self.capture_error = None;
            // Invalidates `flame_lane_cache`: even if this new capture's
            // `selected_frame` happens to match the previous capture's, the
            // generation mismatch forces a rebuild rather than reusing lanes
            // built from the old capture's data.
            self.capture_generation = self.capture_generation.wrapping_add(1);
        } else {
            match gpui::start_capture(gpui::CaptureOptions::default()) {
                Ok(handle) => {
                    self.capture_handle = Some(handle);
                    self.capture = None;
                    self.selected_span = None;
                    self.capture_error = None;
                    self.counter_summary = None;
                    self.frame_durations_ms = Vec::new();
                    self.frame_durations_max_ms = 1.0;
                }
                Err(_already_capturing) => {
                    self.capture_error = Some(
                        "A flamegraph capture is already running elsewhere in this process."
                            .into(),
                    );
                }
            }
        }
        cx.notify();
    }

    fn render_flame_chart_section(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let is_recording = self.capture_handle.is_some();
        let frame_count = self.capture.as_ref().map(|c| c.frame_count()).unwrap_or(0);

        // Keep the time-axis zoom's domain in sync with the selected frame
        // *before* building the header, so the zoom controls below (and
        // their disabled/enabled state) reflect the frame actually on
        // screen. `RangeZoom::set_domain` only resets the visible window
        // when the domain itself changed (i.e. a different frame is now
        // selected), so zoom/pan survives unrelated re-renders of the same
        // frame (e.g. selecting a span).
        if frame_count > 0 {
            let frame_index = self.selected_frame.min(frame_count - 1);
            if let Some(frame) = self.capture.as_ref().and_then(|c| c.frames().nth(frame_index)) {
                let domain_start = frame.frame_start_ns as f64;
                let domain_end = frame.frame_end_ns.max(frame.frame_start_ns + 1) as f64;
                self.flame_zoom.set_domain(domain_start, domain_end);
            }
        }
        let flame_zoomed = self.flame_zoom.is_zoomed();

        let mut header = h_flex().gap_2().items_center().flex_wrap().p_2().child(
            Button::new("flame-toggle-capture")
                .small()
                .when(is_recording, |b| b.danger())
                .when(!is_recording, |b| b.primary())
                .icon(if is_recording { IconName::Pause } else { IconName::Play })
                .label(if is_recording { "Stop Capture" } else { "Start Capture" })
                .on_click(cx.listener(|state, _, _window, cx| state.toggle_capture(cx))),
        );

        if frame_count > 0 {
            let at_start = self.selected_frame == 0;
            let at_end = self.selected_frame + 1 >= frame_count;
            header = header.child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Button::new("flame-prev")
                            .xsmall()
                            .ghost()
                            .icon(IconName::ChevronLeft)
                            .disabled(at_start)
                            .on_click(cx.listener(|state, _, _window, cx| {
                                state.selected_frame = state.selected_frame.saturating_sub(1);
                                state.selected_span = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .min_w(px(90.))
                            .child(format!("Frame {}/{}", self.selected_frame + 1, frame_count)),
                    )
                    .child(
                        Button::new("flame-next")
                            .xsmall()
                            .ghost()
                            .icon(IconName::ChevronRight)
                            .disabled(at_end)
                            .on_click(cx.listener(|state, _, _window, cx| {
                                let frame_count =
                                    state.capture.as_ref().map(|c| c.frame_count()).unwrap_or(0);
                                if state.selected_frame + 1 < frame_count {
                                    state.selected_frame += 1;
                                }
                                state.selected_span = None;
                                cx.notify();
                            })),
                    ),
            );
        }

        header = header.child(div().flex_1());
        header = header.child(TextInput::new(&self.search_input).small().w(px(180.)));
        header = header.child(render_zoom_controls(
            "flame",
            flame_zoomed,
            cx.listener(|state, _, _window, cx| {
                state.flame_zoom.zoom_at(0.5, 1.25);
                cx.notify();
            }),
            cx.listener(|state, _, _window, cx| {
                state.flame_zoom.zoom_at(0.5, 0.8);
                cx.notify();
            }),
            cx.listener(|state, _, _window, cx| {
                state.flame_zoom.reset();
                cx.notify();
            }),
            cx,
        ));

        let mut body = v_flex().gap_2().size_full().child(header);
        if let Some(err) = self.capture_error.clone() {
            body = body.child(Alert::error("flame-capture-error", err));
        }
        if self.capture.is_some() {
            body = body.child(zoom_pan_hint(cx));
        }
        body = body.child(
            div()
                .id("flame-chart-scroll")
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .px_2()
                .pb_2()
                .child(self.render_flame_chart_body(window, cx)),
        );
        if let Some(span) = self.selected_span.clone() {
            body = body.child(self.render_selected_span_details(&span, cx));
        }

        body.into_any_element()
    }

    fn render_flame_chart_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(capture) = self.capture.as_ref() else {
            return profiler_empty_state(
                "Start a capture, interact with the app, then stop it to inspect recorded frames.",
                cx,
            );
        };
        if capture.frame_count() == 0 {
            return profiler_empty_state("The capture stopped with no frames recorded.", cx);
        }

        let frame_index = self.selected_frame.min(capture.frame_count() - 1);
        let Some(frame) = capture.frames().nth(frame_index) else {
            return profiler_empty_state("Frame not found.", cx);
        };

        let cpu_gpu_submit_ns = frame.cpu_gpu_submit_ns;
        let cpu_gpu_fence_observed_ns = frame.cpu_gpu_fence_observed_ns;

        // Reuse the cached lane build for this exact `(capture_generation,
        // frame_index)` pair if there is one -- see `flame_lane_cache`'s
        // field doc. Only a genuinely different frame or capture pays for
        // `build_flame_lanes`; re-renders for hover/zoom/pan/search on the
        // *same* frame just clone the `Rc`.
        let cache_hit = self
            .flame_lane_cache
            .as_ref()
            .is_some_and(|c| c.capture_generation == self.capture_generation && c.frame_index == frame_index);
        let lanes = if cache_hit {
            self.flame_lane_cache.as_ref().unwrap().lanes.clone()
        } else {
            let lanes = Rc::new(build_flame_lanes(frame));
            self.flame_lane_cache = Some(FlameLaneCache {
                capture_generation: self.capture_generation,
                frame_index,
                lanes: lanes.clone(),
            });
            lanes
        };

        if lanes.is_empty() {
            return profiler_empty_state("This frame recorded no spans.", cx);
        }

        self.render_flame_lanes_body(
            &lanes,
            cpu_gpu_submit_ns,
            cpu_gpu_fence_observed_ns,
            window,
            cx,
        )
    }

    /// Renders the flame chart's interactive lane/bar area for an
    /// already-built `lanes` slice. Split out from [`Self::render_flame_chart_body`]
    /// so the flame-chart performance benchmark (see the `tests` module) can
    /// exercise the exact same rendering path against synthetic `FlameLane`
    /// data, without needing a live `gpui::Capture` session (which can only
    /// be produced by an actual `start_capture`/`stop` round trip).
    ///
    /// `cpu_gpu_submit_ns`/`cpu_gpu_fence_observed_ns` are the current
    /// frame's CPU-observed GPU submit/fence-observed instants (see
    /// `gpui::FrameCapture`'s doc comments), plotted as timeline markers
    /// alongside the calibrated GPU span bars when both are `Some`.
    fn render_flame_lanes_body(
        &mut self,
        lanes: &[FlameLane],
        cpu_gpu_submit_ns: Option<u64>,
        cpu_gpu_fence_observed_ns: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // `set_domain` already ran in `render_flame_chart_section` for this
        // same frame, so this just reads the (possibly zoomed/panned)
        // visible window back out.
        let visible_start_ns = self.flame_zoom.visible_start();
        let visible_end_ns = self.flame_zoom.visible_end();
        let visible_span_ns = self.flame_zoom.visible_span();

        // Row height is `FLAME_ROW_HEIGHT` (defined further down alongside
        // `hit_test_lane_bar`), not a separate local constant: the GPU
        // instance geometry built below and the hit-test geometry
        // `hit_test_lane_bar` scans against must agree on this value, or
        // hovering/clicking would silently target the wrong bar.
        // Approximates the label line (`text_xs` + `mt(6px)`) each lane used
        // to render above its bar row, plus the `gap_1()` between lane
        // blocks -- used only to estimate the lanes' total content height
        // for the timeline markers below, not for any hit-testing math.
        const LANE_LABEL_HEIGHT: f32 = 22.0;
        const LANE_GAP: f32 = 4.0;
        const DEFAULT_CHART_WIDTH: f32 = 900.0;
        // The chart canvas has no way to know its own rendered pixel width
        // before layout runs, so bar positions are computed against the
        // width measured on the *previous* render (captured via the
        // `canvas()` overlay below) — one frame of lag that's imperceptible
        // in practice and stabilizes immediately after the first paint.
        let measured_width = f32::from(self.flame_chart_bounds.size.width);
        let chart_width = if measured_width > 1.0 { measured_width } else { DEFAULT_CHART_WIDTH };

        let search = self.flame_search.to_lowercase();
        let selected_key = self
            .selected_span
            .as_ref()
            .map(|s| (s.name.clone(), s.depth, s.duration_ns));

        // Keep the per-lane GPU state and hit-test bounds sized to the
        // current lane count so a frame with fewer lanes than the last one
        // doesn't leave stale surfaces/bounds hanging around.
        self.flame_lane_gpu.truncate(lanes.len());
        self.flame_lane_bounds.resize(lanes.len(), Bounds::default());

        let mut lane_elements: Vec<AnyElement> = Vec::new();
        let mut total_content_height: f32 = 0.0;

        for (lane_index, lane) in lanes.iter().enumerate() {
            lane_elements.push(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .mt(px(6.))
                    .child(format!("{} ({} spans)", lane.label, lane.bars.len()))
                    .into_any_element(),
            );
            total_content_height += LANE_LABEL_HEIGHT;

            let lane_height = (lane.max_depth as f32 + 1.0) * FLAME_ROW_HEIGHT;
            total_content_height += lane_height + LANE_GAP;

            // One instance per visible bar for the GPU pass, plus an owned
            // "hit bar" list the hover/click handlers below can test against
            // without any per-bar GPUI element -- built once, from the same
            // culled set, so the two can never disagree about which bars
            // are on screen.
            //
            // `gpu_available` short-circuits the whole instance side of this
            // loop (color lookup, search-match, selection/hover comparisons,
            // `to_rgb()`) once GPU surfaces are known to be unavailable on
            // this platform/build (see `flame_gpu_unavailable`): `instances`
            // would just be built and then discarded, since `paint_flame_lane`
            // bails out before ever reading it in that case. Hit-testing
            // still needs `hit_bars` regardless, so that half always runs.
            let gpu_available = !self.flame_gpu_unavailable;
            let mut instances: Vec<BarInstance> =
                if gpu_available { Vec::with_capacity(lane.bars.len()) } else { Vec::new() };
            let mut hit_bars: Vec<HitBar> = Vec::with_capacity(lane.bars.len());

            // `BarInstance` coordinates feed a shader whose `viewport` uniform
            // is the surface's *physical* pixel size (`back_view_with_size`,
            // written by `WgpuSurface::prepaint` as `bounds * scale_factor`).
            // Everything else in this function (`chart_width`, `FLAME_ROW_HEIGHT`,
            // `bar_screen_rect`, hit-testing) stays in logical pixels, since
            // that's the space GPUI mouse events and `Bounds<Pixels>` are in
            // -- only the values that cross into the GPU instance buffer get
            // converted here, right at the boundary.
            let scale = window.scale_factor();

            for bar in &lane.bars {
                let bar_start_ns = bar.start_ns as f64;
                let bar_end_ns = bar_start_ns + bar.duration_ns as f64;
                // Cull spans that don't intersect the current zoomed/panned
                // window at all — cheaper, and keeps a deeply-zoomed-in view
                // from still building thousands of off-screen GPU instances.
                if bar_end_ns < visible_start_ns || bar_start_ns > visible_end_ns {
                    continue;
                }
                let (x, width) = bar_screen_rect(bar, visible_start_ns, visible_span_ns, chart_width);
                let top = bar.depth as f32 * FLAME_ROW_HEIGHT;

                if gpu_available {
                    let color = match (bar.category, bar.gpu_pass_kind) {
                        (Some(cat), _) => category_color(cat, cx),
                        (None, Some(kind)) => gpu_pass_color(kind, cx),
                        _ => cx.theme().chart_1,
                    };
                    let matches_search =
                        search.is_empty() || contains_ignore_ascii_case(&bar.label, &search);
                    let is_selected = selected_key.as_ref().is_some_and(|(name, depth, dur)| {
                        *name == bar.label && *depth == bar.depth && *dur == bar.duration_ns
                    });
                    let is_hovered = self.hovered_bar.as_ref().is_some_and(|h| {
                        h.lane_index == lane_index && h.start_ns == bar.start_ns && h.depth == bar.depth
                    });

                    let mut rgba = color.to_rgb();
                    if !matches_search {
                        rgba.a *= 0.25;
                    }

                    instances.push(BarInstance {
                        rect_min: [x * scale, top * scale],
                        rect_max: [(x + width) * scale, (top + (FLAME_ROW_HEIGHT - 2.0)) * scale],
                        color: [rgba.r, rgba.g, rgba.b, rgba.a],
                        corner_radius: 3.0 * scale,
                        highlight: if is_selected || is_hovered { 1.0 } else { 0.0 },
                        _pad: [0.0, 0.0],
                    });
                }
                hit_bars.push(HitBar { bar: bar.clone(), x, width, top });
            }

            // Lazily create (and lazily paint into) this lane's own GPU
            // surface. Falls back to "no bars drawn, but the row still lays
            // out and the hit-test overlay still works" when GPU surfaces
            // aren't available on this platform/build (see
            // `flame_gpu_unavailable`) -- exactly the same data either way,
            // just not turned into pixels.
            let surface_handle = self.paint_flame_lane(lane_index, window, &instances);

            let mut row = div()
                .id(SharedString::from(format!("flame-lane-{lane_index}")))
                .relative()
                .w(px(chart_width))
                .h(px(lane_height));

            if let Some(handle) = surface_handle {
                // Without this, every mouse-move tick during an Inspector
                // sidebar resize (or a window resize) triggers a real
                // background-thread texture reallocation for this lane's
                // surface -- on top of the CPU-side instance rebuild and GPU
                // submission `paint_flame_lane` already does per render.
                // Deferring the actual reallocation until the drag/resize
                // ends (showing a stretched copy of the last frame in the
                // meantime, same as every other interactive-resize surface
                // in this codebase) is what `WgpuSurface` exists to let us
                // opt into.
                row = row.child(wgpu_surface(handle).absolute().inset_0().defer_resize_until_mouse_up(true));
            }

            let entity = cx.entity().clone();
            row = row.child(
                // Per-lane bounds capture, same purpose as the container's
                // own canvas below: mouse events carry only window-absolute
                // positions, so hit-testing needs this lane's own origin to
                // convert them into the local (time, depth-row) space
                // `hit_test_lane_bar` expects.
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

            // A single hit-test overlay per lane replaces what used to be a
            // `.tooltip()`/`.on_click()` pair on *every* bar div: one real,
            // stateful, interactive element per lane instead of one per bar.
            let hit_bars_for_move = Rc::new(hit_bars);
            let hit_bars_for_click = hit_bars_for_move.clone();
            let lane_label_for_click = lane.label.clone();
            row = row
                .child(
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

            if let Some(hover) = self.hovered_bar.as_ref().filter(|h| h.lane_index == lane_index) {
                row = row.child(flame_bar_tooltip(hover, cx));
            }

            lane_elements.push(row.into_any_element());
        }

        if self.flame_gpu_unavailable {
            lane_elements.insert(
                0,
                Alert::warning(
                    "flame-gpu-unavailable",
                    "GPU-accelerated flame bars are unavailable on this platform/build; \
                     lane rows still lay out and remain interactive, but bars aren't drawn.",
                )
                .into_any_element(),
            );
        }

        // CPU-observed GPU submit/fence-observed timeline markers for the
        // currently selected frame (see `gpui::FrameCapture::cpu_gpu_submit_ns`'s
        // doc comment): thin vertical lines across the full lane stack,
        // distinct from the GPU span bars themselves, so the gap between
        // "CPU asked" / "GPU actually started" (first GPU bar) / "GPU
        // actually finished" (last GPU bar) / "CPU found out" is visible on
        // one chart. Only two elements regardless of span count, so these
        // stay plain GPUI divs rather than folding into the instance buffer.
        let mut marker_elements: Vec<AnyElement> = Vec::new();
        if let (Some(submit_ns), Some(fence_ns)) = (cpu_gpu_submit_ns, cpu_gpu_fence_observed_ns) {
            if let Some(x) = ns_to_x(submit_ns, visible_start_ns, visible_span_ns, chart_width) {
                marker_elements.push(timeline_marker(x, total_content_height, cx.theme().warning, "CPU submit"));
            }
            if let Some(x) = ns_to_x(fence_ns, visible_start_ns, visible_span_ns, chart_width) {
                marker_elements.push(timeline_marker(
                    x,
                    total_content_height,
                    cx.theme().success,
                    "CPU fence observed",
                ));
            }
        }

        let entity = cx.entity().clone();

        div()
            .id("flame-chart-canvas")
            .relative()
            .overflow_hidden()
            .w_full()
            .h_full()
            .child(
                // Zero-footprint overlay that exists only to capture this
                // container's screen bounds each render, so the scroll-wheel
                // handler below can turn a cursor position into a time-axis
                // zoom anchor (`ScrollWheelEvent` carries no element bounds
                // of its own).
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
                // Plain scrolling is left alone so it still scrolls the
                // lane list vertically when lanes overflow the viewport;
                // only Ctrl/Cmd+scroll zooms the time axis.
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
                state.flame_zoom.zoom_at(cursor_fraction, zoom_factor_for_wheel_delta(delta_y));
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
            .on_drag_move(cx.listener(|state, event: &DragMoveEvent<ProfilerPanDrag>, _window, cx| {
                let width = f32::from(event.bounds.size.width).max(1.0);
                let x = f32::from(event.event.position.x);
                if let Some(last_x) = state.flame_pan_last_x {
                    state.flame_zoom.pan_by_fraction(-(x - last_x) / width);
                }
                state.flame_pan_last_x = Some(x);
                cx.notify();
            }))
            // Right-click+drag pans the time axis too, alongside the
            // left-button `on_drag`/`on_drag_move` above -- the flame
            // chart's per-lane hit-test overlays use left-click for
            // select-a-span, so right-drag is the conflict-free complement
            // for panning without a modifier key.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|state, _event: &MouseDownEvent, _window, _cx| {
                    // Just resets the drag anchor; nothing rendered depends
                    // on it, so (matching the counters sparkline and GPU
                    // preview's identical handlers) this doesn't `cx.notify()`
                    // -- doing so forced a full lane rebuild/GPU repaint on
                    // every right-click for no visible effect.
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
            .child(v_flex().id("flame-chart-lanes").gap_1().children(lane_elements))
            .children(marker_elements)
            .into_any_element()
    }

    /// Lazily creates this lane's `wgpu_surface` (and the shared
    /// `FlameBarPipeline`, on the very first lane that manages to create
    /// one) and paints `instances` into its current back buffer. Returns
    /// the surface handle to embed in the element tree, or `None` if GPU
    /// surfaces aren't available on this platform/build (in which case
    /// `flame_gpu_unavailable` is latched so the caller can surface a
    /// one-time notice instead of silently drawing nothing).
    fn paint_flame_lane(
        &mut self,
        lane_index: usize,
        window: &Window,
        instances: &[BarInstance],
    ) -> Option<gpui::WgpuSurfaceHandle> {
        if self.flame_lane_gpu.len() <= lane_index {
            self.flame_lane_gpu.resize_with(lane_index + 1, || None);
        }

        if self.flame_lane_gpu[lane_index].is_none() {
            let Some(surface) = window.create_wgpu_surface(
                // Real size doesn't matter here: the `WgpuSurface` element's
                // own `prepaint` requests a resize to match its measured
                // layout bounds every render, same as `back_view_with_size`
                // lagging layout by one frame elsewhere in this file.
                1,
                1,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            ) else {
                self.flame_gpu_unavailable = true;
                return None;
            };
            let device = surface.device().clone();
            if self.flame_bar_pipeline.is_none() {
                self.flame_bar_pipeline = Some(Rc::new(FlameBarPipeline::new(&device, surface.format())));
            }
            let pipeline = self.flame_bar_pipeline.clone().unwrap();
            self.flame_lane_gpu[lane_index] = Some(FlameLaneGpu::new(&device, surface, &pipeline));
        }

        let pipeline = self.flame_bar_pipeline.clone()?;
        let gpu = self.flame_lane_gpu[lane_index].as_mut()?;
        let handle = gpu.surface.clone();
        let Some((view, (width, height))) = handle.back_view_with_size() else {
            // Surface exists but isn't ready to accept a frame this render
            // (e.g. mid-resize) -- the element still shows its last frame.
            return Some(handle);
        };
        gpu.render(&pipeline, instances, &view, width, height);
        drop(view);
        handle.swap_buffers();
        let _ = window;
        Some(handle)
    }

    /// Hit-tests `local` (a cursor position already made relative to this
    /// lane's own bounds) against `hit_bars`, updating `hovered_bar` and
    /// notifying only when the hovered bar actually changed -- mouse-move
    /// fires far more often than the hover target changes, and renotifying
    /// on every pixel would defeat the point of cutting per-bar elements.
    fn update_flame_hover(
        &mut self,
        lane_index: usize,
        hit_bars: &[HitBar],
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let bounds = self.flame_lane_bounds.get(lane_index).copied().unwrap_or_default();
        let local = point(position.x - bounds.origin.x, position.y - bounds.origin.y);
        let found = hit_test_lane_bar(hit_bars, local);

        let new_hover = found.map(|hb| HoveredBar {
            lane_index,
            start_ns: hb.bar.start_ns,
            depth: hb.bar.depth,
            label: hb.bar.label.clone(),
            duration_ns: hb.bar.duration_ns,
            category_label: hb.bar.category_label(),
            local_pos: point(hb.x, hb.top),
        });

        let changed = match (&self.hovered_bar, &new_hover) {
            (None, None) => false,
            (Some(a), Some(b)) => a.lane_index != b.lane_index || a.start_ns != b.start_ns || a.depth != b.depth,
            _ => true,
        };
        self.hovered_bar = new_hover;
        if changed {
            cx.notify();
        }
    }

    /// Hit-tests `position` (window-absolute) against `lane_index`'s
    /// `hit_bars` and, on a hit, sets `selected_span` -- this single hit-test
    /// replaces what used to be a `.on_click()` closure per bar div.
    fn select_flame_bar_at(
        &mut self,
        lane_index: usize,
        hit_bars: &[HitBar],
        lane_label: SharedString,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let bounds = self.flame_lane_bounds.get(lane_index).copied().unwrap_or_default();
        let local = point(position.x - bounds.origin.x, position.y - bounds.origin.y);
        let Some(hit) = hit_test_lane_bar(hit_bars, local) else {
            return;
        };
        self.selected_span = Some(SelectedSpan {
            name: hit.bar.label.clone(),
            lane: lane_label,
            category_label: hit.bar.category_label(),
            depth: hit.bar.depth,
            duration_ns: hit.bar.duration_ns,
            element_type: hit.bar.element_type.clone(),
            element_source: hit.bar.element_source.clone(),
        });
        cx.notify();
    }

    fn render_selected_span_details(&self, span: &SelectedSpan, cx: &Context<Self>) -> AnyElement {
        let mut list = DescriptionList::new()
            .columns(2)
            .label_width(px(90.))
            .bordered(true)
            .child("Duration", format!("{:.3}ms", span.duration_ns as f64 / 1.0e6), 1)
            .child("Depth", span.depth.to_string(), 1)
            .child("Lane", span.lane.clone(), 1)
            .child("Category", span.category_label.clone(), 1);

        if let Some(element_type) = span.element_type.clone() {
            list = list.child("Element", element_type, 1);
        }
        if let Some(source) = span.element_source.clone() {
            list = list.child("Source", source, 1);
        }

        v_flex()
            .gap_1()
            .p_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .text_color(cx.theme().foreground)
                    .child(span.name.clone()),
            )
            .child(list)
            .into_any_element()
    }

    // ── Counters ─────────────────────────────────────────────────────

    fn render_counters_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(capture) = self.capture.as_ref() else {
            return profiler_empty_state(
                "Start and stop a capture (Flame Chart tab) to see aggregate counters for the recorded window.",
                cx,
            );
        };
        if capture.frame_count() == 0 {
            return profiler_empty_state("The capture stopped with no frames recorded.", cx);
        }
        // Computed once in `toggle_capture` when this capture stopped (see
        // `counter_summary`'s field doc) instead of re-aggregated from every
        // frame on every render of this section.
        let Some(summary) = self.counter_summary else {
            return profiler_empty_state("Counter summary unavailable for this capture.", cx);
        };

        v_flex()
            .gap_3()
            .p_2()
            .child(render_counter_tiles(&summary, cx))
            .child(render_draw_call_table(&summary, cx))
            .child(render_atlas_and_events(&summary, cx))
            .child(self.render_frame_duration_sparkline(cx))
            .into_any_element()
    }

    /// Per-frame duration sparkline over the whole capture window, pan+zoom
    /// over the frame-index axis using the same `RangeZoom` math as the
    /// flame chart's time axis (independent state — this is a different
    /// domain — but the identical interaction logic and helpers).
    fn render_frame_duration_sparkline(&mut self, cx: &mut Context<Self>) -> AnyElement {
        // `frame_durations_ms`/`frame_durations_max_ms` are computed once in
        // `toggle_capture` (see their field docs), not re-derived from the
        // capture on every render -- this just guards against the invariant
        // ever being violated (empty until a capture has actually stopped)
        // rather than panicking.
        let frame_count = self.frame_durations_ms.len();
        if frame_count == 0 {
            return div().into_any_element();
        }
        let max_ms = self.frame_durations_max_ms;
        const SPARKLINE_HEIGHT: f32 = 60.0;
        const DEFAULT_CHART_WIDTH: f32 = 900.0;

        self.counters_zoom.set_domain(0.0, frame_count as f64);
        let visible_start = self.counters_zoom.visible_start();
        let visible_end = self.counters_zoom.visible_end();
        let visible_span = self.counters_zoom.visible_span();
        let zoomed = self.counters_zoom.is_zoomed();

        let measured_width = f32::from(self.counters_chart_bounds.size.width);
        let chart_width = if measured_width > 1.0 { measured_width } else { DEFAULT_CHART_WIDTH };
        let bar_width = ((chart_width as f64 / visible_span) as f32).max(1.0);

        let start_index = visible_start.floor().max(0.0) as usize;
        let end_index = (visible_end.ceil() as usize).min(frame_count);

        let mut bar_elements: Vec<AnyElement> = Vec::new();
        for index in start_index..end_index {
            let ms = self.frame_durations_ms[index];
            let height = ((ms / max_ms) * SPARKLINE_HEIGHT).max(1.0);
            let color = if ms > 16.7 { cx.theme().danger } else { cx.theme().chart_1 };
            let x = ((index as f64 - visible_start) / visible_span) as f32 * chart_width;
            bar_elements.push(
                div()
                    .absolute()
                    .bottom(px(0.))
                    .left(px(x))
                    .w(px((bar_width - 1.0).max(1.0)))
                    .h(px(height))
                    .bg(color)
                    .into_any_element(),
            );
        }

        let entity = cx.entity().clone();

        let header = h_flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .text_color(cx.theme().foreground)
                    .child(format!("Frame Duration Over Capture Window (max {:.2}ms)", max_ms)),
            )
            .child(div().flex_1())
            .child(render_zoom_controls(
                "counters",
                zoomed,
                cx.listener(|state, _, _window, cx| {
                    state.counters_zoom.zoom_at(0.5, 1.25);
                    cx.notify();
                }),
                cx.listener(|state, _, _window, cx| {
                    state.counters_zoom.zoom_at(0.5, 0.8);
                    cx.notify();
                }),
                cx.listener(|state, _, _window, cx| {
                    state.counters_zoom.reset();
                    cx.notify();
                }),
                cx,
            ));

        v_flex()
            .gap_1()
            .child(header)
            .child(zoom_pan_hint(cx))
            .child(
                div()
                    .id("frame-duration-sparkline")
                    .relative()
                    .h(px(SPARKLINE_HEIGHT))
                    .overflow_hidden()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        canvas(
                            {
                                let entity = entity.clone();
                                move |bounds, _window, cx| {
                                    entity.update(cx, |state, _cx| state.counters_chart_bounds = bounds);
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
                        let width = f32::from(state.counters_chart_bounds.size.width);
                        if width <= 1.0 {
                            return;
                        }
                        let cursor_fraction = ((f32::from(event.position.x)
                            - f32::from(state.counters_chart_bounds.origin.x))
                            / width)
                            .clamp(0.0, 1.0);
                        let delta_y = f32::from(event.delta.pixel_delta(window.line_height()).y);
                        state.counters_zoom.zoom_at(cursor_fraction, zoom_factor_for_wheel_delta(delta_y));
                        cx.notify();
                    }))
                    .on_click(cx.listener(|state, event: &ClickEvent, _window, cx| {
                        if event.click_count() >= 2 {
                            state.counters_zoom.reset();
                            cx.notify();
                        }
                    }))
                    .on_drag(ProfilerPanDrag, {
                        let entity = entity.clone();
                        move |_, _start_position, _window, cx| {
                            entity.update(cx, |state, _cx| state.counters_pan_last_x = None);
                            cx.new(|_| ProfilerPanDrag)
                        }
                    })
                    .on_drag_move(cx.listener(
                        |state, event: &DragMoveEvent<ProfilerPanDrag>, _window, cx| {
                            let width = f32::from(event.bounds.size.width).max(1.0);
                            let x = f32::from(event.event.position.x);
                            if let Some(last_x) = state.counters_pan_last_x {
                                state.counters_zoom.pan_by_fraction(-(x - last_x) / width);
                            }
                            state.counters_pan_last_x = Some(x);
                            cx.notify();
                        },
                    ))
                    // Right-click+drag pans too, alongside the left-button
                    // `on_drag`/`on_drag_move` above -- see the flame
                    // chart's identical addition for why (button-agnostic
                    // complement to GPUI's left-button-only drag system).
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|state, _event: &MouseDownEvent, _window, cx| {
                            state.counters_pan_last_x = None;
                        }),
                    )
                    .on_mouse_move(cx.listener(|state, event: &MouseMoveEvent, _window, cx| {
                        if event.pressed_button != Some(MouseButton::Right) {
                            return;
                        }
                        let width = f32::from(state.counters_chart_bounds.size.width).max(1.0);
                        let x = f32::from(event.position.x);
                        if let Some(last_x) = state.counters_pan_last_x {
                            state.counters_zoom.pan_by_fraction(-(x - last_x) / width);
                            cx.notify();
                        }
                        state.counters_pan_last_x = Some(x);
                    }))
                    .on_mouse_up(
                        MouseButton::Right,
                        cx.listener(|state, _event: &MouseUpEvent, _window, cx| {
                            state.counters_pan_last_x = None;
                        }),
                    )
                    .children(bar_elements),
            )
            .into_any_element()
    }

    // ── Memory ───────────────────────────────────────────────────────

    fn refresh_memory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.memory_cpu = Some(window.memory_snapshot(cx));
        self.memory_gpu = window.gpu_memory_snapshot();
        self.memory_error = if self.memory_gpu.is_none() {
            Some(
                "GPU memory snapshot unavailable on this platform/backend, or the renderer hasn't been created yet."
                    .into(),
            )
        } else {
            None
        };
        cx.notify();
    }

    fn render_memory_section(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let _ = window;
        let refresh = Button::new("memory-refresh")
            .small()
            .primary()
            .icon(IconName::Refresh)
            .label("Refresh Snapshot")
            .on_click(cx.listener(|state, _, window, cx| state.refresh_memory(window, cx)));

        let mut root = v_flex().gap_3().p_2().child(refresh);

        if let Some(err) = self.memory_error.clone() {
            root = root.child(Alert::warning("memory-warning", err));
        }

        if self.memory_cpu.is_none() && self.memory_gpu.is_none() {
            root = root.child(
                div().text_sm().text_color(cx.theme().muted_foreground).child(
                    "Click Refresh to snapshot WGPUI's current CPU/GPU memory footprint. \
                     This is a live, on-demand query — not tracked per frame.",
                ),
            );
        } else {
            if let Some(cpu) = self.memory_cpu.clone() {
                root = root.child(render_cpu_memory_breakdown(&cpu, cx));
            }
            if let Some(gpu) = self.memory_gpu.clone() {
                root = root.child(render_gpu_memory_breakdown(&gpu, cx));
            }
        }

        root.into_any_element()
    }

    // ── UI tree ──────────────────────────────────────────────────────

    fn start_ui_tree_capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ui_tree_pending {
            return;
        }
        self.ui_tree_error = None;
        self.ui_tree_pending = true;
        gpui::request_ui_tree_capture();
        cx.notify();
        self.schedule_ui_tree_poll(0, window, cx);
    }

    fn schedule_ui_tree_poll(&mut self, attempt: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.ui_tree_poll_task = Some(cx.spawn_in(window, async move |this, cx| {
            Timer::after(CAPTURE_POLL_INTERVAL).await;
            // If the entity was dropped (panel closed) between scheduling and
            // firing, there is nothing left to update — that is the only way
            // this can fail, so it is safe to ignore.
            let _ = this.update_in(cx, |state, window, cx| {
                state.ui_tree_poll_tick(attempt, window, cx);
            });
        }));
    }

    fn ui_tree_poll_tick(&mut self, attempt: u32, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(capture) = gpui::take_completed_ui_tree_capture() {
            self.ui_tree_replay = Some(UiTreeReplay::new(capture));
            self.ui_tree_pending = false;
            self.ui_tree_poll_task = None;
            self.ui_tree_selected = None;
            self.ui_tree_collapsed.clear();
            cx.notify();
            return;
        }
        if attempt >= CAPTURE_POLL_TIMEOUT_ATTEMPTS {
            self.ui_tree_pending = false;
            self.ui_tree_poll_task = None;
            self.ui_tree_error =
                Some("Timed out waiting for a frame to draw and complete the UI tree capture.".into());
            cx.notify();
            return;
        }
        self.schedule_ui_tree_poll(attempt + 1, window, cx);
    }

    fn render_ui_tree_section(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let capture_button = Button::new("ui-tree-capture")
            .small()
            .primary()
            .icon(IconName::Camera)
            .label(if self.ui_tree_pending { "Capturing..." } else { "Capture UI Tree" })
            .loading(self.ui_tree_pending)
            .disabled(self.ui_tree_pending)
            .on_click(cx.listener(|state, _, window, cx| state.start_ui_tree_capture(window, cx)));

        let mut header = h_flex().gap_2().items_center().child(capture_button);
        if self.ui_tree_pending {
            header = header.child(Spinner::new().small());
        }
        if let Some(replay) = self.ui_tree_replay.as_ref() {
            header = header.child(
                div().text_xs().text_color(cx.theme().muted_foreground).child(format!(
                    "{} nodes · {} scene primitives",
                    replay.node_count(),
                    replay.scene().primitive_count()
                )),
            );
        }

        let mut root = v_flex().gap_2().p_2().size_full().child(header);

        if let Some(err) = self.ui_tree_error.clone() {
            root = root.child(Alert::error("ui-tree-error", err));
        }

        if self.ui_tree_replay.is_none() {
            if !self.ui_tree_pending {
                root = root.child(
                    div().text_sm().text_color(cx.theme().muted_foreground).child(
                        "No UI tree captured yet. Trigger one to record the element tree, \
                         resolved layout/style, and paint primitive list for the very next drawn frame.",
                    ),
                );
            }
            return root.into_any_element();
        }

        root = root.child(self.render_ui_tree_list(cx));

        let selected_node = self
            .ui_tree_selected
            .and_then(|ix| self.ui_tree_replay.as_ref().and_then(|r| r.node(ix)).cloned());
        if let Some(node) = selected_node {
            root = root.child(render_ui_tree_node_details(&node, cx));
        }

        let _ = window;
        root.into_any_element()
    }

    fn render_ui_tree_list(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(replay) = self.ui_tree_replay.as_ref() else {
            return div().into_any_element();
        };
        let rows = flatten_ui_tree_rows(replay, &self.ui_tree_collapsed, self.ui_tree_selected);
        let item_count = rows.len();
        let rows_rc = Rc::new(rows);
        let entity = cx.entity().clone();

        uniform_list("profiler-ui-tree", item_count, {
            let rows = rows_rc.clone();
            let entity = entity.clone();
            move |range: Range<usize>, _window: &mut Window, cx: &mut App| {
                // Theme-token colors (rather than the ad hoc hex literals
                // this used to hardcode) so the tree matches the rest of the
                // Profiler tab -- and the Inspector panel's other tabs --
                // instead of carrying its own one-off palette.
                let muted = cx.theme().muted_foreground;
                let selected_bg = cx.theme().list_active;
                let hover_bg = cx.theme().list_hover;
                let type_name_color = cx.theme().info;

                range
                    .map(|i| {
                        let row = &rows[i];
                        let indent = row.depth as f32 * 14.0;
                        let index = row.index;

                        let mut el = div()
                            .id(SharedString::from(format!("ui-tree-row-{}", i)))
                            .flex()
                            .items_center()
                            .h(px(22.))
                            .pl(px(indent))
                            .cursor_pointer()
                            .rounded_sm()
                            .text_xs()
                            .when(row.is_selected, |s| s.bg(selected_bg))
                            .hover(|s| s.bg(hover_bg));

                        if row.has_children {
                            let entity_for_toggle = entity.clone();
                            el = el.child(
                                div()
                                    .w(px(14.))
                                    .text_color(muted)
                                    .child(if row.is_expanded { "▼" } else { "▶" })
                                    .on_mouse_down(MouseButton::Left, move |_, _window, cx| {
                                        entity_for_toggle.update(cx, |state, cx| {
                                            if !state.ui_tree_collapsed.remove(&index) {
                                                state.ui_tree_collapsed.insert(index);
                                            }
                                            cx.notify();
                                        });
                                    }),
                            );
                        } else {
                            el = el.child(div().w(px(14.)));
                        }

                        el = el
                            .child(div().text_color(type_name_color).child(row.type_name.clone()))
                            .child(div().ml(px(6.)).text_color(muted).child(row.bounds_label.clone()));

                        let entity_for_click = entity.clone();
                        el.on_click(move |_, _window, cx| {
                            entity_for_click.update(cx, |state, cx| {
                                state.ui_tree_selected = Some(index);
                                cx.notify();
                            });
                        })
                        .into_any_element()
                    })
                    .collect::<Vec<_>>()
            }
        })
        .w_full()
        .h(px(260.))
        .into_any_element()
    }

    // ── GPU deep capture ─────────────────────────────────────────────

    fn start_deep_capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.deep_capture_pending {
            return;
        }
        self.deep_capture_error = None;
        self.deep_capture_pending = true;
        self.deep_capture_preview = None;
        self.deep_capture_view.reset();
        gpui::request_deep_capture();
        cx.notify();
        self.schedule_deep_capture_poll(0, window, cx);
    }

    fn schedule_deep_capture_poll(&mut self, attempt: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.deep_capture_poll_task = Some(cx.spawn_in(window, async move |this, cx| {
            Timer::after(CAPTURE_POLL_INTERVAL).await;
            let _ = this.update_in(cx, |state, window, cx| {
                state.deep_capture_poll_tick(attempt, window, cx);
            });
        }));
    }

    fn deep_capture_poll_tick(&mut self, attempt: u32, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(capture) = gpui::take_completed_deep_capture() {
            self.deep_capture_replay = Some(DeepCaptureReplay::new(capture));
            self.deep_capture_pending = false;
            self.deep_capture_poll_task = None;
            self.render_deep_capture_preview(window, cx);
            cx.notify();
            return;
        }
        if attempt >= CAPTURE_POLL_TIMEOUT_ATTEMPTS {
            self.deep_capture_pending = false;
            self.deep_capture_poll_task = None;
            self.deep_capture_error =
                Some("Timed out waiting for a frame to draw and complete the deep capture.".into());
            cx.notify();
            return;
        }
        self.schedule_deep_capture_poll(attempt + 1, window, cx);
    }

    fn step_deep_capture(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        {
            let Some(replay) = self.deep_capture_replay.as_mut() else { return };
            if delta > 0 {
                replay.step_to_next_draw_call();
            } else {
                replay.step_to_previous_draw_call();
            }
        }
        // A different draw call means different image content — carrying
        // over a zoom/pan framed around the *previous* call's content would
        // be confusing, so each step starts fresh at fit-to-view.
        self.deep_capture_view.reset();
        self.render_deep_capture_preview(window, cx);
        cx.notify();
    }

    fn seek_deep_capture(&mut self, step: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(replay) = self.deep_capture_replay.as_mut() {
            replay.seek(step);
        }
        self.deep_capture_view.reset();
        self.render_deep_capture_preview(window, cx);
        cx.notify();
    }

    /// Synchronously replays the current draw call against the app's real
    /// `wgpu::Device`/`Queue`. `render_deep_capture_step` blocks on the GPU
    /// (documented as a diagnostic, not a hot-path, operation), which is
    /// acceptable here since it only runs in response to an explicit user
    /// action (trigger/step/seek), not on every render.
    fn render_deep_capture_preview(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        let Some(replay) = self.deep_capture_replay.as_ref() else {
            self.deep_capture_preview = None;
            return;
        };
        let step = replay.current_step();

        let Some((device, queue)) = window.gpu_device_and_queue() else {
            self.deep_capture_preview = Some(DeepCapturePreview::Unavailable(
                "GPU device/queue is not available on this platform or backend.".into(),
            ));
            return;
        };

        // Render at the actual panel size (times scale factor, for real
        // pixel detail), not the full window -- panning/zooming this
        // preview rescales an `img()` element on every mouse-move tick, and
        // rescaling a full-window-resolution source image (often several
        // times larger than the ~300px sidebar it's displayed in) is real,
        // avoidable cost repeated on every pan step. Falls back to a modest
        // default before the panel's bounds have been measured at least
        // once (e.g. the very first capture before any render has run).
        let scale = window.scale_factor();
        let measured = self.deep_capture_panel_bounds.size;
        let (fallback_w, fallback_h) = (320.0, 240.0);
        let width = if f32::from(measured.width) > 1.0 {
            ((f32::from(measured.width) * scale).round() as u32).clamp(1, 2048)
        } else {
            (fallback_w * scale) as u32
        };
        let height = if f32::from(measured.height) > 1.0 {
            ((f32::from(measured.height) * scale).round() as u32).clamp(1, 2048)
        } else {
            (fallback_h * scale) as u32
        };

        self.deep_capture_preview =
            Some(match gpui::render_deep_capture_step(&device, &queue, replay, step, width, height) {
                Ok(output) => match image::RgbaImage::from_raw(output.width, output.height, output.rgba8) {
                    Some(rgba) => {
                        let frame = image::Frame::new(rgba);
                        let render_image = Arc::new(RenderImage::new(smallvec::smallvec![frame]));
                        DeepCapturePreview::Image {
                            image: render_image,
                            width: output.width,
                            height: output.height,
                            texture_unavailable: output.texture_unavailable,
                        }
                    }
                    None => DeepCapturePreview::Unavailable(
                        "Replay produced an image buffer of an unexpected size.".into(),
                    ),
                },
                Err(gpui::ReplayError::UnsupportedDrawCallKind(kind)) => DeepCapturePreview::Unavailable(
                    format!(
                        "No live preview pipeline is wired up yet for {:?} draw calls \
                         (only Quads render a real preview this round).",
                        kind
                    )
                    .into(),
                ),
                Err(err) => DeepCapturePreview::Unavailable(format!("Replay failed: {err}").into()),
            });
    }

    fn render_deep_capture_section(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let trigger = Button::new("deep-capture-trigger")
            .small()
            .primary()
            .icon(IconName::Camera)
            .label(if self.deep_capture_pending { "Capturing..." } else { "Capture Frame" })
            .loading(self.deep_capture_pending)
            .disabled(self.deep_capture_pending)
            .on_click(cx.listener(|state, _, window, cx| state.start_deep_capture(window, cx)));

        let mut header = h_flex().gap_2().items_center().child(trigger);
        if self.deep_capture_pending {
            header = header.child(Spinner::new().small());
        }
        if let Some(replay) = self.deep_capture_replay.as_ref() {
            header = header.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} draw calls captured", replay.draw_call_count())),
            );
        }

        let mut root = v_flex().id("deep-capture-section").gap_3().p_2().size_full().child(header);

        if let Some(err) = self.deep_capture_error.clone() {
            root = root.child(Alert::error("deep-capture-error", err));
        }

        let Some(replay) = self.deep_capture_replay.as_ref() else {
            if !self.deep_capture_pending {
                root = root.child(
                    div().text_sm().text_color(cx.theme().muted_foreground).child(
                        "No deep capture yet. Trigger one to record the very next drawn frame's \
                         full GPU command stream and fixed-buffer resource contents (RenderDoc-style).",
                    ),
                );
            }
            let _ = window;
            return root.into_any_element();
        };

        let current_step = replay.current_step();
        let draw_call_count = replay.draw_call_count();
        let rows: Vec<(usize, DeepCaptureDrawCall, DrawCallResourceStatus)> = (0..draw_call_count)
            .filter_map(|index| {
                replay
                    .capture()
                    .draw_calls
                    .get(index)
                    .cloned()
                    .map(|call| (index, call, replay.resource_status(index)))
            })
            .collect();
        let current_call = replay.current_draw_call().cloned();

        root = root.child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    Button::new("deep-prev")
                        .xsmall()
                        .ghost()
                        .icon(IconName::ChevronLeft)
                        .disabled(current_step == 0)
                        .on_click(cx.listener(|state, _, window, cx| state.step_deep_capture(-1, window, cx))),
                )
                .child(
                    div()
                        .text_xs()
                        .min_w(px(110.))
                        .child(format!("Draw call {} / {}", current_step + 1, draw_call_count.max(1))),
                )
                .child(
                    Button::new("deep-next")
                        .xsmall()
                        .ghost()
                        .icon(IconName::ChevronRight)
                        .disabled(current_step + 1 >= draw_call_count)
                        .on_click(cx.listener(|state, _, window, cx| state.step_deep_capture(1, window, cx))),
                ),
        );

        root = root.child(
            div()
                .id("deep-capture-list")
                .max_h(px(220.))
                .overflow_y_scroll()
                .border_1()
                .border_color(cx.theme().border)
                .rounded_md()
                .children(rows.into_iter().map(|(index, call, status)| {
                    self.render_deep_capture_row(index, &call, status, index == current_step, cx)
                })),
        );

        if let Some(call) = current_call {
            root = root.child(render_deep_capture_call_details(&call, cx));
        }

        root = root.child(self.render_deep_capture_preview_panel(cx));

        root.into_any_element()
    }

    fn render_deep_capture_row(
        &self,
        index: usize,
        call: &DeepCaptureDrawCall,
        status: DrawCallResourceStatus,
        is_current: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let status_color = match status {
            DrawCallResourceStatus::Available => cx.theme().success,
            DrawCallResourceStatus::TextureContentUnavailable => cx.theme().warning,
            DrawCallResourceStatus::BufferReadbackMissing => cx.theme().danger,
            DrawCallResourceStatus::NoResource => cx.theme().muted_foreground,
        };
        let kind_label = format!("{:?}", call.kind);
        let pipeline_label = call.pipeline_label;
        let pass_label = call.pass_label;
        let sequence = call.sequence;

        div()
            .id(SharedString::from(format!("deep-call-row-{}", index)))
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .h(px(24.))
            .text_xs()
            .cursor_pointer()
            .when(is_current, |d| d.bg(cx.theme().list_active))
            .hover(|s| s.bg(cx.theme().list_hover))
            .on_click(cx.listener(move |state, _, window, cx| state.seek_deep_capture(index, window, cx)))
            .child(
                div()
                    .w(px(32.))
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("#{sequence}")),
            )
            .child(div().w(px(7.)).h(px(7.)).rounded_full().bg(status_color))
            .child(div().w(px(90.)).child(kind_label))
            .child(div().flex_1().text_color(cx.theme().muted_foreground).child(pipeline_label))
            .child(div().text_color(cx.theme().muted_foreground).child(pass_label))
            .into_any_element()
    }

    fn render_deep_capture_preview_panel(&self, cx: &Context<Self>) -> AnyElement {
        let entity = cx.entity().clone();
        // Bounds-capture overlay on the panel's own *outer* container --
        // not the fixed-width viewport div further down, whose width is
        // derived *from* this measurement, which would be circular. Lets
        // `display_width` below track the Inspector sidebar's actual,
        // user-resizable width instead of a hardcoded constant.
        let bounds_capture = canvas(
            move |bounds, _window, cx| {
                entity.update(cx, |state, _cx| state.deep_capture_panel_bounds = bounds);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let measured_width = f32::from(self.deep_capture_panel_bounds.size.width);

        let mut title_row = h_flex().items_center().gap_2().child(
            div()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(cx.theme().foreground)
                .child("Live Preview"),
        );

        let content = match &self.deep_capture_preview {
            None => v_flex()
                .gap_1()
                .child(title_row)
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Step to a draw call to render its preview."),
                )
                .into_any_element(),
            Some(DeepCapturePreview::Unavailable(message)) => v_flex()
                .gap_1()
                .child(title_row)
                .child(Alert::warning("deep-capture-preview-unavailable", message.clone()))
                .into_any_element(),
            Some(DeepCapturePreview::Image { image, width, height, texture_unavailable }) => {
                let display_width = if measured_width > 40.0 { measured_width } else { 280.0 };
                let scale = display_width / (*width as f32).max(1.0);
                let display_height = ((*height as f32) * scale).max(1.0);

                // The visible crop window (fractions of the full source
                // image) maps to an oversized, absolutely-positioned copy of
                // the same image inside a clipped viewport: the classic
                // "scale + negative offset behind an overflow:hidden box"
                // trick for panning/zooming into a raster image without any
                // dedicated cropping support from the image element itself.
                let (x0, y0, x1, y1) = self.deep_capture_view.crop_fractions();
                let crop_w = (x1 - x0).max(1e-4);
                let crop_h = (y1 - y0).max(1e-4);
                let scaled_width = display_width / crop_w;
                let scaled_height = display_height / crop_h;
                let offset_x = -x0 * scaled_width;
                let offset_y = -y0 * scaled_height;

                let entity = cx.entity().clone();
                let is_zoomed = self.deep_capture_view.is_zoomed();

                title_row = title_row.child(div().flex_1()).child(render_zoom_controls(
                    "deep-capture",
                    is_zoomed,
                    cx.listener(|state, _, _window, cx| {
                        state.deep_capture_view.zoom_at(0.5, 0.5, 1.25);
                        cx.notify();
                    }),
                    cx.listener(|state, _, _window, cx| {
                        state.deep_capture_view.zoom_at(0.5, 0.5, 0.8);
                        cx.notify();
                    }),
                    cx.listener(|state, _, _window, cx| {
                        state.deep_capture_view.reset();
                        cx.notify();
                    }),
                    cx,
                ));

                v_flex()
                    .gap_1()
                    .child(title_row)
                    .child(zoom_pan_hint(cx))
                    .child(
                        div()
                            .id("deep-capture-preview-viewport")
                            .relative()
                            .border_1()
                            .border_color(cx.theme().border)
                            .rounded_md()
                            .overflow_hidden()
                            .w(px(display_width))
                            .h(px(display_height))
                            .child(
                                // Bounds-capture overlay, same purpose as the
                                // flame chart's: `ScrollWheelEvent` carries
                                // no element bounds, so the scroll handler
                                // below needs this to compute a cursor
                                // fraction for zoom-around-cursor.
                                canvas(
                                    {
                                        let entity = entity.clone();
                                        move |bounds, _window, cx| {
                                            entity.update(cx, |state, _cx| {
                                                state.deep_capture_preview_bounds = bounds
                                            });
                                        }
                                    },
                                    |_, _, _, _| {},
                                )
                                .absolute()
                                .size_full(),
                            )
                            .on_scroll_wheel(cx.listener(
                                |state, event: &ScrollWheelEvent, window, cx| {
                                    if !(event.modifiers.control || event.modifiers.platform) {
                                        return;
                                    }
                                    cx.stop_propagation();
                                    let bounds = state.deep_capture_preview_bounds;
                                    let width = f32::from(bounds.size.width);
                                    let height = f32::from(bounds.size.height);
                                    if width <= 1.0 || height <= 1.0 {
                                        return;
                                    }
                                    let fx = ((f32::from(event.position.x)
                                        - f32::from(bounds.origin.x))
                                        / width)
                                        .clamp(0.0, 1.0);
                                    let fy = ((f32::from(event.position.y)
                                        - f32::from(bounds.origin.y))
                                        / height)
                                        .clamp(0.0, 1.0);
                                    let delta_y =
                                        f32::from(event.delta.pixel_delta(window.line_height()).y);
                                    state
                                        .deep_capture_view
                                        .zoom_at(fx, fy, zoom_factor_for_wheel_delta(delta_y));
                                    cx.notify();
                                },
                            ))
                            .on_click(cx.listener(|state, event: &ClickEvent, _window, cx| {
                                if event.click_count() >= 2 {
                                    state.deep_capture_view.reset();
                                    cx.notify();
                                }
                            }))
                            .on_drag(ProfilerPanDrag, {
                                let entity = entity.clone();
                                move |_, _start_position, _window, cx| {
                                    entity.update(cx, |state, _cx| state.deep_capture_pan_last = None);
                                    cx.new(|_| ProfilerPanDrag)
                                }
                            })
                            .on_drag_move(cx.listener(
                                |state, event: &DragMoveEvent<ProfilerPanDrag>, _window, cx| {
                                    let width = f32::from(event.bounds.size.width).max(1.0);
                                    let height = f32::from(event.bounds.size.height).max(1.0);
                                    let x = f32::from(event.event.position.x);
                                    let y = f32::from(event.event.position.y);
                                    if let Some((last_x, last_y)) = state.deep_capture_pan_last {
                                        state.deep_capture_view.pan_by_fraction(
                                            -(x - last_x) / width,
                                            -(y - last_y) / height,
                                        );
                                    }
                                    state.deep_capture_pan_last = Some((x, y));
                                    cx.notify();
                                },
                            ))
                            // Right-click+drag pans too, alongside the
                            // left-button `on_drag`/`on_drag_move` above --
                            // see the flame chart's identical addition for
                            // why.
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(|state, _event: &MouseDownEvent, _window, cx| {
                                    state.deep_capture_pan_last = None;
                                }),
                            )
                            .on_mouse_move(cx.listener(|state, event: &MouseMoveEvent, _window, cx| {
                                if event.pressed_button != Some(MouseButton::Right) {
                                    return;
                                }
                                let width = f32::from(state.deep_capture_preview_bounds.size.width).max(1.0);
                                let height = f32::from(state.deep_capture_preview_bounds.size.height).max(1.0);
                                let x = f32::from(event.position.x);
                                let y = f32::from(event.position.y);
                                if let Some((last_x, last_y)) = state.deep_capture_pan_last {
                                    state.deep_capture_view.pan_by_fraction(
                                        -(x - last_x) / width,
                                        -(y - last_y) / height,
                                    );
                                    cx.notify();
                                }
                                state.deep_capture_pan_last = Some((x, y));
                            }))
                            .on_mouse_up(
                                MouseButton::Right,
                                cx.listener(|state, _event: &MouseUpEvent, _window, cx| {
                                    state.deep_capture_pan_last = None;
                                }),
                            )
                            .child(
                                img(ImageSource::Render(image.clone()))
                                    .absolute()
                                    .left(px(offset_x))
                                    .top(px(offset_y))
                                    .w(px(scaled_width))
                                    .h(px(scaled_height)),
                            ),
                    )
                    .when(*texture_unavailable, |this| {
                        this.child(
                            div().text_xs().text_color(cx.theme().warning).child(
                                "Placeholder checkerboard: texture content for this draw-call \
                                 kind is not captured yet (atlas/surface readback, issue #72).",
                            ),
                        )
                    })
                    .into_any_element()
            }
        };

        div()
            .id("deep-capture-preview-panel")
            .relative()
            .w_full()
            .child(bounds_capture)
            .child(content)
            .into_any_element()
    }
}

fn profiler_empty_state(message: impl Into<SharedString>, cx: &Context<ProfilerPanel>) -> AnyElement {
    // `px(8.)`, matching the empty/placeholder-state padding `inspector.rs`
    // uses for its own tabs (Styles/Layout/Event Listeners), so the Profiler
    // tab's empty states read as part of the same panel rather than using
    // their own one-off spacing.
    div()
        .p(px(8.))
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(message.into())
        .into_any_element()
}

/// Zoom in / zoom out / reset button trio shared by the flame chart, the
/// counters sparkline, and the GPU deep-capture preview — the three
/// pan+zoomable views in this tab. `id_prefix` namespaces the element ids
/// (e.g. `"flame"`, `"counters"`, `"deep-capture"`); `is_zoomed` disables the
/// reset button when there's nothing to reset.
fn render_zoom_controls(
    id_prefix: &'static str,
    is_zoomed: bool,
    on_zoom_out: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_zoom_in: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_reset: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    _cx: &Context<ProfilerPanel>,
) -> AnyElement {
    h_flex()
        .gap_1()
        .items_center()
        .child(
            Button::new(SharedString::from(format!("{id_prefix}-zoom-out")))
                .xsmall()
                .ghost()
                .icon(IconName::ZoomOut)
                .tooltip("Zoom out")
                .on_click(on_zoom_out),
        )
        .child(
            Button::new(SharedString::from(format!("{id_prefix}-zoom-in")))
                .xsmall()
                .ghost()
                .icon(IconName::ZoomIn)
                .tooltip("Zoom in")
                .on_click(on_zoom_in),
        )
        .child(
            Button::new(SharedString::from(format!("{id_prefix}-zoom-reset")))
                .xsmall()
                .ghost()
                .icon(IconName::Maximize)
                .disabled(!is_zoomed)
                .tooltip("Reset zoom (or double-click the view)")
                .on_click(on_reset),
        )
        .into_any_element()
}

/// Small discoverability hint shown above each pan+zoomable view — there's no
/// other way to learn the interaction, since it's driven entirely by mouse
/// gestures (scroll wheel, drag) rather than visible affordances.
fn zoom_pan_hint(cx: &Context<ProfilerPanel>) -> AnyElement {
    div()
        .px_2()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(
            "Ctrl/Cmd + scroll to zoom \u{00B7} drag or right-click + drag to pan \u{00B7} \
             double-click to reset",
        )
        .into_any_element()
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit_index = 0usize;
    while value >= 1024.0 && unit_index < UNITS.len() - 1 {
        value /= 1024.0;
        unit_index += 1;
    }
    if unit_index == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit_index])
    }
}

// ── Flame chart GPU rendering ────────────────────────────────────────────
//
// Perf fix for the flame chart bringing the app "to its knees" on real
// captures: instead of one absolutely-positioned, `.tooltip()`/`.on_click()`
// bearing GPUI `div()` per span bar (plus a nested label div, so *two* real
// elements per bar), each lane renders its bars via a `wgpu_surface` and a
// single instanced draw call (see `profiler_flame_shader.wgsl`). Interaction
// (hover tooltip, click-to-select) is handled by one hit-test overlay div per
// lane, testing the cursor position against the same bar geometry the GPU
// instances were built from, in plain Rust -- not by any per-bar element.

/// One instance of the flame bar shader's per-bar input (see
/// `profiler_flame_shader.wgsl`'s `BarInstance`). `#[repr(C)]` + `Pod` so a
/// `Vec<BarInstance>` can be uploaded directly via `bytemuck::cast_slice`.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct BarInstance {
    rect_min: [f32; 2],
    rect_max: [f32; 2],
    color: [f32; 4],
    corner_radius: f32,
    highlight: f32,
    _pad: [f32; 2],
}

/// A visible bar's screen-space geometry plus its underlying `FlameBar`,
/// cloned out of `FlameLane::bars` once per render so the hover/click
/// handlers built from it can outlive the `lanes: &[FlameLane]` borrow
/// `render_flame_lanes_body` only holds for the duration of that call.
#[derive(Clone)]
struct HitBar {
    bar: FlameBar,
    x: f32,
    width: f32,
    top: f32,
}

/// The currently-hovered bar, if any, tracked so the single shared tooltip
/// div (not a per-bar `.tooltip()`) knows what to show and where.
#[derive(Clone)]
struct HoveredBar {
    lane_index: usize,
    start_ns: u64,
    depth: u16,
    label: SharedString,
    duration_ns: u32,
    category_label: SharedString,
    /// Position to anchor the tooltip at, in the hovered lane's own local
    /// (relative-to-its-own-bounds) coordinate space.
    local_pos: gpui::Point<f32>,
}

/// Computes a bar's `(x, width)` in chart pixels for the current zoom
/// window. Shared by the GPU-instance builder and `hit_test_lane_bar` so the
/// two can never disagree about where a bar is drawn.
fn bar_screen_rect(bar: &FlameBar, visible_start_ns: f64, visible_span_ns: f64, chart_width: f32) -> (f32, f32) {
    let bar_start_ns = bar.start_ns as f64;
    let x = ((bar_start_ns - visible_start_ns) / visible_span_ns) as f32 * chart_width;
    let width = ((bar.duration_ns as f64 / visible_span_ns) as f32 * chart_width).max(1.5);
    (x, width)
}

/// Case-insensitive substring search that doesn't allocate a lowercased copy
/// of `haystack`. Called for every visible bar's label on every render
/// whenever the flame chart search box is non-empty, so avoiding a
/// per-bar/per-render `String` allocation here (the previous
/// `haystack.to_lowercase().contains(needle_lower)`) matters at real span
/// counts. Span labels are Rust identifiers/type names, so plain ASCII
/// case-folding is sufficient -- this intentionally isn't full Unicode
/// case-insensitive matching.
fn contains_ignore_ascii_case(haystack: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    let haystack = haystack.as_bytes();
    let needle = needle_lower.as_bytes();
    if needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window.iter().zip(needle).all(|(&h, &n)| h.to_ascii_lowercase() == n))
}

/// Maps a nanosecond instant to an x pixel coordinate in the current zoom
/// window, or `None` if it falls outside the visible range.
fn ns_to_x(ns: u64, visible_start_ns: f64, visible_span_ns: f64, chart_width: f32) -> Option<f32> {
    let fraction = (ns as f64 - visible_start_ns) / visible_span_ns;
    if !(0.0..=1.0).contains(&fraction) {
        return None;
    }
    Some(fraction as f32 * chart_width)
}

const FLAME_ROW_HEIGHT: f32 = 20.0;

/// Linear scan over a lane's visible bars for the one under `local` (already
/// relative to the lane's own bounds). Trivial CPU cost even at a few
/// thousand bars per lane, called once per mouse-move/click -- nothing like
/// building a GPUI element per bar on every render.
fn hit_test_lane_bar(hit_bars: &[HitBar], local: gpui::Point<Pixels>) -> Option<&HitBar> {
    let x = f32::from(local.x);
    let y = f32::from(local.y);
    if y < 0.0 {
        return None;
    }
    hit_bars
        .iter()
        .find(|hb| y >= hb.top && y <= hb.top + FLAME_ROW_HEIGHT && x >= hb.x && x <= hb.x + hb.width)
}

fn flame_bar_tooltip(hover: &HoveredBar, cx: &Context<ProfilerPanel>) -> AnyElement {
    div()
        .absolute()
        .left(px(hover.local_pos.x + 8.0))
        .top(px(hover.local_pos.y + FLAME_ROW_HEIGHT))
        .px_2()
        .py_1()
        .rounded_md()
        .bg(cx.theme().popover)
        .border_1()
        .border_color(cx.theme().border)
        .shadow_md()
        .text_xs()
        .text_color(cx.theme().popover_foreground)
        .child(format!(
            "{}\n{:.3}ms \u{00B7} depth {} \u{00B7} {}",
            hover.label,
            hover.duration_ns as f64 / 1.0e6,
            hover.depth,
            hover.category_label
        ))
        .into_any_element()
}

fn timeline_marker(x: f32, height: f32, color: gpui::Hsla, label: &'static str) -> AnyElement {
    div()
        .id(SharedString::from(format!("flame-timeline-marker-{label}")))
        .absolute()
        .top(px(0.))
        .left(px(x))
        .w(px(1.5))
        .h(px(height.max(1.0)))
        .bg(color)
        .into_any_element()
}

/// Compiled once (lazily, by the first lane that manages to create a GPU
/// surface) and shared across every lane's own [`FlameLaneGpu`] -- a
/// `wgpu::RenderPipeline` only depends on the shader + bind group layout +
/// target format, not on any particular lane's instance data.
struct FlameBarPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl FlameBarPipeline {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("flame_bar_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("profiler_flame_shader.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("flame_bar_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("flame_bar_pipeline_layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("flame_bar_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_bar"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_bar"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self { pipeline, bind_group_layout }
    }
}

/// Per-lane GPU render state: one `wgpu_surface` + instance buffer per lane,
/// reused (and grown, never shrunk) across renders. A handful of these exist
/// at once (one per flame-chart lane -- typically the main thread, a couple
/// of background threads, and the GPU lane), nothing like one per bar.
struct FlameLaneGpu {
    surface: gpui::WgpuSurfaceHandle,
    viewport_buffer: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    bind_group: wgpu::BindGroup,
}

impl FlameLaneGpu {
    const INITIAL_CAPACITY: usize = 64;

    fn new(device: &wgpu::Device, surface: gpui::WgpuSurfaceHandle, pipeline: &FlameBarPipeline) -> Self {
        let viewport_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("flame_bar_viewport"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let instance_buffer = Self::make_instance_buffer(device, Self::INITIAL_CAPACITY);
        let bind_group = Self::make_bind_group(device, pipeline, &viewport_buffer, &instance_buffer);
        Self {
            surface,
            viewport_buffer,
            instance_buffer,
            instance_capacity: Self::INITIAL_CAPACITY,
            bind_group,
        }
    }

    fn make_instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("flame_bar_instances"),
            size: (capacity.max(1) * std::mem::size_of::<BarInstance>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn make_bind_group(
        device: &wgpu::Device,
        pipeline: &FlameBarPipeline,
        viewport_buffer: &wgpu::Buffer,
        instance_buffer: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("flame_bar_bind_group"),
            layout: &pipeline.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: viewport_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: instance_buffer.as_entire_binding() },
            ],
        })
    }

    fn ensure_capacity(&mut self, device: &wgpu::Device, pipeline: &FlameBarPipeline, needed: usize) {
        if needed <= self.instance_capacity {
            return;
        }
        let new_capacity = needed.next_power_of_two().max(Self::INITIAL_CAPACITY);
        self.instance_buffer = Self::make_instance_buffer(device, new_capacity);
        self.instance_capacity = new_capacity;
        self.bind_group = Self::make_bind_group(device, pipeline, &self.viewport_buffer, &self.instance_buffer);
    }

    fn render(
        &mut self,
        pipeline: &FlameBarPipeline,
        instances: &[BarInstance],
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) {
        let device = self.surface.device().clone();
        let queue = self.surface.queue().clone();
        self.ensure_capacity(&device, pipeline, instances.len());

        let viewport_data = [width as f32, height as f32, 0.0f32, 0.0f32];
        queue.write_buffer(&self.viewport_buffer, 0, bytemuck::cast_slice(&viewport_data));
        if !instances.is_empty() {
            queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(instances));
        }

        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("flame_bar_encoder") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("flame_bar_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if !instances.is_empty() {
                pass.set_pipeline(&pipeline.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.draw(0..4, 0..instances.len() as u32);
            }
        }
        queue.submit(std::iter::once(encoder.finish()));
    }
}

// ── Flame chart data model ──────────────────────────────────────────────

#[derive(Clone)]
struct FlameBar {
    label: SharedString,
    depth: u16,
    start_ns: u64,
    duration_ns: u32,
    category: Option<gpui::SpanCategory>,
    gpu_pass_kind: Option<gpui::GpuPassKind>,
    element_type: Option<SharedString>,
    element_source: Option<SharedString>,
}

impl FlameBar {
    fn from_cpu(span: &gpui::CpuSpan) -> Self {
        Self {
            label: span_name_label(span.name),
            depth: span.depth,
            start_ns: span.start_ns,
            duration_ns: span.duration_ns,
            category: Some(span.category),
            gpu_pass_kind: None,
            element_type: span.element.map(|e| SharedString::from(e.type_name)),
            element_source: span
                .element
                .and_then(|e| e.source_location)
                .map(|(file, line)| SharedString::from(format!("{file}:{line}"))),
        }
    }

    fn from_gpu(span: &gpui::GpuSpan) -> Self {
        Self {
            label: span_name_label(span.name),
            depth: 0,
            start_ns: span.start_ns,
            duration_ns: span.duration_ns,
            category: None,
            gpu_pass_kind: Some(span.pass_kind),
            element_type: None,
            element_source: None,
        }
    }

    fn category_label(&self) -> SharedString {
        match (self.category, self.gpu_pass_kind) {
            (Some(cat), _) => format!("{cat:?}").into(),
            (None, Some(kind)) => format!("GPU: {kind:?}").into(),
            _ => "—".into(),
        }
    }
}

struct FlameLane {
    label: SharedString,
    bars: Vec<FlameBar>,
    max_depth: u16,
}

fn span_name_label(name: gpui::SpanName) -> SharedString {
    match name {
        gpui::SpanName::Static(s) => SharedString::from(s),
        gpui::SpanName::Interned(id) => SharedString::from(format!("<interned #{id}>")),
    }
}

fn build_flame_lanes(frame: &gpui::FrameCapture) -> Vec<FlameLane> {
    let mut lanes = Vec::new();

    if !frame.cpu_spans.is_empty() {
        let max_depth = frame.cpu_spans.iter().map(|s| s.depth).max().unwrap_or(0);
        lanes.push(FlameLane {
            label: "Main Thread (CPU)".into(),
            bars: frame.cpu_spans.iter().map(FlameBar::from_cpu).collect(),
            max_depth,
        });
    }

    let mut background_by_thread: Vec<(gpui::ThreadKey, Vec<gpui::CpuSpan>)> = Vec::new();
    for span in &frame.background_spans {
        if let Some(entry) = background_by_thread.iter_mut().find(|(key, _)| *key == span.thread_id) {
            entry.1.push(*span);
        } else {
            background_by_thread.push((span.thread_id, vec![*span]));
        }
    }
    for (index, (_key, spans)) in background_by_thread.iter().enumerate() {
        let max_depth = spans.iter().map(|s| s.depth).max().unwrap_or(0);
        lanes.push(FlameLane {
            label: format!("Background Thread {}", index + 1).into(),
            bars: spans.iter().map(FlameBar::from_cpu).collect(),
            max_depth,
        });
    }

    if !frame.gpu_spans.is_empty() {
        lanes.push(FlameLane {
            label: "GPU".into(),
            bars: frame.gpu_spans.iter().map(FlameBar::from_gpu).collect(),
            max_depth: 0,
        });
    }

    lanes
}

fn category_color(category: gpui::SpanCategory, cx: &Context<ProfilerPanel>) -> gpui::Hsla {
    let theme = cx.theme();
    match category {
        gpui::SpanCategory::WindowFrame => theme.chart_1,
        gpui::SpanCategory::ElementRequestLayout => theme.chart_2,
        gpui::SpanCategory::ElementPrepaint => theme.chart_3,
        gpui::SpanCategory::ElementPaint => theme.chart_4,
        gpui::SpanCategory::BackgroundTask => theme.chart_5,
        gpui::SpanCategory::GpuRenderPass => theme.info,
        gpui::SpanCategory::GpuSubmitPresent => theme.success,
        gpui::SpanCategory::UserDefined => theme.warning,
    }
}

fn gpu_pass_color(kind: gpui::GpuPassKind, cx: &Context<ProfilerPanel>) -> gpui::Hsla {
    let theme = cx.theme();
    match kind {
        gpui::GpuPassKind::Main | gpui::GpuPassKind::MainResumed => theme.info,
        gpui::GpuPassKind::FilterGroup | gpui::GpuPassKind::FilterGroupResumed => theme.chart_5,
        gpui::GpuPassKind::FastSurfaceBlit | gpui::GpuPassKind::SubmitPresent => theme.success,
    }
}

// ── Counters rendering ───────────────────────────────────────────────────

fn render_counter_tiles(summary: &gpui::CounterSummary, cx: &Context<ProfilerPanel>) -> AnyElement {
    let tiles = [
        ("FPS", format!("{:.1}", summary.fps)),
        ("Mean Frame", format!("{:.2}ms", summary.mean_frame_duration_ms)),
        ("Max Frame", format!("{:.2}ms", summary.max_frame_duration_ms)),
        ("Present Mode", format!("{}", summary.present_mode)),
        ("Frames Captured", summary.frame_count.to_string()),
        ("Full-draw Frames", summary.full_draw_frame_count.to_string()),
        ("Fast-path Frames", summary.fast_path_frame_count.to_string()),
        ("Atlas Hit Rate", format!("{:.1}%", summary.atlas.cache_hit_rate * 100.0)),
    ];

    h_flex()
        .flex_wrap()
        .gap_2()
        .children(tiles.into_iter().map(|(label, value)| profiler_stat_tile(label, value, cx)))
        .into_any_element()
}

fn profiler_stat_tile(label: &str, value: String, cx: &Context<ProfilerPanel>) -> AnyElement {
    v_flex()
        .w(px(140.))
        .p_2()
        .gap_1()
        .rounded_md()
        .bg(cx.theme().muted)
        .child(div().text_xs().text_color(cx.theme().muted_foreground).child(label.to_string()))
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::BOLD)
                .text_color(cx.theme().foreground)
                .child(value),
        )
        .into_any_element()
}

fn render_draw_call_table(summary: &gpui::CounterSummary, cx: &Context<ProfilerPanel>) -> AnyElement {
    let rows: [(&str, gpui::PassCounterSummary); 8] = [
        ("Quads", summary.draw_calls.quads),
        ("Shadows", summary.draw_calls.shadows),
        ("Mono Sprites", summary.draw_calls.mono_sprites),
        ("Poly Sprites", summary.draw_calls.poly_sprites),
        ("Paths", summary.draw_calls.paths),
        ("Underlines", summary.draw_calls.underlines),
        ("Backdrop Filters", summary.draw_calls.backdrop_filters),
        ("Surfaces", summary.draw_calls.surfaces),
    ];

    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(cx.theme().foreground)
                .child("Draw Calls & Primitives (mean / max, per frame)"),
        )
        .child(
            v_flex().border_1().border_color(cx.theme().border).rounded_md().children(
                rows.into_iter().map(|(name, pass)| {
                    h_flex()
                        .justify_between()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(div().w(px(120.)).text_color(cx.theme().foreground).child(name))
                        .child(
                            div()
                                .w(px(140.))
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("calls {:.1}/{}", pass.draw_calls.mean, pass.draw_calls.max)),
                        )
                        .child(
                            div()
                                .w(px(160.))
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("prims {:.1}/{}", pass.primitives.mean, pass.primitives.max)),
                        )
                        .into_any_element()
                }),
            ),
        )
        .into_any_element()
}

fn render_atlas_and_events(summary: &gpui::CounterSummary, cx: &Context<ProfilerPanel>) -> AnyElement {
    h_flex()
        .gap_3()
        .flex_wrap()
        .child(
            DescriptionList::new()
                .columns(1)
                .label_width(px(120.))
                .bordered(true)
                .child(
                    "Tiles Allocated",
                    format!("{:.1}/{}", summary.atlas.tiles_allocated.mean, summary.atlas.tiles_allocated.max),
                    1,
                )
                .child(
                    "Tiles Evicted",
                    format!("{:.1}/{}", summary.atlas.tiles_evicted.mean, summary.atlas.tiles_evicted.max),
                    1,
                )
                .child(
                    "Cache Hits",
                    format!("{:.1}/{}", summary.atlas.cache_hits.mean, summary.atlas.cache_hits.max),
                    1,
                )
                .child(
                    "Cache Misses",
                    format!("{:.1}/{}", summary.atlas.cache_misses.mean, summary.atlas.cache_misses.max),
                    1,
                ),
        )
        .child(
            DescriptionList::new()
                .columns(1)
                .label_width(px(120.))
                .bordered(true)
                .child(
                    "Input Events",
                    format!(
                        "{:.1}/{}",
                        summary.events.input_events_dispatched.mean, summary.events.input_events_dispatched.max
                    ),
                    1,
                )
                .child(
                    "Notify Calls",
                    format!("{:.1}/{}", summary.events.notify_calls.mean, summary.events.notify_calls.max),
                    1,
                )
                .child(
                    "Invalidated",
                    format!(
                        "{:.1}/{}",
                        summary.events.entities_invalidated.mean, summary.events.entities_invalidated.max
                    ),
                    1,
                ),
        )
        .child(render_gpu_timeline_stats(summary, cx))
        .into_any_element()
}

/// CPU-observed GPU submit/fence-observed timeline correlation stats (see
/// `gpui::GpuTimelineSummary`'s doc comment): how long the GPU queue lagged
/// behind CPU submission, and how long readback/polling latency was, over
/// the capture window. This data is often sparse -- both instants and a
/// finalized, non-empty GPU span set are needed for a frame to count, and
/// GPU capture/readback lags CPU capture by design -- so `samples` (out of
/// `summary.frame_count`) is surfaced alongside the mean/max, not hidden.
fn render_gpu_timeline_stats(summary: &gpui::CounterSummary, cx: &Context<ProfilerPanel>) -> AnyElement {
    let timeline = &summary.gpu_timeline;

    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(cx.theme().foreground)
                .child(format!(
                    "CPU/GPU Submit Timeline ({} of {} frames sampled)",
                    timeline.samples, summary.frame_count
                )),
        )
        .child(
            DescriptionList::new()
                .columns(1)
                .label_width(px(190.))
                .bordered(true)
                .child(
                    "Submit \u{2192} GPU Start",
                    format!(
                        "{:.3}ms / {:.3}ms",
                        timeline.submit_to_gpu_start.mean / 1.0e6,
                        timeline.submit_to_gpu_start.max as f64 / 1.0e6
                    ),
                    1,
                )
                .child(
                    "GPU End \u{2192} Fence Observed",
                    format!(
                        "{:.3}ms / {:.3}ms",
                        timeline.gpu_end_to_fence_observed.mean / 1.0e6,
                        timeline.gpu_end_to_fence_observed.max as f64 / 1.0e6
                    ),
                    1,
                ),
        )
        .into_any_element()
}

// ── Memory rendering ─────────────────────────────────────────────────────

fn render_cpu_memory_breakdown(snapshot: &gpui::MemorySnapshot, cx: &Context<ProfilerPanel>) -> AnyElement {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(cx.theme().foreground)
                .child(format!("CPU Memory — {}", format_bytes(snapshot.total_bytes()))),
        )
        .child(
            DescriptionList::new()
                .columns(1)
                .label_width(px(160.))
                .bordered(true)
                .child("Element Arena", format_bytes(snapshot.element_arena_bytes), 1)
                .child("Glyph Cache", format_bytes(snapshot.text_system.glyph_cache_bytes), 1)
                .child("Shaped Line Cache", format_bytes(snapshot.text_system.shaped_line_cache_bytes), 1)
                .child("Image Cache", format_bytes(snapshot.image_cache_bytes), 1)
                .child("Capture Engine", format_bytes(snapshot.capture_engine_bytes), 1),
        )
        .into_any_element()
}

fn render_gpu_memory_breakdown(snapshot: &gpui::GpuMemorySnapshot, cx: &Context<ProfilerPanel>) -> AnyElement {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(cx.theme().foreground)
                .child(format!("GPU Memory — {}", format_bytes(snapshot.total_bytes()))),
        )
        .child(
            DescriptionList::new()
                .columns(1)
                .label_width(px(160.))
                .bordered(true)
                .child("Fixed Buffers", format_bytes(snapshot.fixed_buffer_bytes), 1)
                .child("Atlas Textures", format_bytes(snapshot.atlas_bytes), 1)
                .child("Surface Registry", format_bytes(snapshot.surface_registry_bytes), 1)
                .child("Swapchain", format_bytes(snapshot.swapchain_bytes), 1),
        )
        .into_any_element()
}

// ── UI tree rendering ─────────────────────────────────────────────────────

struct UiTreeRow {
    index: usize,
    depth: u16,
    has_children: bool,
    is_expanded: bool,
    is_selected: bool,
    type_name: SharedString,
    bounds_label: SharedString,
}

fn flatten_ui_tree_rows(
    replay: &UiTreeReplay,
    collapsed: &HashSet<usize>,
    selected: Option<usize>,
) -> Vec<UiTreeRow> {
    fn walk(
        replay: &UiTreeReplay,
        index: usize,
        collapsed: &HashSet<usize>,
        selected: Option<usize>,
        rows: &mut Vec<UiTreeRow>,
    ) {
        let Some(node) = replay.node(index) else { return };
        let has_children = !replay.children(index).is_empty();
        let is_expanded = !collapsed.contains(&index);
        rows.push(UiTreeRow {
            index,
            depth: node.depth,
            has_children,
            is_expanded,
            is_selected: selected == Some(index),
            type_name: SharedString::from(node.type_name),
            bounds_label: SharedString::from(format!(
                "{:.0}×{:.0} @ ({:.0},{:.0})",
                node.bounds.width, node.bounds.height, node.bounds.x, node.bounds.y
            )),
        });
        if has_children && is_expanded {
            for &child in replay.children(index) {
                walk(replay, child, collapsed, selected, rows);
            }
        }
    }

    let mut rows = Vec::new();
    for &root in replay.roots() {
        walk(replay, root, collapsed, selected, &mut rows);
    }
    rows
}

fn render_ui_tree_node_details(node: &UiElementNode, cx: &Context<ProfilerPanel>) -> AnyElement {
    let mut list = DescriptionList::new()
        .columns(2)
        .label_width(px(110.))
        .bordered(true)
        .child("Type", node.type_name.to_string(), 1)
        .child("Depth", node.depth.to_string(), 1)
        .child(
            "Bounds",
            format!(
                "{:.1},{:.1} {:.1}×{:.1}",
                node.bounds.x, node.bounds.y, node.bounds.width, node.bounds.height
            ),
            1,
        );

    if let Some(style) = &node.style {
        list = list
            .child("Display", format!("{:?}", style.display), 1)
            .child("Visibility", format!("{:?}", style.visibility), 1)
            .child("Position", format!("{:?}", style.position), 1)
            .child("Opacity", style.opacity.map(|o| format!("{o:.2}")).unwrap_or_else(|| "—".into()), 1)
            .child(
                "Background",
                style.background.as_ref().map(|b| format!("{:?}", b)).unwrap_or_else(|| "—".into()),
                1,
            )
            .child(
                "Border Color",
                style.border_color.map(|c| format!("{:?}", c)).unwrap_or_else(|| "—".into()),
                1,
            )
            .child(
                "Text Color",
                style.text_color.map(|c| format!("{:?}", c)).unwrap_or_else(|| "—".into()),
                1,
            );
    }

    v_flex()
        .gap_1()
        .p_2()
        .border_t_1()
        .border_color(cx.theme().border)
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(cx.theme().foreground)
                .child("Node Details"),
        )
        .child(list)
        .into_any_element()
}

// ── GPU deep capture rendering ────────────────────────────────────────────

fn render_deep_capture_call_details(call: &DeepCaptureDrawCall, cx: &Context<ProfilerPanel>) -> AnyElement {
    DescriptionList::new()
        .columns(2)
        .label_width(px(110.))
        .bordered(true)
        .child("Kind", format!("{:?}", call.kind), 1)
        .child("Pipeline", call.pipeline_label.to_string(), 1)
        .child("Pass", call.pass_label.to_string(), 1)
        .child("Bind groups", call.bind_group_count.to_string(), 1)
        .child("Vertex range", format!("{}..{}", call.vertex_range.0, call.vertex_range.1), 1)
        .child("Instance range", format!("{}..{}", call.instance_range.0, call.instance_range.1), 1)
        .child(
            "Buffer",
            call.buffer_kind.map(|k| format!("{:?}", k)).unwrap_or_else(|| "—".into()),
            1,
        )
        .child(
            "Atlas texture",
            call.atlas_texture_id.map(|id| format!("{id:#x}")).unwrap_or_else(|| "—".into()),
            1,
        )
        .into_any_element()
}

// ── Tests ──────────────────────────────────────────────────────────────
//
// `gpui::CpuSpan::thread_id` is a `ThreadKey`, whose only constructor
// (`ThreadKey::current`) is private to gpui-ce's flamegraph module — there
// is no public way to build one outside of a live capture session, so
// `build_flame_lanes`'s CPU/background-thread grouping path (and anything
// else that needs a `CpuSpan`) can't be exercised with a hand-built
// fixture here. Everything else below is real fixture-based coverage of
// this module's own data-transform logic: byte formatting, span-name
// display, GPU-lane construction (which needs no `ThreadKey`), and UI-tree
// flattening/collapsing/selection (`UiTreeCapture`'s fields are all public,
// so it is constructible without a live capture).
#[cfg(test)]
mod tests {
    use super::*;

    // -- Zoom/pan math (`RangeZoom`, `zoom_factor_for_wheel_delta`,
    // `ImageViewport`) -- pure, display-free logic shared by the flame
    // chart, counters sparkline, and GPU preview, so it's fully testable
    // without a window/render pass.

    #[test]
    fn range_zoom_full_starts_unzoomed_and_spans_the_whole_domain() {
        let zoom = RangeZoom::full(0.0, 1000.0);
        assert!(!zoom.is_zoomed());
        assert_eq!(zoom.visible_start(), 0.0);
        assert_eq!(zoom.visible_end(), 1000.0);
    }

    #[test]
    fn range_zoom_at_center_shrinks_span_symmetrically() {
        let mut zoom = RangeZoom::full(0.0, 1000.0);
        zoom.zoom_at(0.5, 0.5); // zoom in 2x around the midpoint
        assert!(zoom.is_zoomed());
        assert_eq!(zoom.visible_start(), 250.0);
        assert_eq!(zoom.visible_end(), 750.0);
    }

    #[test]
    fn range_zoom_anchors_the_cursor_point_in_place() {
        // Zooming in around the *left* edge (fraction 0.0) should keep that
        // edge fixed and only pull the far edge inward.
        let mut zoom = RangeZoom::full(0.0, 1000.0);
        zoom.zoom_at(0.0, 0.5);
        assert_eq!(zoom.visible_start(), 0.0);
        assert_eq!(zoom.visible_end(), 500.0);

        // Zooming in around the *right* edge keeps that edge fixed instead.
        let mut zoom = RangeZoom::full(0.0, 1000.0);
        zoom.zoom_at(1.0, 0.5);
        assert_eq!(zoom.visible_start(), 500.0);
        assert_eq!(zoom.visible_end(), 1000.0);
    }

    #[test]
    fn range_zoom_zoom_at_clamps_to_domain_bounds() {
        // Zooming *out* past the full domain clamps back to it rather than
        // over-shooting past the edges.
        let mut zoom = RangeZoom::full(0.0, 1000.0);
        zoom.zoom_at(0.5, 100.0);
        assert_eq!(zoom.visible_start(), 0.0);
        assert_eq!(zoom.visible_end(), 1000.0);
    }

    #[test]
    fn range_zoom_zoom_at_clamps_to_a_minimum_visible_span() {
        // Repeatedly zooming in hard should bottom out at
        // `MIN_VISIBLE_FRACTION` of the domain rather than collapsing to
        // (or past) a zero-width window.
        let mut zoom = RangeZoom::full(0.0, 1000.0);
        for _ in 0..200 {
            zoom.zoom_at(0.5, 0.5);
        }
        let span = zoom.visible_span();
        assert!(span > 0.0);
        assert!(span >= 1000.0 * RangeZoom::MIN_VISIBLE_FRACTION - 1e-6);
    }

    #[test]
    fn range_zoom_pan_by_fraction_shifts_the_window() {
        let mut zoom = RangeZoom::full(0.0, 1000.0);
        zoom.zoom_at(0.5, 0.5); // now visible = [250, 750]
        zoom.pan_by_fraction(0.5); // shift right by half the visible span (250)
        assert_eq!(zoom.visible_start(), 500.0);
        assert_eq!(zoom.visible_end(), 1000.0);
    }

    #[test]
    fn range_zoom_pan_by_fraction_clamps_at_domain_edges() {
        let mut zoom = RangeZoom::full(0.0, 1000.0);
        zoom.zoom_at(0.5, 0.5); // visible = [250, 750]
        zoom.pan_by_fraction(10.0); // a huge rightward pan
        assert_eq!(zoom.visible_end(), 1000.0);
        assert_eq!(zoom.visible_start(), 500.0); // span is preserved, just slid to the end

        zoom.pan_by_fraction(-10.0); // a huge leftward pan
        assert_eq!(zoom.visible_start(), 0.0);
        assert_eq!(zoom.visible_end(), 500.0);
    }

    #[test]
    fn range_zoom_set_domain_resets_when_domain_changes_but_not_otherwise() {
        let mut zoom = RangeZoom::full(0.0, 1000.0);
        zoom.zoom_at(0.5, 0.5);
        assert!(zoom.is_zoomed());

        // Re-affirming the same domain (e.g. re-rendering the same capture
        // frame) must not disturb the current zoom/pan.
        zoom.set_domain(0.0, 1000.0);
        assert!(zoom.is_zoomed());
        assert_eq!(zoom.visible_start(), 250.0);

        // A genuinely different domain (e.g. a newly selected frame) resets
        // to "fit all".
        zoom.set_domain(0.0, 2000.0);
        assert!(!zoom.is_zoomed());
        assert_eq!(zoom.visible_start(), 0.0);
        assert_eq!(zoom.visible_end(), 2000.0);
    }

    #[test]
    fn range_zoom_reset_restores_the_full_domain() {
        let mut zoom = RangeZoom::full(10.0, 20.0);
        zoom.zoom_at(0.25, 0.4);
        zoom.pan_by_fraction(0.3);
        assert!(zoom.is_zoomed());
        zoom.reset();
        assert!(!zoom.is_zoomed());
        assert_eq!(zoom.visible_start(), 10.0);
        assert_eq!(zoom.visible_end(), 20.0);
    }

    #[test]
    fn range_zoom_value_to_fraction_maps_the_visible_window() {
        let mut zoom = RangeZoom::full(0.0, 1000.0);
        zoom.zoom_at(0.5, 0.5); // visible = [250, 750]
        assert_eq!(zoom.value_to_fraction(250.0), 0.0);
        assert_eq!(zoom.value_to_fraction(750.0), 1.0);
        assert_eq!(zoom.value_to_fraction(500.0), 0.5);
    }

    #[test]
    fn zoom_factor_for_wheel_delta_inverts_and_clamps() {
        // Negative delta (scroll up/away) zooms in: factor < 1.
        assert!(zoom_factor_for_wheel_delta(-100.0) < 1.0);
        // Positive delta (scroll down/toward) zooms out: factor > 1.
        assert!(zoom_factor_for_wheel_delta(100.0) > 1.0);
        // No motion is a no-op factor.
        assert_eq!(zoom_factor_for_wheel_delta(0.0), 1.0);
        // Extreme deltas clamp rather than producing an unbounded factor.
        assert_eq!(zoom_factor_for_wheel_delta(100_000.0), zoom_factor_for_wheel_delta(160.0));
        assert_eq!(zoom_factor_for_wheel_delta(-100_000.0), zoom_factor_for_wheel_delta(-160.0));
    }

    #[test]
    fn image_viewport_starts_unzoomed_and_full() {
        let view = ImageViewport::new();
        assert!(!view.is_zoomed());
        assert_eq!(view.crop_fractions(), (0.0, 0.0, 1.0, 1.0));
    }

    #[test]
    fn image_viewport_zoom_at_applies_independently_per_axis() {
        let mut view = ImageViewport::new();
        // Zoom in 2x anchored at (0.0, 1.0) — top-left in fraction terms for
        // x, bottom-right for y — and check each axis anchors independently.
        view.zoom_at(0.0, 1.0, 0.5);
        assert!(view.is_zoomed());
        let (x0, y0, x1, y1) = view.crop_fractions();
        assert_eq!((x0, x1), (0.0, 0.5));
        assert_eq!((y0, y1), (0.5, 1.0));
    }

    #[test]
    fn image_viewport_pan_by_fraction_clamps_into_the_unit_square() {
        let mut view = ImageViewport::new();
        view.zoom_at(0.5, 0.5, 0.5); // crop = [0.25, 0.25, 0.75, 0.75]
        view.pan_by_fraction(-10.0, -10.0); // huge pan toward the origin
        let (x0, y0, x1, y1) = view.crop_fractions();
        assert_eq!((x0, y0), (0.0, 0.0));
        assert_eq!((x1, y1), (0.5, 0.5));
    }

    #[test]
    fn image_viewport_reset_restores_the_full_unit_square() {
        let mut view = ImageViewport::new();
        view.zoom_at(0.2, 0.8, 0.3);
        view.pan_by_fraction(0.1, -0.1);
        assert!(view.is_zoomed());
        view.reset();
        assert!(!view.is_zoomed());
        assert_eq!(view.crop_fractions(), (0.0, 0.0, 1.0, 1.0));
    }

    #[test]
    fn format_bytes_scales_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(1536), "1.50 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.00 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.00 GB");
    }

    #[test]
    fn span_name_label_formats_static_and_interned() {
        assert_eq!(span_name_label(gpui::SpanName::Static("paint")).to_string(), "paint");
        assert_eq!(span_name_label(gpui::SpanName::Interned(7)).to_string(), "<interned #7>");
    }

    #[test]
    fn flame_bar_category_label_prefers_cpu_category_over_gpu_pass() {
        let cpu_bar = FlameBar {
            label: "a".into(),
            depth: 0,
            start_ns: 0,
            duration_ns: 100,
            category: Some(gpui::SpanCategory::ElementPaint),
            gpu_pass_kind: None,
            element_type: None,
            element_source: None,
        };
        assert_eq!(cpu_bar.category_label().to_string(), "ElementPaint");

        let gpu_bar = FlameBar {
            label: "b".into(),
            depth: 0,
            start_ns: 0,
            duration_ns: 100,
            category: None,
            gpu_pass_kind: Some(gpui::GpuPassKind::Main),
            element_type: None,
            element_source: None,
        };
        assert_eq!(gpu_bar.category_label().to_string(), "GPU: Main");

        let unknown_bar = FlameBar {
            label: "c".into(),
            depth: 0,
            start_ns: 0,
            duration_ns: 100,
            category: None,
            gpu_pass_kind: None,
            element_type: None,
            element_source: None,
        };
        assert_eq!(unknown_bar.category_label().to_string(), "—");
    }

    // -- Flame chart render-path performance --------------------------------
    //
    // Regression coverage for the "hundreds/thousands of real GPUI elements
    // per flame chart render" perf bug: `render_flame_lanes_body` used to
    // build one absolutely-positioned, `.tooltip()`/`.on_click()`-bearing
    // `div()` (plus a nested label div, triggering text shaping) *per span
    // bar*, so a real capture with thousands of spans rebuilt, laid out,
    // hit-test-registered, and painted thousands of stateful interactive
    // elements on every single render. It's now a `wgpu_surface` instanced
    // draw plus at most a couple of real elements (hover tooltip, timeline
    // markers), regardless of span count.
    //
    // `gpui::CpuSpan::thread_id` is a `ThreadKey`, which has no public
    // constructor, so a synthetic `gpui::FrameCapture` can only carry
    // `gpu_spans` (see `build_flame_lanes_produces_a_flat_gpu_lane` above).
    // To get thousands of *CPU* spans too, this builds `FlameLane`/`FlameBar`
    // directly -- the exact data `render_flame_lanes_body` consumes, and the
    // only place upstream of it that `ThreadKey` doesn't gate.
    fn synthetic_flame_lanes(total_bars: usize) -> Vec<FlameLane> {
        let cpu_bar_count = total_bars * 3 / 4;
        let gpu_bar_count = total_bars - cpu_bar_count;

        let mut cpu_bars = Vec::with_capacity(cpu_bar_count);
        let mut start_ns: u64 = 0;
        let mut max_depth: u16 = 0;
        for i in 0..cpu_bar_count {
            // Cycle depth 0..=7 to approximate realistic nested call-stack
            // spans (element request_layout/prepaint/paint recursion).
            let depth = (i % 8) as u16;
            max_depth = max_depth.max(depth);
            let duration_ns: u32 = 2_000 + (i % 50) as u32 * 100;
            cpu_bars.push(FlameBar {
                label: format!("element_{i}").into(),
                depth,
                start_ns,
                duration_ns,
                category: Some(gpui::SpanCategory::ElementPaint),
                gpu_pass_kind: None,
                element_type: Some("Div".into()),
                element_source: Some("profiler.rs:1".into()),
            });
            start_ns += duration_ns as u64;
        }

        let mut gpu_bars = Vec::with_capacity(gpu_bar_count);
        let mut gpu_start_ns: u64 = 0;
        for i in 0..gpu_bar_count {
            let duration_ns: u32 = 5_000 + (i % 20) as u32 * 50;
            gpu_bars.push(FlameBar {
                label: format!("gpu_pass_{i}").into(),
                depth: 0,
                start_ns: gpu_start_ns,
                duration_ns,
                category: None,
                gpu_pass_kind: Some(gpui::GpuPassKind::Main),
                element_type: None,
                element_source: None,
            });
            gpu_start_ns += duration_ns as u64;
        }

        vec![
            FlameLane { label: "Main Thread (CPU)".into(), bars: cpu_bars, max_depth },
            FlameLane { label: "GPU".into(), bars: gpu_bars, max_depth: 0 },
        ]
    }

    /// Real, reproducible before/after measurement for the flame-chart perf
    /// fix: actually drives `render_flame_lanes_body` through a real (headless
    /// test-platform) window's layout/prepaint/paint pipeline via
    /// `VisualTestContext::draw`, not just constructing an `AnyElement` tree
    /// and asserting on it. Run with `cargo test -p ui --features
    /// "inspector,flamegraph" flame_chart_render_performance -- --nocapture`
    /// to see the printed mean/max timings.
    #[gpui::test]
    fn flame_chart_render_performance(cx: &mut gpui::TestAppContext) {
        use gpui::{point, size};

        cx.update(|cx| crate::init(cx));

        const TARGET_BARS: usize = 3200;
        let lanes = synthetic_flame_lanes(TARGET_BARS);
        let total_bars: usize = lanes.iter().map(|lane| lane.bars.len()).sum();
        assert!(total_bars >= 3000, "expected at least 3000 synthetic bars, got {total_bars}");

        let total_span_ns = lanes
            .iter()
            .flat_map(|lane| lane.bars.iter())
            .map(|bar| bar.start_ns + bar.duration_ns as u64)
            .max()
            .unwrap_or(1)
            .max(1);

        // `ProfilerPanel` doesn't implement `Render` (it's driven directly by
        // `inspector.rs` via a plain `fn render(&mut self, ...)`, not the
        // `Render` trait `add_window_view` requires), so this opens an empty
        // test window and constructs the panel entity inside it by hand --
        // the same `cx.new(|cx| ProfilerPanel::new(window, cx))` call
        // `profiler_panel()` makes in production.
        let cx = cx.add_empty_window();
        let view = cx.update(|window, app_cx| app_cx.new(|cx| ProfilerPanel::new(window, cx)));
        view.update(cx, |state, _cx| {
            // Mirrors what `render_flame_chart_section` does for a real
            // capture: the visible window must cover the data's time range,
            // or every bar gets culled before the (old) per-bar loop even
            // gets a chance to build an element for it.
            state.flame_zoom = RangeZoom::full(0.0, total_span_ns as f64);
        });

        // Stateful/interactive elements (the per-bar `.id()`/`.on_click()`
        // divs in the old implementation, and the hit-test overlay in the
        // new one) need `window.current_view()` to resolve at paint time,
        // which only gets pushed onto the window's rendered-entity stack
        // while painting an actual `Entity<impl Render>` -- drawing a
        // pre-built, disembodied `AnyElement` via `cx.draw` directly panics
        // (`current_view` unwraps an empty stack). This thin `Render` shim
        // re-enters `ProfilerPanel::render_flame_lanes_body` from inside a
        // real view render, so each timed iteration goes through the exact
        // same paint path production does.
        struct FlameBenchView {
            panel: Entity<ProfilerPanel>,
            lanes: Rc<Vec<FlameLane>>,
        }
        impl Render for FlameBenchView {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let lanes = self.lanes.clone();
                self.panel
                    .update(cx, |state, cx| state.render_flame_lanes_body(&lanes, None, None, window, cx))
            }
        }

        let lanes = Rc::new(lanes);

        const ITERATIONS: usize = 30;
        let mut durations: Vec<std::time::Duration> = Vec::with_capacity(ITERATIONS);
        for _ in 0..ITERATIONS {
            let panel = view.clone();
            let lanes = lanes.clone();
            let start = std::time::Instant::now();
            cx.draw(point(px(0.), px(0.)), size(px(1400.), px(20_000.)), move |_, cx| {
                cx.new(|_| FlameBenchView { panel, lanes })
            });
            durations.push(start.elapsed());
        }

        let total: std::time::Duration = durations.iter().sum();
        let mean = total / ITERATIONS as u32;
        let max = durations.iter().copied().max().unwrap();
        eprintln!(
            "[flame-chart-bench] bars={total_bars} lanes={} iterations={ITERATIONS} mean={:.4}ms max={:.4}ms",
            lanes.len(),
            mean.as_secs_f64() * 1000.0,
            max.as_secs_f64() * 1000.0,
        );
    }

    // -- GPU flame-bar geometry / hit-testing -------------------------------
    //
    // These are the pure-Rust pieces the GPU-instance builder and the
    // per-lane hit-test overlay both call, so both agree on where a bar is
    // drawn without needing any GPUI element or GPU surface to test.

    fn sample_flame_bar(label: &str, depth: u16, start_ns: u64, duration_ns: u32) -> FlameBar {
        FlameBar {
            label: label.to_string().into(),
            depth,
            start_ns,
            duration_ns,
            category: Some(gpui::SpanCategory::ElementPaint),
            gpu_pass_kind: None,
            element_type: None,
            element_source: None,
        }
    }

    #[test]
    fn bar_screen_rect_maps_a_bar_into_the_visible_window() {
        let bar = sample_flame_bar("a", 0, 250_000, 250_000);
        // Domain is 0..1_000_000ns, fully visible, chart is 1000px wide: the
        // bar covers [250, 500)px.
        let (x, width) = bar_screen_rect(&bar, 0.0, 1_000_000.0, 1000.0);
        assert_eq!(x, 250.0);
        assert_eq!(width, 250.0);
    }

    #[test]
    fn bar_screen_rect_clamps_a_vanishingly_short_bar_to_a_minimum_width() {
        let bar = sample_flame_bar("a", 0, 0, 1);
        let (_, width) = bar_screen_rect(&bar, 0.0, 1_000_000.0, 1000.0);
        assert_eq!(width, 1.5);
    }

    #[test]
    fn ns_to_x_maps_an_instant_inside_the_visible_window() {
        let x = ns_to_x(500_000, 0.0, 1_000_000.0, 1000.0);
        assert_eq!(x, Some(500.0));
    }

    #[test]
    fn ns_to_x_returns_none_outside_the_visible_window() {
        assert_eq!(ns_to_x(2_000_000, 0.0, 1_000_000.0, 1000.0), None);
        assert_eq!(ns_to_x(0, 500_000.0, 1_000_000.0, 1000.0), None);
    }

    fn sample_hit_bar(label: &str, depth: u16, x: f32, width: f32) -> HitBar {
        HitBar {
            bar: sample_flame_bar(label, depth, 0, 1000),
            x,
            width,
            top: depth as f32 * FLAME_ROW_HEIGHT,
        }
    }

    #[test]
    fn hit_test_lane_bar_finds_the_bar_under_the_cursor() {
        let hit_bars = vec![sample_hit_bar("a", 0, 0.0, 100.0), sample_hit_bar("b", 1, 50.0, 100.0)];

        let hit = hit_test_lane_bar(&hit_bars, gpui::point(px(60.0), px(5.0)));
        assert_eq!(hit.map(|hb| hb.bar.label.to_string()), Some("a".to_string()));

        let hit = hit_test_lane_bar(&hit_bars, gpui::point(px(60.0), px(25.0)));
        assert_eq!(hit.map(|hb| hb.bar.label.to_string()), Some("b".to_string()));
    }

    #[test]
    fn hit_test_lane_bar_misses_outside_any_bar() {
        let hit_bars = vec![sample_hit_bar("a", 0, 0.0, 100.0)];
        assert!(hit_test_lane_bar(&hit_bars, gpui::point(px(200.0), px(5.0))).is_none());
        assert!(hit_test_lane_bar(&hit_bars, gpui::point(px(60.0), px(-5.0))).is_none());
    }

    fn sample_gpu_span(name: &'static str, start_ns: u64, duration_ns: u32) -> gpui::GpuSpan {
        gpui::GpuSpan {
            name: gpui::SpanName::Static(name),
            start_ns,
            duration_ns,
            pass_kind: gpui::GpuPassKind::Main,
            query_set_generation: 0,
        }
    }

    #[test]
    fn build_flame_lanes_produces_a_flat_gpu_lane() {
        let frame = gpui::FrameCapture {
            frame_index: 0,
            window_id: 0,
            cpu_spans: Vec::new(),
            background_spans: Vec::new(),
            gpu_spans: vec![
                sample_gpu_span("main_pass", 0, 5_000_000),
                sample_gpu_span("submit", 5_000_000, 1_000_000),
            ],
            gpu_spans_finalized: true,
            gpu_spans_truncated: false,
            frame_start_ns: 0,
            frame_end_ns: 6_000_000,
            cpu_gpu_submit_ns: None,
            cpu_gpu_fence_observed_ns: None,
            counters: Default::default(),
        };

        let lanes = build_flame_lanes(&frame);
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].label.to_string(), "GPU");
        assert_eq!(lanes[0].bars.len(), 2);
        assert_eq!(lanes[0].max_depth, 0);
        assert_eq!(lanes[0].bars[0].label.to_string(), "main_pass");
        assert_eq!(lanes[0].bars[1].label.to_string(), "submit");
    }

    #[test]
    fn build_flame_lanes_is_empty_for_a_frame_with_no_spans() {
        let frame = gpui::FrameCapture {
            frame_index: 0,
            window_id: 0,
            cpu_spans: Vec::new(),
            background_spans: Vec::new(),
            gpu_spans: Vec::new(),
            gpu_spans_finalized: true,
            gpu_spans_truncated: false,
            frame_start_ns: 0,
            frame_end_ns: 0,
            cpu_gpu_submit_ns: None,
            cpu_gpu_fence_observed_ns: None,
            counters: Default::default(),
        };
        assert!(build_flame_lanes(&frame).is_empty());
    }

    fn sample_ui_node(type_name: &'static str, depth: u16) -> UiElementNode {
        UiElementNode {
            type_name,
            global_id_hash: 0,
            depth,
            bounds: Default::default(),
            style: None,
        }
    }

    #[test]
    fn flatten_ui_tree_rows_reconstructs_depth_first_order() {
        // root
        // ├── child_a
        // └── child_b
        let capture = gpui::UiTreeCapture {
            window_id: 0,
            nodes: vec![sample_ui_node("root", 0), sample_ui_node("child_a", 1), sample_ui_node("child_b", 1)],
            scene: Default::default(),
        };
        let replay = UiTreeReplay::new(capture);

        let rows = flatten_ui_tree_rows(&replay, &HashSet::new(), None);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].type_name.to_string(), "root");
        assert!(rows[0].has_children);
        assert_eq!(rows[1].type_name.to_string(), "child_a");
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[2].type_name.to_string(), "child_b");
    }

    #[test]
    fn flatten_ui_tree_rows_hides_collapsed_subtrees() {
        let capture = gpui::UiTreeCapture {
            window_id: 0,
            nodes: vec![
                sample_ui_node("root", 0),
                sample_ui_node("child_a", 1),
                sample_ui_node("grandchild", 2),
                sample_ui_node("child_b", 1),
            ],
            scene: Default::default(),
        };
        let replay = UiTreeReplay::new(capture);

        // Collapse `child_a` (index 1): its child (`grandchild`, index 2)
        // should disappear, but `child_b` (index 3) should still show.
        let mut collapsed = HashSet::new();
        collapsed.insert(1);
        let rows = flatten_ui_tree_rows(&replay, &collapsed, None);

        let names: Vec<String> = rows.iter().map(|r| r.type_name.to_string()).collect();
        assert_eq!(names, vec!["root", "child_a", "child_b"]);
    }

    #[test]
    fn flatten_ui_tree_rows_marks_the_selected_node() {
        let capture = gpui::UiTreeCapture {
            window_id: 0,
            nodes: vec![sample_ui_node("root", 0), sample_ui_node("child", 1)],
            scene: Default::default(),
        };
        let replay = UiTreeReplay::new(capture);

        let rows = flatten_ui_tree_rows(&replay, &HashSet::new(), Some(1));
        assert!(!rows[0].is_selected);
        assert!(rows[1].is_selected);
    }
}

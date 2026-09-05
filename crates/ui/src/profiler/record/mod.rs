//! The Record tab: a Chrome-DevTools-Performance-panel-style whole-capture
//! overview + detail view, spec'd in `.agents/PROFILER_UI_SPEC.md`. Not a
//! second capture mechanism — a different composition of exactly the
//! `Capture`/`FrameCapture` data the Flame Chart/Counters/Diagnostics tabs
//! already read (`ProfilerPanel::capture`), passed in rather than
//! duplicated, so there is exactly one source of truth for "what got
//! captured" shared across every tab.
//!
//! # Module layout
//!
//! Each region of the reference UI is its own file with its own local UI
//! state, composed by [`render`] below:
//!
//! - [`toolbar`] — the panel-specific toolbar (record/stop, clear, the
//!   `☑ Memory` track toggle, pop-out button). §1 of the spec.
//! - [`overview`] — the overview strip: time ruler + stacked CPU activity
//!   graph + frames row + drag-to-select. §2.
//! - [`flame`] — the detail flame chart for the selected range. §3.
//! - [`memory_chart`] — the multi-series step-line chart, shown under the
//!   detail flame chart when the toolbar's Memory toggle is on. §3 (memory
//!   track).
//! - [`summary`] / [`bottom_up`] — two of the four bottom tabs (`Summary`,
//!   `Bottom-up`; `Call tree`/`Event log` are stubbed placeholders in
//!   [`BottomTab`] below — no call-tree/event-log data model exists yet).
//!   §4.
//! - [`insights`] — the left sidebar: our own frame-timing headline metrics
//!   plus diagnostic "insight" cards, built from data this profiler already
//!   captures (`DiagnosticEvent`s, long-task-style spans) rather than
//!   pretending to have Core Web Vitals. §6.
//! - [`popout`] — the "open Record in its own window" feature (top-right of
//!   the Inspector, historically; now the toolbar).
//! - [`data`] — pure `Capture` → plain-struct data prep shared by several of
//!   the above (overview buckets, multi-frame flame lanes, bottom-up rows).
//! No GPUI in this one, deliberately.
//!
//! # State shape
//!
//! [`RecordState`] is one field on `ProfilerPanel` (`ProfilerPanel::record`)
//! rather than a separate `Entity` — the Record tab reads the *same*
//! `capture`/`capture_generation` the other tabs already own, so giving it
//! independent entity lifecycle would just create a second place those
//! could disagree. Every closure below is therefore a
//! `cx.listener(|panel: &mut ProfilerPanel, ev, window, cx| { panel.record.<field>...; cx.notify(); })`
//! reaching into its own nested substate, exactly like every other
//! multi-field `cx.listener` already in this crate (see
//! `ProfilerPanel::record_selection`'s pre-module-split history) — just
//! grouped under `record` instead of flattened onto `ProfilerPanel`
//! directly.

mod bottom_up;
mod data;
mod flame;
mod insights;
mod memory_chart;
mod overview;
mod popout;
mod summary;
mod toolbar;

use gpui::{
    canvas, div, prelude::FluentBuilder as _, px, AnyElement, AppContext as _, Bounds, Context,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Styled, Window,
};

use crate::{
    resizable::{resizable_panel, v_resizable},
    v_flex, ActiveTheme,
};

use super::ProfilerPanel;

pub(crate) use popout::RecordPopout;

/// The bottom tab group's four tabs (§4). `CallTree`/`EventLog` render a
/// documented placeholder — see [`Self::has_content`] — rather than being
/// omitted, so the tab strip itself still matches the reference UI's shape;
/// filling them in needs a real call-tree/event-log data model this
/// profiler doesn't have yet (deferred, same call `.agents/PROFILER_UI_SPEC.md`
/// already made for this tab's own Network/Timings/Interactions overview
/// rows).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum BottomTab {
    #[default]
    Summary,
    BottomUp,
    CallTree,
    EventLog,
}

impl BottomTab {
    pub(crate) const ALL: [BottomTab; 4] = [
        BottomTab::Summary,
        BottomTab::BottomUp,
        BottomTab::CallTree,
        BottomTab::EventLog,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            BottomTab::Summary => "Summary",
            BottomTab::BottomUp => "Bottom-up",
            BottomTab::CallTree => "Call tree",
            BottomTab::EventLog => "Event log",
        }
    }

    fn has_content(self) -> bool {
        matches!(self, BottomTab::Summary | BottomTab::BottomUp)
    }
}

/// All Record-tab state — see the module doc's "State shape" section for why
/// this is a plain nested struct rather than its own `Entity`.
#[derive(Default)]
pub(crate) struct RecordState {
    pub(crate) toolbar: toolbar::ToolbarState,
    pub(crate) overview: overview::OverviewState,
    pub(crate) flame: flame::FlameState,
    pub(crate) memory: memory_chart::MemoryState,
    pub(crate) bottom_up: bottom_up::BottomUpState,
    pub(crate) insights: insights::InsightsState,
    pub(crate) bottom_tab: BottomTab,
    /// Drag-selected `(start_ns, end_ns)` on the overview strip; `None`
    /// means "the whole capture" (the initial state, and what a
    /// double-click-to-clear returns to).
    pub(crate) selection: Option<(u64, u64)>,
    /// `Some` while this tab's content is showing in its own top-level
    /// window (see [`popout`]) — the docked copy shows a placeholder
    /// instead, since a GPU-surfaced flame chart can't correctly present
    /// into two windows' swap chains at once.
    pub(crate) popped_out: Option<gpui::WindowHandle<crate::Root>>,
    /// Computed once when a capture stops (see [`Self::on_capture_stopped`]),
    /// not re-bucketed from every frame's spans on every render.
    overview_data: Option<data::RecordOverview>,
    /// Cached [`data::build_flame_lanes_for_range`] output for the current
    /// `(capture_generation, selection)` pair.
    lane_cache: Option<data::RecordLaneCache>,
    /// Cached [`data::build_bottom_up_rows`] output for the current
    /// `(capture_generation, selection)` pair — see
    /// [`data::RecordBottomUpCache`]'s field doc for why this matters.
    bottom_up_cache: Option<data::RecordBottomUpCache>,
    /// Measured bounds of `#record-main-panels-slot` (see [`render_content`]'s
    /// own bounds-capture canvas) -- the *one* authoritative source every
    /// panel in the vertically-stacked group below reads its width from,
    /// via an explicit `.w(px(..))` rather than any flex/percentage
    /// resolution. This exists specifically so that panel is never again
    /// sized off of Taffy's cross-axis resolution inside a `resizable_panel`
    /// row (documented, historically unreliable there -- see
    /// `overview::render`'s own `#record-overview` doc comment for the full
    /// mechanism), and so no *sibling* panel's own content can ever perturb
    /// another panel's width: every panel reads the exact same number,
    /// measured once, upstream of all of them.
    panels_bounds: gpui::Bounds<Pixels>,
}

impl RecordState {
    /// Resets everything for a fresh capture, except the pop-out window
    /// handle (a capture starting/stopping shouldn't yank a window the user
    /// deliberately opened out from under them).
    pub(crate) fn on_capture_started(&mut self) {
        let popped_out = self.popped_out.take();
        *self = RecordState {
            popped_out,
            ..RecordState::default()
        };
    }

    /// Computed once right as a capture stops — see `overview_data`'s field
    /// doc.
    pub(crate) fn on_capture_stopped(&mut self, frames: &[&gpui::FrameCapture]) {
        self.overview_data = data::build_overview(frames);
        self.selection = None;
        self.lane_cache = None;
        self.bottom_up_cache = None;
    }

    pub(crate) fn overview_data(&self) -> Option<&data::RecordOverview> {
        self.overview_data.as_ref()
    }
}

/// Renders the Record tab as it appears in the *docked* Inspector: the real
/// content ([`render_content`]), unless the tab is currently showing in its
/// own pop-out window, in which case a placeholder — see
/// `RecordState::popped_out`'s field doc.
pub(crate) fn render(
    panel: &mut ProfilerPanel,
    window: &mut Window,
    cx: &mut Context<ProfilerPanel>,
) -> AnyElement {
    if let Some(handle) = panel.record.popped_out {
        return popout::docked_placeholder(handle, cx);
    }
    render_content(panel, window, cx)
}

/// Renders the whole Record tab's actual content: the Insights sidebar on
/// the left (§6), and on the right a fixed toolbar (§1) above a vertically
/// resizable stack of the overview strip (§2), the detail flame chart plus
/// (when toggled) the memory chart (§3), and the bottom tab group (§4) —
/// the same resizable-pane system every other split in this app uses
/// (`h_resizable`/`v_resizable`), not fixed heights. Called both from
/// [`render`] (the docked path) and from [`popout::RecordPopout`] (which
/// bypasses the pop-out placeholder check above since *it* is the popped-out
/// window).
pub(crate) fn render_content(
    panel: &mut ProfilerPanel,
    window: &mut Window,
    cx: &mut Context<ProfilerPanel>,
) -> AnyElement {
    let is_recording = panel.capture_handle.is_some();
    let has_capture = panel.capture.is_some();

    recompute_for_selection(panel);

    let toolbar = toolbar::render(
        &mut panel.record.toolbar,
        is_recording,
        has_capture,
        window,
        cx,
    );

    let Some(capture) = panel.capture.as_ref() else {
        return v_flex()
            .size_full()
            .child(toolbar)
            .child(super::profiler_empty_state(
                "Start and stop a capture to lay the whole recording out on one zoomable \
                 timeline: drag on the overview below to select a range, then inspect it in \
                 the detail flame chart, Summary, and Bottom-up tabs underneath.",
                cx,
            ))
            .into_any_element();
    };
    if capture.frame_count() == 0 {
        return v_flex()
            .size_full()
            .child(toolbar)
            .child(super::profiler_empty_state(
                "The capture stopped with no frames recorded.",
                cx,
            ))
            .into_any_element();
    }

    // The one authoritative width every panel below is given explicitly --
    // see `RecordState::panels_bounds`'s field doc. `<= 1.0` means "not
    // measured yet" (the very first render of this tab), in which case
    // each panel's own `.when(panels_width > 1.0, ..)` below leaves its
    // ordinary flex-based sizing in place for that one frame instead of
    // collapsing to zero width.
    let panels_width = f32::from(panel.record.panels_bounds.size.width);

    let show_memory = panel.record.toolbar.show_memory;
    let selection = panel.record.selection;
    let overview_data = panel.record.overview_data.as_ref();
    // Borrowed, not `.clone()`'d: this can run to thousands of frames deep
    // into a long capture, and every render (every hover, every mouse
    // move) used to pay for a full `Vec<f32>` allocation + copy here for no
    // reason -- `panel.frame_durations_ms` and the disjoint fields borrowed
    // mutably below (`panel.record.overview`/`.insights`) don't conflict,
    // same as `capture`'s own borrow past this point (see the comment a few
    // lines down).
    let frame_durations_ms = &panel.frame_durations_ms;
    // `recompute_for_selection` above already synced `flame_zoom`'s
    // *domain* to `selection` (a no-op if `selection` hasn't changed since
    // last render -- see that function's own doc comment), so its current
    // *visible* window is exactly the detail chart's own zoom/pan state
    // within that domain: the whole domain until the user actually zooms,
    // narrower after. Passing this to the overview lets it show that
    // zoomed-in range as its own selection box -- syncing detail-view zoom
    // back to the overview -- without ever writing to `selection` itself
    // (which stays purely "what a direct drag on the overview last set");
    // see `overview::render`'s own doc comment for why keeping those two
    // separate is what avoids a feedback loop between them.
    // While a drag-select gesture is actively changing `selection` on
    // every tick, `recompute_for_selection` above deliberately left
    // `flame_zoom`'s domain frozen at whatever it was before the drag
    // started (see that function's own doc comment) -- so `detail_visible`
    // would otherwise report that *stale* window here, and since it's
    // preferred first below (`detail_visible.or(selection)`), the overview
    // strip's own selection box would appear to freeze mid-drag even
    // though `selection` itself is still updating live. `None` here falls
    // back to the live `selection` value instead, so the box still tracks
    // the pointer in real time; the (expensive, and correctly deferred)
    // zoomed-window reflection resumes once the drag ends and one real
    // recompute runs.
    let detail_visible = if panel.record.overview.is_dragging_selection() {
        None
    } else {
        selection.map(|_| {
            (
                panel.flame_zoom.visible_start().round() as u64,
                panel.flame_zoom.visible_end().round() as u64,
            )
        })
    };

    let overview_element = overview::render(
        &mut panel.record.overview,
        overview_data,
        selection,
        detail_visible,
        frame_durations_ms,
        panel.capture.as_ref(),
        panel.capture_generation,
        panels_width,
        window,
        cx,
    );

    let lanes = panel
        .record
        .lane_cache
        .as_ref()
        .map(|c| c.lanes.clone())
        .unwrap_or_default();
    let lane_max_bar_duration_ns = panel
        .record
        .lane_cache
        .as_ref()
        .map(|c| c.max_bar_duration_ns.clone())
        .unwrap_or_default();

    // Everything below that still needs `capture` (borrowed from
    // `panel.capture`) runs *before* `flame::render`/`render_bottom_tabs`,
    // which each take `panel: &mut ProfilerPanel` wholesale (they call
    // `&mut self` methods that genuinely touch several of `panel`'s
    // fields at once, e.g. `render_flame_lanes_body`'s shared zoom/GPU
    // state) — the borrow checker can split disjoint *field* borrows
    // through one expression (`&mut panel.record.memory` alongside
    // `capture`), but not a `capture: &Capture` still alive across a call
    // that reborrows `panel` in full. Ordering it this way, rather than
    // re-fetching `panel.capture.as_ref()` redundantly, keeps `capture`
    // bound once and lets its last use naturally precede those calls.
    let frame_durations_max_ms = panel.frame_durations_max_ms;
    let (sel_start, sel_end) = current_selection_bounds(panel);
    let bottom_up_rows = recompute_bottom_up_for_selection(
        &mut panel.record,
        capture,
        panel.capture_generation,
        sel_start,
        sel_end,
    );
    let memory_element = show_memory.then(|| {
        memory_chart::render(
            &mut panel.record.memory,
            capture,
            sel_start,
            sel_end,
            panels_width,
            window,
            cx,
        )
    });
    let insights_element = insights::render(
        &mut panel.record.insights,
        Some(capture),
        panel.capture_generation,
        frame_durations_ms,
        frame_durations_max_ms,
        window,
        cx,
    );

    let flame_element = flame::render(
        panel,
        &lanes,
        &lane_max_bar_duration_ns,
        panels_width,
        window,
        cx,
    );
    let bottom_tab = panel.record.bottom_tab;
    let bottom_element =
        render_bottom_tabs(panel, &bottom_up_rows, bottom_tab, panels_width, window, cx);

    let mut main_column = v_resizable("record-panels")
        .child(
            resizable_panel()
                .size(px(96.))
                .size_range(px(64.)..px(220.))
                .child(overview_element),
        )
        .child(
            resizable_panel()
                .size(px(320.))
                .size_range(px(120.)..px(4000.))
                .child(flame_element),
        );
    if let Some(memory_element) = memory_element {
        main_column = main_column.child(
            resizable_panel()
                .size(px(180.))
                .size_range(px(80.)..px(2000.))
                .child(memory_element),
        );
    }
    main_column = main_column.child(
        resizable_panel()
            .size_range(px(120.)..px(4000.))
            .child(bottom_element),
    );

    div()
        .id("record-tab")
        .size_full()
        .flex()
        .child(
            div()
                .id("record-sidebar-slot")
                .h_full()
                .w(px(240.))
                .min_w(px(180.))
                .max_w(px(420.))
                .flex_shrink_0()
                .border_r_1()
                .border_color(cx.theme().border)
                .child(insights_element),
        )
        .child(
            v_flex()
                .id("record-main-column")
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .child(toolbar)
                .child(
                    div()
                        .id("record-main-panels-slot")
                        .relative()
                        .flex_1()
                        .min_h(px(0.))
                        .child(main_column)
                        .child({
                            // Measures this slot's own bounds -- the single
                            // authoritative width every panel in the group
                            // above reads back on the *next* render via
                            // `panels_width` (see `RecordState::panels_bounds`'s
                            // field doc). This slot's own sizing (`flex_1()`
                            // for height, default cross-axis stretch for
                            // width, no percentage anywhere) is exactly the
                            // shape already established as reliable
                            // elsewhere in this module, unlike anything
                            // further down inside the resizable group
                            // itself.
                            let entity = cx.entity().clone();
                            canvas(
                                move |bounds, _window, cx| {
                                    entity.update(cx, |panel, cx| {
                                        if super::update_measured_bounds(
                                            &mut panel.record.panels_bounds,
                                            bounds,
                                        ) {
                                            cx.notify();
                                        }
                                    });
                                },
                                |_, _, _, _| {},
                            )
                            .absolute()
                            .size_full()
                        }),
                ),
        )
        .into_any_element()
}

fn current_selection_bounds(panel: &ProfilerPanel) -> (u64, u64) {
    let Some(overview) = panel.record.overview_data.as_ref() else {
        return (0, 0);
    };
    panel
        .record
        .selection
        .unwrap_or((overview.domain_start_ns, overview.domain_end_ns))
}

/// Keeps `lane_cache` matched to the current `(capture_generation,
/// selection)` pair, mirroring the Flame Chart tab's own `flame_lane_cache`
/// pattern: only a genuinely different capture or selection pays for
/// `build_flame_lanes_for_range`; re-renders for hover/search on the same
/// range just clone the `Rc`.
///
/// Also skips that rebuild entirely — leaving the *previous* range's lanes
/// on screen, stale but stable — while a drag-select gesture on the
/// overview strip is actively in progress (see
/// `overview::OverviewState::is_dragging_selection`'s doc comment). Without
/// this, every one of the dozens of `on_drag_move` ticks a single drag
/// fires (mouse-move samples arrive far faster than a human notices) called
/// this — each one a fresh `build_flame_lanes_for_range` walk of every span
/// in whatever range the selection had grown to *by that tick* — which is
/// exactly what made dragging a large selection visibly tank the frame
/// rate: the flame chart's own GPU-instanced rendering was never the
/// expensive part, rebuilding its *input data* dozens of times a second was.
/// The one real rebuild this gate defers happens exactly once, right when
/// the drag ends (`overview`'s `on_mouse_up` handlers all force a render
/// after clearing the drag flags this checks).
fn recompute_for_selection(panel: &mut ProfilerPanel) {
    let Some(capture) = panel.capture.as_ref() else {
        return;
    };
    if panel.record.overview.is_dragging_selection() && panel.record.lane_cache.is_some() {
        return;
    }
    let (start_ns, end_ns) = current_selection_bounds(panel);
    let capture_generation = panel.capture_generation;
    let cache_hit = panel
        .record
        .lane_cache
        .as_ref()
        .is_some_and(|c| c.capture_generation == capture_generation && c.range == (start_ns, end_ns));
    if cache_hit {
        return;
    }
    let lanes = data::build_flame_lanes_for_range(capture, start_ns, end_ns);
    let max_bar_duration_ns = std::rc::Rc::new(
        lanes
            .iter()
            .map(|lane| lane.bars.iter().map(|b| b.duration_ns as u64).max().unwrap_or(0))
            .collect(),
    );
    panel.record.lane_cache = Some(data::RecordLaneCache {
        capture_generation,
        range: (start_ns, end_ns),
        lanes: std::rc::Rc::new(lanes),
        max_bar_duration_ns,
    });
    // Keeps the (shared, see `ProfilerPanel::flame_zoom`'s field doc) detail
    // chart's time axis matched to the current selection before `flame`
    // reads it back out, same ordering `render_flame_chart_section` uses
    // for the selected frame's own bounds.
    panel
        .flame_zoom
        .set_domain(start_ns as f64, end_ns.max(start_ns + 1) as f64);
}

/// Keeps `bottom_up_cache` matched to the current `(capture_generation,
/// selection)` pair, exactly mirroring [`recompute_for_selection`]'s own
/// cache above — `build_bottom_up_rows` walks every CPU span of every frame
/// *within the selected range* (it skips whole out-of-range frames outright,
/// same as `build_flame_lanes_for_range`), so it must not run on every
/// render the way it used to (see [`data::RecordBottomUpCache`]'s field doc
/// for what that cost). A genuinely different capture or selection
/// recomputes; re-renders for hover/search/tab-switching on the same range
/// just clone the `Rc`.
///
/// Takes `record`/`capture` as separate disjoint-field borrows rather than
/// `panel: &mut ProfilerPanel` wholesale — same reason `render_content`
/// passes `&mut panel.record.memory` alongside `capture` to
/// `memory_chart::render` instead of `panel` itself: `capture` (borrowed
/// from `panel.capture`) is still alive and re-used by later calls in
/// `render_content`, which a whole-`panel` mutable borrow here would
/// conflict with.
fn recompute_bottom_up_for_selection(
    record: &mut RecordState,
    capture: &gpui::Capture,
    capture_generation: u64,
    start_ns: u64,
    end_ns: u64,
) -> std::rc::Rc<Vec<data::BottomUpRow>> {
    // Same drag-in-progress gate as `recompute_for_selection`, and for the
    // exact same reason: skip the rebuild (keep serving the previous
    // range's stale-but-stable rows) while the selection is still actively
    // changing dozens of times a second, rather than paying for a fresh
    // `build_bottom_up_rows` walk on every one of those ticks.
    let dragging = record.overview.is_dragging_selection();
    let cache_hit = record
        .bottom_up_cache
        .as_ref()
        .is_some_and(|c| c.capture_generation == capture_generation && c.range == (start_ns, end_ns));
    if !cache_hit && !(dragging && record.bottom_up_cache.is_some()) {
        let rows = std::rc::Rc::new(data::build_bottom_up_rows(capture, start_ns, end_ns));
        record.bottom_up_cache = Some(data::RecordBottomUpCache {
            capture_generation,
            range: (start_ns, end_ns),
            rows,
        });
    }
    record
        .bottom_up_cache
        .as_ref()
        .expect("just populated above on a cache miss")
        .rows
        .clone()
}

fn render_bottom_tabs(
    panel: &mut ProfilerPanel,
    rows: &[data::BottomUpRow],
    active: BottomTab,
    // The one authoritative panel width every panel in the resizable group
    // shares -- see `RecordState::panels_bounds`'s field doc. `<= 1.0`
    // means "not measured yet".
    panels_width: f32,
    window: &mut Window,
    cx: &mut Context<ProfilerPanel>,
) -> AnyElement {
    use crate::{
        button::{Button, ButtonVariants as _},
        h_flex,
        styled::Disableable as _,
        Selectable as _, Sizable as _,
    };

    let tab_row = h_flex().gap_1().px_2().py_1().children(BottomTab::ALL.map(|tab| {
        Button::new(gpui::SharedString::from(format!(
            "record-bottom-tab-{}",
            tab.label()
        )))
        .xsmall()
            .ghost()
            .selected(tab == active)
            .disabled(!tab.has_content())
            .label(tab.label())
            .on_click(cx.listener(move |panel, _, _window, cx| {
                if tab.has_content() {
                    panel.record.bottom_tab = tab;
                    cx.notify();
                }
            }))
    }));

    let body = match active {
        BottomTab::Summary => summary::render(rows, cx),
        BottomTab::BottomUp => bottom_up::render(&mut panel.record.bottom_up, rows, window, cx),
        BottomTab::CallTree | BottomTab::EventLog => super::profiler_empty_state(
            "Not built yet: this tab needs a real call-tree/event-log data model this \
             profiler doesn't collect today (see `.agents/PROFILER_UI_SPEC.md`'s own \
             deferral note for this section).",
            cx,
        ),
    };

    v_flex()
        .id("record-bottom-tabs")
        // `flex_1().size_full()` is only this frame's fallback (before
        // `panels_width` is measured); `.when(..)` below overrides it with
        // an explicit pixel width shared by every panel in the resizable
        // group -- see `RecordState::panels_bounds`'s field doc.
        .flex_1()
        .size_full()
        .when(panels_width > 1.0, |el| el.w(px(panels_width)))
        .border_t_1()
        .border_color(cx.theme().border)
        .child(tab_row)
        .child(
            div()
                .id("record-bottom-tab-body")
                .flex_1()
                .min_h(px(0.))
                .overflow_hidden()
                .child(body),
        )
        .into_any_element()
}

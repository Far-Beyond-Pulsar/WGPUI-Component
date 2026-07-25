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
    div, img, prelude::FluentBuilder as _, px, uniform_list, AnyElement, App, AppContext as _,
    Context, DeepCaptureDrawCall, DeepCaptureReplay, DrawCallResourceStatus, Entity, FontWeight,
    Global, ImageSource, Inspector, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, RenderImage, SharedString, StatefulInteractiveElement as _, Styled,
    Subscription, Task, Timer, UiElementNode, UiTreeReplay, Window,
};

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
                        ProfilerSection::FlameChart => self.render_flame_chart_section(cx),
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
            .segmented()
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
            self.selected_frame = capture.frame_count().saturating_sub(1);
            self.capture = Some(capture);
            self.selected_span = None;
            self.capture_error = None;
        } else {
            match gpui::start_capture(gpui::CaptureOptions::default()) {
                Ok(handle) => {
                    self.capture_handle = Some(handle);
                    self.capture = None;
                    self.selected_span = None;
                    self.capture_error = None;
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

    fn render_flame_chart_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let is_recording = self.capture_handle.is_some();
        let frame_count = self.capture.as_ref().map(|c| c.frame_count()).unwrap_or(0);

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

        let mut body = v_flex().gap_2().size_full().child(header);
        if let Some(err) = self.capture_error.clone() {
            body = body.child(Alert::error("flame-capture-error", err));
        }
        body = body.child(
            div()
                .id("flame-chart-scroll")
                .flex_1()
                .min_h(px(0.))
                .overflow_scroll()
                .px_2()
                .pb_2()
                .child(self.render_flame_chart_body(cx)),
        );
        if let Some(span) = self.selected_span.clone() {
            body = body.child(self.render_selected_span_details(&span, cx));
        }

        body.into_any_element()
    }

    fn render_flame_chart_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
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

        let lanes = build_flame_lanes(frame);
        if lanes.is_empty() {
            return profiler_empty_state("This frame recorded no spans.", cx);
        }

        let frame_start_ns = frame.frame_start_ns;
        let total_ns = frame.frame_end_ns.saturating_sub(frame_start_ns).max(1) as f32;
        const PIXELS_PER_MS: f32 = 8.0;
        const ROW_HEIGHT: f32 = 20.0;
        let chart_width = ((total_ns / 1.0e6) * PIXELS_PER_MS).max(500.0);

        let search = self.flame_search.to_lowercase();
        let selected_key = self
            .selected_span
            .as_ref()
            .map(|s| (s.name.clone(), s.depth, s.duration_ns));

        let mut lane_elements: Vec<AnyElement> = Vec::new();
        for lane in &lanes {
            lane_elements.push(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .mt(px(6.))
                    .child(format!("{} ({} spans)", lane.label, lane.bars.len()))
                    .into_any_element(),
            );

            let lane_height = (lane.max_depth as f32 + 1.0) * ROW_HEIGHT;
            let mut bar_elements: Vec<AnyElement> = Vec::new();

            for (bar_index, bar) in lane.bars.iter().enumerate() {
                let x = ((bar.start_ns.saturating_sub(frame_start_ns)) as f32 / total_ns) * chart_width;
                let width = ((bar.duration_ns as f32) / total_ns * chart_width).max(1.5);
                let color = match (bar.category, bar.gpu_pass_kind) {
                    (Some(cat), _) => category_color(cat, cx),
                    (None, Some(kind)) => gpu_pass_color(kind, cx),
                    _ => cx.theme().chart_1,
                };
                let matches_search = search.is_empty() || bar.label.to_lowercase().contains(&search);
                let is_selected = selected_key.as_ref().is_some_and(|(name, depth, dur)| {
                    *name == bar.label && *depth == bar.depth && *dur == bar.duration_ns
                });

                let tooltip_text: SharedString = format!(
                    "{}\n{:.3}ms · depth {} · {}",
                    bar.label,
                    bar.duration_ns as f64 / 1.0e6,
                    bar.depth,
                    bar.category_label()
                )
                .into();

                let lane_label = lane.label.clone();
                let click_bar = bar.clone();

                bar_elements.push(
                    div()
                        .id(SharedString::from(format!(
                            "flame-{}-{}-{}",
                            lane.label, bar.start_ns, bar_index
                        )))
                        .absolute()
                        .top(px(bar.depth as f32 * ROW_HEIGHT))
                        .left(px(x))
                        .w(px(width))
                        .h(px(ROW_HEIGHT - 2.0))
                        .rounded_sm()
                        .bg(color)
                        .when(!matches_search, |d| d.opacity(0.25))
                        .when(is_selected, |d| d.border_2().border_color(cx.theme().foreground))
                        .overflow_hidden()
                        .cursor_pointer()
                        .tooltip(move |window, cx| Tooltip::new(tooltip_text.clone()).build(window, cx))
                        .on_click(cx.listener(move |state, _, _window, cx| {
                            state.selected_span = Some(SelectedSpan {
                                name: click_bar.label.clone(),
                                lane: lane_label.clone(),
                                category_label: click_bar.category_label(),
                                depth: click_bar.depth,
                                duration_ns: click_bar.duration_ns,
                                element_type: click_bar.element_type.clone(),
                                element_source: click_bar.element_source.clone(),
                            });
                            cx.notify();
                        }))
                        .child(
                            div()
                                .px(px(3.))
                                .text_xs()
                                .text_color(cx.theme().background)
                                .child(bar.label.clone()),
                        )
                        .into_any_element(),
                );
            }

            lane_elements.push(
                div()
                    .relative()
                    .w(px(chart_width))
                    .h(px(lane_height))
                    .children(bar_elements)
                    .into_any_element(),
            );
        }

        v_flex()
            .id("flame-chart-canvas")
            .gap_1()
            .children(lane_elements)
            .into_any_element()
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
        let summary = capture.counter_summary();

        v_flex()
            .gap_3()
            .p_2()
            .child(render_counter_tiles(&summary, cx))
            .child(render_draw_call_table(&summary, cx))
            .child(render_atlas_and_events(&summary, cx))
            .child(render_frame_duration_sparkline(capture, cx))
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
            move |range: Range<usize>, _window: &mut Window, _cx: &mut App| {
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
                            .when(row.is_selected, |s| s.bg(gpui::rgba(0x3070ff33)));

                        if row.has_children {
                            let entity_for_toggle = entity.clone();
                            el = el.child(
                                div()
                                    .w(px(14.))
                                    .text_color(gpui::rgba(0x888888ff))
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
                            .child(div().text_color(gpui::rgba(0x8888ffff)).child(row.type_name.clone()))
                            .child(
                                div()
                                    .ml(px(6.))
                                    .text_color(gpui::rgba(0x888888ff))
                                    .child(row.bounds_label.clone()),
                            );

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
        self.render_deep_capture_preview(window, cx);
        cx.notify();
    }

    fn seek_deep_capture(&mut self, step: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(replay) = self.deep_capture_replay.as_mut() {
            replay.seek(step);
        }
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

        let viewport = window.viewport_size();
        let width = (f32::from(viewport.width) as u32).clamp(1, 2048);
        let height = (f32::from(viewport.height) as u32).clamp(1, 2048);

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
        let title = div()
            .text_xs()
            .font_weight(FontWeight::BOLD)
            .text_color(cx.theme().foreground)
            .child("Live Preview");

        match &self.deep_capture_preview {
            None => v_flex()
                .gap_1()
                .child(title)
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Step to a draw call to render its preview."),
                )
                .into_any_element(),
            Some(DeepCapturePreview::Unavailable(message)) => v_flex()
                .gap_1()
                .child(title)
                .child(Alert::warning("deep-capture-preview-unavailable", message.clone()))
                .into_any_element(),
            Some(DeepCapturePreview::Image { image, width, height, texture_unavailable }) => {
                let display_width = 280.0f32;
                let scale = display_width / (*width as f32).max(1.0);
                let display_height = ((*height as f32) * scale).max(1.0);

                v_flex()
                    .gap_1()
                    .child(title)
                    .child(
                        div()
                            .border_1()
                            .border_color(cx.theme().border)
                            .rounded_md()
                            .overflow_hidden()
                            .w(px(display_width))
                            .h(px(display_height))
                            .child(
                                img(ImageSource::Render(image.clone()))
                                    .w(px(display_width))
                                    .h(px(display_height)),
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
        }
    }
}

fn profiler_empty_state(message: impl Into<SharedString>, cx: &Context<ProfilerPanel>) -> AnyElement {
    div()
        .p(px(12.))
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(message.into())
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
        .into_any_element()
}

fn render_frame_duration_sparkline(capture: &gpui::Capture, cx: &Context<ProfilerPanel>) -> AnyElement {
    let durations_ms: Vec<f32> = capture
        .frames()
        .map(|f| (f.frame_end_ns.saturating_sub(f.frame_start_ns)) as f32 / 1.0e6)
        .collect();
    let max_ms = durations_ms.iter().cloned().fold(0.0f32, f32::max).max(1.0);
    const SPARKLINE_HEIGHT: f32 = 60.0;

    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(cx.theme().foreground)
                .child(format!("Frame Duration Over Capture Window (max {:.2}ms)", max_ms)),
        )
        .child(
            div()
                .id("frame-duration-sparkline")
                .h(px(SPARKLINE_HEIGHT))
                .overflow_x_scroll()
                .flex()
                .items_end()
                .gap(px(1.))
                .children(durations_ms.into_iter().map(|ms| {
                    let height = ((ms / max_ms) * SPARKLINE_HEIGHT).max(1.0);
                    let color = if ms > 16.7 { cx.theme().danger } else { cx.theme().chart_1 };
                    div().w(px(3.)).h(px(height)).bg(color).into_any_element()
                })),
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

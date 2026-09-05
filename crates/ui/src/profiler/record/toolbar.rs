//! The Record tab's own toolbar (§1 of `.agents/PROFILER_UI_SPEC.md`):
//! record/stop, clear selection, the `☑ Memory` track toggle, the
//! `☑ Screenshots` filmstrip toggle, and the pop-out-window button.
//! Chrome's reference toolbar also has an import/export pair, a saved-
//! recordings picker, `☐ Dim 3rd parties`, and settings/help icons —
//! deliberately left out rather than rendered as permanently-inert chrome:
//! there's no capture serialization format and no "3rd party origin"
//! concept in a UI framework's own profiler. Matches this spec's own
//! build-order note ("stub what's honest, skip the rest") for the overview
//! strip's Network/Timings/Interactions rows.
//!
//! `☑ Screenshots` is real, unlike the earlier "no filmstrip capability"
//! note this doc comment used to carry: it now gates
//! `gpui::CaptureOptions::capture_screenshots` for the *next* capture
//! (`ProfilerPanel::toggle_capture` reads `ToolbarState::capture_screenshots`
//! directly when starting one — see that function's own comment for why),
//! so it's disabled mid-recording (the option is fixed at `start_capture`
//! time and flipping the checkbox then wouldn't retroactively apply to the
//! session already running).

use gpui::{
    div, prelude::FluentBuilder as _, AnyElement, AppContext as _, Context,
    InteractiveElement as _, IntoElement, ParentElement as _, Styled, Window,
};

use crate::{
    button::{Button, ButtonVariants as _},
    h_flex,
    styled::Disableable as _,
    ActiveTheme, IconName, Selectable as _, Sizable as _,
};

use super::{popout, ProfilerPanel};

pub(crate) struct ToolbarState {
    /// Whether the memory step-line chart pane (§3, memory track) renders
    /// below the detail flame chart.
    pub(crate) show_memory: bool,
    /// Whether the *next* capture should sample periodic screenshots
    /// (`gpui::CaptureOptions::capture_screenshots`) — read directly by
    /// `ProfilerPanel::toggle_capture` when it starts a session, not passed
    /// as a parameter through this module's own `render`. Has no effect on
    /// a capture already in progress (`pub(crate)` rather than private for
    /// exactly that cross-module read).
    pub(crate) capture_screenshots: bool,
}

impl Default for ToolbarState {
    // Not `#[derive(Default)]`: `capture_screenshots` defaults to `true`,
    // not `bool::default()`'s `false` -- recording without a filmstrip is
    // the surprising case a user has to explicitly ask for by unchecking
    // the toggle, not the other way around.
    fn default() -> Self {
        Self {
            show_memory: false,
            capture_screenshots: true,
        }
    }
}

pub(crate) fn render(
    state: &mut ToolbarState,
    is_recording: bool,
    has_capture: bool,
    _window: &mut Window,
    cx: &mut Context<ProfilerPanel>,
) -> AnyElement {
    let show_memory = state.show_memory;
    let capture_screenshots = state.capture_screenshots;
    let popped_out = false; // caller only renders this when docked and visible.

    h_flex()
        .id("record-toolbar")
        .gap_2()
        .items_center()
        .flex_wrap()
        .p_2()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            Button::new("record-toggle-capture")
                .small()
                .when(is_recording, |b| b.danger())
                .when(!is_recording, |b| b.primary())
                .icon(if is_recording {
                    IconName::Pause
                } else {
                    IconName::Play
                })
                .label(if is_recording {
                    "Stop"
                } else {
                    "Record"
                })
                .tooltip(if is_recording {
                    "Stop capture"
                } else {
                    "Start capture"
                })
                .on_click(cx.listener(|panel, _, _window, cx| panel.toggle_capture(cx))),
        )
        .child(
            Button::new("record-clear-selection")
                .small()
                .ghost()
                .icon(IconName::Delete)
                .tooltip("Clear the selected range")
                .disabled(!has_capture)
                .on_click(cx.listener(|panel, _, _window, cx| {
                    panel.record.selection = None;
                    cx.notify();
                })),
        )
        .child(div().flex_1())
        .child(
            Button::new("record-toggle-screenshots")
                .xsmall()
                .ghost()
                .selected(capture_screenshots)
                .icon(IconName::Image)
                .label("Screenshots")
                .tooltip(if is_recording {
                    "Applies to the next capture (fixed for one already running)"
                } else {
                    "Sample periodic screenshots on the next capture"
                })
                .disabled(is_recording)
                .on_click(cx.listener(|panel, _, _window, cx| {
                    panel.record.toolbar.capture_screenshots =
                        !panel.record.toolbar.capture_screenshots;
                    cx.notify();
                })),
        )
        .child(
            Button::new("record-toggle-memory")
                .xsmall()
                .ghost()
                .selected(show_memory)
                .icon(IconName::Cpu)
                .label("Memory")
                .tooltip("Show/hide the memory chart")
                .on_click(cx.listener(|panel, _, _window, cx| {
                    panel.record.toolbar.show_memory = !panel.record.toolbar.show_memory;
                    cx.notify();
                })),
        )
        .child(
            Button::new("record-pop-out")
                .xsmall()
                .ghost()
                .icon(IconName::ExternalLink)
                .tooltip("Open Record in its own window")
                .disabled(popped_out)
                .on_click(cx.listener(|panel, _, window, cx| {
                    if panel.record.popped_out.is_some() {
                        return;
                    }
                    popout::open(cx.entity().clone(), window, cx);
                })),
        )
        .into_any_element()
}

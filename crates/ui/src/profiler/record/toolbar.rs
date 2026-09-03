//! The Record tab's own toolbar (§1 of `.agents/PROFILER_UI_SPEC.md`):
//! record/stop, clear selection, the `☑ Memory` track toggle, and the
//! pop-out-window button. Chrome's reference toolbar also has an import/
//! export pair, a saved-recordings picker, a `☑ Screenshots` filmstrip
//! toggle, `☐ Dim 3rd parties`, and settings/help icons — all deliberately
//! left out rather than rendered as permanently-inert chrome: there's no
//! capture serialization format, no filmstrip capability, and no "3rd
//! party origin" concept in a UI framework's own profiler. Matches this
//! spec's own build-order note ("stub what's honest, skip the rest") for
//! the overview strip's Network/Timings/Interactions rows.

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

#[derive(Default)]
pub(crate) struct ToolbarState {
    /// Whether the memory step-line chart pane (§3, memory track) renders
    /// below the detail flame chart.
    pub(crate) show_memory: bool,
}

pub(crate) fn render(
    state: &mut ToolbarState,
    is_recording: bool,
    has_capture: bool,
    _window: &mut Window,
    cx: &mut Context<ProfilerPanel>,
) -> AnyElement {
    let show_memory = state.show_memory;
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

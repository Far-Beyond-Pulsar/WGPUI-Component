//! "Open Record in its own window" (toolbar button, §1) — full docked-
//! Inspector-sidebar width is rarely enough to make a scrollable timeline
//! actually legible. Reuses the same `Entity<ProfilerPanel>` rather than a
//! second one, so starting/stopping a capture from either the docked panel
//! or the popped-out window keeps them showing the same data.

use gpui::{
    div, point, px, size, App, AnyElement, AppContext as _, Bounds, Context, Entity,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled,
    Window, WindowBounds, WindowDecorations, WindowHandle, WindowKind, WindowOptions,
};

use crate::{
    button::{Button, ButtonVariants as _},
    title_bar::TitleBar,
    v_flex, ActiveTheme, Root, Sizable as _,
};

use super::super::ProfilerPanel;

/// Root view of the pop-out window. Owns no state of its own — it holds the
/// *same* `Entity<ProfilerPanel>` the docked Inspector uses, and renders
/// [`super::render`] directly rather than the section tab bar, so there's
/// exactly one live copy of the Record tab's GPU surfaces/bounds state at
/// any moment (see `RecordState::popped_out`'s field doc for why that
/// matters).
pub(crate) struct RecordPopout {
    panel: Entity<ProfilerPanel>,
}

impl RecordPopout {
    fn new(panel: Entity<ProfilerPanel>, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { panel }
    }
}

impl Render for RecordPopout {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = self.panel.clone();
        let content = panel.update(cx, |state, cx| super::render_content(state, window, cx));

        let mut title_bar = TitleBar::new();
        let panel_for_close = panel.clone();
        title_bar = title_bar.on_close_window(move |_, _window, cx| {
            panel_for_close.update(cx, |state, cx| {
                state.record.popped_out = None;
                cx.notify();
            });
        });

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(title_bar)
            .child(
                div()
                    .id("record-popout-content")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .child(content),
            )
    }
}

/// Opens (or, if already open, no-ops — the caller should check
/// `popped_out` first) a dedicated top-level window showing just the Record
/// tab.
pub(crate) fn open(panel: Entity<ProfilerPanel>, window: &mut Window, cx: &mut App) {
    let origin = window.bounds().origin;
    // `cx.open_window` performs that window's *first* draw synchronously
    // before returning (see `App::open_window`), which would call
    // `panel.update(..)` -- on this exact same `ProfilerPanel` -- to render
    // `RecordPopout`'s content, while whatever caller invoked `open` (a
    // `cx.listener` on this same entity) may still have it open for update.
    // `Entity::update` refuses to nest like that ("cannot update ... while
    // it is already being updated"). Deferring past the end of the current
    // update (same fix shape as `DockArea::render`'s own `cx.defer`-wrapped
    // bounds write) lets that update finish and release the entity before
    // the popout's first draw asks for a fresh one.
    window.defer(cx, move |window, cx| {
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: origin + point(px(60.), px(60.)),
                size: size(px(1200.), px(760.)),
            })),
            titlebar: None,
            window_min_size: Some(size(px(640.), px(420.))),
            kind: WindowKind::Normal,
            is_resizable: true,
            window_decorations: Some(WindowDecorations::Client),
            ..Default::default()
        };
        let opened = cx.open_window(window_options, {
            let panel = panel.clone();
            move |window, cx| {
                let popout = cx.new(|cx| RecordPopout::new(panel.clone(), window, cx));
                cx.new(|cx| Root::new(popout.into(), window, cx))
            }
        });
        if let Ok(handle) = opened {
            panel.update(cx, |state, cx| {
                state.record.popped_out = Some(handle);
                cx.notify();
            });
        }
    });
}

/// What the docked Inspector shows in place of the Record tab while it's
/// popped out — see `RecordState::popped_out`'s field doc for why the
/// docked copy can't also render live content at the same time.
pub(crate) fn docked_placeholder(
    handle: WindowHandle<Root>,
    cx: &mut Context<ProfilerPanel>,
) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from("Record is open in its own window.")),
        )
        .child(
            Button::new("record-refocus-popout")
                .small()
                .label("Bring it to the front")
                .on_click(move |_, _window, cx| {
                    let _ = cx.update_window(handle.into(), |_, window, _| {
                        window.activate_window();
                    });
                }),
        )
        .into_any_element()
}

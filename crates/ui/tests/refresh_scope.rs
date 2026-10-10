//! Tooltips and context menus repaint the view they belong to, not the whole
//! window: a cached view beside them is not rebuilt.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    div, point, px, size, AnyView, AppContext as _, Context, Entity, InteractiveElement as _,
    IntoElement, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, ParentElement as _, Render,
    StyleRefinement, Styled as _, TestAppContext, VisualTestContext, Window,
};
use ui::menu::context_menu::ContextMenuExt as _;
use ui::menu::PopupMenuItem;
use ui::tooltip::HoverTooltip;

/// Counts its renders.
struct Counted {
    renders: Rc<Cell<usize>>,
    content: fn(&mut Window, &mut Context<Counted>) -> gpui::AnyElement,
}

impl Render for Counted {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        (self.content)(window, cx)
    }
}

struct Label;

impl Render for Label {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child("tip")
    }
}

thread_local! {
    static TOOLTIPS_BUILT: Cell<usize> = const { Cell::new(0) };
    static MENUS_BUILT: Cell<usize> = const { Cell::new(0) };
}

/// A host with two cached views: `subject` at the top left, a bystander below.
struct Host {
    subject: Entity<Counted>,
    bystander: Entity<Counted>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let cached = |entity: &Entity<Counted>| {
            AnyView::from(entity.clone())
                .cached(StyleRefinement::default().w(px(100.)).h(px(40.)))
        };
        div()
            .size_full()
            .child(cached(&self.subject))
            .child(cached(&self.bystander))
    }
}

fn host(
    cx: &mut TestAppContext,
    subject: fn(&mut Window, &mut Context<Counted>) -> gpui::AnyElement,
) -> (VisualTestContext, Rc<Cell<usize>>, Rc<Cell<usize>>) {
    cx.update(ui::init);
    let subject_renders = Rc::new(Cell::new(0));
    let bystander_renders = Rc::new(Cell::new(0));
    let (s, b) = (subject_renders.clone(), bystander_renders.clone());
    let window = cx.update(|cx| {
        cx.open_window(
            gpui::WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(400.), px(300.)),
                ))),
                ..Default::default()
            },
            move |window, cx| {
                let subject = cx.new(|_| Counted {
                    renders: s,
                    content: subject,
                });
                let bystander = cx.new(|_| Counted {
                    renders: b,
                    content: |_, _| div().size_full().into_any_element(),
                });
                let host = cx.new(|_| Host { subject, bystander });
                cx.new(|cx| ui::Root::new(host.into(), window, cx))
            },
        )
        .unwrap()
    });
    cx.run_until_parked();
    let cx = VisualTestContext::from_window(window.into(), cx);
    (cx, subject_renders, bystander_renders)
}

#[gpui::test]
fn a_tooltip_shows_after_its_delay_without_a_frame_loop(cx: &mut TestAppContext) {
    let (mut cx, subject, bystander) = host(cx, |_, _| {
        HoverTooltip::new(
            "trigger",
            div().w(px(100.)).h(px(40.)),
            |_, cx| {
                TOOLTIPS_BUILT.with(|n| n.set(n.get() + 1));
                cx.new(|_| Label).into()
            },
        )
        .into_any_element()
    });
    let away = point(px(300.), px(250.));
    cx.simulate_mouse_move(away, None, Modifiers::none());
    cx.run_until_parked();
    let (subject_before, bystander_before) = (subject.get(), bystander.get());

    cx.simulate_mouse_move(point(px(20.), px(20.)), None, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(TOOLTIPS_BUILT.with(Cell::get), 0, "not before the delay");
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
    assert!(TOOLTIPS_BUILT.with(Cell::get) > 0, "shown once the pointer rested");
    assert!(
        subject.get() - subject_before <= 2,
        "the owner repainted {} times while the tooltip waited",
        subject.get() - subject_before
    );
    assert_eq!(bystander.get(), bystander_before, "a cached bystander replays");
}

#[gpui::test]
fn a_context_menu_opens_and_closes_without_rebuilding_cached_views(cx: &mut TestAppContext) {
    let (mut cx, _subject, bystander) = host(cx, |_, _| {
        div()
            .id("menu-target")
            .w(px(100.))
            .h(px(40.))
            .context_menu(|menu, _, _| {
                MENUS_BUILT.with(|n| n.set(n.get() + 1));
                menu.item(PopupMenuItem::new("Item"))
            })
            .into_any_element()
    });
    let target = point(px(20.), px(20.));
    cx.simulate_mouse_move(target, None, Modifiers::none());
    cx.run_until_parked();
    let bystander_before = bystander.get();

    let right = |position| MouseDownEvent {
        button: MouseButton::Right,
        position,
        modifiers: Modifiers::none(),
        click_count: 1,
        first_mouse: false,
    };
    cx.simulate_event(right(target));
    cx.simulate_event(MouseUpEvent {
        button: MouseButton::Right,
        position: target,
        modifiers: Modifiers::none(),
        click_count: 1,
    });
    cx.run_until_parked();
    assert_eq!(MENUS_BUILT.with(Cell::get), 1, "the menu opened");

    // Click away to dismiss it.
    cx.simulate_click(point(px(300.), px(250.)), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(bystander.get(), bystander_before, "a cached bystander replays");
}

//! Where does an absolutely positioned scrollbar overlay end up?
//!
//! `Scrollbar` is an absolute element filling its parent. If it asks for no
//! insets, layout puts it at its *static position*, which in a block container
//! is below the in-flow siblings before it. A scrollbar laid over a list that
//! way is pushed down by the list's height, away from the list's edge. So
//! `Scrollbar` requests explicit zero insets (see its `request_layout`), and
//! the probes below show the difference deterministically.

use gpui::{
    div, px, App, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    ScrollHandle, StatefulInteractiveElement as _, Styled as _, TestAppContext, Window,
};

/// What `Scrollbar` requests: absolute, 100 % x 100 %, grow / shrink 1.
fn scrollbar_style() -> gpui::Div {
    div().absolute().flex_grow().flex_shrink().w_full().h_full()
}

struct Harness {
    list: ScrollHandle,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(820.)).h(px(700.)).p(px(10.)).child(
            div()
                .relative()
                .debug_selector(|| "container".into())
                .border_1()
                .child(
                    div()
                        .id("list")
                        .max_h(px(300.))
                        .overflow_y_scroll()
                        .track_scroll(&self.list)
                        .child(div().h(px(1200.))),
                )
                // No insets: placed after the list in the flow.
                .child(scrollbar_style().debug_selector(|| "auto-insets".into()))
                // What Scrollbar does now.
                .child(
                    scrollbar_style()
                        .top_0()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .debug_selector(|| "zero-insets".into()),
                ),
        )
    }
}

#[gpui::test]
fn an_overlay_without_insets_lands_below_the_list(cx: &mut TestAppContext) {
    cx.update(ui::init);
    let (_, window) = cx.add_window_view(|_, _| Harness {
        list: ScrollHandle::new(),
    });
    window.update(|window, cx: &mut App| window.draw(cx).clear());
    window.update(|window, cx: &mut App| window.draw(cx).clear());

    let container = window.debug_bounds("container").expect("container");
    let auto = window.debug_bounds("auto-insets").expect("auto");
    let zero = window.debug_bounds("zero-insets").expect("zero");

    // With zero insets the overlay covers the container's padding box.
    assert_eq!(zero.origin, container.origin + gpui::point(px(1.), px(1.)));
    assert_eq!(zero.size.width, container.size.width - px(2.));
    assert_eq!(zero.size.height, container.size.height - px(2.));

    // With none it is pushed down by the in-flow list above it.
    assert_eq!(
        auto.origin.y,
        zero.origin.y + px(300.),
        "static position is after the list"
    );
}

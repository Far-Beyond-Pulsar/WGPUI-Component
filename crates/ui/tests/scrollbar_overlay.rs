//! Where does a `Scrollbar` end up when overlaid the way editor panels do it?
//!
//! The probe element has the layout style `Scrollbar` requests
//! (absolute, 100 % x 100 %, flex-grow / shrink 1), so its bounds are where the
//! scrollbar draws.

use gpui::{
    App, AppContext as _, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, ScrollHandle, StatefulInteractiveElement as _, Styled as _, TestAppContext, Window,
    div, px,
};
use ui::scroll::{Scrollbar, ScrollbarState};

/// `Scrollbar`'s layout style. Explicit zero insets matter: an absolute element
/// with auto insets does not follow the scroll offset of a scrolled ancestor.
fn probe() -> gpui::Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .flex_grow()
        .flex_shrink()
        .w_full()
        .h_full()
}

/// A scroll area with its overlaid scrollbar, as the editor panels build it.
fn scroll_area(
    name: &'static str,
    handle: &ScrollHandle,
    state: &ScrollbarState,
    content_height: f32,
) -> gpui::Div {
    div()
        .relative()
        .debug_selector(move || format!("{name}-container"))
        .child(
            div()
                .id(name)
                .max_h(px(300.))
                .overflow_y_scroll()
                .track_scroll(handle)
                .p_1()
                .child(div().h(px(content_height))),
        )
        // Directly in the container, not in an absolute wrapper: an absolute
        // element nested in another absolute element keeps its unscrolled
        // position when an ancestor scrolls.
        .child(probe().debug_selector(move || format!("{name}-probe")))
}

/// The configurator: a form that scrolls, containing a bordered, rounded box
/// that scrolls too (the platform list).
struct Harness {
    form: ScrollHandle,
    form_state: ScrollbarState,
    list: ScrollHandle,
    list_state: ScrollbarState,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(820.))
            .h(px(700.))
            .flex()
            .flex_col()
            .child(div().h(px(50.)))
            .child(
                div()
                    .id("form")
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .child(
                        div()
                            .id("form-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.form)
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_4()
                                    .p_6()
                                    .child(div().h(px(200.)))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_3()
                                            .p_4()
                                            .rounded_lg()
                                            .border_1()
                                            .child(div().h(px(30.)))
                                            .child(div().h(px(30.)))
                                            .child(div().h(px(30.)))
                                            .child(
                                                scroll_area(
                                                    "plat",
                                                    &self.list,
                                                    &self.list_state,
                                                    1200.,
                                                )
                                                .rounded_md()
                                                .border_1(),
                                            ),
                                    )
                                    .child(div().h(px(900.))),
                            ),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .child(Scrollbar::vertical(&self.form_state, &self.form)),
                    ),
            )
    }
}

#[gpui::test]
fn a_nested_bordered_list_keeps_its_scrollbar_on_its_own_edge(cx: &mut TestAppContext) {
    cx.update(ui::init);
    let (_, window) = cx.add_window_view(|_, _| Harness {
        form: ScrollHandle::new(),
        form_state: ScrollbarState::default(),
        list: ScrollHandle::new(),
        list_state: ScrollbarState::default(),
    });
    window.update(|window, cx: &mut App| window.draw(cx).clear());
    window.update(|window, cx: &mut App| window.draw(cx).clear());

    let container = window.debug_bounds("plat-container").expect("container");
    let probe = window.debug_bounds("plat-probe").expect("probe");
    println!("container {container:?}\nprobe     {probe:?}");

    // The overlay is inside the border: the container less 1px each side.
    assert_eq!(probe.size.width, container.size.width - px(2.));
    assert_eq!(probe.size.height, container.size.height - px(2.));
    assert_eq!(probe.origin, container.origin + gpui::point(px(1.), px(1.)));
}

#[gpui::test]
fn scrolling_the_form_moves_the_list_scrollbar_with_it(cx: &mut TestAppContext) {
    cx.update(ui::init);
    let form = ScrollHandle::new();
    let (_, window) = cx.add_window_view({
        let form = form.clone();
        move |_, _| Harness {
            form,
            form_state: ScrollbarState::default(),
            list: ScrollHandle::new(),
            list_state: ScrollbarState::default(),
        }
    });
    window.update(|window, cx: &mut App| window.draw(cx).clear());
    window.update(|window, cx: &mut App| window.draw(cx).clear());
    let before = window.debug_bounds("plat-container").expect("container");

    form.set_offset(gpui::point(px(0.), px(-300.)));
    window.update(|window, _| window.refresh());
    window.update(|window, cx: &mut App| window.draw(cx).clear());
    window.update(|window, cx: &mut App| window.draw(cx).clear());
    let container = window.debug_bounds("plat-container").expect("container");
    let probe = window.debug_bounds("plat-probe").expect("probe");
    println!("SCROLLED container {container:?}\nSCROLLED probe     {probe:?}");

    assert_eq!(container.origin.y, before.origin.y - px(300.), "the box scrolled");
    assert_eq!(probe.origin.y, container.origin.y + px(1.));
}

/// Same nesting, but the scrollbar-style element sits directly in the
/// container instead of inside an absolute wrapper.
struct DirectHarness {
    form: ScrollHandle,
    list: ScrollHandle,
}

impl Render for DirectHarness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(820.)).h(px(700.)).flex().flex_col().child(
            div()
                .id("form-scroll")
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .track_scroll(&self.form)
                .child(div().h(px(200.)))
                .child(
                    div()
                        .relative()
                        .debug_selector(|| "plat-container".into())
                        .border_1()
                        .child(
                            div()
                                .id("plat")
                                .max_h(px(300.))
                                .overflow_y_scroll()
                                .track_scroll(&self.list)
                                .child(div().h(px(1200.))),
                        )
                        .child(probe().debug_selector(|| "plat-probe".into())),
                )
                .child(div().h(px(900.))),
        )
    }
}

#[gpui::test]
fn a_scrollbar_placed_directly_in_the_container_follows_a_scrolled_ancestor(cx: &mut TestAppContext) {
    cx.update(ui::init);
    let form = ScrollHandle::new();
    let (_, window) = cx.add_window_view({
        let form = form.clone();
        move |_, _| DirectHarness { form, list: ScrollHandle::new() }
    });
    window.update(|window, cx: &mut App| window.draw(cx).clear());
    window.update(|window, cx: &mut App| window.draw(cx).clear());

    form.set_offset(gpui::point(px(0.), px(-300.)));
    window.update(|window, _| window.refresh());
    window.update(|window, cx: &mut App| window.draw(cx).clear());
    window.update(|window, cx: &mut App| window.draw(cx).clear());
    let container = window.debug_bounds("plat-container").expect("container");
    let probe = window.debug_bounds("plat-probe").expect("probe");
    println!("DIRECT container {container:?}\nDIRECT probe     {probe:?}");
    assert_eq!(probe.origin.y, container.origin.y + px(1.), "follows the scroll");
    assert_eq!(probe.size.height, container.size.height - px(2.));
}

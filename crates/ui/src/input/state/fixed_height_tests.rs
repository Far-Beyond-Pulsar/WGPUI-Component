use super::*;
use crate::input::TextInput;
use gpui::{
    div, point, px, AppContext, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, ScrollDelta, ScrollWheelEvent, Styled as _, Window,
};
use std::{cell::Cell, rc::Rc};

struct FixedHeightInputView {
    input: Entity<InputState>,
    parent_scrolls: Rc<Cell<usize>>,
}

impl Render for FixedHeightInputView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let parent_scrolls = self.parent_scrolls.clone();
        div()
            .size_full()
            .on_scroll_wheel(move |_, _, _| {
                parent_scrolls.set(parent_scrolls.get() + 1);
            })
            .child(TextInput::new(&self.input).h(px(60.)))
    }
}

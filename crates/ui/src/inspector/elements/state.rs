use super::*;
use crate::scroll::ScrollbarState;

/// Window-local state. Multiple inspector windows must not share scroll or search.
pub(super) struct ElementsState {
    pub filter: Entity<InputState>,
    pub tree_scroll: gpui::UniformListScrollHandle,
    pub tree_scrollbar: ScrollbarState,
    pub details_scroll: gpui::ListState,
    pub details_scrollbar: ScrollbarState,
    pub interaction_scroll: gpui::UniformListScrollHandle,
    pub interaction_scrollbar: ScrollbarState,
    pub collapsed: [bool; 4],
    pub selected: Option<InspectorElementId>,
    _subscription: Subscription,
}

impl ElementsState {
    pub fn new(
        owner: gpui::WeakEntity<Inspector>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter elements…"));
        let subscription = cx.subscribe(&filter, move |this, input, event, cx| {
            if matches!(event, InputEvent::Change) {
                let query = input.read(cx).value();
                this.tree_scroll
                    .scroll_to_item(0, gpui::ScrollStrategy::Top);
                let _ = owner.update(cx, |inspector, cx| {
                    inspector.set_search_query(query);
                    cx.notify();
                });
            }
        });
        Self {
            filter,
            tree_scroll: gpui::UniformListScrollHandle::new(),
            tree_scrollbar: ScrollbarState::default(),
            details_scroll: gpui::ListState::new(4, gpui::ListAlignment::Top, px(200.)),
            details_scrollbar: ScrollbarState::default(),
            interaction_scroll: gpui::UniformListScrollHandle::new(),
            interaction_scrollbar: ScrollbarState::default(),
            collapsed: [false; 4],
            selected: None,
            _subscription: subscription,
        }
    }
}

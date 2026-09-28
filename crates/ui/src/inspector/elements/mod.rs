//! The Elements workspace: independently scrolling, resizable tree and details.
mod box_model;
mod details;
pub(super) mod properties;
mod sizing;
mod state;
mod tree;

use super::*;
use crate::resizable::{h_resizable, resizable_panel};

pub(super) fn render(
    inspector: &mut Inspector,
    window: &mut Window,
    cx: &mut Context<Inspector>,
) -> AnyElement {
    let owner = cx.entity().downgrade();
    let state = window.use_keyed_state("elements-workspace", cx, |window, cx| {
        state::ElementsState::new(owner, window, cx)
    });
    let tree = tree::render(inspector, &state, cx);
    let details = details::render(inspector, &state, window, cx);
    h_resizable("elements-split")
        .child(
            resizable_panel()
                .size_range(px(160.)..gpui::Pixels::MAX)
                .child(tree),
        )
        .child(
            resizable_panel()
                .size_range(px(220.)..gpui::Pixels::MAX)
                .child(details),
        )
        .into_any_element()
}

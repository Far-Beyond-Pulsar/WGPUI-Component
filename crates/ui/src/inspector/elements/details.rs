use super::*;
use crate::scroll::Scrollbar;
use state::ElementsState;

const SECTIONS: [&str; 4] = ["WHY THIS SIZE", "BOX MODEL", "STYLE", "INTERACTIVITY"];

pub(super) fn render(
    inspector: &mut Inspector,
    state: &Entity<ElementsState>,
    _window: &mut Window,
    cx: &mut Context<Inspector>,
) -> AnyElement {
    let Some(id) = inspector.active_element_id().cloned() else {
        return div()
            .p_4()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("Select an element in the tree, or use Pick to inspect the window.")
            .into_any_element();
    };
    state.update(cx, |state, _| {
        if state.selected.as_ref() != Some(&id) {
            state.selected = Some(id.clone());
            state.details_scroll.reset(SECTIONS.len());
            state
                .interaction_scroll
                .scroll_to_item(0, gpui::ScrollStrategy::Top);
        }
    });
    let info = inspector
        .element_infos()
        .iter()
        .find(|info| info.inspector_id == id);
    let title = info
        .map(|info| {
            if info.display_label.is_empty() {
                info.element_type.clone()
            } else {
                info.display_label.clone()
            }
        })
        .unwrap_or_else(|| "Element".into());
    let dimensions = info
        .map(|info| {
            format!(
                "{:.0}×{:.0}",
                info.bounds.size.width.value(),
                info.bounds.size.height.value()
            )
        })
        .unwrap_or_default();
    let location = format!("{}", id.path.source_location);
    let owner = cx.entity().downgrade();
    let section_state = state.clone();
    let scroll = state.read(cx).details_scroll.clone();
    let scrollbar = state.read(cx).details_scrollbar.clone();
    let list = gpui::list(scroll.clone(), move |index, window, cx| {
        let style = if index == 0 {
            window.with_inspector_state::<DivInspectorState, _>(Some(&id), cx, |state, _| {
                state.as_ref().map(|state| state.base_style.clone())
            })
        } else {
            None
        };
        owner
            .update(cx, |inspector, cx| {
                render_section(
                    index,
                    inspector,
                    &section_state,
                    style.as_deref(),
                    window,
                    cx,
                )
            })
            .unwrap_or_else(|_| div().into_any_element())
    })
    .size_full();
    v_flex()
        .size_full()
        .min_w_0()
        .overflow_hidden()
        .border_l_1()
        .border_color(cx.theme().border)
        .child(
            v_flex()
                .p_3()
                .gap_2()
                .flex_shrink_0()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(dimensions),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .child(
                            Link::new("element-source")
                                .href(format!("file://{location}"))
                                .child(location.clone())
                                .flex_1()
                                .overflow_x_hidden(),
                        )
                        .child(Clipboard::new("copy-element-source").value(location)),
                ),
        )
        .child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .child(list)
                .child(Scrollbar::vertical(&scrollbar, &scroll)),
        )
        .into_any_element()
}

fn render_section(
    index: usize,
    inspector: &mut Inspector,
    state: &Entity<ElementsState>,
    style: Option<&StyleRefinement>,
    window: &mut Window,
    cx: &mut Context<Inspector>,
) -> AnyElement {
    let collapsed = state.read(cx).collapsed[index];
    let toggle_state = state.clone();
    let header = h_flex()
        .id(("details-section", index))
        .h(px(32.))
        .px_3()
        .gap_2()
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .cursor_pointer()
        .hover(|el| el.bg(cx.theme().list_hover))
        .child(if collapsed { "›" } else { "⌄" })
        .child(SECTIONS[index])
        .on_click(cx.listener(move |_, _, _, cx| {
            toggle_state.update(cx, |state, _| {
                state.collapsed[index] = !state.collapsed[index];
                state.details_scroll.splice(index..index + 1, 1);
            });
            cx.notify();
        }));
    v_flex()
        .w_full()
        .min_w_0()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(header)
        .when(!collapsed, |el| {
            let content = match index {
                0 => sizing::render(inspector.active_layout(), style, cx),
                1 => inspector
                    .active_layout()
                    .map(|layout| box_model::render(layout, cx))
                    .unwrap_or_else(|| message("Layout data is not available yet.", cx)),
                2 => v_flex()
                    .gap_2()
                    .children(inspector.render_inspector_states(window, cx))
                    .into_any_element(),
                _ => interactivity(inspector, state.read(cx), cx),
            };
            el.child(div().px_3().pb_3().child(content))
        })
        .into_any_element()
}

fn message(text: &'static str, cx: &App) -> AnyElement {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text)
        .into_any_element()
}

fn interactivity(inspector: &Inspector, state: &ElementsState, cx: &App) -> AnyElement {
    let Some(info) = inspector
        .element_infos()
        .iter()
        .find(|info| Some(&info.inspector_id) == inspector.active_element_id())
    else {
        return message("No interaction data available.", cx);
    };
    // Event collections can be arbitrarily large; keep this list virtual too.
    let rows: Vec<_> = info
        .element_states
        .iter()
        .map(|state| (state.clone(), SharedString::from("Active state")))
        .chain(
            info.event_listeners
                .iter()
                .map(|event| (event.event_type.clone(), event.location.clone())),
        )
        .collect();
    if rows.is_empty() {
        return message("No event listeners or active states.", cx);
    }
    let height = px((rows.len().min(8) * 28) as f32);
    let list = uniform_list("element-interactions", rows.len(), move |range, _, cx| {
        range
            .map(|i| {
                h_flex()
                    .h(px(28.))
                    .gap_2()
                    .text_xs()
                    .child(
                        div()
                            .text_color(cx.theme().primary)
                            .child(rows[i].0.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_color(cx.theme().muted_foreground)
                            .child(rows[i].1.clone()),
                    )
                    .into_any_element()
            })
            .collect()
    })
    .track_scroll(&state.interaction_scroll)
    .size_full();
    div()
        .relative()
        .h(height)
        .w_full()
        .overflow_hidden()
        .child(list)
        .child(Scrollbar::vertical(
            &state.interaction_scrollbar,
            &state.interaction_scroll,
        ))
        .into_any_element()
}

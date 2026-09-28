use super::*;
use crate::{input::Input, scroll::Scrollbar};
use state::ElementsState;

struct Row {
    node_id: Option<InspectorElementId>,
    depth: usize,
    label: SharedString,
    dimensions: SharedString,
    expanded: bool,
    children: bool,
    selected: bool,
    interactive: bool,
}

/// Search retains ancestor paths and traverses collapsed branches so matches
/// remain reachable. Clearing the query restores the user's expansion state.
fn flatten(
    nodes: &[InspectorTreeNode],
    depth: usize,
    is_collapsed: &impl Fn(&InspectorElementId) -> bool,
    selected: Option<&InspectorElementId>,
    query: &str,
    rows: &mut Vec<Row>,
) {
    for node in nodes {
        let start = rows.len();
        let expanded = !query.is_empty()
            || node
                .inspector_id
                .as_ref()
                .is_none_or(|id| !is_collapsed(id));
        let label = if node.display_label.is_empty() {
            node.element_type.clone()
        } else {
            node.display_label.clone()
        };
        let matches = query.is_empty()
            || label.to_lowercase().contains(query)
            || node.element_type.to_lowercase().contains(query)
            || node.source_file.to_lowercase().contains(query);
        rows.push(Row {
            node_id: node.inspector_id.clone(),
            depth,
            label,
            dimensions: format!(
                "{:.0}×{:.0}",
                node.bounds.size.width.value(),
                node.bounds.size.height.value()
            )
            .into(),
            expanded,
            children: !node.children.is_empty(),
            selected: node
                .inspector_id
                .as_ref()
                .is_some_and(|id| selected == Some(id)),
            interactive: !node.event_listeners.is_empty(),
        });
        if expanded {
            flatten(
                &node.children,
                depth + 1,
                is_collapsed,
                selected,
                query,
                rows,
            );
        }
        if !matches && rows.len() == start + 1 {
            rows.pop();
        }
    }
}

pub(super) fn render(
    inspector: &mut Inspector,
    state: &Entity<ElementsState>,
    cx: &mut Context<Inspector>,
) -> AnyElement {
    let mut rows = Vec::new();
    flatten(
        inspector.element_tree(),
        0,
        &|id| inspector.is_collapsed(id),
        inspector.active_element_id(),
        &inspector.search_query().trim().to_lowercase(),
        &mut rows,
    );
    let count = rows.len();
    if state.read(cx).selected.as_ref() != inspector.active_element_id() {
        if let Some(index) = rows.iter().position(|row| row.selected) {
            state
                .read(cx)
                .tree_scroll
                .scroll_to_item(index, gpui::ScrollStrategy::Nearest);
        }
    }
    let widest = rows
        .iter()
        .enumerate()
        .max_by_key(|(_, row)| row.depth * 14 + row.label.len() * 7)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let owner = cx.entity().downgrade();
    let state = state.read(cx);
    let list = uniform_list("elements-tree", count, move |range, _, cx| {
        range
            .map(|index| {
                let row = &rows[index];
                let toggle_owner = owner.clone();
                let select_owner = owner.clone();
                let toggle_id = row.node_id.clone();
                let select_id = row.node_id.clone();
                h_flex()
                    .id(("element", index))
                    .h(px(28.))
                    .pr_2()
                    .pl(px(8. + row.depth as f32 * 14.))
                    .gap_1()
                    .text_sm()
                    .cursor_pointer()
                    .when(row.selected, |el| el.bg(cx.theme().list_active))
                    .hover(|el| el.bg(cx.theme().list_hover))
                    .child(
                        div()
                            .id(("disclosure", index))
                            .w(px(16.))
                            .flex_shrink_0()
                            .text_color(cx.theme().muted_foreground)
                            .child(if row.children {
                                if row.expanded {
                                    "⌄"
                                } else {
                                    "›"
                                }
                            } else {
                                ""
                            })
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                if let Some(id) = &toggle_id {
                                    let _ = toggle_owner.update(cx, |inspector, cx| {
                                        inspector.toggle_collapsed(id.clone());
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(div().text_color(cx.theme().muted_foreground).child("•"))
                    .child(div().flex_1().whitespace_nowrap().child(row.label.clone()))
                    .child(
                        div()
                            .w(px(14.))
                            .text_color(cx.theme().primary)
                            .child(if row.interactive { "↖" } else { "" }),
                    )
                    .child(
                        div()
                            .min_w(px(72.))
                            .text_right()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(row.dimensions.clone()),
                    )
                    .on_click(move |_, window, cx| {
                        if let Some(id) = &select_id {
                            let _ = select_owner.update(cx, |inspector, cx| {
                                inspector.set_active_element_id(id.clone(), window);
                                cx.notify();
                            });
                        }
                    })
                    .into_any_element()
            })
            .collect()
    })
    .with_width_from_item(Some(widest))
    .with_horizontal_sizing_behavior(gpui::ListHorizontalSizingBehavior::Unconstrained)
    .track_scroll(&state.tree_scroll)
    .size_full();
    v_flex()
        .size_full()
        .min_w_0()
        .overflow_hidden()
        .child(
            h_flex()
                .p_2()
                .gap_2()
                .flex_shrink_0()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(Input::new(&state.filter).small().flex_1().min_w_0())
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(count.to_string()),
                ),
        )
        .child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .when(count == 0, |el| {
                    el.child(
                        div()
                            .p_3()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("No matching elements."),
                    )
                })
                .when(count > 0, |el| {
                    el.child(list)
                        .child(Scrollbar::both(&state.tree_scrollbar, &state.tree_scroll))
                }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(label: &str, children: Vec<InspectorTreeNode>) -> InspectorTreeNode {
        InspectorTreeNode {
            inspector_id: Some(InspectorElementId {
                path: Rc::new(gpui::InspectorElementPath {
                    global_id: Default::default(),
                    source_location: std::panic::Location::caller(),
                }),
                instance_id: 0,
            }),
            element_type: "div".into(),
            display_label: label.to_owned().into(),
            bounds: Default::default(),
            depth: 0,
            source_file: "example.rs".into(),
            source_line: 1,
            children,
            is_selected: false,
            is_hovered: false,
            event_listeners: Vec::new(),
        }
    }

    #[test]
    fn search_reveals_collapsed_matches_with_ancestors() {
        let nodes = vec![node(
            "root",
            vec![node("match", vec![]), node("unrelated", vec![])],
        )];
        let mut rows = Vec::new();
        flatten(&nodes, 0, &|_| true, None, "match", &mut rows);
        assert_eq!(
            rows.iter()
                .map(|row| row.label.as_ref())
                .collect::<Vec<_>>(),
            ["root", "match"]
        );
        assert_eq!(rows[1].depth, 1);
        rows.clear();
        flatten(&nodes, 0, &|_| true, None, "", &mut rows);
        assert_eq!(rows.len(), 1, "clearing search restores collapsed branches");
    }

    #[test]
    fn unmatched_search_is_empty_and_source_search_works() {
        let nodes = vec![node("root", vec![node("child", vec![])])];
        let mut rows = Vec::new();
        flatten(&nodes, 0, &|_| false, None, "absent", &mut rows);
        assert!(rows.is_empty());
        flatten(&nodes, 0, &|_| true, None, "example.rs", &mut rows);
        assert_eq!(rows.len(), 2);
    }
}

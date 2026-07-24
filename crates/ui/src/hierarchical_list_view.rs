//! Generic Hierarchical Tree View
//!
//! A fully-featured hierarchy component that replicates HierarchyPanel's functionality.
//! Can be used for scene hierarchies, component hierarchies, and any other tree structure.
//!
//! ## Features
//! - N-depth nesting with expand/collapse
//! - Drag-and-drop with modifier keys (regular=nest, Alt=reorder, Shift=un-nest)
//! - Optional disable_nesting mode (only allow reordering, not nesting - for flat lists)
//! - Optional accent color for items (for type indicators, etc.)
//! - Optional root drop zone
//! - Optional custom header with buttons
//! - Optional panel layout (full-page) vs widget layout (compact)
//! - Custom item rendering with icons, colors, and extra content

use gpui::{prelude::*, *};
use std::rc::Rc;
use std::sync::Arc;
use crate::{
    button::{Button, ButtonVariants as _},
    draggable::{DragHandlePosition, Draggable},
    drop_area::DropArea,
    h_flex,
    input::{InputState, TextInput},
    menu::{context_menu::ContextMenu, popup_menu::PopupMenu},
    scroll::{Scrollbar, ScrollbarState},
    v_flex, v_virtual_list, ActiveTheme, Icon, IconName, Sizable, StyledExt, VirtualListScrollHandle,
};

/// Trait for items that can be displayed in a hierarchical tree
pub trait HierarchyItem: Clone + 'static {
    type Id: Clone + PartialEq + std::fmt::Display + 'static;
    type DragPayload: Clone + Render + 'static;

    fn id(&self) -> Self::Id;
    fn name(&self) -> String;
    fn icon(&self) -> IconName;
    fn icon_color<V>(&self, cx: &Context<V>) -> Hsla
    where
        V: Render;
    fn children_ids(&self) -> Vec<Self::Id>;
    fn is_selected(&self) -> bool;
    fn create_drag_payload(&self) -> Self::DragPayload;
    fn drag_drop_id(&self) -> String;

    /// Optional accent color for the item (e.g., type color for variables)
    fn accent_color<V>(&self, _cx: &Context<V>) -> Option<Hsla>
    where
        V: Render,
    {
        None
    }

    /// Optional extra content to render at the end of the row (e.g., visibility toggle)
    fn extra_row_content<V>(&self, _cx: &mut Context<V>) -> Option<AnyElement>
    where
        V: Render,
    {
        None
    }

    /// Custom click behavior (return true if handled, false for default select behavior)
    fn on_click_custom(&self) -> Option<Arc<dyn Fn()>> {
        None
    }

    /// Optional right-click context menu for the whole row.
    fn build_context_menu(
        &self,
        menu: PopupMenu,
        _window: &mut Window,
        _cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        menu
    }

    /// If true, the item's name is replaced by an inline TextInput for renaming.
    fn is_renaming(&self) -> bool {
        false
    }

    /// The TextInput entity to show when `is_renaming()` returns true.
    fn rename_input(&self) -> Option<Entity<InputState>> {
        None
    }
}

/// Layout mode for the hierarchy view
#[derive(Clone)]
pub enum HierarchyLayout {
    /// Full panel with header and scrollable content
    Panel,
    /// Compact widget (like component hierarchy in properties panel)
    Widget,
}

/// Configuration for hierarchical tree view
pub struct HierarchyConfig<Item: HierarchyItem> {
    pub items: Vec<Item>,
    pub root_ids: Vec<Item::Id>,
    pub layout: HierarchyLayout,

    // Header config (for Panel layout)
    pub title: Option<String>,
    pub header_buttons: Vec<AnyElement>,

    // Root drop zone (optional)
    pub root_drop_zone: Option<(String, Arc<dyn Fn(Item::DragPayload)>)>, // (label, on_drop)

    // Widget config (for Widget layout)
    pub widget_title: Option<String>,
    pub widget_icon: Option<IconName>,
    pub widget_add_button: Option<AnyElement>,
    pub empty_message: String,

    // Drag-and-drop options
    pub disable_nesting: bool, // If true, only allow reordering (Alt+Drag behavior), not nesting

    // Callbacks
    pub is_expanded: Arc<dyn Fn(&Item::Id) -> bool>,
    pub on_toggle_expand: Arc<dyn Fn(&Item::Id, &mut Window, &mut App)>,
    pub on_select: Arc<dyn Fn(&Item::Id, &mut Window, &mut App)>,
    pub on_drop: Arc<dyn Fn(Item::DragPayload, &Item::Id, &Modifiers, &mut Window, &mut App)>,
}

/// A flattened entry in the tree for virtual list rendering
struct FlatTreeEntry {
    item_index: usize,
    depth: usize,
}

/// Generic hierarchical tree view component
pub struct HierarchicalTreeView<Item: HierarchyItem> {
    config: HierarchyConfig<Item>,
}

impl<Item: HierarchyItem> HierarchicalTreeView<Item> {
    pub fn new(config: HierarchyConfig<Item>) -> Self {
        Self { config }
    }

    fn get_root_item_indices(&self) -> Vec<usize> {
        self.config
            .items
            .iter()
            .enumerate()
            .filter_map(|(idx, item)| {
                if self.config.root_ids.contains(&item.id()) {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect()
    }

    fn find_item(&self, id: &Item::Id) -> Option<&Item> {
        self.config.items.iter().find(|item| item.id() == *id)
    }

    fn find_item_index(&self, id: &Item::Id) -> Option<usize> {
        self.config.items.iter().position(|item| item.id() == *id)
    }

    fn flatten_tree(&self) -> Vec<FlatTreeEntry> {
        let mut entries = Vec::new();
        for idx in self.get_root_item_indices() {
            self.flatten_node(idx, 0, &mut entries);
        }
        entries
    }

    fn flatten_node(&self, item_index: usize, depth: usize, entries: &mut Vec<FlatTreeEntry>) {
        entries.push(FlatTreeEntry { item_index, depth });
        if let Some(item) = self.config.items.get(item_index) {
            let is_expanded = (self.config.is_expanded)(&item.id());
            if is_expanded {
                for child_id in item.children_ids() {
                    if let Some(child_idx) = self.find_item_index(&child_id) {
                        self.flatten_node(child_idx, depth + 1, entries);
                    }
                }
            }
        }
    }

    pub fn render<V>(mut self, cx: &mut Context<V>) -> AnyElement
    where
        V: 'static + Render,
    {
        let layout = self.config.layout.clone();

        match layout {
            HierarchyLayout::Panel => self.render_panel_layout(cx).into_any_element(),
            HierarchyLayout::Widget => self.render_widget_layout(cx).into_any_element(),
        }
    }

    fn render_panel_layout<V>(mut self, cx: &mut Context<V>) -> impl IntoElement
    where
        V: 'static + Render,
    {
        let flat_entries = Rc::new(self.flatten_tree());
        let items = Arc::new(self.config.items.clone());
        let item_sizes = Rc::new(vec![size(px(0.0), px(28.0)); flat_entries.len()]);

        let is_expanded = self.config.is_expanded.clone();
        let on_toggle_expand = self.config.on_toggle_expand.clone();
        let on_select = self.config.on_select.clone();
        let on_drop = self.config.on_drop.clone();

        let header_buttons = std::mem::take(&mut self.config.header_buttons);
        let item_count = items.len();
        let view = cx.entity().clone();
        let root_drop_zone = self.config.root_drop_zone.clone();

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .w_full()
                    .px_4()
                    .py(px(6.0))
                    .justify_between()
                    .items_center()
                    .bg(cx.theme().sidebar)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .when(self.config.title.is_some(), |this| {
                                this.child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(cx.theme().foreground)
                                        .child(self.config.title.clone().unwrap()),
                                )
                            })
                            .when(self.config.title.is_some(), |this| {
                                this.child(
                                    div()
                                        .px_2()
                                        .py(px(2.0))
                                        .rounded(px(4.0))
                                        .bg(cx.theme().muted.opacity(0.5))
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("{} objects", item_count)),
                                )
                            }),
                    )
                    .child(h_flex().gap_1().children(header_buttons)),
            )
            .child(
                div()
                    .id("hierarchy-panel-scroll")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        v_flex()
                            .id("hierarchy-panel-content")
                            .size_full()
                            .p_2()
                            .when_some(root_drop_zone, |this, (label, on_drop_arc)| {
                                this.child(
                                    DropArea::<Item::DragPayload>::new("hierarchy-root-drop")
                                        .on_drop({
                                            move |payload, _, _| {
                                                (on_drop_arc)(payload.clone());
                                            }
                                        })
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .items_center()
                                                .gap_1()
                                                .h_7()
                                                .pl(px(8.0))
                                                .pr_2()
                                                .rounded(px(4.0))
                                                .border_1()
                                                .border_color(cx.theme().border)
                                                .bg(cx.theme().muted.opacity(0.18))
                                                .child(
                                                    Icon::new(IconName::Folder)
                                                        .size(px(14.0))
                                                        .text_color(cx.theme().muted_foreground),
                                                )
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .font_weight(FontWeight::MEDIUM)
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child(label.clone()),
                                                ),
                                        ),
                                )
                            })
                            .child(
                                v_virtual_list(
                                    view,
                                    "hierarchy-panel-list",
                                    item_sizes,
                                    move |_v, range, _window, cx| {
                                        range
                                            .map(|flat_idx| {
                                                let entry = &flat_entries[flat_idx];
                                                let item = &items[entry.item_index];
                                                render_single_item(
                                                    item,
                                                    entry.depth,
                                                    is_expanded.clone(),
                                                    on_toggle_expand.clone(),
                                                    on_select.clone(),
                                                    on_drop.clone(),
                                                    cx,
                                                )
                                                .into_any_element()
                                            })
                                            .collect()
                                    },
                                )
                                .gap(px(1.0)),
                            ),
                    ),
            )
    }

    fn render_widget_layout<V>(mut self, cx: &mut Context<V>) -> impl IntoElement
    where
        V: 'static + Render,
    {
        let flat_entries = Rc::new(self.flatten_tree());
        let items = Arc::new(self.config.items.clone());
        let item_sizes = Rc::new(vec![size(px(0.0), px(28.0)); flat_entries.len()]);

        let is_expanded = self.config.is_expanded.clone();
        let on_toggle_expand = self.config.on_toggle_expand.clone();
        let on_select = self.config.on_select.clone();
        let on_drop = self.config.on_drop.clone();

        let empty_message = self.config.empty_message.clone();
        let widget_icon = std::mem::take(&mut self.config.widget_icon);
        let widget_title = std::mem::take(&mut self.config.widget_title);
        let widget_add_button = std::mem::take(&mut self.config.widget_add_button);
        let is_empty = flat_entries.is_empty();
        let view = cx.entity().clone();
        let root_drop_zone = self.config.root_drop_zone.clone();
        let item_count = items.len();

        v_flex()
            .w_full()
            .bg(cx.theme().sidebar)
            .rounded(px(8.0))
            .border_1()
            .border_color(cx.theme().border)
            .overflow_hidden()
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py(px(6.0))
                    .justify_between()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .when(widget_icon.is_some(), |this| {
                                this.child(
                                    Icon::new(widget_icon.unwrap())
                                        .small()
                                        .text_color(cx.theme().muted_foreground),
                                )
                            })
                            .when(widget_title.is_some(), |this| {
                                this.child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(cx.theme().foreground)
                                        .child(widget_title.as_ref().unwrap().clone()),
                                )
                            })
                            .child(
                                div()
                                    .px_2()
                                    .py(px(2.0))
                                    .rounded(px(4.0))
                                    .bg(cx.theme().muted.opacity(0.5))
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{}", item_count)),
                            ),
                    )
                    .children(widget_add_button),
            )
            .child(
                v_flex()
                    .w_full()
                    .min_h(px(40.0))
                    .max_h(px(300.0))
                    .p_1()
                    .when_some(root_drop_zone, |this, (label, on_drop_arc)| {
                        this.child(
                            DropArea::<Item::DragPayload>::new("hierarchy-widget-root-drop")
                                .on_drop({
                                    move |payload, _, _| {
                                        (on_drop_arc)(payload.clone());
                                    }
                                })
                                .child(
                                    h_flex()
                                        .w_full()
                                        .items_center()
                                        .gap_1()
                                        .h_7()
                                        .pl(px(8.0))
                                        .pr_2()
                                        .rounded(px(4.0))
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .bg(cx.theme().muted.opacity(0.18))
                                        .child(
                                            Icon::new(IconName::Folder)
                                                .size(px(14.0))
                                                .text_color(cx.theme().muted_foreground),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(cx.theme().muted_foreground)
                                                .child(label.clone()),
                                        ),
                                ),
                        )
                    })
                    .when(is_empty, |el| {
                        el.child(
                            div()
                                .px_2()
                                .py_1()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(empty_message.clone()),
                        )
                    })
                    .when(!is_empty, |el| {
                        el.child(
                            v_flex()
                                .flex_1()
                                .min_h_0()
                                .child(
                                    v_virtual_list(
                                        view,
                                        "hierarchy-widget-list",
                                        item_sizes,
                                        move |_v, range, _window, cx| {
                                            range
                                                .map(|flat_idx| {
                                                    let entry = &flat_entries[flat_idx];
                                                    let item = &items[entry.item_index];
                                                    render_single_item(
                                                        item,
                                                        entry.depth,
                                                        is_expanded.clone(),
                                                        on_toggle_expand.clone(),
                                                        on_select.clone(),
                                                        on_drop.clone(),
                                                        cx,
                                                    )
                                                    .into_any_element()
                                                })
                                                .collect()
                                        },
                                    )
                                    .gap(px(1.0)),
                                ),
                        )
                    }),
            )
    }
}

fn render_single_item<Item: HierarchyItem, V: 'static + Render>(
    item: &Item,
    depth: usize,
    is_expanded: Arc<dyn Fn(&Item::Id) -> bool>,
    on_toggle_expand: Arc<dyn Fn(&Item::Id, &mut Window, &mut App)>,
    on_select: Arc<dyn Fn(&Item::Id, &mut Window, &mut App)>,
    on_drop: Arc<dyn Fn(Item::DragPayload, &Item::Id, &Modifiers, &mut Window, &mut App)>,
    cx: &mut Context<V>,
) -> impl IntoElement {
    let is_selected = item.is_selected();
    let indent = px(depth as f32 * 20.0 + 4.0);
    let item_name = item.name();
    let item_id = item.id();
    let icon = item.icon();
    let icon_color = item.icon_color(cx);
    let accent_color = item.accent_color(cx);

    let text_color = if is_selected {
        cx.theme().accent_foreground
    } else {
        cx.theme().foreground
    };

    let muted_color = if is_selected {
        cx.theme().accent_foreground.opacity(0.7)
    } else {
        cx.theme().muted_foreground
    };

    let children_ids = item.children_ids();
    let has_children = !children_ids.is_empty();
    let expanded = (is_expanded)(&item_id);

    let expand_arrow: AnyElement = if has_children {
        let on_toggle = on_toggle_expand.clone();
        let id_for_toggle = item_id.clone();
        div()
            .w_4()
            .h_4()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(2.0))
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().muted.opacity(0.5)))
            .child(
                Icon::new(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size(px(12.0))
                .text_color(muted_color),
            )
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                (on_toggle)(&id_for_toggle, window, cx);
            })
            .into_any_element()
    } else {
        div().w_4().into_any_element()
    };

    let id_for_select = item_id.clone();
    let custom_click = item.on_click_custom();
    let item_for_context_menu = item.clone();

    let row_content = h_flex()
        .id(SharedString::from(format!("item-{}", item_id)))
        .w_full()
        .items_center()
        .gap_1()
        .h_7()
        .pl(indent)
        .pr_2()
        .rounded(px(4.0))
        .cursor_pointer()
        .when(is_selected, |s| s.bg(cx.theme().accent).shadow_sm())
        .when(!is_selected, |s| {
            s.hover(|style| style.bg(cx.theme().muted.opacity(0.3)))
        })
        .on_click(cx.listener(move |_view, _, window, cx| {
            if let Some(ref custom) = custom_click {
                (custom)();
            } else {
                (on_select)(&id_for_select, window, cx);
            }
            cx.notify();
        }))
        .child(expand_arrow)
        .when(accent_color.is_some(), |el| {
            let accent = accent_color.unwrap();
            el.child(
                div()
                    .flex_shrink_0()
                    .w(px(10.0))
                    .h(px(10.0))
                    .rounded_full()
                    .bg(accent)
                    .border_1()
                    .border_color(cx.theme().border.opacity(0.5)),
            )
        })
        .when(accent_color.is_none(), |el| {
            el.child(
                div()
                    .w_5()
                    .h_5()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.0))
                    .bg(icon_color.opacity(0.15))
                    .child(Icon::new(icon).size(px(14.0)).text_color(if is_selected {
                        text_color
                    } else {
                        icon_color
                    })),
            )
        })
        .child({
            let is_renaming = item.is_renaming();
            if is_renaming {
                if let Some(input_entity) = item.rename_input() {
                    div()
                        .flex_1()
                        .h_7()
                        .child(TextInput::new(&input_entity).w_full().xsmall())
                        .into_any_element()
                } else {
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(text_color)
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(item_name.clone())
                        .into_any_element()
                }
            } else {
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(text_color)
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(item_name.clone())
                    .into_any_element()
            }
        })
        .children(item.extra_row_content(cx))
        .child(
            ContextMenu::new(format!("tree-row-context-menu-{}", item.drag_drop_id())).menu(
                move |menu, window, cx| item_for_context_menu.build_context_menu(menu, window, cx),
            ),
        );

    let drag_payload = item.create_drag_payload();
    let drag_id = item.drag_drop_id();

    let draggable_row = Draggable::new(format!("tree-drag-{}", drag_id), drag_payload)
        .drag_handle(DragHandlePosition::Left)
        .w_full()
        .child(row_content);

    let drop_target_id = item_id.clone();
    DropArea::<Item::DragPayload>::new(format!("tree-drop-{}", drag_id))
        .on_drop(move |payload, window, cx| {
            let modifiers = window.modifiers();
            (on_drop)(payload.clone(), &drop_target_id, &modifiers, window, cx);
        })
        .w_full()
        .child(draggable_row)
}

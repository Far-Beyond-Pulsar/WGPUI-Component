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

use crate::{
    button::{Button, ButtonVariants as _},
    draggable::{DragHandlePosition, Draggable},
    drop_area::DropArea,
    h_flex,
    input::{InputState, TextInput},
    menu::{context_menu::ContextMenu, popup_menu::PopupMenu},
    scroll::{Scrollbar, ScrollbarState},
    v_flex, v_virtual_list, ActiveTheme, Icon, IconName, Sizable, StyledExt,
    VirtualListScrollHandle,
};
use gpui::{prelude::*, *};
use std::rc::Rc;
use std::sync::Arc;

/// Keeps the first occurrence of each hierarchy ID.
pub fn deduplicate_hierarchy_ids<Id: PartialEq>(ids: impl IntoIterator<Item = Id>) -> Vec<Id> {
    let mut unique = Vec::new();
    for id in ids {
        if !unique.contains(&id) {
            unique.push(id);
        }
    }
    unique
}

/// Trait for items that can be displayed in a hierarchical tree
pub trait HierarchyItem: Clone + 'static {
    type Id: Clone + Eq + std::hash::Hash + std::fmt::Display + 'static;
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
    /// Shared, so a caller that keeps its rows between frames hands them over
    /// without copying them (an `Arc<[T]>` from a `Vec` is `.into()`).
    pub items: Arc<[Item]>,
    pub root_ids: Arc<[Item::Id]>,
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

impl<Item: HierarchyItem> HierarchyConfig<Item> {
    /// A content key for [`HierarchicalTreeView::with_list_state`] read off
    /// the tree itself: a hash of the roots, each item's id and children, and
    /// which items are expanded. It walks every item, so it suits a small
    /// tree; a large one should keep a generation of its own instead.
    pub fn content_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.root_ids.hash(&mut hasher);
        for item in self.items.iter() {
            let id = item.id();
            id.hash(&mut hasher);
            item.children_ids().hash(&mut hasher);
            (self.is_expanded)(&id).hash(&mut hasher);
        }
        hasher.finish()
    }
}

/// A flattened entry in the tree for virtual list rendering
struct FlatTreeEntry {
    item_index: usize,
    depth: usize,
}

/// The rows of a panel-layout tree as a persistent `gpui::list`, the list the
/// properties inspector uses. The host keeps one across renders and passes it
/// with [`HierarchicalTreeView::with_list_state`], so the scroll position
/// survives every render, only the rows on screen are built, and a panel
/// resize keeps the measured row heights.
#[derive(Clone)]
pub struct HierarchyListState<Id> {
    list: ListState,
    scrollbar: ScrollbarState,
    /// The ids of the rows last shown, in order, to tell the list which range
    /// changed (so expanding a node keeps the rows above it where they are).
    shown: Rc<std::cell::RefCell<Vec<(Id, usize)>>>,
    /// The flattened rows and the host's content key they were built for
    /// (see [`HierarchicalTreeView::with_list_state`]).
    flat: Rc<std::cell::RefCell<Option<(u64, Rc<Vec<FlatTreeEntry>>)>>>,
}

impl<Id: Clone + PartialEq> HierarchyListState<Id> {
    pub fn new() -> Self {
        Self {
            // The overdraw keeps rows just outside the viewport measured, so
            // a scroll does not pop rows in.
            list: ListState::new(0, ListAlignment::Top, px(200.0)),
            scrollbar: ScrollbarState::default(),
            shown: Rc::default(),
            flat: Rc::default(),
        }
    }

    /// Bring the list in step with the rows to show: splice only the range
    /// that differs from the rows shown last time.
    fn sync(&self, rows: Vec<(Id, usize)>) {
        let mut shown = self.shown.borrow_mut();
        let prefix = shown.iter().zip(&rows).take_while(|(a, b)| a == b).count();
        let suffix = shown[prefix..]
            .iter()
            .rev()
            .zip(rows[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        if prefix != shown.len() || prefix != rows.len() {
            self.list.splice(
                prefix..shown.len() - suffix,
                rows.len() - prefix - suffix,
            );
        }
        *shown = rows;
    }

    /// Scroll the list by `distance` (positive scrolls down).
    pub fn scroll_by(&self, distance: Pixels) {
        self.list.scroll_by(distance);
    }

    /// The row at the top of the viewport.
    pub fn first_visible_row(&self) -> usize {
        self.list.logical_scroll_top().item_ix
    }

    /// The number of rows the list holds.
    pub fn row_count(&self) -> usize {
        self.list.item_count()
    }
}

impl<Id: Clone + PartialEq> Default for HierarchyListState<Id> {
    fn default() -> Self {
        Self::new()
    }
}

/// Generic hierarchical tree view component
pub struct HierarchicalTreeView<Item: HierarchyItem> {
    config: HierarchyConfig<Item>,
    scroll_handle: VirtualListScrollHandle,
    scrollbar_state: ScrollbarState,
    list_state: Option<HierarchyListState<Item::Id>>,
    content_key: u64,
}

impl<Item: HierarchyItem> HierarchicalTreeView<Item> {
    pub fn new(config: HierarchyConfig<Item>) -> Self {
        Self {
            config,
            scroll_handle: VirtualListScrollHandle::new(),
            scrollbar_state: ScrollbarState::default(),
            list_state: None,
            content_key: 0,
        }
    }

    /// Show the panel layout's rows in `state`'s persistent list (see
    /// [`HierarchyListState`]). Without it the rows go in a virtual list
    /// rebuilt each render, whose scroll position does not survive one.
    ///
    /// `content_key` names what the rows are built from: the items, their
    /// order and children, and which nodes are expanded. While it stays the
    /// same, the flattened rows of the last render are reused and the tree is
    /// not walked again, so a render that only changes the panel's size (each
    /// frame of a resize) costs no work proportional to the tree. The host
    /// must change it whenever any of those change.
    pub fn with_list_state(mut self, state: HierarchyListState<Item::Id>, content_key: u64) -> Self {
        self.list_state = Some(state);
        self.content_key = content_key;
        self
    }

    /// The rows shown, in order: every root item (in item order), each
    /// followed by its expanded descendants.
    ///
    /// Runs on every render of the view (a panel resize re-renders it each
    /// frame), so it is O(items + shown rows): one id → index map, one root
    /// set, and a set per expanded node to drop repeated children. It used to
    /// scan the item list per root and per child, which was O(n²).
    fn flatten_tree(&self) -> Vec<FlatTreeEntry> {
        use std::collections::{HashMap, HashSet};

        let items = &self.config.items;
        let mut index: HashMap<Item::Id, usize> = HashMap::with_capacity(items.len());
        let ids: Vec<Item::Id> = items.iter().map(|item| item.id()).collect();
        for (ix, id) in ids.iter().enumerate() {
            // The first item with an id is the one a child id refers to.
            index.entry(id.clone()).or_insert(ix);
        }
        let roots: HashSet<&Item::Id> = self.config.root_ids.iter().collect();

        let mut entries = Vec::new();
        for (ix, id) in ids.iter().enumerate() {
            if roots.contains(id) {
                self.flatten_node(ix, 0, &ids, &index, &mut entries);
            }
        }
        entries
    }

    fn flatten_node(
        &self,
        item_index: usize,
        depth: usize,
        ids: &[Item::Id],
        index: &std::collections::HashMap<Item::Id, usize>,
        entries: &mut Vec<FlatTreeEntry>,
    ) {
        entries.push(FlatTreeEntry { item_index, depth });
        let Some(item) = self.config.items.get(item_index) else {
            return;
        };
        if !(self.config.is_expanded)(&ids[item_index]) {
            return;
        }
        let mut seen = std::collections::HashSet::new();
        for child_id in item.children_ids() {
            if !seen.insert(child_id.clone()) {
                continue;
            }
            if let Some(&child_idx) = index.get(&child_id) {
                self.flatten_node(child_idx, depth + 1, ids, index, entries);
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
        // The rows of the last render, while the host says nothing they are
        // built from changed.
        let content_key = self.content_key;
        let reused = self.list_state.as_ref().and_then(|state| {
            let flat = state.flat.borrow();
            let (key, rows) = flat.as_ref()?;
            (*key == content_key).then(|| rows.clone())
        });
        let fresh = reused.is_none();
        let flat_entries = reused.unwrap_or_else(|| {
            let _scope = gpui::render_stats::scope("hierarchy tree: flatten");
            Rc::new(self.flatten_tree())
        });
        let items = self.config.items.clone();

        let is_expanded = self.config.is_expanded.clone();
        let on_toggle_expand = self.config.on_toggle_expand.clone();
        let on_select = self.config.on_select.clone();
        let on_drop = self.config.on_drop.clone();

        let header_buttons = std::mem::take(&mut self.config.header_buttons);
        let item_count = items.len();
        let view = cx.entity().clone();
        let root_drop_zone = self.config.root_drop_zone.clone();
        let scroll_handle = self.scroll_handle.clone();
        let scrollbar_state = self.scrollbar_state.clone();

        // The rows, and the scrollbar that follows them.
        let (rows, scrollbar): (AnyElement, AnyElement) = match self.list_state.take() {
            Some(state) => {
                if fresh {
                    let _scope = gpui::render_stats::scope("hierarchy tree: list sync");
                    state.sync(
                        flat_entries
                            .iter()
                            .map(|entry| (items[entry.item_index].id(), entry.depth))
                            .collect(),
                    );
                    *state.flat.borrow_mut() = Some((content_key, flat_entries.clone()));
                }
                let flat_entries = flat_entries.clone();
                let items = items.clone();
                let is_expanded = is_expanded.clone();
                let on_toggle_expand = on_toggle_expand.clone();
                let on_select = on_select.clone();
                let on_drop = on_drop.clone();
                let rows = list(
                    state.list.clone(),
                    cx.processor(move |_view: &mut V, ix: usize, _window, cx| {
                        let Some(entry) = flat_entries.get(ix) else {
                            return div().into_any_element();
                        };
                        // The 1px gap the virtual list put between rows.
                        div()
                            .w_full()
                            .pb(px(1.0))
                            .child(render_single_item(
                                &items[entry.item_index],
                                entry.depth,
                                is_expanded.clone(),
                                on_toggle_expand.clone(),
                                on_select.clone(),
                                on_drop.clone(),
                                cx,
                            ))
                            .into_any_element()
                    }),
                )
                .flex_1()
                .min_h_0()
                .w_full()
                .into_any_element();
                let scrollbar = Scrollbar::vertical(&state.scrollbar, &state.list).into_any_element();
                (rows, scrollbar)
            }
            None => {
                let flat_entries = flat_entries.clone();
                let items = items.clone();
                let is_expanded = is_expanded.clone();
                let on_toggle_expand = on_toggle_expand.clone();
                let on_select = on_select.clone();
                let on_drop = on_drop.clone();
                let item_sizes = Rc::new(vec![size(px(0.0), px(28.0)); flat_entries.len()]);
                let rows = v_virtual_list(
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
                .gap(px(1.0))
                .track_scroll(&scroll_handle)
                .into_any_element();
                let scrollbar = Scrollbar::vertical(&scrollbar_state, &scroll_handle).into_any_element();
                (rows, scrollbar)
            }
        };

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
                    .overflow_hidden()
                    .child(
                        v_flex()
                            .id("hierarchy-panel-content")
                            .size_full()
                            .p_2()
                            .when_some(root_drop_zone, |this, (label, on_drop_arc)| {
                                this.child(
                                    DropArea::<Item::DragPayload>::new("hierarchy-root-drop")
                                        // DropArea defaults to `size_full`, which would
                                        // claim 100% of this definite-height column and
                                        // squeeze the virtual list to half the panel once
                                        // flex shrink runs. Keep it content-sized.
                                        .h_auto()
                                        .flex_shrink_0()
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
                            .child(rows),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .child(scrollbar),
                    ),
            )
    }

    fn render_widget_layout<V>(mut self, cx: &mut Context<V>) -> impl IntoElement
    where
        V: 'static + Render,
    {
        let flat_entries = Rc::new(self.flatten_tree());
        // Include the root drop target and padding, then reserve one row for
        // every currently visible item. Taller trees scroll inside the cap.
        let list_height = (36.0 + 28.0 * flat_entries.len() as f32).clamp(40.0, 300.0);
        let items = self.config.items.clone();
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
        let scroll_handle = self.scroll_handle.clone();
        let scrollbar_state = self.scrollbar_state.clone();

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
                    .h(px(list_height))
                    .p_1()
                    .when_some(root_drop_zone, |this, (label, on_drop_arc)| {
                        this.child(
                            DropArea::<Item::DragPayload>::new("hierarchy-widget-root-drop")
                                .h_auto()
                                .flex_shrink_0()
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
                            div()
                                .relative()
                                .flex_1()
                                .min_h_0()
                                .overflow_hidden()
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
                                    .gap(px(1.0))
                                    .track_scroll(&scroll_handle),
                                )
                                .child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .left_0()
                                        .right_0()
                                        .bottom_0()
                                        .child(Scrollbar::vertical(
                                            &scrollbar_state,
                                            &scroll_handle,
                                        )),
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

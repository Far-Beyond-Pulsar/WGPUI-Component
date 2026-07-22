use std::rc::Rc;
use std::sync::Arc;

use crate::button::{Button, ButtonVariants as _};
use crate::popup_menu::PopupMenuExt as _;
use crate::{h_flex, ActiveTheme, IconName, Selectable, Sizable, Size, StyledExt};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    div, Action, AnyElement, App, Corner, Div, Edges, ElementId, IntoElement, ParentElement,
    Pixels, RenderOnce, SharedString, Stateful, StatefulInteractiveElement as _, StyleRefinement,
    Styled, Window,
};
use gpui::{h_list, px, HListScrollHandle, InteractiveElement};
use smallvec::SmallVec;

use super::{Tab, TabVariant};

#[derive(Action, Debug, Clone, Copy, PartialEq, Eq)]
#[action(namespace = tab_bar, no_json)]
pub struct SelectTab(usize);

#[derive(IntoElement)]
pub struct TabBar {
    base: Stateful<Div>,
    style: StyleRefinement,
    scroll_handle: Option<HListScrollHandle>,
    prefix: Option<AnyElement>,
    suffix: Option<AnyElement>,
    item_count: usize,
    build_tab: Option<Box<dyn Fn(usize, &mut Window, &mut App) -> Tab>>,
    last_empty_space: AnyElement,
    selected_index: Option<usize>,
    variant: TabVariant,
    size: Size,
    menu: bool,
    on_click: Option<Arc<dyn Fn(&usize, &mut Window, &mut App) + 'static>>,
    tab_item_top_offset: Pixels,
    item_labels: Vec<(Option<SharedString>, bool)>,
}

impl TabBar {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            base: div().id(id).px(px(-1.)),
            style: StyleRefinement::default(),
            children: SmallVec::new(),
            scroll_handle: None,
            prefix: None,
            suffix: None,
            item_count: 0,
            build_tab: None,
            variant: TabVariant::default(),
            size: Size::default(),
            last_empty_space: div().w_3().into_any_element(),
            selected_index: None,
            on_click: None,
            menu: false,
            tab_item_top_offset: px(0.),
            item_labels: Vec::new(),
        }
    }

    pub fn with_variant(mut self, variant: TabVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn pill(mut self) -> Self {
        self.variant = TabVariant::Pill;
        self
    }

    pub fn outline(mut self) -> Self {
        self.variant = TabVariant::Outline;
        self
    }

    pub fn segmented(mut self) -> Self {
        self.variant = TabVariant::Segmented;
        self
    }

    pub fn underline(mut self) -> Self {
        self.variant = TabVariant::Underline;
        self
    }

    pub fn with_menu(mut self, menu: bool) -> Self {
        self.menu = menu;
        self
    }

    pub fn track_scroll(mut self, scroll_handle: &HListScrollHandle) -> Self {
        self.scroll_handle = Some(scroll_handle.clone());
        self
    }

    pub fn prefix(mut self, prefix: impl IntoElement) -> Self {
        self.prefix = Some(prefix.into_any_element());
        self
    }

    pub fn suffix(mut self, suffix: impl IntoElement) -> Self {
        self.suffix = Some(suffix.into_any_element());
        self
    }

    pub fn children(mut self, children: impl IntoIterator<Item = impl Into<Tab>>) -> Self {
        let children: Vec<Tab> = children.into_iter().map(Into::into).collect();
        self.item_labels = children
            .iter()
            .map(|t| (t.label.clone(), t.disabled))
            .collect();
        self.item_count = children.len();
        self.build_tab = Some(Box::new(move |ix, _window, _cx| children[ix].clone()));
        self
    }

    pub fn child(mut self, child: impl Into<Tab>) -> Self {
        let tab: Tab = child.into();
        self.item_labels.push((tab.label.clone(), tab.disabled));
        self.item_count += 1;
        self
    }

    pub fn selected_index(mut self, index: usize) -> Self {
        self.selected_index = Some(index);
        self
    }

    pub fn last_empty_space(mut self, last_empty_space: impl IntoElement) -> Self {
        self.last_empty_space = last_empty_space.into_any_element();
        self
    }

    pub fn on_click(mut self, on_click: impl Fn(&usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Arc::new(on_click));
        self
    }

    pub(crate) fn tab_item_top_offset(mut self, offset: impl Into<Pixels>) -> Self {
        self.tab_item_top_offset = offset.into();
        self
    }
}

impl Styled for TabBar {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl Sizable for TabBar {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl RenderOnce for TabBar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let (bg, paddings) = match self.variant {
            TabVariant::Tab => (cx.theme().tab_bar, Edges::all(px(0.))),
            TabVariant::Outline => (cx.theme().transparent, Edges::all(px(0.))),
            TabVariant::Pill => (cx.theme().transparent, Edges::all(px(0.))),
            TabVariant::Segmented => {
                let padding_x = match self.size {
                    Size::XSmall => px(2.),
                    Size::Small => px(2.),
                    Size::Large => px(6.),
                    _ => px(5.),
                };
                (
                    cx.theme().tab_bar_segmented,
                    Edges {
                        left: padding_x,
                        right: padding_x,
                        ..Default::default()
                    },
                )
            }
            TabVariant::Underline => (cx.theme().transparent, Edges::all(px(0.))),
        };

        let item_count = self.item_count;
        let build_tab = self.build_tab;
        let variant = self.variant;
        let size = self.size;
        let on_click = self.on_click.clone();
        let selected_index = self.selected_index;
        let tab_item_top_offset = self.tab_item_top_offset;
        let item_labels = self.item_labels.clone();
        let scroll_handle = self.scroll_handle.clone();

        self.base
            .group("tab-bar")
            .on_action({
                let on_click = on_click.clone();
                move |action: &SelectTab, window: &mut Window, cx: &mut App| {
                    if let Some(ref on_click) = on_click {
                        on_click(&action.0, window, cx);
                    }
                }
            })
            .relative()
            .flex()
            .items_center()
            .bg(bg)
            .text_color(cx.theme().tab_foreground)
            .when(
                self.variant == TabVariant::Underline || self.variant == TabVariant::Tab,
                |this| {
                    this.child(
                        div()
                            .id("border-b")
                            .absolute()
                            .left_0()
                            .bottom_0()
                            .size_full()
                            .border_b_1()
                            .border_color(cx.theme().border),
                    )
                },
            )
            .when(
                self.variant == TabVariant::Pill || self.variant == TabVariant::Segmented,
                |this| this.rounded(cx.theme().radius),
            )
            .paddings(paddings)
            .refine_style(&self.style)
            .when_some(self.prefix, |this, prefix| this.child(prefix))
            .child(
                div().flex_1().overflow_hidden().child(
                    h_list("tab-list", item_count, move |range, window, cx| {
                        let Some(ref build_tab) = build_tab else {
                            return vec![];
                        };
                        range
                            .map(|ix| {
                                let mut tab = build_tab(ix, window, cx);
                                tab = tab
                                    .id(ix)
                                    .mt(tab_item_top_offset)
                                    .with_variant(variant)
                                    .with_size(size)
                                    .when_some(selected_index, |this, sel_ix| {
                                        this.selected(sel_ix == ix)
                                    })
                                    .when_some(on_click.clone(), move |this, cb| {
                                        this.on_click(move |_, window, cx| cb(&ix, window, cx))
                                    });
                                tab.into_any_element()
                            })
                            .collect::<Vec<_>>()
                    })
                    .when_some(scroll_handle, |this, handle| {
                        this.track_scroll(&handle)
                    }),
                ),
            )
            .when(self.suffix.is_some() || self.menu, |this| {
                this.child(self.last_empty_space)
            })
            .when(self.menu, |this| {
                this.child(
                    Button::new("more")
                        .xsmall()
                        .ghost()
                        .icon(IconName::ChevronDown)
                        .popup_menu(move |mut this, _, _| {
                            this = this.scrollable();
                            for (ix, (label, disabled)) in item_labels.iter().enumerate() {
                                this = this.menu_with_check_and_disabled(
                                    label.clone().unwrap_or_default(),
                                    selected_index == Some(ix),
                                    Box::new(SelectTab(ix)),
                                    *disabled,
                                );
                            }
                            this
                        })
                        .anchor(Corner::TopRight),
                )
            })
            .when_some(self.suffix, |this, suffix| this.child(suffix))
    }
}

use crate::{
    actions::{Cancel, SelectLeft, SelectRight},
    button::{Button, ButtonVariant, ButtonVariants},
    h_flex,
    popup_menu::PopupMenu,
    Selectable, Sizable,
};
use std::sync::Arc;

use gpui::{
    anchored, deferred, div, img, prelude::FluentBuilder, px, App, AppContext as _, ClickEvent,
    Context, DismissEvent, ImageSource, MouseButton, ObjectFit, Pixels, StyledImage as _, Entity, Focusable, Global, InteractiveElement as _, IntoElement, KeyBinding,
    OwnedMenu, ParentElement, Render, RenderImage, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window,
};

/// Global cache for app menus — used as fallback on platforms where
/// `cx.get_menus()` returns `None` (e.g. Windows cross-platform backend).
pub struct AppMenusCache(pub Vec<OwnedMenu>);
impl Global for AppMenusCache {}

const CONTEXT: &str = "AppMenuBar";
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", Cancel, Some(CONTEXT)),
        KeyBinding::new("left", SelectLeft, Some(CONTEXT)),
        KeyBinding::new("right", SelectRight, Some(CONTEXT)),
    ]);
}

/// The application menu bar, for Windows and Linux.
pub struct AppMenuBar {
    menus: Vec<Entity<AppMenu>>,
    selected_ix: Option<usize>,
    /// The first (app) menu is rendered elsewhere, as a logo; skip it here.
    app_menu_hosted: bool,
}

impl AppMenuBar {
    /// Create a new app menu bar.
    ///
    /// Reads menus from `cx.get_menus()` and falls back to [`AppMenusCache`]
    /// for platforms (e.g. Windows) where the platform backend discards them.
    pub fn new(window: &mut Window, cx: &mut App) -> Entity<Self> {
        let owned_menus = cx
            .get_menus()
            .or_else(|| cx.try_global::<AppMenusCache>().map(|c| c.0.clone()))
            .unwrap_or_default();
        Self::new_with_menus(owned_menus, window, cx)
    }

    /// Create a new app menu bar from a pre-built list of [`OwnedMenu`]s.
    pub fn new_with_menus(
        menus: Vec<OwnedMenu>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let menu_bar = cx.entity();
            let menus = menus
                .iter()
                .enumerate()
                .map(|(ix, menu)| AppMenu::new(ix, menu, menu_bar.clone(), window, cx))
                .collect();

            Self {
                selected_ix: None,
                menus,
                app_menu_hosted: false,
            }
        })
    }

    /// Show the first (app) menu as `logo` and stop rendering it in this bar.
    /// The caller places [`Self::app_menu_view`] wherever the logo should sit;
    /// it opens the same menu as before.
    pub fn set_logo(&mut self, logo: Option<Arc<RenderImage>>, cx: &mut Context<Self>) {
        self.app_menu_hosted = logo.is_some();
        if let Some(first) = self.menus.first() {
            first.update(cx, |menu, cx| {
                menu.logo = logo;
                cx.notify();
            });
        }
        cx.notify();
    }

    /// The first menu as a standalone view, for hosting outside the bar.
    pub fn app_menu_view(&self) -> Option<gpui::AnyView> {
        self.menus.first().map(|menu| menu.clone().into())
    }

    fn move_left(&mut self, _: &SelectLeft, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selected_ix) = self.selected_ix else {
            return;
        };

        let new_ix = if selected_ix == 0 {
            self.menus.len().saturating_sub(1)
        } else {
            selected_ix.saturating_sub(1)
        };
        self.set_selected_ix(Some(new_ix), window, cx);
    }

    fn move_right(&mut self, _: &SelectRight, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selected_ix) = self.selected_ix else {
            return;
        };

        let new_ix = if selected_ix + 1 >= self.menus.len() {
            0
        } else {
            selected_ix + 1
        };
        self.set_selected_ix(Some(new_ix), window, cx);
    }

    fn cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        self.set_selected_ix(None, window, cx);
    }

    fn set_selected_ix(&mut self, ix: Option<usize>, _: &mut Window, cx: &mut Context<Self>) {
        self.selected_ix = ix;
        cx.notify();
    }

    #[inline]
    fn has_activated_menu(&self) -> bool {
        self.selected_ix.is_some()
    }
}

impl Render for AppMenuBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .id("app-menu-bar")
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::move_left))
            .on_action(cx.listener(Self::move_right))
            .on_action(cx.listener(Self::cancel))
            .size_full()
            .gap_x_1()
            .overflow_x_scroll()
            .children(
                self.menus
                    .iter()
                    .skip(usize::from(self.app_menu_hosted))
                    .cloned(),
            )
    }
}

/// A menu in the menu bar.
pub struct AppMenu {
    logo: Option<Arc<RenderImage>>,
    menu_bar: Entity<AppMenuBar>,
    ix: usize,
    name: SharedString,
    menu: OwnedMenu,
    popup_menu: Option<Entity<PopupMenu>>,

    _subscription: Option<Subscription>,
}

impl AppMenu {
    pub(super) fn new(
        ix: usize,
        menu: &OwnedMenu,
        menu_bar: Entity<AppMenuBar>,
        _: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let name = menu.name.clone();
        cx.new(|_| Self {
            logo: None,
            ix,
            menu_bar,
            name,
            menu: menu.clone(),
            popup_menu: None,
            _subscription: None,
        })
    }

    fn build_popup_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<PopupMenu> {
        let popup_menu = match self.popup_menu.as_ref() {
            None => {
                let items = self.menu.items.clone();
                let focused = window.focused(cx);
                let popup_menu = PopupMenu::build(window, cx, |menu, window, cx| {
                    menu.when_some(window.focused(cx), |this, handle| {
                        this.action_context(handle)
                    })
                    .with_menu_items(items, window, cx)
                });
                popup_menu.read(cx).focus_handle(cx).focus(window, cx);
                self._subscription =
                    Some(cx.subscribe_in(&popup_menu, window, Self::handle_dismiss));
                self.popup_menu = Some(popup_menu.clone());

                popup_menu
            }
            Some(menu) => menu.clone(),
        };

        let focus_handle = popup_menu.read(cx).focus_handle(cx);
        if !focus_handle.contains_focused(window, cx) {
            focus_handle.focus(window, cx);
        }

        popup_menu
    }

    fn handle_dismiss(
        &mut self,
        _: &Entity<PopupMenu>,
        _: &DismissEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self._subscription.take();
        self.popup_menu.take();
        self.menu_bar.update(cx, |state, cx| {
            state.cancel(&Cancel, window, cx);
        });
    }

    fn handle_trigger_click(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Stop propagation to prevent titlebar drag
        cx.stop_propagation();

        let is_selected = self.menu_bar.read(cx).selected_ix == Some(self.ix);

        _ = self.menu_bar.update(cx, |state, cx| {
            let new_ix = if is_selected { None } else { Some(self.ix) };
            state.set_selected_ix(new_ix, window, cx);
        });
    }

    fn handle_hover(&mut self, hovered: &bool, window: &mut Window, cx: &mut Context<Self>) {
        if !*hovered {
            return;
        }

        let has_activated_menu = self.menu_bar.read(cx).has_activated_menu();
        if !has_activated_menu {
            return;
        }

        _ = self.menu_bar.update(cx, |state, cx| {
            state.set_selected_ix(Some(self.ix), window, cx);
        });
    }
}

/// Side of the logo image drawn as the app menu trigger.
const LOGO_SIZE: Pixels = px(44.);
/// Margin between the logo and the edge of its hover/selected highlight.
const LOGO_INSET: Pixels = px(4.);

impl AppMenu {
    /// The logo as the app menu trigger. A [`Button`] keeps its fixed
    /// small-button height whatever its child, so the 44 px logo overflowed a
    /// 24 px-tall highlight and only that strip was clickable. This trigger is
    /// a square that wraps the logo, so the highlight and hitbox match it.
    fn render_logo_trigger(
        &self,
        logo: Arc<RenderImage>,
        is_selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ghost = ButtonVariant::Ghost;
        let selected_bg = ghost.selected(false, cx).bg;
        let hover_bg = ghost.hovered(false, cx).bg;
        let active_bg = ghost.active(false, cx).bg;
        // The logo is a rounded square; round the highlight to follow it.
        let radius = LOGO_SIZE * 0.25 + LOGO_INSET;

        div()
            .id("menu")
            .debug_selector(|| "app-menu-logo".into())
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(LOGO_SIZE + LOGO_INSET * 2.)
            .rounded(radius)
            .cursor_default()
            .map(|this| {
                if is_selected {
                    this.bg(selected_bg)
                } else {
                    this.hover(|this| this.bg(hover_bg))
                        .active(|this| this.bg(active_bg))
                }
            })
            .on_mouse_down(MouseButton::Left, |_, window, _| {
                // Like Button: don't move focus on mouse down.
                window.prevent_default();
            })
            .on_click(cx.listener(Self::handle_trigger_click))
            .child(
                img(ImageSource::Render(logo))
                    .size(LOGO_SIZE)
                    .object_fit(ObjectFit::Contain),
            )
    }
}

impl Render for AppMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let menu_bar = self.menu_bar.read(cx);
        let is_selected = menu_bar.selected_ix == Some(self.ix);

        let trigger = match self.logo.clone() {
            Some(logo) => self.render_logo_trigger(logo, is_selected, cx).into_any_element(),
            None => Button::new("menu")
                .small()
                .py_0p5()
                .compact()
                .ghost()
                .label(self.name.clone())
                .selected(is_selected)
                .on_click(cx.listener(Self::handle_trigger_click))
                .into_any_element(),
        };

        div()
            .id(self.ix)
            .relative()
            .child(trigger)
            .on_hover(cx.listener(Self::handle_hover))
            .when(is_selected, |this| {
                this.child(deferred(
                    anchored()
                        .anchor(gpui::Corner::TopLeft)
                        .snap_to_window_with_margin(px(8.))
                        .child(
                            div()
                                .debug_selector(|| "app-menu-popup".into())
                                .size_full()
                                .occlude()
                                .top_1()
                                .child(self.build_popup_menu(window, cx)),
                        ),
                ))
            })
    }
}

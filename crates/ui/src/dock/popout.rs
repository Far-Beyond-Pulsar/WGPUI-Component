//! Pop-out windows: what a tab dragged out of a dock area becomes.
//!
//! A pop-out window has no operating-system decorations (it is created with
//! client-side decorations), so something has to draw its title bar, border
//! and window controls. That something is a [`PopoutDecorations`]: it is
//! handed the window's content and the [`PopoutContext`] of the pop-out, and
//! returns the element the window shows. Install one for the whole app with
//! [`set_popout_decorations`]; without one, [`DefaultPopoutDecorations`]
//! draws the standard title bar and border.
//!
//! ```ignore
//! ui::dock::set_popout_decorations(
//!     |content: AnyElement, popout: &PopoutContext, window: &mut Window, cx: &mut App| {
//!         v_flex()
//!             .size_full()
//!             .child(MyChrome::new(popout.title(window, cx)).on_close({
//!                 let popout = popout.clone();
//!                 move |window, cx| popout.close(window, cx)
//!             }))
//!             .child(content)
//!             .into_any_element()
//!     },
//!     cx,
//! );
//! ```
//!
//! Decorations are looked up every time a pop-out window renders, so they can
//! be replaced (a theme or settings change) and apply to windows that are
//! already open.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    div, px, size, AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity,
    EventEmitter, Global, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render,
    Styled as _, WeakEntity,
    Window, WindowBounds, WindowDecorations, WindowKind, WindowOptions,
};

use super::tab_panel::DockChannel;
use super::{DockArea, DockEvent, DockItem, DockWindowRegistry, PanelSource, PanelView, TabPanel};
use crate::{v_flex, window_border, ActiveTheme as _, TitleBar};

/// Starting size of a pop-out window.
const POPOUT_SIZE: gpui::Size<Pixels> = size(px(800.), px(600.));
/// Smallest size a pop-out window can be resized to.
const POPOUT_MIN_SIZE: gpui::Size<Pixels> = size(px(400.), px(300.));

// ---------------------------------------------------------------------------
// Decorations
// ---------------------------------------------------------------------------

/// What a decoration may know about, and do to, the pop-out it wraps.
#[derive(Clone)]
pub struct PopoutContext {
    /// The panel a single-panel pop-out was opened for.
    primary: Option<Arc<dyn PanelView>>,
    dock: WeakEntity<DockArea>,
    source: Option<PanelSource>,
}

impl PopoutContext {
    /// The panel the window was opened for. `None` for a pop-out opened with
    /// a whole tree ([`PopoutContent::Tree`]).
    pub fn panel(&self) -> Option<&Arc<dyn PanelView>> {
        self.primary.as_ref()
    }

    /// The window's title: the opened-for panel's, as the dock's tab shows
    /// it; for a tree, the active panel of its first tab group.
    pub fn title(&self, window: &Window, cx: &App) -> AnyElement {
        if let Some(panel) = &self.primary {
            return panel.title(window, cx);
        }
        self.dock
            .upgrade()
            .and_then(|dock| {
                dock.read(cx)
                    .tab_panels(cx)
                    .into_iter()
                    .find_map(|(_, tab_panel)| tab_panel.read(cx).active_panel(cx))
            })
            .map(|panel| panel.title(window, cx))
            .unwrap_or_else(|| "Pop-out".into_any_element())
    }

    /// Whether closing the window puts the panel back where it came from.
    pub fn restores_on_close(&self) -> bool {
        self.source.is_some()
    }

    /// Close the window. A panel that was dragged out of a tab panel goes
    /// back to its place there, if that tab panel still exists; a tree
    /// pop-out returns all of its panels.
    pub fn close(&self, window: &mut Window, cx: &mut App) {
        if let Some(source) = &self.source {
            match &self.primary {
                Some(panel) => restore_panel(source, panel, cx),
                None => {
                    let panels: Vec<Arc<dyn PanelView>> = self
                        .dock
                        .upgrade()
                        .map(|dock| {
                            dock.read(cx)
                                .tab_panels(cx)
                                .into_iter()
                                .flat_map(|(_, tab_panel)| tab_panel.read(cx).panels.clone())
                                .collect()
                        })
                        .unwrap_or_default();
                    // Appended in order; one index cannot place several tabs.
                    let source = PanelSource {
                        tab_index: None,
                        ..source.clone()
                    };
                    for panel in &panels {
                        restore_panel(&source, panel, cx);
                    }
                }
            }
        }
        window.remove_window();
    }
}

fn restore_panel(source: &PanelSource, panel: &Arc<dyn PanelView>, cx: &mut App) {
    let panel = panel.clone();
    let tab_index = source.tab_index;
    let tab_panel = source.tab_panel.clone();
    let _ = cx.update_window(source.parent_window, move |_root, window, cx| {
        let Some(tab_panel) = tab_panel.upgrade() else {
            tracing::debug!("pop-out source tab panel is gone; the panel is not restored");
            return;
        };
        tab_panel.update(cx, |tab_panel, cx| match tab_index {
            Some(index) => tab_panel.insert_panel_at(panel, index, window, cx),
            None => tab_panel.add_panel(panel, window, cx),
        });
    });
}

/// Draws the chrome of pop-out windows: title bar, border, window controls.
///
/// `content` is the window's dock area; the returned element is what the
/// window shows, so it must contain `content` and fill the window.
pub trait PopoutDecorations: 'static {
    fn decorate(
        &self,
        content: AnyElement,
        popout: &PopoutContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement;
}

impl<F> PopoutDecorations for F
where
    F: Fn(AnyElement, &PopoutContext, &mut Window, &mut App) -> AnyElement + 'static,
{
    fn decorate(
        &self,
        content: AnyElement,
        popout: &PopoutContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self(content, popout, window, cx)
    }
}

struct InstalledDecorations(Rc<dyn PopoutDecorations>);

impl Global for InstalledDecorations {}

/// Use `decorations` for every pop-out window, open or future.
pub fn set_popout_decorations(decorations: impl PopoutDecorations, cx: &mut App) {
    cx.set_global(InstalledDecorations(Rc::new(decorations)));
}

/// Go back to [`DefaultPopoutDecorations`].
pub fn reset_popout_decorations(cx: &mut App) {
    if cx.has_global::<InstalledDecorations>() {
        cx.remove_global::<InstalledDecorations>();
    }
}

/// The decorations pop-out windows have unless the app installs its own: the
/// standard [`TitleBar`] showing the panel's title, inside a [`window_border`].
pub struct DefaultPopoutDecorations;

impl PopoutDecorations for DefaultPopoutDecorations {
    fn decorate(
        &self,
        content: AnyElement,
        popout: &PopoutContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let background = cx.theme().background;
        let popout_for_close = popout.clone();
        window_border()
            .child(
                v_flex()
                    .size_full()
                    .bg(background)
                    .child(
                        TitleBar::new()
                            .child(popout.title(window, cx))
                            .on_close_window(move |_, window, cx| {
                                popout_for_close.close(window, cx)
                            }),
                    )
                    .child(div().flex_1().overflow_hidden().child(content)),
            )
            .into_any_element()
    }
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

/// The root view of a pop-out window: a dock area wrapped in whatever
/// decorations are installed.
struct PopoutWindow {
    dock: Entity<DockArea>,
    context: PopoutContext,
}

impl Render for PopoutWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = div().size_full().child(self.dock.clone()).into_any_element();
        match cx.try_global::<InstalledDecorations>().map(|d| d.0.clone()) {
            Some(decorations) => decorations.decorate(content, &self.context, window, cx),
            None => DefaultPopoutDecorations.decorate(content, &self.context, window, cx),
        }
    }
}

// ---------------------------------------------------------------------------
// Registry and events
// ---------------------------------------------------------------------------

/// A live pop-out window; see [`live_popouts`].
#[derive(Clone)]
pub struct PopoutInfo {
    pub window: AnyWindowHandle,
    /// The pop-out's own dock area.
    pub dock_area: WeakEntity<DockArea>,
    /// The window the pop-out was opened from, if known.
    pub parent: Option<AnyWindowHandle>,
}

/// What happened to a pop-out window. Events are emitted from inside window
/// callbacks, so a subscriber that needs to update that window should defer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PopoutEvent {
    /// A pop-out window was opened (it is already in [`live_popouts`]).
    Opened { window: AnyWindowHandle },
    /// A pop-out window was removed (it is no longer in [`live_popouts`]).
    Closed { window: AnyWindowHandle },
    /// The pop-out's layout changed (its dock's `LayoutChanged`) or its
    /// window moved or was resized.
    Changed { window: AnyWindowHandle },
}

/// Emits [`PopoutEvent`]s; get it with [`popout_events`] and `cx.subscribe` it.
pub struct PopoutEvents;

impl EventEmitter<PopoutEvent> for PopoutEvents {}

struct PopoutEventsGlobal(Entity<PopoutEvents>);

impl Global for PopoutEventsGlobal {}

/// The app-wide pop-out event source.
///
/// ```ignore
/// cx.subscribe(&ui::dock::popout_events(cx), |_, event: &PopoutEvent, cx| { .. }).detach();
/// ```
pub fn popout_events(cx: &mut App) -> Entity<PopoutEvents> {
    if let Some(global) = cx.try_global::<PopoutEventsGlobal>() {
        return global.0.clone();
    }
    let entity = cx.new(|_| PopoutEvents);
    cx.set_global(PopoutEventsGlobal(entity.clone()));
    entity
}

fn emit_popout_event(event: PopoutEvent, cx: &mut App) {
    popout_events(cx).update(cx, |_, cx| cx.emit(event));
}

struct PopoutRecord {
    window: AnyWindowHandle,
    parent: Option<AnyWindowHandle>,
}

/// Which dock windows are pop-outs. The dock areas themselves are found in the
/// [`DockWindowRegistry`](super::DockWindowRegistry).
#[derive(Default)]
struct PopoutRegistry(Vec<PopoutRecord>);

impl Global for PopoutRegistry {}

/// Every pop-out window that is currently open.
pub fn live_popouts(cx: &App) -> Vec<PopoutInfo> {
    let (Some(popouts), Some(docks)) = (
        cx.try_global::<PopoutRegistry>(),
        cx.try_global::<DockWindowRegistry>(),
    ) else {
        return Vec::new();
    };
    popouts
        .0
        .iter()
        .filter_map(|record| {
            let entry = docks
                .windows
                .iter()
                .find(|entry| entry.window_handle == record.window)?;
            entry.dock_area.upgrade()?;
            Some(PopoutInfo {
                window: record.window,
                dock_area: entry.dock_area.clone(),
                parent: record.parent,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Opening
// ---------------------------------------------------------------------------

/// Builds a pop-out's dock tree once its window and dock area exist. Items
/// must be created against the `WeakEntity<DockArea>` it is given.
pub type PopoutBuilder =
    Box<dyn FnOnce(&mut Window, &WeakEntity<DockArea>, &mut App) -> DockItem>;

/// What a pop-out window shows.
pub enum PopoutContent {
    /// One panel in a single tab panel (what dragging a tab out makes).
    Panel(Arc<dyn PanelView>),
    /// An arbitrary tree, built inside the new window.
    Tree(PopoutBuilder),
}

/// Everything [`open_popout_with`] needs.
pub struct PopoutSpec {
    pub content: PopoutContent,
    pub bounds: WindowBounds,
    /// Where the panel(s) go when the window is closed with its close button.
    pub source: Option<PanelSource>,
    /// The dock channel of the window's tabs.
    pub channel: DockChannel,
    /// The window it is opened from; defaults to `source`'s parent window.
    pub parent: Option<AnyWindowHandle>,
}

impl PopoutSpec {
    /// A single-panel pop-out.
    pub fn panel(
        panel: Arc<dyn PanelView>,
        bounds: WindowBounds,
        channel: DockChannel,
        source: Option<PanelSource>,
    ) -> Self {
        Self {
            content: PopoutContent::Panel(panel),
            bounds,
            source,
            channel,
            parent: None,
        }
    }

    /// A pop-out showing the tree `build` produces.
    pub fn tree(
        build: impl FnOnce(&mut Window, &WeakEntity<DockArea>, &mut App) -> DockItem + 'static,
        bounds: WindowBounds,
        channel: DockChannel,
        source: Option<PanelSource>,
    ) -> Self {
        Self {
            content: PopoutContent::Tree(Box::new(build)),
            bounds,
            source,
            channel,
            parent: None,
        }
    }
}

/// Open a pop-out window as `spec` describes. Returns the new window's handle.
pub fn open_popout_with(spec: PopoutSpec, cx: &mut App) -> Option<AnyWindowHandle> {
    let PopoutSpec {
        content,
        bounds,
        source,
        channel,
        parent,
    } = spec;
    let parent = parent.or(source.as_ref().map(|source| source.parent_window));

    let options = WindowOptions {
        window_bounds: Some(bounds),
        // The decorations draw the title bar; there is no system one.
        titlebar: None,
        window_min_size: Some(POPOUT_MIN_SIZE),
        kind: WindowKind::Normal,
        is_resizable: true,
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };

    let handle: AnyWindowHandle = cx
        .open_window(options, move |window, cx| {
            let window_handle = window.window_handle();
            let dock = cx.new(|cx| {
                DockArea::new_with_channel("detached-dock", Some(1), channel, window, cx)
            });
            let weak = dock.downgrade();

            let primary = match content {
                PopoutContent::Panel(panel) => {
                    let tab_panel = cx.new(|cx| {
                        let mut tab_panel = TabPanel::new(None, weak.clone(), channel, window, cx);
                        tab_panel.closable = true;
                        tab_panel
                    });
                    tab_panel.update(cx, |tab_panel, cx| {
                        tab_panel.add_panel(panel.clone(), window, cx)
                    });
                    dock.update(cx, |dock, cx| {
                        dock.set_center(
                            DockItem::Tabs {
                                view: tab_panel.clone(),
                                active_ix: 0,
                                items: vec![panel.clone()],
                            },
                            window,
                            cx,
                        );
                    });
                    Some(panel)
                }
                PopoutContent::Tree(build) => {
                    let item = build(window, &weak, cx);
                    dock.update(cx, |dock, cx| dock.set_center(item, window, cx));
                    let groups = dock.read(cx).tab_panels(cx);
                    for (_, tab_panel) in groups {
                        tab_panel.update(cx, |tab_panel, _| tab_panel.closable = true);
                    }
                    None
                }
            };

            let popout = cx.new(|cx| {
                let context = PopoutContext {
                    primary,
                    dock: weak.clone(),
                    source,
                };

                cx.subscribe(&dock, move |_, _, event: &DockEvent, cx| {
                    if matches!(event, DockEvent::LayoutChanged) {
                        emit_popout_event(PopoutEvent::Changed { window: window_handle }, cx);
                    }
                })
                .detach();
                cx.observe_window_bounds(window, move |_, _, cx| {
                    emit_popout_event(PopoutEvent::Changed { window: window_handle }, cx);
                })
                .detach();
                // The window's root view is released when the window is removed.
                cx.on_release(move |_, cx| {
                    if cx.has_global::<PopoutRegistry>() {
                        cx.global_mut::<PopoutRegistry>()
                            .0
                            .retain(|record| record.window != window_handle);
                    }
                    emit_popout_event(PopoutEvent::Closed { window: window_handle }, cx);
                })
                .detach();

                PopoutWindow { dock, context }
            });
            cx.new(|cx| crate::Root::new(popout.into(), window, cx))
        })
        .ok()?
        .into();

    if !cx.has_global::<PopoutRegistry>() {
        cx.set_global(PopoutRegistry::default());
    }
    cx.global_mut::<PopoutRegistry>().0.push(PopoutRecord {
        window: handle,
        parent,
    });
    emit_popout_event(PopoutEvent::Opened { window: handle }, cx);
    Some(handle)
}

/// Open a pop-out window showing `panel`, with `origin` as the window's
/// top-left corner in logical screen coordinates.
///
/// `source` says where the panel came from, so closing the window can put it
/// back; `channel` keeps the window's tabs on the dock channel they were
/// dragged from. Returns the new window's handle.
pub(crate) fn open_popout_window(
    panel: Arc<dyn PanelView>,
    origin: gpui::Point<Pixels>,
    channel: DockChannel,
    source: Option<PanelSource>,
    cx: &mut App,
) -> Option<AnyWindowHandle> {
    open_popout_with(
        PopoutSpec::panel(
            panel,
            WindowBounds::Windowed(Bounds::new(origin, POPOUT_SIZE)),
            channel,
            source,
        ),
        cx,
    )
}

/// Where a pop-out window's top-left corner goes so that its title bar sits
/// under the cursor that just dragged the tab out, at `cursor` in logical
/// screen coordinates.
pub(crate) fn origin_under_cursor(cursor: gpui::Point<Pixels>) -> gpui::Point<Pixels> {
    gpui::point(cursor.x - px(120.0), cursor.y - px(18.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decorations_are_installed_replaced_and_reset_app_wide() {
        let test_context = gpui::TestAppContext::single();
        test_context.update(|cx| {
            assert!(!cx.has_global::<InstalledDecorations>());

            set_popout_decorations(
                |content: AnyElement, _: &PopoutContext, _: &mut Window, _: &mut App| content,
                cx,
            );
            assert!(cx.has_global::<InstalledDecorations>());

            // Installing again replaces; there is only ever one.
            set_popout_decorations(DefaultPopoutDecorations, cx);
            assert!(cx.has_global::<InstalledDecorations>());

            reset_popout_decorations(cx);
            assert!(!cx.has_global::<InstalledDecorations>());
            // Resetting twice is fine.
            reset_popout_decorations(cx);
        });
    }
}

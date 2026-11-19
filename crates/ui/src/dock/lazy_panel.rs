//! Sleeping and awake tabs.
//!
//! A [`LazyPanel`] is a tab whose content may not exist yet. It is the one
//! wrapper every restorable tab goes through, so the behaviour is the same for
//! every editor and no editor has to know about it:
//!
//! * **Sleeping**: only the saved [`PanelState`] (file or restore key) is held.
//!   No editor exists, nothing is loaded. The tab is drawn in italics, greyer,
//!   with a leaf icon.
//! * **Waking**: the tab was selected. A loading skeleton is shown while the
//!   real panel is built on the *next frame*, never inside the click or the
//!   render that asked for it.
//! * **Awake**: the real panel exists and is rendered in place.
//!
//! A tab falls asleep again after [`SLEEP_AFTER`] without being displayed,
//! unless it is one of the last [`KEEP_AWAKE_RECENT`] tabs used, shows unsaved
//! changes, or cannot be rebuilt from its saved state. Waking rebuilds it from
//! that state (the file on disk, the restorer data), the same way a restart
//! would.
//!
//! GPUI entities live on the main thread, so "async" here means *deferred and
//! staged*: construction never runs inside the interaction that needs it, and
//! a skeleton is painted first. It cannot move an editor's own constructor off
//! the main thread.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gpui::{
    AppContext as _,
    div, px, AnyElement, App, Context, EntityId,
    EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Window,
};

use super::{Panel, PanelEvent, PanelState, PanelView};
use crate::{
    button::Button, h_flex, popup_menu::PopupMenu, v_flex, ActiveTheme as _, IconName, skeleton::Skeleton,
};

/// A tab not displayed for this long falls asleep.
pub const SLEEP_AFTER: Duration = Duration::from_secs(10 * 60);

/// The most recently used tabs never fall asleep.
pub const KEEP_AWAKE_RECENT: usize = 3;

/// How far along a tab is. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabLife {
    Sleeping,
    Waking,
    Awake,
}

/// Builds the real panel for a saved tab state, or `None` if it cannot be.
pub type LazyBuilder =
    Arc<dyn Fn(&PanelState, &mut Window, &mut App) -> Option<Arc<dyn PanelView>>>;

static RECENT: Mutex<Vec<EntityId>> = Mutex::new(Vec::new());
static NAMES: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);

fn touch(id: EntityId) {
    if let Ok(mut recent) = RECENT.lock() {
        if recent.first() != Some(&id) {
            recent.retain(|other| *other != id);
            recent.insert(0, id);
            recent.truncate(KEEP_AWAKE_RECENT);
        }
    }
}

fn is_recent(id: EntityId) -> bool {
    RECENT.lock().is_ok_and(|recent| recent.contains(&id))
}

/// `Panel::panel_name` wants a `&'static str`; a saved state only has a `String`.
/// Panel names are a small fixed set, so each distinct one is leaked once.
fn intern(name: &str) -> &'static str {
    let mut guard = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    let names = guard.get_or_insert_with(HashSet::new);
    if let Some(existing) = names.get(name) {
        return existing;
    }
    let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.insert(leaked);
    leaked
}

pub struct LazyPanel {
    id: EntityId,
    name: &'static str,
    /// What the panel is rebuilt from. Refreshed from the live panel when it sleeps.
    state: PanelState,
    inner: Option<Arc<dyn PanelView>>,
    life: TabLife,
    builder: LazyBuilder,
    label: Option<SharedString>,
    icon: Option<IconName>,
    /// Displayed right now (rendered and not since deactivated).
    active: bool,
    last_used: Instant,
    /// The last build produced nothing; the tab waits for the next activation.
    failed: bool,
    focus: FocusHandle,
}

impl LazyPanel {
    /// A sleeping tab: nothing is built until it is displayed.
    pub fn sleeping(state: PanelState, builder: LazyBuilder, cx: &mut App) -> Arc<dyn PanelView> {
        Self::create(state, builder, TabLife::Sleeping, cx)
    }

    /// A tab that wakes on the next frame (a file the user just opened).
    pub fn waking(state: PanelState, builder: LazyBuilder, cx: &mut App) -> Arc<dyn PanelView> {
        Self::create(state, builder, TabLife::Waking, cx)
    }

    fn create(
        state: PanelState,
        builder: LazyBuilder,
        life: TabLife,
        cx: &mut App,
    ) -> Arc<dyn PanelView> {
        let label = state
            .file()
            .and_then(|file| std::path::Path::new(file).file_name())
            .map(|name| SharedString::from(name.to_string_lossy().into_owned()));
        let icon = state
            .file()
            .and_then(|file| ICON_RESOLVER.get().and_then(|resolve| resolve(std::path::Path::new(file))));
        let entity = cx.new(|cx| Self {
            id: cx.entity_id(),
            name: intern(&state.panel_name),
            state,
            inner: None,
            life,
            builder,
            label,
            icon,
            active: false,
            last_used: Instant::now(),
            failed: false,
            focus: cx.focus_handle(),
        });
        Arc::new(entity)
    }

    /// Build the real panel now. Called from a frame callback, never from render.
    fn wake_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inner.is_some() {
            return;
        }
        let builder = self.builder.clone();
        let state = self.state.clone();
        match builder(&state, window, cx) {
            Some(panel) => {
                self.name = panel.panel_name(cx);
                self.label = panel.tab_name(cx);
                self.icon = panel.tab_icon(cx);
                self.life = TabLife::Awake;
                self.failed = false;
                self.last_used = Instant::now();
                if self.active {
                    panel.set_active(true, window, cx);
                }
                self.inner = Some(panel);
                cx.emit(PanelEvent::LayoutChanged);
            }
            None => {
                tracing::warn!("tab could not be rebuilt from its saved state; keeping it asleep");
                self.life = TabLife::Sleeping;
                self.failed = true;
            }
        }
        cx.notify();
    }

    fn request_wake(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.life = TabLife::Waking;
        let this = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            _ = this.update(cx, |panel, cx| panel.wake_now(window, cx));
        });
    }

    fn file_name_fallback(&self) -> Option<SharedString> {
        self.label.clone()
    }

    fn skeleton(&self, cx: &App) -> AnyElement {
        if self.failed {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child("This tab could not be reopened from its saved state.")
                .into_any_element();
        }
        // A toolbar strip, a side column and a body of rows: the generic shape
        // of an editor, so nothing here depends on which editor is loading.
        let bars = [0.92, 0.78, 0.85, 0.6, 0.88, 0.45, 0.7, 0.8];
        v_flex()
            .size_full()
            .gap_3()
            .p_4()
            .child(
                h_flex()
                    .gap_2()
                    .child(Skeleton::new().w(px(80.)).h(px(22.)))
                    .child(Skeleton::new().w(px(80.)).h(px(22.)))
                    .child(Skeleton::new().w(px(120.)).h(px(22.))),
            )
            .child(
                h_flex()
                    .size_full()
                    .gap_3()
                    .child(
                        v_flex()
                            .w(px(200.))
                            .h_full()
                            .gap_2()
                            .children((0..6).map(|_| Skeleton::new().h(px(16.)).secondary(true))),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .h_full()
                            .gap_2()
                            .children(bars.iter().map(|w| {
                                Skeleton::new().w(gpui::relative(*w)).h(px(16.))
                            })),
                    ),
            )
            .into_any_element()
    }
}

impl EventEmitter<PanelEvent> for LazyPanel {}

impl Focusable for LazyPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.inner {
            Some(inner) => inner.focus_handle(cx),
            None => self.focus.clone(),
        }
    }
}

impl Render for LazyPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Being rendered means being displayed.
        self.active = true;
        self.last_used = Instant::now();
        touch(self.id);

        if self.inner.is_none() {
            // A failed tab retries only when activated again (`set_active`).
            if self.life == TabLife::Sleeping && !self.failed {
                self.request_wake(window, cx);
            } else if self.life == TabLife::Waking {
                // Created already waking: its first render schedules the build.
                self.request_wake(window, cx);
            }
        }

        match &self.inner {
            Some(inner) => div().size_full().child(inner.view()).into_any_element(),
            None => div()
                .size_full()
                .child(self.skeleton(cx))
                .into_any_element(),
        }
    }
}

impl Panel for LazyPanel {
    fn panel_name(&self) -> &'static str {
        self.name
    }

    fn life(&self) -> TabLife {
        self.life
    }

    fn tab_name(&self, cx: &App) -> Option<SharedString> {
        match &self.inner {
            Some(inner) => inner.tab_name(cx),
            None => self.file_name_fallback(),
        }
    }

    fn tab_icon(&self, cx: &App) -> Option<IconName> {
        match &self.inner {
            Some(inner) => inner.tab_icon(cx),
            None => self.icon.clone(),
        }
    }

    fn tab_unsaved(&self, cx: &App) -> bool {
        self.inner.as_ref().is_some_and(|inner| inner.tab_unsaved(cx))
    }

    fn panel_file_path(&self, cx: &App) -> Option<std::path::PathBuf> {
        match &self.inner {
            Some(inner) => inner.panel_file_path(cx),
            None => self.state.file().map(std::path::PathBuf::from),
        }
    }

    fn title(&self, window: &Window, cx: &App) -> AnyElement {
        match &self.inner {
            Some(inner) => inner.title(window, cx),
            None => self
                .file_name_fallback()
                .unwrap_or_default()
                .into_any_element(),
        }
    }

    fn title_style(&self, cx: &App) -> Option<super::TitleStyle> {
        self.inner.as_ref().and_then(|inner| inner.title_style(cx))
    }

    fn title_suffix(&self, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        self.inner
            .as_ref()
            .and_then(|inner| inner.title_suffix(window, cx))
    }

    fn closable(&self, cx: &App) -> bool {
        self.inner.as_ref().map_or(true, |inner| inner.closable(cx))
    }

    fn zoomable(&self, cx: &App) -> Option<super::PanelControl> {
        match &self.inner {
            Some(inner) => inner.zoomable(cx),
            None => Some(super::PanelControl::Menu),
        }
    }

    fn visible(&self, cx: &App) -> bool {
        self.inner.as_ref().map_or(true, |inner| inner.visible(cx))
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut App) {
        self.active = active;
        if active {
            self.last_used = Instant::now();
            touch(self.id);
            // Selecting a tab whose last build failed tries again.
            self.failed = false;
        }
        if let Some(inner) = &self.inner {
            inner.set_active(active, window, cx);
        }
    }

    fn set_zoomed(&mut self, zoomed: bool, window: &mut Window, cx: &mut App) {
        if let Some(inner) = &self.inner {
            inner.set_zoomed(zoomed, window, cx);
        }
    }

    fn popup_menu(&self, menu: PopupMenu, window: &Window, cx: &App) -> PopupMenu {
        match &self.inner {
            Some(inner) => inner.popup_menu(menu, window, cx),
            None => menu,
        }
    }

    fn toolbar_buttons(&self, window: &mut Window, cx: &mut App) -> Option<Vec<Button>> {
        self.inner
            .as_ref()
            .and_then(|inner| inner.toolbar_buttons(window, cx))
    }

    fn dump(&self, cx: &App) -> PanelState {
        match &self.inner {
            Some(inner) => inner.dump(cx),
            None => self.state.clone(),
        }
    }

    fn inner_padding(&self, cx: &App) -> bool {
        self.inner
            .as_ref()
            .map_or(false, |inner| inner.inner_padding(cx))
    }

    fn discord_icon_key(&self, cx: &App) -> &'static str {
        match &self.inner {
            Some(inner) => inner.discord_icon_key(cx),
            None => "editor",
        }
    }

    fn try_sleep(&mut self, cx: &mut App) -> bool {
        let Some(inner) = self.inner.clone() else {
            return false;
        };
        if self.active
            || self.last_used.elapsed() < SLEEP_AFTER
            || is_recent(self.id)
            || inner.tab_unsaved(cx)
        {
            return false;
        }
        // Only a tab that can be rebuilt may be dropped.
        let state = inner.dump(cx);
        let rebuildable =
            state.owner().is_none() && (state.file().is_some() || state.restore_key().is_some());
        if !rebuildable {
            return false;
        }
        self.label = inner.tab_name(cx).or_else(|| self.label.take());
        self.icon = inner.tab_icon(cx).or(self.icon.clone());
        self.state = state;
        self.inner = None;
        self.life = TabLife::Sleeping;
        true
    }
}

/// Put every idle, rebuildable tab of `tab_panels` to sleep. Returns how many
/// fell asleep. A tab whose file is the owner of another live tab (an editor
/// whose inner panels were moved out) is kept awake: its panels are claimed by
/// that owner and would have nothing to return to.
pub fn sweep_idle(panels: &[Arc<dyn PanelView>], cx: &mut App) -> usize {
    let owners: HashSet<String> = panels
        .iter()
        .filter_map(|panel| panel.dump(cx).owner().map(|(file, _)| file.to_owned()))
        .collect();
    let mut slept = 0;
    for panel in panels {
        if let Some(path) = panel.panel_file_path(cx) {
            if owners.contains(path.to_string_lossy().as_ref()) {
                continue;
            }
        }
        if panel.try_sleep(cx) {
            slept += 1;
        }
    }
    slept
}

/// The colour of the leaf shown on a sleeping tab, in the tab strip and the sidebar.
pub fn sleeping_leaf_color() -> gpui::Hsla {
    gpui::rgb(0x4ade80).into()
}

/// `icon` with the sleeping leaf as a badge in its lower right corner.
pub fn with_sleeping_badge(icon: impl IntoElement, leaf_size: gpui::Pixels) -> gpui::Div {
    div()
        .relative()
        .flex_none()
        .child(icon)
        .child(
            div().absolute().right(px(-4.)).bottom(px(-4.)).child(
                crate::Icon::new(IconName::SleepingLeaf)
                    .size(leaf_size)
                    .text_color(sleeping_leaf_color()),
            ),
        )
}

static ICON_RESOLVER: std::sync::OnceLock<fn(&std::path::Path) -> Option<IconName>> =
    std::sync::OnceLock::new();

/// How a tab that was never built finds the icon of its file type. Set once by
/// the app; later calls are ignored.
pub fn set_icon_resolver(resolver: fn(&std::path::Path) -> Option<IconName>) {
    _ = ICON_RESOLVER.set(resolver);
}

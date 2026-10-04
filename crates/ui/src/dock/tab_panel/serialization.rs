use super::*;
use crate::{button::Button, popup_menu::PopupMenu};

impl Panel for TabPanel {
    fn panel_name(&self) -> &'static str {
        "TabPanel"
    }

    fn title(&self, window: &Window, cx: &App) -> gpui::AnyElement {
        self.active_panel(cx)
            .map(|panel| panel.title(window, cx))
            .unwrap_or("Empty Tab".into_any_element())
    }

    fn closable(&self, cx: &App) -> bool {
        if !self.closable {
            return false;
        }

        self.active_panel(cx)
            .map(|panel| panel.closable(cx))
            .unwrap_or(false)
    }

    fn zoomable(&self, cx: &App) -> Option<PanelControl> {
        self.active_panel(cx).and_then(|panel| panel.zoomable(cx))
    }

    fn visible(&self, cx: &App) -> bool {
        self.visible_panels(cx).next().is_some()
    }

    fn popup_menu(&self, menu: PopupMenu, window: &Window, cx: &App) -> PopupMenu {
        if let Some(panel) = self.active_panel(cx) {
            panel.popup_menu(menu, window, cx)
        } else {
            menu
        }
    }

    fn toolbar_buttons(&self, window: &mut Window, cx: &mut App) -> Option<Vec<Button>> {
        self.active_panel(cx)
            .and_then(|panel| panel.toolbar_buttons(window, cx))
    }

    fn dump(&self, cx: &App) -> PanelState {
        let mut state = PanelState::new(self);
        for panel in self.panels.iter() {
            let mut child = panel.dump(cx);
            // A panel that records nothing of its own still tells us which
            // file it edits, so a saved layout can reopen it.
            if child.file().is_none() {
                if let Some(path) = panel.panel_file_path(cx) {
                    child = child.with_file(path.to_string_lossy().into_owned());
                }
            }
            state.add_child(child);
        }
        state.info = PanelInfo::tabs(self.active_ix);
        state
    }

    fn inner_padding(&self, cx: &App) -> bool {
        self.active_panel(cx)
            .map_or(true, |panel| panel.inner_padding(cx))
    }
}

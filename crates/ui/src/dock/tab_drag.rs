//! Chrome-style cross-window tab drag infrastructure.
//!
//! The system works in three layers:
//!
//! 1. **DockWindowRegistry** (Global) – every DockArea registers its window handle and channel
//!    on creation.  Entries are cleaned up lazily when the weak entity can no longer be upgraded.
//!
//! 2. **DragScreenPosition** (Global) – the source TabPanel writes its current mouse position
//!    (converted to screen coordinates) on every drag-move event.  The drop handler reads this
//!    to decide where to send the panel when the drag ends outside the source window.
//!
//! 3. **Cross-window deposit** – `deposit_panel_into_window` uses `cx.update_window` to add a
//!    panel into another window's DockArea safely, crossing the GPUI window boundary.

use std::sync::Arc;

use gpui::{AnyWindowHandle, App, AppContext as _, Bounds, Global, Pixels, Point};

use super::{DockArea, DockChannel, PanelView};

// ─────────────────────────────────────────────────────────────────────────────
// Registry
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct DockWindowEntry {
    pub window_handle: AnyWindowHandle,
    pub dock_area: gpui::WeakEntity<DockArea>,
    pub channel: DockChannel,
}

/// App-global registry of all live DockArea windows.
pub struct DockWindowRegistry {
    pub windows: Vec<DockWindowEntry>,
}

impl Global for DockWindowRegistry {}

fn ensure_registry(cx: &mut App) {
    if !cx.has_global::<DockWindowRegistry>() {
        cx.set_global(DockWindowRegistry {
            windows: Vec::new(),
        });
    }
}

/// Register a DockArea when it is created.
pub fn register_dock_window(
    window_handle: AnyWindowHandle,
    dock_area: gpui::WeakEntity<DockArea>,
    channel: DockChannel,
    cx: &mut App,
) {
    ensure_registry(cx);
    let reg = cx.global_mut::<DockWindowRegistry>();
    // Clean up dead entries and avoid duplicate registrations.
    reg.windows
        .retain(|e| e.dock_area.upgrade().is_some() && e.window_handle != window_handle);
    reg.windows.push(DockWindowEntry {
        window_handle,
        dock_area,
        channel,
    });
}

/// Remove the entry for a window (call when the window / DockArea closes).
pub fn unregister_dock_window(window_handle: AnyWindowHandle, cx: &mut App) {
    if cx.has_global::<DockWindowRegistry>() {
        cx.global_mut::<DockWindowRegistry>()
            .windows
            .retain(|e| e.window_handle != window_handle);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Every live dock area
// ─────────────────────────────────────────────────────────────────────────────

/// A live [`DockArea`] and the window it is in; see [`all_dock_areas`].
#[derive(Clone)]
pub struct DockAreaInfo {
    pub window: AnyWindowHandle,
    pub dock_area: gpui::WeakEntity<DockArea>,
    /// Whether the window is a pop-out window (its dock area is a pop-out's).
    pub popout: bool,
}

/// Every dock area ever constructed, in construction order. Unlike
/// [`DockWindowRegistry`] (one entry per window, for cross-window drags) this
/// also lists the dock areas an editor builds inside its own tab.
#[derive(Default)]
struct LiveDockAreas(Vec<(AnyWindowHandle, gpui::WeakEntity<DockArea>)>);

impl Global for LiveDockAreas {}

/// Remember a dock area (called when it is constructed). Dead entries are
/// dropped on the way.
pub(super) fn track_dock_area(
    window_handle: AnyWindowHandle,
    dock_area: gpui::WeakEntity<DockArea>,
    cx: &mut App,
) {
    if !cx.has_global::<LiveDockAreas>() {
        cx.set_global(LiveDockAreas::default());
    }
    let live = cx.global_mut::<LiveDockAreas>();
    live.0.retain(|(_, area)| area.upgrade().is_some());
    live.0.push((window_handle, dock_area));
}

/// Every live dock area of the app: the main window's, the ones editors keep
/// inside their own tabs, and pop-outs'. Ordered for deterministic matching:
/// dock areas of ordinary windows first, then pop-outs, each group in
/// construction order. This is how the layout restore finds the inner panels of
/// an editor (see [`PanelState::with_owner`](super::PanelState::with_owner)).
pub fn all_dock_areas(cx: &App) -> Vec<DockAreaInfo> {
    let Some(live) = cx.try_global::<LiveDockAreas>() else {
        return Vec::new();
    };
    let (mut ordinary, mut popouts) = (Vec::new(), Vec::new());
    for (window, area) in &live.0 {
        if area.upgrade().is_none() {
            continue;
        }
        let popout = super::popout::is_popout_window(*window, cx);
        let info = DockAreaInfo {
            window: *window,
            dock_area: area.clone(),
            popout,
        };
        if popout {
            popouts.push(info);
        } else {
            ordinary.push(info);
        }
    }
    ordinary.extend(popouts);
    ordinary
}

// ─────────────────────────────────────────────────────────────────────────────
// ─────────────────────────────────────────────────────────────────────────────
// Drag position tracking
// ─────────────────────────────────────────────────────────────────────────────

/// Tracks the current drag position in screen coordinates.
pub struct DragScreenPosition {
    pub position: Point<Pixels>,
}

impl Global for DragScreenPosition {}

/// Called from TabPanel's on_drag_move with screen-space coordinates.
pub fn set_drag_screen_position(pos: Point<Pixels>, cx: &mut App) {
    if cx.has_global::<DragScreenPosition>() {
        cx.global_mut::<DragScreenPosition>().position = pos;
    } else {
        cx.set_global(DragScreenPosition { position: pos });
    }
}

/// Returns the last recorded drag screen position, if any.
pub fn drag_screen_position(cx: &App) -> Option<Point<Pixels>> {
    cx.try_global::<DragScreenPosition>().map(|g| g.position)
}

// ─────────────────────────────────────────────────────────────────────────────
// Cross-window deposit helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Find the registered dock window whose OS-level bounds contain `screen_pos`,
/// excluding `exclude_window` (the drag source).  Only windows that share
/// `channel` are considered.
pub fn find_target_window(
    screen_pos: Point<Pixels>,
    exclude_window: AnyWindowHandle,
    channel: DockChannel,
    cx: &mut App,
) -> Option<DockWindowEntry> {
    if !cx.has_global::<DockWindowRegistry>() {
        return None;
    }

    // Collect live candidates, purging dead entries while we're at it.
    let candidates: Vec<DockWindowEntry> = {
        let reg = cx.global_mut::<DockWindowRegistry>();
        reg.windows.retain(|e| e.dock_area.upgrade().is_some());
        reg.windows
            .iter()
            .filter(|e| e.window_handle != exclude_window && e.channel == channel)
            .cloned()
            .collect()
    };

    for entry in candidates {
        // Ask GPUI for the current OS-level window bounds (screen coordinates).
        let bounds: Option<Bounds<Pixels>> = cx
            .update_window(entry.window_handle, |_root, window, _cx| window.bounds())
            .ok();

        if let Some(bounds) = bounds {
            if bounds.contains(&screen_pos) {
                return Some(entry);
            }
        }
    }

    None
}

/// Deposit `panel` into the given window's DockArea center via `cx.update_window`.
pub fn deposit_panel_into_window(
    panel: Arc<dyn PanelView>,
    target: &DockWindowEntry,
    cx: &mut App,
) {
    let dock_area_weak = target.dock_area.clone();
    let _ = cx.update_window(target.window_handle, move |_root, window, cx| {
        if let Some(dock_area) = dock_area_weak.upgrade() {
            dock_area.update(cx, |dock, cx| {
                dock.add_panel_to_center(panel, window, cx);
            });
        }
    });
}

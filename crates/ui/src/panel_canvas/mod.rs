//! Panel Canvas: movable, resizable cards over an area, snapping into slots.
//!
//! - [`PanelCanvas`] is the area. It has a background (transparent by
//!   default), clips what overflows it, and holds slots of set sizes that
//!   cards snap into. A canvas with no slots is a free area: cards go
//!   anywhere inside its bounds and never leave them.
//! - [`PanelCanvasState`] is the canvas's state, an entity its owner keeps:
//!   the slots (added and removed at runtime), where each card is, each
//!   card's size, and the drag or resize in flight.
//! - [`RndPanel`] (resize and drag) is a card. Any mix of [`DragHandle`]s and
//!   [`ResizeHandle`]s sits inside it, and it nests: a card can hold another
//!   canvas with its own cards.
//! - [`PanelRef`] names one card on one canvas; the card and its handles all
//!   take it.
//!
//! Drags and resizes are tracked with window-level mouse listeners, so they
//! keep going when the pointer leaves the card (or the canvas). See
//! `.agents/PANEL_CANVAS.md`.

mod canvas;
mod handles;
mod rnd_panel;
mod state;

pub use canvas::PanelCanvas;
pub use handles::{DragHandle, HandleEdge, ResizeHandle};
pub use rnd_panel::RndPanel;
pub use state::{PanelCanvasState, PanelRef, Placement, Slot, SlotAnchor, SlotFit, SlotFlow, SlotGroup};

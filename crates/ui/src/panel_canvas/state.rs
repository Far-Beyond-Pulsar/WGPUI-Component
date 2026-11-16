//! The canvas's state: slots, card placements and sizes, and the drag or
//! resize in flight.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use gpui::{point, px, size, Bounds, Context, Entity, Pixels, Point, SharedString, Size};

use super::handles::HandleEdge;

/// Which edge or corner of the canvas a slot hangs from. The slot's inset is
/// measured from that edge or corner; a centred axis is offset by the inset
/// from the middle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SlotAnchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl SlotAnchor {
    /// Horizontal alignment: -1 left, 0 centre, 1 right.
    pub(crate) fn h(self) -> i8 {
        match self {
            Self::TopLeft | Self::Left | Self::BottomLeft => -1,
            Self::Top | Self::Bottom => 0,
            Self::TopRight | Self::Right | Self::BottomRight => 1,
        }
    }

    /// Vertical alignment: -1 top, 0 centre, 1 bottom.
    pub(crate) fn v(self) -> i8 {
        match self {
            Self::TopLeft | Self::Top | Self::TopRight => -1,
            Self::Left | Self::Right => 0,
            Self::BottomLeft | Self::Bottom | Self::BottomRight => 1,
        }
    }
}

/// How the cards in one slot line up when there are several.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SlotFlow {
    /// Side by side.
    #[default]
    Row,
    /// Stacked.
    Column,
}

/// What a slot does with cards larger than it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SlotFit {
    /// The slot grows to contain its cards (away from its anchor, or pushing
    /// the slots after it along in its [`SlotGroup`]). Its size is then only
    /// its size while empty, as a drop target during a drag.
    #[default]
    Expand,
    /// The slot keeps its size and its cards are shrunk to fit: each is at
    /// most the slot's size, clipped, and several share it.
    Shrink,
}

/// A strip of slots laid out together along one edge: when one slot grows
/// (an [`SlotFit::Expand`] slot gaining cards) the others move along instead
/// of being covered.
///
/// The strip hangs from `anchor` like a slot, spans the canvas along its
/// `flow` and packs its slots towards the anchor in the order they were
/// added. An empty slot takes no room except while a card is being dragged,
/// when it opens up at its size as a drop target.
#[derive(Clone, Debug, PartialEq)]
pub struct SlotGroup {
    pub id: SharedString,
    pub anchor: SlotAnchor,
    pub inset: Point<Pixels>,
    pub flow: SlotFlow,
    pub gap: Pixels,
}

impl SlotGroup {
    pub fn new(id: impl Into<SharedString>, anchor: SlotAnchor) -> Self {
        Self {
            id: id.into(),
            anchor,
            inset: point(px(0.), px(0.)),
            flow: SlotFlow::Row,
            gap: px(8.),
        }
    }

    pub fn inset(mut self, x: Pixels, y: Pixels) -> Self {
        self.inset = point(x, y);
        self
    }

    pub fn flow(mut self, flow: SlotFlow) -> Self {
        self.flow = flow;
        self
    }

    pub fn gap(mut self, gap: Pixels) -> Self {
        self.gap = gap;
        self
    }
}

/// A place on the canvas cards snap into.
///
/// The slot's size is its drop target and the box its cards align in; what
/// happens to cards larger than it is its [`SlotFit`]. Several cards in one
/// slot line up along its [`SlotFlow`]. A slot in a [`SlotGroup`] is placed
/// by the group (its own anchor and inset are not used).
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    pub id: SharedString,
    pub anchor: SlotAnchor,
    pub inset: Point<Pixels>,
    pub size: Size<Pixels>,
    pub flow: SlotFlow,
    pub gap: Pixels,
    pub fit: SlotFit,
    pub group: Option<SharedString>,
}

impl Slot {
    pub fn new(id: impl Into<SharedString>, anchor: SlotAnchor, size: Size<Pixels>) -> Self {
        Self {
            id: id.into(),
            anchor,
            inset: point(px(0.), px(0.)),
            size,
            flow: SlotFlow::Row,
            gap: px(8.),
            fit: SlotFit::Expand,
            group: None,
        }
    }

    pub fn fit(mut self, fit: SlotFit) -> Self {
        self.fit = fit;
        self
    }

    /// Lay this slot out in a [`SlotGroup`] (its anchor is the group's).
    pub fn group(mut self, group: impl Into<SharedString>) -> Self {
        self.group = Some(group.into());
        self
    }

    /// The offset from the anchoring edges (towards the middle).
    pub fn inset(mut self, x: Pixels, y: Pixels) -> Self {
        self.inset = point(x, y);
        self
    }

    pub fn flow(mut self, flow: SlotFlow) -> Self {
        self.flow = flow;
        self
    }

    pub fn gap(mut self, gap: Pixels) -> Self {
        self.gap = gap;
        self
    }

    /// The slot's rectangle inside `canvas` (window coordinates).
    pub fn bounds_in(&self, canvas: Bounds<Pixels>) -> Bounds<Pixels> {
        let (cw, ch) = (f32::from(canvas.size.width), f32::from(canvas.size.height));
        let (w, h) = (f32::from(self.size.width), f32::from(self.size.height));
        let (ix, iy) = (f32::from(self.inset.x), f32::from(self.inset.y));
        let x = match self.anchor.h() {
            -1 => ix,
            0 => (cw - w) / 2. + ix,
            _ => cw - w - ix,
        };
        let y = match self.anchor.v() {
            -1 => iy,
            0 => (ch - h) / 2. + iy,
            _ => ch - h - iy,
        };
        Bounds::new(canvas.origin + point(px(x), px(y)), self.size)
    }
}

/// Where a card is.
#[derive(Clone, Debug, PartialEq)]
pub enum Placement {
    /// In a slot, at `order` among the slot's cards (lowest first).
    Slot { slot: SharedString, order: i32 },
    /// Anywhere: the card's top-left corner relative to the canvas's.
    Free(Point<Pixels>),
}

impl Placement {
    pub fn slot(id: impl Into<SharedString>) -> Self {
        Self::Slot {
            slot: id.into(),
            order: 0,
        }
    }

    pub fn slot_id(&self) -> Option<&SharedString> {
        match self {
            Self::Slot { slot, .. } => Some(slot),
            Self::Free(_) => None,
        }
    }
}

/// One card on one canvas. The card ([`super::RndPanel`]) and its handles all
/// take one.
#[derive(Clone)]
pub struct PanelRef {
    pub(crate) state: Entity<PanelCanvasState>,
    pub(crate) id: SharedString,
}

impl PanelRef {
    pub fn new(state: &Entity<PanelCanvasState>, id: impl Into<SharedString>) -> Self {
        Self {
            state: state.clone(),
            id: id.into(),
        }
    }

    pub fn id(&self) -> &SharedString {
        &self.id
    }

    pub fn state(&self) -> &Entity<PanelCanvasState> {
        &self.state
    }
}

/// What the owner set for a card: absent until the card is first moved or
/// resized (or the owner sets it), so the card's defaults apply until then.
#[derive(Clone, Debug, Default)]
struct PanelEntry {
    placement: Option<Placement>,
    size: Option<Size<Pixels>>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum InteractionKind {
    /// Dragging the card; `grab` is the pointer's offset inside it.
    Move { grab: Point<Pixels> },
    /// Resizing from `edge`; `pointer` is where the resize started.
    Resize {
        edge: HandleEdge,
        start: Bounds<Pixels>,
        pointer: Point<Pixels>,
    },
}

/// The drag or resize in flight.
#[derive(Clone, Debug)]
pub(crate) struct Interaction {
    pub(crate) panel: SharedString,
    pub(crate) kind: InteractionKind,
    /// The card's placement when the interaction began.
    pub(crate) placement: Placement,
    /// The card's live bounds (window coordinates). While resizing a slotted
    /// card only the size is used: the slot keeps placing it.
    pub(crate) bounds: Bounds<Pixels>,
    /// The slot under the pointer while dragging.
    pub(crate) hover_slot: Option<SharedString>,
    /// A full slot under the pointer while dragging: dropping there
    /// sends the card back.
    pub(crate) blocked_slot: Option<SharedString>,
    /// Whether the pointer moved: a click on a handle changes nothing.
    pub(crate) moved: bool,
}

impl Interaction {
    pub(crate) fn is_move(&self) -> bool {
        matches!(self.kind, InteractionKind::Move { .. })
    }
}

/// Per frame facts the canvas and its cards record while drawing, read when
/// a drag or resize starts and ends. Not state: written without notifying.
#[derive(Default)]
pub(crate) struct Geometry {
    pub(crate) canvas: Option<Bounds<Pixels>>,
    pub(crate) generation: u64,
    pub(crate) panels: HashMap<SharedString, PanelFrame>,
    /// Each drawn slot's box, as of the frame it was drawn in.
    pub(crate) slots: HashMap<SharedString, SlotDraw>,
}

/// Where a slot's box was drawn.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SlotDraw {
    pub(crate) generation: u64,
    pub(crate) bounds: Bounds<Pixels>,
    /// Drawn as an empty drop preview, not around cards.
    pub(crate) empty: bool,
}

impl Geometry {
    fn drawn(&self, slot: &Slot) -> Option<SlotDraw> {
        self.slots
            .get(&slot.id)
            .filter(|draw| draw.generation == self.generation)
            .copied()
    }

    /// Record a slot box drawn this frame.
    pub(crate) fn record_slot(&mut self, slot: &Slot, generation: u64, bounds: Bounds<Pixels>, empty: bool) {
        self.slots
            .insert(slot.id.clone(), SlotDraw { generation, bounds, empty });
    }
}

/// The size an empty `slot` previews at while `dragged` (the dragged card's
/// size) is held over the canvas: the room the card would take there, so the
/// preview shows where it will sit. Per axis, the card's size, or the slot's
/// own when the card is smaller; a shrinking slot never grows.
pub(crate) fn preview_size(slot: &Slot, dragged: Option<Size<Pixels>>) -> Size<Pixels> {
    match (slot.fit, dragged) {
        (SlotFit::Expand, Some(card)) => size(
            card.width.max(slot.size.width),
            card.height.max(slot.size.height),
        ),
        _ => slot.size,
    }
}

impl Geometry {

    /// Where `slot` takes drops: its drawn box (when it was drawn this
    /// frame), joined, for a slot outside a group, with its nominal rectangle
    /// so an expanding slot is never a smaller target than its size.
    pub(crate) fn slot_target(&self, slot: &Slot, canvas: Bounds<Pixels>) -> Option<Bounds<Pixels>> {
        let drawn = self.drawn(slot).map(|draw| draw.bounds);
        match (&slot.group, drawn) {
            (Some(_), drawn) => drawn,
            (None, Some(drawn)) => Some(drawn.union(&slot.bounds_in(canvas))),
            (None, None) => Some(slot.bounds_in(canvas)),
        }
    }

    /// Whether `slot` is full: an expanding slot that its cards (the dragged
    /// one aside, which is drawn elsewhere) already grew past its size along
    /// its flow takes no more. An empty slot is never full, however large the
    /// size it remembers.
    pub(crate) fn slot_full(&self, slot: &Slot) -> bool {
        if slot.fit == SlotFit::Shrink {
            return false;
        }
        match self.drawn(slot) {
            Some(draw) if !draw.empty => slot_overflows(slot, draw.bounds.size),
            _ => false,
        }
    }
}

/// Whether a slot box of `drawn` size is past `slot`'s size along its flow.
pub(crate) fn slot_overflows(slot: &Slot, drawn: Size<Pixels>) -> bool {
    let slack = px(0.5);
    match slot.flow {
        SlotFlow::Row => drawn.width > slot.size.width + slack,
        SlotFlow::Column => drawn.height > slot.size.height + slack,
    }
}

pub(crate) struct PanelFrame {
    pub(crate) generation: u64,
    pub(crate) bounds: Option<Bounds<Pixels>>,
    pub(crate) placement: Placement,
    pub(crate) min: Size<Pixels>,
    pub(crate) max: Size<Pixels>,
}

/// A [`super::PanelCanvas`]'s state. Create it with `cx.new` and keep the
/// entity; the canvas, its cards and their handles all read it.
pub struct PanelCanvasState {
    slots: Vec<Slot>,
    groups: Vec<SlotGroup>,
    panels: HashMap<SharedString, PanelEntry>,
    allow_free: bool,
    pub(crate) interaction: Option<Interaction>,
    pub(crate) geometry: Rc<RefCell<Geometry>>,
}

impl Default for PanelCanvasState {
    fn default() -> Self {
        Self::new()
    }
}

impl PanelCanvasState {
    /// A free area: no slots, cards go anywhere inside it.
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            groups: Vec::new(),
            panels: HashMap::new(),
            allow_free: true,
            interaction: None,
            geometry: Rc::default(),
        }
    }

    pub fn with_slots(mut self, slots: impl IntoIterator<Item = Slot>) -> Self {
        self.slots = slots.into_iter().collect();
        self
    }

    /// Whether a card dropped outside every slot stays where it was dropped
    /// (the default) or goes back where it came from.
    pub fn allow_free(mut self, allow: bool) -> Self {
        self.allow_free = allow;
        self
    }

    pub fn with_groups(mut self, groups: impl IntoIterator<Item = SlotGroup>) -> Self {
        self.groups = groups.into_iter().collect();
        self
    }

    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    pub fn groups(&self) -> &[SlotGroup] {
        &self.groups
    }

    pub fn group(&self, id: &str) -> Option<&SlotGroup> {
        self.groups.iter().find(|group| group.id.as_ref() == id)
    }

    /// Add a slot group, or replace the one with the same id.
    pub fn add_group(&mut self, group: SlotGroup, cx: &mut Context<Self>) {
        match self.groups.iter_mut().find(|g| g.id == group.id) {
            Some(existing) => *existing = group,
            None => self.groups.push(group),
        }
        cx.notify();
    }

    /// Remove a slot group. Its slots fall back to their own anchors.
    pub fn remove_group(&mut self, id: &str, cx: &mut Context<Self>) {
        self.groups.retain(|group| group.id.as_ref() != id);
        cx.notify();
    }

    pub fn slot(&self, id: &str) -> Option<&Slot> {
        self.slots.iter().find(|slot| slot.id.as_ref() == id)
    }

    /// Add a slot, or replace the one with the same id.
    pub fn add_slot(&mut self, slot: Slot, cx: &mut Context<Self>) {
        match self.slots.iter_mut().find(|s| s.id == slot.id) {
            Some(existing) => *existing = slot,
            None => self.slots.push(slot),
        }
        cx.notify();
    }

    /// Remove a slot. Its cards stay where they are, placed freely.
    pub fn remove_slot(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(ix) = self.slots.iter().position(|s| s.id.as_ref() == id) else {
            return;
        };
        self.slots.remove(ix);
        let geometry = self.geometry.borrow();
        let canvas = geometry.canvas;
        for (panel, frame) in &geometry.panels {
            if frame.placement.slot_id().is_some_and(|slot| slot.as_ref() == id) {
                let origin = match (frame.bounds, canvas) {
                    (Some(bounds), Some(canvas)) => bounds.origin - canvas.origin,
                    _ => point(px(0.), px(0.)),
                };
                self.panels.entry(panel.clone()).or_default().placement =
                    Some(Placement::Free(origin));
            }
        }
        drop(geometry);
        cx.notify();
    }

    /// Where the owner or the user put a card, if anyone has.
    pub fn placement(&self, panel: &str) -> Option<&Placement> {
        self.panels.get(panel)?.placement.as_ref()
    }

    pub fn set_placement(&mut self, panel: impl Into<SharedString>, placement: Placement, cx: &mut Context<Self>) {
        self.panels.entry(panel.into()).or_default().placement = Some(placement);
        cx.notify();
    }

    /// The card's size, once it was resized (or set).
    pub fn size(&self, panel: &str) -> Option<Size<Pixels>> {
        self.panels.get(panel)?.size
    }

    pub fn set_size(&mut self, panel: impl Into<SharedString>, size: Size<Pixels>, cx: &mut Context<Self>) {
        self.panels.entry(panel.into()).or_default().size = Some(size);
        cx.notify();
    }

    /// Forget a card's placement and size: its defaults apply again.
    pub fn reset(&mut self, panel: &str, cx: &mut Context<Self>) {
        self.panels.remove(panel);
        cx.notify();
    }

    /// The card being dragged or resized, if any.
    pub fn active_panel(&self) -> Option<&SharedString> {
        self.interaction.as_ref().map(|i| &i.panel)
    }

    pub fn is_interacting(&self) -> bool {
        self.interaction.is_some()
    }

    pub(crate) fn allows_free(&self) -> bool {
        self.allow_free
    }

    // ── Interaction ─────────────────────────────────────────────────────

    pub(crate) fn begin_move(&mut self, panel: &SharedString, pointer: Point<Pixels>, cx: &mut Context<Self>) {
        let Some((bounds, placement)) = self.frame_of(panel) else {
            return;
        };
        self.interaction = Some(Interaction {
            panel: panel.clone(),
            kind: InteractionKind::Move {
                grab: pointer - bounds.origin,
            },
            placement,
            bounds,
            hover_slot: None,
            blocked_slot: None,
            moved: false,
        });
        cx.notify();
    }

    pub(crate) fn begin_resize(
        &mut self,
        panel: &SharedString,
        edge: HandleEdge,
        pointer: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some((bounds, placement)) = self.frame_of(panel) else {
            return;
        };
        self.interaction = Some(Interaction {
            panel: panel.clone(),
            kind: InteractionKind::Resize {
                edge,
                start: bounds,
                pointer,
            },
            placement,
            bounds,
            hover_slot: None,
            blocked_slot: None,
            moved: false,
        });
        cx.notify();
    }

    fn frame_of(&self, panel: &SharedString) -> Option<(Bounds<Pixels>, Placement)> {
        let geometry = self.geometry.borrow();
        let frame = geometry.panels.get(panel)?;
        Some((frame.bounds?, frame.placement.clone()))
    }

    pub(crate) fn pointer_moved(&mut self, pointer: Point<Pixels>, cx: &mut Context<Self>) {
        let geometry = self.geometry.borrow();
        let Some(canvas) = geometry.canvas else {
            return;
        };
        let Some(interaction) = self.interaction.as_mut() else {
            return;
        };
        let (min, max) = geometry
            .panels
            .get(&interaction.panel)
            .map(|frame| (frame.min, frame.max))
            .unwrap_or((size(px(0.), px(0.)), size(px(f32::MAX), px(f32::MAX))));
        match interaction.kind {
            InteractionKind::Move { grab } => {
                let target = pointer - grab;
                interaction.bounds.origin = clamp_origin(target, interaction.bounds.size, canvas);
                let under = self.slots.iter().find(|slot| {
                    geometry
                        .slot_target(slot, canvas)
                        .is_some_and(|target| target.contains(&pointer))
                });
                // A full slot is shown refusing the card; it is not a target.
                let full = under.is_some_and(|slot| geometry.slot_full(slot));
                interaction.hover_slot = under.filter(|_| !full).map(|slot| slot.id.clone());
                interaction.blocked_slot = under.filter(|_| full).map(|slot| slot.id.clone());
            }
            InteractionKind::Resize { edge, start, pointer: from } => {
                let delta = pointer - from;
                interaction.bounds = match &interaction.placement {
                    Placement::Free(_) => resize_free(start, edge, delta, min, max, canvas),
                    Placement::Slot { .. } => resize_slotted(start, edge, delta, min, max, canvas),
                };
            }
        }
        interaction.moved = true;
        drop(geometry);
        cx.notify();
    }

    /// End the drag or resize, committing where the card was dropped or the
    /// size it was given.
    pub(crate) fn finish(&mut self, cx: &mut Context<Self>) {
        let Some(interaction) = self.interaction.take() else {
            return;
        };
        cx.notify();
        if !interaction.moved {
            return;
        }
        let geometry = self.geometry.borrow();
        let Some(canvas) = geometry.canvas else {
            return;
        };
        let panel = interaction.panel.clone();
        match interaction.kind {
            InteractionKind::Move { .. } => {
                if interaction.blocked_slot.is_some() {
                    // Dropped on a full slot: back where it came from.
                    return;
                }
                if let Some(slot_id) = interaction.hover_slot.clone() {
                    let flow = self.slot(&slot_id).map(|s| s.flow).unwrap_or_default();
                    // The slot's other cards, in their order.
                    let mut members: Vec<(i32, SharedString, Option<Bounds<Pixels>>)> = geometry
                        .panels
                        .iter()
                        .filter(|(id, frame)| {
                            **id != panel
                                && frame.generation == geometry.generation
                                && frame.placement.slot_id() == Some(&slot_id)
                        })
                        .map(|(id, frame)| {
                            let order = match frame.placement {
                                Placement::Slot { order, .. } => order,
                                Placement::Free(_) => 0,
                            };
                            (order, id.clone(), frame.bounds)
                        })
                        .collect();
                    members.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
                    // Insert before the first card whose middle is past the
                    // pointer along the slot's flow.
                    let pointer = interaction.bounds.origin
                        + match interaction.kind {
                            InteractionKind::Move { grab } => grab,
                            _ => point(px(0.), px(0.)),
                        };
                    let index = members
                        .iter()
                        .position(|(_, _, bounds)| {
                            bounds.is_some_and(|b| match flow {
                                SlotFlow::Row => pointer.x < b.center().x,
                                SlotFlow::Column => pointer.y < b.center().y,
                            })
                        })
                        .unwrap_or(members.len());
                    let mut ordered: Vec<SharedString> =
                        members.into_iter().map(|(_, id, _)| id).collect();
                    ordered.insert(index, panel.clone());
                    drop(geometry);
                    for (order, id) in ordered.into_iter().enumerate() {
                        self.panels.entry(id).or_default().placement = Some(Placement::Slot {
                            slot: slot_id.clone(),
                            order: order as i32,
                        });
                    }
                } else if self.allow_free {
                    drop(geometry);
                    self.panels.entry(panel).or_default().placement =
                        Some(Placement::Free(interaction.bounds.origin - canvas.origin));
                }
            }
            InteractionKind::Resize { .. } => {
                drop(geometry);
                let entry = self.panels.entry(panel).or_default();
                entry.size = Some(interaction.bounds.size);
                if let Placement::Free(_) = interaction.placement {
                    entry.placement = Some(Placement::Free(interaction.bounds.origin - canvas.origin));
                }
            }
        }
    }

    /// The placement and size a card shows with this frame, given its
    /// defaults. Used by the canvas while drawing.
    pub(crate) fn resolve(
        &self,
        panel: &SharedString,
        default_placement: Option<&Placement>,
        fallback_order: i32,
    ) -> (Placement, Option<Size<Pixels>>) {
        let entry = self.panels.get(panel);
        let mut placement = entry
            .and_then(|e| e.placement.clone())
            .or_else(|| {
                default_placement.cloned().map(|p| match p {
                    Placement::Slot { slot, .. } => Placement::Slot {
                        slot,
                        order: fallback_order,
                    },
                    free => free,
                })
            })
            .unwrap_or(Placement::Free(point(px(0.), px(0.))));
        // A slot that no longer exists places nothing.
        if let Placement::Slot { slot, .. } = &placement {
            if self.slot(slot).is_none() {
                placement = Placement::Free(point(px(0.), px(0.)));
            }
        }
        (placement, entry.and_then(|e| e.size))
    }
}

/// Keep a card of `size` with its top-left at `origin` inside `canvas`.
pub(crate) fn clamp_origin(
    origin: Point<Pixels>,
    size: Size<Pixels>,
    canvas: Bounds<Pixels>,
) -> Point<Pixels> {
    let clamp = |v: Pixels, lo: Pixels, extent: Pixels, card: Pixels| {
        let hi = lo + (extent - card).max(px(0.));
        v.max(lo).min(hi)
    };
    point(
        clamp(origin.x, canvas.origin.x, canvas.size.width, size.width),
        clamp(origin.y, canvas.origin.y, canvas.size.height, size.height),
    )
}

/// Resize a freely placed card: the dragged edges follow the pointer, within
/// the card's limits and the canvas.
fn resize_free(
    start: Bounds<Pixels>,
    edge: HandleEdge,
    delta: Point<Pixels>,
    min: Size<Pixels>,
    max: Size<Pixels>,
    canvas: Bounds<Pixels>,
) -> Bounds<Pixels> {
    let (mut left, mut right) = (start.left(), start.right());
    let (mut top, mut bottom) = (start.top(), start.bottom());
    if edge.left() {
        let lo = canvas.left().max(right - max.width);
        left = (left + delta.x).max(lo).min(right - min.width);
    }
    if edge.right() {
        let hi = canvas.right().min(left + max.width);
        right = (right + delta.x).min(hi).max(left + min.width);
    }
    if edge.top() {
        let lo = canvas.top().max(bottom - max.height);
        top = (top + delta.y).max(lo).min(bottom - min.height);
    }
    if edge.bottom() {
        let hi = canvas.bottom().min(top + max.height);
        bottom = (bottom + delta.y).min(hi).max(top + min.height);
    }
    Bounds::from_corners(point(left, top), point(right, bottom))
}

/// Resize a slotted card: the slot keeps placing it, so only its size
/// changes. Dragging an edge outwards grows the card; on the slot's free
/// side that edge follows the pointer exactly.
fn resize_slotted(
    start: Bounds<Pixels>,
    edge: HandleEdge,
    delta: Point<Pixels>,
    min: Size<Pixels>,
    max: Size<Pixels>,
    canvas: Bounds<Pixels>,
) -> Bounds<Pixels> {
    let mut w = start.size.width;
    let mut h = start.size.height;
    if edge.left() {
        w -= delta.x;
    }
    if edge.right() {
        w += delta.x;
    }
    if edge.top() {
        h -= delta.y;
    }
    if edge.bottom() {
        h += delta.y;
    }
    let w = w.min(max.width).min(canvas.size.width).max(min.width);
    let h = h.min(max.height).min(canvas.size.height).max(min.height);
    Bounds::new(start.origin, size(w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas() -> Bounds<Pixels> {
        Bounds::new(point(px(100.), px(50.)), size(px(800.), px(600.)))
    }

    #[test]
    fn slot_bounds_follow_anchor_and_inset() {
        let slot = Slot::new("br", SlotAnchor::BottomRight, size(px(200.), px(100.)))
            .inset(px(8.), px(8.));
        let b = slot.bounds_in(canvas());
        assert_eq!(b.origin, point(px(100. + 800. - 200. - 8.), px(50. + 600. - 100. - 8.)));
        let top = Slot::new("t", SlotAnchor::Top, size(px(200.), px(40.))).inset(px(0.), px(8.));
        assert_eq!(top.bounds_in(canvas()).origin, point(px(100. + 300.), px(58.)));
    }

    #[test]
    fn a_slot_grown_past_its_size_along_its_flow_is_full() {
        let row = Slot::new("row", SlotAnchor::BottomRight, size(px(160.), px(120.)));
        // Its size while empty, or a card smaller than it: room left.
        assert!(!slot_overflows(&row, size(px(160.), px(120.))));
        assert!(!slot_overflows(&row, size(px(100.), px(300.))));
        // Grown past it along the row: full (growing across it is not).
        assert!(slot_overflows(&row, size(px(220.), px(120.))));
        let column = row.clone().flow(SlotFlow::Column);
        assert!(slot_overflows(&column, size(px(100.), px(300.))));
        assert!(!slot_overflows(&column, size(px(400.), px(100.))));

        // A shrinking slot never fills: its cards share it.
        let mut geometry = Geometry::default();
        geometry.generation = 3;
        let shrink = row.clone().fit(SlotFit::Shrink);
        let drawn = Bounds::new(point(px(0.), px(0.)), size(px(500.), px(120.)));
        geometry.record_slot(&row, 3, drawn, false);
        assert!(geometry.slot_full(&row));
        assert!(!geometry.slot_full(&shrink));
        // A box from an earlier frame says nothing.
        geometry.generation = 4;
        assert!(!geometry.slot_full(&row));
        // Emptied, it previews at the size it grew to, and is not full.
        geometry.record_slot(&row, 4, drawn, true);
        assert!(!geometry.slot_full(&row));
    }

    #[test]
    fn an_empty_slot_previews_at_the_dragged_cards_size() {
        let slot = Slot::new("s", SlotAnchor::BottomRight, size(px(160.), px(120.)));
        // Nothing dragged: its own size.
        assert_eq!(preview_size(&slot, None), slot.size);
        // A larger card: the room it would take there.
        assert_eq!(
            preview_size(&slot, Some(size(px(300.), px(200.)))),
            size(px(300.), px(200.))
        );
        // A smaller card: never below the slot's own size (per axis).
        assert_eq!(preview_size(&slot, Some(size(px(90.), px(60.)))), slot.size);
        assert_eq!(
            preview_size(&slot, Some(size(px(250.), px(80.)))),
            size(px(250.), px(120.))
        );
        // A shrinking slot keeps its size: its cards are held to it.
        let shrink = slot.clone().fit(SlotFit::Shrink);
        assert_eq!(preview_size(&shrink, Some(size(px(300.), px(200.)))), shrink.size);
    }

    #[test]
    fn drags_stay_inside_the_canvas() {
        let card = size(px(100.), px(40.));
        assert_eq!(clamp_origin(point(px(0.), px(0.)), card, canvas()), point(px(100.), px(50.)));
        assert_eq!(
            clamp_origin(point(px(2000.), px(2000.)), card, canvas()),
            point(px(800.), px(610.))
        );
        // A card wider than the canvas pins to its left edge.
        let wide = size(px(1000.), px(40.));
        assert_eq!(clamp_origin(point(px(300.), px(60.)), wide, canvas()).x, px(100.));
    }

    #[test]
    fn free_resize_follows_the_pointer_within_limits() {
        let start = Bounds::new(point(px(200.), px(200.)), size(px(300.), px(200.)));
        let min = size(px(100.), px(80.));
        let max = size(px(600.), px(400.));
        let grown = resize_free(start, HandleEdge::BottomRight, point(px(50.), px(30.)), min, max, canvas());
        assert_eq!(grown.size, size(px(350.), px(230.)));
        assert_eq!(grown.origin, start.origin);
        let left = resize_free(start, HandleEdge::Left, point(px(-40.), px(0.)), min, max, canvas());
        assert_eq!(left.origin.x, px(160.));
        assert_eq!(left.right(), start.right());
        // Past the minimum and the canvas edge.
        let tiny = resize_free(start, HandleEdge::Right, point(px(-1000.), px(0.)), min, max, canvas());
        assert_eq!(tiny.size.width, px(100.));
        let out = resize_free(start, HandleEdge::Top, point(px(0.), px(-1000.)), min, max, canvas());
        assert_eq!(out.top(), px(50.));
    }

    #[test]
    fn slotted_resize_grows_outward_from_either_edge() {
        let start = Bounds::new(point(px(200.), px(200.)), size(px(300.), px(200.)));
        let min = size(px(100.), px(80.));
        let max = size(px(600.), px(400.));
        let top = resize_slotted(start, HandleEdge::Top, point(px(0.), px(-50.)), min, max, canvas());
        assert_eq!(top.size.height, px(250.));
        let right = resize_slotted(start, HandleEdge::TopRight, point(px(1000.), px(0.)), min, max, canvas());
        assert_eq!(right.size.width, px(600.));
    }
}

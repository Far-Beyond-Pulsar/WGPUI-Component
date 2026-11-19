//! The Panel Canvas element: slots, slot groups, cards, and the window-level
//! tracking of the drag or resize in flight.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use gpui::{
    canvas, div, point, prelude::FluentBuilder as _, px, AnyElement, App, Bounds, CursorStyle,
    Div, ElementId, Entity, Fill, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, RenderOnce, SharedString, StyleRefinement,
    Styled, Window,
};

use super::{
    rnd_panel::RndPanel,
    state::{
        clamp_origin, preview_size, Geometry, InteractionKind, PanelCanvasState, PanelFrame, Placement, Slot,
        SlotAnchor, SlotFit, SlotFlow, SlotGroup,
    },
};
use crate::ActiveTheme as _;

/// An area holding [`RndPanel`] cards, with slots they snap into.
///
/// Give it a size like any `div` (often `.absolute().inset_0()` over the
/// thing it decorates). Its background is transparent unless set; what
/// overflows it is clipped and cards cannot be dragged out of it. Its own
/// children (`.child`) are drawn under the cards and do not move.
///
/// While a card is dragged it follows the pointer exactly; the slot under
/// the pointer lights up, and dropping the card there snaps it in (after the
/// slot's cards before the pointer). A full slot (see
/// [`super::SlotFit::Expand`]) lights up as refusing, and dropping there
/// sends the card back. Dropped elsewhere, the card stays where it was
/// dropped, unless the state disallows free placement.
#[derive(IntoElement)]
pub struct PanelCanvas {
    id: ElementId,
    state: Entity<PanelCanvasState>,
    panels: Vec<RndPanel>,
    background: Option<Fill>,
    clip: bool,
    base: Div,
}

impl PanelCanvas {
    pub fn new(id: impl Into<ElementId>, state: &Entity<PanelCanvasState>) -> Self {
        Self {
            id: id.into(),
            state: state.clone(),
            panels: Vec::new(),
            background: None,
            clip: true,
            base: div(),
        }
    }

    /// Add a card.
    pub fn panel(mut self, panel: RndPanel) -> Self {
        self.panels.push(panel);
        self
    }

    pub fn panels(mut self, panels: impl IntoIterator<Item = RndPanel>) -> Self {
        self.panels.extend(panels);
        self
    }

    /// The canvas's background (transparent by default).
    pub fn background(mut self, fill: impl Into<Fill>) -> Self {
        self.background = Some(fill.into());
        self
    }

    /// Whether what overflows the canvas is clipped (the default).
    /// Popups drawn deferred (menus, tooltips) are never clipped.
    pub fn clip(mut self, clip: bool) -> Self {
        self.clip = clip;
        self
    }
}

impl ParentElement for PanelCanvas {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.base.extend(elements);
    }
}

impl Styled for PanelCanvas {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

/// An absolute, zero-hitbox box that hangs its content from `anchor` (with
/// `inset` from the anchoring edges). A centred axis spans the canvas and
/// centres the content; the others hug it.
fn hang(anchor: SlotAnchor, inset: gpui::Point<Pixels>, span: Option<SlotFlow>) -> Div {
    let el = div().absolute().flex();
    // A centred axis spans the canvas, shifted by the inset; a strip spans
    // its flow axis, inset at both ends.
    let el = match (anchor.h(), span == Some(SlotFlow::Row)) {
        (0, _) => el.left(inset.x).right(-inset.x),
        (_, true) => el.left(inset.x).right(inset.x),
        (h, false) if h < 0 => el.left(inset.x),
        _ => el.right(inset.x),
    };
    match (anchor.v(), span == Some(SlotFlow::Column)) {
        (0, _) => el.top(inset.y).bottom(-inset.y),
        (_, true) => el.top(inset.y).bottom(inset.y),
        (v, false) if v < 0 => el.top(inset.y),
        _ => el.bottom(inset.y),
    }
}

/// Lay a flex box's children along `flow`, packed towards `anchor`.
fn pack(el: Div, flow: SlotFlow, anchor: SlotAnchor) -> Div {
    let (main, cross) = match flow {
        SlotFlow::Row => (anchor.h(), anchor.v()),
        SlotFlow::Column => (anchor.v(), anchor.h()),
    };
    let el = match flow {
        SlotFlow::Row => el.flex_row(),
        SlotFlow::Column => el.flex_col(),
    };
    let el = match main {
        -1 => el.justify_start(),
        0 => el.justify_center(),
        _ => el.justify_end(),
    };
    match cross {
        -1 => el.items_start(),
        0 => el.items_center(),
        _ => el.items_end(),
    }
}

/// How the slot under a dragged card is lit.
#[derive(Clone, Copy)]
enum Light {
    /// A drop zone while a card is dragged: it would take the card.
    Available,
    /// The drop zone under the pointer: dropping snaps the card in.
    Accepts,
    /// A full slot under the pointer: dropping sends the card back.
    Full,
}

/// A slot's own box: its cards along its flow, sized by its fit, recording
/// where it was drawn. `None` when there is nothing to draw (an empty slot
/// outside a drag).
#[allow(clippy::too_many_arguments)]
fn slot_box(
    slot: &Slot,
    anchor: SlotAnchor,
    members: Vec<AnyElement>,
    dragged: Option<gpui::Size<Pixels>>,
    grouped: bool,
    light: Option<Light>,
    colors: (Hsla, Hsla, Pixels),
    geometry: &Rc<RefCell<Geometry>>,
    generation: u64,
) -> Option<Div> {
    let empty = members.is_empty();
    let dragging = dragged.is_some();
    if empty && !dragging {
        return None;
    }
    let (accept, refuse, radius) = colors;
    let el = pack(div().relative().flex_shrink_0().gap(slot.gap), slot.flow, anchor);
    let el = match slot.fit {
        SlotFit::Shrink => el.w(slot.size.width).h(slot.size.height),
        // Empty, it opens as a drop target at the room the dragged card would
        // take there (the slot's own size if the card is smaller), so it shows
        // where the card will sit; holding cards, it is as large as they are.
        SlotFit::Expand if empty => {
            let preview = preview_size(slot, dragged);
            el.w(preview.width).h(preview.height)
        }
        // During a drag, a slot on its own outlines its whole drop zone (its
        // cards are packed to its anchor, so nothing moves); in a group that
        // would push the neighbours, so it hugs its cards.
        SlotFit::Expand if dragging && !grouped => el.min_w(slot.size.width).min_h(slot.size.height),
        SlotFit::Expand => el,
    };
    let recorded = slot.clone();
    let geometry = geometry.clone();
    Some(
        el.on_frame(move |frame, _, _| {
            geometry
                .borrow_mut()
                .record_slot(&recorded, generation, frame.bounds, empty);
        })
        .when_some(light, |el, light| {
            // (border colour, fill opacity): every drop zone faint, the one
            // under the pointer strong.
            let (color, fill) = match light {
                Light::Available => (accept.opacity(0.45), 0.03),
                Light::Accepts => (accept, 0.08),
                Light::Full => (refuse, 0.08),
            };
            el.child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(radius)
                    .bg(color.opacity(fill))
                    .border_2()
                    .border_dashed()
                    .border_color(color),
            )
        })
        .children(members),
    )
}

impl RenderOnce for PanelCanvas {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let state_entity = self.state;
        let state = state_entity.read(cx);
        let geometry = state.geometry.clone();
        let interaction = state.interaction.clone();
        let slots: Vec<Slot> = state.slots().to_vec();
        let groups: Vec<SlotGroup> = state.groups().to_vec();
        let canvas_bounds = geometry.borrow().canvas;
        // Which slots are full, from the frame just drawn (before this frame
        // starts recording): a full slot is no drop zone.
        let full: Vec<SharedString> = {
            let geometry = geometry.borrow();
            slots
                .iter()
                .filter(|slot| geometry.slot_full(slot))
                .map(|slot| slot.id.clone())
                .collect()
        };
        let generation = {
            let mut geometry = geometry.borrow_mut();
            geometry.generation += 1;
            geometry.generation
        };
        let slot_of = |id: &SharedString| slots.iter().find(|slot| &slot.id == id);

        let mut slotted: HashMap<SharedString, Vec<(i32, usize, AnyElement)>> = HashMap::new();
        let mut free: Vec<AnyElement> = Vec::new();
        let mut floating: Option<AnyElement> = None;

        for (ix, mut panel) in self.panels.into_iter().enumerate() {
            let id = panel.panel.id.clone();
            let (placement, stored_size) =
                state.resolve(&id, panel.default_placement.as_ref(), 1000 + ix as i32);
            let active = interaction.as_ref().filter(|i| i.panel == id);
            let mut size = stored_size.or(panel.default_size);
            if let Some(i) = active {
                if !i.is_move() {
                    size = Some(i.bounds.size);
                }
            }
            // A shrinking slot bounds its cards, and their resizing.
            let shrink_to = placement
                .slot_id()
                .and_then(|slot| slot_of(slot))
                .filter(|slot| slot.fit == SlotFit::Shrink)
                .map(|slot| slot.size);
            let mut max = panel.max_size;
            if let Some(limit) = shrink_to {
                max = gpui::size(max.width.min(limit.width), max.height.min(limit.height));
                size = size.map(|s| gpui::size(s.width.min(max.width), s.height.min(max.height)));
            }
            let last_bounds = {
                let mut geometry = geometry.borrow_mut();
                let last = geometry.panels.get(&id).and_then(|frame| frame.bounds);
                geometry.panels.insert(
                    id.clone(),
                    PanelFrame {
                        generation,
                        bounds: last,
                        placement: placement.clone(),
                        min: panel.min_size,
                        max,
                    },
                );
                last
            };
            panel.applied_size = size;
            panel.geometry = Some(geometry.clone());

            // A dragged card, and a free card being resized, are drawn where
            // the interaction puts them, above everything else.
            let floats = active.filter(|i| i.is_move() || matches!(i.placement, Placement::Free(_)));
            if let (Some(i), Some(canvas)) = (floats, canvas_bounds) {
                let origin = i.bounds.origin - canvas.origin;
                floating = Some(
                    div()
                        .absolute()
                        .left(origin.x)
                        .top(origin.y)
                        .child(panel)
                        .into_any_element(),
                );
                continue;
            }
            match placement {
                Placement::Slot { slot, order } => {
                    panel.shrink_to = shrink_to;
                    slotted
                        .entry(slot)
                        .or_default()
                        .push((order, ix, panel.into_any_element()));
                }
                Placement::Free(origin) => {
                    // Keep it inside the canvas as the canvas resizes.
                    let origin = match (canvas_bounds, size.or(last_bounds.map(|b| b.size))) {
                        (Some(canvas), Some(card)) => clamp_origin(
                            origin,
                            card,
                            Bounds::new(point(px(0.), px(0.)), canvas.size),
                        ),
                        _ => origin,
                    };
                    free.push(
                        div()
                            .absolute()
                            .left(origin.x)
                            .top(origin.y)
                            .child(panel)
                            .into_any_element(),
                    );
                }
            }
        }

        let theme = cx.theme();
        let colors = (theme.drag_border, theme.danger, theme.radius);
        // The dragged card's size, while a card is being moved.
        let dragged = interaction.as_ref().filter(|i| i.is_move()).map(|i| i.bounds.size);
        let light_of = |slot: &Slot| {
            let i = interaction.as_ref().filter(|i| i.is_move())?;
            if i.hover_slot.as_ref() == Some(&slot.id) {
                Some(Light::Accepts)
            } else if i.blocked_slot.as_ref() == Some(&slot.id) {
                Some(Light::Full)
            } else if full.contains(&slot.id) {
                None
            } else {
                Some(Light::Available)
            }
        };
        let mut members_of = |slot: &Slot| {
            let mut members = slotted.remove(&slot.id).unwrap_or_default();
            members.sort_by_key(|(order, ix, _)| (*order, *ix));
            members.into_iter().map(|(_, _, el)| el).collect::<Vec<_>>()
        };

        // Slot groups: one strip each, its slots in order from its anchor (a
        // strip packed to the right or bottom fills right to left, or bottom
        // to top: its first slot is in the anchored corner).
        let mut strips = Vec::new();
        for group in &groups {
            let from_end = match group.flow {
                SlotFlow::Row => group.anchor.h() > 0,
                SlotFlow::Column => group.anchor.v() > 0,
            };
            let mut boxes: Vec<Div> = slots
                .iter()
                .filter(|slot| slot.group.as_ref() == Some(&group.id))
                .filter_map(|slot| {
                    slot_box(
                        slot,
                        group.anchor,
                        members_of(slot),
                        dragged,
                        true,
                        light_of(slot),
                        colors,
                        &geometry,
                        generation,
                    )
                })
                .collect();
            if from_end {
                boxes.reverse();
            }
            strips.push(
                pack(hang(group.anchor, group.inset, Some(group.flow)), group.flow, group.anchor)
                    .gap(group.gap)
                    .children(boxes),
            );
        }
        // Slots on their own.
        let singles: Vec<Div> = slots
            .iter()
            .filter(|slot| slot.group.as_ref().map_or(true, |g| !groups.iter().any(|x| &x.id == g)))
            .filter_map(|slot| {
                let content = slot_box(
                    slot,
                    slot.anchor,
                    members_of(slot),
                    dragged,
                    false,
                    light_of(slot),
                    colors,
                    &geometry,
                    generation,
                )?;
                Some(pack(hang(slot.anchor, slot.inset, None), SlotFlow::Row, slot.anchor).child(content))
            })
            .collect();

        // Drags and resizes are followed at the window, so they go on when
        // the pointer leaves the card or the canvas.
        let tracker = {
            let state = state_entity.clone();
            canvas(
                |_, _, _| (),
                move |_, _, window, cx| {
                    if let Some(i) = state.read(cx).interaction.as_ref() {
                        window.set_window_cursor_style(match i.kind {
                            InteractionKind::Move { .. } if i.blocked_slot.is_some() => {
                                CursorStyle::OperationNotAllowed
                            }
                            InteractionKind::Move { .. } => CursorStyle::ClosedHand,
                            InteractionKind::Resize { edge, .. } => edge.cursor(),
                        });
                    }
                    window.on_mouse_event({
                        let state = state.clone();
                        move |event: &MouseMoveEvent, phase, _, cx| {
                            if !phase.capture() || !state.read(cx).is_interacting() {
                                return;
                            }
                            if event.pressed_button.is_none() {
                                // Released where no release reached us.
                                state.update(cx, |state, cx| state.finish(cx));
                                return;
                            }
                            state.update(cx, |state, cx| state.pointer_moved(event.position, cx));
                            // Nothing under the pointer reacts to a drag.
                            cx.stop_propagation();
                        }
                    });
                    window.on_mouse_event({
                        let state = state.clone();
                        move |event: &MouseUpEvent, phase, _, cx| {
                            if phase.capture()
                                && event.button == MouseButton::Left
                                && state.read(cx).is_interacting()
                            {
                                state.update(cx, |state, cx| state.finish(cx));
                            }
                        }
                    });
                },
            )
            .absolute()
            .size_0()
        };

        let canvas_geometry = geometry.clone();
        // Positioned so the cards (absolute) place against it, but never over
        // a position the caller chose (`.absolute().inset_0()` over a view):
        // forcing `relative` would make it an in-flow box with no size, and
        // its clip would hide every card.
        let mut base = self.base;
        let positioned = base.style().position.is_some();
        base.on_frame(move |frame, _, _| {
            canvas_geometry.borrow_mut().canvas = Some(frame.bounds);
        })
        .id(self.id)
        .when(!positioned, |this| this.relative())
        .when(self.clip, |this| this.overflow_hidden())
        .when_some(self.background, |this, fill| this.bg(fill))
        .children(strips)
        .children(singles)
        .children(free)
        .children(floating)
        .child(tracker)
    }
}

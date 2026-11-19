//! The drag handle and the resize handles a card holds.

use gpui::{
    div, prelude::FluentBuilder as _, px, AnyElement, App, Axis, CursorStyle, Div,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement, RenderOnce,
    StyleRefinement, Styled, Window,
};

use super::state::PanelRef;
use crate::ActiveTheme as _;

/// An edge or corner of a card a [`ResizeHandle`] sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HandleEdge {
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl HandleEdge {
    pub const ALL: [HandleEdge; 8] = [
        Self::Top,
        Self::Bottom,
        Self::Left,
        Self::Right,
        Self::TopLeft,
        Self::TopRight,
        Self::BottomLeft,
        Self::BottomRight,
    ];

    pub fn left(self) -> bool {
        matches!(self, Self::Left | Self::TopLeft | Self::BottomLeft)
    }

    pub fn right(self) -> bool {
        matches!(self, Self::Right | Self::TopRight | Self::BottomRight)
    }

    pub fn top(self) -> bool {
        matches!(self, Self::Top | Self::TopLeft | Self::TopRight)
    }

    pub fn bottom(self) -> bool {
        matches!(self, Self::Bottom | Self::BottomLeft | Self::BottomRight)
    }

    pub fn is_corner(self) -> bool {
        (self.left() || self.right()) && (self.top() || self.bottom())
    }

    pub fn cursor(self) -> CursorStyle {
        match self {
            Self::Left | Self::Right => CursorStyle::ResizeLeftRight,
            Self::Top | Self::Bottom => CursorStyle::ResizeUpDown,
            Self::TopLeft | Self::BottomRight => CursorStyle::ResizeUpLeftDownRight,
            Self::TopRight | Self::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
        }
    }
}

/// The handle that moves a card: press it and the card follows the pointer
/// until the button is released, snapping into the slot under the pointer.
///
/// Style it like a `div` (a toolbar strip, a title bar); by default it shows a
/// grip of three dots. Its own children replace nothing: they are drawn after
/// the grip.
#[derive(IntoElement)]
pub struct DragHandle {
    panel: PanelRef,
    grip: Option<Axis>,
    base: Div,
}

impl DragHandle {
    pub fn new(panel: PanelRef) -> Self {
        Self {
            panel,
            grip: Some(Axis::Vertical),
            base: div(),
        }
    }

    /// Lay the grip's dots out along `axis` (vertical by default).
    pub fn grip(mut self, axis: Axis) -> Self {
        self.grip = Some(axis);
        self
    }

    /// No grip: the handle is only what its children and style draw.
    pub fn no_grip(mut self) -> Self {
        self.grip = None;
        self
    }
}

impl ParentElement for DragHandle {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.base.extend(elements);
    }
}

impl Styled for DragHandle {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl RenderOnce for DragHandle {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let dot = cx.theme().foreground.opacity(0.75);
        let panel = self.panel;
        let dots = |axis: Axis| {
            let line = match axis {
                Axis::Vertical => div().flex().flex_col(),
                Axis::Horizontal => div().flex().flex_row(),
            };
            line.items_center()
                .justify_center()
                .gap(px(2.))
                .children((0..3).map(move |_| div().size(px(2.)).rounded_full().bg(dot)))
        };
        self.base
            .flex()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .cursor(CursorStyle::OpenHand)
            .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                cx.stop_propagation();
                let id = panel.id.clone();
                panel
                    .state
                    .update(cx, |state, cx| state.begin_move(&id, event.position, cx));
            })
            .when_some(self.grip, |this, axis| this.child(dots(axis)))
    }
}

/// A handle on one edge or corner of a card that resizes it. It places itself
/// absolutely on that edge of the card (the card is positioned), so it can go
/// anywhere among the card's children.
///
/// The resize keeps going while the pointer is outside the card: the canvas
/// tracks it with window-level listeners.
#[derive(IntoElement)]
pub struct ResizeHandle {
    panel: PanelRef,
    edge: HandleEdge,
    thickness: f32,
    grip: bool,
}

impl ResizeHandle {
    pub fn new(panel: PanelRef, edge: HandleEdge) -> Self {
        Self {
            panel,
            edge,
            thickness: 6.,
            grip: false,
        }
    }

    /// How far into the card the handle reaches (6px; corners twice that).
    pub fn thickness(mut self, thickness: f32) -> Self {
        self.thickness = thickness;
        self
    }

    /// Draw a small corner mark so the handle can be seen (corners only).
    pub fn grip(mut self, grip: bool) -> Self {
        self.grip = grip;
        self
    }
}

impl RenderOnce for ResizeHandle {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let edge = self.edge;
        let t = px(self.thickness);
        let corner = px(self.thickness * 2.);
        let mark = cx.theme().muted_foreground;
        let panel = self.panel;
        let mut handle = div().absolute().cursor(edge.cursor());
        handle = if edge.is_corner() {
            handle.size(corner)
        } else if edge.left() || edge.right() {
            handle.w(t).top(corner).bottom(corner)
        } else {
            handle.h(t).left(corner).right(corner)
        };
        handle = if edge.left() { handle.left_0() } else { handle };
        handle = if edge.right() { handle.right_0() } else { handle };
        handle = if edge.top() { handle.top_0() } else { handle };
        handle = if edge.bottom() { handle.bottom_0() } else { handle };
        handle
            .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                cx.stop_propagation();
                let id = panel.id.clone();
                panel.state.update(cx, |state, cx| {
                    state.begin_resize(&id, edge, event.position, cx)
                });
            })
            .when(self.grip && edge.is_corner(), |this| {
                let mut glyph = div().absolute().size(px(8.)).border_color(mark);
                glyph = if edge.left() { glyph.left(px(3.)).border_l_2() } else { glyph.right(px(3.)).border_r_2() };
                glyph = if edge.top() { glyph.top(px(3.)).border_t_2() } else { glyph.bottom(px(3.)).border_b_2() };
                this.child(glyph)
            })
    }
}

impl gpui::InteractiveElement for DragHandle {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.base.interactivity()
    }
}

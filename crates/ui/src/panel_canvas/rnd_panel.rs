//! The RnD (resize and drag) panel: a card on a [`super::PanelCanvas`].

use std::{cell::RefCell, rc::Rc};

use gpui::{
    div, prelude::FluentBuilder as _, px, size, AnyElement, App, Div, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement, Pixels, Point, RenderOnce, SharedString, Size,
    StyleRefinement, Styled, Window,
};

use super::{
    handles::{HandleEdge, ResizeHandle},
    state::{Geometry, PanelRef, Placement},
};

/// A card that can be moved and resized on a [`super::PanelCanvas`].
///
/// Put [`super::DragHandle`]s and [`super::ResizeHandle`]s anywhere among its
/// children (or let [`Self::resizable`] add resize handles). It is a positioned
/// `div` you can style; it nests, since its content can hold another canvas
/// with its own cards.
///
/// It goes where its canvas puts it: its default slot or position until the
/// user moves it, then wherever they dropped it. It is as large as its content
/// until it is resized (or given [`Self::default_size`]).
#[derive(IntoElement)]
pub struct RndPanel {
    pub(crate) panel: PanelRef,
    pub(crate) default_placement: Option<Placement>,
    pub(crate) default_size: Option<Size<Pixels>>,
    pub(crate) min_size: Size<Pixels>,
    pub(crate) max_size: Size<Pixels>,
    resize_edges: Vec<HandleEdge>,
    /// Set by the canvas: the size to draw at and where to record bounds.
    pub(crate) applied_size: Option<Size<Pixels>>,
    pub(crate) geometry: Option<Rc<RefCell<Geometry>>>,
    /// Set by the canvas for a card in a shrinking slot: the most it may be.
    pub(crate) shrink_to: Option<Size<Pixels>>,
    base: Div,
}

impl RndPanel {
    pub fn new(panel: PanelRef) -> Self {
        Self {
            panel,
            default_placement: None,
            default_size: None,
            min_size: size(px(48.), px(32.)),
            max_size: size(px(f32::MAX), px(f32::MAX)),
            resize_edges: Vec::new(),
            applied_size: None,
            geometry: None,
            shrink_to: None,
            base: div(),
        }
    }

    pub fn id(&self) -> &SharedString {
        &self.panel.id
    }

    /// The slot the card sits in until the user moves it.
    pub fn default_slot(mut self, slot: impl Into<SharedString>) -> Self {
        self.default_placement = Some(Placement::slot(slot));
        self
    }

    /// Where the card sits (relative to the canvas) until the user moves it.
    pub fn default_position(mut self, position: Point<Pixels>) -> Self {
        self.default_placement = Some(Placement::Free(position));
        self
    }

    /// The card's size until the user resizes it. Without one it is as large
    /// as its content.
    pub fn default_size(mut self, size: Size<Pixels>) -> Self {
        self.default_size = Some(size);
        self
    }

    pub fn min_size(mut self, size: Size<Pixels>) -> Self {
        self.min_size = size;
        self
    }

    pub fn max_size(mut self, size: Size<Pixels>) -> Self {
        self.max_size = size;
        self
    }

    /// Add resize handles on these edges and corners (corners show a grip).
    pub fn resizable(mut self, edges: impl IntoIterator<Item = HandleEdge>) -> Self {
        self.resize_edges.extend(edges);
        self
    }
}

impl ParentElement for RndPanel {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.base.extend(elements);
    }
}

impl Styled for RndPanel {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl RenderOnce for RndPanel {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let id = self.panel.id.clone();
        let geometry = self.geometry;
        let panel = self.panel;
        // Edges before corners, so the corners are on top.
        let mut edges = self.resize_edges;
        edges.sort_by_key(|edge| edge.is_corner());
        edges.dedup();
        self.base
            .relative()
            .when_some(self.applied_size, |this, size| this.w(size.width).h(size.height))
            .map(|this| match self.shrink_to {
                // In a shrinking slot: never past the slot, sharing it.
                Some(limit) => this
                    .max_w(limit.width)
                    .max_h(limit.height)
                    .min_w(px(0.))
                    .min_h(px(0.))
                    .flex_shrink()
                    .overflow_hidden(),
                None => this.flex_shrink_0(),
            })
            .when_some(geometry, |this, geometry| {
                this.on_frame(move |frame, _, _| {
                    if let Some(entry) = geometry.borrow_mut().panels.get_mut(&id) {
                        entry.bounds = Some(frame.bounds);
                    }
                })
            })
            // A card is a surface: presses on it are its own, not the
            // canvas's or whatever is under it.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .children(edges.into_iter().map(move |edge| {
                ResizeHandle::new(panel.clone(), edge).grip(edge.is_corner())
            }))
    }
}

impl gpui::InteractiveElement for RndPanel {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.base.interactivity()
    }
}

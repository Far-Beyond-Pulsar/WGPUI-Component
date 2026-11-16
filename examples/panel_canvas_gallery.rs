//! Example gallery: Panel Canvas, drag handles, resize handles and RnD panels.
//!
//! Each tab exercises the components the way a real app would:
//!
//! - **Slots**: every slot anchor, rows and columns of cards in one slot, and
//!   slots added and removed at runtime.
//! - **Free area**: a canvas with no slots. Cards go anywhere, are clipped by
//!   the area and cannot leave it; one card resizes from every edge.
//! - **Nested**: an IDE-like shell. Its resizable workspace card holds a
//!   canvas of its own, whose cards hold a free canvas of their own, three
//!   levels deep.
//! - **Viewport HUD**: the engine viewport's overlay layout over a fake scene:
//!   toolbars, a stats row, a resizable warnings list.
//! - **Dashboard**: slots only (free placement off). Widgets reorder between
//!   slots and resize inside them.
//!
//! The status bar shows the card being moved or resized on each canvas, so
//! you can see the interaction state while you drag.
//!
//! Run with:
//!   cargo run -p ui --features flamegraph --example panel_canvas_gallery

use gpui::*;
use gpui::prelude::FluentBuilder as _;
use ui::button::{Button, ButtonVariants as _};
use ui::panel_canvas::{
    DragHandle, HandleEdge, PanelCanvas, PanelCanvasState, PanelRef, RndPanel, Slot,
    SlotAnchor, SlotFit, SlotFlow, SlotGroup,
};
use ui::{h_flex, v_flex, ActiveTheme, Root, Sizable as _};

const TABS: [&str; 5] = ["Slots", "Free area", "Nested", "Viewport HUD", "Dashboard"];

/// The HUD tab's bottom overlays: id, title, width, home slot. The render
/// performance panels fill the bottom-left strip rightwards; the others the
/// bottom-right strip leftwards.
const HUD_BOTTOM: [(&str, &str, f32, &str); 5] = [
    ("perf", "Performance", 190., "bl-0"),
    ("pipeline", "Pipeline", 170., "bl-1"),
    ("stats", "Render stats", 210., "bl-2"),
    ("nerds", "Nerds", 220., "br-0"),
    ("net", "Network", 160., "br-1"),
];

struct Gallery {
    tab: usize,
    slots: Entity<PanelCanvasState>,
    extra_slots: usize,
    free: Entity<PanelCanvasState>,
    clipped: Entity<PanelCanvasState>,
    shell: Entity<PanelCanvasState>,
    workspace: Entity<PanelCanvasState>,
    scratch: Entity<PanelCanvasState>,
    hud: Entity<PanelCanvasState>,
    /// Which of the HUD's bottom overlays are on (the toggles above it).
    hud_shown: [bool; 5],
    dashboard: Entity<PanelCanvasState>,
}

fn sz(w: f32, h: f32) -> Size<Pixels> {
    size(px(w), px(h))
}

impl Gallery {
    fn new(cx: &mut Context<Self>) -> Self {
        let slots = cx.new(|_| {
            PanelCanvasState::new().with_slots([
                Slot::new("top-left", SlotAnchor::TopLeft, sz(220., 120.)).inset(px(12.), px(12.)),
                Slot::new("top", SlotAnchor::Top, sz(260., 80.)).inset(px(0.), px(12.)),
                Slot::new("top-right", SlotAnchor::TopRight, sz(220., 120.))
                    .inset(px(12.), px(12.))
                    .flow(SlotFlow::Column),
                Slot::new("left", SlotAnchor::Left, sz(180., 160.)).inset(px(12.), px(0.)),
                Slot::new("right", SlotAnchor::Right, sz(180., 160.)).inset(px(12.), px(0.)),
                Slot::new("bottom-left", SlotAnchor::BottomLeft, sz(320., 120.)).inset(px(12.), px(12.)),
                Slot::new("bottom", SlotAnchor::Bottom, sz(260., 80.)).inset(px(0.), px(12.)),
                Slot::new("bottom-right", SlotAnchor::BottomRight, sz(220., 160.))
                    .inset(px(12.), px(12.))
                    .flow(SlotFlow::Column),
            ])
        });
        let free = cx.new(|_| PanelCanvasState::new());
        let clipped = cx.new(|_| PanelCanvasState::new());
        let shell = cx.new(|_| {
            PanelCanvasState::new().with_slots([
                Slot::new("toolbar", SlotAnchor::Top, sz(520., 44.)).inset(px(0.), px(8.)),
                Slot::new("side", SlotAnchor::TopLeft, sz(200., 300.))
                    .inset(px(8.), px(60.))
                    .flow(SlotFlow::Column),
                Slot::new("main", SlotAnchor::TopRight, sz(560., 420.)).inset(px(8.), px(60.)),
                Slot::new("status", SlotAnchor::BottomLeft, sz(400., 32.)).inset(px(8.), px(8.)),
            ])
        });
        let workspace = cx.new(|_| {
            PanelCanvasState::new().with_slots([
                Slot::new("editor", SlotAnchor::TopLeft, sz(300., 200.)).inset(px(8.), px(36.)),
                Slot::new("inspector", SlotAnchor::TopRight, sz(160., 200.))
                    .inset(px(8.), px(36.))
                    .flow(SlotFlow::Column),
                Slot::new("console", SlotAnchor::Bottom, sz(400., 90.)).inset(px(0.), px(8.)),
            ])
        });
        let scratch = cx.new(|_| PanelCanvasState::new());
        // The engine viewport's layout: a small slot per toolbar (full once
        // it holds one), columns under them, and a bottom strip packed to the
        // right whose slots grow with their overlays and push the others left.
        let hud = cx.new(|_| {
            let mut slots = vec![
                Slot::new("toolbar-left", SlotAnchor::TopLeft, sz(160., 40.)).inset(px(8.), px(8.)),
                Slot::new("toolbar-right", SlotAnchor::TopRight, sz(160., 40.)).inset(px(8.), px(8.)),
                Slot::new("left", SlotAnchor::TopLeft, sz(320., 240.))
                    .inset(px(8.), px(60.))
                    .flow(SlotFlow::Column),
                Slot::new("right", SlotAnchor::TopRight, sz(320., 240.))
                    .inset(px(8.), px(60.))
                    .flow(SlotFlow::Column),
            ];
            // Two strips of one slot size: bottom-left fills rightwards,
            // bottom-right leftwards (a group orders slots from its anchor).
            let bottom = sz(200., 120.);
            slots.extend((0..4).map(|n| Slot::new(format!("bl-{n}"), SlotAnchor::BottomLeft, bottom).group("bottom-left")));
            slots.extend((0..3).map(|n| Slot::new(format!("br-{n}"), SlotAnchor::BottomRight, bottom).group("bottom-right")));
            PanelCanvasState::new()
                .with_groups([
                    SlotGroup::new("bottom-left", SlotAnchor::BottomLeft).inset(px(8.), px(8.)),
                    SlotGroup::new("bottom-right", SlotAnchor::BottomRight).inset(px(8.), px(8.)),
                ])
                .with_slots(slots)
        });
        let dashboard = cx.new(|_| {
            let cell = sz(260., 170.);
            let mut slots = Vec::new();
            for row in 0..2 {
                for col in 0..3 {
                    slots.push(
                        // Shrinking cells: a widget never grows past its cell,
                        // and two in one cell share it.
                        Slot::new(format!("cell-{row}-{col}"), SlotAnchor::TopLeft, cell)
                            .inset(px(16. + col as f32 * 276.), px(16. + row as f32 * 186.))
                            .fit(SlotFit::Shrink),
                    );
                }
            }
            PanelCanvasState::new().with_slots(slots).allow_free(false)
        });
        for state in [&slots, &free, &clipped, &shell, &workspace, &scratch, &hud, &dashboard] {
            cx.observe(state, |_, _, cx| cx.notify()).detach();
        }
        Self {
            tab: 0,
            slots,
            extra_slots: 0,
            free,
            clipped,
            shell,
            workspace,
            scratch,
            hud,
            hud_shown: [true, true, false, true, false],
            dashboard,
        }
    }
}

// ── Building blocks ────────────────────────────────────────────────────────

/// A card with a title bar that is its drag handle.
fn titled_card(panel: PanelRef, title: impl Into<SharedString>, body: impl IntoElement, cx: &App) -> RndPanel {
    let theme = cx.theme();
    RndPanel::new(panel.clone())
        .bg(theme.background.opacity(0.92))
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius)
        .shadow_lg()
        .child(
            v_flex()
                .size_full()
                .child(
                    DragHandle::new(panel)
                        .grip(Axis::Horizontal)
                        .w_full()
                        .h(px(24.))
                        .px_2()
                        .gap_2()
                        .justify_start()
                        .border_b_1()
                        .border_color(theme.border)
                        .bg(theme.secondary)
                        .rounded_t(theme.radius)
                        .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).child(title.into())),
                )
                .child(div().flex_1().min_h_0().p_2().text_xs().child(body)),
        )
}

/// A toolbar card: a vertical grip strip on one side and buttons.
fn toolbar(panel: PanelRef, id: &str, labels: &[&'static str], cx: &App) -> RndPanel {
    let theme = cx.theme();
    RndPanel::new(panel.clone())
        .h(px(40.))
        .bg(theme.background.opacity(0.92))
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius)
        .child(
            h_flex()
                .h_full()
                .child(
                    DragHandle::new(panel)
                        .w(px(12.))
                        .h_full()
                        .border_r_1()
                        .border_color(theme.border)
                        .hover(|s| s.bg(theme.secondary)),
                )
                .child(h_flex().px_1().gap_1().children(labels.iter().enumerate().map(|(ix, label)| {
                    Button::new(SharedString::from(format!("{id}-{ix}"))).label(*label).ghost().small()
                }))),
        )
}

/// A plain coloured tile with a grip in its corner (the whole body is not
/// the handle, so its content stays clickable).
fn tile(panel: PanelRef, label: impl Into<SharedString>, color: Hsla, cx: &App) -> RndPanel {
    let theme = cx.theme();
    RndPanel::new(panel.clone())
        .bg(color.opacity(0.85))
        .rounded(theme.radius)
        .border_1()
        .border_color(color.opacity(1.))
        .shadow_md()
        .child(
            h_flex()
                .size_full()
                .p_1()
                .gap_1()
                .items_start()
                .child(DragHandle::new(panel).size(px(18.)).rounded(px(4.)).bg(black().opacity(0.25)))
                .child(div().text_xs().text_color(white()).child(label.into())),
        )
}

fn lines(n: usize, what: &str) -> impl IntoElement {
    v_flex().gap_0p5().children((0..n).map(|i| div().child(format!("{what} {i}"))))
}

fn hue(h: f32) -> Hsla {
    hsla(h, 0.55, 0.45, 1.)
}

/// The interaction readout for a canvas.
fn status(label: &str, state: &Entity<PanelCanvasState>, cx: &App) -> String {
    match state.read(cx).active_panel() {
        Some(panel) => format!("{label}: {panel}"),
        None => format!("{label}: idle"),
    }
}

// ── Tabs ───────────────────────────────────────────────────────────────────

impl Gallery {
    fn slots_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let s = &self.slots;
        let p = |id: &str| PanelRef::new(s, id);
        let mut canvas = PanelCanvas::new("slots", s)
            .size_full()
            .background(cx.theme().muted.opacity(0.3))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Drag a card by its handle: the slot under the pointer lights up; drop to snap."),
            )
            .panel(toolbar(p("slot-tools"), "slot-tools", &["Add slot", "Remove slot"], cx).default_slot("top"))
            .panel(titled_card(p("a"), "Top left", lines(3, "row"), cx).default_slot("top-left").default_size(sz(200., 110.)))
            .panel(titled_card(p("b"), "Stack 1", "Column slot", cx).default_slot("top-right").default_size(sz(200., 60.)))
            .panel(titled_card(p("c"), "Stack 2", "Column slot", cx).default_slot("top-right").default_size(sz(200., 60.)))
            .panel(tile(p("d"), "Left", hue(0.6), cx).default_slot("left").default_size(sz(120., 80.)))
            .panel(tile(p("e"), "Right", hue(0.3), cx).default_slot("right").default_size(sz(120., 80.)))
            .panel(tile(p("f"), "Row 1", hue(0.0), cx).default_slot("bottom-left").default_size(sz(90., 70.)))
            .panel(tile(p("g"), "Row 2", hue(0.1), cx).default_slot("bottom-left").default_size(sz(90., 70.)))
            .panel(tile(p("h"), "Row 3", hue(0.15), cx).default_slot("bottom-left").default_size(sz(90., 70.)))
            .panel(
                titled_card(p("i"), "Resizable", "Grab the top-left corner or edges", cx)
                    .default_slot("bottom-right")
                    .default_size(sz(200., 120.))
                    .min_size(sz(140., 70.))
                    .resizable([HandleEdge::Top, HandleEdge::Left, HandleEdge::TopLeft]),
            );
        // Slots added at runtime get a card each.
        for n in 0..self.extra_slots {
            canvas = canvas.panel(tile(p(&format!("extra-{n}")), format!("Extra {n}"), hue(0.8), cx).default_slot(format!("extra-{n}")).default_size(sz(110., 50.)));
        }
        let state = self.slots.clone();
        let add = cx.listener(move |this, _: &ClickEvent, _, cx| {
            let n = this.extra_slots;
            this.extra_slots += 1;
            this.slots.update(cx, |s, cx| {
                s.add_slot(
                    Slot::new(format!("extra-{n}"), SlotAnchor::Left, sz(130., 60.))
                        .inset(px(220.), px(-120. + 70. * n as f32)),
                    cx,
                )
            });
        });
        let remove = cx.listener(move |this, _: &ClickEvent, _, cx| {
            if this.extra_slots == 0 {
                return;
            }
            this.extra_slots -= 1;
            let n = this.extra_slots;
            this.slots.update(cx, |s, cx| s.remove_slot(&format!("extra-{n}"), cx));
        });
        let _ = state;
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .p_2()
                    .gap_2()
                    .child(Button::new("add-slot").label("Add a slot (and a card)").small().on_click(add))
                    .child(Button::new("remove-slot").label("Remove the last slot").small().on_click(remove))
                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                        "A removed slot's cards stay where they were, placed freely.",
                    )),
            )
            .child(div().flex_1().min_h_0().child(canvas))
            .into_any_element()
    }

    fn free_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let f = &self.free;
        let c = &self.clipped;
        let theme = cx.theme();
        let free_canvas = PanelCanvas::new("free", f)
            .size_full()
            .background(theme.muted.opacity(0.3))
            .panel(tile(PanelRef::new(f, "one"), "Drag me anywhere", hue(0.55), cx).default_position(point(px(20.), px(20.))).default_size(sz(160., 60.)))
            .panel(tile(PanelRef::new(f, "two"), "Try leaving the area", hue(0.05), cx).default_position(point(px(220.), px(120.))).default_size(sz(180., 60.)))
            .panel(
                titled_card(PanelRef::new(f, "all-edges"), "All edges", "Resize from any edge or corner. Keep dragging outside the card: it keeps resizing.", cx)
                    .default_position(point(px(80.), px(220.)))
                    .default_size(sz(260., 160.))
                    .min_size(sz(120., 80.))
                    .max_size(sz(700., 500.))
                    .resizable(HandleEdge::ALL),
            );
        // A small bounded area: cards bigger than it are clipped by it.
        let clipped_canvas = PanelCanvas::new("clipped", c)
            .size_full()
            .background(theme.secondary)
            .panel(tile(PanelRef::new(c, "big"), "Bigger than its area: clipped", hue(0.75), cx).default_position(point(px(30.), px(30.))).default_size(sz(320., 200.)))
            .panel(tile(PanelRef::new(c, "small"), "Small", hue(0.45), cx).default_position(point(px(10.), px(150.))).default_size(sz(80., 40.)));
        h_flex()
            .size_full()
            .p_3()
            .gap_3()
            .child(div().flex_1().h_full().border_1().border_color(theme.border).child(free_canvas))
            .child(
                v_flex()
                    .w(px(280.))
                    .gap_2()
                    .child(div().text_xs().text_color(theme.muted_foreground).child("A 260×200 area holding a 320×200 card"))
                    .child(div().w(px(260.)).h(px(200.)).border_1().border_color(theme.border).child(clipped_canvas)),
            )
            .into_any_element()
    }

    fn nested_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let shell = &self.shell;
        let ws = &self.workspace;
        let scratch = &self.scratch;
        let theme = cx.theme();

        // Level 3: a free scratchpad inside a workspace card.
        let scratch_canvas = PanelCanvas::new("scratch", scratch)
            .size_full()
            .background(theme.muted.opacity(0.4))
            .panel(tile(PanelRef::new(scratch, "note-a"), "note A", hue(0.12), cx).default_position(point(px(6.), px(6.))).default_size(sz(70., 36.)))
            .panel(tile(PanelRef::new(scratch, "note-b"), "note B", hue(0.62), cx).default_position(point(px(70.), px(40.))).default_size(sz(70., 36.)));

        // Level 2: the workspace's own slots.
        let ws_canvas = PanelCanvas::new("workspace", ws)
            .size_full()
            .child(div().absolute().top_1().left_2().text_xs().text_color(theme.muted_foreground).child("Workspace (its own canvas)"))
            .panel(
                titled_card(PanelRef::new(ws, "editor"), "Editor", lines(8, "fn line"), cx)
                    .default_slot("editor")
                    .default_size(sz(280., 180.))
                    .resizable([HandleEdge::Right, HandleEdge::Bottom, HandleEdge::BottomRight]),
            )
            .panel(titled_card(PanelRef::new(ws, "props"), "Properties", lines(3, "field"), cx).default_slot("inspector").default_size(sz(150., 90.)))
            .panel(
                titled_card(PanelRef::new(ws, "scratch"), "Scratchpad", scratch_canvas, cx)
                    .default_slot("inspector")
                    .default_size(sz(150., 110.))
                    .resizable([HandleEdge::Bottom, HandleEdge::Left, HandleEdge::BottomLeft]),
            )
            .panel(
                titled_card(PanelRef::new(ws, "console"), "Console", lines(4, "log"), cx)
                    .default_slot("console")
                    .default_size(sz(380., 80.))
                    .resizable([HandleEdge::Top]),
            );

        // Level 1: the shell.
        let shell_canvas = PanelCanvas::new("shell", shell)
            .size_full()
            .background(theme.muted.opacity(0.25))
            .panel(toolbar(PanelRef::new(shell, "menu"), "menu", &["File", "Edit", "View", "Build"], cx).default_slot("toolbar"))
            .panel(titled_card(PanelRef::new(shell, "files"), "Files", lines(6, "file"), cx).default_slot("side").default_size(sz(190., 150.)))
            .panel(titled_card(PanelRef::new(shell, "outline"), "Outline", lines(4, "symbol"), cx).default_slot("side").default_size(sz(190., 120.)))
            .panel(
                titled_card(PanelRef::new(shell, "workspace"), "Workspace", ws_canvas, cx)
                    .default_slot("main")
                    .default_size(sz(560., 420.))
                    .min_size(sz(360., 300.))
                    .resizable([HandleEdge::Left, HandleEdge::Bottom, HandleEdge::BottomLeft]),
            )
            .panel(
                RndPanel::new(PanelRef::new(shell, "status"))
                    .default_slot("status")
                    .h(px(28.))
                    .bg(theme.secondary)
                    .rounded(theme.radius)
                    .child(h_flex().h_full().gap_2().pr_2()
                        .child(DragHandle::new(PanelRef::new(shell, "status")).grip(Axis::Vertical).w(px(10.)).h_full())
                        .child(div().text_xs().child("Status bar — drag me to another slot"))),
            );
        div().size_full().p_2().child(shell_canvas).into_any_element()
    }

    fn hud_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let h = &self.hud;
        let p = |id: &str| PanelRef::new(h, id);
        let theme = cx.theme();
        let scene = linear_gradient(
            180.,
            linear_color_stop(hsla(0.58, 0.45, 0.35, 1.), 0.),
            linear_color_stop(hsla(0.28, 0.30, 0.18, 1.), 1.),
        );
        let warnings = titled_card(p("warnings"), "Warnings", lines(30, "Renderer warning"), cx)
            .default_slot("left")
            .default_size(sz(320., 220.))
            .min_size(sz(220., 120.))
            .max_size(sz(700., 520.))
            .resizable([HandleEdge::Right, HandleEdge::Bottom, HandleEdge::BottomRight]);
        let mut canvas = PanelCanvas::new("hud", h)
            .size_full()
            .background(scene)
            .panel(toolbar(p("tools"), "tools", &["Move", "Rotate", "Scale", "Snap"], cx).default_slot("toolbar-left"))
            .panel(toolbar(p("camera"), "camera", &["Lit", "Speed", "Persp"], cx).default_slot("toolbar-right"))
            .panel(warnings)
            .panel(titled_card(p("debug-a"), "Portal links", lines(3, "link"), cx).default_slot("right").default_size(sz(260., 90.)))
            .panel(titled_card(p("debug-b"), "Light probes", lines(3, "probe"), cx).default_slot("right").default_size(sz(260., 90.)));
        // The bottom strip: each overlay turned on takes its slot and pushes
        // the ones to its left further left.
        for (ix, (name, title, width, home)) in HUD_BOTTOM.into_iter().enumerate() {
            if self.hud_shown[ix] {
                canvas = canvas.panel(
                    titled_card(p(name), title, lines(4, "value"), cx)
                        .default_slot(home)
                        .default_size(sz(width, 110.)),
                );
            }
        }
        let toggles = h_flex().p_2().gap_2().children(HUD_BOTTOM.into_iter().enumerate().map(|(ix, (_, title, _, _))| {
            let on = self.hud_shown[ix];
            Button::new(("hud-toggle", ix))
                .label(title)
                .small()
                .when(on, |b| b.primary())
                .when(!on, |b| b.ghost())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.hud_shown[ix] = !this.hud_shown[ix];
                    cx.notify();
                }))
        }))
        .child(div().text_xs().text_color(theme.muted_foreground).child(
            "Toggle overlays: the bottom-left strip fills rightwards, the bottom-right leftwards; each pushes its strip along. A toolbar slot holding a toolbar is full: drag the other toolbar over it.",
        ));
        v_flex()
            .size_full()
            .child(toggles)
            .child(div().flex_1().min_h_0().p_2().child(canvas))
            .into_any_element()
    }

    fn dashboard_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let d = &self.dashboard;
        let theme = cx.theme();
        let mut canvas = PanelCanvas::new("dashboard", d).size_full().background(theme.muted.opacity(0.2));
        let widgets = [("Revenue", 0.35), ("Users", 0.55), ("Errors", 0.0), ("Latency", 0.12), ("Queue", 0.75), ("Oversized 400x300", 0.9)];
        for (ix, (name, h)) in widgets.into_iter().enumerate() {
            let (row, col) = (ix / 3, ix % 3);
            canvas = canvas.panel(
                tile(PanelRef::new(d, name), name, hue(h), cx)
                    .default_slot(format!("cell-{row}-{col}"))
                    // The oversized one asks for more than a cell; the shrinking
                    // cell holds it to the cell.
                    .default_size(if ix == 5 { sz(400., 300.) } else { sz(240., 150.) })
                    .min_size(sz(120., 80.))
                    .resizable([HandleEdge::Right, HandleEdge::Bottom, HandleEdge::BottomRight]),
            );
        }
        let reset = {
            let d = d.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                d.update(cx, |s, cx| {
                    for (name, _) in widgets {
                        s.reset(name, cx);
                    }
                })
            }
        };
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .p_2()
                    .gap_2()
                    .child(Button::new("reset-dashboard").label("Reset layout").small().on_click(reset))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        "Shrinking cells: a widget never grows past its cell (the oversized one is held to it), and two in one cell share it. Free placement is off: a widget dropped outside a cell goes back.",
                    )),
            )
            .child(div().flex_1().min_h_0().child(canvas))
            .into_any_element()
    }
}

impl Render for Gallery {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.tab {
            0 => self.slots_tab(cx),
            1 => self.free_tab(cx),
            2 => self.nested_tab(cx),
            3 => self.hud_tab(cx),
            _ => self.dashboard_tab(cx),
        };
        let readout = [
            status("slots", &self.slots, cx),
            status("free", &self.free, cx),
            status("shell", &self.shell, cx),
            status("workspace", &self.workspace, cx),
            status("scratch", &self.scratch, cx),
            status("hud", &self.hud, cx),
            status("dashboard", &self.dashboard, cx),
        ]
        .join("  ·  ");
        let theme = cx.theme();
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                h_flex()
                    .p_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .children(TABS.iter().enumerate().map(|(ix, name)| {
                        Button::new(("tab", ix))
                            .label(*name)
                            .small()
                            .when(ix == self.tab, |b| b.primary())
                            .when(ix != self.tab, |b| b.ghost())
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.tab = ix;
                                cx.notify();
                            }))
                    })),
            )
            .child(div().flex_1().min_h_0().child(body))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(readout),
            )
    }
}

fn main() {
    env_logger::init();
    Application::new().run(|cx| {
        ui::init(cx);
        ui::Theme::change(ui::ThemeMode::Dark, None, cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: size(px(1280.), px(820.)),
                })),
                titlebar: Some(TitlebarOptions {
                    title: Some("Panel Canvas Gallery".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(Gallery::new);
                cx.new(|cx| Root::new(view.into(), window, cx))
            },
        )
        .unwrap();
        cx.activate(true);
    });
}

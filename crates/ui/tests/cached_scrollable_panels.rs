//! The level editor's World Settings and Properties panels blank out for
//! seconds after interacting with any other UI in the editor workspace,
//! while the hierarchy and viewport next to them never do. This drives the
//! real components -- `.scrollable()`, `NumberInput`, `Button` -- in the same
//! arrangement: a cached workspace whose sibling panels are cached layers of
//! their own, under a title bar that sits outside it.
//!
//! One window is driven like the editor and rendered by a long-lived GPU
//! renderer; an identical one is fully re-rendered after every step. Every
//! frame the first presents must match the second, including idle viewport
//! frames that re-present the last scene. Skips without a GPU adapter.

use gpui::headless::{HeadlessWindow, compare_frames};
use gpui::{
    AnyView, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, StyleRefinement, Styled as _, TestAppContext, Window,
    WindowHandle, div, point, px, size,
};
use ui::{
    ActiveTheme as _, StyledExt as _,
    button::Button,
    h_flex,
    input::{InputState, NumberInput},
    scroll::ScrollbarAxis,
    v_flex,
};

const SECTIONS: usize = 4;
const ROWS: usize = 4;

struct Toolbar;
impl Render for Toolbar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .h(px(40.))
            .w_full()
            .gap_2()
            .px_2()
            .bg(cx.theme().title_bar)
            .children((0..3usize).map(|index| {
                div()
                    .debug_selector(move || format!("toolbar-{index}"))
                    .child(Button::new(("toolbar", index)).label(format!("Tool {index}")).on_click(|_, _, _| {}))
            }))
    }
}

struct Hierarchy;
impl Render for Hierarchy {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .p_2()
            .gap_1()
            .bg(cx.theme().background)
            .children((0..10).map(|index| {
                div()
                    .debug_selector(move || format!("hierarchy-{index}"))
                    .child(format!("Entity {index}"))
            }))
    }
}

/// Built like World Settings: header, then a scrollable body of sections of
/// labelled number inputs.
struct Properties {
    inputs: Vec<Entity<InputState>>,
}
impl Render for Properties {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .h(px(28.))
                    .px_2()
                    .debug_selector(|| "properties-header".to_string())
                    .child("Properties"),
            )
            .child(
                div().flex_1().overflow_hidden().child(
                    div().size_full().scrollable(ScrollbarAxis::Vertical).child(
                        v_flex().w_full().p_3().gap_4().children((0..SECTIONS).map(|section| {
                            v_flex()
                                .gap_2()
                                .child(format!("Section {section}"))
                                .children((0..ROWS).map(|row| {
                                    let index = section * ROWS + row;
                                    h_flex()
                                        .gap_2()
                                        .child(div().w(px(90.)).child(format!("Field {index}")))
                                        .child(
                                            div()
                                                .w(px(140.))
                                                .debug_selector(move || format!("input-{index}"))
                                                .child(NumberInput::new(&self.inputs[index])),
                                        )
                                }))
                        })),
                    ),
                ),
            )
    }
}

/// The `LevelEditorPanel` stand-in: a cached view whose panels are cached
/// sibling layers.
struct Workspace {
    toolbar: Entity<Toolbar>,
    hierarchy: Entity<Hierarchy>,
    properties: Entity<Properties>,
}
impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let fill = || StyleRefinement::default().size_full();
        h_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(
                v_flex()
                    .w(px(360.))
                    .h_full()
                    .child(div().h(px(40.)).w_full().child(AnyView::from(self.toolbar.clone()).cached(fill())))
                    .child(div().flex_1().w_full().child(AnyView::from(self.hierarchy.clone()).cached(fill()))),
            )
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .child(AnyView::from(self.properties.clone()).cached(fill())),
            )
    }
}

/// The application window: a title bar outside the workspace's layer.
/// Wrapped in `ui::Root`, as every engine window is: components reach the
/// window root through an unchecked cast, so anything else is undefined.
struct AppWindow {
    workspace: Entity<Workspace>,
}
impl Render for AppWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .h(px(32.))
                    .w_full()
                    .gap_2()
                    .px_2()
                    .bg(cx.theme().title_bar)
                    .child(
                        div()
                            .debug_selector(|| "titlebar-file".to_string())
                            .child(Button::new("file").label("File").on_click(|_, _, _| {})),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .child(AnyView::from(self.workspace.clone()).cached(StyleRefinement::default().size_full())),
            )
    }
}

fn open_editor(cx: &mut TestAppContext) -> WindowHandle<ui::Root> {
    let root = cx.open_window(size(px(800.), px(480.)), |window, cx| {
        let inputs = (0..SECTIONS * ROWS)
            .map(|index| {
                cx.new(|cx| InputState::new(window, cx).default_value(format!("{}.5", index)))
            })
            .collect();
        let workspace = cx.new(|cx| Workspace {
            toolbar: cx.new(|_| Toolbar),
            hierarchy: cx.new(|_| Hierarchy),
            properties: cx.new(|_| Properties { inputs }),
        });
        let app = cx.new(|_| AppWindow { workspace });
        ui::Root::new(app.into(), window, cx)
    });
    cx.run_until_parked();
    root
}

#[derive(Clone, Copy)]
enum Action {
    Hover,
    Click,
}

#[test]
fn scrollable_panels_keep_their_content_when_sibling_layers_update() {
    eprintln!("TEMP-STEP 0");
    let mut cx = TestAppContext::with_real_text_system();
    eprintln!("TEMP-STEP 1");
    cx.update(ui::init);
    eprintln!("TEMP-STEP 2");
    let live_handle = open_editor(&mut cx);
    let truth_handle = open_editor(&mut cx);
    eprintln!("TEMP-STEP 3");
    let Some(live) = HeadlessWindow::attach(live_handle, &mut cx) else {
        return;
    };
    let truth = HeadlessWindow::attach(truth_handle, &mut cx).expect("second headless window");
    eprintln!("TEMP-STEP 4");
    let (width, height) = live.size();
    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");

    let targets = [
        "toolbar-0",
        "input-1",
        "toolbar-1",
        "hierarchy-3",
        "input-6",
        "titlebar-file",
        "toolbar-2",
        "properties-header",
        "input-13",
    ];
    let mut failures = Vec::new();
    for round in 0..2 {
        for target in targets {
            for action in [Action::Hover, Action::Click] {
                let label = format!(
                    "round {round}: {} {target}",
                    match action {
                        Action::Hover => "hover",
                        Action::Click => "click",
                    }
                );
                eprintln!("TEMP-STEP {label}: full draw");
                // Positions come from a full render, where every element paints.
                truth.draw(&mut cx, true);
                let Some(bounds) = truth.element_bounds(&mut cx, target) else {
                    panic!("{target} was not painted");
                };
                let at = point(
                    bounds.origin.x + bounds.size.width / 2.,
                    bounds.origin.y + bounds.size.height / 2.,
                );
                for window in [&live, &truth] {
                    eprintln!("TEMP-STEP {label}: input");
                    match action {
                        Action::Hover => window.mouse_move(&mut cx, at),
                        Action::Click => window.click(&mut cx, at),
                    }
                }
                eprintln!("TEMP-STEP {label}: after input");
                // The interaction's own frame, as the frame loop draws it.
                live.draw(&mut cx, false);
                truth.draw(&mut cx, true);
                eprintln!("TEMP-STEP {label}: drawn");
                let truth_pixels = truth.presented();
                let check = |frame: Vec<u8>, when: &str| {
                    compare_frames(&frame, &truth_pixels, width, height, &format!("{label}, {when}"), &output)
                };
                failures.extend(check(live.presented(), "interaction frame").err());
                if let Err(error) = live.settle(&mut cx) {
                    failures.push(format!("{label}: {error}"));
                }
                failures.extend(check(live.presented(), "after the renderer's wake-ups").err());
                // Idle viewport frames re-present the last scene.
                for index in 0..3 {
                    failures.extend(check(live.idle_frame(&mut cx), &format!("idle frame {index}")).err());
                }
                if failures.len() >= 5 {
                    panic!("{}", failures.join("\n"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

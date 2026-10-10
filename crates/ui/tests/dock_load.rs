//! A split adds each child once, and a layout restored with
//! `DockArea::load` reports its changes like one built in code.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    div, px, size, App, AppContext as _, Axis, Context, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, Render, Styled as _, TestAppContext, VisualTestContext, Window,
};
use ui::dock::{
    register_panel, DockArea, DockEvent, DockItem, Panel, PanelEvent, PanelState, PanelView,
};

struct Page {
    name: &'static str,
    focus: FocusHandle,
}

impl EventEmitter<PanelEvent> for Page {}

impl Focusable for Page {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Panel for Page {
    fn panel_name(&self) -> &'static str {
        self.name
    }
}

impl Render for Page {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full()
    }
}

const NAMES: [&str; 3] = ["first", "second", "third"];

fn page(name: &'static str, cx: &mut App) -> Arc<dyn PanelView> {
    Arc::new(cx.new(|cx| Page {
        name,
        focus: cx.focus_handle(),
    }))
}

/// A window holding one dock area. Its centre is two tab groups side by side,
/// ["first", "second"] and ["third"], or with `split` false the first group
/// alone.
fn open(cx: &mut TestAppContext, split: bool) -> (Entity<DockArea>, VisualTestContext) {
    cx.update(|cx| {
        ui::init(cx);
        for name in NAMES {
            register_panel(cx, name, move |_, _, _, _, cx| {
                Box::new(cx.new(|cx| Page {
                    name,
                    focus: cx.focus_handle(),
                }))
            });
        }
    });
    let mut dock = None;
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |window, cx| {
            window.resize(size(px(800.), px(600.)));
            let area = cx.new(|cx| DockArea::new("test-dock", None, window, cx));
            let weak = area.downgrade();
            let left = DockItem::tabs(
                vec![page("first", cx), page("second", cx)],
                Some(0),
                &weak,
                window,
                cx,
            );
            let center = if split {
                let right = DockItem::tabs(vec![page("third", cx)], Some(0), &weak, window, cx);
                DockItem::split_with_sizes(
                    Axis::Horizontal,
                    vec![left, right],
                    vec![Some(px(300.)), None],
                    &weak,
                    window,
                    cx,
                )
            } else {
                left
            };
            area.update(cx, |area, cx| area.set_center(center, window, cx));
            dock = Some(area.clone());
            area
        })
        .expect("window")
    });
    cx.run_until_parked();
    (
        dock.expect("dock area"),
        VisualTestContext::from_window(window.into(), cx),
    )
}

fn names(state: &PanelState) -> Vec<String> {
    if state.children.is_empty() {
        return vec![state.panel_name.clone()];
    }
    state.children.iter().flat_map(names).collect()
}

#[gpui::test]
fn a_split_adds_each_child_once(cx: &mut TestAppContext) {
    let (dock, mut cx) = open(cx, true);
    let center = cx.update(|_, cx| dock.read(cx).dump(cx).center);
    assert_eq!(center.children.len(), 2, "one stack entry per item");
    assert_eq!(names(&center), NAMES);
}

/// The centre is a bare tab group: nothing but the dock area itself
/// subscribes to it (a group inside a split reports through the split).
#[gpui::test]
fn a_loaded_layout_reports_its_changes(cx: &mut TestAppContext) {
    let (dock, mut cx) = open(cx, false);
    let state = cx.update(|_, cx| dock.read(cx).dump(cx));

    cx.update(|window, cx| DockArea::load(&dock, state, window, cx))
        .expect("load");
    cx.run_until_parked();
    let restored = cx.update(|_, cx| dock.read(cx).dump(cx).center);
    assert_eq!(names(&restored), NAMES[..2], "the same layout came back");

    let changes = Rc::new(Cell::new(0));
    let _watch = cx.update({
        let changes = changes.clone();
        |_, cx| {
            cx.subscribe(&dock, move |_, event: &DockEvent, _| {
                if matches!(event, DockEvent::LayoutChanged) {
                    changes.set(changes.get() + 1);
                }
            })
        }
    });

    // Switch tabs in the loaded group.
    let tabs = cx.update(|_, cx| match dock.read(cx).items() {
        DockItem::Tabs { view, .. } => view.clone(),
        _ => panic!("the centre is a tab group"),
    });
    tabs.update_in(&mut cx, |tabs, window, cx| tabs.set_active_tab(1, window, cx));
    cx.run_until_parked();
    assert!(changes.get() > 0, "the loaded layout's change reached the dock area");
}

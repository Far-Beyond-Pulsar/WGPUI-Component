//! A `TabPanel` with its tab strip hidden draws only the active panel, flush
//! with its top edge, and still switches tabs.

use std::sync::Arc;

use gpui::{
    div, px, size, App, AppContext as _, Bounds, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _,
    TestAppContext, VisualTestContext, Window,
};
use ui::dock::{DockArea, DockItem, Panel, PanelEvent, PanelView, TabPanel};

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
        let name = self.name;
        div().size_full().debug_selector(move || name.into())
    }
}

struct Host {
    dock: Entity<DockArea>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .debug_selector(|| "host".into())
            .child(self.dock.clone())
    }
}

struct Fixture {
    tabs: Entity<TabPanel>,
    pages: Vec<Entity<Page>>,
    cx: VisualTestContext,
}

fn open(cx: &mut TestAppContext) -> Fixture {
    cx.update(ui::init);
    let mut tabs = None;
    let mut pages = Vec::new();
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |window, cx| {
            window.resize(size(px(800.), px(600.)));
            let dock = cx.new(|cx| DockArea::new("test-dock", None, window, cx));
            let weak = dock.downgrade();
            for name in ["first", "second"] {
                pages.push(cx.new(|cx| Page {
                    name,
                    focus: cx.focus_handle(),
                }));
            }
            let views: Vec<Arc<dyn PanelView>> = pages
                .iter()
                .map(|page| Arc::new(page.clone()) as Arc<dyn PanelView>)
                .collect();
            let item = DockItem::tabs(views, Some(0), &weak, window, cx);
            if let DockItem::Tabs { view, .. } = &item {
                tabs = Some(view.clone());
            }
            dock.update(cx, |dock, cx| dock.set_center(item, window, cx));
            cx.new(|_| Host { dock })
        })
        .expect("window")
    });
    cx.run_until_parked();
    Fixture {
        tabs: tabs.expect("center tabs"),
        pages,
        cx: VisualTestContext::from_window(window.into(), cx),
    }
}

/// Draw until the layout settles: the tab strip sizes itself from the frame
/// before, so showing it takes a second frame. The tab panel caches its active
/// panel's view, which then records no fresh debug bounds, so the pages are
/// marked changed before each frame.
fn draw(f: &mut Fixture) {
    for _ in 0..2 {
        for page in &f.pages {
            page.update(&mut f.cx, |_, cx| cx.notify());
        }
        f.cx.update(|window, cx| window.draw(cx).clear());
    }
}

fn bounds(cx: &mut VisualTestContext, selector: &'static str) -> Option<Bounds<gpui::Pixels>> {
    cx.debug_bounds(selector)
}

#[gpui::test]
fn a_hidden_tab_strip_leaves_only_the_active_panel(cx: &mut TestAppContext) {
    let mut f = open(cx);
    let tabs = f.tabs.clone();

    draw(&mut f);
    let host = bounds(&mut f.cx, "host").expect("host");
    let shown = bounds(&mut f.cx, "first").expect("first page with the strip");
    assert!(
        shown.origin.y > host.origin.y,
        "with the strip, the page starts below it"
    );

    tabs.update(&mut f.cx, |tabs, cx| tabs.set_tab_bar_hidden(true, cx));
    draw(&mut f);
    let hidden = bounds(&mut f.cx, "first").expect("first page without the strip");
    assert_eq!(hidden.origin.y, host.origin.y, "the page takes the strip's place");
    assert!(hidden.size.height > shown.size.height);

    tabs.update_in(&mut f.cx, |tabs, window, cx| tabs.set_active_tab(1, window, cx));
    draw(&mut f);
    let second = bounds(&mut f.cx, "second").expect("tabs still switch");
    assert_eq!(second.origin.y, host.origin.y, "still without the strip");

    tabs.update(&mut f.cx, |tabs, cx| tabs.set_tab_bar_hidden(false, cx));
    draw(&mut f);
    let back = bounds(&mut f.cx, "second").expect("second page with the strip");
    assert_eq!(back.origin.y, shown.origin.y, "showing it again restores the layout");
}

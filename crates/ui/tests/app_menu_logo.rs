//! The app menu shown as the Pulsar logo (Pulsar-Native#1138).
//!
//! A `Button` keeps its fixed small-button height whatever its child, so the
//! 44 px logo overflowed a 24 px-tall highlight and only that strip was
//! clickable. The logo trigger is now a square that wraps the logo.

use std::sync::Arc;

use gpui::{
    div, px, AnyView, App, Context, IntoElement, Modifiers, OwnedMenu, ParentElement as _,
    Render, RenderImage, Styled as _, TestAppContext, Window,
};
use ui::menu::AppMenuBar;

/// The shell's 68 px slot that hosts the logo (`EditorWindowShell`).
const SLOT: f32 = 68.;
const LOGO: f32 = 44.;
const INSET: f32 = 4.;

struct Shell {
    app_menu: AnyView,
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(400.)).child(
            div()
                .w(px(SLOT))
                .h(px(SLOT))
                .flex()
                .items_center()
                .justify_center()
                .child(self.app_menu.clone()),
        )
    }
}

fn logo() -> Arc<RenderImage> {
    let rgba = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 255, 255]));
    Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(rgba)]))
}

#[gpui::test]
fn the_logo_trigger_wraps_the_logo_and_opens_from_its_corner(cx: &mut TestAppContext) {
    cx.update(ui::init);
    let (_, window) = cx.add_window_view(|window, cx| {
        let bar = AppMenuBar::new_with_menus(
            vec![OwnedMenu {
                name: "Pulsar".into(),
                items: vec![],
            }],
            window,
            cx,
        );
        bar.update(cx, |bar, cx| bar.set_logo(Some(logo()), cx));
        Shell {
            app_menu: bar.read(cx).app_menu_view().expect("app menu"),
        }
    });
    window.update(|window, cx: &mut App| window.draw(cx).clear());

    let trigger = window.debug_bounds("app-menu-logo").expect("logo trigger");
    let side = px(LOGO + 2. * INSET);
    assert_eq!(trigger.size.width, side);
    assert_eq!(trigger.size.height, side, "the highlight is as tall as the logo");
    let margin = px((SLOT - (LOGO + 2. * INSET)) / 2.);
    assert_eq!(trigger.origin, gpui::point(margin, margin), "centred in its slot");
    assert!(window.debug_bounds("app-menu-popup").is_none());

    // Near the logo's top-left corner: outside the old 24 px strip.
    let corner = trigger.origin + gpui::point(px(INSET + 2.), px(INSET + 2.));
    window.simulate_click(corner, Modifiers::none());
    window.update(|window, cx: &mut App| window.draw(cx).clear());
    assert!(
        window.debug_bounds("app-menu-popup").is_some(),
        "a click on the logo's corner opens the menu"
    );
}

//! Example: Button hover & active style customization
//!
//! This example demonstrates the difference between:
//! 1. **Default** hover/active — the button uses its variant's built-in hover/active styles.
//!    In `button.rs` lines 537–552, when `has_custom_hover` / `has_custom_active` are `false`,
//!    `RenderOnce` automatically wires `.hover(|this| { ... })` and `.active(|this| { ... })`
//!    with the variant's theme colors.
//! 2. **Custom** hover/active — calling `.hover(|style| ...)` / `.active(|style| ...)` sets
//!    `has_custom_hover` / `has_custom_active` to `true`, so the default logic is **skipped**
//!    and your custom styling is used instead.
//!
//! Run with:
//!   cargo run --example button_custom_hover_active

use gpui::*;
use ui::button::{Button, ButtonVariants as _};
use ui::{ActiveTheme, Root};

struct DemoView;

impl Render for DemoView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let bg_color = theme.colors.background;
        let text_color = theme.colors.foreground;
        let muted_color = theme.colors.muted_foreground;

        div()
            .size_full()
            .bg(bg_color)
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_6()
            // ──────────────────────────────────────────────────────────────────────
            // Section 1: DEFAULT hover & active (variant built-in colors)
            // These buttons do NOT call .hover() or .active(), so
            // `has_custom_hover` and `has_custom_active` are both `false`.
            // RenderOnce (button.rs:537-552) applies the variant's default styles:
            //
            //   .when(!self.has_custom_hover, |this| {
            //       this.hover(|this| {
            //           let hover_style = style.hovered(self.outline, cx);
            //           this.bg(hover_style.bg)
            //               .border_color(hover_style.border)
            //               .text_color(hover_style.fg)
            //       })
            //   })
            //   .when(!self.has_custom_active, |this| {
            //       this.active(|this| {
            //           let active_style = style.active(self.outline, cx);
            //           this.bg(active_style.bg)
            //               .border_color(active_style.border)
            //               .text_color(active_style.fg)
            //       })
            //   })
            // ──────────────────────────────────────────────────────────────────────
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_color(text_color)
                            .text_lg()
                            .font_weight(FontWeight::BOLD)
                            .child("Default hover & active (variant built-in)"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(Button::new("default-primary").primary().label("Primary"))
                            .child(Button::new("default-secondary").label("Secondary"))
                            .child(Button::new("default-danger").danger().label("Danger"))
                            .child(Button::new("default-success").success().label("Success"))
                    )
                    .child(
                        div()
                            .text_color(muted_color)
                            .text_sm()
                            .child("No custom hover/active — variant theme colors applied automatically."),
                    ),
            )
            // ──────────────────────────────────────────────────────────────────────
            // Section 2: CUSTOM hover only
            // Calling `.hover(|style| ...)` sets `has_custom_hover = true` (button.rs:426).
            // This causes `!self.has_custom_hover` to be `false`, so the default
            // hover logic in RenderOnce is SKIPPED. Only your custom style runs.
            // The active state still uses the variant's default.
            // ──────────────────────────────────────────────────────────────────────
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_color(text_color)
                            .text_lg()
                            .font_weight(FontWeight::BOLD)
                            .child("Custom hover (default hover skipped)"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(
                                Button::new("custom-hover-1")
                                    .primary()
                                    .label("Red Hover")
                                    .hover(|style| {
                                        style
                                            .bg(rgb(0xef4444))
                                            .text_color(rgb(0xffffff))
                                    }),
                            )
                            .child(
                                Button::new("custom-hover-2")
                                    .label("Violet Hover")
                                    .hover(|style| {
                                        style
                                            .bg(rgb(0x8b5cf6))
                                            .text_color(rgb(0xffffff))
                                            .border_color(rgb(0x7c3aed))
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .text_color(muted_color)
                            .text_sm()
                            .child("`.hover(|style| ...)` sets `has_custom_hover = true`, skipping the default hover."),
                    ),
            )
            // ──────────────────────────────────────────────────────────────────────
            // Section 3: CUSTOM active only
            // Calling `.active(|style| ...)` sets `has_custom_active = true` (button.rs:439).
            // The default active logic from RenderOnce is SKIPPED.
            // The hover state still uses the variant's default.
            // ──────────────────────────────────────────────────────────────────────
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_color(text_color)
                            .text_lg()
                            .font_weight(FontWeight::BOLD)
                            .child("Custom active (default active skipped)"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(
                                Button::new("custom-active-1")
                                    .primary()
                                    .label("Green Active")
                                    .active(|style| {
                                        style
                                            .bg(rgb(0x22c55e))
                                            .text_color(rgb(0xffffff))
                                    }),
                            )
                            .child(
                                Button::new("custom-active-2")
                                    .danger()
                                    .label("Orange Active")
                                    .active(|style| {
                                        style
                                            .bg(rgb(0xf97316))
                                            .text_color(rgb(0xffffff))
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .text_color(muted_color)
                            .text_sm()
                            .child("`.active(|style| ...)` sets `has_custom_active = true`, skipping the default active."),
                    ),
            )
            // ──────────────────────────────────────────────────────────────────────
            // Section 4: BOTH custom hover AND active
            // Both flags are set to `true`. Neither default runs.
            // ──────────────────────────────────────────────────────────────────────
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_color(text_color)
                            .text_lg()
                            .font_weight(FontWeight::BOLD)
                            .child("Both custom hover & active (both defaults skipped)"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(
                                Button::new("custom-both-1")
                                    .label("Pink Custom")
                                    .hover(|style| {
                                        style
                                            .bg(rgb(0xec4899))
                                            .text_color(rgb(0xffffff))
                                    })
                                    .active(|style| {
                                        style
                                            .bg(rgb(0xdb2777))
                                            .text_color(rgb(0xffffff))
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .text_color(muted_color)
                            .text_sm()
                            .child("Both `.hover(...)` and `.active(...)` — neither default runs. Hover=pink, active=darker pink."),
                    ),
            )
            // ──────────────────────────────────────────────────────────────────────
            // Section 5: Outline variant — default vs custom hover
            // Shows the contrast in outline mode.
            // ──────────────────────────────────────────────────────────────────────
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_color(text_color)
                            .text_lg()
                            .font_weight(FontWeight::BOLD)
                            .child("Outline variant: default vs custom hover"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(
                                Button::new("outline-default")
                                    .outline()
                                    .primary()
                                    .label("Default Outline"),
                            )
                            .child(
                                Button::new("outline-custom")
                                    .outline()
                                    .primary()
                                    .label("Custom Outline")
                                    .hover(|style| {
                                        let overlay_color: Hsla = rgba(0x3b82f640).into();
                                        style
                                            .bg(overlay_color)
                                            .border_color(rgb(0x60a5fa))
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .text_color(muted_color)
                            .text_sm()
                            .child("Left: default outline hover. Right: custom hover overrides the outline hover entirely."),
                    ),
            )
    }
}

fn main() {
    Application::new().run(|cx| {
        ui::init(cx);
        // Default to dark mode. Set before the window opens (Theme::change
        // takes an Option<&mut Window>, no window exists yet at this point)
        // so the first frame already renders dark instead of flashing light
        // then switching.
        ui::Theme::change(ui::ThemeMode::Dark, None, cx);

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: size(px(820.), px(740.)),
                })),
                titlebar: Some(TitlebarOptions {
                    title: Some("Button Hover/Active Demo".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|_cx| DemoView);
                let root = cx.new(|cx| Root::new(view.into(), window, cx));
                // Open with the Inspector panel already visible (ctrl-shift-i /
                // cmd-alt-i also toggles it at any time) so element/style/layout
                // state -- and the profiler tab, once `flamegraph` is enabled --
                // is available without an extra step.
                #[cfg(any(feature = "inspector", debug_assertions))]
                window.toggle_inspector(cx);
                root
            },
        )
        .unwrap();

        cx.activate(true);
    });
}

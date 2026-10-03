use super::super::*;

pub(crate) fn render_color_swatch(
    id_prefix: &'static str,
    color: Hsla,
    clickable: bool,
    state: Entity<ColorPickerState>,
    window: &mut Window,
) -> impl IntoElement {
    render_swatch(id_prefix, color, clickable, false, state, window)
}

pub(crate) fn render_cached_color_swatch(
    color: Hsla,
    state: Entity<ColorPickerState>,
    window: &mut Window,
) -> impl IntoElement {
    render_swatch("all-color", color, true, true, state, window)
}

fn render_swatch(
    id_prefix: &'static str,
    color: Hsla,
    clickable: bool,
    baked: bool,
    state: Entity<ColorPickerState>,
    window: &mut Window,
) -> impl IntoElement {
    div()
        .id(SharedString::from(format!(
            "{id_prefix}-{}",
            color.to_hex()
        )))
        .h_5()
        .w_5()
        .border_1()
        .when(!baked, |this| {
            this.bg(color).border_color(color.darken(0.1))
        })
        .when(baked, |this| this.border_color(gpui::transparent_black()))
        .when(clickable, |this| {
            this.hover(|this| {
                this.border_color(color.darken(0.3))
                    .bg(color.lighten(0.1))
                    .shadow_xs()
            })
            .active(|this| this.border_color(color.darken(0.5)).bg(color.darken(0.2)))
            .on_click(window.listener_for(&state, move |state, _, window, cx| {
                state.apply_external_color(color, true, window, cx);
            }))
        })
}

use super::*;

/// A box-model diagram with fixed visual spacing so labels remain legible even
/// when the element has zero spacing or unusually large dimensions.
pub(super) fn render(layout: &InspectorLayoutInfo, cx: &App) -> AnyElement {
    let content_width = (layout.bounds.size.width
        - layout.border.left
        - layout.border.right
        - layout.padding.left
        - layout.padding.right)
        .max(px(0.));
    let content_height = (layout.bounds.size.height
        - layout.border.top
        - layout.border.bottom
        - layout.padding.top
        - layout.padding.bottom)
        .max(px(0.));
    let content = h_flex()
        .h(px(62.))
        .w_full()
        .min_w_0()
        .items_center()
        .justify_center()
        .rounded_md()
        .bg(gpui::rgba(0x43658b99))
        .text_color(cx.theme().foreground)
        .child(format!(
            "{:.0}×{:.0}",
            content_width.value(),
            content_height.value()
        ))
        .into_any_element();
    let padding = layer(
        "padding",
        layout.padding,
        gpui::rgba(0x557d4699),
        content,
        cx,
    );
    let border = layer("border", layout.border, gpui::rgba(0x96843c99), padding, cx);
    layer("margin", layout.margin, gpui::rgba(0xa5724199), border, cx)
}

fn layer(
    label: &'static str,
    edges: gpui::EdgeWidths,
    color: gpui::Rgba,
    inner: AnyElement,
    cx: &App,
) -> AnyElement {
    let value_color = cx.theme().muted_foreground;
    v_flex()
        .w_full()
        .min_w_0()
        .px_3()
        .py_2()
        .rounded_sm()
        .border_1()
        .border_color(cx.theme().border)
        .bg(color)
        .text_xs()
        .child(
            h_flex()
                .relative()
                .h(px(20.))
                .justify_center()
                .child(
                    div()
                        .absolute()
                        .left_1()
                        .text_color(value_color)
                        .child(label),
                )
                .child(format!("{:.0}", edges.top.value())),
        )
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .w(px(22.))
                        .flex_shrink_0()
                        .text_center()
                        .child(format!("{:.0}", edges.left.value())),
                )
                .child(div().flex_1().min_w_0().child(inner))
                .child(
                    div()
                        .w(px(22.))
                        .flex_shrink_0()
                        .text_center()
                        .child(format!("{:.0}", edges.right.value())),
                ),
        )
        .child(
            div()
                .h(px(20.))
                .text_center()
                .child(format!("{:.0}", edges.bottom.value())),
        )
        .into_any_element()
}

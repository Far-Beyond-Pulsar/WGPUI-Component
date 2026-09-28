use super::*;

/// Describe declared constraints separately from the measured result; flex,
/// min/max constraints, and box sizing can all affect the final bounds.
fn explanation(length: Option<gpui::Length>, measured: gpui::Pixels) -> String {
    match length {
        Some(gpui::Length::Definite(gpui::DefiniteLength::Fraction(fraction))) => format!(
            "{:.0}% of the parent's available size; measured {:.1} px.",
            fraction * 100.,
            measured.value()
        ),
        Some(gpui::Length::Definite(length)) => format!(
            "Style requests {length}; measured {:.1} px.",
            measured.value()
        ),
        _ => format!(
            "Auto: content and parent layout determine the size. Measured {:.1} px.",
            measured.value()
        ),
    }
}

pub(super) fn render(
    layout: Option<&InspectorLayoutInfo>,
    style: Option<&StyleRefinement>,
    cx: &App,
) -> AnyElement {
    let Some(layout) = layout else {
        return div()
            .text_sm()
            .child("Layout data is not available yet.")
            .into_any_element();
    };
    let mut rows = Vec::new();
    for (name, measured, requested) in [
        (
            "Width",
            layout.bounds.size.width,
            style.and_then(|style| style.size.width),
        ),
        (
            "Height",
            layout.bounds.size.height,
            style.and_then(|style| style.size.height),
        ),
    ] {
        let text = if style.is_some() {
            explanation(requested, measured)
        } else {
            format!(
                "Measured {:.1} px. Style constraints are unavailable for this element.",
                measured.value()
            )
        };
        rows.push(
            h_flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .w(px(48.))
                        .flex_shrink_0()
                        .text_color(cx.theme().muted_foreground)
                        .child(name),
                )
                .child(div().flex_1().min_w_0().child(text))
                .into_any_element(),
        );
    }
    v_flex().text_sm().gap_2().children(rows).into_any_element()
}

//! The Summary bottom tab (§4 of `.agents/PROFILER_UI_SPEC.md`): a compact
//! category breakdown for the current selection — color-swatched legend +
//! stacked bar, each segment labeled with its ms total, plus a `Total`
//! figure. The reference also has a secondary origin-attribution table
//! (`1st/3rd party | Transfer size | Main thread time`) below it —
//! deliberately dropped rather than stubbed: there's no "network origin"
//! concept for a UI framework's own profiler to attribute time to, unlike
//! the ruler/CPU-graph rows this tab's sibling views stub for layout parity
//! (see `overview`'s module doc). The category breakdown bar is the part of
//! this tab `.agents/PROFILER_UI_SPEC.md` itself calls out as load-bearing.

use gpui::{
    div, px, relative, AnyElement, AppContext as _, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Styled,
};

use crate::{h_flex, v_flex, ActiveTheme};

use crate::profiler::category_color;

use super::data::{category_index, ns_to_ms_string, BottomUpRow, OVERVIEW_CATEGORIES};
use super::ProfilerPanel;

pub(crate) fn render(rows: &[BottomUpRow], cx: &mut Context<ProfilerPanel>) -> AnyElement {
    if rows.is_empty() {
        return super::super::profiler_empty_state("No spans recorded in the selected range.", cx);
    }

    // Indexed by [`category_index`] rather than a `HashMap<SpanCategory, _>`:
    // `SpanCategory` doesn't derive `Hash`, and there are only
    // [`OVERVIEW_CATEGORIES`]`.len()` of them anyway, so a fixed-size array
    // is both simpler and avoids adding a `Hash` bound to a type this
    // module doesn't own.
    let mut by_category_ns = [0u64; OVERVIEW_CATEGORIES.len()];
    let mut total_ns: u64 = 0;
    for row in rows {
        total_ns += row.self_ns;
        if let Some(category) = row.category {
            by_category_ns[category_index(category)] += row.self_ns;
        }
    }
    let total_ns = total_ns.max(1);

    let mut entries: Vec<(gpui::SpanCategory, u64)> = OVERVIEW_CATEGORIES
        .iter()
        .zip(by_category_ns.iter())
        .filter(|(_, ns)| **ns > 0)
        .map(|(category, ns)| (*category, *ns))
        .collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1));

    let bar = h_flex()
        .w_full()
        .h(px(20.))
        .rounded_md()
        .overflow_hidden()
        .children(entries.iter().map(|(category, ns)| {
            let fraction = (*ns as f64 / total_ns as f64).max(0.001) as f32;
            div()
                .h_full()
                .w(relative(fraction))
                .bg(category_color(*category, cx))
        }));

    let legend = v_flex().gap_1().children(entries.iter().map(|(category, ns)| {
        h_flex()
            .gap_2()
            .items_center()
            .child(
                div()
                    .size(px(10.))
                    .rounded_full()
                    .bg(category_color(*category, cx))
                    .flex_shrink_0(),
            )
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .child(format!("{:?}", category)),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} ms", ns_to_ms_string(*ns))),
            )
    }));

    v_flex()
        .id("record-summary")
        .size_full()
        .p_3()
        .gap_3()
        .child(bar)
        .child(legend)
        .child(
            h_flex()
                .gap_2()
                .pt_2()
                .border_t_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child("Total"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("{} ms", ns_to_ms_string(total_ns))),
                ),
        )
        .into_any_element()
}

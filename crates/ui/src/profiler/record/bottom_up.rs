//! The Bottom-up bottom tab (§4 of `.agents/PROFILER_UI_SPEC.md`): a
//! sortable table of every distinct activity type across the selected
//! range, aggregated leaf-first — "how much time did `Layout` cost in
//! total, regardless of who called it".
//!
//! # v1 note, honestly
//!
//! Sortable Self/Total/Count/Activity columns with genuine self-time (the
//! standard post-order stack algorithm, see [`super::data::build_bottom_up_rows`]).
//! Missing relative to the reference: no text filter box (`Aa`/regex/
//! whole-word toggles), no `No grouping` dropdown, no per-row expand (▶) to
//! reveal callers, no `file:line:col` source-location links (`FlameBar`
//! carries `element_source`, which is the right input for that once wired
//! up), no full-row highlight on the top sorted row. `rows` is recomputed
//! fresh every render rather than cached the way `flame`'s lane cache is —
//! `.agents/PROFILER_UI_SPEC.md`'s own "Deferred" list already flagged this
//! exact gap before this tab had its own module.

use gpui::{
    div, prelude::FluentBuilder as _, px, AnyElement, AppContext as _, Context,
    InteractiveElement as _, IntoElement, ParentElement as _, Styled, Window,
};

use crate::{
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
    ActiveTheme, Selectable as _, Sizable as _,
};

use crate::profiler::category_color;

use super::data::{ns_to_ms_string, BottomUpRow};
use super::ProfilerPanel;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum BottomUpSort {
    #[default]
    SelfTime,
    TotalTime,
    Count,
    Name,
}

#[derive(Default)]
pub(crate) struct BottomUpState {
    pub(crate) sort: BottomUpSort,
}

pub(crate) fn render(
    state: &mut BottomUpState,
    rows: &[BottomUpRow],
    _window: &mut Window,
    cx: &mut Context<ProfilerPanel>,
) -> AnyElement {
    if rows.is_empty() {
        return super::super::profiler_empty_state("No spans recorded in the selected range.", cx);
    }

    let sort = state.sort;
    let mut sorted: Vec<&BottomUpRow> = rows.iter().collect();
    match sort {
        BottomUpSort::SelfTime => sorted.sort_by(|a, b| b.self_ns.cmp(&a.self_ns)),
        BottomUpSort::TotalTime => sorted.sort_by(|a, b| b.total_ns.cmp(&a.total_ns)),
        BottomUpSort::Count => sorted.sort_by(|a, b| b.count.cmp(&a.count)),
        BottomUpSort::Name => sorted.sort_by(|a, b| a.name.cmp(&b.name)),
    }

    let total_self_ns: u64 = rows.iter().map(|r| r.self_ns).sum::<u64>().max(1);

    let header_button = |label: &'static str, this_sort: BottomUpSort| {
        Button::new(gpui::SharedString::from(format!("bottom-up-sort-{label}")))
            .xsmall()
            .when(sort == this_sort, |b| b.selected(true))
            .label(label)
            .on_click(cx.listener(move |panel, _, _window, cx| {
                panel.record.bottom_up.sort = this_sort;
                cx.notify();
            }))
            .into_any_element()
    };

    let header_row = h_flex()
        .gap_2()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(div().w(px(70.)).child(header_button("Self", BottomUpSort::SelfTime)))
        .child(div().w(px(70.)).child(header_button("Total", BottomUpSort::TotalTime)))
        .child(div().w(px(50.)).child(header_button("Count", BottomUpSort::Count)))
        .child(div().flex_1().child(header_button("Activity", BottomUpSort::Name)));

    let rows_elements: Vec<AnyElement> = sorted
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let self_pct = (row.self_ns as f64 / total_self_ns as f64) * 100.0;
            let swatch_color = row
                .category
                .map(|c| category_color(c, cx))
                .unwrap_or(cx.theme().muted_foreground);
            h_flex()
                .gap_2()
                .px_2()
                .py_1()
                .border_b_1()
                .border_color(cx.theme().border.opacity(0.5))
                .when(index == 0, |el| el.bg(cx.theme().selection.opacity(0.15)))
                .hover(|s| s.bg(cx.theme().list_hover))
                .child(
                    div()
                        .w(px(70.))
                        .text_xs()
                        .child(format!("{} ms ({:.1}%)", ns_to_ms_string(row.self_ns), self_pct)),
                )
                .child(
                    div()
                        .w(px(70.))
                        .text_xs()
                        .child(format!("{} ms", ns_to_ms_string(row.total_ns))),
                )
                .child(div().w(px(50.)).text_xs().child(row.count.to_string()))
                .child(
                    h_flex()
                        .flex_1()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .size(px(8.))
                                .rounded_full()
                                .bg(swatch_color)
                                .flex_shrink_0(),
                        )
                        .child(div().text_xs().truncate().child(row.name.clone())),
                )
                .into_any_element()
        })
        .collect();

    v_flex()
        .id("record-bottom-up")
        .size_full()
        .child(header_row)
        .children(rows_elements)
        .into_any_element()
}

//! The Summary bottom tab (§4 of `.agents/PROFILER_UI_SPEC.md`): a compact
//! category breakdown for the current selection — color-swatched legend +
//! stacked bar, each row labeled with its ms total, plus a `Total` figure.
//! The reference also has a secondary origin-attribution table
//! (`1st/3rd party | Transfer size | Main thread time`) below it —
//! deliberately dropped rather than stubbed: there's no "network origin"
//! concept for a UI framework's own profiler to attribute time to, unlike
//! the ruler/CPU-graph rows this tab's sibling views stub for layout parity
//! (see `overview`'s module doc). The category breakdown list is the part of
//! this tab `.agents/PROFILER_UI_SPEC.md` itself calls out as load-bearing,
//! and — per a close read of the actual reference screenshot's crop — *is*
//! the whole left-hand pane: no visible stacked bar sits above Chrome's own
//! legend there either, just `Range: … / [swatch] Category  N ms` rows and a
//! hollow-swatch `Total` row, all right-aligned to a shared edge. The
//! stacked bar below is still built (asked for explicitly, and it's a
//! legitimate at-a-glance summary of the same numbers), just treated as a
//! secondary visualization rather than the thing carrying the row order.
//!
//! No `Range: …` header: that needs the selection's actual `(start_ns,
//! end_ns)`, which isn't in this function's signature today (only the
//! already-filtered `rows`) — widening it means also widening
//! `render_bottom_tabs`'s signature *and* its call in `mod.rs`'s
//! `render_content` (the selection bounds are computed there, one call
//! upstream of `render_bottom_tabs` itself), not just the one
//! `summary::render(rows, cx)` call site. `mod.rs` is being edited by other
//! work concurrently, so that's deferred rather than risked — see this
//! task's own boundary note. The legend, bar, and `Total` figure below are
//! the load-bearing part of this tab either way.

use gpui::{
    div, px, relative, AnyElement, AppContext as _, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Styled,
};

use crate::{h_flex, v_flex, ActiveTheme};

use crate::profiler::category_color;

use super::data::{category_index, BottomUpRow, OVERVIEW_CATEGORIES};
use super::ProfilerPanel;

/// Formats a nanosecond duration the way the reference Summary tab formats
/// its own category totals: whole milliseconds with thousands separators
/// (`"2,208 ms"`, `"10,459 ms"`) — not the sub-millisecond precision
/// `data::ns_to_ms_string` uses for individual Bottom-up rows. A category
/// *total* over a multi-second selection is never usefully precise to
/// 1/100 ms, and without comma grouping a 5-digit total is much harder to
/// scan at a glance. Kept local to this file rather than folded into the
/// shared `ns_to_ms_string` since the two views want genuinely different
/// precision, not just a formatting preference.
fn format_ms_grouped(ns: u64) -> String {
    let ms = (ns + 500_000) / 1_000_000;
    let digits = ms.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().rev().enumerate() {
        if index != 0 && index % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    grouped.chars().rev().collect()
}

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

    // Largest-first, same order the reference list uses (`Rendering` >
    // `System` > `Painting` > `Scripting` in the screenshot) — the stacked
    // bar below iterates this same `Vec`, so its segment order always
    // agrees with the legend's row order.
    let mut entries: Vec<(gpui::SpanCategory, u64)> = OVERVIEW_CATEGORIES
        .iter()
        .zip(by_category_ns.iter())
        .filter(|(_, ns)| **ns > 0)
        .map(|(category, ns)| (*category, *ns))
        .collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1));

    // A thin pill-shaped stacked bar, segments proportional to each
    // category's share of `total_ns`. `gap(px(1.))` leaves a hairline seam
    // between segments (visible as the panel background showing through)
    // rather than colors touching edge-to-edge, matching the subtle
    // segment separation this crate's other stacked/multi-series charts
    // use (see `overview`'s bucket edges) instead of a single solid block.
    let bar = h_flex()
        .w_full()
        .h(px(10.))
        .gap(px(1.))
        .rounded_full()
        .overflow_hidden()
        .children(entries.iter().map(|(category, ns)| {
            let fraction = (*ns as f64 / total_ns as f64).max(0.004) as f32;
            div()
                .h_full()
                .w(relative(fraction))
                .bg(category_color(*category, cx))
        }));

    // One row per category: solid square swatch + name (left, natural
    // width) and the ms total (right) — `justify_between()` on a `w_full()`
    // row is what makes every row's value right-align to the *same* edge
    // regardless of how long the category name or the number is, exactly
    // like the reference's own column (compare `"2,208 ms"` under
    // `Rendering` lining up with `"10,459 ms"` under `Total`, even though
    // neither the name nor the digit count matches row to row).
    let legend = v_flex().gap_2().children(entries.iter().map(|(category, ns)| {
        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .size(px(12.))
                            .flex_shrink_0()
                            .rounded(px(2.))
                            .bg(category_color(*category, cx)),
                    )
                    .child(div().text_sm().child(format!("{:?}", category))),
            )
            .child(
                div()
                    .text_sm()
                    .child(format!("{} ms", format_ms_grouped(*ns))),
            )
    }));

    // `Total`: same row shape as the legend above it (same swatch size, same
    // right-aligned value column) so it reads as one continuous list rather
    // than a visually distinct footer — matching the reference, whose
    // `Total` row sits flush against the last category row with no rule
    // between them. The swatch itself is hollow (border only, no fill) since
    // "Total" isn't a category with its own color.
    let total_row = h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            h_flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .size(px(12.))
                        .flex_shrink_0()
                        .rounded(px(2.))
                        .border_1()
                        .border_color(cx.theme().muted_foreground),
                )
                .child(div().text_sm().child("Total")),
        )
        .child(
            div()
                .text_sm()
                .child(format!("{} ms", format_ms_grouped(total_ns))),
        );

    v_flex()
        .id("record-summary")
        .size_full()
        .p_3()
        .gap_3()
        .child(bar)
        .child(v_flex().gap_2().child(legend).child(total_row))
        .into_any_element()
}

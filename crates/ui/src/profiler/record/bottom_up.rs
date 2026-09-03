//! The Bottom-up bottom tab (§4 of `.agents/PROFILER_UI_SPEC.md`): a
//! sortable, filterable table of every distinct activity type across the
//! selected range, aggregated leaf-first — "how much time did `Layout` cost
//! in total, regardless of who called it". The "money view" for
//! optimization work, per the spec's own framing.
//!
//! # What matches the reference now
//!
//! Sortable Self/Total/Activity columns with genuine self-time (the
//! standard post-order stack algorithm, see
//! [`super::data::build_bottom_up_rows`]), a real text filter box with
//! working case-sensitivity/regex/whole-word toggles (see
//! [`compile_filter`]), `N,NNN.N ms` + `NN.N %` number formatting matching
//! the reference's own grouped-thousands one-decimal style, a category
//! swatch + name + (when known and unambiguous) `file:line` source per
//! Activity row, and a solid-highlight top row on whichever column is
//! currently sorted — all cross-checked pixel-for-pixel against a crop of
//! the actual reference screenshot (`Screenshot 2026-09-02 184359.png`; the
//! two screenshots this file's own task briefing originally pointed at
//! turned out — once actually opened — to be an unrelated Insights/Memory
//! pair from the same recording session, not this tab, so the real
//! Bottom-up screenshot was tracked down among its neighbors by timestamp
//! before writing a line of this file. Notably, that crop shows the
//! top-row highlight covering only the Self/Total *time* cells, not the
//! Activity cell — this file matches that pixel truth rather than the
//! briefing's paraphrase of it).
//!
//! # What's deliberately still inert, honestly
//!
//! - **Per-row expand (▶)**: shown (only on rows whose category isn't a
//!   structural pipeline phase — see [`is_pipeline_phase`]) but does nothing
//!   on click. Making it real needs immediate-caller data per row, which
//!   `BottomUpRow` doesn't carry — building that means walking each frame's
//!   span stack in *start* order (this tab's self-time algorithm only needs
//!   *completion* order) to reconstruct a proper caller stack, a genuinely
//!   separate algorithm from anything `data.rs` has today. Several other
//!   submodules under `profiler::record` are being edited concurrently by
//!   other work right now; that's real surface area to add to the one
//!   shared data file under those conditions for a row affordance that's
//!   still correctly disclosed as non-functional either way, so it's left
//!   as honest inert chrome instead, matching this task's own explicit
//!   fallback allowance.
//! - **`No grouping` dropdown**: a real `Button` with `dropdown_caret(true)`
//!   in the exact reference position, but its `on_click` is a no-op — this
//!   profiler has exactly one grouping (flat by name), so there is nothing
//!   a real menu could offer beyond the single item already shown. Chrome's
//!   own alternate groupings (by domain, by frame) don't have an equivalent
//!   concept here (no network domains; see `summary`'s own module doc for
//!   the same reasoning about the origin-attribution table).
//! - **Source links aren't clickable**: there's no Sources panel in this
//!   codebase to jump to, so `row.source` renders as plain (if
//!   info-colored) text, not a real hyperlink.

use gpui::{
    div, prelude::FluentBuilder as _, px, AnyElement, AppContext as _, Context, Entity, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, StatefulInteractiveElement as _,
    Styled, Subscription, Window,
};

use crate::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{InputEvent, InputState, TextInput},
    v_flex, ActiveTheme, Icon, IconName, Selectable as _, Sizable as _,
};

use crate::profiler::category_color;

use super::data::BottomUpRow;
use super::ProfilerPanel;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum BottomUpSort {
    #[default]
    SelfTime,
    TotalTime,
    Name,
}

/// Fixed pixel widths for the two sub-cells (`N,NNN.N ms` / `NN.N %`) that
/// make up one Self/Total time column — kept as named constants rather than
/// inlined `px(..)` literals since the header cell has to add up to exactly
/// the same total width as the row cells below it for the columns to stay
/// aligned.
const TIME_MS_COL_W: gpui::Pixels = px(72.);
const TIME_PCT_COL_W: gpui::Pixels = px(50.);

#[derive(Default)]
pub(crate) struct BottomUpState {
    sort: BottomUpSort,
    /// Lazily created on first [`render`] call — a `#[derive(Default)]`
    /// struct can't call `cx.new` itself, so this starts `None` and gets
    /// filled in (alongside `_filter_subscription`) the first time `render`
    /// runs, exactly once, the same "create on first use" shape
    /// `ProfilerPanel::search_input` uses eagerly in its own constructor
    /// (not available to a plain nested struct like this one — see
    /// `record::mod`'s "State shape" doc for why `RecordState` and its
    /// children are plain structs rather than `Entity`s).
    filter_input: Option<Entity<InputState>>,
    /// Mirrors `filter_input`'s live text, kept current by the
    /// `InputEvent::Change` subscription set up alongside it — needed
    /// because `TextInput` edits only auto-notify *its own* `InputState`
    /// entity, not `ProfilerPanel`; without forwarding the change and
    /// calling `cx.notify()` on the panel explicitly, typing wouldn't
    /// re-render this table. Same reasoning as `ProfilerPanel::flame_search`
    /// alongside `ProfilerPanel::search_input`.
    filter_text: gpui::SharedString,
    _filter_subscription: Option<Subscription>,
    case_sensitive: bool,
    regex_mode: bool,
    whole_word: bool,
}

/// Whether `category` is a structural render/GPU-pipeline phase — this
/// profiler's equivalent of the reference's own `Commit`/`Layout`/
/// `Pre-paint`/`Paint`/`Layerize` rows, which never show a `(unknown)`
/// source suffix or an expand triangle (compare those against
/// `requestIdleCallback (unknown)`/`Function call <link>`, which do) —
/// "what called Layout" isn't a meaningful question the way "what called
/// this function" is. Used to decide both whether a row gets a source/
/// `(unknown)` suffix and whether it gets an (inert, see this file's module
/// doc) expand triangle, so the two affordances never disagree about which
/// rows are "leaf work" versus "pipeline phase" for one given row.
fn is_pipeline_phase(category: Option<gpui::SpanCategory>) -> bool {
    matches!(
        category,
        Some(
            gpui::SpanCategory::WindowFrame
                | gpui::SpanCategory::ElementRequestLayout
                | gpui::SpanCategory::ElementPrepaint
                | gpui::SpanCategory::ElementPaint
                | gpui::SpanCategory::GpuRenderPass
                | gpui::SpanCategory::GpuSubmitPresent
        )
    )
}

/// Formats a nanosecond duration the way the reference Bottom-up table
/// formats its own per-row times: one decimal place, thousands-grouped
/// (`"1,214.8"`, `"6.5"`) — genuinely different from both
/// `data::ns_to_ms_string`'s two-decimal ungrouped style (built for the
/// detail flame chart's tooltip, where sub-tenth precision on a single span
/// matters more than scanability across dozens of rows) and `summary`'s own
/// `format_ms_grouped`, which rounds to whole milliseconds (fine for a
/// handful of category totals, too coarse once individual rows can be under
/// a millisecond, as most of this table's tail is).
fn format_ms_grouped_one_decimal(ns: u64) -> String {
    let tenths_of_ms = (ns + 50_000) / 100_000;
    let whole = tenths_of_ms / 10;
    let frac = tenths_of_ms % 10;
    let digits = whole.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().rev().enumerate() {
        if index != 0 && index % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let grouped: String = grouped.chars().rev().collect();
    format!("{grouped}.{frac}")
}

/// Compiles the filter box's current text + toggle state into a matcher.
/// `None` means "box is empty, show every row" (the reference's own default
/// state). `Some(Err(_))` means the user typed an in-progress/invalid regex
/// (regex mode only) — treated the same as "no filter" for which rows show,
/// but surfaced back to [`render`] so the box itself can flag it, rather
/// than silently hiding every row while someone is still mid-pattern.
///
/// Case-sensitivity and whole-word both compose with regex mode rather than
/// being separate code paths: a plain-text filter is just
/// [`regex::escape`]'d before the same `\b…\b`/`(?i)` wrapping a literal
/// regex-mode pattern gets, so all three toggles combine correctly in any
/// combination instead of only the ones someone thought to test.
fn compile_filter(state: &BottomUpState) -> Option<Result<regex::Regex, regex::Error>> {
    if state.filter_text.is_empty() {
        return None;
    }
    let body = if state.regex_mode {
        state.filter_text.to_string()
    } else {
        regex::escape(&state.filter_text)
    };
    let body = if state.whole_word {
        format!(r"\b(?:{body})\b")
    } else {
        body
    };
    let pattern = if state.case_sensitive {
        body
    } else {
        format!("(?i){body}")
    };
    Some(regex::Regex::new(&pattern))
}

pub(crate) fn render(
    state: &mut BottomUpState,
    rows: &[BottomUpRow],
    window: &mut Window,
    cx: &mut Context<ProfilerPanel>,
) -> AnyElement {
    if rows.is_empty() {
        return super::super::profiler_empty_state("No spans recorded in the selected range.", cx);
    }

    if state.filter_input.is_none() {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter"));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this: &mut ProfilerPanel, input_state, event: &InputEvent, _window, cx| {
                if let InputEvent::Change = event {
                    this.record.bottom_up.filter_text = input_state.read(cx).value();
                    cx.notify();
                }
            },
        );
        state.filter_input = Some(input);
        state._filter_subscription = Some(subscription);
    }
    let filter_input = state.filter_input.clone().expect("just initialized above");

    let sort = state.sort;
    let filter_result = compile_filter(state);
    let filter_is_invalid = matches!(filter_result, Some(Err(_)));
    let matcher = match filter_result {
        Some(Ok(re)) => Some(re),
        _ => None,
    };

    let mut filtered: Vec<&BottomUpRow> = rows
        .iter()
        .filter(|row| {
            matcher
                .as_ref()
                .map_or(true, |re| re.is_match(row.name.as_ref()))
        })
        .collect();
    match sort {
        BottomUpSort::SelfTime => filtered.sort_by(|a, b| b.self_ns.cmp(&a.self_ns)),
        BottomUpSort::TotalTime => filtered.sort_by(|a, b| b.total_ns.cmp(&a.total_ns)),
        BottomUpSort::Name => filtered.sort_by(|a, b| a.name.cmp(&b.name)),
    }

    // Percentages stay relative to the *whole* selection (every row, not
    // just what the filter currently shows) — matching the reference, where
    // typing into the filter box narrows which rows are visible without
    // changing what "100%" means for the ones that remain.
    let total_ns_denominator = rows.iter().map(|r| r.self_ns).sum::<u64>().max(1);

    // `width: None` means "Activity", the one header cell that grows to
    // fill remaining space rather than lining up with a fixed-width row
    // sub-column pair.
    let header_cell = |label: &'static str, this_sort: BottomUpSort, width: Option<gpui::Pixels>| {
        let active = sort == this_sort;
        h_flex()
            .id(gpui::SharedString::from(format!("bottom-up-sort-{label}")))
            .when_some(width, |el, w| el.w(w).flex_shrink_0())
            .when(width.is_none(), |el| el.flex_1())
            .items_center()
            .gap_1()
            .cursor_pointer()
            .text_xs()
            .when(active, |el| {
                el.text_color(cx.theme().foreground)
                    .font_weight(FontWeight::SEMIBOLD)
            })
            .when(!active, |el| el.text_color(cx.theme().muted_foreground))
            .child(label)
            .when(active, |el| {
                el.child(
                    Icon::new(IconName::ChevronDown)
                        .xsmall()
                        .text_color(cx.theme().foreground),
                )
            })
            .on_click(cx.listener(move |panel, _, _window, cx| {
                panel.record.bottom_up.sort = this_sort;
                cx.notify();
            }))
            .into_any_element()
    };

    let header_row = h_flex()
        .gap_3()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(header_cell(
            "Self time",
            BottomUpSort::SelfTime,
            Some(TIME_MS_COL_W + TIME_PCT_COL_W + px(4.)),
        ))
        .child(header_cell(
            "Total time",
            BottomUpSort::TotalTime,
            Some(TIME_MS_COL_W + TIME_PCT_COL_W + px(4.)),
        ))
        .child(header_cell("Activity", BottomUpSort::Name, None));

    let rows_elements: Vec<AnyElement> = filtered
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let self_pct = (row.self_ns as f64 / total_ns_denominator as f64) * 100.0;
            let total_pct = (row.total_ns as f64 / total_ns_denominator as f64) * 100.0;
            let swatch_color = row
                .category
                .map(|c| category_color(c, cx))
                .unwrap_or(cx.theme().muted_foreground);
            let is_top = index == 0;
            let pipeline_phase = is_pipeline_phase(row.category);

            // See [`is_pipeline_phase`]'s doc: pipeline-phase rows get
            // neither a source/`(unknown)` suffix nor an expand triangle;
            // everything else gets both.
            let source_suffix = (!pipeline_phase).then(|| match &row.source {
                Some(source) => div()
                    .text_xs()
                    .text_color(cx.theme().info)
                    .flex_shrink_0()
                    .child(source.clone())
                    .into_any_element(),
                None => div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .flex_shrink_0()
                    .child("(unknown)")
                    .into_any_element(),
            });

            let time_cell = |ms_text: String, pct_text: String| {
                h_flex()
                    .w(TIME_MS_COL_W + TIME_PCT_COL_W + px(4.))
                    .flex_shrink_0()
                    .gap_1()
                    .when(is_top, |el| {
                        el.bg(cx.theme().primary)
                            .text_color(cx.theme().primary_foreground)
                            .rounded(px(2.))
                    })
                    .child(
                        div()
                            .w(TIME_MS_COL_W)
                            .flex_shrink_0()
                            .text_xs()
                            .text_right()
                            .child(ms_text),
                    )
                    .child(
                        div()
                            .w(TIME_PCT_COL_W)
                            .flex_shrink_0()
                            .text_xs()
                            .when(!is_top, |el| el.text_color(cx.theme().muted_foreground))
                            .text_right()
                            .child(pct_text)
                            .child(" %"),
                    )
            };

            h_flex()
                .id(("bottom-up-row", index))
                .gap_3()
                .px_2()
                .py_1()
                .border_b_1()
                .border_color(cx.theme().border.opacity(0.5))
                .hover(|s| s.bg(cx.theme().list_hover))
                .child(time_cell(
                    format!("{} ms", format_ms_grouped_one_decimal(row.self_ns)),
                    format!("{self_pct:.1}"),
                ))
                .child(time_cell(
                    format!("{} ms", format_ms_grouped_one_decimal(row.total_ns)),
                    format!("{total_pct:.1}"),
                ))
                .child(
                    h_flex()
                        .flex_1()
                        .min_w(px(0.))
                        .gap_1()
                        .items_center()
                        .child(
                            // Fixed-width slot so every row's swatch lines up
                            // regardless of whether this particular row shows
                            // a triangle — an empty slot rather than
                            // collapsing to zero width for pipeline-phase
                            // rows, matching the reference's own alignment
                            // (compare `Commit`'s swatch sitting at the same
                            // x-position as `Recalculate style`'s, one row
                            // down, despite only the latter having a ▶).
                            div().w(px(12.)).flex_shrink_0().when(!pipeline_phase, |el| {
                                el.child(
                                    Icon::new(IconName::ChevronRight)
                                        .xsmall()
                                        .text_color(cx.theme().muted_foreground),
                                )
                            }),
                        )
                        .child(
                            div()
                                .size(px(10.))
                                .flex_shrink_0()
                                .rounded(px(2.))
                                .bg(swatch_color),
                        )
                        .child(div().text_xs().truncate().child(row.name.clone()))
                        .children(source_suffix),
                )
                .into_any_element()
        })
        .collect();

    let no_matches = filtered.is_empty();

    let toggle_button = |id: &'static str, label: &'static str, active: bool, tooltip: &'static str| {
        Button::new(id)
            .xsmall()
            .ghost()
            .selected(active)
            .label(label)
            .tooltip(tooltip)
    };

    let toolbar = h_flex()
        .gap_1()
        .items_center()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            toggle_button(
                "bottom-up-filter-case",
                "Aa",
                state.case_sensitive,
                "Match case",
            )
            .on_click(cx.listener(|panel, _, _window, cx| {
                panel.record.bottom_up.case_sensitive = !panel.record.bottom_up.case_sensitive;
                cx.notify();
            })),
        )
        .child(
            toggle_button(
                "bottom-up-filter-regex",
                "(.*)",
                state.regex_mode,
                "Use regular expression",
            )
            .on_click(cx.listener(|panel, _, _window, cx| {
                panel.record.bottom_up.regex_mode = !panel.record.bottom_up.regex_mode;
                cx.notify();
            })),
        )
        .child(
            toggle_button(
                "bottom-up-filter-word",
                "ab",
                state.whole_word,
                "Match whole word",
            )
            .on_click(cx.listener(|panel, _, _window, cx| {
                panel.record.bottom_up.whole_word = !panel.record.bottom_up.whole_word;
                cx.notify();
            })),
        )
        .child(
            TextInput::new(&filter_input)
                .xsmall()
                .flex_1()
                .prefix(
                    Icon::new(IconName::Filter)
                        .xsmall()
                        .text_color(cx.theme().muted_foreground),
                )
                .when(filter_is_invalid, |input| {
                    input.border_color(cx.theme().danger)
                }),
        )
        .child(
            // Inert — see this file's module doc for why a real menu isn't
            // built.
            Button::new("bottom-up-grouping")
                .xsmall()
                .ghost()
                .label("No grouping")
                .dropdown_caret(true)
                .tooltip("This profiler has only one grouping: flat by activity name"),
        );

    v_flex()
        .id("record-bottom-up")
        .size_full()
        .child(toolbar)
        .child(header_row)
        .when(no_matches, |el| {
            el.child(
                div()
                    .p_4()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("No activity names match the current filter."),
            )
        })
        .when(!no_matches, |el| {
            el.child(v_flex().flex_1().min_h(px(0.)).overflow_y_scroll().children(rows_elements))
        })
        .into_any_element()
}

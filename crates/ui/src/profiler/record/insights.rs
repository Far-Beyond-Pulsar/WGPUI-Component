//! The Insights sidebar (§6 of `.agents/PROFILER_UI_SPEC.md`): headline
//! metrics strip + a vertical list of collapsible diagnostic cards.
//!
//! # v1 note, honestly
//!
//! The reference's headline trio is `LCP`/`INP`/`CLS` — Core Web Vitals,
//! which have no meaning for a UI framework's own profiler (there's no
//! page load, no user input latency metric, no layout-shift metric
//! defined here). This renders the closest honest equivalents this
//! profiler actually has: worst frame time, mean frame time, and a count
//! of janky (>16.7ms, i.e. sub-60fps) frames — all already computed into
//! `ProfilerPanel::frame_durations_ms`/`frame_durations_max_ms`. Insight
//! cards are built from `gpui::DiagnosticEvent`s already captured per
//! frame (via `ProfilerPanel::diagnostic_details`, reused here — same
//! source the plain Diagnostics tab reads) plus a long-task summary from
//! the overview's own bucketing (`OverviewBucket::has_long_task`). No
//! click-to-expand/highlight-the-timeline interaction yet (§6's own
//! "Insights sidebar" bullet describes that as implied, not directly
//! captured, even in the reference screenshots). Cards are grouped by raw
//! `DiagnosticKind` today (`{kind:?}: observed N times`) rather than named/
//! described the way the reference's insight cards are — a real per-kind
//! description belongs here once this view gets its full pass.

use gpui::{
    div, prelude::FluentBuilder as _, px, AnyElement, AppContext as _, Context,
    InteractiveElement as _, IntoElement, ParentElement as _, StatefulInteractiveElement as _,
    Styled,
};

use crate::{h_flex, v_flex, ActiveTheme};

use super::ProfilerPanel;

#[derive(Default)]
pub(crate) struct InsightsState;

struct Insight {
    title: String,
    detail: String,
}

pub(crate) fn render(
    _state: &mut InsightsState,
    capture: Option<&gpui::Capture>,
    frame_durations_ms: &[f32],
    frame_durations_max_ms: f32,
    _window: &mut gpui::Window,
    cx: &mut Context<ProfilerPanel>,
) -> AnyElement {
    let has_data = capture.is_some() && !frame_durations_ms.is_empty();

    let (worst_ms, mean_ms, janky_count) = if has_data {
        let worst = frame_durations_max_ms;
        let mean = frame_durations_ms.iter().sum::<f32>() / frame_durations_ms.len() as f32;
        let janky = frame_durations_ms.iter().filter(|ms| **ms > 16.7).count();
        (Some(worst), Some(mean), janky)
    } else {
        (None, None, 0)
    };

    let headline = h_flex()
        .gap_4()
        .px_2()
        .py_2()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(headline_metric(
            "Worst frame",
            worst_ms.map(|v| format!("{v:.1} ms")),
            worst_ms.is_some_and(|v| v <= 16.7),
            cx,
        ))
        .child(headline_metric(
            "Mean frame",
            mean_ms.map(|v| format!("{v:.1} ms")),
            mean_ms.is_some_and(|v| v <= 16.7),
            cx,
        ))
        .child(headline_metric(
            "Janky frames",
            has_data.then(|| janky_count.to_string()),
            janky_count == 0,
            cx,
        ));

    let insights = capture.map(build_insights).unwrap_or_default();
    let insight_list = if insights.is_empty() {
        div()
            .p_2()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(if has_data {
                "No diagnostics flagged in this capture."
            } else {
                "Start and stop a capture to see insights."
            })
            .into_any_element()
    } else {
        v_flex()
            .gap_1()
            .p_2()
            .children(insights.into_iter().map(|insight| render_insight_card(insight, cx)))
            .into_any_element()
    };

    v_flex()
        .id("record-insights")
        .size_full()
        .child(headline)
        .child(
            div()
                .id("record-insights-list")
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .child(insight_list),
        )
        .into_any_element()
}

fn headline_metric(
    label: &'static str,
    value: Option<String>,
    good: bool,
    cx: &Context<ProfilerPanel>,
) -> AnyElement {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(
            div()
                .text_sm()
                .when(good, |d| d.text_color(cx.theme().success))
                .child(value.unwrap_or_else(|| "\u{2013}".to_string())),
        )
        .into_any_element()
}

fn build_insights(capture: &gpui::Capture) -> Vec<Insight> {
    use std::collections::HashMap;

    let mut by_kind: HashMap<String, u32> = HashMap::new();
    let mut long_task_frames = 0u32;
    for frame in capture.frames() {
        for event in &frame.diagnostics {
            *by_kind.entry(format!("{:?}", event.kind)).or_insert(0) += 1;
        }
        if frame
            .cpu_spans
            .iter()
            .any(|s| s.depth == 0 && s.duration_ns as u64 >= super::data::LONG_TASK_NS)
        {
            long_task_frames += 1;
        }
    }

    let mut insights = Vec::new();
    if long_task_frames > 0 {
        insights.push(Insight {
            title: "Long tasks".to_string(),
            detail: format!(
                "{long_task_frames} frame(s) had a top-level span over 50ms — see the overview \
                 strip's red hatch marks, or the Bottom-up tab for which activity dominated."
            ),
        });
    }
    for (kind, count) in by_kind {
        insights.push(Insight {
            title: kind,
            detail: format!("Observed {count} time(s) in this capture."),
        });
    }
    insights
}

fn render_insight_card(insight: Insight, cx: &Context<ProfilerPanel>) -> AnyElement {
    v_flex()
        .gap_1()
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .child(div().text_sm().child(insight.title))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(insight.detail),
        )
        .into_any_element()
}

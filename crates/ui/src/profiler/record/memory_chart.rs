//! The memory track (§3 of `.agents/PROFILER_UI_SPEC.md`, shown when the
//! toolbar's `☑ Memory` toggle is on): a time series over the selected
//! range.
//!
//! # v1 note, honestly
//!
//! The reference is a single chart with several **overlaid step lines**
//! (JS heap / Documents / Nodes / Listeners / GPU memory) behind a
//! checkbox legend that can hide individual series without rescaling the
//! others. This isn't that yet — no JS heap equivalent exists in a native
//! UI framework, and `ProfilerPanel::memory_cpu`/`memory_gpu` are one-shot
//! on-demand snapshots (`gpui::MemorySnapshot`/`GpuMemorySnapshot`), not a
//! per-frame time series, so they can't back a chart like this without new
//! instrumentation (`.agents/PROFILER_UI_SPEC.md`'s own build-order note
//! already flagged this as needing new counters, not just a new view).
//!
//! What *is* real and per-frame today: each [`gpui::FrameCapture`] already
//! carries its own span/diagnostic counts. This renders those as one
//! sparkline row per series (same `div()`-bar approach
//! `ProfilerPanel::render_frame_duration_sparkline` already uses for the
//! Counters tab, not yet the reference's single overlaid step-line chart
//! with a checkbox legend) — real counts, honestly labeled, standing in
//! for the "several time series, one chart" shape until it's worth
//! building the real overlay.

use gpui::{
    div, px, AnyElement, AppContext as _, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Styled,
};

use crate::{h_flex, v_flex, ActiveTheme};

use super::ProfilerPanel;

#[derive(Default)]
pub(crate) struct MemoryState;

struct Series {
    label: &'static str,
    color_index: u8,
    values: Vec<u32>,
}

pub(crate) fn render(
    _state: &mut MemoryState,
    capture: &gpui::Capture,
    start_ns: u64,
    end_ns: u64,
    _window: &mut gpui::Window,
    cx: &mut Context<ProfilerPanel>,
) -> AnyElement {
    let frames: Vec<&gpui::FrameCapture> = capture
        .frames()
        .filter(|f| f.frame_end_ns >= start_ns && f.frame_start_ns <= end_ns)
        .collect();
    if frames.is_empty() {
        return super::super::profiler_empty_state("No frames in the selected range.", cx);
    }

    let series = [
        Series {
            label: "CPU spans / frame",
            color_index: 0,
            values: frames.iter().map(|f| f.cpu_spans.len() as u32).collect(),
        },
        Series {
            label: "Background spans / frame",
            color_index: 1,
            values: frames
                .iter()
                .map(|f| f.background_spans.len() as u32)
                .collect(),
        },
        Series {
            label: "GPU spans / frame",
            color_index: 2,
            values: frames.iter().map(|f| f.gpu_spans.len() as u32).collect(),
        },
        Series {
            label: "Diagnostics / frame",
            color_index: 3,
            values: frames.iter().map(|f| f.diagnostics.len() as u32).collect(),
        },
    ];

    v_flex()
        .id("record-memory-chart")
        .size_full()
        .gap_2()
        .p_2()
        .children(series.iter().map(|s| render_series_row(s, cx)))
        .into_any_element()
}

fn render_series_row(series: &Series, cx: &Context<ProfilerPanel>) -> AnyElement {
    const HEIGHT: f32 = 28.0;
    let max = series.values.iter().copied().max().unwrap_or(1).max(1);
    let min = series.values.iter().copied().min().unwrap_or(0);
    let color = match series.color_index {
        0 => cx.theme().chart_1,
        1 => cx.theme().chart_2,
        2 => cx.theme().chart_3,
        _ => cx.theme().chart_4,
    };

    let bar_count = series.values.len().max(1);
    let bar_width = (100.0 / bar_count as f32).max(0.2);

    v_flex()
        .gap_1()
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .size(px(8.))
                        .rounded_full()
                        .bg(color)
                        .flex_shrink_0(),
                )
                .child(div().text_xs().flex_1().child(series.label))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("[{min} \u{2013} {max}]")),
                ),
        )
        .child(
            div()
                .relative()
                .h(px(HEIGHT))
                .w_full()
                .rounded_sm()
                .bg(cx.theme().muted.opacity(0.3))
                .overflow_hidden()
                .children(series.values.iter().enumerate().map(|(index, value)| {
                    let height = ((*value as f32 / max as f32) * HEIGHT).max(1.0);
                    div()
                        .absolute()
                        .bottom(px(0.))
                        .left(gpui::relative(index as f32 / bar_count as f32))
                        .w(gpui::relative(bar_width / 100.0))
                        .h(px(height))
                        .bg(color)
                })),
        )
        .into_any_element()
}

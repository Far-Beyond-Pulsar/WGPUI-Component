//! The Insights sidebar (§6 of `.agents/PROFILER_UI_SPEC.md`): a headline
//! metrics strip plus a dense, list-style column of collapsible diagnostic
//! "insight" cards — modeled closely on Chrome DevTools' own Performance
//! panel Insights sidebar.
//!
//! # Honest substitution, not fabrication
//!
//! The reference's headline trio is `LCP`/`INP`/`CLS` — Core Web Vitals,
//! which have no meaning for a UI framework's own profiler (there's no page
//! load, no user-input-latency metric, no layout-shift metric defined
//! here). This renders the closest honest equivalents this profiler
//! actually has: worst frame time, mean frame time, and a count of janky
//! (> [`JANK_MS`], i.e. sub-60fps) frames — all already computed into
//! `ProfilerPanel::frame_durations_ms`/`frame_durations_max_ms`.
//!
//! Likewise, the reference's ~19 named insights (`LCP breakdown`, `3rd
//! parties`, `Render-blocking requests`, `Network dependency tree`, …) are
//! web-platform concepts this profiler has no data for — there is no
//! network, no DOM, no per-element paint cost breakdown. Rather than
//! renaming those exact cards to something misleading, [`build_insights`]
//! derives its own set from data this profiler genuinely records: one card
//! per meaningful cluster of `gpui::DiagnosticKind` (see its doc comments
//! in `crates/ui/wgpui/src/flamegraph.rs` for what each one actually
//! observes), plus a long-task summary built the same way the overview
//! strip's own red hatch marks are (`super::data::LONG_TASK_NS`). Tightly
//! coupled kind pairs that only make sense read together — a resize
//! notification and the handler it triggers, a swapchain reconfiguration
//! and the drawable resize that goes with it, an engine frame and the
//! scene syncs it may trigger — are folded into one card rather than
//! mechanically emitting twelve near-duplicate ones; the module comment on
//! [`build_insights`] documents each grouping's reasoning inline.
//!
//! No click-to-expand/highlight-the-timeline interaction yet (§6's own
//! "Insights sidebar" bullet describes that as implied, not directly
//! captured, even in the reference screenshots) — a card with an actual
//! finding just always renders its detail text below the title, since
//! there's nowhere else to reveal it.

use gpui::{
    div, prelude::FluentBuilder as _, px, AnyElement, AppContext as _, Context, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled,
};

use crate::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable as _};

use super::ProfilerPanel;

/// Frame budget for 60fps, shared by the headline metrics' own "good"
/// thresholding and the janky-frame count itself.
const JANK_MS: f32 = 16.7;

#[derive(Default)]
pub(crate) struct InsightsState {
    /// Whether the collapsed "Passed insights (N)" group at the bottom is
    /// expanded. Starts collapsed, matching the reference screenshots — a
    /// clean capture shouldn't open by dumping every non-finding onto
    /// screen.
    passed_expanded: bool,
    /// Cached [`build_insights`] output, keyed by `capture_generation` —
    /// same reasoning as `record::RecordBottomUpCache`: `build_insights`
    /// walks every diagnostic event in every frame of the capture, so it
    /// must not re-run on every render (every hover, every mouse move) just
    /// because this sidebar always renders alongside everything else in the
    /// Record tab. Unlike the bottom-up cache this isn't keyed by
    /// selection/range too — insights are always computed over the whole
    /// capture, never the selected sub-range.
    insights_cache: Option<(u64, std::rc::Rc<Vec<Insight>>)>,
}

/// One diagnostic insight card. `finding` is the honest hinge this whole
/// view is built on: `Some(detail)` means this capture actually has
/// something to flag, rendered at full visual weight above the fold;
/// `None` means this category was checked and came back clean, so it gets
/// folded into the collapsed "Passed insights" group instead — see the
/// module doc's "Honest substitution" section.
#[derive(Clone)]
struct Insight {
    /// Stable id for this card's element id, independent of `title` so
    /// re-wording a card's copy can never change its element identity.
    id: &'static str,
    title: &'static str,
    finding: Option<String>,
}

pub(crate) fn render(
    state: &mut InsightsState,
    capture: Option<&gpui::Capture>,
    capture_generation: u64,
    frame_durations_ms: &[f32],
    frame_durations_max_ms: f32,
    _window: &mut gpui::Window,
    cx: &mut Context<ProfilerPanel>,
) -> AnyElement {
    let has_data = capture.is_some() && !frame_durations_ms.is_empty();

    let (worst_ms, mean_ms, janky_count) = if has_data {
        let worst = frame_durations_max_ms;
        let mean = frame_durations_ms.iter().sum::<f32>() / frame_durations_ms.len() as f32;
        let janky = frame_durations_ms.iter().filter(|ms| **ms > JANK_MS).count();
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
            worst_ms.is_some_and(|v| v <= JANK_MS),
            has_data,
            cx,
        ))
        .child(headline_metric(
            "Mean frame",
            mean_ms.map(|v| format!("{v:.1} ms")),
            mean_ms.is_some_and(|v| v <= JANK_MS),
            has_data,
            cx,
        ))
        .child(headline_metric(
            "Janky frames",
            has_data.then(|| janky_count.to_string()),
            janky_count == 0,
            has_data,
            cx,
        ));

    let body = if !has_data {
        div()
            .p_2()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child("Start and stop a capture to see insights.")
            .into_any_element()
    } else {
        let all_insights = capture
            .map(|capture| cached_insights(state, capture, capture_generation))
            .unwrap_or_default();
        let (active, passed): (Vec<Insight>, Vec<Insight>) = all_insights
            .iter()
            .cloned()
            .partition(|i| i.finding.is_some());

        let mut list = v_flex().id("record-insights-active").w_full();
        if active.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .py_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Nothing flagged in this capture — see Passed insights below."),
            );
        } else {
            list = list.children(active.into_iter().map(|insight| render_active_card(insight, cx)));
        }
        if !passed.is_empty() {
            list = list
                .child(render_passed_header(passed.len(), state.passed_expanded, cx))
                .when(state.passed_expanded, |list| {
                    list.children(passed.into_iter().map(|insight| render_passed_row(insight, cx)))
                });
        }
        list.into_any_element()
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
                .child(body),
        )
        .into_any_element()
}

/// One headline metric: a small muted label over a larger value, colored
/// green when it's within the "good" range and the theme's danger color
/// when it isn't — matching the reference's `LCP`/`INP`/`CLS` strip
/// (`label` on top, `value` big and green-when-good below) rather than the
/// v1's same-size label/value pair.
fn headline_metric(
    label: &'static str,
    value: Option<String>,
    good: bool,
    has_data: bool,
    cx: &Context<ProfilerPanel>,
) -> AnyElement {
    v_flex()
        .gap_0p5()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .when(has_data && good, |d| d.text_color(cx.theme().success))
                .when(has_data && !good, |d| d.text_color(cx.theme().danger))
                .when(!has_data, |d| d.text_color(cx.theme().muted_foreground))
                .child(value.unwrap_or_else(|| "\u{2013}".to_string())),
        )
        .into_any_element()
}

/// Keeps `InsightsState::insights_cache` matched to the current
/// `capture_generation`, mirroring `record::recompute_for_selection`'s own
/// cache pattern — see `InsightsState::insights_cache`'s field doc for why.
fn cached_insights(
    state: &mut InsightsState,
    capture: &gpui::Capture,
    capture_generation: u64,
) -> std::rc::Rc<Vec<Insight>> {
    let cache_hit = state
        .insights_cache
        .as_ref()
        .is_some_and(|(gen, _)| *gen == capture_generation);
    if !cache_hit {
        state.insights_cache = Some((capture_generation, std::rc::Rc::new(build_insights(capture))));
    }
    state.insights_cache.as_ref().expect("just populated above on a cache miss").1.clone()
}

/// Builds this capture's full insight set — always the same fixed roster
/// (one card per meaningful `DiagnosticKind` cluster, plus long tasks), so
/// which cards end up "active" vs. folded into "Passed insights" is purely
/// a function of what this particular recording contains.
///
/// # Groupings, and why
///
/// - **Long tasks** — not a `DiagnosticKind` at all; built from
///   `cpu_spans` the same way the overview strip's own long-task hatching
///   is (`super::data::LONG_TASK_NS`).
/// - **Window resize churn** — `ResizeEvent` (the native notification) +
///   `ResizeHandling` (the handler it triggers) read together, since a
///   `ResizeHandling` span with no `ResizeEvent` (or vice versa) isn't a
///   scenario that occurs in practice — they're two facts about the same
///   moment.
/// - **Viewport bounds churn** — `BoundsChanged` alone; unlike the pair
///   above, bounds can change independent of a native resize (moving a
///   window across displays with different scale factors, an embedder
///   calling `set_bounds` directly), so it earns its own card.
/// - **Invalidation volume** — `RefreshRequested` alone, read as a ratio
///   against frame count so it scales with capture length instead of using
///   a fixed magic number.
/// - **GPU surface reconfiguration** — `SurfaceReconfigured` +
///   `DrawableResized`, the GPU-side half of a resize (swapchain +
///   size-dependent textures), same reasoning as the window-resize pairing
///   above.
/// - **Fast-path present rate** — `FramePresented` vs.
///   `FastFramePresented` read as a ratio: what fraction of presented
///   frames took the cheap framebuffer-only path vs. a full compositor
///   draw.
/// - **Engine viewport resize churn** — `EngineResize` alone. Distinct
///   from the window's own resize churn: an embedded engine viewport
///   (e.g. a hosted 3D/game view) can resize independent of the window
///   that hosts it, most often from a panel being dragged.
/// - **Engine scene re-sync rate** — `EngineFrame` vs. `EngineSceneSync`
///   read as a ratio: what fraction of engine frames forced a full
///   SceneDB-to-renderer sync rather than reusing the last one.
/// - **Custom diagnostics** — `User`, the generic embedder-supplied kind;
///   any occurrence is worth a look since it's app-authored, not something
///   the framework itself emits routinely.
fn build_insights(capture: &gpui::Capture) -> Vec<Insight> {
    #[derive(Default, Clone, Copy)]
    struct Agg {
        count: u32,
        duration_ns: u64,
    }

    let mut resize_event = Agg::default();
    let mut resize_handling = Agg::default();
    let mut bounds_changed = Agg::default();
    let mut refresh_requested = Agg::default();
    let mut surface_reconfigured = Agg::default();
    let mut drawable_resized = Agg::default();
    let mut frame_presented = Agg::default();
    let mut fast_frame_presented = Agg::default();
    let mut engine_frame = Agg::default();
    let mut engine_resize = Agg::default();
    let mut engine_scene_sync = Agg::default();
    let mut user = Agg::default();
    let mut long_task_frames = 0u32;

    for frame in capture.frames() {
        for event in &frame.diagnostics {
            let agg = match event.kind {
                gpui::DiagnosticKind::ResizeEvent => &mut resize_event,
                gpui::DiagnosticKind::ResizeHandling => &mut resize_handling,
                gpui::DiagnosticKind::BoundsChanged => &mut bounds_changed,
                gpui::DiagnosticKind::RefreshRequested => &mut refresh_requested,
                gpui::DiagnosticKind::SurfaceReconfigured => &mut surface_reconfigured,
                gpui::DiagnosticKind::DrawableResized => &mut drawable_resized,
                gpui::DiagnosticKind::FramePresented => &mut frame_presented,
                gpui::DiagnosticKind::FastFramePresented => &mut fast_frame_presented,
                gpui::DiagnosticKind::EngineFrame => &mut engine_frame,
                gpui::DiagnosticKind::EngineResize => &mut engine_resize,
                gpui::DiagnosticKind::EngineSceneSync => &mut engine_scene_sync,
                gpui::DiagnosticKind::User => &mut user,
            };
            agg.count += 1;
            agg.duration_ns += event.duration_ns;
        }
        if frame
            .cpu_spans
            .iter()
            .any(|s| s.depth == 0 && s.duration_ns as u64 >= super::data::LONG_TASK_NS)
        {
            long_task_frames += 1;
        }
    }

    let frame_count = capture.frame_count() as u32;
    let ms = |ns: u64| ns as f32 / 1_000_000.0;

    vec![
        Insight {
            id: "long-tasks",
            title: "Long tasks",
            finding: (long_task_frames > 0).then(|| {
                format!(
                    "{long_task_frames} frame(s) had a top-level span over 50ms — see the \
                     overview strip's red hatch marks, or the Bottom-up tab for which activity \
                     dominated."
                )
            }),
        },
        Insight {
            id: "resize-churn",
            title: "Window resize churn",
            finding: (resize_event.count > 2).then(|| {
                format!(
                    "{} native resize notification(s) came through this capture, costing {:.1} \
                     ms total in the resize handler. A drag-resize gesture naturally produces a \
                     burst of these; this many outside one deliberate resize is worth a look \
                     (window-manager churn, DPI-change spam, or a layout loop re-triggering its \
                     own resize).",
                    resize_event.count,
                    ms(resize_handling.duration_ns),
                )
            }),
        },
        Insight {
            id: "bounds-churn",
            title: "Viewport bounds churn",
            finding: (bounds_changed.count > 3).then(|| {
                format!(
                    "{} `Window::bounds_changed` notification(s) were recorded, each scheduling \
                     its own refresh independent of a resize event. Frequent bounds changes \
                     without a matching resize usually mean the window moved across displays \
                     with different scale factors, or an embedder is calling `set_bounds` more \
                     than it needs to.",
                    bounds_changed.count,
                )
            }),
        },
        Insight {
            id: "invalidation-volume",
            title: "Invalidation volume",
            finding: {
                let per_frame = if frame_count > 0 {
                    refresh_requested.count as f32 / frame_count as f32
                } else {
                    0.0
                };
                (frame_count > 0 && per_frame > 3.0).then(|| {
                    format!(
                        "{} `Window::refresh` call(s) were recorded across {} frame(s) — {:.1}x \
                         more than one per frame on average. Multiple refreshes landing in the \
                         same frame usually means something is invalidating more state than it \
                         needs to; the Diagnostics tab lists each one with its resulting \
                         invalidation state.",
                        refresh_requested.count, frame_count, per_frame,
                    )
                })
            },
        },
        Insight {
            id: "swapchain-reconfig",
            title: "GPU surface reconfiguration",
            finding: (surface_reconfigured.count + drawable_resized.count > 2).then(|| {
                format!(
                    "{} surface reconfiguration(s) and {} drawable resize(s) recreated GPU-side \
                     surface state — one of the more expensive things this profiler can observe. \
                     More than the one pair expected at capture start usually tracks 1:1 with a \
                     window resize; see Window resize churn above.",
                    surface_reconfigured.count, drawable_resized.count,
                )
            }),
        },
        Insight {
            id: "fast-path-rate",
            title: "Fast-path present rate",
            finding: {
                let total = frame_presented.count + fast_frame_presented.count;
                let fast_pct = if total > 0 {
                    fast_frame_presented.count as f32 / total as f32 * 100.0
                } else {
                    0.0
                };
                (total >= 20 && fast_pct < 10.0).then(|| {
                    format!(
                        "{}/{} presented frame(s) ({:.0}%) used the framebuffer-only fast path; \
                         the rest ran a full compositor draw. A low fast-path rate across an \
                         otherwise idle capture can mean something is marking the whole scene \
                         dirty more often than it needs to.",
                        fast_frame_presented.count, total, fast_pct,
                    )
                })
            },
        },
        Insight {
            id: "engine-resize-churn",
            title: "Engine viewport resize churn",
            finding: (engine_resize.count > 2).then(|| {
                format!(
                    "{} embedded-engine viewport resize(s) were recorded — each one re-derives \
                     GPU render targets for that viewport, independent of the window's own \
                     resize handling. Frequent engine-viewport resizes usually track a panel \
                     being dragged.",
                    engine_resize.count,
                )
            }),
        },
        Insight {
            id: "engine-scene-sync",
            title: "Engine scene re-sync rate",
            finding: {
                let sync_pct = if engine_frame.count > 0 {
                    engine_scene_sync.count as f32 / engine_frame.count as f32 * 100.0
                } else {
                    0.0
                };
                (engine_frame.count >= 5 && sync_pct > 80.0).then(|| {
                    format!(
                        "{}/{} embedded-engine frame(s) ({:.0}%) triggered a full \
                         SceneDB-to-renderer sync. If the embedded scene isn't actually changing \
                         every frame, that ratio should be much lower — each sync re-walks the \
                         whole scene graph rather than reusing the last one.",
                        engine_scene_sync.count, engine_frame.count, sync_pct,
                    )
                })
            },
        },
        Insight {
            id: "user-diagnostics",
            title: "Custom diagnostics",
            finding: (user.count > 0).then(|| {
                format!(
                    "{} app-supplied diagnostic event(s) were recorded via the generic `User` \
                     kind — see the Diagnostics tab for their raw payloads.",
                    user.count,
                )
            }),
        },
    ]
}

/// One insight with an actual finding: rendered at full visual weight,
/// unboxed and unbordered — a dense list row, not a bordered card, to
/// match the reference's own minimal density (see the module doc's
/// screenshots).
fn render_active_card(insight: Insight, cx: &Context<ProfilerPanel>) -> AnyElement {
    v_flex()
        .id(SharedString::from(format!("insight-{}", insight.id)))
        .w_full()
        .gap_1()
        .px_2()
        .py_2()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(cx.theme().foreground)
                .child(insight.title),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(insight.finding.unwrap_or_default()),
        )
        .into_any_element()
}

/// The collapsed/expandable "Passed insights (N)" group header — click to
/// toggle. The count is always current (computed from the live partition
/// in [`render`], not cached), so it updates the moment a fresh capture
/// changes which insights have findings.
fn render_passed_header(count: usize, expanded: bool, cx: &mut Context<ProfilerPanel>) -> AnyElement {
    h_flex()
        .id("record-insights-passed-toggle")
        .w_full()
        .items_center()
        .gap_1()
        .px_2()
        .py_1p5()
        .cursor_pointer()
        .hover(|s| s.bg(cx.theme().list_hover))
        .child(
            Icon::new(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .xsmall()
            .text_color(cx.theme().muted_foreground),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format!("Passed insights ({count})")),
        )
        .on_click(cx.listener(|panel, _event, _window, cx| {
            panel.record.insights.passed_expanded = !panel.record.insights.passed_expanded;
            cx.notify();
        }))
        .into_any_element()
}

/// One insight with nothing to flag, shown only while the "Passed
/// insights" group is expanded — title only, smaller and dimmer than an
/// active card, indented under the group header's disclosure icon.
fn render_passed_row(insight: Insight, cx: &Context<ProfilerPanel>) -> AnyElement {
    div()
        .id(SharedString::from(format!("insight-passed-{}", insight.id)))
        .w_full()
        .pl_6()
        .pr_2()
        .py_1p5()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(insight.title)
        .into_any_element()
}

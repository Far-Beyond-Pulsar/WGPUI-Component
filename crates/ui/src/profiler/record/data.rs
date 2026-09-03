//! Pure data-preparation for the Record tab: no GPUI rendering, no UI state,
//! just `Capture`/`FrameCapture` → the plain structs every rendering
//! submodule under `profiler::record` reads. Kept separate from those
//! submodules (and computed once centrally in [`RecordState::on_capture_stopped`]
//! / [`RecordState::recompute_for_selection`], not per-render) for the same
//! reason `ProfilerPanel::counter_summary`/`frame_durations_ms` are
//! computed once when a capture stops rather than re-aggregated from every
//! recorded frame on every render: panning, hovering, or dragging a
//! selection re-renders this tab constantly without the underlying capture
//! ever changing.
//!
//! This is a straight relocation of logic that lived inline in
//! `profiler/mod.rs` before the Record tab got its own module tree — the
//! bucketing/self-time algorithms themselves aren't new.

use std::rc::Rc;

use gpui::SharedString;

use crate::profiler::{
    build_flame_lanes_with_resolver, span_name_label_resolved, FlameBar, FlameLane,
};

/// Every [`gpui::SpanCategory`] variant, in a fixed order — the index into
/// this array is the index into [`OverviewBucket`]'s per-category totals.
/// `category_index`/`category_color` both key off this same order so the two
/// can never disagree about which slot belongs to which category.
pub(crate) const OVERVIEW_CATEGORIES: [gpui::SpanCategory; 8] = [
    gpui::SpanCategory::WindowFrame,
    gpui::SpanCategory::ElementRequestLayout,
    gpui::SpanCategory::ElementPrepaint,
    gpui::SpanCategory::ElementPaint,
    gpui::SpanCategory::BackgroundTask,
    gpui::SpanCategory::GpuRenderPass,
    gpui::SpanCategory::GpuSubmitPresent,
    gpui::SpanCategory::UserDefined,
];

pub(crate) fn category_index(category: gpui::SpanCategory) -> usize {
    OVERVIEW_CATEGORIES
        .iter()
        .position(|c| *c == category)
        .unwrap_or(0)
}

/// One time-bucket of the overview strip's CPU activity graph: how much
/// depth-0 span time fell in `[start_ns, start_ns + bucket width)`, broken
/// down by category so the overview can render a true **stacked** area
/// (Chrome's own CPU graph is a layered/stacked area chart, not a
/// dominant-color-only bar — see `.agents/PROFILER_UI_SPEC.md` §2.2) rather
/// than collapsing each bucket to a single color.
#[derive(Clone)]
pub(crate) struct OverviewBucket {
    pub(crate) total_ns: u64,
    pub(crate) category_ns: [u64; OVERVIEW_CATEGORIES.len()],
    /// True if any depth-0 span overlapping this bucket exceeded the
    /// long-task threshold ([`LONG_TASK_NS`]) — drives the red hatch marks
    /// along the top edge of the overview's CPU graph (§2.2) and the detail
    /// flame chart's Task blocks (§3), matching Chrome's own long-task
    /// warning channel, which is deliberately orthogonal to category color.
    pub(crate) has_long_task: bool,
}

impl OverviewBucket {
    pub(crate) fn dominant_category(&self) -> Option<gpui::SpanCategory> {
        self.category_ns
            .iter()
            .enumerate()
            .max_by_key(|(_, ns)| **ns)
            .filter(|(_, ns)| **ns > 0)
            .map(|(index, _)| OVERVIEW_CATEGORIES[index])
    }
}

/// A depth-0 span running at least this long is a "long task" — matches the
/// browser-devtools convention (50ms) that Chrome's own long-task hatching
/// uses.
pub(crate) const LONG_TASK_NS: u64 = 50_000_000;

/// Cached overview data for the whole capture window — see
/// [`RecordState::overview`]'s field doc for why this is computed once
/// rather than every render.
pub(crate) struct RecordOverview {
    pub(crate) buckets: Vec<OverviewBucket>,
    pub(crate) domain_start_ns: u64,
    pub(crate) domain_end_ns: u64,
    pub(crate) max_bucket_ns: u64,
}

/// How many buckets the overview bar chart is divided into, regardless of
/// the capture's actual duration — enough resolution to see the shape of a
/// multi-second recording without the per-render cost scaling with frame
/// count once buckets exist (bucketing itself is still `O(spans)`; only the
/// *rendering* cost is bounded by this).
pub(crate) const OVERVIEW_BUCKET_COUNT: usize = 400;

/// Bucket every depth-0 CPU span (the outermost span active at any instant,
/// which — without needing full call-tree reconstruction — gives one
/// representative "what phase was this moment in" signal) across the whole
/// capture into [`OVERVIEW_BUCKET_COUNT`] fixed-width time buckets. A span
/// straddling a bucket boundary contributes to every bucket it overlaps,
/// split proportionally to the overlap — not just whichever bucket it
/// started in — so a long span doesn't make its own bucket look busy while
/// the buckets it also covers look empty.
pub(crate) fn build_overview(frames: &[&gpui::FrameCapture]) -> Option<RecordOverview> {
    let domain_start_ns = frames.first()?.frame_start_ns;
    let domain_end_ns = frames.last()?.frame_end_ns.max(domain_start_ns + 1);
    let span_ns = (domain_end_ns - domain_start_ns).max(1);
    let bucket_ns = (span_ns / OVERVIEW_BUCKET_COUNT as u64).max(1);
    let bucket_count = ((span_ns / bucket_ns) as usize + 1).min(OVERVIEW_BUCKET_COUNT * 2);

    let mut buckets: Vec<OverviewBucket> = (0..bucket_count)
        .map(|_| OverviewBucket {
            total_ns: 0,
            category_ns: [0; OVERVIEW_CATEGORIES.len()],
            has_long_task: false,
        })
        .collect();

    for frame in frames {
        for span in &frame.cpu_spans {
            if span.depth != 0 {
                continue;
            }
            let start = span.start_ns.max(domain_start_ns);
            let end = span
                .start_ns
                .saturating_add(span.duration_ns as u64)
                .min(domain_end_ns);
            if end <= start {
                continue;
            }
            let is_long_task = span.duration_ns as u64 >= LONG_TASK_NS;
            let first_bucket = ((start - domain_start_ns) / bucket_ns) as usize;
            let last_bucket = (((end - domain_start_ns).saturating_sub(1)) / bucket_ns) as usize;
            let category = category_index(span.category);
            for bucket_index in first_bucket..=last_bucket.min(bucket_count - 1) {
                let bucket_start = domain_start_ns + bucket_index as u64 * bucket_ns;
                let bucket_end = bucket_start + bucket_ns;
                let overlap_start = start.max(bucket_start);
                let overlap_end = end.min(bucket_end);
                if overlap_end > overlap_start {
                    let overlap_ns = overlap_end - overlap_start;
                    let bucket = &mut buckets[bucket_index];
                    bucket.total_ns += overlap_ns;
                    bucket.category_ns[category] += overlap_ns;
                    bucket.has_long_task |= is_long_task;
                }
            }
        }
    }

    let max_bucket_ns = buckets.iter().map(|b| b.total_ns).max().unwrap_or(1).max(1);

    Some(RecordOverview {
        buckets,
        domain_start_ns,
        domain_end_ns,
        max_bucket_ns,
    })
}

/// Cached [`build_flame_lanes_for_range`] output, keyed by
/// `(capture_generation, range)` — see `record::flame`'s own cache field.
pub(crate) struct RecordLaneCache {
    pub(crate) capture_generation: u64,
    pub(crate) range: (u64, u64),
    pub(crate) lanes: Rc<Vec<FlameLane>>,
}

/// The multi-frame generalization of the single-frame lane builder: gathers
/// every span across *every* frame in `capture` whose time range intersects
/// `[start_ns, end_ns)`, and lanes them exactly like a single-frame build
/// would (one CPU lane, one lane per background thread, one GPU lane) —
/// spans already carry capture-wide absolute `start_ns` values, so no
/// time-rebasing is needed to concatenate spans from different frames onto
/// one axis.
pub(crate) fn build_flame_lanes_for_range(
    capture: &gpui::Capture,
    start_ns: u64,
    end_ns: u64,
) -> Vec<FlameLane> {
    let intersects = |span_start: u64, span_duration_ns: u32| {
        let span_end = span_start.saturating_add(span_duration_ns as u64);
        span_end >= start_ns && span_start <= end_ns
    };

    let mut cpu: Vec<gpui::CpuSpan> = Vec::new();
    let mut background_by_thread: Vec<(gpui::ThreadKey, Vec<gpui::CpuSpan>)> = Vec::new();
    let mut gpu: Vec<gpui::GpuSpan> = Vec::new();

    for frame in capture.frames() {
        if frame.frame_end_ns < start_ns || frame.frame_start_ns > end_ns {
            continue;
        }
        for span in &frame.cpu_spans {
            if intersects(span.start_ns, span.duration_ns) {
                cpu.push(*span);
            }
        }
        for span in &frame.background_spans {
            if !intersects(span.start_ns, span.duration_ns) {
                continue;
            }
            if let Some(entry) = background_by_thread
                .iter_mut()
                .find(|(key, _)| *key == span.thread_id)
            {
                entry.1.push(*span);
            } else {
                background_by_thread.push((span.thread_id, vec![*span]));
            }
        }
        for span in &frame.gpu_spans {
            if intersects(span.start_ns, span.duration_ns) {
                gpu.push(*span);
            }
        }
    }

    let mut lanes = Vec::new();
    if !cpu.is_empty() {
        let max_depth = cpu.iter().map(|s| s.depth).max().unwrap_or(0);
        lanes.push(FlameLane {
            label: "Main Thread (CPU)".into(),
            bars: cpu
                .iter()
                .map(|span| {
                    FlameBar::from_cpu_with_label(span, span_name_label_resolved(capture, span.name))
                })
                .collect(),
            max_depth,
        });
    }
    for (index, (_key, spans)) in background_by_thread.iter().enumerate() {
        let max_depth = spans.iter().map(|s| s.depth).max().unwrap_or(0);
        lanes.push(FlameLane {
            label: format!("Background Thread {}", index + 1).into(),
            bars: spans
                .iter()
                .map(|span| {
                    FlameBar::from_cpu_with_label(span, span_name_label_resolved(capture, span.name))
                })
                .collect(),
            max_depth,
        });
    }
    if !gpu.is_empty() {
        lanes.push(FlameLane {
            label: "GPU".into(),
            bars: gpu
                .iter()
                .map(|span| {
                    FlameBar::from_gpu_with_label(span, span_name_label_resolved(capture, span.name))
                })
                .collect(),
            max_depth: 0,
        });
    }
    lanes
}

/// One row of the Bottom-up tab: every occurrence of one span *name* across
/// the selected range, aggregated regardless of which frame or call path
/// produced it.
#[derive(Clone)]
pub(crate) struct BottomUpRow {
    pub(crate) name: SharedString,
    pub(crate) category: Option<gpui::SpanCategory>,
    pub(crate) self_ns: u64,
    pub(crate) total_ns: u64,
    pub(crate) count: u32,
    /// `file:line` call site, when every occurrence of this name across the
    /// aggregated range agrees on exactly one ([`SourceAgg::One`]) — reuses
    /// the same `CpuSpan::element`/`ElementAttribution::source_location`
    /// `FlameBar::element_source` already reads for the detail flame chart,
    /// just aggregated across every occurrence instead of one bar. `None`
    /// when no occurrence carried element attribution at all (most
    /// framework-internal spans — `Commit`/`Layout`/`Paint`-style pipeline
    /// phases never have one, matching the reference screenshot, where those
    /// rows show no source suffix at all) *or* when occurrences disagreed
    /// (`SourceAgg::Many` — the same name produced by more than one call
    /// site; showing any single one of them would be misleading). No column
    /// number: `ElementAttribution::source_location` only carries
    /// `(file, line)`, unlike the reference's `file:line:col` — the closest
    /// this profiler's own instrumentation can get.
    pub(crate) source: Option<SharedString>,
}

/// Tracks whether every occurrence of one aggregated name agreed on a single
/// source location — see [`BottomUpRow::source`]'s field doc for why `Many`
/// collapses to `None` rather than picking one arbitrarily.
#[derive(Clone)]
enum SourceAgg {
    None,
    One(SharedString),
    Many,
}

impl SourceAgg {
    fn record(&mut self, source: Option<&SharedString>) {
        match (&self, source) {
            (SourceAgg::Many, _) | (_, None) => {}
            (SourceAgg::None, Some(s)) => *self = SourceAgg::One((*s).clone()),
            (SourceAgg::One(existing), Some(s)) => {
                if existing != s {
                    *self = SourceAgg::Many;
                }
            }
        }
    }

    fn into_option(self) -> Option<SharedString> {
        match self {
            SourceAgg::One(s) => Some(s),
            SourceAgg::None | SourceAgg::Many => None,
        }
    }
}

/// Builds the Bottom-up tab's rows for `[start_ns, end_ns)`.
///
/// Self-time needs the standard post-order stack algorithm, computed over
/// *every* span in each frame (not just the ones inside the selection) and
/// only folded into the aggregate afterward — filtering spans out first
/// would corrupt the parent/child accounting for a span that straddles the
/// selection boundary. `gpui::CpuSpan::depth`'s doc comment guarantees
/// `cpu_spans` arrives in completion order (children before their parent),
/// which is exactly what this needs: for a span completing now at depth
/// `d`, whatever total duration has already accumulated at
/// `child_ns_by_depth[d + 1]` is its direct children's total (they, being
/// deeper, necessarily completed first), so `self = duration - that`; the
/// span's own duration then becomes a child contribution to whatever
/// completes next at depth `d - 1`.
pub(crate) fn build_bottom_up_rows(
    capture: &gpui::Capture,
    start_ns: u64,
    end_ns: u64,
) -> Vec<BottomUpRow> {
    let mut agg: std::collections::HashMap<
        SharedString,
        (u64, u64, u32, Option<gpui::SpanCategory>, SourceAgg),
    > = std::collections::HashMap::new();

    for frame in capture.frames() {
        let mut child_ns_by_depth: Vec<u64> = Vec::new();
        for span in &frame.cpu_spans {
            let depth = span.depth as usize;
            if child_ns_by_depth.len() <= depth + 1 {
                child_ns_by_depth.resize(depth + 2, 0);
            }
            let child_ns = child_ns_by_depth[depth + 1];
            let duration_ns = span.duration_ns as u64;
            let self_ns = duration_ns.saturating_sub(child_ns);
            child_ns_by_depth[depth + 1] = 0;
            child_ns_by_depth[depth] += duration_ns;

            let span_end = span.start_ns.saturating_add(duration_ns);
            if span_end < start_ns || span.start_ns > end_ns {
                continue;
            }
            let name = span_name_label_resolved(capture, span.name);
            // Same `(file, line)` `FlameBar::from_cpu_with_label` reads for
            // the detail flame chart's own `element_source` — see
            // `BottomUpRow::source`'s field doc for why it's folded down to
            // "exactly one, across every occurrence" rather than kept
            // per-occurrence.
            let span_source = span
                .element
                .and_then(|e| e.source_location)
                .map(|(file, line)| SharedString::from(format!("{file}:{line}")));
            let entry = agg
                .entry(name)
                .or_insert((0, 0, 0, Some(span.category), SourceAgg::None));
            entry.0 += self_ns;
            entry.1 += duration_ns;
            entry.2 += 1;
            entry.4.record(span_source.as_ref());
        }
    }

    agg.into_iter()
        .map(
            |(name, (self_ns, total_ns, count, category, source_agg))| BottomUpRow {
                name,
                category,
                self_ns,
                total_ns,
                count,
                source: source_agg.into_option(),
            },
        )
        .collect()
}

pub(crate) fn ns_to_ms_string(ns: u64) -> String {
    format!("{:.2}", ns as f64 / 1.0e6)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_cpu_span(
        start_ns: u64,
        duration_ns: u32,
        depth: u16,
        category: gpui::SpanCategory,
    ) -> gpui::CpuSpan {
        gpui::CpuSpan {
            name: gpui::SpanName::Static("test"),
            category,
            depth,
            start_ns,
            duration_ns,
            thread_id: gpui::ThreadKey::from_raw(0),
            element: None,
        }
    }

    fn sample_frame(frame_index: u64, spans: Vec<gpui::CpuSpan>) -> gpui::FrameCapture {
        let frame_start_ns = spans.iter().map(|s| s.start_ns).min().unwrap_or(0);
        let frame_end_ns = spans
            .iter()
            .map(|s| s.start_ns + s.duration_ns as u64)
            .max()
            .unwrap_or(frame_start_ns + 1);
        gpui::FrameCapture {
            frame_index,
            window_id: 0,
            cpu_spans: spans,
            background_spans: Vec::new(),
            diagnostics: Vec::new(),
            gpu_spans: Vec::new(),
            gpu_spans_finalized: true,
            gpu_spans_truncated: false,
            frame_start_ns,
            frame_end_ns,
            cpu_gpu_submit_ns: None,
            cpu_gpu_fence_observed_ns: None,
            counters: gpui::FrameCounters::default(),
        }
    }

    #[test]
    fn build_overview_buckets_a_single_depth_zero_span() {
        let frame = sample_frame(
            0,
            vec![sample_cpu_span(
                0,
                1_000_000,
                0,
                gpui::SpanCategory::ElementPaint,
            )],
        );
        let overview = build_overview(&[&frame]).expect("overview");
        assert_eq!(overview.domain_start_ns, 0);
        let total: u64 = overview.buckets.iter().map(|b| b.total_ns).sum();
        assert_eq!(total, 1_000_000);
    }

    #[test]
    fn build_overview_flags_long_tasks() {
        let frame = sample_frame(
            0,
            vec![sample_cpu_span(
                0,
                LONG_TASK_NS as u32 + 1,
                0,
                gpui::SpanCategory::ElementPaint,
            )],
        );
        let overview = build_overview(&[&frame]).expect("overview");
        assert!(overview.buckets.iter().any(|b| b.has_long_task));
    }

    #[test]
    fn build_overview_ignores_non_depth_zero_spans() {
        let frame = sample_frame(
            0,
            vec![sample_cpu_span(
                0,
                1_000_000,
                1,
                gpui::SpanCategory::ElementPaint,
            )],
        );
        let overview = build_overview(&[&frame]).expect("overview");
        let total: u64 = overview.buckets.iter().map(|b| b.total_ns).sum();
        assert_eq!(total, 0);
    }

    #[test]
    fn dominant_category_picks_the_largest_slice() {
        let mut bucket = OverviewBucket {
            total_ns: 300,
            category_ns: [0; OVERVIEW_CATEGORIES.len()],
            has_long_task: false,
        };
        bucket.category_ns[category_index(gpui::SpanCategory::ElementPaint)] = 100;
        bucket.category_ns[category_index(gpui::SpanCategory::ElementRequestLayout)] = 200;
        assert_eq!(
            bucket.dominant_category(),
            Some(gpui::SpanCategory::ElementRequestLayout)
        );
    }

    #[test]
    fn dominant_category_is_none_for_an_empty_bucket() {
        let bucket = OverviewBucket {
            total_ns: 0,
            category_ns: [0; OVERVIEW_CATEGORIES.len()],
            has_long_task: false,
        };
        assert_eq!(bucket.dominant_category(), None);
    }

    // `build_bottom_up_rows` itself isn't unit-tested here: `gpui::Capture`
    // has no public constructor outside a live capture session (see
    // `CaptureHandle::stop`), and both `build_bottom_up_rows` and
    // `build_flame_lanes_for_range` need a real `&Capture` (not just
    // `&[FrameCapture]`) for `span_name_label_resolved`'s interned-string
    // table, so there's no seam to unit-test the self-time algorithm in
    // isolation without a live capture. Covered at the integration level
    // instead, same as `build_flame_lanes_for_range`.

    #[test]
    fn ns_to_ms_string_formats_two_decimals() {
        assert_eq!(ns_to_ms_string(1_500_000), "1.50");
    }

    // `SourceAgg` *is* a pure seam (unlike the rest of `build_bottom_up_rows`,
    // see the comment above) — no `Capture` needed, just `SharedString`s — so
    // its three-way "no source seen yet / exactly one / disagreed" collapse
    // gets real coverage even though the aggregation loop it's used from
    // doesn't.
    #[test]
    fn source_agg_stays_none_with_no_occurrences_carrying_a_source() {
        let mut agg = SourceAgg::None;
        agg.record(None);
        agg.record(None);
        assert!(agg.into_option().is_none());
    }

    #[test]
    fn source_agg_resolves_to_the_one_source_every_occurrence_agreed_on() {
        let mut agg = SourceAgg::None;
        let loc: SharedString = "foo.rs:12".into();
        agg.record(Some(&loc));
        agg.record(Some(&loc));
        agg.record(None); // an occurrence with no attribution doesn't count as a conflict.
        assert_eq!(agg.into_option(), Some(loc));
    }

    #[test]
    fn source_agg_collapses_to_none_when_occurrences_disagree() {
        let mut agg = SourceAgg::None;
        agg.record(Some(&"foo.rs:12".into()));
        agg.record(Some(&"bar.rs:34".into()));
        assert!(agg.into_option().is_none());
        // Once collapsed, a later occurrence that *would* have agreed with
        // the first still doesn't resurrect it — "Many" is a one-way trip,
        // not "whichever source most occurrences had".
        let mut agg2 = SourceAgg::One("foo.rs:12".into());
        agg2.record(Some(&"bar.rs:34".into()));
        agg2.record(Some(&"foo.rs:12".into()));
        assert!(agg2.into_option().is_none());
    }
}

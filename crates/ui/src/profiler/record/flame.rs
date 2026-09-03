//! The detail flame chart (§3 of `.agents/PROFILER_UI_SPEC.md`): the
//! zoomed-in, interactive view of whichever range is currently selected on
//! the overview strip.
//!
//! # v1 note, honestly
//!
//! This delegates straight to `ProfilerPanel::render_flame_lanes_body` —
//! the *same* GPU-instanced renderer (`FlameBarPipeline`/`BarInstance`,
//! `profiler_flame_shader.wgsl`), hover tooltip, click-to-select and search
//! the plain Flame Chart tab already uses, over the multi-frame `lanes`
//! this tab builds for the selected range instead of a single frame. That
//! gets pan/zoom/hover/search and GPU rendering for free, correctly, from
//! code that's already shipped and tested — but it does **not** yet match
//! the reference's visual density: no `Task`/`Layout`/`Commit`-style
//! labeled top-level blocks, no red long-task hatch on `Task` blocks (the
//! overview strip already has this — `super::data::OverviewBucket::has_long_task`
//! — the detail chart doesn't yet), no icon gutter per row. `FlameBar`
//! already carries `category`/`gpu_pass_kind`/`element_type` per bar (see
//! that struct), which is what a fuller rendering would key its per-depth
//! block styling and gutter icon off of.
//!
//! Deliberately shares `ProfilerPanel::flame_zoom`/`flame_lane_gpu`/etc.
//! with the plain Flame Chart tab rather than forking that state: the two
//! sections render mutually exclusively (see `ProfilerPanel::render`), so
//! nothing is ever fighting over it at once.

use gpui::{AnyElement, Context, Window};

use crate::profiler::{FlameLane, ProfilerPanel};

#[derive(Default)]
pub(crate) struct FlameState;

pub(crate) fn render(
    panel: &mut ProfilerPanel,
    lanes: &[FlameLane],
    window: &mut Window,
    cx: &mut Context<ProfilerPanel>,
) -> AnyElement {
    panel.render_flame_lanes_body(lanes, None, None, window, cx)
}

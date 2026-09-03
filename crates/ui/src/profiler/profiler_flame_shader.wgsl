// Instanced flame-chart bar renderer, used by `ProfilerPanel`'s GPU-rendered
// flame chart in `profiler.rs`.
//
// One instance per visible span bar. A single draw call renders every bar in
// a lane, replacing what used to be one GPUI `div()` per bar (a stateful,
// hit-test-registered, tooltip-bearing, text-shaping element for every span
// -- the thing that made a real capture with thousands of spans bring the
// whole app to its knees).
//
// This is deliberately much smaller than GPUI's own general-purpose
// `quads.wgsl`: solid fill with a small corner radius and a `highlight`
// factor for the hovered/selected bar (see `BarInstance` in `profiler.rs`).
// No gradients, borders, or content masks -- the flame chart doesn't need
// them, and every bar in a lane shares one pipeline + one instance buffer.

struct Viewport {
    // `.xy` is the surface size in physical pixels; `.zw` is unused padding
    // (uniform buffers must be 16-byte aligned).
    size: vec2<f32>,
    _pad: vec2<f32>,
}

struct BarInstance {
    rect_min: vec2<f32>,
    rect_max: vec2<f32>,
    color: vec4<f32>,
    corner_radius: f32,
    // > 0.5 brightens the bar -- used for the hovered/selected span, a
    // CPU-computed per-instance flag instead of a per-bar click listener.
    highlight: f32,
    _pad: vec2<f32>,
}

@group(0) @binding(0) var<uniform> viewport: Viewport;
@group(0) @binding(1) var<storage, read> bars: array<BarInstance>;

struct VaryingOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) local_pos: vec2<f32>,
    @location(2) half_size: vec2<f32>,
    @location(3) corner_radius: f32,
    @location(4) highlight: f32,
}

@vertex
fn vs_bar(@builtin(vertex_index) vertex_id: u32, @builtin(instance_index) instance_id: u32) -> VaryingOutput {
    let bar = bars[instance_id];
    // Triangle-strip quad: vertex 0=(0,0) 1=(1,0) 2=(0,1) 3=(1,1).
    let unit = vec2<f32>(f32(vertex_id & 1u), f32((vertex_id >> 1u) & 1u));
    let size = max(bar.rect_max - bar.rect_min, vec2<f32>(0.0, 0.0));
    let pixel_pos = bar.rect_min + unit * size;

    var out: VaryingOutput;
    let ndc_x = (pixel_pos.x / viewport.size.x) * 2.0 - 1.0;
    // Pixel space is y-down (origin top-left); clip space is y-up.
    let ndc_y = 1.0 - (pixel_pos.y / viewport.size.y) * 2.0;
    out.position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.color = bar.color;
    out.local_pos = unit * size - size * 0.5;
    out.half_size = size * 0.5;
    out.corner_radius = bar.corner_radius;
    out.highlight = bar.highlight;
    return out;
}

// Signed distance field for an axis-aligned rounded box centered at the
// origin (Inigo Quilez's standard rounded-box SDF formulation).
fn rounded_box_sdf(p: vec2<f32>, half_size: vec2<f32>, radius: f32) -> f32 {
    let q = abs(p) - half_size + vec2<f32>(radius, radius);
    return length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

@fragment
fn fs_bar(input: VaryingOutput) -> @location(0) vec4<f32> {
    let radius = min(input.corner_radius, min(input.half_size.x, input.half_size.y));
    let dist = rounded_box_sdf(input.local_pos, input.half_size, radius);
    // ~1px antialiased edge, matching the AA threshold GPUI's own quad
    // shader uses (`quads.wgsl`'s `antialias_threshold`).
    let coverage = saturate(0.5 - dist);
    if coverage <= 0.0 {
        discard;
    }

    var color = input.color;
    if input.highlight > 0.5 {
        color = vec4<f32>(mix(color.rgb, vec3<f32>(1.0, 1.0, 1.0), 0.4), color.a);
    }
    // Straight (non-premultiplied) alpha output, matching this codebase's
    // convention for `WgpuSurface` content (see `flamegraph_replay.rs`'s
    // `premultiplied_alpha: 0`); the surface's own render pass blends with
    // standard alpha blending against a fully transparent clear.
    return vec4<f32>(color.rgb, color.a * coverage);
}

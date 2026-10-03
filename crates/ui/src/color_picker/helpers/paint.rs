use super::math::{barycentric, clamp01, hsv_to_rgb, triangle_vertices};
use super::*;
use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

const TRIANGLE_BAKE_INTERVAL: Duration = Duration::from_millis(33);

#[derive(Default)]
pub(crate) struct TriangleCache {
    baked: Option<Baked>,
    last_bake: Option<Instant>,
}

fn triangle_bake_due(
    previous: Option<BakeKey>,
    requested: BakeKey,
    elapsed: Duration,
    dragging: bool,
) -> bool {
    match previous {
        Some(old) if old == requested => false,
        Some(old)
            if old.width_px == requested.width_px
                && old.height_px == requested.height_px
                && old.scale_bits == requested.scale_bits =>
        {
            !dragging || elapsed >= TRIANGLE_BAKE_INTERVAL
        }
        _ => true,
    }
}

pub(crate) fn picker_geometry(bounds: Bounds<Pixels>) -> Option<PickerGeometry> {
    let width = bounds.size.width.as_f32();
    let height = bounds.size.height.as_f32();
    if width <= 0.0 || height <= 0.0 {
        return None;
    }

    let radius = width.min(height) * 0.5 - 2.0;
    let inner_r = (radius - HUE_RING_THICKNESS).max(12.0);

    Some(PickerGeometry {
        cx: bounds.origin.x.as_f32() + width * 0.5,
        cy: bounds.origin.y.as_f32() + height * 0.5,
        outer_r: radius,
        inner_r,
    })
}

// ---------------------------------------------------------------------------
// Baked wheel / triangle
//
// The wheel and the S/V triangle used to be re-tessellated on every paint: 360-960
// filled paths for the wheel, four stroke paths of up to ~1,900 points, and one
// filled path per sub-triangle for the S/V triangle (up to ~5,000). Every path is
// a scene primitive with its own `BoundsTree` insert, so an open picker added
// ~3,750 primitives (~6.8 ms of `frame: bounds tree` alone, ~10 ms of paint) to
// *every* UI frame -- and the UI redraws once per viewport frame.
//
// Their appearance only depends on the picker's pixel size (wheel) and on the hue
// (triangle), so each is rasterized once into an image and painted as a single
// primitive. Rasterizing per pixel also makes the gradients smooth instead of
// flat-shaded per sub-triangle.
// ---------------------------------------------------------------------------

/// Hue resolution used to key the triangle bake (0.25 degrees): a drag across the
/// hue ring re-bakes at most this often, and no visible difference results.
const HUE_KEY_STEPS: f32 = 1440.0;

#[derive(Clone, Copy, PartialEq)]
struct BakeKey {
    width_px: u32,
    height_px: u32,
    scale_bits: u32,
    hue_q: u32,
}

struct Baked {
    key: BakeKey,
    image: Arc<gpui::RenderImage>,
    /// Placement relative to the canvas origin, in logical pixels.
    offset: (f32, f32),
    size: (f32, f32),
}

struct SwatchBake {
    baked: Baked,
    cell: f32,
    gap: f32,
    colors: Vec<Vec<Hsla>>,
}

thread_local! {
    // Painting only ever happens on the UI thread, so a thread-local cache is
    // enough and needs no synchronization.
    static WHEEL_CACHE: RefCell<Option<Baked>> = const { RefCell::new(None) };
    static CHECKER_CACHE: RefCell<Option<Baked>> = const { RefCell::new(None) };
    static SWATCH_CACHE: RefCell<Option<SwatchBake>> = const { RefCell::new(None) };
}

fn bake_swatches(key: BakeKey, cell: f32, gap: f32, colors: &[Vec<Hsla>]) -> Option<Baked> {
    let scale = f32::from_bits(key.scale_bits);
    let mut out = vec![0; (key.width_px * key.height_px * 4) as usize];
    // Integrate each rectangle over the pixel footprint so fractional display
    // scales retain the one-logical-pixel borders and gaps.
    let overlap =
        |p: u32, low: f32, high: f32| ((p as f32 + 1.0).min(high) - (p as f32).max(low)).max(0.0);
    for (row, colors) in colors.iter().enumerate() {
        for (col, color) in colors.iter().enumerate() {
            let fill: gpui::Rgba = (*color).into();
            let border: gpui::Rgba = color.darken(0.1).into();
            let left = col as f32 * (cell + gap) * scale;
            let top = row as f32 * (cell + 1.0) * scale;
            let right = left + cell * scale;
            let bottom = top + cell * scale;
            for y in top.floor() as u32..(bottom.ceil() as u32).min(key.height_px) {
                for x in left.floor() as u32..(right.ceil() as u32).min(key.width_px) {
                    let outer = overlap(x, left, right) * overlap(y, top, bottom);
                    let inner = overlap(x, left + scale, right - scale)
                        * overlap(y, top + scale, bottom - scale);
                    let border_coverage = outer - inner;
                    let mix = |a: f32, b: f32| a * fill.a * inner + b * border.a * border_coverage;
                    put_bgra(
                        &mut out,
                        (y * key.width_px + x) as usize,
                        mix(fill.r, border.r),
                        mix(fill.g, border.g),
                        mix(fill.b, border.b),
                        fill.a * inner + border.a * border_coverage,
                    );
                }
            }
        }
    }
    Some(Baked {
        key,
        image: to_render_image(key.width_px, key.height_px, out)?,
        offset: (0.0, 0.0),
        size: (key.width_px as f32 / scale, key.height_px as f32 / scale),
    })
}

pub(crate) fn paint_swatch_grid(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    cell: f32,
    gap: f32,
    colors: &[Vec<Hsla>],
) {
    let scale = window.scale_factor();
    let key = BakeKey {
        width_px: (bounds.size.width.as_f32() * scale).ceil() as u32,
        height_px: (bounds.size.height.as_f32() * scale).ceil() as u32,
        scale_bits: scale.to_bits(),
        hue_q: 0,
    };
    if key.width_px == 0 || key.height_px == 0 {
        return;
    }
    SWATCH_CACHE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let valid = slot.as_ref().is_some_and(|s| {
            s.baked.key == key && s.cell == cell && s.gap == gap && s.colors == colors
        });
        if !valid {
            let Some(baked) = bake_swatches(key, cell, gap, colors) else {
                return;
            };
            if let Some(old) = slot.replace(SwatchBake {
                baked,
                cell,
                gap,
                colors: colors.to_vec(),
            }) {
                let _ = window.drop_image(old.baked.image);
            }
        }
        if let Some(s) = slot.as_ref() {
            let _ = window.paint_image(
                bounds,
                gpui::Corners::default(),
                s.baked.image.clone(),
                0,
                false,
            );
        }
    });
}

/// Geometry re-expressed relative to the canvas origin, so a bake stays valid
/// when the picker moves around the window.
fn local_geometry(bounds: Bounds<Pixels>, geometry: PickerGeometry) -> PickerGeometry {
    PickerGeometry {
        cx: geometry.cx - bounds.origin.x.as_f32(),
        cy: geometry.cy - bounds.origin.y.as_f32(),
        outer_r: geometry.outer_r,
        inner_r: geometry.inner_r,
    }
}

/// Write one premultiplied BGRA pixel (the byte order `RenderImage` expects).
#[inline]
fn put_bgra(out: &mut [u8], index: usize, r: f32, g: f32, b: f32, a: f32) {
    let o = index * 4;
    out[o] = (clamp01(b) * 255.0 + 0.5) as u8;
    out[o + 1] = (clamp01(g) * 255.0 + 0.5) as u8;
    out[o + 2] = (clamp01(r) * 255.0 + 0.5) as u8;
    out[o + 3] = (clamp01(a) * 255.0 + 0.5) as u8;
}

fn to_render_image(width: u32, height: u32, bgra: Vec<u8>) -> Option<Arc<gpui::RenderImage>> {
    // `RgbaImage` is only the byte container here; `RenderImage` frames are BGRA.
    let buffer = image::RgbaImage::from_raw(width, height, bgra)?;
    Some(Arc::new(gpui::RenderImage::new(smallvec::smallvec![
        image::Frame::new(buffer)
    ])))
}

fn bake_wheel(width_px: u32, height_px: u32, scale: f32, g: PickerGeometry) -> Option<Baked> {
    let aa = 1.0 / scale;
    let mut out = vec![0u8; (width_px * height_px * 4) as usize];
    // The four shading rings the vector version stroked over the wheel's edges:
    // (radius, alpha, line width).
    let rings = [
        (g.outer_r + 0.6, 0.18, 1.2),
        (g.outer_r - 0.4, 0.12, 0.9),
        (g.inner_r + 0.4, 0.16, 1.0),
        (g.inner_r - 0.4, 0.10, 0.8),
    ];
    let outer_reach = g.outer_r + 2.0;
    let inner_reach = g.inner_r - 2.0;

    for y in 0..height_px {
        let dy = (y as f32 + 0.5) / scale - g.cy;
        for x in 0..width_px {
            let dx = (x as f32 + 0.5) / scale - g.cx;
            let r = (dx * dx + dy * dy).sqrt();
            if r > outer_reach || r < inner_reach {
                continue;
            }

            let coverage =
                clamp01((g.outer_r - r) / aa + 0.5) * clamp01((r - g.inner_r) / aa + 0.5);
            let (mut cr, mut cg, mut cb, mut ca) = (0.0, 0.0, 0.0, 0.0);
            if coverage > 0.0 {
                let t = ((dy.atan2(dx) + std::f32::consts::FRAC_PI_2) / std::f32::consts::TAU)
                    .rem_euclid(1.0);
                let (hr, hg, hb) = hsv_to_rgb(t, 1.0, 1.0);
                cr = hr * coverage;
                cg = hg * coverage;
                cb = hb * coverage;
                ca = coverage;
            }
            // Black strokes composited over the wheel, premultiplied.
            for (radius, alpha, width) in rings {
                let c = clamp01((width * 0.5 - (r - radius).abs()) / aa + 0.5) * alpha;
                if c > 0.0 {
                    cr *= 1.0 - c;
                    cg *= 1.0 - c;
                    cb *= 1.0 - c;
                    ca += c * (1.0 - ca);
                }
            }
            if ca > 0.0 {
                put_bgra(&mut out, (y * width_px + x) as usize, cr, cg, cb, ca);
            }
        }
    }

    Some(Baked {
        key: BakeKey {
            width_px,
            height_px,
            scale_bits: scale.to_bits(),
            hue_q: 0,
        },
        image: to_render_image(width_px, height_px, out)?,
        offset: (0.0, 0.0),
        size: (width_px as f32 / scale, height_px as f32 / scale),
    })
}

fn bake_triangle(
    canvas_w_px: u32,
    canvas_h_px: u32,
    scale: f32,
    g: PickerGeometry,
    hue: f32,
    key: BakeKey,
) -> Option<Baked> {
    let [a, b, c] = triangle_vertices(g, hue);
    let aa = 1.0 / scale;

    // Only rasterize the triangle's own bounding box.
    let min_x = a.0.min(b.0).min(c.0) - 2.0;
    let min_y = a.1.min(b.1).min(c.1) - 2.0;
    let max_x = a.0.max(b.0).max(c.0) + 2.0;
    let max_y = a.1.max(b.1).max(c.1) + 2.0;
    let x0 = ((min_x * scale).floor().max(0.0)) as u32;
    let y0 = ((min_y * scale).floor().max(0.0)) as u32;
    let x1 = ((max_x * scale).ceil().min(canvas_w_px as f32)) as u32;
    let y1 = ((max_y * scale).ceil().min(canvas_h_px as f32)) as u32;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (w, h) = (x1 - x0, y1 - y0);

    // Distance from a point to each edge is (its barycentric weight) x (that
    // vertex's altitude); used only to anti-alias the triangle's edge.
    let len = |p: (f32, f32), q: (f32, f32)| ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2)).sqrt();
    let area2 = ((b.0 - a.0) * (c.1 - a.1) - (c.0 - a.0) * (b.1 - a.1)).abs();
    if area2 <= f32::EPSILON {
        return None;
    }
    let (alt_a, alt_b, alt_c) = (area2 / len(b, c), area2 / len(c, a), area2 / len(a, b));

    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let p = (
                ((x0 + x) as f32 + 0.5) / scale,
                ((y0 + y) as f32 + 0.5) / scale,
            );
            let (wa, wb, wc) = barycentric(p, a, b, c);
            let edge_distance = (wa * alt_a).min(wb * alt_b).min(wc * alt_c);
            let coverage = clamp01(edge_distance / aa + 0.5);
            if coverage <= 0.0 {
                continue;
            }
            // Same mapping the picker's hit-testing uses: `wa` is the pure hue
            // vertex, `wb` white, `wc` black.
            let (wa, wb) = (wa.max(0.0), wb.max(0.0));
            let v = clamp01(wa + wb);
            let s = if v <= 0.0001 { 0.0 } else { clamp01(wa / v) };
            let (r, g, b_) = hsv_to_rgb(hue, s, v);
            put_bgra(
                &mut out,
                (y * w + x) as usize,
                r * coverage,
                g * coverage,
                b_ * coverage,
                coverage,
            );
        }
    }

    Some(Baked {
        key,
        image: to_render_image(w, h, out)?,
        offset: (x0 as f32 / scale, y0 as f32 / scale),
        size: (w as f32 / scale, h as f32 / scale),
    })
}

/// Paint a cached bake into `bounds`, replacing the cache entry (and releasing
/// the old atlas image) when `key` changed.
fn paint_baked(
    window: &mut Window,
    cache: &'static std::thread::LocalKey<RefCell<Option<Baked>>>,
    bounds: Bounds<Pixels>,
    key: BakeKey,
    bake: impl FnOnce() -> Option<Baked>,
) {
    let placement = cache.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.as_ref().is_some_and(|baked| baked.key == key) {
            return slot.as_ref().map(|b| (b.image.clone(), b.offset, b.size));
        }
        if let Some(old) = slot.take() {
            let _ = window.drop_image(old.image);
        }
        let baked = bake()?;
        let placement = (baked.image.clone(), baked.offset, baked.size);
        *slot = Some(baked);
        Some(placement)
    });

    let Some((image, offset, size_logical)) = placement else {
        return;
    };
    let target = Bounds {
        origin: point(
            bounds.origin.x + px(offset.0),
            bounds.origin.y + px(offset.1),
        ),
        size: size(px(size_logical.0), px(size_logical.1)),
    };
    let _ = window.paint_image(target, gpui::Corners::default(), image, 0, false);
}

pub(crate) fn paint_hue_wheel(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    geometry: PickerGeometry,
) {
    let scale = window.scale_factor();
    let width_px = (bounds.size.width.as_f32() * scale).ceil() as u32;
    let height_px = (bounds.size.height.as_f32() * scale).ceil() as u32;
    if width_px == 0 || height_px == 0 {
        return;
    }
    let key = BakeKey {
        width_px,
        height_px,
        scale_bits: scale.to_bits(),
        hue_q: 0,
    };
    let local = local_geometry(bounds, geometry);
    paint_baked(window, &WHEEL_CACHE, bounds, key, || {
        bake_wheel(width_px, height_px, scale, local)
    });
}

pub(crate) fn paint_sv_triangle(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    geometry: PickerGeometry,
    hue: f32,
    dragging: bool,
    cache: &mut TriangleCache,
) -> f32 {
    let scale = window.scale_factor();
    let width_px = (bounds.size.width.as_f32() * scale).ceil() as u32;
    let height_px = (bounds.size.height.as_f32() * scale).ceil() as u32;
    if width_px == 0 || height_px == 0 {
        return hue;
    }
    let hue_q = (hue.rem_euclid(1.0) * HUE_KEY_STEPS).round() as u32;
    let key = BakeKey {
        width_px,
        height_px,
        scale_bits: scale.to_bits(),
        hue_q,
    };
    let local = local_geometry(bounds, geometry);
    // Bake at the quantized hue so the cached image always matches its key.
    let baked_hue = hue_q as f32 / HUE_KEY_STEPS;
    let now = Instant::now();
    if triangle_bake_due(
        cache.baked.as_ref().map(|b| b.key),
        key,
        cache
            .last_bake
            .map_or(Duration::MAX, |last| now.duration_since(last)),
        dragging,
    ) {
        if let Some(baked) = bake_triangle(width_px, height_px, scale, local, baked_hue, key) {
            if let Some(old) = cache.baked.replace(baked) {
                let _ = window.drop_image(old.image);
            }
            cache.last_bake = Some(Instant::now());
        }
    }
    let Some(baked) = cache.baked.as_ref() else {
        return hue;
    };
    if baked.key != key {
        // Keep repainting until the last requested hue lands, even if the mouse
        // stops moving while its button is still held.
        window.request_animation_frame();
    }
    let target = Bounds {
        origin: point(
            bounds.origin.x + px(baked.offset.0),
            bounds.origin.y + px(baked.offset.1),
        ),
        size: size(px(baked.size.0), px(baked.size.1)),
    };
    let _ = window.paint_image(
        target,
        gpui::Corners::default(),
        baked.image.clone(),
        0,
        false,
    );
    let displayed_hue = baked.key.hue_q as f32 / HUE_KEY_STEPS;

    // The two hairline outlines stay vector: two 3-point paths.
    let [a, b, c] = triangle_vertices(geometry, displayed_hue);
    let mut tri_edge_outer = gpui::PathBuilder::stroke(px(1.2));
    tri_edge_outer.move_to(point(px(a.0), px(a.1)));
    tri_edge_outer.line_to(point(px(b.0), px(b.1)));
    tri_edge_outer.line_to(point(px(c.0), px(c.1)));
    tri_edge_outer.close();
    if let Ok(path) = tri_edge_outer.build() {
        window.paint_path(path, gpui::black().opacity(0.25));
    }

    let mut tri_edge_inner = gpui::PathBuilder::stroke(px(0.8));
    tri_edge_inner.move_to(point(px(a.0), px(a.1)));
    tri_edge_inner.line_to(point(px(b.0), px(b.1)));
    tri_edge_inner.line_to(point(px(c.0), px(c.1)));
    tri_edge_inner.close();
    if let Ok(path) = tri_edge_inner.build() {
        window.paint_path(path, gpui::white().opacity(0.16));
    }
    displayed_hue
}

pub(crate) fn paint_slider_gradient(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    channel: usize,
    rgba: gpui::Rgba,
    value_01: f32,
) {
    let width = bounds.size.width.as_f32();
    let height = bounds.size.height.as_f32();
    let x0 = bounds.origin.x.as_f32();
    let y0 = bounds.origin.y.as_f32();

    let color_at = |t: f32| match channel {
        0 => gpui::Rgba {
            r: t,
            g: rgba.g,
            b: rgba.b,
            a: 1.0,
        },
        1 => gpui::Rgba {
            r: rgba.r,
            g: t,
            b: rgba.b,
            a: 1.0,
        },
        2 => gpui::Rgba {
            r: rgba.r,
            g: rgba.g,
            b: t,
            a: 1.0,
        },
        _ => gpui::Rgba {
            r: rgba.r * t + 0.18 * (1.0 - t),
            g: rgba.g * t + 0.18 * (1.0 - t),
            b: rgba.b * t + 0.18 * (1.0 - t),
            a: 1.0,
        },
    };

    // Every channel ramp is linear in `t`, so one two-stop gradient quad is
    // exactly what the old `width / 2` per-step quads approximated. (90 degrees
    // is left to right.)
    window.paint_quad(fill(
        bounds,
        gpui::linear_gradient(
            90.0,
            gpui::linear_color_stop(color_at(0.0), 0.0),
            gpui::linear_color_stop(color_at(1.0), 1.0),
        ),
    ));

    let thumb_x = x0 + clamp01(value_01) * width;
    let thumb = Bounds {
        origin: point(px(thumb_x - 1.0), px(y0 - 2.0)),
        size: size(px(2.0), px(height + 4.0)),
    };
    window.paint_quad(fill(thumb, gpui::white()));
}

fn bake_checkerboard(key: BakeKey) -> Option<Baked> {
    let scale = f32::from_bits(key.scale_bits);
    let mut out = vec![0; (key.width_px * key.height_px * 4) as usize];
    for y in 0..key.height_px {
        for x in 0..key.width_px {
            let col = (((x as f32 + 0.5) / scale) / CHECKER_CELL_SIZE) as u32;
            let row = (((y as f32 + 0.5) / scale) / CHECKER_CELL_SIZE) as u32;
            let value = if (col + row) % 2 == 0 { 0.30 } else { 0.18 };
            put_bgra(
                &mut out,
                (y * key.width_px + x) as usize,
                value,
                value,
                value,
                1.0,
            );
        }
    }
    Some(Baked {
        key,
        image: to_render_image(key.width_px, key.height_px, out)?,
        offset: (0.0, 0.0),
        size: (key.width_px as f32 / scale, key.height_px as f32 / scale),
    })
}

pub(crate) fn paint_alpha_checkerboard(window: &mut Window, bounds: Bounds<Pixels>) {
    let scale = window.scale_factor();
    let key = BakeKey {
        width_px: (bounds.size.width.as_f32() * scale).ceil() as u32,
        height_px: (bounds.size.height.as_f32() * scale).ceil() as u32,
        scale_bits: scale.to_bits(),
        hue_q: 0,
    };
    if key.width_px == 0 || key.height_px == 0 {
        return;
    }
    paint_baked(window, &CHECKER_CACHE, bounds, key, || {
        bake_checkerboard(key)
    });
}
#[cfg(test)]
mod bake_tests {
    use super::*;

    fn geometry() -> PickerGeometry {
        PickerGeometry {
            cx: 112.0,
            cy: 112.0,
            outer_r: 110.0,
            inner_r: 90.0,
        }
    }

    fn key(scale: f32, hue_q: u32) -> BakeKey {
        BakeKey {
            width_px: (224.0 * scale) as u32,
            height_px: (224.0 * scale) as u32,
            scale_bits: scale.to_bits(),
            hue_q,
        }
    }

    #[test]
    fn triangle_throttle_keeps_latest_request_and_flushes_on_release() {
        let original = key(1.0, 0);
        let intermediate = key(1.0, 100);
        let latest = key(1.0, 200);
        assert!(triangle_bake_due(None, original, Duration::ZERO, true));
        assert!(!triangle_bake_due(
            Some(original),
            intermediate,
            Duration::from_millis(10),
            true
        ));
        assert!(!triangle_bake_due(
            Some(original),
            latest,
            Duration::from_millis(32),
            true
        ));
        assert!(triangle_bake_due(
            Some(original),
            latest,
            TRIANGLE_BAKE_INTERVAL,
            true
        ));
        assert!(!triangle_bake_due(
            Some(latest),
            latest,
            Duration::from_secs(1),
            true
        ));
        assert!(triangle_bake_due(
            Some(original),
            latest,
            Duration::ZERO,
            false
        ));
        assert!(triangle_bake_due(
            Some(original),
            key(2.0, 0),
            Duration::ZERO,
            true
        ));
    }

    #[test]
    fn checker_cells_keep_logical_size_at_each_scale() {
        for scale in [1.0, 1.25, 2.0] {
            let key = key(scale, 0);
            let baked = bake_checkerboard(key).unwrap();
            let at = |x: f32, y: f32| {
                pixel(&baked, (x * scale) as u32, (y * scale) as u32, key.width_px)
            };
            assert_eq!(at(2.0, 2.0), (77, 77, 77, 255));
            assert_eq!(at(10.0, 2.0), (46, 46, 46, 255));
            assert_eq!(at(2.0, 10.0), (46, 46, 46, 255));
            assert_eq!(at(10.0, 10.0), (77, 77, 77, 255));
        }
    }

    #[test]
    fn swatch_grid_preserves_colors_borders_and_transparent_gaps() {
        let colors = vec![vec![gpui::red(), gpui::blue()], vec![gpui::green()]];
        for scale in [1.0, 1.25, 2.0] {
            let key = key(scale, 0);
            let baked = bake_swatches(key, 20.0, 4.0, &colors).unwrap();
            let at = |x: f32, y: f32| {
                pixel(&baked, (x * scale) as u32, (y * scale) as u32, key.width_px)
            };
            assert_eq!(at(10.0, 10.0), (255, 0, 0, 255));
            assert_eq!(at(34.0, 10.0), (0, 0, 255, 255));
            assert!(at(0.0, 10.0).0 < 255);
            assert_eq!(at(22.0, 10.0).3, 0);
            assert_eq!(at(10.0, 20.0).3, 0);
            assert_eq!(at(34.0, 31.0).3, 0);
        }
    }

    /// Run with `cargo test -p ui --lib measure_picker_bakes -- --ignored --nocapture`.
    /// No timing assertions: machines and build profiles vary.
    #[test]
    #[ignore]
    fn measure_picker_bakes() {
        use std::hint::black_box;
        for scale in [1.0, 2.0] {
            let key = key(scale, 0);
            let colors = named_color_palettes()
                .into_iter()
                .map(|(_, mut colors)| {
                    colors.sort_by(|a, b| hsla_to_hsva(*a).2.total_cmp(&hsla_to_hsva(*b).2));
                    colors.into_iter().take(ALL_COLORS_COLS).collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let measure = |name: &str, bake: &mut dyn FnMut() -> Option<Baked>| {
                let warmup = bake().unwrap();
                // Optional raster inspection artifacts, outside the timed section.
                if let Some(directory) = std::env::var_os("PULSAR_PICKER_PREVIEW_DIR") {
                    let mut bytes = warmup.image.as_bytes(0).unwrap().to_vec();
                    for pixel in bytes.chunks_exact_mut(4) {
                        pixel.swap(0, 2);
                        let alpha = pixel[3] as f32 / 255.0;
                        for channel in &mut pixel[..3] {
                            // Composite the premultiplied pixel over neutral gray.
                            *channel = (*channel as f32 + 32.0 * (1.0 - alpha)).round() as u8;
                        }
                        pixel[3] = 255;
                    }
                    image::RgbaImage::from_raw(
                        (warmup.size.0 * scale).round() as u32,
                        (warmup.size.1 * scale).round() as u32,
                        bytes,
                    )
                    .unwrap()
                    .save(std::path::PathBuf::from(directory).join(format!("{name}-{scale}x.png")))
                    .unwrap();
                }
                black_box(warmup);
                let start = Instant::now();
                for _ in 0..50 {
                    black_box(bake().unwrap());
                }
                eprintln!(
                    "{name} {scale}x: {:.3} ms/bake",
                    start.elapsed().as_secs_f64() * 1000.0 / 50.0
                );
            };
            measure("wheel", &mut || {
                bake_wheel(key.width_px, key.height_px, scale, geometry())
            });
            measure("triangle", &mut || {
                bake_triangle(key.width_px, key.height_px, scale, geometry(), 0.37, key)
            });
            measure("checker", &mut || {
                bake_checkerboard(BakeKey {
                    height_px: (52.0 * scale) as u32,
                    ..key
                })
            });
            measure("swatches", &mut || {
                bake_swatches(
                    BakeKey {
                        width_px: (188.0 * scale) as u32,
                        height_px: (188.0 * scale) as u32,
                        ..key
                    },
                    20.0,
                    4.0,
                    &colors,
                )
            });
        }
    }

    /// Read a premultiplied BGRA pixel back as straight (r, g, b, a) bytes.
    fn pixel(baked: &Baked, x: u32, y: u32, width_px: u32) -> (u8, u8, u8, u8) {
        let bytes = baked.image.as_bytes(0).unwrap();
        let o = ((y * width_px + x) * 4) as usize;
        (bytes[o + 2], bytes[o + 1], bytes[o], bytes[o + 3])
    }

    #[test]
    fn wheel_is_hollow_and_starts_at_red_at_the_top() {
        let baked = bake_wheel(224, 224, 1.0, geometry()).unwrap();
        // Centre of the picker is inside the hole: fully transparent.
        assert_eq!(pixel(&baked, 112, 112, 224).3, 0);
        // Top of the ring (angle -90deg == hue 0) is opaque red.
        let (r, g, b, a) = pixel(&baked, 112, 12, 224);
        assert!(
            a > 250 && r > 240 && g < 15 && b < 15,
            "got {r},{g},{b},{a}"
        );
        // Right of the ring (hue 0.25) is greenish-yellow: green dominates blue.
        let (_, g, b, _) = pixel(&baked, 212, 112, 224);
        assert!(g > b);
    }

    #[test]
    fn triangle_corners_are_hue_white_and_black() {
        let g = geometry();
        let hue = 0.0;
        let key = BakeKey {
            width_px: 224,
            height_px: 224,
            scale_bits: 1.0f32.to_bits(),
            hue_q: 0,
        };
        let baked = bake_triangle(224, 224, 1.0, g, hue, key).unwrap();
        let [a, b, c] = triangle_vertices(g, hue);
        let at = |v: (f32, f32)| {
            // Step a couple of pixels toward the centroid so we sample inside.
            let cx = (a.0 + b.0 + c.0) / 3.0;
            let cy = (a.1 + b.1 + c.1) / 3.0;
            let x = v.0 + (cx - v.0) * 0.06;
            let y = v.1 + (cy - v.1) * 0.06;
            let (ox, oy) = (baked.offset.0, baked.offset.1);
            pixel(
                &baked,
                (x - ox) as u32,
                (y - oy) as u32,
                (baked.size.0) as u32,
            )
        };
        let (r, gr, bl, _) = at(a);
        assert!(r > 220 && gr < 40 && bl < 40, "hue vertex {r},{gr},{bl}");
        let (r, gr, bl, _) = at(b);
        assert!(
            r > 220 && gr > 220 && bl > 220,
            "white vertex {r},{gr},{bl}"
        );
        let (r, gr, bl, _) = at(c);
        assert!(r < 40 && gr < 40 && bl < 40, "black vertex {r},{gr},{bl}");
    }
}

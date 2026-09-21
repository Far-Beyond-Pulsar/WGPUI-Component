use super::math::{barycentric, clamp01, hsv_to_rgb, triangle_vertices};
use super::*;
use std::cell::RefCell;
use std::sync::Arc;

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

thread_local! {
    // Painting only ever happens on the UI thread, so a thread-local cache is
    // enough and needs no synchronization.
    static WHEEL_CACHE: RefCell<Option<Baked>> = const { RefCell::new(None) };
    static TRIANGLE_CACHE: RefCell<Option<Baked>> = const { RefCell::new(None) };
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
        key: BakeKey { width_px, height_px, scale_bits: scale.to_bits(), hue_q: 0 },
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
            return slot
                .as_ref()
                .map(|b| (b.image.clone(), b.offset, b.size));
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

pub(crate) fn paint_hue_wheel(window: &mut Window, bounds: Bounds<Pixels>, geometry: PickerGeometry) {
    let scale = window.scale_factor();
    let width_px = (bounds.size.width.as_f32() * scale).ceil() as u32;
    let height_px = (bounds.size.height.as_f32() * scale).ceil() as u32;
    if width_px == 0 || height_px == 0 {
        return;
    }
    let key = BakeKey { width_px, height_px, scale_bits: scale.to_bits(), hue_q: 0 };
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
) {
    let scale = window.scale_factor();
    let width_px = (bounds.size.width.as_f32() * scale).ceil() as u32;
    let height_px = (bounds.size.height.as_f32() * scale).ceil() as u32;
    if width_px == 0 || height_px == 0 {
        return;
    }
    let hue_q = (hue.rem_euclid(1.0) * HUE_KEY_STEPS).round() as u32;
    let key = BakeKey { width_px, height_px, scale_bits: scale.to_bits(), hue_q };
    let local = local_geometry(bounds, geometry);
    // Bake at the quantized hue so the cached image always matches its key.
    let baked_hue = hue_q as f32 / HUE_KEY_STEPS;
    paint_baked(window, &TRIANGLE_CACHE, bounds, key, || {
        bake_triangle(width_px, height_px, scale, local, baked_hue, key)
    });

    // The two hairline outlines stay vector: two 3-point paths.
    let [a, b, c] = triangle_vertices(geometry, hue);
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
        0 => gpui::Rgba { r: t, g: rgba.g, b: rgba.b, a: 1.0 },
        1 => gpui::Rgba { r: rgba.r, g: t, b: rgba.b, a: 1.0 },
        2 => gpui::Rgba { r: rgba.r, g: rgba.g, b: t, a: 1.0 },
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

pub(crate) fn paint_alpha_checkerboard(window: &mut Window, bounds: Bounds<Pixels>) {
    let width = bounds.size.width.as_f32();
    let height = bounds.size.height.as_f32();
    let x0 = bounds.origin.x.as_f32();
    let y0 = bounds.origin.y.as_f32();

    let light = gpui::Rgba {
        r: 0.30,
        g: 0.30,
        b: 0.30,
        a: 1.0,
    };
    let dark = gpui::Rgba {
        r: 0.18,
        g: 0.18,
        b: 0.18,
        a: 1.0,
    };

    // One base quad in the dark colour, then only the light cells on top: half
    // the primitives of painting every cell.
    window.paint_quad(fill(bounds, dark));

    let cols = (width / CHECKER_CELL_SIZE).ceil() as i32;
    let rows = (height / CHECKER_CELL_SIZE).ceil() as i32;

    for row in 0..rows {
        for col in 0..cols {
            if (row + col) % 2 != 0 {
                continue;
            }
            let rect = Bounds {
                origin: point(
                    px(x0 + col as f32 * CHECKER_CELL_SIZE),
                    px(y0 + row as f32 * CHECKER_CELL_SIZE),
                ),
                size: size(px(CHECKER_CELL_SIZE + 0.6), px(CHECKER_CELL_SIZE + 0.6)),
            };
            window.paint_quad(fill(rect, light));
        }
    }
}

#[cfg(test)]
mod bake_tests {
    use super::*;

    fn geometry() -> PickerGeometry {
        PickerGeometry { cx: 112.0, cy: 112.0, outer_r: 110.0, inner_r: 90.0 }
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
        assert!(a > 250 && r > 240 && g < 15 && b < 15, "got {r},{g},{b},{a}");
        // Right of the ring (hue 0.25) is greenish-yellow: green dominates blue.
        let (_, g, b, _) = pixel(&baked, 212, 112, 224);
        assert!(g > b);
    }

    #[test]
    fn triangle_corners_are_hue_white_and_black() {
        let g = geometry();
        let hue = 0.0;
        let key = BakeKey { width_px: 224, height_px: 224, scale_bits: 1.0f32.to_bits(), hue_q: 0 };
        let baked = bake_triangle(224, 224, 1.0, g, hue, key).unwrap();
        let [a, b, c] = triangle_vertices(g, hue);
        let at = |v: (f32, f32)| {
            // Step a couple of pixels toward the centroid so we sample inside.
            let cx = (a.0 + b.0 + c.0) / 3.0;
            let cy = (a.1 + b.1 + c.1) / 3.0;
            let x = v.0 + (cx - v.0) * 0.06;
            let y = v.1 + (cy - v.1) * 0.06;
            let (ox, oy) = (baked.offset.0, baked.offset.1);
            pixel(&baked, (x - ox) as u32, (y - oy) as u32, (baked.size.0) as u32)
        };
        let (r, gr, bl, _) = at(a);
        assert!(r > 220 && gr < 40 && bl < 40, "hue vertex {r},{gr},{bl}");
        let (r, gr, bl, _) = at(b);
        assert!(r > 220 && gr > 220 && bl > 220, "white vertex {r},{gr},{bl}");
        let (r, gr, bl, _) = at(c);
        assert!(r < 40 && gr < 40 && bl < 40, "black vertex {r},{gr},{bl}");
    }
}

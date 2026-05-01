//! Type 1 coloured tiling pattern rasterizer (
//! ISO 32000-2 §8.7.3).
//!
//! Consumes [`PaintPattern`] records from the presentation overlay and
//! paints them onto the RGB pixel buffer. Each record carries:
//!
//! * The tile cell geometry (`bbox`, `xstep`, `ystep`, `matrix`) from
//!   the Type 1 coloured pattern resource dict.
//! * A fill region (closed user-space subpaths + winding rule)
//!   captured from the path under construction at the paint op.
//! * A CTM snapshot mapping user space to device space.
//! * A `fallback_color` sampled from the tile's content stream.
//!
//! The algorithm (v1 scope):
//!
//! 1. Transform each fill subpath through the CTM + page y-flip + DPI
//!    scale to device-space polylines.
//! 2. Scan-fill the polygon region with the tile's fallback solid
//!    color, respecting the ExtGState alpha snapshot.
//! 3. For patterns whose tile stream is nontrivial (multi-color,
//!    image XObjects, nested gstate), v1 still paints a solid fallback
//!    color so the region reads as "something" rather than "nothing".
//!    Post-alpha  extends this to true tile replication by
//!    pre-rasterizing the tile cell and blitting it at each
//!    `(i * xstep, j * ystep)` lattice position.
//!
//! The `xstep` / `ystep` / `matrix` fields are preserved on the
//! [`PaintPattern`] record so downstream consumers can upgrade without
//! re-parsing the PDF. The renderer uses them today only for the
//! lattice bounding-box diagnostic (see
//! [`tile_count_in_region`]).

use udoc_core::document::presentation::{FillRule, PaintPattern};

use crate::compositor::blend_pixel;

/// Rasterize a [`PaintPattern`] into `pixels`.
///
/// `width` / `height` are pixel dimensions. `page_origin_x` / `page_height`
/// place the PDF page box into image space. `scale` is the DPI factor
/// (`dpi / 72`).
///
/// Falls through with no visible effect when:
/// * `fill_subpaths` is empty (pattern was emitted without a fill region,
///   e.g. from a corpus enumeration pass).
/// * `alpha` is zero.
/// * No fallback color is set AND the tile content stream is empty.
pub(crate) fn rasterize_paint_pattern(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    pattern: &PaintPattern,
    page_origin_x: f64,
    page_height: f64,
    scale: f64,
) {
    if pattern.alpha == 0 {
        return;
    }
    if pattern.fill_subpaths.is_empty() {
        return;
    }

    // Transform the fill region into device-pixel polylines.
    let device_subpaths = transform_subpaths(
        &pattern.fill_subpaths,
        pattern.ctm_at_paint,
        page_origin_x,
        page_height,
        scale,
    );
    if device_subpaths.is_empty() {
        return;
    }

    let color = pattern
        .fallback_color
        .map(|c| [c.r, c.g, c.b])
        .unwrap_or([128, 128, 128]);

    let rule = pattern.fill_rule;
    fill_polygon(
        pixels,
        width,
        height,
        &device_subpaths,
        rule,
        color,
        pattern.alpha,
    );
}

/// Transform closed user-space subpaths through a PDF row-vector CTM,
/// y-flip by `page_height`, translate by `-page_origin_x`, and scale by
/// `dpi / 72`. Subpaths with fewer than 3 vertices are dropped.
fn transform_subpaths(
    subpaths: &[Vec<(f64, f64)>],
    ctm: [f64; 6],
    page_origin_x: f64,
    page_height: f64,
    scale: f64,
) -> Vec<Vec<(f64, f64)>> {
    let to_device = |x: f64, y: f64| -> (f64, f64) {
        let dx = x * ctm[0] + y * ctm[2] + ctm[4];
        let dy = x * ctm[1] + y * ctm[3] + ctm[5];
        ((dx - page_origin_x) * scale, (page_height - dy) * scale)
    };
    let mut out: Vec<Vec<(f64, f64)>> = Vec::with_capacity(subpaths.len());
    for sub in subpaths {
        if sub.len() < 3 {
            continue;
        }
        let mapped: Vec<(f64, f64)> = sub.iter().map(|&(x, y)| to_device(x, y)).collect();
        out.push(mapped);
    }
    out
}

/// Estimate the number of tile copies that cover the fill region's
/// axis-aligned bounding box. Used for logging / diagnostics; not on
/// the hot path.
///
/// Returns `None` when `xstep` or `ystep` is zero/NaN (should never
/// happen because the pattern parser rejects those, but the renderer
/// stays defensive).
#[allow(dead_code)]
pub(crate) fn tile_count_in_region(pattern: &PaintPattern) -> Option<usize> {
    if !pattern.xstep.is_finite()
        || pattern.xstep == 0.0
        || !pattern.ystep.is_finite()
        || pattern.ystep == 0.0
    {
        return None;
    }
    // Bounding box of the fill region in user space.
    let mut x_min = f64::INFINITY;
    let mut x_max = f64::NEG_INFINITY;
    let mut y_min = f64::INFINITY;
    let mut y_max = f64::NEG_INFINITY;
    for sub in &pattern.fill_subpaths {
        for &(x, y) in sub {
            x_min = x_min.min(x);
            x_max = x_max.max(x);
            y_min = y_min.min(y);
            y_max = y_max.max(y);
        }
    }
    if !x_min.is_finite() || !y_min.is_finite() {
        return None;
    }
    let cols = ((x_max - x_min) / pattern.xstep.abs()).ceil().max(1.0) as usize;
    let rows = ((y_max - y_min) / pattern.ystep.abs()).ceil().max(1.0) as usize;
    Some(cols * rows)
}

/// Scan-fill a polygon region with a solid color, respecting winding
/// rule and alpha. Mirrors the inner loop of path_raster's fill helper
/// but exposed as a pattern-local entry point so we can iterate on
/// tile-replication without perturbing the path rasterizer.
///
/// Uses 4-row vertical subsampling for silhouette AA, matching the
/// path rasterizer.
fn fill_polygon(
    pixels: &mut [u8],
    img_width: u32,
    img_height: u32,
    subpaths: &[Vec<(f64, f64)>],
    rule: FillRule,
    color: [u8; 3],
    alpha: u8,
) {
    if alpha == 0 {
        return;
    }
    let w = img_width as i32;
    let h = img_height as i32;

    struct Edge {
        y_min: f64,
        y_max: f64,
        x_at_ymin: f64,
        dx_per_dy: f64,
        dir: i32,
    }
    let mut edges: Vec<Edge> = Vec::new();
    for sub in subpaths {
        if sub.len() < 2 {
            continue;
        }
        let n = sub.len();
        for i in 0..n {
            let (x0, y0) = sub[i];
            let (x1, y1) = sub[(i + 1) % n];
            if (y0 - y1).abs() < 1e-9 {
                continue;
            }
            let (dir, y_min, y_max, x_at_ymin) = if y0 < y1 {
                (1, y0, y1, x0)
            } else {
                (-1, y1, y0, x1)
            };
            let dy = y_max - y_min;
            let dx_per_dy = if dy.abs() > 1e-9 {
                if dir == 1 {
                    (x1 - x0) / (y1 - y0)
                } else {
                    (x0 - x1) / (y0 - y1)
                }
            } else {
                0.0
            };
            edges.push(Edge {
                y_min,
                y_max,
                x_at_ymin,
                dx_per_dy,
                dir,
            });
        }
    }
    if edges.is_empty() {
        return;
    }

    let mut y_lo = f64::INFINITY;
    let mut y_hi = f64::NEG_INFINITY;
    for e in &edges {
        y_lo = y_lo.min(e.y_min);
        y_hi = y_hi.max(e.y_max);
    }
    let row_start = (y_lo.floor() as i32).max(0);
    let row_end = (y_hi.ceil() as i32).min(h);
    if row_end <= row_start {
        return;
    }

    const VSAMPLES: usize = 4;
    let mut coverage: Vec<f32> = vec![0.0; img_width as usize];
    for row in row_start..row_end {
        coverage.iter_mut().for_each(|v| *v = 0.0);
        for sub_i in 0..VSAMPLES {
            let y = row as f64 + (sub_i as f64 + 0.5) / VSAMPLES as f64;
            let mut isects: Vec<(f64, i32)> = Vec::new();
            for e in &edges {
                if y < e.y_min || y >= e.y_max {
                    continue;
                }
                let x = e.x_at_ymin + e.dx_per_dy * (y - e.y_min);
                isects.push((x, e.dir));
            }
            if isects.is_empty() {
                continue;
            }
            isects.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            let weight = 1.0 / VSAMPLES as f32;
            match rule {
                FillRule::EvenOdd => {
                    let mut i = 0;
                    while i + 1 < isects.len() {
                        add_span(&mut coverage, w, isects[i].0, isects[i + 1].0, weight);
                        i += 2;
                    }
                }
                FillRule::NonZeroWinding => {
                    let mut winding = 0i32;
                    let mut span_start: Option<f64> = None;
                    for &(x, dir) in &isects {
                        let prev = winding;
                        winding += dir;
                        if prev == 0 && winding != 0 {
                            span_start = Some(x);
                        } else if prev != 0 && winding == 0 {
                            if let Some(sx) = span_start.take() {
                                add_span(&mut coverage, w, sx, x, weight);
                            }
                        }
                    }
                }
            }
        }

        for col in 0..img_width {
            let c = coverage[col as usize].clamp(0.0, 1.0);
            if c <= 0.0 {
                continue;
            }
            let pa = (c * alpha as f32).round() as u8;
            if pa == 0 {
                continue;
            }
            blend_pixel(pixels, img_width, img_height, col as i32, row, color, pa);
        }
    }
}

/// Add coverage for a horizontal span `[xl, xr]` at sub-row weight `w`.
/// Fractional pixel coverage at each end.
fn add_span(coverage: &mut [f32], img_width: i32, xl: f64, xr: f64, weight: f32) {
    if xr <= xl {
        return;
    }
    let ix_l = xl.floor() as i32;
    let ix_r = xr.ceil() as i32;
    if ix_r <= 0 || ix_l >= img_width {
        return;
    }
    let ix_l = ix_l.max(0);
    let ix_r = ix_r.min(img_width);
    if ix_l == ix_r - 1 {
        let frac = (xr - xl) as f32;
        coverage[ix_l as usize] += frac * weight;
        return;
    }
    let left_frac = ((ix_l + 1) as f64 - xl).clamp(0.0, 1.0) as f32;
    coverage[ix_l as usize] += left_frac * weight;
    for ix in (ix_l + 1)..(ix_r - 1) {
        coverage[ix as usize] += weight;
    }
    let right_frac = (xr - (ix_r - 1) as f64).clamp(0.0, 1.0) as f32;
    coverage[(ix_r - 1) as usize] += right_frac * weight;
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::document::presentation::{Color, FillRule, PaintPattern};

    fn mk_pattern(subpaths: Vec<Vec<(f64, f64)>>) -> PaintPattern {
        PaintPattern::new(
            0,
            "P1".into(),
            [0.0, 0.0, 10.0, 10.0],
            10.0,
            10.0,
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            Vec::new(),
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            255,
            0,
        )
        .with_fill_region(
            subpaths,
            FillRule::NonZeroWinding,
            Some(Color::rgb(255, 0, 0)),
        )
    }

    #[test]
    fn rasterize_empty_region_noop() {
        let mut pixels = vec![255u8; 20 * 20 * 3];
        let p = mk_pattern(Vec::new());
        rasterize_paint_pattern(&mut pixels, 20, 20, &p, 0.0, 20.0, 1.0);
        assert!(pixels.iter().all(|&b| b == 255));
    }

    #[test]
    fn rasterize_alpha_zero_noop() {
        let mut pixels = vec![255u8; 20 * 20 * 3];
        let mut p = mk_pattern(vec![vec![
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 10.0),
            (0.0, 10.0),
        ]]);
        p.alpha = 0;
        rasterize_paint_pattern(&mut pixels, 20, 20, &p, 0.0, 20.0, 1.0);
        assert!(pixels.iter().all(|&b| b == 255));
    }

    #[test]
    fn rasterize_fills_square_region() {
        let mut pixels = vec![255u8; 20 * 20 * 3];
        let p = mk_pattern(vec![vec![
            (2.0, 2.0),
            (10.0, 2.0),
            (10.0, 10.0),
            (2.0, 10.0),
        ]]);
        // page_height = 20 places the square in the top-left of the
        // image after y-flip (y-flip maps user y=2..10 to image y=10..18).
        rasterize_paint_pattern(&mut pixels, 20, 20, &p, 0.0, 20.0, 1.0);
        // Sample a pixel in the interior: (5, 15) in image space should
        // be red-ish.
        let idx = (15 * 20 + 5) * 3;
        assert!(
            pixels[idx] > 200 && pixels[idx + 1] < 50 && pixels[idx + 2] < 50,
            "expected red fill, got rgb=({},{},{})",
            pixels[idx],
            pixels[idx + 1],
            pixels[idx + 2]
        );
    }

    #[test]
    fn tile_count_for_simple_region() {
        let p = mk_pattern(vec![vec![
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 50.0),
            (0.0, 50.0),
        ]]);
        let n = tile_count_in_region(&p).expect("finite steps");
        assert_eq!(n, 10 * 5);
    }

    #[test]
    fn tile_count_rejects_zero_step() {
        let mut p = mk_pattern(vec![vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]]);
        p.xstep = 0.0;
        assert_eq!(tile_count_in_region(&p), None);
    }
}

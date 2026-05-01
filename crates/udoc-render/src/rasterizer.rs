//! Glyph rasterizer with per-scanline edge sweep.
//!
//! Computes exact analytical pixel coverage using signed-area integration.
//! For each scanline, active edges are enumerated and their coverage
//! contributions computed independently. A left-to-right running sum
//! within each row converts edge contributions to pixel coverage.
//! The accumulator resets per row (no cross-row state), which avoids
//! floating-point drift and matches FreeType's scanline decomposition
//! approach more closely than global delta accumulation.

use udoc_font::ttf::StemHints;

/// Apply FreeType-style advance compensation for a glyph about to be
/// composited after a previous glyph.
///
/// Given the incoming `cursor_x` (already-advanced pen position from the
/// previous glyph's advance width), the previous glyph's `prev_rsb_shift`
/// and the incoming glyph's `current_lsb_shift` (both in fractional
/// device pixels, both 0 when X-axis auto-hinting is off), return the
/// adjusted cursor position for compositing this glyph.
///
/// Formula (FreeType's continuous-pen variant):
///   adjusted = cursor_x + prev_rsb_shift - current_lsb_shift
///
/// When both shifts are zero (X-axis hinting disabled) the formula is
/// the identity and the default output path is unchanged. With X-axis
/// auto-hinting on, this prevents the sub-pixel drift documented in the
///  round-9 diagnosis: hinted glyph outlines snap their own left /
/// right edges to pixel boundaries, which introduces per-glyph lsb /
/// rsb shifts that the raw advance-cursor does not know about. The
/// composite must subtract the incoming glyph's lsb shift (so it lands
/// back where its origin should) and add the previous glyph's rsb shift
/// (so accumulated shifts chain cleanly through the line).
///
/// Callers typically composite at `adjust_cursor_for_shift(..).floor()`
/// to match FreeType's integer-pixel compositing convention, leaving
/// the fractional remainder as the sub-pixel AA input for the NEXT
/// glyph's bin selection.
#[inline]
pub fn adjust_cursor_for_shift(cursor_x: f64, prev_rsb_shift: f64, current_lsb_shift: f64) -> f64 {
    cursor_x + prev_rsb_shift - current_lsb_shift
}

/// A rasterized glyph bitmap.
#[cfg(feature = "test-internals")]
#[derive(Debug, Clone)]
pub struct GlyphBitmap {
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels.
    pub height: u32,
    /// Horizontal bearing (left edge relative to the glyph origin, in pixels).
    pub left: i32,
    /// Vertical bearing (top edge relative to the baseline, in pixels).
    pub top: i32,
    /// Raw pixel data. See `subpixel` for channel count.
    pub data: Vec<u8>,
    /// When true, data has 3 bytes per pixel (R/G/B subpixel alpha).
    /// When false, data has 1 byte per pixel (uniform alpha).
    pub subpixel: bool,
    /// Sub-pixel shift to apply to the advance cursor BEFORE drawing this
    /// glyph. Emulates FreeType's `lsb_delta` in fractional pixels.
    /// Zero when X-axis auto-hinting is off.
    pub lsb_shift: f64,
    /// Sub-pixel shift to apply to the advance cursor AFTER drawing this
    /// glyph. Emulates FreeType's `rsb_delta`. Zero when X-axis hinting off.
    pub rsb_shift: f64,
}

/// A rasterized glyph bitmap.
#[cfg(not(feature = "test-internals"))]
#[derive(Debug, Clone)]
pub(crate) struct GlyphBitmap {
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    pub data: Vec<u8>,
    /// When true, data has 3 bytes per pixel (R/G/B subpixel alpha).
    /// When false, data has 1 byte per pixel (uniform alpha).
    pub subpixel: bool,
    /// Sub-pixel shift to apply to the advance cursor BEFORE drawing this
    /// glyph. Emulates FreeType's `lsb_delta` in fractional pixels.
    /// Zero when X-axis auto-hinting is off.
    pub lsb_shift: f64,
    /// Sub-pixel shift to apply to the advance cursor AFTER drawing this
    /// glyph. Emulates FreeType's `rsb_delta`. Zero when X-axis hinting off.
    pub rsb_shift: f64,
}

/// Rasterize a glyph outline at a given pixel size.
///
/// `contours`: slices of (x, y, on_curve) points in font units.
/// `scale`: font units to pixels (font_size_px / units_per_em).
/// `x_offset`, `y_offset`: pixel position of the glyph origin.
///
/// Returns None if the outline produces an empty bitmap.
#[cfg(feature = "test-internals")]
pub fn rasterize_outline(
    contours: &[Vec<(f64, f64, bool)>],
    scale: f64,
    x_offset: f64,
    y_offset: f64,
    stem_hints: Option<&StemHints>,
    hint_values: Option<&udoc_font::type1::Type1HintValues>,
) -> Option<GlyphBitmap> {
    rasterize_outline_inner(
        contours,
        scale,
        x_offset,
        y_offset,
        stem_hints,
        hint_values,
        false,
    )
}

#[cfg(not(feature = "test-internals"))]
pub(crate) fn rasterize_outline(
    contours: &[Vec<(f64, f64, bool)>],
    scale: f64,
    x_offset: f64,
    y_offset: f64,
    stem_hints: Option<&StemHints>,
    hint_values: Option<&udoc_font::type1::Type1HintValues>,
) -> Option<GlyphBitmap> {
    rasterize_outline_inner(
        contours,
        scale,
        x_offset,
        y_offset,
        stem_hints,
        hint_values,
        false,
    )
}

/// Rasterize with LCD subpixel rendering (3x horizontal resolution).
/// Returns a GlyphBitmap with 3 bytes per pixel (R, G, B subpixel alpha).
pub(crate) fn rasterize_outline_subpixel(
    contours: &[Vec<(f64, f64, bool)>],
    scale: f64,
    x_offset: f64,
    y_offset: f64,
    stem_hints: Option<&StemHints>,
    hint_values: Option<&udoc_font::type1::Type1HintValues>,
) -> Option<GlyphBitmap> {
    rasterize_outline_inner(
        contours,
        scale,
        x_offset,
        y_offset,
        stem_hints,
        hint_values,
        true,
    )
}

fn rasterize_outline_inner(
    contours: &[Vec<(f64, f64, bool)>],
    scale: f64,
    x_offset: f64,
    y_offset: f64,
    stem_hints: Option<&StemHints>,
    hint_values: Option<&udoc_font::type1::Type1HintValues>,
    subpixel: bool,
) -> Option<GlyphBitmap> {
    if contours.is_empty() || scale <= 0.0 {
        return None;
    }

    // Apply PS hint grid-fitting: builds a unified (original, snapped) mapping
    // from blue zones and stem hints, then interpolates all points through it.
    let hinted: Vec<Vec<(f64, f64, bool)>>;
    let contours = if stem_hints.is_some() || hint_values.is_some() {
        hinted = super::ps_hints::ps_hint_glyph(contours, stem_hints, hint_values, scale);
        &hinted
    } else {
        contours
    };

    // For subpixel rendering, multiply x-coordinates by 3 to get
    // 3x horizontal resolution. The y-coordinates stay the same.
    let x_scale = if subpixel { 3.0 } else { 1.0 };

    // Flatten bezier curves to line segments, computing bounding box.
    let mut segments: Vec<(f32, f32, f32, f32)> = Vec::new(); // (x0,y0,x1,y1)
    let mut x_min = f32::MAX;
    let mut y_min = f32::MAX;
    let mut x_max = f32::MIN;
    let mut y_max = f32::MIN;

    for contour in contours {
        let pts = flatten_contour(contour, scale, x_offset, y_offset);
        if pts.len() < 2 {
            continue;
        }
        for w in pts.windows(2) {
            let (x0, y0) = ((w[0].0 * x_scale) as f32, w[0].1 as f32);
            let (x1, y1) = ((w[1].0 * x_scale) as f32, w[1].1 as f32);
            segments.push((x0, y0, x1, y1));
            x_min = x_min.min(x0).min(x1);
            y_min = y_min.min(y0).min(y1);
            x_max = x_max.max(x0).max(x1);
            y_max = y_max.max(y0).max(y1);
        }
        // Close contour.
        if let (Some(first), Some(last)) = (pts.first(), pts.last()) {
            let (x0, y0) = ((last.0 * x_scale) as f32, last.1 as f32);
            let (x1, y1) = ((first.0 * x_scale) as f32, first.1 as f32);
            segments.push((x0, y0, x1, y1));
        }
    }

    if segments.is_empty() {
        return None;
    }

    // Bounding box in the (possibly 3x) coordinate space.
    let raster_left = x_min.floor() as i32;
    let top = y_min.floor() as i32;
    let raster_width = ((x_max.ceil() as i32) - raster_left).max(1) as u32;
    let height = ((y_max.ceil() as i32) - top).max(1) as u32;

    // For subpixel, the output pixel width is raster_width/3.
    let out_width = if subpixel {
        (raster_width / 3).max(1)
    } else {
        raster_width
    };
    let out_left = if subpixel {
        raster_left / 3
    } else {
        raster_left
    };

    let rw = raster_width as usize;
    let h = height as usize;

    if rw > 25500 || h > 8500 {
        return None;
    }

    // Build edge table from line segments, sorted by y_min.
    let ox = raster_left as f64;
    let oy = top as f64;
    let mut edges: Vec<Edge> = Vec::with_capacity(segments.len());
    for &(x0, y0, x1, y1) in &segments {
        let (x0, y0, x1, y1) = (
            x0 as f64 - ox,
            y0 as f64 - oy,
            x1 as f64 - ox,
            y1 as f64 - oy,
        );
        if (y0 - y1).abs() <= f64::EPSILON {
            continue;
        }
        let (dir, x0, y0, x1, y1) = if y0 < y1 {
            (1.0, x0, y0, x1, y1)
        } else {
            (-1.0, x1, y1, x0, y0)
        };
        // Snap near-vertical edges to exactly vertical toward the glyph
        // interior. CFF outlines can have slightly diagonal stem edges
        // (e.g., CMR8 "1" right edge transitions from stem to flag).
        // Snapping toward the interior (larger coverage side) eliminates
        // banding AND maximizes stem darkness to match FreeType output.
        // Right-side edges (dir > 0, going down on CW contour): use min x
        // Left-side edges (dir < 0, going up on CW contour): use max x
        let raw_dx_per_dy = (x1 - x0) / (y1 - y0);
        let (x_at_ymin, dx_per_dy) = if raw_dx_per_dy.abs() < 0.15 && (y1 - y0) > 3.0 {
            // Weighted toward interior (70/30 blend toward stem side).
            let x_interior = if dir > 0.0 {
                // Right edge: bias toward smaller x (interior)
                x0.min(x1) * 0.7 + x0.max(x1) * 0.3
            } else {
                // Left edge: bias toward larger x (interior)
                x0.max(x1) * 0.7 + x0.min(x1) * 0.3
            };
            (x_interior, 0.0)
        } else {
            (x0, raw_dx_per_dy)
        };
        edges.push(Edge {
            y_min: y0,
            y_max: y1,
            x_at_ymin,
            dx_per_dy,
            dir,
        });
    }
    edges.sort_by(|a, b| {
        a.y_min
            .partial_cmp(&b.y_min)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Per-scanline edge sweep. For each row, compute coverage from scratch
    // using active edges. The accumulator resets per row.
    let mut next_edge = 0usize;
    let mut active: Vec<usize> = Vec::new();

    if subpixel {
        let ow = out_width as usize;
        let mut bitmap = vec![0u8; ow * h * 3];
        let mut row_buf = vec![0.0f64; rw];

        for y in 0..h {
            let yf = y as f64;

            // Activate new edges starting at or before this row.
            while next_edge < edges.len() && edges[next_edge].y_min < yf + 1.0 {
                active.push(next_edge);
                next_edge += 1;
            }
            // Remove expired edges.
            active.retain(|&ei| edges[ei].y_max > yf);

            // Clear row buffer and render each active edge into it.
            row_buf.iter_mut().for_each(|v| *v = 0.0);
            for &ei in &active {
                render_edge_scanline(&mut row_buf, rw, yf, &edges[ei]);
            }

            // Accumulate left-to-right within this row (resets per row).
            let mut acc = 0.0f64;
            for (rx, &delta) in row_buf.iter().enumerate().take(rw) {
                acc += delta;
                let px = rx / 3;
                let sub = rx % 3;
                if px < ow {
                    // Linear coverage mapping (mupdf parity). The
                    // earlier `cov.powf(0.97)` darkening pass was a
                    // blanket compensation for sparse Type1 vstem
                    // hints; that gap is now closed at the hint layer
                    // via `udoc-font::postscript::discovered_stems`
                    // ( #177), so the coverage step is pure linear.
                    let alpha = acc.abs().min(1.0) as f32;
                    bitmap[(y * ow + px) * 3 + sub] = (alpha * 255.0 + 0.5) as u8;
                }
            }
        }

        lcd_filter(&mut bitmap, ow, h);

        Some(GlyphBitmap {
            width: out_width,
            height,
            left: out_left,
            top,
            data: bitmap,
            subpixel: true,
            lsb_shift: 0.0,
            rsb_shift: 0.0,
        })
    } else {
        let w = rw;
        let mut bitmap = vec![0u8; w * h];
        let mut row_buf = vec![0.0f64; w];

        for y in 0..h {
            let yf = y as f64;

            while next_edge < edges.len() && edges[next_edge].y_min < yf + 1.0 {
                active.push(next_edge);
                next_edge += 1;
            }
            active.retain(|&ei| edges[ei].y_max > yf);

            row_buf.iter_mut().for_each(|v| *v = 0.0);
            for &ei in &active {
                render_edge_scanline(&mut row_buf, w, yf, &edges[ei]);
            }

            // Per-row accumulation: resets at each scanline boundary.
            // Linear coverage mapping; see the sub-pixel branch above
            // for the historical gamma rationale.
            let mut acc = 0.0f64;
            for x in 0..w {
                acc += row_buf[x];
                let alpha = acc.abs().min(1.0) as f32;
                bitmap[y * w + x] = (alpha * 255.0 + 0.5) as u8;
            }
        }

        Some(GlyphBitmap {
            width: out_width,
            height,
            left: out_left,
            top,
            data: bitmap,
            subpixel: false,
            lsb_shift: 0.0,
            rsb_shift: 0.0,
        })
    }
}

/// An edge in the scanline edge table.
struct Edge {
    y_min: f64,
    y_max: f64,
    x_at_ymin: f64,
    dx_per_dy: f64,
    dir: f64, // +1.0 downward, -1.0 upward
}

/// Render one edge's contribution to a single scanline row buffer.
///
/// Computes the signed-area coverage contribution of the edge for the
/// scanline at integer row `yf`. Writes coverage deltas into `row` which
/// are later accumulated left-to-right to produce pixel coverage.
fn render_edge_scanline(row: &mut [f64], w: usize, yf: f64, e: &Edge) {
    let y_top = yf.max(e.y_min);
    let y_bot = (yf + 1.0).min(e.y_max);
    let dy = y_bot - y_top;
    if dy <= 0.0 {
        return;
    }

    let x_top = e.x_at_ymin + (y_top - e.y_min) * e.dx_per_dy;
    let x_bot = e.x_at_ymin + (y_bot - e.y_min) * e.dx_per_dy;
    let d = dy * e.dir;

    let (xl, xr) = if x_top < x_bot {
        (x_top, x_bot)
    } else {
        (x_bot, x_top)
    };
    let x0floor = xl.floor();
    let x0i = x0floor as i32;
    let x1i = xr.ceil() as i32;

    if x0i < 0 || x0i as usize >= w {
        return;
    }
    let idx = x0i as usize;

    if x1i <= x0i + 1 {
        // Edge fits within one or two pixel columns.
        let xmf = 0.5 * (x_top + x_bot) - x0floor;
        if idx < w {
            row[idx] += d - d * xmf;
        }
        if idx + 1 < w {
            row[idx + 1] += d * xmf;
        }
    } else {
        // Edge spans multiple pixel columns.
        let s = (xr - xl).recip();
        let x0f = xl - x0floor;
        let x1f = xr - xr.ceil() + 1.0;

        let a0 = 0.5 * s * (1.0 - x0f) * (1.0 - x0f);
        let am = 0.5 * s * x1f * x1f;

        if idx < w {
            row[idx] += d * a0;
        }

        if x1i == x0i + 2 {
            if idx + 1 < w {
                row[idx + 1] += d * (1.0 - a0 - am);
            }
        } else {
            let a1 = s * (1.5 - x0f);
            if idx + 1 < w {
                row[idx + 1] += d * (a1 - a0);
            }
            for xi in (x0i + 2)..(x1i - 1) {
                if xi >= 0 {
                    let i = xi as usize;
                    if i < w {
                        row[i] += d * s;
                    }
                }
            }
            let a2 = a1 + (x1i - x0i - 3) as f64 * s;
            let i = (x1i - 1).max(0) as usize;
            if i < w {
                row[i] += d * (1.0 - a2 - am);
            }
        }

        let i = x1i.max(0) as usize;
        if i < w {
            row[i] += d * am;
        }
    }
}

// ---- LCD subpixel filter ----

/// FreeType-style 5-tap LCD filter to reduce color fringing from subpixel
/// rendering. Convolves the RGB subpixel alpha values horizontally with
/// weights [1/16, 4/16, 6/16, 4/16, 1/16] to smooth transitions between
/// adjacent subpixels.
fn lcd_filter(bitmap: &mut [u8], width: usize, height: usize) {
    if width == 0 {
        return;
    }
    let stride = width * 3;
    let mut filtered = vec![0u8; stride];

    for y in 0..height {
        let row_start = y * stride;
        for (i, out) in filtered.iter_mut().enumerate().take(stride) {
            let mut sum = 0u32;
            // 5-tap filter centered on sample i: weights [1, 4, 6, 4, 1] / 16
            for (k, &weight) in [1u32, 4, 6, 4, 1].iter().enumerate() {
                let si = i as i32 + k as i32 - 2;
                if si >= 0 && (si as usize) < stride {
                    sum += bitmap[row_start + si as usize] as u32 * weight;
                }
            }
            *out = (sum / 16).min(255) as u8;
        }
        bitmap[row_start..row_start + stride].copy_from_slice(&filtered);
    }
}

// ---- Bezier curve flattening ----

#[derive(Debug, Clone, Copy)]
struct Pt(f64, f64);

fn flatten_contour(
    points: &[(f64, f64, bool)],
    scale: f64,
    x_offset: f64,
    y_offset: f64,
) -> Vec<Pt> {
    if points.is_empty() {
        return Vec::new();
    }

    let mut result = Vec::new();
    let to_pt = |x: f64, y: f64| Pt(x * scale + x_offset, -y * scale + y_offset);

    let n = points.len();
    let start_idx = points.iter().position(|p| p.2);
    let start_idx = match start_idx {
        Some(idx) => idx,
        None => {
            let mx = (points[0].0 + points[n - 1].0) / 2.0;
            let my = (points[0].1 + points[n - 1].1) / 2.0;
            result.push(to_pt(mx, my));
            0
        }
    };

    if result.is_empty() {
        result.push(to_pt(points[start_idx].0, points[start_idx].1));
    }

    let mut i = (start_idx + 1) % n;
    let mut count = 0;
    while count < n {
        let (x, y, on_curve) = points[i];
        let next_i = (i + 1) % n;

        if on_curve {
            result.push(to_pt(x, y));
        } else {
            let (nx, ny, n_on) = points[next_i];
            if n_on {
                // Quadratic bezier.
                let p0 = *result.last().unwrap();
                flatten_quad(&mut result, p0, to_pt(x, y), to_pt(nx, ny));
                i = (next_i + 1) % n;
                count += 2;
                continue;
            } else {
                let nni = (next_i + 1) % n;
                let (nnx, nny, nn_on) = points[nni];
                if nn_on {
                    // Cubic bezier.
                    let p0 = *result.last().unwrap();
                    flatten_cubic(&mut result, p0, to_pt(x, y), to_pt(nx, ny), to_pt(nnx, nny));
                    i = (nni + 1) % n;
                    count += 3;
                    continue;
                } else {
                    // Quadratic with implied midpoint.
                    let mx = (x + nx) / 2.0;
                    let my = (y + ny) / 2.0;
                    let p0 = *result.last().unwrap();
                    flatten_quad(&mut result, p0, to_pt(x, y), to_pt(mx, my));
                }
            }
        }

        i = next_i;
        count += 1;
    }

    result
}

fn flatten_quad(result: &mut Vec<Pt>, p0: Pt, p1: Pt, p2: Pt) {
    let devx = p0.0 - 2.0 * p1.0 + p2.0;
    let devy = p0.1 - 2.0 * p1.1 + p2.1;
    let devsq = devx * devx + devy * devy;
    if devsq < 0.025 {
        result.push(p2);
        return;
    }
    let n = 1 + (3.0 * devsq).sqrt().sqrt().floor() as usize;
    let mut p = p0;
    let nr = 1.0 / n as f64;
    let mut t = 0.0;
    for _ in 0..n - 1 {
        t += nr;
        let pn = lerp2(t, lerp2(t, p0, p1), lerp2(t, p1, p2));
        result.push(pn);
        p = pn;
    }
    let _ = p;
    result.push(p2);
}

fn flatten_cubic(result: &mut Vec<Pt>, p0: Pt, p1: Pt, p2: Pt, p3: Pt) {
    flatten_cubic_rec(result, p0, p1, p2, p3, 0);
}

fn flatten_cubic_rec(result: &mut Vec<Pt>, p0: Pt, p1: Pt, p2: Pt, p3: Pt, depth: u8) {
    let longlen = dist(p0, p1) + dist(p1, p2) + dist(p2, p3);
    let shortlen = dist(p0, p3);
    let flatness_sq = longlen * longlen - shortlen * shortlen;
    if depth < 16 && flatness_sq > 0.05 * 0.05 {
        let p01 = lerp2(0.5, p0, p1);
        let p12 = lerp2(0.5, p1, p2);
        let p23 = lerp2(0.5, p2, p3);
        let pa = lerp2(0.5, p01, p12);
        let pb = lerp2(0.5, p12, p23);
        let mp = lerp2(0.5, pa, pb);
        flatten_cubic_rec(result, p0, p01, pa, mp, depth + 1);
        flatten_cubic_rec(result, mp, pb, p23, p3, depth + 1);
    } else {
        result.push(p3);
    }
}

fn lerp2(t: f64, a: Pt, b: Pt) -> Pt {
    Pt(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

fn dist(a: Pt, b: Pt) -> f64 {
    ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rasterize_empty() {
        assert!(rasterize_outline(&[], 1.0, 0.0, 0.0, None, None).is_none());
    }

    #[test]
    fn rasterize_zero_scale() {
        let c = vec![(0.0, 0.0, true), (100.0, 0.0, true), (100.0, 100.0, true)];
        assert!(rasterize_outline(&[c], 0.0, 0.0, 0.0, None, None).is_none());
    }

    #[test]
    fn rasterize_triangle() {
        let c = vec![(0.0, 0.0, true), (500.0, 0.0, true), (250.0, 500.0, true)];
        let bm = rasterize_outline(&[c], 0.05, 0.0, 25.0, None, None).unwrap();
        assert!(bm.width > 0 && bm.height > 0);
    }

    #[test]
    fn rasterize_quadratic() {
        let c = vec![(0.0, 0.0, true), (250.0, 500.0, false), (500.0, 0.0, true)];
        assert!(rasterize_outline(&[c], 0.05, 0.0, 25.0, None, None).is_some());
    }

    #[test]
    fn adjust_cursor_shift_zero_shifts_is_identity() {
        // X-axis auto-hinting disabled (default): all shifts are 0, compensation
        // is a no-op, behavior matches pre-M-38 rendering.
        let cursor = 123.456;
        let adjusted = adjust_cursor_for_shift(cursor, 0.0, 0.0);
        assert_eq!(adjusted, cursor);
    }

    #[test]
    fn adjust_cursor_shift_matches_freetype_continuous_model() {
        // FreeType's continuous-pen advance compensation:
        // adjusted = cursor + prev_rsb_shift - current_lsb_shift.
        //
        // Example chain: two auto-hinted glyphs where the first has
        // rsb_delta = +0.375 px (its right edge snapped right by 3/8 px)
        // and the second has lsb_delta = -0.125 px (its left edge snapped
        // left by 1/8 px). The second glyph's composite position should
        // be cursor + 0.375 - (-0.125) = cursor + 0.5 px, preserving
        // the hinter's left-edge intention relative to the first glyph's
        // right edge.
        let cursor_after_advance = 40.0;
        let prev_rsb = 0.375;
        let current_lsb = -0.125;
        let adjusted = adjust_cursor_for_shift(cursor_after_advance, prev_rsb, current_lsb);
        assert!(
            (adjusted - 40.5).abs() < 1e-12,
            "expected 40.5, got {}",
            adjusted
        );
    }

    #[test]
    fn adjust_cursor_shift_chain_accumulates_full_precision() {
        // A three-glyph chain: the cursor's full-precision adjusted position
        // after glyph N+1 equals advance_N + prev_rsb_N - lsb_{N+1}, tracked
        // through the whole line. We do NOT lose precision to floor() at
        // the advance step: floor only happens at the moment the glyph is
        // composited. The next iteration starts from the unfloored cursor.
        //
        // Simulated chain:
        //   glyph 0: lsb_shift=0.0, rsb_shift=+0.25, advance=10.0
        //   glyph 1: lsb_shift=-0.10, rsb_shift=+0.40, advance=11.0
        //   glyph 2: lsb_shift=+0.05, rsb_shift=0.0, advance=9.0
        // Starting cursor 0.0.
        //
        // After glyph 0: cursor advances to 10.0, prev_rsb = +0.25.
        // Before glyph 1 composite: adjusted = 10.0 + 0.25 - (-0.10) = 10.35.
        // Cursor continues from 10.0 (unadjusted for floor), advances to 21.0.
        // Before glyph 2 composite: adjusted = 21.0 + 0.40 - 0.05 = 21.35.
        let g0_rsb = 0.25;
        let g1_lsb = -0.10;
        let g1_rsb = 0.40;
        let g2_lsb = 0.05;

        let cursor_after_0 = 10.0;
        // glyph 0 is first, no compensation applied -> identity.
        let adjusted_for_0 = adjust_cursor_for_shift(0.0, 0.0, 0.0);
        assert!(
            (adjusted_for_0 - 0.0).abs() < 1e-12,
            "first glyph expected identity 0.0, got {}",
            adjusted_for_0
        );
        let adjusted_for_1 = adjust_cursor_for_shift(cursor_after_0, g0_rsb, g1_lsb);
        assert!(
            (adjusted_for_1 - 10.35).abs() < 1e-12,
            "expected 10.35, got {}",
            adjusted_for_1
        );

        // After glyph 1 compositing the raw cursor keeps advancing from its
        // un-floored value (the compensation only affects where the glyph is
        // DRAWN, not the accumulated pen). cursor += advance_1:
        let cursor_after_1 = cursor_after_0 + 11.0;
        let adjusted_for_2 = adjust_cursor_for_shift(cursor_after_1, g1_rsb, g2_lsb);
        assert!(
            (adjusted_for_2 - 21.35).abs() < 1e-12,
            "expected 21.35, got {}",
            adjusted_for_2
        );
    }

    #[test]
    fn bitmap_dimensions_scale() {
        let c = vec![
            (0.0, 0.0, true),
            (1000.0, 0.0, true),
            (1000.0, 1000.0, true),
            (0.0, 1000.0, true),
        ];
        let b1 = rasterize_outline(std::slice::from_ref(&c), 0.01, 0.0, 10.0, None, None).unwrap();
        let b2 = rasterize_outline(&[c], 0.02, 0.0, 20.0, None, None).unwrap();
        assert!(b2.width > b1.width);
    }
}

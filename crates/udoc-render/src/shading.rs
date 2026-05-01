//! Shading-pattern rasterizer for the `sh` operator (
//!, ISO 32000-2 §8.7.4).
//!
//! Consumes [`PaintShading`] records from the presentation overlay and
//! paints them onto the RGB pixel buffer. Two gradient types:
//!
//! * **Axial (linear):** scan every pixel that falls inside the page
//!   bbox; project its device-space coordinate onto the (P0 -> P1)
//!   axis to obtain `t in R`; clamp (or extend) and look up the color
//!   from the shading's pre-sampled 256-entry sRGB LUT.
//! * **Radial (two-circle):** every pixel's circle-interpolation
//!   parameter `t` is the valid root of a quadratic in `t` derived
//!   from the center/radius interpolation `C(t) = (1-t) C0 + t C1,
//!   r(t) = (1-t) r0 + t r1`; pick the `t` in `[0, 1]` (or the
//!   extended range when Extend is set).
//!
//! Function evaluation is done in [`udoc_pdf::content::shading`] at
//! dict parse time; this module only performs the geometric part.
//!
//! The rasterizer writes to the full page bbox; callers that need
//! clipping should integrate the clip stack at composite time. For
//! v1 the assumption is that 'sh' operators are the dominant visual
//! element at their z-index, which is true for gradient backgrounds
//! and logo panels.

use udoc_core::document::presentation::{PaintShading, PaintShadingKind};

use crate::compositor::blend_pixel;

/// Rasterize a [`PaintShading`] into `pixels`.
///
/// `width` / `height` are pixel dimensions. `page_height` is the
/// PDF-units page height (used to flip from PDF y-up to image y-down).
/// `scale` is the DPI factor (`dpi / 72`).
///
/// The operation is O(w * h) for the whole page; callers that know
/// the shading occupies a sub-region should pre-clip via the gstate
/// clip stack. This matches mupdf's scan-per-pixel approach and is
/// fast enough at 150-300 DPI.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rasterize_paint_shading(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    shading: &PaintShading,
    page_origin_x: f64,
    page_height: f64,
    scale: f64,
) {
    match &shading.kind {
        PaintShadingKind::Axial {
            p0x,
            p0y,
            p1x,
            p1y,
            samples,
            extend_start,
            extend_end,
        } => {
            if samples.is_empty() {
                return;
            }
            rasterize_axial(
                pixels,
                width,
                height,
                shading.ctm,
                page_origin_x,
                page_height,
                scale,
                (*p0x, *p0y),
                (*p1x, *p1y),
                samples,
                *extend_start,
                *extend_end,
                shading.alpha,
            );
        }
        PaintShadingKind::Radial {
            c0x,
            c0y,
            r0,
            c1x,
            c1y,
            r1,
            samples,
            extend_start,
            extend_end,
        } => {
            if samples.is_empty() {
                return;
            }
            rasterize_radial(
                pixels,
                width,
                height,
                shading.ctm,
                page_origin_x,
                page_height,
                scale,
                (*c0x, *c0y),
                *r0,
                (*c1x, *c1y),
                *r1,
                samples,
                *extend_start,
                *extend_end,
                shading.alpha,
            );
        }
        PaintShadingKind::Unsupported { .. } => {
            // Diagnostic already emitted by the PDF interpreter. Skip.
        }
        _ => {
            // Future shading kinds added upstream land here as skip.
        }
    }
}

/// Project a user-space point through the CTM + y-flip + DPI scale, with the
/// page-origin offset applied on the X axis.
#[inline]
fn to_device(
    x: f64,
    y: f64,
    ctm: [f64; 6],
    page_origin_x: f64,
    page_height: f64,
    scale: f64,
) -> (f64, f64) {
    let dx = x * ctm[0] + y * ctm[2] + ctm[4];
    let dy = x * ctm[1] + y * ctm[3] + ctm[5];
    ((dx - page_origin_x) * scale, (page_height - dy) * scale)
}

#[allow(clippy::too_many_arguments)]
fn rasterize_axial(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    ctm: [f64; 6],
    page_origin_x: f64,
    page_height: f64,
    scale: f64,
    p0_user: (f64, f64),
    p1_user: (f64, f64),
    samples: &[[u8; 3]],
    extend_start: bool,
    extend_end: bool,
    alpha: u8,
) {
    // Transform endpoints to device space. Pixel coordinates are
    // relative to the same buffer.
    let (p0x, p0y) = to_device(p0_user.0, p0_user.1, ctm, page_origin_x, page_height, scale);
    let (p1x, p1y) = to_device(p1_user.0, p1_user.1, ctm, page_origin_x, page_height, scale);

    let dx = p1x - p0x;
    let dy = p1y - p0y;
    let axis_len_sq = dx * dx + dy * dy;
    if axis_len_sq < 1e-9 {
        // Degenerate axis -- draw solid middle-of-ramp color.
        let mid = samples[samples.len() / 2];
        fill_solid(pixels, width, height, mid, alpha);
        return;
    }

    let lut_n = samples.len();
    let w = width as usize;
    let h = height as usize;
    for py in 0..h {
        let fy = py as f64 + 0.5;
        for px in 0..w {
            let fx = px as f64 + 0.5;
            // Project (fx, fy) onto the gradient axis.
            let t = ((fx - p0x) * dx + (fy - p0y) * dy) / axis_len_sq;

            let color = if t < 0.0 {
                if extend_start {
                    samples[0]
                } else {
                    continue;
                }
            } else if t > 1.0 {
                if extend_end {
                    samples[lut_n - 1]
                } else {
                    continue;
                }
            } else {
                let idx = (t * (lut_n - 1) as f64).round() as usize;
                samples[idx.min(lut_n - 1)]
            };

            blend_pixel(pixels, width, height, px as i32, py as i32, color, alpha);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn rasterize_radial(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    ctm: [f64; 6],
    page_origin_x: f64,
    page_height: f64,
    scale: f64,
    c0_user: (f64, f64),
    r0_user: f64,
    c1_user: (f64, f64),
    r1_user: f64,
    samples: &[[u8; 3]],
    extend_start: bool,
    extend_end: bool,
    alpha: u8,
) {
    // Transform the centres through the CTM.
    let (c0x, c0y) = to_device(c0_user.0, c0_user.1, ctm, page_origin_x, page_height, scale);
    let (c1x, c1y) = to_device(c1_user.0, c1_user.1, ctm, page_origin_x, page_height, scale);

    // Scale radii by the CTM's average linear scale * DPI scale.
    // For uniform / near-uniform CTMs this is exact; shear/non-uniform
    // scale gets the geometric mean, which is what mupdf does.
    let sx = (ctm[0] * ctm[0] + ctm[1] * ctm[1]).sqrt();
    let sy = (ctm[2] * ctm[2] + ctm[3] * ctm[3]).sqrt();
    let ctm_scale = (sx * sy).sqrt();
    let r0 = r0_user * ctm_scale * scale;
    let r1 = r1_user * ctm_scale * scale;

    // Solve the quadratic at every pixel. See PDF 1.7 §7.9.4.3:
    //   (x - (cx0 + t*dcx))^2 + (y - (cy0 + t*dcy))^2 = (r0 + t*dr)^2
    // Expanded in `t`:  A t^2 + B t + C = 0
    let dcx = c1x - c0x;
    let dcy = c1y - c0y;
    let dr = r1 - r0;
    let a_quad = dcx * dcx + dcy * dcy - dr * dr;

    let lut_n = samples.len();
    let w = width as usize;
    let h = height as usize;
    for py in 0..h {
        let fy = py as f64 + 0.5;
        for px in 0..w {
            let fx = px as f64 + 0.5;
            let qx = fx - c0x;
            let qy = fy - c0y;

            // Solve A t^2 - 2 B t + C = 0 with
            //   B = qx*dcx + qy*dcy + r0*dr
            //   C = qx*qx + qy*qy - r0*r0
            let b_half = qx * dcx + qy * dcy + r0 * dr;
            let c = qx * qx + qy * qy - r0 * r0;

            let t_opt = if a_quad.abs() < 1e-9 {
                // Linear case: degenerate radial collapses to axial.
                // 2*b_half*t = c  ->  t = c / (2 * b_half).
                if b_half.abs() < 1e-9 {
                    None
                } else {
                    Some(c / (2.0 * b_half))
                }
            } else {
                let disc = b_half * b_half - a_quad * c;
                if disc < 0.0 {
                    None
                } else {
                    let root = disc.sqrt();
                    let t_plus = (b_half + root) / a_quad;
                    let t_minus = (b_half - root) / a_quad;
                    // Pick the larger `t` as PDF spec §7.9.4.3 prescribes
                    // (the front-facing solution), but only if its
                    // circle has a non-negative radius.
                    let candidate = |t: f64| {
                        let r_at_t = r0 + t * dr;
                        if r_at_t < 0.0 {
                            None
                        } else {
                            Some(t)
                        }
                    };
                    match (candidate(t_plus), candidate(t_minus)) {
                        (Some(a), Some(b)) => Some(a.max(b)),
                        (Some(a), None) => Some(a),
                        (None, Some(b)) => Some(b),
                        (None, None) => None,
                    }
                }
            };

            let Some(t) = t_opt else {
                continue;
            };

            let color = if t < 0.0 {
                if extend_start {
                    samples[0]
                } else {
                    continue;
                }
            } else if t > 1.0 {
                if extend_end {
                    samples[lut_n - 1]
                } else {
                    continue;
                }
            } else {
                let idx = (t * (lut_n - 1) as f64).round() as usize;
                samples[idx.min(lut_n - 1)]
            };

            blend_pixel(pixels, width, height, px as i32, py as i32, color, alpha);
        }
    }
}

/// Fill the entire pixel buffer with a single color (used for the
/// degenerate axial case where P0 == P1 -- spec says paint the middle
/// color but mupdf / pdfium just use the ramp midpoint).
fn fill_solid(pixels: &mut [u8], width: u32, height: u32, color: [u8; 3], alpha: u8) {
    let w = width as usize;
    let h = height as usize;
    for py in 0..h {
        for px in 0..w {
            blend_pixel(pixels, width, height, px as i32, py as i32, color, alpha);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_ctm() -> [f64; 6] {
        [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
    }

    fn linear_ramp_red() -> Vec<[u8; 3]> {
        (0..256).map(|i| [i as u8, 0, 0]).collect()
    }

    #[test]
    fn axial_paints_gradient_across_width() {
        // 100x10 buffer, axis (0,5) -> (100,5). t=0 at left, t=1 at right.
        let w = 100u32;
        let h = 10u32;
        let mut pixels = vec![255u8; (w * h * 3) as usize];
        let shading = PaintShading::new(
            0,
            PaintShadingKind::Axial {
                p0x: 0.0,
                p0y: 5.0,
                p1x: 100.0,
                p1y: 5.0,
                samples: linear_ramp_red(),
                extend_start: false,
                extend_end: false,
            },
            identity_ctm(),
            255,
            0,
        );
        // page_height=10, scale=1 means device space maps 1:1 after y-flip.
        rasterize_paint_shading(&mut pixels, w, h, &shading, 0.0, 10.0, 1.0);

        // Leftmost column (px=0) should be ~red=0 (dark red = black).
        let left = pixels[0];
        // Rightmost column (px=99) should be red=~255.
        let right_idx = (99 * 3) as usize;
        let right = pixels[right_idx];
        assert!(left < 20, "left should be near-black, got r={}", left);
        assert!(right > 235, "right should be near-red=255, got r={}", right);
    }

    #[test]
    fn axial_degenerate_axis_emits_middle_color() {
        let w = 10u32;
        let h = 10u32;
        let mut pixels = vec![255u8; (w * h * 3) as usize];
        let shading = PaintShading::new(
            0,
            PaintShadingKind::Axial {
                p0x: 5.0,
                p0y: 5.0,
                p1x: 5.0,
                p1y: 5.0, // Identical points.
                samples: linear_ramp_red(),
                extend_start: true,
                extend_end: true,
            },
            identity_ctm(),
            255,
            0,
        );
        rasterize_paint_shading(&mut pixels, w, h, &shading, 0.0, 10.0, 1.0);
        // Middle-of-ramp red should paint every pixel.
        let r = pixels[0];
        assert!(r > 100 && r < 160, "expected mid-ramp red, got {}", r);
    }

    #[test]
    fn radial_paints_bull_pattern() {
        // c0=(5,5) r0=0 ; c1=(5,5) r1=5. t=0 at center -> black; t=1 at edge -> red.
        let w = 10u32;
        let h = 10u32;
        let mut pixels = vec![255u8; (w * h * 3) as usize];
        let shading = PaintShading::new(
            0,
            PaintShadingKind::Radial {
                c0x: 5.0,
                c0y: 5.0,
                r0: 0.0,
                c1x: 5.0,
                c1y: 5.0,
                r1: 5.0,
                samples: linear_ramp_red(),
                extend_start: false,
                extend_end: false,
            },
            identity_ctm(),
            255,
            0,
        );
        rasterize_paint_shading(&mut pixels, w, h, &shading, 0.0, 10.0, 1.0);

        // Center pixel (5,5 -> flipped (5,5)) should be near black.
        let cx = 5usize;
        let cy = 5usize;
        let idx_c = (cy * w as usize + cx) * 3;
        let center_r = pixels[idx_c];
        // Corner pixel at (0,0) lies outside r=5 from center (distance ~7), so extend_end=false -> still white.
        let idx_corner = 0;
        let corner_r = pixels[idx_corner];
        assert!(
            center_r < 100,
            "center should be darker (near black), got r={}",
            center_r
        );
        assert_eq!(
            corner_r, 255,
            "corner outside max radius + extend=false should stay white, got r={}",
            corner_r
        );
    }

    #[test]
    fn unsupported_kind_is_no_op() {
        let w = 10u32;
        let h = 10u32;
        let mut pixels = vec![200u8; (w * h * 3) as usize];
        let shading = PaintShading::new(
            0,
            PaintShadingKind::Unsupported { shading_type: 5 },
            identity_ctm(),
            255,
            0,
        );
        rasterize_paint_shading(&mut pixels, w, h, &shading, 0.0, 10.0, 1.0);
        // No changes.
        for &b in &pixels {
            assert_eq!(b, 200);
        }
    }
}

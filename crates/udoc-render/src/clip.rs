//! Real clip-mask rendering (ISO 32000-2 §8.5.4) and
//! soft-mask rendering ( extension, ISO 32000-2 §11.6.5).
//!
//! Two parallel stacks ride on q/Q alongside the graphics-state stack:
//!
//! - **Hard clip (W/W*)**: builds a binary pixel mask from a list of
//!   clipping regions. Each region is a polygon in device coordinates
//!   (already CTM-multiplied), rasterized via the standard scanline
//!   fill with the region's fill rule. Per-region masks are
//!   AND-intersected. One byte per pixel (255 allowed, 0 clipped).
//! - **Soft mask (ExtGState /SMask)**: builds a per-pixel alpha mask
//!   from a list of `SoftMaskLayer`s. Luminosity subtypes project RGB
//!   to Rec.709 luminance; Alpha subtypes take the mask byte directly.
//!   Stacked soft masks compose by min-combine (the most restrictive
//!   alpha wins, matching the "intersect" semantics the spec implies
//!   for nested transparency groups).
//!
//! Memory model for both: one byte per pixel so the per-pixel gate is
//! a single aligned load. Masks are built on demand at render time
//! (not cached per span), so the hot-path cost is pay-per-masked-op
//! rather than unconditional.

use udoc_core::document::presentation::{
    ClipRegion, ClipRegionFillRule, SoftMaskLayer, SoftMaskSubtype,
};

/// Binary clip mask aligned to the output pixel buffer.
///
/// `pixels[y * width + x]` is `255` if `(x, y)` is inside the effective
/// clip region, `0` if it is clipped out.
#[derive(Debug, Clone)]
pub(crate) struct ClipMask {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl ClipMask {
    /// Create a mask with every pixel allowed.
    fn full(width: u32, height: u32) -> Self {
        let n = width as usize * height as usize;
        Self {
            width,
            height,
            pixels: vec![255u8; n],
        }
    }

    /// `true` if `(x, y)` is inside the clip region. Out-of-bounds
    /// coordinates are rejected so the caller never writes a pixel that
    /// could not possibly be visible.
    #[inline]
    pub(crate) fn allows(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 {
            return false;
        }
        let (ux, uy) = (x as u32, y as u32);
        if ux >= self.width || uy >= self.height {
            return false;
        }
        let idx = uy as usize * self.width as usize + ux as usize;
        self.pixels[idx] != 0
    }
}

/// Build an effective clip mask from a non-empty list of clip regions.
///
/// - Each `ClipRegion` is rasterized independently to a binary mask
///   using its declared fill rule (NonZero or EvenOdd).
/// - The final mask is the bitwise AND of all per-region masks.
///
/// Regions live in page coordinates (y-up); `page_height` and `scale`
/// map them to device-image coordinates (y-down) the same way
/// `render_shape` does for visible fills.
///
/// Returns `None` if `clips` is empty (no clipping: caller should skip
/// the per-pixel gate entirely).
pub(crate) fn build_clip_mask(
    clips: &[ClipRegion],
    width: u32,
    height: u32,
    page_origin_x: f64,
    page_height: f64,
    scale: f64,
) -> Option<ClipMask> {
    if clips.is_empty() {
        return None;
    }
    if width == 0 || height == 0 {
        return None;
    }

    let mut combined = ClipMask::full(width, height);
    for clip in clips {
        let region = rasterize_region(clip, width, height, page_origin_x, page_height, scale);
        // Intersect: combined &= region.
        for (out, incoming) in combined.pixels.iter_mut().zip(region.pixels.iter()) {
            if *incoming == 0 {
                *out = 0;
            }
        }
    }

    Some(combined)
}

/// Rasterize a single clip region to a binary pixel mask.
fn rasterize_region(
    clip: &ClipRegion,
    width: u32,
    height: u32,
    page_origin_x: f64,
    page_height: f64,
    scale: f64,
) -> ClipMask {
    let mut mask = ClipMask {
        width,
        height,
        pixels: vec![0u8; width as usize * height as usize],
    };

    if clip.subpaths.is_empty() {
        return mask;
    }

    // Convert every vertex to device coordinates (y-down).
    let subpaths_device: Vec<Vec<(f64, f64)>> = clip
        .subpaths
        .iter()
        .map(|sub| {
            sub.iter()
                .map(|(x, y)| ((*x - page_origin_x) * scale, (page_height - *y) * scale))
                .collect()
        })
        .collect();

    let (ymin, ymax) = subpaths_device
        .iter()
        .flatten()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), (_, y)| {
            (lo.min(*y), hi.max(*y))
        });
    let y_start = (ymin.floor() as i32).max(0) as u32;
    let y_end = (ymax.ceil() as i32).min(height as i32).max(0) as u32;

    match clip.fill_rule {
        ClipRegionFillRule::NonZero => fill_nonzero(&subpaths_device, &mut mask, y_start, y_end),
        ClipRegionFillRule::EvenOdd => fill_evenodd(&subpaths_device, &mut mask, y_start, y_end),
    }
    mask
}

/// Scanline fill with even-odd parity rule.
fn fill_evenodd(subpaths: &[Vec<(f64, f64)>], mask: &mut ClipMask, y_start: u32, y_end: u32) {
    let width = mask.width;
    let mut xs: Vec<f64> = Vec::new();

    for y in y_start..y_end {
        let y_center = y as f64 + 0.5;
        xs.clear();

        for sub in subpaths {
            if sub.len() < 2 {
                continue;
            }
            for i in 0..sub.len() {
                let (x0, y0) = sub[i];
                let (x1, y1) = sub[(i + 1) % sub.len()];
                // Crossings rule: include the lower endpoint, exclude the upper,
                // so horizontal edges don't double-count.
                if (y0 <= y_center && y1 > y_center) || (y1 <= y_center && y0 > y_center) {
                    let t = (y_center - y0) / (y1 - y0);
                    xs.push(x0 + t * (x1 - x0));
                }
            }
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let row_off = y as usize * width as usize;
        let mut i = 0;
        while i + 1 < xs.len() {
            let x0 = xs[i].max(0.0) as u32;
            let x1 = xs[i + 1].min(width as f64) as u32;
            if x1 > x0 {
                for px in x0..x1 {
                    mask.pixels[row_off + px as usize] = 255;
                }
            }
            i += 2;
        }
    }
}

/// Scanline fill with non-zero winding rule.
fn fill_nonzero(subpaths: &[Vec<(f64, f64)>], mask: &mut ClipMask, y_start: u32, y_end: u32) {
    let width = mask.width;
    // Each crossing carries its sign (+1 for upward, -1 for downward).
    let mut xs: Vec<(f64, i32)> = Vec::new();

    for y in y_start..y_end {
        let y_center = y as f64 + 0.5;
        xs.clear();

        for sub in subpaths {
            if sub.len() < 2 {
                continue;
            }
            for i in 0..sub.len() {
                let (x0, y0) = sub[i];
                let (x1, y1) = sub[(i + 1) % sub.len()];
                let (dir, crosses) = if y0 <= y_center && y1 > y_center {
                    (1i32, true)
                } else if y1 <= y_center && y0 > y_center {
                    (-1i32, true)
                } else {
                    (0, false)
                };
                if crosses {
                    let t = (y_center - y0) / (y1 - y0);
                    xs.push((x0 + t * (x1 - x0), dir));
                }
            }
        }
        xs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        // Walk left to right; fill where winding != 0.
        let row_off = y as usize * width as usize;
        let mut winding = 0i32;
        for window in xs.windows(2) {
            let (x_in, dir_in) = window[0];
            let (x_out, _) = window[1];
            winding += dir_in;
            if winding != 0 {
                let lo = x_in.max(0.0) as u32;
                let hi = x_out.min(width as f64) as u32;
                if hi > lo {
                    for px in lo..hi {
                        mask.pixels[row_off + px as usize] = 255;
                    }
                }
            }
        }
    }
}

/// Per-pixel alpha mask, one byte per pixel (0 = fully masked out,
/// 255 = fully opaque). Parallel to `ClipMask` but carries continuous
/// alpha rather than binary coverage, matching ISO 32000-2 §11.6.5's
/// "per-pixel alpha mask to subsequent paint ops" model.
#[derive(Debug, Clone)]
pub(crate) struct SoftMask {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl SoftMask {
    /// Sample the mask at `(x, y)`. Out-of-bounds returns 255 (no
    /// masking): if the caller lands outside the mask buffer, we let
    /// the pixel through unaltered rather than artificially occluding
    /// it. The caller is responsible for clamping to the intended
    /// paint region.
    #[inline]
    pub(crate) fn alpha_at(&self, x: i32, y: i32) -> u8 {
        if x < 0 || y < 0 {
            return 255;
        }
        let (ux, uy) = (x as u32, y as u32);
        if ux >= self.width || uy >= self.height {
            return 255;
        }
        let idx = uy as usize * self.width as usize + ux as usize;
        self.pixels[idx]
    }
}

/// Build a combined soft-mask from a non-empty list of soft-mask
/// layers. Returns `None` if `layers` is empty (no masking).
///
/// Each layer's bitmap is mapped from its page-coordinate `bbox` to
/// device pixels using the same `page_origin_x` + `page_height` +
/// `scale` convention as the visible-content renderer. Pixels outside
/// a layer's bbox take the layer's `backdrop_alpha`. The combined
/// mask is the per-pixel min across all layers, so nested soft masks
/// compose as an intersection (the most restrictive alpha wins), which
/// matches what the spec implies for nested transparency groups.
///
/// This is the equivalent of [`build_clip_mask`] for the soft-mask
/// stack. Returning `None` lets the caller skip the per-pixel gate
/// when no masking is active.
pub(crate) fn build_soft_mask(
    layers: &[SoftMaskLayer],
    width: u32,
    height: u32,
    page_origin_x: f64,
    page_height: f64,
    scale: f64,
) -> Option<SoftMask> {
    if layers.is_empty() {
        return None;
    }
    if width == 0 || height == 0 {
        return None;
    }

    let mut combined: Option<SoftMask> = None;
    for layer in layers {
        let rendered =
            rasterize_soft_mask_layer(layer, width, height, page_origin_x, page_height, scale);
        combined = Some(match combined {
            None => rendered,
            Some(mut prev) => {
                // Min-combine per pixel. Nested soft masks intersect:
                // every mask must admit a pixel for the pixel to survive.
                for (out, incoming) in prev.pixels.iter_mut().zip(rendered.pixels.iter()) {
                    if *incoming < *out {
                        *out = *incoming;
                    }
                }
                prev
            }
        });
    }

    combined
}

/// Rasterize a single soft-mask layer onto a device-pixel alpha mask.
fn rasterize_soft_mask_layer(
    layer: &SoftMaskLayer,
    width: u32,
    height: u32,
    page_origin_x: f64,
    page_height: f64,
    scale: f64,
) -> SoftMask {
    // Start every pixel at the layer's backdrop alpha. Pixels inside
    // the bbox will be overwritten by sampled mask values below.
    let n = width as usize * height as usize;
    let mut mask = SoftMask {
        width,
        height,
        pixels: vec![layer.backdrop_alpha; n],
    };

    if layer.width == 0 || layer.height == 0 || layer.mask.is_empty() {
        return mask;
    }

    // Mask bbox in device coordinates (y-down). page_origin_x and
    // page_height match the convention used for visible shapes.
    let dst_x0 = (layer.bbox.x_min - page_origin_x) * scale;
    let dst_x1 = (layer.bbox.x_max - page_origin_x) * scale;
    let dst_y0 = (page_height - layer.bbox.y_max) * scale;
    let dst_y1 = (page_height - layer.bbox.y_min) * scale;
    let dst_w = (dst_x1 - dst_x0).max(1e-9);
    let dst_h = (dst_y1 - dst_y0).max(1e-9);

    let px_lo = (dst_x0.floor() as i32).max(0) as u32;
    let px_hi = (dst_x1.ceil() as i32).min(width as i32).max(0) as u32;
    let py_lo = (dst_y0.floor() as i32).max(0) as u32;
    let py_hi = (dst_y1.ceil() as i32).min(height as i32).max(0) as u32;
    if px_hi <= px_lo || py_hi <= py_lo {
        return mask;
    }

    let mw = layer.width as f64;
    let mh = layer.height as f64;

    for py in py_lo..py_hi {
        // Center sample per pixel; map back to mask-space normalized
        // coordinates. Nearest neighbour is fine here -- the visible
        // effect of sub-pixel antialiasing on a SMask boundary is tiny
        // compared to the luminosity->alpha conversion itself.
        let dy = py as f64 + 0.5 - dst_y0;
        let my = ((dy / dst_h) * mh).floor().clamp(0.0, mh - 1.0) as usize;
        let row_src = my * layer.width as usize;
        let row_dst = py as usize * width as usize;

        for px in px_lo..px_hi {
            let dx = px as f64 + 0.5 - dst_x0;
            let mx = ((dx / dst_w) * mw).floor().clamp(0.0, mw - 1.0) as usize;
            mask.pixels[row_dst + px as usize] = layer.mask[row_src + mx];
        }
    }

    // Subtype note: we accept the mask bytes as pre-baked alpha for
    // both Luminosity and Alpha subtypes. Conversion (Rec.709 for
    // luminosity; alpha-channel extraction for alpha) happens at the
    // moment the form XObject is baked into the mask bitmap, not here,
    // because this renderer receives the bitmap already materialized.
    // See `luminosity_from_rgb` for the Rec.709 weights used when the
    // bitmap is materialized from an RGB form.
    let _ = layer.subtype;

    mask
}

/// Rec.709 luminance weight conversion (ISO 32000-2 §11.6.5.2 for
/// `/S /Luminosity` SMasks): Y = 0.2126 R + 0.7152 G + 0.0722 B.
///
/// These are the sRGB / Rec.709 coefficients rather than the older
/// Rec.601 (0.299 / 0.587 / 0.114) because modern PDF producers (and
/// poppler/mupdf) use sRGB-linked grayscale conversion. The difference
/// is usually <1% per channel but adds up on pure-red/pure-blue masks.
///
/// Exposed as `pub(crate)` for PDF-backend plumbing that bakes a form
/// XObject RGB render into a luminosity bitmap. Unit-tested here.
#[inline]
#[allow(dead_code)] // used by tests + future interpreter integration
pub(crate) fn luminosity_from_rgb(r: u8, g: u8, b: u8) -> u8 {
    let y = 0.2126 * r as f64 + 0.7152 * g as f64 + 0.0722 * b as f64;
    y.round().clamp(0.0, 255.0) as u8
}

/// Materialize a soft-mask bitmap from an RGB form XObject render and
/// optional per-pixel alpha channel, given the layer's subtype. Used
/// by PDF-backend plumbing (and tests) to produce the `mask` field on
/// a `SoftMaskLayer` from a rendered form XObject.
///
/// - `rgb` is `width * height * 3` bytes, tightly packed.
/// - `alpha` is `width * height` bytes, or empty to treat the form as
///   fully opaque everywhere it paints.
/// - `subtype` selects conversion: `Luminosity` runs Rec.709 on the
///   RGB buffer, `Alpha` uses the alpha buffer byte-for-byte.
///
/// Returns a Vec with one byte per pixel, suitable for
/// `SoftMaskLayer::mask`.
#[allow(dead_code)] // used by tests + future interpreter integration
pub(crate) fn bake_soft_mask_bytes(
    rgb: &[u8],
    alpha: &[u8],
    width: u32,
    height: u32,
    subtype: SoftMaskSubtype,
) -> Vec<u8> {
    let n = width as usize * height as usize;
    let mut out = vec![0u8; n];
    match subtype {
        SoftMaskSubtype::Luminosity => {
            // If an alpha channel exists, modulate the luminance by it
            // so fully-transparent form pixels contribute nothing.
            // Otherwise take the RGB luminance straight.
            let has_alpha = alpha.len() >= n;
            for i in 0..n {
                if 3 * i + 2 >= rgb.len() {
                    break;
                }
                let y = luminosity_from_rgb(rgb[3 * i], rgb[3 * i + 1], rgb[3 * i + 2]);
                out[i] = if has_alpha {
                    ((y as u16 * alpha[i] as u16) / 255) as u8
                } else {
                    y
                };
            }
        }
        SoftMaskSubtype::Alpha => {
            if alpha.len() >= n {
                out[..n].copy_from_slice(&alpha[..n]);
            } else {
                // No alpha channel: spec-wise /S /Alpha on a form with
                // no alpha source is underspecified. Treat as fully
                // opaque so downstream paint isn't silently hidden.
                out.fill(255);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect_subpath(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
        vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
    }

    #[test]
    fn single_rect_clip_allows_inside_blocks_outside() {
        // Page: 200x200 pts. Clip: (50, 50)..(150, 150) in PDF coords.
        // Device (y-down) at scale 1: page_height=200 -> rect y: (150..50) down.
        let clip = ClipRegion {
            subpaths: vec![rect_subpath(50.0, 50.0, 150.0, 150.0)],
            fill_rule: ClipRegionFillRule::NonZero,
        };
        let mask = build_clip_mask(&[clip], 200, 200, 0.0, 200.0, 1.0).expect("mask");
        // Inside the clip rect in device coords: y in [50, 150), x in [50, 150).
        assert!(mask.allows(100, 100));
        assert!(!mask.allows(10, 10));
        assert!(!mask.allows(190, 190));
    }

    #[test]
    fn empty_clip_list_returns_none() {
        assert!(build_clip_mask(&[], 100, 100, 0.0, 100.0, 1.0).is_none());
    }

    #[test]
    fn intersection_of_two_rects_keeps_only_overlap() {
        let a = ClipRegion {
            subpaths: vec![rect_subpath(0.0, 0.0, 100.0, 100.0)],
            fill_rule: ClipRegionFillRule::NonZero,
        };
        let b = ClipRegion {
            subpaths: vec![rect_subpath(50.0, 50.0, 150.0, 150.0)],
            fill_rule: ClipRegionFillRule::NonZero,
        };
        let mask = build_clip_mask(&[a, b], 200, 200, 0.0, 200.0, 1.0).expect("mask");
        // Overlap: x in [50, 100), y device in [100, 150) ish.
        assert!(mask.allows(75, 125));
        // A-only region: x in [0, 50), y device in [100, 200) ish -> outside b.
        assert!(!mask.allows(10, 125));
        // B-only region: x in [100, 150), y device in [50, 100) ish -> outside a.
        assert!(!mask.allows(125, 75));
    }

    #[test]
    fn even_odd_rule_punches_hole_in_nested_rects() {
        // Outer 0..200 with inner 50..150 cancels out -> only the ring is in.
        let region = ClipRegion {
            subpaths: vec![
                rect_subpath(0.0, 0.0, 200.0, 200.0),
                rect_subpath(50.0, 50.0, 150.0, 150.0),
            ],
            fill_rule: ClipRegionFillRule::EvenOdd,
        };
        let mask = build_clip_mask(&[region], 200, 200, 0.0, 200.0, 1.0).expect("mask");
        // In the ring (between outer and inner): allowed.
        assert!(mask.allows(10, 100));
        // Inside the hole: blocked.
        assert!(!mask.allows(100, 100));
    }

    fn bbox(x_min: f64, y_min: f64, x_max: f64, y_max: f64) -> udoc_core::geometry::BoundingBox {
        udoc_core::geometry::BoundingBox::new(x_min, y_min, x_max, y_max)
    }

    #[test]
    fn soft_mask_empty_returns_none() {
        assert!(build_soft_mask(&[], 100, 100, 0.0, 100.0, 1.0).is_none());
    }

    #[test]
    fn soft_mask_alpha_subtype_samples_bitmap_into_bbox() {
        // 2x2 alpha bitmap, covering page rect (50, 50)..(150, 150).
        // Mask bytes (row-major, y-down within the mask space): 0, 64, 128, 255.
        let layer = SoftMaskLayer {
            subtype: SoftMaskSubtype::Alpha,
            mask: vec![0, 64, 128, 255],
            width: 2,
            height: 2,
            bbox: bbox(50.0, 50.0, 150.0, 150.0),
            backdrop_alpha: 255,
        };
        let mask = build_soft_mask(&[layer], 200, 200, 0.0, 200.0, 1.0).expect("mask");
        // Outside the bbox: backdrop alpha (255).
        assert_eq!(mask.alpha_at(10, 10), 255);
        assert_eq!(mask.alpha_at(190, 190), 255);
        // Inside the bbox, sample from the four quadrants. The bbox
        // spans device rows 50..150; top-left sample at (75, 75)
        // should land in mask cell (0, 0) = 0.
        assert_eq!(mask.alpha_at(75, 75), 0);
        // Top-right -> cell (1, 0) = 64.
        assert_eq!(mask.alpha_at(125, 75), 64);
        // Bottom-left -> cell (0, 1) = 128.
        assert_eq!(mask.alpha_at(75, 125), 128);
        // Bottom-right -> cell (1, 1) = 255.
        assert_eq!(mask.alpha_at(125, 125), 255);
    }

    #[test]
    fn soft_mask_luminosity_subtype_passes_baked_bytes_through() {
        // Luminosity-baked bytes go into SoftMaskLayer.mask already
        // converted. The rasterizer doesn't re-interpret them, it just
        // maps pixel-space to bbox-space. A single-pixel mask of 128
        // should read as 128 everywhere inside the bbox.
        let layer = SoftMaskLayer {
            subtype: SoftMaskSubtype::Luminosity,
            mask: vec![128],
            width: 1,
            height: 1,
            bbox: bbox(0.0, 0.0, 100.0, 100.0),
            backdrop_alpha: 0,
        };
        let mask = build_soft_mask(&[layer], 100, 100, 0.0, 100.0, 1.0).expect("mask");
        assert_eq!(mask.alpha_at(50, 50), 128);
    }

    #[test]
    fn soft_mask_backdrop_alpha_applies_outside_bbox() {
        let layer = SoftMaskLayer {
            subtype: SoftMaskSubtype::Alpha,
            mask: vec![255],
            width: 1,
            height: 1,
            bbox: bbox(40.0, 40.0, 60.0, 60.0),
            backdrop_alpha: 32, // heavy masking outside the form
        };
        let mask = build_soft_mask(&[layer], 100, 100, 0.0, 100.0, 1.0).expect("mask");
        // Outside bbox -> backdrop alpha.
        assert_eq!(mask.alpha_at(5, 5), 32);
        assert_eq!(mask.alpha_at(95, 95), 32);
        // Inside bbox -> mask byte.
        assert_eq!(mask.alpha_at(50, 50), 255);
    }

    #[test]
    fn soft_mask_stack_composes_by_min_combine() {
        // Two layers covering the same region with different alphas.
        // The min-combine should pick the smaller one per pixel
        // (intersection of nested transparency groups).
        let a = SoftMaskLayer {
            subtype: SoftMaskSubtype::Alpha,
            mask: vec![200; 4],
            width: 2,
            height: 2,
            bbox: bbox(0.0, 0.0, 100.0, 100.0),
            backdrop_alpha: 255,
        };
        let b = SoftMaskLayer {
            subtype: SoftMaskSubtype::Alpha,
            mask: vec![100; 4],
            width: 2,
            height: 2,
            bbox: bbox(0.0, 0.0, 100.0, 100.0),
            backdrop_alpha: 255,
        };
        let mask = build_soft_mask(&[a, b], 100, 100, 0.0, 100.0, 1.0).expect("mask");
        assert_eq!(mask.alpha_at(25, 25), 100);
        assert_eq!(mask.alpha_at(75, 75), 100);
    }

    #[test]
    fn luminosity_from_rgb_rec709_weights() {
        // Pure R: Y = 0.2126 * 255 = 54.2 -> 54.
        assert_eq!(luminosity_from_rgb(255, 0, 0), 54);
        // Pure G: Y = 0.7152 * 255 = 182.4 -> 182.
        assert_eq!(luminosity_from_rgb(0, 255, 0), 182);
        // Pure B: Y = 0.0722 * 255 = 18.4 -> 18.
        assert_eq!(luminosity_from_rgb(0, 0, 255), 18);
        // Pure white -> 255, pure black -> 0.
        assert_eq!(luminosity_from_rgb(255, 255, 255), 255);
        assert_eq!(luminosity_from_rgb(0, 0, 0), 0);
    }

    #[test]
    fn bake_soft_mask_bytes_luminosity_converts_rgb() {
        // 2x1: red then green.
        let rgb = [255, 0, 0, 0, 255, 0];
        let baked = bake_soft_mask_bytes(&rgb, &[], 2, 1, SoftMaskSubtype::Luminosity);
        assert_eq!(baked.len(), 2);
        assert_eq!(baked[0], 54); // red luminance
        assert_eq!(baked[1], 182); // green luminance
    }

    #[test]
    fn bake_soft_mask_bytes_alpha_extracts_alpha_channel() {
        let rgb = [255; 12]; // 4 pixels of white
        let alpha = [0, 85, 170, 255];
        let baked = bake_soft_mask_bytes(&rgb, &alpha, 2, 2, SoftMaskSubtype::Alpha);
        assert_eq!(baked, alpha);
    }

    #[test]
    fn bake_soft_mask_bytes_alpha_without_alpha_channel_is_opaque() {
        // /S /Alpha on a form with no alpha source: spec is
        // underdetermined; we default to fully opaque so downstream
        // paint isn't silently invisible.
        let rgb = [128; 6];
        let baked = bake_soft_mask_bytes(&rgb, &[], 2, 1, SoftMaskSubtype::Alpha);
        assert_eq!(baked, vec![255u8; 2]);
    }

    #[test]
    fn bake_soft_mask_bytes_luminosity_modulates_by_alpha() {
        // Solid white, 50% alpha -> luminosity 255 * 128 / 255 = 128.
        let rgb = [255, 255, 255];
        let alpha = [128];
        let baked = bake_soft_mask_bytes(&rgb, &alpha, 1, 1, SoftMaskSubtype::Luminosity);
        assert_eq!(baked, vec![128]);
    }
}

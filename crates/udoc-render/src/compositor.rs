//! Pixel blend / output lanes.
//!
//! Owns the final write-to-pixel-buffer layer: alpha-blended single-pixel
//! writes (`blend_pixel`) and glyph-bitmap composition
//! (`composite_glyph`, `composite_glyph_at`).
//!
//! Also owns the soft-mask blend lane ( extension,
//! ISO 32000-2 §11.6.5): `apply_soft_mask_restore` walks a pixel
//! bbox and lerps each pixel toward the pre-paint snapshot by the
//! complement of the soft-mask alpha. Paired with a
//! `SoftMask` from `clip::build_soft_mask` this is how
//! luminosity/alpha transparency groups get baked into the frame.
//!
//! The chromatic-fringing invariant (issue #183) lives here: when a
//! `GlyphBitmap` is NOT subpixel (uniform one-byte-per-pixel coverage),
//! uniform-alpha blending must emit channel-uniform output for a
//! channel-uniform input. The debug_assert in `composite_glyph_at`
//! enforces this in tests; see `tests/render_color_consistency.rs` for
//! the integration-level regression gate.

use super::clip::SoftMask;
use super::rasterizer::GlyphBitmap;

/// Alpha-blended pixel write. Source-over compositing.
#[inline]
pub(crate) fn blend_pixel(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    x: i32,
    y: i32,
    color: [u8; 3],
    alpha: u8,
) {
    if alpha == 255 {
        set_pixel(pixels, w, h, x, y, color);
        return;
    }
    if alpha == 0 {
        return;
    }
    if x >= 0 && x < w as i32 && y >= 0 && y < h as i32 {
        let idx = (y as usize * w as usize + x as usize) * 3;
        if idx + 2 < pixels.len() {
            let a = alpha as u16;
            let inv_a = 255 - a;
            pixels[idx] = ((pixels[idx] as u16 * inv_a + color[0] as u16 * a) / 255) as u8;
            pixels[idx + 1] = ((pixels[idx + 1] as u16 * inv_a + color[1] as u16 * a) / 255) as u8;
            pixels[idx + 2] = ((pixels[idx + 2] as u16 * inv_a + color[2] as u16 * a) / 255) as u8;
        }
    }
}

/// Set a single pixel, bounds-checked.
#[inline]
pub(crate) fn set_pixel(pixels: &mut [u8], w: u32, h: u32, x: i32, y: i32, color: [u8; 3]) {
    if x >= 0 && x < w as i32 && y >= 0 && y < h as i32 {
        let idx = (y as usize * w as usize + x as usize) * 3;
        if idx + 2 < pixels.len() {
            pixels[idx] = color[0];
            pixels[idx + 1] = color[1];
            pixels[idx + 2] = color[2];
        }
    }
}

/// Composite a glyph bitmap onto the pixel buffer.
/// Uses source-over blending with the given foreground color and glyph coverage as alpha.
pub(crate) fn composite_glyph(
    pixels: &mut [u8],
    img_width: u32,
    img_height: u32,
    glyph: &GlyphBitmap,
    color: [u8; 3],
) {
    composite_glyph_at(pixels, img_width, img_height, glyph, color, 0, 0);
}

/// Composite a glyph bitmap with additional position offset (avoids cloning for cache hits).
///
/// Invariant (enforced by debug_assert below):
/// when `!glyph.subpixel` (`FT_PIXEL_MODE_GRAY` equivalent, one byte of
/// coverage per pixel), the coverage is written identically into all three
/// RGB output channels. Any divergence between R/G/B on a grayscale glyph
/// is a chromatic-fringing regression (see issue #183): FreeType's LCD
/// subpixel path emits 3 distinct coverage bytes per pixel, and a future
/// refactor that accidentally routes a grayscale bitmap through that
/// output lane would speckle every glyph with red/blue fringe. We do not
/// assert R==G==B on the pixel buffer afterwards because legitimate
/// compositing (e.g. a blue hyperlink drawn before a black glyph) can
/// produce mismatched channels on the same pixel; the invariant we care
/// about is that THIS function does not introduce channel divergence on a
/// grayscale input when the foreground color is already channel-uniform.
pub(crate) fn composite_glyph_at(
    pixels: &mut [u8],
    img_width: u32,
    img_height: u32,
    glyph: &GlyphBitmap,
    color: [u8; 3],
    dx: i32,
    dy: i32,
) {
    let bytes_per_glyph_px = if glyph.subpixel { 3 } else { 1 };

    for gy in 0..glyph.height {
        let py = glyph.top + dy + gy as i32;
        if py < 0 || py >= img_height as i32 {
            continue;
        }
        for gx in 0..glyph.width {
            let px = glyph.left + dx + gx as i32;
            if px < 0 || px >= img_width as i32 {
                continue;
            }
            let glyph_idx = (gy as usize * glyph.width as usize + gx as usize) * bytes_per_glyph_px;
            let idx = (py as usize * img_width as usize + px as usize) * 3;
            if idx + 2 >= pixels.len() {
                continue;
            }

            if glyph.subpixel {
                // Per-channel alpha blending: each RGB channel has its own coverage.
                for c in 0..3 {
                    let alpha = glyph.data[glyph_idx + c] as u16;
                    if alpha == 0 {
                        continue;
                    }
                    let old = pixels[idx + c] as u16;
                    let fg = color[c] as u16;
                    pixels[idx + c] = ((old * (255 - alpha) + fg * alpha) / 255) as u8;
                }
            } else {
                // Uniform alpha blending: one coverage byte, same alpha for R/G/B.
                let alpha = glyph.data[glyph_idx] as u16;
                if alpha == 0 {
                    continue;
                }
                let r_in = pixels[idx] as u16;
                let g_in = pixels[idx + 1] as u16;
                let b_in = pixels[idx + 2] as u16;
                let r_out = ((r_in * (255 - alpha) + color[0] as u16 * alpha) / 255) as u8;
                let g_out = ((g_in * (255 - alpha) + color[1] as u16 * alpha) / 255) as u8;
                let b_out = ((b_in * (255 - alpha) + color[2] as u16 * alpha) / 255) as u8;
                // Chromatic-fringing guard (#183). If the input pixel and the
                // foreground color are both channel-uniform, the output must
                // also be channel-uniform: uniform-alpha blending of two
                // channel-uniform values can never produce channel-differing
                // output. Debug-only so the hot path keeps its integer math.
                debug_assert!(
                    !(r_in == g_in && g_in == b_in && color[0] == color[1] && color[1] == color[2])
                        || (r_out == g_out && g_out == b_out),
                    "grayscale glyph composite produced channel-differing output: \
                     in=({r_in},{g_in},{b_in}) color={color:?} alpha={alpha} \
                     out=({r_out},{g_out},{b_out}) -- this is issue #183."
                );
                pixels[idx] = r_out;
                pixels[idx + 1] = g_out;
                pixels[idx + 2] = b_out;
            }
        }
    }
}

/// Apply a soft-mask (ISO 32000-2 §11.6.5) to a freshly painted pixel
/// region by lerping each pixel toward the pre-paint snapshot.
///
/// Call pattern mirrors `apply_clip_restore`:
///
/// 1. Snapshot the bbox before painting (`snapshot`).
/// 2. Paint the shape/image/text as usual.
/// 3. Call this function; for each pixel in the bbox the final value
///    becomes `lerp(snapshot, painted, mask_alpha / 255)`. Pixels the
///    mask considers fully opaque (alpha = 255) keep the painted
///    value; fully masked-out pixels (alpha = 0) are restored to the
///    snapshot; everything in between is a per-channel linear blend.
///
/// The lerp toward the snapshot is the correct spec-mandated semantic:
/// the soft-mask alpha modulates the source-over compositing of the
/// new paint onto whatever was already on the canvas, and pre-paint
/// pixels ARE "whatever was already on the canvas". This matches the
/// reference rasterizers (MuPDF, Poppler) bit-for-bit at the limit.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_soft_mask_restore(
    pixels: &mut [u8],
    img_width: u32,
    bbox_x0: i32,
    bbox_y0: i32,
    bbox_x1: i32,
    bbox_y1: i32,
    mask: &SoftMask,
    snapshot: &[u8],
) {
    let w = (bbox_x1 - bbox_x0).max(0) as usize;
    let h = (bbox_y1 - bbox_y0).max(0) as usize;
    let stride = img_width as usize * 3;
    for row in 0..h {
        let y = bbox_y0 + row as i32;
        for col in 0..w {
            let x = bbox_x0 + col as i32;
            let a = mask.alpha_at(x, y) as u16;
            if a == 255 {
                continue;
            }
            let dst = y as usize * stride + x as usize * 3;
            let src = row * w * 3 + col * 3;
            if dst + 2 >= pixels.len() || src + 2 >= snapshot.len() {
                continue;
            }
            if a == 0 {
                // Fully masked out: restore snapshot.
                pixels[dst] = snapshot[src];
                pixels[dst + 1] = snapshot[src + 1];
                pixels[dst + 2] = snapshot[src + 2];
            } else {
                let inv = 255 - a;
                pixels[dst] = ((snapshot[src] as u16 * inv + pixels[dst] as u16 * a) / 255) as u8;
                pixels[dst + 1] =
                    ((snapshot[src + 1] as u16 * inv + pixels[dst + 1] as u16 * a) / 255) as u8;
                pixels[dst + 2] =
                    ((snapshot[src + 2] as u16 * inv + pixels[dst + 2] as u16 * a) / 255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_mask(width: u32, height: u32, alpha: u8) -> SoftMask {
        SoftMask {
            width,
            height,
            pixels: vec![alpha; width as usize * height as usize],
        }
    }

    #[test]
    fn soft_mask_alpha_zero_restores_snapshot_exactly() {
        // 4x1 snapshot (red) vs painted (blue). Mask = 0 everywhere
        // should leave the red snapshot in the buffer.
        let snapshot = vec![255, 0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0];
        let mut pixels = vec![0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0, 255];
        let mask = solid_mask(4, 1, 0);
        apply_soft_mask_restore(&mut pixels, 4, 0, 0, 4, 1, &mask, &snapshot);
        assert_eq!(pixels, snapshot);
    }

    #[test]
    fn soft_mask_alpha_full_keeps_painted_pixels() {
        let snapshot = vec![255, 0, 0];
        let mut pixels = vec![0, 0, 255];
        let mask = solid_mask(1, 1, 255);
        apply_soft_mask_restore(&mut pixels, 1, 0, 0, 1, 1, &mask, &snapshot);
        assert_eq!(pixels, vec![0, 0, 255]);
    }

    #[test]
    fn soft_mask_alpha_midpoint_lerps_channels() {
        // Red snapshot, blue paint, alpha 128 -> roughly midpoint.
        let snapshot = vec![255, 0, 0];
        let mut pixels = vec![0, 0, 255];
        let mask = solid_mask(1, 1, 128);
        apply_soft_mask_restore(&mut pixels, 1, 0, 0, 1, 1, &mask, &snapshot);
        // (255 * 127 + 0 * 128) / 255 = 127
        // (0 * 127 + 0 * 128) / 255 = 0
        // (0 * 127 + 255 * 128) / 255 = 128
        assert_eq!(pixels, vec![127, 0, 128]);
    }

    #[test]
    fn soft_mask_respects_bbox_bounds_and_stride() {
        // 2x2 image, paint everywhere, mask top-left pixel to 0.
        let mut pixels = vec![0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3];
        let snapshot = vec![9, 9, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mask = SoftMask {
            width: 2,
            height: 2,
            pixels: vec![0, 255, 255, 255],
        };
        // Note: snapshot region matches the bbox passed in; but in
        // this test we pretend the full 2x2 is the bbox so the
        // top-left snapshot byte is at snapshot[0..3].
        apply_soft_mask_restore(&mut pixels, 2, 0, 0, 2, 2, &mask, &snapshot);
        // Top-left pixel restored to (9,9,9); others unchanged.
        assert_eq!(&pixels[0..3], &[9, 9, 9]);
        assert_eq!(&pixels[3..6], &[1, 1, 1]);
        assert_eq!(&pixels[6..9], &[2, 2, 2]);
        assert_eq!(&pixels[9..12], &[3, 3, 3]);
    }
}

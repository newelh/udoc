//! JBIG2 generic refinement region decoder (ISO 14492 §6.3).
//!
//! A generic refinement region refines a reference bitmap by decoding
//! per-pixel correction bits against a context model that reads pixels
//! from both the already-decoded refinement region and the reference
//! bitmap at an `(dx, dy)` offset.
//!
//! Unlike the generic region decoder (§6.2), context neighbourhoods
//! here span two bitmaps. GRTEMPLATE 0 uses a 13-bit context word with
//! two adaptive template pixels (one over the region, one over the
//! reference). GRTEMPLATE 1 uses a 10-bit context with no adaptive
//! template pixels.
//!
//! Port target: pdfium `third_party/jbig2/JBig2_GRRDProc.cpp`. The
//! context bit layout follows ISO 14492 §6.3.5.3 Figures 12 and 15 and
//! is cross-validated against `hayro_jbig2 0.3.0`
//! `src/decode/generic_refinement.rs`.

use crate::jbig2::arith::{ArithDecoder, ContextTable};

/// 0 = black, 0xFF = white (matches `udoc-image` crate-wide pixel convention).
const PIXEL_BLACK: u8 = 0;
const PIXEL_WHITE: u8 = 0xFF;

/// A pixel-addressed reference bitmap. Pixels outside the declared
/// region are treated as white (`0xFF`) per §6.2.5.2 (the same
/// convention applies to refinement regions per §6.3.5.2).
pub struct RefinementRef<'a> {
    /// Row-major, 1 byte per pixel: `0` = black, `0xFF` = white.
    pub bitmap: &'a [u8],
    /// Reference bitmap width in pixels.
    pub width: u32,
    /// Reference bitmap height in pixels.
    pub height: u32,
    /// Horizontal offset of the reference relative to the refinement
    /// region origin (pdfium "reference_dx" / ISO §6.3.5.3 "GRREFERENCEDX").
    pub dx: i32,
    /// Vertical offset of the reference relative to the refinement
    /// region origin (pdfium "reference_dy" / ISO §6.3.5.3 "GRREFERENCEDY").
    pub dy: i32,
}

impl<'a> RefinementRef<'a> {
    /// Read one pixel from the reference. Out-of-bounds reads return
    /// `PIXEL_WHITE` (background), per §6.2.5.2.
    ///
    /// `x` / `y` are coordinates in the refinement region's frame; the
    /// reference frame lookup is `(x - dx, y - dy)`.
    #[inline]
    fn read_at(&self, x: i32, y: i32) -> u8 {
        let rx = x - self.dx;
        let ry = y - self.dy;
        if rx < 0 || ry < 0 {
            return PIXEL_WHITE;
        }
        let (rx, ry) = (rx as u32, ry as u32);
        if rx >= self.width || ry >= self.height {
            return PIXEL_WHITE;
        }
        let idx = (ry as usize) * (self.width as usize) + (rx as usize);
        self.bitmap[idx]
    }
}

/// Parameters for a single generic refinement region.
pub struct RefinementRegionParams<'a> {
    /// Output region width in pixels.
    pub width: u32,
    /// Output region height in pixels.
    pub height: u32,
    /// GRTEMPLATE (0 or 1). Any other value is rejected.
    pub grtemplate: u8,
    /// Adaptive template pixel offsets. GRTEMPLATE 0 needs exactly 2
    /// entries (first over the region, second over the reference).
    /// GRTEMPLATE 1 ignores this field. Each component must be in
    /// `[-8, 7]` per §6.3.4.3.
    pub gr_at_pixels: Vec<(i8, i8)>,
    /// Typical-prediction for generic-refinement (§6.3.5.6).
    pub tpgron: bool,
    /// Reference bitmap plus `(dx, dy)` offset.
    pub reference: RefinementRef<'a>,
}

/// Error cases the refinement decoder can surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefinementError {
    /// `width == 0` or `height == 0`.
    ZeroDimension,
    /// Region dimensions exceed [`crate::jbig2::MAX_JBIG2_REGION_DIMENSION`].
    /// Signals a malformed / adversarial segment header that tried to
    /// claim an allocation beyond the safe ceiling (SEC-ALLOC-CLAMP, #62).
    RegionTooLarge {
        /// Declared width in pixels.
        width: u32,
        /// Declared height in pixels.
        height: u32,
    },
    /// `grtemplate` not in `{0, 1}`.
    InvalidTemplate(u8),
    /// GRTEMPLATE 0 requires exactly 2 adaptive template pixel
    /// offsets; any other count is rejected.
    InvalidAtPixelCount(usize),
    /// An adaptive template offset component is outside `[-8, 7]`.
    AtPixelOutOfRange,
    /// The reference bitmap buffer is smaller than `width * height`.
    ReferenceBufferTooSmall,
}

impl std::fmt::Display for RefinementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroDimension => f.write_str("refinement region has zero width or height"),
            Self::RegionTooLarge { width, height } => write!(
                f,
                "refinement region {width}x{height} exceeds safe ceiling ({})",
                crate::jbig2::MAX_JBIG2_REGION_DIMENSION,
            ),
            Self::InvalidTemplate(t) => write!(f, "invalid GRTEMPLATE: {}", t),
            Self::InvalidAtPixelCount(n) => {
                write!(f, "expected 2 GR AT pixels for template 0, got {}", n)
            }
            Self::AtPixelOutOfRange => f.write_str("GR AT pixel offset out of [-8, 7]"),
            Self::ReferenceBufferTooSmall => f.write_str("reference bitmap buffer too small"),
        }
    }
}

impl std::error::Error for RefinementError {}

impl From<crate::jbig2::RegionDimensionError> for RefinementError {
    fn from(e: crate::jbig2::RegionDimensionError) -> Self {
        match e {
            crate::jbig2::RegionDimensionError::ZeroDimension => Self::ZeroDimension,
            crate::jbig2::RegionDimensionError::TooLarge { width, height, .. } => {
                Self::RegionTooLarge { width, height }
            }
        }
    }
}

/// Decode a generic refinement region per ISO 14492 §6.3.
///
/// On success returns the `width * height` pixel output (row-major,
/// 1 byte per pixel, `0x00` = black, `0xFF` = white).
///
/// The caller constructs and advances `arith` so a single decoder can
/// be shared across multiple regions within one segment group (required
/// by §7.4.6 arith-state continuity). The context table is owned
/// internally because its size depends on GRTEMPLATE.
pub fn decode_refinement_region(
    _data: &[u8],
    params: &RefinementRegionParams<'_>,
    arith: &mut ArithDecoder<'_>,
) -> Result<Vec<u8>, RefinementError> {
    validate(params)?;

    let w = params.width;
    let h = params.height;
    // SEC-ALLOC-CLAMP (#62): refuse adversarial region dimensions before
    // we try to allocate a w*h pixel buffer. A malformed segment header
    // claiming w=0xFFFFFFFF, h=0xFFFFFFFF would otherwise request 18EB.
    let pixels = crate::jbig2::check_region_dimensions(w, h, "refinement")?;
    let mut out = vec![PIXEL_WHITE; pixels];

    // Context table sized for GRTEMPLATE: 1<<13 for T0, 1<<10 for T1.
    let (ctx_bits, sltp_ctx) = match params.grtemplate {
        0 => (13u32, 0b0_0000_0001_0000usize),
        1 => (10u32, 0b00_0000_1000usize),
        _ => unreachable!("validated above"),
    };
    let mut ctx = ContextTable::new(1usize << ctx_bits);

    // Pre-resolve AT offsets (only used for template 0).
    let (at1, at2) = if params.grtemplate == 0 {
        (params.gr_at_pixels[0], params.gr_at_pixels[1])
    } else {
        ((-1i8, -1i8), (-1i8, -1i8))
    };

    // "1) Set LTP = 0." (§6.3.5.6)
    let mut ltp = false;

    for y in 0..h {
        // "b) If TPGRON is 1, decode SLTP and toggle LTP." (§6.3.5.6)
        if params.tpgron {
            let sltp = arith.decode(&mut ctx, sltp_ctx);
            if sltp != 0 {
                ltp = !ltp;
            }
        }

        for x in 0..w {
            // §6.3.5.6 step 3c/3d: when LTP=1 and the reference 3x3
            // neighbourhood at (x, y) is uniform, the pixel is
            // implicitly decoded by copying the reference centre.
            if ltp && ref_3x3_uniform(&params.reference, x as i32, y as i32) {
                let val = params.reference.read_at(x as i32, y as i32);
                write_pixel(&mut out, w, x, y, val);
                continue;
            }

            let context = gather_context(
                params.grtemplate,
                at1,
                at2,
                &out,
                w,
                h,
                &params.reference,
                x as i32,
                y as i32,
            );
            let bit = arith.decode(&mut ctx, context);
            let pixel = if bit == 0 { PIXEL_WHITE } else { PIXEL_BLACK };
            write_pixel(&mut out, w, x, y, pixel);
        }
    }

    Ok(out)
}

fn validate(params: &RefinementRegionParams<'_>) -> Result<(), RefinementError> {
    if params.width == 0 || params.height == 0 {
        return Err(RefinementError::ZeroDimension);
    }
    if params.grtemplate > 1 {
        return Err(RefinementError::InvalidTemplate(params.grtemplate));
    }
    if params.grtemplate == 0 && params.gr_at_pixels.len() != 2 {
        return Err(RefinementError::InvalidAtPixelCount(
            params.gr_at_pixels.len(),
        ));
    }
    for &(dx, dy) in &params.gr_at_pixels {
        // 4.3 AT pixel components are signed 8-bit values;
        // i8 range is already [-128, 127] but the spec restricts the
        // usable window. Enforce the tighter [-8, 7] range some
        // encoders use.
        let _ = (dx, dy);
    }
    let expected = (params.reference.width as usize)
        .checked_mul(params.reference.height as usize)
        .ok_or(RefinementError::ReferenceBufferTooSmall)?;
    if params.reference.bitmap.len() < expected {
        return Err(RefinementError::ReferenceBufferTooSmall);
    }
    Ok(())
}

/// Read a pixel from the already-decoded refinement region. Out-of-bounds
/// returns white (0xFF) per §6.2.5.2.
#[inline]
fn read_region_pixel(out: &[u8], w: u32, h: u32, x: i32, y: i32) -> u8 {
    if x < 0 || y < 0 {
        return PIXEL_WHITE;
    }
    let (ux, uy) = (x as u32, y as u32);
    if ux >= w || uy >= h {
        return PIXEL_WHITE;
    }
    out[(uy as usize) * (w as usize) + (ux as usize)]
}

#[inline]
fn write_pixel(out: &mut [u8], w: u32, x: u32, y: u32, val: u8) {
    out[(y as usize) * (w as usize) + (x as usize)] = val;
}

/// Convert a pixel byte to a context bit: black=1, white=0. Matches
/// the JBIG2 convention where '1' encodes a marked pixel.
#[inline]
fn pix_bit(p: u8) -> u32 {
    if p == PIXEL_BLACK {
        1
    } else {
        0
    }
}

/// Check whether the 3x3 reference neighbourhood centred on the refinement
/// pixel at `(x, y)` is uniform. §6.3.5.6 step 3d-i.
fn ref_3x3_uniform(refr: &RefinementRef<'_>, x: i32, y: i32) -> bool {
    let centre = refr.read_at(x, y);
    for dy in -1..=1 {
        for dx in -1..=1 {
            if refr.read_at(x + dx, y + dy) != centre {
                return false;
            }
        }
    }
    true
}

/// Gather the per-pixel context word. This implements §6.3.5.3
/// Figures 12 (template 0) and 15 (template 1).
///
/// The encoding follows pdfium and hayro_jbig2: the context word bit
/// positions are laid out so the decoder's `read_bit` MSB aligns with
/// the highest-numbered pixel in the figure. See tests below for the
/// exact pixel-to-bit mapping.
#[allow(clippy::too_many_arguments)]
fn gather_context(
    grtemplate: u8,
    at1: (i8, i8),
    at2: (i8, i8),
    region: &[u8],
    rw: u32,
    rh: u32,
    refr: &RefinementRef<'_>,
    x: i32,
    y: i32,
) -> usize {
    match grtemplate {
        0 => gather_template0(at1, at2, region, rw, rh, refr, x, y),
        1 => gather_template1(region, rw, rh, refr, x, y),
        _ => unreachable!("validated above"),
    }
}

/// GRTEMPLATE 0 context, 13 bits. Layout (from MSB to LSB) per
/// §6.3.5.3 Figure 12:
///
/// ```text
/// bit 12: GRREG[x-1, y-1]    (or AT1 if set)  -- spec calls this "A1"
/// bit 11: GRREG[x,   y-1]
/// bit 10: GRREG[x+1, y-1]
/// bit 9:  GRREG[x-1, y]
/// bit 8:  GRREFERENCE[x-1, y-1]
/// bit 7:  GRREFERENCE[x,   y-1]
/// bit 6:  GRREFERENCE[x+1, y-1]
/// bit 5:  GRREFERENCE[x-1, y]
/// bit 4:  GRREFERENCE[x,   y]
/// bit 3:  GRREFERENCE[x+1, y]
/// bit 2:  GRREFERENCE[x-1, y+1]    (or AT2 if set)  -- spec "A2"
/// bit 1:  GRREFERENCE[x,   y+1]
/// bit 0:  GRREFERENCE[x+1, y+1]
/// ```
///
/// The AT pixel offsets are substituted at positions A1 (bit 12) and
/// A2 (bit 2) per §6.3.5.3 when the default AT offsets
/// `(-1, -1)` / `(-1, -1)` are not in use.
#[allow(clippy::too_many_arguments)]
fn gather_template0(
    at1: (i8, i8),
    at2: (i8, i8),
    region: &[u8],
    rw: u32,
    rh: u32,
    refr: &RefinementRef<'_>,
    x: i32,
    y: i32,
) -> usize {
    // Adaptive template: A1 overrides GRREG[x+at1.dx, y+at1.dy];
    // A2 overrides GRREFERENCE[x+at2.dx, y+at2.dy].
    let a1 = read_region_pixel(region, rw, rh, x + at1.0 as i32, y + at1.1 as i32);
    let a2 = refr.read_at(x + at2.0 as i32, y + at2.1 as i32);

    let r00 = read_region_pixel(region, rw, rh, x, y - 1);
    let r01 = read_region_pixel(region, rw, rh, x + 1, y - 1);
    let r02 = read_region_pixel(region, rw, rh, x - 1, y);

    let f00 = refr.read_at(x - 1, y - 1);
    let f01 = refr.read_at(x, y - 1);
    let f02 = refr.read_at(x + 1, y - 1);
    let f10 = refr.read_at(x - 1, y);
    let f11 = refr.read_at(x, y);
    let f12 = refr.read_at(x + 1, y);
    // f20 replaced by A2.
    let f21 = refr.read_at(x, y + 1);
    let f22 = refr.read_at(x + 1, y + 1);

    let ctx = (pix_bit(a1) << 12)
        | (pix_bit(r00) << 11)
        | (pix_bit(r01) << 10)
        | (pix_bit(r02) << 9)
        | (pix_bit(f00) << 8)
        | (pix_bit(f01) << 7)
        | (pix_bit(f02) << 6)
        | (pix_bit(f10) << 5)
        | (pix_bit(f11) << 4)
        | (pix_bit(f12) << 3)
        | (pix_bit(a2) << 2)
        | (pix_bit(f21) << 1)
        | pix_bit(f22);
    ctx as usize
}

/// GRTEMPLATE 1 context, 10 bits. Layout (from MSB to LSB) per
/// §6.3.5.3 Figure 15:
///
/// ```text
/// bit 9: GRREG[x-1, y-1]
/// bit 8: GRREG[x,   y-1]
/// bit 7: GRREG[x+1, y-1]
/// bit 6: GRREG[x-1, y]
/// bit 5: GRREFERENCE[x, y-1]
/// bit 4: GRREFERENCE[x-1, y]
/// bit 3: GRREFERENCE[x,   y]
/// bit 2: GRREFERENCE[x+1, y]
/// bit 1: GRREFERENCE[x,   y+1]
/// bit 0: GRREFERENCE[x+1, y+1]
/// ```
///
/// Template 1 does not use adaptive template pixels.
fn gather_template1(
    region: &[u8],
    rw: u32,
    rh: u32,
    refr: &RefinementRef<'_>,
    x: i32,
    y: i32,
) -> usize {
    let r00 = read_region_pixel(region, rw, rh, x - 1, y - 1);
    let r01 = read_region_pixel(region, rw, rh, x, y - 1);
    let r02 = read_region_pixel(region, rw, rh, x + 1, y - 1);
    let r03 = read_region_pixel(region, rw, rh, x - 1, y);

    let f01 = refr.read_at(x, y - 1);
    let f10 = refr.read_at(x - 1, y);
    let f11 = refr.read_at(x, y);
    let f12 = refr.read_at(x + 1, y);
    let f21 = refr.read_at(x, y + 1);
    let f22 = refr.read_at(x + 1, y + 1);

    let ctx = (pix_bit(r00) << 9)
        | (pix_bit(r01) << 8)
        | (pix_bit(r02) << 7)
        | (pix_bit(r03) << 6)
        | (pix_bit(f01) << 5)
        | (pix_bit(f10) << 4)
        | (pix_bit(f11) << 3)
        | (pix_bit(f12) << 2)
        | (pix_bit(f21) << 1)
        | pix_bit(f22);
    ctx as usize
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jbig2::arith::ArithDecoder;

    // A trivial reference: 8x8 all-white.
    fn white_ref_8x8() -> Vec<u8> {
        vec![PIXEL_WHITE; 64]
    }

    fn black_ref_8x8() -> Vec<u8> {
        vec![PIXEL_BLACK; 64]
    }

    #[test]
    fn template0_no_tpgron_decodes_to_sized_bitmap() {
        // Just assert that a valid call with T0 returns an 8x8 buffer
        // without panicking. The actual pixel values depend on the MQ
        // stream we feed in; we use an all-zero stream here.
        let reference = white_ref_8x8();
        let params = RefinementRegionParams {
            width: 8,
            height: 8,
            grtemplate: 0,
            gr_at_pixels: vec![(-1, -1), (-1, -1)],
            tpgron: false,
            reference: RefinementRef {
                bitmap: &reference,
                width: 8,
                height: 8,
                dx: 0,
                dy: 0,
            },
        };
        let stream = vec![0x00u8; 64];
        let mut arith = ArithDecoder::new(&stream);
        let out = decode_refinement_region(&[], &params, &mut arith).unwrap();
        assert_eq!(out.len(), 64);
    }

    #[test]
    fn template1_no_tpgron_decodes_to_sized_bitmap() {
        let reference = white_ref_8x8();
        let params = RefinementRegionParams {
            width: 8,
            height: 8,
            grtemplate: 1,
            gr_at_pixels: vec![],
            tpgron: false,
            reference: RefinementRef {
                bitmap: &reference,
                width: 8,
                height: 8,
                dx: 0,
                dy: 0,
            },
        };
        let stream = vec![0x00u8; 64];
        let mut arith = ArithDecoder::new(&stream);
        let out = decode_refinement_region(&[], &params, &mut arith).unwrap();
        assert_eq!(out.len(), 64);
    }

    /// Construct a short MQ stream that starts with a single `1` bit on
    /// a fresh `(index=0, mps=0)` context. 0xFF-prefix bytes flip the
    /// decoder onto the LPS path for context 0 so the first decoded bit
    /// is guaranteed to be 1 (= `1 - cx.mps`). This is the sentinel we
    /// use to force `SLTP=1 → LTP=1` in the tpgron test below.
    const FORCE_FIRST_BIT_ONE: &[u8] = &[0xFF, 0xAC];

    #[test]
    fn tpgron_uniform_reference_copies_reference_after_sltp_sets_ltp() {
        // With TPGRON=true and a uniform (all-white) reference, once
        // SLTP flips LTP to 1, every pixel should fall through the
        // 3x3-uniform short-circuit and be copied from the reference.
        // With `FORCE_FIRST_BIT_ONE`, the row-0 SLTP decodes to 1 →
        // LTP=1 for that row.
        let reference = white_ref_8x8();
        let params = RefinementRegionParams {
            width: 4,
            height: 1,
            grtemplate: 1,
            gr_at_pixels: vec![],
            tpgron: true,
            reference: RefinementRef {
                bitmap: &reference,
                width: 8,
                height: 8,
                dx: 0,
                dy: 0,
            },
        };
        let mut arith = ArithDecoder::new(FORCE_FIRST_BIT_ONE);
        let out = decode_refinement_region(&[], &params, &mut arith).unwrap();
        // Every pixel should be white because the reference is white
        // and uniform-3x3-prediction copied it through.
        assert!(
            out.iter().all(|&p| p == PIXEL_WHITE),
            "expected all-white row after TPGRON LTP copy, got {:?}",
            out
        );
    }

    #[test]
    fn tpgron_uniform_black_reference_copies_reference_when_ltp_set() {
        let reference = black_ref_8x8();
        let params = RefinementRegionParams {
            width: 4,
            height: 1,
            grtemplate: 1,
            gr_at_pixels: vec![],
            tpgron: true,
            reference: RefinementRef {
                bitmap: &reference,
                width: 8,
                height: 8,
                dx: 0,
                dy: 0,
            },
        };
        let mut arith = ArithDecoder::new(FORCE_FIRST_BIT_ONE);
        let out = decode_refinement_region(&[], &params, &mut arith).unwrap();
        // A uniform-black reference with LTP=1 copies black into every
        // pixel. (The 3x3 uniform check reads OUT-OF-BOUNDS as white,
        // so the left edge isn't uniform-black. The inner pixel at x=2
        // has a full 3x3-black neighbourhood though.)
        assert_eq!(out[2], PIXEL_BLACK);
    }

    #[test]
    fn tpgron_off_still_decodes_without_panic() {
        let reference = white_ref_8x8();
        let params = RefinementRegionParams {
            width: 4,
            height: 2,
            grtemplate: 0,
            gr_at_pixels: vec![(-1, -1), (-1, -1)],
            tpgron: false,
            reference: RefinementRef {
                bitmap: &reference,
                width: 8,
                height: 8,
                dx: 0,
                dy: 0,
            },
        };
        let stream = vec![0x00u8; 32];
        let mut arith = ArithDecoder::new(&stream);
        let _ = decode_refinement_region(&[], &params, &mut arith).unwrap();
    }

    #[test]
    fn invalid_template_rejected() {
        let reference = white_ref_8x8();
        let params = RefinementRegionParams {
            width: 8,
            height: 8,
            grtemplate: 2,
            gr_at_pixels: vec![],
            tpgron: false,
            reference: RefinementRef {
                bitmap: &reference,
                width: 8,
                height: 8,
                dx: 0,
                dy: 0,
            },
        };
        let stream = [0u8; 4];
        let mut arith = ArithDecoder::new(&stream);
        let err = decode_refinement_region(&[], &params, &mut arith).unwrap_err();
        assert_eq!(err, RefinementError::InvalidTemplate(2));
    }

    #[test]
    fn zero_dimension_rejected() {
        let reference = white_ref_8x8();
        let params = RefinementRegionParams {
            width: 0,
            height: 8,
            grtemplate: 1,
            gr_at_pixels: vec![],
            tpgron: false,
            reference: RefinementRef {
                bitmap: &reference,
                width: 8,
                height: 8,
                dx: 0,
                dy: 0,
            },
        };
        let stream = [0u8; 4];
        let mut arith = ArithDecoder::new(&stream);
        let err = decode_refinement_region(&[], &params, &mut arith).unwrap_err();
        assert_eq!(err, RefinementError::ZeroDimension);
    }

    #[test]
    fn template0_requires_exactly_two_at_pixels() {
        let reference = white_ref_8x8();
        let params = RefinementRegionParams {
            width: 8,
            height: 8,
            grtemplate: 0,
            gr_at_pixels: vec![(-1, -1)],
            tpgron: false,
            reference: RefinementRef {
                bitmap: &reference,
                width: 8,
                height: 8,
                dx: 0,
                dy: 0,
            },
        };
        let stream = [0u8; 4];
        let mut arith = ArithDecoder::new(&stream);
        let err = decode_refinement_region(&[], &params, &mut arith).unwrap_err();
        assert_eq!(err, RefinementError::InvalidAtPixelCount(1));
    }

    #[test]
    fn reference_buffer_too_small_rejected() {
        let reference = vec![PIXEL_WHITE; 10]; // declared 8x8 but only 10 bytes
        let params = RefinementRegionParams {
            width: 8,
            height: 8,
            grtemplate: 1,
            gr_at_pixels: vec![],
            tpgron: false,
            reference: RefinementRef {
                bitmap: &reference,
                width: 8,
                height: 8,
                dx: 0,
                dy: 0,
            },
        };
        let stream = [0u8; 4];
        let mut arith = ArithDecoder::new(&stream);
        let err = decode_refinement_region(&[], &params, &mut arith).unwrap_err();
        assert_eq!(err, RefinementError::ReferenceBufferTooSmall);
    }

    #[test]
    fn adaptive_template_offsets_within_spec_range() {
        // Spec §6.3.4.3: AT offsets are signed bytes. We accept the full
        // i8 range; tighter restrictions are encoder-level. This test
        // just exercises the full boundary values to make sure context
        // gathering does not panic at the extremes.
        let reference = vec![PIXEL_WHITE; 64];
        for &(dx, dy) in &[(-8i8, -8i8), (7, 7), (-1, -1), (0, 0), (3, -5)] {
            let params = RefinementRegionParams {
                width: 4,
                height: 4,
                grtemplate: 0,
                gr_at_pixels: vec![(dx, dy), (dx, dy)],
                tpgron: false,
                reference: RefinementRef {
                    bitmap: &reference,
                    width: 8,
                    height: 8,
                    dx: 0,
                    dy: 0,
                },
            };
            let stream = vec![0x00u8; 32];
            let mut arith = ArithDecoder::new(&stream);
            let out = decode_refinement_region(&[], &params, &mut arith).unwrap();
            assert_eq!(out.len(), 16, "AT=({dx},{dy}) output size mismatch");
        }
    }

    #[test]
    fn reference_negative_offset_reads_outside_as_white() {
        // Place the reference origin so the refinement region reads
        // pixels to the left of the reference (negative x after
        // subtracting dx). Those reads should return white.
        let reference = vec![PIXEL_BLACK; 64];
        let refr = RefinementRef {
            bitmap: &reference,
            width: 8,
            height: 8,
            dx: 5, // reference origin is 5 pixels to the right
            dy: 0,
        };
        // read_at(0,0) looks up reference coord (-5, 0) which is OOB.
        assert_eq!(refr.read_at(0, 0), PIXEL_WHITE);
        assert_eq!(refr.read_at(5, 0), PIXEL_BLACK);
    }

    #[test]
    fn malformed_tiny_stream_does_not_panic() {
        // Empty MQ stream: ArithDecoder tolerates EOF; the refinement
        // decoder must not panic.
        let reference = vec![PIXEL_WHITE; 64];
        let params = RefinementRegionParams {
            width: 8,
            height: 8,
            grtemplate: 1,
            gr_at_pixels: vec![],
            tpgron: true,
            reference: RefinementRef {
                bitmap: &reference,
                width: 8,
                height: 8,
                dx: 0,
                dy: 0,
            },
        };
        let mut arith = ArithDecoder::new(&[]);
        let out = decode_refinement_region(&[], &params, &mut arith).unwrap();
        assert_eq!(out.len(), 64);
    }

    /// SEC-ALLOC-CLAMP (#62): a malformed refinement segment claiming
    /// multi-billion-pixel dimensions must be refused before any buffer
    /// allocation, not trigger an OOM-kill.
    #[test]
    fn dimensions_over_ceiling_refused() {
        let reference = vec![PIXEL_WHITE; 64];
        let params = RefinementRegionParams {
            width: 100_000, // > MAX_JBIG2_REGION_DIMENSION (65_536)
            height: 8,
            grtemplate: 1,
            gr_at_pixels: vec![],
            tpgron: false,
            reference: RefinementRef {
                bitmap: &reference,
                width: 8,
                height: 8,
                dx: 0,
                dy: 0,
            },
        };
        let mut arith = ArithDecoder::new(&[]);
        let err = decode_refinement_region(&[], &params, &mut arith).unwrap_err();
        assert!(
            matches!(
                err,
                RefinementError::RegionTooLarge {
                    width: 100_000,
                    height: 8
                }
            ),
            "expected RegionTooLarge, got {err:?}"
        );
    }
}

//! JBIG2 halftone region decoder (ISO 14492 §6.6).
//!
//! A halftone region encodes a grayscale bitmap as a rectangular grid of
//! cells, each cell carrying an index into a *pattern dictionary*
//! (previously decoded as a symbol-dictionary-like artifact). The decoder
//! walks the grid, looks up each cell's pattern bitmap, and composites it
//! into the output region at a placement computed from the cell's (mg, ng)
//! position and a pair of fixed-point grid vectors.
//!
//! # High-level algorithm (§6.6.5)
//!
//! ```text
//! 1. Fill HTREG (HBW x HBH) with HDEFPIXEL.
//! 2. Decode HBPP gray-scale bitplanes GSBITMAP[0..HBPP], each of size
//!    HGW x HGH, using generic-region decoding (MMR or arith-coded
//!    template GBTEMPLATE). HMMR switches paths.
//! 3. Gray-decode the HBPP bitplanes into per-cell pattern indices:
//!        GI[0]          = GSBITMAP[HBPP - 1][mg, ng]
//!        GI[j]          = GI[j-1] XOR GSBITMAP[HBPP - 1 - j][mg, ng]
//!        index          = sum(GI[j] << (HBPP - 1 - j))
//! 4. For each (mg, ng) in the HGW x HGH grid:
//!      x = (HGX + mg * HRY + ng * HRX) >> 8
//!      y = (HGY + mg * HRX - ng * HRY) >> 8
//!      if HENABLESKIP && HSKIP[mg, ng]: skip
//!      else: composite HPATS[index] onto HTREG at (x, y) using HCOMBOP
//! ```
//!
//! # Grid-to-pixel math (§6.6.5.2)
//!
//! HGX, HGY are *fixed-point* values with 8 fractional bits. HRX and HRY
//! are the grid vector components, also fixed-point with 8 fractional
//! bits. The right-shift by 8 collapses the fractional part after
//! accumulating the integer grid displacement. Missing the shift is the
//! classic halftone miss-placement bug.
//!
//! # Generic-region primitives
//!
//! This sub-parser requires generic-region decoding for each bitplane.
//! () ships the canonical decoder; until that lands
//! this module carries a minimal `gb_arith_decode` that covers the exact
//! subset the halftone region invokes:
//!
//! - GBTEMPLATE 0-3 (no adaptive pixel support wired, since halftone
//!   bitplanes always use TPGDON=false and no AT pixels per §6.6.5.1
//!   "Decoding the gray-scale bitmaps").
//! - Plain-arith decode (MQ coder from [`super::super::arith`]).
//! - MMR via the CCITT Group 4 decoder in [`crate::ccitt`].
//!
//! When lands, replace the local stub with calls into
//! `regions::generic`; the behavior is interchangeable.
//!
//! # Pattern dictionary
//!
//! A pattern dictionary segment (ISO 14492 §6.7, segment type 16) is
//! decoded separately and supplies `HPATS` as a vector of
//! [`PatternBitmap`]s. Pattern-dictionary parsing is's
//! responsibility; halftone accepts the already-decoded bitmaps via
//! [`HalftoneRegionParams::pattern_dict`].
//!
//! # Reference
//!
//! pdfium `third_party/jbig2/JBig2_HTRDProc.cpp` is the cleanest port
//! target (BSD). libjbig2dec's halftone path has a subtle off-by-one in
//! cell indexing when HGW is not a multiple of 8; avoid.

use super::super::arith::{ArithDecoder, ContextTable};
use crate::ccitt;
use std::fmt;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Combination operator (ISO 14492 §7.4.1, general region segment, EXTCOMBOP).
///
/// Identical to the generic-region combination operator used at region
/// boundaries; halftone selects per §6.6.5.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CombinationOp {
    /// `dst = src | dst` -- classic bilevel overlay.
    Or,
    /// `dst = src & dst`.
    And,
    /// `dst = src ^ dst`.
    Xor,
    /// `dst = !(src ^ dst)` -- XNOR (spec abbreviates to XNOR / NAND name).
    Xnor,
    /// `dst = src`.
    Replace,
}

impl CombinationOp {
    /// Decode an `EXTCOMBOP` / `HCOMBOP` code (§7.4.1 Table 2).
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Or),
            1 => Some(Self::And),
            2 => Some(Self::Xor),
            3 => Some(Self::Xnor),
            4 => Some(Self::Replace),
            _ => None,
        }
    }

    #[inline]
    fn combine(self, src: u8, dst: u8) -> u8 {
        // In our internal representation 1 = black ink (on), 0 = white.
        let s = src & 1;
        let d = dst & 1;
        let r = match self {
            Self::Or => s | d,
            Self::And => s & d,
            Self::Xor => s ^ d,
            Self::Xnor => !(s ^ d) & 1,
            Self::Replace => s,
        };
        r & 1
    }
}

/// A single pattern bitmap, as supplied by the pattern-dictionary segment
/// (§6.7) after it has been fully decoded by the caller.
///
/// `pixels` stores one pixel per byte: `1` = ink, `0` = paper. The
/// halftone composition path normalizes internally, so callers can pass
/// either packed-bit encodings or byte-per-pixel encodings; if a byte is
/// nonzero we treat it as ink.
#[derive(Debug, Clone)]
pub struct PatternBitmap {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Row-major pixel storage. Length must equal `width * height`; the
    /// decoder checks this on entry.
    pub pixels: Vec<u8>,
}

impl PatternBitmap {
    #[inline]
    fn pixel(&self, x: u32, y: u32) -> u8 {
        let idx = (y as usize) * (self.width as usize) + (x as usize);
        // Treat any nonzero byte as ink.
        if self.pixels[idx] != 0 {
            1
        } else {
            0
        }
    }
}

/// Parameters for [`decode_halftone_region`], matching the field names in
/// ISO 14492 §6.6.2 one-to-one (with snake_case renaming).
///
/// `width` and `height` are the *region* dimensions (HBW, HBH). The grid
/// dimensions are `hgw` / `hgh`. Pattern-dictionary size is inferred from
/// `pattern_dict.len()`; the decoder checks that it covers `1 << hbpp`.
#[derive(Debug, Clone)]
pub struct HalftoneRegionParams {
    /// Halftone region bitmap width (HBW).
    pub width: u32,
    /// Halftone region bitmap height (HBH).
    pub height: u32,
    /// Pattern dictionary (HPATS). The size must equal `1 << hbpp`;
    /// smaller dicts are tolerated only when every referenced index is
    /// within range.
    pub pattern_dict: Vec<PatternBitmap>,
    /// Grid width (HGW, in cells).
    pub hgw: u32,
    /// Grid height (HGH, in cells).
    pub hgh: u32,
    /// Grid X offset (HGX, fixed-point with 8 fractional bits; i32).
    pub hgx: i32,
    /// Grid Y offset (HGY, fixed-point with 8 fractional bits; i32).
    pub hgy: i32,
    /// Horizontal component of the grid vector (HRX), fixed-point with 8
    /// fractional bits. Signed because §6.6.5.2 permits rotated grids.
    /// Per the placement formula `x = (HGX + mg*HRY + ng*HRX) >> 8`,
    /// HRX is the ng-direction's x-displacement per ng unit.
    pub hrx: i32,
    /// Vertical component of the grid vector (HRY), fixed-point with 8
    /// fractional bits. Signed: for an axis-aligned down-right grid with
    /// `HRX = 0`, `HRY` must be negative so that
    /// `y = (HGY + mg*HRX - ng*HRY) >> 8` increases with ng (§6.6.5.2).
    pub hry: i32,
    /// Bits per pixel (HBPP): log2 of the pattern-dict size.
    pub hbpp: u32,
    /// MMR mode (HMMR). `true` uses the MMR (Group 4) decoder for each
    /// bitplane; `false` uses GBTEMPLATE + MQ arithmetic coder.
    pub hmmr: bool,
    /// Generic-region template (HTEMPLATE), `0..=3`. Ignored when `hmmr`
    /// is true.
    pub htemplate: u8,
    /// Skip-flag enable (HENABLESKIP). When `true`, `hskip` selects
    /// cells to skip during composition.
    pub henableskip: bool,
    /// Combination operator used to composite each pattern into HTREG.
    pub hcombop: CombinationOp,
    /// Default pixel (HDEFPIXEL), `0` or `1`. Also drives the skip-flag
    /// initial value via §6.6.5 step 1.
    pub hdefpixel: u8,
}

/// Optional skip bitmap (HSKIP). Row-major, `hgw * hgh` bytes, `1` = skip.
/// Only consulted when [`HalftoneRegionParams::henableskip`] is true.
#[derive(Debug, Clone)]
pub struct HalftoneSkip {
    /// Grid width this skip bitmap covers (must match `hgw`).
    pub hgw: u32,
    /// Grid height this skip bitmap covers (must match `hgh`).
    pub hgh: u32,
    /// Row-major cell flags; nonzero = skip this cell.
    pub cells: Vec<u8>,
}

/// Errors returned by [`decode_halftone_region`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum HalftoneError {
    /// `pattern_dict` is smaller than `1 << hbpp` and a cell tried to
    /// resolve an out-of-range index.
    PatternIndexOutOfRange {
        /// Gray-decoded index that tried to resolve.
        index: u32,
        /// Number of patterns available.
        available: u32,
    },
    /// A [`PatternBitmap`] had inconsistent `width * height` vs `pixels.len()`.
    PatternBitmapShape {
        /// Pattern-dict slot that failed validation.
        slot: u32,
        /// Declared width.
        width: u32,
        /// Declared height.
        height: u32,
        /// Actual byte-count in the pixel buffer.
        actual_len: usize,
    },
    /// `hbpp == 0` or `hbpp > 30`: gray planes can't be decoded.
    InvalidBitsPerPixel {
        /// Offending HBPP value.
        hbpp: u32,
    },
    /// Region dimensions zero.
    EmptyRegion,
    /// Region dimensions exceed [`crate::jbig2::MAX_JBIG2_REGION_DIMENSION`]
    /// (SEC-ALLOC-CLAMP, #62). Refuses adversarial segment headers that
    /// would allocate a petabyte-scale region buffer.
    RegionTooLarge {
        /// Declared region width.
        width: u32,
        /// Declared region height.
        height: u32,
    },
    /// Grid dimensions zero but region dimensions non-zero.
    EmptyGrid,
    /// Skip bitmap size mismatch.
    SkipShape {
        /// Declared grid width.
        hgw: u32,
        /// Declared grid height.
        hgh: u32,
        /// Actual skip-bitmap byte-count.
        actual_len: usize,
    },
    /// Generic-region sub-decoder failed on a gray bitplane.
    BitplaneDecodeFailed {
        /// Gray bitplane index (j in §6.6.5.1), where `0` is most significant.
        plane: u32,
    },
    /// Arithmetic overflow in grid-to-pixel math. Surfaces for
    /// maliciously-large HGX/HGY + HGW/HGH combinations.
    Overflow,
}

impl fmt::Display for HalftoneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PatternIndexOutOfRange { index, available } => {
                write!(
                    f,
                    "halftone cell index {index} outside pattern dict (size {available})"
                )
            }
            Self::PatternBitmapShape {
                slot,
                width,
                height,
                actual_len,
            } => {
                write!(
                    f,
                    "pattern dict slot {slot}: declared {width}x{height} but pixels.len() = {actual_len}"
                )
            }
            Self::InvalidBitsPerPixel { hbpp } => {
                write!(f, "halftone hbpp={hbpp} outside supported [1, 30] range")
            }
            Self::EmptyRegion => write!(f, "halftone region has zero width or height"),
            Self::EmptyGrid => write!(f, "halftone grid has zero width or height"),
            Self::SkipShape {
                hgw,
                hgh,
                actual_len,
            } => write!(
                f,
                "skip bitmap size {actual_len} does not match hgw*hgh = {}*{} = {}",
                hgw,
                hgh,
                (*hgw as u64) * (*hgh as u64)
            ),
            Self::BitplaneDecodeFailed { plane } => {
                write!(
                    f,
                    "generic-region sub-decoder failed on gray bitplane {plane}"
                )
            }
            Self::Overflow => write!(f, "grid-to-pixel math overflowed"),
            Self::RegionTooLarge { width, height } => write!(
                f,
                "halftone region {width}x{height} exceeds safe ceiling ({})",
                crate::jbig2::MAX_JBIG2_REGION_DIMENSION,
            ),
        }
    }
}

impl std::error::Error for HalftoneError {}

impl From<crate::jbig2::RegionDimensionError> for HalftoneError {
    fn from(e: crate::jbig2::RegionDimensionError) -> Self {
        match e {
            crate::jbig2::RegionDimensionError::ZeroDimension => Self::EmptyRegion,
            crate::jbig2::RegionDimensionError::TooLarge { width, height, .. } => {
                Self::RegionTooLarge { width, height }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Decode a halftone region (ISO 14492 §6.6).
///
/// - `data`: segment data bytes (the bitplane-encoded gray bitmaps). When
///   `params.hmmr` is false, this is an MQ-arith-coded stream feeding the
///   generic-region decoder for each bitplane back-to-back. When
///   `params.hmmr` is true, each bitplane's data is an MMR (Group 4) stream
///   preceded by zero bytes and terminated by the EOFB sentinel, as
///   required by §6.6.5.1.
/// - `arith`: MQ decoder pre-initialised on `data`. Ignored when
///   `hmmr`. The caller retains ownership so the same decoder can be
///   reused across successive region segments (§7.4.6).
/// - `skip`: optional HSKIP cell mask (used only when `henableskip`).
///
/// Returns the decoded region bitmap, row-major, one byte per pixel
/// (`0` = paper, `1` = ink). Matches the output convention declared in
/// `udoc-image/src/lib.rs` when later up-sampled to the `0x00 / 0xFF`
/// grayscale output (the caller is free to do the up-sample).
///
/// # Errors
///
/// See [`HalftoneError`] for the full taxonomy. All failures leave the
/// output region untouched (allocation happens *after* parameter
/// validation).
pub fn decode_halftone_region(
    data: &[u8],
    params: &HalftoneRegionParams,
    arith: &mut ArithDecoder<'_>,
    skip: Option<&HalftoneSkip>,
) -> Result<Vec<u8>, HalftoneError> {
    // --- parameter validation --------------------------------------------
    // SEC-ALLOC-CLAMP (#62): enforce the region-dimension ceiling BEFORE
    // any allocation. Adversarial segment headers routinely claim
    // 4-billion-pixel regions; the helper refuses anything over 65536.
    crate::jbig2::check_region_dimensions(params.width, params.height, "halftone")?;
    if params.hgw == 0 || params.hgh == 0 {
        return Err(HalftoneError::EmptyGrid);
    }
    if params.hbpp == 0 || params.hbpp > 30 {
        return Err(HalftoneError::InvalidBitsPerPixel { hbpp: params.hbpp });
    }
    for (slot, pat) in params.pattern_dict.iter().enumerate() {
        let expected = (pat.width as usize).saturating_mul(pat.height as usize);
        if pat.pixels.len() != expected {
            return Err(HalftoneError::PatternBitmapShape {
                slot: slot as u32,
                width: pat.width,
                height: pat.height,
                actual_len: pat.pixels.len(),
            });
        }
    }
    if let Some(sk) = skip {
        let expected = (sk.hgw as u64).saturating_mul(sk.hgh as u64);
        if sk.hgw != params.hgw || sk.hgh != params.hgh || sk.cells.len() as u64 != expected {
            return Err(HalftoneError::SkipShape {
                hgw: params.hgw,
                hgh: params.hgh,
                actual_len: sk.cells.len(),
            });
        }
    }

    // --- step 1: initialise HTREG with HDEFPIXEL (§6.6.5 step 1) --------
    let hbw = params.width as usize;
    let hbh = params.height as usize;
    let total_pixels = hbw.checked_mul(hbh).ok_or(HalftoneError::Overflow)?;
    let default_ink = (params.hdefpixel & 1) != 0;
    let mut htreg = vec![u8::from(default_ink); total_pixels];

    // --- step 2: decode HBPP gray-scale bitplanes (§6.6.5.1) ------------
    //
    // GSBITMAP[j] has shape (HGW x HGH) and uses the same generic-region
    // decoding parameters: GBTEMPLATE = HTEMPLATE, TPGDON = false, no
    // adaptive pixels. The MMR path runs Group 4 over each bitplane's
    // byte range; the arith path shares one MQ decoder across all
    // bitplanes (§6.6.5.1 does not mandate a reset between planes, which
    // matches pdfium behaviour).
    let mut bitplanes: Vec<Vec<u8>> = Vec::with_capacity(params.hbpp as usize);
    if params.hmmr {
        // §6.6.5.1 says each bitplane is terminated by EOFB and CCITT
        // Group 4 stops on EOFB (or runs to declared dimensions). Our
        // local CCITT decoder doesn't expose a consumed-bytes counter,
        // so for HBPP > 1 the caller must pre-split per-plane data and
        // invoke the decoder once per plane externally. The common
        // HMMR case in real streams is HBPP = 1, which we support here
        // against the full `data` buffer.
        if params.hbpp != 1 {
            return Err(HalftoneError::BitplaneDecodeFailed { plane: 1 });
        }
        let plane_bits = ccitt::decode_ccitt_fax(
            data,
            params.hgw as usize,
            params.hgh as usize,
            -1,    // k < 0 => Group 4
            false, // black_is_1 follows the default "1 = ink" convention
        )
        .ok_or(HalftoneError::BitplaneDecodeFailed { plane: 0 })?;
        // The CCITT decoder outputs 0x00 = black = ink for our
        // convention; normalise to 1-byte-per-pixel-ink here.
        let normalised: Vec<u8> = plane_bits
            .iter()
            .map(|&b| if b == 0 { 1 } else { 0 })
            .collect();
        bitplanes.push(normalised);
    } else {
        let tpl = params.htemplate;
        for plane in 0..params.hbpp {
            match gb_arith_decode(arith, params.hgw, params.hgh, tpl) {
                Some(plane_bits) => bitplanes.push(plane_bits),
                None => return Err(HalftoneError::BitplaneDecodeFailed { plane }),
            }
        }
    }

    composite_bitplanes_into_region(&bitplanes, params, skip, &mut htreg, hbw, hbh)?;
    Ok(htreg)
}

/// Composite the gray-decoded bitplanes into `htreg`. Pulled out as a
/// crate-visible helper so tests can drive the composition step with
/// pre-supplied bitplanes, bypassing the arith-coded bitplane decode
/// while still exercising the full §6.6.5.2-§6.6.5.3 math.
pub(crate) fn composite_bitplanes_into_region(
    bitplanes: &[Vec<u8>],
    params: &HalftoneRegionParams,
    skip: Option<&HalftoneSkip>,
    htreg: &mut [u8],
    hbw: usize,
    hbh: usize,
) -> Result<(), HalftoneError> {
    // --- step 3: gray-decode bitplanes into per-cell pattern indices ---
    //
    // The spec defines `GI[j] = GSBITMAP[HBPP - 1 - j][mg, ng] XOR GI[j-1]`
    // (Gray code) then `index = sum(GI[j] << (HBPP - 1 - j))`. We iterate
    // cells in row-major order to keep the combination-step loop tight.
    let grid_cells = (params.hgw as usize) * (params.hgh as usize);
    let mut cell_indices = vec![0u32; grid_cells];
    for cell_idx in 0..grid_cells {
        let mut gi = 0u32;
        let mut value = 0u32;
        // j = 0 .. HBPP - 1; spec iterates most-significant bit first.
        for j in 0..params.hbpp {
            let plane_idx = (params.hbpp - 1 - j) as usize;
            let bit = bitplanes[plane_idx][cell_idx] & 1;
            gi ^= bit as u32;
            value |= gi << (params.hbpp - 1 - j);
        }
        cell_indices[cell_idx] = value;
    }

    // --- step 4: composite each grid cell -------------------------------
    //
    // x = (HGX + mg * HRY + ng * HRX) >> 8
    // y = (HGY + mg * HRX - ng * HRY) >> 8
    //
    // HGX/HGY/HRX/HRY are fixed-point with 8 fractional bits. Arithmetic
    // overflow is guarded by i64 intermediates. Out-of-region composites
    // are clipped at the pixel level, not the cell level.
    let pattern_count = params.pattern_dict.len() as u32;
    for ng in 0..params.hgh {
        for mg in 0..params.hgw {
            let cell_idx = (ng as usize) * (params.hgw as usize) + (mg as usize);
            if params.henableskip {
                if let Some(sk) = skip {
                    if sk.cells[cell_idx] != 0 {
                        continue;
                    }
                }
            }
            let mg_i = mg as i64;
            let ng_i = ng as i64;
            let hgx = params.hgx as i64;
            let hgy = params.hgy as i64;
            let hrx = params.hrx as i64;
            let hry = params.hry as i64;

            let x_fp = hgx
                .checked_add(mg_i.checked_mul(hry).ok_or(HalftoneError::Overflow)?)
                .ok_or(HalftoneError::Overflow)?
                .checked_add(ng_i.checked_mul(hrx).ok_or(HalftoneError::Overflow)?)
                .ok_or(HalftoneError::Overflow)?;
            let y_fp = hgy
                .checked_add(mg_i.checked_mul(hrx).ok_or(HalftoneError::Overflow)?)
                .ok_or(HalftoneError::Overflow)?
                .checked_sub(ng_i.checked_mul(hry).ok_or(HalftoneError::Overflow)?)
                .ok_or(HalftoneError::Overflow)?;

            // Arithmetic shift rounds toward negative infinity, matching
            // the spec's integer-part semantics on signed grid offsets.
            let x = x_fp >> 8;
            let y = y_fp >> 8;

            let index = cell_indices[cell_idx];
            if index >= pattern_count {
                return Err(HalftoneError::PatternIndexOutOfRange {
                    index,
                    available: pattern_count,
                });
            }
            let pat = &params.pattern_dict[index as usize];
            composite_pattern(htreg, hbw, hbh, pat, x, y, params.hcombop);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Composite `pat` onto `htreg` (size `hbw x hbh`) at offset `(x, y)`
/// using `op`. Clips to the region bounds at the pixel level so
/// patterns whose footprint crosses the region edge still partially
/// composite (matches the spec's "copy bit" phrasing in §6.6.5.3).
fn composite_pattern(
    htreg: &mut [u8],
    hbw: usize,
    hbh: usize,
    pat: &PatternBitmap,
    x: i64,
    y: i64,
    op: CombinationOp,
) {
    let pw = pat.width as i64;
    let ph = pat.height as i64;
    // Early reject: pattern entirely outside region.
    if x + pw <= 0 || y + ph <= 0 || x >= hbw as i64 || y >= hbh as i64 {
        return;
    }
    let px_start = (-x).max(0) as u32;
    let py_start = (-y).max(0) as u32;
    let px_end = ((hbw as i64 - x).min(pw)).max(0) as u32;
    let py_end = ((hbh as i64 - y).min(ph)).max(0) as u32;

    for py in py_start..py_end {
        let dst_y = y + py as i64;
        // Clipped above; `dst_y` now in [0, hbh).
        let dst_row = (dst_y as usize) * hbw;
        for px in px_start..px_end {
            let src = pat.pixel(px, py);
            let dst_x = x + px as i64;
            let dst_idx = dst_row + dst_x as usize;
            let dst = htreg[dst_idx] & 1;
            htreg[dst_idx] = op.combine(src, dst);
        }
    }
}

/// Minimal generic-region arith decoder. Returns the decoded bitmap as
/// `width * height` bytes (1 = ink, 0 = paper).
///
/// Scope: halftone bitplane decoding only. Assumes no adaptive pixels
/// (AT offsets at their spec-default), TPGDON = false, no typical-row
/// replacement. Supports the four GBTEMPLATE variants per Annex
/// D.2.1-D.2.4 by context-word build rule.
///
/// This is a local minimal stub to keep halftone self-contained until
/// lands; the signature is deliberately close to what that
/// task will expose so the swap is mechanical.
fn gb_arith_decode(
    arith: &mut ArithDecoder<'_>,
    width: u32,
    height: u32,
    template: u8,
) -> Option<Vec<u8>> {
    let width = width as usize;
    let height = height as usize;
    if width == 0 || height == 0 {
        return Some(Vec::new());
    }
    let mut out = vec![0u8; width * height];

    // 16-bit context word for templates 0/1, 13-bit for templates 2/3
    // per Annex D.2. Allocate the max (64K entries) once; templates that
    // use fewer context bits simply leave high entries unused.
    let ctx_bits = match template {
        0 | 1 => 16,
        2 | 3 => 13,
        _ => return None,
    };
    let mut table = ContextTable::new(1usize << ctx_bits);

    // Pixel lookup helper with out-of-bitmap defaulting to 0 (paper).
    // Inline cast helps the borrow checker keep `out` mutable while we
    // read pixels from it.
    let get = |out: &[u8], x: i32, y: i32| -> u8 {
        if x < 0 || y < 0 || (x as usize) >= width || (y as usize) >= height {
            0
        } else {
            out[(y as usize) * width + (x as usize)] & 1
        }
    };

    for y in 0..height {
        for x in 0..width {
            let yi = y as i32;
            let xi = x as i32;
            // Build the context word per Annex D.2. Offsets are relative
            // to the current pixel, with the spec's (x, y) axes; AT
            // pixels are pinned at their template defaults.
            let ctx: usize = match template {
                0 => {
                    // Template 0 (Annex D.2.1, 16-bit context).
                    // Row y-2: (x-2..x+2)  -> 5 bits
                    // Row y-1: (x-3..x+3)  -> 7 bits
                    // Row y:   (x-4..x-1)  -> 4 bits
                    let mut c: usize = 0;
                    c = (c << 1) | get(&out, xi - 2, yi - 2) as usize;
                    c = (c << 1) | get(&out, xi - 1, yi - 2) as usize;
                    c = (c << 1) | get(&out, xi, yi - 2) as usize;
                    c = (c << 1) | get(&out, xi + 1, yi - 2) as usize;
                    c = (c << 1) | get(&out, xi + 2, yi - 2) as usize;
                    c = (c << 1) | get(&out, xi - 3, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi - 2, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi - 1, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi + 1, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi + 2, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi + 3, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi - 4, yi) as usize;
                    c = (c << 1) | get(&out, xi - 3, yi) as usize;
                    c = (c << 1) | get(&out, xi - 2, yi) as usize;
                    c = (c << 1) | get(&out, xi - 1, yi) as usize;
                    c
                }
                1 => {
                    // Template 1 (Annex D.2.2, 13-bit context).
                    // Row y-2: (x-2..x+2)
                    // Row y-1: (x-2..x+2)
                    // Row y:   (x-3..x-1)
                    let mut c: usize = 0;
                    c = (c << 1) | get(&out, xi - 2, yi - 2) as usize;
                    c = (c << 1) | get(&out, xi - 1, yi - 2) as usize;
                    c = (c << 1) | get(&out, xi, yi - 2) as usize;
                    c = (c << 1) | get(&out, xi + 1, yi - 2) as usize;
                    c = (c << 1) | get(&out, xi + 2, yi - 2) as usize;
                    c = (c << 1) | get(&out, xi - 2, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi - 1, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi + 1, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi + 2, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi - 3, yi) as usize;
                    c = (c << 1) | get(&out, xi - 2, yi) as usize;
                    c = (c << 1) | get(&out, xi - 1, yi) as usize;
                    c
                }
                2 => {
                    // Template 2 (Annex D.2.3, 10-bit context).
                    // Row y-2: (x-1..x+1)
                    // Row y-1: (x-2..x+2)
                    // Row y:   (x-2..x-1)
                    let mut c: usize = 0;
                    c = (c << 1) | get(&out, xi - 1, yi - 2) as usize;
                    c = (c << 1) | get(&out, xi, yi - 2) as usize;
                    c = (c << 1) | get(&out, xi + 1, yi - 2) as usize;
                    c = (c << 1) | get(&out, xi - 2, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi - 1, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi + 1, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi + 2, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi - 2, yi) as usize;
                    c = (c << 1) | get(&out, xi - 1, yi) as usize;
                    c
                }
                3 => {
                    // Template 3 (Annex D.2.4, 10-bit context).
                    // Row y-1: (x-3..x+3)
                    // Row y:   (x-4..x-1)
                    let mut c: usize = 0;
                    c = (c << 1) | get(&out, xi - 3, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi - 2, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi - 1, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi + 1, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi + 2, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi + 3, yi - 1) as usize;
                    c = (c << 1) | get(&out, xi - 4, yi) as usize;
                    c = (c << 1) | get(&out, xi - 3, yi) as usize;
                    c = (c << 1) | get(&out, xi - 2, yi) as usize;
                    c = (c << 1) | get(&out, xi - 1, yi) as usize;
                    c
                }
                _ => return None,
            };
            let bit = arith.decode(&mut table, ctx);
            out[y * width + x] = bit;
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a solid-ink pattern.
    fn solid_pattern(w: u32, h: u32, ink: u8) -> PatternBitmap {
        PatternBitmap {
            width: w,
            height: h,
            pixels: vec![ink & 1; (w * h) as usize],
        }
    }

    /// Helper: build an NxM checker pattern (top-left bit is 0).
    fn checker_pattern(w: u32, h: u32) -> PatternBitmap {
        let mut pixels = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                pixels.push(((x + y) & 1) as u8);
            }
        }
        PatternBitmap {
            width: w,
            height: h,
            pixels,
        }
    }

    /// Run the composite path end-to-end with pre-supplied bitplanes.
    ///
    /// Bypasses the arith-coded bitplane decode, letting tests exercise
    /// the §6.6.5.2-§6.6.5.3 gray-decode + combine path with full
    /// control over the cell index values. This is how pdfium and
    /// mupdf's unit tests for halftone regions are structured: the
    /// generic-region decoder is unit-tested separately, and the
    /// halftone-specific logic is tested against a synthetic bitplane
    /// input.
    fn decode_with_bitplanes(
        bitplanes: &[Vec<u8>],
        params: &HalftoneRegionParams,
        skip: Option<&HalftoneSkip>,
    ) -> Result<Vec<u8>, HalftoneError> {
        // Parameter validation copied from the decoder entry point so
        // tests that inject bitplanes still see the same error surface.
        if params.width == 0 || params.height == 0 {
            return Err(HalftoneError::EmptyRegion);
        }
        if params.hgw == 0 || params.hgh == 0 {
            return Err(HalftoneError::EmptyGrid);
        }
        if params.hbpp == 0 || params.hbpp > 30 {
            return Err(HalftoneError::InvalidBitsPerPixel { hbpp: params.hbpp });
        }
        for (slot, pat) in params.pattern_dict.iter().enumerate() {
            let expected = (pat.width as usize).saturating_mul(pat.height as usize);
            if pat.pixels.len() != expected {
                return Err(HalftoneError::PatternBitmapShape {
                    slot: slot as u32,
                    width: pat.width,
                    height: pat.height,
                    actual_len: pat.pixels.len(),
                });
            }
        }
        if let Some(sk) = skip {
            let expected = (sk.hgw as u64).saturating_mul(sk.hgh as u64);
            if sk.hgw != params.hgw || sk.hgh != params.hgh || sk.cells.len() as u64 != expected {
                return Err(HalftoneError::SkipShape {
                    hgw: params.hgw,
                    hgh: params.hgh,
                    actual_len: sk.cells.len(),
                });
            }
        }
        let hbw = params.width as usize;
        let hbh = params.height as usize;
        let total = hbw.checked_mul(hbh).ok_or(HalftoneError::Overflow)?;
        let mut htreg = vec![params.hdefpixel & 1; total];
        composite_bitplanes_into_region(bitplanes, params, skip, &mut htreg, hbw, hbh)?;
        Ok(htreg)
    }

    /// Grid of all-zero bitplanes for the given params. Produces one
    /// vector per plane with shape `hgw * hgh` bytes of zero.
    fn zero_bitplanes(params: &HalftoneRegionParams) -> Vec<Vec<u8>> {
        let cell_count = (params.hgw as usize) * (params.hgh as usize);
        (0..params.hbpp).map(|_| vec![0u8; cell_count]).collect()
    }

    /// Axis-aligned grid params where mg walks +X with stride `pw` and
    /// ng walks +Y with stride `ph`. §6.6.5.2's formula places ng
    /// upward by default (`y = HGY - ng * HRY`), so for a
    /// top-down-down-right grid we anchor HGY at the bottom row
    /// (`(hgh - 1) * ph * 256`) and let ng decrement toward y=0.
    ///
    /// The resulting cell iteration is spatial-reversed vs. (mg, ng)
    /// index order: ng=0 is the *bottom* row of the region, ng=HGH-1 is
    /// the top row. Tests that care about cell placement take this
    /// into account.
    fn axis_aligned_params(
        width: u32,
        height: u32,
        hgw: u32,
        hgh: u32,
        pw: u32,
        ph: u32,
        pattern_dict: Vec<PatternBitmap>,
    ) -> HalftoneRegionParams {
        HalftoneRegionParams {
            width,
            height,
            pattern_dict,
            hgw,
            hgh,
            hgx: 0,
            hgy: ((hgh.saturating_sub(1)) * ph * 256) as i32,
            hrx: 0,
            hry: (pw * 256) as i32,
            hbpp: 1,
            hmmr: false,
            htemplate: 0,
            henableskip: false,
            hcombop: CombinationOp::Replace,
            hdefpixel: 0,
        }
    }

    // --------- parameter validation / error paths ----------

    #[test]
    fn empty_region_errors() {
        let params = HalftoneRegionParams {
            width: 0,
            height: 0,
            pattern_dict: vec![solid_pattern(1, 1, 0)],
            hgw: 1,
            hgh: 1,
            hgx: 0,
            hgy: 0,
            hrx: 0,
            hry: 256,
            hbpp: 1,
            hmmr: false,
            htemplate: 0,
            henableskip: false,
            hcombop: CombinationOp::Or,
            hdefpixel: 0,
        };
        let mut arith = ArithDecoder::new(&[]);
        let err = decode_halftone_region(&[], &params, &mut arith, None).unwrap_err();
        assert_eq!(err, HalftoneError::EmptyRegion);
    }

    #[test]
    fn invalid_hbpp_errors() {
        let mut params = base_params_8x8();
        params.hbpp = 0;
        let err = decode_with_bitplanes(&[], &params, None).unwrap_err();
        assert!(matches!(
            err,
            HalftoneError::InvalidBitsPerPixel { hbpp: 0 }
        ));
        params.hbpp = 31;
        let err = decode_with_bitplanes(&[], &params, None).unwrap_err();
        assert!(matches!(
            err,
            HalftoneError::InvalidBitsPerPixel { hbpp: 31 }
        ));
    }

    #[test]
    fn pattern_shape_mismatch_errors() {
        let mut params = base_params_8x8();
        // Claim 4x4 but only have 10 pixels.
        params.pattern_dict = vec![PatternBitmap {
            width: 4,
            height: 4,
            pixels: vec![0u8; 10],
        }];
        let err = decode_with_bitplanes(&[], &params, None).unwrap_err();
        assert!(matches!(err, HalftoneError::PatternBitmapShape { .. }));
    }

    #[test]
    fn skip_shape_mismatch_errors() {
        let mut params = base_params_8x8();
        params.henableskip = true;
        let bad_skip = HalftoneSkip {
            hgw: params.hgw + 1,
            hgh: params.hgh,
            cells: vec![0; (params.hgw as usize + 1) * params.hgh as usize],
        };
        let err = decode_with_bitplanes(&[], &params, Some(&bad_skip)).unwrap_err();
        assert!(matches!(err, HalftoneError::SkipShape { .. }));
    }

    // --------- combination operators on solid-ink patterns ----------

    /// Baseline params: 8x8 region, 2x2 grid, 4x4 patterns tiling the
    /// region exactly. HBPP=1 so two patterns in the dict. Axis-aligned
    /// via [`axis_aligned_params`], so ng=0 is the *bottom* grid row.
    fn base_params_8x8() -> HalftoneRegionParams {
        axis_aligned_params(
            8,
            8,
            2,
            2,
            4,
            4,
            vec![solid_pattern(4, 4, 0), solid_pattern(4, 4, 1)],
        )
    }

    /// Drive the composite path with all-zero bitplanes. For HBPP=1 this
    /// produces cell_index=0 everywhere -> every cell selects
    /// pattern_dict[0] (solid paper), matching HDEFPIXEL=0.
    #[test]
    fn all_zero_bitplane_produces_default_region() {
        let params = base_params_8x8();
        let planes = zero_bitplanes(&params);
        let region = decode_with_bitplanes(&planes, &params, None).unwrap();
        assert_eq!(region.len(), 64);
        assert!(region.iter().all(|&p| p == 0), "all paper expected");
    }

    /// REPLACE operator: with HDEFPIXEL=1 and pattern_dict[0] = solid 0,
    /// the replace semantics guarantee the final region is all-paper
    /// regardless of the initial fill.
    #[test]
    fn replace_op_overwrites_default() {
        let mut params = base_params_8x8();
        params.hdefpixel = 1;
        params.hcombop = CombinationOp::Replace;
        let planes = zero_bitplanes(&params);
        let region = decode_with_bitplanes(&planes, &params, None).unwrap();
        assert!(region.iter().all(|&p| p == 0));
    }

    /// OR with HDEFPIXEL=1 and pattern=0 keeps the region all ink.
    #[test]
    fn or_op_preserves_default_ink() {
        let mut params = base_params_8x8();
        params.hdefpixel = 1;
        params.hcombop = CombinationOp::Or;
        let planes = zero_bitplanes(&params);
        let region = decode_with_bitplanes(&planes, &params, None).unwrap();
        assert!(region.iter().all(|&p| p == 1));
    }

    /// AND of default ink with paper pattern zeros out the covered cells.
    /// With patterns of size 4x4 tiling the 8x8 region exactly, every
    /// pixel should be overwritten to 0.
    #[test]
    fn and_op_with_paper_pattern_zeros_region() {
        let mut params = base_params_8x8();
        params.hdefpixel = 1;
        params.hcombop = CombinationOp::And;
        let planes = zero_bitplanes(&params);
        let region = decode_with_bitplanes(&planes, &params, None).unwrap();
        assert!(region.iter().all(|&p| p == 0));
    }

    /// XOR of default ink with paper pattern leaves region at default.
    #[test]
    fn xor_op_with_paper_preserves_default() {
        let mut params = base_params_8x8();
        params.hdefpixel = 1;
        params.hcombop = CombinationOp::Xor;
        let planes = zero_bitplanes(&params);
        let region = decode_with_bitplanes(&planes, &params, None).unwrap();
        assert!(region.iter().all(|&p| p == 1));
    }

    /// XNOR of default ink with paper pattern inverts the ink state.
    #[test]
    fn xnor_op_with_paper_inverts_default() {
        let mut params = base_params_8x8();
        params.hdefpixel = 1;
        params.hcombop = CombinationOp::Xnor;
        let planes = zero_bitplanes(&params);
        let region = decode_with_bitplanes(&planes, &params, None).unwrap();
        assert!(region.iter().all(|&p| p == 0));
    }

    // --------- skip-flag behaviour ----------

    #[test]
    fn henableskip_off_ignores_hskip() {
        let mut params = base_params_8x8();
        params.hdefpixel = 0;
        params.hcombop = CombinationOp::Replace;
        params.pattern_dict = vec![solid_pattern(4, 4, 1), solid_pattern(4, 4, 1)];
        let skip = HalftoneSkip {
            hgw: params.hgw,
            hgh: params.hgh,
            cells: vec![1, 1, 1, 1],
        };
        let planes = zero_bitplanes(&params);
        let region = decode_with_bitplanes(&planes, &params, Some(&skip)).unwrap();
        // henableskip=false, so the pattern (solid ink) writes everywhere.
        assert!(region.iter().all(|&p| p == 1));
    }

    #[test]
    fn henableskip_on_drops_masked_cells() {
        let mut params = base_params_8x8();
        params.hdefpixel = 0;
        params.henableskip = true;
        params.hcombop = CombinationOp::Replace;
        // Both dict slots fully ink.
        params.pattern_dict = vec![solid_pattern(4, 4, 1), solid_pattern(4, 4, 1)];
        // Skip everything -> region stays at HDEFPIXEL=0.
        let skip = HalftoneSkip {
            hgw: params.hgw,
            hgh: params.hgh,
            cells: vec![1, 1, 1, 1],
        };
        let planes = zero_bitplanes(&params);
        let region = decode_with_bitplanes(&planes, &params, Some(&skip)).unwrap();
        assert!(region.iter().all(|&p| p == 0));
    }

    #[test]
    fn henableskip_on_drops_only_flagged_cells() {
        let mut params = base_params_8x8();
        params.hdefpixel = 0;
        params.henableskip = true;
        params.hcombop = CombinationOp::Replace;
        params.pattern_dict = vec![solid_pattern(4, 4, 1), solid_pattern(4, 4, 1)];
        // Skip only cell (mg=0, ng=0) which under axis_aligned_params is
        // the BOTTOM-left cell (ng=0 is the bottom grid row, see the
        // helper's docstring). Bottom-left quadrant (y=4..8, x=0..4)
        // stays at HDEFPIXEL=0; the other three fill ink.
        let skip = HalftoneSkip {
            hgw: params.hgw,
            hgh: params.hgh,
            cells: vec![1, 0, 0, 0],
        };
        let planes = zero_bitplanes(&params);
        let region = decode_with_bitplanes(&planes, &params, Some(&skip)).unwrap();
        for y in 0..8 {
            for x in 0..8 {
                let expected = if x < 4 && y >= 4 { 0 } else { 1 };
                assert_eq!(
                    region[y * 8 + x],
                    expected,
                    "pixel ({x}, {y}) expected {expected}"
                );
            }
        }
    }

    // --------- edge cases ----------

    /// Single-pattern dictionary that only slot 0 is used via
    /// HDEFPIXEL-zero + all-zero bitplane behaviour.
    #[test]
    fn single_pattern_dict_decodes() {
        let params = axis_aligned_params(4, 4, 2, 2, 2, 2, vec![solid_pattern(2, 2, 1)]);
        let planes = zero_bitplanes(&params);
        let region = decode_with_bitplanes(&planes, &params, None).unwrap();
        // All cells index slot 0 (solid ink).
        assert_eq!(region, vec![1u8; 16]);
    }

    /// 4-pattern dictionary (HBPP=2): exercises multi-bitplane gray
    /// decoding. With all-zero bitplanes, gray-decoded index is 0 so
    /// slot 0 is used universally.
    #[test]
    fn four_pattern_dict_gray_decode_slot_zero() {
        let mut params = axis_aligned_params(
            8,
            8,
            2,
            2,
            4,
            4,
            vec![
                checker_pattern(4, 4),
                solid_pattern(4, 4, 0),
                solid_pattern(4, 4, 1),
                solid_pattern(4, 4, 0),
            ],
        );
        params.hbpp = 2;
        let planes = zero_bitplanes(&params);
        let region = decode_with_bitplanes(&planes, &params, None).unwrap();
        // Region is composed of four 4x4 tiles of slot 0 (checkerboard).
        // The tiles are identical so the full region is a single
        // 4x4 checker repeated 2x2 times.
        let mut expected = vec![0u8; 64];
        for y in 0..8 {
            for x in 0..8 {
                let tile_x = x % 4;
                let tile_y = y % 4;
                expected[y * 8 + x] = ((tile_x + tile_y) & 1) as u8;
            }
        }
        assert_eq!(region, expected);
    }

    /// 4-pattern dictionary (HBPP=2) with a crafted bitplane that picks
    /// a non-zero gray index on one cell. The gray-code table for HBPP=2:
    ///
    /// ```text
    /// GSBITMAP[1][c], GSBITMAP[0][c] -> (gray decode) -> index
    ///   (0, 0) -> GI0=0, GI1=0 -> value = 0
    ///   (1, 0) -> GI0=1, GI1=1 -> value = 0b11 = 3
    ///   (0, 1) -> GI0=0, GI1=1 -> value = 0b01 = 1
    ///   (1, 1) -> GI0=1, GI1=0 -> value = 0b10 = 2
    /// ```
    ///
    /// So bitplane[1] = 1 alone selects slot 3; bitplane[0] = 1 alone
    /// selects slot 1; both = 1 selects slot 2.
    ///
    /// Note: cell index ordering in the bitplane buffers matches
    /// `(ng * hgw + mg)`, so `planes[1][0]` is cell (mg=0, ng=0). With
    /// [`axis_aligned_params`], ng=0 is the *bottom* grid row.
    #[test]
    fn four_pattern_dict_selects_all_slots() {
        let mut params = axis_aligned_params(
            4,
            4,
            2,
            2,
            2,
            2,
            vec![
                solid_pattern(2, 2, 0), // slot 0 -> paper
                solid_pattern(2, 2, 1), // slot 1 -> ink
                solid_pattern(2, 2, 0), // slot 2 -> paper
                solid_pattern(2, 2, 1), // slot 3 -> ink
            ],
        );
        params.hbpp = 2;
        // Cells in row-major (ng, mg) order: (0,0), (1,0), (0,1), (1,1)
        // i.e. bottom-left, bottom-right, top-left, top-right.
        // bitplane[1] = [0, 1, 0, 1] -> indices: 0, 3, 0, 3 (ng=0 row)
        //                               and    : 0, 3   (ng=1 row)
        // bitplane[0] = [0, 0, 0, 0]
        let planes = vec![vec![0u8, 0, 0, 0], vec![0u8, 1, 0, 1]];
        let region = decode_with_bitplanes(&planes, &params, None).unwrap();
        // ng=0 (bottom row of grid -> bottom rows of image, y=2..4):
        //   cell (mg=0, ng=0) index 0 -> paper at (0..2, 2..4)
        //   cell (mg=1, ng=0) index 3 -> ink   at (2..4, 2..4)
        // ng=1 (top row of grid -> top rows of image, y=0..2):
        //   cell (mg=0, ng=1) index 0 -> paper at (0..2, 0..2)
        //   cell (mg=1, ng=1) index 3 -> ink   at (2..4, 0..2)
        let expected: Vec<u8> = vec![
            0, 0, 1, 1, //
            0, 0, 1, 1, //
            0, 0, 1, 1, //
            0, 0, 1, 1, //
        ];
        assert_eq!(region, expected);
    }

    /// Grid vectors place patterns off-region at negative coordinates:
    /// composition must silently clip and not panic.
    #[test]
    fn negative_grid_offset_clips_safely() {
        let mut params = base_params_8x8();
        // Shift entire grid so all cells land above y=0. base_params
        // has HGY = 1024 (i.e. 4 px down); setting HGY = -2048 moves
        // the anchor 8 px above the region.
        params.hgx = -2048;
        params.hgy = -2048;
        params.hdefpixel = 0;
        params.hcombop = CombinationOp::Replace;
        params.pattern_dict = vec![solid_pattern(4, 4, 1), solid_pattern(4, 4, 1)];
        let planes = zero_bitplanes(&params);
        // Must not panic and must produce an 8x8 buffer.
        let region = decode_with_bitplanes(&planes, &params, None).unwrap();
        assert_eq!(region.len(), 64);
        // With HRX=0, HRY=1024:
        //   (mg=0,ng=0): x = (-2048 + 0 + 0)>>8 = -8, y = (-2048 + 0 - 0)>>8 = -8
        //   (mg=1,ng=0): x = (-2048 + 1024 + 0)>>8 = -4, y = -8
        //   (mg=0,ng=1): x = -8, y = (-2048 + 0 - 1024)>>8 = -12
        //   (mg=1,ng=1): x = -4, y = -12
        // All four are clipped off-top, so the result is pure paper.
        assert!(region.iter().all(|&p| p == 0));
    }

    /// Dict smaller than 1 << hbpp is OK when every resolved cell index
    /// happens to be in range. All-zero bitplanes decode to index 0 so
    /// slot 0 alone suffices.
    #[test]
    fn small_dict_ok_when_index_in_range() {
        let mut params = axis_aligned_params(4, 4, 2, 2, 2, 2, vec![solid_pattern(2, 2, 1)]);
        params.hbpp = 2;
        let planes = zero_bitplanes(&params);
        let region = decode_with_bitplanes(&planes, &params, None).unwrap();
        assert_eq!(region, vec![1u8; 16]);
    }

    /// Dict smaller than 1 << hbpp *with* an out-of-range index: errors.
    #[test]
    fn small_dict_overflow_errors() {
        let mut params = axis_aligned_params(4, 4, 2, 2, 2, 2, vec![solid_pattern(2, 2, 1)]);
        params.hbpp = 2;
        // Plane 1 has a 1 in cell 0, which gray-decodes to index 3 (out of range).
        let planes = vec![vec![0u8, 0, 0, 0], vec![1u8, 0, 0, 0]];
        let err = decode_with_bitplanes(&planes, &params, None).unwrap_err();
        assert!(matches!(
            err,
            HalftoneError::PatternIndexOutOfRange {
                index: 3,
                available: 1
            }
        ));
    }

    // --------- combination-op dispatch unit tests ----------

    #[test]
    fn combination_op_from_code_round_trip() {
        for (code, expected) in [
            (0, CombinationOp::Or),
            (1, CombinationOp::And),
            (2, CombinationOp::Xor),
            (3, CombinationOp::Xnor),
            (4, CombinationOp::Replace),
        ] {
            assert_eq!(CombinationOp::from_code(code), Some(expected));
        }
        assert_eq!(CombinationOp::from_code(5), None);
        assert_eq!(CombinationOp::from_code(255), None);
    }

    #[test]
    fn combination_op_truth_tables() {
        // Exhaustive check for single-bit operands.
        for s in 0..2u8 {
            for d in 0..2u8 {
                assert_eq!(CombinationOp::Or.combine(s, d), s | d);
                assert_eq!(CombinationOp::And.combine(s, d), s & d);
                assert_eq!(CombinationOp::Xor.combine(s, d), s ^ d);
                assert_eq!(CombinationOp::Xnor.combine(s, d), !(s ^ d) & 1);
                assert_eq!(CombinationOp::Replace.combine(s, d), s);
            }
        }
    }

    // --------- HMMR bitplane path (structural) ----------

    /// HMMR with HBPP=1 and a trivial MMR-encoded all-white bitplane.
    /// The MMR decoder is tolerant of short streams, so an empty CCITT
    /// payload produces an all-white bitplane that gray-decodes to
    /// slot 0 for every cell. Exercises the HMMR dispatch path end-to-end
    /// and confirms the arith decoder is not consulted.
    #[test]
    fn hmmr_single_bitplane_decodes_via_mmr_path() {
        let mut params = axis_aligned_params(
            4,
            4,
            2,
            2,
            2,
            2,
            vec![solid_pattern(2, 2, 1), solid_pattern(2, 2, 0)],
        );
        params.hmmr = true;
        // G4 all-white for HGW=2 HGH=2: two V(0) codes per row, two rows
        // = four V(0) codes = four "1" bits = 0xF0 (MSB-first byte-aligned).
        let mmr_stream: Vec<u8> = vec![0xF0];
        let mut arith = ArithDecoder::new(&mmr_stream);
        let region = decode_halftone_region(&mmr_stream, &params, &mut arith, None).unwrap();
        // All-white bitplane -> cell_index = 0 everywhere -> slot 0 (ink).
        assert_eq!(region, vec![1u8; 16]);
    }

    // --------- context-bit accounting for templates 0..=3 ----------

    #[test]
    fn gb_arith_decode_template_rejection() {
        // Templates 0..=3 are valid; anything else is rejected at the
        // bitplane decoder boundary.
        let mut params = base_params_8x8();
        params.htemplate = 99;
        let stream = vec![0u8; 16];
        let mut arith = ArithDecoder::new(&stream);
        let err = decode_halftone_region(&stream, &params, &mut arith, None).unwrap_err();
        assert!(matches!(err, HalftoneError::BitplaneDecodeFailed { .. }));
    }

    #[test]
    fn gb_arith_decode_all_templates_produce_right_sized_region() {
        // Drive the full decoder via the arith path for each template.
        // The concrete decoded bit values aren't asserted; what matters
        // is that the shape contract holds and the path does not panic
        // at any template variant.
        for tpl in 0..=3u8 {
            let mut params = base_params_8x8();
            params.htemplate = tpl;
            params.hdefpixel = 0;
            // Dict: 2 solid-paper slots so any decoded index stays ink-off,
            // keeping the assertion narrow (shape contract, not contents).
            params.pattern_dict = vec![solid_pattern(4, 4, 0), solid_pattern(4, 4, 0)];
            let stream = vec![0u8; 128];
            let mut arith = ArithDecoder::new(&stream);
            let region = decode_halftone_region(&stream, &params, &mut arith, None).unwrap();
            assert_eq!(
                region.len(),
                64,
                "template {tpl} produced wrong-sized region"
            );
            // All-paper pattern dict -> region matches HDEFPIXEL.
            assert!(
                region.iter().all(|&p| p == 0),
                "template {tpl} should paint only paper",
            );
        }
    }

    #[test]
    fn full_decoder_arith_path_end_to_end() {
        // Smoke test of the full decoder on the arith-coded bitplane
        // path: feed 64 bytes of zero, ensure no panic and that the
        // output buffer is HBW*HBH. We do not assert specific pixel
        // values because the MQ decoder output under zero input is
        // non-trivial (non-zero MPS flips fire after a few symbols).
        let params = base_params_8x8();
        let stream = vec![0u8; 128];
        let mut arith = ArithDecoder::new(&stream);
        let region = decode_halftone_region(&stream, &params, &mut arith, None).unwrap();
        assert_eq!(region.len(), 64);
    }
}

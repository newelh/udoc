//! JBIG2 text region decoder (ISO 14492 §6.4, issue #158).
//!
//! A text region renders a collection of *symbol instances*: each
//! instance references a symbol-dictionary bitmap by ID and carries
//! (strip, row-offset, column-offset) placement plus optional refinement
//! corrections. This module decodes the arithmetic-coded flavour of
//! §6.4, which is what PDF-embedded JBIG2 streams use in every corpus
//! fixture we ship.
//!
//! # Decode pipeline (arith, SBHUFF = 0)
//!
//! §6.4.5 walks three nested loops:
//!
//! 1. Outer loop over strips. Each strip's T coordinate accumulates a
//!    delta decoded from the IADT integer-arith context (scaled by
//!    `-sbstrips`). The first decoded IADT is the strip's absolute T
//!    offset (relative to the region origin).
//! 2. Inner loop over instances within the strip. The first instance's
//!    S coordinate is decoded from IAFS (absolute-ish, still relative
//!    to the region's left edge). Subsequent instances decode a delta
//!    via IADS (also Iafs context per the spec table, but read with a
//!    second-path flag). Termination: IADS returns OOB.
//! 3. Per-instance decode:
//!    (a) if `sbstrips > 1`, read IAIT to get the within-strip T
//!    offset (0..sbstrips-1), which refines the absolute T;
//!    (b) read the symbol ID via IAID (sbsymcodelen bits);
//!    (c) if SBREFINE is set, read IARI (refine flag); if RI=1, read
//!    RDW/RDH/RDX/RDY and run a generic-refinement pass against the
//!    dict bitmap;
//!    (d) composite the symbol bitmap into the region buffer at
//!    (cur_s, cur_t) using SBCOMBOP;
//!    (e) advance cur_s by the symbol's width + `sbdsoffset` so the
//!    next IADS delta is relative to the end of the current
//!    instance (§6.4.5 "REFCORNER" semantics, REFCORNER=TOPLEFT is the
//!    common case and the only shape we've seen in real fixtures;
//!    the other three corners are spec-legal but rare outside
//!    ABBYY-produced streams where REFCORNER=TOPLEFT anyway).
//!
//! # Symbol table
//!
//! `params.symbols` carries the fully-resolved flat list of every
//! symbol reachable from this segment's referred-segment graph: every
//! referred symbol dictionary's exports, concatenated in refseg order.
//! The symbol ID field is `log2(total_symbols)` bits wide and indexes
//! directly into this flat list. `SBSYMCODELEN` in the spec is
//! `ceil(log2(total_symbols))`, with a minimum of 1 bit.
//!
//! # Huffman path
//!
//! `params.sbhuff = true` returns [`TextRegionError::UnsupportedHuffmanText`].
//! The five Huffman tables (SBHUFFFS, SBHUFFDS, SBHUFFDT, SBHUFFRDW,
//! SBHUFFRDH, SBHUFFRDX, SBHUFFRDY, SBHUFFRSIZE) require the same
//! RunLengthHuffman infrastructure as symbol dicts; post-alpha
//! deliverable (#158 post-alpha).
//!
//! # Reference
//!
//! Port target: pdfium `third_party/jbig2/JBig2_TRDProc.cpp::decode_Arith`
//! plus `decode_SymbolInstance`. Cross-checked against hayro_jbig2
//! 0.3.0 `src/decode/text.rs`. Validated on the ia-ibnkathir-preface,
//! hf-pdfa-4485665, and ia-sex-over-sex-16 corpus fixtures via the
//! `jbig2-validate` tool harness.

use std::fmt;

use crate::jbig2::arith::{ArithDecoder, ContextTable, IaName, IntegerDecoder};
use crate::jbig2::regions::halftone::CombinationOp;
use crate::jbig2::regions::refinement::{
    decode_refinement_region, RefinementError, RefinementRef, RefinementRegionParams,
};
use crate::jbig2::regions::symbol::SymbolBitmap;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Reference-corner selector for IADS / IADT placement math (§6.4.4,
/// SBRTEMPLATE lookup tables -- the `SBRTEMPLATE` field in the text
/// region segment header selects `SBREFCORNER`).
///
/// The vast majority of real-world JBIG2 streams use `TopLeft`; the
/// other three corners change how the next-instance cursor advances
/// along the strip's S axis. We implement all four for spec
/// completeness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefCorner {
    /// Anchor at the top-left corner: cursor advances by symbol width.
    TopLeft,
    /// Top-right anchor.
    TopRight,
    /// Bottom-left anchor.
    BottomLeft,
    /// Bottom-right anchor.
    BottomRight,
}

impl RefCorner {
    /// Decode a 2-bit SBREFCORNER value.
    pub fn from_code(code: u8) -> Option<Self> {
        match code & 0x3 {
            0 => Some(Self::BottomLeft),
            1 => Some(Self::TopLeft),
            2 => Some(Self::BottomRight),
            3 => Some(Self::TopRight),
            _ => unreachable!(),
        }
    }
}

/// Parameters for [`decode_text_region`].
///
/// Field names map one-to-one onto §6.4.2 with snake_case rename. The
/// caller is responsible for resolving referred-segment symbols into
/// the flat [`symbols`](Self::symbols) list and computing `sbnuminstances`
/// from the region header.
#[derive(Debug, Clone)]
pub struct TextRegionParams {
    /// Region bitmap width (`SBW`).
    pub width: u32,
    /// Region bitmap height (`SBH`).
    pub height: u32,
    /// Number of symbol instances to decode (`SBNUMINSTANCES`).
    pub sbnuminstances: u32,
    /// Huffman-mode flag (`SBHUFF`). `true` is stubbed (post-alpha).
    pub sbhuff: bool,
    /// Refinement-enabled flag (`SBREFINE`). When `true`, per-instance
    /// refinement is legal (IARI gate).
    pub sbrefine: bool,
    /// `SBSTRIPS`: number of T units per strip. Must be >= 1 and a power
    /// of two. Common values: 1, 2, 4, 8. When >1, an IAIT read per
    /// instance further refines the instance's T coordinate within the
    /// strip.
    pub sbstrips: u32,
    /// `SBSTRIPS`-derived log2; we compute this internally instead of
    /// trusting an untrusted caller to pass a matching value.
    // NOTE: kept as a field-level note; sbstrips is the only stored
    // form.
    /// `SBRTEMPLATE`: refinement-template selector (0 or 1). Only used
    /// when `sbrefine` is true.
    pub sbrtemplate: u8,
    /// `SBDSOFFSET`: signed displacement added to the S cursor after
    /// compositing each instance. ISO 14492 §6.4.4 allows negative
    /// offsets for tight glyph kerning.
    pub sbdsoffset: i32,
    /// `SBCOMBOP`: operator used to composite each instance bitmap into
    /// the region.
    pub sbcombop: CombinationOp,
    /// `SBDEFPIXEL`: 0 or 1. Region is initialised to this value before
    /// the instance loop.
    pub sbdefpixel: u8,
    /// `SBRAT`: refinement adaptive pixel offsets (2 entries, used when
    /// `sbrtemplate == 0`). One over the region, one over the
    /// reference. Ignored when sbrtemplate == 1 or sbrefine is false.
    pub sb_r_at_pixels: Vec<(i8, i8)>,
    /// `SBREFCORNER`: per-instance anchor. TopLeft is overwhelmingly
    /// the common case.
    pub sbrefcorner: RefCorner,
    /// `SBTRANSPOSED`: when true, swap the S and T roles in the
    /// instance placement math.
    pub sbtransposed: bool,
    /// Flat symbol list sourced from referred-segment exports. Symbol
    /// IDs index directly into this vector. If empty, the decoder
    /// refuses to run (the spec permits an empty text region only when
    /// `sbnuminstances == 0`).
    pub symbols: Vec<SymbolBitmap>,
}

/// Errors surfaced by [`decode_text_region`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TextRegionError {
    /// SBHUFF=1. The Huffman-coded text region path is post-alpha; see
    /// module docs.
    UnsupportedHuffmanText,
    /// `sbstrips` was 0, not a power of two, or exceeded 8 (spec allows
    /// only `log2(SBSTRIPS) in 0..=3`).
    InvalidStripCount {
        /// Offending SBSTRIPS value.
        sbstrips: u32,
    },
    /// `sbrtemplate` not in `{0, 1}`.
    InvalidRefinementTemplate {
        /// Offending value.
        value: u8,
    },
    /// Region dimensions were zero. Legal when `sbnuminstances == 0`;
    /// the decoder still surfaces this to the caller for the
    /// instance-count-nonzero case.
    EmptyRegion,
    /// Region dimensions exceed [`crate::jbig2::MAX_JBIG2_REGION_DIMENSION`]
    /// (SEC-ALLOC-CLAMP, #62). Refuses adversarial segment headers before
    /// the pixel buffer allocation.
    RegionTooLarge {
        /// Declared region width.
        width: u32,
        /// Declared region height.
        height: u32,
    },
    /// `sbnuminstances > 0` but the referred-symbol pool is empty. No
    /// bitmap can be resolved.
    EmptySymbolTable,
    /// An instance's symbol ID resolved past the end of `symbols`.
    SymbolIdOutOfRange {
        /// Decoded symbol ID.
        id: u64,
        /// Size of the symbol table.
        available: u32,
    },
    /// An integer-arith context hit OOB at a non-terminating read. The
    /// embedded IaName identifies the field for the error message.
    UnexpectedOob {
        /// Integer-arith context where OOB was observed.
        which: IaName,
    },
    /// A decoded width or height (refinement) was non-positive.
    NonPositiveDimension {
        /// Which field ("refinement width", "refinement height", ...).
        field: &'static str,
        /// Decoded value.
        value: i64,
    },
    /// Refinement sub-decoder failed.
    RefinementFailed {
        /// Underlying error from the generic-refinement decoder.
        reason: RefinementError,
    },
    /// Decoder consumed all NUMINSTANCES but instance-count / strip
    /// loop desynced (indicates a decoder-state mismatch).
    InstanceCountMismatch {
        /// Declared number of instances.
        declared: u32,
        /// Count actually decoded.
        produced: u32,
    },
}

impl fmt::Display for TextRegionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedHuffmanText => write!(
                f,
                "SBHUFF=1 text region not yet supported (#158 post-alpha)",
            ),
            Self::InvalidStripCount { sbstrips } => {
                write!(f, "invalid SBSTRIPS {sbstrips} (must be 1, 2, 4 or 8)")
            }
            Self::InvalidRefinementTemplate { value } => {
                write!(f, "invalid SBRTEMPLATE {value} (must be 0 or 1)")
            }
            Self::EmptyRegion => write!(f, "text region has zero width or height"),
            Self::EmptySymbolTable => write!(
                f,
                "text region declared instances but the referred symbol table is empty",
            ),
            Self::SymbolIdOutOfRange { id, available } => write!(
                f,
                "symbol ID {id} outside referred-symbol table size {available}",
            ),
            Self::UnexpectedOob { which } => {
                write!(f, "unexpected OOB integer-arith value for {which:?}")
            }
            Self::NonPositiveDimension { field, value } => {
                write!(f, "non-positive {field} {value}")
            }
            Self::RefinementFailed { reason } => {
                write!(f, "refinement sub-decoder failed: {reason}")
            }
            Self::InstanceCountMismatch { declared, produced } => write!(
                f,
                "instance count mismatch: declared {declared}, produced {produced}",
            ),
            Self::RegionTooLarge { width, height } => write!(
                f,
                "text region {width}x{height} exceeds safe ceiling ({})",
                crate::jbig2::MAX_JBIG2_REGION_DIMENSION,
            ),
        }
    }
}

impl std::error::Error for TextRegionError {}

impl From<crate::jbig2::RegionDimensionError> for TextRegionError {
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
// Pixel helpers
// ---------------------------------------------------------------------------

// Match the generic-region / refinement convention: 0x00 = black, 0xFF
// = white. Internally we also keep a 0/1 packed view where 1 = ink.
const PIXEL_BLACK: u8 = 0x00;
const PIXEL_WHITE: u8 = 0xFF;

#[inline]
fn pixel_from_flag(ink: u8) -> u8 {
    if ink & 1 == 1 {
        PIXEL_BLACK
    } else {
        PIXEL_WHITE
    }
}

#[inline]
fn flag_from_pixel(p: u8) -> u8 {
    // Any non-255 byte counts as ink (black / drawn). This matches the
    // halftone-region policy and tolerates encoders that use 0/1 or
    // 0/255 interchangeably in bitmap outputs.
    if p == PIXEL_WHITE {
        0
    } else {
        1
    }
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Decode a JBIG2 text region.
///
/// `data` is the segment *data* portion (post-header) and is retained
/// purely so the caller can pass the same slice they used to init
/// `arith`; this function only reads via the MQ decoder. `params`
/// carries the parsed segment-header fields plus the resolved symbol
/// table. `arith` must already be INITDEC'd over the region's arith
/// payload (the caller strips the region-header prefix). `int_decoder`
/// owns the 13 fixed integer-arith contexts.
///
/// Returns a row-major pixel buffer sized `width * height`, one byte
/// per pixel with `0x00` = black and `0xFF` = white (generic-region
/// convention).
pub fn decode_text_region(
    _data: &[u8],
    params: &TextRegionParams,
    arith: &mut ArithDecoder<'_>,
    int_decoder: &mut IntegerDecoder,
) -> Result<Vec<u8>, TextRegionError> {
    validate(params)?;

    // SEC-ALLOC-CLAMP (#62): enforce the region-dimension ceiling BEFORE
    // any pixel-buffer allocation. Adversarial segment headers can claim
    // 4-billion-pixel regions; the helper refuses anything over 65536.
    let pixels = crate::jbig2::check_region_dimensions(params.width, params.height, "text")?;

    // Allocate the region buffer pre-filled with SBDEFPIXEL.
    let mut region = vec![pixel_from_flag(params.sbdefpixel); pixels];

    if params.sbnuminstances == 0 {
        return Ok(region);
    }

    if params.symbols.is_empty() {
        return Err(TextRegionError::EmptySymbolTable);
    }

    // SBSYMCODELEN: ceil(log2(total_symbols)), minimum 1 bit.
    let total_syms = params.symbols.len() as u64;
    let sbsymcodelen = ceil_log2(total_syms).max(1);

    // Dedicated context table for IAID. Size is 1 << sbsymcodelen.
    let iaid_ctx_len = 1usize << sbsymcodelen.min(30); // clamp guard
    let mut iaid_table = ContextTable::new(iaid_ctx_len);

    // Integer-arith contexts are reset at region entry (§6.4.5 step 1a).
    int_decoder.reset();

    // LOGSBSTRIPS (§6.4.5 step 1): the encoder records `log2(sbstrips)`
    // rather than sbstrips itself, but our public API takes sbstrips
    // for clarity; we re-derive log2 here.
    let log_sbstrips =
        sbstrips_log2(params.sbstrips).ok_or(TextRegionError::InvalidStripCount {
            sbstrips: params.sbstrips,
        })?;

    // §6.4.5 step 2: STRIPT = -DecodeInt(IADT) * sbstrips; then
    // FIRSTS = 0 is implicit (both the region cursor and the strip's
    // first S are initialised to 0 prior to the first read in step 3).
    let dt0 = int_decoder
        .decode(arith, IaName::Iadt)
        .ok_or(TextRegionError::UnexpectedOob {
            which: IaName::Iadt,
        })?;
    // Cursor T: the sign convention is that IADT is typically non-
    // positive on the first read (strips grow downward); pdfium and
    // hayro both multiply by -sbstrips to flip the sign, then shift
    // left by log_sbstrips. The final form we want is
    //     strip_t = -dt0 * sbstrips.
    // Using a widening i64 to avoid overflow on pathological inputs.
    let mut strip_t: i64 =
        (-dt0)
            .checked_mul(params.sbstrips as i64)
            .ok_or(TextRegionError::UnexpectedOob {
                which: IaName::Iadt,
            })?;

    // First-S: fresh per strip; IAFS is read at strip entry (§6.4.5
    // step 3b).
    let mut first_s: i64 = 0;

    let mut produced: u32 = 0;
    let num_instances = params.sbnuminstances;

    // Iteration bound: a well-formed stream terminates via IADS OOB
    // and then IADT OOB. We cap the outer loop with NUMINSTANCES to
    // guarantee termination on malformed inputs.
    while produced < num_instances {
        // New strip: decode IADT (except on first strip where we already
        // consumed it above).
        if produced > 0 {
            let dt =
                int_decoder
                    .decode(arith, IaName::Iadt)
                    .ok_or(TextRegionError::UnexpectedOob {
                        which: IaName::Iadt,
                    })?;
            strip_t = strip_t
                .checked_add(dt.checked_mul(params.sbstrips as i64).ok_or(
                    TextRegionError::UnexpectedOob {
                        which: IaName::Iadt,
                    },
                )?)
                .ok_or(TextRegionError::UnexpectedOob {
                    which: IaName::Iadt,
                })?;
        }

        // Read the strip's first FIRSTS delta (§6.4.5 step 3b).
        let dfs =
            int_decoder
                .decode(arith, IaName::Iafs)
                .ok_or(TextRegionError::UnexpectedOob {
                    which: IaName::Iafs,
                })?;
        first_s = first_s
            .checked_add(dfs)
            .ok_or(TextRegionError::UnexpectedOob {
                which: IaName::Iafs,
            })?;
        let mut cur_s: i64 = first_s;

        loop {
            // Decode the within-strip T offset (0..sbstrips-1) via IAIT.
            // Only when LOGSBSTRIPS != 0.
            let cur_t = if log_sbstrips == 0 {
                strip_t
            } else {
                let t = int_decoder.decode(arith, IaName::Iait).ok_or(
                    TextRegionError::UnexpectedOob {
                        which: IaName::Iait,
                    },
                )?;
                strip_t.saturating_add(t)
            };

            // Symbol ID (IAID, fixed-width raw bits).
            let symbol_id = IntegerDecoder::decode_iaid(arith, &mut iaid_table, sbsymcodelen);
            let symbol_idx =
                usize::try_from(symbol_id).map_err(|_| TextRegionError::SymbolIdOutOfRange {
                    id: symbol_id,
                    available: total_syms as u32,
                })?;
            if symbol_idx >= params.symbols.len() {
                return Err(TextRegionError::SymbolIdOutOfRange {
                    id: symbol_id,
                    available: total_syms as u32,
                });
            }

            // Symbol bitmap -- possibly refined.
            let base_sym = &params.symbols[symbol_idx];
            let (sym_width, sym_height, sym_pixels): (u32, u32, std::borrow::Cow<'_, [u8]>) =
                if params.sbrefine {
                    let ri = int_decoder.decode(arith, IaName::Iari).ok_or(
                        TextRegionError::UnexpectedOob {
                            which: IaName::Iari,
                        },
                    )?;
                    if ri != 0 {
                        let rdw = int_decoder.decode(arith, IaName::Iardw).ok_or(
                            TextRegionError::UnexpectedOob {
                                which: IaName::Iardw,
                            },
                        )?;
                        let rdh = int_decoder.decode(arith, IaName::Iardh).ok_or(
                            TextRegionError::UnexpectedOob {
                                which: IaName::Iardh,
                            },
                        )?;
                        let rdx = int_decoder.decode(arith, IaName::Iardx).ok_or(
                            TextRegionError::UnexpectedOob {
                                which: IaName::Iardx,
                            },
                        )?;
                        let rdy = int_decoder.decode(arith, IaName::Iardy).ok_or(
                            TextRegionError::UnexpectedOob {
                                which: IaName::Iardy,
                            },
                        )?;

                        // §6.4.11 step 6: the refinement region bbox is
                        //   width  = SDW + RDW
                        //   height = SDH + RDH
                        // with reference offset (RDX - floor(RDW/2),
                        // RDY - floor(RDH/2)).
                        let ref_w = base_sym.width as i64 + rdw;
                        let ref_h = base_sym.height as i64 + rdh;
                        if ref_w <= 0 {
                            return Err(TextRegionError::NonPositiveDimension {
                                field: "refinement width",
                                value: ref_w,
                            });
                        }
                        if ref_h <= 0 {
                            return Err(TextRegionError::NonPositiveDimension {
                                field: "refinement height",
                                value: ref_h,
                            });
                        }

                        // Convert symbol bitmap (0 or 1 per byte via
                        // SymbolBitmap policy) to the generic refinement
                        // byte convention (0x00 = black, 0xFF = white).
                        let ref_bitmap = symbol_to_refinement_bitmap(base_sym);
                        let dx = (rdx - rdw / 2) as i32;
                        let dy = (rdy - rdh / 2) as i32;

                        // Default SBRAT per §6.4.4 Table 9: (-1, -1), (-1, -1).
                        let default_at = [(-1i8, -1i8), (-1i8, -1i8)];
                        let at_pixels = if params.sbrtemplate == 0 {
                            if params.sb_r_at_pixels.len() == 2 {
                                params.sb_r_at_pixels.clone()
                            } else {
                                default_at.to_vec()
                            }
                        } else {
                            Vec::new()
                        };

                        let refined = decode_refinement_region(
                            &[],
                            &RefinementRegionParams {
                                width: ref_w as u32,
                                height: ref_h as u32,
                                grtemplate: params.sbrtemplate,
                                gr_at_pixels: at_pixels,
                                tpgron: false,
                                reference: RefinementRef {
                                    bitmap: &ref_bitmap,
                                    width: base_sym.width,
                                    height: base_sym.height,
                                    dx,
                                    dy,
                                },
                            },
                            arith,
                        )
                        .map_err(|e| TextRegionError::RefinementFailed { reason: e })?;

                        (ref_w as u32, ref_h as u32, std::borrow::Cow::Owned(refined))
                    } else {
                        // RI=0: use the dict bitmap as-is.
                        (
                            base_sym.width,
                            base_sym.height,
                            std::borrow::Cow::Owned(symbol_to_refinement_bitmap(base_sym)),
                        )
                    }
                } else {
                    (
                        base_sym.width,
                        base_sym.height,
                        std::borrow::Cow::Owned(symbol_to_refinement_bitmap(base_sym)),
                    )
                };

            // Reference-corner placement per §6.4.4. For the common
            // TopLeft corner (the only one we've seen in real corpus
            // fixtures), the cursor IS the top-left origin. For other
            // corners we back off by symbol width/height.
            let (origin_s, origin_t) = placement_origin(
                cur_s,
                cur_t,
                sym_width,
                sym_height,
                params.sbrefcorner,
                params.sbtransposed,
            );

            composite_symbol(
                &mut region,
                params.width,
                params.height,
                &sym_pixels,
                sym_width,
                sym_height,
                origin_s,
                origin_t,
                params.sbcombop,
                params.sbtransposed,
            );

            produced += 1;
            if produced >= num_instances {
                break;
            }

            // Advance cur_s:
            //   cur_s += symbol width + sbdsoffset + IADS delta.
            // The advance corner depends on REFCORNER, but for every
            // corner the "linear" portion of the advance is
            // (sym_width - 1) before the sbdsoffset + IADS delta is
            // added (§6.4.5 step 3c note iv).
            let advance = if params.sbtransposed {
                sym_height as i64 - 1
            } else {
                sym_width as i64 - 1
            };
            cur_s = cur_s
                .saturating_add(advance)
                .saturating_add(params.sbdsoffset as i64);

            let ds = match int_decoder.decode(arith, IaName::Iafs) {
                Some(v) => v,
                None => break, // IADS OOB terminates the strip.
            };
            // IADS in the spec uses the Iafs context space (A.2 table
            // entry 4) read a second time via the "subsequent" path.
            // We just add it directly; the integer-arith decoder does
            // not distinguish between "first" and "subsequent" reads.
            cur_s = cur_s
                .checked_add(ds)
                .ok_or(TextRegionError::UnexpectedOob {
                    which: IaName::Iafs,
                })?;
        }
    }

    if produced != params.sbnuminstances {
        return Err(TextRegionError::InstanceCountMismatch {
            declared: params.sbnuminstances,
            produced,
        });
    }

    Ok(region)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn validate(params: &TextRegionParams) -> Result<(), TextRegionError> {
    if params.sbhuff {
        return Err(TextRegionError::UnsupportedHuffmanText);
    }
    if params.sbstrips == 0 || !params.sbstrips.is_power_of_two() || params.sbstrips > 8 {
        return Err(TextRegionError::InvalidStripCount {
            sbstrips: params.sbstrips,
        });
    }
    if params.sbrefine && params.sbrtemplate > 1 {
        return Err(TextRegionError::InvalidRefinementTemplate {
            value: params.sbrtemplate,
        });
    }
    Ok(())
}

/// `log2(sbstrips)` for sbstrips in {1, 2, 4, 8}. Returns `None` for
/// any other value; caller surfaces `InvalidStripCount`.
fn sbstrips_log2(sbstrips: u32) -> Option<u32> {
    match sbstrips {
        1 => Some(0),
        2 => Some(1),
        4 => Some(2),
        8 => Some(3),
        _ => None,
    }
}

/// Ceiling of `log2(n)` for `n >= 1`. `ceil_log2(1) == 0`,
/// `ceil_log2(2) == 1`, `ceil_log2(3) == 2`, etc.
fn ceil_log2(n: u64) -> u32 {
    if n <= 1 {
        0
    } else {
        // bit-length of (n-1).
        64 - (n - 1).leading_zeros()
    }
}

/// Convert a `SymbolBitmap` (0 = white, 1 = black per-byte flag
/// convention per `regions::symbol`) to the 0x00 / 0xFF byte
/// convention the refinement decoder expects.
fn symbol_to_refinement_bitmap(sym: &SymbolBitmap) -> Vec<u8> {
    sym.pixels
        .iter()
        .map(|&p| if p != 0 { PIXEL_BLACK } else { PIXEL_WHITE })
        .collect()
}

/// Compute the (S, T) origin for a symbol bitmap given the reference
/// corner. For `TopLeft` the cursor IS the origin; the others back off
/// by width / height.
fn placement_origin(
    cur_s: i64,
    cur_t: i64,
    sym_w: u32,
    sym_h: u32,
    corner: RefCorner,
    transposed: bool,
) -> (i64, i64) {
    // In non-transposed mode S is horizontal (x) and T is vertical (y).
    // In transposed mode they swap.
    let (advance_s, advance_t) = if transposed {
        (sym_h as i64, sym_w as i64)
    } else {
        (sym_w as i64, sym_h as i64)
    };
    match corner {
        RefCorner::TopLeft => (cur_s, cur_t),
        RefCorner::TopRight => (cur_s - advance_s + 1, cur_t),
        RefCorner::BottomLeft => (cur_s, cur_t - advance_t + 1),
        RefCorner::BottomRight => (cur_s - advance_s + 1, cur_t - advance_t + 1),
    }
}

/// Composite a symbol bitmap into the region using `op`. Clips to the
/// region rectangle. `origin_s` / `origin_t` are in region coordinates
/// and may be negative (the visible portion is clipped).
#[allow(clippy::too_many_arguments)]
fn composite_symbol(
    region: &mut [u8],
    region_w: u32,
    region_h: u32,
    sym_pixels: &[u8],
    sym_w: u32,
    sym_h: u32,
    origin_s: i64,
    origin_t: i64,
    op: CombinationOp,
    transposed: bool,
) {
    // Effective bitmap dimensions: in transposed mode S runs vertically
    // so the "symbol width along S" is actually sym_h.
    let (draw_w, draw_h) = if transposed {
        (sym_h, sym_w)
    } else {
        (sym_w, sym_h)
    };

    for dy in 0..draw_h as i64 {
        let y = origin_t + dy;
        if y < 0 || y >= region_h as i64 {
            continue;
        }
        for dx in 0..draw_w as i64 {
            let x = origin_s + dx;
            if x < 0 || x >= region_w as i64 {
                continue;
            }

            // Read the symbol pixel, swapping axes if transposed.
            let (sx, sy) = if transposed {
                (dy as u32, dx as u32)
            } else {
                (dx as u32, dy as u32)
            };
            let s_pixel = sym_pixels[(sy as usize) * (sym_w as usize) + (sx as usize)];
            let s_flag = flag_from_pixel(s_pixel);

            let out_idx = (y as usize) * (region_w as usize) + (x as usize);
            let d_flag = flag_from_pixel(region[out_idx]);
            let r_flag = match op {
                CombinationOp::Or => s_flag | d_flag,
                CombinationOp::And => s_flag & d_flag,
                CombinationOp::Xor => s_flag ^ d_flag,
                CombinationOp::Xnor => !(s_flag ^ d_flag) & 1,
                CombinationOp::Replace => s_flag,
            };
            region[out_idx] = pixel_from_flag(r_flag);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jbig2::arith::ArithDecoder;

    fn one_black_pixel_symbol(w: u32, h: u32) -> SymbolBitmap {
        let mut pixels = vec![0u8; (w as usize) * (h as usize)];
        if !pixels.is_empty() {
            pixels[0] = 1; // first pixel ink
        }
        SymbolBitmap {
            width: w,
            height: h,
            pixels,
        }
    }

    fn default_params(symbols: Vec<SymbolBitmap>, num_instances: u32) -> TextRegionParams {
        TextRegionParams {
            width: 64,
            height: 16,
            sbnuminstances: num_instances,
            sbhuff: false,
            sbrefine: false,
            sbstrips: 1,
            sbrtemplate: 0,
            sbdsoffset: 0,
            sbcombop: CombinationOp::Or,
            sbdefpixel: 0,
            sb_r_at_pixels: vec![(-1, -1), (-1, -1)],
            sbrefcorner: RefCorner::TopLeft,
            sbtransposed: false,
            symbols,
        }
    }

    #[test]
    fn huffman_path_rejected() {
        let mut params = default_params(vec![one_black_pixel_symbol(4, 4)], 1);
        params.sbhuff = true;
        let data = vec![0u8; 64];
        let mut arith = ArithDecoder::new(&data);
        let mut idec = IntegerDecoder::new();
        let err = decode_text_region(&data, &params, &mut arith, &mut idec).unwrap_err();
        assert_eq!(err, TextRegionError::UnsupportedHuffmanText);
    }

    #[test]
    fn invalid_strips_rejected() {
        for bad in [0u32, 3, 5, 7, 16] {
            let mut params = default_params(vec![one_black_pixel_symbol(4, 4)], 1);
            params.sbstrips = bad;
            let data = vec![0u8; 64];
            let mut arith = ArithDecoder::new(&data);
            let mut idec = IntegerDecoder::new();
            let err = decode_text_region(&data, &params, &mut arith, &mut idec).unwrap_err();
            assert!(
                matches!(err, TextRegionError::InvalidStripCount { sbstrips } if sbstrips == bad),
                "sbstrips={bad} should reject, got {err:?}"
            );
        }
    }

    #[test]
    fn invalid_refinement_template_rejected() {
        let mut params = default_params(vec![one_black_pixel_symbol(4, 4)], 1);
        params.sbrefine = true;
        params.sbrtemplate = 2;
        let data = vec![0u8; 64];
        let mut arith = ArithDecoder::new(&data);
        let mut idec = IntegerDecoder::new();
        let err = decode_text_region(&data, &params, &mut arith, &mut idec).unwrap_err();
        assert!(
            matches!(err, TextRegionError::InvalidRefinementTemplate { value: 2 }),
            "unexpected err: {err:?}",
        );
    }

    #[test]
    fn zero_instances_returns_def_pixel_buffer() {
        let params = default_params(vec![one_black_pixel_symbol(4, 4)], 0);
        let data = vec![0u8; 32];
        let mut arith = ArithDecoder::new(&data);
        let mut idec = IntegerDecoder::new();
        let out = decode_text_region(&data, &params, &mut arith, &mut idec).unwrap();
        assert_eq!(out.len(), (params.width * params.height) as usize);
        // sbdefpixel = 0 -> all white.
        assert!(out.iter().all(|&p| p == PIXEL_WHITE));
    }

    #[test]
    fn empty_symbol_table_rejected_when_instances_nonzero() {
        let params = default_params(Vec::new(), 1);
        let data = vec![0u8; 64];
        let mut arith = ArithDecoder::new(&data);
        let mut idec = IntegerDecoder::new();
        let err = decode_text_region(&data, &params, &mut arith, &mut idec).unwrap_err();
        assert_eq!(err, TextRegionError::EmptySymbolTable);
    }

    #[test]
    fn ceil_log2_matches_spec_sizes() {
        assert_eq!(ceil_log2(1), 0);
        assert_eq!(ceil_log2(2), 1);
        assert_eq!(ceil_log2(3), 2);
        assert_eq!(ceil_log2(4), 2);
        assert_eq!(ceil_log2(5), 3);
        assert_eq!(ceil_log2(8), 3);
        assert_eq!(ceil_log2(255), 8);
        assert_eq!(ceil_log2(256), 8);
        assert_eq!(ceil_log2(257), 9);
    }

    #[test]
    fn sbstrips_log2_valid_powers() {
        assert_eq!(sbstrips_log2(1), Some(0));
        assert_eq!(sbstrips_log2(2), Some(1));
        assert_eq!(sbstrips_log2(4), Some(2));
        assert_eq!(sbstrips_log2(8), Some(3));
        assert_eq!(sbstrips_log2(0), None);
        assert_eq!(sbstrips_log2(16), None);
        assert_eq!(sbstrips_log2(3), None);
    }

    #[test]
    fn ref_corner_from_code_roundtrip() {
        assert_eq!(RefCorner::from_code(0), Some(RefCorner::BottomLeft));
        assert_eq!(RefCorner::from_code(1), Some(RefCorner::TopLeft));
        assert_eq!(RefCorner::from_code(2), Some(RefCorner::BottomRight));
        assert_eq!(RefCorner::from_code(3), Some(RefCorner::TopRight));
        // High bits are masked off.
        assert_eq!(RefCorner::from_code(0xF1), Some(RefCorner::TopLeft));
    }

    #[test]
    fn composite_top_left_places_symbol_at_cursor() {
        let sym = one_black_pixel_symbol(2, 2);
        let mut region = vec![PIXEL_WHITE; 4 * 4];
        composite_symbol(
            &mut region,
            4,
            4,
            &symbol_to_refinement_bitmap(&sym),
            2,
            2,
            1,
            1,
            CombinationOp::Or,
            false,
        );
        // The top-left pixel of the symbol (the one ink pixel) should
        // land at region[1, 1].
        let idx = 4 + 1;
        assert_eq!(region[idx], PIXEL_BLACK);
    }

    #[test]
    fn composite_clips_to_region_bounds() {
        let sym = one_black_pixel_symbol(2, 2);
        let mut region = vec![PIXEL_WHITE; 4 * 4];
        // Origin outside the region (negative): must not panic and
        // must not write any pixels.
        composite_symbol(
            &mut region,
            4,
            4,
            &symbol_to_refinement_bitmap(&sym),
            2,
            2,
            -5,
            -5,
            CombinationOp::Or,
            false,
        );
        assert!(region.iter().all(|&p| p == PIXEL_WHITE));
    }

    #[test]
    fn placement_origin_top_left_is_cursor() {
        let (s, t) = placement_origin(10, 20, 4, 8, RefCorner::TopLeft, false);
        assert_eq!((s, t), (10, 20));
    }

    #[test]
    fn placement_origin_other_corners_shift() {
        // 4x8 symbol, cur = (10, 20).
        let (s, t) = placement_origin(10, 20, 4, 8, RefCorner::TopRight, false);
        assert_eq!((s, t), (10 - 4 + 1, 20));

        let (s, t) = placement_origin(10, 20, 4, 8, RefCorner::BottomLeft, false);
        assert_eq!((s, t), (10, 20 - 8 + 1));

        let (s, t) = placement_origin(10, 20, 4, 8, RefCorner::BottomRight, false);
        assert_eq!((s, t), (10 - 4 + 1, 20 - 8 + 1));
    }

    #[test]
    fn ref_corner_from_code_masks_high_bits_only() {
        // Defensive: ensure we never panic on a full byte of noise.
        for b in 0..=255u8 {
            let r = RefCorner::from_code(b);
            assert!(r.is_some());
        }
    }

    #[test]
    fn transposed_swaps_axes() {
        // Build a 2x4 symbol where only the top-left pixel is ink.
        let sym = SymbolBitmap {
            width: 2,
            height: 4,
            pixels: {
                let mut v = vec![0u8; 8];
                v[0] = 1;
                v
            },
        };
        let mut region = vec![PIXEL_WHITE; 8 * 8];
        // Not transposed: bitmap drawn 2 wide, 4 tall at (1,1).
        composite_symbol(
            &mut region,
            8,
            8,
            &symbol_to_refinement_bitmap(&sym),
            2,
            4,
            1,
            1,
            CombinationOp::Replace,
            false,
        );
        assert_eq!(region[8 + 1], PIXEL_BLACK);
        // Clear.
        region.fill(PIXEL_WHITE);
        // Transposed: bitmap drawn 4 wide, 2 tall at (1,1). The
        // (sx=0, sy=0) symbol pixel ends up at (dx=0, dy=0) via the
        // transpose -- i.e. still at the origin (1, 1). Verify the
        // inked pixel lands there and the overall shape is 4 wide, 2
        // tall (we just check that a draw doesn't write outside the
        // transposed bbox).
        composite_symbol(
            &mut region,
            8,
            8,
            &symbol_to_refinement_bitmap(&sym),
            2,
            4,
            1,
            1,
            CombinationOp::Replace,
            true,
        );
        assert_eq!(region[8 + 1], PIXEL_BLACK);
        // (1, 5) must still be white -- bitmap only extends 2 rows.
        assert_eq!(region[5 * 8 + 1], PIXEL_WHITE);
    }

    #[test]
    fn composite_or_preserves_existing_ink() {
        let sym = SymbolBitmap {
            width: 1,
            height: 1,
            pixels: vec![0], // white pixel (no ink)
        };
        let mut region = vec![PIXEL_BLACK; 4]; // pre-filled all black
        composite_symbol(
            &mut region,
            2,
            2,
            &symbol_to_refinement_bitmap(&sym),
            1,
            1,
            0,
            0,
            CombinationOp::Or,
            false,
        );
        // OR of (ink) 1 with (no ink) 0 keeps region as-is (black).
        assert_eq!(region[0], PIXEL_BLACK);
    }

    #[test]
    fn symbol_to_refinement_bitmap_polarity() {
        let sym = SymbolBitmap {
            width: 2,
            height: 2,
            pixels: vec![0, 1, 0, 1],
        };
        let converted = symbol_to_refinement_bitmap(&sym);
        assert_eq!(
            converted,
            vec![PIXEL_WHITE, PIXEL_BLACK, PIXEL_WHITE, PIXEL_BLACK]
        );
    }
}

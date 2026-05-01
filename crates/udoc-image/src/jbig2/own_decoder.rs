//! JBIG2 own-decoder dispatcher (ISO 14492, issue #158).
//!
//! Walks a merged segment list (globals + per-page), parses each
//! segment's type-specific data, dispatches to the right region
//! decoder, and composites the result into a single page bitmap via
//! the page-information segment's declared dimensions.
//!
//! # Scope
//!
//! Arith path only. Huffman paths (text region, symbol dict) are not
//! supported here and the dispatcher surfaces decoder errors as a
//! whole-page fallback (returning `None`) so the caller can try
//! `hayro_wrapper::decode` as a safety net. The  scope is to
//! close the five fixture gate; Huffman + end-of-stripe handling are
//! post-alpha.
//!
//! # Per-segment data parsers (§7.4.x)
//!
//! Each region segment's data starts with §7.4.1 region-info (17
//! bytes: width, height, x, y, combop) followed by type-specific
//! flags + adaptive-template bytes (where applicable). Symbol and
//! pattern dictionaries use their own 2-byte flags header.

use std::collections::HashMap;

use super::arith::{ArithDecoder, IntegerDecoder};
use super::globals::{build_segment_list, SegmentView};
use super::regions::generic::{decode_generic_region, GenericRegionParams};
use super::regions::halftone::{
    decode_halftone_region, CombinationOp, HalftoneRegionParams, PatternBitmap as HTPattern,
};
use super::regions::pattern_dict::{
    decode_pattern_dict, PatternBitmap as PDPattern, PatternDictParams,
};
use super::regions::refinement::{decode_refinement_region, RefinementRef, RefinementRegionParams};
use super::regions::symbol::{decode_symbol_dict, SymbolBitmap, SymbolDictParams};
use super::regions::text::{decode_text_region, RefCorner, TextRegionParams};
use super::segments::SegmentType;

/// Entry point: decode an entire JBIG2 page stream.
///
/// Returns a row-major byte buffer with `0x00` = black and `0xFF` =
/// white matching the crate-wide convention. Returns `None` on any
/// unrecoverable decode error so the caller can fall back to the
/// upstream hayro wrapper.
pub(crate) fn decode_page(data: &[u8], globals: Option<&[u8]>) -> Option<Vec<u8>> {
    let segments = build_segment_list(globals, data).ok()?;

    // Per-segment caches keyed by segment number.
    let mut sym_dict_cache: HashMap<u32, Vec<SymbolBitmap>> = HashMap::new();
    let mut pattern_dict_cache: HashMap<u32, Vec<PDPattern>> = HashMap::new();
    let mut region_cache: HashMap<u32, RegionBitmap> = HashMap::new();

    let mut page: Option<PageCanvas> = None;

    for seg in &segments {
        match seg.header.segment_type {
            SegmentType::PageInformation => {
                let canvas = parse_page_information(seg.data())?;
                page = Some(canvas);
            }
            SegmentType::SymbolDictionary => {
                let referred_syms =
                    collect_referred_symbols(&seg.header.referred_segments, &sym_dict_cache);
                let new_syms = decode_symbol_dict_segment(seg, referred_syms)
                    .ok()
                    .flatten()?;
                sym_dict_cache.insert(seg.header.segment_number, new_syms);
            }
            SegmentType::PatternDictionary => {
                let pats = decode_pattern_dict_segment(seg).ok().flatten()?;
                pattern_dict_cache.insert(seg.header.segment_number, pats);
            }
            SegmentType::ImmediateGenericRegion
            | SegmentType::ImmediateLosslessGenericRegion
            | SegmentType::IntermediateGenericRegion => {
                let bitmap = decode_generic_region_segment(seg).ok().flatten()?;
                if matches!(
                    seg.header.segment_type,
                    SegmentType::IntermediateGenericRegion
                ) {
                    region_cache.insert(seg.header.segment_number, bitmap);
                } else if let Some(pc) = page.as_mut() {
                    pc.composite(&bitmap);
                }
            }
            SegmentType::ImmediateGenericRefinementRegion
            | SegmentType::ImmediateLosslessGenericRefinementRegion
            | SegmentType::IntermediateGenericRefinementRegion => {
                let bitmap = decode_refinement_region_segment(seg, &region_cache)
                    .ok()
                    .flatten()?;
                if matches!(
                    seg.header.segment_type,
                    SegmentType::IntermediateGenericRefinementRegion
                ) {
                    region_cache.insert(seg.header.segment_number, bitmap);
                } else if let Some(pc) = page.as_mut() {
                    pc.composite(&bitmap);
                }
            }
            SegmentType::ImmediateTextRegion
            | SegmentType::ImmediateLosslessTextRegion
            | SegmentType::IntermediateTextRegion => {
                let referred_syms =
                    collect_referred_symbols(&seg.header.referred_segments, &sym_dict_cache);
                let bitmap = decode_text_region_segment(seg, referred_syms)
                    .ok()
                    .flatten()?;
                if matches!(seg.header.segment_type, SegmentType::IntermediateTextRegion) {
                    region_cache.insert(seg.header.segment_number, bitmap);
                } else if let Some(pc) = page.as_mut() {
                    pc.composite(&bitmap);
                }
            }
            SegmentType::ImmediateHalftoneRegion
            | SegmentType::ImmediateLosslessHalftoneRegion
            | SegmentType::IntermediateHalftoneRegion => {
                let bitmap = decode_halftone_region_segment(seg, &pattern_dict_cache)
                    .ok()
                    .flatten()?;
                if matches!(
                    seg.header.segment_type,
                    SegmentType::IntermediateHalftoneRegion
                ) {
                    region_cache.insert(seg.header.segment_number, bitmap);
                } else if let Some(pc) = page.as_mut() {
                    pc.composite(&bitmap);
                }
            }
            SegmentType::EndOfPage
            | SegmentType::EndOfStripe
            | SegmentType::EndOfFile
            | SegmentType::Profiles
            | SegmentType::Tables
            | SegmentType::Reserved(_) => {
                // No-op for our purposes. Profiles/Tables carry
                // encoding metadata we don't read.
            }
        }
    }

    page.map(|p| p.finish())
}

// ---------------------------------------------------------------------------
// Page canvas
// ---------------------------------------------------------------------------

/// Dimensions + pre-filled buffer for the final page bitmap.
struct PageCanvas {
    width: u32,
    height: u32,
    combine_default: CombinationOp,
    /// Row-major, 0x00 = black, 0xFF = white.
    pixels: Vec<u8>,
}

impl PageCanvas {
    fn composite(&mut self, region: &RegionBitmap) {
        composite_into(
            &mut self.pixels,
            self.width,
            self.height,
            &region.pixels,
            region.width,
            region.height,
            region.x,
            region.y,
            region.combop.unwrap_or(self.combine_default),
        );
    }

    fn finish(self) -> Vec<u8> {
        self.pixels
    }
}

/// Parse a page-information segment's data (§7.4.8, 19 bytes).
fn parse_page_information(data: &[u8]) -> Option<PageCanvas> {
    if data.len() < 19 {
        return None;
    }
    let width = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    let height = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    // Skip xres/yres (bytes 8..16).
    let flags = data[16];
    let default_pixel = (flags >> 2) & 0x1;
    let combop_code = (flags >> 3) & 0x3;
    let combine_default = CombinationOp::from_code(combop_code).unwrap_or(CombinationOp::Or);

    // Height of 0xFFFF_FFFF means "unknown" -- fall back to a guess
    // from the striping info. We reject it here because the
    // fixture set all have known heights.
    if width == 0 || height == 0 || height == u32::MAX {
        return None;
    }
    let len = (width as usize).checked_mul(height as usize)?;
    let fill = if default_pixel == 1 { 0x00u8 } else { 0xFFu8 };
    Some(PageCanvas {
        width,
        height,
        combine_default,
        pixels: vec![fill; len],
    })
}

// ---------------------------------------------------------------------------
// Region bitmap (intermediate)
// ---------------------------------------------------------------------------

/// Intermediate region result for caching + compositing.
#[derive(Debug, Clone)]
struct RegionBitmap {
    width: u32,
    height: u32,
    /// X offset within the page.
    x: i32,
    /// Y offset within the page.
    y: i32,
    /// Per-region combop override; `None` = use page default.
    combop: Option<CombinationOp>,
    /// Row-major, 0x00 = black, 0xFF = white.
    pixels: Vec<u8>,
}

/// Parse the §7.4.1 region-segment information field: 17 bytes.
///
/// Returns `(width, height, x, y, combop)`.
fn parse_region_info(data: &[u8]) -> Option<(u32, u32, u32, u32, CombinationOp)> {
    if data.len() < 17 {
        return None;
    }
    let w = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    let h = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    let x = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
    let y = u32::from_be_bytes([data[12], data[13], data[14], data[15]]);
    // Low 3 bits of flags byte: external combination operator.
    let combop = CombinationOp::from_code(data[16] & 0x7).unwrap_or(CombinationOp::Or);
    Some((w, h, x, y, combop))
}

// ---------------------------------------------------------------------------
// Generic region segment decoder (§7.4.6)
// ---------------------------------------------------------------------------

fn decode_generic_region_segment(seg: &SegmentView<'_>) -> Result<Option<RegionBitmap>, ()> {
    let data = seg.data();
    let (width, height, x, y, combop) = parse_region_info(data).ok_or(())?;

    let flags_off = 17;
    if data.len() < flags_off + 1 {
        return Ok(None);
    }
    let flags = data[flags_off];
    let mmr = (flags & 0x01) != 0;
    let template = (flags >> 1) & 0x3;
    let tpgdon = (flags & 0x08) != 0;

    let mut off = flags_off + 1;

    // AT pixel bytes for non-MMR.
    let at_pixels = if !mmr {
        let n_at = if template == 0 { 4 } else { 1 };
        if data.len() < off + n_at * 2 {
            return Ok(None);
        }
        let mut at = Vec::with_capacity(n_at);
        for _ in 0..n_at {
            let ax = data[off] as i8;
            let ay = data[off + 1] as i8;
            at.push((ax, ay));
            off += 2;
        }
        at
    } else {
        Vec::new()
    };

    let arith_payload = &data[off..];

    let params = GenericRegionParams {
        width,
        height,
        gbtemplate: template,
        tpgdon,
        mmr,
        at_pixels,
    };

    let result = if mmr {
        decode_generic_region(arith_payload, &params, None)
    } else {
        let mut arith = ArithDecoder::new(arith_payload);
        decode_generic_region(arith_payload, &params, Some(&mut arith))
    };

    match result {
        Ok(pixels) => Ok(Some(RegionBitmap {
            width,
            height,
            x: x as i32,
            y: y as i32,
            combop: Some(combop),
            pixels,
        })),
        Err(_) => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Refinement region segment decoder (§7.4.7)
// ---------------------------------------------------------------------------

fn decode_refinement_region_segment(
    seg: &SegmentView<'_>,
    region_cache: &HashMap<u32, RegionBitmap>,
) -> Result<Option<RegionBitmap>, ()> {
    let data = seg.data();
    let (width, height, x, y, combop) = parse_region_info(data).ok_or(())?;

    let flags_off = 17;
    if data.len() < flags_off + 1 {
        return Ok(None);
    }
    let flags = data[flags_off];
    let grtemplate = flags & 0x1;
    let tpgron = (flags & 0x2) != 0;

    let mut off = flags_off + 1;
    // AT pixels: 2 pairs for grtemplate == 0.
    let at_pixels = if grtemplate == 0 {
        if data.len() < off + 4 {
            return Ok(None);
        }
        let at = vec![
            (data[off] as i8, data[off + 1] as i8),
            (data[off + 2] as i8, data[off + 3] as i8),
        ];
        off += 4;
        at
    } else {
        Vec::new()
    };

    // Reference bitmap: use the first referred-segment's bitmap.
    let ref_seg = seg.header.referred_segments.first().copied();
    let ref_bitmap = ref_seg.and_then(|n| region_cache.get(&n)).ok_or(())?;

    let arith_payload = &data[off..];
    let mut arith = ArithDecoder::new(arith_payload);
    let result = decode_refinement_region(
        arith_payload,
        &RefinementRegionParams {
            width,
            height,
            grtemplate,
            gr_at_pixels: at_pixels,
            tpgron,
            reference: RefinementRef {
                bitmap: &ref_bitmap.pixels,
                width: ref_bitmap.width,
                height: ref_bitmap.height,
                dx: 0,
                dy: 0,
            },
        },
        &mut arith,
    );
    match result {
        Ok(pixels) => Ok(Some(RegionBitmap {
            width,
            height,
            x: x as i32,
            y: y as i32,
            combop: Some(combop),
            pixels,
        })),
        Err(_) => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Symbol dict segment decoder (§7.4.2)
// ---------------------------------------------------------------------------

fn decode_symbol_dict_segment(
    seg: &SegmentView<'_>,
    referred: Vec<SymbolBitmap>,
) -> Result<Option<Vec<SymbolBitmap>>, ()> {
    let data = seg.data();
    if data.len() < 2 {
        return Ok(None);
    }
    let flags = u16::from_be_bytes([data[0], data[1]]);
    let sd_huff = (flags & 0x01) != 0;
    let sd_refagg = (flags & 0x02) != 0;
    let sd_templates = ((flags >> 10) & 0x3) as u8;
    let sd_refinement = ((flags >> 12) & 0x1) as u8;

    if sd_huff {
        // Huffman-coded symbol dicts are post-alpha per #158 scope.
        return Ok(None);
    }

    let mut off = 2usize;
    // SDAT: adaptive-template pixels, 4 pairs for template 0, 1 pair
    // for templates 1..=3.
    let n_sdat = if sd_templates == 0 { 4 } else { 1 };
    if data.len() < off + n_sdat * 2 {
        return Ok(None);
    }
    let mut sd_at = Vec::with_capacity(n_sdat);
    for _ in 0..n_sdat {
        sd_at.push((data[off] as i8, data[off + 1] as i8));
        off += 2;
    }
    // SDRAT: refinement adaptive template, 2 pairs when SDREFAGG=1.
    let sd_rat = if sd_refagg && sd_refinement == 0 {
        if data.len() < off + 4 {
            return Ok(None);
        }
        let rat = vec![
            (data[off] as i8, data[off + 1] as i8),
            (data[off + 2] as i8, data[off + 3] as i8),
        ];
        off += 4;
        rat
    } else {
        Vec::new()
    };

    if data.len() < off + 8 {
        return Ok(None);
    }
    let num_ex_syms = u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
    let num_new_syms =
        u32::from_be_bytes([data[off + 4], data[off + 5], data[off + 6], data[off + 7]]);
    off += 8;

    let arith_payload = &data[off..];
    let params = SymbolDictParams {
        sd_huff,
        sd_refagg,
        sd_templates,
        sd_refinement,
        sd_at_pixels: sd_at,
        sd_r_at_pixels: sd_rat,
        num_ex_syms,
        num_new_syms,
        referred_symbols: referred,
    };
    let mut arith = ArithDecoder::new(arith_payload);
    let mut idec = IntegerDecoder::new();
    match decode_symbol_dict(arith_payload, &params, &mut arith, &mut idec) {
        Ok(syms) => Ok(Some(syms)),
        Err(_) => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Pattern dict segment decoder (§7.4.4)
// ---------------------------------------------------------------------------

fn decode_pattern_dict_segment(seg: &SegmentView<'_>) -> Result<Option<Vec<PDPattern>>, ()> {
    let data = seg.data();
    if data.len() < 1 + 1 + 1 + 4 {
        return Ok(None);
    }
    let flags = data[0];
    let mmr = (flags & 0x01) != 0;
    let template = (flags >> 1) & 0x3;
    let hd_pw = data[1] as u32;
    let hd_ph = data[2] as u32;
    let gray_max = u32::from_be_bytes([data[3], data[4], data[5], data[6]]);
    let num_patterns = gray_max.checked_add(1).ok_or(())?;

    let payload = &data[7..];
    let params = PatternDictParams {
        width: hd_pw,
        height: hd_ph,
        num_patterns,
        template,
    };

    let mut arith = ArithDecoder::new(payload);
    // decode_pattern_dict accepts a caller-supplied closure for the
    // internal generic region decode. Use MMR or arith per the flag.
    let patterns_result =
        decode_pattern_dict(payload, &params, &mut arith, |w, h, tpl, arith_inner| {
            // Default AT pixels for pattern-dict generic region per
            // §6.7.5 step 3: AT1 = (-HDPW, 0), others default.
            let at_pixels = if tpl == 0 {
                vec![(-(hd_pw as i8), 0), (-3, -1), (2, -2), (-2, -2)]
            } else {
                vec![(-(hd_pw as i8), 0)]
            };
            let gp = GenericRegionParams {
                width: w,
                height: h,
                gbtemplate: tpl,
                tpgdon: false,
                mmr,
                at_pixels,
            };
            if mmr {
                decode_generic_region(payload, &gp, None).ok()
            } else {
                decode_generic_region(payload, &gp, Some(arith_inner)).ok()
            }
        });
    match patterns_result {
        Ok(patterns) => Ok(Some(patterns)),
        Err(_) => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Text region segment decoder (§7.4.3)
// ---------------------------------------------------------------------------

fn decode_text_region_segment(
    seg: &SegmentView<'_>,
    referred_syms: Vec<SymbolBitmap>,
) -> Result<Option<RegionBitmap>, ()> {
    let data = seg.data();
    let (width, height, x, y, combop) = parse_region_info(data).ok_or(())?;

    let mut off = 17usize;
    if data.len() < off + 2 {
        return Ok(None);
    }
    let flags = u16::from_be_bytes([data[off], data[off + 1]]);
    off += 2;
    let sb_huff = (flags & 0x01) != 0;
    let sb_refine = (flags & 0x02) != 0;
    let sb_log_strips = ((flags >> 2) & 0x3) as u32;
    let sb_strips = 1u32 << sb_log_strips;
    let sb_refcorner_code = ((flags >> 4) & 0x3) as u8;
    let sb_transposed = (flags & 0x0040) != 0;
    let sb_combop_code = ((flags >> 7) & 0x3) as u8;
    let sb_defpixel = ((flags >> 9) & 0x1) as u8;
    // DsOffset: 5 bits sign-extended.
    let raw_dsoff = ((flags >> 10) & 0x1F) as i32;
    let sb_dsoffset = if raw_dsoff >= 0x10 {
        raw_dsoff - 0x20
    } else {
        raw_dsoff
    };
    let sb_rtemplate = ((flags >> 15) & 0x1) as u8;

    if sb_huff {
        // Huffman tables follow a second 2-byte flags field. Not
        // supported in this decoder; let caller fall back.
        return Ok(None);
    }

    // SBRAT: 4 bytes when SBREFINE=1 and SBRTEMPLATE=0.
    let sb_rat = if sb_refine && sb_rtemplate == 0 {
        if data.len() < off + 4 {
            return Ok(None);
        }
        let rat = vec![
            (data[off] as i8, data[off + 1] as i8),
            (data[off + 2] as i8, data[off + 3] as i8),
        ];
        off += 4;
        rat
    } else {
        Vec::new()
    };

    if data.len() < off + 4 {
        return Ok(None);
    }
    let sb_numinstances =
        u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
    off += 4;

    // Skip Huffman-table state (not present in arith path).
    let arith_payload = &data[off..];
    let sb_combop = CombinationOp::from_code(sb_combop_code).unwrap_or(CombinationOp::Or);
    let sb_refcorner = RefCorner::from_code(sb_refcorner_code).unwrap_or(RefCorner::TopLeft);

    let params = TextRegionParams {
        width,
        height,
        sbnuminstances: sb_numinstances,
        sbhuff: sb_huff,
        sbrefine: sb_refine,
        sbstrips: sb_strips,
        sbrtemplate: sb_rtemplate,
        sbdsoffset: sb_dsoffset,
        sbcombop: sb_combop,
        sbdefpixel: sb_defpixel,
        sb_r_at_pixels: sb_rat,
        sbrefcorner: sb_refcorner,
        sbtransposed: sb_transposed,
        symbols: referred_syms,
    };
    let mut arith = ArithDecoder::new(arith_payload);
    let mut idec = IntegerDecoder::new();
    match decode_text_region(arith_payload, &params, &mut arith, &mut idec) {
        Ok(pixels) => Ok(Some(RegionBitmap {
            width,
            height,
            x: x as i32,
            y: y as i32,
            combop: Some(combop),
            pixels,
        })),
        Err(_) => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Halftone region segment decoder (§7.4.5)
// ---------------------------------------------------------------------------

fn decode_halftone_region_segment(
    seg: &SegmentView<'_>,
    pattern_cache: &HashMap<u32, Vec<PDPattern>>,
) -> Result<Option<RegionBitmap>, ()> {
    let data = seg.data();
    let (width, height, x, y, combop) = parse_region_info(data).ok_or(())?;

    let mut off = 17usize;
    if data.len() < off + 1 {
        return Ok(None);
    }
    let flags = data[off];
    off += 1;
    let hmmr = (flags & 0x01) != 0;
    let htemplate = (flags >> 1) & 0x3;
    let henableskip = (flags & 0x08) != 0;
    let hcombop_code = (flags >> 4) & 0x7;
    let hdefpixel = (flags >> 7) & 0x1;

    if data.len() < off + 16 {
        return Ok(None);
    }
    let hgw = u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
    let hgh = u32::from_be_bytes([data[off + 4], data[off + 5], data[off + 6], data[off + 7]]);
    let hgx = i32::from_be_bytes([data[off + 8], data[off + 9], data[off + 10], data[off + 11]]);
    let hgy = i32::from_be_bytes([
        data[off + 12],
        data[off + 13],
        data[off + 14],
        data[off + 15],
    ]);
    off += 16;
    if data.len() < off + 4 {
        return Ok(None);
    }
    let hrx = u16::from_be_bytes([data[off], data[off + 1]]) as i32;
    let hry = u16::from_be_bytes([data[off + 2], data[off + 3]]) as i32;
    off += 4;

    // Pattern dict from first referred segment.
    let pat_seg = seg.header.referred_segments.first().copied();
    let patterns = pat_seg.and_then(|n| pattern_cache.get(&n)).ok_or(())?;
    // Convert to halftone's local PatternBitmap type (same shape).
    let ht_patterns: Vec<HTPattern> = patterns
        .iter()
        .map(|p| HTPattern {
            width: p.width,
            height: p.height,
            pixels: p
                .pixels
                .iter()
                .map(|&b| if b == 0x00 { 1 } else { 0 })
                .collect(),
        })
        .collect();

    // HBPP: ceil(log2(num_patterns)). Derive from the pattern-dict
    // cache directly.
    let n_pats = patterns.len() as u64;
    let hbpp = if n_pats <= 1 {
        1
    } else {
        64 - (n_pats - 1).leading_zeros()
    };

    let arith_payload = &data[off..];
    let params = HalftoneRegionParams {
        width,
        height,
        pattern_dict: ht_patterns,
        hgw,
        hgh,
        hgx,
        hgy,
        hrx,
        hry,
        hbpp,
        hmmr,
        htemplate,
        henableskip,
        hcombop: CombinationOp::from_code(hcombop_code).unwrap_or(CombinationOp::Or),
        hdefpixel,
    };
    let mut arith = ArithDecoder::new(arith_payload);
    match decode_halftone_region(arith_payload, &params, &mut arith, None) {
        Ok(pixels) => {
            // Halftone emits 0/1 (paper/ink). Up-sample to the
            // crate-wide 0xFF/0x00 convention before caching.
            let pixels = pixels
                .into_iter()
                .map(|p| if p & 1 == 1 { 0x00 } else { 0xFF })
                .collect();
            Ok(Some(RegionBitmap {
                width,
                height,
                x: x as i32,
                y: y as i32,
                combop: Some(combop),
                pixels,
            }))
        }
        Err(_) => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Referred-symbol resolution
// ---------------------------------------------------------------------------

fn collect_referred_symbols(
    refs: &[u32],
    sym_cache: &HashMap<u32, Vec<SymbolBitmap>>,
) -> Vec<SymbolBitmap> {
    let mut out = Vec::new();
    for &n in refs {
        if let Some(syms) = sym_cache.get(&n) {
            out.extend(syms.iter().cloned());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Generic compositor
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn composite_into(
    dst: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    src: &[u8],
    src_w: u32,
    src_h: u32,
    x: i32,
    y: i32,
    op: CombinationOp,
) {
    for dy in 0..src_h as i64 {
        let ty = y as i64 + dy;
        if ty < 0 || ty >= dst_h as i64 {
            continue;
        }
        for dx in 0..src_w as i64 {
            let tx = x as i64 + dx;
            if tx < 0 || tx >= dst_w as i64 {
                continue;
            }
            let s_pix = src[(dy as usize) * (src_w as usize) + (dx as usize)];
            let d_idx = (ty as usize) * (dst_w as usize) + (tx as usize);
            let d_pix = dst[d_idx];
            // Normalize to 0/1 ink flag (1 = black/ink).
            let s = if s_pix == 0xFF { 0u8 } else { 1u8 };
            let d = if d_pix == 0xFF { 0u8 } else { 1u8 };
            let r = match op {
                CombinationOp::Or => s | d,
                CombinationOp::And => s & d,
                CombinationOp::Xor => s ^ d,
                CombinationOp::Xnor => !(s ^ d) & 1,
                CombinationOp::Replace => s,
            };
            dst[d_idx] = if r == 1 { 0x00 } else { 0xFF };
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_page_information_rejects_too_short() {
        assert!(parse_page_information(&[0u8; 10]).is_none());
    }

    #[test]
    fn parse_page_information_rejects_zero_dims() {
        let mut data = [0u8; 19];
        data[0..4].copy_from_slice(&0u32.to_be_bytes()); // width = 0
        data[4..8].copy_from_slice(&100u32.to_be_bytes());
        assert!(parse_page_information(&data).is_none());
    }

    #[test]
    fn parse_page_information_accepts_valid_header() {
        let mut data = [0u8; 19];
        data[0..4].copy_from_slice(&16u32.to_be_bytes());
        data[4..8].copy_from_slice(&8u32.to_be_bytes());
        data[16] = 0; // default_pixel=0, combop=Or
        let canvas = parse_page_information(&data).unwrap();
        assert_eq!(canvas.width, 16);
        assert_eq!(canvas.height, 8);
        assert_eq!(canvas.pixels.len(), 128);
        assert!(canvas.pixels.iter().all(|&p| p == 0xFF));
    }

    #[test]
    fn parse_region_info_rejects_short_slice() {
        assert!(parse_region_info(&[0u8; 10]).is_none());
    }

    #[test]
    fn parse_region_info_roundtrip() {
        let mut data = [0u8; 17];
        data[0..4].copy_from_slice(&100u32.to_be_bytes());
        data[4..8].copy_from_slice(&50u32.to_be_bytes());
        data[8..12].copy_from_slice(&5u32.to_be_bytes());
        data[12..16].copy_from_slice(&7u32.to_be_bytes());
        data[16] = 2; // XOR
        let (w, h, x, y, op) = parse_region_info(&data).unwrap();
        assert_eq!((w, h, x, y), (100, 50, 5, 7));
        assert_eq!(op, CombinationOp::Xor);
    }

    #[test]
    fn composite_into_or_overwrites_on_ink() {
        let mut dst = vec![0xFFu8; 16]; // 4x4 white
        let src = vec![0x00u8; 4]; // 2x2 all black
        composite_into(&mut dst, 4, 4, &src, 2, 2, 1, 1, CombinationOp::Or);
        // Inked area at (1,1) to (2,2).
        assert_eq!(dst[5], 0x00);
        assert_eq!(dst[6], 0x00);
        assert_eq!(dst[9], 0x00);
        assert_eq!(dst[10], 0x00);
        // Outside stays white.
        assert_eq!(dst[0], 0xFF);
    }

    #[test]
    fn composite_into_clips_negative_coords() {
        let mut dst = vec![0xFFu8; 16];
        let src = vec![0x00u8; 4];
        composite_into(&mut dst, 4, 4, &src, 2, 2, -5, -5, CombinationOp::Or);
        assert!(dst.iter().all(|&p| p == 0xFF));
    }
}

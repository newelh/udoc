//! JBIG2 pattern dictionary decoder (ISO 14492 §6.7).
//!
//! A pattern dictionary is a sequence of small `HDPW × HDPH` bitmap
//! patterns used as the palette for halftone regions (§6.6). Encoding
//! is exactly one generic region (§6.2) of size
//! `(num_patterns × HDPW) × HDPH` whose pixels are the patterns
//! concatenated left-to-right; the decoder splits the single decoded
//! region back into `num_patterns` individual pattern bitmaps.
//!
//! Port target: pdfium `third_party/jbig2/JBig2_PDDProc.cpp`.
//! Cross-validated against `hayro_jbig2 0.3.0` `src/decode/pattern.rs`.
//!
//! This module does not include the generic region decoder itself; it
//! accepts the *already-decoded* collective bitmap via
//! [`split_patterns`] and exposes [`decode_pattern_dict`] as a thin
//! wrapper that defers the generic decode to a caller-supplied
//! closure. This keeps the module decoupled from the concrete
//! generic-region implementation living in `regions/generic.rs`
//! (T2-GENERIC,  Wave 2 peer task).

use crate::jbig2::arith::ArithDecoder;

/// A decoded pattern bitmap (one entry in a pattern dictionary).
///
/// Storage is row-major, 1 byte per pixel: `0x00` = black, `0xFF` =
/// white (matches the `udoc-image` crate-wide convention).
///
/// (the peer halftone-region task) will consume this type
/// when blitting patterns into the halftone output bitmap. The type is
/// defined here because the pattern dictionary is the producer. If
/// needs additional fields (e.g. stride, shifted
/// pre-renderings), they should be added here, not duplicated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternBitmap {
    /// Pattern width in pixels (`HDPW`).
    pub width: u32,
    /// Pattern height in pixels (`HDPH`).
    pub height: u32,
    /// `width * height` pixels, row-major.
    pub pixels: Vec<u8>,
}

/// Parameters for a pattern dictionary decode call.
pub struct PatternDictParams {
    /// `HDPW`: width of each pattern in pixels. Must be > 0.
    pub width: u32,
    /// `HDPH`: height of each pattern in pixels. Must be > 0.
    pub height: u32,
    /// Total number of patterns to produce. 5 this is
    /// `GRAYMAX + 1` (the pattern dictionary segment header stores
    /// `GRAYMAX`). Must be > 0.
    pub num_patterns: u32,
    /// GBTEMPLATE to use for the internal generic region decode. Per
    /// §7.4.4 pattern dictionaries cap the template at 3 (two-bit
    /// field in the segment header).
    pub template: u8,
}

/// Error cases the pattern-dict decoder can surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternDictError {
    /// `width`, `height`, or `num_patterns` is zero.
    ZeroDimension,
    /// `template` not in `{0, 1, 2, 3}`.
    InvalidTemplate(u8),
    /// `width * num_patterns` overflows `u32`, or the collective bitmap
    /// byte-size overflows `usize`.
    DimensionOverflow,
    /// The caller-supplied generic decode returned a bitmap whose size
    /// did not match `(width * num_patterns, height)`.
    BadCollectiveSize {
        /// Expected total pixel count.
        expected: usize,
        /// Actual length the caller returned.
        got: usize,
    },
    /// The caller-supplied generic decode failed.
    GenericDecodeFailed,
}

impl std::fmt::Display for PatternDictError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroDimension => f.write_str("pattern dict dimensions must be non-zero"),
            Self::InvalidTemplate(t) => write!(f, "invalid GBTEMPLATE for pattern dict: {}", t),
            Self::DimensionOverflow => f.write_str("pattern dict dimensions overflow"),
            Self::BadCollectiveSize { expected, got } => write!(
                f,
                "collective bitmap has {} pixels, expected {}",
                got, expected
            ),
            Self::GenericDecodeFailed => f.write_str("inner generic decode failed"),
        }
    }
}

impl std::error::Error for PatternDictError {}

/// Split a collective bitmap of size `(num_patterns * width, height)`
/// into `num_patterns` individual pattern bitmaps of size
/// `(width, height)` each.
///
/// 5 step 4: pattern `k` is the sub-image of the collective
/// bitmap occupying columns `k*width.(k+1)*width` and all rows.
///
/// `collective` must be row-major, 1 byte per pixel, length =
/// `num_patterns * width * height`.
pub fn split_patterns(
    collective: &[u8],
    params: &PatternDictParams,
) -> Result<Vec<PatternBitmap>, PatternDictError> {
    validate(params)?;

    let total_w = params
        .width
        .checked_mul(params.num_patterns)
        .ok_or(PatternDictError::DimensionOverflow)?;
    let expected = (total_w as usize)
        .checked_mul(params.height as usize)
        .ok_or(PatternDictError::DimensionOverflow)?;
    if collective.len() != expected {
        return Err(PatternDictError::BadCollectiveSize {
            expected,
            got: collective.len(),
        });
    }

    let pw = params.width as usize;
    let ph = params.height as usize;
    let stride = total_w as usize;
    let mut out = Vec::with_capacity(params.num_patterns as usize);
    for k in 0..params.num_patterns as usize {
        let start_col = k * pw;
        let mut pixels = Vec::with_capacity(pw * ph);
        for y in 0..ph {
            let row_start = y * stride + start_col;
            pixels.extend_from_slice(&collective[row_start..row_start + pw]);
        }
        out.push(PatternBitmap {
            width: params.width,
            height: params.height,
            pixels,
        });
    }
    Ok(out)
}

/// Decode a pattern dictionary end-to-end. The collective bitmap is
/// produced by `decode_collective`, which the caller supplies so
/// that this module does not have a hard dependency on the concrete
/// generic-region decoder (it lives in a peer module,).
///
/// `decode_collective` receives `(width, height, template, arith)` and
/// must return a row-major `width * height` pixel buffer.
pub fn decode_pattern_dict<F>(
    _data: &[u8],
    params: &PatternDictParams,
    arith: &mut ArithDecoder<'_>,
    mut decode_collective: F,
) -> Result<Vec<PatternBitmap>, PatternDictError>
where
    F: FnMut(u32, u32, u8, &mut ArithDecoder<'_>) -> Option<Vec<u8>>,
{
    validate(params)?;
    let total_w = params
        .width
        .checked_mul(params.num_patterns)
        .ok_or(PatternDictError::DimensionOverflow)?;
    let collective = decode_collective(total_w, params.height, params.template, arith)
        .ok_or(PatternDictError::GenericDecodeFailed)?;
    split_patterns(&collective, params)
}

fn validate(params: &PatternDictParams) -> Result<(), PatternDictError> {
    if params.width == 0 || params.height == 0 || params.num_patterns == 0 {
        return Err(PatternDictError::ZeroDimension);
    }
    if params.template > 3 {
        return Err(PatternDictError::InvalidTemplate(params.template));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const W: u8 = 0xFF;
    const B: u8 = 0x00;

    #[test]
    fn split_patterns_produces_expected_count_and_dims() {
        // 4 patterns of 2x2, laid out left-to-right as an 8x2 collective.
        // Collective (rows):
        //   row 0: [B B] [W W] [B W] [W B]
        //   row 1: [W W] [B B] [B W] [W B]
        let collective = vec![
            B, B, W, W, B, W, W, B, // row 0
            W, W, B, B, B, W, W, B, // row 1
        ];
        let params = PatternDictParams {
            width: 2,
            height: 2,
            num_patterns: 4,
            template: 0,
        };
        let patterns = split_patterns(&collective, &params).unwrap();
        assert_eq!(patterns.len(), 4);
        for p in &patterns {
            assert_eq!(p.width, 2);
            assert_eq!(p.height, 2);
            assert_eq!(p.pixels.len(), 4);
        }
        assert_eq!(patterns[0].pixels, vec![B, B, W, W]);
        assert_eq!(patterns[1].pixels, vec![W, W, B, B]);
        assert_eq!(patterns[2].pixels, vec![B, W, B, W]);
        assert_eq!(patterns[3].pixels, vec![W, B, W, B]);
    }

    #[test]
    fn split_patterns_rejects_wrong_collective_size() {
        let collective = vec![B; 10];
        let params = PatternDictParams {
            width: 2,
            height: 2,
            num_patterns: 4,
            template: 0,
        };
        let err = split_patterns(&collective, &params).unwrap_err();
        assert_eq!(
            err,
            PatternDictError::BadCollectiveSize {
                expected: 16,
                got: 10
            }
        );
    }

    #[test]
    fn split_patterns_rejects_zero_dim() {
        let collective = vec![B; 0];
        let params = PatternDictParams {
            width: 0,
            height: 2,
            num_patterns: 4,
            template: 0,
        };
        let err = split_patterns(&collective, &params).unwrap_err();
        assert_eq!(err, PatternDictError::ZeroDimension);
    }

    #[test]
    fn split_patterns_rejects_invalid_template() {
        let collective = vec![B; 16];
        let params = PatternDictParams {
            width: 2,
            height: 2,
            num_patterns: 4,
            template: 4,
        };
        let err = split_patterns(&collective, &params).unwrap_err();
        assert_eq!(err, PatternDictError::InvalidTemplate(4));
    }

    #[test]
    fn synthesized_4_pattern_dict_via_closure_decoder() {
        // Simulate a run by supplying a closure that
        // returns a known 4-pattern collective bitmap. This matches
        // the ISO 14492 Annex H spec-byte fixture strategy: the
        // pattern dictionary bytes themselves aren't in our corpus,
        // so we test the split-and-bookkeeping layer against synthetic
        // pixel data.
        let collective = vec![
            B, B, W, W, B, W, W, B, // row 0
            B, B, W, W, B, W, W, B, // row 1
            W, W, B, B, B, W, W, B, // row 2
            W, W, B, B, B, W, W, B, // row 3
        ];
        let collective_clone = collective.clone();
        let params = PatternDictParams {
            width: 2,
            height: 4,
            num_patterns: 4,
            template: 0,
        };
        let stream = [0u8; 4];
        let mut arith = ArithDecoder::new(&stream);
        let patterns = decode_pattern_dict(&[], &params, &mut arith, |w, h, _t, _a| {
            assert_eq!(w, 8);
            assert_eq!(h, 4);
            Some(collective_clone.clone())
        })
        .unwrap();
        assert_eq!(patterns.len(), 4);
        assert_eq!(patterns[0].pixels, vec![B, B, B, B, W, W, W, W]);
        assert_eq!(patterns[1].pixels, vec![W, W, W, W, B, B, B, B]);
        assert_eq!(patterns[2].pixels, vec![B, W, B, W, B, W, B, W]);
        assert_eq!(patterns[3].pixels, vec![W, B, W, B, W, B, W, B]);
    }

    #[test]
    fn decode_pattern_dict_propagates_generic_failure() {
        let params = PatternDictParams {
            width: 2,
            height: 2,
            num_patterns: 4,
            template: 0,
        };
        let stream = [0u8; 4];
        let mut arith = ArithDecoder::new(&stream);
        let err = decode_pattern_dict(&[], &params, &mut arith, |_, _, _, _| None).unwrap_err();
        assert_eq!(err, PatternDictError::GenericDecodeFailed);
    }

    #[test]
    fn dimension_overflow_rejected() {
        let params = PatternDictParams {
            width: u32::MAX,
            height: 1,
            num_patterns: 2,
            template: 0,
        };
        let stream = [0u8; 4];
        let mut arith = ArithDecoder::new(&stream);
        let err =
            decode_pattern_dict(&[], &params, &mut arith, |_, _, _, _| Some(vec![])).unwrap_err();
        assert_eq!(err, PatternDictError::DimensionOverflow);
    }

    #[test]
    fn single_pattern_dict_decodes() {
        // GRAYMAX = 0 => num_patterns = 1 (edge case for 1-bpp halftone).
        let collective = vec![B, W, W, B];
        let params = PatternDictParams {
            width: 2,
            height: 2,
            num_patterns: 1,
            template: 0,
        };
        let patterns = split_patterns(&collective, &params).unwrap();
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].pixels, vec![B, W, W, B]);
    }
}

//! JBIG2 decoder.
//!
//! # Status ( close)
//!
//! The own decoder is the only dispatch path. `hayro_jbig2` was dropped
//! from the tree at ( extension) after the own decoder
//! covered all 7 ISO 14492 components landed. Streams whose
//! decoder-support is not yet covered end-to-end (Huffman text regions,
//! Huffman symbol dicts, full SDREFAGG aggregation) now return `None`
//! which surfaces as an `UnsupportedFilter` warning with the raw JBIG2
//! bytes passed through, identical to the `#[cfg(not(feature = "jbig2"))]`
//! path. Callers that need a best-effort decode for those corner cases
//! can self-host a hayro shim against the public
//! [`crate::decode_jbig2`] interface.
//!
//! Known own-decoder gaps (surface as `None` -> passthrough warning):
//! - SBHUFF=1 text regions (§6.4 Huffman path) -- post-alpha.
//! - SDHUFF=1 symbol dicts (§6.5 Huffman path) -- post-alpha.
//! - Full SDREFAGG aggregation with multi-instance text inside a
//!   symbol dict (§6.5.8.2).

// Own JBIG2 decoder modules:
//
// - `arith`        -- MQ arith coder + integer arith decoder (ISO 14492
//                     Annex E + Annex A.2).
// - `segments`     -- segment-header parsing (ISO 14492 §7.2).
// - `regions`      -- generic / refinement / symbol dict / text / halftone.
// - `globals`      -- /JBIG2Globals resolver + merged segment view.
// - `own_decoder`  -- segment-list dispatcher, composites regions into
//                     the page bitmap.

pub mod arith;

pub mod globals;

pub mod regions;

pub mod segments;

mod own_decoder;

// ---------------------------------------------------------------------------
// Shared region-dimension bounds (SEC-ALLOC-CLAMP, task #62)
// ---------------------------------------------------------------------------

/// Maximum width or height accepted for any JBIG2 region or bitmap
/// allocation, in pixels.
///
/// JBIG2 region-dimension fields are u32 (ISO 14492 §7.2), so a malformed
/// segment header can claim dimensions up to ~4.3 billion pixels per side.
/// `width * height` at that scale overflows even u64 and, honoured literally,
/// would OOM-kill any worker process. We cap both dimensions at 65536 --
/// a 1200-DPI A0 scan is ~40k pixels wide, so this leaves 50% headroom for
/// any plausible real-world JBIG2 bitonal scan while refusing the
/// attacker-controlled bogus-magnitude case.
pub const MAX_JBIG2_REGION_DIMENSION: u32 = 65_536;

/// Check region `(width, height)` against [`MAX_JBIG2_REGION_DIMENSION`] and
/// return the total pixel count as a bounded `usize` if it passes.
///
/// The `kind` tag ("generic", "refinement", "halftone", ...) propagates into
/// the error's `ResourceLimitExceeded` payload for structured logging.
///
/// This is the single gatekeeper every region decoder should call before
/// allocating the output buffer.
pub(crate) fn check_region_dimensions(
    width: u32,
    height: u32,
    kind: &'static str,
) -> Result<usize, RegionDimensionError> {
    if width == 0 || height == 0 {
        return Err(RegionDimensionError::ZeroDimension);
    }
    if width > MAX_JBIG2_REGION_DIMENSION || height > MAX_JBIG2_REGION_DIMENSION {
        return Err(RegionDimensionError::TooLarge {
            width,
            height,
            max: MAX_JBIG2_REGION_DIMENSION,
            kind,
        });
    }
    // width * height fits in u64 because both are <= 65536; product <= 2^32.
    let pixels = (width as u64) * (height as u64);
    udoc_core::limits::safe_alloc_size(pixels, udoc_core::limits::DEFAULT_MAX_ALLOC_BYTES, kind)
        .map_err(|_| RegionDimensionError::TooLarge {
            width,
            height,
            max: MAX_JBIG2_REGION_DIMENSION,
            kind,
        })
}

/// Error returned by `check_region_dimensions`. Each region decoder's
/// own error enum carries a `From` for this so the check can appear as
/// a one-line `?` at the top of every region entry point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionDimensionError {
    /// Region declared width=0 or height=0.
    ZeroDimension,
    /// Region dimension exceeded [`MAX_JBIG2_REGION_DIMENSION`]. Almost
    /// certainly a malformed / adversarial segment header.
    TooLarge {
        /// Declared width in pixels.
        width: u32,
        /// Declared height in pixels.
        height: u32,
        /// The ceiling that was enforced.
        max: u32,
        /// The region kind that failed ("generic", "halftone", etc.).
        kind: &'static str,
    },
}

impl std::fmt::Display for RegionDimensionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroDimension => f.write_str("JBIG2 region has zero width or height"),
            Self::TooLarge {
                width,
                height,
                max,
                kind,
            } => write!(
                f,
                "JBIG2 {kind} region dimensions {width}x{height} exceed ceiling {max}"
            ),
        }
    }
}

impl std::error::Error for RegionDimensionError {}

/// Decode JBIG2 embedded data to raw 1-byte-per-pixel grayscale.
/// Returns `None` on decode failure.
///
/// Dispatches to the own decoder ([`own_decoder::decode_page`]) which
/// covers ISO 14492 §6.2-§6.7 arith-coded paths. When the own decoder
/// can't handle the stream (unsupported Huffman path, parse error, or
/// the `jbig2` feature is disabled) we return `None` so the caller can
/// emit `UnsupportedFilter` and pass raw bytes through.
pub(crate) fn decode_jbig2(data: &[u8], globals: Option<&[u8]>) -> Option<Vec<u8>> {
    #[cfg(feature = "jbig2")]
    {
        own_decoder::decode_page(data, globals)
    }
    #[cfg(not(feature = "jbig2"))]
    {
        let _ = (data, globals);
        None
    }
}

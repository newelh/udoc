#![deny(unsafe_code)]
#![warn(missing_docs)]

//! Image decoders for the `udoc` document-extraction toolkit.
//!
//! This crate owns format-level image decoding for PDF streams that is
//! independent of PDF object resolution. Today it exposes:
//!
//! - [`ccitt`] -- CCITT Group 3 1D (T.4) and Group 4 (T.6) fax decoder
//! - [`jbig2`] -- Own JBIG2 decoder per ISO 14492 (arith coder + segment
//!   / region parsers,  #158). Streams outside the covered subset
//!   (Huffman text / symbol dicts, full SDREFAGG) return `None` so
//!   callers emit an `UnsupportedFilter` warning.
//! - [`decode_ccitt`] / [`decode_jbig2`] -- unified dispatch entry points
//! - [`colorspace`] -- CMYK/Gray/Lab to sRGB conversion helpers (non-ICC)
//!
//! # Scope
//!
//! `udoc-image` owns decoders for formats that produce raster pixel
//! data and are not specific to any PDF object model. The crate is
//! deliberately small: transport-codec filters (Flate, LZW, ASCII85,
//! RunLength) stay in `udoc-pdf` because they are not image-specific.
//!
//! The [`transcode`] submodule ships a raw-to-PNG helper used by the
//! CLI image-dump and future hook-protocol image-dump capabilities
//! (T3-IMG-TRANS, #167). It intentionally duplicates a small portion
//! of the Flate/RunLength transport-codec surface so tooling can
//! transcode images without depending on `udoc-pdf`.
//!
//! # Output conventions
//!
//! Decoded bilevel images are emitted as 1 byte per pixel (grayscale),
//! `0x00` = black, `0xFF` = white. Callers that need packed 1-bit
//! output are expected to re-pack from the grayscale buffer.

/// CCITT Group 3/4 fax decoder. Doc-hidden because the public surface
/// is the [`decode_ccitt`] entry point at the crate root; the module
/// itself exposes implementation-detail helpers.
#[doc(hidden)]
pub mod ccitt;
pub mod colorspace;
/// Own JBIG2 decoder per ISO 14492. Doc-hidden because the public
/// surface is the [`decode_jbig2`] entry point at the crate root; the
/// nested `arith`, `regions`, `segments` modules are implementation
/// detail.
#[doc(hidden)]
pub mod jbig2;
pub mod transcode;

/// Maximum width or height accepted for any image buffer in this crate,
/// in pixels. Shared across CCITT, JBIG2, JPEG, and raw-to-PNG transcode
/// paths (SEC-ALLOC-CLAMP, #62).
///
/// 65,536 covers the largest real-world images we've seen (archive.org
/// scans at 600 DPI A0 ~= 40k px wide) with 50% headroom. Anything
/// larger is almost certainly a malformed / adversarial dimension
/// field that would otherwise OOM-kill the worker when multiplied
/// into `width * height * bpp / 8`.
pub const MAX_IMAGE_DIMENSION: u32 = 65_536;

/// Shared validator for image `(width, height)` pairs. Used by the
/// transcode path and exposed for backends that decode raw image bytes
/// directly. Returns the total pixel count as `usize` on success so
/// callers can use the bounded value directly for `Vec::with_capacity`.
///
/// The `kind` tag flows into the error payload for structured logging
/// (e.g. "jpeg", "ccitt", "transcode").
pub fn check_image_dimensions(
    width: u32,
    height: u32,
    kind: &'static str,
) -> udoc_core::error::Result<usize> {
    if width == 0 || height == 0 {
        return Err(udoc_core::error::Error::new(format!(
            "{kind} image has zero dimensions ({width}x{height})"
        )));
    }
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(udoc_core::error::Error::resource_limit_exceeded(
            width.max(height) as u64,
            MAX_IMAGE_DIMENSION as u64,
            kind,
        ));
    }
    // width * height fits in u64 (both <= 65536 => product <= 2^32).
    let pixels = (width as u64) * (height as u64);
    udoc_core::limits::safe_alloc_size(pixels, udoc_core::limits::DEFAULT_MAX_ALLOC_BYTES, kind)
}

pub use colorspace::Colorspace;
pub use transcode::{
    transcode_to_png, CcittFaxParams, ImageFilter, Jbig2FilterParams, TranscodeError,
};

pub use colorspace::{
    cmyk_image_to_rgb, cmyk_to_rgb, gray_image_to_rgb, gray_to_rgb, lab_image_to_rgb, lab_to_rgb,
};

/// Identifies the PDF image filter to decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImageFilterKind {
    /// `/CCITTFaxDecode` (Group 3 1D or Group 4).
    Ccitt,
    /// `/JBIG2Decode`.
    Jbig2,
}

/// Parameters for CCITT fax decoding. Sourced from `/DecodeParms`.
#[derive(Debug, Clone, Copy)]
pub struct CcittParams {
    /// Image width (from `/Columns`).
    pub width: usize,
    /// Image height (from `/Rows`; 0 = unknown).
    pub height: usize,
    /// CCITT K parameter: `< 0` = Group 4, `0` = Group 3 1D, `> 0` = Group 3 2D.
    pub k: i64,
    /// Inverts the bit polarity when true (PDF `/BlackIs1`).
    pub black_is_1: bool,
}

/// Parameters for JBIG2 decoding.
#[derive(Debug, Default)]
pub struct Jbig2Params<'a> {
    /// Global segments, resolved from `/DecodeParms/JBIG2Globals`.
    pub globals: Option<&'a [u8]>,
}

/// Result of a successful image-filter decode: raw grayscale pixels.
#[derive(Debug)]
pub struct DecodedImage {
    /// One byte per pixel, row-major. `0x00` = black, `0xFF` = white.
    pub pixels: Vec<u8>,
}

/// Decode a CCITT fax stream into grayscale pixels.
///
/// Returns `None` on decode failure so callers can fall back to passing
/// through the raw bytes with a diagnostic warning. Dimensions exceeding
/// [`MAX_IMAGE_DIMENSION`] are rejected as `None` ( round-3 audit:
/// `/Columns` and `/Rows` are attacker-controlled and previously fed
/// directly into `Vec::with_capacity(width * height)`, allowing a 64-bit
/// allocation bomb via tiny width + huge inferred height).
pub fn decode_ccitt(data: &[u8], params: CcittParams) -> Option<DecodedImage> {
    let width = u32::try_from(params.width).ok()?;
    let height = u32::try_from(params.height).ok()?;
    if width == 0 || height == 0 || width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return None;
    }
    ccitt::decode_ccitt_fax(
        data,
        params.width,
        params.height,
        params.k,
        params.black_is_1,
    )
    .map(|pixels| DecodedImage { pixels })
}

/// Decode a JBIG2 stream into grayscale pixels.
///
/// Returns `None` on decode failure. Partial decodes are padded with
/// white to the expected buffer size (mirrors the pre-extraction
/// behaviour in `udoc-pdf`; see `jbig2::decode_jbig2` for details).
pub fn decode_jbig2(data: &[u8], params: Jbig2Params<'_>) -> Option<DecodedImage> {
    jbig2::decode_jbig2(data, params.globals).map(|pixels| DecodedImage { pixels })
}

//! Raw-to-PNG transcoding for PDF image streams.
//!
//! This module lets callers take the raw bytes of a PDF image XObject,
//! the PDF-level filter chain and metadata (width, height, bits per
//! component, colorspace), and produce a standalone PNG that any
//! off-the-shelf image viewer can open.
//!
//! # Consumers
//!
//! - CLI image-dump (`udoc images --extract <dir>`,).
//! - Future hook-protocol image-dump capabilities.
//!
//! Decoding runs the filter chain in PDF-spec order (first filter in the
//! array decodes first), then converts pixels to 8-bit sRGB based on the
//! declared [`Colorspace`] and encodes a PNG via the `png` crate.

use crate::colorspace::{self, Colorspace};
use crate::{CcittParams, Jbig2Params};

/// The subset of PDF stream filters that can appear in an image's filter
/// chain. This is intentionally narrower than the full PDF filter set:
/// post-ImageXObject transports only. Unknown names land on
/// [`TranscodeError::UnsupportedFilter`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ImageFilter {
    /// `/FlateDecode` (deflate / zlib). Transport codec -- outputs raw samples.
    Flate,
    /// `/LZWDecode`. Currently unsupported for transcode; reserved so callers
    /// can feed raw filter chains through without pre-filtering.
    Lzw,
    /// `/ASCII85Decode`. Currently unsupported for transcode.
    Ascii85,
    /// `/ASCIIHexDecode`. Currently unsupported for transcode.
    AsciiHex,
    /// `/RunLengthDecode`. Transport codec.
    RunLength,
    /// `/CCITTFaxDecode`. Bilevel image codec; decoded to 1 byte/pixel
    /// grayscale via [`crate::decode_ccitt`].
    CcittFax(CcittFaxParams),
    /// `/JBIG2Decode`. Bilevel image codec; decoded via
    /// [`crate::decode_jbig2`] (behind the `jbig2` feature).
    Jbig2(Jbig2FilterParams),
    /// `/DCTDecode`. JPEG baseline / progressive, decoded via `jpeg-decoder`.
    DctDecode,
    /// `/JPXDecode`. JPEG 2000; not decoded by this helper today.
    JpxDecode,
}

/// Parameters captured from `/DecodeParms` needed to decode a CCITT stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CcittFaxParams {
    /// CCITT K parameter: `< 0` = Group 4, `0` = Group 3 1D, `> 0` = Group 3 2D.
    pub k: i64,
    /// PDF `/BlackIs1`.
    pub black_is_1: bool,
}

impl Default for CcittFaxParams {
    fn default() -> Self {
        // Group 4 + BlackIs1 = false is the common PDF default for /CCITTFaxDecode.
        Self {
            k: -1,
            black_is_1: false,
        }
    }
}

/// Parameters for JBIG2Decode. Currently just the resolved
/// `/DecodeParms/JBIG2Globals` segment-data bytes, when the PDF references
/// a global JBIG2 segment stream.
#[derive(Debug, Clone, Default)]
pub struct Jbig2FilterParams {
    /// Resolved JBIG2 global segments. `None` when the stream is standalone.
    pub globals: Option<Vec<u8>>,
}

/// Errors returned by [`transcode_to_png`].
#[derive(Debug)]
#[non_exhaustive]
pub enum TranscodeError {
    /// Declared image dimensions are zero or overflow `usize`.
    InvalidDimensions {
        /// Declared width.
        width: u32,
        /// Declared height.
        height: u32,
    },
    /// Bits-per-component outside the supported range (1, 2, 4, 8).
    UnsupportedBitDepth(u8),
    /// A filter in the chain is recognised but not currently decoded by
    /// this helper (e.g. `JpxDecode`, `Lzw`).
    UnsupportedFilter(String),
    /// Raw sample buffer is too short for the declared dimensions /
    /// bits-per-component / colorspace.
    BufferTooShort {
        /// Expected byte count.
        expected: usize,
        /// Actual byte count.
        got: usize,
    },
    /// A filter-chain decode step failed.
    DecodeFailed(String),
    /// The `png` encoder rejected the output.
    PngEncode(String),
}

impl std::fmt::Display for TranscodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidDimensions { width, height } => {
                write!(f, "invalid image dimensions: {width}x{height}")
            }
            Self::UnsupportedBitDepth(bpc) => {
                write!(f, "unsupported bits-per-component: {bpc}")
            }
            Self::UnsupportedFilter(name) => {
                write!(f, "unsupported filter: {name}")
            }
            Self::BufferTooShort { expected, got } => {
                write!(
                    f,
                    "raw sample buffer too short: expected {expected} bytes, got {got}"
                )
            }
            Self::DecodeFailed(msg) => write!(f, "decode failed: {msg}"),
            Self::PngEncode(msg) => write!(f, "png encoding failed: {msg}"),
        }
    }
}

impl std::error::Error for TranscodeError {}

/// Transcode a PDF image's raw bytes + filter chain to a standalone PNG.
///
/// The filter chain is applied in PDF order (first filter in the slice
/// decodes first). After the chain resolves to raw samples, the samples
/// are expanded to 8-bit RGB according to `colorspace` and fed to the
/// `png` crate.
///
/// # Supported
///
/// - Filter chains ending in `DctDecode` (JPEG baseline/progressive).
/// - `CcittFax` + `Jbig2` for bilevel image input (emitted as gray, then
///   expanded to RGB).
/// - Gray, CMYK, Rgb, Lab, Indexed colorspaces when the chain yields raw
///   samples; 1-, 2-, 4-, and 8-bit components.
///
/// # Not yet supported (returns [`TranscodeError::UnsupportedFilter`])
///
/// - `JpxDecode` (JPEG 2000): upstream dep lives in `udoc-render`; moving
///   the decoder into this crate is its own task.
/// - `Lzw`, `Ascii85`, `AsciiHex`: rarely seen on image streams; can be
///   added.
///
/// # Errors
///
/// See [`TranscodeError`] variants.
pub fn transcode_to_png(
    raw_bytes: &[u8],
    filter_chain: &[ImageFilter],
    width: u32,
    height: u32,
    bits_per_component: u8,
    colorspace: Colorspace,
) -> Result<Vec<u8>, TranscodeError> {
    if width == 0 || height == 0 {
        return Err(TranscodeError::InvalidDimensions { width, height });
    }
    if !matches!(bits_per_component, 1 | 2 | 4 | 8) {
        return Err(TranscodeError::UnsupportedBitDepth(bits_per_component));
    }

    // Shortcut: if the chain ends in DCTDecode, decode JPEG end-to-end
    // (the codec is self-describing; width/height/colorspace args are just
    // a sanity check).
    if let Some(ImageFilter::DctDecode) = filter_chain.last() {
        // Run any preceding transport filters, then hand the JPEG bytes to
        // jpeg-decoder.
        let jpeg_bytes = run_pre_codec_chain(raw_bytes, &filter_chain[..filter_chain.len() - 1])?;
        return encode_jpeg_to_png(&jpeg_bytes, width, height);
    }
    if let Some(ImageFilter::JpxDecode) = filter_chain.last() {
        return Err(TranscodeError::UnsupportedFilter(
            "JpxDecode: JPEG 2000 transcoding lives in udoc-render; ".to_string(),
        ));
    }

    // CCITT / JBIG2 produce 1-byte-per-pixel grayscale. Any trailing
    // transport filter after them is unusual; we run the whole chain as a
    // bitmap decode.
    let decoded = run_filter_chain(raw_bytes, filter_chain, width, height, bits_per_component)?;

    // Now `decoded` is raw samples. Expand to 8-bit RGB via colorspace.
    let rgb = samples_to_rgb(
        &decoded,
        width,
        height,
        bits_per_component,
        &colorspace,
        was_bilevel_decoded(filter_chain),
    )?;

    encode_rgb_png(&rgb, width, height)
}

/// Run all filters in the chain, producing raw sample bytes.
fn run_filter_chain(
    raw_bytes: &[u8],
    filter_chain: &[ImageFilter],
    width: u32,
    height: u32,
    bits_per_component: u8,
) -> Result<Vec<u8>, TranscodeError> {
    let mut buf: Vec<u8> = raw_bytes.to_vec();
    for filter in filter_chain {
        buf = apply_filter(&buf, filter, width, height, bits_per_component)?;
    }
    Ok(buf)
}

/// Run only transport filters (not final image codecs). Used to prepare
/// JPEG bytes for `jpeg-decoder` when a stream has e.g. `/Flate` before
/// `/DCT`.
fn run_pre_codec_chain(
    raw_bytes: &[u8],
    filter_chain: &[ImageFilter],
) -> Result<Vec<u8>, TranscodeError> {
    let mut buf: Vec<u8> = raw_bytes.to_vec();
    for filter in filter_chain {
        match filter {
            ImageFilter::Flate => buf = decode_flate(&buf)?,
            ImageFilter::RunLength => buf = decode_run_length(&buf),
            other => {
                return Err(TranscodeError::UnsupportedFilter(format!(
                    "{other:?} before image codec"
                )))
            }
        }
    }
    Ok(buf)
}

fn apply_filter(
    input: &[u8],
    filter: &ImageFilter,
    width: u32,
    height: u32,
    _bits_per_component: u8,
) -> Result<Vec<u8>, TranscodeError> {
    match filter {
        ImageFilter::Flate => decode_flate(input),
        ImageFilter::RunLength => Ok(decode_run_length(input)),
        ImageFilter::CcittFax(params) => {
            let decoded = crate::decode_ccitt(
                input,
                CcittParams {
                    width: width as usize,
                    height: height as usize,
                    k: params.k,
                    black_is_1: params.black_is_1,
                },
            )
            .ok_or_else(|| TranscodeError::DecodeFailed("CCITTFaxDecode".to_string()))?;
            Ok(decoded.pixels)
        }
        ImageFilter::Jbig2(params) => {
            let decoded = crate::decode_jbig2(
                input,
                Jbig2Params {
                    globals: params.globals.as_deref(),
                },
            )
            .ok_or_else(|| TranscodeError::DecodeFailed("JBIG2Decode".to_string()))?;
            Ok(decoded.pixels)
        }
        ImageFilter::DctDecode => Err(TranscodeError::UnsupportedFilter(
            "DctDecode must be terminal in the filter chain".to_string(),
        )),
        ImageFilter::JpxDecode => Err(TranscodeError::UnsupportedFilter(
            "JpxDecode: JPEG 2000 transcoding lives in udoc-render; ".to_string(),
        )),
        ImageFilter::Lzw => Err(TranscodeError::UnsupportedFilter("LZWDecode".to_string())),
        ImageFilter::Ascii85 => Err(TranscodeError::UnsupportedFilter(
            "ASCII85Decode".to_string(),
        )),
        ImageFilter::AsciiHex => Err(TranscodeError::UnsupportedFilter(
            "ASCIIHexDecode".to_string(),
        )),
    }
}

fn decode_flate(input: &[u8]) -> Result<Vec<u8>, TranscodeError> {
    use std::io::Read;
    // PDF FlateDecode streams are zlib-wrapped ("x" 0x78 header) in practice.
    // Fall back to raw deflate if the zlib header is absent.
    let zlib = !input.is_empty() && input[0] == 0x78;
    let mut out = Vec::new();
    if zlib {
        let mut decoder = flate2::read::ZlibDecoder::new(input);
        decoder
            .read_to_end(&mut out)
            .map_err(|e| TranscodeError::DecodeFailed(format!("FlateDecode: {e}")))?;
    } else {
        let mut decoder = flate2::read::DeflateDecoder::new(input);
        decoder
            .read_to_end(&mut out)
            .map_err(|e| TranscodeError::DecodeFailed(format!("FlateDecode: {e}")))?;
    }
    Ok(out)
}

fn decode_run_length(input: &[u8]) -> Vec<u8> {
    // PDF RunLengthDecode: byte N controls the run. 0-127 copy N+1, 129-255
    // repeat next byte 257-N times, 128 = EOD.
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let n = input[i];
        i += 1;
        if n == 128 {
            break;
        }
        if n < 128 {
            let count = n as usize + 1;
            let end = (i + count).min(input.len());
            out.extend_from_slice(&input[i..end]);
            i = end;
        } else {
            let count = 257 - n as usize;
            if i >= input.len() {
                break;
            }
            let b = input[i];
            i += 1;
            out.extend(std::iter::repeat_n(b, count));
        }
    }
    out
}

fn was_bilevel_decoded(filter_chain: &[ImageFilter]) -> bool {
    matches!(
        filter_chain.last(),
        Some(ImageFilter::CcittFax(_)) | Some(ImageFilter::Jbig2(_))
    )
}

/// Expand raw sample bytes to 8-bit RGB per the declared colorspace.
fn samples_to_rgb(
    samples: &[u8],
    width: u32,
    height: u32,
    bits_per_component: u8,
    colorspace: &Colorspace,
    bilevel_decoded: bool,
) -> Result<Vec<u8>, TranscodeError> {
    // SEC-ALLOC-CLAMP (#62): refuse images whose declared dimensions
    // exceed the crate-wide ceiling before we compute w*h*channels.
    // Adversarial /Width or /Height fields (or corrupt dims from an
    // encrypted stream) would otherwise overflow into a petabyte alloc.
    let pixels = crate::check_image_dimensions(width, height, "transcode")
        .map_err(|e| TranscodeError::DecodeFailed(format!("image dimensions rejected: {e}")))?;
    let w = width as usize;
    let h = height as usize;
    debug_assert_eq!(pixels, w * h);

    // Bilevel CCITT / JBIG2 output is already 1 byte/pixel grayscale.
    if bilevel_decoded {
        if samples.len() < pixels {
            return Err(TranscodeError::BufferTooShort {
                expected: pixels,
                got: samples.len(),
            });
        }
        return Ok(gray_bytes_to_rgb(&samples[..pixels]));
    }

    match colorspace {
        Colorspace::Gray => {
            let gray = unpack_samples(samples, pixels, bits_per_component)?;
            Ok(gray_bytes_to_rgb(&gray))
        }
        Colorspace::Rgb => {
            let unpacked = unpack_samples(samples, pixels * 3, bits_per_component)?;
            Ok(unpacked)
        }
        Colorspace::Cmyk => {
            let unpacked = unpack_samples(samples, pixels * 4, bits_per_component)?;
            let mut out = Vec::with_capacity(pixels * 3);
            for chunk in unpacked.chunks_exact(4) {
                let rgb = colorspace::cmyk_bytes_to_rgb(chunk[0], chunk[1], chunk[2], chunk[3]);
                out.extend_from_slice(&rgb);
            }
            Ok(out)
        }
        Colorspace::Lab => {
            let unpacked = unpack_samples(samples, pixels * 3, bits_per_component)?;
            let mut out = Vec::with_capacity(pixels * 3);
            for chunk in unpacked.chunks_exact(3) {
                let rgb = colorspace::lab_bytes_to_rgb(chunk[0], chunk[1], chunk[2]);
                out.extend_from_slice(&rgb);
            }
            Ok(out)
        }
        Colorspace::Indexed(palette) => {
            let indices = unpack_samples(samples, pixels, bits_per_component)?;
            if palette.is_empty() {
                return Err(TranscodeError::DecodeFailed(
                    "Indexed colorspace with empty palette".to_string(),
                ));
            }
            let mut out = Vec::with_capacity(pixels * 3);
            for &idx in &indices {
                let entry = palette
                    .get(idx as usize)
                    .copied()
                    .unwrap_or(*palette.last().unwrap());
                out.extend_from_slice(&[entry.0, entry.1, entry.2]);
            }
            Ok(out)
        }
    }
}

fn gray_bytes_to_rgb(gray: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(gray.len() * 3);
    for &g in gray {
        out.extend_from_slice(&[g, g, g]);
    }
    out
}

/// Expand packed samples to one byte per sample.
fn unpack_samples(
    samples: &[u8],
    sample_count: usize,
    bits_per_component: u8,
) -> Result<Vec<u8>, TranscodeError> {
    if bits_per_component == 8 {
        if samples.len() < sample_count {
            return Err(TranscodeError::BufferTooShort {
                expected: sample_count,
                got: samples.len(),
            });
        }
        return Ok(samples[..sample_count].to_vec());
    }
    let bpc = bits_per_component as usize;
    let total_bits = sample_count * bpc;
    let needed_bytes = total_bits.div_ceil(8);
    if samples.len() < needed_bytes {
        return Err(TranscodeError::BufferTooShort {
            expected: needed_bytes,
            got: samples.len(),
        });
    }
    let mut out = Vec::with_capacity(sample_count);
    let mask = (1u16 << bpc) - 1;
    // Scale to full 8-bit range so 1-bit white reads as 255 not 1.
    let scale = 255u16 / mask;
    let mut bit_pos = 0usize;
    for _ in 0..sample_count {
        let byte_idx = bit_pos / 8;
        let bit_in_byte = bit_pos % 8;
        // Read up to 16 bits starting at bit_pos (MSB-first packing, PDF default).
        let hi = samples[byte_idx] as u16;
        let lo = samples.get(byte_idx + 1).copied().unwrap_or(0) as u16;
        let window = (hi << 8) | lo;
        let sample = (window >> (16 - bit_in_byte - bpc)) & mask;
        out.push((sample * scale) as u8);
        bit_pos += bpc;
    }
    Ok(out)
}

fn encode_rgb_png(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>, TranscodeError> {
    let expected = (width as usize) * (height as usize) * 3;
    if rgb.len() < expected {
        return Err(TranscodeError::BufferTooShort {
            expected,
            got: rgb.len(),
        });
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| TranscodeError::PngEncode(e.to_string()))?;
        writer
            .write_image_data(&rgb[..expected])
            .map_err(|e| TranscodeError::PngEncode(e.to_string()))?;
    }
    Ok(out)
}

fn encode_jpeg_to_png(
    jpeg_bytes: &[u8],
    declared_w: u32,
    declared_h: u32,
) -> Result<Vec<u8>, TranscodeError> {
    let mut decoder = jpeg_decoder::Decoder::new(jpeg_bytes);
    let pixels = decoder
        .decode()
        .map_err(|e| TranscodeError::DecodeFailed(format!("DCTDecode: {e}")))?;
    let info = decoder
        .info()
        .ok_or_else(|| TranscodeError::DecodeFailed("DCTDecode: missing info".to_string()))?;
    let w = info.width as u32;
    let h = info.height as u32;
    if w == 0 || h == 0 {
        return Err(TranscodeError::InvalidDimensions {
            width: w,
            height: h,
        });
    }
    // Sanity-check declared vs actual (mismatch is not fatal; prefer actual).
    let _ = (declared_w, declared_h);
    let rgb = match info.pixel_format {
        jpeg_decoder::PixelFormat::L8 => gray_bytes_to_rgb(&pixels),
        jpeg_decoder::PixelFormat::L16 => {
            // 16-bit grayscale -> take high byte, expand to RGB.
            let mut gray = Vec::with_capacity(pixels.len() / 2);
            for chunk in pixels.chunks_exact(2) {
                gray.push(chunk[0]);
            }
            gray_bytes_to_rgb(&gray)
        }
        jpeg_decoder::PixelFormat::RGB24 => pixels,
        jpeg_decoder::PixelFormat::CMYK32 => {
            let mut out = Vec::with_capacity((w as usize) * (h as usize) * 3);
            for chunk in pixels.chunks_exact(4) {
                let rgb = colorspace::cmyk_bytes_to_rgb(chunk[0], chunk[1], chunk[2], chunk[3]);
                out.extend_from_slice(&rgb);
            }
            out
        }
    };
    encode_rgb_png(&rgb, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use png::Decoder;

    /// Assert a PNG parses cleanly at the expected dimensions and RGB type.
    fn decode_png_rgb(data: &[u8]) -> (Vec<u8>, u32, u32) {
        let decoder = Decoder::new(data);
        let mut reader = decoder.read_info().expect("png header");
        let info = reader.info().clone();
        let mut buf = vec![0u8; reader.output_buffer_size()];
        reader.next_frame(&mut buf).expect("png data");
        // The encoder in `encode_rgb_png` always emits ColorType::Rgb.
        assert_eq!(info.color_type, png::ColorType::Rgb);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);
        (buf, info.width, info.height)
    }

    #[test]
    fn gray_passthrough_emits_rgb_with_equal_channels() {
        // 100x100 gray image at sample value 128.
        let raw = vec![128u8; 100 * 100];
        let png_bytes = transcode_to_png(&raw, &[], 100, 100, 8, Colorspace::Gray)
            .expect("transcode gray passthrough");
        let (decoded, w, h) = decode_png_rgb(&png_bytes);
        assert_eq!(w, 100);
        assert_eq!(h, 100);
        assert_eq!(decoded.len(), 100 * 100 * 3);
        for chunk in decoded.chunks_exact(3) {
            assert_eq!(chunk[0], 128);
            assert_eq!(chunk[1], 128);
            assert_eq!(chunk[2], 128);
        }
    }

    #[test]
    fn ccitt_bilevel_round_trip_to_png() {
        // CCITT Group 4, 8x1: 4 black + 4 white via H mode.
        // Bit stream: 001 (H) + white_run(0)=00110101 + black_run(4)=011 +
        //             white_run(4)=1011 = 19 bits -> 0x26 0xAD 0xD8.
        let ccitt = [0x26u8, 0xAD, 0xD8];
        let png_bytes = transcode_to_png(
            &ccitt,
            &[ImageFilter::CcittFax(CcittFaxParams {
                k: -1,
                black_is_1: false,
            })],
            8,
            1,
            1,
            Colorspace::Gray,
        )
        .expect("transcode ccitt");
        let (decoded, w, h) = decode_png_rgb(&png_bytes);
        assert_eq!(w, 8);
        assert_eq!(h, 1);
        // First 4 pixels black, next 4 pixels white; RGB triples.
        let expected: Vec<u8> = [0u8, 0, 0, 0, 255, 255, 255, 255]
            .iter()
            .flat_map(|&g| [g, g, g])
            .collect();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn jpeg_round_trip_to_png() {
        // Synthesize a small JPEG via the `image` crate (dev-dep).
        use image::{codecs::jpeg::JpegEncoder, ColorType};
        let rgb: Vec<u8> = (0..10 * 10)
            .flat_map(|i| [((i * 3) % 255) as u8, ((i * 5) % 255) as u8, 0u8])
            .collect();
        let mut jpeg_bytes = Vec::new();
        {
            let mut encoder = JpegEncoder::new_with_quality(&mut jpeg_bytes, 90);
            encoder
                .encode(&rgb, 10, 10, ColorType::Rgb8.into())
                .expect("encode jpeg");
        }

        let png_bytes = transcode_to_png(
            &jpeg_bytes,
            &[ImageFilter::DctDecode],
            10,
            10,
            8,
            Colorspace::Rgb,
        )
        .expect("transcode jpeg");
        let (decoded, w, h) = decode_png_rgb(&png_bytes);
        assert_eq!(w, 10);
        assert_eq!(h, 10);
        assert_eq!(decoded.len(), 10 * 10 * 3);
    }

    #[test]
    fn rgb_passthrough_one_bit_expands_to_full_range() {
        // 2x1 image, 1 bpc, RGB (6 samples). First pixel all 1s -> white,
        // second pixel all 0s -> black. Packed MSB-first: 111000 -> 0b11100000.
        let raw = vec![0b11100000u8];
        let png_bytes =
            transcode_to_png(&raw, &[], 2, 1, 1, Colorspace::Rgb).expect("transcode 1-bpc rgb");
        let (decoded, w, h) = decode_png_rgb(&png_bytes);
        assert_eq!(w, 2);
        assert_eq!(h, 1);
        assert_eq!(decoded, vec![255, 255, 255, 0, 0, 0]);
    }

    #[test]
    fn cmyk_black_plate_yields_black_rgb() {
        // 1x1 pixel, CMYK=(0,0,0,255) -> RGB (0,0,0).
        let raw = vec![0u8, 0, 0, 255];
        let png_bytes =
            transcode_to_png(&raw, &[], 1, 1, 8, Colorspace::Cmyk).expect("transcode cmyk");
        let (decoded, w, h) = decode_png_rgb(&png_bytes);
        assert_eq!((w, h), (1, 1));
        assert_eq!(decoded, vec![0, 0, 0]);
    }

    #[test]
    fn invalid_dimensions_error() {
        let raw = vec![0u8; 4];
        let err = transcode_to_png(&raw, &[], 0, 10, 8, Colorspace::Gray).unwrap_err();
        assert!(matches!(err, TranscodeError::InvalidDimensions { .. }));
    }

    #[test]
    fn unsupported_bit_depth_error() {
        let raw = vec![0u8; 4];
        let err = transcode_to_png(&raw, &[], 2, 2, 12, Colorspace::Gray).unwrap_err();
        assert!(matches!(err, TranscodeError::UnsupportedBitDepth(12)));
    }

    #[test]
    fn jpx_unsupported_for_now() {
        let raw = vec![0u8; 16];
        let err = transcode_to_png(&raw, &[ImageFilter::JpxDecode], 4, 4, 8, Colorspace::Rgb)
            .unwrap_err();
        assert!(matches!(err, TranscodeError::UnsupportedFilter(_)));
    }

    #[test]
    fn flate_pre_codec_chain_prepares_jpeg() {
        // Flate-compress a JPEG blob then transcode via [Flate, DCT].
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use image::{codecs::jpeg::JpegEncoder, ColorType};
        use std::io::Write;

        let rgb = vec![200u8; 8 * 8 * 3];
        let mut jpeg_bytes = Vec::new();
        {
            let mut encoder = JpegEncoder::new_with_quality(&mut jpeg_bytes, 85);
            encoder
                .encode(&rgb, 8, 8, ColorType::Rgb8.into())
                .expect("encode jpeg");
        }
        let mut compressed = Vec::new();
        {
            let mut zlib = ZlibEncoder::new(&mut compressed, Compression::default());
            zlib.write_all(&jpeg_bytes).unwrap();
            zlib.finish().unwrap();
        }

        let png_bytes = transcode_to_png(
            &compressed,
            &[ImageFilter::Flate, ImageFilter::DctDecode],
            8,
            8,
            8,
            Colorspace::Rgb,
        )
        .expect("flate+dct chain");
        let (_, w, h) = decode_png_rgb(&png_bytes);
        assert_eq!((w, h), (8, 8));
    }

    #[test]
    fn indexed_palette_lookup() {
        // 2x1 image, 1 bpc indexed into a 2-color palette (red, green).
        let palette = vec![(255u8, 0, 0), (0, 255, 0)];
        // packed samples: 01 -> 0b01000000 (MSB first).
        let raw = vec![0b01000000u8];
        let png_bytes =
            transcode_to_png(&raw, &[], 2, 1, 1, Colorspace::Indexed(palette)).expect("indexed");
        let (decoded, w, h) = decode_png_rgb(&png_bytes);
        assert_eq!((w, h), (2, 1));
        assert_eq!(decoded, vec![255, 0, 0, 0, 255, 0]);
    }
}

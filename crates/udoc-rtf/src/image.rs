//! RTF image extraction.
//!
//! Converts parsed image data from the RTF parser into udoc-core PageImage types.

use udoc_core::image::{ImageFilter, PageImage};

use crate::parser::{ImageFormat, ParsedImage};

/// PNG file signature (first 8 bytes).
const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// Try to read PNG dimensions from the IHDR chunk.
/// Returns (width, height) or None if the data is too short or invalid.
fn png_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    // PNG: 8-byte signature, then IHDR chunk: 4-byte length, 4-byte type,
    // 4-byte width (big-endian), 4-byte height (big-endian).
    if data.len() < 24 {
        return None;
    }
    // Validate PNG signature and IHDR chunk type.
    if data[0..8] != PNG_SIGNATURE || &data[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
    let height = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
    Some((width, height))
}

/// Try to read JPEG dimensions by scanning for SOF markers.
/// Returns (width, height) or None if not found.
fn jpeg_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    // Walk JPEG segments properly: each segment starts with 0xFF + marker byte,
    // followed by a 2-byte big-endian length (except for SOI, EOI, and RST markers).
    let mut i = 0;
    while i + 1 < data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = data[i + 1];
        // Skip padding 0xFF bytes.
        if marker == 0xFF {
            i += 1;
            continue;
        }
        // SOI (0xD8), EOI (0xD9), and RST markers (0xD0-0xD7) have no length field.
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        // SOF0-SOF2: extract dimensions.
        if (0xC0..=0xC2).contains(&marker) {
            // SOF segment: 2-byte length, 1-byte precision,
            // 2-byte height (big-endian), 2-byte width (big-endian).
            if i + 9 < data.len() {
                let height = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
                let width = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
                return Some((width, height));
            }
            return None;
        }
        // All other segments: read 2-byte length and skip.
        if i + 3 >= data.len() {
            break;
        }
        let seg_len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        i = i + 2 + seg_len;
    }
    None
}

/// Convert a `ParsedImage` to an udoc-core `PageImage`.
///
/// Returns `None` for EMF, WMF, and Unknown formats (not supported).
/// For PNG and JPEG, returns `Some(PageImage)`. If the parsed dimensions
/// are zero, attempts to read them from the image data header.
pub fn convert_image(parsed: &ParsedImage) -> Option<PageImage> {
    let filter = match parsed.format {
        ImageFormat::Png => ImageFilter::Png,
        ImageFormat::Jpeg => ImageFilter::Jpeg,
        ImageFormat::Emf | ImageFormat::Wmf | ImageFormat::Unknown => return None,
    };

    let (mut width, mut height) = (parsed.width, parsed.height);

    // Fall back to reading dimensions from image headers.
    if width == 0 || height == 0 {
        let from_header = match parsed.format {
            ImageFormat::Png => png_dimensions(&parsed.data),
            ImageFormat::Jpeg => jpeg_dimensions(&parsed.data),
            _ => None,
        };
        if let Some((w, h)) = from_header {
            width = w;
            height = h;
        }
    }

    // Last resort: use goal dimensions (display-intent size in twips)
    // converted to pixels at 96 DPI. 1 inch = 1440 twips, so
    // pixels = twips * 96 / 1440. Not ideal but better than 0x0.
    if (width == 0 || height == 0) && parsed.goal_width > 0 && parsed.goal_height > 0 {
        width = (parsed.goal_width as u64 * 96 / 1440) as u32;
        height = (parsed.goal_height as u64 * 96 / 1440) as u32;
    }

    // Clone is necessary: PageExtractor::images() returns owned PageImage
    // and RtfPage only borrows ParsedDocument. Repeated images() calls
    // will re-clone. Acceptable since images() is not a hot path.
    Some(PageImage::new(
        parsed.data.clone(),
        filter,
        width,
        height,
        8, // default bits_per_component for PNG/JPEG
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_png_image() {
        let parsed = ParsedImage {
            format: ImageFormat::Png,
            data: vec![0x89, 0x50, 0x4E, 0x47],
            width: 100,
            height: 200,
            goal_width: 150,
            goal_height: 300,
        };

        let img = convert_image(&parsed).expect("should produce Some");
        assert_eq!(img.filter, ImageFilter::Png);
        assert_eq!(img.width, 100);
        assert_eq!(img.height, 200);
        assert_eq!(img.bits_per_component, 8);
        assert!(img.bbox.is_none());
        assert_eq!(img.data, vec![0x89, 0x50, 0x4E, 0x47]);
    }

    #[test]
    fn convert_jpeg_image() {
        let parsed = ParsedImage {
            format: ImageFormat::Jpeg,
            data: vec![0xFF, 0xD8, 0xFF, 0xE0],
            width: 640,
            height: 480,
            goal_width: 0,
            goal_height: 0,
        };

        let img = convert_image(&parsed).expect("should produce Some");
        assert_eq!(img.filter, ImageFilter::Jpeg);
        assert_eq!(img.width, 640);
        assert_eq!(img.height, 480);
    }

    #[test]
    fn skip_emf_wmf_unknown() {
        for fmt in [ImageFormat::Emf, ImageFormat::Wmf, ImageFormat::Unknown] {
            let parsed = ParsedImage {
                format: fmt,
                data: vec![0x01, 0x02],
                width: 10,
                height: 10,
                goal_width: 0,
                goal_height: 0,
            };
            assert!(convert_image(&parsed).is_none());
        }
    }

    #[test]
    fn zero_dimensions_fallback_to_png_header() {
        // Minimal PNG-like header with IHDR dimensions.
        let mut data = vec![0u8; 24];
        // PNG signature (8 bytes)
        data[0..8].copy_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
        // IHDR chunk: length (4 bytes), type (4 bytes), width (4 bytes), height (4 bytes)
        data[8..12].copy_from_slice(&[0, 0, 0, 13]); // length
        data[12..16].copy_from_slice(b"IHDR");
        data[16..20].copy_from_slice(&320u32.to_be_bytes()); // width
        data[20..24].copy_from_slice(&240u32.to_be_bytes()); // height

        let parsed = ParsedImage {
            format: ImageFormat::Png,
            data,
            width: 0,
            height: 0,
            goal_width: 0,
            goal_height: 0,
        };

        let img = convert_image(&parsed).expect("should produce Some");
        assert_eq!(img.width, 320);
        assert_eq!(img.height, 240);
    }

    #[test]
    fn goal_dimensions_fallback_when_no_header() {
        // When pixel dimensions and image header are both unavailable,
        // goal dimensions (twips) should be converted to pixels at 96 DPI.
        // 1440 twips = 1 inch = 96 pixels, so 2880 twips = 192 pixels.
        let parsed = ParsedImage {
            format: ImageFormat::Png,
            data: vec![0x00, 0x01], // not a valid PNG header
            width: 0,
            height: 0,
            goal_width: 2880,  // 2 inches
            goal_height: 1440, // 1 inch
        };

        let img = convert_image(&parsed).expect("should produce Some");
        assert_eq!(img.width, 192);
        assert_eq!(img.height, 96);
    }
}

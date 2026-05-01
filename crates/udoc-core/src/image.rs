//! Image output types for document extraction.

use crate::geometry::BoundingBox;

/// An image extracted from a document page.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct PageImage {
    /// Raw image data (compressed or raw depending on filter).
    pub data: Vec<u8>,
    /// The encoding of the image data.
    pub filter: ImageFilter,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Bits per color component.
    pub bits_per_component: u8,
    /// Position on the page. None if position is unknown or the format
    /// does not provide geometric placement.
    pub bbox: Option<BoundingBox>,
    /// Color space name (e.g. "DeviceRGB", "DeviceGray", "DeviceCMYK").
    /// None for non-PDF backends or when color space is unknown.
    pub color_space: Option<String>,
    /// Content stream render order for z-ordering. 0 = no ordering info.
    pub z_index: u32,
    /// True if this is an image mask (stencil). The 1-bit data defines
    /// where to paint `mask_color`. Only used by PDF rendering.
    #[cfg_attr(feature = "serde", serde(default))]
    pub is_mask: bool,
    /// Fill color for image masks (RGB, 0-255). Only meaningful when `is_mask` is true.
    #[cfg_attr(feature = "serde", serde(default))]
    pub mask_color: [u8; 3],
    /// Soft mask alpha channel bytes (0=transparent, 255=opaque).
    #[cfg_attr(feature = "serde", serde(default, skip))]
    pub soft_mask: Option<Vec<u8>>,
    /// Soft mask dimensions.
    #[cfg_attr(feature = "serde", serde(default))]
    pub soft_mask_width: u32,
    #[cfg_attr(feature = "serde", serde(default))]
    pub soft_mask_height: u32,
    /// Optional affine CTM mapping the source unit square (0,0)-(1,1) to
    /// page user space (y-up). Preserves rotation/shear that the AABB `bbox`
    /// loses. None for non-PDF backends where no content-stream CTM exists.
    #[cfg_attr(feature = "serde", serde(default))]
    pub ctm: Option<[f64; 6]>,
}

impl PageImage {
    /// Create a new PageImage.
    pub fn new(
        data: Vec<u8>,
        filter: ImageFilter,
        width: u32,
        height: u32,
        bits_per_component: u8,
        bbox: Option<BoundingBox>,
    ) -> Self {
        Self {
            data,
            filter,
            width,
            height,
            bits_per_component,
            bbox,
            color_space: None,
            z_index: 0,
            is_mask: false,
            mask_color: [0, 0, 0],
            soft_mask: None,
            soft_mask_width: 0,
            soft_mask_height: 0,
            ctm: None,
        }
    }

    /// Create a PageImage by auto-detecting filter and dimensions from raw bytes.
    ///
    /// Uses `detect_image_filter` and `parse_image_dimensions` on the data.
    /// Bits per component defaults to 8. Use this for OOXML/ODF backends where
    /// images are embedded as opaque blobs.
    pub fn from_data(data: Vec<u8>, bbox: Option<BoundingBox>) -> Self {
        let filter = detect_image_filter(&data);
        let (width, height) = parse_image_dimensions(&data);
        Self {
            data,
            filter,
            width,
            height,
            bits_per_component: 8,
            bbox,
            color_space: None,
            z_index: 0,
            is_mask: false,
            mask_color: [0, 0, 0],
            soft_mask: None,
            soft_mask_width: 0,
            soft_mask_height: 0,
            ctm: None,
        }
    }
}

/// Image encoding/compression format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum ImageFilter {
    /// JPEG (DCTDecode in PDF).
    Jpeg,
    /// JPEG 2000 (JPXDecode in PDF).
    Jpeg2000,
    /// PNG.
    Png,
    /// TIFF.
    Tiff,
    /// JBIG2 (JBIG2Decode in PDF).
    Jbig2,
    /// CCITT fax (CCITTFaxDecode in PDF).
    Ccitt,
    /// GIF (GIF87a/GIF89a).
    Gif,
    /// BMP (Windows bitmap).
    Bmp,
    /// EMF (Enhanced Metafile).
    Emf,
    /// WMF (Windows Metafile).
    Wmf,
    /// Raw uncompressed pixel data.
    Raw,
}

/// Detect the image format from magic bytes at the start of the data.
///
/// Returns `ImageFilter::Raw` if no known format is recognized. Supports
/// PNG, JPEG, JPEG 2000, GIF, TIFF, JBIG2, BMP, EMF, and WMF detection.
pub fn detect_image_filter(data: &[u8]) -> ImageFilter {
    if data.len() >= 8 {
        if data.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
            return ImageFilter::Png;
        }
        if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
            return ImageFilter::Gif;
        }
        // JPEG 2000 JP2 container: box signature at bytes 4-7.
        if data[0..4] == [0x00, 0x00, 0x00, 0x0C] && data[4..8] == [0x6A, 0x50, 0x20, 0x20] {
            return ImageFilter::Jpeg2000;
        }
        // JBIG2 standalone file header.
        if data.starts_with(&[0x97, 0x4A, 0x42, 0x32, 0x0D, 0x0A, 0x1A, 0x0A]) {
            return ImageFilter::Jbig2;
        }
    }
    if data.len() >= 4 {
        if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
            return ImageFilter::Jpeg;
        }
        // JPEG 2000 codestream (no JP2 container).
        if data.starts_with(&[0xFF, 0x4F, 0xFF, 0x51]) {
            return ImageFilter::Jpeg2000;
        }
        if data.starts_with(&[0x49, 0x49, 0x2A, 0x00])
            || data.starts_with(&[0x4D, 0x4D, 0x00, 0x2A])
        {
            return ImageFilter::Tiff;
        }
        // EMF: record type 1 (EMR_HEADER) as little-endian u32, with the
        // signature " EMF" (0x20454D46) at bytes 40-43.
        if data.len() >= 44
            && data[0] == 0x01
            && data[1] == 0x00
            && data[2] == 0x00
            && data[3] == 0x00
            && data[40] == 0x20
            && data[41] == 0x45
            && data[42] == 0x4D
            && data[43] == 0x46
        {
            return ImageFilter::Emf;
        }
        // WMF: placeable metafile magic 0xD7CDC69A (little-endian)
        if data.starts_with(&[0xD7, 0xCD, 0xC6, 0x9A]) {
            return ImageFilter::Wmf;
        }
    }
    if data.len() >= 2 && data.starts_with(b"BM") {
        return ImageFilter::Bmp;
    }
    ImageFilter::Raw
}

/// Parse pixel dimensions from image header bytes.
///
/// Returns `(width, height)`; `(0, 0)` if the format is unrecognized or the
/// data is too short. Supports PNG (IHDR) and JPEG (SOF marker scanning).
pub fn parse_image_dimensions(data: &[u8]) -> (u32, u32) {
    // PNG: IHDR chunk starts at byte 8, width at 16-19, height at 20-23 (big-endian).
    if data.len() >= 24 && data.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        let w = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
        let h = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
        return (w, h);
    }
    // JPEG: scan for SOF0..SOF15 markers (0xFFC0..0xFFCF, excluding 0xFFC4/0xFFC8/0xFFCC).
    if data.len() >= 4 && data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        let mut i = 2;
        while i + 3 < data.len() {
            if data[i] != 0xFF {
                break;
            }
            let marker = data[i + 1];
            let seg_len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
            // JPEG segment lengths include the 2-byte length field itself.
            // A seg_len < 2 is malformed and would cause an infinite loop.
            if seg_len < 2 {
                break;
            }
            // SOF markers: 0xC0-0xCF except 0xC4 (DHT), 0xC8 (JPG), 0xCC (DAC)
            if (0xC0..=0xCF).contains(&marker)
                && marker != 0xC4
                && marker != 0xC8
                && marker != 0xCC
                && i + 8 < data.len()
            {
                let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
                let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
                return (w, h);
            }
            i = i.saturating_add(2 + seg_len);
        }
    }
    (0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_filter_eq() {
        assert_eq!(ImageFilter::Jpeg, ImageFilter::Jpeg);
        assert_ne!(ImageFilter::Jpeg, ImageFilter::Png);
    }

    #[test]
    fn page_image_clone() {
        let img = PageImage {
            data: vec![0xFF, 0xD8],
            filter: ImageFilter::Jpeg,
            width: 100,
            height: 200,
            bits_per_component: 8,
            bbox: None,
            color_space: None,
            z_index: 0,
            is_mask: false,
            mask_color: [0, 0, 0],
            soft_mask: None,
            soft_mask_width: 0,
            soft_mask_height: 0,
            ctm: None,
        };
        let cloned = img.clone();
        assert_eq!(cloned.width, 100);
        assert_eq!(cloned.filter, ImageFilter::Jpeg);
    }

    #[test]
    fn detect_png() {
        let data = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        assert_eq!(detect_image_filter(&data), ImageFilter::Png);
    }

    #[test]
    fn detect_jpeg() {
        let data = [0xFF, 0xD8, 0xFF, 0xE0];
        assert_eq!(detect_image_filter(&data), ImageFilter::Jpeg);
    }

    #[test]
    fn detect_gif87a() {
        assert_eq!(detect_image_filter(b"GIF87a.."), ImageFilter::Gif);
    }

    #[test]
    fn detect_gif89a() {
        assert_eq!(detect_image_filter(b"GIF89a.."), ImageFilter::Gif);
    }

    #[test]
    fn detect_tiff_le() {
        let data = [0x49, 0x49, 0x2A, 0x00];
        assert_eq!(detect_image_filter(&data), ImageFilter::Tiff);
    }

    #[test]
    fn detect_tiff_be() {
        let data = [0x4D, 0x4D, 0x00, 0x2A];
        assert_eq!(detect_image_filter(&data), ImageFilter::Tiff);
    }

    #[test]
    fn detect_bmp() {
        assert_eq!(detect_image_filter(b"BM"), ImageFilter::Bmp);
    }

    #[test]
    fn detect_wmf() {
        let data = [0xD7, 0xCD, 0xC6, 0x9A];
        assert_eq!(detect_image_filter(&data), ImageFilter::Wmf);
    }

    #[test]
    fn detect_emf() {
        let mut data = vec![0u8; 44];
        data[0] = 0x01; // record type 1
        data[40] = 0x20; // " EMF" signature
        data[41] = 0x45;
        data[42] = 0x4D;
        data[43] = 0x46;
        assert_eq!(detect_image_filter(&data), ImageFilter::Emf);
    }

    #[test]
    fn detect_unknown() {
        assert_eq!(detect_image_filter(&[0x00, 0x01]), ImageFilter::Raw);
        assert_eq!(detect_image_filter(&[]), ImageFilter::Raw);
    }

    #[test]
    fn dimensions_png() {
        // Minimal PNG: 8-byte signature + IHDR chunk (length 13, "IHDR", width=2, height=3).
        let mut data = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x0D]); // IHDR length
        data.extend_from_slice(b"IHDR");
        data.extend_from_slice(&2u32.to_be_bytes()); // width
        data.extend_from_slice(&3u32.to_be_bytes()); // height
        assert_eq!(parse_image_dimensions(&data), (2, 3));
    }

    #[test]
    fn dimensions_jpeg_seg_len_zero() {
        // JPEG with a malformed segment (seg_len=0) should not loop forever.
        let data = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x00, 0xFF, 0xD9, 0x00, 0x00];
        assert_eq!(parse_image_dimensions(&data), (0, 0));
    }

    #[test]
    fn dimensions_jpeg_sof0() {
        // Minimal JPEG with a SOF0 marker reporting 120x80.
        // Layout: SOI (FF D8), APP0 marker (FF E0) with seg_len=16, then SOF0.
        // SOF0 marker: FF C0, len=17, precision=8, height=80, width=120, ncomp=3.
        let mut data = vec![
            0xFF, 0xD8, // SOI
            0xFF, 0xE0, // APP0 marker
            0x00, 0x10, // APP0 seg_len = 16 (includes the 2 length bytes)
        ];
        // 14 bytes of APP0 payload to fill segment (16 - 2 length bytes = 14)
        data.extend_from_slice(&[
            b'J', b'F', b'I', b'F', 0x00, 0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00,
        ]);
        // SOF0 marker
        data.extend_from_slice(&[
            0xFF, 0xC0, // SOF0
            0x00, 0x11, // seg_len = 17
            0x08, // precision
            0x00, 0x50, // height = 80
            0x00, 0x78, // width = 120
            0x03, // ncomp
        ]);
        assert_eq!(parse_image_dimensions(&data), (120, 80));
    }

    #[test]
    fn dimensions_jpeg_truncated_header() {
        // JPEG with only 4 bytes (SOI + marker byte) -- not enough for seg_len.
        assert_eq!(parse_image_dimensions(&[0xFF, 0xD8, 0xFF, 0xE0]), (0, 0));
        // 5 bytes: still too short for seg_len (needs i+3 accessible).
        assert_eq!(
            parse_image_dimensions(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00]),
            (0, 0)
        );
    }

    #[test]
    fn dimensions_unknown() {
        assert_eq!(parse_image_dimensions(&[0x00]), (0, 0));
        assert_eq!(parse_image_dimensions(&[]), (0, 0));
    }
}

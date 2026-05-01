//! Types for extracted images from PDF pages.
//!
//! These types represent images found in PDF content streams, both inline
//! images (BI/ID/EI operators) and XObject images (Do operator).

/// How the image data is encoded.
///
/// For image filters (DCT, JPEG2000, JBIG2, CCITT), the raw encoded bytes
/// are passed through directly. This is useful for OCR pipelines that
/// accept standard image formats (e.g. hand JPEG bytes to Tesseract).
/// For other encodings, the data is fully decoded to raw pixel bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImageFilter {
    /// DCTDecode: JPEG data. Can be written directly as a .jpg file.
    Jpeg,
    /// JPXDecode: JPEG 2000 data.
    Jpeg2000,
    /// JBIG2Decode: JBIG2 compressed data.
    Jbig2,
    /// CCITTFaxDecode: CCITT Group 3 or Group 4 fax encoding.
    Ccitt,
    /// Raw pixel data (after FlateDecode, LZWDecode, RunLengthDecode,
    /// or no filter). For XObject images, the stream decoder fully decodes
    /// transport filters, so this is usable pixel data in the color space
    /// indicated by `PageImage::color_space`.
    Raw,
    /// Data is still transport-encoded (e.g., FlateDecode, LZWDecode for inline images).
    /// The library was unable to decode the transport filter. The raw encoded bytes are
    /// provided as-is. This only occurs for inline images with transport filters.
    TransportEncoded,
}

/// An image extracted from a PDF page.
///
/// Contains the image data and metadata. Images come from two sources:
/// - Inline images: embedded directly in the content stream (BI/ID/EI)
/// - XObject images: referenced by the Do operator from /Resources
///
/// For images with lossy compression (JPEG, JPEG2000), the raw encoded
/// bytes are passed through to avoid re-encoding artifacts. Use the
/// `filter` field to determine how to interpret the `data` bytes.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PageImage {
    /// X position in device space (points, 1/72 inch).
    pub x: f64,
    /// Y position in device space (points, 1/72 inch).
    pub y: f64,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Display width in points (1/72 inch), derived from the CTM.
    ///
    /// This is the rendered size on the page, not the pixel dimensions.
    /// For example, a 100x100 pixel image can be painted as 200x300 points.
    pub display_width: f64,
    /// Display height in points (1/72 inch), derived from the CTM.
    pub display_height: f64,
    /// Color space name (e.g. "DeviceRGB", "DeviceGray", "DeviceCMYK",
    /// "ICCBased", "Indexed").
    pub color_space: String,
    /// Bits per color component (typically 1, 2, 4, or 8).
    pub bits_per_component: u8,
    /// Image data bytes. Interpretation depends on `filter`.
    pub data: Vec<u8>,
    /// How the `data` bytes are encoded.
    pub filter: ImageFilter,
    /// Whether this is an inline image (BI/ID/EI) or XObject image (Do).
    pub inline: bool,
    /// Marked content ID from the enclosing BMC/BDC scope when this image
    /// was painted. Used to look up /Alt text from the structure tree.
    pub mcid: Option<u32>,
    /// Content stream render order. Used to interleave images, shapes, and
    /// text in the correct z-order during rendering.
    pub z_index: u32,
    /// True if this is an image mask (stencil). The 1-bit data defines
    /// where to paint `mask_color`. 1-bits are painted, 0-bits are transparent.
    pub is_mask: bool,
    /// Fill color at the time this image mask was painted (RGB, 0-255).
    /// Only meaningful when `is_mask` is true.
    pub mask_color: [u8; 3],
    /// Soft mask (SMask) alpha channel. When present, each byte is an
    /// alpha value (0=transparent, 255=opaque) for the corresponding pixel.
    /// Dimensions may differ from the image; scale to match when rendering.
    pub soft_mask: Option<Vec<u8>>,
    /// Soft mask width in pixels.
    pub soft_mask_width: u32,
    /// Soft mask height in pixels.
    pub soft_mask_height: u32,
    /// Full affine CTM mapping the source unit square (0,0)-(1,1) to page
    /// user space (y-up). Preserves rotation/shear that the AABB `(x, y,
    /// display_width, display_height)` loses. For pure scale+translate
    /// placements this is `[display_width, 0, 0, display_height, x, y]`.
    pub ctm: [f64; 6],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_image_filter_variants() {
        assert_ne!(ImageFilter::Jpeg, ImageFilter::Raw);
        assert_eq!(ImageFilter::Jpeg, ImageFilter::Jpeg);
    }

    #[test]
    fn test_page_image_construction() {
        let img = PageImage {
            x: 72.0,
            y: 720.0,
            width: 100,
            height: 200,
            display_width: 150.0,
            display_height: 300.0,
            color_space: "DeviceRGB".to_string(),
            bits_per_component: 8,
            data: vec![0xFF; 100 * 200 * 3],
            filter: ImageFilter::Raw,
            inline: false,
            mcid: None,
            z_index: 0,
            is_mask: false,
            mask_color: [0, 0, 0],
            soft_mask: None,
            soft_mask_width: 0,
            soft_mask_height: 0,
            ctm: [150.0, 0.0, 0.0, 300.0, 72.0, 720.0],
        };
        assert_eq!(img.width, 100);
        assert_eq!(img.height, 200);
        assert_eq!(img.display_width, 150.0);
        assert_eq!(img.display_height, 300.0);
        assert!(!img.inline);
    }
}

//! Text output types for document extraction.
//!
//! These types represent the output of text extraction across all
//! format backends. They carry extracted text with position and style
//! metadata, enabling reading order reconstruction and structured access.

use crate::document::presentation::Color;
use crate::geometry::BoundingBox;

/// Why a font could not be loaded as requested.
///
/// Populated by the font loader (PDF or other format) when a span's font
/// is not an exact match for what the document referenced. Downstream
/// consumers use this to audit or filter text whose rendering fidelity
/// is suspect.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(tag = "kind", content = "detail")
)]
#[non_exhaustive]
pub enum FallbackReason {
    /// Font not embedded in the source document and not found by name
    /// in the bundle or system.
    NotEmbedded,
    /// Font was embedded but corrupt or unparseable. The string carries
    /// the loader's detail message for auditing.
    EmbeddedCorrupt(String),
    /// CID font without ToUnicode and no known predefined CMap, so
    /// character decoding fell back to an identity mapping.
    CidNoToUnicode,
    /// Font name matched a known family (Helvetica, Times, Arial, CMR, ...)
    /// and was routed to a bundled or standard equivalent.
    NameRouted,
    /// Unicode range match routed to a fallback (e.g. CJK characters in a
    /// non-CJK font).
    UnicodeRangeRouted,
    /// Catch-all for a non-Exact [`FontResolution`] variant that was added
    /// after this enum. Used by forward-compat catch-alls so typed callers
    /// always get a [`crate::error::FontFallbackRequired`] payload, even when
    /// the concrete reason isn't known yet. Prefer a specific variant when
    /// one exists.
    Unknown,
}

impl FallbackReason {
    /// Short, stable display form suitable for warnings and JSON output.
    pub fn as_str(&self) -> &'static str {
        match self {
            FallbackReason::NotEmbedded => "NotEmbedded",
            FallbackReason::EmbeddedCorrupt(_) => "EmbeddedCorrupt",
            FallbackReason::CidNoToUnicode => "CidNoToUnicode",
            FallbackReason::NameRouted => "NameRouted",
            FallbackReason::UnicodeRangeRouted => "UnicodeRangeRouted",
            FallbackReason::Unknown => "Unknown",
        }
    }
}

/// Outcome of resolving a font reference for a span.
///
/// `Exact` means the document's font was loaded as-is. The other variants
/// indicate that the text was decoded or rendered using something other than
/// the requested font, which may silently alter the extracted text. Every
/// TextSpan carries one of these to let callers audit or filter unreliable
/// text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(tag = "status")
)]
#[non_exhaustive]
pub enum FontResolution {
    /// Font loaded exactly as referenced in the PDF/document.
    #[default]
    Exact,
    /// Font was substituted from the bundled fallback library
    /// (e.g. CMR10 routed to Latin Modern Roman equivalent).
    Substituted {
        /// Font name as referenced in the document.
        requested: String,
        /// Font name actually loaded.
        resolved: String,
        /// Why the substitution happened.
        reason: FallbackReason,
    },
    /// Synthesized fallback (last resort, likely inaccurate geometry).
    /// Used when no named match exists and we fall back to a generic family.
    SyntheticFallback {
        /// Font name as referenced in the document.
        requested: String,
        /// Generic family used (`"serif"`, `"sans-serif"`, `"monospace"`).
        generic_family: String,
        /// Why the substitution happened.
        reason: FallbackReason,
    },
}

impl FontResolution {
    /// Whether this span used the exact font requested by the document.
    pub fn is_exact(&self) -> bool {
        matches!(self, FontResolution::Exact)
    }

    /// Whether this span used some form of fallback (substituted or synthetic).
    pub fn is_fallback(&self) -> bool {
        !self.is_exact()
    }
}

/// A span of text with position and style metadata.
///
/// Each span represents a contiguous run of text with consistent styling
/// and position. Format backends produce TextSpans; the unified API
/// consumes them for reading order reconstruction and structured output.
///
/// Coordinate system: positions are in points (1/72 inch). PDF uses a
/// bottom-left origin (x=0 is left edge, y=0 is bottom edge). Other
/// paginated formats (PPTX) may use top-left origin. Flow-based formats
/// (DOCX, ODT, RTF) typically set x=0, y=0 since geometry is implicit.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TextSpan {
    /// Unicode text content.
    pub text: String,
    /// X position in page coordinates (points from page left).
    pub x: f64,
    /// Y position in page coordinates (points from page bottom).
    pub y: f64,
    /// Width of the span in points.
    pub width: f64,
    /// Font name, if available. PDF always provides this; other formats
    /// may not (e.g., plain text extraction from some XLSX cells).
    pub font_name: Option<String>,
    /// Font size in points.
    pub font_size: f64,
    /// Whether the text is bold.
    pub is_bold: bool,
    /// Whether the text is italic.
    pub is_italic: bool,
    /// Whether this text was invisible (PDF Tr=3, DOCX hidden text, etc.).
    /// Invisible text is commonly used for OCR overlays on scanned documents.
    pub is_invisible: bool,
    /// Rotation angle in degrees (0 = horizontal, 90 = vertical upward).
    pub rotation: f64,
    /// Fill color as RGB, if not black/default.
    /// None means unspecified or black (the common case).
    pub color: Option<Color>,
    /// Character spacing in text space units, if non-zero.
    pub letter_spacing: Option<f64>,
    /// Whether text rise indicates superscript positioning.
    pub is_superscript: bool,
    /// Whether text rise indicates subscript positioning.
    pub is_subscript: bool,
    /// Per-character advance widths in text space, if available.
    /// Each entry corresponds to one character in `text` (by chars()).
    /// Multiply by `advance_scale` to convert to user-space (page coordinates).
    pub char_advances: Option<Vec<f64>>,
    /// Text-space to user-space horizontal scaling factor.
    pub advance_scale: f64,
    /// Original character codes from the PDF content stream (one byte per
    /// character for simple fonts). Used for by-code glyph lookup in subset
    /// fonts with custom encodings. None for non-PDF backends.
    pub char_codes: Option<Vec<u8>>,
    /// Glyph IDs for composite (CID) fonts. Each entry is the raw 2-byte
    /// character code from the PDF content stream, which equals the GID for
    /// Identity-H/V encodings. None for simple fonts and non-PDF backends.
    pub char_gids: Option<Vec<u16>>,
    /// Per-glyph bounding boxes in format-native page coordinates (PDF: points, y-up).
    ///
    /// Populated by PDF extraction. None for non-PDF backends or when bbox
    /// computation was skipped. The length corresponds to GLYPHS rendered,
    /// not characters: a single ligature glyph mapped to "fi" via ToUnicode
    /// produces one bbox for both chars. When `char_advances` is Some, its
    /// length equals this vector's length (glyph count matches char count).
    /// Otherwise use this vector's length as the glyph count.
    ///
    /// Each bbox is axis-aligned in page space. For rotated text the bbox
    /// is the axis-aligned bounding box of the rotated glyph rectangle.
    pub glyph_bboxes: Option<Vec<BoundingBox>>,
    /// Content stream render order for z-ordering. 0 = no ordering info.
    pub z_index: u32,
    /// Unique per-subset font identifier. None for non-PDF backends and for
    /// fonts without subset information.
    ///
    /// PDF fonts can share the same display `font_name` across multiple
    /// subsets (each subset carries a distinct 6-letter prefix like "ABCDEF+");
    /// the stripped display name collides across subsets. This field preserves
    /// the full subset-prefixed name (or another unique identifier) so renderers
    /// can distinguish subsets and avoid cross-subset glyph program collisions.
    /// When absent, downstream consumers should fall back to `font_name`.
    pub font_id: Option<String>,
    /// How the span's font was resolved.
    ///
    /// [`FontResolution::Exact`] means the document's font was loaded as-is.
    /// Other variants indicate a fallback, which may change the extracted text
    /// (wrong decoding) or geometry (wrong widths). Callers can filter on
    /// `font_resolution.is_fallback()` to flag suspect text.
    pub font_resolution: FontResolution,
}

impl TextSpan {
    /// Create a new TextSpan with required fields. Style fields default to
    /// false/0.0/None.
    pub fn new(text: String, x: f64, y: f64, width: f64, font_size: f64) -> Self {
        Self {
            text,
            x,
            y,
            width,
            font_name: None,
            font_size,
            is_bold: false,
            is_italic: false,
            is_invisible: false,
            rotation: 0.0,
            color: None,
            letter_spacing: None,
            is_superscript: false,
            is_subscript: false,
            char_advances: None,
            advance_scale: 1.0,
            char_codes: None,
            char_gids: None,
            glyph_bboxes: None,
            z_index: 0,
            font_id: None,
            font_resolution: FontResolution::Exact,
        }
    }

    /// Look up the bounding box for the glyph covering `char_index`
    /// (a 0-based index into `text.chars()`).
    ///
    /// Returns None when glyph bboxes are not populated, when the index
    /// is out of range, or when there's a glyph/char count mismatch
    /// (e.g. a ligature glyph expanded to multiple chars via ToUnicode).
    pub fn glyph_bbox_for_char_index(&self, char_index: usize) -> Option<BoundingBox> {
        let bboxes = self.glyph_bboxes.as_ref()?;
        let char_count = self.text.chars().count();
        if bboxes.len() != char_count {
            return None;
        }
        bboxes.get(char_index).copied()
    }

    /// Create a TextSpan with positional and style fields.
    ///
    /// Additional per-span fields (color, letter_spacing, is_superscript,
    /// is_subscript) default to None/false and can be set afterward.
    #[allow(clippy::too_many_arguments)]
    pub fn with_style(
        text: String,
        x: f64,
        y: f64,
        width: f64,
        font_name: Option<String>,
        font_size: f64,
        is_bold: bool,
        is_italic: bool,
        is_invisible: bool,
        rotation: f64,
    ) -> Self {
        Self {
            text,
            x,
            y,
            width,
            font_name,
            font_size,
            is_bold,
            is_italic,
            is_invisible,
            rotation,
            color: None,
            letter_spacing: None,
            is_superscript: false,
            is_subscript: false,
            char_advances: None,
            advance_scale: 1.0,
            char_codes: None,
            char_gids: None,
            glyph_bboxes: None,
            z_index: 0,
            font_id: None,
            font_resolution: FontResolution::Exact,
        }
    }
}

/// A line of text: spans grouped by baseline proximity.
///
/// Spans within a line are sorted left-to-right (or top-to-bottom for
/// vertical lines). Lines are produced by baseline clustering from raw spans.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TextLine {
    /// Spans in this line, sorted by position.
    pub spans: Vec<TextSpan>,
    /// Baseline coordinate. For horizontal text, this is the Y coordinate.
    /// For vertical text, this is the X coordinate.
    pub baseline: f64,
    /// Whether this line contains vertical text (e.g., CJK vertical mode).
    pub is_vertical: bool,
}

impl TextLine {
    /// Create a new TextLine.
    pub fn new(spans: Vec<TextSpan>, baseline: f64, is_vertical: bool) -> Self {
        Self {
            spans,
            baseline,
            is_vertical,
        }
    }

    /// Concatenate all span text in this line.
    pub fn text(&self) -> String {
        let capacity: usize = self.spans.iter().map(|s| s.text.len()).sum();
        let mut out = String::with_capacity(capacity);
        for s in &self.spans {
            out.push_str(&s.text);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_span_new() {
        let span = TextSpan::new("hello".into(), 10.0, 20.0, 50.0, 12.0);
        assert_eq!(span.text, "hello");
        assert_eq!(span.x, 10.0);
        assert_eq!(span.font_size, 12.0);
        assert!(span.font_name.is_none());
        assert!(!span.is_bold);
        assert!(!span.is_invisible);
    }

    #[test]
    fn text_line_text() {
        let line = TextLine::new(
            vec![
                TextSpan::new("hello".into(), 0.0, 0.0, 30.0, 12.0),
                TextSpan::new(" world".into(), 30.0, 0.0, 30.0, 12.0),
            ],
            100.0,
            false,
        );
        assert_eq!(line.text(), "hello world");
    }

    #[test]
    fn text_line_vertical() {
        let line = TextLine::new(vec![], 50.0, true);
        assert!(line.is_vertical);
        assert_eq!(line.text(), "");
    }

    #[test]
    fn font_resolution_default_is_exact() {
        let r: FontResolution = FontResolution::default();
        assert!(r.is_exact());
        assert!(!r.is_fallback());
    }

    #[test]
    fn text_span_carries_exact_resolution_by_default() {
        let span = TextSpan::new("hi".into(), 0.0, 0.0, 10.0, 12.0);
        assert!(span.font_resolution.is_exact());
    }

    #[test]
    fn fallback_reason_variants_have_stable_display_form() {
        assert_eq!(FallbackReason::NotEmbedded.as_str(), "NotEmbedded");
        assert_eq!(
            FallbackReason::EmbeddedCorrupt("blah".into()).as_str(),
            "EmbeddedCorrupt"
        );
        assert_eq!(FallbackReason::CidNoToUnicode.as_str(), "CidNoToUnicode");
        assert_eq!(FallbackReason::NameRouted.as_str(), "NameRouted");
        assert_eq!(
            FallbackReason::UnicodeRangeRouted.as_str(),
            "UnicodeRangeRouted"
        );
    }

    #[test]
    fn substituted_is_not_exact() {
        let r = FontResolution::Substituted {
            requested: "CMR10".into(),
            resolved: "Latin Modern Roman".into(),
            reason: FallbackReason::NameRouted,
        };
        assert!(!r.is_exact());
        assert!(r.is_fallback());
    }

    #[test]
    fn synthetic_fallback_is_not_exact() {
        let r = FontResolution::SyntheticFallback {
            requested: "MysteryFont".into(),
            generic_family: "serif".into(),
            reason: FallbackReason::NotEmbedded,
        };
        assert!(!r.is_exact());
        assert!(r.is_fallback());
    }

    #[test]
    fn embedded_corrupt_carries_detail() {
        let r = FallbackReason::EmbeddedCorrupt("zlib decode failed".into());
        match r {
            FallbackReason::EmbeddedCorrupt(detail) => {
                assert_eq!(detail, "zlib decode failed");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn glyph_bbox_for_char_index_one_to_one() {
        use crate::geometry::BoundingBox;
        let mut span = TextSpan::new("AB".into(), 0.0, 0.0, 20.0, 12.0);
        span.glyph_bboxes = Some(vec![
            BoundingBox::new(0.0, 0.0, 10.0, 12.0),
            BoundingBox::new(10.0, 0.0, 20.0, 12.0),
        ]);
        assert!((span.glyph_bbox_for_char_index(0).unwrap().x_min - 0.0).abs() < 1e-10);
        assert!((span.glyph_bbox_for_char_index(1).unwrap().x_min - 10.0).abs() < 1e-10);
        assert!(span.glyph_bbox_for_char_index(2).is_none());
    }

    #[test]
    fn glyph_bbox_for_char_index_ligature_returns_none() {
        use crate::geometry::BoundingBox;
        let mut lig = TextSpan::new("fi".into(), 0.0, 0.0, 10.0, 12.0);
        lig.glyph_bboxes = Some(vec![BoundingBox::new(0.0, 0.0, 10.0, 12.0)]);
        assert!(lig.glyph_bbox_for_char_index(0).is_none());
        assert!(lig.glyph_bbox_for_char_index(1).is_none());
    }

    #[cfg(feature = "serde")]
    #[test]
    fn font_resolution_serializes_to_tagged_json() {
        let exact = FontResolution::Exact;
        let json = serde_json::to_string(&exact).unwrap();
        assert!(json.contains("\"status\""), "got: {json}");
        assert!(json.contains("\"Exact\""), "got: {json}");

        let sub = FontResolution::Substituted {
            requested: "Helvetica".into(),
            resolved: "standard-14 Helvetica".into(),
            reason: FallbackReason::NameRouted,
        };
        let json = serde_json::to_string(&sub).unwrap();
        assert!(json.contains("\"Substituted\""));
        assert!(json.contains("\"NameRouted\""));
    }
}

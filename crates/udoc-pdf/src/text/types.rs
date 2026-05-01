//! Text output types for PDF text extraction.
//!
//! These types represent the output of content stream interpretation.
//! They carry extracted text with position and font metadata, enabling
//! reading order reconstruction and structured text access.
//!
//! Tiered API design:
//! - `TextSpan`: raw positioned text from a single Tj/TJ operation
//! - `TextLine`: spans grouped by baseline proximity, sorted left-to-right

use std::sync::Arc;

use udoc_core::geometry::BoundingBox;
use udoc_core::text::FontResolution;

/// A span of text extracted from a PDF content stream.
///
/// Each span represents one text-showing operation (Tj or TJ) with its
/// position in device space. Spans carry enough metadata for reading
/// order reconstruction and debugging font issues.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TextSpan {
    /// Unicode text content.
    pub text: String,
    /// X position in device space (points, 1/72 inch).
    pub x: f64,
    /// Y position in device space (points, 1/72 inch).
    /// PDF origin is bottom-left; Y increases upward.
    pub y: f64,
    /// Width of the span in device space (points).
    /// Computed from glyph widths during content stream interpretation.
    pub width: f64,
    /// Font name (subset prefix stripped, e.g. "Helvetica" not "ABCDEF+Helvetica").
    ///
    /// Stored as `Arc<str>` so that spans sharing the same font share one
    /// heap allocation (clone is a refcount bump, not a new allocation).
    /// The intern cache lives in the content interpreter and persists across
    /// pages via the `Document`.
    pub font_name: Arc<str>,
    /// Font size in points.
    pub font_size: f64,
    /// Rotation angle in degrees (0 = horizontal, 90 = vertical upward,
    /// -90 = vertical downward). Computed from the text rendering matrix.
    /// Non-zero rotation causes the span to be grouped separately from
    /// horizontal text during reading order reconstruction.
    pub rotation: f64,
    /// Whether this span uses vertical writing mode (CJK).
    /// Vertical text flows top-to-bottom within a column.
    pub is_vertical: bool,
    /// Marked content ID from BDC operator, if inside a marked content sequence.
    /// Used for structure-tree-based reading order in tagged PDFs.
    ///
    /// Not public: MCID is an internal implementation detail of reading order
    /// reconstruction. Exposing it would commit us to a structural detail that
    /// may change (e.g., if we switch to a richer marked content model).
    pub(crate) mcid: Option<u32>,
    /// Scaled width of the space glyph (char 0x20) in device space points.
    /// Used by the spacing algorithm to determine word boundaries more
    /// accurately than font-size-relative heuristics alone.
    /// None when the font has no space glyph or the width is unreasonable.
    pub(crate) space_width: Option<f64>,
    /// Whether the font has explicit width metrics (/Widths for simple fonts,
    /// /W table for composite fonts). When false, char widths are estimates
    /// (standard font fallback or default 600), and spacing decisions should
    /// use more conservative thresholds.
    #[allow(dead_code)] // reserved for conservative-threshold spacing logic
    pub(crate) has_font_metrics: bool,
    /// Whether this text was rendered with rendering mode 3 (invisible).
    /// Invisible text is commonly used for OCR overlays on scanned documents.
    /// The text is extracted for search/indexing but was not visually rendered.
    ///
    /// To exclude invisible text, filter spans after extraction:
    /// ```no_run
    /// # let mut doc = udoc_pdf::Document::open("doc.pdf").unwrap();
    /// # let mut page = doc.page(0).unwrap();
    /// let mut spans = page.raw_spans().unwrap();
    /// spans.retain(|s| !s.is_invisible);
    /// ```
    pub is_invisible: bool,
    /// Whether this span was extracted from an annotation appearance stream
    /// rather than the main page content stream. Annotation text comes from
    /// form fields (Widget), stamps, free text annotations, etc.
    pub is_annotation: bool,

    /// Fill color as RGB triplet, if not black.
    ///
    /// None means black ([0, 0, 0]), the overwhelmingly common case.
    /// Populated from the graphics state non-stroking color (rg/g/k operators).
    /// DeviceRGB values are stored directly; DeviceGray and DeviceCMYK are
    /// converted to RGB.
    pub color: Option<[u8; 3]>,

    /// Character spacing (Tc) in text space units, if non-zero.
    ///
    /// Forwarded from the graphics state. None means zero (default).
    /// Consumers can use this for letter-spacing in rendered output.
    pub letter_spacing: Option<f64>,

    /// Whether text rise (Ts) indicates superscript positioning.
    ///
    /// True when the graphics state text rise is positive, meaning the
    /// text is shifted upward from the baseline.
    pub is_superscript: bool,

    /// Whether text rise (Ts) indicates subscript positioning.
    ///
    /// True when the graphics state text rise is negative, meaning the
    /// text is shifted downward from the baseline.
    pub is_subscript: bool,

    /// Per-character advance widths in text space.
    ///
    /// Each entry corresponds to one character code processed by the content
    /// interpreter. When the count matches `text.chars().count()`, the renderer
    /// uses these for exact character positioning. When it doesn't match
    /// (ligature mappings, etc.), the renderer falls back to proportional
    /// distribution.
    ///
    /// To convert to user-space (page) coordinates, multiply by `advance_scale`.
    pub(crate) char_advances: Option<Vec<f64>>,
    /// Text-space to user-space horizontal scaling factor.
    ///
    /// This is the horizontal component of the text rendering matrix (Tm * CTM)
    /// at the time the span was created. Multiply by char_advances to get
    /// user-space (page coordinate) advances. Then multiply by DPI scale to
    /// get pixel advances.
    ///
    /// This avoids the bbox-normalization approach which compresses character
    /// spacing due to sidebearing trimming in the bounding box.
    pub(crate) advance_scale: f64,
    /// Original character codes from the content stream (one byte per char
    /// for simple fonts). Used by the renderer to look up glyph outlines
    /// by encoding position for subset fonts with custom encodings.
    pub(crate) char_codes: Option<Vec<u8>>,
    /// Glyph IDs for composite (CID) fonts. Each entry is the raw 2-byte
    /// character code, which equals the GID for Identity-H/V encodings.
    pub(crate) char_gids: Option<Vec<u16>>,
    /// Per-glyph bounding boxes in PDF user space (points, y-up origin).
    ///
    /// One entry per glyph rendered by this Tj/TJ operation. Length equals
    /// the number of glyph codes processed; this is NOT necessarily the
    /// same as `text.chars().count()` because a single glyph can map to
    /// multiple characters via ToUnicode ligature expansion (e.g. one "fi"
    /// glyph maps to chars 'f' and 'i'). Use `char_advances.len()` as the
    /// glyph count when char_advances is Some (they are populated together
    /// only when glyph count == char count); otherwise the glyph count is
    /// the length of this vector.
    ///
    /// Each bbox is axis-aligned in user space. For rotated text the bbox
    /// is the axis-aligned bounding box of the rotated glyph rectangle.
    ///
    /// None when bbox computation was skipped (no font loaded, zero font
    /// size, or a malformed text rendering matrix).
    pub glyph_bboxes: Option<Vec<BoundingBox>>,
    /// Content stream render order for z-ordering.
    pub(crate) z_index: u32,

    /// Raw font name including any subset prefix (e.g. "ABCDEF+Helvetica").
    ///
    /// `font_name` carries the subset-prefix-stripped display name, which can
    /// collide across multiple subsets of the same font within a single PDF.
    /// This field preserves the unique prefixed name so downstream consumers
    /// (notably the renderer font cache) can distinguish subsets and avoid
    /// cross-subset glyph-program collisions. None when the font has no base
    /// font name at all (rare).
    pub font_id: Option<Arc<str>>,
    /// How the font backing this span was resolved. `Exact` for fonts loaded
    /// as referenced; `Substituted`/`SyntheticFallback` for any fallback path
    /// (missing embedded program, ToUnicode absent, standard-14 routing, etc.).
    ///
    /// Every span in a document with suspect fonts carries the same resolution
    /// for the font that produced it; filter with `font_resolution.is_fallback()`
    /// to flag text whose accuracy may be degraded.
    pub font_resolution: FontResolution,
    /// Active clipping regions at text-showing time (
    /// ISO 32000-2 §8.5.4). Empty = no clipping.
    pub active_clips: Vec<crate::table::ClipPathIR>,
}

impl TextSpan {
    /// Create a new TextSpan with the given public fields.
    ///
    /// Internal fields (mcid, space_width, has_font_metrics) are set to
    /// their defaults. This constructor exists so external code (tests,
    /// fuzz targets) can build spans without access to pub(crate) fields.
    pub fn new(
        text: String,
        x: f64,
        y: f64,
        width: f64,
        font_name: impl Into<Arc<str>>,
        font_size: f64,
    ) -> Self {
        let font_name = font_name.into();
        Self {
            text,
            x,
            y,
            width,
            font_name,
            font_size,
            rotation: 0.0,
            is_vertical: false,
            mcid: None,
            space_width: None,
            has_font_metrics: false,
            is_invisible: false,
            is_annotation: false,
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
            active_clips: Vec::new(),
        }
    }
}

/// A line of text: spans grouped by baseline proximity.
///
/// Spans in a line are sorted left-to-right. Lines are produced
/// by baseline clustering from raw spans.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TextLine {
    /// Spans in this line, sorted left-to-right by x position
    /// (or top-to-bottom for vertical lines where `is_vertical` is true).
    pub spans: Vec<TextSpan>,
    /// Common baseline Y coordinate for horizontal text, or column X coordinate
    /// for vertical text. Check `is_vertical` to determine which axis this represents.
    pub baseline: f64,
    /// Whether this line contains vertical CJK text.
    /// When true, `baseline` is a column X coordinate (columns flow
    /// right-to-left) and spans flow top-to-bottom within the column.
    pub is_vertical: bool,
}

impl TextLine {
    /// Concatenate all span text in this line.
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

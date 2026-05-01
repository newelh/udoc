//! Public API for PDF text extraction.
//!
//! The entry point is [`Document`], which owns the PDF data and provides
//! access to pages. Each [`Page`] can extract text at three levels of detail:
//! raw spans, ordered lines, or full text.
//!
//! ```no_run
//! use udoc_pdf::Document;
//!
//! let mut doc = Document::open("example.pdf")?;
//! for i in 0..doc.page_count() {
//!     let mut page = doc.page(i)?;
//!     println!("{}", page.text()?);
//! }
//! # Ok::<(), udoc_pdf::Error>(())
//! ```

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::content::interpreter::{get_page_content, ContentInterpreter};
use crate::content::marked_content::{self, AltTextIndex, StructureTree};
use crate::crypt::CryptHandler;
use crate::diagnostics::{DiagnosticsSink, NullDiagnostics, Warning, WarningContext, WarningKind};
use crate::error::{EncryptionErrorKind, Error, ResultExt};
use crate::geometry::BoundingBox;
use crate::image::PageImage;
use crate::object::resolver::ObjectResolver;
use crate::object::stream::DecodeLimits;
use crate::object::{ObjRef, PdfDictionary, PdfObject, PdfString};
use crate::parse::document_parser::DocumentParser;
use crate::table::{extract_tables, PathSegment, Table};
use crate::text::order::{order_spans, order_spans_with_diagnostics, OrderDiagnostics};
use crate::text::{TextLine, TextSpan};
use crate::text_decode::decode_pdf_text_bytes;
use crate::Result;
use udoc_core::backend::DocumentMetadata;

/// Shared cache of extracted font programs, keyed by raw PDF BaseFont name
/// (including any subset prefix so distinct subsets stay separate).
/// Populated during page interpretation. Font data + program type +
/// optional encoding map + optional parsed /W widths (composite fonts only).
pub type FontCacheEntry = (
    Vec<u8>,
    udoc_font::types::FontProgram,
    Option<Vec<(u8, String)>>,
    Option<(u32, Vec<(u32, f64)>)>,
);
type ExtractedFontCache = Arc<Mutex<HashMap<String, FontCacheEntry>>>;

/// Shared cache of Type3 font metadata for outline extraction.
type ExtractedType3Cache = Arc<Mutex<HashMap<String, crate::content::interpreter::Type3FontInfo>>>;

/// Join text lines into a single newline-separated string.
fn join_text_lines(lines: &[TextLine]) -> String {
    let mut text = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            text.push('\n');
        }
        text.push_str(&line.text());
    }
    text
}

/// Combined result of a single content stream interpretation pass.
///
/// Contains both text spans and images extracted from the page, avoiding
/// the need to interpret the content stream twice when both are needed.
/// Returned by [`Page::extract()`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PageContent {
    /// Raw text spans in content stream order (same as [`Page::raw_spans()`]).
    pub spans: Vec<TextSpan>,
    /// Images found in the content stream (same as [`Page::images()`]).
    pub images: Vec<PageImage>,
    /// Path segments (lines, rectangles) from the content stream.
    /// Only populated when path extraction is enabled (e.g., via [`Page::tables()`]
    /// or [`Page::paths()`]).
    pub paths: Vec<PathSegment>,
}

/// All extracted content from a single page, produced by
/// [`Page::extract_all()`].
///
/// Unlike [`PageContent`] (which contains raw content stream output),
/// this includes post-processed text lines with reading order applied
/// and detected tables. One content stream interpretation produces
/// everything, avoiding the 4x redundant passes that separate
/// `text_lines()` / `tables()` / `images()` / `raw_spans()` calls incur.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FullPageContent {
    /// Text lines in reading order (structure-tree-aware when available).
    pub text_lines: Vec<TextLine>,
    /// Raw text spans in content stream order.
    pub raw_spans: Vec<TextSpan>,
    /// Detected tables with cell text.
    pub tables: Vec<Table>,
    /// Extracted images with optional alt text from the structure tree.
    pub images: Vec<AnnotatedImage<PageImage>>,
}

/// An image paired with its alt text from the document's structure/accessibility tree.
///
/// Generic over the image type so both the internal PDF `PageImage` and the
/// core `udoc_core::image::PageImage` can use the same wrapper.
#[derive(Debug, Clone)]
pub struct AnnotatedImage<I> {
    /// The image data and metadata.
    pub image: I,
    /// Alternative text from the structure tree (/Alt in PDF, alt attribute
    /// in OOXML/ODF). None when the image has no accessibility description.
    pub alt_text: Option<String>,
}

impl PageContent {
    /// Order the raw spans into text lines.
    ///
    /// Applies baseline clustering, word spacing, and reading order
    /// heuristics (multi-column detection, table hints, rotation grouping).
    /// Same result as [`Page::text_lines()`] but without re-interpreting
    /// the content stream. Clones the spans; use [`into_text_lines`](Self::into_text_lines)
    /// to avoid the clone if you don't need the raw spans afterward.
    pub fn text_lines(&self) -> Vec<TextLine> {
        order_spans(self.spans.clone())
    }

    /// Order the raw spans into text lines, consuming self.
    ///
    /// Same as [`text_lines`](Self::text_lines) but avoids cloning the spans.
    pub fn into_text_lines(self) -> Vec<TextLine> {
        order_spans(self.spans)
    }

    /// Concatenate ordered text lines into a single string.
    ///
    /// Uses the same joining logic as [`Page::text()`] but without
    /// re-interpreting the content stream. Clones the spans; use
    /// [`into_text`](Self::into_text) to avoid the clone.
    pub fn text(&self) -> String {
        let lines = self.text_lines();
        join_text_lines(&lines)
    }

    /// Concatenate ordered text lines into a single string, consuming self.
    ///
    /// Same as [`text`](Self::text) but avoids cloning the spans.
    pub fn into_text(self) -> String {
        let lines = order_spans(self.spans);
        join_text_lines(&lines)
    }
}

/// Maximum depth for /Pages tree traversal.
const MAX_PAGE_TREE_DEPTH: usize = 64;

/// Maximum number of pages collected from the page tree.
/// Prevents memory exhaustion from malicious PDFs with millions of /Page entries.
const MAX_PAGES: usize = 100_000;

/// Maximum number of annotations processed per page.
/// Prevents CPU exhaustion from malicious PDFs with thousands of annotations.
const MAX_ANNOTATIONS_PER_PAGE: usize = 500;

/// Maximum depth for bookmark /Outlines tree traversal.
const MAX_BOOKMARK_DEPTH: usize = 64;

/// Maximum total number of bookmarks collected from the outline tree.
/// Prevents unbounded allocation from malicious PDFs with millions of sibling entries.
const MAX_BOOKMARK_COUNT: usize = 10_000;

/// Maximum recursion depth for resolving indirect /Dest references in links.
const MAX_LINK_DEST_DEPTH: usize = 10;

/// Maximum depth for name tree traversal (/Kids nesting).
const MAX_NAME_TREE_DEPTH: usize = 32;

/// Maximum number of name tree entries scanned before giving up.
/// Prevents CPU exhaustion from huge name trees.
const MAX_NAME_TREE_ENTRIES: usize = 50_000;

/// Maximum length of an extracted URI string (bytes).
/// Prevents memory exhaustion from adversarial annotations with multi-MB URIs.
const MAX_URI_LENGTH: usize = 65_536;

/// A hyperlink extracted from a /Link annotation.
#[derive(Debug, Clone)]
pub struct PageLink {
    /// The URL or internal destination.
    pub url: String,
    /// Bounding box on the page (if /Rect is present).
    pub bbox: Option<BoundingBox>,
}

/// Kind of an annotation the renderer needs to draw (
/// #170). Mirrors the subset of ISO 32000-2 §12.5.6 subtypes that ship
/// with a visual appearance. Link/popup/etc. that are navigation-only
/// (no visible ink beyond an optional border) map to `Link`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageAnnotationKind {
    /// /Highlight (§12.5.6.10). Yellow (or /C) multiply overlay across
    /// /QuadPoints.
    Highlight,
    /// /Underline (§12.5.6.10). Line at the bottom edge of each quad.
    Underline,
    /// /StrikeOut (§12.5.6.10). Line at the midline of each quad.
    StrikeOut,
    /// /Squiggly (§12.5.6.10). Wavy line at the bottom of each quad.
    Squiggly,
    /// /Stamp (§12.5.6.12). Typically relies on /AP/N to draw
    /// "CONFIDENTIAL", "DRAFT", etc.
    Stamp,
    /// /Watermark (§12.5.6.22). Optional /Fixed rendering not yet
    /// supported; falls back to /AP/N composition.
    Watermark,
    /// /Link (§12.5.6.5). Visual part only (optional /Border rectangle);
    /// navigation handled by [`Page::links`].
    Link,
    /// /FreeText (§12.5.6.6). /Contents drawn inside /Rect using /DA.
    FreeText,
    /// /Ink (§12.5.6.13). Hand-drawn stroked paths from /InkList.
    Ink,
    /// Catch-all for other subtypes that still ship with /AP/N.
    OtherWithAppearance,
}

/// An annotation enumerated for the renderer.
///
/// Carries the raw dictionary fields the renderer needs to synthesize a
/// visual appearance under §12.5.5 composition. For subtypes with a
/// pre-rendered `/AP/N` stream (Stamp, Watermark, FreeText, some Links)
/// the `ap_stream` + `ap_resources` + `ap_bbox` + `ap_matrix` fields
/// carry the content stream and its coordinate frame so the renderer can
/// map BBox -> Rect per the composition equation. For the text-markup
/// family (Highlight/Underline/StrikeOut/Squiggly) `quad_points` locates
/// the marked words directly in page user space; no AP is needed.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PageAnnotation {
    /// Subtype classification for renderer dispatch.
    pub kind: PageAnnotationKind,
    /// /Rect in page user space (normalized so x_min<=x_max, y_min<=y_max).
    pub rect: udoc_core::geometry::BoundingBox,
    /// /F flag bits (ISO 32000-2 §12.5.3). `Hidden` (bit 2, 0x2) and
    /// `NoView` (bit 6, 0x20) suppress rendering; callers filter on this
    /// before emission.
    pub flags: u32,
    /// /C colour (RGB, 0-255). Defaults depend on subtype
    /// (yellow for Highlight, black for border strokes).
    pub color: Option<[u8; 3]>,
    /// /QuadPoints for text-markup subtypes. Stored as a flat array of
    /// floats in page user space; every 8 values (x1 y1 x2 y2 x3 y3 x4 y4)
    /// define one quad. Empty for non-markup subtypes.
    pub quad_points: Vec<f64>,
    /// /Border width (from /Border or /BS /W). 0.0 means no visible
    /// border.
    pub border_width: f64,
    /// /Contents literal text. Used by FreeText and Ink annotations that
    /// have no appearance stream.
    pub contents: Option<String>,
    /// Raw /AP/N content stream bytes (already decoded). `None` if the
    /// annotation has no /AP entry or the normal-appearance stream is
    /// missing/unreadable.
    pub ap_stream: Option<Vec<u8>>,
    /// /AP/N stream /Resources dictionary. Resolved once at enumeration
    /// time so the renderer can interpret the stream without re-walking
    /// the resolver.
    pub ap_resources: Option<PdfDictionary>,
    /// /AP/N /BBox (x_min, y_min, x_max, y_max). Required for the
    /// §12.5.5 Rect-fit transform. Defaults to `[0 0 rect.w rect.h]` if
    /// missing.
    pub ap_bbox: Option<[f64; 4]>,
    /// /AP/N /Matrix (6 entries, PDF row-vector convention). Defaults to
    /// identity if missing.
    pub ap_matrix: Option<[f64; 6]>,
    /// /InkList for Ink annotations. Each inner Vec is one stroked path
    /// as flat (x, y) pairs in page user space.
    pub ink_list: Vec<Vec<(f64, f64)>>,
}

impl PageAnnotation {
    /// Returns true when `flags` has bit 2 (`Hidden`) or bit 6 (`NoView`)
    /// set, per ISO 32000-2 §12.5.3 Table 165. Renderers must skip.
    pub fn is_hidden(&self) -> bool {
        const HIDDEN: u32 = 1 << 1; // bit 2 (1-indexed)
        const NO_VIEW: u32 = 1 << 5; // bit 6 (1-indexed)
        (self.flags & HIDDEN) != 0 || (self.flags & NO_VIEW) != 0
    }
}

/// A bookmark entry from the document's /Outlines tree.
#[derive(Debug, Clone)]
pub struct BookmarkEntry {
    /// The bookmark title text.
    pub title: String,
    /// Child bookmarks.
    pub children: Vec<BookmarkEntry>,
}

/// Annotation subtypes that are decorative (lines, shapes, highlights, etc.)
/// and should be skipped during text extraction.
/// Note: Link annotations are handled separately for hyperlink extraction.
const DECORATIVE_SUBTYPES: &[&[u8]] = &[
    b"Popup",
    b"Line",
    b"Square",
    b"Circle",
    b"Highlight",
    b"Underline",
    b"StrikeOut",
    b"Ink",
    b"Sound",
    b"Movie",
    b"Screen",
    b"PrinterMark",
    b"TrapNet",
    b"Watermark",
];

/// Configuration for PDF text extraction.
///
/// Controls diagnostics and decode limits. Use `Config::default()` for
/// sensible defaults (null diagnostics, 250 MB decompression limit).
///
/// ```
/// use udoc_pdf::{Config, CollectingDiagnostics};
/// use std::sync::Arc;
///
/// let diag = Arc::new(CollectingDiagnostics::new());
/// let config = Config::default().with_diagnostics(diag);
/// ```
#[non_exhaustive]
pub struct Config {
    /// Diagnostics sink for warnings and info messages.
    /// Default: [`NullDiagnostics`] (discards all).
    pub diagnostics: Arc<dyn DiagnosticsSink>,
    /// Limits applied when decoding stream data.
    /// Default: 250 MB max decompressed size, 100:1 max ratio (above 10 MB floor).
    pub decode_limits: DecodeLimits,
    /// Password for encrypted PDFs.
    /// Default: None. Empty-password documents are tried automatically.
    pub password: Option<Vec<u8>>,
}

impl Config {
    /// Set a custom diagnostics sink.
    ///
    /// # Examples
    ///
    /// ```
    /// use udoc_pdf::{Config, CollectingDiagnostics};
    /// use std::sync::Arc;
    ///
    /// let diag = Arc::new(CollectingDiagnostics::new());
    /// let config = Config::default().with_diagnostics(diag.clone());
    ///
    /// // After parsing, inspect collected warnings:
    /// // let warnings = diag.warnings();
    /// ```
    pub fn with_diagnostics(mut self, diagnostics: Arc<dyn DiagnosticsSink>) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    /// Set custom decode limits.
    ///
    /// # Examples
    ///
    /// ```
    /// use udoc_pdf::Config;
    /// use udoc_pdf::DecodeLimits;
    ///
    /// let mut limits = DecodeLimits::default();
    /// limits.max_decompressed_size = 100 * 1024 * 1024; // 100 MB
    /// let config = Config::default().with_decode_limits(limits);
    /// ```
    pub fn with_decode_limits(mut self, limits: DecodeLimits) -> Self {
        self.decode_limits = limits;
        self
    }

    /// Set a password for opening encrypted PDFs.
    pub fn with_password(mut self, password: impl Into<Vec<u8>>) -> Self {
        self.password = Some(password.into());
        self
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            diagnostics: Arc::new(NullDiagnostics),
            decode_limits: DecodeLimits::default(),
            password: None,
        }
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("decode_limits", &self.decode_limits)
            .field("password", &self.password.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

/// A parsed PDF document.
///
/// Owns the raw PDF bytes and parsed structure. Provides access to pages
/// via [`page()`](Document::page). `Document` is `Send` but not `Sync`
/// (the internal resolver uses interior mutability for caching).
///
/// Field order matters: `resolver` must be declared (and therefore dropped)
/// before `data`, because the resolver holds a pointer into `data`.
pub struct Document {
    /// Object resolver. Holds a pointer into `data` via lifetime extension.
    /// Must be dropped before `data` (guaranteed by field declaration order,
    /// reinforced by the Drop impl).
    resolver: Option<ObjectResolver<'static>>,
    /// Raw PDF bytes (owned, heap-allocated, immutable). The resolver borrows
    /// from this via the lifetime-extended pointer. Using `Box<[u8]>` instead of
    /// `Vec<u8>` to prevent reallocation (safety invariant for the self-referential
    /// borrow). Field exists to keep the allocation alive.
    #[allow(dead_code)] // backing store for self-referential borrow; must outlive resolver
    data: Box<[u8]>,
    /// Page ObjRefs collected from the /Pages tree, in document order.
    page_refs: Vec<ObjRef>,
    /// Parsed structure tree for tagged PDFs. None if the document has no
    /// /StructTreeRoot or if parsing failed. Used for MCID-based reading order.
    structure_tree: Option<StructureTree>,
    /// Per-page alt-text index built once from `structure_tree` at open
    /// time so image extraction doesn't walk the full tree per page (#150).
    alt_text_index: AltTextIndex,
    /// Diagnostics sink (shared with resolver).
    diagnostics: Arc<dyn DiagnosticsSink>,
    /// Cached document metadata extracted from the /Info dictionary at load time.
    metadata: DocumentMetadata,
    /// `true` iff the source PDF declared encryption (i.e. the trailer
    /// had an `/Encrypt` entry). Set during [`setup_encryption`] before
    /// password validation, so it is accurate even when decryption
    /// succeeded with a supplied password. Surfaced via
    /// [`udoc_core::backend::FormatBackend::is_encrypted`] (W0-IS-ENCRYPTED).
    is_encrypted: bool,
    /// Cross-page cache of Form XObject ObjRefs that produced no text.
    /// Shared across all pages so decorative forms (logos, watermarks, borders)
    /// are only interpreted once per document instead of once per page.
    textless_forms_cache: Arc<Mutex<HashSet<ObjRef>>>,
    /// Cross-page font name intern cache.
    /// Maps font ObjRef to interned display + raw names (see
    /// [`crate::content::interpreter::InternedFontName`]). Shared across
    /// pages so fonts that appear on multiple pages reuse the same Arcs.
    font_name_cache: Arc<Mutex<HashMap<ObjRef, crate::content::interpreter::InternedFontName>>>,
    /// Cached catalog dictionary from /Root. Resolved once, reused for named
    /// destination lookups, bookmarks, etc. instead of re-resolving from the
    /// trailer on every call.
    cached_catalog: Option<PdfDictionary>,
    /// Extracted font program data from all pages interpreted so far.
    /// Keyed by display name (subset prefix stripped). Populated during page
    /// interpretation via take_font_data() on the ContentInterpreter.
    /// Shared via `Arc<Mutex<_>>` so Pages can write to it during interpretation.
    extracted_fonts: ExtractedFontCache,
    /// Type3 font metadata for outline extraction (CharProcs, FontMatrix, etc.).
    extracted_type3: ExtractedType3Cache,
}

impl Document {
    /// Open a PDF file from disk.
    ///
    /// Reads the entire file into memory, parses the document structure
    /// (header, xref, trailer), and walks the page tree.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use udoc_pdf::Document;
    ///
    /// let doc = Document::open("report.pdf")?;
    /// println!("Pages: {}", doc.page_count());
    /// # Ok::<(), udoc_pdf::Error>(())
    /// ```
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_config(path, Config::default())
    }

    /// Open a PDF file with custom configuration.
    pub fn open_with_config(path: impl AsRef<Path>, config: Config) -> Result<Self> {
        let data = std::fs::read(path.as_ref()).map_err(Error::Io)?;
        Self::from_bytes_with_config(data, config)
    }

    /// Open an encrypted PDF with a password.
    pub fn open_with_password(
        path: impl AsRef<Path>,
        password: impl Into<Vec<u8>>,
    ) -> Result<Self> {
        Self::open_with_config(path, Config::default().with_password(password))
    }

    /// Parse a PDF from an in-memory byte buffer.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use udoc_pdf::Document;
    ///
    /// # let pdf_bytes = std::fs::read(
    /// #     std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    /// #         .join("tests/corpus/minimal/hex_string.pdf"),
    /// # ).unwrap();
    /// let doc = Document::from_bytes(pdf_bytes)?;
    /// assert!(doc.page_count() > 0);
    /// # Ok::<(), udoc_pdf::Error>(())
    /// ```
    pub fn from_bytes(data: Vec<u8>) -> Result<Self> {
        Self::from_bytes_with_config(data, Config::default())
    }

    /// Parse a PDF from in-memory bytes with a diagnostics sink.
    ///
    /// Follows the same constructor convention as other backends (DOCX, XLSX, etc.),
    /// accepting a `&[u8]` slice and a core diagnostics sink. Bridges the core
    /// `DiagnosticsSink` to the PDF-internal `DiagnosticsSink`. Since
    /// the bridge maps `WarningKind` -> `WarningKind`
    /// directly without a lossy String round-trip.
    pub fn from_bytes_with_diag(
        data: &[u8],
        diag: std::sync::Arc<dyn udoc_core::diagnostics::DiagnosticsSink>,
    ) -> Result<Self> {
        let bridge = Self::core_diag_bridge(diag);
        Self::from_bytes_with_config(data.to_vec(), Config::default().with_diagnostics(bridge))
    }

    /// Construct an `Arc<dyn DiagnosticsSink>` (PDF flavor) that forwards
    /// every PDF warning into the supplied core sink.
    ///
    /// Exposed for the `udoc` facade (and other callers that build a PDF
    /// `Config` directly) so they can wire a single core diagnostics sink
    /// through both the path-based and bytes-based PDF entry points
    /// without duplicating the bridge wiring. See.
    pub fn core_diag_bridge(
        diag: std::sync::Arc<dyn udoc_core::diagnostics::DiagnosticsSink>,
    ) -> std::sync::Arc<dyn DiagnosticsSink> {
        std::sync::Arc::new(CoreDiagBridge(diag))
    }

    /// Parse an encrypted PDF from bytes with a password.
    pub fn from_bytes_with_password(data: Vec<u8>, password: impl Into<Vec<u8>>) -> Result<Self> {
        Self::from_bytes_with_config(data, Config::default().with_password(password))
    }

    /// Parse a PDF from an in-memory byte buffer with custom configuration.
    ///
    /// # Safety note (internal)
    ///
    /// Uses one `unsafe` block to create a self-referential struct: the
    /// `ObjectResolver` borrows from the `Box<[u8]>` that `Document` owns.
    /// This is sound because:
    /// 1. `data` is heap-allocated (`Box<[u8]>`) and cannot be reallocated
    ///    (boxed slices have no capacity, so no push/reserve/shrink).
    /// 2. `resolver` is declared before `data` in the struct, so it is
    ///    dropped first (Rust drops fields in declaration order).
    /// 3. The `Drop` impl also explicitly takes the resolver before data
    ///    is dropped, as defense-in-depth.
    /// 4. `Document` does not implement `Clone`, so the data cannot be
    ///    moved out from under the resolver.
    #[allow(unsafe_code)]
    pub fn from_bytes_with_config(data: Vec<u8>, config: Config) -> Result<Self> {
        // Convert to Box<[u8]> to prevent future reallocation (safety invariant).
        let data: Box<[u8]> = data.into_boxed_slice();

        // Parse document structure (header, xref, trailer).
        let parser = DocumentParser::with_diagnostics(&data, config.diagnostics.clone());
        let structure = parser.parse().context("parsing document structure")?;

        // Extend the borrow lifetime from the local `data` to 'static.
        // Sound because `data` is owned by Document and outlives the resolver
        // (see safety note above).
        let static_ref: &'static [u8] =
            unsafe { std::slice::from_raw_parts(data.as_ptr(), data.len()) };

        let mut resolver = ObjectResolver::from_document_with_diagnostics(
            static_ref,
            structure,
            config.diagnostics.clone(),
        );
        resolver.set_decode_limits(config.decode_limits);

        // Check for /Encrypt in trailer and set up decryption if present.
        // Returns true iff the trailer declared /Encrypt (regardless of
        // whether decryption succeeded with the supplied password).
        // Used to populate `Document::is_encrypted` for downstream
        // callers (CLI inspect, Python `doc.is_encrypted`).
        let is_encrypted = setup_encryption(
            &mut resolver,
            config.password.as_deref(),
            &config.diagnostics,
        )?;

        // Walk the page tree to collect page refs.
        let page_refs = collect_page_refs(&mut resolver, &config.diagnostics)?;

        // Parse the structure tree for tagged PDFs (best-effort).
        let structure_tree = parse_structure_tree_from_catalog(&mut resolver);

        // Build the per-page alt-text index once so image annotation on
        // each page is an O(1) hash lookup instead of an O(tree) walk (#150).
        let alt_text_index = AltTextIndex::build(structure_tree.as_ref());

        // Extract document metadata from /Info dictionary (best-effort).
        let metadata = extract_info_metadata(&mut resolver, page_refs.len());

        Ok(Document {
            resolver: Some(resolver),
            data,
            page_refs,
            structure_tree,
            alt_text_index,
            diagnostics: config.diagnostics,
            metadata,
            is_encrypted,
            textless_forms_cache: Arc::new(Mutex::new(HashSet::new())),
            font_name_cache: Arc::new(Mutex::new(HashMap::new())),
            cached_catalog: None,
            extracted_fonts: Arc::new(Mutex::new(HashMap::new())),
            extracted_type3: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Number of pages in the document.
    pub fn page_count(&self) -> usize {
        self.page_refs.len()
    }

    /// `true` iff the source PDF declared encryption (i.e. the trailer
    /// had an `/Encrypt` entry). Set during `from_bytes_with_config`.
    /// Independent of whether decryption *succeeded*: an encrypted PDF
    /// the caller supplied a correct password for produces a Document
    /// with `is_encrypted() == true` and fully-extracted content.
    /// Surfaced through `FormatBackend::is_encrypted` for facade callers.
    pub fn is_encrypted(&self) -> bool {
        self.is_encrypted
    }

    /// Release document-scoped caches.
    ///
    /// Drops the object/stream caches inside the object resolver plus
    /// the cross-page shared caches (textless-form, font-name, extracted
    /// fonts, extracted Type3). The document itself remains open and the
    /// next [`Page`] request will re-populate caches as needed.
    ///
    /// Intended for long-running batch workers that want to cap process
    /// RSS without closing + reopening documents. Peak memory *within* a
    /// single content-stream interpretation is untouched.
    pub fn reset_document_caches(&mut self) {
        if let Some(resolver) = self.resolver.as_mut() {
            resolver.reset_caches();
        }
        // Shrink shared per-document maps by replacing them. `Arc::clone`s
        // that outlive this call (e.g., a Page that is in the middle of a
        // content-stream run on another thread) keep their own reference;
        // the new empty map only affects future Pages opened from this
        // Document.
        if let Ok(mut g) = self.textless_forms_cache.lock() {
            *g = HashSet::new();
        }
        if let Ok(mut g) = self.font_name_cache.lock() {
            *g = HashMap::new();
        }
        if let Ok(mut g) = self.extracted_fonts.lock() {
            *g = HashMap::new();
        }
        if let Ok(mut g) = self.extracted_type3.lock() {
            *g = HashMap::new();
        }
    }

    /// Get extracted font program data from all pages interpreted so far.
    ///
    /// Returns a map of font display name -> (raw bytes, program type).
    /// Populated during page interpretation; call after extracting at least
    /// one page to get the fonts used on that page. Fonts are deduplicated
    /// by display name (first-seen wins for fonts appearing across pages).
    pub fn font_programs(&self) -> HashMap<String, FontCacheEntry> {
        self.extracted_fonts
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Extract Type3 glyph outlines for rendering.
    ///
    /// For each Type3 font found during page extraction, resolves each
    /// CharProc stream and converts the path data to a serialized
    /// GlyphOutline. Returns (font_name, unicode_char, serialized_bytes)
    /// tuples suitable for storing as FontAssets.
    ///
    /// Must be called after page extraction (uses cached Type3 metadata).
    pub fn type3_font_outlines(&mut self) -> Vec<(String, char, Vec<u8>)> {
        use udoc_font::type3_outline::{extract_charproc_outline, serialize_outline};

        let resolver = match self.resolver.as_mut() {
            Some(r) => r,
            None => return Vec::new(),
        };

        let type3_cache = self
            .extracted_type3
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();

        let mut result = Vec::new();

        // Iterate Type3 fonts in deterministic name order. type3_cache is a
        // HashMap so iteration order varies with the random hasher seed;
        // downstream FontCache builds Type3 outlines into another HashMap
        // keyed by (font_name, char), but the order of insertion into the
        // AssetStore Vec matters because it's preserved through to render.
        let mut type3_entries: Vec<_> = type3_cache.values().collect();
        type3_entries.sort_by(|a, b| a.name.cmp(&b.name));

        for info in type3_entries {
            // Sort glyph_names by code for deterministic iteration.
            let mut glyph_entries: Vec<_> = info.glyph_names.iter().collect();
            glyph_entries.sort_by_key(|(&code, _)| code);
            for (&code, glyph_name) in glyph_entries {
                let char_proc_ref = match info.char_procs.get(glyph_name) {
                    Some(r) => *r,
                    None => continue,
                };

                // Resolve the CharProc stream to get the raw bytes.
                let stream_data = match resolver.resolve(char_proc_ref) {
                    Ok(PdfObject::Stream(ref s)) => {
                        match resolver.decode_stream_data(s, Some(char_proc_ref)) {
                            Ok(data) => data,
                            Err(_) => continue,
                        }
                    }
                    _ => continue,
                };

                if let Some(outline) = extract_charproc_outline(&stream_data, info.font_matrix) {
                    let unicode_char = udoc_font::encoding::parse_glyph_name(glyph_name).unwrap_or(
                        if (0x20..=0x7E).contains(&code) {
                            code as char
                        } else {
                            char::REPLACEMENT_CHARACTER
                        },
                    );

                    let serialized = serialize_outline(&outline);
                    result.push((info.name.clone(), unicode_char, serialized));
                }
            }
        }

        result
    }

    /// Access a page by zero-based index.
    ///
    /// Returns a [`Page`] that borrows from this document. The page provides
    /// methods for text extraction at various levels of detail.
    ///
    /// Because this method takes `&mut self`, only one [`Page`] can exist at
    /// a time. Extract text from one page before requesting the next:
    ///
    /// ```no_run
    /// # use udoc_pdf::Document;
    /// let mut doc = Document::open("example.pdf")?;
    /// // Process pages sequentially:
    /// for i in 0..doc.page_count() {
    ///     let text = doc.page(i)?.text()?;
    ///     println!("{text}");
    /// }
    /// # Ok::<(), udoc_pdf::Error>(())
    /// ```
    ///
    /// Resolve the document catalog from `/Root` once and cache it for
    /// subsequent named-destination, bookmark, and structure-tree lookups
    /// (#149). Subsequent callers get `self.cached_catalog` as
    /// `Some(dict)` without touching the trailer.
    fn ensure_catalog_cached(&mut self) {
        if self.cached_catalog.is_some() {
            return;
        }
        if let Some(resolver) = self.resolver.as_mut() {
            if let Some(root_ref) = resolver
                .trailer()
                .and_then(|t| t.get(b"Root"))
                .and_then(|r| r.as_reference())
            {
                if let Ok(catalog) = resolver.resolve_dict(root_ref) {
                    self.cached_catalog = Some(catalog);
                }
            }
        }
    }

    /// # Errors
    ///
    /// Returns an error if `index` is out of range or the page dictionary
    /// cannot be resolved.
    pub fn page(&mut self, index: usize) -> Result<Page<'_>> {
        if index >= self.page_refs.len() {
            return Err(Error::structure(format!(
                "page index {} out of range (document has {} pages)",
                index,
                self.page_refs.len()
            )));
        }

        // Lazily resolve and cache the catalog dict from /Root on first
        // access. Subsequent calls (both page() and bookmarks()) reuse the
        // cache, so named-destination lookups do not re-walk the trailer
        // on every resolution (#149).
        self.ensure_catalog_cached();

        let page_ref = self.page_refs[index];
        let resolver = self
            .resolver
            .as_mut()
            .ok_or_else(|| Error::structure("document has been closed (resolver dropped)"))?;

        let page_dict = resolver
            .resolve_dict(page_ref)
            .context(format!("resolving page {index} dictionary"))?;

        Ok(Page {
            index,
            page_ref,
            dict: page_dict,
            resolver,
            diagnostics: &self.diagnostics,
            structure_tree: &self.structure_tree,
            alt_text_index: &self.alt_text_index,
            textless_forms_cache: self.textless_forms_cache.clone(),
            font_name_cache: self.font_name_cache.clone(),
            cached_catalog: &self.cached_catalog,
            extracted_fonts: self.extracted_fonts.clone(),
            extracted_type3: self.extracted_type3.clone(),
        })
    }

    /// Extract the document bookmark (outline) tree.
    ///
    /// Parses the /Outlines dictionary from the document catalog and walks
    /// the /First//Next linked list. Uses cycle detection and depth limits.
    /// Returns an empty vec if the document has no outlines.
    pub fn bookmarks(&mut self) -> Vec<BookmarkEntry> {
        // Reuse the cached catalog (#149) so /Outlines lookups do not walk
        // the trailer again. Falls back to resolving /Root directly only
        // when caching failed (e.g. malformed trailer, error on first
        // resolve_dict).
        self.ensure_catalog_cached();
        let resolver = match self.resolver.as_mut() {
            Some(r) => r,
            None => return Vec::new(),
        };

        let catalog_ref;
        let catalog_owned;
        let catalog: &PdfDictionary = match self.cached_catalog.as_ref() {
            Some(c) => c,
            None => {
                catalog_ref = match resolver
                    .trailer()
                    .and_then(|t| t.get(b"Root"))
                    .and_then(|r| r.as_reference())
                {
                    Some(r) => r,
                    None => return Vec::new(),
                };
                catalog_owned = match resolver.resolve_dict(catalog_ref) {
                    Ok(c) => c,
                    Err(e) => {
                        self.diagnostics.warning(Warning::new(
                            None,
                            WarningKind::InvalidState,
                            format!("failed to resolve catalog for bookmarks: {e}"),
                        ));
                        return Vec::new();
                    }
                };
                &catalog_owned
            }
        };

        let outlines_ref = match catalog.get(b"Outlines").and_then(|o| o.as_reference()) {
            Some(r) => r,
            None => return Vec::new(),
        };

        let outlines_dict = match resolver.resolve_dict(outlines_ref) {
            Ok(d) => d,
            Err(e) => {
                self.diagnostics.warning(Warning::new(
                    None,
                    WarningKind::InvalidState,
                    format!("failed to resolve /Outlines dict: {e}"),
                ));
                return Vec::new();
            }
        };

        let first_ref = match outlines_dict.get(b"First").and_then(|f| f.as_reference()) {
            Some(r) => r,
            None => return Vec::new(),
        };

        let mut visited = HashSet::new();
        visited.insert(outlines_ref);
        walk_bookmark_list(
            resolver,
            first_ref,
            &mut visited,
            0,
            self.diagnostics.as_ref(),
        )
    }
}

/// Walk a linked list of bookmark entries (/First//Next chain).
fn walk_bookmark_list(
    resolver: &mut ObjectResolver<'_>,
    first_ref: ObjRef,
    visited: &mut HashSet<ObjRef>,
    depth: usize,
    diagnostics: &dyn DiagnosticsSink,
) -> Vec<BookmarkEntry> {
    if depth >= MAX_BOOKMARK_DEPTH {
        diagnostics.warning(Warning::new(
            None,
            WarningKind::ResourceLimit,
            format!("bookmark depth limit ({MAX_BOOKMARK_DEPTH}) reached, truncating subtree"),
        ));
        return Vec::new();
    }

    let mut entries = Vec::new();
    let mut current_ref = Some(first_ref);

    while let Some(ref_id) = current_ref {
        if !visited.insert(ref_id) {
            break; // Cycle detected
        }
        if visited.len() > MAX_BOOKMARK_COUNT {
            diagnostics.warning(Warning::new(
                None,
                WarningKind::ResourceLimit,
                format!("bookmark count limit ({MAX_BOOKMARK_COUNT}) reached, skipping remaining"),
            ));
            break;
        }

        let dict = match resolver.resolve_dict(ref_id) {
            Ok(d) => d,
            Err(e) => {
                // The /Outlines tree is a linked list (/First//Next). When one
                // node fails to resolve, the /Next pointer is inside the
                // unresolvable dict, so we lose the rest of the sibling chain.
                diagnostics.warning(Warning::new(
                    None,
                    WarningKind::InvalidState,
                    format!("failed to resolve bookmark entry {}: {e}", ref_id),
                ));
                break;
            }
        };

        // Extract title
        let title = dict
            .get(b"Title")
            .and_then(|t| match t {
                PdfObject::String(s) => Some(decode_pdf_text_bytes(s.as_bytes())),
                _ => None,
            })
            .unwrap_or_default();

        // Recurse into children
        let children = if let Some(child_ref) = dict.get(b"First").and_then(|f| f.as_reference()) {
            walk_bookmark_list(resolver, child_ref, visited, depth + 1, diagnostics)
        } else {
            Vec::new()
        };

        entries.push(BookmarkEntry { title, children });

        // Follow /Next sibling
        current_ref = dict.get(b"Next").and_then(|n| n.as_reference());
    }

    entries
}

/// Resolve a named destination to its destination array.
///
/// Looks up the name in the document catalog's `/Names`/`/Dests` name tree
/// (PDF 1.2+) and falls back to the legacy `/Dests` dictionary (PDF 1.1).
/// Returns the resolved destination object (typically an array like
/// `[page_ref /Fit]`), or None if not found.
fn resolve_named_dest(
    resolver: &mut ObjectResolver<'_>,
    name: &[u8],
    diagnostics: &dyn DiagnosticsSink,
    cached_catalog: &Option<PdfDictionary>,
) -> Option<PdfObject> {
    // Use the cached catalog if available, otherwise fall back to resolving
    // from the trailer (should only happen if page() was not used).
    let fallback_catalog;
    let catalog = if let Some(ref cached) = cached_catalog {
        cached
    } else {
        let root_ref = resolver
            .trailer()
            .and_then(|t| t.get(b"Root"))
            .and_then(|r| r.as_reference())?;
        fallback_catalog = match resolver.resolve_dict(root_ref) {
            Ok(c) => c,
            Err(e) => {
                diagnostics.warning(Warning::new(
                    None,
                    WarningKind::InvalidState,
                    format!("failed to resolve catalog for named dest: {e}"),
                ));
                return None;
            }
        };
        &fallback_catalog
    };

    // Try /Names/Dests name tree first (PDF 1.2+)
    if let Some(names_obj) = catalog.get(b"Names") {
        let names_dict = match names_obj {
            PdfObject::Dictionary(d) => Some(d.clone()),
            PdfObject::Reference(r) => resolver.resolve_dict(*r).ok(),
            _ => None,
        };
        if let Some(names_dict) = names_dict {
            if let Some(dests_obj) = names_dict.get(b"Dests") {
                let dests_node = match dests_obj {
                    PdfObject::Dictionary(d) => Some(d.clone()),
                    PdfObject::Reference(r) => resolver.resolve_dict(*r).ok(),
                    _ => None,
                };
                if let Some(dests_node) = dests_node {
                    let mut entries_scanned = 0;
                    let mut visited_kids = std::collections::HashSet::new();
                    if let Some(dest) = walk_name_tree(
                        resolver,
                        &dests_node,
                        name,
                        0,
                        &mut entries_scanned,
                        &mut visited_kids,
                        diagnostics,
                    ) {
                        return Some(dest);
                    }
                }
            }
        }
    }

    // Fallback: legacy /Dests dictionary (PDF 1.1)
    if let Some(dests_obj) = catalog.get(b"Dests") {
        let dests_dict = match dests_obj {
            PdfObject::Dictionary(d) => Some(d.clone()),
            PdfObject::Reference(r) => resolver.resolve_dict(*r).ok(),
            _ => None,
        };
        if let Some(dests_dict) = dests_dict {
            if let Some(dest_val) = dests_dict.get(name) {
                // Legacy dests can be either an array directly or a dict with /D key
                let resolved = match dest_val {
                    PdfObject::Reference(r) => resolver.resolve(*r).ok(),
                    other => Some(other.clone()),
                };
                if let Some(resolved) = resolved {
                    return match &resolved {
                        PdfObject::Dictionary(d) => d.get(b"D").cloned(),
                        PdfObject::Array(_) => Some(resolved),
                        _ => None,
                    };
                }
            }
        }
    }

    None
}

/// Walk a PDF name tree to find a key.
///
/// Name trees (PDF spec 7.9.6) have two node types:
/// - Leaf nodes: `/Names` array of `[key1 value1 key2 value2 ...]`
/// - Intermediate nodes: `/Kids` array of child node references
///
/// Both can have `/Limits [min max]` for fast range pruning.
fn walk_name_tree(
    resolver: &mut ObjectResolver<'_>,
    node: &PdfDictionary,
    name: &[u8],
    depth: usize,
    entries_scanned: &mut usize,
    visited_kids: &mut std::collections::HashSet<ObjRef>,
    diagnostics: &dyn DiagnosticsSink,
) -> Option<PdfObject> {
    if depth >= MAX_NAME_TREE_DEPTH {
        diagnostics.warning(Warning::new(
            None,
            WarningKind::ResourceLimit,
            format!("name tree depth limit ({MAX_NAME_TREE_DEPTH}) reached, stopping lookup"),
        ));
        return None;
    }

    // Check /Limits for range pruning: [min_key max_key]
    if let Some(PdfObject::Array(limits)) = node.get(b"Limits") {
        if limits.len() >= 2 {
            let min_bytes = match &limits[0] {
                PdfObject::String(s) => Some(s.as_bytes()),
                _ => None,
            };
            let max_bytes = match &limits[1] {
                PdfObject::String(s) => Some(s.as_bytes()),
                _ => None,
            };
            if let (Some(min), Some(max)) = (min_bytes, max_bytes) {
                if name < min || name > max {
                    return None;
                }
            }
        }
    }

    // Leaf node: /Names array [key1 val1 key2 val2 ...]
    if let Some(PdfObject::Array(names_arr)) = node.get(b"Names") {
        let mut i = 0;
        while i + 1 < names_arr.len() {
            *entries_scanned += 1;
            if *entries_scanned > MAX_NAME_TREE_ENTRIES {
                diagnostics.warning(Warning::new(
                    None,
                    WarningKind::ResourceLimit,
                    format!(
                        "name tree entry limit ({MAX_NAME_TREE_ENTRIES}) reached, stopping lookup"
                    ),
                ));
                return None;
            }
            let key_bytes = match &names_arr[i] {
                PdfObject::String(s) => s.as_bytes(),
                _ => {
                    i += 2;
                    continue;
                }
            };
            if key_bytes == name {
                let value = &names_arr[i + 1];
                // Resolve if indirect reference
                return match value {
                    PdfObject::Reference(r) => resolver.resolve(*r).ok(),
                    other => Some(other.clone()),
                };
            }
            i += 2;
        }
        return None;
    }

    // Intermediate node: /Kids array of child references
    if let Some(PdfObject::Array(kids)) = node.get(b"Kids") {
        for kid_obj in kids {
            let kid_dict = match kid_obj {
                PdfObject::Reference(r) => {
                    if !visited_kids.insert(*r) {
                        // Cycle detected: this /Kids node was already visited.
                        continue;
                    }
                    match resolver.resolve_dict(*r) {
                        Ok(d) => d,
                        Err(_) => continue,
                    }
                }
                PdfObject::Dictionary(d) => d.clone(),
                _ => continue,
            };
            if let Some(result) = walk_name_tree(
                resolver,
                &kid_dict,
                name,
                depth + 1,
                entries_scanned,
                visited_kids,
                diagnostics,
            ) {
                return Some(result);
            }
        }
    }

    None
}

impl fmt::Debug for Document {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Document")
            .field("page_count", &self.page_refs.len())
            .field("data_len", &self.data.len())
            .field("has_structure_tree", &self.structure_tree.is_some())
            .finish()
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        // Drop resolver first to release the borrow on data.
        self.resolver.take();
    }
}

/// A single page in a PDF document.
///
/// Borrows from the parent [`Document`]. Provides several levels of
/// text extraction detail:
/// - [`raw_spans()`](Page::raw_spans): individual text spans with positions
/// - [`text_lines()`](Page::text_lines): spans grouped into reading-order lines
/// - [`text()`](Page::text): plain text string
///
/// Additional extraction:
/// - [`images()`](Page::images): extracted images with metadata
/// - [`tables()`](Page::tables): detected tables with cell text
/// - [`path_segments()`](Page::path_segments): flattened path segments (table detector IR)
/// - [`paths()`](Page::paths): canonical page paths for renderer consumption
/// - [`extract()`](Page::extract): all content (text + images) in a single pass
///
/// Each individual method re-interprets the content stream. Use
/// [`extract()`](Page::extract) when you need multiple outputs to avoid
/// redundant work.
pub struct Page<'a> {
    /// Zero-based page index.
    index: usize,
    /// Page object reference (for structure tree lookups).
    page_ref: ObjRef,
    /// Page dictionary.
    dict: PdfDictionary,
    /// Shared resolver.
    resolver: &'a mut ObjectResolver<'static>,
    /// Diagnostics sink (borrows from Document, cloned only when needed).
    diagnostics: &'a Arc<dyn DiagnosticsSink>,
    /// Reference to the document's structure tree (None for untagged PDFs).
    structure_tree: &'a Option<StructureTree>,
    /// Shared per-page alt-text index (#150). Built once at Document open
    /// so `extract_full` can look up alt texts without walking the tree.
    alt_text_index: &'a AltTextIndex,
    /// Cross-page textless form XObject cache (shared with Document).
    textless_forms_cache: Arc<Mutex<HashSet<ObjRef>>>,
    /// Cross-page font name intern cache (shared with Document).
    font_name_cache: Arc<Mutex<HashMap<ObjRef, crate::content::interpreter::InternedFontName>>>,
    /// Cached catalog dictionary, resolved once at the Document level.
    /// Avoids re-resolving from the trailer on every named dest lookup.
    cached_catalog: &'a Option<PdfDictionary>,
    /// Cross-page extracted font programs (shared with Document).
    extracted_fonts: ExtractedFontCache,
    /// Cross-page Type3 font metadata (shared with Document).
    extracted_type3: ExtractedType3Cache,
}

impl fmt::Debug for Page<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Page")
            .field("index", &self.index)
            .field("page_ref", &self.page_ref)
            .finish()
    }
}

impl<'a> Page<'a> {
    /// Run the content stream interpreter once.
    ///
    /// When `with_images` is true, images are extracted alongside text.
    /// When false, inline images are skipped and Image XObjects are ignored,
    /// which is faster for text-only callers.
    fn interpret_page(&mut self, with_images: bool) -> Result<PageContent> {
        self.interpret_page_full(with_images, false)
    }

    /// Interpret the page content stream with the canonical
    /// [`PagePath`](crate::content::path::PagePath) IR enabled.
    ///
    /// Unlike [`interpret_page_full`](Self::interpret_page_full), which
    /// produces the flattened [`PathSegment`] buffer used by the table
    /// detector, this path runs a fresh interpreter with
    /// `set_extract_page_paths(true)` so the canonical moveto/lineto/
    /// curveto/closepath segments survive with their CTM snapshots and
    /// stroke styles intact. The table-detector buffer is discarded.
    fn page_paths_ir(&mut self) -> Result<Vec<crate::content::path::PagePath>> {
        let resources = resolve_page_resources(self.resolver, &self.dict)
            .context("resolving page /Resources")?;

        let content = get_page_content(self.resolver, &self.dict, Some(self.index))
            .context("reading page content stream")?;

        if content.is_empty() {
            return Ok(Vec::new());
        }

        let mut interp = ContentInterpreter::new(
            &resources,
            self.resolver,
            self.diagnostics.clone(),
            Some(self.index),
        );
        interp.set_extract_images(false);
        interp.set_extract_page_paths(true);
        interp.set_shared_textless_forms(self.textless_forms_cache.clone());
        interp.set_font_display_name_cache(self.font_name_cache.clone());
        interp
            .interpret(&content)
            .context("interpreting page content stream")?;
        Ok(interp.take_page_paths())
    }

    /// Like [`page_paths_ir`](Self::page_paths_ir) but returns both paths
    /// and shading-pattern records from a single interpreter pass.
    ///
    /// Callers that want both (facade conversion) should use this to
    /// avoid running the content stream twice.
    fn page_paths_and_shadings_ir(
        &mut self,
    ) -> Result<(
        Vec<crate::content::path::PagePath>,
        Vec<crate::content::path::PageShading>,
    )> {
        let (paths, shadings, _patterns) = self.page_paint_records_ir()?;
        Ok((paths, shadings))
    }

    /// Like [`page_paths_and_shadings_ir`](Self::page_paths_and_shadings_ir)
    /// but also returns Type 1 coloured tiling-pattern records.
    fn page_paint_records_ir(
        &mut self,
    ) -> Result<(
        Vec<crate::content::path::PagePath>,
        Vec<crate::content::path::PageShading>,
        Vec<crate::content::path::PageTilingPattern>,
    )> {
        let resources = resolve_page_resources(self.resolver, &self.dict)
            .context("resolving page /Resources")?;

        let content = get_page_content(self.resolver, &self.dict, Some(self.index))
            .context("reading page content stream")?;

        if content.is_empty() {
            return Ok((Vec::new(), Vec::new(), Vec::new()));
        }

        let mut interp = ContentInterpreter::new(
            &resources,
            self.resolver,
            self.diagnostics.clone(),
            Some(self.index),
        );
        interp.set_extract_images(false);
        interp.set_extract_page_paths(true);
        interp.set_shared_textless_forms(self.textless_forms_cache.clone());
        interp.set_font_display_name_cache(self.font_name_cache.clone());
        interp
            .interpret(&content)
            .context("interpreting page content stream")?;
        Ok((
            interp.take_page_paths(),
            interp.take_page_shadings(),
            interp.take_page_tiling_patterns(),
        ))
    }

    /// Interpret the page content stream with configurable extraction.
    ///
    /// `with_images`: when true, inline and XObject images are extracted.
    /// `with_paths`: when true, path segments (lines, rects) are captured
    /// for table detection.
    fn interpret_page_full(&mut self, with_images: bool, with_paths: bool) -> Result<PageContent> {
        let resources = resolve_page_resources(self.resolver, &self.dict)
            .context("resolving page /Resources")?;

        let content = get_page_content(self.resolver, &self.dict, Some(self.index))
            .context("reading page content stream")?;

        let mut spans;
        let images;
        let paths;

        if content.is_empty() {
            spans = Vec::new();
            images = Vec::new();
            paths = Vec::new();
        } else {
            let mut interp = ContentInterpreter::new(
                &resources,
                self.resolver,
                self.diagnostics.clone(),
                Some(self.index),
            );
            interp.set_extract_images(with_images);
            interp.set_extract_paths(with_paths);
            interp.set_shared_textless_forms(self.textless_forms_cache.clone());
            interp.set_font_display_name_cache(self.font_name_cache.clone());
            spans = interp
                .interpret(&content)
                .context("interpreting page content stream")?;
            images = if with_images {
                interp.take_images()
            } else {
                Vec::new()
            };
            paths = if with_paths {
                interp.take_paths()
            } else {
                Vec::new()
            };

            // Collect embedded font data for rendering.
            let font_data = interp.take_font_data();
            if !font_data.is_empty() {
                if let Ok(mut cache) = self.extracted_fonts.lock() {
                    for (name, data, program, enc_map, cid_widths) in font_data {
                        cache
                            .entry(name)
                            .or_insert((data, program, enc_map, cid_widths));
                    }
                }
            }

            // Collect Type3 font metadata for outline extraction.
            let type3_info = interp.take_type3_info();
            if !type3_info.is_empty() {
                if let Ok(mut cache) = self.extracted_type3.lock() {
                    for info in type3_info {
                        cache.entry(info.name.clone()).or_insert(info);
                    }
                }
            }
        }

        // Extract text from annotation appearance streams and merge.
        let annotation_spans = self.interpret_annotations();
        if !annotation_spans.is_empty() {
            spans.extend(annotation_spans);
        }

        Ok(PageContent {
            spans,
            images,
            paths,
        })
    }

    /// Extract text from annotation appearance streams on this page.
    ///
    /// Iterates the /Annots array, resolves each annotation dictionary,
    /// and extracts text from appearance streams (/AP /N) for Widget,
    /// FreeText, Stamp, and Text annotations. Decorative annotations
    /// (Link, Highlight, etc.) are skipped.
    ///
    /// Returns spans with `is_annotation: true`. Errors in individual
    /// annotations are logged as warnings; the method never fails the
    /// entire page extraction.
    fn interpret_annotations(&mut self) -> Vec<TextSpan> {
        let annots_array = match self.dict.get(b"Annots") {
            Some(obj) => obj.clone(),
            None => return Vec::new(),
        };

        // /Annots can be an array or a reference to an array
        let annot_refs: Vec<PdfObject> = match annots_array {
            PdfObject::Array(arr) => arr,
            PdfObject::Reference(r) => match self.resolver.resolve(r) {
                Ok(PdfObject::Array(arr)) => arr,
                Ok(_) => {
                    self.warn_annot("page /Annots is not an array");
                    return Vec::new();
                }
                Err(e) => {
                    self.warn_annot(&format!("failed to resolve /Annots: {e}"));
                    return Vec::new();
                }
            },
            _ => {
                self.warn_annot("page /Annots is not an array or reference");
                return Vec::new();
            }
        };

        let mut spans = Vec::new();
        let mut visited_ap = HashSet::new();

        for (i, annot_obj) in annot_refs.iter().enumerate() {
            if i >= MAX_ANNOTATIONS_PER_PAGE {
                self.warn_annot(&format!(
                    "annotation limit ({MAX_ANNOTATIONS_PER_PAGE}) reached, skipping remaining"
                ));
                break;
            }

            let annot_ref = match annot_obj.as_reference() {
                Some(r) => r,
                None => {
                    // Inline annotation dict (unusual but possible)
                    if let Some(d) = annot_obj.as_dict() {
                        self.extract_annotation_text(d, &mut spans, &mut visited_ap);
                    }
                    continue;
                }
            };

            let annot_dict = match self.resolver.resolve_dict(annot_ref) {
                Ok(d) => d,
                Err(e) => {
                    self.warn_annot(&format!("failed to resolve annotation {annot_ref}: {e}"));
                    continue;
                }
            };

            self.extract_annotation_text(&annot_dict, &mut spans, &mut visited_ap);
        }

        // Mark all annotation spans
        for span in &mut spans {
            span.is_annotation = true;
        }

        spans
    }

    /// Extract text from a single annotation dictionary.
    ///
    /// Dispatches based on /Subtype: Widget/FreeText/Stamp get appearance
    /// stream interpretation, Widget without /AP falls back to /V value,
    /// Text annotations extract /Contents.
    fn extract_annotation_text(
        &mut self,
        annot_dict: &PdfDictionary,
        spans: &mut Vec<TextSpan>,
        visited_ap: &mut HashSet<ObjRef>,
    ) {
        let subtype = match annot_dict.get_name(b"Subtype") {
            Some(s) => s,
            None => return, // No subtype, skip
        };

        // Skip decorative annotation types
        if DECORATIVE_SUBTYPES.contains(&subtype) {
            return;
        }

        // Parse /Rect for positioning (used for fallback text placement)
        let rect = parse_rect(annot_dict);

        match subtype {
            b"Widget" | b"FreeText" | b"Stamp" => {
                // Try to interpret /AP /N appearance stream
                let ap_spans = self.interpret_appearance_stream(annot_dict, visited_ap);
                if !ap_spans.is_empty() {
                    spans.extend(ap_spans);
                    return;
                }

                // Widget fallback: extract /V string value if no appearance stream
                if subtype == b"Widget" {
                    if let Some(text) = self.extract_annot_value(annot_dict) {
                        if let Some((x, y)) = rect {
                            spans.push(TextSpan::new(text, x, y, 0.0, String::new(), 12.0));
                        }
                    }
                }
            }
            b"Text" => {
                // /Text annotations: extract /Contents string
                if let Some(text) = extract_string_value(annot_dict, b"Contents") {
                    if !text.is_empty() {
                        if let Some((x, y)) = rect {
                            spans.push(TextSpan::new(text, x, y, 0.0, String::new(), 12.0));
                        }
                    }
                }
            }
            // All other annotations (including Link): try the appearance stream.
            // Link URLs are extracted separately by links(); this gets visible text.
            _ => {
                // Unknown subtype with potential text. Try appearance stream.
                let ap_spans = self.interpret_appearance_stream(annot_dict, visited_ap);
                spans.extend(ap_spans);
            }
        }
    }

    /// Interpret the /AP /N (normal appearance) stream of an annotation.
    ///
    /// Resolves the appearance stream, sets up a ContentInterpreter scoped
    /// to the annotation, and returns any text spans found.
    fn interpret_appearance_stream(
        &mut self,
        annot_dict: &PdfDictionary,
        visited_ap: &mut HashSet<ObjRef>,
    ) -> Vec<TextSpan> {
        // Get /AP dictionary
        let ap_dict = match self.resolve_annot_sub_dict(annot_dict, b"AP") {
            Some(d) => d,
            None => return Vec::new(),
        };

        // Get /N (normal appearance) from /AP
        let n_entry = match ap_dict.get(b"N") {
            Some(obj) => obj.clone(),
            None => return Vec::new(),
        };

        // /N can be a stream reference directly, or a dictionary mapping
        // appearance state names to streams (for checkboxes, radio buttons, etc.).
        // For simplicity, handle the direct stream case and the /AS-keyed case.
        let stream_ref = match n_entry {
            PdfObject::Reference(r) => r,
            PdfObject::Dictionary(ref sub_dict) => {
                // /N is a sub-dictionary keyed by appearance state.
                // Look up /AS (appearance state) to find the right stream.
                let as_name = annot_dict.get_name(b"AS").unwrap_or(b"Off");
                match sub_dict.get_ref(as_name) {
                    Some(r) => r,
                    None => return Vec::new(),
                }
            }
            _ => return Vec::new(),
        };

        // Cycle detection for /AP references
        if !visited_ap.insert(stream_ref) {
            self.warn_annot(&format!(
                "circular /AP reference detected for {stream_ref}, skipping"
            ));
            return Vec::new();
        }

        // Resolve the appearance stream
        let stream = match self.resolver.resolve(stream_ref) {
            Ok(PdfObject::Stream(s)) => s,
            Ok(_) => return Vec::new(),
            Err(e) => {
                self.warn_annot(&format!(
                    "failed to resolve appearance stream {stream_ref}: {e}"
                ));
                return Vec::new();
            }
        };

        // Get the appearance stream's /Resources
        let ap_resources = self
            .resolver
            .get_resolved_dict(&stream.dict, b"Resources")
            .ok()
            .flatten()
            .unwrap_or_default();

        // Decode the stream content
        let content_data = match self
            .resolver
            .decode_stream_data(&stream, Some(stream_ref))
            .context("decoding annotation appearance stream")
        {
            Ok(data) => data,
            Err(e) => {
                self.warn_annot(&format!(
                    "failed to decode appearance stream {stream_ref}: {e}"
                ));
                return Vec::new();
            }
        };

        if content_data.is_empty() {
            return Vec::new();
        }

        // Create a fresh interpreter for this appearance stream
        let mut interp = ContentInterpreter::new(
            &ap_resources,
            self.resolver,
            self.diagnostics.clone(),
            Some(self.index),
        );
        interp.set_extract_images(false);
        interp.set_font_display_name_cache(self.font_name_cache.clone());

        match interp.interpret(&content_data) {
            Ok(ap_spans) => ap_spans,
            Err(e) => {
                self.warn_annot(&format!(
                    "error interpreting appearance stream {stream_ref}: {e}"
                ));
                Vec::new()
            }
        }
    }

    /// Interpret an annotation's /AP/N appearance stream and return the
    /// emitted page paths + text spans already composited into page user
    /// space via the ISO 32000-2 §12.5.5 `Matrix * RectFit` transform.
    ///
    /// The annotation must have been produced by
    /// [`Page::annotations`](Page::annotations). Returns `(paths, spans)`,
    /// either possibly empty. `/BBox` defaults to `[0 0 rect.w rect.h]`
    /// when absent, `/Matrix` defaults to identity; this matches the
    /// spec's §12.5.5 algorithm.
    pub fn interpret_annotation_appearance(
        &mut self,
        annotation: &PageAnnotation,
    ) -> (Vec<crate::content::path::PagePath>, Vec<TextSpan>) {
        let ap_stream = match annotation.ap_stream.as_deref() {
            Some(s) => s,
            None => return (Vec::new(), Vec::new()),
        };
        let ap_resources = annotation.ap_resources.clone().unwrap_or_default();
        let ap_bbox = annotation.ap_bbox.unwrap_or([
            0.0,
            0.0,
            annotation.rect.x_max - annotation.rect.x_min,
            annotation.rect.y_max - annotation.rect.y_min,
        ]);
        let ap_matrix = annotation
            .ap_matrix
            .unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

        // Build the §12.5.5 composite matrix: Matrix * RectFit.
        let composite = compose_appearance_matrix(annotation.rect, ap_bbox, ap_matrix);

        // Prepend the composite matrix as a `cm` operator so the content
        // interpreter applies it to every emitted path CTM and text
        // matrix. Wrap in `q`/`Q` so state mutations inside the AP
        // stream don't leak.
        let mut wrapped = Vec::with_capacity(ap_stream.len() + 64);
        wrapped.extend_from_slice(b"q\n");
        wrapped.extend_from_slice(
            format!(
                "{} {} {} {} {} {} cm\n",
                composite[0], composite[1], composite[2], composite[3], composite[4], composite[5]
            )
            .as_bytes(),
        );
        wrapped.extend_from_slice(ap_stream);
        wrapped.extend_from_slice(b"\nQ\n");

        let mut interp = ContentInterpreter::new(
            &ap_resources,
            self.resolver,
            self.diagnostics.clone(),
            Some(self.index),
        );
        interp.set_extract_images(false);
        interp.set_extract_page_paths(true);
        interp.set_font_display_name_cache(self.font_name_cache.clone());
        let spans = match interp.interpret(&wrapped) {
            Ok(s) => s,
            Err(e) => {
                self.warn_annot(&format!("error interpreting appearance stream: {e}"));
                return (Vec::new(), Vec::new());
            }
        };
        let paths = interp.take_page_paths();
        (paths, spans)
    }

    /// Extract the /V value from a Widget annotation dictionary.
    ///
    /// /V can be a string (text field), name (choice field), or absent.
    /// For text fields, decodes the PDF text string to Unicode.
    fn extract_annot_value(&mut self, annot_dict: &PdfDictionary) -> Option<String> {
        // /V may be in the annotation dict itself or in a merged field dict.
        // Try direct first.
        let v_obj = match annot_dict.get(b"V") {
            Some(obj) => obj.clone(),
            None => return None,
        };

        match v_obj {
            PdfObject::String(s) => {
                let text = decode_pdf_text_string(&s);
                if text.is_empty() {
                    None
                } else {
                    Some(text)
                }
            }
            PdfObject::Name(name) => {
                let text = String::from_utf8_lossy(&name).into_owned();
                if text.is_empty() || text == "Off" {
                    None
                } else {
                    Some(text)
                }
            }
            PdfObject::Reference(r) => match self.resolver.resolve(r) {
                Ok(PdfObject::String(s)) => {
                    let text = decode_pdf_text_string(&s);
                    if text.is_empty() {
                        None
                    } else {
                        Some(text)
                    }
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Resolve a sub-dictionary from an annotation dict entry.
    /// Handles both direct dictionaries and indirect references.
    fn resolve_annot_sub_dict(
        &mut self,
        parent: &PdfDictionary,
        key: &[u8],
    ) -> Option<PdfDictionary> {
        match parent.get(key)? {
            PdfObject::Dictionary(d) => Some(d.clone()),
            PdfObject::Reference(r) => self.resolver.resolve_dict(*r).ok(),
            _ => None,
        }
    }

    /// Emit a warning related to annotation processing.
    fn warn_annot(&self, message: &str) {
        self.diagnostics.warning(Warning::with_context(
            None,
            WarningKind::InvalidState,
            WarningContext {
                page_index: Some(self.index),
                obj_ref: None,
            },
            format!("annotations: {message}"),
        ));
    }

    /// Extract all page content in a single interpretation pass.
    ///
    /// Returns both text spans and images from one pass over the content
    /// stream. Use this instead of calling [`raw_spans()`](Page::raw_spans)
    /// and [`images()`](Page::images) separately when you need both.
    ///
    /// Note: `paths` will be empty in the returned [`PageContent`]. Use
    /// [`Page::paths()`] for raw path segments or [`Page::tables()`] for
    /// table extraction.
    pub fn extract(&mut self) -> Result<PageContent> {
        self.interpret_page(true)
    }

    /// Extract all page content (text, images, and paths) in a single pass.
    ///
    /// Like [`extract()`](Page::extract) but also captures path segments.
    /// Use this when you need both text and tables from the same page to
    /// avoid interpreting the content stream twice:
    ///
    /// ```ignore
    /// // ignore: needs an open `Page` and `extract_tables` (a private helper),
    /// // both threading the page resolver lifetime; the snippet illustrates
    /// // the call shape rather than a runnable example.
    /// let content = page.extract_all()?;
    /// let text = content.text();
    /// let tables = extract_tables(&content.paths, &content.spans, &page_bbox, &diag);
    /// ```
    pub fn extract_all(&mut self) -> Result<PageContent> {
        self.interpret_page_full(true, true)
    }

    /// Extract raw text spans in content stream order.
    ///
    /// Each span represents one text-showing operation (Tj/TJ) with its
    /// position, font, and Unicode text. No ordering or line grouping is applied.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use udoc_pdf::Document;
    ///
    /// # let pdf_bytes = std::fs::read(
    /// #     std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    /// #         .join("tests/corpus/minimal/hex_string.pdf"),
    /// # ).unwrap();
    /// let mut doc = Document::from_bytes(pdf_bytes)?;
    /// let spans = doc.page(0)?.raw_spans()?;
    /// for span in &spans {
    ///     println!("({:.0}, {:.0}): {}", span.x, span.y, span.text);
    /// }
    /// # Ok::<(), udoc_pdf::Error>(())
    /// ```
    pub fn raw_spans(&mut self) -> Result<Vec<TextSpan>> {
        self.interpret_page(false).map(|pc| pc.spans)
    }

    /// Extract text as ordered lines.
    ///
    /// Spans are grouped by baseline proximity into [`TextLine`]s, sorted
    /// top-to-bottom. Within each line, spans are sorted left-to-right
    /// with word gap detection.
    ///
    /// For tagged PDFs with a structure tree, spans are first reordered
    /// by MCID (marked content ID) according to the document's logical
    /// structure before geometric ordering is applied.
    ///
    /// Note: internally interprets the content stream without image extraction.
    /// Use [`extract()`](Page::extract) when you need both text and images to
    /// avoid redundant work.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use udoc_pdf::Document;
    ///
    /// # let pdf_bytes = std::fs::read(
    /// #     std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    /// #         .join("tests/corpus/minimal/multipage.pdf"),
    /// # ).unwrap();
    /// let mut doc = Document::from_bytes(pdf_bytes)?;
    /// let lines = doc.page(0)?.text_lines()?;
    /// for line in &lines {
    ///     println!("y={:.0}: {}", line.baseline, line.text());
    /// }
    /// # Ok::<(), udoc_pdf::Error>(())
    /// ```
    pub fn text_lines(&mut self) -> Result<Vec<TextLine>> {
        let spans = self.raw_spans()?;
        let structure_order = self.page_structure_order();
        let diag = OrderDiagnostics {
            sink: self.diagnostics.as_ref(),
            page_index: self.index,
        };
        Ok(order_spans_with_diagnostics(
            spans,
            structure_order.as_ref(),
            Some(&diag),
        ))
    }

    /// Get the page bounding box from /CropBox or /MediaBox.
    ///
    /// Resolves indirect references. Per ISO 32000-1 §7.7.3.4, /MediaBox and
    /// /CropBox are inheritable: when absent from the leaf /Page dict, walk
    /// up the /Parent chain. Falls back to US Letter (612x792 pt) only if
    /// no box is found anywhere in the tree, emitting a diagnostic warning.
    pub fn page_bbox(&mut self) -> BoundingBox {
        // Try /CropBox first, then /MediaBox. Each key walks up the parent
        // chain independently because /CropBox may be declared on the leaf
        // while /MediaBox is inherited, or vice versa.
        for key in &[b"CropBox" as &[u8], b"MediaBox" as &[u8]] {
            if let Some(bbox) = self.resolve_inherited_bbox(key) {
                return bbox;
            }
        }
        // Fallback: US Letter
        self.diagnostics.warning(Warning::with_context(
            None,
            WarningKind::InvalidState,
            WarningContext {
                page_index: Some(self.index),
                obj_ref: None,
            },
            "no valid /CropBox or /MediaBox, using US Letter fallback (612x792)",
        ));
        BoundingBox::new(0.0, 0.0, 612.0, 792.0)
    }

    /// Walk the /Page -> /Parent chain looking for an inheritable box entry
    /// (/MediaBox or /CropBox). Returns the first valid 4-element numeric
    /// array found. Stops on cycles or when /Parent is missing.
    fn resolve_inherited_bbox(&mut self, key: &[u8]) -> Option<BoundingBox> {
        fn parse_bbox(obj: PdfObject) -> Option<BoundingBox> {
            let PdfObject::Array(arr) = obj else {
                return None;
            };
            if arr.len() != 4 {
                return None;
            }
            let vals: Vec<f64> = arr.iter().map(|o| o.as_f64().unwrap_or(0.0)).collect();
            Some(BoundingBox::new(vals[0], vals[1], vals[2], vals[3]))
        }

        // Leaf page dict first.
        if let Ok(Some(obj)) = self.resolver.get_and_resolve(&self.dict, key) {
            if let Some(bbox) = parse_bbox(obj) {
                return Some(bbox);
            }
        }

        // Walk up /Parent chain. Same depth cap + cycle guard as
        // resolve_page_resources.
        let mut parent_ref = self.dict.get_ref(b"Parent")?;
        let mut visited = HashSet::new();
        for _ in 0..MAX_PARENT_CHAIN_DEPTH {
            if !visited.insert(parent_ref) {
                break;
            }
            let parent_dict = self.resolver.resolve_dict(parent_ref).ok()?;
            if let Ok(Some(obj)) = self.resolver.get_and_resolve(&parent_dict, key) {
                if let Some(bbox) = parse_bbox(obj) {
                    return Some(bbox);
                }
            }
            parent_ref = parent_dict.get_ref(b"Parent")?;
        }
        None
    }
    /// Get the page rotation in degrees (0, 90, 180, 270).
    ///
    /// Per PDF spec 7.7.3.4, /Rotate is inheritable: if absent on the page
    /// dict, walk the /Parent chain. Returns 0 if absent everywhere or the
    /// value is not a valid multiple of 90.
    pub fn rotation(&mut self) -> u16 {
        // Check page dict first, then walk /Parent chain.
        let mut current_dict: Option<PdfDictionary> = None;
        let mut visited: HashSet<ObjRef> = HashSet::new();
        let mut parent_ref_opt = self.dict.get_ref(b"Parent");

        loop {
            let dict_ref = current_dict.as_ref().unwrap_or(&self.dict);
            match self.resolver.get_and_resolve(dict_ref, b"Rotate") {
                Ok(Some(obj)) => {
                    let deg = obj.as_i64().unwrap_or(0);
                    let normalized = ((deg % 360 + 360) % 360) as u16;
                    return match normalized {
                        0 | 90 | 180 | 270 => normalized,
                        _ => 0,
                    };
                }
                Ok(None) => {}
                Err(_) => {
                    self.diagnostics.warning(Warning::with_context(
                        None,
                        WarningKind::InvalidState,
                        WarningContext {
                            page_index: Some(self.index),
                            obj_ref: None,
                        },
                        "could not resolve /Rotate, defaulting to 0",
                    ));
                    return 0;
                }
            }

            // Advance to parent.
            let parent_ref = match parent_ref_opt {
                Some(r) => r,
                None => return 0,
            };
            if !visited.insert(parent_ref) || visited.len() > MAX_PARENT_CHAIN_DEPTH {
                return 0;
            }
            let parent_dict = match self.resolver.resolve_dict(parent_ref) {
                Ok(d) => d,
                Err(_) => return 0,
            };
            parent_ref_opt = parent_dict.get_ref(b"Parent");
            current_dict = Some(parent_dict);
        }
    }

    /// Get the structure tree ordering for this page, if available.
    fn page_structure_order(&self) -> Option<marked_content::PageStructureOrder> {
        self.structure_tree
            .as_ref()
            .and_then(|tree| marked_content::get_page_structure_order(tree, self.page_ref))
    }

    /// Extract images from the page.
    ///
    /// Returns all images found in the content stream, both inline images
    /// (BI/ID/EI) and XObject images (Do operator). For images with lossy
    /// compression (JPEG, JPEG2000), the raw encoded bytes are passed
    /// through to avoid re-encoding artifacts. Check [`PageImage::filter`]
    /// to determine how to interpret the data bytes.
    pub fn images(&mut self) -> Result<Vec<PageImage>> {
        self.interpret_page(true).map(|pc| pc.images)
    }

    /// Extract flattened path segments (lines, rectangles, polygons) from
    /// the page.
    ///
    /// Returns the pre-CTM-transformed, curve-flattened segments consumed
    /// by the built-in table detector. Useful for diagram extraction,
    /// form-field detection, or custom table algorithms.
    ///
    /// For the raw, CTM-preserving, cubic-curve-preserving IR consumed by
    /// the page renderer, use [`Page::paths`] instead.
    pub fn path_segments(&mut self) -> Result<Vec<PathSegment>> {
        self.interpret_page_full(false, true).map(|pc| pc.paths)
    }

    /// Extract canonical page paths for renderer consumption.
    ///
    /// Each path-painting operator in the page content stream emits one
    /// [`PagePath`](crate::content::path::PagePath) with:
    ///
    /// - Canonical [`PathSegmentKind`](crate::content::path::PathSegmentKind)
    ///   moveto/lineto/curveto/closepath segments in user space (cubic
    ///   curves preserved, `v`/`y`/`re` expanded).
    /// - Explicit [`FillRule`](crate::content::path::FillRule) for filled
    ///   paths (no implicit default).
    /// - [`StrokeStyle`](crate::content::path::StrokeStyle) snapshot taken
    ///   at paint time (line width, cap, join, miter limit, dash pattern,
    ///   color).
    /// - CTM snapshot at the moment of the paint operator
    ///   ([`Matrix3`](crate::content::path::Matrix3)).
    /// - A paint-order index `z` for deterministic back-to-front
    ///   composition.
    ///
    /// Clip-only paths (W/W* followed by a paint, or the `n` operator)
    /// are not emitted here; will route those through a
    /// dedicated clip stack.
    pub fn paths(&mut self) -> Result<Vec<crate::content::path::PagePath>> {
        self.page_paths_ir()
    }

    /// Return the shading-pattern records emitted by 'sh' operators on
    /// this page, paired with the paths (same interpreter pass).
    ///
    /// Each entry carries the shading geometry + a pre-sampled color
    /// LUT, the CTM snapshot at the paint op, and a monotonic `z`
    /// index sharing the back-to-front ordering with returned paths.
    /// Types 2 (axial) and 3 (radial) decode fully; other types are
    /// recorded as [`PageShadingKind::Unsupported`](crate::content::path::PageShadingKind::Unsupported)
    /// and the renderer skips them.
    ///
    /// Prefer [`paths_and_shadings`](Self::paths_and_shadings) when you
    /// want both -- it runs the content stream exactly once.
    ///
    /// (ISO 32000-2 §8.7.4).
    pub fn shadings(&mut self) -> Result<Vec<crate::content::path::PageShading>> {
        Ok(self.page_paths_and_shadings_ir()?.1)
    }

    /// Return both paths and shadings from a single interpreter pass.
    /// Use this instead of calling [`paths`](Self::paths) and
    /// [`shadings`](Self::shadings) separately.
    pub fn paths_and_shadings(
        &mut self,
    ) -> Result<(
        Vec<crate::content::path::PagePath>,
        Vec<crate::content::path::PageShading>,
    )> {
        self.page_paths_and_shadings_ir()
    }

    /// Like [`paths_and_shadings`](Self::paths_and_shadings) but also
    /// returns Type 1 coloured tiling-pattern records (one per path
    /// paint op fired with a Pattern-colorspace fill bound). Prefer
    /// this when the renderer needs all three streams.
    ///
    ///ISO 32000-2 §8.7.3, .
    pub fn paths_shadings_and_patterns(
        &mut self,
    ) -> Result<(
        Vec<crate::content::path::PagePath>,
        Vec<crate::content::path::PageShading>,
        Vec<crate::content::path::PageTilingPattern>,
    )> {
        self.page_paint_records_ir()
    }

    /// Detect and extract tables from the page.
    ///
    /// Uses ruled-line detection to find tables formed by stroked/filled
    /// paths (rectangles, line segments). Each table contains rows and
    /// cells with extracted text content.
    ///
    /// Returns an empty vector if no tables are detected on the page.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use udoc_pdf::Document;
    ///
    /// # let pdf_bytes = std::fs::read(
    /// #     std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    /// #         .join("tests/corpus/minimal/table_layout.pdf"),
    /// # ).unwrap();
    /// let mut doc = Document::from_bytes(pdf_bytes)?;
    /// let tables = doc.page(0)?.tables()?;
    /// for table in &tables {
    ///     println!("Table: {} rows, {} cols", table.rows.len(), table.num_columns);
    /// }
    /// # Ok::<(), udoc_pdf::Error>(())
    /// ```
    pub fn tables(&mut self) -> Result<Vec<Table>> {
        let content = self.interpret_page_full(false, true)?;
        let page_bbox = self.page_bbox();
        Ok(extract_tables(
            &content.paths,
            &content.spans,
            &page_bbox,
            self.diagnostics.as_ref(),
        ))
    }

    /// Extract the full page text as a string.
    ///
    /// Lines are joined by newlines. This is the simplest API for
    /// getting readable text from a page. Internally re-interprets the
    /// content stream; use [`extract()`](Page::extract) and
    /// [`PageContent::text()`] to avoid redundant work when you also
    /// need images or raw spans.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use udoc_pdf::Document;
    ///
    /// # let pdf_bytes = std::fs::read(
    /// #     std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    /// #         .join("tests/corpus/minimal/hex_string.pdf"),
    /// # ).unwrap();
    /// let mut doc = Document::from_bytes(pdf_bytes)?;
    /// let text = doc.page(0)?.text()?;
    /// assert!(!text.is_empty());
    /// # Ok::<(), udoc_pdf::Error>(())
    /// ```
    pub fn text(&mut self) -> Result<String> {
        let lines = self.text_lines()?;
        Ok(join_text_lines(&lines))
    }

    /// Render the page's text spans onto a monospace grid using their
    /// PDF coordinates, returning one line per row.
    ///
    /// This is the position-faithful counterpart to [`Page::text`].
    /// Reading-order extraction infers logical flow and returns prose;
    /// layout-mode rendering preserves visual columns and tabular
    /// alignment by projecting each glyph onto a terminal grid sized to
    /// `opts.columns` cells wide. Equivalent to poppler's
    /// `pdftotext -layout`.
    ///
    /// Glyph bboxes are used for placement when populated by the
    /// content interpreter; otherwise chars are distributed evenly
    /// across the span's advance width. Invisible spans and (by
    /// default) rotated spans are skipped.
    pub fn text_layout(&mut self, opts: &crate::text::LayoutOptions) -> Result<String> {
        let spans = self.raw_spans()?;
        Ok(crate::text::render_layout(&spans, opts))
    }

    /// Extract hyperlinks from /Link annotations on this page.
    ///
    /// Walks the /Annots array, filters for /Subtype /Link, and extracts
    /// the URL from /A or /Dest entries. Supports /URI (external), /GoTo
    /// (internal), and direct /Dest destinations.
    pub fn links(&mut self) -> Vec<PageLink> {
        // Collect annotation item identifiers without cloning the entire
        // Annots array. Items are almost always indirect references
        // (ObjRef); inline dicts are rare, and we filter them by /Subtype
        // before cloning so non-Link inline annotations (Text, Highlight,
        // Ink, ...) never allocate (#153).
        enum AnnotItem {
            Ref(ObjRef),
            InlineDict(PdfDictionary),
        }

        fn is_link_dict(d: &PdfDictionary) -> bool {
            d.get_name(b"Subtype") == Some(b"Link")
        }

        let items: Vec<AnnotItem> = match self.dict.get(b"Annots") {
            Some(PdfObject::Array(arr)) => arr
                .iter()
                .filter_map(|obj| match obj {
                    PdfObject::Reference(r) => Some(AnnotItem::Ref(*r)),
                    PdfObject::Dictionary(d) if is_link_dict(d) => {
                        Some(AnnotItem::InlineDict(d.clone()))
                    }
                    _ => None,
                })
                .collect(),
            Some(PdfObject::Reference(r)) => match self.resolver.resolve(*r) {
                Ok(PdfObject::Array(arr)) => arr
                    .into_iter()
                    .filter_map(|obj| match obj {
                        PdfObject::Reference(r) => Some(AnnotItem::Ref(r)),
                        PdfObject::Dictionary(d) if is_link_dict(&d) => {
                            Some(AnnotItem::InlineDict(d))
                        }
                        _ => None,
                    })
                    .collect(),
                _ => return Vec::new(),
            },
            _ => return Vec::new(),
        };

        let mut links = Vec::new();
        for (i, item) in items.into_iter().enumerate() {
            if i >= MAX_ANNOTATIONS_PER_PAGE {
                self.warn_annot(&format!(
                    "link annotation limit ({MAX_ANNOTATIONS_PER_PAGE}) reached, skipping remaining"
                ));
                break;
            }

            let annot_dict = match item {
                AnnotItem::Ref(r) => match self.resolver.resolve_dict(r) {
                    Ok(d) => d,
                    Err(e) => {
                        self.diagnostics.warning(Warning::new(
                            None,
                            WarningKind::InvalidState,
                            format!("failed to resolve annotation {}: {e}", r),
                        ));
                        continue;
                    }
                },
                AnnotItem::InlineDict(d) => d,
            };

            // Only process /Link annotations.
            if annot_dict.get_name(b"Subtype") != Some(b"Link") {
                continue;
            }

            // Extract URL from /A action or direct /Dest.
            let url = if let Some(action_obj) = annot_dict.get(b"A") {
                self.extract_link_action(action_obj)
            } else if let Some(dest_obj) = annot_dict.get(b"Dest") {
                self.extract_link_dest(dest_obj)
            } else {
                None
            };

            if let Some(url) = url {
                let bbox = annot_dict
                    .get(b"Rect")
                    .and_then(|r| r.as_array())
                    .and_then(|arr| {
                        if arr.len() >= 4 {
                            let llx = arr[0].as_f64()?;
                            let lly = arr[1].as_f64()?;
                            let urx = arr[2].as_f64()?;
                            let ury = arr[3].as_f64()?;
                            // Normalize: PDF spec 12.5.2 allows inverted rects.
                            Some(BoundingBox {
                                x_min: llx.min(urx),
                                y_min: lly.min(ury),
                                x_max: llx.max(urx),
                                y_max: lly.max(ury),
                            })
                        } else {
                            None
                        }
                    });
                links.push(PageLink { url, bbox });
            }
        }
        links
    }

    /// Enumerate all renderable annotations on this page (
    /// #170).
    ///
    /// Walks the `/Annots` array, resolves each annotation, and extracts
    /// the fields needed to draw the annotation's visual appearance per
    /// ISO 32000-2 §12.5.5:
    ///
    /// * `/Rect` (normalized), `/F` flag bits, `/C` colour, `/Contents`.
    /// * `/QuadPoints` for text-markup subtypes
    ///   (Highlight/Underline/StrikeOut/Squiggly).
    /// * `/InkList` for Ink annotations.
    /// * Decoded `/AP/N` stream + `/BBox` + `/Matrix` + resources for
    ///   Stamp/Watermark/FreeText and other subtypes that carry a
    ///   pre-rendered appearance.
    ///
    /// Decorative-only subtypes with no visible output (e.g. `/Popup`,
    /// `/Sound`, `/Movie`, `/Screen`) are skipped. Annotations with
    /// `/F` bit 2 (Hidden) or bit 6 (NoView) are still returned so
    /// callers can inspect them; use
    /// [`PageAnnotation::is_hidden`](PageAnnotation::is_hidden) to
    /// filter.
    ///
    /// Errors on individual annotations are logged via the diagnostics
    /// sink and the annotation is skipped; the method does not fail
    /// the whole page.
    pub fn annotations(&mut self) -> Vec<PageAnnotation> {
        let annots_obj = match self.dict.get(b"Annots") {
            Some(obj) => obj.clone(),
            None => return Vec::new(),
        };

        let annot_items: Vec<PdfObject> = match annots_obj {
            PdfObject::Array(arr) => arr,
            PdfObject::Reference(r) => match self.resolver.resolve(r) {
                Ok(PdfObject::Array(arr)) => arr,
                _ => return Vec::new(),
            },
            _ => return Vec::new(),
        };

        let mut out: Vec<PageAnnotation> = Vec::new();
        for (i, item) in annot_items.iter().enumerate() {
            if i >= MAX_ANNOTATIONS_PER_PAGE {
                self.warn_annot(&format!(
                    "annotation limit ({MAX_ANNOTATIONS_PER_PAGE}) reached, skipping remaining"
                ));
                break;
            }
            let annot_dict = match item {
                PdfObject::Dictionary(d) => d.clone(),
                PdfObject::Reference(r) => match self.resolver.resolve_dict(*r) {
                    Ok(d) => d,
                    Err(e) => {
                        self.warn_annot(&format!("failed to resolve annotation {r}: {e}"));
                        continue;
                    }
                },
                _ => continue,
            };
            if let Some(ann) = self.build_page_annotation(&annot_dict) {
                out.push(ann);
            }
        }
        out
    }

    /// Classify an annotation dictionary and build a [`PageAnnotation`].
    /// Returns `None` when the annotation is decorative-only (Popup,
    /// Sound, Movie, Screen, ...) or lacks a valid /Rect.
    fn build_page_annotation(&mut self, annot_dict: &PdfDictionary) -> Option<PageAnnotation> {
        let subtype = annot_dict.get_name(b"Subtype")?;
        let kind = match subtype {
            b"Highlight" => PageAnnotationKind::Highlight,
            b"Underline" => PageAnnotationKind::Underline,
            b"StrikeOut" => PageAnnotationKind::StrikeOut,
            b"Squiggly" => PageAnnotationKind::Squiggly,
            b"Stamp" => PageAnnotationKind::Stamp,
            b"Watermark" => PageAnnotationKind::Watermark,
            b"Link" => PageAnnotationKind::Link,
            b"FreeText" => PageAnnotationKind::FreeText,
            b"Ink" => PageAnnotationKind::Ink,
            // Decorative / non-visual / tooltip-only subtypes.
            b"Popup" | b"Sound" | b"Movie" | b"Screen" | b"PrinterMark" | b"TrapNet" => {
                return None;
            }
            // Other subtypes (Square, Circle, Line, Polygon, PolyLine,
            // Caret, FileAttachment, RichMedia, Widget, Text) may carry
            // an /AP/N that we can composite blindly. Fall through.
            _ => PageAnnotationKind::OtherWithAppearance,
        };

        // /Rect is required for placement.
        let rect_arr = annot_dict.get_array(b"Rect")?;
        if rect_arr.len() < 4 {
            return None;
        }
        let llx = rect_arr[0].as_f64()?;
        let lly = rect_arr[1].as_f64()?;
        let urx = rect_arr[2].as_f64()?;
        let ury = rect_arr[3].as_f64()?;
        let rect = udoc_core::geometry::BoundingBox::new(
            llx.min(urx),
            lly.min(ury),
            llx.max(urx),
            lly.max(ury),
        );

        // /F flags (unsigned integer bitfield).
        let flags = annot_dict.get_i64(b"F").unwrap_or(0) as u32;

        // /C colour: 1 entry = grayscale, 3 = RGB, 4 = CMYK. Map to RGB.
        let color = annot_dict.get_array(b"C").and_then(|arr| match arr.len() {
            1 => {
                let g = arr[0].as_f64()?;
                let v = (g.clamp(0.0, 1.0) * 255.0).round() as u8;
                Some([v, v, v])
            }
            3 => {
                let r = (arr[0].as_f64()?.clamp(0.0, 1.0) * 255.0).round() as u8;
                let g = (arr[1].as_f64()?.clamp(0.0, 1.0) * 255.0).round() as u8;
                let b = (arr[2].as_f64()?.clamp(0.0, 1.0) * 255.0).round() as u8;
                Some([r, g, b])
            }
            4 => {
                let c = arr[0].as_f64()?.clamp(0.0, 1.0);
                let m = arr[1].as_f64()?.clamp(0.0, 1.0);
                let y = arr[2].as_f64()?.clamp(0.0, 1.0);
                let k = arr[3].as_f64()?.clamp(0.0, 1.0);
                // Naive CMYK -> RGB (no ICC). Matches .
                let r = ((1.0 - c) * (1.0 - k) * 255.0).round() as u8;
                let g = ((1.0 - m) * (1.0 - k) * 255.0).round() as u8;
                let b = ((1.0 - y) * (1.0 - k) * 255.0).round() as u8;
                Some([r, g, b])
            }
            _ => None,
        });

        // /QuadPoints: array of f64 in groups of 8 (4 corners per quad).
        let quad_points: Vec<f64> = annot_dict
            .get_array(b"QuadPoints")
            .map(|arr| arr.iter().filter_map(|o| o.as_f64()).collect())
            .unwrap_or_default();

        // /Border = [h_radius v_radius width [dash_array]]. /BS /W takes
        // precedence when present.
        let border_width = {
            let from_bs = annot_dict.get_dict(b"BS").and_then(|bs| bs.get_f64(b"W"));
            let from_border = annot_dict
                .get_array(b"Border")
                .and_then(|arr| arr.get(2).and_then(|o| o.as_f64()));
            from_bs.or(from_border).unwrap_or(match kind {
                PageAnnotationKind::Link => 0.0,
                _ => 1.0,
            })
        };

        // /Contents string.
        let contents = extract_string_value(annot_dict, b"Contents");

        // /InkList: array of arrays of numbers (one stroked polyline per entry).
        let ink_list: Vec<Vec<(f64, f64)>> = annot_dict
            .get_array(b"InkList")
            .map(|arr| {
                arr.iter()
                    .filter_map(|o| o.as_array())
                    .map(|pts| {
                        let nums: Vec<f64> = pts.iter().filter_map(|o| o.as_f64()).collect();
                        nums.chunks_exact(2).map(|c| (c[0], c[1])).collect()
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Resolve /AP/N appearance stream, its /BBox + /Matrix + resources.
        let (ap_stream, ap_resources, ap_bbox, ap_matrix) =
            self.resolve_appearance_stream(annot_dict);

        Some(PageAnnotation {
            kind,
            rect,
            flags,
            color,
            quad_points,
            border_width,
            contents,
            ap_stream,
            ap_resources,
            ap_bbox,
            ap_matrix,
            ink_list,
        })
    }

    /// Resolve the /AP/N (normal-appearance) stream and its metadata.
    ///
    /// Handles both direct-stream /N and /AS-keyed sub-dicts. Returns
    /// (decoded_bytes, resources, bbox, matrix). Returns `None` for any
    /// field that is missing or unreadable; errors are logged.
    #[allow(clippy::type_complexity)]
    fn resolve_appearance_stream(
        &mut self,
        annot_dict: &PdfDictionary,
    ) -> (
        Option<Vec<u8>>,
        Option<PdfDictionary>,
        Option<[f64; 4]>,
        Option<[f64; 6]>,
    ) {
        let ap_dict = match self.resolve_annot_sub_dict(annot_dict, b"AP") {
            Some(d) => d,
            None => return (None, None, None, None),
        };
        let n_entry = match ap_dict.get(b"N") {
            Some(obj) => obj.clone(),
            None => return (None, None, None, None),
        };
        let stream_ref = match n_entry {
            PdfObject::Reference(r) => r,
            PdfObject::Dictionary(ref sub_dict) => {
                let as_name = annot_dict.get_name(b"AS").unwrap_or(b"Off");
                match sub_dict.get_ref(as_name) {
                    Some(r) => r,
                    None => return (None, None, None, None),
                }
            }
            _ => return (None, None, None, None),
        };
        let stream = match self.resolver.resolve(stream_ref) {
            Ok(PdfObject::Stream(s)) => s,
            _ => return (None, None, None, None),
        };

        let bbox = stream
            .dict
            .get_array(b"BBox")
            .filter(|arr| arr.len() >= 4)
            .and_then(|arr| {
                Some([
                    arr[0].as_f64()?,
                    arr[1].as_f64()?,
                    arr[2].as_f64()?,
                    arr[3].as_f64()?,
                ])
            });

        let matrix = stream
            .dict
            .get_array(b"Matrix")
            .filter(|arr| arr.len() >= 6)
            .and_then(|arr| {
                Some([
                    arr[0].as_f64()?,
                    arr[1].as_f64()?,
                    arr[2].as_f64()?,
                    arr[3].as_f64()?,
                    arr[4].as_f64()?,
                    arr[5].as_f64()?,
                ])
            });

        let resources = self
            .resolver
            .get_resolved_dict(&stream.dict, b"Resources")
            .ok()
            .flatten();

        let decoded = match self.resolver.decode_stream_data(&stream, Some(stream_ref)) {
            Ok(d) => Some(d),
            Err(e) => {
                self.warn_annot(&format!(
                    "failed to decode appearance stream {stream_ref}: {e}"
                ));
                None
            }
        };

        (decoded, resources, bbox, matrix)
    }

    /// Extract URL from a /A action dictionary.
    fn extract_link_action(&mut self, action_obj: &PdfObject) -> Option<String> {
        let action_dict = match action_obj {
            PdfObject::Dictionary(d) => d.clone(),
            PdfObject::Reference(r) => match self.resolver.resolve_dict(*r) {
                Ok(d) => d,
                Err(e) => {
                    self.diagnostics.warning(Warning::new(
                        None,
                        WarningKind::InvalidState,
                        format!("failed to resolve link action {}: {e}", r),
                    ));
                    return None;
                }
            },
            _ => return None,
        };

        let action_type = action_dict.get_name(b"S")?;
        match action_type {
            b"URI" => {
                // External link: /S /URI /URI (string)
                let mut uri = extract_string_value(&action_dict, b"URI")?;
                if uri.len() > MAX_URI_LENGTH {
                    self.diagnostics.warning(Warning::new(
                        None,
                        WarningKind::ResourceLimit,
                        format!(
                            "URI truncated from {} to {} bytes",
                            uri.len(),
                            MAX_URI_LENGTH
                        ),
                    ));
                    uri.truncate(MAX_URI_LENGTH);
                }
                Some(uri)
            }
            b"GoTo" => {
                // Internal link: /S /GoTo /D (destination)
                if let Some(dest) = action_dict.get(b"D") {
                    self.extract_link_dest(dest)
                } else {
                    None
                }
            }
            // /GoToR (remote document), /Launch (application), /Named, /Thread
            // are deliberately unsupported in v1. Returns None so the link is
            // silently dropped.
            _ => None,
        }
    }

    /// Extract a human-readable destination string from a /Dest value.
    fn extract_link_dest(&mut self, dest_obj: &PdfObject) -> Option<String> {
        self.extract_link_dest_inner(dest_obj, 0)
    }

    fn extract_link_dest_inner(&mut self, dest_obj: &PdfObject, depth: usize) -> Option<String> {
        if depth >= MAX_LINK_DEST_DEPTH {
            return None;
        }
        match dest_obj {
            PdfObject::String(s) => {
                // Try to resolve the named destination via the catalog name tree
                if let Some(resolved) = resolve_named_dest(
                    self.resolver,
                    s.as_bytes(),
                    self.diagnostics.as_ref(),
                    self.cached_catalog,
                ) {
                    if let Some(result) = self.extract_link_dest_inner(&resolved, depth + 1) {
                        return Some(result);
                    }
                }
                // Fallback: return the name as-is
                let text = decode_pdf_text_bytes(s.as_bytes());
                Some(format!("#{text}"))
            }
            PdfObject::Name(n) => {
                // Try to resolve the named destination via the catalog name tree
                if let Some(resolved) = resolve_named_dest(
                    self.resolver,
                    n,
                    self.diagnostics.as_ref(),
                    self.cached_catalog,
                ) {
                    if let Some(result) = self.extract_link_dest_inner(&resolved, depth + 1) {
                        return Some(result);
                    }
                }
                // Fallback: return the name as-is
                let name = String::from_utf8_lossy(n);
                Some(format!("#{name}"))
            }
            PdfObject::Array(arr) => {
                // [page_ref /FitType ...] -- extract the fit type when present.
                if arr.is_empty() {
                    return None;
                }
                let fit_type = arr
                    .get(1)
                    .and_then(|obj| match obj {
                        PdfObject::Name(n) => Some(String::from_utf8_lossy(n).to_string()),
                        _ => None,
                    })
                    .unwrap_or_default();
                if fit_type.is_empty() {
                    Some("#page".to_string())
                } else {
                    Some(format!("#page/{fit_type}"))
                }
            }
            PdfObject::Reference(r) => match self.resolver.resolve(*r) {
                Ok(resolved) => self.extract_link_dest_inner(&resolved, depth + 1),
                Err(e) => {
                    self.diagnostics.warning(Warning::new(
                        None,
                        WarningKind::InvalidState,
                        format!("failed to resolve link dest {}: {e}", r),
                    ));
                    None
                }
            },
            _ => None,
        }
    }

    /// Extract all processed content from the page in a single pass.
    ///
    /// Returns text lines (with structure-tree ordering), raw spans,
    /// detected tables, and images. Interprets the content stream once
    /// with both image and path extraction enabled, then derives
    /// text lines and tables from that single pass.
    ///
    /// Prefer this over calling [`text_lines()`](Page::text_lines),
    /// [`tables()`](Page::tables), [`images()`](Page::images), and
    /// [`raw_spans()`](Page::raw_spans) separately when you need more
    /// than one of these outputs: each of those methods re-interprets
    /// the content stream independently.
    pub fn extract_full(&mut self) -> Result<FullPageContent> {
        let content = self.interpret_page_full(true, true)?;
        let structure_order = self.page_structure_order();
        let diag = OrderDiagnostics {
            sink: self.diagnostics.as_ref(),
            page_index: self.index,
        };
        let text_lines = order_spans_with_diagnostics(
            content.spans.clone(),
            structure_order.as_ref(),
            Some(&diag),
        );
        let page_bbox = self.page_bbox();
        let tables = extract_tables(
            &content.paths,
            &content.spans,
            &page_bbox,
            self.diagnostics.as_ref(),
        );
        // Look up alt texts from the pre-built per-page index (#150).
        // An empty map here means the PDF is untagged or has no /Alt
        // entries for this page; in either case image.alt_text stays None.
        let alt_text_map = self.alt_text_index.alt_texts_for_page(self.page_ref);
        let annotated_images: Vec<AnnotatedImage<PageImage>> = content
            .images
            .into_iter()
            .map(|img| {
                let alt_text = img.mcid.and_then(|mcid| alt_text_map.get(&mcid).cloned());
                AnnotatedImage {
                    image: img,
                    alt_text,
                }
            })
            .collect();
        Ok(FullPageContent {
            text_lines,
            raw_spans: content.spans,
            tables,
            images: annotated_images,
        })
    }
}

/// Parse /Rect from an annotation dictionary, returning (llx, ury) -- the
/// lower-left X and upper-right Y, used as the text anchor position near
/// the top of the annotation box.
///
/// /Rect is [llx, lly, urx, ury]. We use (llx, ury) so that text appears
/// near the top of the annotation box (PDF Y-axis goes upward, so ury is
/// the top edge in device space).
fn parse_rect(dict: &PdfDictionary) -> Option<(f64, f64)> {
    let arr = dict.get_array(b"Rect")?;
    if arr.len() < 4 {
        return None;
    }
    let llx = arr[0].as_f64()?;
    let ury = arr[3].as_f64()?;
    Some((llx, ury))
}

/// Extract a string value from a dictionary entry, decoding to Unicode.
fn extract_string_value(dict: &PdfDictionary, key: &[u8]) -> Option<String> {
    let s = dict.get_str(key)?;
    Some(decode_pdf_text_string(s))
}

/// Decode a PDF text string to Unicode.
///
/// Delegates to the shared `decode_pdf_text_bytes` which handles both
/// UTF-16BE (with BOM) and PDFDocEncoding (Table D.2).
#[inline]
fn decode_pdf_text_string(s: &PdfString) -> String {
    decode_pdf_text_bytes(s.as_bytes())
}

/// ISO 32000-2 §12.5.5 form-to-page composite: `Matrix * RectFit`.
///
/// Maps a point from form space through the annotation's /Matrix into
/// transformed-form space, then through the rect-fit onto the
/// annotation's /Rect in page user space. Returned as a row-vector 6-
/// element affine matrix.
fn compose_appearance_matrix(
    rect: udoc_core::geometry::BoundingBox,
    bbox: [f64; 4],
    matrix: [f64; 6],
) -> [f64; 6] {
    // Transform /BBox corners by /Matrix and take the axis-aligned bound.
    let corners = [
        (bbox[0], bbox[1]),
        (bbox[2], bbox[1]),
        (bbox[2], bbox[3]),
        (bbox[0], bbox[3]),
    ];
    let apply = |m: [f64; 6], x: f64, y: f64| -> (f64, f64) {
        (x * m[0] + y * m[2] + m[4], x * m[1] + y * m[3] + m[5])
    };
    let mut tx_min = f64::INFINITY;
    let mut ty_min = f64::INFINITY;
    let mut tx_max = f64::NEG_INFINITY;
    let mut ty_max = f64::NEG_INFINITY;
    for (cx, cy) in corners {
        let (x, y) = apply(matrix, cx, cy);
        tx_min = tx_min.min(x);
        ty_min = ty_min.min(y);
        tx_max = tx_max.max(x);
        ty_max = ty_max.max(y);
    }
    let dx = (tx_max - tx_min).max(1e-9);
    let dy = (ty_max - ty_min).max(1e-9);
    let sx = (rect.x_max - rect.x_min) / dx;
    let sy = (rect.y_max - rect.y_min) / dy;
    let tx = rect.x_min - tx_min * sx;
    let ty = rect.y_min - ty_min * sy;
    let fit: [f64; 6] = [sx, 0.0, 0.0, sy, tx, ty];
    // Row-vector matrix multiply: `matrix * fit`.
    [
        matrix[0] * fit[0] + matrix[1] * fit[2],
        matrix[0] * fit[1] + matrix[1] * fit[3],
        matrix[2] * fit[0] + matrix[3] * fit[2],
        matrix[2] * fit[1] + matrix[3] * fit[3],
        matrix[4] * fit[0] + matrix[5] * fit[2] + fit[4],
        matrix[4] * fit[1] + matrix[5] * fit[3] + fit[5],
    ]
}

/// Maximum depth to traverse the /Parent chain when resolving inherited
/// /Resources. Prevents infinite loops from malformed parent cycles.
const MAX_PARENT_CHAIN_DEPTH: usize = 10;

/// Resolve page /Resources, checking the page dict first then walking up
/// the /Parent chain for inherited resources (PDF spec 7.7.3.4).
///
/// /Resources is one of several inheritable page entries (along with
/// /MediaBox, /CropBox, /Rotate). Many PDF generators place /Resources
/// on the root /Pages node rather than on each individual page.
fn resolve_page_resources(
    resolver: &mut ObjectResolver<'_>,
    page_dict: &PdfDictionary,
) -> Result<PdfDictionary> {
    // Try the page's own /Resources first.
    if let Some(resources) = resolver
        .get_resolved_dict(page_dict, b"Resources")
        .context("resolving page /Resources")?
    {
        return Ok(resources);
    }

    // Walk up /Parent chain looking for inherited /Resources.
    // Track parent_ref directly to avoid cloning page dictionaries.
    let mut parent_ref = match page_dict.get_ref(b"Parent") {
        Some(r) => r,
        None => return Ok(PdfDictionary::default()),
    };

    let mut visited = HashSet::new();
    for _ in 0..MAX_PARENT_CHAIN_DEPTH {
        if !visited.insert(parent_ref) {
            break; // cycle detected
        }

        let parent_dict = resolver
            .resolve_dict(parent_ref)
            .context("resolving parent /Pages node")?;

        if let Some(resources) = resolver
            .get_resolved_dict(&parent_dict, b"Resources")
            .context("resolving inherited /Resources")?
        {
            return Ok(resources);
        }

        match parent_dict.get_ref(b"Parent") {
            Some(r) => parent_ref = r,
            None => break,
        }
    }

    // No /Resources found anywhere in the tree.
    Ok(PdfDictionary::default())
}

/// Walk the /Pages tree and collect all leaf page ObjRefs in document order.
fn collect_page_refs(
    resolver: &mut ObjectResolver<'_>,
    diagnostics: &Arc<dyn DiagnosticsSink>,
) -> Result<Vec<ObjRef>> {
    let trailer = resolver
        .trailer()
        .ok_or_else(|| Error::structure("no trailer in document"))?
        .clone();

    let root_ref = trailer
        .get_ref(b"Root")
        .ok_or_else(|| Error::structure("trailer has no /Root"))?;

    let catalog = resolver
        .resolve_dict(root_ref)
        .context("resolving document catalog")?;

    let pages_ref = catalog
        .get_ref(b"Pages")
        .ok_or_else(|| Error::structure("catalog has no /Pages"))?;

    let mut pages = Vec::new();
    let mut visited = HashSet::new();
    walk_page_tree(
        resolver,
        diagnostics,
        pages_ref,
        &mut pages,
        &mut visited,
        0,
    )?;
    Ok(pages)
}

/// Push a leaf page, enforcing the MAX_PAGES limit. Returns `false` if the
/// limit was reached (caller should stop traversal).
fn push_page(
    diagnostics: &Arc<dyn DiagnosticsSink>,
    pages: &mut Vec<ObjRef>,
    node_ref: ObjRef,
) -> bool {
    if pages.len() >= MAX_PAGES {
        diagnostics.warning(Warning::new(
            None,
            WarningKind::ResourceLimit,
            format!("page count limit ({MAX_PAGES}) reached, ignoring remaining pages"),
        ));
        return false;
    }
    pages.push(node_ref);
    true
}

/// Recursively walk the page tree, collecting leaf page ObjRefs.
fn walk_page_tree(
    resolver: &mut ObjectResolver<'_>,
    diagnostics: &Arc<dyn DiagnosticsSink>,
    node_ref: ObjRef,
    pages: &mut Vec<ObjRef>,
    visited: &mut HashSet<ObjRef>,
    depth: usize,
) -> Result<()> {
    if depth > MAX_PAGE_TREE_DEPTH {
        return Err(Error::structure(format!(
            "page tree depth limit ({MAX_PAGE_TREE_DEPTH}) exceeded"
        )));
    }

    if !visited.insert(node_ref) {
        diagnostics.warning(Warning::new(
            None,
            WarningKind::PageTreeCycle,
            format!("cycle detected in page tree at {node_ref}, skipping subtree"),
        ));
        return Ok(());
    }

    let dict = resolver
        .resolve_dict(node_ref)
        .context(format!("resolving page tree node {node_ref}"))?;

    let type_name = dict.get_name(b"Type");

    match type_name {
        Some(b"Pages") => {
            if let Some(kids) = dict.get_array(b"Kids") {
                walk_kids(resolver, diagnostics, kids, pages, visited, depth)?;
            } else if dict.get(b"MediaBox").is_some() || dict.get(b"Contents").is_some() {
                // /Pages node with page-like properties but no /Kids: treat as
                // a mistyped leaf page (some generators emit /Type /Pages on
                // what is really a single page).
                diagnostics.warning(Warning::new(
                    None,
                    WarningKind::InvalidPageTree,
                    format!(
                        "/Pages node {node_ref} has no /Kids but has page properties, treating as leaf page"
                    ),
                ));
                if !push_page(diagnostics, pages, node_ref) {
                    return Ok(());
                }
            } else {
                // /Pages node with no /Kids and no page properties: skip it.
                diagnostics.warning(Warning::new(
                    None,
                    WarningKind::InvalidPageTree,
                    format!("/Pages node {node_ref} has no /Kids and no page properties, skipping"),
                ));
            }
        }
        Some(b"Page") => {
            if !push_page(diagnostics, pages, node_ref) {
                return Ok(());
            }
        }
        None => {
            // Some generators omit /Type. Heuristic: if /Kids exists, it's a Pages node.
            if let Some(kids) = dict.get_array(b"Kids") {
                walk_kids(resolver, diagnostics, kids, pages, visited, depth)?;
            } else {
                // Treat as leaf page.
                if !push_page(diagnostics, pages, node_ref) {
                    return Ok(());
                }
            }
        }
        Some(_) => {
            // Unknown type, skip.
        }
    }

    Ok(())
}

/// Detect /Encrypt in the trailer and set up decryption on the resolver.
///
/// If the trailer has no /Encrypt, this is a no-op and returns `false`.
/// Otherwise, resolves the /Encrypt dictionary, extracts the file ID from
/// /ID, validates the password, attaches a CryptHandler to the resolver,
/// and returns `true`. The boolean is the "this document declared
/// encryption" signal that gets surfaced via `Document::is_encrypted` --
/// it is set even when decryption succeeded (the user supplied a correct
/// password).
fn setup_encryption(
    resolver: &mut ObjectResolver<'_>,
    password: Option<&[u8]>,
    diagnostics: &Arc<dyn DiagnosticsSink>,
) -> Result<bool> {
    // Clone the trailer up front so we don't hold a borrow on `resolver`
    // while calling mutable methods like `resolve()` and `set_crypt_handler()` below.
    let trailer = match resolver.trailer() {
        Some(t) => t.clone(),
        None => return Ok(false),
    };

    // Check if /Encrypt exists in trailer
    let encrypt_entry = match trailer.get(b"Encrypt") {
        Some(entry) => entry.clone(),
        None => {
            if password.is_some() {
                diagnostics.warning(Warning::new(
                    None,
                    WarningKind::EncryptedDocument,
                    "password provided but document is not encrypted",
                ));
            }
            return Ok(false);
        }
    };

    // Track the obj_ref of the /Encrypt dict (for the exception list)
    let encrypt_obj_ref = encrypt_entry.as_reference();

    // Resolve the /Encrypt dictionary (may be an indirect reference)
    let encrypt_dict = match encrypt_entry {
        PdfObject::Reference(r) => resolver
            .resolve_dict(r)
            .context("resolving /Encrypt dictionary")?,
        PdfObject::Dictionary(d) => d,
        _ => {
            return Err(Error::structure(
                "/Encrypt entry is not a dictionary or reference",
            ));
        }
    };

    // Extract the first element of the /ID array (file identifier for key derivation).
    // /ID is required for encrypted documents (PDF spec 7.5.5).
    let file_id = trailer
        .get_array(b"ID")
        .and_then(|arr| arr.first())
        .and_then(|obj| obj.as_pdf_string())
        .map(|s| s.as_bytes().to_vec())
        .ok_or_else(|| {
            Error::encryption(EncryptionErrorKind::MissingField(
                "ID (file identifier required for encrypted documents)".into(),
            ))
        })?;

    let handler = CryptHandler::from_encrypt_dict(
        &encrypt_dict,
        &file_id,
        password,
        encrypt_obj_ref,
        diagnostics,
    )
    .context("setting up encryption")?;

    resolver.set_crypt_handler(Arc::new(handler));
    // /Encrypt was present and decryption was set up successfully; tell
    // the caller so they can flag the resulting Document as encrypted.
    Ok(true)
}

/// Parse the structure tree from the document catalog.
///
/// Returns None if the document has no /StructTreeRoot, if the catalog
/// cannot be resolved, or if parsing fails. Failures are silently ignored
/// because a missing/broken structure tree just means we fall back to
/// geometric reading order.
fn parse_structure_tree_from_catalog(resolver: &mut ObjectResolver<'_>) -> Option<StructureTree> {
    let trailer = resolver.trailer()?.clone();
    let root_ref = trailer.get_ref(b"Root")?;
    let catalog = resolver.resolve_dict(root_ref).ok()?;
    marked_content::parse_structure_tree(resolver, &catalog)
}

/// Extract document metadata from the PDF /Info dictionary.
///
/// The trailer may contain an /Info key pointing to a dictionary with
/// standard keys like /Title, /Author, /Subject, /Creator, /Producer,
/// /CreationDate, and /ModDate. Missing or unresolvable entries are
/// silently skipped (returns page-count-only metadata on any failure).
fn extract_info_metadata(resolver: &mut ObjectResolver<'_>, page_count: usize) -> DocumentMetadata {
    let mut meta = DocumentMetadata::with_page_count(page_count);

    let info_dict = match resolve_info_dict(resolver) {
        Some(d) => d,
        None => return meta,
    };

    meta.title = extract_string_value(&info_dict, b"Title").filter(|s| !s.is_empty());
    meta.author = extract_string_value(&info_dict, b"Author").filter(|s| !s.is_empty());
    meta.subject = extract_string_value(&info_dict, b"Subject").filter(|s| !s.is_empty());
    meta.creator = extract_string_value(&info_dict, b"Creator").filter(|s| !s.is_empty());
    meta.producer = extract_string_value(&info_dict, b"Producer").filter(|s| !s.is_empty());
    meta.creation_date =
        extract_string_value(&info_dict, b"CreationDate").filter(|s| !s.is_empty());
    meta.modification_date = extract_string_value(&info_dict, b"ModDate").filter(|s| !s.is_empty());

    meta
}

/// Resolve the /Info dictionary from the trailer. Returns None if the
/// trailer has no /Info entry or if the referenced object is not a dictionary.
fn resolve_info_dict(resolver: &mut ObjectResolver<'_>) -> Option<PdfDictionary> {
    let trailer = resolver.trailer()?.clone();

    // /Info can be either an indirect reference or an inline dictionary.
    match trailer.get(b"Info")? {
        PdfObject::Reference(r) => resolver.resolve_dict(*r).ok(),
        PdfObject::Dictionary(d) => Some(d.clone()),
        _ => None,
    }
}

/// Iterate /Kids entries, warning on non-reference values.
fn walk_kids(
    resolver: &mut ObjectResolver<'_>,
    diagnostics: &Arc<dyn DiagnosticsSink>,
    kids: &[PdfObject],
    pages: &mut Vec<ObjRef>,
    visited: &mut HashSet<ObjRef>,
    depth: usize,
) -> Result<()> {
    for (i, kid) in kids.iter().enumerate() {
        match *kid {
            PdfObject::Reference(r) => {
                if let Err(e) = walk_page_tree(resolver, diagnostics, r, pages, visited, depth + 1)
                {
                    diagnostics.warning(Warning::new(
                        None,
                        WarningKind::InvalidPageTree,
                        format!("/Kids entry {i} ({r}) could not be resolved: {e}, skipping"),
                    ));
                }
            }
            _ => {
                diagnostics.warning(Warning::new(
                    None,
                    WarningKind::InvalidPageTree,
                    format!(
                        "/Kids entry {i} is {} instead of a reference, skipping",
                        kid.type_name()
                    ),
                ));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Bridge adapter: udoc_core DiagnosticsSink -> udoc_pdf DiagnosticsSink
//
// Bridges the PDF Warning type to the core Warning type. The PDF
// WarningKind variants now live as typed variants on
// udoc_core::diagnostics::WarningKind, so the bridge
// maps enum -> enum directly without the previous lossy
// `format!("{:?}", kind)` string round-trip. Callers at the facade can
// pattern-match on udoc_core::diagnostics::WarningKind::StreamLengthMismatch
// instead of grepping a Debug string.

pub(crate) struct CoreDiagBridge(
    pub(crate) std::sync::Arc<dyn udoc_core::diagnostics::DiagnosticsSink>,
);

fn pdf_kind_to_core(kind: WarningKind) -> udoc_core::diagnostics::WarningKind {
    use udoc_core::diagnostics::WarningKind as CoreKind;
    match kind {
        WarningKind::MalformedXref => CoreKind::MalformedXref,
        WarningKind::MalformedToken => CoreKind::MalformedToken,
        WarningKind::MalformedString => CoreKind::MalformedString,
        WarningKind::GarbageBytes => CoreKind::GarbageBytes,
        WarningKind::UnknownKeyword => CoreKind::UnknownKeyword,
        WarningKind::UnexpectedToken => CoreKind::UnexpectedToken,
        WarningKind::UnterminatedCollection => CoreKind::UnterminatedCollection,
        WarningKind::StreamLengthMismatch => CoreKind::StreamLengthMismatch,
        WarningKind::StreamExtendsPastEof => CoreKind::StreamExtendsPastEof,
        WarningKind::UnsupportedFilter => CoreKind::UnsupportedFilter,
        WarningKind::DecodeError => CoreKind::DecodeError,
        WarningKind::ObjectHeaderMismatch => CoreKind::ObjectHeaderMismatch,
        WarningKind::MissingEndObj => CoreKind::MissingEndObj,
        WarningKind::PageTreeCycle => CoreKind::PageTreeCycle,
        WarningKind::InvalidPageTree => CoreKind::InvalidPageTree,
        WarningKind::FontError => CoreKind::FontError,
        WarningKind::FontLoaded => CoreKind::FontLoaded,
        WarningKind::FontMetricsDisagreement => CoreKind::FontMetricsDisagreement,
        WarningKind::FallbackFontSubstitution => CoreKind::FallbackFontSubstitution,
        WarningKind::InvalidState => CoreKind::InvalidState,
        WarningKind::InvalidImageMetadata => CoreKind::InvalidImageMetadata,
        WarningKind::UnsupportedFeature => CoreKind::UnsupportedFeature,
        WarningKind::UnsupportedShadingType => CoreKind::UnsupportedShadingType,
        WarningKind::UnsupportedPatternType => CoreKind::UnsupportedPatternType,
        WarningKind::ResourceLimit => CoreKind::ResourceLimit,
        WarningKind::TierSelection => CoreKind::TierSelection,
        WarningKind::ReadingOrder => CoreKind::ReadingOrder,
        WarningKind::EncryptedDocument => CoreKind::EncryptedDocument,
    }
}

impl DiagnosticsSink for CoreDiagBridge {
    fn warning(&self, warn: Warning) {
        use udoc_core::diagnostics as core_diag;
        let core_level = match warn.level {
            crate::diagnostics::WarningLevel::Info => core_diag::WarningLevel::Info,
            crate::diagnostics::WarningLevel::Warning => core_diag::WarningLevel::Warning,
        };
        let mut ctx = core_diag::WarningContext::default();
        ctx.page_index = warn.context.page_index;
        let core_warn = core_diag::Warning::new(pdf_kind_to_core(warn.kind), warn.message)
            .with_level(core_level)
            .with_context(ctx);
        let core_warn = if let Some(off) = warn.offset {
            core_warn.at_offset(off)
        } else {
            core_warn
        };
        self.0.warning(core_warn);
    }
}

// ---------------------------------------------------------------------------
// FormatBackend implementation
// ---------------------------------------------------------------------------

use udoc_core::backend::{FormatBackend, PageExtractor};

impl FormatBackend for Document {
    type Page<'a> = PdfPageExtractor<'a>;

    fn page_count(&self) -> usize {
        self.page_refs.len()
    }

    fn page(&mut self, index: usize) -> udoc_core::error::Result<PdfPageExtractor<'_>> {
        let page = Document::page(self, index).map_err(crate::convert::convert_error)?;
        Ok(PdfPageExtractor { inner: page })
    }

    fn metadata(&self) -> DocumentMetadata {
        self.metadata.clone()
    }

    fn is_encrypted(&self) -> bool {
        self.is_encrypted
    }
}

/// Adapter wrapping PDF's Page<'a> to implement the core PageExtractor trait.
pub struct PdfPageExtractor<'a> {
    inner: Page<'a>,
}

impl<'a> PdfPageExtractor<'a> {
    /// Page bounding box from /CropBox or /MediaBox.
    ///
    /// Returns a core BoundingBox (converted from PDF's internal type).
    pub fn page_bbox(&mut self) -> udoc_core::geometry::BoundingBox {
        crate::convert::convert_bbox(&self.inner.page_bbox())
    }

    /// Page rotation in degrees (0, 90, 180, 270).
    pub fn rotation(&mut self) -> u16 {
        self.inner.rotation()
    }

    /// Extract flattened path segments (lines, rectangles, polygons) from
    /// the page.
    ///
    /// For the canonical renderer-facing IR (cubic curves + CTM snapshot +
    /// explicit fill rule + stroke style), use [`Self::paths`].
    pub fn path_segments(&mut self) -> udoc_core::error::Result<Vec<PathSegment>> {
        self.inner
            .path_segments()
            .map_err(crate::convert::convert_error)
    }

    /// Extract canonical page paths for renderer consumption.
    pub fn paths(&mut self) -> udoc_core::error::Result<Vec<crate::content::path::PagePath>> {
        self.inner.paths().map_err(crate::convert::convert_error)
    }

    /// Extract paths and shadings from a single interpreter pass
    ///. Prefer this when both are needed by the renderer.
    pub fn paths_and_shadings(
        &mut self,
    ) -> udoc_core::error::Result<(
        Vec<crate::content::path::PagePath>,
        Vec<crate::content::path::PageShading>,
    )> {
        self.inner
            .paths_and_shadings()
            .map_err(crate::convert::convert_error)
    }

    /// Extract paths, shadings, and Type 1 coloured tiling patterns in
    /// a single interpreter pass. Prefer this
    /// when the renderer needs all three streams.
    pub fn paths_shadings_and_patterns(
        &mut self,
    ) -> udoc_core::error::Result<(
        Vec<crate::content::path::PagePath>,
        Vec<crate::content::path::PageShading>,
        Vec<crate::content::path::PageTilingPattern>,
    )> {
        self.inner
            .paths_shadings_and_patterns()
            .map_err(crate::convert::convert_error)
    }

    /// Extract all content in a single content stream interpretation pass.
    ///
    /// Returns text lines, raw spans, tables, and images, all converted to
    /// core types. This is 4x more efficient than calling `text_lines()`,
    /// `tables()`, `images()`, and `raw_spans()` separately.
    pub fn extract_full(&mut self) -> udoc_core::error::Result<ExtractedPage> {
        let full = self
            .inner
            .extract_full()
            .map_err(crate::convert::convert_error)?;
        // consume by value so the per-page text_lines / raw_spans /
        // tables / images vecs MOVE their owned heap fields (text, char_*,
        // glyph_bboxes, image data, soft_mask, etc.) into the core types
        // instead of cloning. We're the last user of `full` here.
        Ok(ExtractedPage {
            text_lines: crate::convert::convert_text_lines_owned(full.text_lines),
            raw_spans: crate::convert::convert_text_spans_owned(full.raw_spans),
            tables: crate::convert::convert_tables_owned(full.tables),
            images: full
                .images
                .into_iter()
                .map(|ai| AnnotatedImage {
                    image: crate::convert::convert_page_image_owned(ai.image),
                    alt_text: ai.alt_text,
                })
                .collect(),
        })
    }
}

/// All page content extracted in a single pass, converted to core types.
///
/// Produced by [`PdfPageExtractor::extract_full()`].
#[derive(Debug)]
pub struct ExtractedPage {
    /// Text lines in reading order.
    pub text_lines: Vec<udoc_core::text::TextLine>,
    /// Raw text spans in content stream order.
    pub raw_spans: Vec<udoc_core::text::TextSpan>,
    /// Detected tables.
    pub tables: Vec<udoc_core::table::Table>,
    /// Extracted images with optional alt text from the structure tree.
    pub images: Vec<AnnotatedImage<udoc_core::image::PageImage>>,
}

impl<'a> PdfPageExtractor<'a> {
    /// Extract hyperlinks from /Link annotations.
    pub fn links(&mut self) -> Vec<PageLink> {
        self.inner.links()
    }

    /// Enumerate renderable annotations on this page (
    /// #170). Proxies through to [`Page::annotations`].
    pub fn annotations(&mut self) -> Vec<PageAnnotation> {
        self.inner.annotations()
    }

    /// Interpret an annotation's /AP/N appearance stream and return the
    /// composited page paths + core-typed text spans. Proxies through to
    /// [`Page::interpret_annotation_appearance`] and then converts the
    /// PDF-internal text spans to format-agnostic core spans so callers
    /// at the facade can treat them like any other positioned span.
    pub fn interpret_annotation_appearance(
        &mut self,
        annotation: &PageAnnotation,
    ) -> (
        Vec<crate::content::path::PagePath>,
        Vec<udoc_core::text::TextSpan>,
    ) {
        let (paths, pdf_spans) = self.inner.interpret_annotation_appearance(annotation);
        // caller owns pdf_spans (interp-internal); consume.
        let core_spans = crate::convert::convert_text_spans_owned(pdf_spans);
        (paths, core_spans)
    }
}

impl<'a> PageExtractor for PdfPageExtractor<'a> {
    fn text(&mut self) -> udoc_core::error::Result<String> {
        self.inner.text().map_err(crate::convert::convert_error)
    }

    fn text_lines(&mut self) -> udoc_core::error::Result<Vec<udoc_core::text::TextLine>> {
        let lines = self
            .inner
            .text_lines()
            .map_err(crate::convert::convert_error)?;
        // lines is owned and dropped after; move into core types.
        Ok(crate::convert::convert_text_lines_owned(lines))
    }

    fn raw_spans(&mut self) -> udoc_core::error::Result<Vec<udoc_core::text::TextSpan>> {
        let spans = self
            .inner
            .raw_spans()
            .map_err(crate::convert::convert_error)?;
        // same.
        Ok(crate::convert::convert_text_spans_owned(spans))
    }

    fn tables(&mut self) -> udoc_core::error::Result<Vec<udoc_core::table::Table>> {
        let tables = self.inner.tables().map_err(crate::convert::convert_error)?;
        // same.
        Ok(crate::convert::convert_tables_owned(tables))
    }

    fn images(&mut self) -> udoc_core::error::Result<Vec<udoc_core::image::PageImage>> {
        let images = self.inner.images().map_err(crate::convert::convert_error)?;
        // same; image data buffers are large.
        Ok(crate::convert::convert_page_images_owned(images))
    }

    fn page_bbox(&mut self) -> Option<udoc_core::geometry::BoundingBox> {
        Some(PdfPageExtractor::page_bbox(self))
    }

    fn rotation(&mut self) -> u16 {
        PdfPageExtractor::rotation(self)
    }

    /// Override the trait default to walk the content stream once
    /// instead of three times. PDF text + tables + images all share
    /// the content-stream interpretation pass; a single
    /// [`PdfPageExtractor::extract_full`] call yields all three.
    /// This is ~4x faster than the trait default's three separate
    /// trait calls.
    fn bundle(
        &mut self,
        layers: &udoc_core::backend::LayerConfig,
    ) -> udoc_core::error::Result<udoc_core::backend::PageBundle> {
        let extracted = PdfPageExtractor::extract_full(self)?;
        // Honor layer flags by clearing the disabled buckets. The
        // backend already did the work; this matches the trait
        // contract that disabled layers must come back empty.
        let lines = extracted.text_lines;
        let tables = if layers.tables {
            extracted.tables
        } else {
            Vec::new()
        };
        let images = if layers.images {
            extracted.images.into_iter().map(|ai| ai.image).collect()
        } else {
            Vec::new()
        };
        Ok(udoc_core::backend::PageBundle {
            lines,
            tables,
            images,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::CollectingDiagnostics;
    use std::path::Path;

    /// Minimal PDF builder for unit tests. Keeps track of object offsets and
    /// produces a valid xref + trailer so Document::from_bytes can parse it.
    struct TestPdfBuilder {
        buf: Vec<u8>,
        objects: Vec<(u32, u64)>,
    }

    impl TestPdfBuilder {
        fn new() -> Self {
            let mut buf = Vec::new();
            buf.extend_from_slice(b"%PDF-1.4\n");
            TestPdfBuilder {
                buf,
                objects: Vec::new(),
            }
        }

        fn add_object(&mut self, obj_num: u32, body: &[u8]) {
            let offset = self.buf.len() as u64;
            self.objects.push((obj_num, offset));
            self.buf
                .extend_from_slice(format!("{} 0 obj\n", obj_num).as_bytes());
            self.buf.extend_from_slice(body);
            self.buf.extend_from_slice(b"\nendobj\n");
        }

        fn add_stream_object(&mut self, obj_num: u32, dict_extra: &str, data: &[u8]) {
            let offset = self.buf.len() as u64;
            self.objects.push((obj_num, offset));
            self.buf.extend_from_slice(
                format!(
                    "{} 0 obj\n<< /Length {} {} >>\nstream\n",
                    obj_num,
                    data.len(),
                    dict_extra
                )
                .as_bytes(),
            );
            self.buf.extend_from_slice(data);
            self.buf.extend_from_slice(b"\nendstream\nendobj\n");
        }

        fn finish(self, root_obj: u32) -> Vec<u8> {
            self.finish_with_trailer(root_obj, "")
        }

        fn finish_with_info(self, root_obj: u32, info_obj: u32) -> Vec<u8> {
            self.finish_with_trailer(root_obj, &format!(" /Info {} 0 R", info_obj))
        }

        fn finish_with_trailer(mut self, root_obj: u32, extra_trailer: &str) -> Vec<u8> {
            let xref_offset = self.buf.len();
            let size = self.objects.iter().map(|(n, _)| *n).max().unwrap_or(0) + 1;

            self.buf
                .extend_from_slice(format!("xref\n0 {}\n", size).as_bytes());
            let mut offsets = vec![None; size as usize];
            for &(num, off) in &self.objects {
                offsets[num as usize] = Some(off);
            }
            self.buf.extend_from_slice(b"0000000000 65535 f \r\n");
            for entry in offsets.iter().skip(1) {
                if let Some(off) = entry {
                    self.buf
                        .extend_from_slice(format!("{:010} 00000 n \r\n", off).as_bytes());
                } else {
                    self.buf.extend_from_slice(b"0000000000 00000 f \r\n");
                }
            }
            self.buf.extend_from_slice(
                format!(
                    "trailer\n<< /Size {} /Root {} 0 R{} >>\nstartxref\n{}\n%%EOF\n",
                    size, root_obj, extra_trailer, xref_offset
                )
                .as_bytes(),
            );
            self.buf
        }
    }

    /// /Pages node with /MediaBox and /Contents but no /Kids should be treated
    /// as a mistyped leaf page.
    #[test]
    fn test_missing_kids_with_page_properties_treated_as_leaf() {
        let mut b = TestPdfBuilder::new();
        b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
        // Object 2 is /Type /Pages but has /MediaBox and /Contents (no /Kids).
        b.add_object(
            2,
            b"<< /Type /Pages /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);

        let diag = Arc::new(CollectingDiagnostics::new());
        let config = Config::default().with_diagnostics(diag.clone());
        let mut doc = Document::from_bytes_with_config(pdf, config)
            .expect("document should open despite missing /Kids");
        assert_eq!(
            doc.page_count(),
            1,
            "mistyped /Pages node should count as one page"
        );

        // Should be able to extract text from it.
        let mut page = doc.page(0).expect("page 0 should be accessible");
        let text = page.text().expect("text extraction should succeed");
        assert!(
            text.contains("Hello"),
            "expected 'Hello' in extracted text, got: {text}"
        );

        // Verify warning was emitted.
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.kind == WarningKind::InvalidPageTree
                    && w.message.contains("treating as leaf page")),
            "expected InvalidPageTree warning about leaf page, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    /// /Pages node with no /Kids and no page properties should be skipped.
    #[test]
    fn test_missing_kids_no_page_properties_skipped() {
        let mut b = TestPdfBuilder::new();
        // /Pages node with nothing useful.
        b.add_object(2, b"<< /Type /Pages >>");
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);

        let diag = Arc::new(CollectingDiagnostics::new());
        let config = Config::default().with_diagnostics(diag.clone());
        let doc = Document::from_bytes_with_config(pdf, config)
            .expect("document should open despite empty /Pages node");
        assert_eq!(
            doc.page_count(),
            0,
            "empty /Pages node should yield zero pages"
        );

        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.kind == WarningKind::InvalidPageTree && w.message.contains("skipping")),
            "expected InvalidPageTree warning about skipping, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    /// Deep page tree where one intermediate /Pages node is missing /Kids.
    /// Remaining pages from other branches should still be extracted.
    #[test]
    fn test_missing_kids_in_intermediate_node_other_pages_survive() {
        let mut b = TestPdfBuilder::new();
        b.add_object(6, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (PageOne) Tj ET");
        // A valid leaf page (obj 4).
        b.add_object(
            4,
            b"<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources << /Font << /F1 6 0 R >> >> >>",
        );
        // Intermediate /Pages node with valid /Kids (obj 3).
        b.add_object(3, b"<< /Type /Pages /Kids [4 0 R] /Count 1 >>");
        // Broken intermediate /Pages node with no /Kids (obj 7).
        b.add_object(7, b"<< /Type /Pages >>");
        // Root /Pages node referencing both branches.
        b.add_object(2, b"<< /Type /Pages /Kids [3 0 R 7 0 R] /Count 1 >>");
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);

        let diag = Arc::new(CollectingDiagnostics::new());
        let config = Config::default().with_diagnostics(diag.clone());
        let mut doc = Document::from_bytes_with_config(pdf, config)
            .expect("document should open despite broken branch");
        assert_eq!(
            doc.page_count(),
            1,
            "should have 1 page from the valid branch"
        );

        let mut page = doc.page(0).expect("page 0 should be accessible");
        let text = page.text().expect("text extraction should succeed");
        assert!(
            text.contains("PageOne"),
            "expected 'PageOne' in extracted text, got: {text}"
        );

        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.kind == WarningKind::InvalidPageTree),
            "expected InvalidPageTree warning for broken branch, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    /// /Kids entry referencing a non-existent object should be skipped with a
    /// warning, not fail the entire page tree walk.
    #[test]
    fn test_unresolvable_kids_entry_skipped() {
        let mut b = TestPdfBuilder::new();
        b.add_object(5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(4, "", b"BT /F1 12 Tf 100 700 Td (Good) Tj ET");
        // Valid page (obj 3).
        b.add_object(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
        );
        // /Kids references obj 3 (valid) and obj 99 (does not exist).
        b.add_object(2, b"<< /Type /Pages /Kids [3 0 R 99 0 R] /Count 2 >>");
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);

        let diag = Arc::new(CollectingDiagnostics::new());
        let config = Config::default().with_diagnostics(diag.clone());
        let mut doc = Document::from_bytes_with_config(pdf, config)
            .expect("document should open despite unresolvable /Kids entry");
        assert_eq!(
            doc.page_count(),
            1,
            "should have 1 page from the valid entry"
        );

        let mut page = doc.page(0).expect("page 0 should be accessible");
        let text = page.text().expect("text extraction should succeed");
        assert!(
            text.contains("Good"),
            "expected 'Good' in extracted text, got: {text}"
        );

        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.kind == WarningKind::InvalidPageTree
                    && w.message.contains("could not be resolved")),
            "expected InvalidPageTree warning about unresolvable entry, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    /// Validate Tier 1 spacing on real-world (non-TeX) PDFs.
    ///
    /// Checks which files produce spans with space_width set (Tier 1 path)
    /// vs falling through to Tier 2 font-size-relative heuristics.
    ///  retro action item (BL-009).
    #[test]
    fn tier1_spacing_validation_realworld() {
        let corpus_dir = Path::new("tests/corpus/realworld");
        if !corpus_dir.exists() {
            eprintln!("realworld corpus not found, skipping tier1 validation");
            return;
        }

        let mut results: Vec<(String, usize, usize, usize)> = Vec::new();

        for entry in std::fs::read_dir(corpus_dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "pdf") {
                continue;
            }

            let file_name = path.file_name().unwrap().to_string_lossy().to_string();
            let mut doc = match Document::open(&path) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("{file_name}: failed to open: {e}");
                    continue;
                }
            };

            let mut total_spans = 0usize;
            let mut tier1_spans = 0usize; // space_width is Some
            let page_count = doc.page_count();

            for i in 0..page_count {
                let mut page = match doc.page(i) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                let spans = match page.raw_spans() {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                for span in &spans {
                    total_spans += 1;
                    if span.space_width.is_some() {
                        tier1_spans += 1;
                    }
                }
            }

            results.push((file_name, total_spans, tier1_spans, page_count));
        }

        results.sort_by(|a, b| a.0.cmp(&b.0));

        eprintln!("\n=== Tier 1 Spacing Validation (realworld corpus) ===");
        eprintln!(
            "{:<30} {:>6} {:>8} {:>8} {:>7}",
            "File", "Pages", "Spans", "Tier1", "Pct"
        );
        eprintln!("{}", "-".repeat(65));

        let mut any_tier1 = false;
        for (name, total, tier1, pages) in &results {
            let pct = if *total > 0 {
                (*tier1 as f64 / *total as f64) * 100.0
            } else {
                0.0
            };
            if *tier1 > 0 {
                any_tier1 = true;
            }
            eprintln!(
                "{:<30} {:>6} {:>8} {:>8} {:>6.1}%",
                name, pages, total, tier1, pct
            );
        }
        eprintln!("{}", "-".repeat(65));

        let total_all: usize = results.iter().map(|r| r.1).sum();
        let tier1_all: usize = results.iter().map(|r| r.2).sum();
        let pct_all = if total_all > 0 {
            (tier1_all as f64 / total_all as f64) * 100.0
        } else {
            0.0
        };
        eprintln!(
            "{:<30} {:>6} {:>8} {:>8} {:>6.1}%",
            "TOTAL", "", total_all, tier1_all, pct_all
        );

        // Document the result: we expect at least some files to exercise Tier 1
        // on non-TeX PDFs (standard fonts with space glyph widths).
        if !any_tier1 {
            eprintln!("\nWARNING: No Tier 1 spans found in any realworld file.");
            eprintln!("Tier 1 spacing (font space width) is not firing on this corpus.");
        }
    }

    // -----------------------------------------------------------------------
    // Annotation text extraction tests
    // -----------------------------------------------------------------------

    /// Page with no /Annots: early return, no crash.
    #[test]
    fn test_annot_no_annots() {
        let mut b = TestPdfBuilder::new();
        b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("should open");
        assert_eq!(doc.page_count(), 1);
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text extraction");
        assert!(
            text.contains("Hello"),
            "page text should contain 'Hello', got: {text}"
        );
        // No annotation text; just verify no crash.
        let spans = doc.page(0).expect("page 0").raw_spans().expect("spans");
        assert!(
            spans.iter().all(|s| !s.is_annotation),
            "no spans should be annotation-sourced"
        );
    }

    /// FreeText annotation with /AP /N appearance stream containing text.
    #[test]
    fn test_annot_freetext_appearance_stream() {
        let mut b = TestPdfBuilder::new();
        // Font
        b.add_object(
            10,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        );
        // Page content stream (main text)
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (MainText) Tj ET");
        // Appearance stream for FreeText annotation
        b.add_stream_object(
            6,
            "/Subtype /Form /Resources << /Font << /F1 10 0 R >> >>",
            b"BT /F1 10 Tf 0 0 Td (AnnotText) Tj ET",
        );
        // FreeText annotation dict
        b.add_object(
            5,
            b"<< /Type /Annot /Subtype /FreeText /Rect [200 500 400 520] /AP << /N 6 0 R >> >>",
        );
        // Page with /Annots
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 10 0 R >> >> /Annots [5 0 R] >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("should open");
        let mut page = doc.page(0).expect("page 0");
        let spans = page.raw_spans().expect("spans");

        let main_spans: Vec<_> = spans.iter().filter(|s| !s.is_annotation).collect();
        let annot_spans: Vec<_> = spans.iter().filter(|s| s.is_annotation).collect();

        assert!(
            main_spans.iter().any(|s| s.text.contains("MainText")),
            "should have main text, got: {:?}",
            main_spans.iter().map(|s| &s.text).collect::<Vec<_>>()
        );
        assert!(
            annot_spans.iter().any(|s| s.text.contains("AnnotText")),
            "should have annotation text, got: {:?}",
            annot_spans.iter().map(|s| &s.text).collect::<Vec<_>>()
        );

        // Full text should include both
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text");
        assert!(text.contains("MainText"), "full text missing MainText");
        assert!(text.contains("AnnotText"), "full text missing AnnotText");
    }

    /// Widget annotation with /AP /N appearance stream.
    #[test]
    fn test_annot_widget_with_appearance() {
        let mut b = TestPdfBuilder::new();
        b.add_object(
            10,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        );
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (PageBody) Tj ET");
        // Widget appearance stream
        b.add_stream_object(
            6,
            "/Subtype /Form /Resources << /Font << /F1 10 0 R >> >>",
            b"BT /F1 10 Tf 2 2 Td (FieldValue) Tj ET",
        );
        // Widget annotation with /AP
        b.add_object(
            5,
            b"<< /Type /Annot /Subtype /Widget /Rect [100 400 300 420] /AP << /N 6 0 R >> /V (FieldValue) >>",
        );
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 10 0 R >> >> /Annots [5 0 R] >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("should open");
        let mut page = doc.page(0).expect("page 0");
        let spans = page.raw_spans().expect("spans");
        let annot_spans: Vec<_> = spans.iter().filter(|s| s.is_annotation).collect();
        assert!(
            annot_spans.iter().any(|s| s.text.contains("FieldValue")),
            "Widget annotation text should be extracted via /AP, got: {:?}",
            annot_spans.iter().map(|s| &s.text).collect::<Vec<_>>()
        );
    }

    /// Widget annotation with /V but no /AP: fallback to /V string value.
    #[test]
    fn test_annot_widget_v_only() {
        let mut b = TestPdfBuilder::new();
        b.add_object(
            10,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        );
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (PageBody) Tj ET");
        // Widget annotation with /V but no /AP
        b.add_object(
            5,
            b"<< /Type /Annot /Subtype /Widget /Rect [100 400 300 420] /V (FormInput) >>",
        );
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 10 0 R >> >> /Annots [5 0 R] >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("should open");
        let mut page = doc.page(0).expect("page 0");
        let spans = page.raw_spans().expect("spans");
        let annot_spans: Vec<_> = spans.iter().filter(|s| s.is_annotation).collect();
        assert!(
            annot_spans.iter().any(|s| s.text.contains("FormInput")),
            "Widget /V fallback should produce annotation span, got: {:?}",
            annot_spans.iter().map(|s| &s.text).collect::<Vec<_>>()
        );
    }

    /// Stamp annotation with appearance stream.
    #[test]
    fn test_annot_stamp() {
        let mut b = TestPdfBuilder::new();
        b.add_object(
            10,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        );
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Body) Tj ET");
        // Stamp appearance stream
        b.add_stream_object(
            6,
            "/Subtype /Form /Resources << /Font << /F1 10 0 R >> >>",
            b"BT /F1 14 Tf 10 10 Td (APPROVED) Tj ET",
        );
        // Stamp annotation
        b.add_object(
            5,
            b"<< /Type /Annot /Subtype /Stamp /Rect [200 600 400 650] /AP << /N 6 0 R >> >>",
        );
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 10 0 R >> >> /Annots [5 0 R] >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("should open");
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text");
        assert!(
            text.contains("APPROVED"),
            "Stamp text should be extracted, got: {text}"
        );
    }

    /// Verify is_annotation is true for annotation spans and false for regular spans.
    #[test]
    fn test_annot_is_annotation_flag() {
        let mut b = TestPdfBuilder::new();
        b.add_object(
            10,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        );
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Regular) Tj ET");
        b.add_stream_object(
            6,
            "/Subtype /Form /Resources << /Font << /F1 10 0 R >> >>",
            b"BT /F1 10 Tf 0 0 Td (FromAnnot) Tj ET",
        );
        b.add_object(
            5,
            b"<< /Type /Annot /Subtype /FreeText /Rect [200 500 400 520] /AP << /N 6 0 R >> >>",
        );
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 10 0 R >> >> /Annots [5 0 R] >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("should open");
        let mut page = doc.page(0).expect("page 0");
        let spans = page.raw_spans().expect("spans");

        for span in &spans {
            if span.text.contains("Regular") {
                assert!(
                    !span.is_annotation,
                    "regular content span should have is_annotation=false"
                );
            }
            if span.text.contains("FromAnnot") {
                assert!(
                    span.is_annotation,
                    "annotation span should have is_annotation=true"
                );
            }
        }

        // Verify we got both types
        assert!(
            spans.iter().any(|s| s.text.contains("Regular")),
            "should have regular span"
        );
        assert!(
            spans.iter().any(|s| s.text.contains("FromAnnot")),
            "should have annotation span"
        );
    }

    /// Decorative annotation subtypes (Link, Highlight, etc.) should be skipped.
    #[test]
    fn test_annot_decorative_skipped() {
        let mut b = TestPdfBuilder::new();
        b.add_object(
            10,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        );
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Body) Tj ET");
        // Link annotation (should be skipped)
        b.add_object(
            5,
            b"<< /Type /Annot /Subtype /Link /Rect [100 600 300 620] /Contents (ShouldNotAppear) >>",
        );
        // Highlight annotation (should be skipped)
        b.add_object(
            7,
            b"<< /Type /Annot /Subtype /Highlight /Rect [100 580 300 600] /Contents (AlsoSkipped) >>",
        );
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 10 0 R >> >> /Annots [5 0 R 7 0 R] >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("should open");
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text");
        assert!(
            !text.contains("ShouldNotAppear"),
            "Link annotation text should not be extracted"
        );
        assert!(
            !text.contains("AlsoSkipped"),
            "Highlight annotation text should not be extracted"
        );
    }

    /// Text annotation (/Subtype /Text) extracts /Contents.
    #[test]
    fn test_annot_text_contents() {
        let mut b = TestPdfBuilder::new();
        b.add_object(
            10,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        );
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Body) Tj ET");
        // /Text annotation with /Contents string
        b.add_object(
            5,
            b"<< /Type /Annot /Subtype /Text /Rect [100 600 120 620] /Contents (NoteText) >>",
        );
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 10 0 R >> >> /Annots [5 0 R] >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("should open");
        let mut page = doc.page(0).expect("page 0");
        let spans = page.raw_spans().expect("spans");
        let annot_spans: Vec<_> = spans.iter().filter(|s| s.is_annotation).collect();
        assert!(
            annot_spans.iter().any(|s| s.text.contains("NoteText")),
            "/Text annotation /Contents should be extracted, got: {:?}",
            annot_spans.iter().map(|s| &s.text).collect::<Vec<_>>()
        );
    }

    /// text_lines() should include annotation spans in the output.
    #[test]
    fn test_annot_text_lines_includes_annotations() {
        let mut b = TestPdfBuilder::new();
        b.add_object(
            10,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        );
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Main) Tj ET");
        b.add_stream_object(
            6,
            "/Subtype /Form /Resources << /Font << /F1 10 0 R >> >>",
            b"BT /F1 10 Tf 100 500 Td (AnnotLine) Tj ET",
        );
        b.add_object(
            5,
            b"<< /Type /Annot /Subtype /FreeText /Rect [100 490 300 510] /AP << /N 6 0 R >> >>",
        );
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 10 0 R >> >> /Annots [5 0 R] >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("should open");
        let mut page = doc.page(0).expect("page 0");
        let lines = page.text_lines().expect("text_lines");
        let all_text: String = lines.iter().map(|l| l.text()).collect::<Vec<_>>().join(" ");
        assert!(
            all_text.contains("AnnotLine"),
            "text_lines should include annotation text, got: {all_text}"
        );
    }

    // -----------------------------------------------------------------------
    // Coverage gap tests (C-003)
    // -----------------------------------------------------------------------

    fn open_cov_pdf() -> Document {
        let mut b = TestPdfBuilder::new();
        b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);
        Document::from_bytes(pdf).expect("open minimal PDF")
    }

    #[test]
    fn cov_config_debug_redacts_password() {
        let config = Config::default().with_password(b"secret".to_vec());
        let dbg = format!("{config:?}");
        assert!(dbg.contains("[redacted]") && !dbg.contains("secret"));
    }

    #[test]
    fn cov_config_debug_no_password() {
        let dbg = format!("{:?}", Config::default());
        assert!(dbg.contains("None"));
    }

    #[test]
    fn cov_document_debug() {
        let doc = open_cov_pdf();
        let dbg = format!("{doc:?}");
        assert!(dbg.contains("Document") && dbg.contains("page_count"));
    }

    #[test]
    fn cov_page_debug() {
        let mut doc = open_cov_pdf();
        let page = doc.page(0).expect("page 0");
        let dbg = format!("{page:?}");
        assert!(dbg.contains("Page") && dbg.contains("index"));
    }

    #[test]
    fn cov_page_text_lines_and_text() {
        let mut doc = open_cov_pdf();
        let lines = doc.page(0).expect("p").text_lines().expect("tl");
        assert!(!lines.is_empty());
        let text = doc.page(0).expect("p").text().expect("t");
        assert!(text.contains("Hello"));
    }

    #[test]
    fn cov_page_paths() {
        let mut doc = open_cov_pdf();
        let paths = doc.page(0).expect("p").path_segments().expect("paths");
        assert!(paths.is_empty());
    }

    #[test]
    fn cov_page_tables() {
        let mut doc = open_cov_pdf();
        let tables = doc.page(0).expect("p").tables().expect("tables");
        assert!(tables.is_empty());
    }

    #[test]
    fn cov_page_bbox_public() {
        let mut doc = open_cov_pdf();
        let mut page = doc.page(0).expect("page 0");
        let bbox = page.page_bbox();
        // Coverage PDF has a MediaBox, should get real dimensions
        assert!(bbox.width() > 0.0);
        assert!(bbox.height() > 0.0);
    }

    #[test]
    fn cov_page_rotation_default() {
        let mut doc = open_cov_pdf();
        let mut page = doc.page(0).expect("page 0");
        // Coverage PDF has no /Rotate key, should default to 0
        assert_eq!(page.rotation(), 0);
    }

    #[test]
    fn cov_page_bbox_fallback() {
        let mut b = TestPdfBuilder::new();
        b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (NoBBox) Tj ET");
        b.add_object(
            2,
            b"<< /Type /Page /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);
        let diag = Arc::new(CollectingDiagnostics::new());
        let config = Config::default().with_diagnostics(diag.clone());
        let mut doc = Document::from_bytes_with_config(pdf, config).expect("open");
        let _tables = doc.page(0).expect("p").tables().expect("tables");
        assert!(diag
            .warnings()
            .iter()
            .any(|w| w.message.contains("US Letter fallback")));
    }

    #[test]
    fn cov_parse_rect_valid() {
        let mut dict = PdfDictionary::new();
        dict.insert(
            b"Rect".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Real(10.0),
                PdfObject::Real(20.0),
                PdfObject::Real(200.0),
                PdfObject::Real(300.0),
            ]),
        );
        assert_eq!(parse_rect(&dict), Some((10.0, 300.0)));
    }

    #[test]
    fn cov_parse_rect_short() {
        let mut dict = PdfDictionary::new();
        dict.insert(
            b"Rect".to_vec(),
            PdfObject::Array(vec![PdfObject::Real(10.0)]),
        );
        assert_eq!(parse_rect(&dict), None);
    }

    #[test]
    fn cov_parse_rect_missing() {
        assert_eq!(parse_rect(&PdfDictionary::new()), None);
    }

    #[test]
    fn cov_extract_string_value_present() {
        let mut dict = PdfDictionary::new();
        dict.insert(
            b"Contents".to_vec(),
            PdfObject::String(PdfString::new(b"hello".to_vec())),
        );
        assert_eq!(
            extract_string_value(&dict, b"Contents"),
            Some("hello".to_string())
        );
    }

    #[test]
    fn cov_extract_string_value_missing() {
        assert_eq!(extract_string_value(&PdfDictionary::new(), b"X"), None);
    }

    #[test]
    fn cov_decode_pdf_text_string_utf16be() {
        let s = PdfString::new(vec![0xFE, 0xFF, 0x00, 0x41, 0x00, 0x42]);
        assert_eq!(decode_pdf_text_string(&s), "AB");
    }

    #[test]
    fn cov_decode_pdf_text_string_ascii() {
        let s = PdfString::new(b"hello".to_vec());
        assert_eq!(decode_pdf_text_string(&s), "hello");
    }

    #[test]
    fn cov_annot_widget_v_indirect_ref() {
        let mut b = TestPdfBuilder::new();
        b.add_object(
            10,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        );
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Main) Tj ET");
        b.add_object(7, b"(FieldValue)");
        b.add_object(
            5,
            b"<< /Type /Annot /Subtype /Widget /Rect [50 490 300 510] /V 7 0 R >>",
        );
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 10 0 R >> >> /Annots [5 0 R] >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);
        let mut doc = Document::from_bytes(pdf).expect("open");
        let spans = doc.page(0).expect("p").raw_spans().expect("s");
        assert!(spans
            .iter()
            .any(|s| s.is_annotation && s.text.contains("FieldValue")));
    }

    #[test]
    fn cov_annot_widget_v_ref_to_non_string() {
        let mut b = TestPdfBuilder::new();
        b.add_object(
            10,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        );
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Main) Tj ET");
        b.add_object(7, b"42");
        b.add_object(
            5,
            b"<< /Type /Annot /Subtype /Widget /Rect [50 490 300 510] /V 7 0 R >>",
        );
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 10 0 R >> >> /Annots [5 0 R] >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);
        let mut doc = Document::from_bytes(pdf).expect("open");
        let spans = doc.page(0).expect("p").raw_spans().expect("s");
        assert!(!spans.iter().any(|s| s.is_annotation));
    }

    #[test]
    fn cov_annot_widget_v_array() {
        let mut b = TestPdfBuilder::new();
        b.add_object(
            10,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        );
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Main) Tj ET");
        b.add_object(
            5,
            b"<< /Type /Annot /Subtype /Widget /Rect [50 490 300 510] /V [1 2 3] >>",
        );
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 10 0 R >> >> /Annots [5 0 R] >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);
        let mut doc = Document::from_bytes(pdf).expect("open");
        let spans = doc.page(0).expect("p").raw_spans().expect("s");
        assert!(!spans.iter().any(|s| s.is_annotation));
    }

    #[test]
    fn cov_annot_ap_wrong_type() {
        let mut b = TestPdfBuilder::new();
        b.add_object(
            10,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        );
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Main) Tj ET");
        b.add_object(
            5,
            b"<< /Type /Annot /Subtype /FreeText /Rect [50 490 300 510] /AP 42 >>",
        );
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 10 0 R >> >> /Annots [5 0 R] >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);
        let mut doc = Document::from_bytes(pdf).expect("open");
        let spans = doc.page(0).expect("p").raw_spans().expect("s");
        assert!(!spans.iter().any(|s| s.is_annotation));
    }

    #[test]
    fn cov_annot_text_no_rect() {
        let mut b = TestPdfBuilder::new();
        b.add_object(
            10,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        );
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Main) Tj ET");
        b.add_object(5, b"<< /Type /Annot /Subtype /Text /Contents (NoteText) >>");
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 10 0 R >> >> /Annots [5 0 R] >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);
        let mut doc = Document::from_bytes(pdf).expect("open");
        let spans = doc.page(0).expect("p").raw_spans().expect("s");
        assert!(!spans.iter().any(|s| s.is_annotation));
    }

    #[test]
    fn cov_annot_text_empty_contents() {
        let mut b = TestPdfBuilder::new();
        b.add_object(
            10,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        );
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Main) Tj ET");
        b.add_object(
            5,
            b"<< /Type /Annot /Subtype /Text /Rect [50 490 300 510] /Contents () >>",
        );
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 10 0 R >> >> /Annots [5 0 R] >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);
        let mut doc = Document::from_bytes(pdf).expect("open");
        let spans = doc.page(0).expect("p").raw_spans().expect("s");
        assert!(!spans.iter().any(|s| s.is_annotation));
    }

    #[test]
    fn cov_annot_widget_v_no_rect() {
        let mut b = TestPdfBuilder::new();
        b.add_object(
            10,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        );
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Main) Tj ET");
        b.add_object(5, b"<< /Type /Annot /Subtype /Widget /V (SomeValue) >>");
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 10 0 R >> >> /Annots [5 0 R] >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);
        let mut doc = Document::from_bytes(pdf).expect("open");
        let spans = doc.page(0).expect("p").raw_spans().expect("s");
        assert!(!spans.iter().any(|s| s.is_annotation));
    }

    #[test]
    fn cov_page_tree_no_type_with_kids() {
        let mut b = TestPdfBuilder::new();
        b.add_object(5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(4, "", b"BT /F1 12 Tf 100 700 Td (Leaf) Tj ET");
        b.add_object(
            3,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
        );
        b.add_object(2, b"<< /Kids [3 0 R] /Count 1 >>");
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);
        let mut doc = Document::from_bytes(pdf).expect("open");
        assert_eq!(doc.page_count(), 1);
        assert!(doc.page(0).expect("p").text().expect("t").contains("Leaf"));
    }

    #[test]
    fn cov_page_tree_no_type_no_kids() {
        let mut b = TestPdfBuilder::new();
        b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (NoType) Tj ET");
        b.add_object(
            2,
            b"<< /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);
        let mut doc = Document::from_bytes(pdf).expect("open");
        assert_eq!(doc.page_count(), 1);
        assert!(doc
            .page(0)
            .expect("p")
            .text()
            .expect("t")
            .contains("NoType"));
    }

    #[test]
    fn cov_page_tree_kids_non_reference() {
        let mut b = TestPdfBuilder::new();
        b.add_object(5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(4, "", b"BT /F1 12 Tf 100 700 Td (Good) Tj ET");
        b.add_object(
            3,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
        );
        b.add_object(2, b"<< /Type /Pages /Kids [3 0 R 42] /Count 1 >>");
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);
        let diag = Arc::new(CollectingDiagnostics::new());
        let config = Config::default().with_diagnostics(diag.clone());
        let doc = Document::from_bytes_with_config(pdf, config).expect("open");
        assert_eq!(doc.page_count(), 1);
        assert!(diag
            .warnings()
            .iter()
            .any(|w| w.kind == WarningKind::InvalidPageTree
                && w.message.contains("instead of a reference")));
    }

    #[test]
    fn cov_page_tree_unknown_type() {
        let mut b = TestPdfBuilder::new();
        b.add_object(3, b"<< /Type /Bogus >>");
        b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);
        let doc = Document::from_bytes(pdf).expect("open");
        assert_eq!(doc.page_count(), 0);
    }

    #[test]
    fn cov_inherited_resources() {
        let mut b = TestPdfBuilder::new();
        b.add_object(5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(4, "", b"BT /F1 12 Tf 100 700 Td (Inherited) Tj ET");
        b.add_object(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
        );
        b.add_object(
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources << /Font << /F1 5 0 R >> >> >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);
        let mut doc = Document::from_bytes(pdf).expect("open");
        assert!(doc
            .page(0)
            .expect("p")
            .text()
            .expect("t")
            .contains("Inherited"));
    }

    #[test]
    fn cov_no_resources_no_parent() {
        let mut b = TestPdfBuilder::new();
        b.add_stream_object(3, "", b"BT 100 700 Td (NoRes) Tj ET");
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);
        let mut doc = Document::from_bytes(pdf).expect("open");
        let _spans = doc.page(0).expect("p").raw_spans().expect("s");
    }

    #[test]
    fn cov_page_extract() {
        let mut doc = open_cov_pdf();
        let content = doc.page(0).expect("p").extract().expect("extract");
        assert!(!content.spans.is_empty());
    }

    #[test]
    fn cov_page_images() {
        let mut doc = open_cov_pdf();
        let images = doc.page(0).expect("p").images().expect("images");
        assert!(images.is_empty());
    }

    #[test]
    fn cov_config_with_decode_limits() {
        let limits = DecodeLimits {
            max_decompressed_size: 999,
            ..DecodeLimits::default()
        };
        let config = Config::default().with_decode_limits(limits);
        assert_eq!(config.decode_limits.max_decompressed_size, 999);
    }

    #[test]
    fn cov_page_content_text_methods() {
        let mut doc = open_cov_pdf();
        let content = doc.page(0).expect("p").extract().expect("e");
        assert!(content.text().contains("Hello"));
        assert!(!content.text_lines().is_empty());
    }

    #[test]
    fn cov_page_content_into_text() {
        let mut doc = open_cov_pdf();
        let content = doc.page(0).expect("p").extract().expect("e");
        assert!(content.into_text().contains("Hello"));
    }

    #[test]
    fn cov_page_content_into_text_lines() {
        let mut doc = open_cov_pdf();
        let content = doc.page(0).expect("p").extract().expect("e");
        assert!(!content.into_text_lines().is_empty());
    }

    #[test]
    fn cov_page_structure_order() {
        let mut doc = open_cov_pdf();
        let page = doc.page(0).expect("p");
        assert!(page.page_structure_order().is_none());
    }

    #[test]
    fn cov_extract_all() {
        let mut doc = open_cov_pdf();
        let content = doc.page(0).expect("p").extract_all().expect("extract_all");
        assert!(!content.spans.is_empty());
    }

    #[test]
    fn cov_extract_full() {
        let mut doc = open_cov_pdf();
        let full = doc
            .page(0)
            .expect("p")
            .extract_full()
            .expect("extract_full");
        assert!(!full.text_lines.is_empty());
        assert!(!full.raw_spans.is_empty());
        // Tables and images may or may not be present in the test PDF,
        // but the method should succeed and return valid vectors.
        let _ = full.tables;
        for ai in &full.images {
            let _ = &ai.image;
            let _ = &ai.alt_text;
        }
    }

    #[test]
    fn cov_pdf_page_extractor_extract_full() {
        use udoc_core::backend::FormatBackend;
        let mut doc = open_cov_pdf();
        let mut page = FormatBackend::page(&mut doc, 0).expect("page");
        let extracted = page.extract_full().expect("extract_full");
        // Core types should be populated
        assert!(!extracted.text_lines.is_empty());
        assert!(!extracted.raw_spans.is_empty());
    }

    // -----------------------------------------------------------------------
    // /Info dictionary -> metadata tests
    // -----------------------------------------------------------------------

    #[test]
    fn metadata_from_info_dict() {
        use udoc_core::backend::FormatBackend;

        let mut b = TestPdfBuilder::new();
        b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Hi) Tj ET");
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        b.add_object(
            5,
            b"<< /Title (Test Document) /Author (Jane Doe) /Subject (Testing) /Creator (udoc tests) /Producer (udoc-pdf) /CreationDate (D:20260101120000Z) /ModDate (D:20260315080000Z) >>",
        );
        let pdf = b.finish_with_info(1, 5);

        let doc = Document::from_bytes(pdf).expect("open");
        let meta = FormatBackend::metadata(&doc);

        assert_eq!(meta.title.as_deref(), Some("Test Document"));
        assert_eq!(meta.author.as_deref(), Some("Jane Doe"));
        assert_eq!(meta.subject.as_deref(), Some("Testing"));
        assert_eq!(meta.creator.as_deref(), Some("udoc tests"));
        assert_eq!(meta.producer.as_deref(), Some("udoc-pdf"));
        assert_eq!(meta.creation_date.as_deref(), Some("D:20260101120000Z"));
        assert_eq!(meta.modification_date.as_deref(), Some("D:20260315080000Z"));
        assert_eq!(meta.page_count, 1);
    }

    #[test]
    fn metadata_no_info_dict() {
        use udoc_core::backend::FormatBackend;

        let mut b = TestPdfBuilder::new();
        b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Hi) Tj ET");
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        // No /Info in the trailer
        let pdf = b.finish(1);

        let doc = Document::from_bytes(pdf).expect("open");
        let meta = FormatBackend::metadata(&doc);

        assert!(meta.title.is_none());
        assert!(meta.author.is_none());
        assert!(meta.subject.is_none());
        assert!(meta.creator.is_none());
        assert!(meta.producer.is_none());
        assert!(meta.creation_date.is_none());
        assert!(meta.modification_date.is_none());
        assert_eq!(meta.page_count, 1);
    }

    #[test]
    fn metadata_partial_info_dict() {
        use udoc_core::backend::FormatBackend;

        let mut b = TestPdfBuilder::new();
        b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Hi) Tj ET");
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        // /Info with only /Title, other fields missing
        b.add_object(5, b"<< /Title (Partial) >>");
        let pdf = b.finish_with_info(1, 5);

        let doc = Document::from_bytes(pdf).expect("open");
        let meta = FormatBackend::metadata(&doc);

        assert_eq!(meta.title.as_deref(), Some("Partial"));
        assert!(meta.author.is_none());
        assert!(meta.subject.is_none());
        assert!(meta.creator.is_none());
        assert!(meta.producer.is_none());
    }

    #[test]
    fn metadata_utf16be_title() {
        use udoc_core::backend::FormatBackend;

        let mut b = TestPdfBuilder::new();
        b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Hi) Tj ET");
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        // /Title as UTF-16BE hex string: BOM + "AB"
        b.add_object(5, b"<< /Title <FEFF00410042> >>");
        let pdf = b.finish_with_info(1, 5);

        let doc = Document::from_bytes(pdf).expect("open");
        let meta = FormatBackend::metadata(&doc);

        assert_eq!(meta.title.as_deref(), Some("AB"));
    }

    #[test]
    fn metadata_empty_string_fields_become_none() {
        use udoc_core::backend::FormatBackend;

        let mut b = TestPdfBuilder::new();
        b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Hi) Tj ET");
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        // /Info with empty strings
        b.add_object(5, b"<< /Title () /Author () >>");
        let pdf = b.finish_with_info(1, 5);

        let doc = Document::from_bytes(pdf).expect("open");
        let meta = FormatBackend::metadata(&doc);

        // Empty strings should become None, not Some("")
        assert!(meta.title.is_none());
        assert!(meta.author.is_none());
    }

    #[test]
    fn links_uri_annotation() {
        let mut b = TestPdfBuilder::new();
        b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
        // Link annotation with /A /URI action and /Rect
        b.add_object(
            5,
            b"<< /Type /Annot /Subtype /Link /Rect [100 200 300 220] /A << /S /URI /URI (https://example.com) >> >>",
        );
        // Page with /Annots pointing to the link annotation
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots [5 0 R] >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("open");
        let mut page = doc.page(0).expect("page 0");
        let links = page.links();

        assert_eq!(links.len(), 1, "expected 1 link, got {}", links.len());
        assert_eq!(links[0].url, "https://example.com");
        let bbox = links[0].bbox.expect("link should have a bbox");
        assert!((bbox.x_min - 100.0).abs() < 1e-6);
        assert!((bbox.y_min - 200.0).abs() < 1e-6);
        assert!((bbox.x_max - 300.0).abs() < 1e-6);
        assert!((bbox.y_max - 220.0).abs() < 1e-6);
    }

    #[test]
    fn links_no_annots() {
        let mut b = TestPdfBuilder::new();
        b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
        // Page without /Annots
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("open");
        let mut page = doc.page(0).expect("page 0");
        let links = page.links();

        assert!(links.is_empty(), "expected no links, got {}", links.len());
    }

    #[test]
    fn bookmarks_with_outlines() {
        let mut b = TestPdfBuilder::new();
        b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Body) Tj ET");
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
        );
        // First outline item: Chapter 1 (no children)
        b.add_object(6, b"<< /Title (Chapter 1) >>");
        // Second outline item: Chapter 2 (no children), linked via /Next on item 6
        b.add_object(7, b"<< /Title (Chapter 2) >>");
        // Outlines root: /First points to Chapter 1, /Last to Chapter 2
        // We encode /Next on item 6 -> 7 and /Prev on 7 -> 6.
        // Rebuild item 6 with /Next reference:
        b.add_object(8, b"<< /Title (Chapter 1) /Next 7 0 R >>");
        // Outlines dictionary
        b.add_object(
            5,
            b"<< /Type /Outlines /First 8 0 R /Last 7 0 R /Count 2 >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R >>");
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("open");
        let bookmarks = doc.bookmarks();

        assert!(!bookmarks.is_empty(), "expected bookmarks, got none");
        assert_eq!(bookmarks[0].title, "Chapter 1");
        assert_eq!(bookmarks.len(), 2, "expected 2 top-level bookmarks");
        assert_eq!(bookmarks[1].title, "Chapter 2");
    }

    #[test]
    fn bookmarks_no_outlines() {
        let mut b = TestPdfBuilder::new();
        b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Body) Tj ET");
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
        );
        // Catalog without /Outlines
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("open");
        let bookmarks = doc.bookmarks();

        assert!(
            bookmarks.is_empty(),
            "expected no bookmarks, got {}",
            bookmarks.len()
        );
    }

    #[test]
    fn named_dest_resolved_via_name_tree() {
        // Build a PDF where a link annotation has /Dest (section1)
        // and the catalog has /Names/Dests name tree that maps "section1"
        // to [page_ref /Fit].
        let mut b = TestPdfBuilder::new();
        b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
        // Link annotation with /Dest as a string (named destination)
        b.add_object(
            5,
            b"<< /Type /Annot /Subtype /Link /Rect [100 200 300 220] /Dest (section1) >>",
        );
        // Page with the link annotation
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots [5 0 R] >>",
        );
        // Name tree leaf: maps "section1" -> [2 0 R /Fit]
        b.add_object(6, b"<< /Names [(section1) [2 0 R /Fit]] >>");
        // Catalog with /Names/Dests pointing to the name tree
        b.add_object(
            1,
            b"<< /Type /Catalog /Pages 2 0 R /Names << /Dests 6 0 R >> >>",
        );
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("open");
        let mut page = doc.page(0).expect("page 0");
        let links = page.links();

        assert_eq!(links.len(), 1, "expected 1 link, got {}", links.len());
        assert_eq!(
            links[0].url, "#page/Fit",
            "named dest should resolve to page dest"
        );
    }

    #[test]
    fn named_dest_resolved_via_legacy_dests_dict() {
        // Build a PDF with a legacy /Dests dictionary on the catalog.
        let mut b = TestPdfBuilder::new();
        b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
        // Link annotation with /Dest as a name (named destination)
        b.add_object(
            5,
            b"<< /Type /Annot /Subtype /Link /Rect [100 200 300 220] /Dest /chapter1 >>",
        );
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots [5 0 R] >>",
        );
        // Legacy /Dests: name -> dest array
        b.add_object(6, b"<< /chapter1 [2 0 R /XYZ 0 792 0] >>");
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R /Dests 6 0 R >>");
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("open");
        let mut page = doc.page(0).expect("page 0");
        let links = page.links();

        assert_eq!(links.len(), 1, "expected 1 link, got {}", links.len());
        assert_eq!(
            links[0].url, "#page/XYZ",
            "legacy dest should resolve to page dest"
        );
    }

    #[test]
    fn named_dest_fallback_when_not_found() {
        // Named dest that doesn't exist in any tree should fall back to #<name>.
        let mut b = TestPdfBuilder::new();
        b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
        b.add_object(
            5,
            b"<< /Type /Annot /Subtype /Link /Rect [100 200 300 220] /Dest (nonexistent) >>",
        );
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots [5 0 R] >>",
        );
        // Empty name tree
        b.add_object(6, b"<< /Names [] >>");
        b.add_object(
            1,
            b"<< /Type /Catalog /Pages 2 0 R /Names << /Dests 6 0 R >> >>",
        );
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("open");
        let mut page = doc.page(0).expect("page 0");
        let links = page.links();

        assert_eq!(links.len(), 1, "expected 1 link, got {}", links.len());
        assert_eq!(links[0].url, "#nonexistent", "should fall back to #<name>");
    }

    #[test]
    fn named_dest_via_goto_action() {
        // GoTo action with a named string destination, resolved via name tree.
        let mut b = TestPdfBuilder::new();
        b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
        // Link annotation with /A /GoTo action pointing to named dest
        b.add_object(
            5,
            b"<< /Type /Annot /Subtype /Link /Rect [100 200 300 220] /A << /S /GoTo /D (toc) >> >>",
        );
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots [5 0 R] >>",
        );
        // Name tree mapping "toc" -> [2 0 R /FitH 700]
        b.add_object(6, b"<< /Names [(toc) [2 0 R /FitH 700]] >>");
        b.add_object(
            1,
            b"<< /Type /Catalog /Pages 2 0 R /Names << /Dests 6 0 R >> >>",
        );
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("open");
        let mut page = doc.page(0).expect("page 0");
        let links = page.links();

        assert_eq!(links.len(), 1, "expected 1 link, got {}", links.len());
        assert_eq!(links[0].url, "#page/FitH", "GoTo named dest should resolve");
    }

    /// Verify links() works with inline Annots array (no clone of entire array).
    #[test]
    fn test_links_inline_annots_no_full_clone() {
        let mut b = TestPdfBuilder::new();
        b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
        // Page with inline Annots array containing a /Link and a non-link annotation.
        b.add_object(
            2,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 3 0 R \
              /Resources << /Font << /F1 4 0 R >> >> \
              /Annots [ \
                << /Type /Annot /Subtype /Link /Rect [10 10 100 30] \
                   /A << /S /URI /URI (https://example.com) >> >> \
                << /Type /Annot /Subtype /Text /Rect [200 200 220 220] >> \
              ] >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("should open");
        let mut page = doc.page(0).expect("page 0");
        let links = page.links();

        // Should find exactly the URI link, skipping the /Text annotation.
        assert_eq!(links.len(), 1, "expected 1 link, got {}", links.len());
        assert_eq!(links[0].url, "https://example.com");
        assert!(links[0].bbox.is_some(), "link should have a bounding box");
    }

    /// Catalog is resolved once and cached across multiple page() calls.
    /// Named dest resolution on the second page should use the cached catalog
    /// instead of re-resolving from the trailer.
    #[test]
    fn test_catalog_cached_for_named_dest() {
        let mut b = TestPdfBuilder::new();
        b.add_object(6, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (P1) Tj ET");
        b.add_stream_object(8, "", b"BT /F1 12 Tf 100 700 Td (P2) Tj ET");
        // Page 1 with a named dest link
        b.add_object(
            3,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 5 0 R \
              /Resources << /Font << /F1 6 0 R >> >> \
              /Annots [<< /Subtype /Link /Rect [0 0 100 20] /Dest (toc) >>] >>",
        );
        // Page 2 with a named dest link
        b.add_object(
            4,
            b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 8 0 R \
              /Resources << /Font << /F1 6 0 R >> >> \
              /Annots [<< /Subtype /Link /Rect [0 0 100 20] /Dest (toc) >>] >>",
        );
        b.add_object(2, b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
        // Name tree mapping "toc" -> [3 0 R /FitH 700]
        b.add_object(7, b"<< /Names [(toc) [3 0 R /FitH 700]] >>");
        b.add_object(
            1,
            b"<< /Type /Catalog /Pages 2 0 R /Names << /Dests 7 0 R >> >>",
        );
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("open");
        assert!(
            doc.cached_catalog.is_none(),
            "catalog should not be cached before first page()"
        );

        // First page() populates the cache.
        let mut page0 = doc.page(0).expect("page 0");
        let links0 = page0.links();
        assert_eq!(links0.len(), 1, "page 0 should have 1 link");
        drop(page0);
        assert!(
            doc.cached_catalog.is_some(),
            "catalog should be cached after first page()"
        );

        // Second page() reuses the cached catalog.
        let mut page1 = doc.page(1).expect("page 1");
        let links1 = page1.links();
        assert_eq!(links1.len(), 1, "page 1 should have 1 link");
        assert_eq!(
            links1[0].url, "#page/FitH",
            "named dest should resolve from cached catalog"
        );
    }

    /// ISO 32000-1 §7.7.3.4: /MediaBox is inheritable. A leaf /Page without
    /// its own /MediaBox must pick up the value declared on an ancestor
    /// /Pages node. govdocs1/005020 triggered this: the file is legal-size
    /// (612x1008) but the /MediaBox lives on the intermediate /Pages node,
    /// so without inheritance we rendered at the US Letter fallback and
    /// baseline was off by 316 pt (450 px at 150 DPI).
    #[test]
    fn test_page_bbox_inherits_mediabox_from_grandparent() {
        let mut b = TestPdfBuilder::new();
        b.add_object(6, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (hi) Tj ET");
        // Leaf page: no /MediaBox, inherits from grandparent (obj 3).
        b.add_object(
            4,
            b"<< /Type /Page /Parent 2 0 R /Contents 5 0 R /Resources << /Font << /F1 6 0 R >> >> >>",
        );
        // Intermediate /Pages: also no /MediaBox.
        b.add_object(
            2,
            b"<< /Type /Pages /Kids [4 0 R] /Count 1 /Parent 3 0 R >>",
        );
        // Root /Pages: /MediaBox here (legal size 612 x 1008).
        b.add_object(
            3,
            b"<< /Type /Pages /Kids [2 0 R] /Count 1 /MediaBox [0 0 612 1008] >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 3 0 R >>");
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("open");
        let mut page = doc.page(0).expect("page 0");
        let bbox = page.page_bbox();
        assert_eq!(bbox.x_min, 0.0);
        assert_eq!(bbox.y_min, 0.0);
        assert_eq!(bbox.x_max, 612.0);
        assert_eq!(
            bbox.y_max, 1008.0,
            "expected legal size inherited from grandparent, got {bbox:?}"
        );
    }

    /// Leaf /Page with its own /MediaBox must win over any ancestor value.
    #[test]
    fn test_page_bbox_leaf_mediabox_overrides_parent() {
        let mut b = TestPdfBuilder::new();
        b.add_object(6, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (hi) Tj ET");
        // Leaf declares letter size.
        b.add_object(
            4,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources << /Font << /F1 6 0 R >> >> >>",
        );
        // Parent declares a bigger box that must be overridden.
        b.add_object(
            2,
            b"<< /Type /Pages /Kids [4 0 R] /Count 1 /MediaBox [0 0 1000 1500] >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("open");
        let mut page = doc.page(0).expect("page 0");
        let bbox = page.page_bbox();
        assert_eq!(bbox.x_max, 612.0);
        assert_eq!(bbox.y_max, 792.0);
    }

    /// /CropBox inheritance is independent of /MediaBox: a leaf with no
    /// /CropBox but its own /MediaBox, and a /CropBox on the ancestor,
    /// should pick up the inherited /CropBox.
    #[test]
    fn test_page_bbox_inherits_cropbox_independently() {
        let mut b = TestPdfBuilder::new();
        b.add_object(6, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (hi) Tj ET");
        // Leaf declares /MediaBox only.
        b.add_object(
            4,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources << /Font << /F1 6 0 R >> >> >>",
        );
        // Parent declares /CropBox (tighter than MediaBox).
        b.add_object(
            2,
            b"<< /Type /Pages /Kids [4 0 R] /Count 1 /CropBox [36 36 576 756] >>",
        );
        b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let pdf = b.finish(1);

        let mut doc = Document::from_bytes(pdf).expect("open");
        let mut page = doc.page(0).expect("page 0");
        let bbox = page.page_bbox();
        // /CropBox wins over /MediaBox when both are available (leaf+parent).
        assert_eq!(bbox.x_min, 36.0);
        assert_eq!(bbox.y_min, 36.0);
        assert_eq!(bbox.x_max, 576.0);
        assert_eq!(bbox.y_max, 756.0);
    }
}

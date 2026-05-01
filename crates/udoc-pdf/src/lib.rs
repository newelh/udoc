#![deny(unsafe_code)]

//! A Rust library for extracting text, images, and tables from PDF files.
//!
//! Optimized for robustness against malformed files. Not a renderer.
//!
//! # Quick start
//!
//! ```no_run
//! let mut doc = udoc_pdf::Document::open("example.pdf")?;
//! for i in 0..doc.page_count() {
//!     let mut page = doc.page(i)?;
//!     println!("{}", page.text()?);
//! }
//! # Ok::<(), udoc_pdf::Error>(())
//! ```
//!
//! # Tiered API
//!
//! Text extraction is available at three levels of detail:
//!
//! - [`Page::text()`] returns a plain `String` with reading order reconstruction.
//! - [`Page::text_lines()`] returns [`TextLine`]s with baseline and position metadata.
//! - [`Page::raw_spans()`] returns [`TextSpan`]s in content stream order (no ordering applied).
//!
//! Image extraction is available via [`Page::images()`]. Use [`Page::extract()`] to get
//! both text and images from a single interpretation pass.
//!
//! # Features
//!
//! - **Font coverage**: Type1, TrueType, CID (CJK) with predefined CMaps, ToUnicode,
//!   encoding overrides, Adobe Glyph List fallback
//! - **Stream filters**: Flate, LZW, ASCII85, ASCIIHex, RunLength, with PNG and TIFF predictors
//! - **Reading order**: baseline clustering, word gaps, multi-column detection, table hints,
//!   rotation grouping, vertical CJK
//! - **Images**: inline (BI/ID/EI) and XObject, with pass-through for JPEG/JPEG2000/JBIG2/CCITT
//! - **Diagnostics**: pluggable [`DiagnosticsSink`] with structured [`Warning`]s (kind, severity,
//!   offset, page index, object reference)
//!
//! # Safety
//!
//! The crate enforces `#![deny(unsafe_code)]` at the root. One isolated `unsafe` block exists in
//! [`Document::from_bytes_with_config`] for self-referential lifetime extension, with documented
//! safety invariants. All resource consumption is bounded: recursion depth, decompression size,
//! cache size, collection size, filter chain depth, and span accumulation.
//!
//! # Dependencies
//!
//! Five runtime dependencies, all from RustCrypto or the Rust ecosystem:
//! [`flate2`](https://crates.io/crates/flate2) for FlateDecode (zlib),
//! [`md-5`](https://crates.io/crates/md-5) for encryption key derivation,
//! [`sha2`](https://crates.io/crates/sha2) for R6 password validation,
//! [`aes`](https://crates.io/crates/aes) + [`cbc`](https://crates.io/crates/cbc) for AES-CBC
//! decryption.
//! RC4 is implemented directly since the RustCrypto rc4 crate requires compile-time key sizes.
//!
//! # Architecture
//!
//! Layers are strict: `document -> text -> content/font -> object -> parse -> io`.
//! Each layer depends only on the layers below it.

// Geometry types. Exposed for integration tests and fuzz targets via test-internals feature.
#[cfg(any(test, feature = "test-internals"))]
pub mod geometry;
#[cfg(not(any(test, feature = "test-internals")))]
pub(crate) mod geometry;

// I/O abstraction layer. Internal only.
pub(crate) mod io;

// PDF object model. Exposed for integration tests and fuzz targets via test-internals feature.
// Used by subsystem_integration, corpus_integration, font_spike, fuzz_content, etc.
#[cfg(any(test, feature = "test-internals"))]
pub mod object;
#[cfg(not(any(test, feature = "test-internals")))]
pub(crate) mod object;

// PDF parsing layer. Exposed for integration tests and fuzz targets via test-internals feature.
// Used by corpus_integration, extended_corpus, fuzz_lexer, fuzz_document_parser, etc.
#[cfg(any(test, feature = "test-internals"))]
pub mod parse;
#[cfg(not(any(test, feature = "test-internals")))]
pub(crate) mod parse;

// Table extraction. Exposed for integration tests and fuzz targets via test-internals feature.
// Used by table_golden tests, fuzz_table_detection
#[cfg(any(test, feature = "test-internals"))]
pub mod table;
#[cfg(not(any(test, feature = "test-internals")))]
pub(crate) mod table;

// Text ordering and reconstruction. Exposed for integration tests and fuzz targets via test-internals.
// Used by fuzz_reading_order (order_spans), profile_phases example
#[cfg(any(test, feature = "test-internals"))]
pub mod text;
#[cfg(not(any(test, feature = "test-internals")))]
pub(crate) mod text;

/// Spatial clustering, gap detection, and bbox filter helpers for PDF
/// text. Promoted from `examples/investigate_*.rs` per
/// /  A.6. See
/// [`text::cluster`](text::cluster) for the underlying module; this
/// re-export keeps the helpers reachable when `text` is `pub(crate)`
/// (default build).
pub use text::cluster;

/// Content stream interpreter. Internal module, exposed for integration testing.
// Used by fuzz_content
#[cfg(any(test, feature = "test-internals"))]
pub mod content;
#[cfg(not(any(test, feature = "test-internals")))]
pub(crate) mod content;
mod convert;
pub(crate) mod crypt;
mod diagnostics;
mod document;
mod error;
/// Font subsystem. Public for renderer access from the facade crate;
/// doc-hidden to keep it out of the downstream-user API tour. See
///  for the freeze policy.
#[doc(hidden)]
pub mod font;
mod image;
/// Type 1 coloured tiling pattern parsing (ISO 32000-2 §8.7.3,
///). The parser is public so
/// integration tests and (Wave 3) can consume
/// [`pattern::TilingPattern`] directly. Doc-hidden because the
/// signature references `pub(crate)` types from `object::types`.
#[doc(hidden)]
pub mod pattern;
pub(crate) mod text_decode;

pub use diagnostics::{
    CollectingDiagnostics, DiagnosticsSink, NullDiagnostics, Warning, WarningContext, WarningKind,
    WarningLevel,
};
pub use document::{
    AnnotatedImage, BookmarkEntry, Config, Document, ExtractedPage, FullPageContent, Page,
    PageAnnotation, PageAnnotationKind, PageContent, PageLink, PdfPageExtractor,
};
pub use error::{
    EncryptionError, EncryptionErrorKind, Error, Limit, ResourceLimitError, Result, ResultExt,
};
pub use geometry::BoundingBox;
pub use image::{ImageFilter, PageImage};
// DecodeLimits is part of the public API: used in Config::with_decode_limits doc example.
/// Canonical renderer-facing path IR. Distinct from
/// [`table::PathSegment`]: that type is the pre-flattened line/rect
/// representation used by the table detector; this one is the raw,
/// curve-preserving, CTM-snapshot-carrying IR consumed by the page
/// renderer.
pub use content::path::{
    tile_fallback_color, Color as PathColor, FillRule as PathFillRule, LineCap, LineJoin, Matrix3,
    PagePath, PageShading, PageShadingKind, PageTilingPattern,
    PathSegmentKind as PagePathSegmentKind, Point as PathPoint, ShadingLut, StrokeStyle,
};
pub use object::colorspace::{classify_pattern_colorspace, Colorspace, PatternColorspace};
pub use object::stream::DecodeLimits;
pub use pattern::{parse_tiling_pattern, ParseOutcome, TilingPattern};
pub use table::{
    extract_tables, ClipPathIR, FillRule, PathSegment, PathSegmentKind, Table, TableCell,
    TableDetectionMethod, TableRow,
};
pub use text::{TextLine, TextSpan};

// Re-export for fuzz targets. coherence is pub(crate) but fuzz
// targets build with cfg(fuzzing) and need access to stream_order_coherence.
// Also gated on test-internals so `cargo check` in fuzz/ works (fuzz/ enables
// test-internals but `cargo check` does not set cfg(fuzzing)).
// Used by fuzz_reading_order
#[cfg(any(fuzzing, feature = "test-internals"))]
pub use text::coherence::stream_order_coherence;

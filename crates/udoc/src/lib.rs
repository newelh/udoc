#![deny(unsafe_code)]
#![warn(missing_docs)]

//! udoc -- Extract text, tables, and images from documents.
//!
//! A unified toolkit for extracting structured content from 12 document formats.
//! Format detection is automatic from magic bytes and container inspection.
//!
//! # Supported formats
//!
//! | Format | Notes |
//! |--------|-------|
//! | PDF | Production-quality. Text, tables, images, CJK, multi-column, stream filters. |
//! | DOCX | Word 2007+. Paragraphs, tables, headings, numbering, headers/footers. |
//! | XLSX | Excel 2007+. Sheets, typed cells, shared strings, date formats. |
//! | PPTX | PowerPoint 2007+. Slides, shape trees, tables, speaker notes. |
//! | RTF | Text, tables, images, Unicode, 20+ CJK codepages. |
//! | ODT | OpenDocument Text. Paragraphs, tables, headings, lists. |
//! | ODS | OpenDocument Spreadsheet. Sheets, typed cells. |
//! | ODP | OpenDocument Presentation. Slides, shape content, notes. |
//! | DOC | Word 97-2003 (binary). Text extraction via CFB stream parsing. |
//! | XLS | Excel 97-2003 (BIFF8). Sheets, cells, SST, CONTINUE record reassembly. |
//! | PPT | PowerPoint 97-2003 (binary). Slides, text frames. |
//! | Markdown | CommonMark + GFM subset. Headings, lists, tables, code blocks. |
//!
//! # Quick start
//!
//! Extract a PDF from in-memory bytes (uses the bundled `hello.pdf` fixture
//! so the example runs end to end in the doctest sandbox):
//!
//! ```ignore
//! // From in-memory bytes (no I/O dependency).
//! let bytes = include_bytes!("../../../tests/corpus/minimal/hello.pdf");
//! let doc = udoc::extract_bytes(bytes)?;
//! assert_eq!(doc.metadata.page_count, 1);
//! for block in &doc.content {
//!     let _ = block.text();
//! }
//!
//! // Streaming page-by-page access via the Extractor.
//! let mut ext = udoc::Extractor::from_bytes(bytes)?;
//! for i in 0..ext.page_count() {
//!     let _ = ext.page_text(i)?;
//! }
//! # Ok::<(), udoc::Error>(())
//! ```
//!
//! For path-based extraction the call site is the obvious analogue:
//!
//! ```no_run
//! // ignore-runtime: requires a user-supplied file path that isn't bundled.
//! let doc = udoc::extract("report.pdf")?;
//! println!("{}", doc.metadata.page_count);
//! # Ok::<(), udoc::Error>(())
//! ```
//!
//! # Custom configuration
//!
//! ```ignore
//! use udoc::{Config, Format};
//!
//! let bytes = include_bytes!("../../../tests/corpus/minimal/hello.pdf");
//! let config = Config::new()
//!     .format(Format::Pdf)            // skip auto-detection
//!     .pages("1")?;                   // only page 1
//!
//! let doc = udoc::extract_bytes_with(bytes, config)?;
//! assert!(doc.metadata.page_count >= 1);
//! # Ok::<(), udoc::Error>(())
//! ```
//!
//! # Capturing diagnostics
//!
//! Extraction is intentionally lenient: malformed inputs produce warnings, not
//! errors, so callers see partial results rather than nothing. Wire a
//! [`CollectingDiagnostics`] sink to inspect what was skipped or recovered.
//!
//! ```ignore
//! use std::sync::Arc;
//! use udoc::{CollectingDiagnostics, Config};
//!
//! let bytes = include_bytes!("../../../tests/corpus/minimal/hello.pdf");
//! let diag = Arc::new(CollectingDiagnostics::new());
//! let mut config = Config::new();
//! config.diagnostics = diag.clone();
//!
//! let _ = udoc::extract_bytes_with(bytes, config)?;
//!
//! // The minimal fixture parses cleanly, so no warnings are expected.
//! for w in diag.warnings() {
//!     eprintln!("[{}] {}", w.kind, w.message);
//! }
//! # Ok::<(), udoc::Error>(())
//! ```
//!
//! # Long-running batch workers
//!
//! When a single process extracts thousands of documents in a loop, glibc's
//! allocator can retain pages beyond what's reachable. Set a soft memory
//! budget and the [`Extractor`] will release per-document caches between
//! files when RSS exceeds it. Peak memory *within* a document is unaffected.
//!
//! ```ignore
//! use udoc::{Config, Extractor};
//! use udoc_core::limits::Limits;
//!
//! let bytes = include_bytes!("../../../tests/corpus/minimal/hello.pdf");
//! let cfg = Config::new()
//!     .limits(Limits::builder().memory_budget(Some(2_000_000_000)).build()); // 2 GB
//! // Same loop you'd use over a paths iterator, run once on the bundled fixture.
//! let mut ext = Extractor::from_bytes_with(bytes, cfg.clone())?;
//! let _ = ext.text()?;
//! # Ok::<(), udoc::Error>(())
//! ```
//!
//! For deterministic resets without RSS polling call
//! [`Extractor::reset_document_caches`](crate::Extractor::reset_document_caches)
//! every N documents instead. For 20K+ corpora the recommended pattern is
//! subprocess fork; see `bench-compare render --batch-subprocess-size`.

mod backend_trait;
// cli is no longer reachable through the facade
// (it was `#[doc(hidden)] pub mod cli;` before). The cli/* source files
// are `#[path]`-mounted directly from `main.rs` (bin target). Keeping
// cli out of the lib avoids compiling it twice and keeps the lib's
// public-API surface clean. Sub-modules in cli/mod.rs stay
// `pub(crate)` so the bin-side `mod cli;` declaration only exposes the
// surfaces `main.rs` actually uses.
pub mod config;
mod convert;
pub mod detect;
mod doc_backend;
mod docx_backend;
pub mod extractor;
#[cfg(feature = "json")]
pub mod hooks;
mod md_backend;
mod odf_backend;
#[cfg(feature = "json")]
#[doc(hidden)]
pub mod output;
mod pdf_backend;
mod ppt_backend;
mod pptx_backend;
/// Page rendering module. Renders document pages to PNG images for OCR
/// hooks and layout detection models.
pub mod render;
mod rtf_backend;
mod xls_backend;
mod xlsx_backend;

// Re-exports: core error types
pub use udoc_core::error::{Error, Result, ResultExt};

// Re-exports: geometry
pub use udoc_core::geometry::BoundingBox;

// Re-exports: diagnostics
pub use udoc_core::diagnostics::{
    CollectingDiagnostics, DiagnosticsSink, NullDiagnostics, TeeDiagnostics, Warning,
    WarningContext, WarningKind, WarningLevel,
};

// Re-exports:  Document model types
pub use udoc_core::document::{
    // Presentation overlay
    Alignment,
    // Asset store
    AssetConfig,
    AssetRef,
    AssetStore,
    // Content spine
    Block,
    BlockLayout,
    // Table types
    CellValue,
    // Interactions overlay
    ChangeType,
    ColSpec,
    Color,
    Comment,
    // Relationships overlay
    ComponentRef,
    // Core types
    Document,
    DocumentMetadata,
    ExtendedTextStyle,
    FlowDirection,
    FontAsset,
    FontProgramType,
    FootnoteDef,
    FormField,
    FormFieldType,
    ImageAsset,
    ImageData,
    ImageRef,
    Inline,
    Interactions,
    LayoutInfo,
    LayoutMode,
    ListItem,
    ListKind,
    NodeId,
    // Overlays
    Overlay,
    Padding,
    PageDef,
    PositionedSpan,
    Presentation,
    Relationships,
    SectionRole,
    ShapeKind,
    SpanStyle,
    SparseOverlay,
    TableCell,
    TableData,
    TableRow,
    TocEntry,
    TrackedChange,
};

// Hard ceilings. These are not user-tunable knobs; they are
// internal recursion / arena limits enforced by the document model so
// untrusted input cannot exhaust memory.
//
// cut from the public surface. The constants
// remain reachable via `udoc_core::document::*` for fuzz/test code that
// enables the `test-internals` feature on both crates. Production
// callers should not branch on internal arena ceilings; let the model
// emit a `WarningKind::ResourceLimit` instead.
#[cfg(feature = "test-internals")]
pub use udoc_core::document::{MAX_CELL_VALUE_DEPTH, MAX_COMMENT_DEPTH, MAX_NODE_ID};

// Re-exports: Config types
pub use config::{Config, LayerConfig, PageRange};

// Re-exports: Format detection
pub use detect::Format;

// Re-exports: Extractor
pub use extractor::Extractor;

/// Page-level types namespaced to avoid collisions with document model types.
pub mod page {
    pub use udoc_core::image::{ImageFilter, PageImage};
    pub use udoc_core::table::{
        Table as PageTable, TableCell as PageTableCell, TableRow as PageTableRow,
    };
    pub use udoc_core::text::{TextLine, TextSpan};
}

// ---------------------------------------------------------------------------
// Test-internals: expose pub(crate) items for fuzz targets
// ---------------------------------------------------------------------------

/// Internal APIs exposed for fuzz targets. Not part of the public API.
/// Gated behind the `test-internals` feature flag so downstream users
/// cannot accidentally depend on these.
// Used by fuzz_hook_response
#[cfg(all(feature = "test-internals", feature = "json"))]
pub mod internals {
    pub use crate::hooks::response::apply_response;
}

// ---------------------------------------------------------------------------
// Free functions: one-shot extraction
// ---------------------------------------------------------------------------

/// Extract a full document from a file path with default configuration.
///
/// Detects the format automatically. Returns the unified Document model.
///
/// ```no_run
/// // ignore-runtime: requires a user-supplied file path that isn't bundled.
/// let doc = udoc::extract("report.pdf")?;
/// for block in &doc.content {
///     println!("{}", block.text());
/// }
/// # Ok::<(), udoc::Error>(())
/// ```
pub fn extract(path: impl AsRef<std::path::Path>) -> Result<Document> {
    extract_with(path, Config::default())
}

/// Extract a full document from a file path with custom configuration.
///
/// ```no_run
/// // ignore-runtime: requires a user-supplied file path that isn't bundled.
/// use udoc::{Config, Format};
/// let cfg = Config::new().format(Format::Pdf);
/// let doc = udoc::extract_with("report.pdf", cfg)?;
/// assert!(doc.metadata.page_count >= 1);
/// # Ok::<(), udoc::Error>(())
/// ```
pub fn extract_with(path: impl AsRef<std::path::Path>, config: Config) -> Result<Document> {
    Extractor::open_with(path, config)?.into_document()
}

/// Extract a full document from in-memory bytes with default configuration.
///
/// Detects the format from magic bytes. Returns the unified Document model.
///
/// ```ignore
/// let bytes = include_bytes!("../../../tests/corpus/minimal/hello.pdf");
/// let doc = udoc::extract_bytes(bytes)?;
/// assert_eq!(doc.metadata.page_count, 1);
/// # Ok::<(), udoc::Error>(())
/// ```
pub fn extract_bytes(data: &[u8]) -> Result<Document> {
    extract_bytes_with(data, Config::default())
}

/// Extract a full document from in-memory bytes with custom configuration.
///
/// ```ignore
/// use udoc::{Config, Format};
/// let bytes = include_bytes!("../../../tests/corpus/minimal/hello.pdf");
/// let cfg = Config::new().format(Format::Pdf); // skip auto-detection
/// let doc = udoc::extract_bytes_with(bytes, cfg)?;
/// assert_eq!(doc.metadata.page_count, 1);
/// # Ok::<(), udoc::Error>(())
/// ```
pub fn extract_bytes_with(data: &[u8], config: Config) -> Result<Document> {
    Extractor::from_bytes_with(data, config)?.into_document()
}

// ---------------------------------------------------------------------------
// Internal helpers shared by free functions and Extractor
// ---------------------------------------------------------------------------

pub(crate) fn open_pdf_path(path: &std::path::Path, config: &Config) -> Result<udoc_pdf::Document> {
    // Bridges the facade DiagnosticsSink (kind: udoc_core::WarningKind)
    // to the PDF-internal DiagnosticsSink (kind: udoc_pdf::WarningKind).
    // moved the PDF kind variants into the core enum so
    // the bridge maps enum -> enum without a lossy String round-trip.
    let mut pdf_config = udoc_pdf::Config::default().with_diagnostics(
        udoc_pdf::Document::core_diag_bridge(config.diagnostics.clone()),
    );
    if let Some(ref pw) = config.password {
        pdf_config = pdf_config.with_password(pw.as_bytes().to_vec());
    }
    udoc_pdf::Document::open_with_config(path, pdf_config)
        .map_err(|e| wrap_pdf_open_error(format!("opening PDF '{}'", path.display()), e))
}

pub(crate) fn open_pdf_bytes(data: &[u8], config: &Config) -> Result<udoc_pdf::Document> {
    // Always bridge the core DiagnosticsSink, with or without a password.
    let mut pdf_config = udoc_pdf::Config::default().with_diagnostics(
        udoc_pdf::Document::core_diag_bridge(config.diagnostics.clone()),
    );
    if let Some(ref pw) = config.password {
        pdf_config = pdf_config.with_password(pw.as_bytes().to_vec());
    }
    udoc_pdf::Document::from_bytes_with_config(data.to_vec(), pdf_config)
        .map_err(|e| wrap_pdf_open_error("opening PDF from bytes".to_string(), e))
}

/// Wrap a `udoc_pdf::Error` as a core [`Error`] for the `Extractor::open*`
/// path, preserving the typed encryption signal so callers can dispatch
/// via [`Error::is_encryption_error`] / [`Error::encryption_info`]
/// instead of substring-matching the displayed message.
///
/// Plain (non-encryption) PDF errors keep the existing
/// `Error::with_source` shape -- the user-facing display chain is
/// unchanged. ( verify-report.md gap #7.)
fn wrap_pdf_open_error(ctx: String, err: udoc_pdf::Error) -> Error {
    use udoc_core::error::EncryptionReason;
    use udoc_pdf::EncryptionErrorKind;

    if let udoc_pdf::Error::Encryption(enc) = &err {
        let reason = match &enc.kind {
            EncryptionErrorKind::InvalidPassword => EncryptionReason::PasswordRequired,
            EncryptionErrorKind::UnsupportedFilter(name) => {
                EncryptionReason::UnsupportedAlgorithm(format!("filter={name}"))
            }
            EncryptionErrorKind::UnsupportedVersion { v, r } => {
                EncryptionReason::UnsupportedAlgorithm(format!("V={v} R={r}"))
            }
            EncryptionErrorKind::MissingField(field) => {
                EncryptionReason::Malformed(format!("missing field: {field}"))
            }
            EncryptionErrorKind::InvalidField(d) => EncryptionReason::Malformed(d.clone()),
            EncryptionErrorKind::DecryptionFailed(d) => EncryptionReason::Other(d.clone()),
            // EncryptionErrorKind is #[non_exhaustive]; future variants
            // map to Other with the Display string until a finer
            // mapping is added.
            other => EncryptionReason::Other(other.to_string()),
        };
        // Preserve the displayed PDF error chain in the context so
        // user-facing messages still surface "encryption error: ..." verbatim.
        return Error::encryption_required(reason)
            .with_context(format!("{err}"))
            .with_context(ctx);
    }
    Error::with_source(ctx, err)
}

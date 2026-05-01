#![deny(unsafe_code)]

//! PPTX (PresentationML) text extraction backend for the udoc toolkit.
//!
//! Parses OOXML PresentationML packages (.pptx) and exposes slide content
//! through the `FormatBackend` and `PageExtractor` traits from udoc-core.
//!
//! # Architecture
//!
//! - Each slide maps to one "page" in the FormatBackend API
//! - Slide order comes from `p:sldIdLst` in presentation.xml (not filesystem)
//! - Text is extracted from shape trees (`p:spTree`) via DrawingML elements
//! - Reading order uses Y-then-X coordinate sorting
//! - Master/layout template text is NOT extracted (only slide-level shapes)
//!
//! # Usage
//!
//! ```ignore
//! use std::sync::Arc;
//! use udoc_core::backend::FormatBackend;
//! use udoc_core::diagnostics::NullDiagnostics;
//! use udoc_pptx::PptxDocument;
//!
//! let bytes = include_bytes!("../tests/corpus/real-world/test.pptx");
//! let doc = PptxDocument::from_bytes_with_diag(bytes, Arc::new(NullDiagnostics))?;
//! assert!(doc.page_count() > 0);
//! # Ok::<(), udoc_core::error::Error>(())
//! ```

/// Maximum archive size before rejection (256 MB).
pub const MAX_FILE_SIZE: u64 = udoc_core::limits::DEFAULT_MAX_FILE_SIZE;

mod convert;
mod document;
pub mod error;
mod notes;
mod shapes;
mod table;
mod text;

// Public API: document/page types, conversion function, facade helpers, and error types.
pub use convert::pptx_to_document;
pub use document::{heading_level_from_placeholder, PptxDocument, PptxPage, ShapeSummary};
pub use udoc_core::error::{Error, Result, ResultExt};

/// Maximum shape tree recursion depth (group nesting).
const MAX_SHAPE_DEPTH: usize = 256;

/// Maximum number of slides to process per presentation.
const MAX_SLIDES: usize = 10_000;

/// Maximum number of shapes per slide.
const MAX_SHAPES_PER_SLIDE: usize = 100_000;

/// Maximum number of images extracted per slide. Shapes are bounded by
/// MAX_SHAPES_PER_SLIDE, but this provides an explicit, tighter cap so
/// image extraction cost is clearly bounded.
const MAX_IMAGES_PER_SLIDE: usize = 10_000;

/// Maximum number of rows per table.
const MAX_TABLE_ROWS: usize = 100_000;

/// Maximum number of cells per table row.
const MAX_CELLS_PER_ROW: usize = 10_000;

/// Maximum number of paragraphs per text body.
const MAX_PARAGRAPHS: usize = 100_000;

/// Maximum text content length per element (10 MB).
const MAX_TEXT_LENGTH: usize = 10 * 1024 * 1024;

/// EMU (English Metric Units) per point. 1 point = 12700 EMU.
const EMU_PER_POINT: f64 = 12700.0;

/// PPTX font size units: hundredths of a point. 100 = 1pt.
const FONT_SIZE_DIVISOR: f64 = 100.0;

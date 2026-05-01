#![deny(unsafe_code)]
//! DOCX text extraction backend for the udoc document toolkit.
//!
//! Parses Office Open XML (OOXML) word-processing documents (.docx) and
//! extracts text, tables, headings, lists, headers/footers, and footnotes
//! using udoc-core's format-agnostic types.
//!
//! Handles both Transitional and Strict OOXML namespace URIs.
//!
//! # Example
//!
//! ```ignore
//! use std::sync::Arc;
//! use udoc_core::backend::FormatBackend;
//! use udoc_core::diagnostics::NullDiagnostics;
//! use udoc_docx::DocxDocument;
//!
//! // Drive the backend on a real bundled fixture.
//! let bytes = include_bytes!("../tests/corpus/real-world/SampleDoc.docx");
//! let doc = DocxDocument::from_bytes_with_diag(bytes, Arc::new(NullDiagnostics))?;
//! assert!(doc.page_count() >= 1);
//! # Ok::<(), udoc_core::error::Error>(())
//! ```

/// Maximum archive size before rejection (256 MB).
pub const MAX_FILE_SIZE: u64 = udoc_core::limits::DEFAULT_MAX_FILE_SIZE;

mod ancillary;
mod convert;
pub mod document;
pub mod error;
pub(crate) mod numbering;
pub(crate) mod parser;
pub(crate) mod styles;
pub(crate) mod table;

pub use convert::docx_to_document;
pub use document::DocxDocument;
pub use error::{Error, Result, ResultExt};

// Types exposed by DocxDocument's public methods. These are part of the
// DOCX crate's documented API (needed to iterate body(), headers(), etc.).
pub use parser::{BodyElement, Endnote, Footnote, Paragraph};
pub use styles::StyleMap;

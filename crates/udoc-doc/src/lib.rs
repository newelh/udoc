#![deny(unsafe_code)]
//! Word binary (.doc) extraction backend for the udoc document toolkit.
//!
//! Parses legacy Word 97-2003 binary files and extracts text, paragraphs,
//! and metadata using udoc-core's format-agnostic types.
//!
//! # Example
//!
//! ```ignore
//! use std::sync::Arc;
//! use udoc_core::backend::FormatBackend;
//! use udoc_core::diagnostics::NullDiagnostics;
//! use udoc_doc::DocDocument;
//!
//! let bytes = include_bytes!("../tests/corpus/real-world/footnote.doc");
//! let doc = DocDocument::from_bytes_with_diag(bytes, Arc::new(NullDiagnostics))?;
//! assert!(doc.page_count() >= 1);
//! # Ok::<(), udoc_core::error::Error>(())
//! ```

pub(crate) mod convert;
pub mod document;
pub mod error;
pub(crate) mod fib;
pub(crate) mod font_table;
pub(crate) mod piece_table;
pub(crate) mod properties;
pub(crate) mod tables;
pub(crate) mod text;

#[cfg(any(test, feature = "test-internals"))]
pub mod test_util;

pub use convert::doc_to_document;
pub use document::DocDocument;
pub use error::{Error, Result, ResultExt};

/// Maximum file size for DOC documents (256 MB).
pub const MAX_FILE_SIZE: u64 = udoc_core::limits::DEFAULT_MAX_FILE_SIZE;

/// Maximum number of pieces in the piece table.
pub const MAX_PIECES: usize = 1_000_000;

/// Maximum number of paragraphs.
pub const MAX_PARAGRAPHS: usize = 10_000_000;

/// Maximum total text length across all stories (50 MB).
/// Higher than DEFAULT_MAX_TEXT_LENGTH because DOC stores the entire document
/// in a single contiguous piece table, not per-element.
pub const MAX_TEXT_LENGTH: usize = 50 * 1024 * 1024;

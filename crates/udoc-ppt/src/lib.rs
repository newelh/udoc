#![deny(unsafe_code)]
//! PowerPoint binary (.ppt) extraction backend for the udoc document toolkit.
//!
//! Parses legacy PowerPoint 97-2003 binary files and extracts text, slides,
//! and metadata using udoc-core's format-agnostic types.
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//! use udoc_core::backend::FormatBackend;
//! use udoc_core::diagnostics::NullDiagnostics;
//! use udoc_ppt::PptDocument;
//!
//! let bytes = include_bytes!("../tests/corpus/real-world/with_textbox.ppt");
//! let doc = PptDocument::from_bytes_with_diag(bytes, Arc::new(NullDiagnostics))?;
//! assert!(doc.page_count() >= 1);
//! # Ok::<(), udoc_core::error::Error>(())
//! ```

pub(crate) mod convert;
pub mod document;
pub mod error;
pub(crate) mod persist;
#[cfg(not(feature = "test-internals"))]
pub(crate) mod records;
#[cfg(feature = "test-internals")]
pub mod records;
pub(crate) mod slides;
pub(crate) mod styles;

#[cfg(any(test, feature = "test-internals"))]
pub mod test_util;

pub use convert::ppt_to_document;
pub use document::PptDocument;
pub use error::{Error, Result, ResultExt};

/// Maximum file size for PPT documents (256 MB).
pub const MAX_FILE_SIZE: u64 = udoc_core::limits::DEFAULT_MAX_FILE_SIZE;

/// Maximum number of records to parse before bailing out.
pub const MAX_RECORDS: usize = 1_000_000;

/// Maximum nesting depth for container records.
pub const MAX_RECORD_DEPTH: usize = 64;

/// Maximum number of slides in a presentation.
pub const MAX_SLIDES: usize = 10_000;

/// Maximum total text length across all slides (10 MB).
pub const MAX_TEXT_LENGTH: usize = 10 * 1024 * 1024;

/// Maximum number of UserEditAtom entries in the edit chain.
pub const MAX_EDITS: usize = 1_000;

/// Maximum number of style runs across all text.
pub const MAX_STYLE_RUNS: usize = 100_000;

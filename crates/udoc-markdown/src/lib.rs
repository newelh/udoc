#![deny(unsafe_code)]
//! Markdown text extraction backend for the udoc document toolkit.
//!
//! Parses Markdown documents (CommonMark + GFM subset) and extracts text,
//! tables, and structure using udoc-core's format-agnostic types.
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//! use udoc_core::diagnostics::NullDiagnostics;
//! use udoc_markdown::MdDocument;
//!
//! let bytes = b"# Hello\n\nA paragraph.\n";
//! let doc = MdDocument::from_bytes_with_diag(bytes, Arc::new(NullDiagnostics))?;
//! // The minimal fixture parses cleanly.
//! assert_eq!(doc.warnings().len(), 0);
//! assert!(!doc.blocks().is_empty());
//! # Ok::<(), udoc_core::error::Error>(())
//! ```

/// Maximum file size before rejection (256 MB).
pub const MAX_FILE_SIZE: u64 = udoc_core::limits::DEFAULT_MAX_FILE_SIZE;

pub(crate) mod block;
pub mod document;
pub mod error;
pub(crate) mod inline;
pub(crate) mod table;

// Public API: document type, AST types, and error types.
pub use block::MdBlock;
pub use document::MdDocument;
pub use inline::{parse_inlines, parse_inlines_with_warnings, MdInline};
pub use udoc_core::error::{Error, Result, ResultExt};

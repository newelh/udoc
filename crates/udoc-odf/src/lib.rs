#![deny(unsafe_code)]
//! ODF text extraction backend for the udoc document toolkit.
//!
//! Parses OpenDocument Format files (ODT, ODS, ODP) and extracts text,
//! tables, headings, lists, and metadata using udoc-core's format-agnostic
//! types.
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//! use udoc_core::backend::FormatBackend;
//! use udoc_core::diagnostics::NullDiagnostics;
//! use udoc_odf::OdfDocument;
//!
//! let bytes = include_bytes!("../tests/corpus/real-world/synthetic_basic.odt");
//! let doc = OdfDocument::from_bytes_with_diag(bytes, Arc::new(NullDiagnostics))?;
//! assert!(doc.page_count() >= 1);
//! # Ok::<(), udoc_core::error::Error>(())
//! ```

/// Maximum archive size before rejection (256 MB).
pub const MAX_FILE_SIZE: u64 = udoc_core::limits::DEFAULT_MAX_FILE_SIZE;

mod convert;
pub mod document;
pub mod error;
mod manifest;
mod meta;
mod odp;
mod ods;
mod odt;
mod styles;

pub use convert::odf_to_document;
pub use document::OdfDocument;
pub use error::{Error, Result, ResultExt};

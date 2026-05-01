#![deny(unsafe_code)]

//! XLSX backend for the udoc document extraction toolkit.
//!
//! Provides `XlsxDocument` for opening and extracting content from Excel
//! (.xlsx) files. Implements `FormatBackend` and `PageExtractor` from
//! udoc-core. One sheet = one logical page.
//!
//! # Usage
//!
//! ```ignore
//! use std::sync::Arc;
//! use udoc_core::backend::{FormatBackend, PageExtractor};
//! use udoc_core::diagnostics::NullDiagnostics;
//!
//! // Drive the backend on a real bundled fixture.
//! let bytes = include_bytes!("../tests/corpus/real-world/InlineStrings.xlsx");
//! let mut doc = udoc_xlsx::XlsxDocument::from_bytes_with_diag(bytes, Arc::new(NullDiagnostics))?;
//! for i in 0..doc.page_count() {
//!     let mut page = doc.page(i)?;
//!     let _ = page.text()?;
//! }
//! # Ok::<(), udoc_core::error::Error>(())
//! ```

/// Maximum archive size before rejection (256 MB).
pub const MAX_FILE_SIZE: u64 = udoc_core::limits::DEFAULT_MAX_FILE_SIZE;

mod cell_ref;
mod convert;
mod document;
pub mod error;
mod formats;
mod merge;
mod shared_strings;
mod sheet;
mod styles;
mod workbook;

// Public API: document type, page handle, conversion function, and error types.
pub use convert::xlsx_to_document;
pub use document::{XlsxDocument, XlsxPage};
pub use udoc_core::error::{Error, Result, ResultExt};

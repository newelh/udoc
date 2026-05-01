#![deny(unsafe_code)]

//! XLS (BIFF8) backend for udoc.
//!
//! Extracts text, tables, and metadata from Excel 97-2003 (.xls) files.
//! Uses the CFB (Compound File Binary) container from `udoc-containers`
//! to read the Workbook stream, then parses BIFF8 records with transparent
//! CONTINUE record reassembly.
//!
//! # Example
//!
//! ```ignore
//! use std::sync::Arc;
//! use udoc_core::backend::FormatBackend;
//! use udoc_core::diagnostics::NullDiagnostics;
//! use udoc_xls::XlsDocument;
//!
//! let bytes = include_bytes!("../tests/corpus/real-world/chinese_provinces.xls");
//! let doc = XlsDocument::from_bytes_with_diag(bytes, Arc::new(NullDiagnostics))?;
//! assert!(doc.page_count() >= 1);
//! # Ok::<(), udoc_core::error::Error>(())
//! ```

pub(crate) mod cells;
pub(crate) mod convert;
pub mod document;
pub mod error;
pub(crate) mod formats;
pub(crate) mod records;
pub(crate) mod sst;
#[cfg(any(test, feature = "test-internals"))]
pub mod test_util;
pub(crate) mod workbook;

pub use convert::xls_to_document;
pub use document::XlsDocument;
pub use udoc_core::error::{Error, Result, ResultExt};

/// Maximum file size we will attempt to parse (256 MB).
pub const MAX_FILE_SIZE: u64 = udoc_core::limits::DEFAULT_MAX_FILE_SIZE;

/// Maximum number of BIFF8 records in a Workbook stream.
pub const MAX_RECORDS: usize = 2_000_000;

/// Maximum size of a single logical record after CONTINUE reassembly (4 MB).
pub const MAX_RECORD_SIZE: usize = 4 * 1024 * 1024;

/// Maximum number of entries in the Shared String Table.
pub const MAX_SST_ENTRIES: usize = 1_048_576;

/// Maximum number of sheets in a workbook.
pub const MAX_SHEETS: usize = 256;

/// Maximum number of cells per sheet (65536 rows * 256 cols).
pub const MAX_CELLS_PER_SHEET: usize = 16_777_216;

/// Maximum length of a single string (characters).
pub const MAX_STRING_LENGTH: usize = 32_767;

/// Maximum number of FORMAT records.
pub const MAX_FORMAT_RECORDS: usize = 10_000;

/// Maximum number of XF (Extended Format) records.
pub const MAX_XF_RECORDS: usize = 65_536;

// BIFF8 row/col limits (65536 rows, 256 cols) are inherently enforced
// by the u16 type used in cell record parsing.

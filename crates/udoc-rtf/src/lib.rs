#![deny(unsafe_code)]
//! RTF text extraction backend for the udoc document toolkit.
//!
//! Parses Rich Text Format (RTF) documents and extracts text, tables,
//! and images using udoc-core's format-agnostic types.
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//! use udoc_core::diagnostics::NullDiagnostics;
//! use udoc_rtf::RtfDocument;
//!
//! // Tiny inline RTF: hello world. RTF is plain ASCII so we can hard-code
//! // a runnable fixture without bundling a separate file.
//! let bytes = br"{\rtf1\ansi Hello, world.}";
//! let doc = RtfDocument::from_bytes_with_diag(bytes, Arc::new(NullDiagnostics))?;
//! // RTF is a flow format -- always exactly one logical page.
//! assert_eq!(doc.warnings().len(), 0);
//! # Ok::<(), udoc_core::error::Error>(())
//! ```

/// Maximum file size before rejection (256 MB).
pub const MAX_FILE_SIZE: u64 = udoc_core::limits::DEFAULT_MAX_FILE_SIZE;

pub(crate) mod codepage;
pub(crate) mod convert;
pub mod document;
pub mod error;
pub(crate) mod image;
pub(crate) mod lexer;
pub(crate) mod parser;
pub(crate) mod state;
pub(crate) mod table;

// Public API: document type, error types, and lexer for fuzz targets.
pub use convert::rtf_to_document;
pub use document::RtfDocument;
pub use error::{Error, Result, ResultExt};
pub use lexer::Lexer;

/// Convert an ASCII hex character to its 4-bit value.
/// Used by both the lexer (hex escapes) and parser (image hex data).
pub(crate) fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

//! Parsing layer: lexer, object parser, and document structure parser.
//!
//! This layer operates on byte slices and produces tokens, parsed PDF objects,
//! and document structure (xref tables, trailers). Most users do not need this
//! module directly; the [`Document`](crate::Document) API uses it internally.
//!
//! Use these types if you need low-level access to PDF parsing (e.g., building
//! custom PDF tools or inspecting raw object structure).

pub mod document_parser;
mod lexer;
pub mod object_parser;

// Re-exported for external consumers when test-internals is enabled (fuzz targets, integration
// tests). Without that feature, the modules are pub(crate) and these re-exports are unused.
#[cfg(any(test, feature = "test-internals"))]
pub use document_parser::DocumentParser;
pub use document_parser::{DocumentStructure, PdfVersion, XrefEntry, XrefTable};
#[cfg(any(test, feature = "test-internals"))]
pub use lexer::LexError;
pub use lexer::{Lexer, Token};

#![deny(unsafe_code)]

//! Format-agnostic document extraction types for the udoc toolkit.
//!
//! This crate defines the shared output types and traits used by all
//! format backends (PDF, DOCX, XLSX, etc.) in the udoc toolkit.
//! Its only runtime dependency is `encoding_rs` for codepage decoding.
//!
//! # Core types
//!
//! - [`geometry::BoundingBox`] -- axis-aligned bounding rectangle
//! - [`text::TextSpan`], [`text::TextLine`] -- positioned text output
//! - [`table::Table`], [`table::TableRow`], [`table::TableCell`] -- table output
//! - [`image::PageImage`], [`image::ImageFilter`] -- image output
//! - [`error::Error`], [`error::Result`] -- error handling
//! - [`diagnostics::DiagnosticsSink`] -- warning/info sink
//!
//! # Document model
//!
//! The [`document`] module provides the unified document model: a five-layer
//! spine+overlay architecture for representing extracted documents from any
//! format (PDF, DOCX, XLSX, PPTX, etc.).
//!
//! - [`document::Document`] -- top-level document with content spine and overlays
//! - [`document::Block`], [`document::Inline`] -- content spine types
//! - [`document::NodeId`] -- stable node identifier
//! - [`document::Overlay`], [`document::SparseOverlay`] -- per-node data stores

pub mod backend;
pub mod codepage;
/// Internal helpers used by every backend crate to convert format-native
/// types into the shared `Document` model. Doc-hidden because the helper
/// API is shaped around backend-author needs, not downstream library
/// consumers.
#[doc(hidden)]
pub mod convert;
pub mod diagnostics;
pub mod document;
pub mod error;
pub mod geometry;
pub mod image;
/// Internal I/O helpers (size-bounded readers, etc.) shared by backend
/// crates. Doc-hidden.
#[doc(hidden)]
pub mod io;
pub mod limits;
/// Internal metrics primitives used by `bench-compare` and the renderer
/// pipeline. Doc-hidden.
#[doc(hidden)]
pub mod metrics;
pub mod table;
pub mod text;

/// Maximum nesting depth for recursive document structures (XML elements,
/// RTF groups, Markdown blocks, document model nodes, etc.). Used across
/// all backends to prevent stack overflow on pathological input.
pub const MAX_NESTING_DEPTH: usize = 256;

// Used by backend integration tests (not fuzz)
#[cfg(feature = "test-internals")]
pub mod test_harness;

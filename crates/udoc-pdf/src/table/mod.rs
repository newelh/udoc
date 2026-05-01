//! Table extraction from PDF pages.
//!
//! Detects tables by analyzing ruled lines (drawn paths forming grids)
//! and text alignment patterns. Supports column/row spans, header
//! detection, and multi-page table continuation hints.
//!
//! # Architecture
//!
//! - `types`: Public types (`Table`, `TableRow`, `TableCell`, etc.)
//! - `detect`: Ruled-line (lattice) and h-line table detection
//! - `detect_text`: Text-alignment table detection (borderless fallback)
//! - `extract`: Cell text filling, header detection, quality filtering
//! - `span_merge`: Word-level span merging for table text
//! - `text_edge`: Nurminen text-edge column detection

pub(crate) mod detect;
pub(crate) mod detect_text;
pub(crate) mod extract;
pub(crate) mod span_merge;
pub(crate) mod text_edge;
pub(crate) mod types;

// Public API: pipeline entry point and types.
pub use extract::extract_tables;
pub use types::{
    ClipPathIR, FillRule, PathSegment, PathSegmentKind, Table, TableCell, TableDetectionMethod,
    TableRow,
};

// Internal: individual detectors and post-processing.
// Exposed for integration testing and fuzz targets; not part of the stable public API.
// Gated behind cfg(test) or the `test-internals` feature flag.
// Used by fuzz_table_detection
#[cfg(any(test, feature = "test-internals"))]
pub use detect::{detect_hline_tables, detect_tables};
// Used by fuzz_table_detection
#[cfg(any(test, feature = "test-internals"))]
pub use detect_text::detect_text_tables;
// Used by fuzz_table_detection
#[cfg(any(test, feature = "test-internals"))]
pub use extract::fill_table_text;
// Used by table_golden integration tests (not fuzz)
#[cfg(any(test, feature = "test-internals"))]
pub use extract::detect_header_rows;

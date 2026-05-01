//! PPTX error types -- re-exports from udoc-core.
//!
//! Also provides PPTX-specific error helpers for missing parts.

pub use udoc_core::error::{Error, Result, ResultExt};

/// Create an error for a missing required PPTX part.
pub(crate) fn missing_part(part_name: &str) -> Error {
    Error::new(format!("missing required PPTX part: {part_name}"))
}

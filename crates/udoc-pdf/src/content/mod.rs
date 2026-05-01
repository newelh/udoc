//! Content stream interpreter for PDF text extraction.
//!
//! Interprets PDF content streams (the drawing instructions inside pages)
//! to extract text with position and font information. Produces TextSpans
//! that can be ordered into lines and pages.
//!
//! This module is internal. Use the [`Document`](crate::Document) and
//! [`Page`](crate::Page) API for text extraction.

/// Per-page (font_id, glyph_code) -> decoded text LRU.
pub(crate) mod decode_cache;
pub mod interpreter;
pub(crate) mod marked_content;
/// Path IR types for renderer consumption. Canonical
/// moveto/lineto/curveto/closepath segments with a CTM snapshot and
/// stroke style captured at paint time.
pub mod path;
/// Page `/Resources` sub-dict extraction helpers (
/// ). Currently scoped to `/Pattern`; other resource classes
/// live inline inside `interpreter.rs` for historical reasons.
pub(crate) mod resource;
/// Shading-pattern dictionary parsing + function sampling (ISO 32000-2
/// §8.7.4,).
pub(crate) mod shading;

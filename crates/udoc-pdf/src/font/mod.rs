//! PDF-specific font loading.
//!
//! Format-agnostic font parsing (CFF, Type1, TrueType, CMap, encoding,
//! hinting) lives in the `udoc-font` crate. This module holds the bits
//! that can't move: the font dictionary loader (which uses the PDF
//! `ObjectResolver`) and the Type3 PDF-ref wrapper.

pub mod loader;
pub mod type3_pdf;

pub(crate) use loader::load_font;

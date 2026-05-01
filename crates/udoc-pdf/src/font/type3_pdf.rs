//! PDF-specific refs that accompany a Type3 font.
//!
//! `Type3FontCore` in `udoc-font` holds the format-agnostic Type3 metadata
//! (encoding, widths, font matrix, glyph names). The CharProc stream
//! references and /Resources dict ref are inherently PDF-shaped (they're
//! `ObjRef`s into the document) so they live here, paired with the core
//! via `ObjRef` keys in the interpreter's side map.

use std::collections::HashMap;

use crate::object::ObjRef;

/// PDF-side Type3 font data: CharProc stream references and /Resources
/// dict ref. The format-agnostic core lives in
/// `udoc_font::types::Type3FontCore` and is stored on the `Font::Type3`
/// enum variant alongside other fonts.
#[derive(Debug, Clone, Default)]
pub struct Type3FontPdfRefs {
    /// Glyph name -> CharProc stream reference.
    /// Used for CharProc text extraction.
    pub char_procs: HashMap<String, ObjRef>,
    /// /Resources dict ref for CharProc stream interpretation.
    /// CharProc interpretation uses these resources instead of page resources.
    pub resources_ref: Option<ObjRef>,
}

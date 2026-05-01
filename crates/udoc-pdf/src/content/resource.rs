//! Page `/Resources /Pattern` sub-dict extraction for the content
//! interpreter.
//!
//! PDF pages carry a `/Resources` dict with named sub-dicts for each
//! resource class (`/Font`, `/XObject`, `/ExtGState`, `/Pattern`,
//! `/Shading`, `/ColorSpace`, `/Properties`). The interpreter already
//! has extractors for fonts, xobjects, extgstates, shadings, and
//! colorspaces; this one closes the gap for patterns.
//!
//! The returned map keys are resource names (`"P1"`, `"Pat0"`) and
//! values are the raw [`PdfObject`] as they appear in the sub-dict,
//! unresolved. The pattern parser
//! ([`crate::pattern::parse_tiling_pattern`]) resolves references on
//! demand.
//!
//! Analogous to `extract_shading_resources` in
//! `crate::content::interpreter`: both sub-dicts can hold indirect
//! references or inline streams, so we defer resolution.

use std::collections::HashMap;

use crate::object::resolver::ObjectResolver;
use crate::object::{PdfDictionary, PdfObject};

/// Collect `/Resources/Pattern` entries from a page resources dict.
///
/// Keys are UTF-8 lossy-decoded pattern names (typically ASCII in
/// real PDFs). Values are the raw [`PdfObject`] referents; callers
/// that need the dict form should call
/// [`ObjectResolver::resolve`](ObjectResolver::resolve) on the stored
/// value.
///
/// Returns an empty map when the page has no `/Pattern` sub-dict or
/// when it fails to resolve. Errors are swallowed silently because
/// pattern resources are optional -- a page without patterns is
/// valid and should not produce a diagnostic.
///
/// Consumed by when the content interpreter
/// hits an `scn` op in Pattern colorspace.
pub fn extract_pattern_resources(
    page_resources: &PdfDictionary,
    resolver: &mut ObjectResolver<'_>,
) -> HashMap<String, PdfObject> {
    let mut map = HashMap::new();
    let Ok(Some(sub)) = resolver.get_resolved_dict(page_resources, b"Pattern") else {
        return map;
    };
    for (name, value) in sub.iter() {
        let name_str = String::from_utf8_lossy(name).into_owned();
        map.insert(name_str, value.clone());
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{ObjRef, PdfDictionary, PdfObject};
    use crate::parse::document_parser::XrefTable;

    #[test]
    fn empty_when_no_pattern_subdict() {
        let dict = PdfDictionary::new();
        let mut r = ObjectResolver::new(&[], XrefTable::new());
        let out = extract_pattern_resources(&dict, &mut r);
        assert!(out.is_empty());
    }

    #[test]
    fn collects_inline_pattern_refs() {
        let mut inner = PdfDictionary::new();
        inner.insert(b"P1".to_vec(), PdfObject::Reference(ObjRef::new(10, 0)));
        inner.insert(b"P2".to_vec(), PdfObject::Reference(ObjRef::new(11, 0)));
        let mut page_res = PdfDictionary::new();
        page_res.insert(b"Pattern".to_vec(), PdfObject::Dictionary(inner));

        let mut r = ObjectResolver::new(&[], XrefTable::new());
        let out = extract_pattern_resources(&page_res, &mut r);
        assert_eq!(out.len(), 2);
        assert!(out.contains_key("P1"));
        assert!(out.contains_key("P2"));
        match out.get("P1").unwrap() {
            PdfObject::Reference(r) => assert_eq!(*r, ObjRef::new(10, 0)),
            other => panic!("expected Reference, got {other:?}"),
        }
    }
}

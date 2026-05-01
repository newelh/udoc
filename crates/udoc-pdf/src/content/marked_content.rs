//! Structure tree parsing and MCID-based reading order for tagged PDFs.
//!
//! Tagged PDFs carry a `/StructTreeRoot` in the document catalog that
//! defines the logical structure (paragraphs, headings, tables, etc.).
//! Each structure element can reference marked content IDs (MCIDs) that
//! link back to content on specific pages.
//!
//! This module parses the structure tree and provides MCID ordering that
//! the reading order module can use to reorder spans according to the
//! document's intended logical sequence rather than content stream order.

use crate::object::resolver::ObjectResolver;
use crate::object::{ObjRef, PdfDictionary, PdfObject};
use crate::text_decode::decode_pdf_text_bytes;

/// Maximum recursion depth when traversing the structure tree.
/// Prevents stack overflow from deeply nested or circular structures.
const MAX_STRUCT_DEPTH: usize = 100;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// A node in the PDF structure tree.
#[derive(Debug, Clone)]
pub(crate) struct StructElement {
    /// Structure type (e.g. "P", "Span", "Table", "H1").
    #[allow(dead_code)] // used in tests; reserved for semantic structure export
    pub struct_type: String,
    /// Children: either sub-elements or MCIDs.
    pub children: Vec<StructChild>,
    /// Page reference (if this element is associated with a specific page).
    pub page_ref: Option<ObjRef>,
    /// Alternative text from the /Alt entry (accessibility description).
    /// Common on Figure elements; provides a textual description of images.
    pub alt_text: Option<String>,
}

/// A child of a structure element.
#[derive(Debug, Clone)]
pub(crate) enum StructChild {
    /// A sub-element in the structure tree.
    Element(StructElement),
    /// A marked content ID referencing content on a page.
    Mcid(u32),
    /// An object reference to a sub-element (to be resolved later).
    /// The ObjRef is retained for diagnostics; the traversal skips these.
    ObjReference(#[allow(dead_code)] ObjRef), // unresolved struct tree ref; kept for completeness
}

/// Parsed structure tree for a document.
#[derive(Debug)]
pub(crate) struct StructureTree {
    /// Root elements.
    pub root_elements: Vec<StructElement>,
}

/// Ordering information extracted from the structure tree for a single page.
#[derive(Debug)]
pub(crate) struct PageStructureOrder {
    /// MCIDs in document-logical order for this page.
    pub mcid_order: Vec<u32>,
}

/// Trait for items that may carry a marked content ID.
pub(crate) trait HasMcid {
    fn mcid(&self) -> Option<u32>;
}

// ---------------------------------------------------------------------------
// Structure tree parsing
// ---------------------------------------------------------------------------

/// Parse the document's structure tree from the catalog dictionary.
///
/// Returns `None` if the catalog has no `/StructTreeRoot`, if the tree root
/// is malformed, or if parsing fails. This is best-effort: a missing or
/// broken structure tree just means we fall back to geometric ordering.
pub(crate) fn parse_structure_tree(
    resolver: &mut ObjectResolver,
    catalog: &PdfDictionary,
) -> Option<StructureTree> {
    // Look up /StructTreeRoot, resolving indirect references.
    let tree_root_obj = match catalog.get(b"StructTreeRoot") {
        Some(PdfObject::Reference(r)) => resolver.resolve(*r).ok()?,
        Some(obj) => obj.clone(),
        None => return None,
    };

    let tree_root = tree_root_obj.as_dict()?;

    // /K can be a single element, an array, or an integer MCID.
    let kids_obj = match tree_root.get(b"K") {
        Some(PdfObject::Reference(r)) => resolver.resolve(*r).ok()?,
        Some(obj) => obj.clone(),
        None => {
            return Some(StructureTree {
                root_elements: Vec::new(),
            })
        }
    };

    let root_elements = parse_kids(resolver, &kids_obj, 0);

    Some(StructureTree { root_elements })
}

/// Parse the /K value of a structure element into a list of children.
///
/// /K can be:
/// - An integer: a single MCID
/// - A dictionary: a single child element
/// - An array: mix of integers, dicts, and references
/// - A reference to any of the above
fn parse_kids(
    resolver: &mut ObjectResolver,
    kids_obj: &PdfObject,
    depth: usize,
) -> Vec<StructElement> {
    if depth >= MAX_STRUCT_DEPTH {
        return Vec::new();
    }

    match kids_obj {
        PdfObject::Dictionary(_) => {
            // Single child element
            parse_struct_element(resolver, kids_obj, depth)
                .into_iter()
                .collect()
        }
        PdfObject::Array(arr) => arr
            .iter()
            .filter_map(|item| {
                let resolved = match item {
                    PdfObject::Reference(r) => resolver.resolve(*r).ok(),
                    other => Some(other.clone()),
                };
                resolved.and_then(|obj| parse_struct_element(resolver, &obj, depth))
            })
            .collect(),
        // Single MCID at root level: wrap in a synthetic element.
        PdfObject::Integer(n) => {
            if *n >= 0 {
                vec![StructElement {
                    struct_type: String::new(),
                    children: vec![StructChild::Mcid(*n as u32)],
                    page_ref: None,
                    alt_text: None,
                }]
            } else {
                Vec::new()
            }
        }
        // Reference: resolve and recurse (count against depth budget).
        PdfObject::Reference(r) => match resolver.resolve(*r).ok() {
            Some(obj) => parse_kids(resolver, &obj, depth + 1),
            None => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// Parse a single structure element from a PdfObject.
///
/// Expects a dictionary with at least /S (structure type). /K (kids) and
/// /Pg (page reference) are optional.
fn parse_struct_element(
    resolver: &mut ObjectResolver,
    obj: &PdfObject,
    depth: usize,
) -> Option<StructElement> {
    if depth >= MAX_STRUCT_DEPTH {
        return None;
    }

    let dict = obj.as_dict()?;

    // /S is the structure type name. Missing /S means this isn't a real
    // structure element (could be a marked-content reference dict).
    let struct_type = match dict.get(b"S") {
        Some(PdfObject::Name(name)) => String::from_utf8_lossy(name).into_owned(),
        _ => {
            // No /S: might be a marked-content reference dict with /MCID.
            // These appear as direct children in /K arrays.
            if let Some(PdfObject::Integer(mcid)) = dict.get(b"MCID") {
                if *mcid >= 0 {
                    // This is a marked-content reference, not a structure element.
                    // Return a synthetic element wrapping the MCID. The page ref
                    // comes from /Pg on this dict.
                    let page_ref = extract_page_ref(dict);
                    return Some(StructElement {
                        struct_type: String::new(),
                        children: vec![StructChild::Mcid(*mcid as u32)],
                        page_ref,
                        alt_text: None,
                    });
                }
            }
            return None;
        }
    };

    let page_ref = extract_page_ref(dict);

    // Extract /Alt text (accessibility alternative text, common on Figure elements).
    let alt_text = extract_alt_text(dict);

    // Parse children from /K.
    let children = match dict.get(b"K") {
        Some(PdfObject::Reference(r)) => match resolver.resolve(*r).ok() {
            Some(resolved) => parse_children(resolver, &resolved, depth + 1),
            None => Vec::new(),
        },
        Some(obj) => parse_children(resolver, obj, depth + 1),
        None => Vec::new(),
    };

    Some(StructElement {
        struct_type,
        children,
        page_ref,
        alt_text,
    })
}

/// Extract an ObjRef from the /Pg entry of a structure element dict.
fn extract_page_ref(dict: &PdfDictionary) -> Option<ObjRef> {
    match dict.get(b"Pg") {
        Some(PdfObject::Reference(r)) => Some(*r),
        // /Pg should always be an indirect reference. Non-standard values
        // are silently ignored (fallback to no page association).
        _ => None,
    }
}

/// Extract alternative text from the /Alt entry of a structure element dict.
///
/// /Alt is a PDF text string (UTF-16BE or PDFDocEncoding) providing an
/// accessibility description. Most commonly found on Figure elements.
/// Returns None if /Alt is absent or not a string.
fn extract_alt_text(dict: &PdfDictionary) -> Option<String> {
    match dict.get(b"Alt") {
        Some(PdfObject::String(s)) => {
            let text = decode_pdf_text_bytes(s.as_bytes());
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        }
        _ => None,
    }
}

/// Parse /K value into a list of StructChild entries.
///
/// This handles the three forms /K can take:
/// - Integer: single MCID
/// - Dictionary: single child (either MCRef dict or struct element)
/// - Array: mix of the above
fn parse_children(
    resolver: &mut ObjectResolver,
    obj: &PdfObject,
    depth: usize,
) -> Vec<StructChild> {
    if depth >= MAX_STRUCT_DEPTH {
        return Vec::new();
    }

    match obj {
        PdfObject::Integer(n) => {
            // Single MCID
            if *n >= 0 {
                vec![StructChild::Mcid(*n as u32)]
            } else {
                Vec::new()
            }
        }
        PdfObject::Dictionary(_) => parse_child_dict(resolver, obj, depth),
        PdfObject::Array(arr) => arr
            .iter()
            .flat_map(|item| match item {
                PdfObject::Integer(n) if *n >= 0 => {
                    vec![StructChild::Mcid(*n as u32)]
                }
                PdfObject::Reference(r) => match resolver.resolve(*r).ok() {
                    Some(resolved) => parse_child_dict(resolver, &resolved, depth + 1),
                    None => vec![StructChild::ObjReference(*r)],
                },
                PdfObject::Dictionary(_) => parse_child_dict(resolver, item, depth),
                _ => Vec::new(),
            })
            .collect(),
        PdfObject::Reference(r) => match resolver.resolve(*r).ok() {
            Some(resolved) => parse_children(resolver, &resolved, depth + 1),
            None => vec![StructChild::ObjReference(*r)],
        },
        _ => Vec::new(),
    }
}

/// Parse a single dictionary child, which can be either a marked-content
/// reference (has /MCID but no /S) or a structure element (has /S).
fn parse_child_dict(
    resolver: &mut ObjectResolver,
    obj: &PdfObject,
    depth: usize,
) -> Vec<StructChild> {
    let dict = match obj.as_dict() {
        Some(d) => d,
        None => return Vec::new(),
    };

    // Check if this is a marked-content reference dict (has /MCID, no /S).
    if let Some(PdfObject::Integer(mcid)) = dict.get(b"MCID") {
        if *mcid >= 0 {
            return vec![StructChild::Mcid(*mcid as u32)];
        }
    }

    // Otherwise try to parse as a structure element.
    match parse_struct_element(resolver, obj, depth) {
        Some(elem) => vec![StructChild::Element(elem)],
        None => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Page ordering
// ---------------------------------------------------------------------------

/// Extract the document-logical MCID order for a specific page.
///
/// Does a depth-first traversal of the structure tree and collects MCIDs
/// belonging to the given page. MCIDs appear in the order they are
/// encountered in the tree, which represents the document's intended
/// logical reading sequence.
///
/// Returns `None` if no MCIDs are found for the page.
pub(crate) fn get_page_structure_order(
    tree: &StructureTree,
    page_ref: ObjRef,
) -> Option<PageStructureOrder> {
    let mut mcid_order = Vec::new();
    for elem in &tree.root_elements {
        collect_mcids_for_page(elem, page_ref, &mut mcid_order, 0);
    }

    if mcid_order.is_empty() {
        None
    } else {
        Some(PageStructureOrder { mcid_order })
    }
}

/// Recursively collect MCIDs for a page from a structure element (DFS).
///
/// `depth` guards against stack overflow from deeply nested trees that
/// somehow survived the parse-phase depth limit (defense-in-depth).
fn collect_mcids_for_page(
    elem: &StructElement,
    page_ref: ObjRef,
    out: &mut Vec<u32>,
    depth: usize,
) {
    if depth >= MAX_STRUCT_DEPTH {
        return;
    }

    // Check if this element is associated with our target page.
    // An element matches if its page_ref matches, or if it has no page_ref
    // (inherited from parent or applies to all pages).
    //
    // page_ref=None matching any page is intentional: high-level struct
    // elements (Document, Part) often lack /Pg. Their direct MCID children
    // are rare (MCIDs are typically on leaf elements that DO have /Pg),
    // but when present they should be included. For child Elements, the
    // recursion checks each child's own page_ref independently.
    let page_matches = elem.page_ref.is_none() || elem.page_ref == Some(page_ref);

    for child in &elem.children {
        match child {
            StructChild::Mcid(mcid) => {
                if page_matches {
                    out.push(*mcid);
                }
            }
            StructChild::Element(child_elem) => {
                collect_mcids_for_page(child_elem, page_ref, out, depth + 1);
            }
            StructChild::ObjReference(_) => {
                // Unresolved reference. We can't follow it without a resolver,
                // so skip. This shouldn't happen if parse_structure_tree resolved
                // everything it could.
            }
        }
    }
}

/// Collect alt texts from structure elements for a specific page.
///
/// Returns a map from MCID to alt text string. Only structure elements that
/// have both an /Alt entry and MCIDs associated with the target page are
/// included. Alt text propagates from parent to child: if a Figure element
/// has /Alt and contains MCIDs, each MCID inherits the Figure's alt text.
///
/// Prefer [`AltTextIndex::build`] + [`AltTextIndex::alt_texts_for_page`]
/// when looking up alt texts for many pages: the index walks the structure
/// tree once at Document open, not once per page (#150). This per-page
/// walker is retained as the baseline referenced by regression tests.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn get_page_alt_texts(
    tree: &StructureTree,
    page_ref: ObjRef,
) -> std::collections::HashMap<u32, String> {
    let mut result = std::collections::HashMap::new();
    for elem in &tree.root_elements {
        collect_alt_texts_for_page(elem, page_ref, None, &mut result, 0);
    }
    result
}

/// Pre-built per-page alt-text index over a structure tree.
///
/// The structure tree walk in [`get_page_alt_texts`] is O(tree size) per
/// page; for an N-page document that becomes O(N * tree size). [`Self::build`]
/// walks the tree once and partitions MCIDs into per-page buckets so each
/// page lookup is O(1) hash + clone (#150). Elements whose structure element
/// carries no `/Pg` reference attribute their MCIDs to every page, matching
/// the optimistic-attribution behaviour of `get_page_alt_texts`.
#[derive(Debug, Default)]
pub(crate) struct AltTextIndex {
    /// Alt texts for MCIDs whose ancestor structure element named a
    /// specific page ref.
    per_page: std::collections::HashMap<ObjRef, std::collections::HashMap<u32, String>>,
    /// Alt texts for MCIDs whose ancestor structure element had no `/Pg`
    /// reference. These propagate to every page (lossy pass-through for
    /// non-conformant PDFs, but matches the legacy walker's behaviour).
    unscoped: std::collections::HashMap<u32, String>,
}

impl AltTextIndex {
    /// Build the index by walking the structure tree once. Returns an
    /// empty index if `tree` is `None`.
    pub fn build(tree: Option<&StructureTree>) -> Self {
        let mut idx = AltTextIndex::default();
        if let Some(tree) = tree {
            for elem in &tree.root_elements {
                index_element(elem, None, None, &mut idx, 0);
            }
        }
        idx
    }

    /// Alt texts for a single page: per-page entries merged with any
    /// unscoped entries. Returns an owned map to match the legacy
    /// `get_page_alt_texts` signature used by the image annotator.
    pub fn alt_texts_for_page(&self, page_ref: ObjRef) -> std::collections::HashMap<u32, String> {
        let per_page = self.per_page.get(&page_ref);
        if per_page.is_none() && self.unscoped.is_empty() {
            return std::collections::HashMap::new();
        }
        let mut out = self.unscoped.clone();
        if let Some(entries) = per_page {
            // Per-page entries override unscoped when MCIDs collide; the
            // page-specific attribution is more trustworthy than the
            // fallback "attribute to every page" behaviour.
            for (mcid, alt) in entries {
                out.insert(*mcid, alt.clone());
            }
        }
        out
    }
}

/// Recursive walker used by [`AltTextIndex::build`]. Mirrors
/// [`collect_alt_texts_for_page`] but routes MCIDs to either the
/// per-page bucket for their ancestor structure element's `/Pg` or to
/// the `unscoped` bucket when no `/Pg` is present in the ancestor chain.
fn index_element(
    elem: &StructElement,
    scoped_page: Option<ObjRef>,
    inherited_alt: Option<&str>,
    out: &mut AltTextIndex,
    depth: usize,
) {
    if depth >= MAX_STRUCT_DEPTH {
        return;
    }

    let effective_alt = elem.alt_text.as_deref().or(inherited_alt);
    let effective_page = elem.page_ref.or(scoped_page);

    for child in &elem.children {
        match child {
            StructChild::Mcid(mcid) => {
                if let Some(alt) = effective_alt {
                    match effective_page {
                        Some(page) => {
                            out.per_page
                                .entry(page)
                                .or_default()
                                .insert(*mcid, alt.to_string());
                        }
                        None => {
                            out.unscoped.insert(*mcid, alt.to_string());
                        }
                    }
                }
            }
            StructChild::Element(child_elem) => {
                index_element(child_elem, effective_page, effective_alt, out, depth + 1);
            }
            StructChild::ObjReference(_) => {}
        }
    }
}

/// Recursively collect alt texts for MCIDs on a given page.
///
/// `inherited_alt` carries the nearest ancestor's alt text down to MCIDs.
/// A child element's own /Alt overrides the inherited value.
#[cfg_attr(not(test), allow(dead_code))]
fn collect_alt_texts_for_page(
    elem: &StructElement,
    page_ref: ObjRef,
    inherited_alt: Option<&str>,
    out: &mut std::collections::HashMap<u32, String>,
    depth: usize,
) {
    if depth >= MAX_STRUCT_DEPTH {
        return;
    }

    // Use this element's alt_text if present, otherwise inherit from parent.
    let effective_alt = elem.alt_text.as_deref().or(inherited_alt);

    // When page_ref is None the element has no page association in the struct
    // tree. We optimistically attribute its MCIDs to the current page. This can
    // over-attribute if MCIDs collide across pages (non-conformant but possible),
    // but omitting them would lose alt text for legitimate single-page documents
    // that simply omit /Pg keys.
    let page_matches = elem.page_ref.is_none() || elem.page_ref == Some(page_ref);

    for child in &elem.children {
        match child {
            StructChild::Mcid(mcid) => {
                if page_matches {
                    if let Some(alt) = effective_alt {
                        out.insert(*mcid, alt.to_string());
                    }
                }
            }
            StructChild::Element(child_elem) => {
                collect_alt_texts_for_page(child_elem, page_ref, effective_alt, out, depth + 1);
            }
            StructChild::ObjReference(_) => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Reordering
// ---------------------------------------------------------------------------

/// Reorder items by structure tree MCID order.
///
/// Items with MCIDs present in the structure order are placed first, in the
/// order defined by the structure tree. Items without MCIDs (or with MCIDs
/// not in the structure order) retain their relative position and are
/// appended after the structured items.
///
/// Takes `&mut Vec<T>` (not `&mut [T]`) because we drain the vec via
/// `mem::take`, partition into two groups, then reassemble. An in-place
/// slice sort can't handle the drain-and-rebuild strategy.
pub(crate) fn reorder_by_structure<T: HasMcid>(items: &mut Vec<T>, order: &PageStructureOrder) {
    if items.is_empty() || order.mcid_order.is_empty() {
        return;
    }

    // Build a position map: MCID -> index in structure order.
    let position_map: std::collections::HashMap<u32, usize> = order
        .mcid_order
        .iter()
        .enumerate()
        .map(|(i, &mcid)| (mcid, i))
        .collect();

    // Partition into structured (has MCID in order) and unstructured.
    let taken = std::mem::take(items);
    let mut structured: Vec<(usize, T)> = Vec::new();
    let mut unstructured: Vec<T> = Vec::new();

    for item in taken {
        match item.mcid().and_then(|m| position_map.get(&m).copied()) {
            Some(pos) => structured.push((pos, item)),
            None => unstructured.push(item),
        }
    }

    // Sort structured items by their position in the structure tree.
    structured.sort_by_key(|(pos, _)| *pos);

    // Reassemble: structured first, unstructured after.
    items.reserve(structured.len() + unstructured.len());
    items.extend(structured.into_iter().map(|(_, item)| item));
    items.extend(unstructured);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: build a StructElement
    fn elem(
        struct_type: &str,
        children: Vec<StructChild>,
        page_ref: Option<ObjRef>,
    ) -> StructElement {
        StructElement {
            struct_type: struct_type.to_string(),
            children,
            page_ref,
            alt_text: None,
        }
    }

    // Helper: a test item that implements HasMcid
    #[derive(Debug, Clone, PartialEq)]
    struct TestItem {
        label: String,
        mcid: Option<u32>,
    }

    impl TestItem {
        fn new(label: &str, mcid: Option<u32>) -> Self {
            Self {
                label: label.to_string(),
                mcid,
            }
        }
    }

    impl HasMcid for TestItem {
        fn mcid(&self) -> Option<u32> {
            self.mcid
        }
    }

    // -- parse_structure_tree tests require ObjectResolver, tested via integration.
    // Unit tests focus on data structure manipulation. --

    #[test]
    fn simple_structure_tree_one_level() {
        // Tree: Document -> [P(mcid=0), P(mcid=1)]
        let page = ObjRef::new(1, 0);
        let tree = StructureTree {
            root_elements: vec![elem(
                "Document",
                vec![
                    StructChild::Element(elem("P", vec![StructChild::Mcid(0)], Some(page))),
                    StructChild::Element(elem("P", vec![StructChild::Mcid(1)], Some(page))),
                ],
                None,
            )],
        };

        let order = get_page_structure_order(&tree, page);
        assert!(order.is_some());
        assert_eq!(order.as_ref().map(|o| &o.mcid_order), Some(&vec![0, 1]));
    }

    #[test]
    fn nested_structure_tree() {
        // Tree: Document -> P -> [Span(mcid=0), Span(mcid=1)]
        let page = ObjRef::new(1, 0);
        let tree = StructureTree {
            root_elements: vec![elem(
                "Document",
                vec![StructChild::Element(elem(
                    "P",
                    vec![
                        StructChild::Element(elem("Span", vec![StructChild::Mcid(0)], Some(page))),
                        StructChild::Element(elem("Span", vec![StructChild::Mcid(1)], Some(page))),
                    ],
                    Some(page),
                ))],
                None,
            )],
        };

        let order = get_page_structure_order(&tree, page);
        assert_eq!(order.as_ref().map(|o| &o.mcid_order), Some(&vec![0, 1]));
    }

    #[test]
    fn k_as_integer_single_mcid() {
        // A structure element with /K as a plain integer (single MCID).
        let page = ObjRef::new(1, 0);
        let tree = StructureTree {
            root_elements: vec![elem("P", vec![StructChild::Mcid(42)], Some(page))],
        };

        let order = get_page_structure_order(&tree, page);
        assert_eq!(order.as_ref().map(|o| &o.mcid_order), Some(&vec![42]));
    }

    #[test]
    fn k_as_array_multiple_children() {
        // /K as array with mixed MCIDs and child elements.
        let page = ObjRef::new(2, 0);
        let tree = StructureTree {
            root_elements: vec![elem(
                "P",
                vec![
                    StructChild::Mcid(0),
                    StructChild::Element(elem("Span", vec![StructChild::Mcid(1)], Some(page))),
                    StructChild::Mcid(2),
                ],
                Some(page),
            )],
        };

        let order = get_page_structure_order(&tree, page);
        assert_eq!(order.as_ref().map(|o| &o.mcid_order), Some(&vec![0, 1, 2]));
    }

    #[test]
    fn k_as_single_dict_element() {
        // /K is a single dict (not an array). parse_kids wraps this case.
        let page = ObjRef::new(1, 0);
        let tree = StructureTree {
            root_elements: vec![elem(
                "Document",
                vec![StructChild::Element(elem(
                    "P",
                    vec![StructChild::Mcid(5)],
                    Some(page),
                ))],
                None,
            )],
        };

        let order = get_page_structure_order(&tree, page);
        assert_eq!(order.as_ref().map(|o| &o.mcid_order), Some(&vec![5]));
    }

    #[test]
    fn depth_limit_enforcement() {
        // Build a deeply nested tree beyond MAX_STRUCT_DEPTH.
        // Both parse and DFS traversal bail out at the limit.
        let page = ObjRef::new(1, 0);

        // Build bottom-up: innermost has an MCID, outer layers just wrap.
        let mut current = elem("Span", vec![StructChild::Mcid(99)], Some(page));
        for _ in 0..MAX_STRUCT_DEPTH + 10 {
            current = elem("Div", vec![StructChild::Element(current)], None);
        }

        let tree = StructureTree {
            root_elements: vec![current],
        };

        // DFS traversal now enforces MAX_STRUCT_DEPTH, so the deeply
        // nested MCID is unreachable. No stack overflow either way.
        let order = get_page_structure_order(&tree, page);
        assert!(order.is_none());
    }

    #[test]
    fn get_page_structure_order_dfs_ordering() {
        // Verify that DFS traversal produces the correct MCID order.
        //
        // Tree structure:
        //   Document
        //     P (mcid=2)
        //     Table
        //       TR
        //         TD (mcid=0)
        //         TD (mcid=3)
        //     P (mcid=1)
        //
        // DFS order: 2, 0, 3, 1
        let page = ObjRef::new(1, 0);
        let tree = StructureTree {
            root_elements: vec![elem(
                "Document",
                vec![
                    StructChild::Element(elem("P", vec![StructChild::Mcid(2)], Some(page))),
                    StructChild::Element(elem(
                        "Table",
                        vec![StructChild::Element(elem(
                            "TR",
                            vec![
                                StructChild::Element(elem(
                                    "TD",
                                    vec![StructChild::Mcid(0)],
                                    Some(page),
                                )),
                                StructChild::Element(elem(
                                    "TD",
                                    vec![StructChild::Mcid(3)],
                                    Some(page),
                                )),
                            ],
                            None,
                        ))],
                        None,
                    )),
                    StructChild::Element(elem("P", vec![StructChild::Mcid(1)], Some(page))),
                ],
                None,
            )],
        };

        let order = get_page_structure_order(&tree, page).expect("should have order");
        assert_eq!(order.mcid_order, vec![2, 0, 3, 1]);
    }

    #[test]
    fn reorder_by_structure_mixed_items() {
        let order = PageStructureOrder {
            mcid_order: vec![2, 0, 1],
        };

        let mut items = vec![
            TestItem::new("a", Some(0)),
            TestItem::new("b", Some(1)),
            TestItem::new("c", Some(2)),
            TestItem::new("d", None),     // no MCID
            TestItem::new("e", Some(99)), // MCID not in order
        ];

        reorder_by_structure(&mut items, &order);

        // Structured items in order: mcid=2 ("c"), mcid=0 ("a"), mcid=1 ("b")
        // Unstructured items preserve relative order: "d", "e"
        assert_eq!(items[0].label, "c");
        assert_eq!(items[1].label, "a");
        assert_eq!(items[2].label, "b");
        assert_eq!(items[3].label, "d");
        assert_eq!(items[4].label, "e");
    }

    #[test]
    fn reorder_by_structure_all_structured() {
        let order = PageStructureOrder {
            mcid_order: vec![1, 0],
        };

        let mut items = vec![
            TestItem::new("first", Some(0)),
            TestItem::new("second", Some(1)),
        ];

        reorder_by_structure(&mut items, &order);

        assert_eq!(items[0].label, "second"); // mcid=1 first in order
        assert_eq!(items[1].label, "first"); // mcid=0 second
    }

    #[test]
    fn reorder_by_structure_no_structured() {
        let order = PageStructureOrder {
            mcid_order: vec![5, 6, 7],
        };

        let mut items = vec![TestItem::new("a", None), TestItem::new("b", Some(99))];

        reorder_by_structure(&mut items, &order);

        // Nothing matches, original order preserved.
        assert_eq!(items[0].label, "a");
        assert_eq!(items[1].label, "b");
    }

    #[test]
    fn reorder_by_structure_empty_items() {
        let order = PageStructureOrder {
            mcid_order: vec![0, 1],
        };
        let mut items: Vec<TestItem> = Vec::new();
        reorder_by_structure(&mut items, &order);
        assert!(items.is_empty());
    }

    #[test]
    fn reorder_by_structure_empty_order() {
        let order = PageStructureOrder {
            mcid_order: Vec::new(),
        };
        let mut items = vec![TestItem::new("a", Some(0))];
        reorder_by_structure(&mut items, &order);
        assert_eq!(items[0].label, "a");
    }

    #[test]
    fn missing_struct_tree_root_returns_none() {
        // parse_structure_tree is tested indirectly here: verify that
        // get_page_structure_order returns None for an empty tree.
        let tree = StructureTree {
            root_elements: Vec::new(),
        };
        let result = get_page_structure_order(&tree, ObjRef::new(1, 0));
        assert!(result.is_none());
    }

    #[test]
    fn malformed_elements_are_skipped() {
        // Elements without page_ref matching don't contribute MCIDs.
        let page1 = ObjRef::new(1, 0);
        let page2 = ObjRef::new(2, 0);

        let tree = StructureTree {
            root_elements: vec![
                elem("P", vec![StructChild::Mcid(0)], Some(page1)),
                elem("P", vec![StructChild::Mcid(1)], Some(page2)),
                elem("P", vec![StructChild::Mcid(2)], Some(page1)),
            ],
        };

        // Only MCIDs for page1.
        let order = get_page_structure_order(&tree, page1).expect("should have order");
        assert_eq!(order.mcid_order, vec![0, 2]);
    }

    #[test]
    fn page_ref_none_matches_any_page() {
        // Elements with no page_ref should contribute MCIDs to any page query.
        let page = ObjRef::new(1, 0);
        let tree = StructureTree {
            root_elements: vec![
                elem("P", vec![StructChild::Mcid(0)], None),
                elem("P", vec![StructChild::Mcid(1)], Some(page)),
            ],
        };

        let order = get_page_structure_order(&tree, page).expect("should have order");
        assert_eq!(order.mcid_order, vec![0, 1]);
    }

    #[test]
    fn obj_reference_children_are_skipped() {
        // Unresolved ObjReference children should be safely skipped.
        let page = ObjRef::new(1, 0);
        let tree = StructureTree {
            root_elements: vec![elem(
                "P",
                vec![
                    StructChild::Mcid(0),
                    StructChild::ObjReference(ObjRef::new(99, 0)),
                    StructChild::Mcid(1),
                ],
                Some(page),
            )],
        };

        let order = get_page_structure_order(&tree, page).expect("should have order");
        assert_eq!(order.mcid_order, vec![0, 1]);
    }

    #[test]
    fn parse_kids_from_pdf_objects() {
        // Test parse_children with raw PdfObject values (no resolver needed
        // since there are no references to resolve).

        // /K as integer
        let obj = PdfObject::Integer(42);
        let children = parse_children_no_resolver(&obj, 0);
        assert_eq!(children.len(), 1);
        assert!(matches!(children[0], StructChild::Mcid(42)));

        // /K as array of integers
        let obj = PdfObject::Array(vec![
            PdfObject::Integer(0),
            PdfObject::Integer(1),
            PdfObject::Integer(2),
        ]);
        let children = parse_children_no_resolver(&obj, 0);
        assert_eq!(children.len(), 3);
        assert!(matches!(children[0], StructChild::Mcid(0)));
        assert!(matches!(children[1], StructChild::Mcid(1)));
        assert!(matches!(children[2], StructChild::Mcid(2)));

        // Negative integer is skipped
        let obj = PdfObject::Integer(-1);
        let children = parse_children_no_resolver(&obj, 0);
        assert!(children.is_empty());
    }

    /// Helper that calls parse_children without an actual ObjectResolver.
    /// Only works for objects that don't contain indirect references.
    fn parse_children_no_resolver(obj: &PdfObject, depth: usize) -> Vec<StructChild> {
        // Create a minimal resolver with empty data.
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data, xref);
        parse_children(&mut resolver, obj, depth)
    }

    #[test]
    fn parse_struct_element_from_dict() {
        let mut dict = PdfDictionary::new();
        dict.insert(b"S".to_vec(), PdfObject::Name(b"P".to_vec()));
        dict.insert(b"K".to_vec(), PdfObject::Integer(42));

        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data, xref);

        let elem = parse_struct_element(&mut resolver, &PdfObject::Dictionary(dict), 0);
        assert!(elem.is_some());
        let elem = elem.expect("element should parse");
        assert_eq!(elem.struct_type, "P");
        assert_eq!(elem.children.len(), 1);
        assert!(matches!(elem.children[0], StructChild::Mcid(42)));
    }

    #[test]
    fn parse_struct_element_with_nested_kids() {
        // P element with /K array containing an MCID dict and a child element.
        let mut mcid_dict = PdfDictionary::new();
        mcid_dict.insert(b"MCID".to_vec(), PdfObject::Integer(0));

        let mut child_elem_dict = PdfDictionary::new();
        child_elem_dict.insert(b"S".to_vec(), PdfObject::Name(b"Span".to_vec()));
        child_elem_dict.insert(b"K".to_vec(), PdfObject::Integer(1));

        let mut p_dict = PdfDictionary::new();
        p_dict.insert(b"S".to_vec(), PdfObject::Name(b"P".to_vec()));
        p_dict.insert(
            b"K".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Dictionary(mcid_dict),
                PdfObject::Dictionary(child_elem_dict),
            ]),
        );

        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data, xref);

        let elem = parse_struct_element(&mut resolver, &PdfObject::Dictionary(p_dict), 0);
        let elem = elem.expect("element should parse");
        assert_eq!(elem.struct_type, "P");
        assert_eq!(elem.children.len(), 2);
        assert!(matches!(elem.children[0], StructChild::Mcid(0)));
        assert!(matches!(elem.children[1], StructChild::Element(_)));

        if let StructChild::Element(ref child) = elem.children[1] {
            assert_eq!(child.struct_type, "Span");
            assert_eq!(child.children.len(), 1);
            assert!(matches!(child.children[0], StructChild::Mcid(1)));
        }
    }

    #[test]
    fn parse_struct_element_no_s_no_mcid_returns_none() {
        // Dict without /S and without /MCID should return None.
        let mut dict = PdfDictionary::new();
        dict.insert(b"K".to_vec(), PdfObject::Integer(5));

        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data, xref);

        let result = parse_struct_element(&mut resolver, &PdfObject::Dictionary(dict), 0);
        assert!(result.is_none());
    }

    #[test]
    fn parse_struct_element_mcid_dict_without_s() {
        // Dict with /MCID but no /S should produce a synthetic element.
        let mut dict = PdfDictionary::new();
        dict.insert(b"MCID".to_vec(), PdfObject::Integer(7));

        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data, xref);

        let elem = parse_struct_element(&mut resolver, &PdfObject::Dictionary(dict), 0);
        let elem = elem.expect("mcid dict should produce element");
        assert_eq!(elem.struct_type, "");
        assert_eq!(elem.children.len(), 1);
        assert!(matches!(elem.children[0], StructChild::Mcid(7)));
    }

    #[test]
    fn parse_structure_tree_no_struct_tree_root() {
        let catalog = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data, xref);

        let result = parse_structure_tree(&mut resolver, &catalog);
        assert!(result.is_none());
    }

    #[test]
    fn parse_structure_tree_empty_root() {
        // /StructTreeRoot exists but has no /K.
        let mut tree_root = PdfDictionary::new();
        tree_root.insert(
            b"Type".to_vec(),
            PdfObject::Name(b"StructTreeRoot".to_vec()),
        );

        let mut catalog = PdfDictionary::new();
        catalog.insert(b"StructTreeRoot".to_vec(), PdfObject::Dictionary(tree_root));

        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data, xref);

        let result = parse_structure_tree(&mut resolver, &catalog);
        assert!(result.is_some());
        assert!(result
            .as_ref()
            .map(|t| t.root_elements.is_empty())
            .unwrap_or(false));
    }

    // ---------------------------------------------------------------
    // Edge-case tests for reorder_by_structure
    // ---------------------------------------------------------------

    #[test]
    fn reorder_duplicate_mcids_in_order() {
        // If the structure order contains the same MCID twice, the HashMap
        // keeps the last index. Items with that MCID are placed at that
        // position. This is a degenerate case (malformed tree) but we
        // should not panic.
        let order = PageStructureOrder {
            mcid_order: vec![0, 1, 0], // MCID 0 appears at index 0 and 2
        };

        let mut items = vec![TestItem::new("a", Some(0)), TestItem::new("b", Some(1))];

        reorder_by_structure(&mut items, &order);

        // MCID 0 -> last position in map is index 2
        // MCID 1 -> position 1
        // So sorted by position: b (pos 1), a (pos 2)
        assert_eq!(items[0].label, "b");
        assert_eq!(items[1].label, "a");
    }

    #[test]
    fn reorder_items_with_mcids_not_in_order() {
        // Items whose MCIDs exist but are not in the structure order
        // should be treated as unstructured and appended after structured items,
        // preserving their relative order.
        let order = PageStructureOrder {
            mcid_order: vec![2],
        };

        let mut items = vec![
            TestItem::new("a", Some(5)),
            TestItem::new("b", Some(2)),
            TestItem::new("c", Some(10)),
            TestItem::new("d", Some(7)),
        ];

        reorder_by_structure(&mut items, &order);

        // Only "b" (mcid=2) is structured. The rest are unstructured.
        assert_eq!(items[0].label, "b");
        // Unstructured items preserve their original relative order.
        assert_eq!(items[1].label, "a");
        assert_eq!(items[2].label, "c");
        assert_eq!(items[3].label, "d");
    }

    #[test]
    fn reorder_all_items_have_no_mcid() {
        // When every item has no MCID at all, all are unstructured.
        // Original order must be fully preserved.
        let order = PageStructureOrder {
            mcid_order: vec![0, 1, 2],
        };

        let mut items = vec![
            TestItem::new("x", None),
            TestItem::new("y", None),
            TestItem::new("z", None),
        ];

        reorder_by_structure(&mut items, &order);

        assert_eq!(items[0].label, "x");
        assert_eq!(items[1].label, "y");
        assert_eq!(items[2].label, "z");
    }

    #[test]
    fn reorder_stability_same_mcid() {
        // Multiple items with the same MCID should maintain their relative
        // order (stable sort). This can happen when a single marked content
        // section produces multiple spans.
        let order = PageStructureOrder {
            mcid_order: vec![1, 0],
        };

        let mut items = vec![
            TestItem::new("a0_first", Some(0)),
            TestItem::new("a0_second", Some(0)),
            TestItem::new("b1_first", Some(1)),
            TestItem::new("b1_second", Some(1)),
        ];

        reorder_by_structure(&mut items, &order);

        // MCID 1 items first (both at position 0), then MCID 0 items (both at position 1).
        // Within each group, the original relative order is preserved because
        // sort_by_key is stable.
        assert_eq!(items[0].label, "b1_first");
        assert_eq!(items[1].label, "b1_second");
        assert_eq!(items[2].label, "a0_first");
        assert_eq!(items[3].label, "a0_second");
    }

    // ---------------------------------------------------------------
    // Edge-case tests for get_page_structure_order (DFS)
    // ---------------------------------------------------------------

    #[test]
    fn dfs_five_levels_deep() {
        // Verify DFS order through 5 levels of nesting.
        //
        //   Document
        //     Section
        //       Div
        //         P
        //           Span (mcid=10)
        //     P (mcid=20)
        //
        // DFS should yield [10, 20].
        let page = ObjRef::new(1, 0);
        let tree = StructureTree {
            root_elements: vec![elem(
                "Document",
                vec![
                    StructChild::Element(elem(
                        "Section",
                        vec![StructChild::Element(elem(
                            "Div",
                            vec![StructChild::Element(elem(
                                "P",
                                vec![StructChild::Element(elem(
                                    "Span",
                                    vec![StructChild::Mcid(10)],
                                    Some(page),
                                ))],
                                Some(page),
                            ))],
                            None,
                        ))],
                        None,
                    )),
                    StructChild::Element(elem("P", vec![StructChild::Mcid(20)], Some(page))),
                ],
                None,
            )],
        };

        let order = get_page_structure_order(&tree, page).expect("should find MCIDs");
        assert_eq!(order.mcid_order, vec![10, 20]);
    }

    #[test]
    fn dfs_no_page_ref_matches_different_target_pages() {
        // An element with page_ref=None should contribute MCIDs no matter
        // which page we query.
        let tree = StructureTree {
            root_elements: vec![elem("P", vec![StructChild::Mcid(0)], None)],
        };

        let order1 = get_page_structure_order(&tree, ObjRef::new(1, 0));
        let order2 = get_page_structure_order(&tree, ObjRef::new(99, 0));

        assert_eq!(order1.as_ref().map(|o| &o.mcid_order), Some(&vec![0]));
        assert_eq!(order2.as_ref().map(|o| &o.mcid_order), Some(&vec![0]));
    }

    #[test]
    fn dfs_interleaved_mcids_same_page() {
        // Multiple structure elements on the same page with MCIDs that aren't
        // in numeric order. Verify DFS traversal order, not numeric order.
        //
        //   Document
        //     H1 (mcid=3, page=1)
        //     P (page=1)
        //       Span (mcid=1, page=1)
        //       Span (mcid=4, page=1)
        //     P (mcid=0, page=1)
        //     P (mcid=2, page=2)   <-- different page, should be excluded
        //
        // DFS for page 1: [3, 1, 4, 0]
        let page1 = ObjRef::new(1, 0);
        let page2 = ObjRef::new(2, 0);

        let tree = StructureTree {
            root_elements: vec![elem(
                "Document",
                vec![
                    StructChild::Element(elem("H1", vec![StructChild::Mcid(3)], Some(page1))),
                    StructChild::Element(elem(
                        "P",
                        vec![
                            StructChild::Element(elem(
                                "Span",
                                vec![StructChild::Mcid(1)],
                                Some(page1),
                            )),
                            StructChild::Element(elem(
                                "Span",
                                vec![StructChild::Mcid(4)],
                                Some(page1),
                            )),
                        ],
                        Some(page1),
                    )),
                    StructChild::Element(elem("P", vec![StructChild::Mcid(0)], Some(page1))),
                    StructChild::Element(elem("P", vec![StructChild::Mcid(2)], Some(page2))),
                ],
                None,
            )],
        };

        let order = get_page_structure_order(&tree, page1).expect("should find MCIDs");
        assert_eq!(order.mcid_order, vec![3, 1, 4, 0]);
    }

    #[test]
    fn dfs_obj_references_skipped_in_multi_level_tree() {
        // ObjReference children at various levels of the tree should be
        // silently skipped without affecting the MCIDs collected from
        // Element and Mcid children.
        let page = ObjRef::new(1, 0);
        let tree = StructureTree {
            root_elements: vec![elem(
                "Document",
                vec![
                    StructChild::ObjReference(ObjRef::new(50, 0)),
                    StructChild::Element(elem(
                        "P",
                        vec![
                            StructChild::Mcid(0),
                            StructChild::ObjReference(ObjRef::new(51, 0)),
                            StructChild::Element(elem(
                                "Span",
                                vec![
                                    StructChild::ObjReference(ObjRef::new(52, 0)),
                                    StructChild::Mcid(1),
                                ],
                                Some(page),
                            )),
                        ],
                        Some(page),
                    )),
                    StructChild::ObjReference(ObjRef::new(53, 0)),
                    StructChild::Element(elem("P", vec![StructChild::Mcid(2)], Some(page))),
                ],
                None,
            )],
        };

        let order = get_page_structure_order(&tree, page).expect("should find MCIDs");
        assert_eq!(order.mcid_order, vec![0, 1, 2]);
    }

    #[test]
    fn dfs_multiple_root_elements() {
        // Structure tree with multiple root elements. DFS visits them in order.
        let page = ObjRef::new(1, 0);
        let tree = StructureTree {
            root_elements: vec![
                elem("P", vec![StructChild::Mcid(5)], Some(page)),
                elem("P", vec![StructChild::Mcid(3)], Some(page)),
                elem("P", vec![StructChild::Mcid(8)], Some(page)),
            ],
        };

        let order = get_page_structure_order(&tree, page).expect("should find MCIDs");
        assert_eq!(order.mcid_order, vec![5, 3, 8]);
    }

    #[test]
    fn dfs_no_mcids_for_page_returns_none() {
        // All MCIDs belong to a different page. Query for a page with no
        // content should return None.
        let page1 = ObjRef::new(1, 0);
        let page2 = ObjRef::new(2, 0);

        let tree = StructureTree {
            root_elements: vec![
                elem("P", vec![StructChild::Mcid(0)], Some(page1)),
                elem("P", vec![StructChild::Mcid(1)], Some(page1)),
            ],
        };

        let result = get_page_structure_order(&tree, page2);
        assert!(result.is_none());
    }

    // ---------------------------------------------------------------
    // Edge-case tests for parse_children
    // ---------------------------------------------------------------

    #[test]
    fn parse_children_single_integer() {
        // /K is a single integer: should produce one Mcid child.
        let obj = PdfObject::Integer(7);
        let children = parse_children_no_resolver(&obj, 0);
        assert_eq!(children.len(), 1);
        assert!(matches!(children[0], StructChild::Mcid(7)));
    }

    #[test]
    fn parse_children_mixed_types_in_array() {
        // /K array with integers, MCID dicts, and struct element dicts.
        let mut mcid_dict = PdfDictionary::new();
        mcid_dict.insert(b"MCID".to_vec(), PdfObject::Integer(10));

        let mut elem_dict = PdfDictionary::new();
        elem_dict.insert(b"S".to_vec(), PdfObject::Name(b"Span".to_vec()));
        elem_dict.insert(b"K".to_vec(), PdfObject::Integer(20));

        let obj = PdfObject::Array(vec![
            PdfObject::Integer(5),            // bare MCID
            PdfObject::Dictionary(mcid_dict), // MCID dict
            PdfObject::Dictionary(elem_dict), // struct element
            PdfObject::Integer(15),           // another bare MCID
        ]);

        let children = parse_children_no_resolver(&obj, 0);

        // Integer(5) -> Mcid(5)
        assert!(matches!(children[0], StructChild::Mcid(5)));
        // MCID dict -> Mcid(10)
        assert!(matches!(children[1], StructChild::Mcid(10)));
        // Struct element -> Element with child Mcid(20)
        assert!(matches!(children[2], StructChild::Element(_)));
        if let StructChild::Element(ref e) = children[2] {
            assert_eq!(e.struct_type, "Span");
            assert!(matches!(e.children[0], StructChild::Mcid(20)));
        }
        // Integer(15) -> Mcid(15)
        assert!(matches!(children[3], StructChild::Mcid(15)));
    }

    #[test]
    fn parse_children_negative_integers_in_array() {
        // Negative integers in /K arrays should be silently skipped.
        let obj = PdfObject::Array(vec![
            PdfObject::Integer(0),
            PdfObject::Integer(-5),
            PdfObject::Integer(1),
        ]);

        let children = parse_children_no_resolver(&obj, 0);
        assert_eq!(children.len(), 2);
        assert!(matches!(children[0], StructChild::Mcid(0)));
        assert!(matches!(children[1], StructChild::Mcid(1)));
    }

    #[test]
    fn parse_children_unresolvable_reference_in_array() {
        // A reference in /K that can't be resolved should produce an
        // ObjReference child (not panic).
        let obj = PdfObject::Array(vec![
            PdfObject::Integer(0),
            PdfObject::Reference(ObjRef::new(999, 0)), // won't resolve
            PdfObject::Integer(1),
        ]);

        let children = parse_children_no_resolver(&obj, 0);
        // Integer(0) -> Mcid(0)
        assert!(matches!(children[0], StructChild::Mcid(0)));
        // Unresolvable ref -> ObjReference
        assert!(matches!(children[1], StructChild::ObjReference(_)));
        // Integer(1) -> Mcid(1)
        assert!(matches!(children[2], StructChild::Mcid(1)));
    }

    #[test]
    fn parse_children_at_depth_limit() {
        // parse_children called at MAX_STRUCT_DEPTH should return empty.
        let obj = PdfObject::Integer(42);
        let children = parse_children_no_resolver(&obj, MAX_STRUCT_DEPTH);
        assert!(children.is_empty());
    }

    #[test]
    fn parse_children_non_object_types_ignored() {
        // Non-integer, non-dict, non-ref items in /K array are silently ignored.
        let obj = PdfObject::Array(vec![
            PdfObject::Integer(0),
            PdfObject::Boolean(true),
            PdfObject::Name(b"bogus".to_vec()),
            PdfObject::Null,
            PdfObject::Integer(1),
        ]);

        let children = parse_children_no_resolver(&obj, 0);
        assert_eq!(children.len(), 2);
        assert!(matches!(children[0], StructChild::Mcid(0)));
        assert!(matches!(children[1], StructChild::Mcid(1)));
    }

    // ---------------------------------------------------------------
    // Depth limit / large tree tests
    // ---------------------------------------------------------------

    #[test]
    fn large_tree_at_exact_depth_limit() {
        // Build a tree deeper than MAX_STRUCT_DEPTH.
        // The DFS traversal enforces the same depth limit, so the
        // deeply nested MCID is unreachable.
        let page = ObjRef::new(1, 0);

        let mut current = elem("Span", vec![StructChild::Mcid(42)], Some(page));
        for _ in 0..MAX_STRUCT_DEPTH + 5 {
            current = elem("Div", vec![StructChild::Element(current)], None);
        }

        let tree = StructureTree {
            root_elements: vec![current],
        };

        // DFS enforces MAX_STRUCT_DEPTH, so the MCID is unreachable.
        let order = get_page_structure_order(&tree, page);
        assert!(order.is_none());
    }

    #[test]
    fn dfs_tree_just_under_depth_limit() {
        // Build a tree at exactly MAX_STRUCT_DEPTH - 1 wrapping levels.
        // The MCID should be reachable.
        let page = ObjRef::new(1, 0);

        let mut current = elem("Span", vec![StructChild::Mcid(42)], Some(page));
        // MAX_STRUCT_DEPTH - 2 wraps: root at depth 0, innermost at depth
        // MAX_STRUCT_DEPTH - 2, its children processed at MAX_STRUCT_DEPTH - 1.
        for _ in 0..MAX_STRUCT_DEPTH - 2 {
            current = elem("Div", vec![StructChild::Element(current)], None);
        }

        let tree = StructureTree {
            root_elements: vec![current],
        };

        let order = get_page_structure_order(&tree, page);
        assert!(order.is_some());
        assert_eq!(order.as_ref().map(|o| &o.mcid_order), Some(&vec![42]));
    }

    #[test]
    fn parse_children_depth_limit_truncates_tree() {
        // When parsing, a struct element dict at MAX_STRUCT_DEPTH should
        // not be parsed (returns None), so its MCID children are lost.
        let mut inner_dict = PdfDictionary::new();
        inner_dict.insert(b"S".to_vec(), PdfObject::Name(b"Span".to_vec()));
        inner_dict.insert(b"K".to_vec(), PdfObject::Integer(99));

        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data, xref);

        // Parse at depth = MAX_STRUCT_DEPTH: should return None.
        let result = parse_struct_element(
            &mut resolver,
            &PdfObject::Dictionary(inner_dict),
            MAX_STRUCT_DEPTH,
        );
        assert!(result.is_none());
    }

    #[test]
    fn parse_structure_tree_with_direct_elements() {
        // /StructTreeRoot with /K as an array of direct struct elements.
        let mut p1 = PdfDictionary::new();
        p1.insert(b"S".to_vec(), PdfObject::Name(b"P".to_vec()));
        p1.insert(b"K".to_vec(), PdfObject::Integer(0));

        let mut p2 = PdfDictionary::new();
        p2.insert(b"S".to_vec(), PdfObject::Name(b"P".to_vec()));
        p2.insert(b"K".to_vec(), PdfObject::Integer(1));

        let mut tree_root = PdfDictionary::new();
        tree_root.insert(
            b"Type".to_vec(),
            PdfObject::Name(b"StructTreeRoot".to_vec()),
        );
        tree_root.insert(
            b"K".to_vec(),
            PdfObject::Array(vec![PdfObject::Dictionary(p1), PdfObject::Dictionary(p2)]),
        );

        let mut catalog = PdfDictionary::new();
        catalog.insert(b"StructTreeRoot".to_vec(), PdfObject::Dictionary(tree_root));

        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data, xref);

        let tree = parse_structure_tree(&mut resolver, &catalog).expect("should parse");
        assert_eq!(tree.root_elements.len(), 2);
        assert_eq!(tree.root_elements[0].struct_type, "P");
        assert_eq!(tree.root_elements[1].struct_type, "P");
    }

    // ---------------------------------------------------------------
    // /Alt text extraction tests
    // ---------------------------------------------------------------

    /// Helper: build a StructElement with alt text.
    fn elem_with_alt(
        struct_type: &str,
        children: Vec<StructChild>,
        page_ref: Option<ObjRef>,
        alt: &str,
    ) -> StructElement {
        StructElement {
            struct_type: struct_type.to_string(),
            children,
            page_ref,
            alt_text: Some(alt.to_string()),
        }
    }

    #[test]
    fn parse_struct_element_with_alt_text() {
        // Build a Figure dict with /Alt.
        let mut dict = PdfDictionary::new();
        dict.insert(b"S".to_vec(), PdfObject::Name(b"Figure".to_vec()));
        dict.insert(b"K".to_vec(), PdfObject::Integer(5));
        dict.insert(
            b"Alt".to_vec(),
            PdfObject::String(crate::object::PdfString::new(
                b"A photo of a sunset".to_vec(),
            )),
        );

        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data, xref);

        let elem = parse_struct_element(&mut resolver, &PdfObject::Dictionary(dict), 0);
        let elem = elem.expect("Figure with /Alt should parse");
        assert_eq!(elem.struct_type, "Figure");
        assert_eq!(elem.alt_text.as_deref(), Some("A photo of a sunset"));
        assert_eq!(elem.children.len(), 1);
        assert!(matches!(elem.children[0], StructChild::Mcid(5)));
    }

    #[test]
    fn parse_struct_element_without_alt_text() {
        // A paragraph with no /Alt should have alt_text = None.
        let mut dict = PdfDictionary::new();
        dict.insert(b"S".to_vec(), PdfObject::Name(b"P".to_vec()));
        dict.insert(b"K".to_vec(), PdfObject::Integer(0));

        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data, xref);

        let elem = parse_struct_element(&mut resolver, &PdfObject::Dictionary(dict), 0);
        let elem = elem.expect("P without /Alt should parse");
        assert!(elem.alt_text.is_none());
    }

    #[test]
    fn parse_struct_element_with_empty_alt_text() {
        // An empty /Alt string should produce alt_text = None (filtered out).
        let mut dict = PdfDictionary::new();
        dict.insert(b"S".to_vec(), PdfObject::Name(b"Figure".to_vec()));
        dict.insert(b"K".to_vec(), PdfObject::Integer(0));
        dict.insert(
            b"Alt".to_vec(),
            PdfObject::String(crate::object::PdfString::new(Vec::new())),
        );

        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data, xref);

        let elem = parse_struct_element(&mut resolver, &PdfObject::Dictionary(dict), 0);
        let elem = elem.expect("Figure with empty /Alt should parse");
        assert!(elem.alt_text.is_none());
    }

    #[test]
    fn parse_struct_element_with_utf16be_alt_text() {
        // /Alt encoded as UTF-16BE with BOM.
        let mut dict = PdfDictionary::new();
        dict.insert(b"S".to_vec(), PdfObject::Name(b"Figure".to_vec()));
        dict.insert(b"K".to_vec(), PdfObject::Integer(3));
        // UTF-16BE BOM + "Hi" (0x0048, 0x0069)
        let utf16_bytes = vec![0xFE, 0xFF, 0x00, 0x48, 0x00, 0x69];
        dict.insert(
            b"Alt".to_vec(),
            PdfObject::String(crate::object::PdfString::new(utf16_bytes)),
        );

        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data, xref);

        let elem = parse_struct_element(&mut resolver, &PdfObject::Dictionary(dict), 0);
        let elem = elem.expect("Figure with UTF-16BE /Alt should parse");
        assert_eq!(elem.alt_text.as_deref(), Some("Hi"));
    }

    #[test]
    fn get_page_alt_texts_figure_with_alt() {
        // Tree: Document -> Figure(alt="Chart", mcid=0) + P(mcid=1)
        let page = ObjRef::new(1, 0);
        let tree = StructureTree {
            root_elements: vec![elem(
                "Document",
                vec![
                    StructChild::Element(elem_with_alt(
                        "Figure",
                        vec![StructChild::Mcid(0)],
                        Some(page),
                        "Chart showing growth",
                    )),
                    StructChild::Element(elem("P", vec![StructChild::Mcid(1)], Some(page))),
                ],
                None,
            )],
        };

        let alts = get_page_alt_texts(&tree, page);
        assert_eq!(alts.len(), 1);
        assert_eq!(
            alts.get(&0).map(|s| s.as_str()),
            Some("Chart showing growth")
        );
        // P has no alt text, so MCID 1 should not be in the map.
        assert!(!alts.contains_key(&1));
    }

    #[test]
    fn get_page_alt_texts_inherited_from_parent() {
        // Tree: Figure(alt="Photo") -> Span(mcid=5) + Span(mcid=6)
        // Children inherit the parent's alt text.
        let page = ObjRef::new(1, 0);
        let tree = StructureTree {
            root_elements: vec![elem_with_alt(
                "Figure",
                vec![
                    StructChild::Element(elem("Span", vec![StructChild::Mcid(5)], Some(page))),
                    StructChild::Element(elem("Span", vec![StructChild::Mcid(6)], Some(page))),
                ],
                Some(page),
                "Photo",
            )],
        };

        let alts = get_page_alt_texts(&tree, page);
        assert_eq!(alts.len(), 2);
        assert_eq!(alts.get(&5).map(|s| s.as_str()), Some("Photo"));
        assert_eq!(alts.get(&6).map(|s| s.as_str()), Some("Photo"));
    }

    #[test]
    fn get_page_alt_texts_child_overrides_parent() {
        // Tree: Figure(alt="Outer") -> Figure(alt="Inner", mcid=0)
        // The inner Figure's own alt text should override.
        let page = ObjRef::new(1, 0);
        let tree = StructureTree {
            root_elements: vec![elem_with_alt(
                "Figure",
                vec![StructChild::Element(elem_with_alt(
                    "Figure",
                    vec![StructChild::Mcid(0)],
                    Some(page),
                    "Inner",
                ))],
                None,
                "Outer",
            )],
        };

        let alts = get_page_alt_texts(&tree, page);
        assert_eq!(alts.get(&0).map(|s| s.as_str()), Some("Inner"));
    }

    #[test]
    fn get_page_alt_texts_no_figures() {
        // Tree with only paragraphs, no alt text anywhere.
        let page = ObjRef::new(1, 0);
        let tree = StructureTree {
            root_elements: vec![elem(
                "Document",
                vec![
                    StructChild::Element(elem("P", vec![StructChild::Mcid(0)], Some(page))),
                    StructChild::Element(elem("P", vec![StructChild::Mcid(1)], Some(page))),
                ],
                None,
            )],
        };

        let alts = get_page_alt_texts(&tree, page);
        assert!(alts.is_empty());
    }

    #[test]
    fn get_page_alt_texts_wrong_page() {
        // Alt text from a different page should not appear.
        let page1 = ObjRef::new(1, 0);
        let page2 = ObjRef::new(2, 0);
        let tree = StructureTree {
            root_elements: vec![elem_with_alt(
                "Figure",
                vec![StructChild::Mcid(0)],
                Some(page2),
                "Only on page 2",
            )],
        };

        let alts = get_page_alt_texts(&tree, page1);
        assert!(alts.is_empty());
    }

    // -----------------------------------------------------------------------
    // AltTextIndex (#150): build once, look up many
    // -----------------------------------------------------------------------

    #[test]
    fn alt_text_index_matches_per_page_walker_on_multi_page_tree() {
        // Two pages, two Figures with alt text scoped to their own page.
        let page1 = ObjRef::new(1, 0);
        let page2 = ObjRef::new(2, 0);
        let tree = StructureTree {
            root_elements: vec![elem(
                "Document",
                vec![
                    StructChild::Element(elem_with_alt(
                        "Figure",
                        vec![StructChild::Mcid(0)],
                        Some(page1),
                        "Chart on page 1",
                    )),
                    StructChild::Element(elem_with_alt(
                        "Figure",
                        vec![StructChild::Mcid(3)],
                        Some(page2),
                        "Chart on page 2",
                    )),
                ],
                None,
            )],
        };

        let idx = AltTextIndex::build(Some(&tree));
        assert_eq!(
            idx.alt_texts_for_page(page1),
            get_page_alt_texts(&tree, page1)
        );
        assert_eq!(
            idx.alt_texts_for_page(page2),
            get_page_alt_texts(&tree, page2)
        );
        // Page 2's alt text does not leak into page 1 and vice versa.
        assert!(!idx.alt_texts_for_page(page1).contains_key(&3));
        assert!(!idx.alt_texts_for_page(page2).contains_key(&0));
    }

    #[test]
    fn alt_text_index_handles_unscoped_elements() {
        // A Figure with no /Pg reference: its alt text should appear on
        // every queried page (optimistic-attribution parity with the
        // legacy per-page walker).
        let page1 = ObjRef::new(1, 0);
        let page2 = ObjRef::new(2, 0);
        let tree = StructureTree {
            root_elements: vec![elem_with_alt(
                "Figure",
                vec![StructChild::Mcid(7)],
                None,
                "Unscoped alt",
            )],
        };
        let idx = AltTextIndex::build(Some(&tree));
        assert_eq!(
            idx.alt_texts_for_page(page1).get(&7).map(String::as_str),
            Some("Unscoped alt")
        );
        assert_eq!(
            idx.alt_texts_for_page(page2).get(&7).map(String::as_str),
            Some("Unscoped alt")
        );
    }

    #[test]
    fn alt_text_index_empty_on_no_tree() {
        let idx = AltTextIndex::build(None);
        let page = ObjRef::new(1, 0);
        assert!(idx.alt_texts_for_page(page).is_empty());
    }
}

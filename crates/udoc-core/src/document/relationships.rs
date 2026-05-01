//! Relationships overlay: how nodes connect to each other and to
//! external resources.
//!
//! Footnotes, bookmarks, captions, table of contents, component references.

use std::collections::HashMap;

use super::content::Block;
use super::overlay::SparseOverlay;
use super::NodeId;

/// Maximum number of hyperlinks stored per document. Prevents unbounded
/// allocation from malicious documents with millions of link entries.
pub const MAX_HYPERLINKS: usize = 50_000;

/// Maximum number of bookmarks stored per document. Prevents unbounded
/// allocation from malicious documents with deeply nested outline trees.
pub const MAX_BOOKMARKS: usize = 50_000;

/// Maximum number of footnote/endnote definitions per document.
pub const MAX_FOOTNOTES: usize = 50_000;

/// Maximum number of table of contents entries per document.
pub const MAX_TOC_ENTRIES: usize = 50_000;

/// Maximum number of caption associations per document.
pub const MAX_CAPTIONS: usize = 50_000;

/// Maximum number of component references per document.
pub const MAX_COMPONENT_REFS: usize = 50_000;

/// Result of attempting to add a bookmark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookmarkAddResult {
    /// Bookmark was inserted.
    Added,
    /// A bookmark with the same name already exists. The original is kept.
    Duplicate,
    /// The maximum number of bookmarks has been reached.
    LimitReached,
}

impl BookmarkAddResult {
    /// Returns true if the bookmark was successfully added.
    pub fn is_added(self) -> bool {
        matches!(self, BookmarkAddResult::Added)
    }
}

/// Result of attempting to add a footnote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FootnoteAddResult {
    /// Footnote was inserted.
    Added,
    /// A footnote with the same label already exists. The original is kept.
    Duplicate,
    /// The maximum number of footnotes has been reached.
    LimitReached,
}

impl FootnoteAddResult {
    /// Returns true if the footnote was successfully added.
    pub fn is_added(self) -> bool {
        matches!(self, FootnoteAddResult::Added)
    }
}

/// Relationships overlay: how nodes connect.
///
/// All collections are private with controlled mutation methods that enforce
/// per-document limits. Use the accessor methods to read and the `add_*`
/// methods to insert.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Relationships {
    /// Footnote/endnote definitions keyed by label.
    footnotes: HashMap<String, FootnoteDef>,
    /// Bookmarks: name -> target. Named positions within the document.
    bookmarks: HashMap<String, BookmarkTarget>,
    /// Hyperlinks: external URLs found in the document.
    hyperlinks: Vec<String>,
    /// Caption associations: image/table node -> caption node.
    captions: SparseOverlay<NodeId>,
    /// Table of contents entries in order.
    toc_entries: Vec<TocEntry>,
    /// Component/template references (future).
    component_refs: SparseOverlay<ComponentRef>,
}

impl Relationships {
    /// Whether the named node has any per-node data in this overlay.
    /// Drives [`crate::document::Document::relationships_for`] (
    ///). Per-node fields: `captions`,
    /// `component_refs`. Other fields (footnotes, bookmarks,
    /// hyperlinks, toc_entries) are document-wide and don't
    /// participate.
    pub fn has_node(&self, node: NodeId) -> bool {
        self.captions.contains(node) || self.component_refs.contains(node)
    }

    /// Read all collected hyperlink URLs.
    pub fn hyperlinks(&self) -> &[String] {
        &self.hyperlinks
    }

    /// Add a hyperlink URL if the limit has not been reached.
    /// Returns `true` if the URL was added, `false` if the limit was hit.
    pub fn add_hyperlink(&mut self, url: String) -> bool {
        if self.hyperlinks.len() >= MAX_HYPERLINKS {
            return false;
        }
        self.hyperlinks.push(url);
        true
    }

    /// Batch-insert hyperlink URLs, respecting the [`MAX_HYPERLINKS`] limit.
    /// Returns the number of URLs actually inserted.
    pub fn extend_hyperlinks(&mut self, urls: impl IntoIterator<Item = String>) -> usize {
        let remaining = MAX_HYPERLINKS.saturating_sub(self.hyperlinks.len());
        let before = self.hyperlinks.len();
        self.hyperlinks.extend(urls.into_iter().take(remaining));
        self.hyperlinks.len() - before
    }

    /// Read all bookmarks.
    pub fn bookmarks(&self) -> &HashMap<String, BookmarkTarget> {
        &self.bookmarks
    }

    /// Insert a bookmark if the limit has not been reached and the name is new.
    pub fn add_bookmark(&mut self, name: String, target: BookmarkTarget) -> BookmarkAddResult {
        if self.bookmarks.len() >= MAX_BOOKMARKS {
            return BookmarkAddResult::LimitReached;
        }
        use std::collections::hash_map::Entry;
        if let Entry::Vacant(e) = self.bookmarks.entry(name) {
            e.insert(target);
            BookmarkAddResult::Added
        } else {
            BookmarkAddResult::Duplicate
        }
    }

    /// Read all footnote/endnote definitions.
    pub fn footnotes(&self) -> &HashMap<String, FootnoteDef> {
        &self.footnotes
    }

    /// Insert a footnote definition.
    pub fn add_footnote(&mut self, label: String, def: FootnoteDef) -> FootnoteAddResult {
        if self.footnotes.len() >= MAX_FOOTNOTES {
            return FootnoteAddResult::LimitReached;
        }
        use std::collections::hash_map::Entry;
        if let Entry::Vacant(e) = self.footnotes.entry(label) {
            e.insert(def);
            FootnoteAddResult::Added
        } else {
            FootnoteAddResult::Duplicate
        }
    }

    // -- TOC entries ---------------------------------------------------------

    /// Read all table of contents entries.
    pub fn toc_entries(&self) -> &[TocEntry] {
        &self.toc_entries
    }

    /// Add a TOC entry if the limit has not been reached.
    /// Returns `true` if inserted.
    pub fn add_toc_entry(&mut self, entry: TocEntry) -> bool {
        if self.toc_entries.len() >= MAX_TOC_ENTRIES {
            return false;
        }
        self.toc_entries.push(entry);
        true
    }

    /// Read caption associations.
    pub fn captions(&self) -> &SparseOverlay<NodeId> {
        &self.captions
    }

    /// Set a caption association: `node_id` is captioned by `caption_id`.
    /// Returns `false` if the caption limit has been reached.
    pub fn set_caption(&mut self, node_id: NodeId, caption_id: NodeId) -> bool {
        if self.captions.len() >= MAX_CAPTIONS {
            return false;
        }
        self.captions.set(node_id, caption_id);
        true
    }

    // -- Component refs ------------------------------------------------------

    /// Read component/template references.
    pub fn component_refs(&self) -> &SparseOverlay<ComponentRef> {
        &self.component_refs
    }

    /// Set a component reference for a node.
    /// Returns `false` if the component reference limit has been reached.
    pub fn set_component_ref(&mut self, node_id: NodeId, comp_ref: ComponentRef) -> bool {
        if self.component_refs.len() >= MAX_COMPONENT_REFS {
            return false;
        }
        self.component_refs.set(node_id, comp_ref);
        true
    }
}

/// A footnote or endnote definition.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct FootnoteDef {
    pub label: String,
    pub content: Vec<Block>,
}

impl FootnoteDef {
    /// Create a new footnote definition.
    pub fn new(label: String, content: Vec<Block>) -> Self {
        Self { label, content }
    }
}

/// Where a bookmark points.
///
/// Some formats (e.g. DOCX) mark bookmark positions between elements rather
/// than on a specific node. `Positional` means "bookmark exists but target
/// node is unresolved."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum BookmarkTarget {
    /// Bookmark resolved to a specific document node.
    Resolved(NodeId),
    /// Bookmark marks a position but doesn't target a specific node.
    Positional,
}

/// A table of contents entry.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct TocEntry {
    pub level: u8,
    pub text: String,
    pub target: Option<NodeId>,
}

/// A component or template reference (future use).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct ComponentRef {
    pub component_id: String,
    pub overrides: Vec<(String, String)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relationships_default() {
        let r = Relationships::default();
        assert!(r.footnotes().is_empty());
        assert!(r.bookmarks().is_empty());
        assert!(r.hyperlinks().is_empty());
        assert!(r.captions().is_empty());
        assert!(r.toc_entries().is_empty());
        assert!(r.component_refs().is_empty());
    }

    #[test]
    fn relationships_with_footnotes() {
        let mut r = Relationships::default();
        assert_eq!(
            r.add_footnote(
                "1".into(),
                FootnoteDef {
                    label: "1".into(),
                    content: vec![],
                },
            ),
            FootnoteAddResult::Added,
        );
        assert_eq!(r.footnotes().len(), 1);
        assert!(r.footnotes().contains_key("1"));
    }

    #[test]
    fn relationships_with_bookmarks() {
        let mut r = Relationships::default();
        assert_eq!(
            r.add_bookmark("intro".into(), BookmarkTarget::Resolved(NodeId::new(5))),
            BookmarkAddResult::Added,
        );
        assert_eq!(
            r.bookmarks().get("intro"),
            Some(&BookmarkTarget::Resolved(NodeId::new(5)))
        );
        // DOCX-style unresolved bookmark.
        assert_eq!(
            r.add_bookmark("toc_anchor".into(), BookmarkTarget::Positional),
            BookmarkAddResult::Added,
        );
        assert_eq!(
            r.bookmarks().get("toc_anchor"),
            Some(&BookmarkTarget::Positional)
        );
    }

    #[test]
    fn relationships_with_toc() {
        let mut r = Relationships::default();
        assert!(r.add_toc_entry(TocEntry {
            level: 1,
            text: "Introduction".into(),
            target: Some(NodeId::new(10)),
        }));
        assert!(r.add_toc_entry(TocEntry {
            level: 2,
            text: "Background".into(),
            target: None,
        }));
        assert_eq!(r.toc_entries().len(), 2);
    }

    #[test]
    fn captions_overlay() {
        let mut r = Relationships::default();
        let table_id = NodeId::new(3);
        let caption_id = NodeId::new(4);
        r.set_caption(table_id, caption_id);
        assert_eq!(r.captions().get(table_id), Some(&caption_id));
    }

    #[test]
    fn add_hyperlink_enforces_limit() {
        let mut r = Relationships::default();
        for i in 0..MAX_HYPERLINKS {
            assert!(r.add_hyperlink(format!("https://example.com/{i}")));
        }
        assert_eq!(r.hyperlinks().len(), MAX_HYPERLINKS);
        assert!(!r.add_hyperlink("https://overflow.com".into()));
        assert_eq!(r.hyperlinks().len(), MAX_HYPERLINKS);
    }

    #[test]
    fn extend_hyperlinks_enforces_limit() {
        let mut r = Relationships::default();
        // Fill most of the capacity.
        for i in 0..MAX_HYPERLINKS - 5 {
            r.add_hyperlink(format!("https://example.com/{i}"));
        }
        // Try to add 10 more; only 5 should fit.
        let urls: Vec<String> = (0..10).map(|i| format!("https://extra.com/{i}")).collect();
        let inserted = r.extend_hyperlinks(urls);
        assert_eq!(inserted, 5);
        assert_eq!(r.hyperlinks().len(), MAX_HYPERLINKS);
    }

    #[test]
    fn add_bookmark_enforces_limit() {
        let mut r = Relationships::default();
        for i in 0..MAX_BOOKMARKS {
            assert_eq!(
                r.add_bookmark(format!("bm_{i}"), BookmarkTarget::Positional),
                BookmarkAddResult::Added,
            );
        }
        assert_eq!(r.bookmarks().len(), MAX_BOOKMARKS);
        assert_eq!(
            r.add_bookmark("overflow".into(), BookmarkTarget::Positional),
            BookmarkAddResult::LimitReached,
        );
        assert_eq!(r.bookmarks().len(), MAX_BOOKMARKS);
    }

    #[test]
    fn add_bookmark_rejects_duplicate() {
        let mut r = Relationships::default();
        assert_eq!(
            r.add_bookmark("intro".into(), BookmarkTarget::Positional),
            BookmarkAddResult::Added,
        );
        assert_eq!(
            r.add_bookmark("intro".into(), BookmarkTarget::Resolved(NodeId::new(5))),
            BookmarkAddResult::Duplicate,
        );
        // Original value preserved.
        assert_eq!(
            r.bookmarks().get("intro"),
            Some(&BookmarkTarget::Positional)
        );
    }

    #[test]
    fn add_footnote_enforces_limit() {
        let mut r = Relationships::default();
        for i in 0..MAX_FOOTNOTES {
            assert_eq!(
                r.add_footnote(
                    format!("fn:{i}"),
                    FootnoteDef::new(format!("fn:{i}"), vec![])
                ),
                FootnoteAddResult::Added,
            );
        }
        assert_eq!(r.footnotes().len(), MAX_FOOTNOTES);
        assert_eq!(
            r.add_footnote(
                "overflow".into(),
                FootnoteDef::new("overflow".into(), vec![])
            ),
            FootnoteAddResult::LimitReached,
        );
        assert_eq!(r.footnotes().len(), MAX_FOOTNOTES);
    }

    #[test]
    fn add_footnote_rejects_duplicate() {
        let mut r = Relationships::default();
        assert_eq!(
            r.add_footnote("fn:1".into(), FootnoteDef::new("fn:1".into(), vec![])),
            FootnoteAddResult::Added,
        );
        assert_eq!(
            r.add_footnote("fn:1".into(), FootnoteDef::new("fn:1".into(), vec![])),
            FootnoteAddResult::Duplicate,
        );
        assert_eq!(r.footnotes().len(), 1);
    }

    #[test]
    fn add_toc_entry_enforces_limit() {
        let mut r = Relationships::default();
        for i in 0..MAX_TOC_ENTRIES {
            assert!(r.add_toc_entry(TocEntry {
                level: 1,
                text: format!("entry {i}"),
                target: None,
            }));
        }
        assert_eq!(r.toc_entries().len(), MAX_TOC_ENTRIES);
        assert!(!r.add_toc_entry(TocEntry {
            level: 1,
            text: "overflow".into(),
            target: None,
        }));
        assert_eq!(r.toc_entries().len(), MAX_TOC_ENTRIES);
    }
}

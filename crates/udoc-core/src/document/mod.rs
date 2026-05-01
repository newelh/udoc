//! Document model types for format-agnostic document representation.
//!
//! The document model has five layers:
//! 1. **Content Spine** (`Vec<Block>`) -- text-first Block/Inline tree
//! 2. **Presentation** -- geometry, fonts, colors, page layout
//! 3. **Relationships** -- footnotes, bookmarks, cross-references
//! 4. **Metadata** -- title, author, page count, properties
//! 5. **Interactions** -- forms, comments, tracked changes
//!
//! Every node in the tree has a [`NodeId`]. Overlay types ([`Overlay`],
//! [`SparseOverlay`]) provide per-node data indexed by NodeId.

// Submodules are pub for direct path access from sibling crates that
// build the document tree node-by-node, but doc-hidden so the API tour
// shows the flat re-exports below rather than the file layout.
#[doc(hidden)]
pub mod assets;
#[doc(hidden)]
pub mod content;
#[doc(hidden)]
pub mod interactions;
#[doc(hidden)]
pub mod overlay;
#[doc(hidden)]
pub mod presentation;
#[doc(hidden)]
pub mod relationships;
#[doc(hidden)]
pub mod table;

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

pub use assets::{AssetConfig, AssetRef, AssetStore, FontAsset, FontProgramType, ImageAsset};
pub use content::{
    Block, ImageData, ImageRef, Inline, ListItem, ListKind, SectionRole, ShapeKind, SpanStyle,
};
pub use interactions::{
    ChangeType, Comment, FormField, FormFieldType, Interactions, TrackedChange,
};
pub use overlay::{Overlay, SparseOverlay};
pub use presentation::{
    Alignment, BlockLayout, ClipRegion, ClipRegionFillRule, ColSpec, Color, ExtendedTextStyle,
    FillRule, FlowDirection, ImagePlacement, LayoutInfo, LayoutMode, Padding, PageDef, PageShape,
    PaintLineCap, PaintLineJoin, PaintPath, PaintPattern, PaintSegment, PaintShading,
    PaintShadingKind, PaintStroke, PathShapeKind, PositionedSpan, Presentation, SoftMaskLayer,
    SoftMaskSubtype,
};
pub use relationships::{
    BookmarkAddResult, BookmarkTarget, ComponentRef, FootnoteAddResult, FootnoteDef, Relationships,
    TocEntry,
};
pub use table::{CellValue, TableCell, TableData, TableRow};

// Hard ceilings. Internal recursion / arena limits enforced by
// the document model so untrusted input cannot exhaust memory.
//
// MAX_NODE_ID, MAX_COMMENT_DEPTH, and
// MAX_CELL_VALUE_DEPTH are not part of the public API. The constants
// stay crate-visible (`pub(crate)`) for internal use by sibling modules
// and tests; they are re-exported as `pub` only under the
// `test-internals` feature so fuzz targets can reach them. The
// relationships ceilings stay doc-hidden because they are not in the
// `udoc` facade re-export set.
#[cfg(feature = "test-internals")]
pub use interactions::MAX_COMMENT_DEPTH;
#[doc(hidden)]
pub use relationships::{MAX_BOOKMARKS, MAX_FOOTNOTES, MAX_HYPERLINKS, MAX_TOC_ENTRIES};
#[cfg(feature = "test-internals")]
pub use table::MAX_CELL_VALUE_DEPTH;

/// Maximum NodeId value (16 million). Enforced by `alloc_node_id()`,
/// `try_alloc_node_id()`, and `Overlay::set()` to prevent OOM from
/// runaway allocation. Hard ceiling.
///
/// not part of the public API. `pub(crate)` so
/// sibling modules in `udoc-core` (and tests via `super::*`) keep
/// access; re-exported as `pub` from this module only under the
/// `test-internals` feature for fuzz targets.
#[cfg(feature = "test-internals")]
pub const MAX_NODE_ID: u64 = 16_000_000;
#[cfg(not(feature = "test-internals"))]
pub(crate) const MAX_NODE_ID: u64 = 16_000_000;

/// Stable identifier for any node in the document tree.
/// Sequential u64, unique within a document. Cheap to copy and hash.
/// Public and stable: third parties can use NodeId to key external data.
///
/// The inner value is private. Use [`NodeId::value()`] to read it.
/// For fresh IDs, prefer [`Document::alloc_node_id()`] or
/// [`Document::try_alloc_node_id()`] which enforce the `MAX_NODE_ID` limit.
/// [`NodeId::new()`] is available for reconstructing IDs from serialized
/// data but does not check the limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u64);

impl NodeId {
    /// Read the inner u64 value.
    pub fn value(&self) -> u64 {
        self.0
    }

    /// Create a NodeId from a raw u64 without checking `MAX_NODE_ID`.
    ///
    /// Use `Document::alloc_node_id()` or `Document::try_alloc_node_id()`
    /// for fresh allocation (they enforce the limit). This constructor is
    /// for reconstructing IDs from serialized wire data (hooks, JSON
    /// round-trips) or in tests.
    pub fn new(val: u64) -> Self {
        NodeId(val)
    }
}

impl fmt::Display for NodeId {
    /// Render as `"node:{id}"` for stable, human-readable logging.
    ///
    /// distinguishes node ids from raw
    /// integers in mixed log output, matches the citation-anchor
    /// format used by the markdown emitter (T1b-MARKDOWN-OUT).
    ///
    /// ```
    /// use udoc_core::document::NodeId;
    /// assert_eq!(format!("{}", NodeId::new(1234)), "node:1234");
    /// ```
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "node:{}", self.value())
    }
}

// Custom serde: serialize as a plain u64, not as a struct.
#[cfg(feature = "serde")]
impl serde::Serialize for NodeId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(self.value())
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for NodeId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let val = u64::deserialize(deserializer)?;
        Ok(NodeId::new(val))
    }
}

/// Document-level metadata.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct DocumentMetadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub creation_date: Option<String>,
    pub modification_date: Option<String>,
    pub page_count: usize,
    /// Custom/extended properties.
    pub properties: HashMap<String, String>,
}

impl DocumentMetadata {
    /// Create metadata with just a page count. Other fields default to None.
    pub fn with_page_count(page_count: usize) -> Self {
        Self {
            page_count,
            ..Default::default()
        }
    }
}

/// The complete extracted document.
///
/// Five-layer spine+overlay architecture:
/// - `content`: Block/Inline tree (always present)
/// - `presentation`: geometry, fonts, colors (optional)
/// - `relationships`: footnotes, bookmarks (optional)
/// - `metadata`: title, author, etc. (always present)
/// - `interactions`: forms, comments (optional)
/// - `assets`: detachable store for heavy binary data (images, fonts)
#[derive(Debug)]
#[non_exhaustive]
pub struct Document {
    /// Content spine: block-level elements in reading order.
    pub content: Vec<Block>,
    /// Presentation layer (geometry, fonts, colors, page layout).
    pub presentation: Option<Presentation>,
    /// Relationships layer (footnotes, bookmarks, cross-references).
    pub relationships: Option<Relationships>,
    /// Document-level metadata.
    pub metadata: DocumentMetadata,
    /// Interaction layer (forms, comments, tracked changes).
    pub interactions: Option<Interactions>,
    /// Asset store for images and fonts. Referenced by ImageRef/AssetRef indices.
    pub assets: AssetStore,
    /// Diagnostics collected during extraction, populated by the
    /// facade when `Config::collect_diagnostics` is true (
    ///). Read via [`Document::diagnostics`].
    /// Field is `pub(crate)` so callers can't mutate it after the fact;
    /// the facade installs a sink and drains it into the resulting
    /// Document at the conversion boundary.
    pub(crate) diagnostics: Vec<crate::diagnostics::Warning>,
    /// `true` iff the source document declared encryption
    /// (regardless of whether decryption succeeded). Populated by the
    /// PDF backend at the conversion boundary; default `false` for
    /// formats with no encryption support. Read via
    /// [`Document::is_encrypted`]. Not serialized -- this is
    /// extraction-time state, like [`Document::diagnostics`].
    pub(crate) is_encrypted: bool,
    /// Next NodeId value (for consumers that add nodes).
    /// AtomicU64 so that parallel page extraction can allocate NodeIds
    /// from multiple threads without requiring &mut self.
    next_node_id: AtomicU64,
}

// Manual Clone: AtomicU64 does not derive Clone. We snapshot the current
// counter value; both the original and clone then allocate independently.
//
// WARNING: IDs allocated after cloning may collide across the original and
// the clone. Do not merge overlay data between a Document and its clone if
// both have allocated new NodeIds since the clone was made.
//
// Clone must not run concurrently with alloc_node_id() on the same Document.
impl Clone for Document {
    fn clone(&self) -> Self {
        Self {
            content: self.content.clone(),
            presentation: self.presentation.clone(),
            relationships: self.relationships.clone(),
            metadata: self.metadata.clone(),
            interactions: self.interactions.clone(),
            assets: self.assets.clone(),
            diagnostics: self.diagnostics.clone(),
            is_encrypted: self.is_encrypted,
            next_node_id: AtomicU64::new(self.next_node_id.load(Ordering::Relaxed)),
        }
    }
}

// Custom serde for Document: skip absent (None) layers, add "version": 1.
#[cfg(feature = "serde")]
impl serde::Serialize for Document {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        // Count fields: version + content + metadata + assets always present,
        // plus "images" for backward compat, optional layers only when Some.
        let mut count = 5; // version, content, metadata, assets, images
        if self.presentation.is_some() {
            count += 1;
        }
        if self.relationships.is_some() {
            count += 1;
        }
        if self.interactions.is_some() {
            count += 1;
        }
        let mut map = serializer.serialize_map(Some(count))?;
        map.serialize_entry("version", &1u32)?;
        map.serialize_entry("content", &self.content)?;
        if let Some(ref p) = self.presentation {
            map.serialize_entry("presentation", p)?;
        }
        if let Some(ref r) = self.relationships {
            map.serialize_entry("relationships", r)?;
        }
        map.serialize_entry("metadata", &self.metadata)?;
        if let Some(ref i) = self.interactions {
            map.serialize_entry("interactions", i)?;
        }
        map.serialize_entry("assets", &self.assets)?;
        // Backward compat: serialize "images" key from assets.images().
        map.serialize_entry("images", self.assets.images())?;
        map.end()
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Document {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{MapAccess, Visitor};

        struct DocumentVisitor;

        impl<'de> Visitor<'de> for DocumentVisitor {
            type Value = Document;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a document object with version, content, metadata")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
                // Reset synthetic ID counter so each Document deserialization
                // gets a fresh range (prevents exhaustion in long-running processes).
                table::reset_synthetic_ids();

                let mut content: Option<Vec<Block>> = None;
                let mut presentation: Option<Presentation> = None;
                let mut relationships: Option<Relationships> = None;
                let mut metadata: Option<DocumentMetadata> = None;
                let mut interactions: Option<Interactions> = None;
                let mut assets: Option<AssetStore> = None;
                let mut images: Option<Vec<ImageAsset>> = None;
                let mut version: Option<u32> = None;

                while let Some(key) = access.next_key::<String>()? {
                    match key.as_str() {
                        "version" => {
                            version = Some(access.next_value()?);
                        }
                        "content" => {
                            content = Some(access.next_value()?);
                        }
                        "presentation" => {
                            presentation = Some(access.next_value()?);
                        }
                        "relationships" => {
                            relationships = Some(access.next_value()?);
                        }
                        "metadata" => {
                            metadata = Some(access.next_value()?);
                        }
                        "interactions" => {
                            interactions = Some(access.next_value()?);
                        }
                        "assets" => {
                            assets = Some(access.next_value()?);
                        }
                        "images" => {
                            // Backward compat: accept old "images" key.
                            images = Some(access.next_value()?);
                        }
                        _ => {
                            // Skip unknown fields for forward compat
                            let _ = access.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }

                // Validate version if present. Reject unknown future versions.
                if let Some(v) = version {
                    if v != 1 {
                        return Err(serde::de::Error::custom(format!(
                            "unsupported document version {}, expected 1",
                            v
                        )));
                    }
                }

                let content = content.ok_or_else(|| serde::de::Error::missing_field("content"))?;
                let metadata =
                    metadata.ok_or_else(|| serde::de::Error::missing_field("metadata"))?;

                // Merge: if we have an "assets" key, use it. Otherwise fall back
                // to building an AssetStore from the old "images" array.
                let assets = if let Some(store) = assets {
                    store
                } else {
                    let mut store = AssetStore::new();
                    for img in images.unwrap_or_default() {
                        store.add_image(img);
                    }
                    store
                };

                // Compute next_node_id by scanning ALL NodeIds in the tree
                // (blocks, inlines, list items, table rows, table cells).
                // Depth-limited to prevent stack overflow from pathological input.
                let mut max_id = 0u64;
                fn track_id(id: NodeId, max_id: &mut u64) {
                    // Skip synthetic IDs (used by TableCell deserialization).
                    if id.value() >= table::SYNTHETIC_ID_BASE {
                        return;
                    }
                    if id.value() >= *max_id {
                        *max_id = id.value() + 1;
                    }
                }
                fn scan_inlines(inlines: &[Inline], max_id: &mut u64, depth: usize) {
                    if depth > content::MAX_RECURSION_DEPTH {
                        return;
                    }
                    for inline in inlines {
                        track_id(inline.id(), max_id);
                        if let Inline::Link { content, .. } = inline {
                            scan_inlines(content, max_id, depth + 1);
                        }
                    }
                }
                fn scan_blocks(blocks: &[Block], max_id: &mut u64, depth: usize) {
                    if depth > content::MAX_RECURSION_DEPTH {
                        return;
                    }
                    for block in blocks {
                        track_id(block.id(), max_id);
                        match block {
                            Block::Heading { content, .. } | Block::Paragraph { content, .. } => {
                                scan_inlines(content, max_id, depth + 1);
                            }
                            Block::Section { children, .. } | Block::Shape { children, .. } => {
                                scan_blocks(children, max_id, depth + 1);
                            }
                            Block::List { items, .. } => {
                                for item in items {
                                    track_id(item.id, max_id);
                                    scan_blocks(&item.content, max_id, depth + 1);
                                }
                            }
                            Block::Table { table, .. } => {
                                for row in &table.rows {
                                    track_id(row.id, max_id);
                                    for cell in &row.cells {
                                        track_id(cell.id, max_id);
                                        scan_blocks(&cell.content, max_id, depth + 1);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                scan_blocks(&content, &mut max_id, 0);

                let doc = Document {
                    content,
                    presentation,
                    relationships,
                    metadata,
                    interactions,
                    assets,
                    // Diagnostics are extraction-time state, not part of
                    // the serialized document model. Round-trip yields
                    // empty.
                    diagnostics: Vec::new(),
                    // is_encrypted is extraction-time state, not in the
                    // serialized model. Round-trip yields false.
                    is_encrypted: false,
                    next_node_id: AtomicU64::new(max_id),
                };
                Ok(doc)
            }
        }

        deserializer.deserialize_map(DocumentVisitor)
    }
}

impl Document {
    /// Create an empty document with default metadata and no content.
    pub fn new() -> Self {
        Self {
            content: Vec::new(),
            presentation: None,
            relationships: None,
            metadata: DocumentMetadata::default(),
            interactions: None,
            assets: AssetStore::new(),
            diagnostics: Vec::new(),
            is_encrypted: false,
            next_node_id: AtomicU64::new(0),
        }
    }

    /// Diagnostics collected during extraction.
    ///
    /// Populated by the facade when `Config::collect_diagnostics` is true
    /// (the default). Empty otherwise -- e.g. when a custom
    /// [`crate::diagnostics::DiagnosticsSink`] is the only sink and the
    /// caller has not opted into the Tee mode.
    ///
    /// Cap: respects `Config::limits.max_warnings` (default `Some(1000)`,
    /// per-Document, NOT per-page). When the cap is reached, a single
    /// synthetic [`crate::diagnostics::WarningKind::WarningsTruncated`]
    /// carrying the suppressed count is appended; further warnings are
    /// dropped on the floor with no allocation.
    ///
    /// ```
    /// use udoc_core::document::Document;
    /// let doc = Document::new();
    /// assert!(doc.diagnostics().is_empty());
    /// ```
    pub fn diagnostics(&self) -> &[crate::diagnostics::Warning] {
        &self.diagnostics
    }

    /// Replace the diagnostics buffer wholesale. Intended for the facade
    /// to call at the conversion boundary after draining its internal
    /// sink. Not part of the documented public API (doc-hidden);
    /// downstream code reads via [`Document::diagnostics`]. Marked `pub`
    /// only so that the facade crate `udoc` can populate it from
    /// outside `udoc-core`.
    #[doc(hidden)]
    pub fn set_diagnostics(&mut self, ws: Vec<crate::diagnostics::Warning>) {
        self.diagnostics = ws;
    }

    /// `true` iff the source document declared encryption.
    ///
    /// Set by the backend at the conversion boundary. Independent of
    /// whether decryption *succeeded*: an encrypted PDF that the user
    /// supplied a correct password for produces a Document with
    /// `is_encrypted() == true` and fully-extracted content. Format
    /// backends with no encryption support always return `false`.
    ///
    /// Used by `udoc inspect` (CLI) and the Python `Document.is_encrypted`
    /// property to give downstream code a typed signal without
    /// substring-matching error messages. ( verify-report.md gap #7;
    ///.)
    ///
    /// ```
    /// use udoc_core::document::Document;
    /// let doc = Document::new();
    /// assert!(!doc.is_encrypted());
    /// ```
    pub fn is_encrypted(&self) -> bool {
        self.is_encrypted
    }

    /// Set the encryption flag. Intended for the facade and for
    /// per-format backends to call at the conversion boundary.
    /// Not part of the documented public API (doc-hidden);
    /// downstream code reads via [`Document::is_encrypted`]. Marked
    /// `pub` only so that the facade `udoc` and the per-backend crates
    /// can populate it from outside `udoc-core`.
    #[doc(hidden)]
    pub fn set_is_encrypted(&mut self, encrypted: bool) {
        self.is_encrypted = encrypted;
    }

    /// Return a borrow of the [`Presentation`] overlay if and only if
    /// the document has one AND the named node has any per-node data
    /// in it (geometry, styling, layout, page assignment, or column
    /// specs). Returns `None` when no overlay exists or the node is
    /// not represented in the overlay.
    ///
    ///. The coarse "has any
    /// data?" filter answers UI queries like "show or hide the
    /// presentation panel for the selected node?" -- the consumer
    /// then drills into individual fields on the returned
    /// [`Presentation`] borrow.
    ///
    /// ```
    /// use udoc_core::document::Document;
    /// let doc = Document::new();
    /// // Empty doc has no presentation overlay; accessor returns None.
    /// assert!(doc.presentation_for(doc.alloc_node_id()).is_none());
    /// ```
    pub fn presentation_for(&self, node: NodeId) -> Option<&Presentation> {
        let p = self.presentation.as_ref()?;
        if p.geometry.contains(node)
            || p.text_styling.contains(node)
            || p.block_layout.contains(node)
            || p.column_specs.contains(node)
            || p.layout_info.contains(node)
            || p.page_assignments.contains(node)
        {
            Some(p)
        } else {
            None
        }
    }

    /// Mutable variant of [`Document::presentation_for`].
    pub fn presentation_for_mut(&mut self, node: NodeId) -> Option<&mut Presentation> {
        let has = match self.presentation.as_ref() {
            Some(p) => {
                p.geometry.contains(node)
                    || p.text_styling.contains(node)
                    || p.block_layout.contains(node)
                    || p.column_specs.contains(node)
                    || p.layout_info.contains(node)
                    || p.page_assignments.contains(node)
            }
            None => false,
        };
        if has {
            self.presentation.as_mut()
        } else {
            None
        }
    }

    /// Return a borrow of the [`Relationships`] overlay if and only if
    /// the document has one AND the named node has caption or
    /// component-ref data keyed by it.
    ///
    ///. Other Relationships fields
    /// (footnotes, bookmarks, hyperlinks, toc_entries) are NOT
    /// per-node, so they don't participate in the gating predicate.
    pub fn relationships_for(&self, node: NodeId) -> Option<&Relationships> {
        let r = self.relationships.as_ref()?;
        if r.has_node(node) {
            Some(r)
        } else {
            None
        }
    }

    /// Mutable variant of [`Document::relationships_for`].
    pub fn relationships_for_mut(&mut self, node: NodeId) -> Option<&mut Relationships> {
        let has = self
            .relationships
            .as_ref()
            .is_some_and(|r| r.has_node(node));
        if has {
            self.relationships.as_mut()
        } else {
            None
        }
    }

    /// Return a borrow of the [`Interactions`] overlay if and only if
    /// the document has one AND any form field, comment, or tracked
    /// change is anchored at the named node.
    ///
    pub fn interactions_for(&self, node: NodeId) -> Option<&Interactions> {
        let i = self.interactions.as_ref()?;
        if i.has_node(node) {
            Some(i)
        } else {
            None
        }
    }

    /// Mutable variant of [`Document::interactions_for`].
    pub fn interactions_for_mut(&mut self, node: NodeId) -> Option<&mut Interactions> {
        let has = self.interactions.as_ref().is_some_and(|i| i.has_node(node));
        if has {
            self.interactions.as_mut()
        } else {
            None
        }
    }

    /// Allocate the next NodeId. Thread-safe. Panicking variant.
    ///
    /// Suitable for test code and contexts where node exhaustion is impossible
    /// (e.g., small, known documents). For production code handling untrusted
    /// input, use [`try_alloc_node_id`](Document::try_alloc_node_id) instead
    /// to avoid panics.
    ///
    /// # Panics
    /// Panics if the allocation would exceed `MAX_NODE_ID` (16 million).
    pub fn alloc_node_id(&self) -> NodeId {
        let id = self.next_node_id.fetch_add(1, Ordering::Relaxed);
        assert!(
            id < MAX_NODE_ID,
            "Document: NodeId allocation exceeded maximum ({})",
            MAX_NODE_ID
        );
        NodeId::new(id)
    }

    /// Try to allocate the next NodeId. Thread-safe.
    ///
    /// Returns `None` if the allocation would exceed `MAX_NODE_ID` (16 million).
    pub fn try_alloc_node_id(&self) -> Option<NodeId> {
        loop {
            let current = self.next_node_id.load(Ordering::Relaxed);
            if current >= MAX_NODE_ID {
                return None;
            }
            // CAS loop: atomically check-and-increment so the counter never
            // drifts past MAX_NODE_ID under concurrent access.
            match self.next_node_id.compare_exchange_weak(
                current,
                current + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(id) => return Some(NodeId::new(id)),
                Err(_) => continue, // another thread raced, retry
            }
        }
    }

    /// Set the next_node_id counter (test-only). Used to test boundary
    /// conditions near MAX_NODE_ID without allocating millions of IDs.
    #[cfg(test)]
    pub(crate) fn set_next_node_id_for_test(&self, val: u64) {
        self.next_node_id.store(val, Ordering::Relaxed);
    }

    /// Walk all top-level and nested blocks recursively, depth-first,
    /// calling `f` on each Block. Note: ListItem, TableRow, and TableCell
    /// nodes are not visited directly (they have NodeIds but are not Blocks).
    /// To access those, match on the Block variant and iterate manually.
    pub fn walk<F: FnMut(&Block)>(&self, f: &mut F) {
        fn walk_blocks<F: FnMut(&Block)>(blocks: &[Block], f: &mut F, depth: usize) {
            if depth > content::MAX_RECURSION_DEPTH {
                return;
            }
            for block in blocks {
                f(block);
                match block {
                    Block::Section { children, .. } | Block::Shape { children, .. } => {
                        walk_blocks(children, f, depth + 1)
                    }
                    Block::List { items, .. } => {
                        for item in items {
                            walk_blocks(&item.content, f, depth + 1);
                        }
                    }
                    Block::Table { table, .. } => {
                        for row in &table.rows {
                            for cell in &row.cells {
                                walk_blocks(&cell.content, f, depth + 1);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        walk_blocks(&self.content, f, 0);
    }

    /// Walk all blocks mutably, depth-first.
    pub fn walk_mut<F: FnMut(&mut Block)>(&mut self, f: &mut F) {
        fn walk_blocks_mut<F: FnMut(&mut Block)>(blocks: &mut [Block], f: &mut F, depth: usize) {
            if depth > content::MAX_RECURSION_DEPTH {
                return;
            }
            for block in blocks {
                f(block);
                match block {
                    Block::Section { children, .. } | Block::Shape { children, .. } => {
                        walk_blocks_mut(children, f, depth + 1)
                    }
                    Block::List { items, .. } => {
                        for item in items {
                            walk_blocks_mut(&mut item.content, f, depth + 1);
                        }
                    }
                    Block::Table { table, .. } => {
                        for row in &mut table.rows {
                            for cell in &mut row.cells {
                                walk_blocks_mut(&mut cell.content, f, depth + 1);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        walk_blocks_mut(&mut self.content, f, 0);
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;

#[cfg(all(test, feature = "serde"))]
mod serde_tests {
    use super::*;

    #[test]
    fn node_id_serializes_as_u64() {
        let id = NodeId::new(42);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "42");

        let deserialized: NodeId = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, id);
    }

    #[test]
    fn span_style_sparse_serialization() {
        // Empty style -> {}
        let plain = SpanStyle::default();
        let json = serde_json::to_string(&plain).unwrap();
        assert_eq!(json, "{}");

        // Only true fields present
        let bold_italic = SpanStyle {
            bold: true,
            italic: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&bold_italic).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["bold"], true);
        assert_eq!(val["italic"], true);
        assert!(val.get("underline").is_none());
        assert!(val.get("strikethrough").is_none());
    }

    #[test]
    fn span_style_roundtrip() {
        let style = SpanStyle {
            bold: true,
            italic: false,
            underline: true,
            strikethrough: false,
            superscript: false,
            subscript: true,
        };
        let json = serde_json::to_string(&style).unwrap();
        let deserialized: SpanStyle = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, style);
    }

    #[test]
    fn block_internally_tagged() {
        let block = Block::Heading {
            id: NodeId::new(5),
            level: 2,
            content: vec![Inline::Text {
                id: NodeId::new(6),
                text: "Hello".into(),
                style: SpanStyle::default(),
            }],
        };
        let json = serde_json::to_string(&block).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["type"], "heading");
        assert_eq!(val["id"], 5);
        assert_eq!(val["level"], 2);
    }

    #[test]
    fn block_page_break_tagged() {
        let block = Block::PageBreak { id: NodeId::new(7) };
        let json = serde_json::to_string(&block).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["type"], "page_break");
        assert_eq!(val["id"], 7);
    }

    #[test]
    fn inline_internally_tagged() {
        let inline = Inline::Text {
            id: NodeId::new(8),
            text: "Hello".into(),
            style: SpanStyle {
                bold: true,
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&inline).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["type"], "text");
        assert_eq!(val["id"], 8);
        assert_eq!(val["text"], "Hello");
        assert_eq!(val["style"]["bold"], true);
    }

    #[test]
    fn cell_value_internally_tagged() {
        let num = CellValue::Number(42.0);
        let json = serde_json::to_string(&num).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["type"], "number");

        let formula = CellValue::Formula {
            expression: "=A1+B1".into(),
            result: Some(Box::new(CellValue::Number(42.0))),
        };
        let json = serde_json::to_string(&formula).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["type"], "formula");
        assert_eq!(val["expression"], "=A1+B1");
        assert_eq!(val["result"]["type"], "number");
    }

    #[test]
    fn table_cell_always_uses_full_form() {
        // All cells serialize with content array to preserve inner NodeIds
        // on round-trip (flattened "text" form loses them).
        let cell = TableCell {
            id: NodeId::new(10),
            content: vec![Block::Paragraph {
                id: NodeId::new(11),
                content: vec![Inline::Text {
                    id: NodeId::new(12),
                    text: "hello".into(),
                    style: SpanStyle::default(),
                }],
            }],
            col_span: 1,
            row_span: 1,
            value: None,
        };
        let json = serde_json::to_string(&cell).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            val.get("content").is_some(),
            "should always use content array"
        );
        assert!(
            val.get("text").is_none(),
            "should not use flattened text form"
        );
        // Round-trip preserves inner NodeIds.
        let cell2: TableCell = serde_json::from_str(&json).unwrap();
        assert_eq!(cell2.content[0].id(), NodeId::new(11));
    }

    #[test]
    fn table_cell_full_content() {
        // Complex cell: styled text -> full content array
        let cell = TableCell {
            id: NodeId::new(10),
            content: vec![Block::Paragraph {
                id: NodeId::new(11),
                content: vec![Inline::Text {
                    id: NodeId::new(12),
                    text: "bold".into(),
                    style: SpanStyle {
                        bold: true,
                        ..Default::default()
                    },
                }],
            }],
            col_span: 1,
            row_span: 1,
            value: None,
        };
        let json = serde_json::to_string(&cell).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(val.get("content").is_some());
        assert!(val.get("text").is_none());
    }

    #[test]
    fn overlay_string_keyed() {
        let mut o: Overlay<f64> = Overlay::new();
        o.set(NodeId::new(0), 0.5);
        o.set(NodeId::new(3), 0.9);
        let json = serde_json::to_string(&o).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["0"], 0.5);
        assert_eq!(val["3"], 0.9);
        assert!(val.get("1").is_none());
    }

    #[test]
    fn overlay_roundtrip() {
        let mut o: Overlay<String> = Overlay::new();
        o.set(NodeId::new(1), "hello".into());
        o.set(NodeId::new(5), "world".into());
        let json = serde_json::to_string(&o).unwrap();
        let deserialized: Overlay<String> = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.get(NodeId::new(1)), Some(&"hello".to_string()));
        assert_eq!(deserialized.get(NodeId::new(5)), Some(&"world".to_string()));
        assert_eq!(deserialized.get(NodeId::new(0)), None);
    }

    #[test]
    fn sparse_overlay_string_keyed() {
        let mut s: SparseOverlay<i32> = SparseOverlay::new();
        s.set(NodeId::new(10), 100);
        s.set(NodeId::new(20), 200);
        let json = serde_json::to_string(&s).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["10"], 100);
        assert_eq!(val["20"], 200);
    }

    #[test]
    fn sparse_overlay_roundtrip() {
        let mut s: SparseOverlay<String> = SparseOverlay::new();
        s.set(NodeId::new(100), "x".into());
        let json = serde_json::to_string(&s).unwrap();
        let deserialized: SparseOverlay<String> = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.get(NodeId::new(100)), Some(&"x".to_string()));
    }

    #[test]
    fn overlay_rejects_node_id_exceeding_max() {
        // NodeId exceeding MAX_NODE_ID should produce a serde error, not a panic
        let json = r#"{"17000000": 42}"#;
        let result = serde_json::from_str::<Overlay<i32>>(json);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("exceeds maximum"), "error: {}", err);
    }

    #[test]
    fn sparse_overlay_rejects_node_id_exceeding_max() {
        let json = r#"{"17000000": 42}"#;
        let result = serde_json::from_str::<SparseOverlay<i32>>(json);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("exceeds maximum"), "error: {}", err);
    }

    #[test]
    fn document_serialization_version_and_skip_none_layers() {
        let doc = Document::new();
        let json = serde_json::to_string(&doc).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["version"], 1);
        assert!(val.get("content").is_some());
        assert!(val.get("metadata").is_some());
        // Optional layers absent when None
        assert!(val.get("presentation").is_none());
        assert!(val.get("relationships").is_none());
        assert!(val.get("interactions").is_none());
    }

    #[test]
    fn document_serialization_includes_present_layers() {
        let mut doc = Document::new();
        doc.presentation = Some(Presentation::default());
        let json = serde_json::to_string(&doc).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(val.get("presentation").is_some());
    }

    #[test]
    fn document_roundtrip() {
        let mut doc = Document::new();
        doc.content.push(Block::Paragraph {
            id: NodeId::new(0),
            content: vec![Inline::Text {
                id: NodeId::new(1),
                text: "hello".into(),
                style: SpanStyle::default(),
            }],
        });
        doc.metadata.title = Some("Test".into());
        doc.metadata.page_count = 1;

        let json = serde_json::to_string(&doc).unwrap();
        let deserialized: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.content.len(), 1);
        assert_eq!(deserialized.metadata.title.as_deref(), Some("Test"));
        assert_eq!(deserialized.content[0].text(), "hello");
    }

    #[test]
    fn block_roundtrip_all_variants() {
        // Test that each block variant round-trips through serde
        let blocks = vec![
            Block::PageBreak { id: NodeId::new(0) },
            Block::ThematicBreak { id: NodeId::new(1) },
            Block::CodeBlock {
                id: NodeId::new(2),
                text: "code".into(),
                language: Some("rust".into()),
            },
        ];
        for block in &blocks {
            let json = serde_json::to_string(block).unwrap();
            let deserialized: Block = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized.id(), block.id());
        }
    }
}

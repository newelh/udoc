//! W1-CONVERT: direct PyObject visitor.
//!
//! Walks the udoc `Document` tree and constructs PyObjects in place,
//! avoiding the spike-era JSON roundtrip. No `serde-pyobject` dep.
//!
//! Visitor shape:
//!   - one converter per node kind (`document_to_py`, `block_to_py`,
//!     `inline_to_py`, `table_to_py`, `image_to_py`, `warning_to_py`,
//!     `page_to_py`);
//!   - all return owned `Py<...>` handles so callers can store them in
//!     pyclass fields without lifetime juggling;
//!   - the `Document` itself moves into a `PyDocument` and the children
//!     reference fields off it directly (no shared `Py<PyDocument>`
//!     handle is plumbed through; W1-METHODS-DOCUMENT decides whether
//!     to add lazy overlay lookup later -- the value pyclasses are
//!     owned snapshots).

use std::path::PathBuf;
use std::sync::OnceLock;

use pyo3::prelude::*;
use udoc_facade::page::ImageFilter;
use udoc_facade::{
    AssetStore, Block, BoundingBox, Document, DocumentMetadata, ImageAsset, Inline, ListKind,
    SectionRole, ShapeKind, TableCell, TableData, TableRow, Warning, WarningLevel,
};

use crate::chunks::PyBoundingBox;
use crate::document::PyDocument;
use crate::types::{
    PyBlock, PyDocumentMetadata, PyFormat, PyImage, PyInline, PyPage, PyTable, PyTableCell,
    PyTableRow, PyWarning,
};

// ---------------------------------------------------------------------------
// Top-level entry point.
// ---------------------------------------------------------------------------

/// Convert an owned Rust `Document` into a `Py<PyDocument>` handle.
///
/// `source` is the path the document was extracted from (None for
/// `extract_bytes`). `format` is the detected/forced format.
pub fn document_to_py(
    py: Python<'_>,
    doc: Document,
    source: Option<PathBuf>,
    format: Option<udoc_facade::Format>,
) -> PyResult<Py<PyDocument>> {
    let py_doc = PyDocument {
        inner: doc,
        source,
        format: format.map(PyFormat::from_rust),
        cached_pages: OnceLock::new(),
        cached_blocks: OnceLock::new(),
        cached_warnings: OnceLock::new(),
        cached_metadata: OnceLock::new(),
    };
    Py::new(py, py_doc)
}

// ---------------------------------------------------------------------------
// Metadata.
// ---------------------------------------------------------------------------

/// Convert a `DocumentMetadata` into the matching pyclass.
pub fn metadata_to_py(py: Python<'_>, meta: &DocumentMetadata) -> PyResult<Py<PyDocumentMetadata>> {
    Py::new(
        py,
        PyDocumentMetadata {
            title: meta.title.clone(),
            author: meta.author.clone(),
            subject: meta.subject.clone(),
            creator: meta.creator.clone(),
            producer: meta.producer.clone(),
            creation_date: meta.creation_date.clone(),
            modification_date: meta.modification_date.clone(),
            page_count: meta.page_count,
            properties: meta.properties.clone(),
        },
    )
}

// ---------------------------------------------------------------------------
// Inline span.
// ---------------------------------------------------------------------------

/// Convert an `Inline` node into a `Py<PyInline>`.
pub fn inline_to_py(py: Python<'_>, inline: &Inline) -> PyResult<Py<PyInline>> {
    let span = match inline {
        Inline::Text { id, text, style } => PyInline {
            kind: "text".into(),
            node_id: id.value(),
            text: Some(text.clone()),
            bold: style.bold,
            italic: style.italic,
            underline: style.underline,
            strikethrough: style.strikethrough,
            superscript: style.superscript,
            subscript: style.subscript,
            url: None,
            content: vec![],
            label: None,
            alt_text: None,
            image_index: None,
        },
        Inline::Code { id, text } => PyInline {
            kind: "code".into(),
            node_id: id.value(),
            text: Some(text.clone()),
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            superscript: false,
            subscript: false,
            url: None,
            content: vec![],
            label: None,
            alt_text: None,
            image_index: None,
        },
        Inline::Link { id, url, content } => {
            let mut child_pys = Vec::with_capacity(content.len());
            for child in content {
                child_pys.push(inline_to_py(py, child)?);
            }
            PyInline {
                kind: "link".into(),
                node_id: id.value(),
                text: None,
                bold: false,
                italic: false,
                underline: false,
                strikethrough: false,
                superscript: false,
                subscript: false,
                url: Some(url.clone()),
                content: child_pys,
                label: None,
                alt_text: None,
                image_index: None,
            }
        }
        Inline::FootnoteRef { id, label } => PyInline {
            kind: "footnote_ref".into(),
            node_id: id.value(),
            text: None,
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            superscript: false,
            subscript: false,
            url: None,
            content: vec![],
            label: Some(label.clone()),
            alt_text: None,
            image_index: None,
        },
        Inline::InlineImage {
            id,
            image_ref,
            alt_text,
        } => PyInline {
            kind: "inline_image".into(),
            node_id: id.value(),
            text: None,
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            superscript: false,
            subscript: false,
            url: None,
            content: vec![],
            label: None,
            alt_text: alt_text.clone(),
            image_index: Some(image_ref.index()),
        },
        Inline::SoftBreak { id } => PyInline {
            kind: "soft_break".into(),
            node_id: id.value(),
            text: None,
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            superscript: false,
            subscript: false,
            url: None,
            content: vec![],
            label: None,
            alt_text: None,
            image_index: None,
        },
        Inline::LineBreak { id } => PyInline {
            kind: "line_break".into(),
            node_id: id.value(),
            text: None,
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            superscript: false,
            subscript: false,
            url: None,
            content: vec![],
            label: None,
            alt_text: None,
            image_index: None,
        },
        // The Inline enum is #[non_exhaustive]. New variants added in
        // future udoc-core versions degrade to a generic "text"-shaped
        // span with empty payload until W1-METHODS-TYPES grows a
        // first-class branch.
        _ => PyInline {
            kind: "text".into(),
            node_id: 0,
            text: Some(String::new()),
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            superscript: false,
            subscript: false,
            url: None,
            content: vec![],
            label: None,
            alt_text: None,
            image_index: None,
        },
    };
    Py::new(py, span)
}

/// Concatenate plain text out of an inline tree (helper for paragraph /
/// heading text reductions).
pub fn extract_inline_text(spans: &[Inline]) -> String {
    let mut out = String::new();
    walk_inline_text(spans, &mut out);
    out
}

fn walk_inline_text(spans: &[Inline], out: &mut String) {
    for span in spans {
        match span {
            Inline::Text { text, .. } | Inline::Code { text, .. } => out.push_str(text),
            Inline::Link { content, .. } => walk_inline_text(content, out),
            Inline::SoftBreak { .. } => out.push(' '),
            Inline::LineBreak { .. } => out.push('\n'),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Block.
// ---------------------------------------------------------------------------

/// Convert a `Block` node into a `Py<PyBlock>`.
///
/// `assets` is threaded through every recursion level so W1-METHODS-DOCUMENT
/// can swap the foundation's "store the image_index, dereference later"
/// strategy for inline asset resolution without changing the visitor
/// surface.
#[allow(clippy::only_used_in_recursion)]
pub fn block_to_py(py: Python<'_>, block: &Block, assets: &AssetStore) -> PyResult<Py<PyBlock>> {
    let py_block = match block {
        Block::Paragraph { id, content } => {
            let spans = inline_vec_to_py(py, content)?;
            PyBlock {
                kind: "paragraph".into(),
                node_id: id.value(),
                text: Some(extract_inline_text(content)),
                level: None,
                spans,
                table: None,
                list_kind: None,
                list_start: None,
                items: vec![],
                language: None,
                image_index: None,
                alt_text: None,
                section_role: None,
                shape_kind: None,
                children: vec![],
            }
        }
        Block::Heading { id, level, content } => {
            let spans = inline_vec_to_py(py, content)?;
            PyBlock {
                kind: "heading".into(),
                node_id: id.value(),
                text: Some(extract_inline_text(content)),
                level: Some(u32::from(*level)),
                spans,
                table: None,
                list_kind: None,
                list_start: None,
                items: vec![],
                language: None,
                image_index: None,
                alt_text: None,
                section_role: None,
                shape_kind: None,
                children: vec![],
            }
        }
        Block::Table { id, table } => {
            let py_table = table_to_py(py, *id, table)?;
            PyBlock {
                kind: "table".into(),
                node_id: id.value(),
                text: None,
                level: None,
                spans: vec![],
                table: Some(py_table),
                list_kind: None,
                list_start: None,
                items: vec![],
                language: None,
                image_index: None,
                alt_text: None,
                section_role: None,
                shape_kind: None,
                children: vec![],
            }
        }
        Block::List {
            id,
            items,
            kind,
            start,
        } => {
            let mut item_pys: Vec<Vec<Py<PyBlock>>> = Vec::with_capacity(items.len());
            for item in items {
                let mut item_blocks = Vec::with_capacity(item.content.len());
                for child in &item.content {
                    item_blocks.push(block_to_py(py, child, assets)?);
                }
                item_pys.push(item_blocks);
            }
            PyBlock {
                kind: "list".into(),
                node_id: id.value(),
                text: None,
                level: None,
                spans: vec![],
                table: None,
                list_kind: Some(match kind {
                    ListKind::Ordered => "ordered".into(),
                    ListKind::Unordered => "unordered".into(),
                    // ListKind is #[non_exhaustive].
                    _ => "unknown".into(),
                }),
                list_start: Some(*start),
                items: item_pys,
                language: None,
                image_index: None,
                alt_text: None,
                section_role: None,
                shape_kind: None,
                children: vec![],
            }
        }
        Block::CodeBlock { id, text, language } => PyBlock {
            kind: "code_block".into(),
            node_id: id.value(),
            text: Some(text.clone()),
            level: None,
            spans: vec![],
            table: None,
            list_kind: None,
            list_start: None,
            items: vec![],
            language: language.clone(),
            image_index: None,
            alt_text: None,
            section_role: None,
            shape_kind: None,
            children: vec![],
        },
        Block::Image {
            id,
            image_ref,
            alt_text,
        } => PyBlock {
            kind: "image".into(),
            node_id: id.value(),
            text: None,
            level: None,
            spans: vec![],
            table: None,
            list_kind: None,
            list_start: None,
            items: vec![],
            language: None,
            image_index: Some(image_ref.index()),
            alt_text: alt_text.clone(),
            section_role: None,
            shape_kind: None,
            children: vec![],
        },
        Block::PageBreak { id } => PyBlock {
            kind: "page_break".into(),
            node_id: id.value(),
            text: None,
            level: None,
            spans: vec![],
            table: None,
            list_kind: None,
            list_start: None,
            items: vec![],
            language: None,
            image_index: None,
            alt_text: None,
            section_role: None,
            shape_kind: None,
            children: vec![],
        },
        Block::ThematicBreak { id } => PyBlock {
            kind: "thematic_break".into(),
            node_id: id.value(),
            text: None,
            level: None,
            spans: vec![],
            table: None,
            list_kind: None,
            list_start: None,
            items: vec![],
            language: None,
            image_index: None,
            alt_text: None,
            section_role: None,
            shape_kind: None,
            children: vec![],
        },
        Block::Section { id, role, children } => {
            let mut child_pys = Vec::with_capacity(children.len());
            for child in children {
                child_pys.push(block_to_py(py, child, assets)?);
            }
            PyBlock {
                kind: "section".into(),
                node_id: id.value(),
                text: None,
                level: None,
                spans: vec![],
                table: None,
                list_kind: None,
                list_start: None,
                items: vec![],
                language: None,
                image_index: None,
                alt_text: None,
                section_role: role.as_ref().map(section_role_str),
                shape_kind: None,
                children: child_pys,
            }
        }
        Block::Shape {
            id,
            kind,
            children,
            alt_text,
        } => {
            let mut child_pys = Vec::with_capacity(children.len());
            for child in children {
                child_pys.push(block_to_py(py, child, assets)?);
            }
            PyBlock {
                kind: "shape".into(),
                node_id: id.value(),
                text: None,
                level: None,
                spans: vec![],
                table: None,
                list_kind: None,
                list_start: None,
                items: vec![],
                language: None,
                image_index: None,
                alt_text: alt_text.clone(),
                section_role: None,
                shape_kind: Some(shape_kind_str(kind)),
                children: child_pys,
            }
        }
        // Block is #[non_exhaustive]; future variants degrade to an
        // empty paragraph rather than panicking.
        _ => PyBlock {
            kind: "paragraph".into(),
            node_id: 0,
            text: Some(String::new()),
            level: None,
            spans: vec![],
            table: None,
            list_kind: None,
            list_start: None,
            items: vec![],
            language: None,
            image_index: None,
            alt_text: None,
            section_role: None,
            shape_kind: None,
            children: vec![],
        },
    };
    Py::new(py, py_block)
}

fn inline_vec_to_py(py: Python<'_>, content: &[Inline]) -> PyResult<Vec<Py<PyInline>>> {
    let mut out = Vec::with_capacity(content.len());
    for inline in content {
        out.push(inline_to_py(py, inline)?);
    }
    Ok(out)
}

fn section_role_str(role: &SectionRole) -> String {
    match role {
        SectionRole::Generic => "generic".into(),
        SectionRole::Article => "article".into(),
        SectionRole::Navigation => "navigation".into(),
        SectionRole::Complementary => "complementary".into(),
        SectionRole::Header => "header".into(),
        SectionRole::Footer => "footer".into(),
        SectionRole::Main => "main".into(),
        SectionRole::HeadersFooters => "headers_footers".into(),
        SectionRole::Footnotes => "footnotes".into(),
        SectionRole::Endnotes => "endnotes".into(),
        SectionRole::Notes => "notes".into(),
        SectionRole::Blockquote => "blockquote".into(),
        SectionRole::Named(s) => format!("named:{s}"),
        // SectionRole is #[non_exhaustive].
        _ => "unknown".into(),
    }
}

fn shape_kind_str(kind: &ShapeKind) -> String {
    match kind {
        ShapeKind::Rectangle => "rectangle".into(),
        ShapeKind::Ellipse => "ellipse".into(),
        ShapeKind::Polygon => "polygon".into(),
        ShapeKind::Path => "path".into(),
        ShapeKind::Line => "line".into(),
        ShapeKind::Group => "group".into(),
        ShapeKind::Frame => "frame".into(),
        ShapeKind::Canvas => "canvas".into(),
        ShapeKind::Custom(s) => format!("custom:{s}"),
        // ShapeKind is #[non_exhaustive].
        _ => "unknown".into(),
    }
}

// ---------------------------------------------------------------------------
// Tables.
// ---------------------------------------------------------------------------

/// Convert a `TableData` (carried inside `Block::Table`) plus the parent
/// block id into a `Py<PyTable>`. `node_id` is the parent block's id so
/// callers can route overlay lookups (presentation bbox, etc.).
pub fn table_to_py(
    py: Python<'_>,
    node_id: udoc_facade::NodeId,
    table: &TableData,
) -> PyResult<Py<PyTable>> {
    let mut rows = Vec::with_capacity(table.rows.len());
    for row in &table.rows {
        rows.push(table_row_to_py(py, row)?);
    }
    Py::new(
        py,
        PyTable {
            node_id: node_id.value(),
            rows,
            num_columns: table.num_columns,
            header_row_count: table.header_row_count,
            has_header_row: table.header_row_count > 0,
            may_continue_from_previous: table.may_continue_from_previous,
            may_continue_to_next: table.may_continue_to_next,
        },
    )
}

fn table_row_to_py(py: Python<'_>, row: &TableRow) -> PyResult<Py<PyTableRow>> {
    let mut cells = Vec::with_capacity(row.cells.len());
    for cell in &row.cells {
        cells.push(table_cell_to_py(py, cell)?);
    }
    Py::new(
        py,
        PyTableRow {
            node_id: row.id.value(),
            cells,
            is_header: row.is_header,
        },
    )
}

fn table_cell_to_py(py: Python<'_>, cell: &TableCell) -> PyResult<Py<PyTableCell>> {
    // Convert content blocks (the cell can hold paragraphs, lists, even
    // nested tables). We don't have an `assets` handle here -- inline
    // images inside table cells route through `block_to_py` which only
    // needs the asset store for `image_index` lookups, which are by-index
    // and don't dereference the bytes. We pass an empty AssetStore: the
    // index is preserved and W1-METHODS-DOCUMENT can resolve later via
    // the parent Document handle.
    let empty_store = AssetStore::default();
    let mut content = Vec::with_capacity(cell.content.len());
    for block in &cell.content {
        content.push(block_to_py(py, block, &empty_store)?);
    }
    Py::new(
        py,
        PyTableCell {
            node_id: cell.id.value(),
            text: cell.text(),
            content,
            col_span: cell.col_span,
            row_span: cell.row_span,
            value: cell.value.as_ref().map(|v| format!("{v:?}")),
        },
    )
}

// ---------------------------------------------------------------------------
// Images.
// ---------------------------------------------------------------------------

/// Convert a `Block::Image` placement (resolved against the asset store)
/// into a `Py<PyImage>` carrying the encoded bytes + dimensions.
///
/// Returns Ok(None) if the block is not an image variant or the image
/// asset cannot be resolved (e.g. the asset store was scrubbed).
pub fn image_to_py(
    py: Python<'_>,
    block: &Block,
    assets: &AssetStore,
) -> PyResult<Option<Py<PyImage>>> {
    let (node_id, image_ref, alt_text) = match block {
        Block::Image {
            id,
            image_ref,
            alt_text,
        } => (id.value(), *image_ref, alt_text.clone()),
        _ => return Ok(None),
    };
    let Some(asset) = assets.image(image_ref) else {
        return Ok(None);
    };
    Ok(Some(image_asset_to_py(
        py,
        node_id,
        image_ref.index(),
        asset,
        alt_text,
        None,
    )?))
}

/// Build a `Py<PyImage>` from the resolved fields. Used by
/// `image_to_py` (block placement) and by W1-METHODS-DOCUMENT's
/// `images()` iterator.
pub fn image_asset_to_py(
    py: Python<'_>,
    node_id: u64,
    asset_index: usize,
    asset: &ImageAsset,
    alt_text: Option<String>,
    bbox: Option<BoundingBox>,
) -> PyResult<Py<PyImage>> {
    let bbox_py = match bbox {
        Some(bb) => Some(Py::new(py, PyBoundingBox::from_rust(bb))?),
        None => None,
    };
    Py::new(
        py,
        PyImage {
            node_id,
            asset_index,
            width: asset.width,
            height: asset.height,
            bits_per_component: asset.bits_per_component,
            filter: image_filter_str(asset.filter),
            data: asset.data.clone(),
            alt_text,
            bbox: bbox_py,
        },
    )
}

fn image_filter_str(filter: ImageFilter) -> String {
    match filter {
        ImageFilter::Jpeg => "jpeg".into(),
        ImageFilter::Jpeg2000 => "jpeg2000".into(),
        ImageFilter::Png => "png".into(),
        ImageFilter::Tiff => "tiff".into(),
        ImageFilter::Jbig2 => "jbig2".into(),
        ImageFilter::Ccitt => "ccitt".into(),
        ImageFilter::Gif => "gif".into(),
        ImageFilter::Bmp => "bmp".into(),
        ImageFilter::Emf => "emf".into(),
        ImageFilter::Wmf => "wmf".into(),
        ImageFilter::Raw => "raw".into(),
        // ImageFilter is #[non_exhaustive].
        _ => "unknown".into(),
    }
}

// ---------------------------------------------------------------------------
// Diagnostics.
// ---------------------------------------------------------------------------

/// Convert a `Warning` into a `Py<PyWarning>`.
pub fn warning_to_py(py: Python<'_>, warning: &Warning) -> PyResult<Py<PyWarning>> {
    Py::new(
        py,
        PyWarning {
            kind: warning.kind.as_str().to_string(),
            level: match warning.level {
                WarningLevel::Info => "info".into(),
                WarningLevel::Warning => "warning".into(),
                // WarningLevel is #[non_exhaustive].
                _ => "warning".into(),
            },
            message: warning.message.clone(),
            offset: warning.offset,
            page_index: warning.context.page_index,
            detail: warning.context.detail.clone(),
        },
    )
}

// ---------------------------------------------------------------------------
// Pages.
// ---------------------------------------------------------------------------

/// Convert a logical page into a `Py<PyPage>`.
///
/// The udoc Document model has no first-class `Page` type; pages are a
/// derived concept. The W1-FOUNDATION shape: one PyPage per integer in
/// `[0, doc.metadata.page_count)` carrying *all* top-level blocks (the
/// per-page partition is overlay-driven and lands in W1-METHODS-DOCUMENT
/// when it walks the presentation overlay's `PageDef` map).
///
/// Callers walking pages will see redundant blocks across pages today;
/// that is acceptable for the foundation because every other entry
/// point that exposes pages goes through this function and W1-METHODS-*
/// can replace its body without churning the call sites.
pub fn page_to_py(py: Python<'_>, doc: &Document, page_index: usize) -> PyResult<Py<PyPage>> {
    let mut blocks = Vec::with_capacity(doc.content.len());
    for block in &doc.content {
        blocks.push(block_to_py(py, block, &doc.assets)?);
    }
    Py::new(
        py,
        PyPage {
            index: page_index,
            blocks,
        },
    )
}

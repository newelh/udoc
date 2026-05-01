//! Shared conversion helpers for building the Document model.
//!
//! These utilities are used by both the facade's format-specific converters
//! and by backend crates that implement their own `into_document` functions
//! (e.g., DOCX). Centralizing them here avoids duplication across crates.

use std::collections::HashSet;

use crate::diagnostics::{DiagnosticsSink, Warning};
use crate::document::*;
use crate::error::{Error, Result};

/// Allocate a NodeId, returning an error instead of panicking on exhaustion.
pub fn alloc_id(doc: &Document) -> Result<NodeId> {
    doc.try_alloc_node_id()
        .ok_or_else(|| Error::new("NodeId allocation limit exceeded"))
}

/// Create a single `Inline::Text` node with a fresh NodeId.
pub fn text_inline(doc: &Document, text: String) -> Result<Inline> {
    let id = alloc_id(doc)?;
    Ok(Inline::Text {
        id,
        text,
        style: SpanStyle::default(),
    })
}

/// Create a Paragraph block containing a single plain-text span.
pub fn text_paragraph(doc: &Document, text: String) -> Result<Block> {
    let id = alloc_id(doc)?;
    let inline = text_inline(doc, text)?;
    Ok(Block::Paragraph {
        id,
        content: vec![inline],
    })
}

/// Convert core table rows/cells into document model TableRows.
///
/// Each cell's text becomes a single-paragraph block. Col/row spans
/// and header flags are copied from the source table.
pub fn convert_table_rows(doc: &Document, table: &crate::table::Table) -> Result<Vec<TableRow>> {
    table
        .rows
        .iter()
        .map(|row| {
            let row_id = alloc_id(doc)?;
            let cells: Vec<TableCell> = row
                .cells
                .iter()
                .map(|cell| {
                    let cell_id = alloc_id(doc)?;
                    let content = vec![text_paragraph(doc, cell.text.clone())?];
                    let mut tc = TableCell::new(cell_id, content);
                    tc.col_span = cell.col_span;
                    tc.row_span = cell.row_span;
                    Ok(tc)
                })
                .collect::<Result<Vec<TableCell>>>()?;
            let mut tr = TableRow::new(row_id, cells);
            tr.is_header = row.is_header;
            Ok(tr)
        })
        .collect()
}

/// Build a TableData from converted rows and a source core::table::Table.
///
/// Copies num_columns and header_row_count from the source table.
pub fn build_table_data(rows: Vec<TableRow>, source: &crate::table::Table) -> TableData {
    let mut td = TableData::new(rows);
    td.num_columns = source.num_columns;
    td.header_row_count = source.header_row_count;
    td
}

/// Forward parse warnings to the diagnostics sink.
///
/// Used by backends that accumulate warnings as `Vec<String>` internally
/// (DOCX, RTF, Markdown). The `kind` parameter is the warning kind string
/// (e.g., "RtfParse", "DocxParse").
pub fn propagate_warnings(warnings: &[String], diag: &dyn DiagnosticsSink, kind: &str) {
    for msg in warnings {
        diag.warning(Warning::new(kind, msg.as_str()));
    }
}

/// Convert twips to points (1 point = 20 twips).
///
/// Used by OOXML (DOCX) and RTF parsers for spacing/indentation values.
pub const fn twips_to_points(twips: f64) -> f64 {
    twips / 20.0
}

/// Insert a PageBreak block if the document already has content.
///
/// Used by multi-page/sheet formats (PDF, PPTX, XLSX) to separate pages.
pub fn maybe_insert_page_break(doc: &mut Document) -> Result<()> {
    if !doc.content.is_empty() {
        let break_id = alloc_id(doc)?;
        doc.content.push(Block::PageBreak { id: break_id });
    }
    Ok(())
}

/// Convert core tables into Block::Table and push them onto `doc.content`.
///
/// Shared by non-PDF converters (RTF, PPTX, XLSX). PDF needs per-table
/// y_top sorting and presentation overlay, so it uses its own loop.
pub fn push_tables(doc: &mut Document, tables: &[crate::table::Table]) -> Result<()> {
    for table in tables {
        let table_id = alloc_id(doc)?;
        let rows = convert_table_rows(doc, table)?;
        let td = build_table_data(rows, table);
        doc.content.push(Block::Table {
            id: table_id,
            table: td,
        });
    }
    Ok(())
}

/// Push a named section (footnotes, endnotes, notes, etc.) if non-empty.
///
/// Shared pattern: allocate a section NodeId, wrap blocks in a
/// `Block::Section` with a role mapped from `role` via
/// [`SectionRole::from_name`], and push onto `doc.content`. Does nothing
/// if `blocks` is empty.
///
/// Since #146 the string `role` is mapped through `SectionRole::from_name`
/// so common values ("footnotes", "endnotes", "headers-footers", "notes",
/// "blockquote") produce typed variants instead of `SectionRole::Named`.
pub fn push_named_section(doc: &mut Document, role: &str, blocks: Vec<Block>) -> Result<()> {
    if !blocks.is_empty() {
        let section_id = alloc_id(doc)?;
        doc.content.push(Block::Section {
            id: section_id,
            role: Some(SectionRole::from_name(role)),
            children: blocks,
        });
    }
    Ok(())
}

/// Set extended text styling on a node if any fields are present.
///
/// Handles the common pattern: check if styling is needed, ensure the
/// Presentation overlay exists, store it.
/// Returns `true` if styling was applied.
pub fn set_text_styling(doc: &mut Document, node_id: NodeId, style: ExtendedTextStyle) -> bool {
    if style.is_empty() {
        return false;
    }
    let pres = doc.presentation.get_or_insert_with(Presentation::default);
    pres.text_styling.set(node_id, style);
    true
}

/// Register a hyperlink URL on the `Relationships` overlay, deduplicating
/// via the caller-provided `seen` set.
///
/// This is the single-pass replacement for walking the finished content tree
/// to collect `Inline::Link` URLs (#142). Each backend that emits
/// `Inline::Link` calls this helper at construction time, passing a
/// `HashSet<String>` created once in its `*_to_document` entry point.
///
/// Empty URLs and already-seen URLs are silently ignored. Otherwise the
/// URL is inserted into the [`Relationships::hyperlinks`] list (bounded by
/// `MAX_HYPERLINKS`). Returns `true` when the URL was newly added.
pub fn register_hyperlink(doc: &mut Document, seen: &mut HashSet<String>, url: &str) -> bool {
    if url.is_empty() {
        return false;
    }
    if !seen.insert(url.to_string()) {
        return false;
    }
    let rels = doc.relationships.get_or_insert_with(Relationships::default);
    rels.add_hyperlink(url.to_string())
}

/// Narrow per-run shape that DOCX / PPTX / RTF paragraphs share after
/// extracting text + styling + optional hyperlink from their per-format
/// run type. Consumed by [`push_run_inline`] (#143).
pub struct RunData<'a> {
    pub text: &'a str,
    pub style: SpanStyle,
    pub extended: ExtendedTextStyle,
    pub hyperlink_url: Option<&'a str>,
}

/// Emit a run as `Inline::Text` (or hyperlink-wrapped `Inline::Link`),
/// wiring text styling and deduping the hyperlink URL into `Relationships`.
/// Shared by DOCX/PPTX/RTF `paragraph_to_inlines` (#143).
pub fn push_run_inline(
    doc: &mut Document,
    seen_urls: &mut HashSet<String>,
    out: &mut Vec<Inline>,
    run: RunData<'_>,
) -> Result<()> {
    let inline_id = alloc_id(doc)?;
    set_text_styling(doc, inline_id, run.extended);

    if let Some(url) = run.hyperlink_url {
        let link_id = alloc_id(doc)?;
        register_hyperlink(doc, seen_urls, url);
        out.push(Inline::Link {
            id: link_id,
            url: url.to_string(),
            content: vec![Inline::Text {
                id: inline_id,
                text: run.text.to_string(),
                style: run.style,
            }],
        });
    } else {
        out.push(Inline::Text {
            id: inline_id,
            text: run.text.to_string(),
            style: run.style,
        });
    }
    Ok(())
}

/// Set block-level layout on a node if any fields are present.
///
/// Handles the common pattern: check if layout is needed, ensure the
/// Presentation overlay exists, store it.
/// Returns `true` if layout was applied.
pub fn set_block_layout(doc: &mut Document, block_id: NodeId, layout: BlockLayout) -> bool {
    if layout.is_empty() {
        return false;
    }
    let pres = doc.presentation.get_or_insert_with(Presentation::default);
    pres.block_layout.set(block_id, layout);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::CollectingDiagnostics;
    use crate::document::Document;
    use crate::table::{Table, TableCell as CoreTableCell, TableRow as CoreTableRow};

    #[test]
    fn alloc_id_succeeds() {
        let doc = Document::new();
        let id = alloc_id(&doc).expect("allocation should succeed");
        assert_eq!(id.value(), 0);

        let id2 = alloc_id(&doc).expect("second allocation should succeed");
        assert_eq!(id2.value(), 1);
    }

    #[test]
    fn text_inline_creates_text_with_correct_fields() {
        let doc = Document::new();
        let inline = text_inline(&doc, "hello world".into()).expect("should succeed");
        match &inline {
            Inline::Text { text, style, .. } => {
                assert_eq!(text, "hello world");
                assert!(style.is_plain(), "style should be default/plain");
            }
            other => panic!("expected Inline::Text, got {:?}", other),
        }
    }

    #[test]
    fn text_paragraph_creates_paragraph_with_single_text() {
        let doc = Document::new();
        let block = text_paragraph(&doc, "paragraph text".into()).expect("should succeed");
        match &block {
            Block::Paragraph { content, .. } => {
                assert_eq!(content.len(), 1);
                assert_eq!(content[0].text(), "paragraph text");
            }
            other => panic!("expected Block::Paragraph, got {:?}", other),
        }
    }

    #[test]
    fn text_paragraph_allocates_two_node_ids() {
        let doc = Document::new();
        let block = text_paragraph(&doc, "x".into()).expect("should succeed");
        // Paragraph gets one id, inline text gets another.
        let para_id = block.id();
        let inline_id = match &block {
            Block::Paragraph { content, .. } => content[0].id(),
            _ => panic!("expected Paragraph"),
        };
        assert_ne!(para_id, inline_id);
    }

    #[test]
    fn convert_table_rows_empty_table() {
        let doc = Document::new();
        let table = Table::new(vec![], None);
        let rows = convert_table_rows(&doc, &table).expect("should succeed");
        assert!(rows.is_empty());
    }

    #[test]
    fn convert_table_rows_single_cell() {
        let doc = Document::new();
        let table = Table::new(
            vec![CoreTableRow::new(vec![CoreTableCell::new(
                "cell".into(),
                None,
            )])],
            None,
        );
        let rows = convert_table_rows(&doc, &table).expect("should succeed");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cells.len(), 1);
        assert_eq!(rows[0].cells[0].text(), "cell");
        assert_eq!(rows[0].cells[0].col_span, 1);
        assert_eq!(rows[0].cells[0].row_span, 1);
        assert!(!rows[0].is_header);
    }

    #[test]
    fn convert_table_rows_2x2_with_spans() {
        let doc = Document::new();
        let mut header_row = CoreTableRow::new(vec![
            CoreTableCell::with_spans("merged".into(), None, 2, 1),
            CoreTableCell::with_spans("tall".into(), None, 1, 2),
        ]);
        header_row.is_header = true;

        let data_row = CoreTableRow::new(vec![
            CoreTableCell::new("a".into(), None),
            CoreTableCell::new("b".into(), None),
        ]);

        let table = Table::new(vec![header_row, data_row], None);
        let rows = convert_table_rows(&doc, &table).expect("should succeed");

        assert_eq!(rows.len(), 2);
        // Header row
        assert!(rows[0].is_header);
        assert_eq!(rows[0].cells.len(), 2);
        assert_eq!(rows[0].cells[0].col_span, 2);
        assert_eq!(rows[0].cells[0].row_span, 1);
        assert_eq!(rows[0].cells[0].text(), "merged");
        assert_eq!(rows[0].cells[1].col_span, 1);
        assert_eq!(rows[0].cells[1].row_span, 2);
        assert_eq!(rows[0].cells[1].text(), "tall");
        // Data row
        assert!(!rows[1].is_header);
        assert_eq!(rows[1].cells.len(), 2);
        assert_eq!(rows[1].cells[0].text(), "a");
        assert_eq!(rows[1].cells[1].text(), "b");
    }

    #[test]
    fn build_table_data_copies_source_fields() {
        let doc = Document::new();
        let mut header = CoreTableRow::new(vec![CoreTableCell::new("H1".into(), None)]);
        header.is_header = true;
        let source = Table::new(
            vec![
                header,
                CoreTableRow::new(vec![CoreTableCell::new("D1".into(), None)]),
            ],
            None,
        );
        let rows = convert_table_rows(&doc, &source).expect("should succeed");
        let td = build_table_data(rows, &source);

        assert_eq!(td.num_columns, source.num_columns);
        assert_eq!(td.header_row_count, source.header_row_count);
        assert_eq!(td.rows.len(), 2);
    }

    #[test]
    fn propagate_warnings_empty_list() {
        let diag = CollectingDiagnostics::new();
        propagate_warnings(&[], &diag, "TestParse");
        assert!(diag.warnings().is_empty());
    }

    #[test]
    fn propagate_warnings_multiple() {
        let diag = CollectingDiagnostics::new();
        let warnings = vec![
            "first warning".to_string(),
            "second warning".to_string(),
            "third warning".to_string(),
        ];
        propagate_warnings(&warnings, &diag, "DocxParse");
        let collected = diag.warnings();
        assert_eq!(collected.len(), 3);
        assert_eq!(collected[0].kind, "DocxParse");
        assert_eq!(collected[0].message, "first warning");
        assert_eq!(collected[1].message, "second warning");
        assert_eq!(collected[2].message, "third warning");
        // All should have the same kind
        for w in &collected {
            assert_eq!(w.kind, "DocxParse");
        }
    }

    #[test]
    fn set_text_styling_all_none_does_nothing() {
        let mut doc = Document::new();
        let id = alloc_id(&doc).expect("alloc should succeed");
        let applied = set_text_styling(&mut doc, id, ExtendedTextStyle::default());
        assert!(!applied);
        assert!(doc.presentation.is_none());
    }

    #[test]
    fn set_text_styling_single_field_creates_overlay() {
        let mut doc = Document::new();
        let id = alloc_id(&doc).expect("alloc should succeed");
        let applied = set_text_styling(
            &mut doc,
            id,
            ExtendedTextStyle {
                font_name: Some("Helvetica".into()),
                ..Default::default()
            },
        );
        assert!(applied);
        let pres = doc
            .presentation
            .as_ref()
            .expect("presentation should exist");
        let style = pres
            .text_styling
            .get(id)
            .expect("styling should exist for node");
        assert_eq!(style.font_name.as_deref(), Some("Helvetica"));
        assert!(style.font_size.is_none());
        assert!(style.color.is_none());
        assert!(style.background_color.is_none());
        assert!(style.letter_spacing.is_none());
    }

    #[test]
    fn set_block_layout_all_none_does_nothing() {
        let mut doc = Document::new();
        let id = alloc_id(&doc).expect("alloc should succeed");
        let applied = set_block_layout(&mut doc, id, BlockLayout::default());
        assert!(!applied);
        assert!(doc.presentation.is_none());
    }

    #[test]
    fn set_block_layout_single_field_creates_overlay() {
        let mut doc = Document::new();
        let id = alloc_id(&doc).expect("alloc should succeed");
        let applied = set_block_layout(
            &mut doc,
            id,
            BlockLayout {
                alignment: Some(Alignment::Center),
                ..Default::default()
            },
        );
        assert!(applied);
        let pres = doc
            .presentation
            .as_ref()
            .expect("presentation should exist");
        let layout = pres
            .block_layout
            .get(id)
            .expect("layout should exist for node");
        assert_eq!(layout.alignment, Some(Alignment::Center));
        assert!(layout.indent_left.is_none());
        assert!(layout.indent_right.is_none());
        assert!(layout.space_before.is_none());
        assert!(layout.space_after.is_none());
        assert!(layout.background_color.is_none());
    }
}

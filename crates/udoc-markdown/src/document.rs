//! Markdown document abstraction and trait implementations.
//!
//! Provides `MdDocument` for opening and extracting content from Markdown files.
//! Implements `FormatBackend` and `PageExtractor` from udoc-core.

use std::sync::Arc;

use udoc_core::backend::{DocumentMetadata, FormatBackend, PageExtractor};
use udoc_core::diagnostics::{DiagnosticsSink, Warning};
use udoc_core::error::Result;
use udoc_core::image::PageImage;
use udoc_core::table::Table;
use udoc_core::text::{TextLine, TextSpan};

use crate::block::{parse_blocks, MdBlock, MdWarning};
use crate::inline::MdInline;
use crate::MAX_FILE_SIZE;

/// Top-level Markdown document handle.
pub struct MdDocument {
    blocks: Vec<MdBlock>,
    warnings: Vec<MdWarning>,
}

impl MdDocument {
    udoc_core::define_backend_constructors!(MAX_FILE_SIZE, "Markdown");

    /// Parse Markdown from in-memory bytes with a diagnostics sink.
    pub fn from_bytes_with_diag(data: &[u8], diag: Arc<dyn DiagnosticsSink>) -> Result<Self> {
        let text = String::from_utf8_lossy(data);
        let mut warnings: Vec<MdWarning> = Vec::new();
        if text.contains('\u{FFFD}') {
            warnings.push((
                "InvalidEncoding".to_string(),
                "input contains non-UTF-8 bytes (replaced with U+FFFD)".to_string(),
            ));
        }
        let result = parse_blocks(&text);
        warnings.extend(result.warnings);
        // Propagate accumulated parse warnings to the diagnostics sink.
        for (kind, msg) in &warnings {
            diag.warning(Warning::new(kind.as_str(), msg.as_str()));
        }
        Ok(Self {
            blocks: result.blocks,
            warnings,
        })
    }

    /// Returns warnings accumulated during parsing.
    pub fn warnings(&self) -> &[MdWarning] {
        &self.warnings
    }

    /// Returns the parsed blocks (for testing/inspection).
    pub fn blocks(&self) -> &[MdBlock] {
        &self.blocks
    }
}

/// Page handle for Markdown. The whole document is one logical "page".
pub struct MdPage<'a> {
    blocks: &'a [MdBlock],
}

impl FormatBackend for MdDocument {
    type Page<'a> = MdPage<'a>;

    fn page_count(&self) -> usize {
        1
    }

    fn page(&mut self, index: usize) -> Result<MdPage<'_>> {
        udoc_core::backend::validate_single_page(index, "Markdown")?;
        Ok(MdPage {
            blocks: &self.blocks,
        })
    }

    fn metadata(&self) -> DocumentMetadata {
        DocumentMetadata::with_page_count(1)
    }
}

impl PageExtractor for MdPage<'_> {
    fn text(&mut self) -> Result<String> {
        let mut parts = Vec::new();
        for block in self.blocks {
            let text = block_to_text(block);
            if !text.is_empty() {
                parts.push(text);
            }
        }
        Ok(parts.join("\n"))
    }

    fn text_lines(&mut self) -> Result<Vec<TextLine>> {
        let mut lines = Vec::new();
        let mut y = 0.0;
        collect_text_lines(self.blocks, &mut lines, &mut y);
        Ok(lines)
    }

    fn raw_spans(&mut self) -> Result<Vec<TextSpan>> {
        let mut spans = Vec::new();
        let mut y = 0.0;
        collect_raw_spans(self.blocks, &mut spans, &mut y);
        Ok(spans)
    }

    fn tables(&mut self) -> Result<Vec<Table>> {
        let mut tables = Vec::new();
        collect_tables(self.blocks, &mut tables);
        Ok(tables)
    }

    fn images(&mut self) -> Result<Vec<PageImage>> {
        // Markdown images are URL references, not embedded data.
        Ok(vec![])
    }
}

// ---------------------------------------------------------------------------
// Text extraction helpers
// ---------------------------------------------------------------------------

fn block_to_text(block: &MdBlock) -> String {
    match block {
        MdBlock::Heading { content, .. } => inlines_to_text(content),
        MdBlock::Paragraph { content } => inlines_to_text(content),
        MdBlock::CodeBlock { text, .. } => text.clone(),
        MdBlock::ThematicBreak => String::new(),
        MdBlock::List { items, .. } => items
            .iter()
            .map(|item| {
                item.content
                    .iter()
                    .map(block_to_text)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n"),
        MdBlock::Table { header, rows, .. } => {
            let mut parts = Vec::new();
            let header_text: Vec<String> = header.iter().map(|c| inlines_to_text(c)).collect();
            parts.push(header_text.join("\t"));
            for row in rows {
                let row_text: Vec<String> = row.iter().map(|c| inlines_to_text(c)).collect();
                parts.push(row_text.join("\t"));
            }
            parts.join("\n")
        }
        MdBlock::Blockquote { children } => children
            .iter()
            .map(block_to_text)
            .collect::<Vec<_>>()
            .join("\n"),
        MdBlock::Image { alt, .. } => alt.clone(),
    }
}

fn inlines_to_text(inlines: &[MdInline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            MdInline::Text { text, .. } => out.push_str(text),
            MdInline::Code { text } => out.push_str(text),
            MdInline::Link { content, .. } => out.push_str(&inlines_to_text(content)),
            MdInline::Image { alt, .. } => out.push_str(alt),
            MdInline::SoftBreak => out.push(' '),
            MdInline::LineBreak => out.push('\n'),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// TextLine / TextSpan extraction
// ---------------------------------------------------------------------------

fn collect_text_lines(blocks: &[MdBlock], lines: &mut Vec<TextLine>, y: &mut f64) {
    for block in blocks {
        match block {
            MdBlock::Heading { content, .. } | MdBlock::Paragraph { content } => {
                let spans = inlines_to_spans(content, *y);
                if !spans.is_empty() {
                    lines.push(TextLine::new(spans, *y, false));
                    *y += 1.0;
                }
            }
            MdBlock::CodeBlock { text, .. } => {
                for code_line in text.lines() {
                    let span = TextSpan::new(code_line.to_string(), 0.0, *y, 0.0, 0.0);
                    lines.push(TextLine::new(vec![span], *y, false));
                    *y += 1.0;
                }
            }
            MdBlock::List { items, .. } => {
                for item in items {
                    collect_text_lines(&item.content, lines, y);
                }
            }
            MdBlock::Table { header, rows, .. } => {
                // Header line.
                let header_spans: Vec<TextSpan> = header
                    .iter()
                    .flat_map(|cell| inlines_to_spans(cell, *y))
                    .collect();
                if !header_spans.is_empty() {
                    lines.push(TextLine::new(header_spans, *y, false));
                    *y += 1.0;
                }
                // Data rows.
                for row in rows {
                    let row_spans: Vec<TextSpan> = row
                        .iter()
                        .flat_map(|cell| inlines_to_spans(cell, *y))
                        .collect();
                    if !row_spans.is_empty() {
                        lines.push(TextLine::new(row_spans, *y, false));
                        *y += 1.0;
                    }
                }
            }
            MdBlock::Blockquote { children } => {
                collect_text_lines(children, lines, y);
            }
            MdBlock::Image { alt, .. } => {
                if !alt.is_empty() {
                    let span = TextSpan::new(alt.clone(), 0.0, *y, 0.0, 0.0);
                    lines.push(TextLine::new(vec![span], *y, false));
                    *y += 1.0;
                }
            }
            MdBlock::ThematicBreak => {}
        }
    }
}

fn collect_raw_spans(blocks: &[MdBlock], spans: &mut Vec<TextSpan>, y: &mut f64) {
    for block in blocks {
        match block {
            MdBlock::Heading { content, .. } | MdBlock::Paragraph { content } => {
                spans.extend(inlines_to_spans(content, *y));
                *y += 1.0;
            }
            MdBlock::CodeBlock { text, .. } => {
                for code_line in text.lines() {
                    spans.push(TextSpan::new(code_line.to_string(), 0.0, *y, 0.0, 0.0));
                    *y += 1.0;
                }
            }
            MdBlock::List { items, .. } => {
                for item in items {
                    collect_raw_spans(&item.content, spans, y);
                }
            }
            MdBlock::Table { header, rows, .. } => {
                for cell in header {
                    spans.extend(inlines_to_spans(cell, *y));
                }
                *y += 1.0;
                for row in rows {
                    for cell in row {
                        spans.extend(inlines_to_spans(cell, *y));
                    }
                    *y += 1.0;
                }
            }
            MdBlock::Blockquote { children } => {
                collect_raw_spans(children, spans, y);
            }
            MdBlock::Image { alt, .. } => {
                if !alt.is_empty() {
                    spans.push(TextSpan::new(alt.clone(), 0.0, *y, 0.0, 0.0));
                    *y += 1.0;
                }
            }
            MdBlock::ThematicBreak => {}
        }
    }
}

fn inlines_to_spans(inlines: &[MdInline], y: f64) -> Vec<TextSpan> {
    let mut spans = Vec::new();
    for inline in inlines {
        match inline {
            MdInline::Text {
                text, bold, italic, ..
            } => {
                if !text.is_empty() {
                    let mut span = TextSpan::new(text.clone(), 0.0, y, 0.0, 0.0);
                    span.is_bold = *bold;
                    span.is_italic = *italic;
                    spans.push(span);
                }
            }
            MdInline::Code { text } => {
                if !text.is_empty() {
                    spans.push(TextSpan::new(text.clone(), 0.0, y, 0.0, 0.0));
                }
            }
            MdInline::Link { content, .. } => {
                spans.extend(inlines_to_spans(content, y));
            }
            MdInline::Image { alt, .. } => {
                if !alt.is_empty() {
                    spans.push(TextSpan::new(alt.clone(), 0.0, y, 0.0, 0.0));
                }
            }
            MdInline::SoftBreak => {
                spans.push(TextSpan::new(" ".to_string(), 0.0, y, 0.0, 0.0));
            }
            MdInline::LineBreak => {
                spans.push(TextSpan::new("\n".to_string(), 0.0, y, 0.0, 0.0));
            }
        }
    }
    spans
}

// ---------------------------------------------------------------------------
// Table extraction
// ---------------------------------------------------------------------------

fn collect_tables(blocks: &[MdBlock], tables: &mut Vec<Table>) {
    for block in blocks {
        match block {
            MdBlock::Table {
                header,
                rows,
                col_count,
            } => {
                let cols = *col_count;
                let mut core_rows = Vec::new();

                // Header row, normalized to col_count cells.
                let header_cells = normalize_row_cells(header, cols);
                core_rows.push(udoc_core::table::TableRow::with_header(header_cells, true));

                // Data rows, normalized to col_count cells.
                for row in rows {
                    let cells = normalize_row_cells(row, cols);
                    core_rows.push(udoc_core::table::TableRow::new(cells));
                }

                let mut table = Table::new(core_rows, None);
                table.num_columns = cols;
                table.header_row_count = 1;
                tables.push(table);
            }
            MdBlock::List { items, .. } => {
                for item in items {
                    collect_tables(&item.content, tables);
                }
            }
            MdBlock::Blockquote { children } => {
                collect_tables(children, tables);
            }
            _ => {}
        }
    }
}

/// Normalize a row to exactly `col_count` cells: pad with empty cells or truncate.
fn normalize_row_cells(
    cells: &[Vec<MdInline>],
    col_count: usize,
) -> Vec<udoc_core::table::TableCell> {
    let mut result: Vec<udoc_core::table::TableCell> = cells
        .iter()
        .take(col_count)
        .map(|cell| udoc_core::table::TableCell::new(inlines_to_text(cell), None))
        .collect();
    // Pad with empty cells if row has fewer cells than col_count.
    while result.len() < col_count {
        result.push(udoc_core::table::TableCell::new(String::new(), None));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_count_is_one() {
        let doc = MdDocument::from_bytes(b"# Hello").unwrap();
        assert_eq!(doc.page_count(), 1);
    }

    #[test]
    fn page_out_of_range() {
        let mut doc = MdDocument::from_bytes(b"# Hello").unwrap();
        assert!(doc.page(1).is_err());
        assert!(doc.page(100).is_err());
    }

    #[test]
    fn basic_text_extraction() {
        let mut doc = MdDocument::from_bytes(b"# Hello\n\nWorld").unwrap();
        let mut page = doc.page(0).unwrap();
        let text = page.text().unwrap();
        assert!(text.contains("Hello"), "got: {text}");
        assert!(text.contains("World"), "got: {text}");
    }

    #[test]
    fn metadata_defaults() {
        let doc = MdDocument::from_bytes(b"text").unwrap();
        let meta = doc.metadata();
        assert_eq!(meta.page_count, 1);
        assert!(meta.title.is_none());
    }

    #[test]
    fn text_lines_count() {
        let mut doc = MdDocument::from_bytes(b"# H1\n\nParagraph one.\n\nParagraph two.").unwrap();
        let mut page = doc.page(0).unwrap();
        let lines = page.text_lines().unwrap();
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn raw_spans_flatten() {
        let mut doc = MdDocument::from_bytes(b"Hello **bold** world").unwrap();
        let mut page = doc.page(0).unwrap();
        let spans = page.raw_spans().unwrap();
        assert!(spans.len() >= 2, "got {} spans", spans.len());
    }

    #[test]
    fn table_extraction() {
        let input = b"| A | B |\n| --- | --- |\n| 1 | 2 |";
        let mut doc = MdDocument::from_bytes(input).unwrap();
        let mut page = doc.page(0).unwrap();
        let tables = page.tables().unwrap();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].rows.len(), 2);
        assert_eq!(tables[0].num_columns, 2);
        assert_eq!(tables[0].rows[0].cells[0].text, "A");
        assert_eq!(tables[0].rows[1].cells[0].text, "1");
    }

    #[test]
    fn images_empty() {
        let mut doc = MdDocument::from_bytes(b"![alt](img.png)").unwrap();
        let mut page = doc.page(0).unwrap();
        let images = page.images().unwrap();
        assert!(images.is_empty());
    }

    #[test]
    fn from_bytes_round_trip() {
        let data = b"Hello from bytes";
        let mut doc = MdDocument::from_bytes(data).unwrap();
        assert_eq!(doc.page_count(), 1);
        let mut page = doc.page(0).unwrap();
        let text = page.text().unwrap();
        assert_eq!(text, "Hello from bytes");
    }

    #[test]
    fn utf8_lossy_handling() {
        let data: &[u8] = &[
            0x48, 0x65, 0x6C, 0x6C, 0x6F, 0xFF, 0x20, 0x57, 0x6F, 0x72, 0x6C, 0x64,
        ];
        let doc = MdDocument::from_bytes(data).unwrap();
        assert!(!doc.warnings().is_empty());
    }

    #[test]
    fn code_block_text_extraction() {
        let input = b"```rust\nfn main() {}\n```";
        let mut doc = MdDocument::from_bytes(input).unwrap();
        let mut page = doc.page(0).unwrap();
        let text = page.text().unwrap();
        assert!(text.contains("fn main() {}"), "got: {text}");
    }

    #[test]
    fn blockquote_text_extraction() {
        let input = b"> quoted text\n> more quoted";
        let mut doc = MdDocument::from_bytes(input).unwrap();
        let mut page = doc.page(0).unwrap();
        let text = page.text().unwrap();
        assert!(text.contains("quoted text"), "got: {text}");
    }

    #[test]
    fn list_text_extraction() {
        let input = b"- item one\n- item two\n- item three";
        let mut doc = MdDocument::from_bytes(input).unwrap();
        let mut page = doc.page(0).unwrap();
        let text = page.text().unwrap();
        assert!(text.contains("item one"), "got: {text}");
        assert!(text.contains("item three"), "got: {text}");
    }

    #[test]
    fn max_file_size_constant() {
        assert_eq!(
            super::MAX_FILE_SIZE,
            udoc_core::limits::DEFAULT_MAX_FILE_SIZE
        );
    }

    #[test]
    fn file_size_limit_accepts_small_file() {
        let dir = std::env::temp_dir().join(format!("udoc_md_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("small.md");
        std::fs::write(&path, "# Hello").unwrap();
        assert!(MdDocument::open(&path).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn table_cell_count_normalization() {
        // Row has fewer cells than col_count (separator says 3 columns).
        let input = b"| A | B | C |\n| --- | --- | --- |\n| 1 | 2 |";
        let mut doc = MdDocument::from_bytes(input).unwrap();
        let mut page = doc.page(0).unwrap();
        let tables = page.tables().unwrap();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].num_columns, 3);
        // Header has 3 cells.
        assert_eq!(tables[0].rows[0].cells.len(), 3);
        // Data row should be padded to 3 cells.
        assert_eq!(tables[0].rows[1].cells.len(), 3);
        assert_eq!(tables[0].rows[1].cells[2].text, "");
    }
}

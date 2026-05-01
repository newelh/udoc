//! RTF document abstraction and trait implementations.
//!
//! Provides `RtfDocument` for opening and extracting content from RTF files.
//! Implements `FormatBackend` and `PageExtractor` from udoc-core.

use std::sync::Arc;

use udoc_core::backend::{DocumentMetadata, FormatBackend, PageExtractor};
use udoc_core::diagnostics::{DiagnosticsSink, Warning};
use udoc_core::error::{Result, ResultExt};
use udoc_core::image::PageImage;
use udoc_core::table::Table;
use udoc_core::text::{TextLine, TextSpan};

use crate::parser::{Paragraph, ParsedDocument, Parser};
use crate::MAX_FILE_SIZE;

/// Top-level RTF document handle.
pub struct RtfDocument {
    parsed: ParsedDocument,
}

impl RtfDocument {
    udoc_core::define_backend_constructors!(MAX_FILE_SIZE, "RTF");

    /// Parse RTF from in-memory bytes with a diagnostics sink.
    pub fn from_bytes_with_diag(data: &[u8], diag: Arc<dyn DiagnosticsSink>) -> Result<Self> {
        let parsed = Parser::parse(data).context("parsing RTF document")?;
        // Propagate accumulated parse warnings to the diagnostics sink.
        for msg in &parsed.warnings {
            diag.warning(Warning::new("RtfParse", msg.as_str()));
        }
        Ok(Self { parsed })
    }

    /// Returns warnings accumulated during parsing (malformed input,
    /// skipped content, unsupported features, etc.).
    pub fn warnings(&self) -> &[String] {
        &self.parsed.warnings
    }

    /// Access the parsed document data (paragraphs, tables, images, etc.).
    /// Used by the converter to build the Document model with full formatting.
    pub fn parsed(&self) -> &ParsedDocument {
        &self.parsed
    }

    /// Convert parsed tables to core Table type for the converter.
    pub fn core_tables(&self) -> Vec<Table> {
        self.parsed
            .tables
            .iter()
            .map(crate::table::convert_table)
            .collect()
    }
}

/// Page handle for RTF. The whole document is one logical "page"
/// since RTF is a flow format without explicit page breaks.
pub struct RtfPage<'a> {
    parsed: &'a ParsedDocument,
}

impl FormatBackend for RtfDocument {
    type Page<'a> = RtfPage<'a>;

    fn page_count(&self) -> usize {
        1
    }

    fn page(&mut self, index: usize) -> Result<RtfPage<'_>> {
        udoc_core::backend::validate_single_page(index, "RTF")?;
        Ok(RtfPage {
            parsed: &self.parsed,
        })
    }

    fn metadata(&self) -> DocumentMetadata {
        let mut meta = DocumentMetadata::with_page_count(1);
        meta.title = self.parsed.metadata.title.clone();
        meta.author = self.parsed.metadata.author.clone();
        meta.subject = self.parsed.metadata.subject.clone();
        meta
    }
}

/// Join a paragraph's visible runs into a single string.
///
/// Near-identical to udoc-docx's paragraph_text. Kept separate because
/// each operates on a format-specific Paragraph type (different Run fields).
fn paragraph_text(para: &Paragraph) -> String {
    para.runs
        .iter()
        .filter(|r| !r.invisible)
        .map(|r| r.text.as_str())
        .collect()
}

/// Convert a paragraph's visible runs into TextSpans.
///
/// Near-identical to udoc-docx's paragraph_spans. Kept separate because
/// each operates on a format-specific Paragraph type (RTF Run has bool
/// bold/italic, DOCX Run has `Option<bool>`).
fn paragraph_spans(para: &Paragraph, y: f64) -> Vec<TextSpan> {
    para.runs
        .iter()
        .filter(|r| !r.invisible)
        .map(|r| {
            TextSpan::with_style(
                r.text.clone(),
                0.0,
                y,
                0.0,
                r.font_name.as_deref().map(str::to_string),
                r.font_size_pts,
                r.bold,
                r.italic,
                r.invisible,
                0.0,
            )
        })
        .collect()
}

impl PageExtractor for RtfPage<'_> {
    fn text(&mut self) -> Result<String> {
        let parts: Vec<String> = self.parsed.paragraphs.iter().map(paragraph_text).collect();
        Ok(parts.join("\n"))
    }

    fn text_lines(&mut self) -> Result<Vec<TextLine>> {
        let lines: Vec<TextLine> = self
            .parsed
            .paragraphs
            .iter()
            .enumerate()
            .map(|(i, para)| {
                let spans = paragraph_spans(para, i as f64);
                TextLine::new(spans, i as f64, false)
            })
            .collect();
        Ok(lines)
    }

    fn raw_spans(&mut self) -> Result<Vec<TextSpan>> {
        let spans: Vec<TextSpan> = self
            .parsed
            .paragraphs
            .iter()
            .enumerate()
            .flat_map(|(i, para)| paragraph_spans(para, i as f64))
            .collect();
        Ok(spans)
    }

    fn tables(&mut self) -> Result<Vec<Table>> {
        Ok(self
            .parsed
            .tables
            .iter()
            .map(crate::table::convert_table)
            .collect())
    }

    fn images(&mut self) -> Result<Vec<PageImage>> {
        Ok(self
            .parsed
            .images
            .iter()
            .filter_map(crate::image::convert_image)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus_path(name: &str) -> std::path::PathBuf {
        let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("tests");
        p.push("corpus");
        p.push(name);
        p
    }

    fn open_corpus(name: &str) -> RtfDocument {
        RtfDocument::open(corpus_path(name)).expect("failed to open corpus file")
    }

    #[test]
    fn basic_text_extraction() {
        let mut doc = open_corpus("basic.rtf");
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert!(text.contains("Hello, world!"), "got: {text}");
        assert!(
            text.contains("This is a simple RTF document."),
            "got: {text}"
        );
        assert!(text.contains("It has three paragraphs."), "got: {text}");
    }

    #[test]
    fn page_count_is_one() {
        let doc = open_corpus("basic.rtf");
        assert_eq!(doc.page_count(), 1);
    }

    #[test]
    fn page_out_of_range() {
        let mut doc = open_corpus("basic.rtf");
        assert!(doc.page(1).is_err());
        assert!(doc.page(100).is_err());
    }

    #[test]
    fn metadata_from_corpus() {
        let doc = open_corpus("metadata.rtf");
        let meta = doc.metadata();
        assert_eq!(meta.title.as_deref(), Some("Test Document"));
        assert_eq!(meta.author.as_deref(), Some("Test Author"));
        assert_eq!(meta.subject.as_deref(), Some("Testing RTF"));
        assert_eq!(meta.page_count, 1);
    }

    #[test]
    fn table_basic_conversion() {
        let mut doc = open_corpus("table_basic.rtf");
        let mut page = doc.page(0).expect("page 0");
        let tables = page.tables().expect("tables()");
        assert_eq!(tables.len(), 1);

        let table = &tables[0];
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0].cells.len(), 3);
        assert_eq!(table.rows[0].cells[0].text, "Name");
        assert_eq!(table.rows[0].cells[1].text, "Age");
        assert_eq!(table.rows[0].cells[2].text, "City");
        assert_eq!(table.rows[1].cells[0].text, "Alice");
    }

    #[test]
    fn text_lines_count() {
        let mut doc = open_corpus("basic.rtf");
        let mut page = doc.page(0).expect("page 0");
        let lines = page.text_lines().expect("text_lines()");
        // basic.rtf has 3 paragraphs.
        assert_eq!(lines.len(), 3);
        assert!(!lines[0].spans.is_empty());
    }

    #[test]
    fn raw_spans_flatten() {
        let mut doc = open_corpus("basic.rtf");
        let mut page = doc.page(0).expect("page 0");
        let spans = page.raw_spans().expect("raw_spans()");
        // At least one span per paragraph.
        assert!(spans.len() >= 3);
    }

    #[test]
    fn from_bytes_round_trip() {
        let data = b"{\\rtf1 Hello from bytes}";
        let mut doc = RtfDocument::from_bytes(data).expect("from_bytes");
        assert_eq!(doc.page_count(), 1);
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert_eq!(text, "Hello from bytes");
    }

    #[test]
    fn hidden_text_excluded_from_text() {
        let mut doc = open_corpus("hidden_text.rtf");
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        // text() filters out invisible runs.
        assert!(text.contains("Visible text."), "got: {text}");
        assert!(!text.contains("Hidden text."), "got: {text}");
    }

    #[test]
    fn hidden_text_excluded_from_text_lines() {
        let mut doc = open_corpus("hidden_text.rtf");
        let mut page = doc.page(0).expect("page 0");
        let lines = page.text_lines().expect("text_lines()");
        // text_lines() should also filter invisible spans for consistency.
        let all_span_text: String = lines
            .iter()
            .flat_map(|l| &l.spans)
            .map(|s| s.text.as_str())
            .collect();
        assert!(
            all_span_text.contains("Visible text."),
            "got: {all_span_text}"
        );
        assert!(
            !all_span_text.contains("Hidden text."),
            "hidden text leaked into text_lines(): {all_span_text}"
        );
    }
}

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

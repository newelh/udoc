//! DocDocument and page types implementing FormatBackend/PageExtractor.
//!
//! `DocDocument` opens a .doc file via CFB, parses the FIB, piece table,
//! and properties, then extracts text and tables. The entire DOC is treated
//! as a single logical "page" in the FormatBackend sense.

use std::sync::Arc;

use udoc_containers::cfb::summary_info::SUMMARY_INFO_STREAM_NAME;
use udoc_containers::cfb::{parse_summary_information, CfbArchive, SummaryInfo};
use udoc_core::backend::{DocumentMetadata, FormatBackend, PageExtractor};
use udoc_core::diagnostics::{DiagnosticsSink, Warning};
use udoc_core::image::PageImage;
use udoc_core::table::Table;
use udoc_core::text::{TextLine, TextSpan};

use crate::error::{Error, Result, ResultExt};
use crate::fib;
use crate::font_table;
use crate::piece_table::PieceTable;
use crate::properties::{self, CharacterProperties, ParagraphProperties};
use crate::tables;
use crate::text::{self, DocParagraph, StoryBoundaries};
use crate::MAX_FILE_SIZE;

/// A parsed DOC document ready for content extraction.
///
/// Implements `FormatBackend` where the entire document is a single page.
#[derive(Debug)]
pub struct DocDocument {
    /// Body text paragraphs (non-table paragraphs after table detection).
    paragraphs: Vec<DocParagraph>,
    /// Detected tables from in-table paragraph runs.
    tables: Vec<Table>,
    /// Footnote paragraphs from the footnote story.
    footnotes: Vec<DocParagraph>,
    /// Endnote paragraphs from the endnote story.
    endnotes: Vec<DocParagraph>,
    /// Header/footer paragraphs from the header/footer story.
    headers_footers: Vec<DocParagraph>,
    /// Paragraph properties (for istd-based heading detection).
    para_props: Vec<ParagraphProperties>,
    /// Character properties (for bold/italic per-run).
    char_props: Vec<CharacterProperties>,
    /// Font names from the SttbfFfn table (index = font_index from CHPX).
    font_names: Vec<String>,
    /// Document metadata.
    metadata: DocumentMetadata,
}

/// A page view for content extraction (DOC has exactly one page).
pub struct DocPage<'a> {
    doc: &'a DocDocument,
}

impl DocDocument {
    udoc_core::define_backend_constructors!(MAX_FILE_SIZE, "DOC");

    /// Access paragraphs (for in-crate conversion).
    pub(crate) fn paragraphs(&self) -> &[DocParagraph] {
        &self.paragraphs
    }

    /// Access paragraph properties (for in-crate conversion).
    pub(crate) fn para_props(&self) -> &[ParagraphProperties] {
        &self.para_props
    }

    /// Access character properties (for in-crate conversion).
    pub(crate) fn char_props(&self) -> &[CharacterProperties] {
        &self.char_props
    }

    /// Access detected tables (for in-crate conversion).
    pub(crate) fn tables_ref(&self) -> &[Table] {
        &self.tables
    }

    /// Access footnote paragraphs (for in-crate conversion).
    pub(crate) fn footnotes(&self) -> &[DocParagraph] {
        &self.footnotes
    }

    /// Access endnote paragraphs (for in-crate conversion).
    pub(crate) fn endnotes(&self) -> &[DocParagraph] {
        &self.endnotes
    }

    /// Access header/footer paragraphs (for in-crate conversion).
    pub(crate) fn headers_footers(&self) -> &[DocParagraph] {
        &self.headers_footers
    }

    /// Access font names (for in-crate font_name resolution on TextSpans
    /// and font name forwarding to the presentation overlay).
    pub(crate) fn font_names(&self) -> &[String] {
        &self.font_names
    }

    /// Create a DOC document from in-memory bytes with diagnostics.
    pub fn from_bytes_with_diag(data: &[u8], diag: Arc<dyn DiagnosticsSink>) -> Result<Self> {
        if data.len() as u64 > MAX_FILE_SIZE {
            return Err(Error::new(format!(
                "DOC file too large ({} bytes, limit is {} bytes)",
                data.len(),
                MAX_FILE_SIZE
            )));
        }

        let archive =
            CfbArchive::new(data, Arc::clone(&diag)).context("opening DOC CFB archive")?;

        // Read WordDocument stream (required).
        let wd_entry = archive
            .find("WordDocument")
            .ok_or_else(|| Error::new("WordDocument stream not found"))?;
        let wd_data = archive
            .read(wd_entry)
            .context("reading WordDocument stream")?;

        // Parse FIB.
        let fib_result = fib::parse_fib(&wd_data).context("parsing FIB from WordDocument")?;

        // Read Table stream.
        let tbl_entry = archive.find(fib_result.table_stream_name).ok_or_else(|| {
            Error::new(format!("{} stream not found", fib_result.table_stream_name))
        })?;
        let tbl_data = archive.read(tbl_entry).context("reading table stream")?;

        // Parse piece table, with fast-save fallback for files where CLX is empty.
        // total_ccp = sum of all story character counts + 1 (the document terminator).
        let total_ccp = fib_result
            .ccp_text
            .saturating_add(fib_result.ccp_ftn)
            .saturating_add(fib_result.ccp_hdd)
            .saturating_add(fib_result.ccp_atn)
            .saturating_add(fib_result.ccp_edn)
            .saturating_add(1);
        let piece_table = PieceTable::parse_with_fast_save_fallback(
            &tbl_data,
            fib_result.fc_clx,
            fib_result.lcb_clx,
            fib_result.fib_size,
            total_ccp,
            &*diag,
        )
        .context("parsing piece table from CLX")?;

        // Extract body paragraphs.
        let boundaries = StoryBoundaries::from_fib(&fib_result);
        let all_paragraphs = text::extract_body_paragraphs(&piece_table, &wd_data, &boundaries)
            .context("extracting body paragraphs")?;

        // Extract footnote paragraphs (warn on failure, don't abort).
        let footnotes = match text::extract_footnote_paragraphs(&piece_table, &wd_data, &boundaries)
        {
            Ok(paras) => paras,
            Err(e) => {
                diag.warning(Warning::new(
                    "DocFootnoteExtractionFailed",
                    format!("failed to extract footnotes: {e}"),
                ));
                Vec::new()
            }
        };

        // Extract endnote paragraphs (warn on failure, don't abort).
        let endnotes = match text::extract_endnote_paragraphs(&piece_table, &wd_data, &boundaries) {
            Ok(paras) => paras,
            Err(e) => {
                diag.warning(Warning::new(
                    "DocEndnoteExtractionFailed",
                    format!("failed to extract endnotes: {e}"),
                ));
                Vec::new()
            }
        };

        // Extract header/footer paragraphs (warn on failure, don't abort).
        let headers_footers =
            match text::extract_header_footer_paragraphs(&piece_table, &wd_data, &boundaries) {
                Ok(paras) => paras,
                Err(e) => {
                    diag.warning(Warning::new(
                        "DocHeaderFooterExtractionFailed",
                        format!("failed to extract headers/footers: {e}"),
                    ));
                    Vec::new()
                }
            };

        // Parse paragraph properties (warn on failure, don't abort).
        let para_props = match properties::parse_paragraph_properties(
            &tbl_data,
            &wd_data,
            &fib_result,
            &piece_table,
        ) {
            Ok(props) => props,
            Err(e) => {
                diag.warning(Warning::new(
                    "DocParagraphPropertiesFailed",
                    format!("failed to parse paragraph properties: {e}"),
                ));
                Vec::new()
            }
        };

        // Parse character properties (warn on failure, don't abort).
        let char_props = match properties::parse_character_properties(
            &tbl_data,
            &wd_data,
            &fib_result,
            &piece_table,
        ) {
            Ok(props) => props,
            Err(e) => {
                diag.warning(Warning::new(
                    "DocCharacterPropertiesFailed",
                    format!("failed to parse character properties: {e}"),
                ));
                Vec::new()
            }
        };

        // Parse font table (warn on failure, don't abort).
        let font_names = match font_table::parse_font_table(&tbl_data, &fib_result) {
            Ok(names) => names,
            Err(e) => {
                diag.warning(Warning::new(
                    "DocFontTableFailed",
                    format!("failed to parse font table: {e}"),
                ));
                Vec::new()
            }
        };

        // Detect tables from paragraph properties.
        let (detected_tables, remaining_paragraphs) =
            tables::detect_tables(&all_paragraphs, &para_props);

        // Parse SummaryInformation metadata (warn on failure, don't abort).
        let summary = match archive.find(SUMMARY_INFO_STREAM_NAME) {
            Some(entry) => match archive.read(entry) {
                Ok(si_data) => match parse_summary_information(&si_data) {
                    Ok(si) => si,
                    Err(e) => {
                        diag.warning(Warning::new(
                            "DocSummaryInfoParseFailed",
                            format!("failed to parse SummaryInformation: {e}"),
                        ));
                        SummaryInfo::default()
                    }
                },
                Err(e) => {
                    diag.warning(Warning::new(
                        "DocSummaryInfoReadFailed",
                        format!("failed to read SummaryInformation stream: {e}"),
                    ));
                    SummaryInfo::default()
                }
            },
            None => SummaryInfo::default(),
        };

        // Build metadata.
        let mut metadata = DocumentMetadata::with_page_count(1);
        metadata.title = summary.title;
        metadata.subject = summary.subject;
        metadata.author = summary.author;

        Ok(Self {
            paragraphs: remaining_paragraphs,
            tables: detected_tables,
            footnotes,
            endnotes,
            headers_footers,
            para_props,
            char_props,
            font_names,
            metadata,
        })
    }
}

impl FormatBackend for DocDocument {
    type Page<'a> = DocPage<'a>;

    fn page_count(&self) -> usize {
        1
    }

    fn page(&mut self, index: usize) -> Result<DocPage<'_>> {
        udoc_core::backend::validate_single_page(index, "DOC")?;
        Ok(DocPage { doc: self })
    }

    fn metadata(&self) -> DocumentMetadata {
        self.metadata.clone()
    }
}

impl PageExtractor for DocPage<'_> {
    fn text(&mut self) -> Result<String> {
        let all_paras = self
            .doc
            .paragraphs
            .iter()
            .chain(self.doc.footnotes.iter())
            .chain(self.doc.endnotes.iter())
            .chain(self.doc.headers_footers.iter())
            .filter(|p| !p.text.is_empty());

        let mut result = String::new();
        for para in all_paras {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(&para.text);
        }
        Ok(result)
    }

    fn text_lines(&mut self) -> Result<Vec<TextLine>> {
        let mut lines = Vec::new();
        let mut line_idx: usize = 0;

        // Body paragraphs.
        for para in &self.doc.paragraphs {
            if para.text.is_empty() {
                continue;
            }
            let y = line_idx as f64;
            let spans = spans_for_paragraph(para, &self.doc.char_props, &self.doc.font_names, y);
            lines.push(TextLine::new(spans, y, false));
            line_idx += 1;
        }

        // Footnotes.
        for para in &self.doc.footnotes {
            if para.text.is_empty() {
                continue;
            }
            let y = line_idx as f64;
            let spans = spans_for_paragraph(para, &self.doc.char_props, &self.doc.font_names, y);
            lines.push(TextLine::new(spans, y, false));
            line_idx += 1;
        }

        // Endnotes.
        for para in &self.doc.endnotes {
            if para.text.is_empty() {
                continue;
            }
            let y = line_idx as f64;
            let spans = spans_for_paragraph(para, &self.doc.char_props, &self.doc.font_names, y);
            lines.push(TextLine::new(spans, y, false));
            line_idx += 1;
        }

        // Headers/footers.
        for para in &self.doc.headers_footers {
            if para.text.is_empty() {
                continue;
            }
            let y = line_idx as f64;
            let spans = spans_for_paragraph(para, &self.doc.char_props, &self.doc.font_names, y);
            lines.push(TextLine::new(spans, y, false));
            line_idx += 1;
        }

        Ok(lines)
    }

    fn raw_spans(&mut self) -> Result<Vec<TextSpan>> {
        let mut spans = Vec::new();
        let mut idx: usize = 0;

        let all_paras = self
            .doc
            .paragraphs
            .iter()
            .chain(self.doc.footnotes.iter())
            .chain(self.doc.endnotes.iter())
            .chain(self.doc.headers_footers.iter());

        for para in all_paras {
            if para.text.is_empty() {
                continue;
            }
            let y = idx as f64;
            let para_spans =
                spans_for_paragraph(para, &self.doc.char_props, &self.doc.font_names, y);
            idx += para_spans.len();
            spans.extend(para_spans);
        }

        Ok(spans)
    }

    fn tables(&mut self) -> Result<Vec<Table>> {
        Ok(self.doc.tables.clone())
    }

    fn images(&mut self) -> Result<Vec<PageImage>> {
        // DOC image extraction deferred.
        Ok(Vec::new())
    }
}

/// Build text spans for a paragraph, splitting by character property runs.
///
/// If character properties overlap the paragraph's CP range, splits the
/// text into runs with bold/italic from each CharacterProperties entry.
/// Falls back to a single span for the whole paragraph when no character
/// properties overlap. When font_names is non-empty, populates font_name
/// on each span from the character property's font_index.
fn spans_for_paragraph(
    para: &DocParagraph,
    char_props: &[CharacterProperties],
    font_names: &[String],
    y: f64,
) -> Vec<TextSpan> {
    // Find character property runs that overlap this paragraph's CP range.
    let overlapping: Vec<&CharacterProperties> = char_props
        .iter()
        .filter(|cp| cp.cp_start < para.cp_end && cp.cp_end > para.cp_start)
        .collect();

    if overlapping.is_empty() {
        // No character properties: single plain span
        return vec![TextSpan::new(para.text.clone(), 0.0, y, 0.0, 0.0)];
    }

    // Split the paragraph text according to character property boundaries.
    let mut spans = Vec::new();
    let para_chars: Vec<char> = para.text.chars().collect();
    let para_len = para_chars.len() as u32;

    for cp in &overlapping {
        // Clip the character property range to this paragraph
        let run_cp_start = cp.cp_start.max(para.cp_start);
        let run_cp_end = cp.cp_end.min(para.cp_end);

        if run_cp_start >= run_cp_end {
            continue;
        }

        // Convert CP offsets to character indices within the paragraph text
        let char_start = (run_cp_start - para.cp_start) as usize;
        let char_end = ((run_cp_end - para.cp_start) as usize).min(para_len as usize);

        if char_start >= char_end || char_start >= para_chars.len() {
            continue;
        }

        let text: String = para_chars[char_start..char_end].iter().collect();
        if text.is_empty() {
            continue;
        }

        let mut span = TextSpan::new(text, 0.0, y, 0.0, 0.0);
        span.is_bold = cp.bold.unwrap_or(false);
        span.is_italic = cp.italic.unwrap_or(false);

        // Resolve font name from font_index.
        if let Some(idx) = cp.font_index {
            if let Some(name) = font_names.get(idx as usize) {
                span.font_name = Some(name.clone());
            }
        }

        spans.push(span);
    }

    // If no spans were produced from char props (edge case), fall back
    if spans.is_empty() {
        return vec![TextSpan::new(para.text.clone(), 0.0, y, 0.0, 0.0)];
    }

    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::build_minimal_doc;
    use udoc_core::backend::{FormatBackend, PageExtractor};

    #[test]
    fn from_bytes_basic_text() {
        let doc_bytes = build_minimal_doc("Hello World");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes should succeed");
        assert_eq!(doc.page_count(), 1);

        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert_eq!(text, "Hello World");
    }

    #[test]
    fn page_count_is_one() {
        let doc_bytes = build_minimal_doc("test");
        let doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        assert_eq!(doc.page_count(), 1);
    }

    #[test]
    fn text_returns_content() {
        let doc_bytes = build_minimal_doc("Hello World");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        assert_eq!(page.text().expect("text()"), "Hello World");
    }

    #[test]
    fn multiple_paragraphs() {
        let doc_bytes = build_minimal_doc("Para 1\rPara 2");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert_eq!(text, "Para 1\nPara 2");
    }

    #[test]
    fn empty_document() {
        let doc_bytes = build_minimal_doc("");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        assert_eq!(doc.page_count(), 1);
        let mut page = doc.page(0).expect("page 0");
        assert_eq!(page.text().expect("text()"), "");
    }

    #[test]
    fn page_out_of_range_errors() {
        let doc_bytes = build_minimal_doc("test");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        assert!(doc.page(1).is_err());
    }

    #[test]
    fn metadata_has_page_count_1() {
        let doc_bytes = build_minimal_doc("test");
        let doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let meta = doc.metadata();
        assert_eq!(meta.page_count, 1);
    }

    #[test]
    fn text_lines_returns_lines() {
        let doc_bytes = build_minimal_doc("Line1\rLine2");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let lines = page.text_lines().expect("text_lines()");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans[0].text, "Line1");
        assert_eq!(lines[1].spans[0].text, "Line2");
    }

    #[test]
    fn raw_spans_returns_spans() {
        let doc_bytes = build_minimal_doc("Span test");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let spans = page.raw_spans().expect("raw_spans()");
        assert!(!spans.is_empty());
        assert_eq!(spans[0].text, "Span test");
    }

    #[test]
    fn tables_initially_empty_for_plain_text() {
        let doc_bytes = build_minimal_doc("no tables here");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        assert!(page.tables().expect("tables()").is_empty());
    }

    #[test]
    fn images_always_empty() {
        let doc_bytes = build_minimal_doc("test");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        assert!(page.images().expect("images()").is_empty());
    }

    #[test]
    fn non_cfb_data_rejected() {
        let result = DocDocument::from_bytes(b"not a CFB file");
        assert!(result.is_err());
    }

    // ---------------------------------------------------------------
    // Fast-save fallback integration tests
    // ---------------------------------------------------------------

    #[test]
    fn fast_save_fallback_extracts_text() {
        use crate::test_util::build_minimal_fast_save_doc;

        let doc_bytes = build_minimal_fast_save_doc("Hello FastSave");
        let mut doc =
            DocDocument::from_bytes(&doc_bytes).expect("fast-save doc should parse without error");
        assert_eq!(doc.page_count(), 1);

        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert_eq!(text, "Hello FastSave");
    }

    #[test]
    fn fast_save_fallback_empty_text() {
        use crate::test_util::build_minimal_fast_save_doc;

        let doc_bytes = build_minimal_fast_save_doc("");
        let mut doc =
            DocDocument::from_bytes(&doc_bytes).expect("empty fast-save doc should parse");
        let mut page = doc.page(0).expect("page 0");
        assert_eq!(page.text().expect("text()"), "");
    }

    #[test]
    fn fast_save_fallback_multiple_paragraphs() {
        use crate::test_util::build_minimal_fast_save_doc;

        // Paragraph marks are U+000D in the DOC character stream.
        let doc_bytes = build_minimal_fast_save_doc("Para1\rPara2");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert_eq!(text, "Para1\nPara2");
    }

    #[test]
    fn text_includes_footnotes() {
        use crate::test_util::build_minimal_doc_with_notes;

        // Body "Hello\r", footnote "FnText\r", no endnotes
        let doc_bytes = build_minimal_doc_with_notes("Hello\r", "FnText\r", "");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert!(text.contains("Hello"), "body text missing: {text}");
        assert!(text.contains("FnText"), "footnote text missing: {text}");
    }

    #[test]
    fn text_includes_endnotes() {
        use crate::test_util::build_minimal_doc_with_notes;

        let doc_bytes = build_minimal_doc_with_notes("Body\r", "", "EndText\r");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert!(text.contains("Body"), "body text missing: {text}");
        assert!(text.contains("EndText"), "endnote text missing: {text}");
    }

    #[test]
    fn text_includes_footnotes_and_endnotes() {
        use crate::test_util::build_minimal_doc_with_notes;

        let doc_bytes = build_minimal_doc_with_notes("Body\r", "Fn1\r", "En1\r");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert!(text.contains("Body"), "body text missing: {text}");
        assert!(text.contains("Fn1"), "footnote text missing: {text}");
        assert!(text.contains("En1"), "endnote text missing: {text}");
    }

    #[test]
    fn no_notes_still_works() {
        use crate::test_util::build_minimal_doc_with_notes;

        let doc_bytes = build_minimal_doc_with_notes("Just body\r", "", "");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert_eq!(text, "Just body");
    }

    #[test]
    fn text_includes_headers_footers() {
        use crate::test_util::build_minimal_doc_with_all_stories;

        let doc_bytes = build_minimal_doc_with_all_stories("Body\r", "", "Page Header\r", "");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert!(text.contains("Body"), "body text missing: {text}");
        assert!(text.contains("Page Header"), "header text missing: {text}");
    }

    #[test]
    fn text_includes_all_stories() {
        use crate::test_util::build_minimal_doc_with_all_stories;

        let doc_bytes = build_minimal_doc_with_all_stories(
            "Body\r",
            "Footnote\r",
            "Header\rFooter\r",
            "Endnote\r",
        );
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert!(text.contains("Body"), "body text missing: {text}");
        assert!(text.contains("Footnote"), "footnote text missing: {text}");
        assert!(text.contains("Header"), "header text missing: {text}");
        assert!(text.contains("Footer"), "footer text missing: {text}");
        assert!(text.contains("Endnote"), "endnote text missing: {text}");
    }

    #[test]
    fn text_lines_includes_headers_footers() {
        use crate::test_util::build_minimal_doc_with_all_stories;

        let doc_bytes = build_minimal_doc_with_all_stories("Body\r", "", "HdrLine\r", "");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let lines = page.text_lines().expect("text_lines()");
        let texts: Vec<&str> = lines.iter().map(|l| l.spans[0].text.as_str()).collect();
        assert!(texts.contains(&"Body"), "body line missing: {texts:?}");
        assert!(texts.contains(&"HdrLine"), "header line missing: {texts:?}");
    }

    #[test]
    fn raw_spans_includes_headers_footers() {
        use crate::test_util::build_minimal_doc_with_all_stories;

        let doc_bytes = build_minimal_doc_with_all_stories("Body\r", "", "HdrSpan\r", "");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let spans = page.raw_spans().expect("raw_spans()");
        let texts: Vec<&str> = spans.iter().map(|s| s.text.as_str()).collect();
        assert!(texts.contains(&"Body"), "body span missing: {texts:?}");
        assert!(texts.contains(&"HdrSpan"), "header span missing: {texts:?}");
    }

    #[test]
    fn no_headers_footers_still_works() {
        use crate::test_util::build_minimal_doc_with_all_stories;

        let doc_bytes = build_minimal_doc_with_all_stories("Body only\r", "", "", "");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert_eq!(text, "Body only");
    }

    #[test]
    fn metadata_from_summary_information() {
        use crate::test_util::build_minimal_doc_with_metadata;

        let doc_bytes = build_minimal_doc_with_metadata(
            "Hello",
            &[
                (0x0002, "My Document Title"),
                (0x0003, "My Subject"),
                (0x0004, "Author Name"),
            ],
        );
        let doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let meta = doc.metadata();
        assert_eq!(meta.title.as_deref(), Some("My Document Title"));
        assert_eq!(meta.subject.as_deref(), Some("My Subject"));
        assert_eq!(meta.author.as_deref(), Some("Author Name"));
        assert_eq!(meta.page_count, 1);
    }

    #[test]
    fn metadata_partial_summary_information() {
        use crate::test_util::build_minimal_doc_with_metadata;

        // Only title, no subject or author.
        let doc_bytes = build_minimal_doc_with_metadata("Hello", &[(0x0002, "Just a Title")]);
        let doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let meta = doc.metadata();
        assert_eq!(meta.title.as_deref(), Some("Just a Title"));
        assert!(meta.subject.is_none());
        assert!(meta.author.is_none());
    }

    #[test]
    fn metadata_no_summary_information_stream() {
        // build_minimal_doc does not include a SummaryInformation stream.
        let doc_bytes = build_minimal_doc("Hello");
        let doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let meta = doc.metadata();
        assert!(meta.title.is_none());
        assert!(meta.subject.is_none());
        assert!(meta.author.is_none());
    }
}

//! DOCX document abstraction and trait implementations.
//!
//! Provides `DocxDocument` for opening and extracting content from DOCX files.
//! Implements `FormatBackend` and `PageExtractor` from udoc-core.

use std::sync::Arc;

use udoc_core::backend::{DocumentMetadata, FormatBackend, PageExtractor};
use udoc_core::diagnostics::DiagnosticsSink;
use udoc_core::error::{Error, Result};
use udoc_core::image::PageImage;
use udoc_core::table::Table;
use udoc_core::text::{TextLine, TextSpan};

use crate::parser::{BodyElement, Paragraph, ParsedDocument};
use crate::table::{DocxTableRow, VMergeState};

use crate::MAX_FILE_SIZE;

/// A flattened content item from the DOCX content hierarchy.
/// Used to walk headers -> body -> footnotes -> endnotes -> footers once.
enum ContentItem<'a> {
    Para(&'a Paragraph),
    TableRow(&'a DocxTableRow),
}

/// Iterate all content items from all document sections in order:
/// headers, body (paragraphs + table rows), footnotes, endnotes, footers.
///
/// Body elements need special handling (paragraphs vs table rows are different
/// variants), so body items are collected into a Vec. The other four sections
/// are flat paragraph lists that chain directly.
fn all_content_items(parsed: &ParsedDocument) -> impl Iterator<Item = ContentItem<'_>> {
    let headers = parsed
        .headers
        .iter()
        .flat_map(|paras| paras.iter().map(ContentItem::Para));

    // Body mixes paragraphs and table rows; collect to avoid type mismatch
    // in the iterator chain.
    let mut body_items = Vec::new();
    for elem in &parsed.body {
        match elem {
            BodyElement::Paragraph(para) => body_items.push(ContentItem::Para(para)),
            BodyElement::Table(tbl) => {
                for row in &tbl.rows {
                    body_items.push(ContentItem::TableRow(row));
                }
            }
        }
    }

    let footnotes = parsed
        .footnotes
        .iter()
        .flat_map(|note| note.paragraphs.iter().map(ContentItem::Para));

    let endnotes = parsed
        .endnotes
        .iter()
        .flat_map(|note| note.paragraphs.iter().map(ContentItem::Para));

    let footers = parsed
        .footers
        .iter()
        .flat_map(|paras| paras.iter().map(ContentItem::Para));

    headers
        .chain(body_items)
        .chain(footnotes)
        .chain(endnotes)
        .chain(footers)
}

/// Top-level DOCX document handle.
pub struct DocxDocument {
    parsed: ParsedDocument,
}

impl DocxDocument {
    udoc_core::define_backend_constructors!(MAX_FILE_SIZE, "DOCX");

    /// Parse DOCX from in-memory bytes with a diagnostics sink.
    ///
    /// The input bytes are cloned into an `Arc<[u8]>` so the parser can
    /// re-open the OPC package lazily for image decoding (#140). The
    /// text-only extraction path does not touch image bytes.
    pub fn from_bytes_with_diag(data: &[u8], diag: Arc<dyn DiagnosticsSink>) -> Result<Self> {
        let raw_bytes: Arc<[u8]> = Arc::from(data);
        let parsed = crate::parser::parse_docx(raw_bytes, diag)
            .map_err(|e| Error::with_source("parsing DOCX document", e))?;
        Ok(Self { parsed })
    }

    /// Returns warnings accumulated during parsing.
    pub fn warnings(&self) -> &[String] {
        &self.parsed.warnings
    }

    /// Access body elements in document order (paragraphs and tables interleaved).
    pub fn body(&self) -> &[BodyElement] {
        &self.parsed.body
    }

    /// Access the styles for heading/list resolution.
    ///
    /// Lazily parses styles.xml on first call. Subsequent calls return the
    /// cached result. The PageExtractor path (text/text_lines/raw_spans)
    /// does not trigger style parsing.
    pub fn styles(&self) -> &crate::styles::StyleMap {
        self.parsed.styles()
    }

    /// Returns true if styles.xml has been parsed (for testing lazy init).
    #[cfg(test)]
    pub(crate) fn styles_parsed(&self) -> bool {
        self.parsed.styles_parsed()
    }

    /// Returns true if image parts have been decoded (for testing lazy init).
    #[cfg(test)]
    pub(crate) fn images_loaded(&self) -> bool {
        self.parsed.images_loaded()
    }

    /// Access the numbering definitions.
    pub fn numbering(&self) -> &crate::numbering::NumberingDefs {
        &self.parsed.numbering
    }

    /// Access header paragraphs.
    pub fn headers(&self) -> &[Vec<Paragraph>] {
        &self.parsed.headers
    }

    /// Access footer paragraphs.
    pub fn footers(&self) -> &[Vec<Paragraph>] {
        &self.parsed.footers
    }

    /// Access footnotes.
    pub fn footnotes(&self) -> &[crate::parser::Footnote] {
        &self.parsed.footnotes
    }

    /// Access endnotes.
    pub fn endnotes(&self) -> &[crate::parser::Endnote] {
        &self.parsed.endnotes
    }

    /// Access bookmark names collected during parsing.
    pub fn bookmarks(&self) -> &[String] {
        &self.parsed.bookmarks
    }
}

/// Page handle for DOCX. The whole document is one logical "page"
/// since DOCX is a flow format without explicit page breaks.
pub struct DocxPage<'a> {
    parsed: &'a ParsedDocument,
}

impl FormatBackend for DocxDocument {
    type Page<'a> = DocxPage<'a>;

    fn page_count(&self) -> usize {
        1
    }

    fn page(&mut self, index: usize) -> Result<DocxPage<'_>> {
        udoc_core::backend::validate_single_page(index, "DOCX")?;
        Ok(DocxPage {
            parsed: &self.parsed,
        })
    }

    fn metadata(&self) -> DocumentMetadata {
        let mut meta = self.parsed.metadata.clone();
        meta.page_count = 1;
        meta
    }
}

/// Join a paragraph's visible runs into a single string.
///
/// Near-identical to udoc-rtf's paragraph_text. Kept separate because
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
/// Near-identical to udoc-rtf's paragraph_spans. Kept separate because
/// each operates on a format-specific Paragraph type (DOCX Run has
/// Option<bool> bold/italic, RTF Run has plain bool).
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
                r.font_size_pts.unwrap_or(0.0),
                r.bold.unwrap_or(false),
                r.italic.unwrap_or(false),
                false, // already filtered invisible runs
                0.0,
            )
        })
        .collect()
}

impl PageExtractor for DocxPage<'_> {
    fn text(&mut self) -> Result<String> {
        let mut parts: Vec<String> = Vec::new();

        for item in all_content_items(self.parsed) {
            match item {
                ContentItem::Para(para) => {
                    let text = paragraph_text(para);
                    if !text.is_empty() {
                        parts.push(text);
                    }
                }
                ContentItem::TableRow(row) => {
                    let row_text: Vec<&str> = row
                        .cells
                        .iter()
                        .filter(|c| c.v_merge != VMergeState::Continue)
                        .map(|c| c.text.as_str())
                        .collect();
                    if row_text.iter().any(|s| !s.is_empty()) {
                        parts.push(row_text.join("\t"));
                    }
                }
            }
        }

        Ok(parts.join("\n"))
    }

    fn text_lines(&mut self) -> Result<Vec<TextLine>> {
        let mut lines = Vec::new();
        let mut line_idx: usize = 0;

        for item in all_content_items(self.parsed) {
            match item {
                ContentItem::Para(para) => {
                    let y = line_idx as f64;
                    let spans = paragraph_spans(para, y);
                    if spans.iter().any(|s| !s.text.is_empty()) {
                        lines.push(TextLine::new(spans, y, false));
                        line_idx += 1;
                    }
                }
                ContentItem::TableRow(row) => {
                    let y = line_idx as f64;
                    let cell_spans: Vec<TextSpan> = row
                        .cells
                        .iter()
                        .filter(|c| c.v_merge != VMergeState::Continue)
                        .map(|c| TextSpan::new(c.text.clone(), 0.0, y, 0.0, 0.0))
                        .collect();
                    if cell_spans.iter().any(|s| !s.text.is_empty()) {
                        lines.push(TextLine::new(cell_spans, y, false));
                        line_idx += 1;
                    }
                }
            }
        }

        Ok(lines)
    }

    fn raw_spans(&mut self) -> Result<Vec<TextSpan>> {
        let mut all_spans = Vec::new();
        let mut para_idx: usize = 0;

        for item in all_content_items(self.parsed) {
            match item {
                ContentItem::Para(para) => {
                    let new_spans = paragraph_spans(para, para_idx as f64);
                    if new_spans.iter().any(|s| !s.text.is_empty()) {
                        all_spans.extend(new_spans);
                        para_idx += 1;
                    }
                }
                ContentItem::TableRow(row) => {
                    let y = para_idx as f64;
                    let cell_spans: Vec<TextSpan> = row
                        .cells
                        .iter()
                        .filter(|c| c.v_merge != VMergeState::Continue)
                        .map(|c| TextSpan::new(c.text.clone(), 0.0, y, 0.0, 0.0))
                        .collect();
                    if cell_spans.iter().any(|s| !s.text.is_empty()) {
                        all_spans.extend(cell_spans);
                        para_idx += 1;
                    }
                }
            }
        }

        Ok(all_spans)
    }

    fn tables(&mut self) -> Result<Vec<Table>> {
        Ok(self
            .parsed
            .body
            .iter()
            .filter_map(|elem| match elem {
                BodyElement::Table(tbl) => Some(crate::table::convert_table(tbl)),
                _ => None,
            })
            .collect())
    }

    fn images(&mut self) -> Result<Vec<PageImage>> {
        // First call to `images()` triggers the deferred image-bytes read
        // from the DOCX ZIP (#140). Subsequent calls reuse the cached
        // `Arc<[u8]>` data per image.
        let imgs = self.parsed.images();
        let mut result = Vec::with_capacity(imgs.len());
        for img in imgs {
            // PageImage takes `Vec<u8>`; go Arc -> Vec via one copy. If we
            // later lift PageImage to Arc<[u8]> this copy drops entirely.
            result.push(PageImage::from_data(img.data.as_ref().to_vec(), None));
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_containers::test_util::build_stored_zip;

    fn make_docx_bytes() -> Vec<u8> {
        let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

        let package_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
        Target="word/document.xml"/>
</Relationships>"#;

        let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Hello World</w:t></w:r></w:p>
        <w:p>
            <w:r>
                <w:rPr><w:b/><w:i/></w:rPr>
                <w:t xml:space="preserve">Bold and italic </w:t>
            </w:r>
            <w:r><w:t>normal text</w:t></w:r>
        </w:p>
        <w:p>
            <w:r><w:t>Unicode: </w:t></w:r>
            <w:r><w:t xml:space="preserve">cafe&#x301; </w:t></w:r>
        </w:p>
    </w:body>
</w:document>"#;

        build_stored_zip(&[
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", package_rels),
            ("word/document.xml", document_xml),
        ])
    }

    #[test]
    fn basic_text_extraction() {
        let data = make_docx_bytes();
        let mut doc = DocxDocument::from_bytes(&data).expect("from_bytes");
        assert_eq!(doc.page_count(), 1);

        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert!(text.contains("Hello World"), "got: {text}");
        assert!(text.contains("Bold and italic"), "got: {text}");
    }

    #[test]
    fn text_lines_basic() {
        let data = make_docx_bytes();
        let mut doc = DocxDocument::from_bytes(&data).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let lines = page.text_lines().expect("text_lines()");
        assert_eq!(lines.len(), 3, "expected 3 paragraphs as lines");
    }

    #[test]
    fn raw_spans_basic() {
        let data = make_docx_bytes();
        let mut doc = DocxDocument::from_bytes(&data).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let spans = page.raw_spans().expect("raw_spans()");
        // At least one span per run.
        assert!(
            spans.len() >= 3,
            "expected at least 3 spans, got {}",
            spans.len()
        );

        // Check bold/italic on second paragraph's first run.
        let bold_spans: Vec<_> = spans.iter().filter(|s| s.is_bold).collect();
        assert!(!bold_spans.is_empty(), "expected at least one bold span");
    }

    #[test]
    fn page_out_of_range() {
        let data = make_docx_bytes();
        let mut doc = DocxDocument::from_bytes(&data).expect("from_bytes");
        assert!(doc.page(1).is_err());
    }

    #[test]
    fn metadata_basic() {
        let data = make_docx_bytes();
        let doc = DocxDocument::from_bytes(&data).expect("from_bytes");
        let meta = doc.metadata();
        assert_eq!(meta.page_count, 1);
    }

    #[test]
    fn tables_empty_on_no_tables() {
        let data = make_docx_bytes();
        let mut doc = DocxDocument::from_bytes(&data).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let tables = page.tables().expect("tables()");
        assert!(tables.is_empty());
    }

    #[test]
    fn images_empty() {
        let data = make_docx_bytes();
        let mut doc = DocxDocument::from_bytes(&data).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let images = page.images().expect("images()");
        assert!(images.is_empty());
    }

    #[test]
    fn image_extraction_returns_png() {
        // Minimal 1x1 white PNG
        let png_data: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08,
            0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xE2, 0x21, 0xBC,
            0x33, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];

        let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Default Extension="png" ContentType="image/png"/>
    <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

        let package_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
        Target="word/document.xml"/>
</Relationships>"#;

        // Document rels with an image relationship
        let doc_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image"
        Target="media/image1.png"/>
</Relationships>"#;

        let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Hello with image</w:t></w:r></w:p>
    </w:body>
</w:document>"#;

        let data = build_stored_zip(&[
            ("[Content_Types].xml", content_types as &[u8]),
            ("_rels/.rels", package_rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml),
            ("word/media/image1.png", png_data),
        ]);

        let mut doc = DocxDocument::from_bytes(&data).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let images = page.images().expect("images()");
        assert_eq!(images.len(), 1, "should extract one image");
        assert_eq!(images[0].filter, udoc_core::image::ImageFilter::Png);
        assert_eq!(images[0].width, 1);
        assert_eq!(images[0].height, 1);
    }

    #[test]
    fn not_a_zip() {
        let result = DocxDocument::from_bytes(b"this is not a zip file");
        assert!(result.is_err(), "should fail on non-ZIP input");
    }

    #[test]
    fn text_extraction_does_not_trigger_image_decode() {
        // Regression for #140: text/text_lines must not read image ZIP parts.
        // Build a DOCX with an image, extract text, then assert images_loaded
        // stays false. Calling images() flips it true.
        let png_data: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08,
            0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xE2, 0x21, 0xBC,
            0x33, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Default Extension="png" ContentType="image/png"/>
    <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;
        let package_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
        let doc_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/>
</Relationships>"#;
        let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body><w:p><w:r><w:t>Hello with image</w:t></w:r></w:p></w:body>
</w:document>"#;
        let data = build_stored_zip(&[
            ("[Content_Types].xml", content_types as &[u8]),
            ("_rels/.rels", package_rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml),
            ("word/media/image1.png", png_data),
        ]);

        let mut doc = DocxDocument::from_bytes(&data).expect("from_bytes");
        assert!(
            !doc.images_loaded(),
            "images should not be loaded at open time"
        );

        {
            let mut page = doc.page(0).expect("page 0");
            let _text = page.text().expect("text()");
            let _lines = page.text_lines().expect("text_lines()");
            let _spans = page.raw_spans().expect("raw_spans()");
        }
        assert!(
            !doc.images_loaded(),
            "images should not be loaded after text-only extraction"
        );

        // First call to images() flips the flag.
        {
            let mut page = doc.page(0).expect("page 0");
            let imgs = page.images().expect("images()");
            assert_eq!(imgs.len(), 1);
        }
        assert!(
            doc.images_loaded(),
            "images should be loaded after images() call"
        );
    }

    #[test]
    fn empty_input() {
        let result = DocxDocument::from_bytes(b"");
        assert!(result.is_err(), "should fail on empty input");
    }

    #[test]
    fn zip_but_not_docx() {
        // Valid ZIP but missing [Content_Types].xml or officeDocument relationship.
        let data = build_stored_zip(&[("dummy.txt", b"hello" as &[u8])]);
        let result = DocxDocument::from_bytes(&data);
        assert!(result.is_err(), "should fail on ZIP without DOCX structure");
    }

    #[test]
    fn text_extraction_does_not_trigger_style_parse() {
        let data = make_docx_bytes();
        let mut doc = DocxDocument::from_bytes(&data).expect("from_bytes");

        // Styles should not be parsed yet (lazy init).
        assert!(
            !doc.styles_parsed(),
            "styles should not be parsed at open time"
        );

        // Extract text via PageExtractor -- should NOT trigger style parsing.
        {
            let mut page = doc.page(0).expect("page 0");
            let text = page.text().expect("text()");
            assert!(text.contains("Hello World"));
        }

        assert!(
            !doc.styles_parsed(),
            "styles should not be parsed after text extraction"
        );

        // Now access styles explicitly -- should trigger parsing.
        let _styles = doc.styles();
        assert!(
            doc.styles_parsed(),
            "styles should be parsed after explicit access"
        );
    }

    #[test]
    fn unicode_content() {
        let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

        let package_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
        Target="word/document.xml"/>
</Relationships>"#;

        let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>&#x4F60;&#x597D;&#x4E16;&#x754C;</w:t></w:r></w:p>
    </w:body>
</w:document>"#;

        let data = build_stored_zip(&[
            ("[Content_Types].xml", content_types as &[u8]),
            ("_rels/.rels", package_rels as &[u8]),
            ("word/document.xml", document_xml as &[u8]),
        ]);

        let mut doc = DocxDocument::from_bytes(&data).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert!(
            text.contains('\u{4F60}'),
            "should contain Chinese characters, got: {text}"
        );
    }
}

//! ODF document abstraction and trait implementations.
//!
//! Provides `OdfDocument` for opening and extracting content from ODF files
//! (ODT, ODS, ODP). Implements `FormatBackend` and `PageExtractor`.

use std::sync::Arc;

use udoc_containers::xml::XmlReader;
use udoc_containers::zip::ZipArchive;
use udoc_core::backend::{DocumentMetadata, FormatBackend, PageExtractor};
use udoc_core::diagnostics::{DiagnosticsSink, Warning};
use udoc_core::image::PageImage;
use udoc_core::table::{Table, TableCell, TableRow};
use udoc_core::text::{TextLine, TextSpan};

use crate::error::{Error, Result, ResultExt};
use crate::manifest::{self, OdfType};
use crate::meta;
use crate::odp::{self, OdpBody};
use crate::ods::{self, OdsBody};
use crate::odt::{self, OdtBody, OdtElement};
use crate::styles::{self, OdfStyleMap};

use crate::MAX_FILE_SIZE;

/// Internal body representation: one variant per ODF subformat.
enum OdfBody {
    Text(OdtBody),
    Spreadsheet(OdsBody),
    Presentation(OdpBody),
}

/// Top-level ODF document handle.
pub struct OdfDocument {
    body: OdfBody,
    styles: OdfStyleMap,
    metadata: DocumentMetadata,
    warnings: Vec<String>,
    /// Images extracted from the ODF package.
    images: Vec<PageImage>,
}

impl OdfDocument {
    udoc_core::define_backend_constructors!(MAX_FILE_SIZE, "ODF");

    /// Parse ODF from in-memory bytes with a diagnostics sink.
    pub fn from_bytes_with_diag(data: &[u8], diag: Arc<dyn DiagnosticsSink>) -> Result<Self> {
        let zip = ZipArchive::new(data, Arc::clone(&diag)).context("opening ODF ZIP archive")?;

        // Detect subformat from mimetype file or manifest.
        let doc_type = detect_doc_type(&zip, &diag)?;

        // Parse meta.xml for metadata.
        let metadata = match zip.find("meta.xml") {
            Some(entry) => match zip.read(entry) {
                Ok(meta_data) => meta::parse_meta(&meta_data).unwrap_or_default(),
                Err(e) => {
                    diag.warning(Warning::new(
                        "OdfMetaReadFailed",
                        format!("could not read meta.xml: {e}, using defaults"),
                    ));
                    DocumentMetadata::default()
                }
            },
            None => DocumentMetadata::default(),
        };

        // Read content.xml (required).
        let content_entry = zip
            .find("content.xml")
            .ok_or_else(|| Error::new("ODF archive missing content.xml"))?;
        let content_data = zip.read(content_entry).context("reading content.xml")?;

        // Only ODT (text documents) needs style parsing for bold/italic/heading
        // resolution. ODS and ODP never reference styles, so skip entirely to
        // avoid wasting ~48% of extraction time on spreadsheets/presentations.
        let style_map = if doc_type == OdfType::Text {
            // Parse automatic styles from content.xml first (they take precedence).
            let mut content_styles =
                parse_content_styles(&content_data, diag.as_ref()).unwrap_or_default();

            // Parse styles from styles.xml (document-level defaults) and merge.
            // merge_defaults keeps existing entries, so content.xml automatic styles
            // take precedence over styles.xml document styles.
            let doc_styles = match zip.find("styles.xml") {
                Some(entry) => match zip.read(entry) {
                    Ok(styles_data) => {
                        styles::parse_styles(&styles_data, diag.as_ref()).unwrap_or_default()
                    }
                    Err(e) => {
                        diag.warning(Warning::new(
                            "OdfStylesReadFailed",
                            format!("could not read styles.xml: {e}"),
                        ));
                        OdfStyleMap::default()
                    }
                },
                None => OdfStyleMap::default(),
            };
            content_styles.merge_defaults(doc_styles);
            content_styles
        } else {
            OdfStyleMap::default()
        };

        // Parse the body based on detected type.
        let body = match doc_type {
            OdfType::Text => {
                let odt_body = odt::parse_odt_body(&content_data, &style_map, diag.as_ref())
                    .context("parsing ODT body")?;
                OdfBody::Text(odt_body)
            }
            OdfType::Spreadsheet => {
                let ods_body = ods::parse_ods_body(&content_data, diag.as_ref())
                    .context("parsing ODS body")?;
                OdfBody::Spreadsheet(ods_body)
            }
            OdfType::Presentation => {
                let odp_body = odp::parse_odp_body(&content_data, diag.as_ref())
                    .context("parsing ODP body")?;
                OdfBody::Presentation(odp_body)
            }
        };

        let warnings = match &body {
            OdfBody::Text(b) => b.warnings.clone(),
            _ => Vec::new(),
        };

        // Extract images from the ZIP for ODT documents.
        let images = match &body {
            OdfBody::Text(b) => extract_odt_images(&b.image_refs, &zip, &diag),
            _ => Vec::new(),
        };

        Ok(Self {
            body,
            styles: style_map,
            metadata,
            warnings,
            images,
        })
    }

    /// Returns warnings accumulated during parsing.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Access the style map.
    pub(crate) fn styles(&self) -> &OdfStyleMap {
        &self.styles
    }

    /// Access the ODT body (if this is a text document).
    pub(crate) fn odt_body(&self) -> Option<&OdtBody> {
        match &self.body {
            OdfBody::Text(b) => Some(b),
            _ => None,
        }
    }

    /// Access the ODS body (if this is a spreadsheet).
    pub(crate) fn ods_body(&self) -> Option<&OdsBody> {
        match &self.body {
            OdfBody::Spreadsheet(b) => Some(b),
            _ => None,
        }
    }

    /// Returns whether any styles were parsed from this document.
    /// Used by tests to verify that ODS/ODP skip style parsing.
    #[cfg(test)]
    pub(crate) fn styles_parsed(&self) -> bool {
        !self.styles.is_empty()
    }

    /// Access the ODP body (if this is a presentation).
    pub(crate) fn odp_body(&self) -> Option<&OdpBody> {
        match &self.body {
            OdfBody::Presentation(b) => Some(b),
            _ => None,
        }
    }
}

/// Extract images from the ODF ZIP for ODT image references.
fn extract_odt_images(
    image_refs: &[odt::OdtImageRef],
    zip: &ZipArchive<'_>,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Vec<PageImage> {
    let mut images = Vec::new();
    for img_ref in image_refs {
        let entry = match zip.find(&img_ref.href) {
            Some(e) => e,
            None => {
                diag.warning(Warning::new(
                    "OdfImageMissing",
                    format!("image not found in ZIP: {}", img_ref.href),
                ));
                continue;
            }
        };
        let data = match zip.read(entry) {
            Ok(d) => d,
            Err(e) => {
                diag.warning(Warning::new(
                    "OdfImageReadFailed",
                    format!("could not read image {}: {e}", img_ref.href),
                ));
                continue;
            }
        };
        images.push(PageImage::from_data(data, None));
    }
    images
}

/// Detect ODF subformat from the ZIP archive.
fn detect_doc_type(zip: &ZipArchive<'_>, diag: &Arc<dyn DiagnosticsSink>) -> Result<OdfType> {
    // Try mimetype file first (per ODF spec, first entry, uncompressed).
    if let Some(entry) = zip.find("mimetype") {
        if let Ok(content) = zip.read_string(entry) {
            if let Some(dt) = manifest::detect_type_from_mimetype_file(&content) {
                return Ok(dt);
            }
        }
    }

    // Fall back to manifest.xml.
    if let Some(entry) = zip.find("META-INF/manifest.xml") {
        if let Ok(manifest_data) = zip.read(entry) {
            if let Ok(info) = manifest::parse_manifest(&manifest_data) {
                if let Some(dt) = info.doc_type {
                    return Ok(dt);
                }
            }
        }
    }

    // Last resort: scan content.xml for the body element type.
    if let Some(entry) = zip.find("content.xml") {
        if let Ok(content_data) = zip.read(entry) {
            if let Some(dt) = detect_from_content_xml(&content_data) {
                return Ok(dt);
            }
        }
    }

    diag.warning(Warning::new(
        "OdfTypeDetectionFailed",
        "could not determine ODF subformat, defaulting to ODT",
    ));
    Ok(OdfType::Text)
}

/// Detect ODF type by scanning content.xml for office:text/spreadsheet/presentation.
fn detect_from_content_xml(data: &[u8]) -> Option<OdfType> {
    let mut reader = XmlReader::new(data).ok()?;
    let ns_office = udoc_containers::xml::namespace::ns::OFFICE;

    loop {
        match reader.next_element() {
            Ok(udoc_containers::xml::XmlEvent::StartElement {
                local_name,
                namespace_uri,
                ..
            }) if namespace_uri.as_deref() == Some(ns_office) => match local_name.as_ref() {
                "text" => return Some(OdfType::Text),
                "spreadsheet" => return Some(OdfType::Spreadsheet),
                "presentation" => return Some(OdfType::Presentation),
                _ => {}
            },
            Ok(udoc_containers::xml::XmlEvent::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    None
}

/// Parse automatic styles from content.xml's office:automatic-styles section.
fn parse_content_styles(data: &[u8], diag: &dyn DiagnosticsSink) -> Result<OdfStyleMap> {
    styles::parse_styles(data, diag)
}

/// Page handle for ODF. The meaning depends on the subformat:
/// - ODT: single logical page (the whole document).
/// - ODS: one page per sheet.
/// - ODP: one page per slide.
pub struct OdfPage<'a> {
    doc: &'a OdfDocument,
    index: usize,
}

impl FormatBackend for OdfDocument {
    type Page<'a> = OdfPage<'a>;

    fn page_count(&self) -> usize {
        match &self.body {
            OdfBody::Text(_) => 1,
            OdfBody::Spreadsheet(b) => b.sheets.len(),
            OdfBody::Presentation(b) => b.slides.len(),
        }
    }

    fn page(&mut self, index: usize) -> Result<OdfPage<'_>> {
        let count = self.page_count();
        if index >= count {
            let unit = match &self.body {
                OdfBody::Text(_) => "page",
                OdfBody::Spreadsheet(_) => "sheet",
                OdfBody::Presentation(_) => "slide",
            };
            return Err(Error::new(format!(
                "{unit} index {index} out of range (document has {count} {unit}s)"
            )));
        }
        Ok(OdfPage { doc: self, index })
    }

    fn metadata(&self) -> DocumentMetadata {
        let mut meta = self.metadata.clone();
        meta.page_count = self.page_count();
        meta
    }
}

impl PageExtractor for OdfPage<'_> {
    fn text(&mut self) -> Result<String> {
        match &self.doc.body {
            OdfBody::Text(body) => {
                let mut parts = Vec::new();
                for elem in &body.elements {
                    match elem {
                        OdtElement::Paragraph(p) => {
                            let text = p.runs.iter().map(|r| r.text.as_str()).collect::<String>();
                            if !text.is_empty() {
                                parts.push(text);
                            }
                        }
                        OdtElement::Table(t) => {
                            for row in &t.rows {
                                let row_text: Vec<&str> =
                                    row.cells.iter().map(|c| c.text.as_str()).collect();
                                if row_text.iter().any(|s| !s.is_empty()) {
                                    parts.push(row_text.join("\t"));
                                }
                            }
                        }
                        OdtElement::List(l) => {
                            for item in &l.items {
                                for p in &item.paragraphs {
                                    let text =
                                        p.runs.iter().map(|r| r.text.as_str()).collect::<String>();
                                    if !text.is_empty() {
                                        parts.push(text);
                                    }
                                }
                            }
                        }
                    }
                }
                // Append footnotes and endnotes after body content.
                for p in &body.footnotes {
                    let text = p.runs.iter().map(|r| r.text.as_str()).collect::<String>();
                    if !text.is_empty() {
                        parts.push(text);
                    }
                }
                for p in &body.endnotes {
                    let text = p.runs.iter().map(|r| r.text.as_str()).collect::<String>();
                    if !text.is_empty() {
                        parts.push(text);
                    }
                }
                Ok(parts.join("\n"))
            }
            OdfBody::Spreadsheet(body) => {
                let sheet = &body.sheets[self.index];
                let mut lines = Vec::new();
                for row in &sheet.rows {
                    let cells: Vec<&str> = row.cells.iter().map(|c| c.text.as_str()).collect();
                    // Trim trailing empty cells.
                    let mut trimmed = cells.as_slice();
                    while trimmed.last().map(|s| s.is_empty()).unwrap_or(false) {
                        trimmed = &trimmed[..trimmed.len() - 1];
                    }
                    if trimmed.iter().any(|s| !s.is_empty()) {
                        lines.push(trimmed.join("\t"));
                    }
                }
                Ok(lines.join("\n"))
            }
            OdfBody::Presentation(body) => {
                let slide = &body.slides[self.index];
                let mut parts: Vec<&str> = slide
                    .paragraphs
                    .iter()
                    .filter(|p| !p.text.is_empty())
                    .map(|p| p.text.as_str())
                    .collect();
                if let Some(ref notes) = slide.notes {
                    if !notes.is_empty() {
                        parts.push(notes.as_str());
                    }
                }
                Ok(parts.join("\n"))
            }
        }
    }

    fn text_lines(&mut self) -> Result<Vec<TextLine>> {
        match &self.doc.body {
            OdfBody::Text(body) => {
                let mut lines = Vec::new();
                let mut line_idx: usize = 0;

                for elem in &body.elements {
                    match elem {
                        OdtElement::Paragraph(p) => {
                            let spans = paragraph_spans(p, line_idx as f64);
                            if spans.iter().any(|s| !s.text.is_empty()) {
                                lines.push(TextLine::new(spans, line_idx as f64, false));
                                line_idx += 1;
                            }
                        }
                        OdtElement::Table(t) => {
                            for row in &t.rows {
                                let y = line_idx as f64;
                                let spans: Vec<TextSpan> = row
                                    .cells
                                    .iter()
                                    .map(|c| TextSpan::new(c.text.clone(), 0.0, y, 0.0, 0.0))
                                    .collect();
                                if spans.iter().any(|s| !s.text.is_empty()) {
                                    lines.push(TextLine::new(spans, y, false));
                                    line_idx += 1;
                                }
                            }
                        }
                        OdtElement::List(l) => {
                            for item in &l.items {
                                for p in &item.paragraphs {
                                    let spans = paragraph_spans(p, line_idx as f64);
                                    if spans.iter().any(|s| !s.text.is_empty()) {
                                        lines.push(TextLine::new(spans, line_idx as f64, false));
                                        line_idx += 1;
                                    }
                                }
                            }
                        }
                    }
                }
                // Append footnotes and endnotes.
                for p in body.footnotes.iter().chain(body.endnotes.iter()) {
                    let spans = paragraph_spans(p, line_idx as f64);
                    if spans.iter().any(|s| !s.text.is_empty()) {
                        lines.push(TextLine::new(spans, line_idx as f64, false));
                        line_idx += 1;
                    }
                }
                Ok(lines)
            }
            OdfBody::Spreadsheet(body) => {
                let sheet = &body.sheets[self.index];
                let mut lines = Vec::new();
                for (row_idx, row) in sheet.rows.iter().enumerate() {
                    let y = row_idx as f64;
                    let spans: Vec<TextSpan> = row
                        .cells
                        .iter()
                        .filter(|c| !c.text.is_empty())
                        .enumerate()
                        .map(|(col_idx, c)| {
                            TextSpan::new(c.text.clone(), col_idx as f64, y, 0.0, 0.0)
                        })
                        .collect();
                    if !spans.is_empty() {
                        lines.push(TextLine::new(spans, y, false));
                    }
                }
                Ok(lines)
            }
            OdfBody::Presentation(body) => {
                let slide = &body.slides[self.index];
                let mut lines = Vec::new();
                for (idx, p) in slide.paragraphs.iter().enumerate() {
                    if p.text.is_empty() {
                        continue;
                    }
                    let y = idx as f64;
                    let span = TextSpan::new(p.text.clone(), 0.0, y, 0.0, 0.0);
                    lines.push(TextLine::new(vec![span], y, false));
                }
                // Include speaker notes to match text() output.
                if let Some(ref notes) = slide.notes {
                    if !notes.is_empty() {
                        let y = slide.paragraphs.len() as f64;
                        let span = TextSpan::new(notes.clone(), 0.0, y, 0.0, 0.0);
                        lines.push(TextLine::new(vec![span], y, false));
                    }
                }
                Ok(lines)
            }
        }
    }

    fn raw_spans(&mut self) -> Result<Vec<TextSpan>> {
        match &self.doc.body {
            OdfBody::Text(body) => {
                let mut spans = Vec::new();
                let mut para_idx: usize = 0;

                for elem in &body.elements {
                    match elem {
                        OdtElement::Paragraph(p) => {
                            let new_spans = paragraph_spans(p, para_idx as f64);
                            if new_spans.iter().any(|s| !s.text.is_empty()) {
                                spans.extend(new_spans);
                                para_idx += 1;
                            }
                        }
                        OdtElement::Table(t) => {
                            for row in &t.rows {
                                let y = para_idx as f64;
                                let cell_spans: Vec<TextSpan> = row
                                    .cells
                                    .iter()
                                    .map(|c| TextSpan::new(c.text.clone(), 0.0, y, 0.0, 0.0))
                                    .collect();
                                if cell_spans.iter().any(|s| !s.text.is_empty()) {
                                    spans.extend(cell_spans);
                                    para_idx += 1;
                                }
                            }
                        }
                        OdtElement::List(l) => {
                            for item in &l.items {
                                for p in &item.paragraphs {
                                    let new_spans = paragraph_spans(p, para_idx as f64);
                                    if new_spans.iter().any(|s| !s.text.is_empty()) {
                                        spans.extend(new_spans);
                                        para_idx += 1;
                                    }
                                }
                            }
                        }
                    }
                }
                // Append footnotes and endnotes.
                for p in body.footnotes.iter().chain(body.endnotes.iter()) {
                    let new_spans = paragraph_spans(p, para_idx as f64);
                    if new_spans.iter().any(|s| !s.text.is_empty()) {
                        spans.extend(new_spans);
                        para_idx += 1;
                    }
                }
                Ok(spans)
            }
            OdfBody::Spreadsheet(body) => {
                let sheet = &body.sheets[self.index];
                let spans: Vec<TextSpan> = sheet
                    .rows
                    .iter()
                    .enumerate()
                    .flat_map(|(row_idx, row)| {
                        row.cells
                            .iter()
                            .filter(|c| !c.text.is_empty())
                            .enumerate()
                            .map(move |(col_idx, c)| {
                                TextSpan::new(
                                    c.text.clone(),
                                    col_idx as f64,
                                    row_idx as f64,
                                    0.0,
                                    0.0,
                                )
                            })
                    })
                    .collect();
                Ok(spans)
            }
            OdfBody::Presentation(body) => {
                let slide = &body.slides[self.index];
                let mut spans: Vec<TextSpan> = slide
                    .paragraphs
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| !p.text.is_empty())
                    .map(|(idx, p)| TextSpan::new(p.text.clone(), 0.0, idx as f64, 0.0, 0.0))
                    .collect();
                // Include speaker notes to match text() output.
                if let Some(ref notes) = slide.notes {
                    if !notes.is_empty() {
                        let y = slide.paragraphs.len() as f64;
                        spans.push(TextSpan::new(notes.clone(), 0.0, y, 0.0, 0.0));
                    }
                }
                Ok(spans)
            }
        }
    }

    fn tables(&mut self) -> Result<Vec<Table>> {
        match &self.doc.body {
            OdfBody::Text(body) => Ok(body
                .elements
                .iter()
                .filter_map(|elem| match elem {
                    OdtElement::Table(t) => Some(convert_odt_table(t)),
                    _ => None,
                })
                .collect()),
            OdfBody::Spreadsheet(body) => {
                let sheet = &body.sheets[self.index];
                if sheet.rows.is_empty() {
                    return Ok(Vec::new());
                }

                let rows: Vec<TableRow> = sheet
                    .rows
                    .iter()
                    .map(|row| {
                        let cells: Vec<TableCell> = row
                            .cells
                            .iter()
                            .map(|c| {
                                TableCell::with_spans(c.text.clone(), None, c.col_span, c.row_span)
                            })
                            .collect();
                        TableRow::new(cells)
                    })
                    .collect();

                Ok(vec![Table::new(rows, None)])
            }
            OdfBody::Presentation(_) => {
                // ODP doesn't have tables in the same sense.
                Ok(Vec::new())
            }
        }
    }

    fn images(&mut self) -> Result<Vec<PageImage>> {
        // Clone required: OdfPage holds a shared ref to OdfDocument.
        // Called once per page during conversion. Images are only extracted
        // for ODT (single page), so this clone happens at most once.
        // ODS/ODP return empty since image extraction is ODT-only.
        if self.doc.images.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self.doc.images.clone())
    }
}

/// Convert an OdtParagraph into TextSpans.
fn paragraph_spans(para: &odt::OdtParagraph, y: f64) -> Vec<TextSpan> {
    para.runs
        .iter()
        .map(|r| {
            TextSpan::with_style(
                r.text.clone(),
                0.0,
                y,
                0.0,
                None,
                0.0,
                r.bold.unwrap_or(false),
                r.italic.unwrap_or(false),
                false,
                0.0,
            )
        })
        .collect()
}

/// Convert an OdtTable to udoc-core Table type (for convert.rs).
pub(crate) fn convert_odt_table_for_convert(t: &odt::OdtTable) -> Table {
    convert_odt_table(t)
}

/// Convert an OdtTable to udoc-core Table type.
fn convert_odt_table(t: &odt::OdtTable) -> Table {
    let rows: Vec<TableRow> = t
        .rows
        .iter()
        .map(|row| {
            let cells: Vec<TableCell> = row
                .cells
                .iter()
                .map(|c| TableCell::with_spans(c.text.clone(), None, c.col_span, c.row_span))
                .collect();
            TableRow::new(cells)
        })
        .collect();

    Table::new(rows, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_containers::test_util::build_stored_zip;

    fn make_odt_bytes(content_xml: &[u8]) -> Vec<u8> {
        build_stored_zip(&[
            (
                "mimetype",
                b"application/vnd.oasis.opendocument.text" as &[u8],
            ),
            ("content.xml", content_xml),
        ])
    }

    fn make_ods_bytes(content_xml: &[u8]) -> Vec<u8> {
        build_stored_zip(&[
            (
                "mimetype",
                b"application/vnd.oasis.opendocument.spreadsheet" as &[u8],
            ),
            ("content.xml", content_xml),
        ])
    }

    fn make_odp_bytes(content_xml: &[u8]) -> Vec<u8> {
        build_stored_zip(&[
            (
                "mimetype",
                b"application/vnd.oasis.opendocument.presentation" as &[u8],
            ),
            ("content.xml", content_xml),
        ])
    }

    #[test]
    fn odt_basic_text_extraction() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:p>Hello World</text:p>
      <text:p>Second line</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let mut doc = OdfDocument::from_bytes(&data).expect("from_bytes");
        assert_eq!(doc.page_count(), 1);

        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert!(text.contains("Hello World"), "got: {text}");
        assert!(text.contains("Second line"), "got: {text}");
    }

    #[test]
    fn odt_text_lines() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:p>Line 1</text:p>
      <text:p>Line 2</text:p>
      <text:p>Line 3</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let mut doc = OdfDocument::from_bytes(&data).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let lines = page.text_lines().expect("text_lines()");
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn ods_basic_extraction() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:spreadsheet>
      <table:table table:name="Sheet1">
        <table:table-row>
          <table:table-cell office:value-type="string"><text:p>A1</text:p></table:table-cell>
          <table:table-cell office:value-type="string"><text:p>B1</text:p></table:table-cell>
        </table:table-row>
      </table:table>
    </office:spreadsheet>
  </office:body>
</office:document-content>"#;

        let data = make_ods_bytes(content);
        let mut doc = OdfDocument::from_bytes(&data).expect("from_bytes");
        assert_eq!(doc.page_count(), 1);

        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert_eq!(text, "A1\tB1");
    }

    #[test]
    fn odp_basic_extraction() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
    xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:presentation>
      <draw:page draw:name="Slide1">
        <draw:frame presentation:class="title">
          <draw:text-box>
            <text:p>Title</text:p>
          </draw:text-box>
        </draw:frame>
      </draw:page>
    </office:presentation>
  </office:body>
</office:document-content>"#;

        let data = make_odp_bytes(content);
        let mut doc = OdfDocument::from_bytes(&data).expect("from_bytes");
        assert_eq!(doc.page_count(), 1);

        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert!(text.contains("Title"), "got: {text}");
    }

    #[test]
    fn page_out_of_range() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text><text:p>Hello</text:p></office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let mut doc = OdfDocument::from_bytes(&data).expect("from_bytes");
        assert!(doc.page(1).is_err());
    }

    #[test]
    fn not_a_zip() {
        let result = OdfDocument::from_bytes(b"this is not a zip file");
        assert!(result.is_err(), "should fail on non-ZIP input");
    }

    #[test]
    fn empty_input() {
        let result = OdfDocument::from_bytes(b"");
        assert!(result.is_err(), "should fail on empty input");
    }

    #[test]
    fn zip_but_not_odf() {
        let data = build_stored_zip(&[("dummy.txt", b"hello" as &[u8])]);
        let result = OdfDocument::from_bytes(&data);
        assert!(result.is_err(), "should fail on ZIP without ODF structure");
    }

    #[test]
    fn metadata_basic() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text><text:p>Hello</text:p></office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let doc = OdfDocument::from_bytes(&data).expect("from_bytes");
        let meta = doc.metadata();
        assert_eq!(meta.page_count, 1);
    }

    #[test]
    fn tables_empty_on_no_tables() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text><text:p>No tables here</text:p></office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let mut doc = OdfDocument::from_bytes(&data).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let tables = page.tables().expect("tables()");
        assert!(tables.is_empty());
    }

    #[test]
    fn images_empty() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text><text:p>Hello</text:p></office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let mut doc = OdfDocument::from_bytes(&data).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let images = page.images().expect("images()");
        assert!(images.is_empty());
    }

    fn make_odt_with_notes(body_text: &str, footnote_text: &str, endnote_text: &str) -> Vec<u8> {
        let mut notes = String::new();
        if !footnote_text.is_empty() {
            notes.push_str(&format!(
                r#"<text:note text:note-class="footnote"><text:note-citation>1</text:note-citation><text:note-body><text:p>{footnote_text}</text:p></text:note-body></text:note>"#
            ));
        }
        if !endnote_text.is_empty() {
            notes.push_str(&format!(
                r#"<text:note text:note-class="endnote"><text:note-citation>i</text:note-citation><text:note-body><text:p>{endnote_text}</text:p></text:note-body></text:note>"#
            ));
        }
        let content = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:p>{body_text}{notes}</text:p>
    </office:text>
  </office:body>
</office:document-content>"#
        );
        make_odt_bytes(content.as_bytes())
    }

    #[test]
    fn odt_text_includes_footnotes() {
        let data = make_odt_with_notes("Hello", "FnText", "");
        let mut doc = OdfDocument::from_bytes(&data).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert!(text.contains("Hello"), "body text missing: {text}");
        assert!(text.contains("FnText"), "footnote text missing: {text}");
    }

    #[test]
    fn odt_text_includes_endnotes() {
        let data = make_odt_with_notes("Body", "", "EnText");
        let mut doc = OdfDocument::from_bytes(&data).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert!(text.contains("Body"), "body text missing: {text}");
        assert!(text.contains("EnText"), "endnote text missing: {text}");
    }

    #[test]
    fn odt_text_includes_both_notes() {
        let data = make_odt_with_notes("Body", "Fn1", "En1");
        let mut doc = OdfDocument::from_bytes(&data).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert!(text.contains("Body"), "body text missing: {text}");
        assert!(text.contains("Fn1"), "footnote text missing: {text}");
        assert!(text.contains("En1"), "endnote text missing: {text}");
    }

    #[test]
    fn odt_text_lines_includes_notes() {
        let data = make_odt_with_notes("Body", "FnLine", "");
        let mut doc = OdfDocument::from_bytes(&data).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let lines = page.text_lines().expect("text_lines()");
        // Should have body line + footnote line.
        assert!(
            lines.len() >= 2,
            "expected at least 2 lines, got {}",
            lines.len()
        );
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            all_text.contains("FnLine"),
            "footnote missing from text_lines: {all_text}"
        );
    }

    #[test]
    fn odt_raw_spans_includes_notes() {
        let data = make_odt_with_notes("Body", "", "EnSpan");
        let mut doc = OdfDocument::from_bytes(&data).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let spans = page.raw_spans().expect("raw_spans()");
        let all_text: String = spans
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            all_text.contains("EnSpan"),
            "endnote missing from raw_spans: {all_text}"
        );
    }

    #[test]
    fn odt_no_notes_still_works() {
        let data = make_odt_with_notes("Just body", "", "");
        let mut doc = OdfDocument::from_bytes(&data).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert_eq!(text, "Just body");
    }

    #[test]
    fn test_ods_does_not_parse_styles() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:spreadsheet>
      <table:table table:name="Sheet1">
        <table:table-row>
          <table:table-cell office:value-type="string"><text:p>A1</text:p></table:table-cell>
        </table:table-row>
      </table:table>
    </office:spreadsheet>
  </office:body>
</office:document-content>"#;

        let data = make_ods_bytes(content);
        let mut doc = OdfDocument::from_bytes(&data).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let _text = page.text().expect("text()");
        assert!(
            !doc.styles_parsed(),
            "ODS should skip style parsing entirely"
        );
    }

    #[test]
    fn test_odt_does_parse_styles() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:automatic-styles>
    <style:style style:name="T1" style:family="text">
      <style:text-properties fo:font-weight="bold"/>
    </style:style>
  </office:automatic-styles>
  <office:body>
    <office:text>
      <text:p><text:span text:style-name="T1">Bold text</text:span></text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let doc = OdfDocument::from_bytes(&data).expect("from_bytes");
        assert!(
            doc.styles_parsed(),
            "ODT with style definitions should parse styles"
        );
    }

    #[test]
    fn odt_image_extraction() {
        // Minimal 1x1 white PNG
        let png_data: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
            0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xDE, // 8-bit RGB
            0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, // IDAT chunk
            0x08, 0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xE2, 0x21,
            0xBC, 0x33, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, // IEND
            0xAE, 0x42, 0x60, 0x82,
        ];

        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
    xmlns:xlink="http://www.w3.org/1999/xlink">
  <office:body>
    <office:text>
      <text:p>Before image</text:p>
      <draw:frame draw:name="img1">
        <draw:image xlink:href="Pictures/image1.png" xlink:type="simple"/>
      </draw:frame>
      <text:p>After image</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let data = build_stored_zip(&[
            (
                "mimetype",
                b"application/vnd.oasis.opendocument.text" as &[u8],
            ),
            ("content.xml", content as &[u8]),
            ("Pictures/image1.png", png_data),
        ]);

        let mut doc = OdfDocument::from_bytes(&data).expect("from_bytes");
        let mut page = doc.page(0).expect("page");
        let images = page.images().expect("images");
        assert_eq!(images.len(), 1, "should extract one image");
        assert_eq!(images[0].filter, udoc_core::image::ImageFilter::Png);
        assert_eq!(images[0].width, 1);
        assert_eq!(images[0].height, 1);

        // Text still works
        let text = page.text().expect("text");
        assert!(text.contains("Before image"));
        assert!(text.contains("After image"));
    }

    #[test]
    fn odt_no_images_returns_empty() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text><text:p>No images</text:p></office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let mut doc = OdfDocument::from_bytes(&data).expect("from_bytes");
        let mut page = doc.page(0).expect("page");
        let images = page.images().expect("images");
        assert!(images.is_empty());
    }
}

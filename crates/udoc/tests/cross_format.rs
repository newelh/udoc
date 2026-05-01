//! Cross-format metadata integration tests.
//!
//! Verifies that metadata() works correctly across all supported formats.
//! Each test constructs a minimal synthetic file in memory and exercises
//! the full facade path via udoc::extract_bytes() or udoc::Extractor.

use udoc_containers::test_util::{
    build_stored_zip, DOCX_PACKAGE_RELS_WITH_CORE, XLSX_PACKAGE_RELS_WITH_CORE,
    XLSX_WB_RELS_1SHEET, XLSX_WORKBOOK_1SHEET,
};
use udoc_doc::test_util::{build_minimal_doc, build_minimal_doc_with_metadata};
use udoc_ppt::test_util::{
    build_ppt_cfb, build_slide_persist_atom, build_text_chars_atom, build_text_header_atom,
};
use udoc_xls::test_util::build_minimal_xls;

// ---------------------------------------------------------------------------
// Shared core.xml for OOXML tests
// ---------------------------------------------------------------------------

const CORE_XML_WITH_TITLE: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties
    xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:dcterms="http://purl.org/dc/terms/">
    <dc:title>Cross-Format Test Title</dc:title>
    <dc:creator>Test Author</dc:creator>
</cp:coreProperties>"#;

// ---------------------------------------------------------------------------
// DOCX: page_count == 1, title from dc:title
// ---------------------------------------------------------------------------

fn make_docx_with_title() -> Vec<u8> {
    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Hello from DOCX metadata test</w:t></w:r></w:p>
    </w:body>
</w:document>"#;

    let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
    <Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>
</Types>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", DOCX_PACKAGE_RELS_WITH_CORE),
        ("word/document.xml", document_xml),
        ("docProps/core.xml", CORE_XML_WITH_TITLE),
    ])
}

#[test]
fn cross_format_docx_page_count() {
    let data = make_docx_with_title();
    let doc = udoc::extract_bytes(&data).expect("DOCX extract should succeed");
    assert_eq!(doc.metadata.page_count, 1, "DOCX page_count should be 1");
}

#[test]
fn cross_format_docx_title() {
    let data = make_docx_with_title();
    let doc = udoc::extract_bytes(&data).expect("DOCX extract should succeed");
    assert_eq!(
        doc.metadata.title.as_deref(),
        Some("Cross-Format Test Title"),
        "DOCX title should match dc:title"
    );
}

#[test]
fn cross_format_docx_author() {
    let data = make_docx_with_title();
    let doc = udoc::extract_bytes(&data).expect("DOCX extract should succeed");
    assert_eq!(
        doc.metadata.author.as_deref(),
        Some("Test Author"),
        "DOCX author should match dc:creator"
    );
}

// ---------------------------------------------------------------------------
// XLSX: page_count > 0 (one sheet = one page), title from dc:title
// ---------------------------------------------------------------------------

fn make_xlsx_with_title() -> Vec<u8> {
    let sheet_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1"><c r="A1"><v>42</v></c></row>
    </sheetData>
</worksheet>"#;

    let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
    <Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>
</Types>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", XLSX_PACKAGE_RELS_WITH_CORE),
        ("xl/workbook.xml", XLSX_WORKBOOK_1SHEET),
        ("xl/_rels/workbook.xml.rels", XLSX_WB_RELS_1SHEET),
        ("xl/worksheets/sheet1.xml", sheet_xml),
        ("docProps/core.xml", CORE_XML_WITH_TITLE),
    ])
}

#[test]
fn cross_format_xlsx_page_count() {
    let data = make_xlsx_with_title();
    let doc = udoc::extract_bytes(&data).expect("XLSX extract should succeed");
    assert!(
        doc.metadata.page_count > 0,
        "XLSX page_count should be > 0, got {}",
        doc.metadata.page_count
    );
}

#[test]
fn cross_format_xlsx_title() {
    let data = make_xlsx_with_title();
    let doc = udoc::extract_bytes(&data).expect("XLSX extract should succeed");
    assert_eq!(
        doc.metadata.title.as_deref(),
        Some("Cross-Format Test Title"),
        "XLSX title should match dc:title"
    );
}

// ---------------------------------------------------------------------------
// RTF: page_count == 1 (RTF is always single-page in our model)
// ---------------------------------------------------------------------------

fn make_rtf_bytes() -> Vec<u8> {
    b"{\\rtf1\\ansi{\\fonttbl{\\f0 Arial;}}\\f0 Hello from RTF metadata test.}".to_vec()
}

#[test]
fn cross_format_rtf_page_count() {
    let data = make_rtf_bytes();
    let doc = udoc::extract_bytes(&data).expect("RTF extract should succeed");
    assert_eq!(doc.metadata.page_count, 1, "RTF page_count should be 1");
}

// ---------------------------------------------------------------------------
// Markdown: page_count == 1
// ---------------------------------------------------------------------------

fn make_md_bytes() -> Vec<u8> {
    b"# Cross-Format Test\n\nThis is a markdown document for metadata testing.\n".to_vec()
}

#[test]
fn cross_format_md_page_count() {
    let data = make_md_bytes();
    let config = udoc::Config::new().format(udoc::Format::Md);
    let doc = udoc::extract_bytes_with(&data, config).expect("Markdown extract should succeed");
    assert_eq!(
        doc.metadata.page_count, 1,
        "Markdown page_count should be 1"
    );
}

// ---------------------------------------------------------------------------
// ODT: page_count == 1 (text document)
// ---------------------------------------------------------------------------

fn make_odt_bytes() -> Vec<u8> {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:p>ODT metadata cross-format test.</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

    build_stored_zip(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.text" as &[u8],
        ),
        ("content.xml", content_xml),
    ])
}

#[test]
fn cross_format_odt_page_count() {
    let data = make_odt_bytes();
    let doc = udoc::extract_bytes(&data).expect("ODT extract should succeed");
    assert_eq!(doc.metadata.page_count, 1, "ODT page_count should be 1");
}

// ---------------------------------------------------------------------------
// ODS: page_count > 0 (one sheet)
// ---------------------------------------------------------------------------

fn make_ods_bytes() -> Vec<u8> {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:spreadsheet>
      <table:table table:name="Sheet1">
        <table:table-row>
          <table:table-cell office:value-type="string"><text:p>ODS test</text:p></table:table-cell>
        </table:table-row>
      </table:table>
    </office:spreadsheet>
  </office:body>
</office:document-content>"#;

    build_stored_zip(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.spreadsheet" as &[u8],
        ),
        ("content.xml", content_xml),
    ])
}

#[test]
fn cross_format_ods_page_count() {
    let data = make_ods_bytes();
    let doc = udoc::extract_bytes(&data).expect("ODS extract should succeed");
    assert!(
        doc.metadata.page_count > 0,
        "ODS page_count should be > 0, got {}",
        doc.metadata.page_count
    );
}

// ---------------------------------------------------------------------------
// ODP: page_count > 0 (one slide)
// ---------------------------------------------------------------------------

fn make_odp_bytes() -> Vec<u8> {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:presentation>
      <draw:page draw:name="Slide1">
        <draw:frame>
          <draw:text-box>
            <text:p>ODP metadata cross-format test.</text:p>
          </draw:text-box>
        </draw:frame>
      </draw:page>
    </office:presentation>
  </office:body>
</office:document-content>"#;

    build_stored_zip(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.presentation" as &[u8],
        ),
        ("content.xml", content_xml),
    ])
}

#[test]
fn cross_format_odp_page_count() {
    let data = make_odp_bytes();
    let doc = udoc::extract_bytes(&data).expect("ODP extract should succeed");
    assert!(
        doc.metadata.page_count > 0,
        "ODP page_count should be > 0, got {}",
        doc.metadata.page_count
    );
}

// ---------------------------------------------------------------------------
// DOC: page_count == 1, title from SummaryInformation
// ---------------------------------------------------------------------------

// PIDSI_TITLE = 0x0002 per Windows property set spec.
const PIDSI_TITLE: u32 = 0x0002;
const PIDSI_AUTHOR: u32 = 0x0004;

#[test]
fn cross_format_doc_page_count() {
    let data = build_minimal_doc("DOC metadata cross-format test.");
    let doc = udoc::extract_bytes(&data).expect("DOC extract should succeed");
    assert_eq!(doc.metadata.page_count, 1, "DOC page_count should be 1");
}

#[test]
fn cross_format_doc_title() {
    let data = build_minimal_doc_with_metadata(
        "DOC document with title",
        &[
            (PIDSI_TITLE, "DOC Test Title"),
            (PIDSI_AUTHOR, "DOC Author"),
        ],
    );
    let doc = udoc::extract_bytes(&data).expect("DOC extract with metadata should succeed");
    assert_eq!(
        doc.metadata.title.as_deref(),
        Some("DOC Test Title"),
        "DOC title should come from SummaryInformation"
    );
}

// ---------------------------------------------------------------------------
// XLS: page_count > 0 (one sheet)
// ---------------------------------------------------------------------------

#[test]
fn cross_format_xls_page_count() {
    let data = build_minimal_xls(&["hello"], &[("Sheet1", &[(0, 0, "hello")])]);
    let doc = udoc::extract_bytes(&data).expect("XLS extract should succeed");
    assert!(
        doc.metadata.page_count > 0,
        "XLS page_count should be > 0, got {}",
        doc.metadata.page_count
    );
}

#[test]
fn cross_format_xls_multi_sheet_page_count() {
    let data = build_minimal_xls(
        &["a", "b"],
        &[("Sheet1", &[(0, 0, "a")]), ("Sheet2", &[(0, 0, "b")])],
    );
    let doc = udoc::extract_bytes(&data).expect("XLS multi-sheet extract should succeed");
    assert_eq!(
        doc.metadata.page_count, 2,
        "XLS with 2 sheets should have page_count == 2"
    );
}

// ---------------------------------------------------------------------------
// PPT: page_count > 0 (one slide)
// ---------------------------------------------------------------------------

fn make_ppt_bytes() -> Vec<u8> {
    let mut slwt = Vec::new();
    slwt.extend_from_slice(&build_slide_persist_atom(1));
    slwt.extend_from_slice(&build_text_header_atom(0));
    slwt.extend_from_slice(&build_text_chars_atom("PPT metadata cross-format test"));
    build_ppt_cfb(&slwt, &[])
}

#[test]
fn cross_format_ppt_page_count() {
    let data = make_ppt_bytes();
    let config = udoc::Config::new()
        .format(udoc::Format::Ppt)
        .layers(udoc::LayerConfig::content_only());
    let doc = udoc::extract_bytes_with(&data, config).expect("PPT extract should succeed");
    assert!(
        doc.metadata.page_count > 0,
        "PPT page_count should be > 0, got {}",
        doc.metadata.page_count
    );
}

// ---------------------------------------------------------------------------
// Extractor::format() round-trip: verify format detection works for all
// in-memory formats used above.
// ---------------------------------------------------------------------------

#[test]
fn cross_format_detect_docx() {
    let data = make_docx_with_title();
    let ext = udoc::Extractor::from_bytes(&data).expect("from_bytes should succeed");
    assert_eq!(ext.format(), udoc::Format::Docx);
}

#[test]
fn cross_format_detect_xlsx() {
    let data = make_xlsx_with_title();
    let ext = udoc::Extractor::from_bytes(&data).expect("from_bytes should succeed");
    assert_eq!(ext.format(), udoc::Format::Xlsx);
}

#[test]
fn cross_format_detect_odt() {
    let data = make_odt_bytes();
    let ext = udoc::Extractor::from_bytes(&data).expect("from_bytes should succeed");
    assert_eq!(ext.format(), udoc::Format::Odt);
}

#[test]
fn cross_format_detect_ods() {
    let data = make_ods_bytes();
    let ext = udoc::Extractor::from_bytes(&data).expect("from_bytes should succeed");
    assert_eq!(ext.format(), udoc::Format::Ods);
}

#[test]
fn cross_format_detect_odp() {
    let data = make_odp_bytes();
    let ext = udoc::Extractor::from_bytes(&data).expect("from_bytes should succeed");
    assert_eq!(ext.format(), udoc::Format::Odp);
}

// ---------------------------------------------------------------------------
// Extractor streaming: page_count() matches metadata.page_count
// ---------------------------------------------------------------------------

#[test]
fn cross_format_extractor_page_count_consistent_docx() {
    let data = make_docx_with_title();
    let ext = udoc::Extractor::from_bytes(&data).expect("from_bytes should succeed");
    let doc = udoc::extract_bytes(&data).expect("extract_bytes should succeed");
    assert_eq!(
        ext.page_count(),
        doc.metadata.page_count,
        "Extractor::page_count() should match Document::metadata.page_count for DOCX"
    );
}

#[test]
fn cross_format_extractor_page_count_consistent_xlsx() {
    let data = make_xlsx_with_title();
    let ext = udoc::Extractor::from_bytes(&data).expect("from_bytes should succeed");
    let doc = udoc::extract_bytes(&data).expect("extract_bytes should succeed");
    assert_eq!(
        ext.page_count(),
        doc.metadata.page_count,
        "Extractor::page_count() should match Document::metadata.page_count for XLSX"
    );
}

#[test]
fn cross_format_extractor_page_count_consistent_xls() {
    let data = build_minimal_xls(&["x"], &[("Sheet1", &[(0, 0, "x")])]);
    let ext = udoc::Extractor::from_bytes(&data).expect("from_bytes should succeed");
    let doc = udoc::extract_bytes(&data).expect("extract_bytes should succeed");
    assert_eq!(
        ext.page_count(),
        doc.metadata.page_count,
        "Extractor::page_count() should match Document::metadata.page_count for XLS"
    );
}

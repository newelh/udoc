//! XLSX integration tests for the udoc facade.

use udoc_containers::test_util::{
    build_stored_zip, XLSX_CONTENT_TYPES, XLSX_PACKAGE_RELS, XLSX_WB_RELS_1SHEET,
    XLSX_WORKBOOK_1SHEET,
};

/// Build a minimal XLSX ZIP with a single sheet.
fn make_xlsx_bytes(sheet_xml: &[u8]) -> Vec<u8> {
    build_stored_zip(&[
        ("[Content_Types].xml", XLSX_CONTENT_TYPES),
        ("_rels/.rels", XLSX_PACKAGE_RELS),
        ("xl/workbook.xml", XLSX_WORKBOOK_1SHEET),
        ("xl/_rels/workbook.xml.rels", XLSX_WB_RELS_1SHEET),
        ("xl/worksheets/sheet1.xml", sheet_xml),
    ])
}

// ---------------------------------------------------------------------------
// extract_bytes() one-shot
// ---------------------------------------------------------------------------

#[test]
fn extract_bytes_xlsx_basic() {
    let sheet = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1"><v>Hello</v></c>
            <c r="B1"><v>World</v></c>
        </row>
    </sheetData>
</worksheet>"#;
    let data = make_xlsx_bytes(sheet);
    let doc = udoc::extract_bytes(&data).expect("extract_bytes should succeed");

    assert!(!doc.content.is_empty(), "document should have content");
    assert_eq!(doc.metadata.page_count, 1);

    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        all_text.contains("Hello"),
        "should contain 'Hello', got: {all_text}"
    );
    assert!(
        all_text.contains("World"),
        "should contain 'World', got: {all_text}"
    );
}

#[test]
fn extract_bytes_xlsx_format_detection() {
    let sheet = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData/>
</worksheet>"#;
    let data = make_xlsx_bytes(sheet);

    let ext = udoc::Extractor::from_bytes(&data).expect("from_bytes should succeed");
    assert_eq!(ext.format(), udoc::Format::Xlsx);
}

// ---------------------------------------------------------------------------
// Extractor streaming
// ---------------------------------------------------------------------------

#[test]
fn extractor_xlsx_page_text() {
    let sheet = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1"><c r="A1"><v>42</v></c><c r="B1"><v>3.14</v></c></row>
        <row r="2"><c r="A2"><v>100</v></c></row>
    </sheetData>
</worksheet>"#;
    let data = make_xlsx_bytes(sheet);

    let mut ext = udoc::Extractor::from_bytes(&data).expect("from_bytes should succeed");
    assert_eq!(ext.page_count(), 1);

    let text = ext.page_text(0).expect("page_text should succeed");
    assert!(text.contains("42"), "got: {text}");
    assert!(text.contains("3.14"), "got: {text}");
    assert!(text.contains("100"), "got: {text}");
}

#[test]
fn extractor_xlsx_into_document() {
    let sheet = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1"><c r="A1"><v>test</v></c></row>
    </sheetData>
</worksheet>"#;
    let data = make_xlsx_bytes(sheet);

    let ext = udoc::Extractor::from_bytes(&data).expect("from_bytes should succeed");
    let doc = ext.into_document().expect("into_document should succeed");
    assert!(!doc.content.is_empty());
}

// ---------------------------------------------------------------------------
// Table extraction in Document model
// ---------------------------------------------------------------------------

#[test]
fn extract_xlsx_table_in_document_model() {
    let sheet = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1"><c r="A1"><v>Name</v></c><c r="B1"><v>Age</v></c></row>
        <row r="2"><c r="A2"><v>Alice</v></c><c r="B2"><v>30</v></c></row>
    </sheetData>
</worksheet>"#;
    let data = make_xlsx_bytes(sheet);
    let doc = udoc::extract_bytes(&data).expect("extract should succeed");

    let has_table = doc
        .content
        .iter()
        .any(|b| matches!(b, udoc::Block::Table { .. }));
    assert!(has_table, "XLSX should produce a Table block");
}

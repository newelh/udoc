//! Tests for XLSX malformed file recovery.
//!
//! Each test verifies that a malformed XLSX file can be handled without
//! panicking: either returning Err or Ok with partial content.

use std::sync::Arc;

use udoc_containers::test_util::{
    build_stored_zip, XLSX_CONTENT_TYPES, XLSX_PACKAGE_RELS, XLSX_WB_RELS_1SHEET,
    XLSX_WORKBOOK_1SHEET,
};
use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_core::diagnostics::CollectingDiagnostics;

// Aliases matching the short names used throughout these tests.
const XLSX_WORKBOOK: &[u8] = XLSX_WORKBOOK_1SHEET;
const XLSX_WB_RELS: &[u8] = XLSX_WB_RELS_1SHEET;

/// Random bytes that are not a ZIP file at all.
#[test]
fn truncated_zip_no_panic() {
    let garbage = b"This is definitely not a ZIP file \x00\xFF\xFE\x01";
    let result = udoc_xlsx::XlsxDocument::from_bytes(garbage);
    // Must not panic. Should return Err.
    assert!(result.is_err(), "truncated/garbage input should return Err");
}

/// Empty input.
#[test]
fn empty_input_no_panic() {
    let result = udoc_xlsx::XlsxDocument::from_bytes(b"");
    assert!(result.is_err(), "empty input should return Err");
}

/// Valid ZIP archive but missing xl/workbook.xml entirely.
#[test]
fn missing_workbook_xml() {
    // ZIP with only [Content_Types].xml and _rels/.rels, no xl/workbook.xml.
    let data = build_stored_zip(&[
        ("[Content_Types].xml", XLSX_CONTENT_TYPES),
        ("_rels/.rels", XLSX_PACKAGE_RELS),
    ]);
    let result = udoc_xlsx::XlsxDocument::from_bytes(&data);
    // Should fail gracefully (Err), not panic.
    assert!(result.is_err(), "missing xl/workbook.xml should return Err");
}

/// Valid OPC structure with workbook but sheet XML is missing from the ZIP.
#[test]
fn missing_sheet_data() {
    // Has workbook.xml and workbook rels pointing to sheet1.xml, but
    // xl/worksheets/sheet1.xml is not present in the archive.
    let data = build_stored_zip(&[
        ("[Content_Types].xml", XLSX_CONTENT_TYPES),
        ("_rels/.rels", XLSX_PACKAGE_RELS),
        ("xl/workbook.xml", XLSX_WORKBOOK),
        ("xl/_rels/workbook.xml.rels", XLSX_WB_RELS),
    ]);
    let diag = Arc::new(CollectingDiagnostics::new());
    let result = udoc_xlsx::XlsxDocument::from_bytes_with_diag(&data, diag.clone());
    // May succeed with empty raw_sheets or fail. Must not panic.
    match result {
        Err(_) => {} // expected
        Ok(mut doc) => {
            // If it parsed, accessing the sheet should fail gracefully.
            if let Ok(mut page) = doc.page(0) {
                let _ = page.text(); // must not panic
            }
            assert!(
                !diag.warnings().is_empty(),
                "missing sheet data should produce at least one warning"
            );
        }
    }
}

/// Valid ZIP + workbook + rels but the sheet XML itself is garbage.
#[test]
fn invalid_sheet_xml() {
    let bad_sheet = b"<<<this is not valid XML>>>&&&";
    let data = build_stored_zip(&[
        ("[Content_Types].xml", XLSX_CONTENT_TYPES),
        ("_rels/.rels", XLSX_PACKAGE_RELS),
        ("xl/workbook.xml", XLSX_WORKBOOK),
        ("xl/_rels/workbook.xml.rels", XLSX_WB_RELS),
        ("xl/worksheets/sheet1.xml", bad_sheet as &[u8]),
    ]);
    let diag = Arc::new(CollectingDiagnostics::new());
    let result = udoc_xlsx::XlsxDocument::from_bytes_with_diag(&data, diag.clone());
    // Should parse the package OK but fail when accessing the sheet.
    match result {
        Err(_) => {} // acceptable
        Ok(mut doc) => {
            // Parsing the sheet should either return Err or partial content.
            match doc.page(0) {
                Err(_) => {} // expected
                Ok(mut page) => {
                    let _ = page.text(); // must not panic
                }
            }
            // Note: the XLSX backend currently processes invalid sheet XML
            // at page() time, not at from_bytes() time, so diagnostics may
            // not be emitted if the error is returned instead.
        }
    }
}

/// Valid ZIP + workbook but the workbook XML has no <sheets> element.
#[test]
fn workbook_no_sheets_element() {
    let bad_workbook = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
    xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
</workbook>"#;

    let data = build_stored_zip(&[
        ("[Content_Types].xml", XLSX_CONTENT_TYPES),
        ("_rels/.rels", XLSX_PACKAGE_RELS),
        ("xl/workbook.xml", bad_workbook as &[u8]),
        ("xl/_rels/workbook.xml.rels", XLSX_WB_RELS),
    ]);
    let diag = Arc::new(CollectingDiagnostics::new());
    let result = udoc_xlsx::XlsxDocument::from_bytes_with_diag(&data, diag.clone());
    // Must not panic. May return Err or Ok with 0 pages.
    match result {
        Err(_) => {}
        Ok(doc) => {
            assert_eq!(doc.page_count(), 0, "empty workbook should have 0 sheets");
        }
    }
}

/// More realistic truncation: start of a ZIP signature then cut off.
#[test]
fn truncated_zip_partial_header_no_panic() {
    let partial = b"PK\x03\x04truncated";
    let result = udoc_xlsx::XlsxDocument::from_bytes(partial);
    assert!(result.is_err(), "truncated ZIP should return Err");
}

/// Sheet XML with wrong namespace. Parser should handle gracefully.
#[test]
fn wrong_namespace_no_panic() {
    let sheet_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://example.com/not-a-real-namespace">
    <sheetData>
        <row r="1">
            <c r="A1"><v>42</v></c>
        </row>
    </sheetData>
</worksheet>"#;

    let data = build_stored_zip(&[
        ("[Content_Types].xml", XLSX_CONTENT_TYPES),
        ("_rels/.rels", XLSX_PACKAGE_RELS),
        ("xl/workbook.xml", XLSX_WORKBOOK),
        ("xl/_rels/workbook.xml.rels", XLSX_WB_RELS),
        ("xl/worksheets/sheet1.xml", sheet_xml as &[u8]),
    ]);

    let diag = Arc::new(CollectingDiagnostics::new());
    let result = udoc_xlsx::XlsxDocument::from_bytes_with_diag(&data, diag.clone());
    // Must not panic. May return Err or Ok with empty/partial text.
    match result {
        Err(_) => {}
        Ok(mut doc) => {
            if let Ok(mut page) = doc.page(0) {
                let _ = page.text(); // must not panic
            }
            // Note: the XLSX backend currently parses wrong-namespace XML
            // without emitting diagnostics. A future improvement could warn
            // when the sheet namespace does not match the expected SML URI.
        }
    }
}

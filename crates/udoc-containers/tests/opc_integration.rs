//! Integration tests for the OPC package layer.
//!
//! These tests build minimal DOCX- and XLSX-like ZIP archives in memory and
//! exercise the full OPC navigation stack: content type lookup, package rels,
//! per-part rels, part reading, and relative URI resolution.

use std::sync::Arc;

use udoc_containers::opc::{rel_types, OpcPackage, TargetMode};
use udoc_containers::test_util::{build_stored_zip as build_zip, XLSX_PACKAGE_RELS};
use udoc_containers::xml::{ns, XmlEvent, XmlReader};
use udoc_core::diagnostics::NullDiagnostics;

// --------------------------------------------------------------------------
// DOCX fixture data
// --------------------------------------------------------------------------

// Not imported from test_util: this version has a styles.xml Override entry
// that the OPC navigation tests need for content_type_lookup assertions.
const DOCX_CONTENT_TYPES: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/word/document.xml"
        ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
    <Override PartName="/word/styles.xml"
        ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
</Types>"#;

// Not imported from test_util: this version has a core-properties relationship
// (rId2) that the OPC navigation tests assert on (pkg_rels.len() == 2).
const DOCX_PACKAGE_RELS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
        Target="word/document.xml"/>
    <Relationship Id="rId2"
        Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties"
        Target="docProps/core.xml"/>
</Relationships>"#;

const DOCX_DOCUMENT_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Hello from DOCX!</w:t></w:r></w:p>
        <w:p><w:r><w:t>Second run.</w:t></w:r></w:p>
    </w:body>
</w:document>"#;

const DOCX_DOCUMENT_RELS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles"
        Target="styles.xml"/>
    <Relationship Id="rId2"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink"
        Target="https://example.com"
        TargetMode="External"/>
</Relationships>"#;

const DOCX_STYLES_XML: &[u8] =
    b"<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"/>";

fn make_docx_zip() -> Vec<u8> {
    build_zip(&[
        ("[Content_Types].xml", DOCX_CONTENT_TYPES),
        ("_rels/.rels", DOCX_PACKAGE_RELS),
        ("word/document.xml", DOCX_DOCUMENT_XML),
        ("word/_rels/document.xml.rels", DOCX_DOCUMENT_RELS),
        ("word/styles.xml", DOCX_STYLES_XML),
    ])
}

// --------------------------------------------------------------------------
// XLSX fixture data
// --------------------------------------------------------------------------

// Not imported from test_util: this version has sheet1 and sharedStrings
// Override entries that the OPC content type lookup tests assert on.
const XLSX_CONTENT_TYPES: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/xl/workbook.xml"
        ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
    <Override PartName="/xl/worksheets/sheet1.xml"
        ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
    <Override PartName="/xl/sharedStrings.xml"
        ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>
</Types>"#;

// XLSX_PACKAGE_RELS is imported from test_util (semantically identical).

const XLSX_WORKBOOK_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
    <sheets>
        <sheet name="Sheet1" sheetId="1" r:id="rId1"/>
    </sheets>
</workbook>"#;

const XLSX_WORKBOOK_RELS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
        Target="worksheets/sheet1.xml"/>
    <Relationship Id="rId2"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings"
        Target="sharedStrings.xml"/>
</Relationships>"#;

const XLSX_SHEET1_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1"><c r="A1" t="s"><v>0</v></c></row>
    </sheetData>
</worksheet>"#;

const XLSX_SHARED_STRINGS_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1" uniqueCount="1">
    <si><t>Hello XLSX</t></si>
</sst>"#;

fn make_xlsx_zip() -> Vec<u8> {
    build_zip(&[
        ("[Content_Types].xml", XLSX_CONTENT_TYPES),
        ("_rels/.rels", XLSX_PACKAGE_RELS),
        ("xl/workbook.xml", XLSX_WORKBOOK_XML),
        ("xl/_rels/workbook.xml.rels", XLSX_WORKBOOK_RELS),
        ("xl/worksheets/sheet1.xml", XLSX_SHEET1_XML),
        ("xl/sharedStrings.xml", XLSX_SHARED_STRINGS_XML),
    ])
}

// --------------------------------------------------------------------------
// DOCX smoke test: navigate from package rels -> document -> styles
// --------------------------------------------------------------------------

/// Full DOCX navigation smoke test.
///
/// 1. Open the package.
/// 2. Find the officeDocument relationship in package rels.
/// 3. Read word/document.xml and parse it with XmlReader.
/// 4. Find the styles relationship in word/_rels/document.xml.rels.
/// 5. Resolve the styles URI relative to document.xml.
/// 6. Read word/styles.xml via the resolved URI.
#[test]
fn docx_navigate_package_to_main_document() {
    let zip = make_docx_zip();
    let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();

    // --- Step 1: package rels contain exactly 2 entries ---
    let pkg_rels = pkg.package_rels();
    assert_eq!(pkg_rels.len(), 2, "expected 2 package-level relationships");

    // --- Step 2: find officeDocument relationship ---
    let doc_rel = pkg
        .find_package_rel_by_type(rel_types::OFFICE_DOCUMENT)
        .expect("officeDocument relationship not found in package rels");

    assert_eq!(
        doc_rel.target, "word/document.xml",
        "officeDocument target should be word/document.xml"
    );
    assert_eq!(
        doc_rel.target_mode,
        TargetMode::Internal,
        "officeDocument should be an internal target"
    );
    assert_eq!(doc_rel.id, "rId1");

    // --- Step 3: read and parse word/document.xml ---
    let doc_xml = pkg.read_part_string(&doc_rel.target).unwrap();
    assert!(
        doc_xml.contains("Hello from DOCX!"),
        "document.xml should contain expected text"
    );

    // Parse with XmlReader to verify namespace resolution.
    let mut reader = XmlReader::new(doc_xml.as_bytes()).unwrap();
    let ev = reader.next_element().unwrap();
    match &ev {
        XmlEvent::StartElement {
            local_name,
            namespace_uri,
            ..
        } => {
            assert_eq!(local_name, "document");
            assert_eq!(namespace_uri.as_deref(), Some(ns::WML));
        }
        other => panic!("expected <w:document>, got: {other:?}"),
    }

    // --- Step 4: per-part relationships for word/document.xml ---
    let doc_part = format!("/{}", doc_rel.target);
    let doc_rels = pkg.part_rels(&doc_part);

    let styles = doc_rels
        .iter()
        .find(|r| r.rel_type == rel_types::STYLES)
        .expect("styles relationship not found in document rels");
    let link = doc_rels
        .iter()
        .find(|r| r.rel_type == rel_types::HYPERLINK)
        .expect("hyperlink relationship not found in document rels");

    assert_eq!(
        doc_rels.len(),
        2,
        "word/document.xml should have 2 relationships (styles + hyperlink)"
    );
    assert_eq!(styles.target, "styles.xml");
    assert_eq!(link.target, "https://example.com");
    assert_eq!(link.target_mode, TargetMode::External);

    // --- Step 5: resolve styles URI relative to document.xml ---
    let styles_uri = pkg.resolve_uri(&doc_part, &styles.target);
    assert_eq!(
        styles_uri, "/word/styles.xml",
        "resolved styles URI should be /word/styles.xml"
    );

    // --- Step 6: read styles.xml via resolved URI ---
    let styles_xml = pkg.read_part_string(&styles_uri).unwrap();
    assert!(
        styles_xml.contains("styles"),
        "styles.xml content should contain 'styles'"
    );
}

/// Content type lookup for Override and Default entries in a DOCX package.
#[test]
fn docx_content_type_lookup() {
    let zip = make_docx_zip();
    let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();

    // Override: exact PartName match.
    assert_eq!(
        pkg.content_type("/word/document.xml"),
        Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"),
        "Override content type for document.xml"
    );
    assert_eq!(
        pkg.content_type("/word/styles.xml"),
        Some("application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"),
        "Override content type for styles.xml"
    );

    // Default: matched by extension.
    assert_eq!(
        pkg.content_type("/_rels/.rels"),
        Some("application/vnd.openxmlformats-package.relationships+xml"),
        "Default content type for .rels extension"
    );
}

/// Reading a part via leading-slash and non-slash forms both work.
#[test]
fn docx_read_part_with_and_without_leading_slash() {
    let zip = make_docx_zip();
    let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();

    let without = pkg.read_part_string("word/document.xml").unwrap();
    let with_slash = pkg.read_part_string("/word/document.xml").unwrap();
    assert_eq!(
        without, with_slash,
        "leading slash must not affect part reading"
    );
    assert!(without.contains("Hello from DOCX!"));
}

/// Per-part rels are cached: calling part_rels twice returns the same data
/// without re-parsing the .rels file.
#[test]
fn docx_per_part_rels_cached() {
    let zip = make_docx_zip();
    let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();

    let rels_first = pkg.part_rels("/word/document.xml").to_vec();
    let rels_second = pkg.part_rels("/word/document.xml").to_vec();

    assert_eq!(rels_first.len(), rels_second.len());
    for (a, b) in rels_first.iter().zip(rels_second.iter()) {
        assert_eq!(a.id, b.id);
        assert_eq!(a.rel_type, b.rel_type);
        assert_eq!(a.target, b.target);
    }
}

/// A part with no .rels file returns an empty slice, not an error.
#[test]
fn docx_missing_rels_file_is_empty_not_error() {
    let zip = make_docx_zip();
    let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();

    // styles.xml has no corresponding word/_rels/styles.xml.rels in our fixture.
    let rels = pkg.part_rels("/word/styles.xml");
    assert!(
        rels.is_empty(),
        "no .rels file should yield empty rels, not an error"
    );
}

/// Requesting a part that does not exist returns an OPC error.
#[test]
fn docx_missing_part_is_error() {
    let zip = make_docx_zip();
    let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();

    let result = pkg.read_part("word/nonexistent.xml");
    assert!(result.is_err(), "missing part should return an error");

    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("not found"),
        "error message should mention 'not found', got: {msg}"
    );
}

/// Opening a ZIP that has no [Content_Types].xml fails with a clear OPC error.
#[test]
fn missing_content_types_returns_opc_error() {
    let zip = build_zip(&[("_rels/.rels", b"<Relationships/>")]);
    let result = OpcPackage::new(&zip, Arc::new(NullDiagnostics));
    assert!(result.is_err(), "missing Content_Types.xml should error");

    let msg = format!("{}", result.err().unwrap());
    assert!(
        msg.contains("Content_Types"),
        "error should mention Content_Types, got: {msg}"
    );
}

// --------------------------------------------------------------------------
// XLSX smoke test: navigate from package rels -> workbook -> sheet1
// --------------------------------------------------------------------------

/// Full XLSX navigation smoke test.
///
/// 1. Open the package.
/// 2. Find the officeDocument relationship pointing to xl/workbook.xml.
/// 3. Read and parse xl/workbook.xml to find the sheet r:id reference.
/// 4. Navigate workbook rels to find the worksheet relationship.
/// 5. Resolve the worksheet URI relative to workbook.xml.
/// 6. Read xl/worksheets/sheet1.xml and parse it.
/// 7. Navigate workbook rels to find the sharedStrings relationship.
/// 8. Read xl/sharedStrings.xml and verify content.
#[test]
fn xlsx_navigate_package_to_workbook_to_sheet() {
    let zip = make_xlsx_zip();
    let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();

    // --- Step 1+2: package rels -> workbook ---
    let wb_rel = pkg
        .find_package_rel_by_type(rel_types::OFFICE_DOCUMENT)
        .expect("officeDocument relationship not found");

    assert_eq!(wb_rel.target, "xl/workbook.xml");
    assert_eq!(wb_rel.target_mode, TargetMode::Internal);

    // --- Step 3: read and parse workbook.xml ---
    let wb_xml = pkg.read_part_string(&wb_rel.target).unwrap();
    assert!(
        wb_xml.contains("Sheet1"),
        "workbook.xml should reference Sheet1"
    );

    // Parse the workbook to extract the sheet r:id from the <sheet> element.
    let sheet_rid = {
        let mut reader = XmlReader::new(wb_xml.as_bytes()).unwrap();
        let mut found = None;
        loop {
            match reader.next_element().unwrap() {
                XmlEvent::StartElement {
                    local_name,
                    attributes,
                    ..
                } if local_name == "sheet" => {
                    // r:id attribute carries the relationship ID linking to the worksheet.
                    let rid_attr = attributes
                        .iter()
                        .find(|a| a.local_name == "id")
                        .expect("r:id on <sheet>");
                    assert_eq!(rid_attr.prefix, "r");
                    found = Some(rid_attr.value.clone());
                    break;
                }
                XmlEvent::Eof => break,
                _ => {}
            }
        }
        found.expect("<sheet> element with r:id not found in workbook.xml")
    };
    assert_eq!(sheet_rid, "rId1");

    // --- Step 4: workbook rels ---
    let wb_part = format!("/{}", wb_rel.target);
    let wb_rels = pkg.part_rels(&wb_part);

    let sheet_rel = wb_rels
        .iter()
        .find(|r| r.id == sheet_rid)
        .expect("worksheet rel not found by rId");
    let ss_rel = wb_rels
        .iter()
        .find(|r| r.rel_type == rel_types::SHARED_STRINGS)
        .expect("sharedStrings relationship not found in workbook rels");

    assert_eq!(
        wb_rels.len(),
        2,
        "workbook should have 2 rels (worksheet + sharedStrings)"
    );
    assert_eq!(sheet_rel.rel_type, rel_types::WORKSHEET);
    assert_eq!(sheet_rel.target, "worksheets/sheet1.xml");
    assert_eq!(sheet_rel.target_mode, TargetMode::Internal);
    assert_eq!(ss_rel.target, "sharedStrings.xml");

    // --- Step 5: resolve worksheet URI relative to workbook.xml ---
    let sheet_uri = pkg.resolve_uri(&wb_part, &sheet_rel.target);
    assert_eq!(
        sheet_uri, "/xl/worksheets/sheet1.xml",
        "worksheet URI should be /xl/worksheets/sheet1.xml"
    );

    // --- Step 6: read and parse sheet1.xml ---
    let sheet_xml = pkg.read_part_string(&sheet_uri).unwrap();
    assert!(
        sheet_xml.contains("sheetData"),
        "sheet1.xml should contain sheetData"
    );

    // Parse to verify the worksheet namespace resolves to SML.
    let mut reader = XmlReader::new(sheet_xml.as_bytes()).unwrap();
    let ev = reader.next_element().unwrap();
    match &ev {
        XmlEvent::StartElement {
            local_name,
            namespace_uri,
            ..
        } => {
            assert_eq!(local_name, "worksheet");
            assert_eq!(
                namespace_uri.as_deref(),
                Some(ns::SML),
                "worksheet element should resolve to SpreadsheetML namespace"
            );
        }
        other => panic!("expected <worksheet>, got: {other:?}"),
    }

    // --- Step 7+8: navigate to sharedStrings ---
    let ss_uri = pkg.resolve_uri(&wb_part, &ss_rel.target);
    assert_eq!(ss_uri, "/xl/sharedStrings.xml");

    let ss_xml = pkg.read_part_string(&ss_uri).unwrap();
    assert!(
        ss_xml.contains("Hello XLSX"),
        "sharedStrings.xml should contain 'Hello XLSX'"
    );
}

/// Content type lookups for XLSX Override entries and Default extensions.
#[test]
fn xlsx_content_type_lookup() {
    let zip = make_xlsx_zip();
    let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();

    assert_eq!(
        pkg.content_type("/xl/workbook.xml"),
        Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"),
        "Override content type for workbook.xml"
    );

    assert_eq!(
        pkg.content_type("/xl/worksheets/sheet1.xml"),
        Some("application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"),
        "Override content type for sheet1.xml"
    );

    assert_eq!(
        pkg.content_type("/_rels/.rels"),
        Some("application/vnd.openxmlformats-package.relationships+xml"),
        "Default content type for .rels files"
    );
}

/// Relative URI resolution from a deeply nested part (xl/worksheets/sheet1.xml)
/// correctly traverses parent directories.
#[test]
fn xlsx_relative_uri_resolution_from_worksheet() {
    let zip = make_xlsx_zip();
    let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();

    // From sheet1.xml, "../sharedStrings.xml" should resolve to /xl/sharedStrings.xml.
    let resolved = pkg.resolve_uri("/xl/worksheets/sheet1.xml", "../sharedStrings.xml");
    assert_eq!(resolved, "/xl/sharedStrings.xml");

    // From workbook.xml, "worksheets/sheet1.xml" -> /xl/worksheets/sheet1.xml.
    let resolved = pkg.resolve_uri("/xl/workbook.xml", "worksheets/sheet1.xml");
    assert_eq!(resolved, "/xl/worksheets/sheet1.xml");

    // Absolute target is returned normalized, ignoring source.
    let resolved = pkg.resolve_uri("/xl/worksheets/sheet1.xml", "/xl/workbook.xml");
    assert_eq!(resolved, "/xl/workbook.xml");
}

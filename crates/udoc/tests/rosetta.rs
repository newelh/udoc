//! Rosetta cross-format content tests.
//!
//! Verifies that the same logical content -- a heading, a paragraph naming
//! known strings, and a table -- is correctly extracted across all formats
//! that can represent it synthetically. Each test builds a minimal file
//! in memory, extracts it with udoc::extract_bytes(), and asserts that key
//! strings appear in the extracted text.
//!
//! The Rosetta content:
//!   Heading:   "Test Document"
//!   Paragraph: "Alice and Bob work at Acme Corp in London."
//!   Table row: Alice, 28, London
//!   Table row: Bob, 35, Paris
//!
//! No external files are used. Nothing is written to disk.

use udoc_containers::test_util::{
    build_stored_zip, DOCX_CONTENT_TYPES, DOCX_PACKAGE_RELS, XLSX_CONTENT_TYPES, XLSX_PACKAGE_RELS,
    XLSX_WB_RELS_1SHEET, XLSX_WORKBOOK_1SHEET,
};
use udoc_xls::test_util::build_minimal_xls;

// ---------------------------------------------------------------------------
// Content the Rosetta tests verify
// ---------------------------------------------------------------------------

const MUST_CONTAIN: &[&str] = &[
    "Test Document",
    "Alice",
    "Bob",
    "Acme Corp",
    "London",
    "Paris",
    "28",
    "35",
];

fn all_text(doc: &udoc::Document) -> String {
    doc.content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_rosetta_content(text: &str, format_name: &str) {
    for token in MUST_CONTAIN {
        assert!(
            text.contains(token),
            "{format_name}: expected '{token}' in extracted text, got:\n{text}"
        );
    }
}

// ---------------------------------------------------------------------------
// DOCX
// ---------------------------------------------------------------------------

fn make_rosetta_docx() -> Vec<u8> {
    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:pPr><w:pStyle w:val="Heading1"/></w:pPr>
      <w:r><w:t>Test Document</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Alice and Bob work at Acme Corp in London.</w:t></w:r>
    </w:p>
    <w:tbl>
      <w:tr>
        <w:tc><w:p><w:r><w:t>Name</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>Age</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>City</w:t></w:r></w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:tc><w:p><w:r><w:t>Alice</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>28</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>London</w:t></w:r></w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:tc><w:p><w:r><w:t>Bob</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>35</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>Paris</w:t></w:r></w:p></w:tc>
      </w:tr>
    </w:tbl>
  </w:body>
</w:document>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", DOCX_CONTENT_TYPES),
        ("_rels/.rels", DOCX_PACKAGE_RELS),
        ("word/document.xml", document_xml),
    ])
}

#[test]
fn rosetta_docx() {
    let data = make_rosetta_docx();
    let doc = udoc::extract_bytes(&data).expect("DOCX extract should succeed");
    assert_rosetta_content(&all_text(&doc), "DOCX");
}

// ---------------------------------------------------------------------------
// XLSX
// ---------------------------------------------------------------------------

fn make_rosetta_xlsx() -> Vec<u8> {
    // XLSX has no paragraph/heading layer. Embed "Test Document", "Acme Corp",
    // "Alice and Bob ... London." as cell values alongside the table data so
    // all Rosetta tokens are present.
    let sheet_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1" t="inlineStr"><is><t>Test Document</t></is></c>
    </row>
    <row r="2">
      <c r="A2" t="inlineStr"><is><t>Alice and Bob work at Acme Corp in London.</t></is></c>
    </row>
    <row r="3">
      <c r="A3" t="inlineStr"><is><t>Name</t></is></c>
      <c r="B3" t="inlineStr"><is><t>Age</t></is></c>
      <c r="C3" t="inlineStr"><is><t>City</t></is></c>
    </row>
    <row r="4">
      <c r="A4" t="inlineStr"><is><t>Alice</t></is></c>
      <c r="B4" t="inlineStr"><is><t>28</t></is></c>
      <c r="C4" t="inlineStr"><is><t>London</t></is></c>
    </row>
    <row r="5">
      <c r="A5" t="inlineStr"><is><t>Bob</t></is></c>
      <c r="B5" t="inlineStr"><is><t>35</t></is></c>
      <c r="C5" t="inlineStr"><is><t>Paris</t></is></c>
    </row>
  </sheetData>
</worksheet>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", XLSX_CONTENT_TYPES),
        ("_rels/.rels", XLSX_PACKAGE_RELS),
        ("xl/workbook.xml", XLSX_WORKBOOK_1SHEET),
        ("xl/_rels/workbook.xml.rels", XLSX_WB_RELS_1SHEET),
        ("xl/worksheets/sheet1.xml", sheet_xml),
    ])
}

#[test]
fn rosetta_xlsx() {
    let data = make_rosetta_xlsx();
    let doc = udoc::extract_bytes(&data).expect("XLSX extract should succeed");
    assert_rosetta_content(&all_text(&doc), "XLSX");
}

// ---------------------------------------------------------------------------
// RTF
// ---------------------------------------------------------------------------

fn make_rosetta_rtf() -> Vec<u8> {
    // Minimal RTF with heading, paragraph, and table rows.
    // RTF table syntax: \trowd marks row start, \cell ends a cell, \row ends a row.
    br#"{\rtf1\ansi
{\fonttbl{\f0 Arial;}}
\f0\fs24
{\pard\sb200\sa200 Test Document\par}
{\pard Alice and Bob work at Acme Corp in London.\par}
\trowd
\cellx2000\cellx3000\cellx5000
Name\cell 28\cell London\cell\row
\trowd
\cellx2000\cellx3000\cellx5000
Alice\cell 28\cell London\cell\row
\trowd
\cellx2000\cellx3000\cellx5000
Bob\cell 35\cell Paris\cell\row
}"#
    .to_vec()
}

#[test]
fn rosetta_rtf() {
    let data = make_rosetta_rtf();
    let doc = udoc::extract_bytes(&data).expect("RTF extract should succeed");
    assert_rosetta_content(&all_text(&doc), "RTF");
}

// ---------------------------------------------------------------------------
// Markdown
// ---------------------------------------------------------------------------

fn make_rosetta_md() -> Vec<u8> {
    b"# Test Document\n\nAlice and Bob work at Acme Corp in London.\n\n\
| Name  | Age | City   |\n\
|-------|-----|--------|\n\
| Alice | 28  | London |\n\
| Bob   | 35  | Paris  |\n"
        .to_vec()
}

#[test]
fn rosetta_md() {
    let data = make_rosetta_md();
    let config = udoc::Config::new().format(udoc::Format::Md);
    let doc = udoc::extract_bytes_with(&data, config).expect("Markdown extract should succeed");
    assert_rosetta_content(&all_text(&doc), "Markdown");
}

// ---------------------------------------------------------------------------
// ODT
// ---------------------------------------------------------------------------

fn make_rosetta_odt() -> Vec<u8> {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0">
  <office:body>
    <office:text>
      <text:h text:outline-level="1">Test Document</text:h>
      <text:p>Alice and Bob work at Acme Corp in London.</text:p>
      <table:table>
        <table:table-row>
          <table:table-cell><text:p>Name</text:p></table:table-cell>
          <table:table-cell><text:p>Age</text:p></table:table-cell>
          <table:table-cell><text:p>City</text:p></table:table-cell>
        </table:table-row>
        <table:table-row>
          <table:table-cell><text:p>Alice</text:p></table:table-cell>
          <table:table-cell><text:p>28</text:p></table:table-cell>
          <table:table-cell><text:p>London</text:p></table:table-cell>
        </table:table-row>
        <table:table-row>
          <table:table-cell><text:p>Bob</text:p></table:table-cell>
          <table:table-cell><text:p>35</text:p></table:table-cell>
          <table:table-cell><text:p>Paris</text:p></table:table-cell>
        </table:table-row>
      </table:table>
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
fn rosetta_odt() {
    let data = make_rosetta_odt();
    let doc = udoc::extract_bytes(&data).expect("ODT extract should succeed");
    assert_rosetta_content(&all_text(&doc), "ODT");
}

// ---------------------------------------------------------------------------
// XLS (via build_minimal_xls)
// ---------------------------------------------------------------------------

#[test]
fn rosetta_xls() {
    // build_minimal_xls only supports string cells via the SST.
    // Embed all Rosetta tokens as SST strings.
    let sst = &[
        "Test Document",
        "Alice and Bob work at Acme Corp in London.",
        "Name",
        "Age",
        "City",
        "Alice",
        "28",
        "London",
        "Bob",
        "35",
        "Paris",
    ];
    let cells: &[(u16, u16, &str)] = &[
        (0, 0, "Test Document"),
        (1, 0, "Alice and Bob work at Acme Corp in London."),
        (2, 0, "Name"),
        (2, 1, "Age"),
        (2, 2, "City"),
        (3, 0, "Alice"),
        (3, 1, "28"),
        (3, 2, "London"),
        (4, 0, "Bob"),
        (4, 1, "35"),
        (4, 2, "Paris"),
    ];
    let data = build_minimal_xls(sst, &[("Sheet1", cells)]);
    let doc = udoc::extract_bytes(&data).expect("XLS extract should succeed");
    assert_rosetta_content(&all_text(&doc), "XLS");
}

//! Golden file tests for XLSX text extraction.
//!
//! Run with `BLESS=1 cargo test -p udoc-xlsx --test golden_files` to update expected files.

use std::path::PathBuf;
use udoc_containers::test_util::{
    build_stored_zip, XLSX_CONTENT_TYPES, XLSX_PACKAGE_RELS, XLSX_PACKAGE_RELS_WITH_CORE,
    XLSX_WB_RELS_1SHEET, XLSX_WORKBOOK_1SHEET,
};
use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_core::test_harness::assert_golden;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

// ---------------------------------------------------------------------------
// Shared XLSX ZIP scaffolding (format-specific multi-sheet constants only)
// ---------------------------------------------------------------------------

/// Two-sheet workbook.xml.
const XLSX_WORKBOOK_2SHEET: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
    xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
    <sheets>
        <sheet name="Sheet1" sheetId="1" r:id="rId1"/>
        <sheet name="Sheet2" sheetId="2" r:id="rId2"/>
    </sheets>
</workbook>"#;

/// Workbook rels for two sheets.
const XLSX_WB_RELS_2SHEET: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
        Target="worksheets/sheet1.xml"/>
    <Relationship Id="rId2"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
        Target="worksheets/sheet2.xml"/>
</Relationships>"#;

// ---------------------------------------------------------------------------
// basic_text -- single sheet with text cells in A1 and B1
// ---------------------------------------------------------------------------

fn build_basic_text_xlsx() -> Vec<u8> {
    let sheet1 = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" t="inlineStr"><is><t>Hello</t></is></c>
            <c r="B1" t="inlineStr"><is><t>World</t></is></c>
        </row>
    </sheetData>
</worksheet>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", XLSX_CONTENT_TYPES),
        ("_rels/.rels", XLSX_PACKAGE_RELS),
        ("xl/workbook.xml", XLSX_WORKBOOK_1SHEET),
        ("xl/_rels/workbook.xml.rels", XLSX_WB_RELS_1SHEET),
        ("xl/worksheets/sheet1.xml", sheet1),
    ])
}

#[test]
fn golden_basic_text() {
    let data = build_basic_text_xlsx();
    let mut doc = udoc_xlsx::XlsxDocument::from_bytes(&data).expect("from_bytes");
    assert_eq!(doc.page_count(), 1);
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("basic_text", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// numeric_cells -- sheet with numeric values and text
// ---------------------------------------------------------------------------

fn build_numeric_cells_xlsx() -> Vec<u8> {
    let sheet1 = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" t="inlineStr"><is><t>Name</t></is></c>
            <c r="B1" t="inlineStr"><is><t>Value</t></is></c>
        </row>
        <row r="2">
            <c r="A2" t="inlineStr"><is><t>Pi</t></is></c>
            <c r="B2"><v>3.14159</v></c>
        </row>
        <row r="3">
            <c r="A3" t="inlineStr"><is><t>Answer</t></is></c>
            <c r="B3"><v>42</v></c>
        </row>
    </sheetData>
</worksheet>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", XLSX_CONTENT_TYPES),
        ("_rels/.rels", XLSX_PACKAGE_RELS),
        ("xl/workbook.xml", XLSX_WORKBOOK_1SHEET),
        ("xl/_rels/workbook.xml.rels", XLSX_WB_RELS_1SHEET),
        ("xl/worksheets/sheet1.xml", sheet1),
    ])
}

#[test]
fn golden_numeric_cells() {
    let data = build_numeric_cells_xlsx();
    let mut doc = udoc_xlsx::XlsxDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("numeric_cells", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// multi_sheet -- two sheets, verify page_count and text from each
// ---------------------------------------------------------------------------

fn build_multi_sheet_xlsx() -> Vec<u8> {
    let sheet1 = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" t="inlineStr"><is><t>Sheet one content</t></is></c>
        </row>
    </sheetData>
</worksheet>"#;

    let sheet2 = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" t="inlineStr"><is><t>Sheet two content</t></is></c>
        </row>
        <row r="2">
            <c r="A2" t="inlineStr"><is><t>Second row</t></is></c>
        </row>
    </sheetData>
</worksheet>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", XLSX_CONTENT_TYPES),
        ("_rels/.rels", XLSX_PACKAGE_RELS),
        ("xl/workbook.xml", XLSX_WORKBOOK_2SHEET),
        ("xl/_rels/workbook.xml.rels", XLSX_WB_RELS_2SHEET),
        ("xl/worksheets/sheet1.xml", sheet1),
        ("xl/worksheets/sheet2.xml", sheet2),
    ])
}

#[test]
fn golden_multi_sheet() {
    let data = build_multi_sheet_xlsx();
    let mut doc = udoc_xlsx::XlsxDocument::from_bytes(&data).expect("from_bytes");
    assert_eq!(doc.page_count(), 2);

    let mut page0 = doc.page(0).expect("page 0");
    let text0 = page0.text().expect("text() page 0");
    assert_golden("multi_sheet_page0", &text0, &golden_dir());

    let mut page1 = doc.page(1).expect("page 1");
    let text1 = page1.text().expect("text() page 1");
    assert_golden("multi_sheet_page1", &text1, &golden_dir());
}

// ---------------------------------------------------------------------------
// empty_sheet -- sheet with no data
// ---------------------------------------------------------------------------

fn build_empty_sheet_xlsx() -> Vec<u8> {
    let sheet1 = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData/>
</worksheet>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", XLSX_CONTENT_TYPES),
        ("_rels/.rels", XLSX_PACKAGE_RELS),
        ("xl/workbook.xml", XLSX_WORKBOOK_1SHEET),
        ("xl/_rels/workbook.xml.rels", XLSX_WB_RELS_1SHEET),
        ("xl/worksheets/sheet1.xml", sheet1),
    ])
}

#[test]
fn golden_empty_sheet() {
    let data = build_empty_sheet_xlsx();
    let mut doc = udoc_xlsx::XlsxDocument::from_bytes(&data).expect("from_bytes");
    assert_eq!(doc.page_count(), 1);
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("empty_sheet", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// metadata -- XLSX with docProps/core.xml, verify title/author
// ---------------------------------------------------------------------------

fn build_metadata_xlsx() -> Vec<u8> {
    let sheet1 = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" t="inlineStr"><is><t>Data with metadata</t></is></c>
        </row>
    </sheetData>
</worksheet>"#;

    let core_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties
    xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:dcterms="http://purl.org/dc/terms/">
    <dc:title>Quarterly Sales Report</dc:title>
    <dc:creator>Jane Doe</dc:creator>
    <dc:subject>Sales Data Q4</dc:subject>
</cp:coreProperties>"#;

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
        ("xl/worksheets/sheet1.xml", sheet1),
        ("docProps/core.xml", core_xml),
    ])
}

#[test]
fn golden_metadata_text() {
    let data = build_metadata_xlsx();
    let mut doc = udoc_xlsx::XlsxDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("metadata_text", &text, &golden_dir());
}

#[test]
fn golden_metadata_fields() {
    let data = build_metadata_xlsx();
    let doc = udoc_xlsx::XlsxDocument::from_bytes(&data).expect("from_bytes");
    let meta = doc.metadata();

    let output = format!(
        "title: {}\nauthor: {}\nsubject: {}\npage_count: {}",
        meta.title.as_deref().unwrap_or("(none)"),
        meta.author.as_deref().unwrap_or("(none)"),
        meta.subject.as_deref().unwrap_or("(none)"),
        meta.page_count,
    );
    assert_golden("metadata", &output, &golden_dir());
}

// ---------------------------------------------------------------------------
// table_extraction -- verify tables() on a simple grid
// ---------------------------------------------------------------------------

fn build_table_xlsx() -> Vec<u8> {
    let sheet1 = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" t="inlineStr"><is><t>Name</t></is></c>
            <c r="B1" t="inlineStr"><is><t>Score</t></is></c>
            <c r="C1" t="inlineStr"><is><t>Grade</t></is></c>
        </row>
        <row r="2">
            <c r="A2" t="inlineStr"><is><t>Alice</t></is></c>
            <c r="B2"><v>95</v></c>
            <c r="C2" t="inlineStr"><is><t>A</t></is></c>
        </row>
        <row r="3">
            <c r="A3" t="inlineStr"><is><t>Bob</t></is></c>
            <c r="B3"><v>82</v></c>
            <c r="C3" t="inlineStr"><is><t>B</t></is></c>
        </row>
    </sheetData>
</worksheet>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", XLSX_CONTENT_TYPES),
        ("_rels/.rels", XLSX_PACKAGE_RELS),
        ("xl/workbook.xml", XLSX_WORKBOOK_1SHEET),
        ("xl/_rels/workbook.xml.rels", XLSX_WB_RELS_1SHEET),
        ("xl/worksheets/sheet1.xml", sheet1),
    ])
}

#[test]
fn golden_table_text() {
    let data = build_table_xlsx();
    let mut doc = udoc_xlsx::XlsxDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("table_text", &text, &golden_dir());
}

#[test]
fn table_extraction() {
    let data = build_table_xlsx();
    let mut doc = udoc_xlsx::XlsxDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let tables = page.tables().expect("tables()");
    assert_eq!(tables.len(), 1, "expected 1 table");
    assert_eq!(tables[0].rows.len(), 3, "expected 3 rows");
    assert_eq!(tables[0].rows[0].cells.len(), 3, "expected 3 cells per row");
    assert_eq!(tables[0].rows[0].cells[0].text, "Name");
    assert_eq!(tables[0].rows[0].cells[1].text, "Score");
    assert_eq!(tables[0].rows[1].cells[0].text, "Alice");
    assert_eq!(tables[0].rows[1].cells[1].text, "95");
    assert_eq!(tables[0].rows[2].cells[0].text, "Bob");
    assert_eq!(tables[0].rows[2].cells[2].text, "B");
}

// ---------------------------------------------------------------------------
// merged_cells -- sheet with <mergeCells> entries; verify col_span/row_span
// ---------------------------------------------------------------------------

fn build_merged_cells_xlsx() -> Vec<u8> {
    // Layout:
    //   Row 1: "Header" spans A1:C1 (col_span=3)
    //   Row 2: "Left" in A2, "Right" spans B2:C2 (col_span=2)
    //   Row 3: "Tall" in A3 spans A3:A4 (row_span=2), "X" in B3, "Y" in C3
    //   Row 4: (A4 covered by merge), "P" in B4, "Q" in C4
    let sheet1 = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" t="inlineStr"><is><t>Header</t></is></c>
        </row>
        <row r="2">
            <c r="A2" t="inlineStr"><is><t>Left</t></is></c>
            <c r="B2" t="inlineStr"><is><t>Right</t></is></c>
        </row>
        <row r="3">
            <c r="A3" t="inlineStr"><is><t>Tall</t></is></c>
            <c r="B3" t="inlineStr"><is><t>X</t></is></c>
            <c r="C3" t="inlineStr"><is><t>Y</t></is></c>
        </row>
        <row r="4">
            <c r="B4" t="inlineStr"><is><t>P</t></is></c>
            <c r="C4" t="inlineStr"><is><t>Q</t></is></c>
        </row>
    </sheetData>
    <mergeCells count="3">
        <mergeCell ref="A1:C1"/>
        <mergeCell ref="B2:C2"/>
        <mergeCell ref="A3:A4"/>
    </mergeCells>
</worksheet>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", XLSX_CONTENT_TYPES),
        ("_rels/.rels", XLSX_PACKAGE_RELS),
        ("xl/workbook.xml", XLSX_WORKBOOK_1SHEET),
        ("xl/_rels/workbook.xml.rels", XLSX_WB_RELS_1SHEET),
        ("xl/worksheets/sheet1.xml", sheet1),
    ])
}

#[test]
fn golden_merged_cells_text() {
    let data = build_merged_cells_xlsx();
    let mut doc = udoc_xlsx::XlsxDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("merged_cells_text", &text, &golden_dir());
}

#[test]
fn merged_cells_spans() {
    let data = build_merged_cells_xlsx();
    let mut doc = udoc_xlsx::XlsxDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let tables = page.tables().expect("tables()");
    assert_eq!(tables.len(), 1, "expected 1 table");

    // Row 0: "Header" should span 3 columns.
    let row0 = &tables[0].rows[0];
    assert_eq!(row0.cells[0].text, "Header");
    assert_eq!(
        row0.cells[0].col_span, 3,
        "Header should have col_span=3, got {}",
        row0.cells[0].col_span
    );

    // Row 1: "Right" should span 2 columns.
    let row1 = &tables[0].rows[1];
    let right = row1
        .cells
        .iter()
        .find(|c| c.text == "Right")
        .expect("Right cell");
    assert_eq!(
        right.col_span, 2,
        "Right should have col_span=2, got {}",
        right.col_span
    );

    // Row 2: "Tall" should span 2 rows.
    let row2 = &tables[0].rows[2];
    let tall = row2
        .cells
        .iter()
        .find(|c| c.text == "Tall")
        .expect("Tall cell");
    assert_eq!(
        tall.row_span, 2,
        "Tall should have row_span=2, got {}",
        tall.row_span
    );
}

// ---------------------------------------------------------------------------
// shared_strings -- sheet using sharedStrings.xml (not inline strings)
// ---------------------------------------------------------------------------

fn build_shared_strings_xlsx() -> Vec<u8> {
    let shared_strings = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="6" uniqueCount="6">
    <si><t>Department</t></si>
    <si><t>Budget</t></si>
    <si><t>Engineering</t></si>
    <si><t>Marketing</t></si>
    <si><t>Operations</t></si>
    <si><t>Total</t></si>
</sst>"#;

    let sheet1 = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" t="s"><v>0</v></c>
            <c r="B1" t="s"><v>1</v></c>
        </row>
        <row r="2">
            <c r="A2" t="s"><v>2</v></c>
            <c r="B2"><v>150000</v></c>
        </row>
        <row r="3">
            <c r="A3" t="s"><v>3</v></c>
            <c r="B3"><v>75000</v></c>
        </row>
        <row r="4">
            <c r="A4" t="s"><v>4</v></c>
            <c r="B4"><v>90000</v></c>
        </row>
        <row r="5">
            <c r="A5" t="s"><v>5</v></c>
            <c r="B5"><v>315000</v></c>
        </row>
    </sheetData>
</worksheet>"#;

    // Content types must declare sharedStrings.
    let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
    <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
    <Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>
</Types>"#;

    let wb_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
        Target="worksheets/sheet1.xml"/>
    <Relationship Id="rId2"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings"
        Target="sharedStrings.xml"/>
</Relationships>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", XLSX_PACKAGE_RELS),
        ("xl/workbook.xml", XLSX_WORKBOOK_1SHEET),
        ("xl/_rels/workbook.xml.rels", wb_rels),
        ("xl/worksheets/sheet1.xml", sheet1),
        ("xl/sharedStrings.xml", shared_strings),
    ])
}

#[test]
fn golden_shared_strings_text() {
    let data = build_shared_strings_xlsx();
    let mut doc = udoc_xlsx::XlsxDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("shared_strings_text", &text, &golden_dir());
}

#[test]
fn shared_strings_resolves_indices() {
    let data = build_shared_strings_xlsx();
    let mut doc = udoc_xlsx::XlsxDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let tables = page.tables().expect("tables()");
    assert_eq!(tables.len(), 1, "expected 1 table");
    let rows = &tables[0].rows;
    assert_eq!(rows[0].cells[0].text, "Department");
    assert_eq!(rows[0].cells[1].text, "Budget");
    assert_eq!(rows[1].cells[0].text, "Engineering");
    assert_eq!(rows[3].cells[0].text, "Operations");
}

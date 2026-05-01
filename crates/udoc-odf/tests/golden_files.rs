//! Golden file tests for ODF text extraction (ODT, ODS, ODP).
//!
//! Run with `BLESS=1 cargo test -p udoc-odf --test golden_files` to update expected files.

use std::path::PathBuf;
use udoc_containers::test_util::build_stored_zip;
use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_core::test_harness::assert_golden;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

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

// ---------------------------------------------------------------------------
// ODT 1: odt_basic_text -- paragraphs with inline formatting (text:span)
// ---------------------------------------------------------------------------

fn build_odt_basic_text() -> Vec<u8> {
    let styles_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:styles>
    <style:style style:name="BoldStyle" style:family="text">
      <style:text-properties fo:font-weight="bold"/>
    </style:style>
    <style:style style:name="ItalicStyle" style:family="text">
      <style:text-properties fo:font-style="italic"/>
    </style:style>
  </office:styles>
</office:document-styles>"#;

    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:p>First paragraph of plain text.</text:p>
      <text:p>This has <text:span text:style-name="BoldStyle">bold</text:span> and <text:span text:style-name="ItalicStyle">italic</text:span> words.</text:p>
      <text:p>Third paragraph.</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

    build_stored_zip(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.text" as &[u8],
        ),
        ("styles.xml", styles_xml),
        ("content.xml", content_xml),
    ])
}

#[test]
fn golden_odt_basic_text() {
    let data = build_odt_basic_text();
    let mut doc = udoc_odf::OdfDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("odt_basic_text", &text, &golden_dir());
}

#[test]
fn odt_basic_text_spans() {
    let data = build_odt_basic_text();
    let mut doc = udoc_odf::OdfDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let spans = page.raw_spans().expect("raw_spans()");

    // Should have spans across all paragraphs.
    assert!(!spans.is_empty(), "expected non-empty spans");

    // Check that bold span is present.
    let bold_span = spans.iter().find(|s| s.text == "bold");
    assert!(bold_span.is_some(), "expected a span with text 'bold'");
    assert!(
        bold_span.unwrap().is_bold,
        "bold span should have is_bold=true"
    );

    // Check that italic span is present.
    let italic_span = spans.iter().find(|s| s.text == "italic");
    assert!(italic_span.is_some(), "expected a span with text 'italic'");
    assert!(
        italic_span.unwrap().is_italic,
        "italic span should have is_italic=true"
    );
}

// ---------------------------------------------------------------------------
// ODT 2: odt_headings_lists -- headings and list items
// ---------------------------------------------------------------------------

fn build_odt_headings_lists() -> Vec<u8> {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:h text:outline-level="1">Chapter One</text:h>
      <text:h text:outline-level="2">Section 1.1</text:h>
      <text:p>Body text under the heading.</text:p>
      <text:list>
        <text:list-item><text:p>First item</text:p></text:list-item>
        <text:list-item><text:p>Second item</text:p></text:list-item>
        <text:list-item><text:p>Third item</text:p></text:list-item>
      </text:list>
    </office:text>
  </office:body>
</office:document-content>"#;

    make_odt_bytes(content_xml)
}

#[test]
fn golden_odt_headings_lists() {
    let data = build_odt_headings_lists();
    let mut doc = udoc_odf::OdfDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("odt_headings_lists", &text, &golden_dir());
}

#[test]
fn odt_headings_lists_text_lines() {
    let data = build_odt_headings_lists();
    let mut doc = udoc_odf::OdfDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let lines = page.text_lines().expect("text_lines()");

    // 2 headings + 1 body + 3 list items = 6 lines.
    assert_eq!(lines.len(), 6, "expected 6 text lines, got {}", lines.len());
}

// ---------------------------------------------------------------------------
// ODT 3: odt_table -- table with merged cells
// ---------------------------------------------------------------------------

fn build_odt_table() -> Vec<u8> {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0">
  <office:body>
    <office:text>
      <text:p>Before the table.</text:p>
      <table:table>
        <table:table-row>
          <table:table-cell table:number-columns-spanned="2"><text:p>Merged Header</text:p></table:table-cell>
          <table:covered-table-cell/>
          <table:table-cell><text:p>C1</text:p></table:table-cell>
        </table:table-row>
        <table:table-row>
          <table:table-cell><text:p>A2</text:p></table:table-cell>
          <table:table-cell><text:p>B2</text:p></table:table-cell>
          <table:table-cell><text:p>C2</text:p></table:table-cell>
        </table:table-row>
      </table:table>
      <text:p>After the table.</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

    make_odt_bytes(content_xml)
}

#[test]
fn golden_odt_table() {
    let data = build_odt_table();
    let mut doc = udoc_odf::OdfDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("odt_table", &text, &golden_dir());
}

#[test]
fn odt_table_extraction() {
    let data = build_odt_table();
    let mut doc = udoc_odf::OdfDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let tables = page.tables().expect("tables()");

    assert_eq!(tables.len(), 1, "expected 1 table");
    assert_eq!(tables[0].rows.len(), 2, "expected 2 rows");

    // First row: merged cell spanning 2 columns + C1.
    assert_eq!(tables[0].rows[0].cells[0].text, "Merged Header");
    assert_eq!(tables[0].rows[0].cells[0].col_span, 2);
    assert_eq!(tables[0].rows[0].cells[1].text, "C1");

    // Second row: A2, B2, C2.
    assert_eq!(tables[0].rows[1].cells.len(), 3);
    assert_eq!(tables[0].rows[1].cells[0].text, "A2");
    assert_eq!(tables[0].rows[1].cells[2].text, "C2");
}

// ---------------------------------------------------------------------------
// ODS 1: ods_basic_cells -- cells with different value types
// ---------------------------------------------------------------------------

fn build_ods_basic_cells() -> Vec<u8> {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:spreadsheet>
      <table:table table:name="Sheet1">
        <table:table-row>
          <table:table-cell office:value-type="string"><text:p>Name</text:p></table:table-cell>
          <table:table-cell office:value-type="string"><text:p>Value</text:p></table:table-cell>
          <table:table-cell office:value-type="string"><text:p>Date</text:p></table:table-cell>
          <table:table-cell office:value-type="string"><text:p>Active</text:p></table:table-cell>
        </table:table-row>
        <table:table-row>
          <table:table-cell office:value-type="string"><text:p>Widget</text:p></table:table-cell>
          <table:table-cell office:value-type="float" office:value="3.14"><text:p>3.14</text:p></table:table-cell>
          <table:table-cell office:value-type="date" office:date-value="2025-06-15"><text:p>Jun 15</text:p></table:table-cell>
          <table:table-cell office:value-type="boolean" office:boolean-value="true"><text:p>Yes</text:p></table:table-cell>
        </table:table-row>
      </table:table>
    </office:spreadsheet>
  </office:body>
</office:document-content>"#;

    make_ods_bytes(content_xml)
}

#[test]
fn golden_ods_basic_cells() {
    let data = build_ods_basic_cells();
    let mut doc = udoc_odf::OdfDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("ods_basic_cells", &text, &golden_dir());
}

#[test]
fn ods_basic_cells_typed_values() {
    let data = build_ods_basic_cells();
    let mut doc = udoc_odf::OdfDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let tables = page.tables().expect("tables()");

    assert_eq!(tables.len(), 1);
    let row1 = &tables[0].rows[1];
    // Float: typed attribute value.
    assert_eq!(row1.cells[1].text, "3.14");
    // Date: ISO 8601 from office:date-value.
    assert_eq!(row1.cells[2].text, "2025-06-15");
    // Boolean.
    assert_eq!(row1.cells[3].text, "TRUE");
}

// ---------------------------------------------------------------------------
// ODS 2: ods_multi_sheet -- multiple sheets
// ---------------------------------------------------------------------------

fn build_ods_multi_sheet() -> Vec<u8> {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:spreadsheet>
      <table:table table:name="Revenue">
        <table:table-row>
          <table:table-cell office:value-type="string"><text:p>Q1</text:p></table:table-cell>
          <table:table-cell office:value-type="float" office:value="1000"><text:p>1000</text:p></table:table-cell>
        </table:table-row>
        <table:table-row>
          <table:table-cell office:value-type="string"><text:p>Q2</text:p></table:table-cell>
          <table:table-cell office:value-type="float" office:value="1500"><text:p>1500</text:p></table:table-cell>
        </table:table-row>
      </table:table>
      <table:table table:name="Expenses">
        <table:table-row>
          <table:table-cell office:value-type="string"><text:p>Rent</text:p></table:table-cell>
          <table:table-cell office:value-type="float" office:value="500"><text:p>500</text:p></table:table-cell>
        </table:table-row>
      </table:table>
    </office:spreadsheet>
  </office:body>
</office:document-content>"#;

    make_ods_bytes(content_xml)
}

#[test]
fn golden_ods_multi_sheet_0() {
    let data = build_ods_multi_sheet();
    let mut doc = udoc_odf::OdfDocument::from_bytes(&data).expect("from_bytes");
    assert_eq!(doc.page_count(), 2, "expected 2 sheets");

    let mut page0 = doc.page(0).expect("page 0");
    let text = page0.text().expect("text()");
    assert_golden("ods_multi_sheet_0", &text, &golden_dir());
}

#[test]
fn golden_ods_multi_sheet_1() {
    let data = build_ods_multi_sheet();
    let mut doc = udoc_odf::OdfDocument::from_bytes(&data).expect("from_bytes");

    let mut page1 = doc.page(1).expect("page 1");
    let text = page1.text().expect("text()");
    assert_golden("ods_multi_sheet_1", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// ODS 3: ods_merged_cells -- column/row spans with covered-table-cell
// ---------------------------------------------------------------------------

fn build_ods_merged_cells() -> Vec<u8> {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:spreadsheet>
      <table:table table:name="Sheet1">
        <table:table-row>
          <table:table-cell table:number-columns-spanned="3" table:number-rows-spanned="1"
                           office:value-type="string"><text:p>Wide Header</text:p></table:table-cell>
          <table:covered-table-cell/>
          <table:covered-table-cell/>
        </table:table-row>
        <table:table-row>
          <table:table-cell office:value-type="string"><text:p>A</text:p></table:table-cell>
          <table:table-cell office:value-type="string"><text:p>B</text:p></table:table-cell>
          <table:table-cell office:value-type="string"><text:p>C</text:p></table:table-cell>
        </table:table-row>
      </table:table>
    </office:spreadsheet>
  </office:body>
</office:document-content>"#;

    make_ods_bytes(content_xml)
}

#[test]
fn golden_ods_merged_cells() {
    let data = build_ods_merged_cells();
    let mut doc = udoc_odf::OdfDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("ods_merged_cells", &text, &golden_dir());
}

#[test]
fn ods_merged_cells_spans() {
    let data = build_ods_merged_cells();
    let mut doc = udoc_odf::OdfDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let tables = page.tables().expect("tables()");

    assert_eq!(tables.len(), 1);
    // First row: only the merged cell (covered cells are skipped).
    assert_eq!(tables[0].rows[0].cells.len(), 1);
    assert_eq!(tables[0].rows[0].cells[0].text, "Wide Header");
    assert_eq!(tables[0].rows[0].cells[0].col_span, 3);

    // Second row: 3 regular cells.
    assert_eq!(tables[0].rows[1].cells.len(), 3);
}

// ---------------------------------------------------------------------------
// ODP 1: odp_basic_slides -- two slides with title and body text
// ---------------------------------------------------------------------------

fn build_odp_basic_slides() -> Vec<u8> {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
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
            <text:p>Welcome to ODP</text:p>
          </draw:text-box>
        </draw:frame>
        <draw:frame>
          <draw:text-box>
            <text:p>Introduction body text</text:p>
          </draw:text-box>
        </draw:frame>
      </draw:page>
      <draw:page draw:name="Slide2">
        <draw:frame presentation:class="title">
          <draw:text-box>
            <text:p>Second Slide</text:p>
          </draw:text-box>
        </draw:frame>
        <draw:frame>
          <draw:text-box>
            <text:p>More content here</text:p>
          </draw:text-box>
        </draw:frame>
      </draw:page>
    </office:presentation>
  </office:body>
</office:document-content>"#;

    make_odp_bytes(content_xml)
}

#[test]
fn golden_odp_basic_slides_0() {
    let data = build_odp_basic_slides();
    let mut doc = udoc_odf::OdfDocument::from_bytes(&data).expect("from_bytes");
    assert_eq!(doc.page_count(), 2, "expected 2 slides");

    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("odp_basic_slides_0", &text, &golden_dir());
}

#[test]
fn golden_odp_basic_slides_1() {
    let data = build_odp_basic_slides();
    let mut doc = udoc_odf::OdfDocument::from_bytes(&data).expect("from_bytes");

    let mut page = doc.page(1).expect("page 1");
    let text = page.text().expect("text()");
    assert_golden("odp_basic_slides_1", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// ODP 2: odp_with_notes -- slide with speaker notes
// ---------------------------------------------------------------------------

fn build_odp_with_notes() -> Vec<u8> {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
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
            <text:p>Presentation Title</text:p>
          </draw:text-box>
        </draw:frame>
        <draw:frame>
          <draw:text-box>
            <text:p>Slide body content</text:p>
          </draw:text-box>
        </draw:frame>
        <presentation:notes>
          <draw:frame>
            <draw:text-box>
              <text:p>Remember to mention the quarterly results.</text:p>
              <text:p>Also discuss the roadmap.</text:p>
            </draw:text-box>
          </draw:frame>
        </presentation:notes>
      </draw:page>
    </office:presentation>
  </office:body>
</office:document-content>"#;

    make_odp_bytes(content_xml)
}

#[test]
fn golden_odp_with_notes() {
    let data = build_odp_with_notes();
    let mut doc = udoc_odf::OdfDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("odp_with_notes", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// ODP 3: odp_formatted -- slide with bold/italic text in shapes
// (ODP text:span handling is limited since the text-box collector
//  concatenates text, but we verify the text output is correct.)
// ---------------------------------------------------------------------------

fn build_odp_formatted() -> Vec<u8> {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
    xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:body>
    <office:presentation>
      <draw:page draw:name="Slide1">
        <draw:frame presentation:class="title">
          <draw:text-box>
            <text:p>Formatted Slide Title</text:p>
          </draw:text-box>
        </draw:frame>
        <draw:frame presentation:class="subtitle">
          <draw:text-box>
            <text:p>Subtitle with emphasis</text:p>
          </draw:text-box>
        </draw:frame>
        <draw:frame>
          <draw:text-box>
            <text:p>Regular body paragraph one.</text:p>
            <text:p>Regular body paragraph two.</text:p>
          </draw:text-box>
        </draw:frame>
      </draw:page>
    </office:presentation>
  </office:body>
</office:document-content>"#;

    make_odp_bytes(content_xml)
}

#[test]
fn golden_odp_formatted() {
    let data = build_odp_formatted();
    let mut doc = udoc_odf::OdfDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("odp_formatted", &text, &golden_dir());
}

#[test]
fn odp_formatted_text_lines() {
    let data = build_odp_formatted();
    let mut doc = udoc_odf::OdfDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let lines = page.text_lines().expect("text_lines()");

    // Title + subtitle + 2 body paragraphs = 4 lines.
    assert_eq!(lines.len(), 4, "expected 4 text lines, got {}", lines.len());
}

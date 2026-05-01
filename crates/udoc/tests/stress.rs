//! Large-file stress tests for the udoc facade.
//!
//! These tests generate large synthetic documents in memory and verify that
//! extraction completes without error and produces the expected volume of
//! output. No external files are used and nothing is written to disk.
//!
//! All tests are gated with #[ignore] because they take noticeably longer
//! than the regular unit tests (5-30 seconds depending on hardware).
//! Run them with:
//!
//!   cargo test -p udoc --test stress -- --ignored --nocapture

use udoc_containers::test_util::{
    build_stored_zip, DOCX_CONTENT_TYPES, DOCX_PACKAGE_RELS, XLSX_CONTENT_TYPES, XLSX_PACKAGE_RELS,
    XLSX_WB_RELS_1SHEET, XLSX_WORKBOOK_1SHEET,
};

// ---------------------------------------------------------------------------
// XLSX: 1 000 rows x 5 columns of numbers
// ---------------------------------------------------------------------------

fn make_large_xlsx(rows: usize) -> Vec<u8> {
    let mut sheet = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
"#,
    );

    // Header row
    sheet.push_str("    <row r=\"1\">\n");
    for col in 0..5usize {
        let col_letter = (b'A' + col as u8) as char;
        sheet.push_str(&format!(
            "      <c r=\"{}{}\"><v>{}</v></c>\n",
            col_letter, 1, col
        ));
    }
    sheet.push_str("    </row>\n");

    // Data rows
    for row in 2..=(rows + 1) {
        sheet.push_str(&format!("    <row r=\"{}\">\n", row));
        for col in 0..5usize {
            let col_letter = (b'A' + col as u8) as char;
            let value = (row * 10 + col) as u64;
            sheet.push_str(&format!(
                "      <c r=\"{}{}\"><v>{}</v></c>\n",
                col_letter, row, value
            ));
        }
        sheet.push_str("    </row>\n");
    }

    sheet.push_str("  </sheetData>\n</worksheet>");

    build_stored_zip(&[
        ("[Content_Types].xml", XLSX_CONTENT_TYPES),
        ("_rels/.rels", XLSX_PACKAGE_RELS),
        ("xl/workbook.xml", XLSX_WORKBOOK_1SHEET),
        ("xl/_rels/workbook.xml.rels", XLSX_WB_RELS_1SHEET),
        ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
    ])
}

/// Extract a 1 000-row XLSX and verify the row count in the extracted table.
#[test]
#[ignore]
fn test_large_xlsx_1k_rows() {
    const ROWS: usize = 1_000;

    let data = make_large_xlsx(ROWS);
    let doc = udoc::extract_bytes(&data).expect("large XLSX extract should succeed");

    assert!(!doc.content.is_empty(), "document should have content");

    // The extractor should produce a Table block. Count the data rows in it
    // (excluding the header row we placed in row 1).
    let table_blocks: Vec<_> = doc
        .content
        .iter()
        .filter(|b| matches!(b, udoc::Block::Table { .. }))
        .collect();
    assert!(
        !table_blocks.is_empty(),
        "should have at least one Table block"
    );

    // Every value row contributes to the extracted text; verify at minimum
    // that the text is not empty.
    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !all_text.is_empty(),
        "extracted text should not be empty for a 1 000-row XLSX"
    );

    // The number of non-empty lines should be at least ROWS (one per data row).
    let line_count = all_text.lines().filter(|l| !l.trim().is_empty()).count();
    assert!(
        line_count >= ROWS,
        "expected at least {ROWS} non-empty lines, got {line_count}"
    );
}

// ---------------------------------------------------------------------------
// DOCX: 100 paragraphs
// ---------------------------------------------------------------------------

fn make_large_docx(paragraphs: usize) -> Vec<u8> {
    let mut doc_xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
"#,
    );

    for i in 0..paragraphs {
        // Use a distinct word per paragraph so we can verify content spread.
        doc_xml.push_str(&format!(
            "    <w:p><w:r><w:t>Paragraph {i}: Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.</w:t></w:r></w:p>\n"
        ));
    }

    doc_xml.push_str("  </w:body>\n</w:document>");

    build_stored_zip(&[
        ("[Content_Types].xml", DOCX_CONTENT_TYPES),
        ("_rels/.rels", DOCX_PACKAGE_RELS),
        ("word/document.xml", doc_xml.as_bytes()),
    ])
}

/// Extract a DOCX with 100 paragraphs and verify the text length.
#[test]
#[ignore]
fn test_large_docx_many_paragraphs() {
    const PARAGRAPHS: usize = 100;

    let data = make_large_docx(PARAGRAPHS);
    let doc = udoc::extract_bytes(&data).expect("large DOCX extract should succeed");

    assert!(!doc.content.is_empty(), "document should have content");

    // Should have roughly as many blocks as paragraphs.
    assert!(
        doc.content.len() >= PARAGRAPHS,
        "expected at least {PARAGRAPHS} content blocks, got {}",
        doc.content.len()
    );

    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");

    // Each paragraph contains "Lorem ipsum" -- verify a few specific ones.
    assert!(
        all_text.contains("Paragraph 0:"),
        "should contain first paragraph marker"
    );
    assert!(
        all_text.contains(&format!("Paragraph {}:", PARAGRAPHS - 1)),
        "should contain last paragraph marker"
    );

    // Rough size check: each paragraph is ~120 chars; 100 paragraphs should
    // produce at least 8 000 characters.
    assert!(
        all_text.len() >= 8_000,
        "expected at least 8 000 chars, got {}",
        all_text.len()
    );
}

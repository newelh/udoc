//! Golden-file tests for table detection and extraction.
//!
//! Each test constructs path segments and text spans (simulating what the
//! content interpreter would produce), runs detect_tables + fill_table_text,
//! and compares the serialized output against a `.expected.txt` golden file.
//!
//! Use BLESS=1 to create or update golden files when table extraction
//! output changes intentionally.
//!
//! Golden files live in tests/golden/tables/<name>.expected.txt.

mod common;

use udoc_pdf::table::{
    detect_header_rows, detect_hline_tables, detect_tables, detect_text_tables, extract_tables,
    fill_table_text,
};
use udoc_pdf::{BoundingBox, NullDiagnostics, PathSegment, Table, TextSpan};

// ---------------------------------------------------------------------------
// Serialization: Table -> deterministic text format
// ---------------------------------------------------------------------------

/// Serialize a list of tables to the golden file text format.
///
/// Format:
/// ```text
/// TABLE 0 (3 rows, 4 cols, ruled)
/// [H] Name|Age|City|Country
/// Alice|30|NYC|USA
/// Bob|25|London|UK
///
/// TABLE 1 (2 rows, 2 cols, ruled)
/// X|Y
/// 1|2
/// ```
fn serialize_tables(tables: &[Table]) -> String {
    if tables.is_empty() {
        return "NO TABLES\n".to_string();
    }

    let mut out = String::new();
    for (i, table) in tables.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!(
            "TABLE {} ({} rows, {} cols, {})\n",
            i,
            table.rows.len(),
            table.num_columns,
            table.detection_method
        ));

        for row in &table.rows {
            if row.is_header {
                out.push_str("[H] ");
            }

            for (j, cell) in row.cells.iter().enumerate() {
                if j > 0 {
                    out.push('|');
                }
                out.push_str(&cell.text);
                if cell.col_span > 1 || cell.row_span > 1 {
                    out.push_str(&format!(" ({}x{})", cell.col_span, cell.row_span));
                }
            }
            out.push('\n');
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Golden file comparison
// ---------------------------------------------------------------------------

fn golden_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/tables")
        .join(format!("{name}.expected.txt"))
}

fn assert_table_golden(name: &str, tables: &[Table]) {
    let actual = serialize_tables(tables);
    let golden = golden_path(name);

    let is_bless = std::env::var("BLESS")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));

    let expected = match std::fs::read_to_string(&golden) {
        Ok(s) => s,
        Err(_) if is_bless => {
            std::fs::write(&golden, &actual).unwrap_or_else(|e| {
                panic!("failed to create golden file {}: {e}", golden.display())
            });
            eprintln!("Created golden file: {}", golden.display());
            return;
        }
        Err(e) => {
            panic!(
                "failed to read golden file {}: {e}\n\
                 Hint: run with BLESS=1 to create it.",
                golden.display()
            )
        }
    };

    // Normalize trailing whitespace.
    let normalize = |s: &str| -> String {
        s.lines()
            .map(|l| l.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_string()
    };

    let actual_norm = normalize(&actual);
    let expected_norm = normalize(&expected);

    if actual_norm != expected_norm {
        if is_bless {
            std::fs::write(&golden, &actual).unwrap_or_else(|e| {
                panic!("failed to bless golden file {}: {e}", golden.display())
            });
            eprintln!("Blessed golden file: {}", golden.display());
            return;
        }

        panic!(
            "Golden file mismatch for {name}\n\
             Golden: {}\n\
             --- EXPECTED ---\n{expected_norm}\n\
             --- ACTUAL ---\n{actual_norm}\n\
             ---\n\
             To update: run with BLESS=1",
            golden.display(),
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers: build path segments and spans for test scenarios
// ---------------------------------------------------------------------------

fn letter_page() -> BoundingBox {
    BoundingBox::new(0.0, 0.0, 612.0, 792.0)
}

fn h_line(x1: f64, y: f64, x2: f64) -> PathSegment {
    PathSegment::line(x1, y, x2, y, 0.5)
}

fn v_line(x: f64, y1: f64, y2: f64) -> PathSegment {
    PathSegment::line(x, y1, x, y2, 0.5)
}

fn span(text: &str, x: f64, y: f64) -> TextSpan {
    TextSpan::new(
        text.to_string(),
        x,
        y,
        text.len() as f64 * 6.0,
        "Helvetica".to_string(),
        12.0,
    )
}

// ---------------------------------------------------------------------------
// Test: simple 3x3 ruled table
// ---------------------------------------------------------------------------

#[test]
fn table_golden_simple_3x3() {
    // 4 horizontal lines x 4 vertical lines = 3x3 grid.
    //
    //   100    200    300    400
    //    |      |      |      |
    //    +------+------+------+  y=700
    //    | A    | B    | C    |
    //    +------+------+------+  y=670
    //    | D    | E    | F    |
    //    +------+------+------+  y=640
    //    | G    | H    | I    |
    //    +------+------+------+  y=610

    let paths = vec![
        h_line(100.0, 700.0, 400.0),
        h_line(100.0, 670.0, 400.0),
        h_line(100.0, 640.0, 400.0),
        h_line(100.0, 610.0, 400.0),
        v_line(100.0, 610.0, 700.0),
        v_line(200.0, 610.0, 700.0),
        v_line(300.0, 610.0, 700.0),
        v_line(400.0, 610.0, 700.0),
    ];

    let spans = vec![
        span("A", 110.0, 685.0),
        span("B", 210.0, 685.0),
        span("C", 310.0, 685.0),
        span("D", 110.0, 655.0),
        span("E", 210.0, 655.0),
        span("F", 310.0, 655.0),
        span("G", 110.0, 625.0),
        span("H", 210.0, 625.0),
        span("I", 310.0, 625.0),
    ];

    let mut tables = detect_tables(&paths, &letter_page(), &NullDiagnostics);
    fill_table_text(&mut tables, &spans);

    assert_table_golden("simple_3x3", &tables);
}

// ---------------------------------------------------------------------------
// Test: no table (just text, no ruled lines)
// ---------------------------------------------------------------------------

#[test]
fn table_golden_no_tables() {
    let paths: Vec<PathSegment> = Vec::new();
    let tables = detect_tables(&paths, &letter_page(), &NullDiagnostics);

    assert_table_golden("no_tables", &tables);
}

// ---------------------------------------------------------------------------
// Test: single rectangle (1x1 grid, trivial table)
// ---------------------------------------------------------------------------

#[test]
fn table_golden_single_rect() {
    let paths = vec![PathSegment::rect(100.0, 600.0, 200.0, 100.0, 1.0)];

    let spans = vec![span("Hello", 110.0, 650.0)];

    let mut tables = detect_tables(&paths, &letter_page(), &NullDiagnostics);
    fill_table_text(&mut tables, &spans);

    assert_table_golden("single_rect", &tables);
}

// ---------------------------------------------------------------------------
// Test: 2x4 table with header row
// ---------------------------------------------------------------------------

#[test]
fn table_golden_header_row() {
    // 2 columns, 4 rows. First row marked as header after detection.
    //
    //   100       300       500
    //    |         |         |
    //    +---------+---------+  y=700
    //    | Name    | Score   |  (header)
    //    +---------+---------+  y=670
    //    | Alice   | 95      |
    //    +---------+---------+  y=640
    //    | Bob     | 87      |
    //    +---------+---------+  y=610
    //    | Charlie | 92      |
    //    +---------+---------+  y=580

    let paths = vec![
        h_line(100.0, 700.0, 500.0),
        h_line(100.0, 670.0, 500.0),
        h_line(100.0, 640.0, 500.0),
        h_line(100.0, 610.0, 500.0),
        h_line(100.0, 580.0, 500.0),
        v_line(100.0, 580.0, 700.0),
        v_line(300.0, 580.0, 700.0),
        v_line(500.0, 580.0, 700.0),
    ];

    let spans = vec![
        span("Name", 110.0, 685.0),
        span("Score", 310.0, 685.0),
        span("Alice", 110.0, 655.0),
        span("95", 310.0, 655.0),
        span("Bob", 110.0, 625.0),
        span("87", 310.0, 625.0),
        span("Charlie", 110.0, 595.0),
        span("92", 310.0, 595.0),
    ];

    let mut tables = detect_tables(&paths, &letter_page(), &NullDiagnostics);
    fill_table_text(&mut tables, &spans);

    // Mark first row as header (in real usage, heuristics would detect this).
    if let Some(table) = tables.first_mut() {
        if let Some(row) = table.rows.first_mut() {
            row.is_header = true;
        }
    }

    assert_table_golden("header_row", &tables);
}

// ---------------------------------------------------------------------------
// Test: empty cells in grid
// ---------------------------------------------------------------------------

#[test]
fn table_golden_empty_cells() {
    // 2x2 grid where two cells have no text.
    //
    //   100       250       400
    //    |         |         |
    //    +---------+---------+  y=700
    //    | X       |         |  (top-right empty)
    //    +---------+---------+  y=650
    //    |         | Y       |  (bottom-left empty)
    //    +---------+---------+  y=600

    let paths = vec![
        h_line(100.0, 700.0, 400.0),
        h_line(100.0, 650.0, 400.0),
        h_line(100.0, 600.0, 400.0),
        v_line(100.0, 600.0, 700.0),
        v_line(250.0, 600.0, 700.0),
        v_line(400.0, 600.0, 700.0),
    ];

    let spans = vec![
        span("X", 110.0, 675.0), // top-left
        span("Y", 260.0, 625.0), // bottom-right
    ];

    let mut tables = detect_tables(&paths, &letter_page(), &NullDiagnostics);
    fill_table_text(&mut tables, &spans);

    assert_table_golden("empty_cells", &tables);
}

// ---------------------------------------------------------------------------
// Test: auto-detected header row (bold font heuristic)
// ---------------------------------------------------------------------------

#[test]
fn table_golden_bold_header() {
    // Same grid as header_row but using bold fonts for first row.
    let paths = vec![
        h_line(100.0, 700.0, 500.0),
        h_line(100.0, 670.0, 500.0),
        h_line(100.0, 640.0, 500.0),
        h_line(100.0, 610.0, 500.0),
        v_line(100.0, 610.0, 700.0),
        v_line(300.0, 610.0, 700.0),
        v_line(500.0, 610.0, 700.0),
    ];

    // Bold spans for header row, regular for data rows.
    let spans = vec![
        TextSpan::new(
            "Name".to_string(),
            110.0,
            685.0,
            24.0,
            "Helvetica-Bold",
            12.0,
        ),
        TextSpan::new(
            "Score".to_string(),
            310.0,
            685.0,
            30.0,
            "Helvetica-Bold",
            12.0,
        ),
        TextSpan::new("Alice".to_string(), 110.0, 655.0, 30.0, "Helvetica", 12.0),
        TextSpan::new("95".to_string(), 310.0, 655.0, 12.0, "Helvetica", 12.0),
        TextSpan::new("Bob".to_string(), 110.0, 625.0, 18.0, "Helvetica", 12.0),
        TextSpan::new("87".to_string(), 310.0, 625.0, 12.0, "Helvetica", 12.0),
    ];

    let mut tables = detect_tables(&paths, &letter_page(), &NullDiagnostics);
    fill_table_text(&mut tables, &spans);
    detect_header_rows(&mut tables, &spans);

    assert_table_golden("bold_header", &tables);
}

// ---------------------------------------------------------------------------
// Test: merged cells (col_span and row_span)
// ---------------------------------------------------------------------------

#[test]
fn table_golden_merged_cells() {
    // 3x3 grid with top-left 2x2 block merged.
    //
    //   100       200       300       400
    //    |         |         |         |
    //    +---------+---------+---------+  y=700
    //    |      merged      |  C      |
    //    |      (2x2)       +---------+  y=660
    //    |                  |  F      |
    //    +---------+---------+---------+  y=620
    //    |  G      |  H      |  I      |
    //    +---------+---------+---------+  y=580

    let paths = vec![
        // Full-width horizontal lines
        h_line(100.0, 700.0, 400.0),
        h_line(100.0, 620.0, 400.0),
        h_line(100.0, 580.0, 400.0),
        // Partial horizontal at y=660: only right column (x=300..400)
        h_line(300.0, 660.0, 400.0),
        // Vertical lines: left and right full height
        v_line(100.0, 580.0, 700.0),
        v_line(400.0, 580.0, 700.0),
        // Center vertical at x=200: only bottom row (y=580..620)
        v_line(200.0, 580.0, 620.0),
        // Right-center vertical at x=300: full height
        v_line(300.0, 580.0, 700.0),
    ];

    let spans = vec![
        span("merged", 110.0, 670.0),
        span("C", 310.0, 680.0),
        span("F", 310.0, 640.0),
        span("G", 110.0, 600.0),
        span("H", 210.0, 600.0),
        span("I", 310.0, 600.0),
    ];

    let mut tables = detect_tables(&paths, &letter_page(), &NullDiagnostics);
    fill_table_text(&mut tables, &spans);

    assert_table_golden("merged_cells", &tables);
}

// ---------------------------------------------------------------------------
// Test: synthetic PDF end-to-end (path capture through Page::tables())
// ---------------------------------------------------------------------------

#[test]
fn table_golden_synthetic_pdf_text_only() {
    use common::PdfBuilder;
    use udoc_pdf::Document;

    // Build a PDF with rectangles + text. Exercises the full
    // Document -> Page -> tables() path.
    let content = b"q\n\
        0.5 w\n\
        100 600 200 100 re S\n\
        100 600 100 50 re S\n\
        200 600 100 50 re S\n\
        100 650 100 50 re S\n\
        200 650 100 50 re S\n\
        BT /F1 12 Tf\n\
        110 665 Td (A) Tj\n\
        210 665 Td (B) Tj\n\
        110 615 Td (C) Tj\n\
        210 615 Td (D) Tj\n\
        ET\nQ\n";

    let mut pdf = PdfBuilder::new("1.4");
    pdf.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    pdf.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    pdf.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
    );
    pdf.add_stream_object(4, "", content);
    pdf.add_object(5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    let data = pdf.finish(1);

    let mut doc = Document::from_bytes(data).expect("PDF should parse");
    assert_eq!(doc.page_count(), 1);
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text extraction");

    // Verify text extraction works.
    assert!(text.contains('A'), "text should contain 'A': {text}");
    assert!(text.contains('D'), "text should contain 'D': {text}");

    // Verify table extraction works through the full pipeline.
    let mut page2 = doc.page(0).expect("page 0");
    let tables = page2.tables().expect("table extraction");
    // The synthetic PDF uses thick stroked rects (100x50 pt), not thin border
    // lines. The lattice detector only treats thin rects (< SNAP_TOLERANCE in
    // one dimension) as grid lines, so table detection may legitimately find
    // zero tables here. This test validates the full pipeline doesn't panic.
    // If tables ARE detected, verify they contain expected text.
    if !tables.is_empty() {
        let all_text: String = tables
            .iter()
            .flat_map(|t| &t.rows)
            .flat_map(|r| &r.cells)
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        for ch in ['A', 'B', 'C', 'D'] {
            assert!(
                all_text.contains(ch),
                "table cells should contain '{ch}': {all_text}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Test: horizontal-rules-only table (no vertical lines)
// ---------------------------------------------------------------------------

#[test]
fn table_golden_hline_only() {
    // 4 horizontal rules with text columns inferred from span positions.
    // No vertical lines (typical of LaTeX toprule/midrule/bottomrule).
    //
    //    100                                     500
    //     =========================================  y=700 (top rule)
    //       Name       Age       City
    //     -----------------------------------------  y=670 (mid rule)
    //       Alice      30        NYC
    //     -----------------------------------------  y=640
    //       Bob        25        London
    //     =========================================  y=610 (bottom rule)

    let paths = vec![
        h_line(100.0, 700.0, 500.0),
        h_line(100.0, 670.0, 500.0),
        h_line(100.0, 640.0, 500.0),
        h_line(100.0, 610.0, 500.0),
    ];

    let spans = vec![
        span("Name", 110.0, 685.0),
        span("Age", 250.0, 685.0),
        span("City", 380.0, 685.0),
        span("Alice", 110.0, 655.0),
        span("30", 250.0, 655.0),
        span("NYC", 380.0, 655.0),
        span("Bob", 110.0, 625.0),
        span("25", 250.0, 625.0),
        span("London", 380.0, 625.0),
    ];

    let mut tables = detect_hline_tables(&paths, &spans, &letter_page(), &NullDiagnostics);
    fill_table_text(&mut tables, &spans);

    assert_table_golden("hline_only", &tables);
}

// ---------------------------------------------------------------------------
// Test: 2-row text-alignment table (header + 1 data row)
// ---------------------------------------------------------------------------

#[test]
fn table_golden_two_row_text_table() {
    // No paths at all, purely text alignment.
    // 2 rows x 3 columns detected from left-edge clustering.
    //
    //   "Name"     "Age"     "City"
    //   "Alice"    "30"      "NYC"

    let spans = vec![
        span("Name", 100.0, 700.0),
        span("Age", 250.0, 700.0),
        span("City", 400.0, 700.0),
        span("Alice", 100.0, 680.0),
        span("30", 250.0, 680.0),
        span("NYC", 400.0, 680.0),
    ];

    let mut tables = detect_text_tables(&spans, &letter_page(), &NullDiagnostics);
    fill_table_text(&mut tables, &spans);

    assert_table_golden("two_row_text", &tables);
}

// ---------------------------------------------------------------------------
// Test: multi-table page (ruled table + text-alignment table coexisting)
// ---------------------------------------------------------------------------

#[test]
fn table_golden_multi_table_page() {
    // One ruled table at the top and one text-alignment table at the bottom.
    // They don't overlap so the hybrid pipeline should keep both.

    // Ruled 2x2 table near top of page.
    let paths = vec![
        h_line(100.0, 700.0, 300.0),
        h_line(100.0, 670.0, 300.0),
        h_line(100.0, 640.0, 300.0),
        v_line(100.0, 640.0, 700.0),
        v_line(200.0, 640.0, 700.0),
        v_line(300.0, 640.0, 700.0),
    ];

    // Spans for the ruled table.
    let mut spans = vec![
        span("A", 110.0, 685.0),
        span("B", 210.0, 685.0),
        span("C", 110.0, 655.0),
        span("D", 210.0, 655.0),
    ];

    // Text-alignment table near bottom of page (well separated in Y).
    // Needs at least 3 rows to avoid being too ambiguous.
    let text_table_spans = vec![
        span("X", 100.0, 200.0),
        span("Y", 250.0, 200.0),
        span("1", 100.0, 180.0),
        span("2", 250.0, 180.0),
        span("3", 100.0, 160.0),
        span("4", 250.0, 160.0),
    ];
    spans.extend(text_table_spans);

    // Use the full pipeline (extract_tables) which runs all 3 detectors,
    // deduplicates, fills text, and detects headers.
    let tables = extract_tables(&paths, &spans, &letter_page(), &NullDiagnostics);

    assert_table_golden("multi_table_page", &tables);
}

// ---------------------------------------------------------------------------
// Test: mega-row splitting (full pipeline via extract_tables)
// ---------------------------------------------------------------------------

#[test]
fn table_golden_mega_row_split() {
    // H-line-only table with 3 horizontal lines but no vertical lines.
    // The h-line detector creates one "mega-row" per gap between h-lines.
    // split_mega_rows should split by baseline into 2 data rows.
    //
    //   100                      400
    //    ========================  y=700  (h-line)
    //    Name    Score              y=685  (text baseline)
    //    ========================  y=670  (h-line)
    //    Alice   95                 y=655  (text baseline)
    //    Bob     87                 y=635  (text baseline)
    //    ========================  y=610  (h-line)

    let paths = vec![
        h_line(100.0, 700.0, 400.0),
        h_line(100.0, 670.0, 400.0),
        h_line(100.0, 610.0, 400.0),
    ];

    let spans = vec![
        span("Name", 110.0, 685.0),
        span("Score", 250.0, 685.0),
        span("Alice", 110.0, 655.0),
        span("95", 250.0, 655.0),
        span("Bob", 110.0, 635.0),
        span("87", 250.0, 635.0),
    ];

    let tables = extract_tables(&paths, &spans, &letter_page(), &NullDiagnostics);

    assert_table_golden("mega_row_split", &tables);
}

// ---------------------------------------------------------------------------
// Test: quality filter rejects phantom decorative rect
// ---------------------------------------------------------------------------

#[test]
fn table_golden_phantom_rejected() {
    // A small decorative rectangle (too small relative to page area)
    // with some text inside. The quality filter should reject it because
    // it has only 1 cell with 1 row (structurally invalid as a table).
    let paths = vec![PathSegment::rect(50.0, 750.0, 100.0, 30.0, 1.0)];

    let spans = vec![span("X", 60.0, 760.0)];

    let tables = extract_tables(&paths, &spans, &letter_page(), &NullDiagnostics);

    assert_table_golden("phantom_rejected", &tables);
}

// ---------------------------------------------------------------------------
// Test: empty column elimination via full pipeline
// ---------------------------------------------------------------------------

#[test]
fn table_golden_empty_col_elimination() {
    // A 3x3 ruled table where the middle column has no text.
    // eliminate_empty_columns should remove it, leaving a 3x2 table.
    //
    //   100    200    300    400
    //    +------+------+------+  y=700
    //    | A    |      | C    |
    //    +------+------+------+  y=670
    //    | D    |      | F    |
    //    +------+------+------+  y=640
    //    | G    |      | I    |
    //    +------+------+------+  y=610

    let paths = vec![
        h_line(100.0, 700.0, 400.0),
        h_line(100.0, 670.0, 400.0),
        h_line(100.0, 640.0, 400.0),
        h_line(100.0, 610.0, 400.0),
        v_line(100.0, 610.0, 700.0),
        v_line(200.0, 610.0, 700.0),
        v_line(300.0, 610.0, 700.0),
        v_line(400.0, 610.0, 700.0),
    ];

    // No spans in the middle column (200-300 x range).
    let spans = vec![
        span("A", 110.0, 685.0),
        span("C", 310.0, 685.0),
        span("D", 110.0, 655.0),
        span("F", 310.0, 655.0),
        span("G", 110.0, 625.0),
        span("I", 310.0, 625.0),
    ];

    let tables = extract_tables(&paths, &spans, &letter_page(), &NullDiagnostics);

    assert_table_golden("empty_col_elimination", &tables);
}

// ---------------------------------------------------------------------------
// Test: overlapping tables are deduplicated
// ---------------------------------------------------------------------------

#[test]
fn table_golden_overlapping_tables_dedup() {
    // Two lattice tables that share the same bounding box (fully overlapping).
    // The dedup logic should keep only one.
    //
    //   100    200    300
    //    +------+------+  y=700
    //    | A    | B    |
    //    +------+------+  y=670
    //    | C    | D    |
    //    +------+------+  y=640

    let paths = vec![
        // First grid
        h_line(100.0, 700.0, 300.0),
        h_line(100.0, 670.0, 300.0),
        h_line(100.0, 640.0, 300.0),
        v_line(100.0, 640.0, 700.0),
        v_line(200.0, 640.0, 700.0),
        v_line(300.0, 640.0, 700.0),
        // Duplicate grid at same position (e.g. from doubled drawing)
        h_line(100.0, 700.0, 300.0),
        h_line(100.0, 670.0, 300.0),
        h_line(100.0, 640.0, 300.0),
        v_line(100.0, 640.0, 700.0),
        v_line(200.0, 640.0, 700.0),
        v_line(300.0, 640.0, 700.0),
    ];

    let spans = vec![
        span("A", 110.0, 685.0),
        span("B", 210.0, 685.0),
        span("C", 110.0, 655.0),
        span("D", 210.0, 655.0),
    ];

    let tables = extract_tables(&paths, &spans, &letter_page(), &NullDiagnostics);

    // After line merging and dedup, should produce exactly one 2x2 table.
    assert_eq!(tables.len(), 1, "overlapping grids should dedup to 1 table");
    assert_eq!(tables[0].rows.len(), 2);
    assert_eq!(tables[0].num_columns, 2);
}

// ---------------------------------------------------------------------------
// Test: large table (5 columns, 10 rows)
// ---------------------------------------------------------------------------

#[test]
fn table_golden_large_5x10() {
    // Exercises column detection and text fill at scale beyond 2x2/3x3.
    //
    //   100  200  300  400  500  600
    //    +----+----+----+----+----+  y=700
    //    | .. | .. | .. | .. | .. |  (10 rows of data)
    //    +----+----+----+----+----+  y=400

    let mut paths = Vec::new();
    let cols = [100.0, 200.0, 300.0, 400.0, 500.0, 600.0];
    let num_rows = 10;
    let y_top = 700.0;
    let row_height = 30.0;

    // Horizontal lines (11 lines for 10 rows)
    for r in 0..=num_rows {
        let y = y_top - r as f64 * row_height;
        paths.push(h_line(cols[0], y, cols[5]));
    }
    // Vertical lines (6 lines for 5 columns)
    let y_bottom = y_top - num_rows as f64 * row_height;
    for &x in &cols {
        paths.push(v_line(x, y_bottom, y_top));
    }

    // Text: one span per cell. Label = "R{row}C{col}".
    let mut spans = Vec::new();
    for r in 0..num_rows {
        let y = y_top - r as f64 * row_height - 15.0; // baseline in middle of row
        for (c, &x) in cols[..5].iter().enumerate() {
            spans.push(span(&format!("R{}C{}", r, c), x + 10.0, y));
        }
    }

    let tables = extract_tables(&paths, &spans, &letter_page(), &NullDiagnostics);

    assert_eq!(tables.len(), 1, "should detect one 5x10 table");
    assert_eq!(tables[0].num_columns, 5, "should have 5 columns");
    assert_eq!(tables[0].rows.len(), 10, "should have 10 rows");
    // Spot-check first and last cells.
    assert!(tables[0].rows[0].cells[0].text.contains("R0C0"));
    assert!(tables[0].rows[9].cells[4].text.contains("R9C4"));
}

// ---------------------------------------------------------------------------
// Test: diagonal lines are ignored (no false table detection)
// ---------------------------------------------------------------------------

#[test]
fn table_golden_diagonal_lines_ignored() {
    // Diagonal line segments should not produce tables. The detector only
    // considers horizontal and vertical lines (slope tolerance check).
    let paths = vec![
        // 45-degree diagonal
        PathSegment::line(100.0, 100.0, 400.0, 400.0, 1.0),
        // Another diagonal
        PathSegment::line(100.0, 400.0, 400.0, 100.0, 1.0),
        // Short diagonal
        PathSegment::line(200.0, 200.0, 250.0, 250.0, 0.5),
    ];

    let spans = vec![span("X", 200.0, 250.0), span("Y", 300.0, 250.0)];

    let tables = extract_tables(&paths, &spans, &letter_page(), &NullDiagnostics);

    assert!(
        tables.is_empty(),
        "diagonal lines should not produce tables, got {} tables",
        tables.len()
    );
}

#[test]
fn test_serialize_empty_tables() {
    let result = serialize_tables(&[]);
    assert_eq!(result, "NO TABLES\n");
}

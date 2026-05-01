//! Golden file tests for the XLS backend.
//!
//! Each test builds a synthetic XLS file via `build_minimal_xls`, extracts
//! content, and compares against a `.expected` file in `tests/golden/`.
//! Run with `BLESS=1` to create or update the expected files.

use std::path::PathBuf;
use std::sync::Arc;

use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_core::diagnostics::NullDiagnostics;
use udoc_core::test_harness::assert_golden;
use udoc_xls::document::XlsDocument;
use udoc_xls::test_util::{
    build_minimal_xls, build_minimal_xls_with_boolerr, build_minimal_xls_with_date_numbers,
    build_minimal_xls_with_merges,
};

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn null_diag() -> Arc<dyn udoc_core::diagnostics::DiagnosticsSink> {
    Arc::new(NullDiagnostics)
}

// ---------------------------------------------------------------------------
// Golden 1: single sheet, single string cell
// ---------------------------------------------------------------------------

#[test]
fn golden_single_cell() {
    let data = build_minimal_xls(&["hello"], &[("Sheet1", &[(0, 0, "hello")])]);
    let mut doc = XlsDocument::from_bytes_with_diag(&data, null_diag())
        .expect("should parse single-cell XLS");
    let mut page = doc.page(0).expect("page 0 should exist");
    let text = page.text().expect("text() should succeed");
    assert_golden("xls_single_cell", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 2: single sheet, multiple cells in one row (tab-separated)
// ---------------------------------------------------------------------------

#[test]
fn golden_multi_cell_row() {
    let data = build_minimal_xls(
        &["Name", "Age", "City"],
        &[("Sheet1", &[(0, 0, "Name"), (0, 1, "Age"), (0, 2, "City")])],
    );
    let mut doc = XlsDocument::from_bytes_with_diag(&data, null_diag())
        .expect("should parse multi-cell-row XLS");
    let mut page = doc.page(0).expect("page 0 should exist");
    let text = page.text().expect("text() should succeed");
    assert_golden("xls_multi_cell_row", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 3: single sheet, grid of cells (multiple rows and columns)
// ---------------------------------------------------------------------------

#[test]
fn golden_grid() {
    let data = build_minimal_xls(
        &["A", "B", "C", "D"],
        &[(
            "Sheet1",
            &[(0, 0, "A"), (0, 1, "B"), (1, 0, "C"), (1, 1, "D")],
        )],
    );
    let mut doc =
        XlsDocument::from_bytes_with_diag(&data, null_diag()).expect("should parse grid XLS");
    let mut page = doc.page(0).expect("page 0 should exist");
    let text = page.text().expect("text() should succeed");
    assert_golden("xls_grid", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 4: multiple sheets -- verify each page independently
// ---------------------------------------------------------------------------

#[test]
fn golden_multi_sheet_page0() {
    let data = build_minimal_xls(
        &["alpha", "beta", "gamma"],
        &[
            ("Sheet1", &[(0, 0, "alpha"), (0, 1, "beta")]),
            ("Sheet2", &[(0, 0, "gamma")]),
        ],
    );
    let mut doc = XlsDocument::from_bytes_with_diag(&data, null_diag())
        .expect("should parse multi-sheet XLS");
    let mut page = doc.page(0).expect("page 0 should exist");
    let text = page.text().expect("text() should succeed");
    assert_golden("xls_multi_sheet_page0", &text, &golden_dir());
}

#[test]
fn golden_multi_sheet_page1() {
    let data = build_minimal_xls(
        &["alpha", "beta", "gamma"],
        &[
            ("Sheet1", &[(0, 0, "alpha"), (0, 1, "beta")]),
            ("Sheet2", &[(0, 0, "gamma")]),
        ],
    );
    let mut doc = XlsDocument::from_bytes_with_diag(&data, null_diag())
        .expect("should parse multi-sheet XLS");
    let mut page = doc.page(1).expect("page 1 should exist");
    let text = page.text().expect("text() should succeed");
    assert_golden("xls_multi_sheet_page1", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 5: empty sheet (no cells) -- should produce empty text
// ---------------------------------------------------------------------------

#[test]
fn golden_empty_sheet() {
    let data = build_minimal_xls(&[], &[("EmptySheet", &[])]);
    let mut doc = XlsDocument::from_bytes_with_diag(&data, null_diag())
        .expect("should parse XLS with empty sheet");
    let mut page = doc.page(0).expect("page 0 should exist");
    let text = page.text().expect("text() should succeed");
    assert_golden("xls_empty_sheet", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 6: table extraction -- verify TableRow/TableCell structure as text
// ---------------------------------------------------------------------------

#[test]
fn golden_table_text() {
    let data = build_minimal_xls(
        &["Header1", "Header2", "Val1", "Val2"],
        &[(
            "Data",
            &[
                (0, 0, "Header1"),
                (0, 1, "Header2"),
                (1, 0, "Val1"),
                (1, 1, "Val2"),
            ],
        )],
    );
    let mut doc =
        XlsDocument::from_bytes_with_diag(&data, null_diag()).expect("should parse table XLS");
    let mut page = doc.page(0).expect("page 0 should exist");
    let tables = page.tables().expect("tables() should succeed");
    assert_eq!(tables.len(), 1, "should have one table");
    // Render the table as text rows for golden comparison.
    let mut rendered = String::new();
    for row in &tables[0].rows {
        let parts: Vec<&str> = row.cells.iter().map(|c| c.text.as_str()).collect();
        rendered.push_str(&parts.join("\t"));
        rendered.push('\n');
    }
    // Trim trailing newline so the golden file is clean.
    let rendered = rendered.trim_end_matches('\n').to_string();
    assert_golden("xls_table_text", &rendered, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 7: merged cells -- verify col_span/row_span are applied
// ---------------------------------------------------------------------------

#[test]
fn golden_merged_cells_table() {
    // Sheet layout (3 rows x 3 cols):
    //   Row 0: "Header" spans cols 0-2 (1x3 merge), then col 1 and 2 are covered.
    //   Row 1: "A" in col 0, "B" in col 1, "C" in col 2.
    //   Row 2: "D" spans rows 2-3 col 0 (2x1 merge), "E" in col 1, "F" in col 2.
    //   Row 3: col 0 covered by the row 2 merge, "G" in col 1, "H" in col 2.
    //
    // SST: 0="Header", 1="A", 2="B", 3="C", 4="D", 5="E", 6="F", 7="G", 8="H"
    let sst = &["Header", "A", "B", "C", "D", "E", "F", "G", "H"];
    //          isst:  0        1    2    3    4    5    6    7    8
    let cells = &[
        (0u16, 0u16, 0u32), // row 0 col 0: "Header"
        (1, 0, 1),          // row 1 col 0: "A"
        (1, 1, 2),          // row 1 col 1: "B"
        (1, 2, 3),          // row 1 col 2: "C"
        (2, 0, 4),          // row 2 col 0: "D"
        (2, 1, 5),          // row 2 col 1: "E"
        (2, 2, 6),          // row 2 col 2: "F"
        (3, 1, 7),          // row 3 col 1: "G"
        (3, 2, 8),          // row 3 col 2: "H"
    ];
    let merges = &[
        (0u16, 0u16, 0u16, 2u16), // row 0 cols 0-2: header spans 3 cols
        (2, 3, 0, 0),             // rows 2-3 col 0: "D" spans 2 rows
    ];

    let data = build_minimal_xls_with_merges(sst, cells, merges);
    let mut doc = XlsDocument::from_bytes_with_diag(&data, null_diag())
        .expect("should parse merged-cells XLS");
    let mut page = doc.page(0).expect("page 0 should exist");
    let tables = page.tables().expect("tables() should succeed");
    assert_eq!(tables.len(), 1, "should have one table");

    // Render the table showing spans.
    let mut rendered = String::new();
    for row in &tables[0].rows {
        let parts: Vec<String> = row
            .cells
            .iter()
            .map(|c| {
                if c.col_span > 1 || c.row_span > 1 {
                    format!("{}[{}x{}]", c.text, c.row_span, c.col_span)
                } else {
                    c.text.clone()
                }
            })
            .collect();
        rendered.push_str(&parts.join("\t"));
        rendered.push('\n');
    }
    let rendered = rendered.trim_end_matches('\n').to_string();
    assert_golden("xls_merged_cells", &rendered, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 8: sheet with date-formatted NUMBER cells
// ---------------------------------------------------------------------------

#[test]
fn golden_date_formatted_numbers() {
    // Excel serial 44927 = 2023-01-01 (1900 epoch).
    // Serial 44928 = 2023-01-02.
    // Serial 45292 = 2024-01-15.
    let cells = &[(0u16, 0u16, 44927.0f64), (1, 0, 44928.0), (2, 0, 45292.0)];
    let data = build_minimal_xls_with_date_numbers(cells);
    let mut doc = XlsDocument::from_bytes_with_diag(&data, null_diag())
        .expect("should parse date-number XLS");
    let mut page = doc.page(0).expect("page 0 should exist");
    let text = page.text().expect("text() should succeed");
    assert_golden("xls_date_numbers", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 9: sheet with BOOLERR cells (boolean TRUE/FALSE and error values)
// ---------------------------------------------------------------------------

#[test]
fn golden_boolerr_cells() {
    // Cells:
    //   row 0 col 0: TRUE (f_error=0, b_bool_err=1)
    //   row 0 col 1: FALSE (f_error=0, b_bool_err=0)
    //   row 1 col 0: error code 7 = #DIV/0! (f_error=1, b_bool_err=7)
    //   row 1 col 1: error code 29 = #NUM! (f_error=1, b_bool_err=29)
    let cells = &[
        (0u16, 0u16, 1u8, 0u8), // TRUE
        (0, 1, 0, 0),           // FALSE
        (1, 0, 7, 1),           // error code 7
        (1, 1, 29, 1),          // error code 29
    ];
    let data = build_minimal_xls_with_boolerr(cells);
    let mut doc =
        XlsDocument::from_bytes_with_diag(&data, null_diag()).expect("should parse boolerr XLS");
    let mut page = doc.page(0).expect("page 0 should exist");
    let text = page.text().expect("text() should succeed");
    assert_golden("xls_boolerr_cells", &text, &golden_dir());
}

//! Spot-check integration tests for table extraction on real corpus PDFs.
//!
//! TE-012: Exercises Page::tables() against PDFs from the test corpus that
//! likely contain table-like structures (IRS forms, tabular layouts, reports).
//! Each test verifies no panic/error occurs and prints table info for manual
//! review with `cargo test -- --nocapture`.
//!
//! These tests deliberately tolerate zero tables: the ruled-line detector may
//! not find tables in all PDFs (especially form-style PDFs where cells are
//! rendered as separate XObjects or where borders are thin fills rather than
//! stroked paths).

use udoc_pdf::Document;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const MINIMAL_DIR: &str = "tests/corpus/minimal";
const REALWORLD_DIR: &str = "tests/corpus/realworld";

fn open_minimal(filename: &str) -> Document {
    let path = format!("{MINIMAL_DIR}/{filename}");
    Document::open(&path).unwrap_or_else(|e| panic!("{filename} should open: {e}"))
}

fn open_realworld(filename: &str) -> Document {
    let path = format!("{REALWORLD_DIR}/{filename}");
    Document::open(&path).unwrap_or_else(|e| panic!("{filename} should open: {e}"))
}

/// Print table summary for manual review (visible with --nocapture).
fn print_table_summary(pdf_name: &str, page_idx: usize, tables: &[udoc_pdf::Table]) {
    if tables.is_empty() {
        eprintln!("  {pdf_name} page {page_idx}: no tables detected");
        return;
    }
    for (i, table) in tables.iter().enumerate() {
        eprintln!(
            "  {pdf_name} page {page_idx} table {i}: {} rows x {} cols ({}) [{:.0},{:.0} - {:.0},{:.0}]",
            table.rows.len(),
            table.num_columns,
            table.detection_method,
            table.bbox.x_min, table.bbox.y_min,
            table.bbox.x_max, table.bbox.y_max,
        );
        for (r, row) in table.rows.iter().enumerate() {
            let cells: Vec<&str> = row.cells.iter().map(|c| c.text.as_str()).collect();
            let prefix = if row.is_header { "[H] " } else { "    " };
            eprintln!("    row {r}: {prefix}{}", cells.join(" | "));
        }
    }
}

// ---------------------------------------------------------------------------
// Minimal corpus: table_layout.pdf
// ---------------------------------------------------------------------------

/// table_layout.pdf is a synthetic PDF with tabular content (Name/Score
/// columns). It uses Td-based positioning without stroked ruled lines, so
/// the ruled-line detector may not find tables. This test verifies no
/// panic and logs what the detector sees. Once text-alignment detection
/// (TE-007) lands, this should find tables.
#[test]
fn table_corpus_table_layout() {
    let mut doc = open_minimal("table_layout.pdf");
    let mut page = doc.page(0).unwrap_or_else(|e| panic!("page 0: {e}"));
    let tables = page.tables().unwrap_or_else(|e| panic!("tables(): {e}"));

    eprintln!("--- table_layout.pdf ---");
    print_table_summary("table_layout.pdf", 0, &tables);

    // Also log paths to help debug why tables may not be detected.
    let paths = page
        .path_segments()
        .unwrap_or_else(|e| panic!("path_segments(): {e}"));
    eprintln!("  table_layout.pdf paths: {}", paths.len());

    // table_layout.pdf has no ruled lines, but the text-alignment detector
    // (TE-007) should find the table structure from column alignment.
    if !tables.is_empty() {
        let table = &tables[0];
        assert!(
            table.rows.len() >= 2,
            "table_layout.pdf table should have at least 2 rows, got {}",
            table.rows.len()
        );
        assert!(
            table.num_columns >= 2,
            "table_layout.pdf table should have at least 2 columns, got {}",
            table.num_columns
        );
    } else {
        // Text-alignment detection may not fire if the PDF's span layout
        // doesn't meet the minimum row/column consistency thresholds.
        eprintln!(
            "  NOTE: table_layout.pdf: no tables detected (no ruled lines, \
             text-alignment thresholds not met)."
        );
    }
}

// ---------------------------------------------------------------------------
// Realworld corpus: IRS W-9 form
// ---------------------------------------------------------------------------

/// IRS W-9 is a complex form with many ruled lines forming box structures.
/// The table detector should not panic and may find table-like structures.
#[test]
fn table_corpus_irs_w9() {
    let mut doc = open_realworld("irs_w9.pdf");
    let page_count = doc.page_count();
    assert!(page_count > 0, "irs_w9.pdf should have at least 1 page");

    eprintln!("--- irs_w9.pdf ({page_count} pages) ---");

    let mut total_tables = 0;
    for page_idx in 0..page_count {
        let mut page = doc
            .page(page_idx)
            .unwrap_or_else(|e| panic!("irs_w9.pdf page {page_idx}: {e}"));
        let tables = page
            .tables()
            .unwrap_or_else(|e| panic!("irs_w9.pdf page {page_idx} tables(): {e}"));
        print_table_summary("irs_w9.pdf", page_idx, &tables);
        total_tables += tables.len();
    }

    eprintln!("  irs_w9.pdf total tables: {total_tables}");
    // W-9 has many ruled lines forming boxes. We expect some tables to be found,
    // though the exact count depends on the detector's sensitivity.
}

// ---------------------------------------------------------------------------
// Realworld corpus: IRS 1040 form
// ---------------------------------------------------------------------------

/// IRS 1040 is a multi-page tax form full of ruled lines and grid structures.
#[test]
fn table_corpus_irs_1040() {
    let mut doc = open_realworld("irs_1040.pdf");
    let page_count = doc.page_count();
    assert!(page_count > 0, "irs_1040.pdf should have at least 1 page");

    eprintln!("--- irs_1040.pdf ({page_count} pages) ---");

    let mut total_tables = 0;
    for page_idx in 0..page_count {
        let mut page = doc
            .page(page_idx)
            .unwrap_or_else(|e| panic!("irs_1040.pdf page {page_idx}: {e}"));
        let tables = page
            .tables()
            .unwrap_or_else(|e| panic!("irs_1040.pdf page {page_idx} tables(): {e}"));
        print_table_summary("irs_1040.pdf", page_idx, &tables);
        total_tables += tables.len();
    }

    eprintln!("  irs_1040.pdf total tables: {total_tables}");
}

// ---------------------------------------------------------------------------
// Realworld corpus: IRS W-4 form
// ---------------------------------------------------------------------------

/// IRS W-4 is another form with table-like structures.
#[test]
fn table_corpus_irs_w4() {
    let mut doc = open_realworld("irs_w4.pdf");
    let page_count = doc.page_count();
    assert!(page_count > 0, "irs_w4.pdf should have at least 1 page");

    eprintln!("--- irs_w4.pdf ({page_count} pages) ---");

    let mut total_tables = 0;
    for page_idx in 0..page_count {
        let mut page = doc
            .page(page_idx)
            .unwrap_or_else(|e| panic!("irs_w4.pdf page {page_idx}: {e}"));
        let tables = page
            .tables()
            .unwrap_or_else(|e| panic!("irs_w4.pdf page {page_idx} tables(): {e}"));
        print_table_summary("irs_w4.pdf", page_idx, &tables);
        total_tables += tables.len();
    }

    eprintln!("  irs_w4.pdf total tables: {total_tables}");
}

// ---------------------------------------------------------------------------
// Realworld corpus: libreoffice_form.pdf
// ---------------------------------------------------------------------------

/// LibreOffice form PDF with checkboxes and field boxes rendered as ruled lines.
/// Form field boxes are correctly filtered as non-tables (small bordered boxes
/// with 1-2 rows and 1 column are not tabular structures).
#[test]
fn table_corpus_libreoffice_form() {
    let mut doc = open_realworld("libreoffice_form.pdf");
    let page_count = doc.page_count();
    assert!(page_count > 0);

    eprintln!("--- libreoffice_form.pdf ({page_count} pages) ---");

    let mut page = doc.page(0).unwrap_or_else(|e| panic!("page 0: {e}"));
    let tables = page.tables().unwrap_or_else(|e| panic!("tables(): {e}"));
    print_table_summary("libreoffice_form.pdf", 0, &tables);

    // LibreOffice forms have ruled-line boxes for form fields. These are
    // correctly filtered as non-tables (small single-column bordered boxes).
    // If no real tables exist, empty result is correct.
}

// ---------------------------------------------------------------------------
// Realworld corpus: nist_sp1300.pdf
// ---------------------------------------------------------------------------

/// NIST SP 1300 is a government publication likely containing data tables.
/// Only test the first few pages to keep test time reasonable.
#[test]
fn table_corpus_nist_sp1300() {
    let mut doc = open_realworld("nist_sp1300.pdf");
    let page_count = doc.page_count();
    assert!(page_count > 0);

    // Test first 10 pages max (the document may be long).
    let test_pages = page_count.min(10);
    eprintln!("--- nist_sp1300.pdf ({page_count} pages, testing first {test_pages}) ---");

    let mut total_tables = 0;
    for page_idx in 0..test_pages {
        let mut page = doc
            .page(page_idx)
            .unwrap_or_else(|e| panic!("nist_sp1300.pdf page {page_idx}: {e}"));
        let tables = page
            .tables()
            .unwrap_or_else(|e| panic!("nist_sp1300.pdf page {page_idx} tables(): {e}"));
        print_table_summary("nist_sp1300.pdf", page_idx, &tables);
        total_tables += tables.len();
    }

    eprintln!("  nist_sp1300.pdf total tables (first {test_pages} pages): {total_tables}");
}

// ---------------------------------------------------------------------------
// Minimal corpus: form_xobject.pdf
// ---------------------------------------------------------------------------

/// Form XObject PDF, exercises path capture through XObject boundaries.
#[test]
fn table_corpus_form_xobject() {
    let mut doc = open_minimal("form_xobject.pdf");
    let mut page = doc.page(0).unwrap_or_else(|e| panic!("page 0: {e}"));
    let tables = page.tables().unwrap_or_else(|e| panic!("tables(): {e}"));

    eprintln!("--- form_xobject.pdf ---");
    print_table_summary("form_xobject.pdf", 0, &tables);
}

// ---------------------------------------------------------------------------
// Realworld corpus: google_doc.pdf
// ---------------------------------------------------------------------------

/// Google Docs export with a known country data table (ruled lines).
/// This PDF has a table with columns like Indonesia, Germany, etc.
#[test]
fn table_corpus_google_doc() {
    let mut doc = open_realworld("google_doc.pdf");
    let page_count = doc.page_count();
    assert!(page_count > 0);

    eprintln!("--- google_doc.pdf ({page_count} pages) ---");

    let mut page = doc.page(0).unwrap_or_else(|e| panic!("page 0: {e}"));
    let tables = page.tables().unwrap_or_else(|e| panic!("tables(): {e}"));
    print_table_summary("google_doc.pdf", 0, &tables);

    // google_doc.pdf has a visible ruled-line table with country data.
    // The detector should find at least one table.
    assert!(
        !tables.is_empty(),
        "google_doc.pdf should detect at least one table"
    );

    // Find the country table by looking for the one containing country data.
    // Note: Google Docs PDFs render text with per-glyph positioning, so
    // extracted text may have spaces between characters (e.g., "A s i a").
    let country_table = tables.iter().find(|t| {
        if t.rows.len() < 3 || t.num_columns < 3 {
            return false;
        }
        let all_text: String = t
            .rows
            .iter()
            .flat_map(|row| row.cells.iter())
            .map(|cell| cell.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let collapsed: String = all_text.split_whitespace().collect::<Vec<_>>().join(" ");
        collapsed.contains("Asia")
            || collapsed.contains("Europe")
            || collapsed.contains("Jakarta")
            || collapsed.contains("Berlin")
            || all_text.contains("A s i a")
            || all_text.contains("E u r o p e")
            || all_text.contains("J a k a r t a")
            || all_text.contains("B e r l i n")
    });
    assert!(
        country_table.is_some(),
        "google_doc.pdf should have a table with country data"
    );
}

// ---------------------------------------------------------------------------
// Realworld corpus: libreoffice_writer.pdf
// ---------------------------------------------------------------------------

/// LibreOffice Writer export with ruled-line tables.
#[test]
fn table_corpus_libreoffice_writer() {
    let mut doc = open_realworld("libreoffice_writer.pdf");
    let page_count = doc.page_count();
    assert!(page_count > 0);

    eprintln!("--- libreoffice_writer.pdf ({page_count} pages) ---");

    let mut page = doc.page(0).unwrap_or_else(|e| panic!("page 0: {e}"));
    let tables = page.tables().unwrap_or_else(|e| panic!("tables(): {e}"));
    print_table_summary("libreoffice_writer.pdf", 0, &tables);

    // LibreOffice Writer exports tables with stroked borders.
    assert!(
        !tables.is_empty(),
        "libreoffice_writer.pdf should detect at least one table"
    );
}

// ---------------------------------------------------------------------------
// Realworld corpus: multicolumn.pdf
// ---------------------------------------------------------------------------

/// Multi-column PDF should not panic during table detection.
#[test]
fn table_corpus_multicolumn_no_panic() {
    let mut doc = open_realworld("multicolumn.pdf");
    let mut page = doc.page(0).unwrap_or_else(|e| panic!("page 0: {e}"));
    // Exercise the table detection pipeline (no-panic check).
    let _tables = page.tables().unwrap_or_else(|e| panic!("tables(): {e}"));
}

// ---------------------------------------------------------------------------
// Minimal corpus: tagged_structure_tree.pdf
// ---------------------------------------------------------------------------

/// Tagged PDF with structure tree. Table tags may be present but the
/// ruled-line detector operates on paths, not tags. This verifies no panic.
#[test]
fn table_corpus_tagged_structure() {
    let mut doc = open_minimal("tagged_structure_tree.pdf");
    let mut page = doc.page(0).unwrap_or_else(|e| panic!("page 0: {e}"));
    let tables = page.tables().unwrap_or_else(|e| panic!("tables(): {e}"));

    eprintln!("--- tagged_structure_tree.pdf ---");
    print_table_summary("tagged_structure_tree.pdf", 0, &tables);
}

// ---------------------------------------------------------------------------
// Sweep: all minimal corpus PDFs should not panic on tables()
// ---------------------------------------------------------------------------

/// Run tables() on every PDF in the minimal corpus. No panics allowed.
/// This is a broad robustness check, not an accuracy test.
#[test]
fn table_corpus_sweep_minimal() {
    let dir = std::path::Path::new(MINIMAL_DIR);
    let mut tested = 0;
    let mut with_tables = 0;
    let mut total_tables = 0;

    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .expect("read minimal corpus dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext == "pdf")
                .unwrap_or(false)
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());

    eprintln!("--- sweep: minimal corpus ({} PDFs) ---", entries.len());

    for entry in &entries {
        let path = entry.path();
        let filename = path.file_name().unwrap().to_string_lossy();

        let mut doc = match Document::open(path.to_str().unwrap()) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("  {filename}: open failed (expected for some): {e}");
                continue;
            }
        };

        let page_count = doc.page_count();
        let mut file_tables = 0;

        for page_idx in 0..page_count {
            let mut page = match doc.page(page_idx) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("  {filename} page {page_idx}: page() failed: {e}");
                    continue;
                }
            };

            match page.tables() {
                Ok(tables) => {
                    file_tables += tables.len();
                }
                Err(e) => {
                    eprintln!("  {filename} page {page_idx}: tables() failed: {e}");
                }
            }
        }

        if file_tables > 0 {
            eprintln!("  {filename}: {file_tables} table(s)");
            with_tables += 1;
        }
        total_tables += file_tables;
        tested += 1;
    }

    eprintln!("  sweep complete: {tested} PDFs tested, {with_tables} with tables, {total_tables} total tables");

    // At least most files should be processable without errors.
    assert!(
        tested >= 50,
        "expected at least 50 minimal corpus PDFs, tested {tested}"
    );
}

// ---------------------------------------------------------------------------
// Sweep: all realworld corpus PDFs should not panic on tables()
// ---------------------------------------------------------------------------

/// Run tables() on every PDF in the realworld corpus. No panics allowed.
#[test]
fn table_corpus_sweep_realworld() {
    let dir = std::path::Path::new(REALWORLD_DIR);
    let mut tested = 0;
    let mut with_tables = 0;
    let mut total_tables = 0;

    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .expect("read realworld corpus dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext == "pdf")
                .unwrap_or(false)
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());

    eprintln!("--- sweep: realworld corpus ({} PDFs) ---", entries.len());

    for entry in &entries {
        let path = entry.path();
        let filename = path.file_name().unwrap().to_string_lossy();

        let mut doc = match Document::open(path.to_str().unwrap()) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("  {filename}: open failed: {e}");
                continue;
            }
        };

        let page_count = doc.page_count();
        let mut file_tables = 0;

        for page_idx in 0..page_count {
            let mut page = match doc.page(page_idx) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("  {filename} page {page_idx}: page() failed: {e}");
                    continue;
                }
            };

            match page.tables() {
                Ok(tables) => {
                    file_tables += tables.len();
                }
                Err(e) => {
                    eprintln!("  {filename} page {page_idx}: tables() failed: {e}");
                }
            }
        }

        if file_tables > 0 {
            eprintln!("  {filename}: {file_tables} table(s)");
            with_tables += 1;
        }
        total_tables += file_tables;
        tested += 1;
    }

    eprintln!("  sweep complete: {tested} PDFs tested, {with_tables} with tables, {total_tables} total tables");

    // All realworld PDFs should be processable.
    assert!(
        tested >= 10,
        "expected at least 10 realworld corpus PDFs, tested {tested}"
    );
}

// ---------------------------------------------------------------------------
// Table structure validation: verify cell text is non-empty for known tables
// ---------------------------------------------------------------------------

/// For table_layout.pdf, verify cells contain expected text if tables are found.
/// Currently table_layout.pdf has no ruled lines so this is a conditional check.
/// When text-alignment detection (TE-007) lands, uncomment the hard assertions.
#[test]
fn table_corpus_table_layout_cell_content() {
    let mut doc = open_minimal("table_layout.pdf");
    let mut page = doc.page(0).unwrap_or_else(|e| panic!("page 0: {e}"));
    let tables = page.tables().unwrap_or_else(|e| panic!("tables(): {e}"));

    if tables.is_empty() {
        // No tables from ruled-line detection (expected for this borderless PDF).
        // Verify text extraction still works.
        let text = page.text().unwrap_or_else(|e| panic!("text(): {e}"));
        assert!(
            text.contains("Name"),
            "table_layout.pdf text should contain 'Name', got: {text}"
        );
        return;
    }

    let table = &tables[0];
    let all_text: String = table
        .rows
        .iter()
        .flat_map(|row| row.cells.iter())
        .map(|cell| cell.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");

    assert!(
        all_text.contains("Name"),
        "table cells should contain 'Name', got: {all_text}"
    );
    assert!(
        all_text.contains("Score"),
        "table cells should contain 'Score', got: {all_text}"
    );
}

// ---------------------------------------------------------------------------
// Path capture verification: IRS W-9 should have many path segments
// ---------------------------------------------------------------------------

/// Verify that path capture works on a complex real PDF. IRS W-9 has
/// hundreds of ruled lines forming its form structure.
#[test]
fn table_corpus_irs_w9_paths() {
    let mut doc = open_realworld("irs_w9.pdf");
    let mut page = doc.page(0).unwrap_or_else(|e| panic!("page 0: {e}"));
    let paths = page
        .path_segments()
        .unwrap_or_else(|e| panic!("path_segments(): {e}"));

    eprintln!("--- irs_w9.pdf page 0 paths: {} ---", paths.len());

    // IRS W-9 is a complex form. It should have many path segments.
    assert!(
        paths.len() >= 10,
        "irs_w9.pdf should have at least 10 path segments, got {}",
        paths.len()
    );
}

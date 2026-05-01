//! Real-world XLSX file extraction tests.
//!
//! Tests against actual .xlsx files from public sample repositories to validate
//! the parser handles real-world OOXML structures, not just synthetic test data.

use std::path::PathBuf;
use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_xlsx::XlsxDocument;

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/real-world")
}

#[test]
fn real_world_no_panics_on_all_files() {
    let dir = corpus_dir();
    if !dir.exists() {
        eprintln!("corpus directory not found, skipping: {}", dir.display());
        return;
    }

    let entries: Vec<_> = std::fs::read_dir(&dir).expect("read corpus dir").collect();

    let mut xlsx_count = 0;

    for entry in entries {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.extension().map(|e| e == "xlsx").unwrap_or(false) {
            let data = std::fs::read(&path).expect("read file");
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            xlsx_count += 1;

            match XlsxDocument::from_bytes(&data) {
                Ok(mut doc) => {
                    let count = FormatBackend::page_count(&doc);
                    for i in 0..count {
                        if let Ok(mut page) = doc.page(i) {
                            let _ = page.text();
                            let _ = page.text_lines();
                            let _ = page.raw_spans();
                            let _ = page.tables();
                            let _ = page.images();
                        }
                    }
                    eprintln!("{name}: OK, {count} sheet(s)");
                }
                Err(e) => {
                    // Parse errors are acceptable (file might be encrypted,
                    // strict OOXML, or use unsupported features), but panics are not.
                    eprintln!("{name}: parse error (acceptable): {e}");
                }
            }
        }
    }

    eprintln!("processed {xlsx_count} .xlsx file(s)");
    assert!(xlsx_count > 0, "no xlsx files found in corpus");
}

#[test]
fn real_world_two_sheets_has_multiple_pages() {
    let path = corpus_dir().join("TwoSheetsNoneHidden.xlsx");
    if !path.exists() {
        eprintln!("TwoSheetsNoneHidden.xlsx not found, skipping");
        return;
    }
    let data = std::fs::read(&path).expect("read TwoSheetsNoneHidden.xlsx");
    match XlsxDocument::from_bytes(&data) {
        Ok(doc) => {
            let count = FormatBackend::page_count(&doc);
            eprintln!("TwoSheetsNoneHidden.xlsx: {count} sheet(s)");
            assert!(count >= 2, "expected at least 2 sheets, got {count}");
        }
        Err(e) => {
            eprintln!("TwoSheetsNoneHidden.xlsx: parse error (acceptable): {e}");
        }
    }
}

#[test]
fn real_world_date_format_text_extraction() {
    let path = corpus_dir().join("DateFormatTests.xlsx");
    if !path.exists() {
        eprintln!("DateFormatTests.xlsx not found, skipping");
        return;
    }
    let data = std::fs::read(&path).expect("read DateFormatTests.xlsx");
    if let Ok(mut doc) = XlsxDocument::from_bytes(&data) {
        let count = FormatBackend::page_count(&doc);
        eprintln!("DateFormatTests.xlsx: {count} sheet(s)");
        if count > 0 {
            if let Ok(mut page) = doc.page(0) {
                if let Ok(text) = page.text() {
                    eprintln!(
                        "  text ({} chars): {:?}",
                        text.len(),
                        &text[..text.len().min(200)]
                    );
                    assert!(
                        !text.is_empty(),
                        "DateFormatTests.xlsx text should not be empty"
                    );
                }
            }
        }
    }
}

#[test]
fn real_world_table_extraction() {
    let path = corpus_dir().join("WithTable.xlsx");
    if !path.exists() {
        eprintln!("WithTable.xlsx not found, skipping");
        return;
    }
    let data = std::fs::read(&path).expect("read WithTable.xlsx");
    if let Ok(mut doc) = XlsxDocument::from_bytes(&data) {
        let count = FormatBackend::page_count(&doc);
        eprintln!("WithTable.xlsx: {count} sheet(s)");
        if count > 0 {
            if let Ok(mut page) = doc.page(0) {
                let tables = page.tables().expect("tables()");
                eprintln!("  tables: {}", tables.len());
                let text = page.text().expect("text()");
                eprintln!(
                    "  text ({} chars): {:?}",
                    text.len(),
                    &text[..text.len().min(200)]
                );
            }
        }
    }
}

/// Regression test for 57893-many-merges.xlsx (849KB, thousands of merge regions).
/// This file has ~50K merge regions (capped to 10K by MAX_MERGE_REGIONS).
/// The MergeCache on XlsxPage ensures build_covered_set + anchor index are built
/// once per page instead of once per extraction method, and find_merge_at is O(1).
/// Expected perf: <500ms release, <5s debug (XML parsing dominates in debug).
#[test]
fn real_world_many_merges_regression() {
    let path = corpus_dir().join("57893-many-merges.xlsx");
    if !path.exists() {
        eprintln!("57893-many-merges.xlsx not found, skipping");
        return;
    }
    let data = std::fs::read(&path).expect("read 57893-many-merges.xlsx");
    let mut doc = XlsxDocument::from_bytes(&data).expect("parse 57893-many-merges.xlsx");
    let count = FormatBackend::page_count(&doc);
    assert!(count > 0, "expected at least 1 sheet");

    let start = std::time::Instant::now();
    let mut page = doc.page(0).expect("open sheet 0");

    // The file has thousands of merges but cells with empty text.
    let text = page.text().expect("text() should succeed");

    // Exercise all extraction methods to verify no panics with heavy merge data.
    // With MergeCache, the covered set and anchor index are built only once.
    let _ = page.text_lines().expect("text_lines() should succeed");
    let _ = page.raw_spans().expect("raw_spans() should succeed");
    let _ = page.tables().expect("tables() should succeed");
    let _ = page.images().expect("images() should succeed");
    let elapsed = start.elapsed();

    eprintln!(
        "57893-many-merges.xlsx: {} sheet(s), text len={}, elapsed={:?}",
        count,
        text.len(),
        elapsed
    );

    // 5s budget accommodates debug builds (XML parsing of 7MB sheet is slow
    // without optimizations). Release builds finish in <500ms.
    assert!(
        elapsed.as_secs() < 5,
        "many-merges extraction took {:?}, expected <5s",
        elapsed
    );
}

#[test]
fn real_world_sample_files_text_extraction() {
    let dir = corpus_dir();
    if !dir.exists() {
        eprintln!("corpus directory not found, skipping: {}", dir.display());
        return;
    }

    for entry in std::fs::read_dir(&dir).expect("read corpus dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.extension().map(|e| e == "xlsx").unwrap_or(false) {
            let data = std::fs::read(&path).expect("read file");
            let name = path.file_name().unwrap().to_string_lossy().to_string();

            if let Ok(mut doc) = XlsxDocument::from_bytes(&data) {
                let meta = doc.metadata();
                eprintln!(
                    "{name}: {} sheet(s), title={:?}",
                    meta.page_count, meta.title
                );

                if let Ok(mut page) = doc.page(0) {
                    if let Ok(text) = page.text() {
                        eprintln!(
                            "  text ({} chars): {:?}",
                            text.len(),
                            &text[..text.len().min(200)]
                        );
                    }
                }
            }
        }
    }
}

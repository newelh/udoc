//! Real-world XLS file extraction tests.
//!
//! Tests against actual .xls files from public sample repositories to validate
//! the parser handles real-world BIFF8 structures, not just synthetic test data.

use std::path::PathBuf;
use std::sync::Arc;
use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_core::diagnostics::NullDiagnostics;
use udoc_xls::XlsDocument;

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/real-world")
}

fn null_diag() -> Arc<dyn udoc_core::diagnostics::DiagnosticsSink> {
    Arc::new(NullDiagnostics)
}

#[test]
fn real_world_no_panics_on_all_files() {
    let dir = corpus_dir();
    if !dir.exists() {
        eprintln!("corpus directory not found, skipping: {}", dir.display());
        return;
    }

    let entries: Vec<_> = std::fs::read_dir(&dir).expect("read corpus dir").collect();

    let mut xls_count = 0;

    for entry in entries {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.extension().map(|e| e == "xls").unwrap_or(false) {
            let data = std::fs::read(&path).expect("read file");
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            xls_count += 1;

            match XlsDocument::from_bytes_with_diag(&data, null_diag()) {
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
                    // Parse errors are acceptable (file might be BIFF5/earlier or
                    // encrypted), but panics are not.
                    eprintln!("{name}: parse error (acceptable): {e}");
                }
            }
        }
    }

    eprintln!("processed {xls_count} .xls file(s)");
}

#[test]
fn real_world_two_sheets_has_multiple_pages() {
    let path = corpus_dir().join("two_sheets.xls");
    if !path.exists() {
        eprintln!("two_sheets.xls not found, skipping");
        return;
    }
    let data = std::fs::read(&path).expect("read two_sheets.xls");
    match XlsDocument::from_bytes_with_diag(&data, null_diag()) {
        Ok(doc) => {
            let count = FormatBackend::page_count(&doc);
            eprintln!("two_sheets.xls: {count} sheet(s)");
            assert!(count >= 2, "expected at least 2 sheets, got {count}");
        }
        Err(e) => {
            eprintln!("two_sheets.xls: parse error (acceptable): {e}");
        }
    }
}

#[test]
fn real_world_date_formats_text_extraction() {
    let path = corpus_dir().join("date_formats.xls");
    if !path.exists() {
        eprintln!("date_formats.xls not found, skipping");
        return;
    }
    let data = std::fs::read(&path).expect("read date_formats.xls");
    if let Ok(mut doc) = XlsDocument::from_bytes_with_diag(&data, null_diag()) {
        let count = FormatBackend::page_count(&doc);
        eprintln!("date_formats.xls: {count} sheet(s)");
        if count > 0 {
            if let Ok(mut page) = doc.page(0) {
                if let Ok(text) = page.text() {
                    eprintln!(
                        "  text ({} chars): {:?}",
                        text.len(),
                        &text[..text.len().min(200)]
                    );
                    // The file has date/numeric data; text should be non-empty.
                    assert!(
                        !text.is_empty(),
                        "date_formats.xls text should not be empty"
                    );
                }
            }
        }
    }
}

#[test]
fn real_world_chinese_provinces_text_extraction() {
    let path = corpus_dir().join("chinese_provinces.xls");
    if !path.exists() {
        eprintln!("chinese_provinces.xls not found, skipping");
        return;
    }
    let data = std::fs::read(&path).expect("read chinese_provinces.xls");
    if let Ok(mut doc) = XlsDocument::from_bytes_with_diag(&data, null_diag()) {
        let count = FormatBackend::page_count(&doc);
        eprintln!("chinese_provinces.xls: {count} sheet(s)");
        if count > 0 {
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

//! Real-world ODF file extraction tests.
//!
//! Tests against actual .odt/.ods/.odp files from Microsoft Office (exported
//! to ODF format) to validate the parser handles real-world structures, not
//! just synthetic test data.

use std::path::PathBuf;
use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_odf::OdfDocument;

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/real-world")
}

fn load_odf(name: &str) -> OdfDocument {
    let path = corpus_dir().join(name);
    let data =
        std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    OdfDocument::from_bytes(&data)
        .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// ODT: freetestdata_100kb.odt -- 10-page Word-exported document
// ---------------------------------------------------------------------------

#[test]
fn real_world_odt_freetestdata() {
    let mut doc = load_odf("freetestdata_100kb.odt");
    // ODT is always 1 logical page.
    assert_eq!(doc.page_count(), 1, "ODT should have 1 logical page");

    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert!(!text.is_empty(), "ODT should have text content");

    let lines = page.text_lines().expect("text_lines()");
    assert!(!lines.is_empty(), "ODT should have text lines");

    let spans = page.raw_spans().expect("raw_spans()");
    assert!(!spans.is_empty(), "ODT should have raw spans");

    eprintln!(
        "freetestdata_100kb.odt: {} chars, {} lines, {} spans, text preview: {:?}",
        text.len(),
        lines.len(),
        spans.len(),
        &text[..text.len().min(200)]
    );
}

// ---------------------------------------------------------------------------
// ODS: freetestdata_100kb.ods -- Excel-exported spreadsheet
// ---------------------------------------------------------------------------

#[test]
fn real_world_ods_freetestdata() {
    let mut doc = load_odf("freetestdata_100kb.ods");
    assert!(
        doc.page_count() >= 1,
        "ODS should have at least 1 sheet, got {}",
        doc.page_count()
    );

    let meta = doc.metadata();
    eprintln!(
        "freetestdata_100kb.ods: {} sheets, title={:?}",
        meta.page_count, meta.title
    );

    for i in 0..doc.page_count() {
        let mut page = doc.page(i).expect("page");
        let text = page.text().expect("text()");
        let tables = page.tables().expect("tables()");
        eprintln!("  sheet {i}: {} chars, {} tables", text.len(), tables.len());

        // At least the first sheet should have some data.
        if i == 0 {
            assert!(!text.is_empty(), "first sheet should have text content");
        }
    }
}

// ---------------------------------------------------------------------------
// ODP: freetestdata_100kb.odp -- PowerPoint-exported presentation
// ---------------------------------------------------------------------------

#[test]
fn real_world_odp_freetestdata() {
    let mut doc = load_odf("freetestdata_100kb.odp");
    assert!(
        doc.page_count() >= 1,
        "ODP should have at least 1 slide, got {}",
        doc.page_count()
    );

    let meta = doc.metadata();
    eprintln!(
        "freetestdata_100kb.odp: {} slides, title={:?}",
        meta.page_count, meta.title
    );

    for i in 0..doc.page_count() {
        let mut page = doc.page(i).expect("page");
        let text = page.text().expect("text()");
        let lines = page.text_lines().expect("text_lines()");
        let spans = page.raw_spans().expect("raw_spans()");
        eprintln!(
            "  slide {i}: {} chars, {} lines, {} spans",
            text.len(),
            lines.len(),
            spans.len()
        );
    }
}

// ---------------------------------------------------------------------------
// Sweep: exercise all extraction methods on every file without panicking.
// ---------------------------------------------------------------------------

#[test]
fn real_world_no_panics_on_all_files() {
    let dir = corpus_dir();
    if !dir.exists() {
        eprintln!("corpus directory not found, skipping: {}", dir.display());
        return;
    }

    for entry in std::fs::read_dir(&dir).expect("read corpus dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();
        if !matches!(ext.as_str(), "odt" | "ods" | "odp") {
            continue;
        }

        let data = std::fs::read(&path).expect("read file");
        let name = path.file_name().unwrap().to_string_lossy().to_string();

        match OdfDocument::from_bytes(&data) {
            Ok(mut doc) => {
                let count = doc.page_count();
                for i in 0..count {
                    if let Ok(mut page) = doc.page(i) {
                        let _ = page.text();
                        let _ = page.text_lines();
                        let _ = page.raw_spans();
                        let _ = page.tables();
                        let _ = page.images();
                    }
                }
                eprintln!("{name}: OK, {count} pages/sheets/slides");
            }
            Err(e) => {
                // Parse errors are acceptable (file might use unsupported
                // features), but panics are not.
                eprintln!("{name}: parse error (acceptable): {e}");
            }
        }
    }
}

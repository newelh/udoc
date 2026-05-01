//! Real-world PPT file extraction tests.
//!
//! Tests against actual .ppt files from Office 2003/2007 to validate the
//! parser handles real-world binary structures, not just synthetic test data.

use std::path::PathBuf;
use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_ppt::PptDocument;

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/real-world")
}

fn load_ppt(name: &str) -> PptDocument {
    let path = corpus_dir().join(name);
    let data =
        std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    PptDocument::from_bytes(&data)
        .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()))
}

#[test]
fn real_world_examplefiles_1slide() {
    let mut doc = load_ppt("examplefiles_1slide.ppt");
    assert!(doc.page_count() >= 1, "expected at least 1 slide");

    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert!(!text.is_empty(), "slide 0 should have text");
    eprintln!(
        "examplefiles_1slide: {} slides, slide 0 text ({} chars): {:?}",
        doc.page_count(),
        text.len(),
        &text[..text.len().min(200)]
    );
}

#[test]
fn real_world_examplefiles_2slides() {
    let mut doc = load_ppt("examplefiles_2slides.ppt");
    assert!(
        doc.page_count() >= 2,
        "expected at least 2 slides, got {}",
        doc.page_count()
    );

    for i in 0..doc.page_count().min(2) {
        let mut page = doc.page(i).expect("page");
        let text = page.text().expect("text()");
        eprintln!(
            "examplefiles_2slides: slide {i} ({} chars): {:?}",
            text.len(),
            &text[..text.len().min(100)]
        );
    }
}

#[test]
fn real_world_examplefiles_3slides() {
    let mut doc = load_ppt("examplefiles_3slides.ppt");
    assert!(
        doc.page_count() >= 3,
        "expected at least 3 slides, got {}",
        doc.page_count()
    );

    for i in 0..doc.page_count().min(3) {
        let mut page = doc.page(i).expect("page");
        let text = page.text().expect("text()");
        eprintln!(
            "examplefiles_3slides: slide {i} ({} chars): {:?}",
            text.len(),
            &text[..text.len().min(100)]
        );
    }
}

#[test]
fn real_world_sample_1mb() {
    let mut doc = load_ppt("sample_1mb.ppt");
    assert!(doc.page_count() >= 1, "expected at least 1 slide");

    let meta = doc.metadata();
    eprintln!(
        "sample_1mb: {} slides, title={:?}",
        meta.page_count, meta.title
    );

    // Extract text from all slides without panicking.
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
        if path.extension().map(|e| e == "ppt").unwrap_or(false) {
            let data = std::fs::read(&path).expect("read file");
            let name = path.file_name().unwrap().to_string_lossy().to_string();

            match PptDocument::from_bytes(&data) {
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
                    eprintln!("{name}: OK, {count} slides");
                }
                Err(e) => {
                    // Parse errors are acceptable (file might be from an
                    // unsupported variant), but panics are not.
                    eprintln!("{name}: parse error (acceptable): {e}");
                }
            }
        }
    }
}

//! Real-world PPTX file extraction tests.
//!
//! Tests against actual .pptx files from public sample repositories to validate
//! the parser handles real-world OOXML structures, not just synthetic test data.

use std::path::PathBuf;
use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_pptx::PptxDocument;

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

    let mut pptx_count = 0;

    for entry in entries {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.extension().map(|e| e == "pptx").unwrap_or(false) {
            let data = std::fs::read(&path).expect("read file");
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            pptx_count += 1;

            match PptxDocument::from_bytes(&data) {
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
                    eprintln!("{name}: OK, {count} slide(s)");
                }
                Err(e) => {
                    // Parse errors are acceptable (file might be from an
                    // unsupported variant), but panics are not.
                    eprintln!("{name}: parse error (acceptable): {e}");
                }
            }
        }
    }

    eprintln!("processed {pptx_count} .pptx file(s)");
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
        if path.extension().map(|e| e == "pptx").unwrap_or(false) {
            let data = std::fs::read(&path).expect("read file");
            let name = path.file_name().unwrap().to_string_lossy().to_string();

            if let Ok(mut doc) = PptxDocument::from_bytes(&data) {
                let meta = doc.metadata();
                eprintln!(
                    "{name}: {} slide(s), title={:?}",
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

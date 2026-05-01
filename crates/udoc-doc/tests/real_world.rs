//! Real-world DOC file extraction tests.
//!
//! Tests against actual .doc files from public sample repositories to validate
//! the parser handles real-world binary structures, not just synthetic test data.

use std::path::PathBuf;
use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_doc::DocDocument;

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

    let mut doc_count = 0;

    for entry in entries {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.extension().map(|e| e == "doc").unwrap_or(false) {
            let data = std::fs::read(&path).expect("read file");
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            doc_count += 1;

            match DocDocument::from_bytes(&data) {
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
                    eprintln!("{name}: OK, {count} page(s)");
                }
                Err(e) => {
                    // Parse errors are acceptable (file might be from an
                    // unsupported variant), but panics are not.
                    eprintln!("{name}: parse error (acceptable): {e}");
                }
            }
        }
    }

    eprintln!("processed {doc_count} .doc file(s)");
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
        if path.extension().map(|e| e == "doc").unwrap_or(false) {
            let data = std::fs::read(&path).expect("read file");
            let name = path.file_name().unwrap().to_string_lossy().to_string();

            if let Ok(mut doc) = DocDocument::from_bytes(&data) {
                let meta = doc.metadata();
                eprintln!(
                    "{name}: {} page(s), title={:?}",
                    meta.page_count, meta.title
                );

                if let Ok(mut page) = doc.page(0) {
                    if let Ok(text) = page.text() {
                        // Truncate at a char boundary to avoid panics on multi-byte chars.
                        let preview_end = text
                            .char_indices()
                            .take(200)
                            .last()
                            .map(|(i, c)| i + c.len_utf8())
                            .unwrap_or(0);
                        eprintln!(
                            "  text ({} chars): {:?}",
                            text.chars().count(),
                            &text[..preview_end]
                        );
                    }
                }
            }
        }
    }
}

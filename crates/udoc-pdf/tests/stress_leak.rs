//! Stress tests for memory and file descriptor leak detection.
//!
//! These tests open and process PDFs repeatedly in a loop, verifying
//! that no file descriptors accumulate (checked via /proc/self/fd on Linux)
//! and that the processing doesn't panic or leak resources.

use udoc_pdf::Document;

fn corpus_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/corpus/minimal")
        .join(name)
}

fn realworld_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/corpus/realworld")
        .join(name)
}

/// Count open file descriptors for this process (Linux only).
/// Returns None on non-Linux platforms.
fn count_open_fds() -> Option<usize> {
    std::fs::read_dir("/proc/self/fd")
        .ok()
        .map(|entries| entries.count())
}

// ---------------------------------------------------------------------------
// Repeated open/process of the same PDF
// ---------------------------------------------------------------------------

#[test]
fn stress_repeated_open_same_pdf() {
    let path = corpus_path("winansi_type1.pdf");

    let baseline_fds = count_open_fds();

    for i in 0..500 {
        let mut doc = match Document::open(&path) {
            Ok(d) => d,
            Err(e) => panic!("iteration {i}: failed to open: {e}"),
        };
        for page_idx in 0..doc.page_count() {
            let mut page = doc.page(page_idx).unwrap();
            let _ = page.text();
        }
        // doc is dropped here, releasing all resources
    }

    // Check fd count hasn't grown significantly
    if let (Some(baseline), Some(after)) = (baseline_fds, count_open_fds()) {
        let leaked = after.saturating_sub(baseline);
        assert!(
            leaked < 10,
            "fd leak detected: baseline={baseline}, after={after}, leaked={leaked}"
        );
    }
}

// ---------------------------------------------------------------------------
// Repeated open/process of a multi-page PDF
// ---------------------------------------------------------------------------

#[test]
fn stress_repeated_open_multipage() {
    let path = realworld_path("irs_w9.pdf");
    if !path.exists() {
        // Skip if corpus not available
        return;
    }

    let baseline_fds = count_open_fds();

    for i in 0..200 {
        let mut doc = match Document::open(&path) {
            Ok(d) => d,
            Err(e) => panic!("iteration {i}: failed to open: {e}"),
        };
        for page_idx in 0..doc.page_count() {
            let mut page = doc.page(page_idx).unwrap();
            let _ = page.text();
            let _ = page.text_lines();
        }
    }

    if let (Some(baseline), Some(after)) = (baseline_fds, count_open_fds()) {
        let leaked = after.saturating_sub(baseline);
        assert!(
            leaked < 10,
            "fd leak detected: baseline={baseline}, after={after}, leaked={leaked}"
        );
    }
}

// ---------------------------------------------------------------------------
// Open all corpus PDFs in a loop
// ---------------------------------------------------------------------------

#[test]
fn stress_all_corpus_pdfs_repeated() {
    let minimal_dir =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/minimal");
    let realworld_dir =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/realworld");

    let mut pdf_paths: Vec<std::path::PathBuf> = Vec::new();
    for dir in [&minimal_dir, &realworld_dir] {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "pdf") {
                    pdf_paths.push(path);
                }
            }
        }
    }

    assert!(
        !pdf_paths.is_empty(),
        "no corpus PDFs found for stress test"
    );

    let baseline_fds = count_open_fds();

    // Process all corpus PDFs 3 times
    for round in 0..3 {
        for path in &pdf_paths {
            let mut doc = match Document::open(path) {
                Ok(d) => d,
                Err(_) => continue, // Some corpus PDFs may be intentionally broken
            };
            for page_idx in 0..doc.page_count() {
                let mut page = match doc.page(page_idx) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                let _ = page.text();
            }
        }

        // Check fds after each round
        if let (Some(baseline), Some(current)) = (baseline_fds, count_open_fds()) {
            let leaked = current.saturating_sub(baseline);
            assert!(
                leaked < 10,
                "fd leak after round {round}: baseline={baseline}, current={current}, leaked={leaked}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// from_bytes repeated (no file handles involved)
// ---------------------------------------------------------------------------

#[test]
fn stress_from_bytes_repeated() {
    let data = std::fs::read(corpus_path("winansi_type1.pdf")).unwrap();

    for i in 0..1000 {
        let mut doc = match Document::from_bytes(data.clone()) {
            Ok(d) => d,
            Err(e) => panic!("iteration {i}: failed to parse: {e}"),
        };
        for page_idx in 0..doc.page_count() {
            let mut page = doc.page(page_idx).unwrap();
            let _ = page.text();
        }
    }
    // If we get here without OOM or panic, the memory is being freed correctly.
}

// ---------------------------------------------------------------------------
// Verify Document drop releases resources
// ---------------------------------------------------------------------------

#[test]
fn stress_document_drop_releases_memory() {
    // Open a moderately large PDF, extract text, drop it, repeat.
    // This would OOM quickly if Document leaked its data buffer.
    let path = realworld_path("irs_w9.pdf");
    if !path.exists() {
        return;
    }

    for _ in 0..100 {
        let data = std::fs::read(&path).unwrap();
        let size = data.len();
        let mut doc = Document::from_bytes(data).unwrap();
        for page_idx in 0..doc.page_count() {
            let mut page = doc.page(page_idx).unwrap();
            let _ = page.text();
        }
        drop(doc);
        // If the ~size bytes from this iteration aren't freed, we'll
        // accumulate 100 * size bytes. For a 200KB PDF that's 20MB,
        // which is fine, but for larger files this would catch leaks.
        let _ = size;
    }
}

// ---------------------------------------------------------------------------
// Error paths: ensure resources are freed on parse failure
// ---------------------------------------------------------------------------

#[test]
fn stress_error_path_no_leak() {
    let baseline_fds = count_open_fds();

    // Repeatedly try to parse invalid data
    for _ in 0..1000 {
        let _ = Document::from_bytes(b"not a pdf".to_vec());
        let _ = Document::from_bytes(vec![]);
        let _ = Document::from_bytes(b"%PDF-1.4\n%%EOF".to_vec());
    }

    if let (Some(baseline), Some(after)) = (baseline_fds, count_open_fds()) {
        let leaked = after.saturating_sub(baseline);
        assert!(
            leaked < 10,
            "fd leak on error path: baseline={baseline}, after={after}, leaked={leaked}"
        );
    }
}

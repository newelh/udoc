//! PPTX integration tests for the udoc facade.
//!
//! Corpus files live in `tests/corpus/pptx/`. Since they may be created by
//! another agent in parallel, each test skips gracefully when the expected
//! file is missing.

use std::path::PathBuf;

fn corpus_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/pptx")
        .join(name)
}

/// Return the path if the file exists, or print a skip message and return
/// None. Tests should early-return on None.
fn require_corpus(name: &str) -> Option<PathBuf> {
    let path = corpus_path(name);
    if path.exists() {
        Some(path)
    } else {
        eprintln!(
            "SKIP: corpus file '{}' not found at {}",
            name,
            path.display()
        );
        None
    }
}

// ---------------------------------------------------------------------------
// extract() one-shot
// ---------------------------------------------------------------------------

#[test]
fn extract_pptx_basic() {
    let Some(path) = require_corpus("simple_text.pptx") else {
        return;
    };
    let doc = udoc::extract(&path).expect("extract should succeed");
    assert!(
        doc.metadata.page_count > 0,
        "PPTX should have at least one slide"
    );
    assert!(!doc.content.is_empty(), "document should have content");

    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !all_text.trim().is_empty(),
        "extracted text should not be empty, got: {all_text}"
    );
}

#[test]
fn extract_pptx_multipage() {
    let Some(path) = require_corpus("multipage.pptx") else {
        return;
    };
    let doc = udoc::extract(&path).expect("extract should succeed");
    assert!(
        doc.metadata.page_count >= 2,
        "multipage PPTX should have at least 2 slides, got {}",
        doc.metadata.page_count
    );

    // Verify per-slide text: use Extractor streaming to get individual pages.
    let mut ext = udoc::Extractor::open(&path).expect("opening multipage PPTX should succeed");
    for i in 0..ext.page_count() {
        let text = ext
            .page_text(i)
            .unwrap_or_else(|e| panic!("page_text({i}) failed: {e}"));
        // Each slide should produce some text (even if just a title placeholder).
        // We don't assert non-empty here because some slides may be intentionally blank.
        let _ = text;
    }
}

// ---------------------------------------------------------------------------
// extract_bytes()
// ---------------------------------------------------------------------------

#[test]
fn extract_pptx_from_bytes() {
    let Some(path) = require_corpus("simple_text.pptx") else {
        return;
    };
    let data = std::fs::read(&path).expect("reading corpus file");
    let doc = udoc::extract_bytes(&data).expect("extract_bytes should succeed");
    assert!(
        doc.metadata.page_count > 0,
        "PPTX from bytes should have at least one slide"
    );
    assert!(
        !doc.content.is_empty(),
        "document from bytes should have content"
    );
}

// ---------------------------------------------------------------------------
// Extractor streaming
// ---------------------------------------------------------------------------

#[test]
fn extractor_pptx_page_text() {
    let Some(path) = require_corpus("simple_text.pptx") else {
        return;
    };
    let mut ext = udoc::Extractor::open(&path).expect("open should succeed");
    assert!(ext.page_count() > 0, "should have at least one slide");
    assert_eq!(ext.format(), udoc::Format::Pptx);

    let text = ext.page_text(0).expect("page_text(0) should succeed");
    assert!(
        !text.trim().is_empty(),
        "first slide text should not be empty, got: {text}"
    );
}

#[test]
fn extractor_pptx_into_document() {
    let Some(path) = require_corpus("simple_text.pptx") else {
        return;
    };
    let ext = udoc::Extractor::open(&path).expect("open should succeed");
    let doc = ext.into_document().expect("into_document should succeed");
    assert!(
        !doc.content.is_empty(),
        "materialized document should have content"
    );
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

#[test]
fn extract_pptx_tables() {
    let Some(path) = require_corpus("table.pptx") else {
        return;
    };
    let doc = udoc::extract(&path).expect("extract should succeed");

    let has_table = doc
        .content
        .iter()
        .any(|b| matches!(b, udoc::Block::Table { .. }));
    assert!(has_table, "PPTX with tables should produce Table blocks");

    let table_text: String = doc
        .content
        .iter()
        .filter(|b| matches!(b, udoc::Block::Table { .. }))
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !table_text.trim().is_empty(),
        "table text should not be empty, got: {table_text}"
    );
}

// ---------------------------------------------------------------------------
// Speaker notes
// ---------------------------------------------------------------------------

#[test]
fn extract_pptx_notes() {
    let Some(path) = require_corpus("speaker_notes.pptx") else {
        return;
    };
    let doc = udoc::extract(&path).expect("extract should succeed");

    // Speaker notes should appear as Section blocks with role Notes.
    let has_notes_section = doc.content.iter().any(|b| {
        matches!(
            b,
            udoc::Block::Section {
                role: Some(udoc::SectionRole::Notes),
                ..
            }
        )
    });
    assert!(
        has_notes_section,
        "PPTX with speaker notes should produce a Section block with role Notes"
    );

    // Verify the notes section contains text.
    let notes_text: String = doc
        .content
        .iter()
        .filter(|b| {
            matches!(
                b,
                udoc::Block::Section {
                    role: Some(udoc::SectionRole::Notes),
                    ..
                }
            )
        })
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !notes_text.trim().is_empty(),
        "notes section text should not be empty, got: {notes_text}"
    );
}

// ---------------------------------------------------------------------------
// Format detection
// ---------------------------------------------------------------------------

#[test]
fn extract_pptx_format_detection() {
    let Some(path) = require_corpus("simple_text.pptx") else {
        return;
    };
    let ext = udoc::Extractor::open(&path).expect("open should succeed");
    assert_eq!(
        ext.format(),
        udoc::Format::Pptx,
        "PPTX file should be detected as Format::Pptx"
    );
}

// ---------------------------------------------------------------------------
// content_only config: no presentation layer
// ---------------------------------------------------------------------------

#[test]
fn extract_pptx_no_presentation() {
    let Some(path) = require_corpus("simple_text.pptx") else {
        return;
    };
    let config = udoc::Config::new().layers(udoc::LayerConfig::content_only());
    let doc = udoc::extract_with(&path, config).expect("extract_with should succeed");
    assert!(
        doc.presentation.is_none(),
        "content_only config should strip the presentation layer"
    );
    // Content spine should still be populated.
    assert!(
        !doc.content.is_empty(),
        "content should be present even with content_only config"
    );
}

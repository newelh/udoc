//! Markdown integration tests for the udoc facade.

use std::path::PathBuf;

fn corpus_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../udoc-markdown/tests/corpus")
        .join(name)
}

// ---------------------------------------------------------------------------
// extract() one-shot
// ---------------------------------------------------------------------------

#[test]
fn extract_md_basic() {
    let doc = udoc::extract(corpus_path("basic.md")).expect("extract should succeed");
    assert!(!doc.content.is_empty(), "document should have content");
    assert_eq!(doc.metadata.page_count, 1);

    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        all_text.contains("Heading 1"),
        "should contain 'Heading 1', got: {all_text}"
    );
    assert!(
        all_text.contains("first paragraph"),
        "should contain 'first paragraph', got: {all_text}"
    );
}

#[test]
fn extract_md_headings() {
    let doc = udoc::extract(corpus_path("basic.md")).expect("extract should succeed");
    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    // All 6 heading levels should be present as text.
    for i in 1..=6 {
        assert!(
            all_text.contains(&format!("Heading {i}")),
            "should contain 'Heading {i}', got: {all_text}"
        );
    }
}

// ---------------------------------------------------------------------------
// extract_bytes()
// ---------------------------------------------------------------------------

#[test]
fn extract_bytes_md() {
    let data = std::fs::read(corpus_path("basic.md")).expect("read file");
    // Markdown has no magic bytes, so we must specify the format explicitly.
    let mut config = udoc::Config::default();
    config.format = Some(udoc::Format::Md);
    let doc = udoc::extract_bytes_with(&data, config).expect("extract_bytes should succeed");
    assert!(!doc.content.is_empty());
}

#[test]
fn extract_bytes_md_inline() {
    let data = b"Hello from markdown bytes";
    let mut config = udoc::Config::default();
    config.format = Some(udoc::Format::Md);
    let doc = udoc::extract_bytes_with(data, config).expect("extract_bytes should succeed");
    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(all_text, "Hello from markdown bytes");
}

// ---------------------------------------------------------------------------
// Extractor streaming
// ---------------------------------------------------------------------------

#[test]
fn extractor_md_page_text() {
    let mut ext = udoc::Extractor::open(corpus_path("basic.md")).expect("open should succeed");
    assert_eq!(ext.page_count(), 1);
    assert_eq!(ext.format(), udoc::Format::Md);

    let text = ext.page_text(0).expect("page_text should succeed");
    assert!(text.contains("Heading 1"), "got: {text}");
}

#[test]
fn extractor_md_into_document() {
    let ext = udoc::Extractor::open(corpus_path("basic.md")).expect("open should succeed");
    let doc = ext.into_document().expect("into_document should succeed");
    assert!(!doc.content.is_empty());
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

#[test]
fn extract_md_with_tables() {
    let doc = udoc::extract(corpus_path("tables.md")).expect("extract should succeed");

    let has_table = doc
        .content
        .iter()
        .any(|b| matches!(b, udoc::Block::Table { .. }));
    assert!(
        has_table,
        "Markdown with tables should produce Table blocks"
    );

    let table_text: String = doc
        .content
        .iter()
        .filter(|b| matches!(b, udoc::Block::Table { .. }))
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        table_text.contains("Name"),
        "table should contain 'Name', got: {table_text}"
    );
    assert!(
        table_text.contains("Alice"),
        "table should contain 'Alice', got: {table_text}"
    );
}

// ---------------------------------------------------------------------------
// Code blocks
// ---------------------------------------------------------------------------

#[test]
fn extract_md_code_blocks() {
    let doc = udoc::extract(corpus_path("code_blocks.md")).expect("extract should succeed");
    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        all_text.contains("println!"),
        "should contain code content, got: {all_text}"
    );
}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

#[test]
fn extract_md_lists() {
    let doc = udoc::extract(corpus_path("lists.md")).expect("extract should succeed");
    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        all_text.contains("Item one"),
        "should contain 'Item one', got: {all_text}"
    );
    assert!(
        all_text.contains("First"),
        "should contain 'First', got: {all_text}"
    );
}

// ---------------------------------------------------------------------------
// No presentation layer for Markdown (flow format, no geometry)
// ---------------------------------------------------------------------------

#[test]
fn extract_md_no_presentation() {
    let doc = udoc::extract(corpus_path("basic.md")).expect("extract should succeed");
    assert!(
        doc.presentation.is_none(),
        "Markdown should not produce a presentation layer"
    );
}

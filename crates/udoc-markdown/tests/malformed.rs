//! Tests for Markdown malformed file recovery.
//!
//! Each test verifies that a malformed Markdown file can be parsed without
//! panicking and produces at least partial text output.

use udoc_core::backend::{FormatBackend, PageExtractor};

fn corpus_malformed_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/malformed")
}

#[test]
fn unclosed_code_fence_recovers() {
    let path = corpus_malformed_dir().join("unclosed_code_fence.md");
    let mut doc = udoc_markdown::MdDocument::open(&path).expect("should parse without panic");
    assert_eq!(doc.page_count(), 1);
    assert!(
        !doc.warnings().is_empty(),
        "malformed file should produce at least one warning"
    );
    let mut page = doc.page(0).expect("should get page");
    let text = page.text().expect("should extract text");
    assert!(!text.is_empty(), "should produce partial text");
    assert!(
        text.contains("Some text before"),
        "should contain 'Some text before', got: {text}"
    );
}

#[test]
fn unmatched_emphasis_recovers() {
    let path = corpus_malformed_dir().join("unmatched_emphasis.md");
    let mut doc = udoc_markdown::MdDocument::open(&path).expect("should parse without panic");
    assert_eq!(doc.page_count(), 1);
    let mut page = doc.page(0).expect("should get page");
    let text = page.text().expect("should extract text");
    assert!(!text.is_empty(), "should produce partial text");
    assert!(
        text.contains("Normal text here"),
        "should contain 'Normal text here', got: {text}"
    );
}

#[test]
fn broken_links_recovers() {
    let path = corpus_malformed_dir().join("broken_links.md");
    let mut doc = udoc_markdown::MdDocument::open(&path).expect("should parse without panic");
    assert_eq!(doc.page_count(), 1);
    let mut page = doc.page(0).expect("should get page");
    let text = page.text().expect("should extract text");
    assert!(!text.is_empty(), "should produce partial text");
    assert!(
        text.contains("Normal text after"),
        "should contain 'Normal text after', got: {text}"
    );
}

#[test]
fn deeply_nested_recovers() {
    let path = corpus_malformed_dir().join("deeply_nested.md");
    let mut doc = udoc_markdown::MdDocument::open(&path).expect("should parse without panic");
    assert_eq!(doc.page_count(), 1);
    let mut page = doc.page(0).expect("should get page");
    let text = page.text().expect("should extract text");
    assert!(!text.is_empty(), "should produce partial text");
    assert!(
        text.contains("Normal text after"),
        "should contain 'Normal text after', got: {text}"
    );
}

#[test]
fn mixed_line_endings_recovers() {
    let path = corpus_malformed_dir().join("mixed_line_endings.md");
    let mut doc = udoc_markdown::MdDocument::open(&path).expect("should parse without panic");
    assert_eq!(doc.page_count(), 1);
    let mut page = doc.page(0).expect("should get page");
    let text = page.text().expect("should extract text");
    assert!(!text.is_empty(), "should produce partial text");
    assert!(
        text.contains("Line one"),
        "should contain 'Line one', got: {text}"
    );
    assert!(
        text.contains("Line four"),
        "should contain 'Line four', got: {text}"
    );
}

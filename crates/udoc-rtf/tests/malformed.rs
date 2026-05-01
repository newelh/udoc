//! Tests for RTF malformed file recovery.
//!
//! Each test verifies that a malformed RTF file can be parsed without
//! panicking and produces at least partial text output.

use udoc_core::backend::{FormatBackend, PageExtractor};

fn corpus_malformed_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/malformed")
}

#[test]
fn unclosed_groups_recovers() {
    let path = corpus_malformed_dir().join("unclosed_groups.rtf");
    let mut doc = udoc_rtf::RtfDocument::open(&path).expect("should parse without panic");
    assert_eq!(doc.page_count(), 1);
    assert!(
        !doc.warnings().is_empty(),
        "malformed file should produce at least one warning"
    );
    let mut page = doc.page(0).expect("should get page");
    let text = page.text().expect("should extract text");
    assert!(!text.is_empty(), "should produce partial text");
    assert!(
        text.contains("Some text"),
        "should contain 'Some text', got: {text}"
    );
}

#[test]
fn bad_control_word_recovers() {
    let path = corpus_malformed_dir().join("bad_control_word.rtf");
    let mut doc = udoc_rtf::RtfDocument::open(&path).expect("should parse without panic");
    assert_eq!(doc.page_count(), 1);
    assert!(
        !doc.warnings().is_empty(),
        "malformed file should produce at least one warning"
    );
    let mut page = doc.page(0).expect("should get page");
    let text = page.text().expect("should extract text");
    assert!(!text.is_empty(), "should produce partial text");
    assert!(
        text.contains("Normal text"),
        "should contain 'Normal text', got: {text}"
    );
    assert!(
        text.contains("Final text"),
        "should contain 'Final text', got: {text}"
    );
}

#[test]
fn truncated_unicode_recovers() {
    let path = corpus_malformed_dir().join("truncated_unicode.rtf");
    let mut doc = udoc_rtf::RtfDocument::open(&path).expect("should parse without panic");
    assert_eq!(doc.page_count(), 1);
    assert!(
        !doc.warnings().is_empty(),
        "malformed file should produce at least one warning"
    );
    let mut page = doc.page(0).expect("should get page");
    let text = page.text().expect("should extract text");
    assert!(!text.is_empty(), "should produce partial text");
    assert!(
        text.contains("Hello"),
        "should contain 'Hello', got: {text}"
    );
}

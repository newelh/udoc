//! Golden file tests for RTF text extraction.
//!
//! Run with `BLESS=1 cargo test -p udoc-rtf --test golden_files` to update expected files.

use std::path::PathBuf;
use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_core::test_harness::assert_golden;

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn extract_text(filename: &str) -> String {
    let path = corpus_dir().join(filename);
    let mut doc = udoc_rtf::RtfDocument::open(&path)
        .unwrap_or_else(|e| panic!("failed to open {filename}: {e}"));
    let mut page = doc.page(0).expect("page 0");
    page.text().expect("text()")
}

#[test]
fn golden_basic_text() {
    let text = extract_text("basic.rtf");
    assert_golden("basic_text", &text, &golden_dir());
}

#[test]
fn golden_formatting_text() {
    let text = extract_text("formatting.rtf");
    assert_golden("formatting_text", &text, &golden_dir());
}

#[test]
fn golden_unicode_text() {
    let text = extract_text("unicode.rtf");
    assert_golden("unicode_text", &text, &golden_dir());
}

#[test]
fn golden_special_chars_text() {
    let text = extract_text("special_chars.rtf");
    assert_golden("special_chars_text", &text, &golden_dir());
}

#[test]
fn golden_table_text() {
    let text = extract_text("table_basic.rtf");
    assert_golden("table_text", &text, &golden_dir());
}

#[test]
fn golden_hidden_text() {
    let text = extract_text("hidden_text.rtf");
    assert_golden("hidden_text", &text, &golden_dir());
}

#[test]
fn golden_symbol_charset_text() {
    let text = extract_text("symbol_charset.rtf");
    assert_golden("symbol_charset_text", &text, &golden_dir());
}

#[test]
fn golden_metadata() {
    let path = corpus_dir().join("metadata.rtf");
    let doc = udoc_rtf::RtfDocument::open(&path).expect("failed to open metadata.rtf");
    let meta = doc.metadata();

    let output = format!(
        "title: {}\nauthor: {}\nsubject: {}\npage_count: {}",
        meta.title.as_deref().unwrap_or("(none)"),
        meta.author.as_deref().unwrap_or("(none)"),
        meta.subject.as_deref().unwrap_or("(none)"),
        meta.page_count,
    );
    assert_golden("metadata", &output, &golden_dir());
}

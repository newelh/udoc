//! Golden file tests for Markdown text extraction.
//!
//! Run with `BLESS=1 cargo test -p udoc-markdown --test golden_files` to update expected files.

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
    let mut doc = udoc_markdown::MdDocument::open(&path)
        .unwrap_or_else(|e| panic!("failed to open {filename}: {e}"));
    let mut page = doc.page(0).expect("page 0");
    page.text().expect("text()")
}

#[test]
fn golden_basic_text() {
    let text = extract_text("basic.md");
    assert_golden("basic_text", &text, &golden_dir());
}

#[test]
fn golden_formatting_text() {
    let text = extract_text("formatting.md");
    assert_golden("formatting_text", &text, &golden_dir());
}

#[test]
fn golden_links_text() {
    let text = extract_text("links.md");
    assert_golden("links_text", &text, &golden_dir());
}

#[test]
fn golden_code_blocks_text() {
    let text = extract_text("code_blocks.md");
    assert_golden("code_blocks_text", &text, &golden_dir());
}

#[test]
fn golden_lists_text() {
    let text = extract_text("lists.md");
    assert_golden("lists_text", &text, &golden_dir());
}

#[test]
fn golden_tables_text() {
    let text = extract_text("tables.md");
    assert_golden("tables_text", &text, &golden_dir());
}

#[test]
fn golden_complex_text() {
    let text = extract_text("complex.md");
    assert_golden("complex_text", &text, &golden_dir());
}

#[test]
fn golden_blockquotes_text() {
    let text = extract_text("blockquotes.md");
    assert_golden("blockquotes_text", &text, &golden_dir());
}

#[test]
fn golden_thematic_breaks_text() {
    let text = extract_text("thematic_breaks.md");
    assert_golden("thematic_breaks_text", &text, &golden_dir());
}

#[test]
fn golden_edge_cases_text() {
    let text = extract_text("edge_cases.md");
    assert_golden("edge_cases_text", &text, &golden_dir());
}

//! Golden file tests for DOC text extraction.
//!
//! Run with `BLESS=1 cargo test -p udoc-doc --test golden_files` to update expected files.

use std::path::PathBuf;
use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_core::test_harness::assert_golden;
use udoc_doc::test_util::{
    build_clx_with_offsets, build_fib, build_minimal_doc, build_minimal_doc_with_all_stories,
    build_minimal_doc_with_bold_italic, build_minimal_doc_with_notes,
};
use udoc_doc::DocDocument;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

// ---------------------------------------------------------------------------
// Helper: build a DOC file with UTF-16LE (uncompressed) text
// ---------------------------------------------------------------------------

fn build_utf16_doc(text: &str) -> Vec<u8> {
    let text_bytes: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let ccp_text = text.encode_utf16().count() as u32;

    // Build placeholder FIB to measure its size.
    let placeholder_fib = build_fib(ccp_text, 0, 0, false);
    let fib_size = placeholder_fib.len();

    // Text goes right after the FIB.
    let text_offset = fib_size as u32;

    // Build CLX: single uncompressed piece (is_compressed = false).
    let clx = build_clx_with_offsets(&[(0, ccp_text, text_offset, false)]);
    let fc_clx = 0u32;
    let lcb_clx = clx.len() as u32;

    let real_fib = build_fib(ccp_text, fc_clx, lcb_clx, false);

    let mut word_doc = real_fib;
    word_doc.extend_from_slice(&text_bytes);

    let table_stream = clx;

    udoc_containers::test_util::build_cfb(&[("WordDocument", &word_doc), ("0Table", &table_stream)])
}

// ---------------------------------------------------------------------------
// Helper: build a DOC with two pieces (mixed encoding)
// ---------------------------------------------------------------------------

fn build_two_piece_doc(text1: &str, text2: &str) -> Vec<u8> {
    let text1_bytes = text1.as_bytes();
    let text2_bytes = text2.as_bytes();
    let ccp1 = text1_bytes.len() as u32;
    let ccp2 = text2_bytes.len() as u32;
    let ccp_text = ccp1 + ccp2;

    // Placeholder FIB to get size.
    let placeholder_fib = build_fib(ccp_text, 0, 0, false);
    let fib_size = placeholder_fib.len();

    let offset1 = fib_size as u32;
    let offset2 = offset1 + text1_bytes.len() as u32;

    // Two compressed pieces.
    let clx = build_clx_with_offsets(&[(0, ccp1, offset1, true), (ccp1, ccp_text, offset2, true)]);
    let fc_clx = 0u32;
    let lcb_clx = clx.len() as u32;

    let real_fib = build_fib(ccp_text, fc_clx, lcb_clx, false);

    let mut word_doc = real_fib;
    word_doc.extend_from_slice(text1_bytes);
    word_doc.extend_from_slice(text2_bytes);

    let table_stream = clx;

    udoc_containers::test_util::build_cfb(&[("WordDocument", &word_doc), ("0Table", &table_stream)])
}

// ---------------------------------------------------------------------------
// Golden 1: single paragraph
// ---------------------------------------------------------------------------

#[test]
fn golden_doc_single_paragraph() {
    let data = build_minimal_doc("Hello World");
    let mut doc = DocDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("doc_single_paragraph", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 2: two paragraphs
// ---------------------------------------------------------------------------

#[test]
fn golden_doc_two_paragraphs() {
    let data = build_minimal_doc("First paragraph\rSecond paragraph");
    let mut doc = DocDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("doc_two_paragraphs", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 3: unicode text (UTF-16LE uncompressed piece)
// ---------------------------------------------------------------------------

#[test]
fn golden_doc_unicode_text() {
    let data = build_utf16_doc("Caf\u{00e9} r\u{00e9}sum\u{00e9}");
    let mut doc = DocDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("doc_unicode_text", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 4: empty document
// ---------------------------------------------------------------------------

#[test]
fn golden_doc_empty_document() {
    let data = build_minimal_doc("");
    let mut doc = DocDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("doc_empty_document", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 5: long document (10+ paragraphs)
// ---------------------------------------------------------------------------

#[test]
fn golden_doc_long_document() {
    let paragraphs: Vec<&str> = vec![
        "Chapter 1: Introduction",
        "The quick brown fox jumps over the lazy dog.",
        "This document tests multi-paragraph extraction.",
        "Line four with some additional text content.",
        "Paragraph five is here now.",
        "Six paragraphs in and still going strong.",
        "Lucky number seven.",
        "Eight is great.",
        "Nine lives of a cat.",
        "Ten: the final paragraph in this test.",
        "Bonus: paragraph eleven for good measure.",
    ];
    let input = paragraphs.join("\r");
    let data = build_minimal_doc(&input);
    let mut doc = DocDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("doc_long_document", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 6: special characters (page break, column break)
// ---------------------------------------------------------------------------

#[test]
fn golden_doc_special_chars() {
    // 0x0C = page break, 0x0E = column break in DOC special character semantics
    let data = build_minimal_doc("Before break\x0cAfter page break\rNext paragraph");
    let mut doc = DocDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("doc_special_chars", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 7: cell marks (table content via \x07)
// ---------------------------------------------------------------------------

#[test]
fn golden_doc_cell_marks() {
    // Cell marks (\x07) delimit table cells in DOC. The text layer strips
    // them during paragraph extraction.
    let data = build_minimal_doc("Cell A\x07Cell B\x07\rRow end\r");
    let mut doc = DocDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("doc_cell_marks", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 8: multiline text with varied content
// ---------------------------------------------------------------------------

#[test]
fn golden_doc_multiline() {
    let data =
        build_minimal_doc("Title of Document\rAuthor: Test Suite\r\rBody text begins here.\rEnd.");
    let mut doc = DocDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("doc_multiline", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 9: two-piece document (multi-piece assembly)
// ---------------------------------------------------------------------------

#[test]
fn golden_doc_two_pieces() {
    let data = build_two_piece_doc("First piece. ", "Second piece.");
    let mut doc = DocDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("doc_two_pieces", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 10: text_lines extraction
// ---------------------------------------------------------------------------

#[test]
fn golden_doc_text_lines() {
    let data = build_minimal_doc("Heading\rBody paragraph one.\rBody paragraph two.");
    let mut doc = DocDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let lines = page.text_lines().expect("text_lines()");

    assert_eq!(lines.len(), 3, "expected 3 text lines");
    assert_eq!(lines[0].spans[0].text, "Heading");
    assert_eq!(lines[1].spans[0].text, "Body paragraph one.");
    assert_eq!(lines[2].spans[0].text, "Body paragraph two.");
}

// ---------------------------------------------------------------------------
// Golden 11: raw_spans extraction
// ---------------------------------------------------------------------------

#[test]
fn golden_doc_raw_spans() {
    let data = build_minimal_doc("Span A\rSpan B");
    let mut doc = DocDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let spans = page.raw_spans().expect("raw_spans()");

    assert_eq!(spans.len(), 2, "expected 2 raw spans");
    assert_eq!(spans[0].text, "Span A");
    assert_eq!(spans[1].text, "Span B");
}

// ---------------------------------------------------------------------------
// Golden 12: metadata check
// ---------------------------------------------------------------------------

#[test]
fn golden_doc_metadata() {
    let data = build_minimal_doc("metadata test");
    let doc = DocDocument::from_bytes(&data).expect("from_bytes");
    let meta = doc.metadata();
    assert_eq!(meta.page_count, 1, "DOC always reports 1 page");
}

// ---------------------------------------------------------------------------
// Golden 13: 1Table stream variant (table_stream_bit = true)
// ---------------------------------------------------------------------------

#[test]
fn golden_doc_1table_stream() {
    let text = "Using 1Table stream";
    let text_bytes = text.as_bytes();
    let ccp_text = text_bytes.len() as u32;

    let placeholder_fib = build_fib(ccp_text, 0, 0, true);
    let fib_size = placeholder_fib.len();
    let text_offset = fib_size as u32;

    let clx = build_clx_with_offsets(&[(0, ccp_text, text_offset, true)]);
    let fc_clx = 0u32;
    let lcb_clx = clx.len() as u32;

    let real_fib = build_fib(ccp_text, fc_clx, lcb_clx, true);
    let mut word_doc = real_fib;
    word_doc.extend_from_slice(text_bytes);

    let table_stream = clx;

    let data = udoc_containers::test_util::build_cfb(&[
        ("WordDocument", &word_doc),
        ("1Table", &table_stream),
    ]);

    let mut doc = DocDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let actual = page.text().expect("text()");
    assert_golden("doc_1table_stream", &actual, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 14: footnotes
// ---------------------------------------------------------------------------

#[test]
fn golden_doc_footnotes() {
    // Body text "Main body text\r", footnote "Footnote content\r".
    let data = build_minimal_doc_with_notes("Main body text\r", "Footnote content\r", "");
    let mut doc = DocDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("doc_footnotes", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 15: endnotes
// ---------------------------------------------------------------------------

#[test]
fn golden_doc_endnotes() {
    // Body text "Main document body\r", endnote "Endnote reference text\r".
    let data = build_minimal_doc_with_notes("Main document body\r", "", "Endnote reference text\r");
    let mut doc = DocDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("doc_endnotes", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 16: headers and footers
// ---------------------------------------------------------------------------

#[test]
fn golden_doc_headers_footers() {
    // Body "Document body\r", header "Page Header\r", footer "Page Footer\r".
    let data =
        build_minimal_doc_with_all_stories("Document body\r", "", "Page Header\rPage Footer\r", "");
    let mut doc = DocDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("doc_headers_footers", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 17: bold and italic formatting via CharacterProperties
// ---------------------------------------------------------------------------

#[test]
fn golden_doc_bold_italic() {
    // Text: "Bold text\rItalic text\rPlain text"
    // Run breakdown (character counts include the '\r' separators):
    //   "Bold text" = 9 chars (bold)
    //   "\r" = 1 char (plain separator, but part of the body CP space)
    //   "Italic text" = 11 chars (italic)
    //   "\r" = 1 char
    //   "Plain text" = 10 chars (plain)
    // Total = 32 chars, stored as a single piece.
    //
    // We describe runs as character ranges:
    //   run 0: 9 chars, bold
    //   run 1: 1 char, plain (paragraph mark after "Bold text")
    //   run 2: 11 chars, italic
    //   run 3: 1 char, plain
    //   run 4: 10 chars, plain
    let text = "Bold text\rItalic text\rPlain text";
    let data = build_minimal_doc_with_bold_italic(
        text,
        &[
            (9, true, false),
            (1, false, false),
            (11, false, true),
            (1, false, false),
            (10, false, false),
        ],
    );
    let mut doc = DocDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");

    // Verify text() golden output (plain text, no formatting markers).
    let text_out = page.text().expect("text()");
    assert_golden("doc_bold_italic", &text_out, &golden_dir());

    // Also verify that bold/italic flags are set correctly on spans.
    let mut page2 = doc.page(0).expect("page 0 again");
    let spans = page2.raw_spans().expect("raw_spans()");

    // Find the bold span (text contains "Bold text").
    let bold_span = spans.iter().find(|s| s.text.contains("Bold text"));
    assert!(bold_span.is_some(), "expected span with 'Bold text'");
    assert!(bold_span.unwrap().is_bold, "Bold text span should be bold");

    // Find the italic span (text contains "Italic text").
    let italic_span = spans.iter().find(|s| s.text.contains("Italic text"));
    assert!(italic_span.is_some(), "expected span with 'Italic text'");
    assert!(
        italic_span.unwrap().is_italic,
        "Italic text span should be italic"
    );

    // Find the plain span (text contains "Plain text").
    let plain_span = spans.iter().find(|s| s.text.contains("Plain text"));
    assert!(plain_span.is_some(), "expected span with 'Plain text'");
    assert!(
        !plain_span.unwrap().is_bold,
        "Plain text span should not be bold"
    );
    assert!(
        !plain_span.unwrap().is_italic,
        "Plain text span should not be italic"
    );
}

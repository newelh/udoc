//! Golden file tests for PPT text extraction.
//!
//! Run with `BLESS=1 cargo test -p udoc-ppt --test golden_files` to update expected files.

use std::path::PathBuf;
use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_core::test_harness::assert_golden;
use udoc_ppt::test_util::*;
use udoc_ppt::PptDocument;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

// ---------------------------------------------------------------------------
// Golden 1: single slide with title only
// ---------------------------------------------------------------------------

fn build_ppt_title_only() -> Vec<u8> {
    let mut slwt = Vec::new();
    slwt.extend_from_slice(&build_slide_persist_atom(1));
    slwt.extend_from_slice(&build_text_header_atom(0)); // Title
    slwt.extend_from_slice(&build_text_chars_atom("Quarterly Review 2025"));
    build_ppt_cfb(&slwt, &[])
}

#[test]
fn golden_ppt_title_only() {
    let data = build_ppt_title_only();
    let mut doc = PptDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("ppt_title_only", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 2: single slide with title + body
// ---------------------------------------------------------------------------

fn build_ppt_title_body() -> Vec<u8> {
    let mut slwt = Vec::new();
    slwt.extend_from_slice(&build_slide_persist_atom(1));
    slwt.extend_from_slice(&build_text_header_atom(0)); // Title
    slwt.extend_from_slice(&build_text_chars_atom("Project Status"));
    slwt.extend_from_slice(&build_text_header_atom(1)); // Body
    slwt.extend_from_slice(&build_text_chars_atom("All milestones on track"));
    slwt.extend_from_slice(&build_text_header_atom(1)); // Body
    slwt.extend_from_slice(&build_text_chars_atom("Budget within 5% of plan"));
    build_ppt_cfb(&slwt, &[])
}

#[test]
fn golden_ppt_title_body() {
    let data = build_ppt_title_body();
    let mut doc = PptDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("ppt_title_body", &text, &golden_dir());
}

#[test]
fn ppt_title_body_text_lines() {
    let data = build_ppt_title_body();
    let mut doc = PptDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let lines = page.text_lines().expect("text_lines()");

    assert_eq!(lines.len(), 3, "expected 3 text lines, got {}", lines.len());
    assert!(
        lines[0].spans[0].is_bold,
        "title should be bold in text_lines()"
    );
    assert!(
        !lines[1].spans[0].is_bold,
        "body should not be bold in text_lines()"
    );
}

// ---------------------------------------------------------------------------
// Golden 3: two slides with different text (ordering)
// ---------------------------------------------------------------------------

fn build_ppt_two_slides() -> Vec<u8> {
    let mut slwt = Vec::new();

    // Slide 1
    slwt.extend_from_slice(&build_slide_persist_atom(1));
    slwt.extend_from_slice(&build_text_header_atom(0)); // Title
    slwt.extend_from_slice(&build_text_chars_atom("Introduction"));
    slwt.extend_from_slice(&build_text_header_atom(1)); // Body
    slwt.extend_from_slice(&build_text_chars_atom("Welcome to the presentation"));

    // Slide 2
    slwt.extend_from_slice(&build_slide_persist_atom(2));
    slwt.extend_from_slice(&build_text_header_atom(0)); // Title
    slwt.extend_from_slice(&build_text_chars_atom("Agenda"));
    slwt.extend_from_slice(&build_text_header_atom(1)); // Body
    slwt.extend_from_slice(&build_text_chars_atom("Review Q3 numbers"));
    slwt.extend_from_slice(&build_text_header_atom(1)); // Body
    slwt.extend_from_slice(&build_text_chars_atom("Discuss Q4 plan"));

    build_ppt_cfb(&slwt, &[])
}

#[test]
fn golden_ppt_two_slides_0() {
    let data = build_ppt_two_slides();
    let mut doc = PptDocument::from_bytes(&data).expect("from_bytes");
    assert_eq!(doc.page_count(), 2, "expected 2 slides");

    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("ppt_two_slides_0", &text, &golden_dir());
}

#[test]
fn golden_ppt_two_slides_1() {
    let data = build_ppt_two_slides();
    let mut doc = PptDocument::from_bytes(&data).expect("from_bytes");

    let mut page = doc.page(1).expect("page 1");
    let text = page.text().expect("text()");
    assert_golden("ppt_two_slides_1", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 4: slide with speaker notes
// ---------------------------------------------------------------------------

fn build_ppt_with_notes() -> Vec<u8> {
    let mut slide_slwt = Vec::new();
    slide_slwt.extend_from_slice(&build_slide_persist_atom(1));
    slide_slwt.extend_from_slice(&build_text_header_atom(0)); // Title
    slide_slwt.extend_from_slice(&build_text_chars_atom("Financial Results"));
    slide_slwt.extend_from_slice(&build_text_header_atom(1)); // Body
    slide_slwt.extend_from_slice(&build_text_chars_atom("Revenue up 15% YoY"));

    let mut notes_slwt = Vec::new();
    notes_slwt.extend_from_slice(&build_slide_persist_atom(100));
    notes_slwt.extend_from_slice(&build_text_header_atom(2)); // Notes
    notes_slwt.extend_from_slice(&build_text_chars_atom(
        "Mention the APAC growth specifically",
    ));
    notes_slwt.extend_from_slice(&build_text_header_atom(2)); // Notes
    notes_slwt.extend_from_slice(&build_text_chars_atom(
        "Prepare for questions about margins",
    ));

    build_ppt_cfb(&slide_slwt, &notes_slwt)
}

#[test]
fn golden_ppt_with_notes() {
    let data = build_ppt_with_notes();
    let mut doc = PptDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("ppt_with_notes", &text, &golden_dir());
}

#[test]
fn ppt_with_notes_contains_notes_marker() {
    let data = build_ppt_with_notes();
    let mut doc = PptDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");

    assert!(text.contains("[Notes]"), "expected [Notes] marker in text");
    assert!(
        text.contains("APAC growth"),
        "expected notes content in text"
    );
}

// ---------------------------------------------------------------------------
// Golden 5: empty presentation (0 text in SLWT)
// ---------------------------------------------------------------------------

fn build_ppt_empty() -> Vec<u8> {
    // Empty SLWT but still a valid DocumentContainer.
    build_ppt_cfb(&[], &[])
}

#[test]
fn golden_ppt_empty() {
    let data = build_ppt_empty();
    let doc = PptDocument::from_bytes(&data).expect("from_bytes");
    assert_eq!(doc.page_count(), 0, "empty presentation has 0 slides");
}

// ---------------------------------------------------------------------------
// Golden 6: slide with subtitle and center title text types
// ---------------------------------------------------------------------------

fn build_ppt_subtitle_center_title() -> Vec<u8> {
    let mut slwt = Vec::new();
    slwt.extend_from_slice(&build_slide_persist_atom(1));
    slwt.extend_from_slice(&build_text_header_atom(5)); // CenterTitle
    slwt.extend_from_slice(&build_text_chars_atom("Centered Main Title"));
    slwt.extend_from_slice(&build_text_header_atom(6)); // Subtitle
    slwt.extend_from_slice(&build_text_chars_atom("A descriptive subtitle"));
    slwt.extend_from_slice(&build_text_header_atom(1)); // Body
    slwt.extend_from_slice(&build_text_chars_atom("Regular body content"));
    build_ppt_cfb(&slwt, &[])
}

#[test]
fn golden_ppt_subtitle_center_title() {
    let data = build_ppt_subtitle_center_title();
    let mut doc = PptDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("ppt_subtitle_center_title", &text, &golden_dir());
}

#[test]
fn ppt_subtitle_center_title_bold_flags() {
    let data = build_ppt_subtitle_center_title();
    let mut doc = PptDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let spans = page.raw_spans().expect("raw_spans()");

    assert_eq!(spans.len(), 3);
    // CenterTitle and Subtitle should be bold.
    assert!(spans[0].is_bold, "CenterTitle should be bold");
    assert!(spans[1].is_bold, "Subtitle should be bold");
    assert!(!spans[2].is_bold, "Body should not be bold");
}

// ---------------------------------------------------------------------------
// Golden 7: slide with only body text (no title shape)
// ---------------------------------------------------------------------------

fn build_ppt_body_only() -> Vec<u8> {
    let mut slwt = Vec::new();
    slwt.extend_from_slice(&build_slide_persist_atom(1));
    // Type 1 = Body; no Title (type 0) present.
    slwt.extend_from_slice(&build_text_header_atom(1));
    slwt.extend_from_slice(&build_text_chars_atom("First bullet point"));
    slwt.extend_from_slice(&build_text_header_atom(1));
    slwt.extend_from_slice(&build_text_chars_atom("Second bullet point"));
    slwt.extend_from_slice(&build_text_header_atom(1));
    slwt.extend_from_slice(&build_text_chars_atom("Third bullet point"));
    build_ppt_cfb(&slwt, &[])
}

#[test]
fn golden_ppt_body_only() {
    let data = build_ppt_body_only();
    let mut doc = PptDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("ppt_body_only", &text, &golden_dir());
}

#[test]
fn ppt_body_only_not_bold() {
    let data = build_ppt_body_only();
    let mut doc = PptDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let spans = page.raw_spans().expect("raw_spans()");
    for span in &spans {
        assert!(!span.is_bold, "body-only slides should have no bold spans");
    }
}

// ---------------------------------------------------------------------------
// Golden 8: multiple text frames on one slide (title + body + notes text)
// ---------------------------------------------------------------------------

fn build_ppt_multi_frame() -> Vec<u8> {
    let mut slide_slwt = Vec::new();
    slide_slwt.extend_from_slice(&build_slide_persist_atom(1));
    // Title
    slide_slwt.extend_from_slice(&build_text_header_atom(0));
    slide_slwt.extend_from_slice(&build_text_chars_atom("Product Roadmap"));
    // Body (three frames simulating a multi-column layout)
    slide_slwt.extend_from_slice(&build_text_header_atom(1));
    slide_slwt.extend_from_slice(&build_text_chars_atom(": Discovery"));
    slide_slwt.extend_from_slice(&build_text_header_atom(1));
    slide_slwt.extend_from_slice(&build_text_chars_atom(": Design"));
    slide_slwt.extend_from_slice(&build_text_header_atom(1));
    slide_slwt.extend_from_slice(&build_text_chars_atom(": Delivery"));
    // Other text (type 4 = Other)
    slide_slwt.extend_from_slice(&build_text_header_atom(4));
    slide_slwt.extend_from_slice(&build_text_chars_atom("Confidential"));

    let mut notes_slwt = Vec::new();
    notes_slwt.extend_from_slice(&build_slide_persist_atom(100));
    notes_slwt.extend_from_slice(&build_text_header_atom(2)); // Notes
    notes_slwt.extend_from_slice(&build_text_chars_atom("Focus on customer value"));

    build_ppt_cfb(&slide_slwt, &notes_slwt)
}

#[test]
fn golden_ppt_multi_frame() {
    let data = build_ppt_multi_frame();
    let mut doc = PptDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("ppt_multi_frame", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// Golden 9: slide with CP1252 encoded text via TextBytesAtom
// ---------------------------------------------------------------------------

fn build_ppt_cp1252_bytes() -> Vec<u8> {
    let mut slwt = Vec::new();
    slwt.extend_from_slice(&build_slide_persist_atom(1));
    slwt.extend_from_slice(&build_text_header_atom(0)); // Title
                                                        // CP1252: "Caf\xe9" = "Cafe" with an acute e (U+00E9).
                                                        // TextBytesAtom stores single-byte characters, decoded as Latin-1/CP1252.
    slwt.extend_from_slice(&build_text_bytes_atom(b"Caf\xe9 Menu"));
    slwt.extend_from_slice(&build_text_header_atom(1)); // Body
                                                        // CP1252: "\xc9l\xe8ve" = "Eleve" with diacritics (U+00C9, U+00E8).
    slwt.extend_from_slice(&build_text_bytes_atom(b"\xc9l\xe8ve Summary"));
    build_ppt_cfb(&slwt, &[])
}

#[test]
fn golden_ppt_cp1252_bytes() {
    let data = build_ppt_cp1252_bytes();
    let mut doc = PptDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("ppt_cp1252_bytes", &text, &golden_dir());
}

#[test]
fn ppt_cp1252_bytes_contains_expected_chars() {
    let data = build_ppt_cp1252_bytes();
    let mut doc = PptDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    // U+00E9 = e with acute, U+00C9 = E with acute, U+00E8 = e with grave.
    assert!(text.contains('\u{00E9}'), "expected e-acute in output");
    assert!(text.contains('\u{00C9}'), "expected E-acute in output");
    assert!(text.contains('\u{00E8}'), "expected e-grave in output");
}

// ---------------------------------------------------------------------------
// Golden 10: empty slide (persist atom but no text content)
// ---------------------------------------------------------------------------

fn build_ppt_empty_slide() -> Vec<u8> {
    let mut slwt = Vec::new();
    // Slide has a persist atom but no TextHeaderAtom/TextCharsAtom records.
    slwt.extend_from_slice(&build_slide_persist_atom(1));
    build_ppt_cfb(&slwt, &[])
}

#[test]
fn golden_ppt_empty_slide() {
    let data = build_ppt_empty_slide();
    let mut doc = PptDocument::from_bytes(&data).expect("from_bytes");
    // One slide exists but has no text.
    assert_eq!(doc.page_count(), 1, "should have 1 slide");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("ppt_empty_slide", &text, &golden_dir());
}

#[test]
fn ppt_empty_slide_produces_empty_text() {
    let data = build_ppt_empty_slide();
    let mut doc = PptDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert!(
        text.trim().is_empty(),
        "empty slide should produce empty text, got: {text:?}"
    );
}

//! Coverage tests for src/font/loader.rs.
//!
//! Targets three specific uncovered code paths:
//! 1. Type1 font loading (SimpleSubtype::Type1 branch in load_font)
//! 2. Encoding dict with BaseEncoding + Differences merge
//! 3. CID font fallback when DescendantFonts is missing or malformed

mod common;

use common::PdfBuilder;
use udoc_pdf::Document;

// ---------------------------------------------------------------------------
// Test 1: Type1 font loading path
// ---------------------------------------------------------------------------
// The /Subtype /Type1 branch in load_font goes through load_simple_font with
// SimpleSubtype::Type1. Existing tests cover TrueType but not Type1 directly
// through the full pipeline with text extraction verification.

#[test]
fn type1_font_text_extraction() {
    let pdf = build_type1_font_pdf();
    let mut doc = Document::from_bytes(pdf).expect("should parse Type1 font PDF");
    assert_eq!(doc.page_count(), 1);
    let mut page = doc.page(0).expect("should get page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("Hello"),
        "expected 'Hello' in extracted text, got: {text:?}"
    );
}

/// Build a minimal PDF with a /Subtype /Type1 font (no explicit encoding,
/// so the loader defaults to StandardEncoding for Type1).
fn build_type1_font_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    let content = b"BT /F1 12 Tf 72 700 Td (Hello) Tj ET";
    b.add_stream_object(5, "", content);

    // Type1 font with no explicit encoding -- exercises the Type1 branch
    // in load_font and the BuiltIn -> Standard default in load_simple_font
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    b.add_object(6, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources 6 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    b.finish(1)
}

// ---------------------------------------------------------------------------
// Test 2: Type1 with explicit WinAnsiEncoding (name encoding path)
// ---------------------------------------------------------------------------

#[test]
fn type1_font_winansi_encoding() {
    let pdf = build_type1_winansi_pdf();
    let mut doc = Document::from_bytes(pdf).expect("should parse Type1+WinAnsi PDF");
    let mut page = doc.page(0).expect("should get page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("ABC"),
        "expected 'ABC' in extracted text, got: {text:?}"
    );
}

fn build_type1_winansi_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    let content = b"BT /F1 12 Tf 72 700 Td (ABC) Tj ET";
    b.add_stream_object(5, "", content);

    b.add_object(
        4,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier /Encoding /WinAnsiEncoding >>",
    );

    b.add_object(6, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources 6 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    b.finish(1)
}

// ---------------------------------------------------------------------------
// Test 3: Encoding dict with /BaseEncoding + /Differences merge
// ---------------------------------------------------------------------------
// This exercises load_encoding_dict where both a base encoding and
// /Differences are present, producing an Encoding::Custom table that
// merges the base with overrides.

#[test]
fn encoding_base_plus_differences_merge() {
    let pdf = build_encoding_merge_pdf();
    let mut doc = Document::from_bytes(pdf).expect("should parse encoding merge PDF");
    let mut page = doc.page(0).expect("should get page 0");
    let text = page.text().expect("should extract text");

    // Code point 65 is overridden to /Aacute (U+00C1)
    // Code point 66 should still come from WinAnsi base (B = U+0042)
    // Code point 67 is overridden to /Ccedilla (U+00C7)
    assert!(
        text.contains('\u{00C1}'),
        "expected Aacute (U+00C1) at code 65, got: {text:?}"
    );
    assert!(
        text.contains('B'),
        "expected 'B' at code 66 from WinAnsi base, got: {text:?}"
    );
    assert!(
        text.contains('\u{00C7}'),
        "expected Ccedilla (U+00C7) at code 67, got: {text:?}"
    );
}

/// Build a PDF whose font has /Encoding with both /BaseEncoding and /Differences.
/// Code 65 -> /Aacute, code 67 -> /Ccedilla. Code 66 falls through to WinAnsi (B).
fn build_encoding_merge_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    // Content stream: emit bytes 65 (A), 66 (B), 67 (C) via literal string
    let content = b"BT /F1 12 Tf 72 700 Td (ABC) Tj ET";
    b.add_stream_object(5, "", content);

    // Encoding dictionary as a separate object
    b.add_object(
        7,
        b"<< /Type /Encoding /BaseEncoding /WinAnsiEncoding /Differences [65 /Aacute 67 /Ccedilla] >>",
    );

    // Font referencing the encoding object
    b.add_object(
        4,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding 7 0 R >>",
    );

    b.add_object(6, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources 6 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    b.finish(1)
}

// ---------------------------------------------------------------------------
// Test 4: Encoding dict with /Differences only (no /BaseEncoding)
// ---------------------------------------------------------------------------
// When /BaseEncoding is absent, the base defaults to StandardEncoding.

#[test]
fn encoding_differences_only_no_base() {
    let pdf = build_encoding_differences_only_pdf();
    let mut doc = Document::from_bytes(pdf).expect("should parse differences-only PDF");
    let mut page = doc.page(0).expect("should get page 0");
    let text = page.text().expect("should extract text");

    // Code 65 overridden to /Aacute
    assert!(
        text.contains('\u{00C1}'),
        "expected Aacute (U+00C1) from /Differences, got: {text:?}"
    );
}

fn build_encoding_differences_only_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    let content = b"BT /F1 12 Tf 72 700 Td (A) Tj ET";
    b.add_stream_object(5, "", content);

    // Encoding dict with /Differences but no /BaseEncoding
    b.add_object(7, b"<< /Type /Encoding /Differences [65 /Aacute] >>");

    b.add_object(
        4,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding 7 0 R >>",
    );

    b.add_object(6, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources 6 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    b.finish(1)
}

// ---------------------------------------------------------------------------
// Test 5: Inline encoding dict (not an indirect reference)
// ---------------------------------------------------------------------------
// Exercises the PdfObject::Dictionary branch in load_encoding directly.

#[test]
fn encoding_inline_dict_with_differences() {
    let pdf = build_inline_encoding_dict_pdf();
    let mut doc = Document::from_bytes(pdf).expect("should parse inline encoding dict PDF");
    let mut page = doc.page(0).expect("should get page 0");
    let text = page.text().expect("should extract text");

    // Code 65 -> /Aacute (U+00C1)
    assert!(
        text.contains('\u{00C1}'),
        "expected Aacute from inline /Encoding dict, got: {text:?}"
    );
}

/// Font dict embeds /Encoding as a direct dictionary value (not a reference).
fn build_inline_encoding_dict_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    let content = b"BT /F1 12 Tf 72 700 Td (A) Tj ET";
    b.add_stream_object(5, "", content);

    // Font with inline encoding dict
    b.add_object(
        4,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding << /BaseEncoding /WinAnsiEncoding /Differences [65 /Aacute] >> >>",
    );

    b.add_object(6, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources 6 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    b.finish(1)
}

// ---------------------------------------------------------------------------
// Test 6: CID font fallback -- missing DescendantFonts
// ---------------------------------------------------------------------------
// When /DescendantFonts is missing entirely, the loader should return a
// default CID font and not crash.

#[test]
fn cid_font_fallback_missing_descendants() {
    let pdf = build_cid_font_missing_descendants_pdf();
    let mut doc = Document::from_bytes(pdf).expect("should parse PDF with missing DescendantFonts");
    let mut page = doc.page(0).expect("should get page 0");
    // Text extraction may produce garbage or empty output, but must not panic
    let _text = page.text().expect("text extraction should not fail");
}

/// Type0 font with no /DescendantFonts entry at all.
fn build_cid_font_missing_descendants_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    // Content stream using 2-byte CID encoding (hex string)
    let content = b"BT /F1 12 Tf 72 700 Td <0048> Tj ET";
    b.add_stream_object(5, "", content);

    // Type0 font missing /DescendantFonts
    b.add_object(
        4,
        b"<< /Type /Font /Subtype /Type0 /BaseFont /Arial /Encoding /Identity-H >>",
    );

    b.add_object(6, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources 6 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    b.finish(1)
}

// ---------------------------------------------------------------------------
// Test 7: CID font fallback -- DescendantFonts contains non-reference
// ---------------------------------------------------------------------------
// When /DescendantFonts array has a direct (non-reference) element,
// the loader should fall through to the default CID font.

#[test]
fn cid_font_fallback_non_reference_descendant() {
    let pdf = build_cid_font_non_ref_descendant_pdf();
    let mut doc = Document::from_bytes(pdf).expect("should parse PDF with non-ref DescendantFonts");
    let mut page = doc.page(0).expect("should get page 0");
    // Must not panic; text may be empty or replacement chars
    let _text = page.text().expect("text extraction should not fail");
}

/// Type0 font where /DescendantFonts contains a direct integer instead of a reference.
fn build_cid_font_non_ref_descendant_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    let content = b"BT /F1 12 Tf 72 700 Td <0048> Tj ET";
    b.add_stream_object(5, "", content);

    // /DescendantFonts array with a non-reference value (integer 999)
    b.add_object(
        4,
        b"<< /Type /Font /Subtype /Type0 /BaseFont /Arial /Encoding /Identity-H /DescendantFonts [999] >>",
    );

    b.add_object(6, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources 6 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    b.finish(1)
}

// ---------------------------------------------------------------------------
// Test 8: CID font fallback -- DescendantFonts points to non-existent object
// ---------------------------------------------------------------------------
// When /DescendantFonts references an object that doesn't exist,
// the CID font load should fail and the composite font should still
// be usable (graceful degradation).

#[test]
fn cid_font_fallback_dangling_ref() {
    let pdf = build_cid_font_dangling_ref_pdf();
    let mut doc =
        Document::from_bytes(pdf).expect("should parse PDF with dangling DescendantFonts ref");
    let mut page = doc.page(0).expect("should get page 0");
    // The dangling ref will cause an error in load_cid_font, but the
    // content interpreter should handle it gracefully
    let _text = page.text().expect("text extraction should not fail");
}

fn build_cid_font_dangling_ref_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    let content = b"BT /F1 12 Tf 72 700 Td <0048> Tj ET";
    b.add_stream_object(5, "", content);

    // /DescendantFonts references object 99 which does not exist
    b.add_object(
        4,
        b"<< /Type /Font /Subtype /Type0 /BaseFont /Arial /Encoding /Identity-H /DescendantFonts [99 0 R] >>",
    );

    b.add_object(6, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources 6 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    b.finish(1)
}

// ---------------------------------------------------------------------------
// Test 9: MMType1 font loading (another simple font subtype branch)
// ---------------------------------------------------------------------------

#[test]
fn mmtype1_font_text_extraction() {
    let pdf = build_mmtype1_font_pdf();
    let mut doc = Document::from_bytes(pdf).expect("should parse MMType1 font PDF");
    let mut page = doc.page(0).expect("should get page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("Test"),
        "expected 'Test' in extracted text, got: {text:?}"
    );
}

fn build_mmtype1_font_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    let content = b"BT /F1 12 Tf 72 700 Td (Test) Tj ET";
    b.add_stream_object(5, "", content);

    b.add_object(
        4,
        b"<< /Type /Font /Subtype /MMType1 /BaseFont /MyriadMM >>",
    );

    b.add_object(6, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources 6 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    b.finish(1)
}

// ---------------------------------------------------------------------------
// Test 10: Unknown font subtype falls back to Type1
// ---------------------------------------------------------------------------

#[test]
fn unknown_font_subtype_falls_back_to_type1() {
    let pdf = build_unknown_subtype_pdf();
    let mut doc = Document::from_bytes(pdf).expect("should parse unknown subtype PDF");
    let mut page = doc.page(0).expect("should get page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("Fallback"),
        "expected 'Fallback' in extracted text, got: {text:?}"
    );
}

fn build_unknown_subtype_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    let content = b"BT /F1 12 Tf 72 700 Td (Fallback) Tj ET";
    b.add_stream_object(5, "", content);

    // Completely made-up subtype
    b.add_object(
        4,
        b"<< /Type /Font /Subtype /BogusType99 /BaseFont /Helvetica >>",
    );

    b.add_object(6, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources 6 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    b.finish(1)
}

// ---------------------------------------------------------------------------
// Test 11: CID font with empty DescendantFonts array
// ---------------------------------------------------------------------------

#[test]
fn cid_font_fallback_empty_descendants_array() {
    let pdf = build_cid_font_empty_descendants_pdf();
    let mut doc = Document::from_bytes(pdf).expect("should parse PDF with empty DescendantFonts");
    let mut page = doc.page(0).expect("should get page 0");
    let _text = page.text().expect("text extraction should not fail");
}

fn build_cid_font_empty_descendants_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    let content = b"BT /F1 12 Tf 72 700 Td <0048> Tj ET";
    b.add_stream_object(5, "", content);

    // /DescendantFonts is an empty array
    b.add_object(
        4,
        b"<< /Type /Font /Subtype /Type0 /BaseFont /Arial /Encoding /Identity-H /DescendantFonts [] >>",
    );

    b.add_object(6, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources 6 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    b.finish(1)
}

//! Integration tests for the public API (Document, Page, Config).
//!
//! Tests that the public API works end-to-end on real corpus PDFs.

mod common;

use std::sync::Arc;

use common::PdfBuilder;
use udoc_pdf::{
    CollectingDiagnostics, Config, Document, ImageFilter, PageImage, WarningKind, WarningLevel,
};

fn corpus_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/corpus/minimal")
        .join(name)
}

// ---------------------------------------------------------------------------
// Document::open / from_bytes
// ---------------------------------------------------------------------------

#[test]
fn test_document_open() {
    let doc = Document::open(corpus_path("winansi_type1.pdf")).unwrap();
    assert!(doc.page_count() > 0);
}

#[test]
fn test_document_open_nonexistent() {
    let err = Document::open("nonexistent.pdf");
    assert!(err.is_err());
}

#[test]
fn test_document_from_bytes() {
    let data = std::fs::read(corpus_path("winansi_type1.pdf")).unwrap();
    let doc = Document::from_bytes(data).unwrap();
    assert!(doc.page_count() > 0);
}

#[test]
fn test_document_from_bytes_invalid() {
    let err = Document::from_bytes(b"not a pdf".to_vec());
    assert!(err.is_err());
}

// ---------------------------------------------------------------------------
// Page count
// ---------------------------------------------------------------------------

#[test]
fn test_page_count_single() {
    let doc = Document::open(corpus_path("winansi_type1.pdf")).unwrap();
    assert_eq!(doc.page_count(), 1);
}

#[test]
fn test_page_count_multi() {
    let doc = Document::open(corpus_path("multipage.pdf")).unwrap();
    assert!(
        doc.page_count() > 1,
        "multipage.pdf should have multiple pages"
    );
}

// ---------------------------------------------------------------------------
// Page::text() -- non-empty on at least 10 corpus PDFs
// ---------------------------------------------------------------------------

#[test]
fn test_page_text_nonempty_on_corpus() {
    let corpus_files = [
        "winansi_type1.pdf",
        "macroman_type1.pdf",
        "flate_content.pdf",
        "xelatex.pdf",
        "xelatex-drawboard.pdf",
        "form_xobject.pdf",
        "multipage.pdf",
        "content_array.pdf",
        "simpletype3font.pdf",
        "wrong_length.pdf",
        "nested_pages.pdf",
        "two_flate_streams.pdf",
    ];

    let mut nonempty_count = 0;

    for filename in &corpus_files {
        let path = corpus_path(filename);
        let mut doc = match Document::open(&path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("  {filename}: open failed: {e}");
                continue;
            }
        };

        let mut file_has_text = false;
        for i in 0..doc.page_count() {
            let mut page = match doc.page(i) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("  {filename} page {i}: page() failed: {e}");
                    continue;
                }
            };
            match page.text() {
                Ok(text) if !text.trim().is_empty() => {
                    file_has_text = true;
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("  {filename} page {i}: text() failed: {e}");
                }
            }
        }

        if file_has_text {
            nonempty_count += 1;
        }
    }

    assert!(
        nonempty_count >= 10,
        "expected at least 10 corpus PDFs with non-empty text, got {nonempty_count}"
    );
}

// ---------------------------------------------------------------------------
// Page::text_lines() and Page::raw_spans()
// ---------------------------------------------------------------------------

#[test]
fn test_page_text_lines() {
    let mut doc = Document::open(corpus_path("winansi_type1.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let lines = page.text_lines().unwrap();
    assert!(!lines.is_empty(), "winansi_type1 should have text lines");
    // Each line should have at least one span
    for line in &lines {
        assert!(!line.spans.is_empty());
    }
}

#[test]
fn test_page_raw_spans() {
    let mut doc = Document::open(corpus_path("winansi_type1.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let spans = page.raw_spans().unwrap();
    assert!(!spans.is_empty(), "winansi_type1 should have raw spans");
    // Each span should have non-empty text
    for span in &spans {
        assert!(!span.text.is_empty());
    }
}

// ---------------------------------------------------------------------------
// Config with custom diagnostics
// ---------------------------------------------------------------------------

#[test]
fn test_config_with_diagnostics() {
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());

    let mut doc = Document::open_with_config(corpus_path("wrong_length.pdf"), config).unwrap();

    // Extract text to trigger warnings
    for i in 0..doc.page_count() {
        let mut page = doc.page(i).unwrap();
        let _ = page.text();
    }

    // wrong_length.pdf should produce stream length warnings
    let warnings = diag.warnings();
    assert!(
        !warnings.is_empty(),
        "wrong_length.pdf should produce at least one warning"
    );
}

// ---------------------------------------------------------------------------
// Page index out of range
// ---------------------------------------------------------------------------

#[test]
fn test_page_index_out_of_range() {
    let mut doc = Document::open(corpus_path("winansi_type1.pdf")).unwrap();
    let err = doc.page(999);
    assert!(err.is_err());
}

// ---------------------------------------------------------------------------
// Correctness spot-checks (I-003)
// ---------------------------------------------------------------------------

#[test]
fn test_correctness_winansi_type1() {
    let mut doc = Document::open(corpus_path("winansi_type1.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    // This PDF contains "Hello World" in WinAnsiEncoding Type1
    assert!(
        text.contains("Hello"),
        "winansi_type1.pdf should contain 'Hello', got: {text}"
    );
}

#[test]
fn test_correctness_macroman_type1() {
    let mut doc = Document::open(corpus_path("macroman_type1.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    // This PDF contains "Hello World" in MacRomanEncoding Type1
    assert!(
        text.contains("Hello"),
        "macroman_type1.pdf should contain 'Hello', got: {text}"
    );
}

#[test]
fn test_correctness_multipage() {
    let mut doc = Document::open(corpus_path("multipage.pdf")).unwrap();
    assert_eq!(doc.page_count(), 5, "multipage.pdf should have 5 pages");

    // Each page contains "Page N" where N is 1-indexed.
    for i in 0..doc.page_count() {
        let mut page = doc.page(i).unwrap();
        let text = page.text().unwrap();
        let expected = format!("Page {}", i + 1);
        assert!(
            text.contains(&expected),
            "multipage.pdf page {i} should contain '{expected}', got: {text}"
        );
    }
}

// ---------------------------------------------------------------------------
// Diagnostics pipeline: WarningLevel and WarningContext (D-013)
// ---------------------------------------------------------------------------

#[test]
fn test_diagnostics_warning_level_and_context() {
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());

    // Use a PDF with fonts to trigger font loading info messages
    let mut doc = Document::open_with_config(corpus_path("winansi_type1.pdf"), config).unwrap();
    for i in 0..doc.page_count() {
        let mut page = doc.page(i).unwrap();
        let _ = page.text();
    }

    let warnings = diag.warnings();

    // D-011: at least one Info-level message (from font loading)
    assert!(
        warnings.iter().any(|w| w.level == WarningLevel::Info),
        "expected at least one Info-level diagnostic, got: {:?}",
        warnings.iter().map(|w| &w.level).collect::<Vec<_>>()
    );

    // D-012: at least one warning with obj_ref set (from font loader)
    assert!(
        warnings.iter().any(|w| w.context.obj_ref.is_some()),
        "expected at least one diagnostic with obj_ref, got: {:?}",
        warnings
            .iter()
            .map(|w| &w.context.obj_ref)
            .collect::<Vec<_>>()
    );

    // page_index should be populated on content interpreter warnings.
    // Font loading info messages don't have page_index (loaded before
    // content interpretation). Use a corpus PDF that triggers content
    // interpreter warnings to test page_index separately.
}

#[test]
fn test_diagnostics_page_index_populated() {
    // Build a PDF that references a font not in /Resources, triggering a
    // content-level warning from the interpreter (which sets page_index).
    let mut builder = PdfBuilder::new("1.4");
    builder.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    builder.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    builder.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R >>",
    );
    builder.add_stream_object(4, "", b"BT /F1 12 Tf (hello) Tj ET");
    let pdf_bytes = builder.finish(1);

    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    let mut doc = Document::from_bytes_with_config(pdf_bytes, config).unwrap();
    let mut page = doc.page(0).unwrap();
    let _ = page.text();

    let warnings = diag.warnings();
    assert!(
        !warnings.is_empty(),
        "should produce content-level diagnostics for missing font"
    );
    let has_page_index = warnings.iter().any(|w| w.context.page_index.is_some());
    assert!(
        has_page_index,
        "at least one warning should have page_index populated, got: {:?}",
        warnings
            .iter()
            .map(|w| (&w.kind, &w.context))
            .collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Empty page
// ---------------------------------------------------------------------------

#[test]
fn test_empty_page() {
    let mut doc = Document::open(corpus_path("empty_page.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    assert!(text.trim().is_empty(), "empty_page.pdf should have no text");
}

// ---------------------------------------------------------------------------
// Form XObject
// ---------------------------------------------------------------------------

#[test]
fn test_form_xobject_text() {
    let mut doc = Document::open(corpus_path("form_xobject.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    assert!(
        !text.trim().is_empty(),
        "form_xobject.pdf should have text (rendered via XObject)"
    );
}

// ---------------------------------------------------------------------------
// Error recovery tests (T-002)
// ---------------------------------------------------------------------------

/// PDF with a missing /Font entry: page has /Resources but no /Font dict.
/// The interpreter should produce empty text (or FFFD), not panic.
#[test]
fn test_error_recovery_missing_font() {
    let pdf = build_pdf_missing_font();
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    let mut doc = Document::from_bytes_with_config(pdf, config).unwrap();

    let mut page = doc.page(0).unwrap();
    // Should not panic. Text may be empty or contain FFFD.
    let text = page.text().unwrap();
    let _ = text; // result doesn't matter, just no panic

    // Should have a warning about font not found
    let warnings = diag.warnings();
    assert!(
        warnings
            .iter()
            .any(|w| w.message.contains("not found") || w.message.contains("font")),
        "expected font-related warning, got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

/// PDF with a truncated content stream: stream data ends abruptly.
/// The interpreter should extract whatever text it can, not panic.
#[test]
fn test_error_recovery_truncated_stream() {
    let pdf = build_pdf_truncated_content();
    let mut doc = Document::from_bytes(pdf).unwrap();
    let mut page = doc.page(0).unwrap();
    // Should not panic on truncated content
    let _text = page.text().unwrap();
}

/// PDF with a circular Form XObject reference at the public API level.
/// The interpreter should detect the cycle and warn, not hang.
#[test]
fn test_error_recovery_circular_xobject() {
    let pdf = build_pdf_circular_xobject();
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    let mut doc = Document::from_bytes_with_config(pdf, config).unwrap();

    let mut page = doc.page(0).unwrap();
    // Should not hang or panic
    let _text = page.text().unwrap();

    let warnings = diag.warnings();
    assert!(
        warnings.iter().any(|w| w.message.contains("circular")),
        "expected circular XObject warning, got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// ExtGState (CI-001) and Marked Content (CI-002) tests
// ---------------------------------------------------------------------------

/// PDF using ExtGState to set font via gs operator.
#[test]
fn test_extgstate_font_override() {
    let pdf = build_pdf_extgstate_font();
    let mut doc = Document::from_bytes(pdf).unwrap();
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    assert!(
        text.contains("GS font"),
        "ExtGState font override should produce text, got: {text}"
    );
}

/// PDF using Tr operator to set text rendering mode 3 (invisible).
/// Invisible text (e.g. OCR overlays) is now extracted with is_invisible=true.
#[test]
fn test_invisible_text_tr_operator() {
    let pdf = build_pdf_invisible_text_tr_operator();
    let mut doc = Document::from_bytes(pdf).unwrap();
    let mut page = doc.page(0).unwrap();

    // All text should be extracted, including invisible
    let text = page.text().unwrap();
    assert!(
        text.contains("invisible"),
        "Tr=3 text should now be extracted, got: {text}"
    );
    assert!(
        text.contains("visible"),
        "visible text should be present, got: {text}"
    );
    assert!(
        text.contains("also visible"),
        "text after Tr=0 reset should be present, got: {text}"
    );

    // Verify is_invisible flag via raw_spans
    let spans = page.raw_spans().unwrap();
    let invisible_spans: Vec<_> = spans.iter().filter(|s| s.is_invisible).collect();
    let visible_spans: Vec<_> = spans.iter().filter(|s| !s.is_invisible).collect();

    assert!(
        !invisible_spans.is_empty(),
        "should have at least one invisible span"
    );
    assert!(
        invisible_spans.iter().any(|s| s.text.contains("invisible")),
        "invisible span should contain 'invisible'"
    );
    assert!(
        visible_spans.iter().any(|s| s.text.contains("visible")),
        "visible spans should contain 'visible'"
    );
    assert!(
        visible_spans.iter().all(|s| !s.is_invisible),
        "visible spans should have is_invisible=false"
    );
}

/// PDF with marked content operators (BMC/BDC/EMC).
#[test]
fn test_marked_content_passthrough() {
    let pdf = build_pdf_marked_content();
    let mut doc = Document::from_bytes(pdf).unwrap();
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    // Text inside marked content should still be extracted
    assert!(
        text.contains("marked text"),
        "text inside marked content should be extracted, got: {text}"
    );
}

/// PDF with unbalanced EMC (more EMC than BMC). Should not panic.
#[test]
fn test_marked_content_unbalanced_emc() {
    let pdf = build_pdf_unbalanced_emc();
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    let mut doc = Document::from_bytes_with_config(pdf, config).unwrap();
    let mut page = doc.page(0).unwrap();
    let _text = page.text().unwrap(); // should not panic
    let warnings = diag.warnings();
    assert!(
        warnings.iter().any(|w| w.message.contains("EMC")),
        "expected EMC underflow warning, got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

/// PDF with malformed ExtGState /Font (bad size) but valid /TL.
/// Verifies that a bad /Font doesn't skip text state params.
#[test]
fn test_extgstate_bad_font_applies_tl() {
    let pdf = build_pdf_extgstate_bad_font_valid_tl();
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    let mut doc = Document::from_bytes_with_config(pdf, config).unwrap();
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    // /TL should still be applied (T* moves to next line), so both lines appear
    assert!(
        text.contains("Line one"),
        "first line should be present, got: {text}"
    );
    assert!(
        text.contains("Line two"),
        "second line (via T* after /TL from ExtGState) should be present, got: {text}"
    );
    // Should warn about bad font size
    let warnings = diag.warnings();
    assert!(
        warnings
            .iter()
            .any(|w| w.message.contains("Font size is not a number")),
        "expected warning about bad font size, got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

// -- Helpers for building CI-001/CI-002 test PDFs --

fn build_pdf_extgstate_font() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_object(7, b"<< /Type /ExtGState /Font [4 0 R 12] >>");
    b.add_stream_object(3, "", b"BT /GS1 gs 100 700 Td (GS font) Tj ET");
    b.add_object(
        5,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> /ExtGState << /GS1 7 0 R >> >> >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [5 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

fn build_pdf_invisible_text_tr_operator() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(
        3,
        "",
        b"BT /F1 12 Tf 100 700 Td (visible) Tj 3 Tr 100 680 Td (invisible) Tj 0 Tr 100 660 Td (also visible) Tj ET",
    );
    b.add_object(
        5,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [5 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

fn build_pdf_marked_content() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(
        3,
        "",
        b"BT /F1 12 Tf 100 700 Td /Span BMC (marked text) Tj EMC ET",
    );
    b.add_object(
        5,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [5 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

fn build_pdf_unbalanced_emc() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (text) Tj EMC ET");
    b.add_object(
        5,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [5 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

fn build_pdf_extgstate_bad_font_valid_tl() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_object(7, b"<< /Type /ExtGState /Font [4 0 R (bad)] /TL 14 >>");
    b.add_stream_object(
        3,
        "",
        b"BT /F1 12 Tf 100 700 Td /GS1 gs (Line one) Tj T* (Line two) Tj ET",
    );
    b.add_object(
        5,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> /ExtGState << /GS1 7 0 R >> >> >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [5 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

// -- Helpers for building error-recovery test PDFs --

fn build_pdf_missing_font() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Hi) Tj ET");
    b.add_object(
        4,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << >> >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [4 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

fn build_pdf_truncated_content() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj");
    b.add_object(
        5,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [5 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

fn build_pdf_circular_xobject() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    b.add_stream_object(
        4,
        "/Type /XObject /Subtype /Form /BBox [0 0 100 100]",
        b"/Fm1 Do",
    );
    b.add_stream_object(3, "", b"/Fm1 Do");
    b.add_object(
        5,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /XObject << /Fm1 4 0 R >> >> >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [5 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

// ---------------------------------------------------------------------------
// Error recovery expansion (ER-001 through ER-008)
// ---------------------------------------------------------------------------

/// ER-001: Xref with invalid offsets (past EOF, pointing to wrong location).
/// The resolver should return errors on bad objects, not panic. The font at
/// the bogus offset cannot be loaded, so a FontError warning is emitted and
/// text extraction degrades gracefully.
#[test]
fn test_er001_xref_invalid_offsets() {
    let pdf = build_pdf_xref_invalid_offsets();
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    // Document should still open (xref parses fine, offsets are just wrong).
    // Page tree objects have valid offsets; font object (obj 4) has a bogus
    // offset, so font loading fails but text extraction degrades gracefully.
    let mut doc = Document::from_bytes_with_config(pdf, config)
        .expect("document with valid page tree but bogus font offset should open");
    assert!(doc.page_count() > 0, "document should have pages");
    let mut page = doc.page(0).expect("page 0 should be accessible");
    // text() succeeds but produces empty/degraded output since the font
    // at the bogus offset cannot be loaded.
    let _text = page.text().unwrap();

    // FontError warning should be emitted about the bad offset
    let warnings = diag.warnings();
    assert!(
        warnings
            .iter()
            .any(|w| w.kind == WarningKind::FontError && w.level == WarningLevel::Warning),
        "expected FontError warning about unreachable font, got: {:?}",
        warnings
            .iter()
            .map(|w| format!("[{:?}] {:?}: {}", w.level, w.kind, w.message))
            .collect::<Vec<_>>()
    );
}

/// ER-002: Object type mismatch (/Font points to a string instead of a dict).
/// The interpreter should warn and recover, not panic. The text() call
/// succeeds but with degraded output (font cannot be loaded).
#[test]
fn test_er002_font_type_mismatch() {
    let pdf = build_pdf_font_type_mismatch();
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    let mut doc = Document::from_bytes_with_config(pdf, config).unwrap();
    let mut page = doc.page(0).unwrap();
    // Should not panic. Text may be empty or contain fallback chars.
    let _text = page.text().unwrap();

    // Diagnostics should include a FontError warning about the type mismatch
    let warnings = diag.warnings();
    assert!(
        warnings.iter().any(|w| w.kind == WarningKind::FontError
            && w.level == WarningLevel::Warning
            && w.message.contains("expected dictionary")),
        "expected FontError warning about type mismatch, got: {:?}",
        warnings
            .iter()
            .map(|w| format!("[{:?}] {:?}: {}", w.level, w.kind, w.message))
            .collect::<Vec<_>>()
    );
}

/// ER-003: Page tree with missing /Kids or /Count.
/// Document should open with 0 pages and emit an InvalidPageTree warning.
#[test]
fn test_er003_page_tree_missing_kids() {
    let pdf = build_pdf_missing_kids();
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    let doc = Document::from_bytes_with_config(pdf, config)
        .expect("missing /Kids should be tolerated, not an error");
    assert_eq!(doc.page_count(), 0);
    let warnings = diag.warnings();
    assert!(
        warnings
            .iter()
            .any(|w| w.kind == WarningKind::InvalidPageTree),
        "expected InvalidPageTree warning, got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

/// ER-004: Content stream ending mid-operator (no ET, no endstream marker in content).
/// The interpreter should extract whatever text it can before the truncation.
#[test]
fn test_er004_content_mid_operator() {
    let pdf = build_pdf_content_mid_operator();
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    let mut doc = Document::from_bytes_with_config(pdf, config).unwrap();
    let mut page = doc.page(0).unwrap();
    // Partial recovery: "Hello" was rendered before the stream truncated.
    let text = page.text().unwrap();
    assert!(
        text.contains("Hello"),
        "truncated content should still yield 'Hello', got: {text}"
    );

    // Diagnostics should include font loading info at minimum
    let warnings = diag.warnings();
    assert!(
        !warnings.is_empty(),
        "content with truncated operators should produce diagnostics"
    );
}

/// ER-005: Font with no /Encoding and no /ToUnicode (double fallback).
/// The interpreter should use fallback encoding and produce text output.
#[test]
fn test_er005_font_no_encoding_no_tounicode() {
    let pdf = build_pdf_font_no_encoding();
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    let mut doc = Document::from_bytes_with_config(pdf, config).unwrap();
    let mut page = doc.page(0).unwrap();
    // Fallback encoding (StandardEncoding for Type1) should produce text.
    let text = page.text().unwrap();
    assert!(
        !text.is_empty(),
        "font fallback should produce non-empty text"
    );

    // Should have diagnostics about font loading (encoding info)
    let warnings = diag.warnings();
    assert!(
        !warnings.is_empty(),
        "font with no explicit encoding should produce at least one diagnostic"
    );
    // Verify font loading info messages are present
    assert!(
        warnings.iter().any(|w| w.kind == WarningKind::FontLoaded),
        "expected FontLoaded diagnostic about encoding fallback, got: {:?}",
        warnings
            .iter()
            .map(|w| format!("[{:?}] {:?}: {}", w.level, w.kind, w.message))
            .collect::<Vec<_>>()
    );
}

/// ER-006: Stream with negative/zero/huge /Length values.
/// The parser should recover via endstream scanning, emit StreamLengthMismatch
/// warnings, and still extract the correct text.
#[test]
fn test_er006_stream_bad_length() {
    for length in [-1i64, 0, 999_999_999] {
        let pdf = build_pdf_stream_bad_length(length);
        let diag = Arc::new(CollectingDiagnostics::new());
        let config = Config::default().with_diagnostics(diag.clone());
        let mut doc = Document::from_bytes_with_config(pdf, config)
            .unwrap_or_else(|e| panic!("length={length}: document should open, got: {e}"));
        assert!(doc.page_count() > 0, "length={length}: should have pages");
        let mut page = doc
            .page(0)
            .unwrap_or_else(|e| panic!("length={length}: page 0 should load, got: {e}"));
        let text = page.text().unwrap();
        assert!(
            text.contains("Hi"),
            "length={length}: endstream recovery should yield 'Hi', got: {text}"
        );
        // StreamLengthMismatch warning should be emitted for every bad length
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.kind == WarningKind::StreamLengthMismatch
                    && w.level == WarningLevel::Warning),
            "length={length}: expected StreamLengthMismatch warning, got: {:?}",
            warnings
                .iter()
                .map(|w| format!("[{:?}] {:?}: {}", w.level, w.kind, w.message))
                .collect::<Vec<_>>()
        );
    }
}

/// ER-007: PDF with no %%EOF marker.
/// The parser should still find startxref and parse successfully. Text
/// extraction should produce "Hi" since the content is otherwise valid.
#[test]
fn test_er007_no_eof_marker() {
    let pdf = build_pdf_no_eof_marker();
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    // Should parse successfully (%%EOF is not strictly required for
    // finding startxref).
    let mut doc =
        Document::from_bytes_with_config(pdf, config).expect("PDF without %%EOF should still open");
    assert!(doc.page_count() > 0, "document should have pages");
    let mut page = doc.page(0).expect("page 0 should be accessible");
    let text = page.text().unwrap();
    assert!(
        text.contains("Hi"),
        "no-EOF PDF should still extract 'Hi', got: {text}"
    );
    // Font loading diagnostics should still fire normally
    let warnings = diag.warnings();
    assert!(
        !warnings.is_empty(),
        "should have at least font loading diagnostics"
    );
}

/// ER-008: Trailer with missing /Root.
/// The parser should fail with an InvalidStructure error mentioning /Root.
/// The error's context chain should provide structured information.
#[test]
fn test_er008_trailer_missing_root() {
    let pdf = build_pdf_missing_root();
    let result = Document::from_bytes(pdf);
    // Should fail with a structure error, not panic.
    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("PDF with no /Root in trailer should fail to open"),
    };
    let err_msg = format!("{err}");
    assert!(
        err_msg.contains("Root") || err_msg.contains("root") || err_msg.contains("catalog"),
        "error should mention missing Root/catalog, got: {err_msg}"
    );
    // Verify the error is an InvalidStructure variant with context chain
    let debug_msg = format!("{err:?}");
    assert!(
        debug_msg.contains("InvalidStructure") || debug_msg.contains("StructureError"),
        "error should be InvalidStructure variant, got: {debug_msg}"
    );
}

// -- Helpers for ER-001 through ER-008 --
// ER-001 (xref_invalid_offsets), ER-006 (stream_bad_length), ER-007
// (no_eof_marker), ER-008 (missing_root) stay hand-built because they
// need byte-level control over xref offsets, stream lengths, or trailer.

fn build_pdf_xref_invalid_offsets() -> Vec<u8> {
    // Xref points object 4 (the font) at offset 99999 (past EOF).
    // Must be hand-built: PdfBuilder generates correct xref offsets.
    let content = b"BT /F1 12 Tf 100 700 Td (Hi) Tj ET";
    let mut b = Vec::new();
    use std::io::Write;
    writeln!(b, "%PDF-1.4").unwrap();

    let _obj4_real_off = b.len();
    b.extend_from_slice(
        b"4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
    );

    let obj3_off = b.len();
    write!(b, "3 0 obj\n<< /Length {} >>\nstream\n", content.len()).unwrap();
    b.extend_from_slice(content);
    b.extend_from_slice(b"\nendstream\nendobj\n");

    let obj5_off = b.len();
    b.extend_from_slice(b"5 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>\nendobj\n");

    let obj2_off = b.len();
    b.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [5 0 R] /Count 1 >>\nendobj\n");

    let obj1_off = b.len();
    b.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let xref_off = b.len();
    write!(b, "xref\n0 6\n").unwrap();
    write!(b, "0000000000 65535 f \r\n").unwrap();
    write!(b, "{:010} 00000 n \r\n", obj1_off).unwrap();
    write!(b, "{:010} 00000 n \r\n", obj2_off).unwrap();
    write!(b, "{:010} 00000 n \r\n", obj3_off).unwrap();
    // Point font object at bogus offset past EOF
    write!(b, "0000099999 00000 n \r\n").unwrap();
    write!(b, "{:010} 00000 n \r\n", obj5_off).unwrap();
    write!(
        b,
        "trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_off}\n%%EOF\n"
    )
    .unwrap();
    b
}

fn build_pdf_font_type_mismatch() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    // Object 4 is a string instead of a font dictionary
    b.add_object(4, b"(This is not a font)");
    b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Hi) Tj ET");
    b.add_object(
        5,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [5 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

fn build_pdf_missing_kids() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(2, b"<< /Type /Pages >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

fn build_pdf_content_mid_operator() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    // Ends abruptly with no ET and a dangling number
    b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj 50");
    b.add_object(
        5,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [5 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

fn build_pdf_font_no_encoding() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    // Font with no /Encoding and no /ToUnicode
    b.add_object(
        4,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /SomeUnknownFont >>",
    );
    b.add_stream_object(3, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    b.add_object(
        5,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [5 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

fn build_pdf_stream_bad_length(length: i64) -> Vec<u8> {
    // Content stream with a bogus /Length value
    let actual_content = b"BT /F1 12 Tf 100 700 Td (Hi) Tj ET";
    let mut b = Vec::new();
    use std::io::Write;
    writeln!(b, "%PDF-1.4").unwrap();

    let obj4_off = b.len();
    b.extend_from_slice(
        b"4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
    );

    let obj3_off = b.len();
    // Use the bogus length in the dict, but write real content with endstream
    write!(b, "3 0 obj\n<< /Length {} >>\nstream\n", length).unwrap();
    b.extend_from_slice(actual_content);
    b.extend_from_slice(b"\nendstream\nendobj\n");

    let obj5_off = b.len();
    b.extend_from_slice(b"5 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>\nendobj\n");

    let obj2_off = b.len();
    b.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [5 0 R] /Count 1 >>\nendobj\n");

    let obj1_off = b.len();
    b.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let xref_off = b.len();
    write!(b, "xref\n0 6\n").unwrap();
    write!(b, "0000000000 65535 f \r\n").unwrap();
    write!(b, "{:010} 00000 n \r\n", obj1_off).unwrap();
    write!(b, "{:010} 00000 n \r\n", obj2_off).unwrap();
    write!(b, "{:010} 00000 n \r\n", obj3_off).unwrap();
    write!(b, "{:010} 00000 n \r\n", obj4_off).unwrap();
    write!(b, "{:010} 00000 n \r\n", obj5_off).unwrap();
    write!(
        b,
        "trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_off}\n%%EOF\n"
    )
    .unwrap();
    b
}

fn build_pdf_no_eof_marker() -> Vec<u8> {
    // Valid PDF structure but no %%EOF at the end
    let content = b"BT /F1 12 Tf 100 700 Td (Hi) Tj ET";
    let mut b = Vec::new();
    use std::io::Write;
    writeln!(b, "%PDF-1.4").unwrap();

    let obj4_off = b.len();
    b.extend_from_slice(
        b"4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
    );

    let obj3_off = b.len();
    write!(b, "3 0 obj\n<< /Length {} >>\nstream\n", content.len()).unwrap();
    b.extend_from_slice(content);
    b.extend_from_slice(b"\nendstream\nendobj\n");

    let obj5_off = b.len();
    b.extend_from_slice(b"5 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>\nendobj\n");

    let obj2_off = b.len();
    b.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [5 0 R] /Count 1 >>\nendobj\n");

    let obj1_off = b.len();
    b.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let xref_off = b.len();
    write!(b, "xref\n0 6\n").unwrap();
    write!(b, "0000000000 65535 f \r\n").unwrap();
    write!(b, "{:010} 00000 n \r\n", obj1_off).unwrap();
    write!(b, "{:010} 00000 n \r\n", obj2_off).unwrap();
    write!(b, "{:010} 00000 n \r\n", obj3_off).unwrap();
    write!(b, "{:010} 00000 n \r\n", obj4_off).unwrap();
    write!(b, "{:010} 00000 n \r\n", obj5_off).unwrap();
    // Trailer and startxref but NO %%EOF
    write!(
        b,
        "trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_off}\n"
    )
    .unwrap();
    b
}

fn build_pdf_missing_root() -> Vec<u8> {
    // Valid PDF structure but trailer has no /Root entry
    let mut b = Vec::new();
    use std::io::Write;
    writeln!(b, "%PDF-1.4").unwrap();

    let obj1_off = b.len();
    b.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let xref_off = b.len();
    write!(b, "xref\n0 2\n").unwrap();
    write!(b, "0000000000 65535 f \r\n").unwrap();
    write!(b, "{:010} 00000 n \r\n", obj1_off).unwrap();
    // Trailer WITHOUT /Root
    write!(b, "trailer\n<< /Size 2 >>\nstartxref\n{xref_off}\n%%EOF\n").unwrap();
    b
}

// ---------------------------------------------------------------------------
// Page::extract() single-pass API
// ---------------------------------------------------------------------------

/// Verify that extract() returns both spans and images in one call,
/// and that its convenience methods (text_lines, text) produce the
/// same output as the separate Page methods.
#[test]
fn test_page_extract_single_pass() {
    let mut doc = Document::open(corpus_path("inline_image.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let content = page.extract().unwrap();

    assert!(
        !content.spans.is_empty(),
        "extract() should return text spans"
    );
    assert!(!content.images.is_empty(), "extract() should return images");

    // Convenience methods should produce the same output as separate calls
    let text_from_extract = content.text();
    let lines_from_extract = content.text_lines();

    let mut page2 = doc.page(0).unwrap();
    let text_from_page = page2.text().unwrap();
    let mut page3 = doc.page(0).unwrap();
    let lines_from_page = page3.text_lines().unwrap();

    assert_eq!(
        text_from_extract, text_from_page,
        "PageContent::text() should match Page::text()"
    );
    assert_eq!(
        lines_from_extract.len(),
        lines_from_page.len(),
        "PageContent::text_lines() should produce same number of lines"
    );
    for (i, (from_extract, from_page)) in lines_from_extract
        .iter()
        .zip(lines_from_page.iter())
        .enumerate()
    {
        assert_eq!(
            from_extract.text(),
            from_page.text(),
            "line {} text mismatch between extract() and text_lines()",
            i
        );
    }
}

/// Verify that PageContent::into_text_lines() produces identical results
/// to PageContent::text_lines() (consuming variant avoids clone).
#[test]
fn test_page_content_into_text_lines() {
    let mut doc = Document::open(corpus_path("winansi_type1.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let content = page.extract().unwrap();

    // Clone to compare
    let lines_cloned = content.text_lines();
    let _text_before = content.text();

    // Consuming variant should produce identical results
    let lines_consumed = content.into_text_lines();
    assert_eq!(lines_cloned.len(), lines_consumed.len());
    for (i, (cloned, consumed)) in lines_cloned.iter().zip(lines_consumed.iter()).enumerate() {
        assert_eq!(
            cloned.text(),
            consumed.text(),
            "line {} mismatch between text_lines() and into_text_lines()",
            i
        );
    }
}

// ---------------------------------------------------------------------------
// Image extraction tests (I-003)
// ---------------------------------------------------------------------------

/// Inline image PDF: BI/ID/EI in content stream with text before and after.
/// Verifies that inline images are extracted and text is not corrupted.
#[test]
fn test_page_images_inline() {
    let mut doc = Document::open(corpus_path("inline_image.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();

    // Text extraction should still work (not corrupted by inline image data)
    let text = page.text().unwrap();
    assert!(
        text.contains("Hello"),
        "inline image PDF should contain 'Hello', got: {text}"
    );
    assert!(
        text.contains("World"),
        "inline image PDF should contain 'World', got: {text}"
    );

    // Re-acquire the page (page borrows &mut doc)
    let mut page = doc.page(0).unwrap();
    let images = page.images().unwrap();

    assert!(
        !images.is_empty(),
        "inline image PDF should have at least one image"
    );

    // Find the inline image
    let inline_imgs: Vec<&PageImage> = images.iter().filter(|img| img.inline).collect();
    assert_eq!(
        inline_imgs.len(),
        1,
        "should have exactly 1 inline image, got {}",
        inline_imgs.len()
    );

    let img = inline_imgs[0];
    assert_eq!(img.width, 2, "inline image width should be 2");
    assert_eq!(img.height, 2, "inline image height should be 2");
    assert_eq!(
        img.color_space, "DeviceRGB",
        "inline image color space should be DeviceRGB"
    );
    assert_eq!(img.bits_per_component, 8, "inline image BPC should be 8");
    // 2x2 RGB = 12 bytes of pixel data
    assert_eq!(
        img.data.len(),
        12,
        "inline image data should be 12 bytes (2x2x3), got {}",
        img.data.len()
    );
    assert_eq!(
        img.filter,
        ImageFilter::Raw,
        "inline image filter should be Raw"
    );
}

/// Text-only PDF should have no images.
#[test]
fn test_page_images_empty() {
    let mut doc = Document::open(corpus_path("winansi_type1.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let images = page.images().unwrap();
    assert!(
        images.is_empty(),
        "text-only PDF should have no images, got {}",
        images.len()
    );
}

/// Image XObject PDF: external image referenced via Do operator.
/// Verifies XObject images are extracted with inline=false.
#[test]
fn test_page_images_xobject() {
    let mut doc = Document::open(corpus_path("image_xobject.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();

    // Text should still be present
    let text = page.text().unwrap();
    assert!(
        text.contains("Image page"),
        "image xobject PDF should contain 'Image page', got: {text}"
    );

    // Re-acquire page for image extraction
    let mut page = doc.page(0).unwrap();
    let images = page.images().unwrap();

    assert!(
        !images.is_empty(),
        "image xobject PDF should have at least one image"
    );

    // Find the XObject image (inline=false)
    let xobj_imgs: Vec<&PageImage> = images.iter().filter(|img| !img.inline).collect();
    assert_eq!(
        xobj_imgs.len(),
        1,
        "should have exactly 1 XObject image, got {}",
        xobj_imgs.len()
    );

    let img = xobj_imgs[0];
    assert_eq!(img.width, 2, "XObject image width should be 2");
    assert_eq!(img.height, 2, "XObject image height should be 2");
    assert_eq!(
        img.color_space, "DeviceRGB",
        "XObject image color space should be DeviceRGB"
    );
    assert_eq!(img.bits_per_component, 8, "XObject image BPC should be 8");
    assert!(!img.inline, "XObject image should have inline=false");
    assert_eq!(
        img.filter,
        ImageFilter::Raw,
        "XObject image filter should be Raw"
    );
    // 2x2 RGB = 12 bytes
    assert_eq!(
        img.data.len(),
        12,
        "XObject image data should be 12 bytes (2x2x3), got {}",
        img.data.len()
    );
}

/// XObject image built from bytes (not from corpus file) to test
/// the from_bytes path with an Image XObject.
#[test]
fn test_page_images_xobject_from_bytes() {
    let pdf = build_pdf_image_xobject();
    let mut doc = Document::from_bytes(pdf).unwrap();
    let mut page = doc.page(0).unwrap();

    let images = page.images().unwrap();
    assert_eq!(images.len(), 1, "should have 1 image");
    assert!(!images[0].inline);
    assert_eq!(images[0].width, 1);
    assert_eq!(images[0].height, 1);
    assert_eq!(images[0].color_space, "DeviceGray");
}

// ---------------------------------------------------------------------------
// Multi-page reading order quality (P-003)
// ---------------------------------------------------------------------------

/// Verify reading order consistency: lines within each page should be
/// in top-to-bottom order (descending Y in PDF coordinates).
#[test]
fn test_reading_order_multipage() {
    let pdf = build_pdf_multiline_multipage();
    let mut doc = Document::from_bytes(pdf).unwrap();
    assert_eq!(doc.page_count(), 2);
    for i in 0..doc.page_count() {
        let mut page = doc.page(i).unwrap();
        let lines = page.text_lines().unwrap();
        assert!(
            lines.len() > 1,
            "page {i} should have multiple lines for ordering test, got {}",
            lines.len()
        );
        // Lines should be in top-to-bottom order (descending Y in PDF coords)
        for j in 1..lines.len() {
            assert!(
                lines[j - 1].baseline >= lines[j].baseline,
                "page {i}: lines not in top-to-bottom order \
                 (line {} y={:.1} < line {} y={:.1})",
                j - 1,
                lines[j - 1].baseline,
                j,
                lines[j].baseline
            );
        }
    }
}

/// Verify text extraction on the two-column corpus PDF.
/// This PDF has interleaved column order in the content stream (L1,R1,L2,R2).
/// With high coherence, we trust stream order (matching MuPDF ground truth),
/// so left and right column text appear on the same lines.
#[test]
fn test_reading_order_two_column() {
    let mut doc = Document::open(corpus_path("two_column.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines.len() >= 6,
        "two_column.pdf should produce at least 6 lines, got {}",
        lines.len()
    );
    // Content stream has interleaved columns. With coherent stream order,
    // we trust it and produce interleaved output where each line contains
    // text from both left and right columns (matching MuPDF behavior).
    // First line should have left-column "fox" and right-column "Village".
    assert!(
        lines[0].contains("fox") && lines[0].contains("Village"),
        "first line should contain both left and right column text: {:?}",
        lines[0]
    );
    // Last line should have content from both columns too.
    assert!(
        lines[5].contains("Snow") && lines[5].contains("Stars"),
        "sixth line should contain both left and right column text: {:?}",
        lines[5]
    );
}

/// Verify text extraction on the three-column corpus PDF.
/// Content stream has interleaved columns. With coherent stream order,
/// each output line contains text from all three columns.
#[test]
fn test_reading_order_three_column() {
    let mut doc = Document::open(corpus_path("three_column.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines.len() >= 6,
        "three_column.pdf should produce at least 6 lines, got {}",
        lines.len()
    );
    // Each line should contain text from all three columns (interleaved).
    assert!(
        lines[0].contains("first column")
            && lines[0].contains("second column")
            && lines[0].contains("third column"),
        "first line should have text from all three columns: {:?}",
        lines[0]
    );
    // Last line should contain summary text from each column.
    assert!(
        lines[5].contains("summary of part one") && lines[5].contains("Column two"),
        "last line should have text from multiple columns: {:?}",
        lines[5]
    );
}

/// Verify rotated text is extracted from the rotated text corpus PDF.
/// Horizontal text should appear before rotated text.
#[test]
fn test_reading_order_rotated_text() {
    let mut doc = Document::open(corpus_path("rotated_text.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines.len() >= 4,
        "rotated_text.pdf should produce at least 4 lines, got {}",
        lines.len()
    );
    // Horizontal lines come first, rotated lines after.
    let horiz_last = lines.iter().position(|l| l.contains("line three"));
    let rotated_first = lines.iter().position(|l| l.contains("Rotated"));
    assert!(
        horiz_last.is_some() && rotated_first.is_some(),
        "expected horizontal and rotated content in output: {text:?}"
    );
    assert!(
        horiz_last.unwrap() < rotated_first.unwrap(),
        "horizontal text should appear before rotated text"
    );
}

/// Verify table layout PDF reads rows together (not columns).
/// Each row's label and value should appear on the same line.
#[test]
fn test_reading_order_table_layout() {
    let mut doc = Document::open(corpus_path("table_layout.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines.len() >= 5,
        "table_layout.pdf should produce at least 5 lines, got {}",
        lines.len()
    );
    // Row-by-row ordering: each row should have its data together.
    // Header row should have "Name" and "Score" on the same line.
    let header = lines.iter().find(|l| l.contains("Name"));
    assert!(
        header.is_some() && header.unwrap().contains("Score"),
        "header row should contain both 'Name' and 'Score': {text:?}"
    );
    // Data row: "Alice" should be on the same line as "London".
    let alice_row = lines.iter().find(|l| l.contains("Alice"));
    assert!(
        alice_row.is_some() && alice_row.unwrap().contains("London"),
        "Alice row should contain 'London': {text:?}"
    );
}

/// Verify image extraction returns empty for text-only multi-page PDF.
#[test]
fn test_images_empty_on_multipage() {
    let mut doc = Document::open(corpus_path("multipage.pdf")).unwrap();
    for i in 0..doc.page_count() {
        let mut page = doc.page(i).unwrap();
        let images = page.images().unwrap();
        assert!(
            images.is_empty(),
            "multipage.pdf page {i} should have no images, got {}",
            images.len()
        );
    }
}

/// Verify that raw_spans() returns positioned spans on a real corpus PDF.
/// This exercises the escape hatch API for downstream reading order.
#[test]
fn test_raw_spans_positioning() {
    let mut doc = Document::open(corpus_path("xelatex.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let spans = page.raw_spans().unwrap();
    assert!(
        spans.len() > 5,
        "xelatex.pdf page 0 should have many spans, got {}",
        spans.len()
    );
    // Verify spans have non-zero positions and sizes
    for span in &spans {
        assert!(!span.text.is_empty(), "span should have non-empty text");
        assert!(
            span.font_size > 0.0,
            "span should have positive font size, got {}",
            span.font_size
        );
    }
}

/// Verify the new image corpus PDFs are accessible and produce text via
/// the full pipeline. Text and images should coexist without corruption.
#[test]
fn test_corpus_image_pdfs_text_not_corrupted() {
    // inline_image.pdf has text + inline image
    let mut doc = Document::open(corpus_path("inline_image.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    assert!(
        text.contains("Hello") && text.contains("World"),
        "inline_image.pdf text should contain 'Hello' and 'World', got: {text}"
    );

    // image_xobject.pdf has text + XObject image
    let mut doc = Document::open(corpus_path("image_xobject.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    assert!(
        text.contains("Image page"),
        "image_xobject.pdf text should contain 'Image page', got: {text}"
    );
}

/// Verify diagnostics are emitted when extracting images from image PDFs.
/// Font loading info should still appear alongside image extraction.
#[test]
fn test_image_extraction_with_diagnostics() {
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    let mut doc = Document::open_with_config(corpus_path("inline_image.pdf"), config).unwrap();

    let mut page = doc.page(0).unwrap();
    let _text = page.text().unwrap();

    let mut page = doc.page(0).unwrap();
    let images = page.images().unwrap();

    // Should have the inline image
    assert!(!images.is_empty(), "should have inline image");

    // Should have font loading diagnostics
    let warnings = diag.warnings();
    assert!(
        warnings.iter().any(|w| w.kind == WarningKind::FontLoaded),
        "expected FontLoaded diagnostic from image PDF"
    );
}

/// Verify that images inside Form XObjects are extracted by Page::images().
/// A Form XObject's content stream contains an inline image (BI/ID/EI).
/// The page's content stream invokes the Form XObject via /Fm1 Do.
#[test]
fn test_page_images_from_form_xobject() {
    let pdf = build_pdf_form_xobject_with_inline_image();
    let mut doc = Document::from_bytes(pdf).unwrap();
    let mut page = doc.page(0).unwrap();

    let images = page.images().unwrap();
    assert!(
        !images.is_empty(),
        "Form XObject with inline image should produce at least one image"
    );
    let inline_imgs: Vec<&PageImage> = images.iter().filter(|img| img.inline).collect();
    assert_eq!(
        inline_imgs.len(),
        1,
        "should have exactly 1 inline image from Form XObject, got {}",
        inline_imgs.len()
    );
    let img = inline_imgs[0];
    assert_eq!(
        img.width, 2,
        "inline image from Form XObject width should be 2"
    );
    assert_eq!(
        img.height, 2,
        "inline image from Form XObject height should be 2"
    );
}

/// Verify that a malformed inline image with absurdly large claimed
/// dimensions does not hang or OOM. The parser should skip it gracefully.
#[test]
fn test_inline_image_absurd_dimensions_no_hang() {
    use std::time::Instant;

    // Build a minimal PDF with BI claiming 999999x999999 but only 4 bytes of data
    let pdf = build_pdf_inline_image_absurd_dims();

    let start = Instant::now();
    let mut doc = Document::from_bytes(pdf).unwrap();
    let mut page = doc.page(0).unwrap();
    // Text extraction (which triggers content stream parsing including BI/ID/EI)
    let _text = page.text().unwrap_or_default();
    let mut page = doc.page(0).unwrap();
    let images = page.images().unwrap_or_default();

    assert!(
        start.elapsed().as_secs() < 5,
        "parsing should not hang on absurd inline image dimensions"
    );
    // Structural check: any extracted image data should be small (the PDF
    // only has 4 bytes of actual data, regardless of claimed dimensions).
    for img in &images {
        assert!(
            img.data.len() < 1024,
            "image data should be small, got {} bytes",
            img.data.len()
        );
    }
}

// -- Helper for building image test PDFs from bytes --

fn build_pdf_form_xobject_with_inline_image() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    // Form XObject (obj 6) whose content stream contains an inline image.
    // 2x2 RGB = 12 bytes of pixel data.
    let mut form_content: Vec<u8> = Vec::new();
    form_content.extend_from_slice(b"BI /W 2 /H 2 /CS /RGB /BPC 8 ID ");
    form_content.extend_from_slice(&[0xFF; 12]);
    form_content.extend_from_slice(b"\nEI\n");
    b.add_stream_object(
        6,
        "/Type /XObject /Subtype /Form /BBox [0 0 100 100]",
        &form_content,
    );

    // Page content: invoke the Form XObject via Do
    b.add_stream_object(3, "", b"/Fm1 Do");

    b.add_object(
        5,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /XObject << /Fm1 6 0 R >> >> >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [5 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

fn build_pdf_inline_image_absurd_dims() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // Content stream with an inline image claiming 999999x999999 but only 4 bytes of data.
    let mut content: Vec<u8> = Vec::new();
    content.extend_from_slice(b"BT /F1 12 Tf 100 700 Td (Before) Tj ET\n");
    content.extend_from_slice(b"BI /W 999999 /H 999999 /CS /G /BPC 8 ID ");
    content.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
    content.extend_from_slice(b"\nEI\n");
    content.extend_from_slice(b"BT /F1 12 Tf 100 680 Td (After) Tj ET\n");
    b.add_stream_object(3, "", &content);

    b.add_object(
        5,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [5 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

fn build_pdf_image_xobject() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    // 1x1 DeviceGray image (1 byte)
    b.add_stream_object(
        7,
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
        &[0x80],
    );

    b.add_stream_object(6, "", b"/Im1 Do");

    b.add_object(5, b"<< /XObject << /Im1 7 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

// -- Helpers for review-fix tests --

/// Two-page PDF where each page has 3 lines at distinct Y positions.
fn build_pdf_multiline_multipage() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // Page 1: 3 lines
    b.add_stream_object(
        10,
        "",
        b"BT /F1 12 Tf 72 700 Td (Page 1 Line 1) Tj 0 -20 Td (Page 1 Line 2) Tj 0 -20 Td (Page 1 Line 3) Tj ET",
    );
    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        6,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 10 0 R /Resources 5 0 R >>",
    );

    // Page 2: 3 lines
    b.add_stream_object(
        11,
        "",
        b"BT /F1 12 Tf 72 700 Td (Page 2 Line 1) Tj 0 -20 Td (Page 2 Line 2) Tj 0 -20 Td (Page 2 Line 3) Tj ET",
    );
    b.add_object(7, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        8,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 11 0 R /Resources 7 0 R >>",
    );

    b.add_object(2, b"<< /Type /Pages /Kids [6 0 R 8 0 R] /Count 2 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF with an Image XObject using /Filter /DCTDecode (JPEG passthrough).
/// Uses a minimal valid JPEG structure (not a real image, just enough to be
/// recognized as JPEG by the filter classification logic).
fn build_pdf_jpeg_image_xobject() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    // Minimal JPEG: SOI + APP0 header + EOI.
    // Not a valid decodable image, but the library passes JPEG through without decoding.
    let jpeg_data: Vec<u8> = vec![
        0xFF, 0xD8, // SOI (Start of Image)
        0xFF, 0xE0, // APP0 marker
        0x00, 0x10, // Length = 16
        b'J', b'F', b'I', b'F', 0x00, // "JFIF\0"
        0x01, 0x01, // Version 1.1
        0x00, // Units: no units
        0x00, 0x01, // X density = 1
        0x00, 0x01, // Y density = 1
        0x00, 0x00, // Thumbnail: 0x0
        0xFF, 0xD9, // EOI (End of Image)
    ];

    b.add_stream_object(
        7,
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode",
        &jpeg_data,
    );

    b.add_stream_object(6, "", b"/Im1 Do");

    b.add_object(5, b"<< /XObject << /Im1 7 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF with an Image XObject placed at a known CTM position (100, 200).
fn build_pdf_image_at_position() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    // 1x1 DeviceGray image
    b.add_stream_object(
        7,
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
        &[0x80],
    );

    // Content stream: translate to (100, 200), then paint image
    b.add_stream_object(6, "", b"q 1 0 0 1 100 200 cm /Im1 Do Q");

    b.add_object(5, b"<< /XObject << /Im1 7 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF with both an inline image and an XObject image on the same page.
fn build_pdf_mixed_images() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // XObject image: 1x1 DeviceGray
    b.add_stream_object(
        7,
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
        &[0x80],
    );

    // Content stream: text + inline image + XObject image
    let mut content: Vec<u8> = Vec::new();
    content.extend_from_slice(b"BT /F1 12 Tf 72 700 Td (Text) Tj ET\n");
    // Inline image: 2x2 RGB = 12 bytes
    content.extend_from_slice(b"BI /W 2 /H 2 /CS /RGB /BPC 8 ID ");
    content.extend_from_slice(&[0xFF; 12]);
    content.extend_from_slice(b"\nEI\n");
    // XObject image via Do
    content.extend_from_slice(b"/Im1 Do\n");

    b.add_stream_object(6, "", &content);

    b.add_object(5, b"<< /Font << /F1 4 0 R >> /XObject << /Im1 7 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

// ---------------------------------------------------------------------------
// Review fix tests (B1, C1-C5, G3)
// ---------------------------------------------------------------------------

/// extract() on an empty page should return empty PageContent (C4).
#[test]
fn test_page_extract_empty() {
    let mut doc = Document::open(corpus_path("empty_page.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let content = page.extract().unwrap();
    assert!(content.spans.is_empty(), "empty page should have no spans");
    assert!(
        content.images.is_empty(),
        "empty page should have no images"
    );
    assert!(content.text().is_empty(), "empty page text should be empty");
    assert!(
        content.text_lines().is_empty(),
        "empty page should have no text lines"
    );
}

/// JPEG (DCTDecode) filter on an Image XObject should be classified as
/// ImageFilter::Jpeg, not Raw (C1).
#[test]
fn test_page_images_jpeg_filter() {
    let pdf = build_pdf_jpeg_image_xobject();
    let mut doc = Document::from_bytes(pdf).unwrap();
    let mut page = doc.page(0).unwrap();
    let images = page.images().unwrap();

    assert_eq!(images.len(), 1, "should have 1 image");
    let img = &images[0];
    assert_eq!(
        img.filter,
        ImageFilter::Jpeg,
        "DCTDecode image should have Jpeg filter, got {:?}",
        img.filter
    );
    assert!(!img.inline, "XObject image should not be inline");
    assert_eq!(img.width, 1);
    assert_eq!(img.height, 1);
    // Data should be the raw JPEG bytes (passed through, not decoded)
    assert!(
        img.data.len() > 2,
        "JPEG data should have some bytes, got {}",
        img.data.len()
    );
}

/// Image XObject position should reflect the CTM translation (C3).
#[test]
fn test_page_image_position() {
    let pdf = build_pdf_image_at_position();
    let mut doc = Document::from_bytes(pdf).unwrap();
    let mut page = doc.page(0).unwrap();
    let images = page.images().unwrap();

    assert_eq!(images.len(), 1, "should have 1 image");
    let img = &images[0];
    assert!(
        (img.x - 100.0).abs() < 0.1,
        "image x should be ~100.0, got {}",
        img.x
    );
    assert!(
        (img.y - 200.0).abs() < 0.1,
        "image y should be ~200.0, got {}",
        img.y
    );
}

/// Page with both inline and XObject images should extract both (C5).
#[test]
fn test_page_images_mixed() {
    let pdf = build_pdf_mixed_images();
    let mut doc = Document::from_bytes(pdf).unwrap();
    let mut page = doc.page(0).unwrap();
    let content = page.extract().unwrap();

    // Should have text
    let text = content.text();
    assert!(text.contains("Text"), "should contain 'Text', got: {text}");

    // Should have both image types
    let inline_imgs: Vec<_> = content.images.iter().filter(|i| i.inline).collect();
    let xobject_imgs: Vec<_> = content.images.iter().filter(|i| !i.inline).collect();
    assert_eq!(
        inline_imgs.len(),
        1,
        "should have 1 inline image, got {}",
        inline_imgs.len()
    );
    assert_eq!(
        xobject_imgs.len(),
        1,
        "should have 1 XObject image, got {}",
        xobject_imgs.len()
    );

    // Inline: 2x2 RGB
    assert_eq!(inline_imgs[0].width, 2);
    assert_eq!(inline_imgs[0].height, 2);
    assert_eq!(inline_imgs[0].color_space, "DeviceRGB");

    // XObject: 1x1 DeviceGray
    assert_eq!(xobject_imgs[0].width, 1);
    assert_eq!(xobject_imgs[0].height, 1);
    assert_eq!(xobject_imgs[0].color_space, "DeviceGray");
}

/// Inline image data size limit should prevent memory exhaustion (P1).
#[test]
fn test_inline_image_data_size_limit() {
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());

    // Build a PDF with a very large inline image (data > 4 MB)
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    let mut content: Vec<u8> = Vec::new();
    content.extend_from_slice(b"BT /F1 12 Tf 72 700 Td (Before) Tj ET\n");
    content.extend_from_slice(b"BI /W 2049 /H 2048 /CS /G /BPC 8 ID ");
    // 2049*2048 = 4196352 bytes, exceeds 4 MB (4194304) limit
    content.extend_from_slice(&vec![0xAA; 2049 * 2048]);
    content.extend_from_slice(b"\nEI\n");
    content.extend_from_slice(b"BT /F1 12 Tf 72 680 Td (After) Tj ET\n");
    b.add_stream_object(3, "", &content);

    b.add_object(
        5,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [5 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let mut doc = Document::from_bytes_with_config(pdf, config).unwrap();
    let mut page = doc.page(0).unwrap();
    let content = page.extract().unwrap();

    // Text should still be extracted (parser continues after skipping oversized image)
    let text = content.text();
    assert!(
        text.contains("Before"),
        "text before oversized image should be extracted"
    );
    assert!(
        text.contains("After"),
        "text after oversized image should be extracted"
    );

    // The oversized inline image should be skipped
    assert!(
        content.images.is_empty(),
        "oversized inline image should be skipped, got {} images",
        content.images.len()
    );

    // Should have emitted a warning about the size limit
    let warnings = diag.warnings();
    assert!(
        warnings
            .iter()
            .any(|w| w.kind == WarningKind::InvalidImageMetadata
                && w.message.contains("exceeds limit")),
        "should warn about inline image data size exceeding limit"
    );
}

// ---------------------------------------------------------------------------
// Password-on-unencrypted warning
// ---------------------------------------------------------------------------

#[test]
fn test_password_on_unencrypted_warns() {
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default()
        .with_password(b"unused".to_vec())
        .with_diagnostics(diag.clone());

    let path = corpus_path("winansi_type1.pdf");
    let mut doc = Document::open_with_config(path, config).unwrap();
    let mut page = doc.page(0).unwrap();
    let _ = page.text();

    let warnings = diag.warnings();
    assert!(
        warnings.iter().any(
            |w| w.kind == WarningKind::EncryptedDocument && w.message.contains("not encrypted")
        ),
        "expected warning about password on unencrypted doc, got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Send+Sync static assertions
// ---------------------------------------------------------------------------

#[test]
fn test_document_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<Document>();
}

#[test]
fn test_config_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Config>();
}

// ---------------------------------------------------------------------------
// PageContent::into_text() coverage
// ---------------------------------------------------------------------------

#[test]
fn test_page_content_into_text() {
    let mut doc = Document::open(corpus_path("winansi_type1.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let content = page.extract().unwrap();

    // Get text via text() for comparison
    let text_ref = content.text();

    // Re-extract since into_text consumes
    let mut page2 = doc.page(0).unwrap();
    let content2 = page2.extract().unwrap();
    let text_consumed = content2.into_text();

    assert_eq!(
        text_ref, text_consumed,
        "into_text() should produce the same result as text()"
    );
}

// ---------------------------------------------------------------------------
// Config::with_decode_limits() coverage
// ---------------------------------------------------------------------------

#[test]
fn test_config_with_decode_limits() {
    use udoc_pdf::object::stream::DecodeLimits;

    let mut limits = DecodeLimits::default();
    limits.max_decompressed_size = 1024;
    let config = Config::default().with_decode_limits(limits);
    assert_eq!(config.decode_limits.max_decompressed_size, 1024);

    // Should still open a PDF fine (our test PDFs are small)
    let mut doc = Document::open_with_config(corpus_path("winansi_type1.pdf"), config).unwrap();
    let mut page = doc.page(0).unwrap();
    // Text extraction may work or fail depending on stream sizes,
    // but the config option should be accepted
    let _ = page.text();
}

// ---------------------------------------------------------------------------
// Document::from_bytes_with_password() coverage
// ---------------------------------------------------------------------------

#[test]
fn test_from_bytes_with_password_on_unencrypted() {
    let data = std::fs::read(corpus_path("winansi_type1.pdf")).unwrap();
    // Calling with a password on an unencrypted PDF should still work (with a warning)
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default()
        .with_password(b"password")
        .with_diagnostics(diag.clone());
    let mut doc = Document::from_bytes_with_config(data, config).unwrap();
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    assert!(text.contains("Hello"));

    let warnings = diag.warnings();
    assert!(
        warnings
            .iter()
            .any(|w| w.kind == WarningKind::EncryptedDocument),
        "expected EncryptedDocument warning for password on unencrypted PDF"
    );
}

// ---------------------------------------------------------------------------
// Page tree: unknown /Type skips node
// ---------------------------------------------------------------------------

#[test]
fn test_page_tree_unknown_type_skipped() {
    // Build a PDF where a /Kids entry has /Type /Foo (unknown type)
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Real page) Tj ET");
    b.add_object(
        6,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    // Node with unknown /Type (not /Page or /Pages)
    b.add_object(7, b"<< /Type /Foo /SomeKey /SomeValue >>");
    b.add_object(2, b"<< /Type /Pages /Kids [7 0 R 6 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let mut doc = Document::from_bytes(pdf).unwrap();
    // Node with /Type /Foo should be skipped, leaving only the real page
    assert_eq!(doc.page_count(), 1);
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    assert!(
        text.contains("Real page"),
        "should extract text from the valid page"
    );
}

// ---------------------------------------------------------------------------
// Page tree: /Kids with non-reference entries warns
// ---------------------------------------------------------------------------

#[test]
fn test_page_tree_non_reference_kids_warns() {
    // Build a PDF where /Kids contains a non-reference (integer) entry
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Valid page) Tj ET");
    b.add_object(
        6,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    // /Kids has an integer (42) and a valid reference
    b.add_object(2, b"<< /Type /Pages /Kids [42 6 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    let doc = Document::from_bytes_with_config(pdf, config).unwrap();
    // Should still have the valid page
    assert_eq!(doc.page_count(), 1);

    let warnings = diag.warnings();
    assert!(
        warnings
            .iter()
            .any(|w| w.kind == WarningKind::InvalidPageTree
                && w.message.contains("instead of a reference")),
        "expected InvalidPageTree warning about non-reference /Kids entry, got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Page tree: no /Type heuristic (Pages node without /Type but has /Kids)
// ---------------------------------------------------------------------------

#[test]
fn test_page_tree_no_type_with_kids_treated_as_pages() {
    // Build a PDF where a Pages node has no /Type but has /Kids
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    b.add_object(
        6,
        b"<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    // Intermediate Pages node WITHOUT /Type but WITH /Kids
    b.add_object(3, b"<< /Kids [6 0 R] /Count 1 >>");
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let mut doc = Document::from_bytes(pdf).unwrap();
    assert_eq!(doc.page_count(), 1);
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    assert!(text.contains("Hello"));
}

// ---------------------------------------------------------------------------
// Page tree: no /Type, no /Kids treated as leaf page
// ---------------------------------------------------------------------------

#[test]
fn test_page_tree_no_type_no_kids_treated_as_page() {
    // A node with no /Type and no /Kids should be treated as a leaf page
    let mut b = PdfBuilder::new("1.4");
    // Node 3 has no /Type and no /Kids (just has /MediaBox, treated as leaf)
    b.add_object(3, b"<< /Parent 2 0 R /MediaBox [0 0 612 792] >>");
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let doc = Document::from_bytes(pdf).unwrap();
    // Should treat node 3 as a leaf page
    assert_eq!(doc.page_count(), 1);
}

// ---------------------------------------------------------------------------
// Page tree: cycle detection
// ---------------------------------------------------------------------------

#[test]
fn test_page_tree_cycle_warns() {
    // Build a PDF where a Pages node has itself as a kid (cycle)
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    b.add_object(
        6,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    // Pages node that references itself AND a valid page
    b.add_object(2, b"<< /Type /Pages /Kids [2 0 R 6 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    let doc = Document::from_bytes_with_config(pdf, config).unwrap();
    // The cycle should be detected; the valid page should still be collected
    assert_eq!(doc.page_count(), 1);

    let warnings = diag.warnings();
    assert!(
        warnings
            .iter()
            .any(|w| w.kind == WarningKind::PageTreeCycle),
        "expected PageTreeCycle warning, got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// setup_encryption: /Encrypt is not a dict or reference
// ---------------------------------------------------------------------------

#[test]
fn test_encrypt_entry_not_dict_or_ref() {
    // Build PDF where /Encrypt is an integer (invalid)
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    b.add_object(
        6,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [6 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish_with_trailer("/Encrypt 42", 1);

    let result = Document::from_bytes(pdf);
    match result {
        Err(err) => {
            let err_msg = format!("{err}");
            assert!(
                err_msg.contains("dictionary")
                    || err_msg.contains("reference")
                    || err_msg.contains("Encrypt"),
                "error should mention dictionary/reference issue, got: {err_msg}"
            );
        }
        Ok(_) => panic!("PDF with /Encrypt as integer should fail"),
    }
}

// ---------------------------------------------------------------------------
// join_text_lines: empty input (via Page::text on empty page)
// ---------------------------------------------------------------------------

#[test]
fn test_page_text_empty_returns_empty_string() {
    let mut doc = Document::open(corpus_path("empty_page.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    assert!(text.is_empty(), "empty page text should be empty string");
}

// ---------------------------------------------------------------------------
// Deeply nested page tree
// ---------------------------------------------------------------------------

#[test]
fn test_deeply_nested_page_tree() {
    // Build a PDF with several layers of /Pages nesting
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(10, "", b"BT /F1 12 Tf 100 700 Td (Deep page) Tj ET");
    b.add_object(
        11,
        b"<< /Type /Page /Parent 9 0 R /MediaBox [0 0 612 792] /Contents 10 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    // Nested /Pages: 9 -> 8 -> 7 -> 6 -> 5 -> [11]
    b.add_object(5, b"<< /Type /Pages /Kids [11 0 R] /Count 1 >>");
    b.add_object(6, b"<< /Type /Pages /Kids [5 0 R] /Count 1 >>");
    b.add_object(7, b"<< /Type /Pages /Kids [6 0 R] /Count 1 >>");
    b.add_object(8, b"<< /Type /Pages /Kids [7 0 R] /Count 1 >>");
    b.add_object(9, b"<< /Type /Pages /Kids [8 0 R] /Count 1 >>");
    b.add_object(2, b"<< /Type /Pages /Kids [9 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let mut doc = Document::from_bytes(pdf).unwrap();
    assert_eq!(doc.page_count(), 1);
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    assert!(
        text.contains("Deep page"),
        "deeply nested page should still produce text"
    );
}

// ---------------------------------------------------------------------------
// Document::open_with_password convenience method
// ---------------------------------------------------------------------------

#[test]
fn test_open_with_password_on_unencrypted() {
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default()
        .with_password(b"pass")
        .with_diagnostics(diag.clone());
    let doc = Document::open_with_config(corpus_path("winansi_type1.pdf"), config).unwrap();
    assert!(doc.page_count() > 0);

    let warnings = diag.warnings();
    assert!(
        warnings
            .iter()
            .any(|w| w.kind == WarningKind::EncryptedDocument),
        "expected password-on-unencrypted warning"
    );
}

// ---------------------------------------------------------------------------
// Page tree: catalog missing /Pages
// ---------------------------------------------------------------------------

#[test]
fn test_catalog_missing_pages() {
    let mut b = PdfBuilder::new("1.4");
    // Catalog without /Pages
    b.add_object(1, b"<< /Type /Catalog >>");
    let pdf = b.finish(1);

    let result = Document::from_bytes(pdf);
    match result {
        Err(err) => {
            let err_msg = format!("{err}");
            assert!(
                err_msg.contains("Pages") || err_msg.contains("pages"),
                "error should mention missing /Pages, got: {err_msg}"
            );
        }
        Ok(_) => panic!("PDF with catalog missing /Pages should fail"),
    }
}

// ---------------------------------------------------------------------------
// Page::extract empty PageContent text convenience methods
// ---------------------------------------------------------------------------

#[test]
fn test_page_content_empty_text_methods() {
    // Use empty page from corpus to get an empty PageContent
    let mut doc = Document::open(corpus_path("empty_page.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let content = page.extract().unwrap();
    assert!(content.text().is_empty());
    assert!(content.text_lines().is_empty());
}

#[test]
fn test_page_content_empty_into_text_and_lines() {
    let mut doc = Document::open(corpus_path("empty_page.pdf")).unwrap();
    let mut page = doc.page(0).unwrap();
    let content = page.extract().unwrap();
    assert!(content.into_text().is_empty());

    // Also test into_text_lines on empty content
    let mut page2 = doc.page(0).unwrap();
    let content2 = page2.extract().unwrap();
    assert!(content2.into_text_lines().is_empty());
}

// ---------------------------------------------------------------------------
// Tier selection integration tests (T2-TIER)
// ---------------------------------------------------------------------------

/// Build a single-column PDF with coherent top-to-bottom stream order.
/// Tier 1 should be selected (stream order coherence > threshold).
fn build_pdf_single_column_coherent() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    // Multiple lines in coherent top-to-bottom order. Each line is narrow
    // relative to the page width so they won't be classified as full-width.
    // We add a short "tag" span far right on the first line to widen the bbox.
    let content = b"BT /F1 12 Tf \
        72 700 Td (First paragraph line) Tj \
        328 0 Td (ref) Tj \
        -328 -20 Td (Second paragraph line) Tj \
        0 -20 Td (Third paragraph line) Tj \
        0 -20 Td (Fourth paragraph line) Tj \
        0 -20 Td (Fifth paragraph line) Tj \
        ET";
    b.add_stream_object(3, "", content);
    b.add_object(
        5,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [5 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// Build a two-column PDF. Content stream order: left column first (top
/// to bottom), then right column (top to bottom). The columns are well
/// separated so X-Y cut should detect them. Tier 2 should fire because
/// X-Y cut finds 2 partitions.
fn build_pdf_two_column() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    // Left column at x=72, right column at x=340, each with 6 lines.
    let content = b"BT /F1 10 Tf \
        72 700 Td (Left col line one) Tj \
        0 -16 Td (Left col line two) Tj \
        0 -16 Td (Left col line three) Tj \
        0 -16 Td (Left col line four) Tj \
        0 -16 Td (Left col line five) Tj \
        0 -16 Td (Left col line six) Tj \
        268 96 Td (Right col line one) Tj \
        0 -16 Td (Right col line two) Tj \
        0 -16 Td (Right col line three) Tj \
        0 -16 Td (Right col line four) Tj \
        0 -16 Td (Right col line five) Tj \
        0 -16 Td (Right col line six) Tj \
        ET";
    b.add_stream_object(3, "", content);
    b.add_object(
        5,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [5 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

#[test]
fn test_tier_selection_single_column_coherent() {
    let pdf = build_pdf_single_column_coherent();
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    let mut doc = Document::from_bytes_with_config(pdf, config).unwrap();
    let mut page = doc.page(0).unwrap();
    let _text = page.text().unwrap();

    let tier_msgs: Vec<_> = diag
        .warnings()
        .into_iter()
        .filter(|w| w.kind == WarningKind::TierSelection)
        .collect();
    assert_eq!(tier_msgs.len(), 1, "expected one TierSelection diagnostic");
    // Single-column with coherent stream order and X-Y cut finding 1 partition
    // should select Tier 1.
    assert!(
        tier_msgs[0].message.contains("tier=1"),
        "single-column coherent PDF should select Tier 1, got: {}",
        tier_msgs[0].message
    );
    assert_eq!(tier_msgs[0].level, WarningLevel::Info);
    assert_eq!(tier_msgs[0].context.page_index, Some(0));
}

#[test]
fn test_tier_selection_two_column() {
    let pdf = build_pdf_two_column();
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    let mut doc = Document::from_bytes_with_config(pdf, config).unwrap();
    let mut page = doc.page(0).unwrap();
    let _text = page.text().unwrap();

    let tier_msgs: Vec<_> = diag
        .warnings()
        .into_iter()
        .filter(|w| w.kind == WarningKind::TierSelection)
        .collect();
    assert_eq!(tier_msgs.len(), 1, "expected one TierSelection diagnostic");
    // Two-column PDF with coherent stream order: stream-order column
    // detection finds the Y-jump between columns and processes each column
    // independently within Tier 1 (no X-Y cut needed).
    assert!(
        tier_msgs[0].message.contains("tier=1"),
        "two-column coherent PDF should use Tier 1 stream-order columns, got: {}",
        tier_msgs[0].message
    );
    assert_eq!(tier_msgs[0].level, WarningLevel::Info);
    assert_eq!(tier_msgs[0].context.page_index, Some(0));
}

// -- Resource inheritance tests --

/// Build a PDF where /Resources is on the /Pages node, not on the page itself.
/// This tests the /Parent chain walk in resolve_page_resources.
fn build_pdf_inherited_resources() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    let content = b"BT /F1 12 Tf 72 700 Td (Inherited resources) Tj ET";
    b.add_stream_object(3, "", content);
    // Page has NO /Resources, only /Parent pointing to /Pages node
    b.add_object(
        5,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R >>",
    );
    // /Pages node carries /Resources (inherited by all child pages)
    b.add_object(
        2,
        b"<< /Type /Pages /Kids [5 0 R] /Count 1 /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

#[test]
fn test_inherited_resources_from_pages_node() {
    let pdf = build_pdf_inherited_resources();
    let mut doc = Document::from_bytes(pdf).unwrap();
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    assert!(
        text.contains("Inherited resources"),
        "should extract text using inherited /Resources, got: {text:?}"
    );
}

/// Build a PDF where /Resources is on a grandparent /Pages node (two levels up).
fn build_pdf_grandparent_resources() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    let content = b"BT /F1 12 Tf 72 700 Td (Grandparent resources) Tj ET";
    b.add_stream_object(3, "", content);
    // Page: no /Resources
    b.add_object(
        5,
        b"<< /Type /Page /Parent 6 0 R /MediaBox [0 0 612 792] /Contents 3 0 R >>",
    );
    // Intermediate /Pages: no /Resources
    b.add_object(
        6,
        b"<< /Type /Pages /Parent 2 0 R /Kids [5 0 R] /Count 1 >>",
    );
    // Root /Pages: has /Resources
    b.add_object(
        2,
        b"<< /Type /Pages /Kids [6 0 R] /Count 1 /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

#[test]
fn test_inherited_resources_from_grandparent() {
    let pdf = build_pdf_grandparent_resources();
    let mut doc = Document::from_bytes(pdf).unwrap();
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    assert!(
        text.contains("Grandparent resources"),
        "should extract text using grandparent /Resources, got: {text:?}"
    );
}

#[test]
fn test_page_resources_override_parent() {
    // Page has its own /Resources, parent also has /Resources.
    // Page's /Resources should take precedence.
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_object(7, b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>");
    let content = b"BT /F1 12 Tf 72 700 Td (Page resources win) Tj ET";
    b.add_stream_object(3, "", content);
    // Page has /Resources with /F1 -> Helvetica
    b.add_object(
        5,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    // Parent has /Resources with /F1 -> Courier (should NOT be used)
    b.add_object(
        2,
        b"<< /Type /Pages /Kids [5 0 R] /Count 1 /Resources << /Font << /F1 7 0 R >> >> >>",
    );
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let mut doc = Document::from_bytes(pdf).unwrap();
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    assert!(
        text.contains("Page resources win"),
        "page /Resources should override parent, got: {text:?}"
    );
}

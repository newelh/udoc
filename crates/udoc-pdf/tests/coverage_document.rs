//! Targeted coverage tests for src/document.rs.
//!
//! Exercises annotation error paths, extract_all(), open_with_password(),
//! resource limit guards, and missing /ID in encrypted documents.

mod common;

use std::sync::Arc;

use common::PdfBuilder;
use udoc_pdf::{CollectingDiagnostics, Config, Document, WarningKind};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a minimal valid single-page PDF with a given page dict body.
/// Returns the raw PDF bytes.
///
/// Layout:
///   obj 1 = Catalog (/Pages -> 2 0 R)
///   obj 2 = Pages (/Kids [3 0 R])
///   obj 3 = Page (caller-supplied body merged with required fields)
///   obj 4 = Font (Helvetica)
///   obj 5 = content stream (empty by default)
fn build_single_page_pdf(page_extra: &str) -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    let page_body = format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> {} >>",
        page_extra
    );
    b.add_object(3, page_body.as_bytes());
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// Open a PDF from bytes with collecting diagnostics, returning (doc, diag).
fn open_with_diag(data: Vec<u8>) -> (Document, Arc<CollectingDiagnostics>) {
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    let doc = Document::from_bytes_with_config(data, config).expect("document should open");
    (doc, diag)
}

// ---------------------------------------------------------------------------
// 1. Annotation error paths
// ---------------------------------------------------------------------------

/// /Annots is not an array (it's a string). Should warn and produce no annotation spans.
#[test]
fn annots_not_array_warns() {
    let pdf = build_single_page_pdf("/Annots (not an array)");
    let (mut doc, diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    // Should not crash, text extraction still works.
    let _text = page.text().unwrap();

    let warnings = diag.warnings();
    assert!(
        warnings.iter().any(|w| {
            w.kind == WarningKind::InvalidState
                && w.message.contains("annotations")
                && w.message.contains("not an array")
        }),
        "expected annotation warning about non-array /Annots, got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

/// /Annots is an integer (wrong type entirely). Should warn via the catch-all arm.
#[test]
fn annots_wrong_type_warns() {
    let pdf = build_single_page_pdf("/Annots 42");
    let (mut doc, diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    let _text = page.text().unwrap();

    let warnings = diag.warnings();
    assert!(
        warnings.iter().any(|w| {
            w.kind == WarningKind::InvalidState
                && w.message.contains("annotations")
                && w.message.contains("not an array")
        }),
        "expected annotation warning about non-array /Annots, got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

/// /Annots is a reference to a non-array object. Should warn.
#[test]
fn annots_ref_to_non_array_warns() {
    // obj 10 is a string, not an array
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    b.add_object(10, b"(I am a string not an array)");
    let page_body = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
        /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots 10 0 R >>";
    b.add_object(3, page_body);
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let (mut doc, diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    let _text = page.text().unwrap();

    let warnings = diag.warnings();
    assert!(
        warnings.iter().any(|w| {
            w.kind == WarningKind::InvalidState
                && w.message.contains("annotations")
                && w.message.contains("not an array")
        }),
        "expected annotation warning for ref-to-non-array, got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

/// /Annots is a reference to an object that does not exist. Should warn about resolution failure.
#[test]
fn annots_ref_unresolvable_warns() {
    let pdf = build_single_page_pdf("/Annots 999 0 R");
    let (mut doc, diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    let _text = page.text().unwrap();

    let warnings = diag.warnings();
    assert!(
        warnings.iter().any(|w| {
            w.kind == WarningKind::InvalidState
                && w.message.contains("annotations")
                && w.message.contains("resolve")
        }),
        "expected annotation warning about failed resolve, got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

/// /Annots array contains a reference to a non-existent annotation object.
/// Should warn per-annotation and not fail the page.
#[test]
fn annots_array_with_unresolvable_ref_warns() {
    let pdf = build_single_page_pdf("/Annots [999 0 R]");
    let (mut doc, diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    let _text = page.text().unwrap();

    let warnings = diag.warnings();
    assert!(
        warnings.iter().any(|w| {
            w.kind == WarningKind::InvalidState
                && w.message.contains("annotations")
                && w.message.contains("failed to resolve annotation")
        }),
        "expected per-annotation resolve warning, got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

/// Annotation with unknown /Subtype tries appearance stream (covers the _ arm
/// of the subtype match in extract_annotation_text).
#[test]
fn annots_unknown_subtype_tries_appearance() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    // Annotation with unknown subtype, no /AP dict
    b.add_object(
        10,
        b"<< /Type /Annot /Subtype /CustomWeird /Rect [0 0 100 100] >>",
    );
    let page_body = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
        /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots [10 0 R] >>";
    b.add_object(3, page_body);
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let (mut doc, _diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    // Should not crash; text extraction still works
    let text = page.text().unwrap();
    assert!(text.contains("Hello"));
}

/// Annotation with /Subtype /Text and /Contents extracts text.
#[test]
fn annots_text_subtype_extracts_contents() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (MainText) Tj ET");
    // /Text annotation with /Contents
    b.add_object(
        10,
        b"<< /Type /Annot /Subtype /Text /Rect [50 50 200 200] /Contents (NoteText) >>",
    );
    let page_body = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
        /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots [10 0 R] >>";
    b.add_object(3, page_body);
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let (mut doc, _diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    let spans = page.raw_spans().unwrap();
    // Should have annotation text
    assert!(
        spans.iter().any(|s| s.text.contains("NoteText")),
        "expected NoteText from annotation, got spans: {:?}",
        spans.iter().map(|s| &s.text).collect::<Vec<_>>()
    );
}

/// Annotation with /Subtype /Widget and /V (value) but no /AP falls back to /V.
#[test]
fn annots_widget_fallback_to_v_value() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (MainText) Tj ET");
    // Widget annotation with /V but no /AP
    b.add_object(
        10,
        b"<< /Type /Annot /Subtype /Widget /Rect [10 10 200 30] /V (FieldValue) >>",
    );
    let page_body = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
        /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots [10 0 R] >>";
    b.add_object(3, page_body);
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let (mut doc, _diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    let spans = page.raw_spans().unwrap();
    assert!(
        spans.iter().any(|s| s.text.contains("FieldValue")),
        "expected FieldValue from widget /V fallback, got spans: {:?}",
        spans.iter().map(|s| &s.text).collect::<Vec<_>>()
    );
}

/// Widget annotation with /V as a /Name value (not a string).
#[test]
fn annots_widget_v_name_value() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Main) Tj ET");
    // Widget with /V as a Name
    b.add_object(
        10,
        b"<< /Type /Annot /Subtype /Widget /Rect [10 10 200 30] /V /Yes >>",
    );
    let page_body = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
        /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots [10 0 R] >>";
    b.add_object(3, page_body);
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let (mut doc, _diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    let spans = page.raw_spans().unwrap();
    assert!(
        spans.iter().any(|s| s.text.contains("Yes")),
        "expected 'Yes' from widget /V name, got spans: {:?}",
        spans.iter().map(|s| &s.text).collect::<Vec<_>>()
    );
}

/// Widget annotation with /V /Off should produce no annotation text.
#[test]
fn annots_widget_v_off_skipped() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Main) Tj ET");
    b.add_object(
        10,
        b"<< /Type /Annot /Subtype /Widget /Rect [10 10 200 30] /V /Off >>",
    );
    let page_body = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
        /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots [10 0 R] >>";
    b.add_object(3, page_body);
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let (mut doc, _diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    let spans = page.raw_spans().unwrap();
    // Should not have any annotation span with "Off"
    assert!(
        !spans.iter().any(|s| s.is_annotation && s.text == "Off"),
        "widget /V /Off should not produce annotation text"
    );
}

/// Decorative annotation subtypes (Link, Highlight, etc.) should be skipped.
#[test]
fn annots_decorative_subtypes_skipped() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Main) Tj ET");
    // Link annotation (decorative)
    b.add_object(
        10,
        b"<< /Type /Annot /Subtype /Link /Rect [0 0 100 20] /Contents (LinkText) >>",
    );
    // Highlight annotation (decorative)
    b.add_object(
        11,
        b"<< /Type /Annot /Subtype /Highlight /Rect [0 0 100 20] /Contents (HighlightText) >>",
    );
    let page_body = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
        /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots [10 0 R 11 0 R] >>";
    b.add_object(3, page_body);
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let (mut doc, _diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    let spans = page.raw_spans().unwrap();
    assert!(
        !spans.iter().any(|s| s.is_annotation),
        "decorative annotations should not produce text spans"
    );
}

/// Annotation without /Subtype is silently skipped.
#[test]
fn annots_missing_subtype_skipped() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    // Annotation with no /Subtype
    b.add_object(10, b"<< /Type /Annot /Rect [0 0 100 20] >>");
    let page_body = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
        /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots [10 0 R] >>";
    b.add_object(3, page_body);
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let (mut doc, _diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    // Should not crash, annotation without subtype is just skipped
    let text = page.text().unwrap();
    assert!(text.contains("Hello"));
}

/// Inline annotation dictionary in /Annots array (not a reference).
/// Exercises the non-reference branch in the annotation loop.
#[test]
fn annots_inline_dict_in_array() {
    // Build a PDF where /Annots contains a literal dict (unusual but valid per spec).
    // We can't easily embed a literal dict in PDF text here through the builder,
    // so we construct this more manually.
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    // /Annots array containing a direct dict object
    let page_body = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
        /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> \
        /Annots [<< /Type /Annot /Subtype /Text /Rect [10 10 100 100] /Contents (InlineNote) >>] >>";
    b.add_object(3, page_body);
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let (mut doc, _diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    let spans = page.raw_spans().unwrap();
    assert!(
        spans.iter().any(|s| s.text.contains("InlineNote")),
        "expected inline annotation text, got spans: {:?}",
        spans.iter().map(|s| &s.text).collect::<Vec<_>>()
    );
}

/// Annotation with /AP that resolves to a non-stream. Should be skipped silently.
#[test]
fn annots_ap_resolves_to_non_stream() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    // obj 11 is just a string, not a stream
    b.add_object(11, b"(not a stream)");
    // Annotation with /AP /N pointing to a non-stream object
    b.add_object(
        10,
        b"<< /Type /Annot /Subtype /FreeText /Rect [10 10 200 30] /AP << /N 11 0 R >> >>",
    );
    let page_body = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
        /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots [10 0 R] >>";
    b.add_object(3, page_body);
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let (mut doc, _diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    // Should not crash
    let text = page.text().unwrap();
    assert!(text.contains("Hello"));
}

/// Annotation with /AP /N reference to non-existent object. Should warn.
#[test]
fn annots_ap_stream_unresolvable_warns() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    // Annotation with /AP /N pointing to non-existent object
    b.add_object(
        10,
        b"<< /Type /Annot /Subtype /FreeText /Rect [10 10 200 30] /AP << /N 888 0 R >> >>",
    );
    let page_body = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
        /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots [10 0 R] >>";
    b.add_object(3, page_body);
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let (mut doc, diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    let _text = page.text().unwrap();

    let warnings = diag.warnings();
    assert!(
        warnings.iter().any(|w| {
            w.kind == WarningKind::InvalidState
                && w.message.contains("annotations")
                && w.message.contains("resolve")
        }),
        "expected warning about unresolvable appearance stream, got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

/// Circular /AP reference (annotation's /AP /N points back to itself via the
/// same ObjRef). Should warn about cycle and not loop forever.
#[test]
fn annots_circular_ap_reference_warns() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    // Two annotations both pointing to the same /AP stream (obj 11).
    // The second will see obj 11 is already visited.
    b.add_stream_object(
        11,
        "/Subtype /Form",
        b"BT /F1 12 Tf 10 10 Td (APText) Tj ET",
    );
    b.add_object(
        10,
        b"<< /Type /Annot /Subtype /FreeText /Rect [10 10 200 30] /AP << /N 11 0 R >> >>",
    );
    b.add_object(
        12,
        b"<< /Type /Annot /Subtype /FreeText /Rect [10 40 200 60] /AP << /N 11 0 R >> >>",
    );
    let page_body = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
        /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots [10 0 R 12 0 R] >>";
    b.add_object(3, page_body);
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let (mut doc, diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    let _text = page.text().unwrap();

    let warnings = diag.warnings();
    assert!(
        warnings.iter().any(|w| {
            w.kind == WarningKind::InvalidState && w.message.contains("circular /AP reference")
        }),
        "expected circular AP reference warning, got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

/// Annotation with /AP /N that is a dict keyed by appearance state.
/// Exercises the sub-dictionary branch in interpret_appearance_stream.
#[test]
fn annots_ap_n_subdictionary_with_as() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (MainText) Tj ET");
    // Appearance stream for "Yes" state
    b.add_stream_object(
        11,
        "/Subtype /Form /Resources << /Font << /F1 4 0 R >> >>",
        b"BT /F1 10 Tf 5 5 Td (Checked) Tj ET",
    );
    // /AP /N is a dict with state keys; /AS selects the active state
    b.add_object(
        10,
        b"<< /Type /Annot /Subtype /Widget /Rect [10 10 30 30] \
          /AP << /N << /Yes 11 0 R >> >> /AS /Yes >>",
    );
    let page_body = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
        /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots [10 0 R] >>";
    b.add_object(3, page_body);
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let (mut doc, _diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    let spans = page.raw_spans().unwrap();
    // The appearance stream should produce "Checked" text
    assert!(
        spans.iter().any(|s| s.text.contains("Checked")),
        "expected 'Checked' from AP /N sub-dict, got spans: {:?}",
        spans.iter().map(|s| &s.text).collect::<Vec<_>>()
    );
}

/// /AP /N sub-dict where the selected /AS key does not exist. Should produce
/// no annotation text (silent fallback to empty).
#[test]
fn annots_ap_n_subdictionary_missing_as_key() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Main) Tj ET");
    b.add_stream_object(11, "/Subtype /Form", b"BT /F1 10 Tf 5 5 Td (X) Tj ET");
    // /AS is /Yes but /N only has /No key
    b.add_object(
        10,
        b"<< /Type /Annot /Subtype /Widget /Rect [10 10 30 30] \
          /AP << /N << /No 11 0 R >> >> /AS /Yes >>",
    );
    let page_body = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
        /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots [10 0 R] >>";
    b.add_object(3, page_body);
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let (mut doc, _diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    // Should not crash
    let _text = page.text().unwrap();
}

/// Annotation with /AP /N that is not a reference or dict (e.g., a name).
/// Exercises the catch-all _ arm in the stream_ref match.
#[test]
fn annots_ap_n_wrong_type() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    // /AP /N is a Name, not a reference or dict
    b.add_object(
        10,
        b"<< /Type /Annot /Subtype /FreeText /Rect [10 10 200 30] /AP << /N /SomeName >> >>",
    );
    let page_body = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
        /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots [10 0 R] >>";
    b.add_object(3, page_body);
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let (mut doc, _diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    // Should not crash
    let text = page.text().unwrap();
    assert!(text.contains("Hello"));
}

/// Annotation with empty appearance stream content. Exercises the
/// content_data.is_empty() check in interpret_appearance_stream.
#[test]
fn annots_ap_empty_stream_content() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    // Empty appearance stream
    b.add_stream_object(11, "/Subtype /Form", b"");
    b.add_object(
        10,
        b"<< /Type /Annot /Subtype /FreeText /Rect [10 10 200 30] /AP << /N 11 0 R >> >>",
    );
    let page_body = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
        /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots [10 0 R] >>";
    b.add_object(3, page_body);
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pdf = b.finish(1);

    let (mut doc, _diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    // Should not crash
    let text = page.text().unwrap();
    assert!(text.contains("Hello"));
}

// ---------------------------------------------------------------------------
// 2. extract_all()
// ---------------------------------------------------------------------------

/// Page::extract_all() returns text spans, images, and paths in a single pass.
#[test]
fn extract_all_returns_content() {
    let pdf = build_single_page_pdf("");
    let (mut doc, _diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    let content = page.extract_all().unwrap();

    // Should have text spans from the content stream
    assert!(
        !content.spans.is_empty(),
        "extract_all should return non-empty spans"
    );
    // .text() should work on PageContent
    let text = content.text();
    assert!(text.contains("Hello"), "PageContent::text() should work");
}

/// Extract text and text_lines from PageContent.
#[test]
fn extract_all_text_methods() {
    let pdf = build_single_page_pdf("");
    let (mut doc, _diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    let content = page.extract_all().unwrap();

    let lines = content.text_lines();
    assert!(!lines.is_empty(), "text_lines on PageContent should work");

    let text = content.into_text();
    assert!(
        text.contains("Hello"),
        "into_text should produce the same text"
    );
}

// ---------------------------------------------------------------------------
// 3. open_with_password()
// ---------------------------------------------------------------------------

/// Document::open_with_password on an encrypted PDF.
#[test]
fn open_with_password_encrypted() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/corpus/encrypted/rc4_128_user_password.pdf");
    if !path.exists() {
        eprintln!("encrypted corpus not found, skipping open_with_password test");
        return;
    }

    let mut doc = Document::open_with_password(&path, b"test123")
        .expect("should open encrypted PDF with correct password");
    assert!(doc.page_count() > 0);
    let mut page = doc.page(0).unwrap();
    let text = page.text().unwrap();
    assert!(
        text.contains("Encrypted"),
        "should extract text from encrypted PDF"
    );
}

/// Document::open_with_password with wrong password fails.
#[test]
fn open_with_password_wrong_password() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/corpus/encrypted/rc4_128_user_password.pdf");
    if !path.exists() {
        eprintln!("encrypted corpus not found, skipping wrong password test");
        return;
    }

    let result = Document::open_with_password(&path, b"wrong");
    assert!(result.is_err(), "wrong password should fail");
}

/// Document::open_with_password on a non-existent file.
#[test]
fn open_with_password_nonexistent_file() {
    let result = Document::open_with_password("/nonexistent/path.pdf", b"pass");
    assert!(result.is_err(), "opening nonexistent file should fail");
}

// ---------------------------------------------------------------------------
// 4. Resource limit guards
// ---------------------------------------------------------------------------

/// Page tree deeper than MAX_PAGE_TREE_DEPTH (64) should fail.
#[test]
fn page_tree_depth_limit_exceeded() {
    // Build a page tree that is 70 levels deep. Each intermediate node is
    // a /Pages object whose /Kids points to the next level.
    let mut b = PdfBuilder::new("1.4");

    // Leaf page at the bottom (obj 100)
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Deep) Tj ET");
    b.add_object(
        100,
        b"<< /Type /Page /MediaBox [0 0 612 792] /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
    );

    // Build chain: obj 99 -> obj 100, obj 98 -> obj 99, ... obj 30 -> obj 31
    // That's 70 levels of nesting (obj 30 through obj 100 = 71 objects).
    for i in (30..100).rev() {
        let body = format!("<< /Type /Pages /Kids [{} 0 R] /Count 1 >>", i + 1);
        b.add_object(i, body.as_bytes());
    }

    // Catalog points to the topmost /Pages node (obj 30)
    b.add_object(1, b"<< /Type /Catalog /Pages 30 0 R >>");
    let pdf = b.finish(1);

    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    let result = Document::from_bytes_with_config(pdf, config);
    // The depth limit should cause a warning or error during page tree walk.
    // Since walk_kids wraps errors as warnings, this may still succeed but
    // with zero pages, or it may error.
    match result {
        Ok(doc) => {
            // If it opened successfully, the deep branch was skipped.
            // The warning about depth should be present.
            let warnings = diag.warnings();
            let has_depth_warning = warnings.iter().any(|w| {
                w.message.contains("depth limit") || w.message.contains("could not be resolved")
            });
            assert!(
                has_depth_warning || doc.page_count() == 0,
                "expected depth warning or zero pages, got {} pages and warnings: {:?}",
                doc.page_count(),
                warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
            );
        }
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("depth limit"),
                "expected depth limit error, got: {msg}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 5. Missing /ID in encrypted documents
// ---------------------------------------------------------------------------

/// Encrypted PDF with no /ID in trailer should fail with a clear error.
#[test]
fn encrypted_missing_id_fails() {
    // Build a PDF that has /Encrypt in the trailer but no /ID array.
    let mut b = PdfBuilder::new("1.4");
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    // Minimal /Encrypt dict (enough to trigger the encryption path)
    b.add_object(
        6,
        b"<< /Filter /Standard /V 1 /R 2 /O (12345678901234567890123456789012) /U (12345678901234567890123456789012) /P -4 >>",
    );
    // Trailer with /Encrypt but NO /ID
    let pdf = b.finish_with_trailer("/Encrypt 6 0 R", 1);

    let result = Document::from_bytes(pdf);
    match result {
        Ok(_) => panic!("expected error for encrypted PDF without /ID"),
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("ID") || msg.contains("identifier"),
                "expected error about missing ID, got: {msg}"
            );
        }
    }
}

/// Password provided for non-encrypted PDF should produce a warning (not an error).
#[test]
fn password_on_unencrypted_warns() {
    let pdf = build_single_page_pdf("");
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default()
        .with_diagnostics(diag.clone())
        .with_password(b"unnecessary".to_vec());
    let doc = Document::from_bytes_with_config(pdf, config)
        .expect("non-encrypted PDF should still open when password is provided");
    assert!(doc.page_count() > 0);

    let warnings = diag.warnings();
    assert!(
        warnings.iter().any(|w| {
            w.kind == WarningKind::EncryptedDocument && w.message.contains("not encrypted")
        }),
        "expected warning about unnecessary password, got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// 6. Page index out of range
// ---------------------------------------------------------------------------

#[test]
fn page_index_out_of_range() {
    let pdf = build_single_page_pdf("");
    let (mut doc, _diag) = open_with_diag(pdf);
    assert_eq!(doc.page_count(), 1);
    match doc.page(5) {
        Ok(_) => panic!("page(5) on a 1-page document should fail"),
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("out of range"),
                "expected out of range error, got: {msg}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 7. PageContent::into_text_lines()
// ---------------------------------------------------------------------------

#[test]
fn page_content_into_text_lines() {
    let pdf = build_single_page_pdf("");
    let (mut doc, _diag) = open_with_diag(pdf);
    let mut page = doc.page(0).unwrap();
    let content = page.extract_all().unwrap();
    let lines = content.into_text_lines();
    assert!(!lines.is_empty(), "into_text_lines should produce lines");
}

//! Integration tests for font-resolution observability (M-32a).
//!
//! Verifies that the PDF backend:
//! 1. Emits a structured `FallbackFontSubstitution` warning at every
//!    fallback substitution point.
//! 2. Populates `TextSpan::font_resolution` with the correct variant
//!    so downstream consumers can audit or filter suspect text.

mod common;

use common::PdfBuilder;
use std::sync::Arc;
use udoc_core::text::{FallbackReason, FontResolution};
use udoc_pdf::{CollectingDiagnostics, Config, Document, WarningKind};

/// Build a PDF that references /Helvetica (a standard-14 font) without
/// embedding any FontFile. The loader should classify this as
/// `Substituted { reason: NameRouted }` because the font name is a known
/// standard and we route to built-in metrics.
fn build_standard14_no_embed_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    let content = b"BT /F1 12 Tf 72 700 Td (Hello) Tj ET";
    b.add_stream_object(5, "", content);
    // No FontDescriptor -> no embedded program. BaseFont names a standard-14 font.
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

/// Build a PDF that references a non-standard font by name (TimesNewRoman
/// is not a standard-14 face) with no FontDescriptor and no embedded program.
/// Should classify as `SyntheticFallback { reason: NotEmbedded }`.
fn build_nonstandard_no_embed_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    let content = b"BT /F1 12 Tf 72 700 Td (Hi) Tj ET";
    b.add_stream_object(5, "", content);
    b.add_object(
        4,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /SomeRandomFont-Regular >>",
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

/// Build a PDF with a Type0 (composite) font that has Identity-H encoding
/// and no ToUnicode CMap. Should classify as `Substituted { reason: CidNoToUnicode }`.
fn build_cid_no_tounicode_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    // Two-byte codes for Identity-H: 0x0041 -> 'A' under identity fallback.
    let content = b"BT /F1 12 Tf 72 700 Td <0041> Tj ET";
    b.add_stream_object(10, "", content);
    // CIDFontType2 descendant, no ToUnicode anywhere.
    b.add_object(
        7,
        b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /STFangsong \
          /CIDSystemInfo << /Registry (Adobe) /Ordering (GB1) /Supplement 0 >> \
          /DW 1000 >>",
    );
    b.add_object(
        4,
        b"<< /Type /Font /Subtype /Type0 /BaseFont /STFangsong \
          /Encoding /Identity-H /DescendantFonts [7 0 R] >>",
    );
    b.add_object(6, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 10 0 R /Resources 6 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

fn open_with_collector(pdf: Vec<u8>) -> (Document, Arc<CollectingDiagnostics>) {
    let diag = Arc::new(CollectingDiagnostics::new());
    let cfg = Config::default().with_diagnostics(diag.clone());
    let doc = Document::from_bytes_with_config(pdf, cfg).expect("should parse");
    (doc, diag)
}

#[test]
fn standard14_name_routed_substitution() {
    let pdf = build_standard14_no_embed_pdf();
    let (mut doc, diag) = open_with_collector(pdf);

    let mut page = doc.page(0).expect("page 0");
    let spans = page.raw_spans().expect("raw spans");

    assert!(!spans.is_empty(), "expected at least one span");
    for span in &spans {
        match &span.font_resolution {
            FontResolution::Substituted {
                requested,
                reason: FallbackReason::NameRouted,
                resolved,
            } => {
                assert!(
                    requested.contains("Helvetica"),
                    "requested should mention Helvetica, got {requested}"
                );
                assert!(
                    resolved.contains("standard-14"),
                    "resolved should indicate standard-14 routing, got {resolved}"
                );
            }
            other => panic!("expected NameRouted Substituted, got {other:?}"),
        }
    }

    // Exactly one FallbackFontSubstitution warning per font (not per span).
    let warnings = diag.warnings();
    let fallback_warnings: Vec<_> = warnings
        .iter()
        .filter(|w| w.kind == WarningKind::FallbackFontSubstitution)
        .collect();
    assert_eq!(
        fallback_warnings.len(),
        1,
        "expected one FallbackFontSubstitution warning per font, got {} ({:?})",
        fallback_warnings.len(),
        fallback_warnings
    );
    let msg = &fallback_warnings[0].message;
    assert!(
        msg.contains("Helvetica") && msg.contains("NameRouted"),
        "warning should mention Helvetica and NameRouted, got: {msg}"
    );
}

#[test]
fn nonstandard_font_synthetic_fallback() {
    let pdf = build_nonstandard_no_embed_pdf();
    let (mut doc, diag) = open_with_collector(pdf);

    let mut page = doc.page(0).expect("page 0");
    let spans = page.raw_spans().expect("raw spans");

    assert!(!spans.is_empty(), "expected at least one span");
    for span in &spans {
        match &span.font_resolution {
            FontResolution::SyntheticFallback {
                requested,
                generic_family,
                reason: FallbackReason::NotEmbedded,
            } => {
                assert!(
                    requested.contains("SomeRandomFont") || requested.contains("Regular"),
                    "requested should name the font, got {requested}"
                );
                assert!(
                    matches!(
                        generic_family.as_str(),
                        "serif" | "sans-serif" | "monospace"
                    ),
                    "generic_family should be one of the standard buckets, got {generic_family}"
                );
            }
            other => panic!("expected SyntheticFallback/NotEmbedded, got {other:?}"),
        }
    }

    let warnings = diag.warnings();
    let fallback_warnings: Vec<_> = warnings
        .iter()
        .filter(|w| w.kind == WarningKind::FallbackFontSubstitution)
        .collect();
    assert_eq!(fallback_warnings.len(), 1);
    let msg = &fallback_warnings[0].message;
    assert!(
        msg.contains("NotEmbedded"),
        "warning should mention NotEmbedded, got: {msg}"
    );
}

#[test]
fn composite_font_no_tounicode_classifies_as_cid_fallback() {
    let pdf = build_cid_no_tounicode_pdf();
    let (mut doc, diag) = open_with_collector(pdf);

    let mut page = doc.page(0).expect("page 0");
    let spans = page.raw_spans().expect("raw spans");

    assert!(!spans.is_empty(), "expected at least one span");
    for span in &spans {
        assert!(
            matches!(
                &span.font_resolution,
                FontResolution::Substituted {
                    reason: FallbackReason::CidNoToUnicode,
                    ..
                }
            ),
            "expected CidNoToUnicode Substituted, got {:?}",
            span.font_resolution,
        );
    }

    let warnings = diag.warnings();
    let cid_warnings: Vec<_> = warnings
        .iter()
        .filter(|w| {
            w.kind == WarningKind::FallbackFontSubstitution && w.message.contains("CidNoToUnicode")
        })
        .collect();
    assert!(
        !cid_warnings.is_empty(),
        "expected CidNoToUnicode warning, got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

#[test]
fn exact_resolution_emits_no_fallback_warning() {
    // A Type3 font defines its own glyphs via content streams: no embedded
    // font program is expected, and the classifier should NOT flag it as a
    // fallback. Use the existing simpletype3font corpus PDF.
    let bytes = std::fs::read("tests/corpus/minimal/simpletype3font.pdf")
        .expect("corpus file should exist");
    let (mut doc, diag) = open_with_collector(bytes);
    let mut page = doc.page(0).expect("page 0");
    let spans = page.raw_spans().expect("raw spans");

    // Every span should be Exact (Type3 fonts aren't fallbacks).
    for span in &spans {
        assert!(
            span.font_resolution.is_exact(),
            "Type3 span should be Exact, got {:?}",
            span.font_resolution
        );
    }

    // No FallbackFontSubstitution warnings emitted for this document.
    for w in diag.warnings() {
        assert_ne!(
            w.kind,
            WarningKind::FallbackFontSubstitution,
            "unexpected fallback warning for Type3 font: {}",
            w.message
        );
    }
}

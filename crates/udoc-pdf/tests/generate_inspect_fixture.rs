//! Generates the synthetic 100-page PDF used by the `udoc inspect`
//! perf check ( W0-100PAGE-FIXTURE).
//!
//! Run with: `cargo test --test generate_inspect_fixture -- --ignored`
//! Only needs to run when the fixture format changes. Output is committed.
//!
//!  specced a 500ms p95 budget for `udoc inspect` on a 100-page PDF
//! (sample_size=5 by construction). The  verify report flagged that
//! the largest local PDF was 30 pages, deferring AC #6 to . This
//! generator closes that gap.
//!
//! # Layout
//!
//! Single shared font (Helvetica) and resources dict. 100 page objects,
//! each with its own uncompressed content stream containing one text
//! show op: `BT /F1 12 Tf 250 400 Td (Page N of 100) Tj ET`. Object
//! numbering: 1=Catalog, 2=Pages, 3=Font, 4=Resources, then pairs of
//! (page, content) starting at 5. xref is traditional. No timestamps,
//! no random IDs, no /Info dict, so the output is byte-reproducible.
//!
//! Target size: ~30-40 KB. Budget: <500 KB.

mod common;

use common::PdfBuilder;
use std::path::Path;

const OUTPUT_PATH: &str = "../../tests/corpus/inspect-perf/100page.pdf";
const PAGE_COUNT: u32 = 100;

/// Build a 100-page PDF with one short line per page.
///
/// Object plan (deterministic):
///   1 = Catalog
///   2 = Pages tree
///   3 = Font (Helvetica)
///   4 = Resources (references Font 3)
///   5..=104 = Page objects
///   105..=204 = Content streams
fn gen_100_page_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    // Font: Helvetica is one of the 14 PDF base fonts. No embed needed.
    b.add_object(3, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // Shared resources dictionary referenced by every page.
    b.add_object(4, b"<< /Font << /F1 3 0 R >> >>");

    // 100 page objects (5..=104) and 100 content streams (105..=204).
    let mut kids = String::with_capacity(PAGE_COUNT as usize * 6);
    for i in 0..PAGE_COUNT {
        let page_obj = 5 + i;
        let content_obj = 5 + PAGE_COUNT + i;

        if i > 0 {
            kids.push(' ');
        }
        kids.push_str(&format!("{page_obj} 0 R"));

        // Page dict.
        let page_body = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Contents {content_obj} 0 R /Resources 4 0 R >>"
        );
        b.add_object(page_obj, page_body.as_bytes());

        // Content stream: one centered line "Page N of 100".
        // Helvetica 12pt at x=250 puts "Page N of 100" near horizontal
        // center on a 612-wide letter page. y=400 is mid-page.
        let stream = format!(
            "BT /F1 12 Tf 250 400 Td (Page {} of {PAGE_COUNT}) Tj ET",
            i + 1
        );
        b.add_stream_object(content_obj, "", stream.as_bytes());
    }

    // Pages tree.
    let pages_body = format!("<< /Type /Pages /Kids [{kids}] /Count {PAGE_COUNT} >>");
    b.add_object(2, pages_body.as_bytes());

    // Catalog.
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    b.finish(1)
}

#[test]
#[ignore] // Only run manually: cargo test --test generate_inspect_fixture -- --ignored
fn generate_inspect_perf_fixture() {
    let path = Path::new(OUTPUT_PATH);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|e| panic!("creating {}: {e}", parent.display()));
    }

    let data = gen_100_page_pdf();
    eprintln!("100page.pdf: {} bytes", data.len());
    assert!(
        data.len() < 500_000,
        "fixture exceeds 500 KB budget: {} bytes",
        data.len()
    );

    std::fs::write(path, &data).unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
    eprintln!("wrote {}", path.display());
}

// ---------------------------------------------------------------------------
// In-process sanity tests (always run, exercise the bytes without needing
// the disk fixture). Keeps the generator honest if the disk fixture is
// regenerated and the byte layout shifts unexpectedly.
// ---------------------------------------------------------------------------

#[test]
fn generated_bytes_have_pdf_header() {
    let data = gen_100_page_pdf();
    assert!(data.starts_with(b"%PDF-1.4"));
    // %%EOF terminator.
    assert!(data.ends_with(b"%%EOF\n"));
}

#[test]
fn generated_bytes_under_size_budget() {
    let data = gen_100_page_pdf();
    assert!(
        data.len() < 500_000,
        "100page.pdf grew past 500 KB budget: {} bytes",
        data.len()
    );
}

#[test]
fn generated_bytes_are_byte_reproducible() {
    // Two independent generations must be byte-identical (no timestamps,
    // no RNG, no env-dependent state).
    let a = gen_100_page_pdf();
    let b = gen_100_page_pdf();
    assert_eq!(a, b, "generator is not deterministic");
}

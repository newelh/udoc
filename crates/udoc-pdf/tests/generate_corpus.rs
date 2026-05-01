//! Generates hand-crafted minimal test PDFs for the corpus.
//!
//! Run with: cargo test --test generate_corpus -- --ignored
//! Only needs to run when adding/modifying test PDFs. Output is committed.

mod common;

use common::PdfBuilder;
use std::io::Write;
use std::path::Path;

const OUTPUT_DIR: &str = "tests/corpus/minimal";

fn compress_deflate(data: &[u8]) -> Vec<u8> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

fn write_if_changed(path: &Path, data: &[u8]) {
    if path.exists() {
        if let Ok(existing) = std::fs::read(path) {
            if existing == data {
                return; // no change
            }
        }
    }
    std::fs::write(path, data)
        .unwrap_or_else(|e| panic!("failed to write {}: {}", path.display(), e));
    eprintln!("  wrote {}", path.display());
}

// ---- PDF generators ----

/// Minimal valid PDF 1.0 (oldest version we support).
fn gen_minimal_pdf10() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.0");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
    );
    b.finish(1)
}

/// Minimal valid PDF 1.4 (common version).
fn gen_minimal_pdf14() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
    );
    b.finish(1)
}

/// Minimal valid PDF 2.0 (newest version).
fn gen_minimal_pdf20() -> Vec<u8> {
    let mut b = PdfBuilder::new("2.0");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
    );
    b.finish(1)
}

/// Traditional xref with FlateDecode content stream.
fn gen_flate_content_stream() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    // Content stream: "BT /F1 12 Tf 100 700 Td (Hello World) Tj ET"
    let content = b"BT /F1 12 Tf 100 700 Td (Hello World) Tj ET";
    let compressed = compress_deflate(content);

    // Font dictionary (minimal Type1)
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // Resources
    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");

    // Content stream (compressed)
    b.add_stream_object(6, "/Filter /FlateDecode", &compressed);

    // Page
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );

    // Pages
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");

    // Catalog
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    b.finish(1)
}

/// PDF with deliberately wrong /Length on a stream (recovery test).
fn gen_wrong_length() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    // Content stream with WRONG length (says 10, actual is longer)
    let content = b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET";
    b.register_object_offset(4);
    // Deliberately set Length to 10 (wrong, actual content is 38 bytes)
    write!(b.buf, "4 0 obj\n<< /Length 10 >>\nstream\n").unwrap();
    b.buf.extend_from_slice(content);
    b.buf.extend_from_slice(b"\nendstream\nendobj\n");

    b.add_object(5, b"<< /Font << /F1 3 0 R >> >>");
    b.add_object(3, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_object(
        6,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [6 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    b.finish(1)
}

/// PDF with a bad xref entry (garbled offset for one object).
fn gen_bad_xref_entry() -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");

    let obj1_offset = data.len();
    data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let obj2_offset = data.len();
    data.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    let obj3_offset = data.len();
    data.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );

    let xref_offset = data.len();
    write!(data, "xref\n0 4\n").unwrap();
    write!(data, "0000000000 65535 f \r\n").unwrap();
    write!(data, "{:010} 00000 n \r\n", obj1_offset).unwrap();
    // Garbled entry for object 2: non-numeric offset
    write!(data, "00000XXXXX 00000 n \r\n").unwrap();
    write!(data, "{:010} 00000 n \r\n", obj3_offset).unwrap();

    write!(
        data,
        "trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
        xref_offset
    )
    .unwrap();

    // Also fix: store the real obj2_offset somewhere the parser could
    // potentially find (but the xref entry itself is garbled, so object 2
    // won't be resolvable via xref).
    let _ = obj2_offset; // suppress unused warning

    data
}

/// Multi-page PDF (5 pages).
fn gen_multipage() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    // Font
    b.add_object(
        13,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    );

    // Shared resources dict (all pages reference /F1)
    b.add_object(14, b"<< /Font << /F1 13 0 R >> >>");

    // 5 page objects: 3-7
    let mut kids = Vec::new();
    for i in 0..5u32 {
        let page_num = 3 + i;
        let content = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R /Resources 14 0 R >>",
            8 + i
        );
        b.add_object(page_num, content.as_bytes());
        kids.push(format!("{} 0 R", page_num));

        // Content stream for each page
        let stream_content = format!("BT /F1 12 Tf 100 700 Td (Page {}) Tj ET", i + 1);
        b.add_stream_object(8 + i, "", stream_content.as_bytes());
    }

    // Pages tree
    let pages = format!("<< /Type /Pages /Kids [{}] /Count 5 >>", kids.join(" "));
    b.add_object(2, pages.as_bytes());

    // Catalog
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    b.finish(1)
}

/// Empty page (minimal structure, no content stream).
fn gen_empty_page() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
    );
    b.finish(1)
}

/// PDF with WinAnsiEncoding Type1 font.
fn gen_winansi_type1() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    // Content stream with text using WinAnsi characters
    let content = b"BT /F1 12 Tf 72 700 Td (Hello World \\223smart quotes\\224) Tj ET";
    b.add_stream_object(5, "", content);

    // Font with explicit WinAnsiEncoding
    b.add_object(
        4,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /TimesNewRoman /Encoding /WinAnsiEncoding >>",
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

/// PDF with MacRomanEncoding Type1 font.
fn gen_macroman_type1() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    let content = b"BT /F1 12 Tf 72 700 Td (Hello) Tj ET";
    b.add_stream_object(5, "", content);

    b.add_object(
        4,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier /Encoding /MacRomanEncoding >>",
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

/// PDF with content stream as an array of streams.
fn gen_content_array() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    // Two content streams that combine
    let content1 = b"BT /F1 12 Tf 100 700 Td";
    let content2 = b"(Hello from array) Tj ET";
    b.add_stream_object(5, "", content1);
    b.add_stream_object(6, "", content2);

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_object(7, b"<< /Font << /F1 4 0 R >> >>");

    // Contents is an ARRAY of stream refs
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents [5 0 R 6 0 R] /Resources 7 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    b.finish(1)
}

/// PDF with Info dictionary and ID array in trailer.
fn gen_with_info() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(
        4,
        b"<< /Title (Test PDF) /Author (udoc-pdf generator) /Producer (udoc-pdf test suite) /CreationDate (D:20260101000000Z) >>",
    );

    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
    );

    b.finish_with_trailer("/Info 4 0 R /ID [<abc123> <abc123>]", 1)
}

/// Linearized-like PDF (has /Linearized dict but not truly linearized).
/// Tests that we don't choke on the /Linearized hint dict.
fn gen_pseudo_linearized() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    // Linearization dict (first object in file)
    b.add_object(
        10,
        b"<< /Linearized 1.0 /L 5000 /H [100 200] /O 3 /E 4000 /N 1 /T 4500 >>",
    );

    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
    );

    b.finish(1)
}

/// PDF with two FlateDecode streams (tests multiple filter decoding).
fn gen_two_flate_streams() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    let content1 = b"BT /F1 12 Tf 100 700 Td (Page 1) Tj ET";
    let content2 = b"BT /F1 12 Tf 100 700 Td (Page 2) Tj ET";
    let c1 = compress_deflate(content1);
    let c2 = compress_deflate(content2);

    b.add_stream_object(5, "/Filter /FlateDecode", &c1);
    b.add_stream_object(6, "/Filter /FlateDecode", &c2);

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_object(7, b"<< /Font << /F1 4 0 R >> >>");

    b.add_object(
        8,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources 7 0 R >>",
    );
    b.add_object(
        9,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 7 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [8 0 R 9 0 R] /Count 2 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    b.finish(1)
}

/// PDF with a Form XObject containing text, invoked via the Do operator.
///
/// Structure:
///   obj 1: Catalog
///   obj 2: Pages
///   obj 3: Page (references content stream 6, resources with XObject /Fm1 and Font /F1)
///   obj 4: Font (Helvetica, Type1)
///   obj 5: Form XObject stream (text content with BT/ET, has /Matrix for translation)
///   obj 6: Page content stream (page-level text + /Fm1 Do)
fn gen_form_xobject() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    // Font
    b.add_object(
        4,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    );

    // Form XObject stream: text inside a BT/ET block
    let xobj_content = b"BT /F1 12 Tf 10 20 Td (Hello from XObject) Tj ET";
    b.add_stream_object(
        5,
        "/Type /XObject /Subtype /Form /BBox [0 0 200 50] /Resources << /Font << /F1 4 0 R >> >> /Matrix [1 0 0 1 100 600]",
        xobj_content,
    );

    // Page content stream: page-level text, then invoke the Form XObject
    let page_content = b"BT /F1 14 Tf 72 700 Td (Page text) Tj ET\n/Fm1 Do";
    b.add_stream_object(6, "", page_content);

    // Page (references both XObject and Font in resources)
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources << /XObject << /Fm1 5 0 R >> /Font << /F1 4 0 R >> >> >>",
    );

    // Pages tree
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");

    // Catalog
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    b.finish(1)
}

/// PDF with nested Pages tree (2-level).
fn gen_nested_pages() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    // Two sub-Pages nodes, each with one page
    b.add_object(
        4,
        b"<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>",
    );
    b.add_object(
        5,
        b"<< /Type /Page /Parent 6 0 R /MediaBox [0 0 612 792] >>",
    );

    b.add_object(
        3,
        b"<< /Type /Pages /Parent 2 0 R /Kids [4 0 R] /Count 1 >>",
    );
    b.add_object(
        6,
        b"<< /Type /Pages /Parent 2 0 R /Kids [5 0 R] /Count 1 >>",
    );

    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    b.finish(1)
}

/// PDF using ExtGState to override font via gs operator.
fn gen_extgstate_font() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_object(7, b"<< /Type /ExtGState /Font [4 0 R 12] >>");

    let content = b"BT /GS1 gs 100 700 Td (ExtGState font) Tj ET";
    b.add_stream_object(6, "", content);

    b.add_object(
        5,
        b"<< /Font << /F1 4 0 R >> /ExtGState << /GS1 7 0 R >> >>",
    );
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF with marked content (BMC/EMC) around text.
fn gen_marked_content() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    let content = b"BT /F1 12 Tf 100 700 Td /Span BMC (Marked text) Tj EMC ET";
    b.add_stream_object(6, "", content);

    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF with multiple fonts on one page.
fn gen_multiple_fonts() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_object(
        7,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier /Encoding /WinAnsiEncoding >>",
    );

    let content =
        b"BT /F1 12 Tf 100 700 Td (Helvetica text) Tj /F2 10 Tf 100 680 Td (Courier text) Tj ET";
    b.add_stream_object(6, "", content);

    b.add_object(5, b"<< /Font << /F1 4 0 R /F2 7 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF with TJ array positioning (kerning).
fn gen_tj_array() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // TJ array with kerning adjustments
    let content = b"BT /F1 12 Tf 100 700 Td [(W) -80 (orld)] TJ ET";
    b.add_stream_object(6, "", content);

    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF with text state operators (Tc, Tw, TL, Tz, Ts).
fn gen_text_state() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // Use various text state operators
    let content =
        b"BT /F1 12 Tf 100 700 Td 14 TL (Line one) Tj T* (Line two) Tj T* (Line three) Tj ET";
    b.add_stream_object(6, "", content);

    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF using single-quote (') move-and-show operator.
fn gen_single_quote() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    let content =
        b"BT /F1 12 Tf 14 TL 100 700 Td (First line) Tj (Second line) ' (Third line) ' ET";
    b.add_stream_object(6, "", content);

    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF using double-quote (") move-and-show with word/char spacing.
fn gen_double_quote() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // " operator: aw ac string -> set Tw, Tc, then T* + Tj
    let content = b"BT /F1 12 Tf 14 TL 100 700 Td (Normal) Tj 2 0 (Spaced words) \" ET";
    b.add_stream_object(6, "", content);

    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF with multiple text objects (multiple BT/ET pairs).
fn gen_multi_bt() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    let content =
        b"BT /F1 12 Tf 100 700 Td (First block) Tj ET BT /F1 10 Tf 100 680 Td (Second block) Tj ET";
    b.add_stream_object(6, "", content);

    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF with Tm (text matrix) operator for absolute positioning.
fn gen_text_matrix() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // Position text with absolute Tm matrix
    let content = b"BT /F1 12 Tf 1 0 0 1 100 700 Tm (Positioned) Tj ET";
    b.add_stream_object(6, "", content);

    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF with CTM transformation (cm operator + text).
fn gen_ctm_text() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // Apply CTM transformation then draw text
    let content = b"q 1 0 0 1 50 50 cm BT /F1 12 Tf 100 700 Td (Transformed) Tj ET Q";
    b.add_stream_object(6, "", content);

    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF with invisible text (rendering mode 3).
fn gen_invisible_text() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // Tr 3 = invisible text
    let content = b"BT /F1 12 Tf 100 700 Td (Visible) Tj 3 Tr 100 680 Td (Hidden) Tj 0 Tr 100 660 Td (Also visible) Tj ET";
    b.add_stream_object(6, "", content);

    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF with hex string content.
fn gen_hex_string() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // Use hex string encoding for "Hex"
    let content = b"BT /F1 12 Tf 100 700 Td <486578> Tj ET";
    b.add_stream_object(6, "", content);

    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF with escaped parentheses in strings.
fn gen_escaped_parens() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    let content = b"BT /F1 12 Tf 100 700 Td (Paren \\(test\\)) Tj ET";
    b.add_stream_object(6, "", content);

    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF with flate-compressed content and ExtGState on same page.
fn gen_flate_extgstate() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.add_object(7, b"<< /Type /ExtGState /TL 14 >>");

    let content = b"BT /F1 12 Tf /GS1 gs 100 700 Td (Flate plus GS) Tj T* (Next line) Tj ET";
    let compressed = compress_deflate(content);
    b.add_stream_object(6, "/Filter /FlateDecode", &compressed);

    b.add_object(
        5,
        b"<< /Font << /F1 4 0 R >> /ExtGState << /GS1 7 0 R >> >>",
    );
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF with TD operator (move and set leading).
fn gen_td_operator() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // TD sets leading to -ty and moves
    let content = b"BT /F1 12 Tf 100 700 Td (Line A) Tj 0 -14 TD (Line B) Tj T* (Line C) Tj ET";
    b.add_stream_object(6, "", content);

    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF with nested Form XObjects (XObject inside XObject).
fn gen_nested_xobject() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // Inner XObject: just text
    let inner_content = b"BT /F1 10 Tf 0 0 Td (Nested) Tj ET";
    b.add_stream_object(
        8,
        "/Type /XObject /Subtype /Form /BBox [0 0 200 50] /Resources << /Font << /F1 4 0 R >> >>",
        inner_content,
    );

    // Outer XObject: text + invoke inner
    let outer_content = b"BT /F1 12 Tf 0 20 Td (Outer) Tj ET /Fm2 Do";
    b.add_stream_object(
        7,
        "/Type /XObject /Subtype /Form /BBox [0 0 200 100] /Resources << /Font << /F1 4 0 R >> /XObject << /Fm2 8 0 R >> >> /Matrix [1 0 0 1 100 600]",
        outer_content,
    );

    // Page content: text + invoke outer XObject
    let page_content = b"BT /F1 14 Tf 72 700 Td (Page) Tj ET /Fm1 Do";
    b.add_stream_object(6, "", page_content);

    b.add_object(5, b"<< /Font << /F1 4 0 R >> /XObject << /Fm1 7 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF with an inline image (BI/ID/EI) and text on the same page.
///
/// Verifies that inline image parsing does not corrupt text extraction.
/// Content stream has text before the inline image, then more text after.
fn gen_inline_image() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // Content stream: text, then a 2x2 RGB inline image, then more text.
    // The 12 bytes of image data are all 0xFF (white pixels): 2x2 RGB.
    let mut content: Vec<u8> = Vec::new();
    content.extend_from_slice(b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET\n");
    content.extend_from_slice(b"BI /W 2 /H 2 /CS /RGB /BPC 8 ID ");
    // 2x2 RGB = 12 bytes of pixel data
    content.extend_from_slice(&[0xFF; 12]);
    content.extend_from_slice(b"\nEI\n");
    content.extend_from_slice(b"BT /F1 12 Tf 100 680 Td (World) Tj ET");

    b.add_stream_object(6, "", &content);

    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF with an Image XObject (external image referenced by Do operator).
///
/// A 2x2 DeviceRGB image stored as a separate stream object, painted on
/// the page via the Do operator. Also has text for combined extraction.
fn gen_image_xobject() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // Image XObject: 2x2 RGB, 12 bytes of pixel data
    let image_data: Vec<u8> = vec![
        0xFF, 0x00, 0x00, // pixel (0,0) red
        0x00, 0xFF, 0x00, // pixel (1,0) green
        0x00, 0x00, 0xFF, // pixel (0,1) blue
        0xFF, 0xFF, 0x00, // pixel (1,1) yellow
    ];
    b.add_stream_object(
        7,
        "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8",
        &image_data,
    );

    // Page content: draw text, then paint the image
    let page_content = b"BT /F1 12 Tf 100 700 Td (Image page) Tj ET\n/Im1 Do";
    b.add_stream_object(6, "", page_content);

    b.add_object(5, b"<< /Font << /F1 4 0 R >> /XObject << /Im1 7 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// Two-column layout for reading order v2 testing.
///
/// Left column at x=72, right column at x=350. Each column has 6 lines
/// of short text (5+ words per line) with 14pt leading. The large gap
/// between columns should trigger multi-column detection and column-first
/// reading order (all left lines before all right lines).
///
/// Text is kept short (under 25 chars) so left column ends well before x=200,
/// creating a 150+ pt gap to the right column at x=350.
fn gen_two_column() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // Short lines so left column stays narrow, creating a large gap.
    // Each line has 5+ words to pass the table detection check.
    let left_lines = [
        "The fox runs over hills",
        "Sun sets behind the trees",
        "Long shadows cross fields",
        "Birds fly south for winter",
        "Bare trees stand in cold",
        "Snow falls on the ground",
    ];
    let right_lines = [
        "Village prepares for fest",
        "Banners hang from the roofs",
        "Music plays in the square",
        "Kids run through the streets",
        "They laugh until the evening",
        "Stars appear in dark skies",
    ];

    let mut content = Vec::new();
    for i in 0..6 {
        let y = 700 - (i * 14);
        // Left column
        writeln!(
            content,
            "BT /F1 10 Tf 72 {} Td ({}) Tj ET",
            y, left_lines[i]
        )
        .unwrap();
        // Right column
        writeln!(
            content,
            "BT /F1 10 Tf 350 {} Td ({}) Tj ET",
            y, right_lines[i]
        )
        .unwrap();
    }

    b.add_stream_object(6, "", &content);
    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// Three-column layout for reading order v2 testing.
///
/// Three columns at x=36, x=250, x=460. Each has 6 lines of short text
/// (5+ words). The large gaps between columns (~100pt) should trigger
/// detection of two column boundaries, reading columns left to right.
fn gen_three_column() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // Short lines so each column stays narrow, preserving large gaps.
    // Each line must have 4+ words to avoid table detection.
    let col1 = [
        "The first column starts here",
        "We discuss the methods used",
        "New approaches were tried now",
        "Our analysis shows key results",
        "Many factors lead to outcomes",
        "A summary of part one done",
    ];
    let col2 = [
        "The second column has content",
        "More details give good context",
        "Our measures show big changes",
        "A baseline comparison shows gains",
        "A further study is now needed",
        "Column two fully wraps up here",
    ];
    let col3 = [
        "The third column shows findings",
        "New results confirm the idea",
        "The data supports clear trends",
        "Strong significance was achieved",
        "Many replications gave outcomes",
        "We conclude with next clear steps",
    ];

    let mut content = Vec::new();
    for i in 0..6 {
        let y = 700 - (i * 14);
        writeln!(content, "BT /F1 9 Tf 36 {} Td ({}) Tj ET", y, col1[i]).unwrap();
        writeln!(content, "BT /F1 9 Tf 250 {} Td ({}) Tj ET", y, col2[i]).unwrap();
        writeln!(content, "BT /F1 9 Tf 460 {} Td ({}) Tj ET", y, col3[i]).unwrap();
    }

    b.add_stream_object(6, "", &content);
    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// Rotated text mixed with horizontal text.
///
/// Three horizontal lines followed by a 90-degree rotated text block.
/// The rotated text uses Tm with rotation matrix [0 1 -1 0 x y].
/// Rotated spans should be grouped separately and appear after
/// horizontal text in the output.
fn gen_rotated_text() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // Horizontal text (3 lines)
    // Then rotated text (90 degrees CCW) using Tm operator
    let content = b"BT /F1 12 Tf 72 700 Td (Horizontal line one) Tj ET\n\
                    BT /F1 12 Tf 72 680 Td (Horizontal line two) Tj ET\n\
                    BT /F1 12 Tf 72 660 Td (Horizontal line three) Tj ET\n\
                    BT /F1 12 Tf 0 1 -1 0 500 400 Tm (Rotated upward) Tj ET\n\
                    BT /F1 12 Tf 0 1 -1 0 500 300 Tm (Rotated second) Tj ET";

    b.add_stream_object(6, "", content);
    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// Table layout with short entries in a grid pattern.
///
/// A 6-row, 4-column table with short text entries (1-2 words each).
/// Column gaps are large but entries are short, so the table detection
/// heuristic should prevent column-first reading order. Instead, rows
/// should be read left to right, top to bottom.
fn gen_table_layout() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    let col1 = ["Name", "Alice", "Bob", "Carol", "Dave", "Eve"];
    let col2 = ["Age", "28", "35", "42", "31", "27"];
    let col3 = ["City", "London", "Paris", "Berlin", "Tokyo", "Cairo"];
    let col4 = ["Score", "95", "87", "91", "78", "99"];

    let mut content = Vec::new();
    for i in 0..6 {
        let y = 700 - (i * 14);
        writeln!(content, "BT /F1 10 Tf 72 {} Td ({}) Tj ET", y, col1[i]).unwrap();
        writeln!(content, "BT /F1 10 Tf 200 {} Td ({}) Tj ET", y, col2[i]).unwrap();
        writeln!(content, "BT /F1 10 Tf 320 {} Td ({}) Tj ET", y, col3[i]).unwrap();
        writeln!(content, "BT /F1 10 Tf 440 {} Td ({}) Tj ET", y, col4[i]).unwrap();
    }

    b.add_stream_object(6, "", &content);
    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// PDF with embedded CMap stream using bfchar mappings for a Type0 composite font.
///
/// The CMap maps 2-byte codes to Unicode characters. No ToUnicode stream;
/// the embedded CMap IS the character mapping. Content uses hex string
/// encoding to show "Hello!" via the mapped codes.
fn gen_embedded_cmap_bfchar() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    // Obj 1: Catalog
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    // Obj 2: Pages
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");

    // Obj 4: CMap stream (embedded CMap with bfchar mappings)
    let cmap_data = b"/CIDInit /ProcSet findresource begin\n\
12 dict begin\n\
begincmap\n\
/CMapName /TestCMap def\n\
/CMapType 2 def\n\
/CIDSystemInfo << /Registry (Test) /Ordering (Identity) /Supplement 0 >> def\n\
1 begincodespacerange\n\
<0000> <FFFF>\n\
endcodespacerange\n\
5 beginbfchar\n\
<0001> <0048>\n\
<0002> <0065>\n\
<0003> <006C>\n\
<0004> <006F>\n\
<0005> <0021>\n\
endbfchar\n\
endcmap\n\
CMapName currentdict /CMap defineresource pop\n\
end\n\
end";
    b.add_stream_object(4, "", cmap_data);

    // Obj 5: CIDFont (descendant)
    b.add_object(
        5,
        b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /TestFont \
          /CIDSystemInfo << /Registry (Test) /Ordering (Identity) /Supplement 0 >> \
          /DW 1000 >>",
    );

    // Obj 6: Type0 font referencing embedded CMap and descendant CIDFont
    b.add_object(
        6,
        b"<< /Type /Font /Subtype /Type0 /BaseFont /TestFont \
          /Encoding 4 0 R /DescendantFonts [5 0 R] >>",
    );

    // Obj 7: Resources
    b.add_object(7, b"<< /Font << /F1 6 0 R >> >>");

    // Obj 8: Content stream - shows "Hello!" via hex codes
    // H=0001, e=0002, l=0003, l=0003, o=0004, !=0005
    let content = b"BT /F1 12 Tf 100 700 Td <000100020003000300040005> Tj ET";
    b.add_stream_object(8, "", content);

    // Obj 3: Page
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 8 0 R /Resources 7 0 R >>",
    );

    b.finish(1)
}

/// PDF with embedded CMap (cidrange) AND a separate ToUnicode stream.
///
/// The CMap maps 1-byte codes 0x41-0x5A to CIDs 100-125 via cidrange.
/// The ToUnicode stream maps the same byte codes to Unicode A-H.
/// Content shows <41424344> which produces "ABCD" via ToUnicode.
fn gen_embedded_cmap_cidrange() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    // Obj 1: Catalog
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    // Obj 2: Pages
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");

    // Obj 4: CMap stream (embedded CMap with cidrange)
    let cmap_data = b"/CIDInit /ProcSet findresource begin\n\
12 dict begin\n\
begincmap\n\
/CMapName /TestCIDRange def\n\
/CMapType 2 def\n\
/CIDSystemInfo << /Registry (Test) /Ordering (Identity) /Supplement 0 >> def\n\
1 begincodespacerange\n\
<00> <FF>\n\
endcodespacerange\n\
1 begincidrange\n\
<41> <5A> 100\n\
endcidrange\n\
endcmap\n\
CMapName currentdict /CMap defineresource pop\n\
end\n\
end";
    b.add_stream_object(4, "", cmap_data);

    // Obj 5: ToUnicode CMap stream (maps byte codes 0x41-0x48 to Unicode A-H)
    let tounicode_data = b"/CIDInit /ProcSet findresource begin\n\
12 dict begin\n\
begincmap\n\
/CMapName /TestToUnicode def\n\
/CMapType 2 def\n\
1 begincodespacerange\n\
<00> <FF>\n\
endcodespacerange\n\
8 beginbfchar\n\
<41> <0041>\n\
<42> <0042>\n\
<43> <0043>\n\
<44> <0044>\n\
<45> <0045>\n\
<46> <0046>\n\
<47> <0047>\n\
<48> <0048>\n\
endbfchar\n\
endcmap\n\
CMapName currentdict /CMap defineresource pop\n\
end\n\
end";
    b.add_stream_object(5, "", tounicode_data);

    // Obj 6: CIDFont (descendant)
    b.add_object(
        6,
        b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /TestRangeFont \
          /CIDSystemInfo << /Registry (Test) /Ordering (Identity) /Supplement 0 >> \
          /DW 1000 >>",
    );

    // Obj 7: Type0 font with embedded CMap encoding + ToUnicode
    b.add_object(
        7,
        b"<< /Type /Font /Subtype /Type0 /BaseFont /TestRangeFont \
          /Encoding 4 0 R /DescendantFonts [6 0 R] /ToUnicode 5 0 R >>",
    );

    // Obj 9: Resources
    b.add_object(9, b"<< /Font << /F1 7 0 R >> >>");

    // Obj 8: Content stream - shows "ABCD" via hex codes
    let content = b"BT /F1 12 Tf 100 700 Td <41424344> Tj ET";
    b.add_stream_object(8, "", content);

    // Obj 3: Page
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 8 0 R /Resources 9 0 R >>",
    );

    b.finish(1)
}

/// Tagged PDF with StructTreeRoot for marked content ordering tests.
///
/// Two paragraphs placed in reverse geometric order (second paragraph at
/// higher y-coordinate) but with structure tree ordering MCID 0 before
/// MCID 1. Tests whether structure ordering or geometric ordering wins.
fn gen_tagged_structure_tree() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    // Obj 1: Catalog with MarkInfo and StructTreeRoot
    b.add_object(
        1,
        b"<< /Type /Catalog /Pages 2 0 R \
          /MarkInfo << /Marked true >> /StructTreeRoot 9 0 R >>",
    );

    // Obj 2: Pages
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");

    // Obj 4: Font (Helvetica)
    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // Obj 7: Resources
    b.add_object(7, b"<< /Font << /F1 4 0 R >> >>");

    // Obj 8: Content stream
    // "Second paragraph" at y=700 (higher, geometrically first when reading top-down)
    // "First paragraph" at y=500 (lower, geometrically second)
    // MCID 1 wraps "Second paragraph", MCID 0 wraps "First paragraph"
    //
    // Uses Tm (set text matrix) for absolute positioning. Td would be
    // cumulative, making the second position relative to the first.
    let content = b"BT\n\
/F1 12 Tf\n\
1 0 0 1 100 700 Tm\n\
/P <</MCID 1>> BDC (Second paragraph) Tj EMC\n\
1 0 0 1 100 500 Tm\n\
/P <</MCID 0>> BDC (First paragraph) Tj EMC\n\
ET";
    b.add_stream_object(8, "", content);

    // Obj 12: ParentTree (number tree mapping StructParents -> struct elements)
    // Entry 0 maps to array of struct element refs for page 3's MCIDs
    b.add_object(12, b"<< /Type /NumberTree /Nums [0 [10 0 R 11 0 R]] >>");

    // Obj 9: StructTreeRoot
    b.add_object(
        9,
        b"<< /Type /StructTreeRoot /K [10 0 R 11 0 R] /ParentTree 12 0 R >>",
    );

    // Obj 10: Struct element P (first paragraph, MCID 0)
    b.add_object(10, b"<< /Type /StructElem /S /P /K 0 /Pg 3 0 R >>");

    // Obj 11: Struct element P (second paragraph, MCID 1)
    b.add_object(11, b"<< /Type /StructElem /S /P /K 1 /Pg 3 0 R >>");

    // Obj 3: Page with StructParents
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 8 0 R /Resources 7 0 R /StructParents 0 >>",
    );

    b.finish(1)
}

/// PDF with q/Q graphics state save/restore around text.
fn gen_save_restore() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    let content =
        b"q BT /F1 12 Tf 100 700 Td (Saved state) Tj ET Q BT /F1 12 Tf 100 680 Td (Restored) Tj ET";
    b.add_stream_object(6, "", content);

    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// Two-column layout with a full-width header and footer.
///
/// Simulates an academic paper: title spans both columns at the top,
/// then two columns of body text, then a footer spanning both columns.
/// This tests the reading order engine's ability to handle mixed-width
/// regions (full-width + columnar) on the same page.
fn gen_twocol_header_footer() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // Header spans full width, then two columns, then footer.
    let mut content = Vec::new();

    // Full-width title at top
    writeln!(
        content,
        "BT /F1 14 Tf 72 740 Td (Analysis of Multi-Column Document Layouts in Practice) Tj ET"
    )
    .unwrap();

    // Left column body text (x=72)
    let left = [
        "The study examines how columns",
        "are used in modern documents",
        "Results show clear patterns here",
        "Many layouts use two columns now",
        "This approach saves paper space",
        "Readers prefer columnar layouts",
    ];
    // Right column body text (x=320)
    let right = [
        "Related work covers many areas",
        "Prior studies found mixed results",
        "Our method improves on baselines",
        "The data was collected last year",
        "Statistical tests confirm gains",
        "Future work will expand the scope",
    ];

    for i in 0..6 {
        let y = 700 - (i * 14);
        writeln!(content, "BT /F1 10 Tf 72 {} Td ({}) Tj ET", y, left[i]).unwrap();
        writeln!(content, "BT /F1 10 Tf 320 {} Td ({}) Tj ET", y, right[i]).unwrap();
    }

    // Full-width footer at bottom
    writeln!(
        content,
        "BT /F1 9 Tf 72 580 Td (Page 1 of 1 -- Conference on Document Analysis 2026) Tj ET"
    )
    .unwrap();

    b.add_stream_object(6, "", &content);
    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// All text in rendering mode 3 (invisible). Simulates scanned+OCR PDFs
/// where the OCR layer is invisible text placed over a scanned image.
///
/// Tr=3 text is extracted with `is_invisible: true` on TextSpan.
/// The golden file expects all four lines of OCR text.
fn gen_render_mode3_ocr() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // All text uses Tr=3 (invisible). This simulates an OCR layer on a scanned page.
    let content = b"BT /F1 12 Tf 3 Tr \
        72 700 Td (This is invisible OCR text on a scanned page) Tj \
        0 -16 Td (The quick brown fox jumps over the lazy dog) Tj \
        0 -16 Td (Page numbers and headers are also invisible) Tj \
        0 -16 Td (A real OCR layer would overlay a scanned image) Tj ET";
    b.add_stream_object(6, "", content);

    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// Two unequal-width columns (sidebar layout).
///
/// Left column is narrow (x=72, ~120pt wide) and right column is wide
/// (x=220, ~320pt wide). Simulates a sidebar/main-content layout.
fn gen_twocol_unequal() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // Narrow left sidebar, wide right main content
    let sidebar = [
        "Quick links",
        "Section one",
        "Section two",
        "Section three",
        "References here",
        "Index at end",
    ];
    let main_content = [
        "The main content area has much more room for text here",
        "This allows for longer sentences and paragraphs in the body",
        "Important details are presented in the wider column right side",
        "Figures and tables would appear alongside this text normally",
        "Cross references point to the sidebar navigation on the left",
        "The conclusion summarizes all findings from this full document",
    ];

    let mut content = Vec::new();
    for i in 0..6 {
        let y = 700 - (i * 16);
        // Narrow sidebar at x=72
        writeln!(content, "BT /F1 9 Tf 72 {} Td ({}) Tj ET", y, sidebar[i]).unwrap();
        // Wide main content at x=220
        writeln!(
            content,
            "BT /F1 10 Tf 220 {} Td ({}) Tj ET",
            y, main_content[i]
        )
        .unwrap();
    }

    b.add_stream_object(6, "", &content);
    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// Page tree node with missing /Type (recovery heuristic).
///
/// The intermediate Pages node omits /Type entirely. The parser should
/// detect /Kids and treat it as a Pages node anyway. This exercises the
/// fallback path in document.rs walk_page_tree_node.
fn gen_missing_type_pages() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    let content = b"BT /F1 12 Tf 72 700 Td (Recovered from missing Type) Tj ET";
    b.add_stream_object(6, "", content);

    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");

    // Page node (has /Type /Page as normal)
    b.add_object(
        3,
        b"<< /Type /Page /Parent 7 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );

    // Intermediate Pages node: intentionally MISSING /Type
    // Has /Kids so the heuristic should detect it as a Pages node.
    b.add_object(7, b"<< /Parent 2 0 R /Kids [3 0 R] /Count 1 >>");

    // Root Pages node (normal)
    b.add_object(2, b"<< /Type /Pages /Kids [7 0 R] /Count 1 >>");

    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// Multi-page two-column document.
///
/// Two pages, each with two-column layout. Tests that reading order
/// handles columns correctly across page boundaries. Each page has
/// distinct content so column interleaving bugs are obvious.
fn gen_multipage_twocol() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // Page 1 content
    let p1_left = [
        "Introduction to the study here",
        "Background covers prior research",
        "Methods section describes our work",
        "Data was collected from many sites",
    ];
    let p1_right = [
        "Abstract of the full paper text",
        "Key findings are summarized below",
        "The approach uses novel techniques",
        "Sample sizes were large and varied",
    ];

    let mut c1 = Vec::new();
    for i in 0..4 {
        let y = 700 - (i * 16);
        writeln!(c1, "BT /F1 10 Tf 72 {} Td ({}) Tj ET", y, p1_left[i]).unwrap();
        writeln!(c1, "BT /F1 10 Tf 330 {} Td ({}) Tj ET", y, p1_right[i]).unwrap();
    }

    b.add_stream_object(6, "", &c1);
    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );

    // Page 2 content
    let p2_left = [
        "Results of experiments are shown",
        "Table one lists all the raw scores",
        "Figure two plots the trend lines",
        "Discussion interprets these results",
    ];
    let p2_right = [
        "Analysis confirms the main hypothesis",
        "Error bars are within acceptable range",
        "Comparison with baselines shows gains",
        "Conclusion outlines the next clear steps",
    ];

    let mut c2 = Vec::new();
    for i in 0..4 {
        let y = 700 - (i * 16);
        writeln!(c2, "BT /F1 10 Tf 72 {} Td ({}) Tj ET", y, p2_left[i]).unwrap();
        writeln!(c2, "BT /F1 10 Tf 330 {} Td ({}) Tj ET", y, p2_right[i]).unwrap();
    }

    b.add_stream_object(8, "", &c2);
    b.add_object(9, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        7,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 8 0 R /Resources 9 0 R >>",
    );

    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R 7 0 R] /Count 2 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

/// Mixed visible and invisible text interleaved on the same page.
///
/// Simulates a scanned document where some pages have both real text
/// elements (like headers, page numbers from the PDF generator) and
/// OCR text (Tr=3) from the scan. Both visible and invisible text are
/// extracted; invisible spans have `is_invisible: true`.
fn gen_mixed_render_modes() -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    b.add_object(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");

    // Mix of visible (Tr=0) and invisible (Tr=3) text.
    // The visible header and footer come from the PDF generator.
    // The invisible body is OCR text overlaying a scan.
    let content = b"BT /F1 14 Tf 0 Tr 72 740 Td (Document Title Visible) Tj ET \
        BT /F1 10 Tf 3 Tr 72 700 Td (This body text is from OCR invisible) Tj ET \
        BT /F1 10 Tf 3 Tr 72 684 Td (More invisible OCR content on page) Tj ET \
        BT /F1 10 Tf 3 Tr 72 668 Td (The scan shows a chart and table) Tj ET \
        BT /F1 9 Tf 0 Tr 72 600 Td (Footer text is visible page one) Tj ET";
    b.add_stream_object(6, "", content);

    b.add_object(5, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources 5 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

#[test]
#[ignore] // Only run manually: cargo test --test generate_corpus -- --ignored
fn generate_all_corpus_pdfs() {
    let dir = Path::new(OUTPUT_DIR);
    assert!(
        dir.exists(),
        "corpus directory does not exist: {}",
        dir.display()
    );

    let files: Vec<(&str, Vec<u8>)> = vec![
        ("minimal_10.pdf", gen_minimal_pdf10()),
        ("minimal_14.pdf", gen_minimal_pdf14()),
        ("minimal_20.pdf", gen_minimal_pdf20()),
        ("flate_content.pdf", gen_flate_content_stream()),
        ("wrong_length.pdf", gen_wrong_length()),
        ("bad_xref_entry.pdf", gen_bad_xref_entry()),
        ("multipage.pdf", gen_multipage()),
        ("empty_page.pdf", gen_empty_page()),
        ("winansi_type1.pdf", gen_winansi_type1()),
        ("macroman_type1.pdf", gen_macroman_type1()),
        ("content_array.pdf", gen_content_array()),
        ("with_info.pdf", gen_with_info()),
        ("pseudo_linearized.pdf", gen_pseudo_linearized()),
        ("two_flate_streams.pdf", gen_two_flate_streams()),
        ("nested_pages.pdf", gen_nested_pages()),
        ("form_xobject.pdf", gen_form_xobject()),
        ("extgstate_font.pdf", gen_extgstate_font()),
        ("marked_content.pdf", gen_marked_content()),
        ("multiple_fonts.pdf", gen_multiple_fonts()),
        ("tj_array.pdf", gen_tj_array()),
        ("text_state.pdf", gen_text_state()),
        ("single_quote.pdf", gen_single_quote()),
        ("double_quote.pdf", gen_double_quote()),
        ("multi_bt.pdf", gen_multi_bt()),
        ("text_matrix.pdf", gen_text_matrix()),
        ("ctm_text.pdf", gen_ctm_text()),
        ("invisible_text.pdf", gen_invisible_text()),
        ("hex_string.pdf", gen_hex_string()),
        ("escaped_parens.pdf", gen_escaped_parens()),
        ("flate_extgstate.pdf", gen_flate_extgstate()),
        ("td_operator.pdf", gen_td_operator()),
        ("nested_xobject.pdf", gen_nested_xobject()),
        ("save_restore.pdf", gen_save_restore()),
        ("inline_image.pdf", gen_inline_image()),
        ("image_xobject.pdf", gen_image_xobject()),
        ("two_column.pdf", gen_two_column()),
        ("three_column.pdf", gen_three_column()),
        ("rotated_text.pdf", gen_rotated_text()),
        ("table_layout.pdf", gen_table_layout()),
        ("embedded_cmap_bfchar.pdf", gen_embedded_cmap_bfchar()),
        ("embedded_cmap_cidrange.pdf", gen_embedded_cmap_cidrange()),
        ("tagged_structure_tree.pdf", gen_tagged_structure_tree()),
        ("twocol_header_footer.pdf", gen_twocol_header_footer()),
        ("render_mode3_ocr.pdf", gen_render_mode3_ocr()),
        ("twocol_unequal.pdf", gen_twocol_unequal()),
        ("missing_type_pages.pdf", gen_missing_type_pages()),
        ("multipage_twocol.pdf", gen_multipage_twocol()),
        ("mixed_render_modes.pdf", gen_mixed_render_modes()),
    ];

    eprintln!("Generating {} corpus PDFs in {}", files.len(), OUTPUT_DIR);
    for (name, data) in &files {
        let path = dir.join(name);
        write_if_changed(&path, data);
    }
    eprintln!("Done. {} files generated.", files.len());
}

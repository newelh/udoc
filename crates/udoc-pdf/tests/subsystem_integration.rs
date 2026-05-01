//! Integration tests for subsystem boundaries and cross-subsystem contracts.
//!
//! H-W5: Tests that exercise the interfaces between subsystems:
//! - Object resolver: resolution, cycle detection, caching, reference chains
//! - Stream decoding: filter chains via full parse->resolve->decode pipeline
//! - Content interpreter: hand-crafted content streams through public API
//! - Table extraction: end-to-end through public API
//! - Cross-subsystem edge cases: malformed objects, empty pages, missing resources

mod common;

use std::sync::Arc;

use common::PdfBuilder;
use udoc_pdf::object::resolver::ObjectResolver;
use udoc_pdf::object::stream::{decode_stream, DecodeLimits};
use udoc_pdf::object::{ObjRef, PdfDictionary, PdfObject};
use udoc_pdf::parse::document_parser::DocumentParser;
use udoc_pdf::{CollectingDiagnostics, Config, Document, NullDiagnostics, WarningKind};

// ===========================================================================
// Helpers
// ===========================================================================

/// Build a minimal single-page PDF from raw content stream bytes.
/// Returns owned bytes suitable for Document::from_bytes.
fn build_single_page_pdf(content: &[u8]) -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");

    // Catalog (obj 1)
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");

    // Pages (obj 2)
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");

    // Page (obj 3) -- references content stream and resources
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
    );

    // Content stream (obj 4)
    b.add_stream_object(4, "", content);

    // Minimal Type1 font (obj 5) -- enough to decode single-byte text
    b.add_object(
        5,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
          /Encoding /WinAnsiEncoding >>",
    );

    b.finish(1)
}

/// Build a single-page PDF with custom resources dict fragment.
fn build_pdf_with_resources(content: &[u8], resources: &[u8]) -> Vec<u8> {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");

    let mut page = Vec::new();
    page.extend_from_slice(
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources ",
    );
    page.extend_from_slice(resources);
    page.extend_from_slice(b" >>");
    b.add_object(3, &page);

    b.add_stream_object(4, "", content);
    b.finish(1)
}

// ===========================================================================
// 1. Object resolver integration tests
// ===========================================================================

#[test]
fn resolver_resolves_indirect_reference_chain() {
    // Object 1 -> ref to 2, Object 2 -> ref to 3, Object 3 -> integer 42
    // resolve_chain should follow the chain to 42.
    let mut b = PdfBuilder::new("1.4");
    b.add_object(1, b"2 0 R");
    b.add_object(2, b"3 0 R");
    b.add_object(3, b"42");
    // Need a minimal catalog so DocumentParser is happy
    b.add_object(10, b"<< /Type /Catalog /Pages 11 0 R >>");
    b.add_object(11, b"<< /Type /Pages /Kids [] /Count 0 >>");
    let data = b.finish(10);

    let diag = Arc::new(CollectingDiagnostics::new());
    let doc = DocumentParser::with_diagnostics(&data, diag.clone())
        .parse()
        .expect("should parse");
    let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag);

    let obj = resolver.resolve(ObjRef::new(1, 0)).expect("resolve obj 1");
    // Direct resolve gives us the reference, not the final value
    assert!(matches!(obj, PdfObject::Reference(_)));

    // resolve_chain follows the chain
    let final_obj = resolver
        .resolve_chain(PdfObject::Reference(ObjRef::new(1, 0)), 10)
        .expect("resolve_chain");
    assert_eq!(final_obj, PdfObject::Integer(42));
}

#[test]
fn resolver_cycle_detection_returns_error() {
    // Object 1 references 2, object 2 references 1 -- cycle.
    // resolve_chain should detect the cycle and return an error.
    let mut b = PdfBuilder::new("1.4");
    b.add_object(1, b"2 0 R");
    b.add_object(2, b"1 0 R");
    b.add_object(10, b"<< /Type /Catalog /Pages 11 0 R >>");
    b.add_object(11, b"<< /Type /Pages /Kids [] /Count 0 >>");
    let data = b.finish(10);

    let diag = Arc::new(CollectingDiagnostics::new());
    let doc = DocumentParser::with_diagnostics(&data, diag.clone())
        .parse()
        .expect("should parse");
    let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag);

    // resolve_chain with a depth limit should terminate
    let result = resolver.resolve_chain(PdfObject::Reference(ObjRef::new(1, 0)), 10);
    // Either error or returns a reference (depth exhausted). Either way, no hang.
    assert!(
        result.is_err() || matches!(result.unwrap(), PdfObject::Reference(_)),
        "cycle should either error or stop at a reference"
    );
}

#[test]
fn resolver_missing_object_returns_error() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.add_object(2, b"<< /Type /Pages /Kids [] /Count 0 >>");
    let data = b.finish(1);

    let doc = DocumentParser::new(&data).parse().expect("should parse");
    let mut resolver = ObjectResolver::from_document(&data, doc);

    let result = resolver.resolve(ObjRef::new(999, 0));
    assert!(result.is_err(), "missing object should return error");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not found"),
        "error should mention 'not found', got: {err_msg}"
    );
}

#[test]
fn resolver_caching_returns_consistent_results() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.add_object(2, b"<< /Type /Pages /Kids [] /Count 0 >>");
    b.add_object(3, b"<< /Key /Value >>");
    let data = b.finish(1);

    let doc = DocumentParser::new(&data).parse().expect("should parse");
    let mut resolver = ObjectResolver::from_document(&data, doc);

    // First resolution
    let obj1 = resolver.resolve(ObjRef::new(3, 0)).expect("first resolve");
    assert_eq!(resolver.cache_len(), 1);

    // Second resolution should be cache hit
    let obj2 = resolver.resolve(ObjRef::new(3, 0)).expect("second resolve");
    assert_eq!(obj1, obj2);
    assert_eq!(resolver.cache_len(), 1);
}

#[test]
fn resolver_lru_eviction_still_resolves() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.add_object(2, b"<< /Type /Pages /Kids [] /Count 0 >>");
    for i in 3..=12 {
        let body = format!("{}", i * 10);
        b.add_object(i, body.as_bytes());
    }
    let data = b.finish(1);

    let doc = DocumentParser::new(&data).parse().expect("should parse");
    let mut resolver = ObjectResolver::from_document(&data, doc);
    resolver.set_cache_max(3);

    // Resolve 10 objects with cache max of 3
    for i in 3..=12 {
        let obj = resolver.resolve(ObjRef::new(i, 0)).expect("resolve");
        assert_eq!(obj, PdfObject::Integer((i * 10) as i64));
    }

    // Re-resolve the first object (evicted from cache, should re-parse)
    let obj = resolver.resolve(ObjRef::new(3, 0)).expect("re-resolve");
    assert_eq!(obj, PdfObject::Integer(30));
}

// ===========================================================================
// 2. Stream decoding integration tests
// ===========================================================================

#[test]
fn stream_decode_no_filter() {
    let raw = b"Hello, World!";
    let dict = PdfDictionary::new();
    let limits = DecodeLimits::default();
    let result = decode_stream(raw, &dict, &limits, &NullDiagnostics, 0).expect("decode");
    assert_eq!(result, b"Hello, World!");
}

#[test]
fn stream_decode_ascii_hex_filter() {
    let raw = b"48656C6C6F>";
    let mut dict = PdfDictionary::new();
    dict.insert(
        b"Filter".to_vec(),
        PdfObject::Name(b"ASCIIHexDecode".to_vec()),
    );
    let limits = DecodeLimits::default();
    let result = decode_stream(raw, &dict, &limits, &NullDiagnostics, 0).expect("decode");
    assert_eq!(result, b"Hello");
}

#[test]
fn stream_decode_ascii85_filter() {
    // "Man " encoded in ASCII85 is "9jqo^"
    let raw = b"9jqo^~>";
    let mut dict = PdfDictionary::new();
    dict.insert(
        b"Filter".to_vec(),
        PdfObject::Name(b"ASCII85Decode".to_vec()),
    );
    let limits = DecodeLimits::default();
    let result = decode_stream(raw, &dict, &limits, &NullDiagnostics, 0).expect("decode");
    assert_eq!(result, b"Man ");
}

#[test]
fn stream_decode_flate_filter() {
    // Compress "Hello, World!" with flate and pass through decode_stream
    use std::io::Write;
    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(b"Hello, World!").unwrap();
    let compressed = encoder.finish().unwrap();

    let mut dict = PdfDictionary::new();
    dict.insert(b"Filter".to_vec(), PdfObject::Name(b"FlateDecode".to_vec()));
    let limits = DecodeLimits::default();
    let result = decode_stream(&compressed, &dict, &limits, &NullDiagnostics, 0).expect("decode");
    assert_eq!(result, b"Hello, World!");
}

#[test]
fn stream_decode_filter_chain_hex_then_flate() {
    // Chain: ASCIIHexDecode -> FlateDecode
    // First compress, then hex-encode
    use std::io::Write;
    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(b"chained filters work").unwrap();
    let compressed = encoder.finish().unwrap();

    // Hex-encode the compressed data
    let mut hex = String::new();
    for byte in &compressed {
        hex.push_str(&format!("{:02X}", byte));
    }
    hex.push('>');

    let mut dict = PdfDictionary::new();
    dict.insert(
        b"Filter".to_vec(),
        PdfObject::Array(vec![
            PdfObject::Name(b"ASCIIHexDecode".to_vec()),
            PdfObject::Name(b"FlateDecode".to_vec()),
        ]),
    );
    let limits = DecodeLimits::default();
    let result =
        decode_stream(hex.as_bytes(), &dict, &limits, &NullDiagnostics, 0).expect("decode");
    assert_eq!(result, b"chained filters work");
}

#[test]
fn stream_decode_unknown_filter_returns_error() {
    let raw = b"data";
    let mut dict = PdfDictionary::new();
    dict.insert(b"Filter".to_vec(), PdfObject::Name(b"BogusFilter".to_vec()));
    let limits = DecodeLimits::default();
    let result = decode_stream(raw, &dict, &limits, &NullDiagnostics, 0);
    assert!(result.is_err(), "unknown filter should error");
}

#[test]
fn stream_decode_through_resolver_pipeline() {
    // Full pipeline: parse PDF -> resolve stream object -> decode
    let mut b = PdfBuilder::new("1.4");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
    );
    b.add_stream_object(4, "", b"BT /F1 12 Tf (Hello) Tj ET");
    let data = b.finish(1);

    let diag = Arc::new(CollectingDiagnostics::new());
    let doc = DocumentParser::with_diagnostics(&data, diag.clone())
        .parse()
        .expect("parse");
    let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag);

    let stream = resolver
        .resolve_stream(ObjRef::new(4, 0))
        .expect("resolve stream");
    let decoded = resolver
        .decode_stream_data(&stream, Some(ObjRef::new(4, 0)))
        .expect("decode stream");
    assert_eq!(decoded, b"BT /F1 12 Tf (Hello) Tj ET");
}

// ===========================================================================
// 3. Content interpreter integration tests (through public API)
// ===========================================================================

#[test]
fn content_interpreter_basic_text_extraction() {
    let content = b"BT /F1 12 Tf 100 700 Td (Hello World) Tj ET";
    let pdf = build_single_page_pdf(content);
    let mut doc = Document::from_bytes(pdf).expect("open");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text");
    assert!(
        text.contains("Hello World"),
        "expected 'Hello World', got: {text:?}"
    );
}

#[test]
fn content_interpreter_multiple_text_objects() {
    let content = b"BT /F1 12 Tf 100 700 Td (First) Tj ET \
                    BT /F1 12 Tf 100 680 Td (Second) Tj ET";
    let pdf = build_single_page_pdf(content);
    let mut doc = Document::from_bytes(pdf).expect("open");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text");
    assert!(text.contains("First"), "missing 'First' in: {text:?}");
    assert!(text.contains("Second"), "missing 'Second' in: {text:?}");
}

#[test]
fn content_interpreter_tj_array_operator() {
    // TJ array: [(Hello) -500 (World)]
    // The negative number inserts spacing between "Hello" and "World"
    let content = b"BT /F1 12 Tf 100 700 Td [(Hello) -500 (World)] TJ ET";
    let pdf = build_single_page_pdf(content);
    let mut doc = Document::from_bytes(pdf).expect("open");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text");
    assert!(text.contains("Hello"), "missing 'Hello' in: {text:?}");
    assert!(text.contains("World"), "missing 'World' in: {text:?}");
}

#[test]
fn content_interpreter_text_positioning_td() {
    // Td operator moves the text position
    let content = b"BT /F1 12 Tf 72 720 Td (Line1) Tj 0 -14 Td (Line2) Tj ET";
    let pdf = build_single_page_pdf(content);
    let mut doc = Document::from_bytes(pdf).expect("open");
    let mut page = doc.page(0).expect("page 0");
    let spans = page.raw_spans().expect("raw_spans");
    assert!(
        spans.len() >= 2,
        "expected at least 2 spans, got {}",
        spans.len()
    );
}

#[test]
fn content_interpreter_graphics_state_save_restore() {
    // q/Q should save and restore graphics state, including text state
    let content = b"BT /F1 12 Tf 100 700 Td (Before) Tj ET \
                    q \
                    BT /F1 24 Tf 100 680 Td (Inside) Tj ET \
                    Q \
                    BT /F1 12 Tf 100 660 Td (After) Tj ET";
    let pdf = build_single_page_pdf(content);
    let mut doc = Document::from_bytes(pdf).expect("open");
    let mut page = doc.page(0).expect("page 0");
    let spans = page.raw_spans().expect("raw_spans");
    assert!(
        spans.len() >= 3,
        "expected at least 3 spans for Before/Inside/After, got {}",
        spans.len()
    );
    let text = page.text().expect("text");
    assert!(text.contains("Before"), "missing 'Before'");
    assert!(text.contains("Inside"), "missing 'Inside'");
    assert!(text.contains("After"), "missing 'After'");
}

#[test]
fn content_interpreter_empty_content_stream() {
    let content = b"";
    let pdf = build_single_page_pdf(content);
    let mut doc = Document::from_bytes(pdf).expect("open");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text");
    assert!(text.is_empty(), "empty content should produce no text");
}

#[test]
fn content_interpreter_text_with_single_quote() {
    // The ' operator is equivalent to T* (Tj)
    let content = b"BT /F1 12 Tf 100 700 Td (Line1) ' ET";
    let pdf = build_single_page_pdf(content);
    let mut doc = Document::from_bytes(pdf).expect("open");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text");
    assert!(text.contains("Line1"), "missing 'Line1' in: {text:?}");
}

#[test]
fn content_interpreter_text_with_double_quote() {
    // The " operator sets word/char spacing then shows text
    let content = b"BT /F1 12 Tf 100 700 Td 0 0 (Quoted) \" ET";
    let pdf = build_single_page_pdf(content);
    let mut doc = Document::from_bytes(pdf).expect("open");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text");
    assert!(text.contains("Quoted"), "missing 'Quoted' in: {text:?}");
}

// ===========================================================================
// 4. Cross-subsystem edge cases
// ===========================================================================

#[test]
fn empty_page_no_content_stream() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    // Page with no /Contents at all
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
    );
    let data = b.finish(1);

    let mut doc = Document::from_bytes(data).expect("open");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text");
    assert!(
        text.is_empty(),
        "page without /Contents should have no text"
    );
}

#[test]
fn page_with_empty_resources() {
    let content = b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET";
    let pdf = build_pdf_with_resources(content, b"<< >>");
    let mut doc = Document::from_bytes(pdf).expect("open");
    let mut page = doc.page(0).expect("page 0");
    // Font /F1 is not in resources, so text extraction should handle gracefully
    // (warn and skip, or produce empty/replacement text)
    let result = page.text();
    // Should not panic; may succeed with partial text or warnings
    assert!(result.is_ok(), "missing font should not cause panic");
}

#[test]
fn page_with_no_resources() {
    let content = b"BT 100 700 Td (Hello) Tj ET";
    let mut b = PdfBuilder::new("1.4");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    // Page without /Resources key
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
    );
    b.add_stream_object(4, "", content);
    let data = b.finish(1);

    let mut doc = Document::from_bytes(data).expect("open");
    let mut page = doc.page(0).expect("page 0");
    // Should not panic; the interpreter should handle missing resources
    let result = page.text();
    assert!(result.is_ok(), "missing /Resources should not cause panic");
}

#[test]
fn content_array_multiple_streams() {
    // /Contents as an array of stream references
    let mut b = PdfBuilder::new("1.4");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents [4 0 R 5 0 R] /Resources << /Font << /F1 6 0 R >> >> >>",
    );
    b.add_stream_object(4, "", b"BT /F1 12 Tf 100 700 Td (Part1) Tj ET");
    b.add_stream_object(5, "", b"BT /F1 12 Tf 100 680 Td (Part2) Tj ET");
    b.add_object(
        6,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
          /Encoding /WinAnsiEncoding >>",
    );
    let data = b.finish(1);

    let mut doc = Document::from_bytes(data).expect("open");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text");
    assert!(text.contains("Part1"), "missing 'Part1' in: {text:?}");
    assert!(text.contains("Part2"), "missing 'Part2' in: {text:?}");
}

#[test]
fn multipage_pdf_all_pages_accessible() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>");

    // Three pages, each with different text
    for (page_obj, stream_obj, text) in [(3, 6, "PageA"), (4, 7, "PageB"), (5, 8, "PageC")] {
        let page_body = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Contents {} 0 R /Resources << /Font << /F1 9 0 R >> >> >>",
            stream_obj
        );
        b.add_object(page_obj, page_body.as_bytes());
        let content = format!("BT /F1 12 Tf 100 700 Td ({}) Tj ET", text);
        b.add_stream_object(stream_obj, "", content.as_bytes());
    }

    b.add_object(
        9,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
          /Encoding /WinAnsiEncoding >>",
    );
    let data = b.finish(1);

    let mut doc = Document::from_bytes(data).expect("open");
    assert_eq!(doc.page_count(), 3);

    for (i, expected) in ["PageA", "PageB", "PageC"].iter().enumerate() {
        let mut page = doc.page(i).unwrap_or_else(|_| panic!("page {i}"));
        let text = page.text().unwrap_or_else(|_| panic!("text page {i}"));
        assert!(
            text.contains(expected),
            "page {i}: expected '{expected}', got: {text:?}"
        );
    }
}

#[test]
fn out_of_bounds_page_index_returns_error() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
    );
    let data = b.finish(1);

    let mut doc = Document::from_bytes(data).expect("open");
    assert_eq!(doc.page_count(), 1);

    let result = doc.page(1);
    assert!(result.is_err(), "page(1) on single-page PDF should error");

    let result = doc.page(999);
    assert!(result.is_err(), "page(999) should error");
}

// ===========================================================================
// 5. Diagnostics integration tests
// ===========================================================================

#[test]
fn diagnostics_capture_warnings_from_malformed_pdf() {
    // Build a PDF with wrong stream length to trigger diagnostics
    let mut b = PdfBuilder::new("1.4");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
    );

    // Manually write a stream with wrong /Length
    let actual_content = b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET";
    let wrong_length = actual_content.len() + 50; // intentionally wrong
    b.register_object_offset(4);
    let stream_header = format!("4 0 obj\n<< /Length {} >>\nstream\n", wrong_length);
    b.buf.extend_from_slice(stream_header.as_bytes());
    b.buf.extend_from_slice(actual_content);
    b.buf.extend_from_slice(b"\nendstream\nendobj\n");

    b.add_object(
        5,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
          /Encoding /WinAnsiEncoding >>",
    );
    let data = b.finish(1);

    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    let mut doc = Document::from_bytes_with_config(data, config).expect("open");

    // Text extraction should still work (recovery scanning)
    let mut page = doc.page(0).expect("page 0");
    let _text = page.text().expect("text");

    let warnings = diag.warnings();
    // Should have at least one warning about stream length mismatch
    let has_stream_warning = warnings.iter().any(|w| {
        matches!(w.kind, WarningKind::StreamLengthMismatch)
            || w.message.contains("length")
            || w.message.contains("endstream")
    });
    assert!(
        has_stream_warning,
        "expected stream length warning, got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

#[test]
fn diagnostics_sink_receives_font_warnings() {
    // Reference a font that does not exist in resources
    let content = b"BT /F99 12 Tf 100 700 Td (X) Tj ET";
    let pdf = build_single_page_pdf(content);

    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    let mut doc = Document::from_bytes_with_config(pdf, config).expect("open");
    let mut page = doc.page(0).expect("page 0");
    // This should produce warnings about unknown font /F99
    let _text = page.text().expect("text");

    let warnings = diag.warnings();
    let has_font_warning = warnings.iter().any(|w| {
        matches!(w.kind, WarningKind::FontError | WarningKind::InvalidState)
            || w.message.contains("font")
            || w.message.contains("F99")
    });
    assert!(
        has_font_warning,
        "expected font warning for /F99, got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

// ===========================================================================
// 6. Document structure edge cases
// ===========================================================================

#[test]
fn nested_pages_tree_resolves() {
    // Pages tree with nesting: root Pages -> intermediate Pages -> leaf Pages
    let mut b = PdfBuilder::new("1.4");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    // Root Pages node with two kids (one Pages node, one Page)
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>");
    // Intermediate Pages node
    b.add_object(
        3,
        b"<< /Type /Pages /Parent 2 0 R /Kids [4 0 R] /Count 1 >>",
    );
    // First page (under intermediate)
    b.add_object(
        4,
        b"<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] \
          /Contents 6 0 R /Resources << /Font << /F1 8 0 R >> >> >>",
    );
    // Second page (direct child of root)
    b.add_object(
        5,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 7 0 R /Resources << /Font << /F1 8 0 R >> >> >>",
    );
    b.add_stream_object(6, "", b"BT /F1 12 Tf 100 700 Td (Nested) Tj ET");
    b.add_stream_object(7, "", b"BT /F1 12 Tf 100 700 Td (Direct) Tj ET");
    b.add_object(
        8,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
          /Encoding /WinAnsiEncoding >>",
    );
    let data = b.finish(1);

    let mut doc = Document::from_bytes(data).expect("open");
    assert_eq!(doc.page_count(), 2);

    let mut p0 = doc.page(0).expect("page 0");
    let text0 = p0.text().expect("text 0");
    assert!(text0.contains("Nested"), "page 0: {text0:?}");

    let mut p1 = doc.page(1).expect("page 1");
    let text1 = p1.text().expect("text 1");
    assert!(text1.contains("Direct"), "page 1: {text1:?}");
}

#[test]
fn config_with_restrictive_decode_limits() {
    let content = b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET";
    let pdf = build_single_page_pdf(content);

    // Very restrictive decode limits: 1 byte max decompressed size
    let mut limits = DecodeLimits::default();
    limits.max_decompressed_size = 1;
    limits.max_decompression_ratio = 1;
    limits.ratio_floor_size = 0;
    let config = Config::default().with_decode_limits(limits);
    let result = Document::from_bytes_with_config(pdf, config);
    // The content stream is uncompressed, so decode limits should not block it.
    // Should not panic regardless.
    if let Ok(mut doc) = result {
        if let Ok(mut page) = doc.page(0) {
            let _ = page.text();
        }
    }
}

// ===========================================================================
// 7. Table extraction through public API
// ===========================================================================

#[test]
fn table_extraction_on_page_with_no_paths() {
    // Page with text but no drawn paths -- table detection should return empty
    let content = b"BT /F1 12 Tf 100 700 Td (Just text) Tj ET";
    let pdf = build_single_page_pdf(content);
    let mut doc = Document::from_bytes(pdf).expect("open");
    let mut page = doc.page(0).expect("page 0");
    let tables = page.tables().expect("tables");
    // No ruled lines, so ruled-line detector should find nothing (text-edge
    // detection may or may not fire depending on span count)
    assert!(
        tables.len() <= 1,
        "page with only one text span should have at most 1 table, got {}",
        tables.len()
    );
}

#[test]
fn table_extraction_on_empty_page() {
    let mut b = PdfBuilder::new("1.4");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
    );
    let data = b.finish(1);

    let mut doc = Document::from_bytes(data).expect("open");
    let mut page = doc.page(0).expect("page 0");
    let tables = page.tables().expect("tables");
    assert!(tables.is_empty(), "empty page should have no tables");
}

// ===========================================================================
// 8. Parse -> Object contract tests
// ===========================================================================

#[test]
fn document_parser_produces_valid_xref_for_resolver() {
    // Verify that DocumentParser output can be consumed by ObjectResolver
    let mut b = PdfBuilder::new("1.4");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.add_object(2, b"<< /Type /Pages /Kids [] /Count 0 >>");
    b.add_object(3, b"(test string)");
    let data = b.finish(1);

    let doc = DocumentParser::new(&data).parse().expect("parse");

    // Verify xref has entries for our objects
    assert!(doc.xref.len() >= 3, "xref should have at least 3 entries");

    // Feed into resolver
    let mut resolver = ObjectResolver::from_document(&data, doc);

    // Resolve each object
    let catalog = resolver.resolve_dict(ObjRef::new(1, 0)).expect("catalog");
    assert_eq!(catalog.get_name(b"Type"), Some(b"Catalog".as_slice()));

    let pages = resolver.resolve_dict(ObjRef::new(2, 0)).expect("pages");
    assert_eq!(pages.get_name(b"Type"), Some(b"Pages".as_slice()));

    let obj3 = resolver.resolve(ObjRef::new(3, 0)).expect("obj 3");
    let s = obj3.as_pdf_string().expect("expected string");
    assert_eq!(s.as_bytes(), b"test string");
}

#[test]
fn document_parser_incremental_update_xref_merged() {
    // Build a PDF that simulates an incremental update by having
    // two xref sections. The second xref should override the first.
    // We test this using the xelatex-drawboard.pdf from the corpus.
    let data = std::fs::read("tests/corpus/minimal/xelatex-drawboard.pdf")
        .expect("read xelatex-drawboard.pdf");

    let diag = Arc::new(CollectingDiagnostics::new());
    let doc = DocumentParser::with_diagnostics(&data, diag.clone())
        .parse()
        .expect("parse");

    // Verify the merged xref has entries from multiple revisions
    assert!(
        doc.xref.len() > 5,
        "incremental update should merge many xref entries, got {}",
        doc.xref.len()
    );

    // The resolver should work with merged xref
    let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag);
    let trailer = resolver.trailer().expect("trailer").clone();
    let root_ref = trailer.get_ref(b"Root").expect("/Root");
    let catalog = resolver.resolve_dict(root_ref).expect("catalog");
    assert_eq!(catalog.get_name(b"Type"), Some(b"Catalog".as_slice()));
}

// ===========================================================================
// 9. Text ordering integration
// ===========================================================================

#[test]
fn text_lines_api_returns_positioned_lines() {
    let content = b"BT /F1 12 Tf 72 720 Td (Line One) Tj 0 -14 Td (Line Two) Tj ET";
    let pdf = build_single_page_pdf(content);
    let mut doc = Document::from_bytes(pdf).expect("open");
    let mut page = doc.page(0).expect("page 0");
    let lines = page.text_lines().expect("text_lines");
    assert!(
        lines.len() >= 2,
        "expected at least 2 lines, got {}",
        lines.len()
    );

    // Lines should have position info
    for line in &lines {
        // Baseline should be set
        assert!(line.baseline.is_finite(), "baseline should be finite");
    }
}

#[test]
fn raw_spans_api_preserves_stream_order() {
    let content = b"BT /F1 12 Tf 72 720 Td (First) Tj 0 -14 Td (Second) Tj ET";
    let pdf = build_single_page_pdf(content);
    let mut doc = Document::from_bytes(pdf).expect("open");
    let mut page = doc.page(0).expect("page 0");
    let spans = page.raw_spans().expect("raw_spans");
    assert!(
        spans.len() >= 2,
        "expected at least 2 spans, got {}",
        spans.len()
    );
    // First span should contain "First" and come before "Second" in stream order
    let first_idx = spans.iter().position(|s| s.text.contains("First"));
    let second_idx = spans.iter().position(|s| s.text.contains("Second"));
    assert!(first_idx.is_some(), "should have span with 'First'");
    assert!(second_idx.is_some(), "should have span with 'Second'");
    assert!(
        first_idx.unwrap() < second_idx.unwrap(),
        "raw_spans should preserve stream order"
    );
}

// ===========================================================================
// 10. Form XObject (content stream within content stream)
// ===========================================================================

#[test]
fn form_xobject_text_extracted() {
    // Use a real corpus PDF that has a form XObject
    let data =
        std::fs::read("tests/corpus/minimal/form_xobject.pdf").expect("read form_xobject.pdf");
    let mut doc = Document::from_bytes(data).expect("open");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text");
    // form_xobject.pdf should produce some text (from the XObject)
    assert!(
        !text.is_empty(),
        "form_xobject.pdf should produce non-empty text"
    );
}

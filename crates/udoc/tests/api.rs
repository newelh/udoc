//! T-026: API + serialization integration tests.
//!
//! Uses committed minimal corpus PDFs so all tests run on every checkout.

use std::path::PathBuf;

fn test_pdf() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../udoc-pdf/tests/corpus/minimal/table_layout.pdf")
}

// ---------------------------------------------------------------------------
// extract() round-trip
// ---------------------------------------------------------------------------

#[test]
fn extract_roundtrip() {
    let doc = udoc::extract(test_pdf()).expect("extract should succeed");
    assert!(!doc.content.is_empty(), "document should have content");
    assert_eq!(doc.metadata.page_count, 1, "table_layout.pdf has 1 page");
}

// ---------------------------------------------------------------------------
// Extractor streaming
// ---------------------------------------------------------------------------

#[test]
fn extractor_page_text() {
    let mut ext = udoc::Extractor::open(test_pdf()).expect("open should succeed");
    assert_eq!(ext.page_count(), 1);

    let text = ext.page_text(0).expect("page_text should succeed");
    assert!(text.contains("Alice"), "page 0 should contain table data");
}

#[test]
fn extractor_metadata() {
    let ext = udoc::Extractor::open(test_pdf()).expect("open should succeed");
    let meta = ext.metadata();
    assert_eq!(meta.page_count, 1);
}

#[test]
fn extractor_format() {
    let ext = udoc::Extractor::open(test_pdf()).expect("open should succeed");
    assert_eq!(ext.format(), udoc::Format::Pdf);
}

#[test]
fn extractor_into_document() {
    let ext = udoc::Extractor::open(test_pdf()).expect("open should succeed");
    let doc = ext.into_document().expect("into_document should succeed");
    assert!(!doc.content.is_empty());
}

// ---------------------------------------------------------------------------
// Config propagation
// ---------------------------------------------------------------------------

#[test]
fn config_content_only() {
    let config = udoc::Config::new().layers(udoc::LayerConfig::content_only());
    let doc = udoc::extract_with(test_pdf(), config).expect("extract should succeed");
    assert!(
        doc.presentation.is_none(),
        "content_only should suppress presentation"
    );
}

// ---------------------------------------------------------------------------
// Format detection
// ---------------------------------------------------------------------------

#[test]
fn detect_format_pdf_magic() {
    let format = udoc::detect::detect_format(b"%PDF-1.7 blah blah");
    assert_eq!(format, Some(udoc::Format::Pdf));
}

#[test]
fn detect_format_rtf_magic() {
    let format = udoc::detect::detect_format(b"{\\rtf1\\ansi something");
    assert_eq!(format, Some(udoc::Format::Rtf));
}

#[test]
fn detect_format_unknown() {
    let format = udoc::detect::detect_format(b"this is not a document");
    assert_eq!(format, None);
}

// ---------------------------------------------------------------------------
// JSON serialization
// ---------------------------------------------------------------------------

#[test]
fn json_serialization_version() {
    let doc = udoc::extract(test_pdf()).expect("extract should succeed");
    let json = serde_json::to_string(&doc).expect("serialize should succeed");
    assert!(
        json.contains("\"version\":1"),
        "JSON should contain version:1"
    );
    assert!(json.contains("\"content\""), "JSON should contain content");
    assert!(
        json.contains("\"metadata\""),
        "JSON should contain metadata"
    );
}

#[test]
fn json_roundtrip() {
    let doc = udoc::extract(test_pdf()).expect("extract should succeed");
    let json = serde_json::to_string(&doc).expect("serialize should succeed");
    let deserialized: udoc::Document =
        serde_json::from_str(&json).expect("deserialize should succeed");
    assert_eq!(
        deserialized.content.len(),
        doc.content.len(),
        "content length should match after round-trip"
    );
    assert_eq!(
        deserialized.metadata.page_count, doc.metadata.page_count,
        "page count should match after round-trip"
    );
    // Verify actual content survived round-trip (not just length).
    for (i, (orig, deser)) in doc
        .content
        .iter()
        .zip(deserialized.content.iter())
        .enumerate()
    {
        assert_eq!(
            orig.text(),
            deser.text(),
            "block {i} text should match after round-trip"
        );
    }
    assert_eq!(
        deserialized.assets.images().len(),
        doc.assets.images().len(),
        "image count should match after round-trip"
    );
    // Verify presentation layer survives round-trip.
    assert_eq!(
        deserialized.presentation.is_some(),
        doc.presentation.is_some(),
        "presentation layer presence should match"
    );
}

// ---------------------------------------------------------------------------
// JSONL format
// ---------------------------------------------------------------------------

#[test]
fn jsonl_format() {
    let doc = udoc::extract(test_pdf()).expect("extract should succeed");

    let mut buf = Vec::new();
    udoc::output::jsonl::write_jsonl(&doc, "PDF", &mut buf, None, 0).expect("write_jsonl");
    let output = String::from_utf8(buf).expect("valid utf8");
    let lines: Vec<&str> = output.lines().collect();

    // At least header + footer
    assert!(lines.len() >= 2, "JSONL should have at least 2 lines");

    // First line: header
    let header: serde_json::Value =
        serde_json::from_str(lines[0]).expect("header should be valid JSON");
    assert_eq!(header["udoc"], "header");
    assert_eq!(header["version"], 1);

    // Last line: footer
    let footer: serde_json::Value =
        serde_json::from_str(lines[lines.len() - 1]).expect("footer should be valid JSON");
    assert_eq!(footer["udoc"], "footer");
}

// ---------------------------------------------------------------------------
// TSV output
// ---------------------------------------------------------------------------

#[test]
fn tsv_output() {
    let doc = udoc::extract(test_pdf()).expect("extract should succeed");

    let mut buf = Vec::new();
    let pa = doc.presentation.as_ref().map(|p| &p.page_assignments);
    udoc::output::tables::write_tables(&doc, &mut buf, pa).expect("write_tables");
    let output = String::from_utf8(buf).expect("valid utf8");
    assert!(
        output.contains("Name"),
        "table output should contain table headers"
    );
}

// ---------------------------------------------------------------------------
// Default text output
// ---------------------------------------------------------------------------

#[test]
fn text_output() {
    let doc = udoc::extract(test_pdf()).expect("extract should succeed");

    let mut buf = Vec::new();
    udoc::output::text::write_text(&doc, &mut buf).expect("write_text");
    let output = String::from_utf8(buf).expect("valid utf8");
    assert!(!output.is_empty(), "text output should be non-empty");
    assert!(
        output.contains("Alice"),
        "text output should contain table data"
    );
}

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

#[test]
fn extract_nonexistent_file() {
    let result = udoc::extract("/nonexistent/path/to/file.pdf");
    assert!(result.is_err(), "extracting nonexistent file should error");
}

// ---------------------------------------------------------------------------
// PageRange
// ---------------------------------------------------------------------------

#[test]
fn page_range_parse_simple() {
    let range = udoc::PageRange::parse("1,3,5-10").expect("valid range");
    assert!(range.contains(0)); // page 1
    assert!(!range.contains(1)); // page 2
    assert!(range.contains(2)); // page 3
    assert!(!range.contains(3)); // page 4
    assert!(range.contains(4)); // page 5
    assert!(range.contains(9)); // page 10
    assert!(!range.contains(10)); // page 11
    assert_eq!(range.len(), 8); // 1, 3, 5, 6, 7, 8, 9, 10
}

#[test]
fn page_range_errors() {
    assert!(udoc::PageRange::parse("0").is_err());
    assert!(udoc::PageRange::parse("abc").is_err());
    assert!(udoc::PageRange::parse("5-3").is_err());
}

// ---------------------------------------------------------------------------
// extract_bytes
// ---------------------------------------------------------------------------

#[test]
fn extract_bytes_pdf() {
    let data = std::fs::read(test_pdf()).expect("read file");
    let doc = udoc::extract_bytes(&data).expect("extract_bytes should succeed");
    assert!(!doc.content.is_empty());
}

#[test]
fn extract_bytes_invalid() {
    let result = udoc::extract_bytes(b"this is not a PDF");
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// JSON output via write_json
// ---------------------------------------------------------------------------

#[test]
fn json_no_presentation_key() {
    let doc = udoc::extract(test_pdf()).expect("extract should succeed");

    let mut buf = Vec::new();
    udoc::output::json::write_json(&doc, &mut buf, false, false, false).expect("write_json");
    let output = String::from_utf8(buf).expect("valid utf8");
    assert!(
        !output.contains("\"presentation\""),
        "no-presentation should omit presentation key"
    );
}

#[test]
fn json_pretty_output() {
    let doc = udoc::extract(test_pdf()).expect("extract should succeed");

    let mut buf = Vec::new();
    udoc::output::json::write_json(&doc, &mut buf, true, true, false).expect("write_json");
    let output = String::from_utf8(buf).expect("valid utf8");
    assert!(
        output.lines().count() > 1,
        "pretty JSON should be multi-line"
    );
}

// ---------------------------------------------------------------------------
// Presentation.pages populated
// ---------------------------------------------------------------------------

#[test]
fn presentation_pages_populated() {
    let doc = udoc::extract(test_pdf()).expect("extract should succeed");
    let pres = doc
        .presentation
        .as_ref()
        .expect("default extraction should include presentation");

    assert!(
        !pres.pages.is_empty(),
        "presentation.pages should be populated"
    );

    let page0 = &pres.pages[0];
    assert_eq!(page0.index, 0);
    assert!(page0.width > 0.0, "page width should be positive");
    assert!(page0.height > 0.0, "page height should be positive");
    // Standard rotations only
    assert!(
        matches!(page0.rotation, 0 | 90 | 180 | 270),
        "rotation should be 0/90/180/270"
    );
}

#[test]
fn presentation_pages_match_page_count() {
    let doc = udoc::extract(test_pdf()).expect("extract should succeed");
    let pres = doc.presentation.as_ref().unwrap();

    assert_eq!(
        pres.pages.len(),
        doc.metadata.page_count,
        "pages vec length should match page_count"
    );
}

// ---------------------------------------------------------------------------
// Limits enforcement
// ---------------------------------------------------------------------------

#[test]
fn limits_max_file_size_rejects_large_file() {
    use udoc_core::limits::Limits;

    // Set a tiny max_file_size (1 byte) so any real file is rejected.
    let config = udoc::Config::new().limits(Limits::builder().max_file_size(1).build());
    let result = udoc::extract_with(test_pdf(), config);
    assert!(
        result.is_err(),
        "file exceeding max_file_size should be rejected"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("exceeds max_file_size"),
        "error should mention max_file_size limit, got: {err}"
    );
}

#[test]
fn limits_max_file_size_rejects_large_bytes() {
    use udoc_core::limits::Limits;

    let data = std::fs::read(test_pdf()).expect("read file");
    // Set limit to 10 bytes (any valid PDF is larger).
    let config = udoc::Config::new().limits(Limits::builder().max_file_size(10).build());
    let result = udoc::extract_bytes_with(&data, config);
    assert!(
        result.is_err(),
        "bytes exceeding max_file_size should be rejected"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("exceeds max_file_size"),
        "error should mention max_file_size limit, got: {err}"
    );
}

#[test]
fn limits_generous_allows_normal_file() {
    use udoc_core::limits::Limits;

    // 256 MB limit (the default) should allow our test PDF.
    let config =
        udoc::Config::new().limits(Limits::builder().max_file_size(256 * 1024 * 1024).build());
    let doc = udoc::extract_with(test_pdf(), config).expect("normal file should pass limits");
    assert!(!doc.content.is_empty());
}

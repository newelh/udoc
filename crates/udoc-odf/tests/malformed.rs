//! Malformed input recovery tests for ODF backend.
//!
//! Verifies that the ODF parser handles broken input gracefully:
//! returns errors with context (not panics), and recovers from
//! partial/empty content where possible.

use udoc_containers::test_util::build_stored_zip;
use udoc_core::backend::{FormatBackend, PageExtractor};

// ---------------------------------------------------------------------------
// 1. Truncated/random bytes that are not a valid ZIP.
// ---------------------------------------------------------------------------

#[test]
fn malformed_truncated_zip() {
    let garbage = b"PK\x03\x04not-really-a-zip-just-random-garbage-bytes-1234567890";
    let result = udoc_odf::OdfDocument::from_bytes(garbage);
    match result {
        Ok(_) => panic!("truncated ZIP should return an error, not succeed"),
        Err(e) => {
            let err_msg = format!("{e}");
            assert!(
                err_msg.contains("ZIP") || err_msg.contains("zip") || err_msg.contains("ODF"),
                "error should mention ZIP or ODF context, got: {err_msg}"
            );
        }
    }
}

#[test]
fn malformed_random_bytes() {
    let random = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0xFF, 0x42, 0x13];
    let result = udoc_odf::OdfDocument::from_bytes(&random);
    assert!(
        result.is_err(),
        "random bytes should return an error, not panic"
    );
}

// ---------------------------------------------------------------------------
// 2. Valid ZIP with mimetype but no content.xml.
// ---------------------------------------------------------------------------

#[test]
fn malformed_missing_content_xml() {
    let data = build_stored_zip(&[(
        "mimetype",
        b"application/vnd.oasis.opendocument.text" as &[u8],
    )]);

    let result = udoc_odf::OdfDocument::from_bytes(&data);
    match result {
        Ok(_) => panic!("ODF ZIP without content.xml should return an error, not succeed"),
        Err(e) => {
            let err_msg = format!("{e}");
            assert!(
                err_msg.contains("content.xml"),
                "error should mention missing content.xml, got: {err_msg}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 3. Valid ODF ZIP where content.xml has an empty office:body.
// ---------------------------------------------------------------------------

#[test]
fn malformed_empty_content_body() {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text/>
  </office:body>
</office:document-content>"#;

    let data = build_stored_zip(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.text" as &[u8],
        ),
        ("content.xml", content_xml as &[u8]),
    ]);

    // Should succeed with an empty document, not error.
    let mut doc = udoc_odf::OdfDocument::from_bytes(&data).expect("empty body should parse OK");
    assert_eq!(doc.page_count(), 1, "ODT always has 1 logical page");

    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert!(
        text.is_empty(),
        "empty body should produce empty text, got: {text:?}"
    );
}

// ---------------------------------------------------------------------------
// 4. Valid ODF ZIP with empty office:spreadsheet body.
// ---------------------------------------------------------------------------

#[test]
fn malformed_empty_spreadsheet_body() {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0">
  <office:body>
    <office:spreadsheet/>
  </office:body>
</office:document-content>"#;

    let data = build_stored_zip(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.spreadsheet" as &[u8],
        ),
        ("content.xml", content_xml as &[u8]),
    ]);

    let doc = udoc_odf::OdfDocument::from_bytes(&data).expect("empty spreadsheet should parse OK");
    assert_eq!(doc.page_count(), 0, "empty spreadsheet has 0 sheets");
}

// ---------------------------------------------------------------------------
// 5. Valid ODF ZIP with empty office:presentation body.
// ---------------------------------------------------------------------------

#[test]
fn malformed_empty_presentation_body() {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
    xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0">
  <office:body>
    <office:presentation/>
  </office:body>
</office:document-content>"#;

    let data = build_stored_zip(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.presentation" as &[u8],
        ),
        ("content.xml", content_xml as &[u8]),
    ]);

    let doc = udoc_odf::OdfDocument::from_bytes(&data).expect("empty presentation should parse OK");
    assert_eq!(doc.page_count(), 0, "empty presentation has 0 slides");
}

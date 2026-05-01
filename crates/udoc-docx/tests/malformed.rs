//! Tests for DOCX malformed file recovery.
//!
//! Each test verifies that a malformed DOCX file can be handled without
//! panicking: either returning Err or Ok with partial content.

use std::sync::Arc;

use udoc_containers::test_util::{build_stored_zip, DOCX_CONTENT_TYPES, DOCX_PACKAGE_RELS};
use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_core::diagnostics::CollectingDiagnostics;

/// Random bytes that are not a ZIP file at all.
#[test]
fn truncated_zip_no_panic() {
    let garbage = b"This is definitely not a ZIP file \x00\xFF\xFE\x01";
    let result = udoc_docx::DocxDocument::from_bytes(garbage);
    // Must not panic. Should return Err.
    assert!(result.is_err(), "truncated/garbage input should return Err");
}

/// More realistic truncation: start of a ZIP signature then cut off.
#[test]
fn truncated_zip_partial_header_no_panic() {
    let partial = b"PK\x03\x04truncated";
    let result = udoc_docx::DocxDocument::from_bytes(partial);
    assert!(result.is_err(), "truncated ZIP should return Err");
}

/// Empty input.
#[test]
fn empty_input_no_panic() {
    let result = udoc_docx::DocxDocument::from_bytes(b"");
    assert!(result.is_err(), "empty input should return Err");
}

/// Valid ZIP archive but missing word/document.xml entirely.
#[test]
fn missing_document_xml_no_panic() {
    // ZIP with only [Content_Types].xml and _rels/.rels, no word/document.xml.
    let data = build_stored_zip(&[
        ("[Content_Types].xml", DOCX_CONTENT_TYPES),
        ("_rels/.rels", DOCX_PACKAGE_RELS),
    ]);
    let result = udoc_docx::DocxDocument::from_bytes(&data);
    // Should fail gracefully (Err), not panic.
    assert!(
        result.is_err(),
        "missing word/document.xml should return Err"
    );
}

/// Valid ZIP with document.xml present but containing garbled XML content.
#[test]
fn invalid_xml_no_panic() {
    let bad_xml = b"<<<this is not valid XML at all>>>>&&&";
    let data = build_stored_zip(&[
        ("[Content_Types].xml", DOCX_CONTENT_TYPES),
        ("_rels/.rels", DOCX_PACKAGE_RELS),
        ("word/document.xml", bad_xml as &[u8]),
    ]);
    let diag = Arc::new(CollectingDiagnostics::new());
    let result = udoc_docx::DocxDocument::from_bytes_with_diag(&data, diag.clone());
    // Should return Err or Ok with empty content. Must not panic.
    match result {
        Err(_) => {} // expected
        Ok(mut doc) => {
            // If it somehow parses, it should produce empty or partial content.
            if let Ok(mut page) = doc.page(0) {
                let _ = page.text(); // must not panic
            }
            assert!(
                !diag.warnings().is_empty(),
                "invalid XML should produce at least one warning"
            );
        }
    }
}

/// Valid ZIP + valid XML structure but with mangled namespace URIs.
#[test]
fn wrong_namespace_no_panic() {
    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://example.com/not-a-real-namespace">
    <w:body>
        <w:p><w:r><w:t>Some text</w:t></w:r></w:p>
    </w:body>
</w:document>"#;

    let data = build_stored_zip(&[
        ("[Content_Types].xml", DOCX_CONTENT_TYPES),
        ("_rels/.rels", DOCX_PACKAGE_RELS),
        ("word/document.xml", document_xml as &[u8]),
    ]);

    let diag = Arc::new(CollectingDiagnostics::new());
    let result = udoc_docx::DocxDocument::from_bytes_with_diag(&data, diag.clone());
    // Must not panic. May return Err or Ok with empty text.
    match result {
        Err(_) => {}
        Ok(mut doc) => {
            if let Ok(mut page) = doc.page(0) {
                let _ = page.text(); // must not panic
            }
            // Note: the DOCX backend currently parses wrong-namespace XML
            // without emitting diagnostics. A future improvement could warn
            // when the document namespace does not match the expected WML URI.
        }
    }
}

/// Valid ZIP but no [Content_Types].xml at all.
#[test]
fn missing_content_types_returns_err() {
    let data = build_stored_zip(&[
        ("_rels/.rels", DOCX_PACKAGE_RELS),
        ("word/document.xml", b"<w:document/>" as &[u8]),
    ]);
    let result = udoc_docx::DocxDocument::from_bytes(&data);
    assert!(
        result.is_err(),
        "missing [Content_Types].xml should return Err"
    );
}

/// Valid ZIP + [Content_Types].xml but no officeDocument relationship in _rels/.rels.
#[test]
fn missing_office_document_rel_returns_err() {
    let empty_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;

    let data = build_stored_zip(&[
        ("[Content_Types].xml", DOCX_CONTENT_TYPES),
        ("_rels/.rels", empty_rels as &[u8]),
    ]);
    let result = udoc_docx::DocxDocument::from_bytes(&data);
    assert!(
        result.is_err(),
        "missing officeDocument relationship should return Err"
    );
}

/// XML with deeply nested elements (tests depth limit).
#[test]
fn deeply_nested_xml_no_panic() {
    // Build XML with 300 levels of nesting (exceeds MAX_NESTING_DEPTH of 256).
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>"#,
    );
    for _ in 0..300 {
        xml.push_str("<w:p>");
    }
    xml.push_str("<w:r><w:t>Deep</w:t></w:r>");
    for _ in 0..300 {
        xml.push_str("</w:p>");
    }
    xml.push_str("</w:body></w:document>");

    let data = build_stored_zip(&[
        ("[Content_Types].xml", DOCX_CONTENT_TYPES),
        ("_rels/.rels", DOCX_PACKAGE_RELS),
        ("word/document.xml", xml.as_bytes()),
    ]);

    let diag = Arc::new(CollectingDiagnostics::new());
    let result = udoc_docx::DocxDocument::from_bytes_with_diag(&data, diag.clone());
    // Must not panic. May skip deeply nested content or return Err.
    match result {
        Err(_) => {}
        Ok(mut doc) => {
            if let Ok(mut page) = doc.page(0) {
                let _ = page.text(); // must not panic
            }
            assert!(
                !diag.warnings().is_empty(),
                "deeply nested XML should produce at least one warning"
            );
        }
    }
}

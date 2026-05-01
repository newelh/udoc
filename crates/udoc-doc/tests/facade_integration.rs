//! Facade integration test for DOC backend.
//!
//! Verifies that the full udoc::extract_bytes() pipeline works for DOC files.

use udoc_doc::test_util::build_minimal_doc;

#[test]
fn facade_extract_bytes_doc() {
    let data = build_minimal_doc("Facade integration test");
    let doc = udoc::extract_bytes(&data).expect("extract_bytes should succeed for DOC");

    // The Document model should have some content.
    assert!(
        !doc.content.is_empty(),
        "extracted Document should have content blocks"
    );
}

#[test]
fn facade_extract_bytes_two_paragraphs() {
    let data = build_minimal_doc("First para\rSecond para");
    let doc = udoc::extract_bytes(&data).expect("extract_bytes should succeed");

    // Should have at least 2 content blocks (one per paragraph).
    assert!(
        doc.content.len() >= 2,
        "expected at least 2 blocks, got {}",
        doc.content.len()
    );
}

#[test]
fn facade_extract_bytes_empty_doc() {
    let data = build_minimal_doc("");
    let doc = udoc::extract_bytes(&data).expect("extract_bytes should succeed for empty DOC");

    // Empty doc produces a valid Document (possibly with no content blocks).
    assert_eq!(
        doc.metadata.page_count, 1,
        "DOC metadata should show 1 page"
    );
}

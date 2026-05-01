//! XLS integration tests for the udoc facade.

use udoc_xls::test_util::build_minimal_xls;

// ---------------------------------------------------------------------------
// extract_bytes_with / Extractor tests
// ---------------------------------------------------------------------------

#[test]
fn extract_bytes_xls_single_sheet() {
    let data = build_minimal_xls(
        &["Hello", "World"],
        &[("Sheet1", &[(0, 0, "Hello"), (0, 1, "World")])],
    );
    let doc = udoc::extract_bytes(&data).expect("extract_bytes should succeed");

    assert_eq!(doc.metadata.page_count, 1);
    assert!(!doc.content.is_empty(), "document should have content");

    // Check the table content.
    let table_blocks: Vec<_> = doc
        .content
        .iter()
        .filter(|b| matches!(b, udoc::Block::Table { .. }))
        .collect();
    assert_eq!(table_blocks.len(), 1, "should have one table block");
}

#[test]
fn extractor_xls_page_text() {
    let data = build_minimal_xls(
        &["foo", "bar"],
        &[("Sheet1", &[(0, 0, "foo"), (0, 1, "bar")])],
    );
    let mut ext =
        udoc::Extractor::from_bytes_with(&data, udoc::Config::default()).expect("should open");
    assert_eq!(ext.page_count(), 1);

    let text = ext.page_text(0).expect("should extract text");
    assert!(
        text.contains("foo"),
        "text should contain 'foo', got: {text}"
    );
    assert!(
        text.contains("bar"),
        "text should contain 'bar', got: {text}"
    );
}

#[test]
fn extractor_xls_multi_sheet() {
    let data = build_minimal_xls(
        &["a", "b"],
        &[("Sheet1", &[(0, 0, "a")]), ("Sheet2", &[(0, 0, "b")])],
    );
    let mut ext = udoc::Extractor::from_bytes_with(&data, udoc::Config::default())
        .expect("should open multi-sheet XLS");
    assert_eq!(ext.page_count(), 2);
    assert_eq!(ext.page_text(0).unwrap(), "a");
    assert_eq!(ext.page_text(1).unwrap(), "b");
}

#[test]
fn extract_bytes_xls_empty_workbook() {
    let data = build_minimal_xls(&[], &[]);
    let doc = udoc::extract_bytes(&data).expect("extract_bytes should succeed on empty workbook");
    assert_eq!(doc.metadata.page_count, 0);
    assert!(doc.content.is_empty());
}

#[test]
fn extractor_xls_into_document() {
    let data = build_minimal_xls(&["val"], &[("Sheet1", &[(0, 0, "val")])]);
    let ext =
        udoc::Extractor::from_bytes_with(&data, udoc::Config::default()).expect("should open");
    let doc = ext.into_document().expect("should convert to document");
    assert_eq!(doc.metadata.page_count, 1);

    let has_table = doc
        .content
        .iter()
        .any(|b| matches!(b, udoc::Block::Table { .. }));
    assert!(has_table, "document should contain a table block");
}

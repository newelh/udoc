//! RTF integration tests for the udoc facade.

use std::path::PathBuf;

fn corpus_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../udoc-rtf/tests/corpus")
        .join(name)
}

// ---------------------------------------------------------------------------
// extract() one-shot
// ---------------------------------------------------------------------------

#[test]
fn extract_rtf_basic() {
    let doc = udoc::extract(corpus_path("basic.rtf")).expect("extract should succeed");
    assert!(!doc.content.is_empty(), "document should have content");
    assert_eq!(doc.metadata.page_count, 1);

    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        all_text.contains("Hello"),
        "should contain 'Hello', got: {all_text}"
    );
}

#[test]
fn extract_rtf_metadata() {
    let doc = udoc::extract(corpus_path("metadata.rtf")).expect("extract should succeed");
    assert_eq!(doc.metadata.title.as_deref(), Some("Test Document"));
    assert_eq!(doc.metadata.author.as_deref(), Some("Test Author"));
}

// ---------------------------------------------------------------------------
// extract_bytes()
// ---------------------------------------------------------------------------

#[test]
fn extract_bytes_rtf() {
    let data = std::fs::read(corpus_path("basic.rtf")).expect("read file");
    let doc = udoc::extract_bytes(&data).expect("extract_bytes should succeed");
    assert!(!doc.content.is_empty());
}

#[test]
fn extract_bytes_rtf_inline() {
    let data = b"{\\rtf1 Hello from bytes}";
    let doc = udoc::extract_bytes(data).expect("extract_bytes should succeed");
    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(all_text, "Hello from bytes");
}

// ---------------------------------------------------------------------------
// Extractor streaming
// ---------------------------------------------------------------------------

#[test]
fn extractor_rtf_page_text() {
    let mut ext = udoc::Extractor::open(corpus_path("basic.rtf")).expect("open should succeed");
    assert_eq!(ext.page_count(), 1);
    assert_eq!(ext.format(), udoc::Format::Rtf);

    let text = ext.page_text(0).expect("page_text should succeed");
    assert!(text.contains("Hello"), "got: {text}");
}

#[test]
fn extractor_rtf_into_document() {
    let ext = udoc::Extractor::open(corpus_path("basic.rtf")).expect("open should succeed");
    let doc = ext.into_document().expect("into_document should succeed");
    assert!(!doc.content.is_empty());
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

#[test]
fn extract_rtf_with_tables() {
    let doc = udoc::extract(corpus_path("table_basic.rtf")).expect("extract should succeed");

    let has_table = doc
        .content
        .iter()
        .any(|b| matches!(b, udoc::Block::Table { .. }));
    assert!(has_table, "RTF with tables should produce Table blocks");

    let table_text: String = doc
        .content
        .iter()
        .filter(|b| matches!(b, udoc::Block::Table { .. }))
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        table_text.contains("Name"),
        "table should contain 'Name', got: {table_text}"
    );
}

// ---------------------------------------------------------------------------
// Hidden text filtering
// ---------------------------------------------------------------------------

#[test]
fn extract_rtf_hidden_text_filtered() {
    let doc = udoc::extract(corpus_path("hidden_text.rtf")).expect("extract should succeed");
    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all_text.contains("Visible"), "got: {all_text}");
    assert!(
        !all_text.contains("Hidden"),
        "hidden text should be filtered, got: {all_text}"
    );
}

// ---------------------------------------------------------------------------
// RTF presentation layer: text styling (font, color, size) and block layout
// ---------------------------------------------------------------------------

#[test]
fn extract_rtf_has_presentation_with_styling() {
    let doc = udoc::extract(corpus_path("basic.rtf")).expect("extract should succeed");
    // RTF now routes through the backend crate's converter (udoc_rtf::rtf_to_document)
    // which populates text_styling (font name, color, size) and block_layout (alignment,
    // indent, spacing). basic.rtf has a \fonttbl with "Times New Roman", so the
    // presentation layer should contain at least one text_styling entry with a font name.
    let pres = doc
        .presentation
        .as_ref()
        .expect("RTF should produce a presentation layer with text styling");
    assert!(
        !pres.text_styling.is_empty(),
        "RTF presentation should have text_styling entries for font names"
    );
}

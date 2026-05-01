//! PPT integration tests for the udoc facade.
//!
//! Tests the end-to-end pipeline: CFB bytes -> PptDocument -> Document model.
//! Uses in-memory constructed PPT files since we don't have corpus .ppt files.

use udoc_ppt::test_util::*;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn extract_bytes_ppt_basic() {
    let mut slwt = Vec::new();
    slwt.extend_from_slice(&build_slide_persist_atom(1));
    slwt.extend_from_slice(&build_text_header_atom(0)); // Title
    slwt.extend_from_slice(&build_text_chars_atom("PPT Facade Test"));
    slwt.extend_from_slice(&build_text_header_atom(1)); // Body
    slwt.extend_from_slice(&build_text_chars_atom("Body content"));

    let data = build_ppt_cfb(&slwt, &[]);
    let config = udoc::Config::new()
        .format(udoc::Format::Ppt)
        .layers(udoc::LayerConfig::content_only());
    let doc = udoc::extract_bytes_with(&data, config).expect("extract should succeed");

    assert_eq!(doc.metadata.page_count, 1);
    assert!(!doc.content.is_empty(), "document should have content");

    // First block should be a heading (title).
    assert!(
        matches!(&doc.content[0], udoc::Block::Heading { level: 1, .. }),
        "title should be H1, got: {:?}",
        &doc.content[0]
    );
    assert_eq!(doc.content[0].text(), "PPT Facade Test");

    // Second block should be a paragraph (body).
    assert!(
        matches!(&doc.content[1], udoc::Block::Paragraph { .. }),
        "body should be paragraph"
    );
    assert_eq!(doc.content[1].text(), "Body content");
}

#[test]
fn extract_bytes_ppt_format_detection() {
    let mut slwt = Vec::new();
    slwt.extend_from_slice(&build_slide_persist_atom(1));
    slwt.extend_from_slice(&build_text_header_atom(0));
    slwt.extend_from_slice(&build_text_chars_atom("Auto-detect PPT"));

    let data = build_ppt_cfb(&slwt, &[]);

    // No explicit format -- should auto-detect from CFB magic + "PowerPoint Document" stream.
    let doc = udoc::extract_bytes(&data).expect("format auto-detection should work for PPT");
    assert_eq!(doc.metadata.page_count, 1);
}

#[test]
fn extractor_ppt_page_text() {
    let mut slwt = Vec::new();
    slwt.extend_from_slice(&build_slide_persist_atom(1));
    slwt.extend_from_slice(&build_text_header_atom(0));
    slwt.extend_from_slice(&build_text_chars_atom("Slide One"));
    slwt.extend_from_slice(&build_slide_persist_atom(2));
    slwt.extend_from_slice(&build_text_header_atom(0));
    slwt.extend_from_slice(&build_text_chars_atom("Slide Two"));

    let data = build_ppt_cfb(&slwt, &[]);
    let config = udoc::Config::new().format(udoc::Format::Ppt);
    let mut ext = udoc::Extractor::from_bytes_with(&data, config).expect("extractor should open");

    assert_eq!(ext.page_count(), 2);
    assert_eq!(ext.format(), udoc::Format::Ppt);

    let text0 = ext.page_text(0).expect("page 0 text");
    assert!(text0.contains("Slide One"), "page 0 text: {text0}");

    let text1 = ext.page_text(1).expect("page 1 text");
    assert!(text1.contains("Slide Two"), "page 1 text: {text1}");
}

#[test]
fn extractor_ppt_into_document() {
    let mut slwt = Vec::new();
    slwt.extend_from_slice(&build_slide_persist_atom(1));
    slwt.extend_from_slice(&build_text_header_atom(0));
    slwt.extend_from_slice(&build_text_chars_atom("Document Model Test"));

    let data = build_ppt_cfb(&slwt, &[]);
    let config = udoc::Config::new().format(udoc::Format::Ppt);
    let ext = udoc::Extractor::from_bytes_with(&data, config).expect("extractor should open");

    let doc = ext.into_document().expect("into_document should succeed");
    assert_eq!(doc.metadata.page_count, 1);
    assert!(!doc.content.is_empty());
}

#[test]
fn extract_ppt_notes_in_document_model() {
    let mut slide_slwt = Vec::new();
    slide_slwt.extend_from_slice(&build_slide_persist_atom(1));
    slide_slwt.extend_from_slice(&build_text_header_atom(0));
    slide_slwt.extend_from_slice(&build_text_chars_atom("Main Content"));

    let mut notes_slwt = Vec::new();
    notes_slwt.extend_from_slice(&build_slide_persist_atom(100));
    notes_slwt.extend_from_slice(&build_text_header_atom(2));
    notes_slwt.extend_from_slice(&build_text_chars_atom("Speaker notes here"));

    let data = build_ppt_cfb(&slide_slwt, &notes_slwt);
    let config = udoc::Config::new()
        .format(udoc::Format::Ppt)
        .layers(udoc::LayerConfig::content_only());
    let doc = udoc::extract_bytes_with(&data, config).expect("extract should succeed");

    // Should have heading + section(notes).
    assert_eq!(doc.content.len(), 2);

    // Notes should be a Section block with role Notes.
    match &doc.content[1] {
        udoc::Block::Section { role, children, .. } => {
            assert_eq!(role.as_ref(), Some(&udoc::SectionRole::Notes));
            assert_eq!(children[0].text(), "Speaker notes here");
        }
        other => panic!("expected Section for notes, got: {:?}", other),
    }
}

#[test]
fn extract_ppt_no_presentation() {
    let mut slwt = Vec::new();
    slwt.extend_from_slice(&build_slide_persist_atom(1));
    slwt.extend_from_slice(&build_text_header_atom(0));
    slwt.extend_from_slice(&build_text_chars_atom("No presentation layer"));

    let data = build_ppt_cfb(&slwt, &[]);
    let config = udoc::Config::new()
        .format(udoc::Format::Ppt)
        .layers(udoc::LayerConfig::content_only());
    let doc = udoc::extract_bytes_with(&data, config).expect("extract should succeed");

    assert!(
        doc.presentation.is_none(),
        "content_only config should not have presentation layer"
    );
}

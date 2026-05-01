//! ODF integration tests for the udoc facade.
//!
//! Tests the end-to-end pipeline: ZIP bytes -> OdfDocument -> Document model.
//! Uses in-memory constructed ODF files via `build_stored_zip`.

use udoc_containers::test_util::build_stored_zip;

// ---------------------------------------------------------------------------
// Shared helpers for building minimal ODF ZIP files
// ---------------------------------------------------------------------------

fn make_odt_bytes(content_xml: &[u8]) -> Vec<u8> {
    build_stored_zip(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.text" as &[u8],
        ),
        ("content.xml", content_xml),
    ])
}

fn make_ods_bytes(content_xml: &[u8]) -> Vec<u8> {
    build_stored_zip(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.spreadsheet" as &[u8],
        ),
        ("content.xml", content_xml),
    ])
}

fn make_odp_bytes(content_xml: &[u8]) -> Vec<u8> {
    build_stored_zip(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.presentation" as &[u8],
        ),
        ("content.xml", content_xml),
    ])
}

// ---------------------------------------------------------------------------
// ODT: extract_bytes one-shot
// ---------------------------------------------------------------------------

#[test]
fn extract_bytes_odt_basic() {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:p>Hello from ODT</text:p>
      <text:p>Second paragraph.</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

    let data = make_odt_bytes(content_xml);
    let doc = udoc::extract_bytes(&data).expect("extract_bytes should succeed");

    assert!(!doc.content.is_empty(), "document should have content");
    assert_eq!(doc.metadata.page_count, 1);

    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        all_text.contains("Hello from ODT"),
        "should contain 'Hello from ODT', got: {all_text}"
    );
    assert!(
        all_text.contains("Second paragraph"),
        "should contain 'Second paragraph', got: {all_text}"
    );
}

#[test]
fn extract_bytes_odt_headings() {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:h text:outline-level="1">Chapter Title</text:h>
      <text:p>Body text here.</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

    let data = make_odt_bytes(content_xml);
    let config = udoc::Config::new()
        .format(udoc::Format::Odt)
        .layers(udoc::LayerConfig::content_only());
    let doc = udoc::extract_bytes_with(&data, config).expect("extract should succeed");

    assert_eq!(doc.content.len(), 2);
    assert!(
        matches!(&doc.content[0], udoc::Block::Heading { level: 1, .. }),
        "expected Heading level 1, got: {:?}",
        doc.content[0]
    );
    assert_eq!(doc.content[0].text(), "Chapter Title");
    assert!(matches!(&doc.content[1], udoc::Block::Paragraph { .. }));
    assert_eq!(doc.content[1].text(), "Body text here.");
}

// ---------------------------------------------------------------------------
// ODS: extract_bytes one-shot
// ---------------------------------------------------------------------------

#[test]
fn extract_bytes_ods_basic() {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:spreadsheet>
      <table:table table:name="Sheet1">
        <table:table-row>
          <table:table-cell office:value-type="string"><text:p>Name</text:p></table:table-cell>
          <table:table-cell office:value-type="string"><text:p>Value</text:p></table:table-cell>
        </table:table-row>
        <table:table-row>
          <table:table-cell office:value-type="string"><text:p>Widget</text:p></table:table-cell>
          <table:table-cell office:value-type="float" office:value="42"><text:p>42</text:p></table:table-cell>
        </table:table-row>
      </table:table>
    </office:spreadsheet>
  </office:body>
</office:document-content>"#;

    let data = make_ods_bytes(content_xml);
    let doc = udoc::extract_bytes(&data).expect("extract_bytes should succeed");

    assert!(!doc.content.is_empty(), "document should have content");
    assert_eq!(doc.metadata.page_count, 1);

    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        all_text.contains("Name"),
        "should contain 'Name', got: {all_text}"
    );
    assert!(
        all_text.contains("Widget"),
        "should contain 'Widget', got: {all_text}"
    );
}

#[test]
fn extract_bytes_ods_table_block() {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:spreadsheet>
      <table:table table:name="Data">
        <table:table-row>
          <table:table-cell office:value-type="string"><text:p>A1</text:p></table:table-cell>
          <table:table-cell office:value-type="string"><text:p>B1</text:p></table:table-cell>
        </table:table-row>
      </table:table>
    </office:spreadsheet>
  </office:body>
</office:document-content>"#;

    let data = make_ods_bytes(content_xml);
    let doc = udoc::extract_bytes(&data).expect("extract should succeed");

    let has_table = doc
        .content
        .iter()
        .any(|b| matches!(b, udoc::Block::Table { .. }));
    assert!(has_table, "ODS should produce a Table block");
}

// ---------------------------------------------------------------------------
// ODP: extract_bytes one-shot
// ---------------------------------------------------------------------------

#[test]
fn extract_bytes_odp_basic() {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
    xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:presentation>
      <draw:page draw:name="Slide1">
        <draw:frame presentation:class="title">
          <draw:text-box>
            <text:p>Presentation Title</text:p>
          </draw:text-box>
        </draw:frame>
        <draw:frame>
          <draw:text-box>
            <text:p>Slide body content</text:p>
          </draw:text-box>
        </draw:frame>
      </draw:page>
    </office:presentation>
  </office:body>
</office:document-content>"#;

    let data = make_odp_bytes(content_xml);
    let doc = udoc::extract_bytes(&data).expect("extract_bytes should succeed");

    assert!(!doc.content.is_empty(), "document should have content");
    assert_eq!(doc.metadata.page_count, 1);

    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        all_text.contains("Presentation Title"),
        "should contain 'Presentation Title', got: {all_text}"
    );
    assert!(
        all_text.contains("Slide body content"),
        "should contain 'Slide body content', got: {all_text}"
    );
}

#[test]
fn extract_bytes_odp_multi_slide() {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
    xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:presentation>
      <draw:page draw:name="Slide1">
        <draw:frame presentation:class="title">
          <draw:text-box><text:p>First Slide</text:p></draw:text-box>
        </draw:frame>
      </draw:page>
      <draw:page draw:name="Slide2">
        <draw:frame presentation:class="title">
          <draw:text-box><text:p>Second Slide</text:p></draw:text-box>
        </draw:frame>
      </draw:page>
    </office:presentation>
  </office:body>
</office:document-content>"#;

    let data = make_odp_bytes(content_xml);
    let config = udoc::Config::new().format(udoc::Format::Odp);
    let mut ext = udoc::Extractor::from_bytes_with(&data, config).expect("extractor should open");

    assert_eq!(ext.page_count(), 2);
    assert_eq!(ext.format(), udoc::Format::Odp);

    let text0 = ext.page_text(0).expect("page 0 text");
    assert!(text0.contains("First Slide"), "page 0 text: {text0}");

    let text1 = ext.page_text(1).expect("page 1 text");
    assert!(text1.contains("Second Slide"), "page 1 text: {text1}");
}

// ---------------------------------------------------------------------------
// Format detection from bytes
// ---------------------------------------------------------------------------

#[test]
fn detect_odt_from_bytes() {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text><text:p>test</text:p></office:text>
  </office:body>
</office:document-content>"#;

    let data = make_odt_bytes(content_xml);
    let ext = udoc::Extractor::from_bytes(&data).expect("from_bytes should succeed");
    assert_eq!(ext.format(), udoc::Format::Odt);
}

#[test]
fn detect_ods_from_bytes() {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:spreadsheet>
      <table:table table:name="Sheet1">
        <table:table-row>
          <table:table-cell office:value-type="string"><text:p>x</text:p></table:table-cell>
        </table:table-row>
      </table:table>
    </office:spreadsheet>
  </office:body>
</office:document-content>"#;

    let data = make_ods_bytes(content_xml);
    let ext = udoc::Extractor::from_bytes(&data).expect("from_bytes should succeed");
    assert_eq!(ext.format(), udoc::Format::Ods);
}

#[test]
fn detect_odp_from_bytes() {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:presentation>
      <draw:page draw:name="Slide1">
        <draw:frame><draw:text-box><text:p>x</text:p></draw:text-box></draw:frame>
      </draw:page>
    </office:presentation>
  </office:body>
</office:document-content>"#;

    let data = make_odp_bytes(content_xml);
    let ext = udoc::Extractor::from_bytes(&data).expect("from_bytes should succeed");
    assert_eq!(ext.format(), udoc::Format::Odp);
}

// ---------------------------------------------------------------------------
// Extractor streaming: into_document()
// ---------------------------------------------------------------------------

#[test]
fn extractor_odt_into_document() {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text><text:p>Document model test</text:p></office:text>
  </office:body>
</office:document-content>"#;

    let data = make_odt_bytes(content_xml);
    let config = udoc::Config::new().format(udoc::Format::Odt);
    let ext = udoc::Extractor::from_bytes_with(&data, config).expect("from_bytes should succeed");
    let doc = ext.into_document().expect("into_document should succeed");
    assert!(!doc.content.is_empty());
    assert_eq!(doc.metadata.page_count, 1);
}

#[test]
fn extractor_ods_into_document() {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:spreadsheet>
      <table:table table:name="Sheet1">
        <table:table-row>
          <table:table-cell office:value-type="string"><text:p>test</text:p></table:table-cell>
        </table:table-row>
      </table:table>
    </office:spreadsheet>
  </office:body>
</office:document-content>"#;

    let data = make_ods_bytes(content_xml);
    let config = udoc::Config::new().format(udoc::Format::Ods);
    let ext = udoc::Extractor::from_bytes_with(&data, config).expect("from_bytes should succeed");
    let doc = ext.into_document().expect("into_document should succeed");
    assert!(!doc.content.is_empty());
}

#[test]
fn extractor_odp_into_document() {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:presentation>
      <draw:page draw:name="Slide1">
        <draw:frame><draw:text-box><text:p>test</text:p></draw:text-box></draw:frame>
      </draw:page>
    </office:presentation>
  </office:body>
</office:document-content>"#;

    let data = make_odp_bytes(content_xml);
    let config = udoc::Config::new().format(udoc::Format::Odp);
    let ext = udoc::Extractor::from_bytes_with(&data, config).expect("from_bytes should succeed");
    let doc = ext.into_document().expect("into_document should succeed");
    assert!(!doc.content.is_empty());
    assert_eq!(doc.metadata.page_count, 1);
}

// ---------------------------------------------------------------------------
// content_only config: no presentation layer
// ---------------------------------------------------------------------------

#[test]
fn extract_odt_no_presentation() {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text><text:p>Test</text:p></office:text>
  </office:body>
</office:document-content>"#;

    let data = make_odt_bytes(content_xml);
    let config = udoc::Config::new()
        .format(udoc::Format::Odt)
        .layers(udoc::LayerConfig::content_only());
    let doc = udoc::extract_bytes_with(&data, config).expect("extract should succeed");

    assert!(
        doc.presentation.is_none(),
        "content_only config should strip the presentation layer"
    );
    assert!(
        !doc.content.is_empty(),
        "content should be present even with content_only config"
    );
}

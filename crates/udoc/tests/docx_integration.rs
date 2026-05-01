//! DOCX integration tests for the udoc facade.

use udoc_containers::test_util::build_stored_zip;

/// Build a minimal DOCX ZIP in memory for testing.
fn make_docx_bytes(document_xml: &[u8]) -> Vec<u8> {
    let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

    let package_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
        Target="word/document.xml"/>
</Relationships>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", package_rels),
        ("word/document.xml", document_xml),
    ])
}

// ---------------------------------------------------------------------------
// extract_bytes() one-shot
// ---------------------------------------------------------------------------

#[test]
fn extract_bytes_docx_basic() {
    let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Hello from DOCX</w:t></w:r></w:p>
    </w:body>
</w:document>"#;
    let data = make_docx_bytes(xml);
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
        all_text.contains("Hello from DOCX"),
        "should contain 'Hello from DOCX', got: {all_text}"
    );
}

#[test]
fn extract_bytes_docx_format_detection() {
    let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Format test</w:t></w:r></w:p>
    </w:body>
</w:document>"#;
    let data = make_docx_bytes(xml);

    // Format should be auto-detected as DOCX from ZIP magic + content types.
    let ext = udoc::Extractor::from_bytes(&data).expect("from_bytes should succeed");
    assert_eq!(ext.format(), udoc::Format::Docx);
}

// ---------------------------------------------------------------------------
// Extractor streaming
// ---------------------------------------------------------------------------

#[test]
fn extractor_docx_page_text() {
    let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>First paragraph</w:t></w:r></w:p>
        <w:p><w:r><w:t>Second paragraph</w:t></w:r></w:p>
    </w:body>
</w:document>"#;
    let data = make_docx_bytes(xml);

    let mut ext = udoc::Extractor::from_bytes(&data).expect("from_bytes should succeed");
    assert_eq!(ext.page_count(), 1);

    let text = ext.page_text(0).expect("page_text should succeed");
    assert!(text.contains("First paragraph"), "got: {text}");
    assert!(text.contains("Second paragraph"), "got: {text}");
}

#[test]
fn extractor_docx_into_document() {
    let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Document model test</w:t></w:r></w:p>
    </w:body>
</w:document>"#;
    let data = make_docx_bytes(xml);

    let ext = udoc::Extractor::from_bytes(&data).expect("from_bytes should succeed");
    let doc = ext.into_document().expect("into_document should succeed");
    assert!(!doc.content.is_empty());
}

// ---------------------------------------------------------------------------
// Bold/Italic/Font properties propagated
// ---------------------------------------------------------------------------

#[test]
fn extract_docx_bold_italic() {
    let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p>
            <w:r>
                <w:rPr><w:b/><w:i/></w:rPr>
                <w:t>Bold and Italic</w:t>
            </w:r>
            <w:r><w:t> normal</w:t></w:r>
        </w:p>
    </w:body>
</w:document>"#;
    let data = make_docx_bytes(xml);

    let doc = udoc::extract_bytes(&data).expect("extract should succeed");
    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all_text.contains("Bold and Italic"), "got: {all_text}");
    assert!(all_text.contains("normal"), "got: {all_text}");

    // Check that bold/italic are preserved in the Block/Inline tree.
    for block in &doc.content {
        if let udoc::Block::Paragraph { content, .. } = block {
            for inline in content {
                if let udoc::Inline::Text { text, style, .. } = inline {
                    if text.contains("Bold") {
                        assert!(style.bold, "expected bold on 'Bold and Italic'");
                        assert!(style.italic, "expected italic on 'Bold and Italic'");
                    }
                    if text.contains("normal") {
                        assert!(!style.bold, "expected not bold on 'normal'");
                        assert!(!style.italic, "expected not italic on 'normal'");
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

#[test]
fn extract_docx_with_tables() {
    let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Before table</w:t></w:r></w:p>
        <w:tbl>
            <w:tr>
                <w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc>
                <w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc>
            </w:tr>
            <w:tr>
                <w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc>
                <w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc>
            </w:tr>
        </w:tbl>
        <w:p><w:r><w:t>After table</w:t></w:r></w:p>
    </w:body>
</w:document>"#;
    let data = make_docx_bytes(xml);
    let doc = udoc::extract_bytes(&data).expect("extract should succeed");

    let has_table = doc
        .content
        .iter()
        .any(|b| matches!(b, udoc::Block::Table { .. }));
    assert!(has_table, "DOCX with tables should produce Table blocks");

    let table_text: String = doc
        .content
        .iter()
        .filter(|b| matches!(b, udoc::Block::Table { .. }))
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        table_text.contains("A1"),
        "table should contain 'A1', got: {table_text}"
    );
}

// ---------------------------------------------------------------------------
// Tracked changes: deletions filtered, insertions kept
// ---------------------------------------------------------------------------

#[test]
fn extract_docx_tracked_changes() {
    let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p>
            <w:r><w:t>Visible</w:t></w:r>
            <w:del><w:r><w:t>Deleted</w:t></w:r></w:del>
            <w:ins><w:r><w:t> Inserted</w:t></w:r></w:ins>
        </w:p>
    </w:body>
</w:document>"#;
    let data = make_docx_bytes(xml);
    let doc = udoc::extract_bytes(&data).expect("extract should succeed");

    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all_text.contains("Visible"), "got: {all_text}");
    assert!(!all_text.contains("Deleted"), "got: {all_text}");
    assert!(all_text.contains("Inserted"), "got: {all_text}");
}

// ---------------------------------------------------------------------------
// Hidden text (w:vanish) filtered from visible output
// ---------------------------------------------------------------------------

#[test]
fn extract_docx_hidden_text_filtered() {
    let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p>
            <w:r><w:t>Visible text</w:t></w:r>
            <w:r>
                <w:rPr><w:vanish/></w:rPr>
                <w:t>Hidden text</w:t>
            </w:r>
        </w:p>
    </w:body>
</w:document>"#;
    let data = make_docx_bytes(xml);
    let doc = udoc::extract_bytes(&data).expect("extract should succeed");

    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all_text.contains("Visible text"), "got: {all_text}");
    assert!(
        !all_text.contains("Hidden text"),
        "hidden text should be filtered, got: {all_text}"
    );
}

// ---------------------------------------------------------------------------
// No presentation layer for DOCX (flow format, no geometry)
// ---------------------------------------------------------------------------

#[test]
fn extract_docx_no_presentation() {
    let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Test</w:t></w:r></w:p>
    </w:body>
</w:document>"#;
    let data = make_docx_bytes(xml);
    let doc = udoc::extract_bytes(&data).expect("extract should succeed");
    assert!(
        doc.presentation.is_none(),
        "DOCX should not produce a presentation layer"
    );
}

// ---------------------------------------------------------------------------
// Heading detection from styles.xml
// ---------------------------------------------------------------------------

/// Build a DOCX with styles.xml for heading detection tests.
fn make_docx_with_styles(document_xml: &[u8], styles_xml: &[u8]) -> Vec<u8> {
    let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
    <Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
</Types>"#;

    let package_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
        Target="word/document.xml"/>
</Relationships>"#;

    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles"
        Target="styles.xml"/>
</Relationships>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", package_rels),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/document.xml", document_xml),
        ("word/styles.xml", styles_xml),
    ])
}

#[test]
fn extract_docx_heading_from_outline_level() {
    let styles_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="Heading1">
    <w:name w:val="heading 1"/>
    <w:pPr><w:outlineLvl w:val="0"/></w:pPr>
    <w:rPr><w:b/></w:rPr>
  </w:style>
  <w:style w:type="paragraph" w:styleId="Heading2">
    <w:name w:val="heading 2"/>
    <w:pPr><w:outlineLvl w:val="1"/></w:pPr>
  </w:style>
</w:styles>"#;

    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p>
            <w:pPr><w:pStyle w:val="Heading1"/></w:pPr>
            <w:r><w:t>Title</w:t></w:r>
        </w:p>
        <w:p><w:r><w:t>Body text</w:t></w:r></w:p>
        <w:p>
            <w:pPr><w:pStyle w:val="Heading2"/></w:pPr>
            <w:r><w:t>Section</w:t></w:r>
        </w:p>
        <w:p><w:r><w:t>More body text</w:t></w:r></w:p>
    </w:body>
</w:document>"#;

    let data = make_docx_with_styles(document_xml, styles_xml);
    let doc = udoc::extract_bytes(&data).expect("extract should succeed");

    // Should have 4 blocks: Heading1, Paragraph, Heading2, Paragraph.
    assert_eq!(
        doc.content.len(),
        4,
        "expected 4 blocks, got {:?}",
        doc.content
    );

    // First block: Heading level 1.
    assert!(
        matches!(&doc.content[0], udoc::Block::Heading { level: 1, .. }),
        "expected Heading level 1, got {:?}",
        doc.content[0]
    );
    assert_eq!(doc.content[0].text(), "Title");

    // Second block: body text paragraph.
    assert!(matches!(&doc.content[1], udoc::Block::Paragraph { .. }));
    assert_eq!(doc.content[1].text(), "Body text");

    // Third block: Heading level 2.
    assert!(
        matches!(&doc.content[2], udoc::Block::Heading { level: 2, .. }),
        "expected Heading level 2, got {:?}",
        doc.content[2]
    );
    assert_eq!(doc.content[2].text(), "Section");

    // Fourth block: body text paragraph.
    assert!(matches!(&doc.content[3], udoc::Block::Paragraph { .. }));

    // Heading1 style has w:b in rPr. The run has no direct bold, so it
    // should inherit bold=true from the style.
    if let udoc::Block::Heading { content, .. } = &doc.content[0] {
        if let udoc::Inline::Text { style, .. } = &content[0] {
            assert!(
                style.bold,
                "heading run should inherit bold from Heading1 style"
            );
        }
    }

    // Body text has no style bold, run has no direct bold -> not bold.
    if let udoc::Block::Paragraph { content, .. } = &doc.content[1] {
        if let udoc::Inline::Text { style, .. } = &content[0] {
            assert!(!style.bold, "body text should not be bold");
        }
    }
}

#[test]
fn extract_docx_interleaved_table_order() {
    let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Before table</w:t></w:r></w:p>
        <w:tbl>
            <w:tr>
                <w:tc><w:p><w:r><w:t>Cell</w:t></w:r></w:p></w:tc>
            </w:tr>
        </w:tbl>
        <w:p><w:r><w:t>After table</w:t></w:r></w:p>
    </w:body>
</w:document>"#;
    let data = make_docx_bytes(xml);
    let doc = udoc::extract_bytes(&data).expect("extract should succeed");

    // Should have 3 blocks in document order: Paragraph, Table, Paragraph.
    assert_eq!(doc.content.len(), 3);
    assert!(matches!(&doc.content[0], udoc::Block::Paragraph { .. }));
    assert_eq!(doc.content[0].text(), "Before table");
    assert!(matches!(&doc.content[1], udoc::Block::Table { .. }));
    assert!(matches!(&doc.content[2], udoc::Block::Paragraph { .. }));
    assert_eq!(doc.content[2].text(), "After table");
}

// ---------------------------------------------------------------------------
// List support (numbering.xml -> Block::List)
// ---------------------------------------------------------------------------

/// Build a DOCX with numbering.xml for list tests.
fn make_docx_with_numbering(document_xml: &[u8], numbering_xml: &[u8]) -> Vec<u8> {
    let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

    let package_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
        Target="word/document.xml"/>
</Relationships>"#;

    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering"
        Target="numbering.xml"/>
</Relationships>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", package_rels),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/document.xml", document_xml),
        ("word/numbering.xml", numbering_xml),
    ])
}

#[test]
fn extract_docx_bullet_list() {
    let numbering_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="bullet"/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Intro</w:t></w:r></w:p>
        <w:p>
            <w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr>
            <w:r><w:t>Item one</w:t></w:r>
        </w:p>
        <w:p>
            <w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr>
            <w:r><w:t>Item two</w:t></w:r>
        </w:p>
        <w:p><w:r><w:t>Outro</w:t></w:r></w:p>
    </w:body>
</w:document>"#;

    let data = make_docx_with_numbering(document_xml, numbering_xml);
    let doc = udoc::extract_bytes(&data).expect("extract should succeed");

    // Should have 3 blocks: Paragraph("Intro"), List (2 items), Paragraph("Outro").
    assert_eq!(
        doc.content.len(),
        3,
        "expected 3 blocks, got {:?}",
        doc.content
            .iter()
            .map(|b| format!("{:?}", std::mem::discriminant(b)))
            .collect::<Vec<_>>()
    );
    assert!(matches!(&doc.content[0], udoc::Block::Paragraph { .. }));
    assert_eq!(doc.content[0].text(), "Intro");

    // The list block.
    match &doc.content[1] {
        udoc::Block::List { items, kind, .. } => {
            assert_eq!(*kind, udoc::ListKind::Unordered);
            assert_eq!(items.len(), 2);
            let item0_text: String = items[0]
                .content
                .iter()
                .map(|b| b.text())
                .collect::<Vec<_>>()
                .join("\n");
            let item1_text: String = items[1]
                .content
                .iter()
                .map(|b| b.text())
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(item0_text, "Item one");
            assert_eq!(item1_text, "Item two");
        }
        other => panic!("expected List block, got {:?}", other),
    }

    assert!(matches!(&doc.content[2], udoc::Block::Paragraph { .. }));
    assert_eq!(doc.content[2].text(), "Outro");
}

#[test]
fn extract_docx_ordered_list() {
    let numbering_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="decimal"/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="2">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p>
            <w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr>
            <w:r><w:t>First</w:t></w:r>
        </w:p>
        <w:p>
            <w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr>
            <w:r><w:t>Second</w:t></w:r>
        </w:p>
        <w:p>
            <w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr>
            <w:r><w:t>Third</w:t></w:r>
        </w:p>
    </w:body>
</w:document>"#;

    let data = make_docx_with_numbering(document_xml, numbering_xml);
    let doc = udoc::extract_bytes(&data).expect("extract should succeed");

    // Should have 1 block: an ordered List with 3 items.
    assert_eq!(doc.content.len(), 1);
    match &doc.content[0] {
        udoc::Block::List {
            items, kind, start, ..
        } => {
            assert_eq!(*kind, udoc::ListKind::Ordered);
            assert_eq!(*start, 1);
            assert_eq!(items.len(), 3);
        }
        other => panic!("expected List block, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// Ancillary content: headers, footers, footnotes, endnotes
// ---------------------------------------------------------------------------

/// Build a DOCX with headers and footers.
fn make_docx_with_ancillary(
    document_xml: &[u8],
    header_xml: Option<&[u8]>,
    footer_xml: Option<&[u8]>,
    footnotes_xml: Option<&[u8]>,
) -> Vec<u8> {
    let mut content_types = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>"#,
    );
    if header_xml.is_some() {
        content_types.push_str(r#"
    <Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>"#);
    }
    if footer_xml.is_some() {
        content_types.push_str(r#"
    <Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>"#);
    }
    if footnotes_xml.is_some() {
        content_types.push_str(r#"
    <Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/>"#);
    }
    content_types.push_str("\n</Types>");

    let package_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
        Target="word/document.xml"/>
</Relationships>"#;

    let mut doc_rels = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    let mut rel_id = 1;
    if header_xml.is_some() {
        doc_rels.push_str(&format!(
            r#"
    <Relationship Id="rId{rel_id}"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header"
        Target="header1.xml"/>"#
        ));
        rel_id += 1;
    }
    if footer_xml.is_some() {
        doc_rels.push_str(&format!(
            r#"
    <Relationship Id="rId{rel_id}"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer"
        Target="footer1.xml"/>"#
        ));
        rel_id += 1;
    }
    if footnotes_xml.is_some() {
        doc_rels.push_str(&format!(
            r#"
    <Relationship Id="rId{rel_id}"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes"
        Target="footnotes.xml"/>"#
        ));
    }
    doc_rels.push_str("\n</Relationships>");

    let mut parts: Vec<(&str, &[u8])> = vec![
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", package_rels),
        ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
        ("word/document.xml", document_xml),
    ];
    if let Some(h) = header_xml {
        parts.push(("word/header1.xml", h));
    }
    if let Some(f) = footer_xml {
        parts.push(("word/footer1.xml", f));
    }
    if let Some(fn_xml) = footnotes_xml {
        parts.push(("word/footnotes.xml", fn_xml));
    }

    build_stored_zip(&parts)
}

#[test]
fn extract_docx_with_header_and_footer() {
    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Body content</w:t></w:r></w:p>
    </w:body>
</w:document>"#;

    let header_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:p><w:r><w:t>Header text</w:t></w:r></w:p>
</w:hdr>"#;

    let footer_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:p><w:r><w:t>Footer text</w:t></w:r></w:p>
</w:ftr>"#;

    let data = make_docx_with_ancillary(document_xml, Some(header_xml), Some(footer_xml), None);
    let doc = udoc::extract_bytes(&data).expect("extract should succeed");

    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        all_text.contains("Header text"),
        "header should be in output, got: {all_text}"
    );
    assert!(
        all_text.contains("Body content"),
        "body should be in output, got: {all_text}"
    );
    assert!(
        all_text.contains("Footer text"),
        "footer should be in output, got: {all_text}"
    );

    // Header should come before body, footer after body.
    // First block should be a Section with Header role.
    assert!(
        matches!(
            &doc.content[0],
            udoc::Block::Section {
                role: Some(udoc::SectionRole::Header),
                ..
            }
        ),
        "first block should be header section, got {:?}",
        doc.content[0]
    );
    // Last block should be a Section with Footer role.
    let last = doc.content.last().unwrap();
    assert!(
        matches!(
            last,
            udoc::Block::Section {
                role: Some(udoc::SectionRole::Footer),
                ..
            }
        ),
        "last block should be footer section, got {:?}",
        last
    );
}

#[test]
fn extract_docx_with_footnotes() {
    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Body with footnote ref</w:t></w:r></w:p>
    </w:body>
</w:document>"#;

    let footnotes_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:footnote w:id="0" w:type="separator">
        <w:p><w:r><w:t>---</w:t></w:r></w:p>
    </w:footnote>
    <w:footnote w:id="1" w:type="continuationSeparator">
        <w:p/>
    </w:footnote>
    <w:footnote w:id="2">
        <w:p><w:r><w:t>This is the footnote text.</w:t></w:r></w:p>
    </w:footnote>
</w:footnotes>"#;

    let data = make_docx_with_ancillary(document_xml, None, None, Some(footnotes_xml));
    let doc = udoc::extract_bytes(&data).expect("extract should succeed");

    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        all_text.contains("Body with footnote ref"),
        "body should be in output, got: {all_text}"
    );
    assert!(
        all_text.contains("This is the footnote text."),
        "footnote should be in output, got: {all_text}"
    );
    // Separator notes (id 0, 1) should be excluded.
    assert!(
        !all_text.contains("---"),
        "separator note should be excluded, got: {all_text}"
    );
}

// ---------------------------------------------------------------------------
// mc:AlternateContent in facade
// ---------------------------------------------------------------------------

#[test]
fn extract_docx_mc_alternate_content() {
    let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
    <w:body>
        <w:p><w:r><w:t>Before</w:t></w:r></w:p>
        <mc:AlternateContent>
            <mc:Choice>
                <w:p><w:r><w:t>Choice only</w:t></w:r></w:p>
            </mc:Choice>
            <mc:Fallback>
                <w:p><w:r><w:t>Fallback</w:t></w:r></w:p>
            </mc:Fallback>
        </mc:AlternateContent>
        <w:p><w:r><w:t>After</w:t></w:r></w:p>
    </w:body>
</w:document>"#;
    let data = make_docx_bytes(xml);
    let doc = udoc::extract_bytes(&data).expect("extract should succeed");

    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(all_text.contains("Before"), "got: {all_text}");
    assert!(
        all_text.contains("Fallback"),
        "fallback should be included, got: {all_text}"
    );
    assert!(
        !all_text.contains("Choice only"),
        "choice should be skipped, got: {all_text}"
    );
    assert!(all_text.contains("After"), "got: {all_text}");
}

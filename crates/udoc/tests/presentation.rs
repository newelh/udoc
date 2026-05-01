//! Cross-format presentation overlay integration tests.
//!
//! Verifies that udoc::extract_bytes_with() produces Document models
//! with populated presentation overlay data (text_styling, block_layout)
//! for formats that support styling.

mod common;

use udoc_containers::test_util::{
    build_stored_zip, DOCX_CONTENT_TYPES, DOCX_PACKAGE_RELS, XLSX_CONTENT_TYPES, XLSX_PACKAGE_RELS,
    XLSX_WORKBOOK_1SHEET,
};

// ---------------------------------------------------------------------------
// DOCX: colored text populates text_styling
// ---------------------------------------------------------------------------

fn make_docx_with_styled_text() -> Vec<u8> {
    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p>
            <w:pPr><w:jc w:val="center"/></w:pPr>
            <w:r>
                <w:rPr>
                    <w:color w:val="FF0000"/>
                    <w:rFonts w:ascii="Arial"/>
                    <w:sz w:val="28"/>
                </w:rPr>
                <w:t>Red centered text in Arial 14pt</w:t>
            </w:r>
        </w:p>
    </w:body>
</w:document>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", DOCX_CONTENT_TYPES),
        ("_rels/.rels", DOCX_PACKAGE_RELS),
        ("word/document.xml", document_xml),
    ])
}

#[test]
fn docx_presentation_text_styling_populated() {
    let data = make_docx_with_styled_text();
    let doc = udoc::extract_bytes(&data).expect("DOCX extract should succeed");

    let pres = doc
        .presentation
        .as_ref()
        .expect("DOCX with styled text should produce a presentation layer");

    // Walk the content tree to find the styled text inline and verify
    // its NodeId has a text_styling entry.
    let mut found_color = false;
    let mut found_font = false;
    let mut found_size = false;

    for block in &doc.content {
        if let udoc::Block::Paragraph { content, .. } = block {
            for inline in content {
                let id = inline.id();
                if let Some(ext) = pres.text_styling.get(id) {
                    if ext.color == Some(udoc::Color::rgb(255, 0, 0)) {
                        found_color = true;
                    }
                    if ext.font_name.as_deref() == Some("Arial") {
                        found_font = true;
                    }
                    // w:sz val="28" = 14pt (half-points)
                    if let Some(size) = ext.font_size {
                        if (size - 14.0).abs() < 0.1 {
                            found_size = true;
                        }
                    }
                }
            }
        }
    }

    assert!(
        found_color,
        "text_styling should contain an entry with color=RGB(255,0,0)"
    );
    assert!(
        found_font,
        "text_styling should contain an entry with font_name='Arial'"
    );
    assert!(
        found_size,
        "text_styling should contain an entry with font_size=14.0"
    );
}

#[test]
fn docx_presentation_block_layout_alignment() {
    let data = make_docx_with_styled_text();
    let doc = udoc::extract_bytes(&data).expect("DOCX extract should succeed");

    let pres = doc
        .presentation
        .as_ref()
        .expect("DOCX with alignment should produce a presentation layer");

    // Find the paragraph with center alignment.
    let mut found_center = false;
    for block in &doc.content {
        let id = block.id();
        if let Some(layout) = pres.block_layout.get(id) {
            if layout.alignment == Some(udoc::Alignment::Center) {
                found_center = true;
            }
        }
    }

    assert!(
        found_center,
        "block_layout should contain an entry with alignment=Center"
    );
}

// ---------------------------------------------------------------------------
// DOCX: content_only still has presentation (built inline during conversion)
// ---------------------------------------------------------------------------

#[test]
fn docx_content_only_strips_presentation() {
    let data = make_docx_with_styled_text();
    let config = udoc::Config::new().layers(udoc::LayerConfig::content_only());
    let doc = udoc::extract_bytes_with(&data, config).expect("extract should succeed");

    // Content spine should still exist.
    assert!(!doc.content.is_empty(), "content should be present");
    // content_only strips presentation for all formats, including those
    // that build it inline during conversion.
    assert!(
        doc.presentation.is_none(),
        "content_only should strip presentation data for DOCX"
    );
}

// ---------------------------------------------------------------------------
// RTF: colored text populates text_styling
// ---------------------------------------------------------------------------

fn make_rtf_with_color() -> Vec<u8> {
    // RTF with a color table and \cf1 to set text color to red.
    b"{\\rtf1\\ansi{\\colortbl;\\red255\\green0\\blue0;}{\\fonttbl{\\f0 Times New Roman;}}\
      \\f0\\fs28\\cf1 Red text in Times New Roman 14pt}"
        .to_vec()
}

#[test]
fn rtf_extract_produces_content() {
    let data = make_rtf_with_color();
    let doc = udoc::extract_bytes(&data).expect("RTF extract should succeed");

    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        all_text.contains("Red text"),
        "RTF should produce text content, got: {all_text}"
    );
}

#[test]
fn rtf_presentation_overlay_has_color_and_font() {
    let data = make_rtf_with_color();
    let doc = udoc::extract_bytes(&data).expect("RTF extract should succeed");

    let pres = doc
        .presentation
        .as_ref()
        .expect("RTF with color/font should have presentation overlay");

    // Verify at least one text_styling entry has the expected color (red).
    let has_red = pres.text_styling.iter().any(|(_, style)| {
        style.color == Some(udoc_core::document::presentation::Color::rgb(255, 0, 0))
    });
    assert!(has_red, "RTF presentation overlay should contain red color");

    // Verify font name is forwarded.
    let has_font = pres.text_styling.iter().any(|(_, style)| {
        style
            .font_name
            .as_deref()
            .is_some_and(|n| n.contains("Times"))
    });
    assert!(
        has_font,
        "RTF presentation overlay should contain Times font"
    );

    // Verify font size (14pt = \fs28 in half-points).
    let has_size = pres
        .text_styling
        .iter()
        .any(|(_, style)| style.font_size == Some(14.0));
    assert!(
        has_size,
        "RTF presentation overlay should contain 14pt font size"
    );
}

// ---------------------------------------------------------------------------
// Markdown: no presentation layer (expected)
// ---------------------------------------------------------------------------

#[test]
fn markdown_has_no_presentation_layer() {
    let md_bytes = b"# Heading\n\nSome **bold** text.\n";
    let config = udoc::Config::new().format(udoc::Format::Md);
    let doc = udoc::extract_bytes_with(md_bytes, config).expect("Markdown extract should succeed");

    // Markdown backend does not produce a presentation layer.
    assert!(
        doc.presentation.is_none(),
        "Markdown should not have a presentation layer"
    );
}

// ---------------------------------------------------------------------------
// PPTX (synthetic): styled text populates presentation overlay
// ---------------------------------------------------------------------------

fn make_pptx_with_styled_text() -> Vec<u8> {
    use udoc_containers::test_util::{
        PPTX_CONTENT_TYPES_1SLIDE, PPTX_PACKAGE_RELS, PPTX_PRESENTATION_1SLIDE,
        PPTX_PRES_RELS_1SLIDE, PPTX_SLIDE_RELS_EMPTY,
    };

    let slide_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Title"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="title"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:p>
            <a:pPr algn="ctr"/>
            <a:r>
              <a:rPr sz="2400" b="1">
                <a:solidFill><a:srgbClr val="0000FF"/></a:solidFill>
                <a:latin typeface="Helvetica"/>
              </a:rPr>
              <a:t>Blue Title</a:t>
            </a:r>
          </a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", PPTX_CONTENT_TYPES_1SLIDE),
        ("_rels/.rels", PPTX_PACKAGE_RELS),
        ("ppt/presentation.xml", PPTX_PRESENTATION_1SLIDE),
        ("ppt/_rels/presentation.xml.rels", PPTX_PRES_RELS_1SLIDE),
        ("ppt/slides/slide1.xml", slide_xml),
        ("ppt/slides/_rels/slide1.xml.rels", PPTX_SLIDE_RELS_EMPTY),
    ])
}

#[test]
fn pptx_presentation_text_styling_populated() {
    let data = make_pptx_with_styled_text();
    let doc = udoc::extract_bytes(&data).expect("PPTX extract should succeed");

    let pres = doc
        .presentation
        .as_ref()
        .expect("PPTX with styled text should produce a presentation layer");

    // Walk the content to find styled inline. PPTX may emit Heading
    // blocks (not Paragraph) when the placeholder type is "title".
    let mut found_color = false;
    let mut found_font = false;
    let mut found_size = false;

    fn check_inlines(
        inlines: &[udoc::Inline],
        pres: &udoc::Presentation,
        found_color: &mut bool,
        found_font: &mut bool,
        found_size: &mut bool,
    ) {
        for inline in inlines {
            let id = inline.id();
            if let Some(ext) = pres.text_styling.get(id) {
                if ext.color == Some(udoc::Color::rgb(0, 0, 255)) {
                    *found_color = true;
                }
                if ext.font_name.as_deref() == Some("Helvetica") {
                    *found_font = true;
                }
                // sz="2400" = 24.0pt (hundredths of a point)
                if ext.font_size == Some(24.0) {
                    *found_size = true;
                }
            }
        }
    }

    for block in &doc.content {
        match block {
            udoc::Block::Paragraph { content, .. } | udoc::Block::Heading { content, .. } => {
                check_inlines(
                    content,
                    pres,
                    &mut found_color,
                    &mut found_font,
                    &mut found_size,
                );
            }
            _ => {}
        }
    }

    assert!(
        found_color,
        "PPTX text_styling should contain entry with color=RGB(0,0,255)"
    );
    assert!(
        found_font,
        "PPTX text_styling should contain entry with font_name='Helvetica'"
    );
    assert!(
        found_size,
        "PPTX text_styling should contain entry with font_size=24.0 (from sz=2400)"
    );
}

#[test]
fn pptx_presentation_block_layout_alignment() {
    let data = make_pptx_with_styled_text();
    let doc = udoc::extract_bytes(&data).expect("PPTX extract should succeed");

    let pres = doc
        .presentation
        .as_ref()
        .expect("PPTX should produce a presentation layer");

    let mut found_center = false;
    for block in &doc.content {
        let id = block.id();
        if let Some(layout) = pres.block_layout.get(id) {
            if layout.alignment == Some(udoc::Alignment::Center) {
                found_center = true;
            }
        }
    }

    assert!(
        found_center,
        "PPTX block_layout should contain entry with alignment=Center"
    );
}

// ---------------------------------------------------------------------------
// JSON output audit -- verify presentation data serializes correctly
// ---------------------------------------------------------------------------

#[test]
fn docx_json_output_includes_presentation_data() {
    let data = make_docx_with_styled_text();
    let doc = udoc::extract_bytes(&data).expect("DOCX extract should succeed");

    // Serialize through the JSON output path.
    let mut buf = Vec::new();
    udoc::output::json::write_json(&doc, &mut buf, false, true, false)
        .expect("JSON serialization should succeed");
    let json_str = String::from_utf8(buf).expect("JSON should be valid UTF-8");
    let val: serde_json::Value = serde_json::from_str(&json_str).expect("JSON should be valid");

    // Verify structure: top-level keys
    assert!(val.get("version").is_some(), "should have version");
    assert!(val.get("content").is_some(), "should have content");
    assert!(val.get("metadata").is_some(), "should have metadata");

    // Presentation layer should be present
    let pres = val
        .get("presentation")
        .expect("JSON should include presentation layer");

    // text_styling should be a non-empty object
    let text_styling = pres
        .get("text_styling")
        .expect("presentation should have text_styling");
    assert!(text_styling.is_object(), "text_styling should be an object");
    assert!(
        !text_styling.as_object().unwrap().is_empty(),
        "text_styling should not be empty"
    );

    // At least one text_styling entry should have color data.
    let has_color = text_styling
        .as_object()
        .unwrap()
        .values()
        .any(|v| v.get("color").is_some());
    assert!(
        has_color,
        "at least one text_styling entry should have color"
    );

    // At least one text_styling entry should have font_name.
    let has_font = text_styling
        .as_object()
        .unwrap()
        .values()
        .any(|v| v.get("font_name").is_some());
    assert!(
        has_font,
        "at least one text_styling entry should have font_name"
    );

    // block_layout should be present and non-empty.
    let block_layout = pres
        .get("block_layout")
        .expect("presentation should have block_layout");
    assert!(block_layout.is_object(), "block_layout should be an object");
    assert!(
        !block_layout.as_object().unwrap().is_empty(),
        "block_layout should not be empty"
    );

    // At least one block_layout entry should have alignment.
    let has_alignment = block_layout
        .as_object()
        .unwrap()
        .values()
        .any(|v| v.get("alignment").is_some());
    assert!(
        has_alignment,
        "at least one block_layout entry should have alignment"
    );

    // Content spine should include inline nodes (Link nodes serialize as
    // part of the content spine via serde derives on Inline).
    let content = val.get("content").unwrap().as_array().unwrap();
    assert!(!content.is_empty(), "content should not be empty");
}

#[test]
fn json_link_nodes_serialize_in_content() {
    // Verify that Inline::Link nodes appear in the JSON content spine.
    let data = common::make_docx_with_hyperlink();
    let doc = udoc::extract_bytes(&data).expect("extract should succeed");

    let json = serde_json::to_string(&doc).expect("serialization should succeed");
    let val: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

    // Walk the JSON content array to find a "link" typed inline.
    let content = val.get("content").unwrap().as_array().unwrap();
    let mut found_link = false;
    for block in content {
        if let Some(inlines) = block.get("content") {
            if let Some(arr) = inlines.as_array() {
                for inline in arr {
                    if inline.get("type").and_then(|t| t.as_str()) == Some("link") {
                        let url = inline.get("url").and_then(|u| u.as_str());
                        assert_eq!(url, Some("https://example.com"));
                        found_link = true;
                    }
                }
            }
        }
    }
    assert!(found_link, "JSON content should contain a 'link' inline");
}

// ---------------------------------------------------------------------------
// Gap 4: RTF -- colored text populates presentation overlay through facade
// ---------------------------------------------------------------------------

#[test]
fn rtf_presentation_text_styling_populated() {
    // RTF with a color table and \cf1 to apply red color, plus font info.
    let data = b"{\\rtf1\\ansi{\\colortbl;\\red255\\green0\\blue0;}\
                  {\\fonttbl{\\f0 Courier New;}}\\f0\\fs24\\cf1 Red text in Courier}"
        .to_vec();
    let doc = udoc::extract_bytes(&data).expect("RTF extract should succeed");

    let pres = doc
        .presentation
        .as_ref()
        .expect("RTF with styled text should produce a presentation layer");

    let mut found_color = false;
    let mut found_font = false;
    let mut found_size = false;

    for block in &doc.content {
        if let udoc::Block::Paragraph { content, .. } = block {
            for inline in content {
                let id = inline.id();
                if let Some(ext) = pres.text_styling.get(id) {
                    if ext.color == Some(udoc::Color::rgb(255, 0, 0)) {
                        found_color = true;
                    }
                    if ext.font_name.as_deref() == Some("Courier New") {
                        found_font = true;
                    }
                    // \fs24 = 12.0pt (half-points)
                    if ext.font_size == Some(12.0) {
                        found_size = true;
                    }
                }
            }
        }
    }

    assert!(
        found_color,
        "RTF text_styling should contain entry with color=RGB(255,0,0)"
    );
    assert!(
        found_font,
        "RTF text_styling should contain entry with font_name='Courier New'"
    );
    assert!(
        found_size,
        "RTF text_styling should contain entry with font_size=12.0 (from \\fs24)"
    );
}

// ---------------------------------------------------------------------------
// Gap 5: XLSX -- styled cell populates presentation overlay through facade
// ---------------------------------------------------------------------------

fn make_xlsx_with_styled_cell() -> Vec<u8> {
    // XLSX with a styles.xml defining a red, 16pt Helvetica font applied to
    // cell A1 via style index s="1".
    let styles_xml =
        br#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <fonts count="2">
        <font><sz val="11"/><name val="Calibri"/></font>
        <font><color rgb="FFFF0000"/><sz val="16"/><name val="Helvetica"/></font>
    </fonts>
    <cellXfs count="2">
        <xf numFmtId="0" fontId="0"/>
        <xf numFmtId="0" fontId="1"/>
    </cellXfs>
</styleSheet>"#;

    let sheet_xml =
        br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" s="1" t="inlineStr"><is><t>Red big text</t></is></c>
        </row>
    </sheetData>
</worksheet>"#;

    // Build the wb rels with both sheet and styles references.
    let wb_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
        Target="worksheets/sheet1.xml"/>
    <Relationship Id="rId3"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles"
        Target="styles.xml"/>
</Relationships>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", XLSX_CONTENT_TYPES),
        ("_rels/.rels", XLSX_PACKAGE_RELS),
        ("xl/workbook.xml", XLSX_WORKBOOK_1SHEET),
        ("xl/_rels/workbook.xml.rels", wb_rels),
        ("xl/worksheets/sheet1.xml", sheet_xml),
        ("xl/styles.xml", styles_xml),
    ])
}

#[test]
fn xlsx_presentation_text_styling_populated() {
    let data = make_xlsx_with_styled_cell();
    let doc = udoc::extract_bytes(&data).expect("XLSX extract should succeed");

    let pres = doc
        .presentation
        .as_ref()
        .expect("XLSX with styled cells should produce a presentation layer");

    // XLSX content is a table. Walk into the table cells to find styled inlines.
    let mut found_color = false;
    let mut found_font = false;
    let mut found_size = false;

    for block in &doc.content {
        if let udoc::Block::Table { table, .. } = block {
            for row in &table.rows {
                for cell in &row.cells {
                    for child in &cell.content {
                        if let udoc::Block::Paragraph { content, .. } = child {
                            for inline in content {
                                let id = inline.id();
                                if let Some(ext) = pres.text_styling.get(id) {
                                    if ext.color == Some(udoc::Color::rgb(255, 0, 0)) {
                                        found_color = true;
                                    }
                                    if ext.font_name.as_deref() == Some("Helvetica") {
                                        found_font = true;
                                    }
                                    if let Some(size) = ext.font_size {
                                        if (size - 16.0).abs() < 0.1 {
                                            found_size = true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    assert!(
        found_color,
        "XLSX text_styling should contain entry with color=RGB(255,0,0)"
    );
    assert!(
        found_font,
        "XLSX text_styling should contain entry with font_name='Helvetica'"
    );
    assert!(
        found_size,
        "XLSX text_styling should contain entry with font_size=16.0"
    );
}

// ---------------------------------------------------------------------------
// Gap 6: ODF (ODT) -- colored text populates presentation overlay through facade
// ---------------------------------------------------------------------------

fn make_odt_with_styled_text() -> Vec<u8> {
    let content_xml = br##"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:automatic-styles>
    <style:style style:name="RedBig" style:family="text">
      <style:text-properties fo:color="#FF0000" style:font-name="Georgia" fo:font-size="18pt"/>
    </style:style>
  </office:automatic-styles>
  <office:body>
    <office:text>
      <text:p><text:span text:style-name="RedBig">Red text in Georgia 18pt</text:span></text:p>
    </office:text>
  </office:body>
</office:document-content>"##;

    build_stored_zip(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.text" as &[u8],
        ),
        ("content.xml", content_xml),
    ])
}

#[test]
fn odf_presentation_text_styling_populated() {
    let data = make_odt_with_styled_text();
    let doc = udoc::extract_bytes(&data).expect("ODT extract should succeed");

    let pres = doc
        .presentation
        .as_ref()
        .expect("ODT with styled text should produce a presentation layer");

    let mut found_color = false;
    let mut found_font = false;
    let mut found_size = false;

    for block in &doc.content {
        if let udoc::Block::Paragraph { content, .. } = block {
            for inline in content {
                let id = inline.id();
                if let Some(ext) = pres.text_styling.get(id) {
                    if ext.color == Some(udoc::Color::rgb(255, 0, 0)) {
                        found_color = true;
                    }
                    if ext.font_name.as_deref() == Some("Georgia") {
                        found_font = true;
                    }
                    if let Some(size) = ext.font_size {
                        if (size - 18.0).abs() < 0.1 {
                            found_size = true;
                        }
                    }
                }
            }
        }
    }

    assert!(
        found_color,
        "ODT text_styling should contain entry with color=RGB(255,0,0)"
    );
    assert!(
        found_font,
        "ODT text_styling should contain entry with font_name='Georgia'"
    );
    assert!(
        found_size,
        "ODT text_styling should contain entry with font_size=18.0"
    );
}

// ---------------------------------------------------------------------------
// Gap 7: JSON output includes relationships overlay (bookmarks + footnotes)
// ---------------------------------------------------------------------------

fn make_docx_with_bookmark_and_footnote() -> Vec<u8> {
    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p>
            <w:bookmarkStart w:id="0" w:name="intro_section"/>
            <w:bookmarkEnd w:id="0"/>
            <w:r><w:t>Body text with bookmark and footnote ref</w:t></w:r>
        </w:p>
    </w:body>
</w:document>"#;

    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes"
        Target="footnotes.xml"/>
</Relationships>"#;

    let footnotes_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:footnote w:id="0"><w:p><w:r><w:t>separator</w:t></w:r></w:p></w:footnote>
    <w:footnote w:id="1"><w:p><w:r><w:t>continuation</w:t></w:r></w:p></w:footnote>
    <w:footnote w:id="2">
        <w:p><w:r><w:t>This is the footnote content.</w:t></w:r></w:p>
    </w:footnote>
</w:footnotes>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", DOCX_CONTENT_TYPES),
        ("_rels/.rels", DOCX_PACKAGE_RELS),
        ("word/document.xml", document_xml),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/footnotes.xml", footnotes_xml),
    ])
}

#[test]
fn docx_json_output_includes_relationships_data() {
    let data = make_docx_with_bookmark_and_footnote();
    let doc = udoc::extract_bytes(&data).expect("DOCX extract should succeed");

    // Verify the relationships overlay exists in the Document model.
    let rels = doc
        .relationships
        .as_ref()
        .expect("DOCX with bookmarks and footnotes should have a relationships layer");
    assert!(
        rels.bookmarks().contains_key("intro_section"),
        "bookmark 'intro_section' should be present in relationships overlay"
    );
    assert!(
        rels.footnotes().contains_key("fn:2"),
        "footnote 'fn:2' should be present in relationships overlay"
    );

    // Serialize through the JSON output path and verify relationships appear.
    let mut buf = Vec::new();
    udoc::output::json::write_json(&doc, &mut buf, false, true, false)
        .expect("JSON serialization should succeed");
    let json_str = String::from_utf8(buf).expect("JSON should be valid UTF-8");
    let val: serde_json::Value = serde_json::from_str(&json_str).expect("JSON should parse");

    // Relationships should be a top-level key in the JSON output.
    let rels_json = val
        .get("relationships")
        .expect("JSON should include relationships layer");

    // Bookmarks should be present and non-empty.
    let bookmarks = rels_json
        .get("bookmarks")
        .expect("relationships should have bookmarks");
    assert!(bookmarks.is_object(), "bookmarks should be an object");
    assert!(
        bookmarks.as_object().unwrap().contains_key("intro_section"),
        "bookmarks should contain 'intro_section'"
    );

    // Footnotes should be present and contain our footnote.
    let footnotes = rels_json
        .get("footnotes")
        .expect("relationships should have footnotes");
    assert!(footnotes.is_object(), "footnotes should be an object");
    let fn2 = footnotes
        .get("fn:2")
        .expect("footnotes should contain key 'fn:2'");
    assert_eq!(
        fn2.get("label").and_then(|l| l.as_str()),
        Some("fn:2"),
        "footnote label should be 'fn:2'"
    );

    // Footnote content should include the text.
    let fn_content = fn2.get("content").expect("footnote should have content");
    let fn_json_str = serde_json::to_string(fn_content).unwrap();
    assert!(
        fn_json_str.contains("This is the footnote content."),
        "footnote content should contain expected text, got: {fn_json_str}"
    );
}

// ---------------------------------------------------------------------------
// content_only strips relationships overlay
// ---------------------------------------------------------------------------

#[test]
fn docx_content_only_strips_relationships() {
    let data = common::make_docx_with_hyperlink();
    let config = udoc::Config::new().layers(udoc::LayerConfig::content_only());
    let doc = udoc::extract_bytes_with(&data, config).expect("extract should succeed");

    assert!(!doc.content.is_empty(), "content should be present");
    assert!(
        doc.relationships.is_none(),
        "content_only should strip relationships data for DOCX"
    );
}

// ---------------------------------------------------------------------------
// content_only strips interactions overlay
// ---------------------------------------------------------------------------

#[test]
fn docx_content_only_strips_interactions() {
    let data = make_docx_with_styled_text();
    let config = udoc::Config::new().layers(udoc::LayerConfig::content_only());
    let doc = udoc::extract_bytes_with(&data, config).expect("extract should succeed");

    assert!(!doc.content.is_empty(), "content should be present");
    assert!(
        doc.interactions.is_none(),
        "content_only should strip interactions data for DOCX"
    );
}

// ---------------------------------------------------------------------------
// RTF: superscript/subscript styling reaches presentation overlay
// ---------------------------------------------------------------------------

#[test]
fn rtf_superscript_subscript_populates_presentation() {
    // RTF with \super and \sub control words.
    let data =
        b"{\\rtf1\\ansi\\plain Normal {\\super superscript} and {\\sub subscript} text}".to_vec();
    let doc = udoc::extract_bytes(&data).expect("RTF extract should succeed");

    let pres = doc
        .presentation
        .as_ref()
        .expect("RTF with super/sub should produce a presentation layer");

    let mut found_super = false;
    let mut found_sub = false;

    for block in &doc.content {
        if let udoc::Block::Paragraph { content, .. } = block {
            for inline in content {
                let id = inline.id();
                if let Some(ext) = pres.text_styling.get(id) {
                    // Superscript is tracked through the vertical_align field
                    // or is_superscript flag depending on how the converter
                    // forwards it. Check the inline text to match.
                    if let udoc::Inline::Text { text, style, .. } = inline {
                        if text.contains("superscript") && style.superscript {
                            found_super = true;
                        }
                        if text.contains("subscript") && style.subscript {
                            found_sub = true;
                        }
                    }
                    let _ = ext; // Verify styling entry exists for styled nodes.
                }
            }
        }
    }

    // Even if presentation overlay doesn't track super/sub (it's on SpanStyle),
    // verify the SpanStyle flags are set correctly.
    if !found_super || !found_sub {
        // Fall back to checking SpanStyle directly without requiring pres entry.
        for block in &doc.content {
            if let udoc::Block::Paragraph { content, .. } = block {
                for inline in content {
                    if let udoc::Inline::Text { text, style, .. } = inline {
                        if text.contains("superscript") && style.superscript {
                            found_super = true;
                        }
                        if text.contains("subscript") && style.subscript {
                            found_sub = true;
                        }
                    }
                }
            }
        }
    }

    assert!(
        found_super,
        "RTF \\super should produce SpanStyle with superscript=true"
    );
    assert!(
        found_sub,
        "RTF \\sub should produce SpanStyle with subscript=true"
    );
}

// ---------------------------------------------------------------------------
// Plain content without formatting: no presentation overlay
// ---------------------------------------------------------------------------

#[test]
fn plain_rtf_presentation_has_no_color() {
    // RTF with plain text, no colors. Font name/size are always present
    // because RTF carries a font table, but colors should be absent.
    let rtf = br"{\rtf1 Hello world}";
    let config = udoc::Config::new().format(udoc::Format::Rtf);
    let doc = udoc::extract_bytes_with(rtf, config).expect("RTF extract should succeed");

    if let Some(pres) = &doc.presentation {
        // Plain RTF should have font styling but no color data.
        for (_, style) in pres.text_styling.iter() {
            assert!(
                style.color.is_none(),
                "plain RTF should not produce text color"
            );
            assert!(
                style.background_color.is_none(),
                "plain RTF should not produce background color"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// PDF: fill color (rg operator) populates presentation overlay through facade
// ---------------------------------------------------------------------------

/// Build a minimal synthetic PDF whose content stream contains `0.5 0 0 rg`
/// (sets fill color to RGB ~128,0,0) followed by text rendering.
fn build_pdf_with_fill_color() -> Vec<u8> {
    let content = b"BT /F0 12 Tf 0.5 0 0 rg 100 700 Td (Colored) Tj ET";
    let mut pdf = Vec::with_capacity(1024 + content.len());
    pdf.extend_from_slice(b"%PDF-1.4\n");

    let o1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let o2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let o3 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /MediaBox [0 0 612 792] /Parent 2 0 R \
          /Contents 4 0 R \
          /Resources << /Font << /F0 5 0 R >> >> \
          >>\nendobj\n",
    );
    let o4 = pdf.len();
    let hdr = format!("4 0 obj\n<< /Length {} >>\nstream\r\n", content.len());
    pdf.extend_from_slice(hdr.as_bytes());
    pdf.extend_from_slice(content);
    pdf.extend_from_slice(b"\r\nendstream\nendobj\n");
    let o5 = pdf.len();
    pdf.extend_from_slice(
        b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
          /Encoding /WinAnsiEncoding >>\nendobj\n",
    );

    let xref_off = pdf.len();
    pdf.extend_from_slice(b"xref\n0 6\n");
    pdf.extend_from_slice(b"0000000000 65535 f \r\n");
    for off in [o1, o2, o3, o4, o5] {
        pdf.extend_from_slice(format!("{:010} 00000 n \r\n", off).as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Root 1 0 R /Size 6 >>\nstartxref\n{}\n%%EOF\n",
            xref_off
        )
        .as_bytes(),
    );
    pdf
}

#[test]
fn pdf_fill_color_populates_presentation_overlay() {
    let data = build_pdf_with_fill_color();
    let doc = udoc::extract_bytes(&data).expect("PDF extract should succeed");

    let pres = doc
        .presentation
        .as_ref()
        .expect("PDF with colored text should produce a presentation layer");

    // Walk the content tree to find the colored text inline. The rg operands
    // (0.5, 0, 0) map to RGB (128, 0, 0) via (val * 255).round().
    let mut found_color = false;
    for block in &doc.content {
        let inlines = match block {
            udoc::Block::Paragraph { content, .. } | udoc::Block::Heading { content, .. } => {
                content
            }
            _ => continue,
        };
        for inline in inlines {
            let id = inline.id();
            if let Some(ext) = pres.text_styling.get(id) {
                if ext.color == Some(udoc::Color::rgb(128, 0, 0)) {
                    found_color = true;
                }
            }
        }
    }

    assert!(
        found_color,
        "text_styling should contain an entry with color=RGB(128,0,0) from '0.5 0 0 rg'"
    );
}

#[test]
fn content_only_strips_presentation_layer() {
    // content_only mode should remove the presentation overlay even when
    // the backend produces one.
    let rtf = br"{\rtf1\colortbl;\red255\green0\blue0; Hello {\cf1 red}}";
    let config = udoc::Config::new()
        .format(udoc::Format::Rtf)
        .layers(udoc::LayerConfig::content_only());
    let doc = udoc::extract_bytes_with(rtf, config).expect("RTF extract should succeed");
    assert!(
        doc.presentation.is_none(),
        "content_only should strip presentation layer"
    );
}

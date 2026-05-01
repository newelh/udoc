//! Cross-format hyperlink integration tests.
//!
//! Verifies that udoc::extract_bytes_with() produces Document models
//! containing Inline::Link nodes for formats that support hyperlinks.
//! Each test builds a minimal synthetic file in memory and exercises
//! the full pipeline from bytes to Document model.

mod common;

use udoc_containers::test_util::{
    build_stored_zip, PPTX_CONTENT_TYPES_1SLIDE, PPTX_PACKAGE_RELS, PPTX_PRESENTATION_1SLIDE,
    PPTX_PRES_RELS_1SLIDE, XLSX_CONTENT_TYPES, XLSX_PACKAGE_RELS, XLSX_WB_RELS_1SHEET,
    XLSX_WORKBOOK_1SHEET,
};

const EXPECTED_URL: &str = "https://example.com";

/// Walk every inline slice reachable from a block tree, calling `on_inlines` for each.
fn walk_blocks<F>(block: &udoc::Block, on_inlines: &mut F)
where
    F: FnMut(&[udoc::Inline]),
{
    match block {
        udoc::Block::Paragraph { content, .. } | udoc::Block::Heading { content, .. } => {
            on_inlines(content);
        }
        udoc::Block::Table { table, .. } => {
            for row in &table.rows {
                for cell in &row.cells {
                    for child in &cell.content {
                        walk_blocks(child, on_inlines);
                    }
                }
            }
        }
        udoc::Block::List { items, .. } => {
            for item in items {
                for child in &item.content {
                    walk_blocks(child, on_inlines);
                }
            }
        }
        udoc::Block::Section { children, .. } | udoc::Block::Shape { children, .. } => {
            for child in children {
                walk_blocks(child, on_inlines);
            }
        }
        _ => {}
    }
}

/// Walk the Document tree and collect text content inside Inline::Link nodes.
fn collect_link_texts(doc: &udoc::Document) -> Vec<String> {
    let mut texts = Vec::new();
    for block in &doc.content {
        walk_blocks(block, &mut |inlines| {
            collect_link_texts_from_inlines(inlines, &mut texts);
        });
    }
    texts
}

fn collect_link_texts_from_inlines(inlines: &[udoc::Inline], texts: &mut Vec<String>) {
    for inline in inlines {
        if let udoc::Inline::Link { content, .. } = inline {
            let text: String = content
                .iter()
                .filter_map(|i| match i {
                    udoc::Inline::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            if !text.is_empty() {
                texts.push(text);
            }
            collect_link_texts_from_inlines(content, texts);
        }
    }
}

/// Walk the Document tree and collect all Inline::Link URLs found.
fn collect_link_urls(doc: &udoc::Document) -> Vec<String> {
    let mut urls = Vec::new();
    for block in &doc.content {
        walk_blocks(block, &mut |inlines| {
            collect_links_from_inlines(inlines, &mut urls);
        });
    }
    urls
}

fn collect_links_from_inlines(inlines: &[udoc::Inline], urls: &mut Vec<String>) {
    for inline in inlines {
        match inline {
            udoc::Inline::Link { url, content, .. } => {
                urls.push(url.clone());
                collect_links_from_inlines(content, urls);
            }
            // Leaf inlines with no children.
            udoc::Inline::Text { .. }
            | udoc::Inline::Code { .. }
            | udoc::Inline::FootnoteRef { .. }
            | udoc::Inline::InlineImage { .. }
            | udoc::Inline::SoftBreak { .. }
            | udoc::Inline::LineBreak { .. } => {}
            // Inline is #[non_exhaustive]; this wildcard is required by the
            // compiler but means new child-bearing variants will be silently
            // skipped. When adding new Inline variants with children, update
            // the match arms above.
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// DOCX: hyperlink via w:hyperlink + document.xml.rels
// ---------------------------------------------------------------------------

#[test]
fn docx_hyperlink_produces_inline_link() {
    let data = common::make_docx_with_hyperlink();
    let doc = udoc::extract_bytes(&data).expect("DOCX extract should succeed");

    let urls = collect_link_urls(&doc);
    assert!(
        urls.iter().any(|u| u == EXPECTED_URL),
        "DOCX should produce Inline::Link with URL '{}', found URLs: {:?}",
        EXPECTED_URL,
        urls
    );

    // Verify that link text content is preserved inside the Inline::Link.
    let link_texts = collect_link_texts(&doc);
    assert!(
        link_texts.iter().any(|t| t.contains("Click here")),
        "DOCX hyperlink should preserve link text 'Click here', found: {:?}",
        link_texts,
    );

    // Verify relationships overlay is populated.
    let rels = doc
        .relationships
        .as_ref()
        .expect("DOCX with hyperlinks should have relationships overlay");
    assert!(
        rels.hyperlinks().iter().any(|u| u == EXPECTED_URL),
        "DOCX relationships.hyperlinks should contain '{}', found: {:?}",
        EXPECTED_URL,
        rels.hyperlinks(),
    );
}

// ---------------------------------------------------------------------------
// XLSX: hyperlink via <hyperlinks> + sheet rels
// ---------------------------------------------------------------------------

fn make_xlsx_with_hyperlink() -> Vec<u8> {
    let sheet_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
           xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
    <sheetData>
        <row r="1"><c r="A1" t="inlineStr"><is><t>Click here</t></is></c></row>
    </sheetData>
    <hyperlinks>
        <hyperlink ref="A1" r:id="rId1"/>
    </hyperlinks>
</worksheet>"#;

    let sheet_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink"
        Target="https://example.com"
        TargetMode="External"/>
</Relationships>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", XLSX_CONTENT_TYPES),
        ("_rels/.rels", XLSX_PACKAGE_RELS),
        ("xl/workbook.xml", XLSX_WORKBOOK_1SHEET),
        ("xl/_rels/workbook.xml.rels", XLSX_WB_RELS_1SHEET),
        ("xl/worksheets/sheet1.xml", sheet_xml),
        ("xl/worksheets/_rels/sheet1.xml.rels", sheet_rels),
    ])
}

#[test]
fn xlsx_hyperlink_produces_inline_link() {
    let data = make_xlsx_with_hyperlink();
    let doc = udoc::extract_bytes(&data).expect("XLSX extract should succeed");

    let urls = collect_link_urls(&doc);
    assert!(
        urls.iter().any(|u| u == EXPECTED_URL),
        "XLSX should produce Inline::Link with URL '{}', found URLs: {:?}",
        EXPECTED_URL,
        urls
    );

    // Verify that link text content is preserved inside the Inline::Link.
    let link_texts = collect_link_texts(&doc);
    assert!(
        link_texts.iter().any(|t| t.contains("Click here")),
        "XLSX hyperlink should preserve cell text 'Click here', found: {:?}",
        link_texts,
    );

    // Verify relationships overlay is populated.
    let rels = doc
        .relationships
        .as_ref()
        .expect("XLSX with hyperlinks should have relationships overlay");
    assert!(
        rels.hyperlinks().iter().any(|u| u == EXPECTED_URL),
        "XLSX relationships.hyperlinks should contain '{}', found: {:?}",
        EXPECTED_URL,
        rels.hyperlinks(),
    );
}

// ---------------------------------------------------------------------------
// PPTX: hyperlink via a:hlinkClick + slide rels
// ---------------------------------------------------------------------------

fn make_pptx_with_hyperlink() -> Vec<u8> {
    let slide_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Body"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="body"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:p>
            <a:r>
              <a:rPr>
                <a:hlinkClick r:id="rId3"/>
              </a:rPr>
              <a:t>Click me</a:t>
            </a:r>
          </a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

    let slide_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId3"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink"
        Target="https://example.com" TargetMode="External"/>
</Relationships>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", PPTX_CONTENT_TYPES_1SLIDE),
        ("_rels/.rels", PPTX_PACKAGE_RELS),
        ("ppt/presentation.xml", PPTX_PRESENTATION_1SLIDE),
        ("ppt/_rels/presentation.xml.rels", PPTX_PRES_RELS_1SLIDE),
        ("ppt/slides/slide1.xml", slide_xml),
        ("ppt/slides/_rels/slide1.xml.rels", slide_rels),
    ])
}

#[test]
fn pptx_hyperlink_produces_inline_link() {
    let data = make_pptx_with_hyperlink();
    let doc = udoc::extract_bytes(&data).expect("PPTX extract should succeed");

    let urls = collect_link_urls(&doc);
    assert!(
        urls.iter().any(|u| u == EXPECTED_URL),
        "PPTX should produce Inline::Link with URL '{}', found URLs: {:?}",
        EXPECTED_URL,
        urls
    );

    // Verify that link text content is preserved inside the Inline::Link.
    let link_texts = collect_link_texts(&doc);
    assert!(
        link_texts.iter().any(|t| t.contains("Click me")),
        "PPTX hyperlink should preserve link text 'Click me', found: {:?}",
        link_texts,
    );

    // Verify relationships overlay is populated.
    let rels = doc
        .relationships
        .as_ref()
        .expect("PPTX with hyperlinks should have relationships overlay");
    assert!(
        rels.hyperlinks().iter().any(|u| u == EXPECTED_URL),
        "PPTX relationships.hyperlinks should contain '{}', found: {:?}",
        EXPECTED_URL,
        rels.hyperlinks(),
    );
}

// ---------------------------------------------------------------------------
// Markdown: inline link [text](url)
// ---------------------------------------------------------------------------

#[test]
fn markdown_hyperlink_produces_inline_link() {
    let md_bytes = b"Here is a [link](https://example.com) in markdown.\n";
    let config = udoc::Config::new().format(udoc::Format::Md);
    let doc = udoc::extract_bytes_with(md_bytes, config).expect("Markdown extract should succeed");

    let urls = collect_link_urls(&doc);
    assert!(
        urls.iter().any(|u| u == EXPECTED_URL),
        "Markdown should produce Inline::Link with URL '{}', found URLs: {:?}",
        EXPECTED_URL,
        urls
    );

    let link_texts = collect_link_texts(&doc);
    assert!(
        link_texts.iter().any(|t| t.contains("link")),
        "Markdown hyperlink should preserve link text 'link', found: {:?}",
        link_texts,
    );

    // Verify relationships overlay is populated.
    let rels = doc
        .relationships
        .as_ref()
        .expect("Markdown with hyperlinks should have relationships overlay");
    assert!(
        rels.hyperlinks().iter().any(|u| u == EXPECTED_URL),
        "Markdown relationships.hyperlinks should contain '{}', found: {:?}",
        EXPECTED_URL,
        rels.hyperlinks(),
    );
}

// ---------------------------------------------------------------------------
// ODF: hyperlink via text:a xlink:href
// ---------------------------------------------------------------------------

fn make_odf_with_hyperlink() -> Vec<u8> {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:xlink="http://www.w3.org/1999/xlink">
  <office:body>
    <office:text>
      <text:p>Visit <text:a xlink:href="https://example.com">our site</text:a> today</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

    build_stored_zip(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.text" as &[u8],
        ),
        ("content.xml", content_xml),
    ])
}

#[test]
fn odf_hyperlink_produces_inline_link() {
    let data = make_odf_with_hyperlink();
    let doc = udoc::extract_bytes(&data).expect("ODF extract should succeed");

    let urls = collect_link_urls(&doc);
    assert!(
        urls.iter().any(|u| u == EXPECTED_URL),
        "ODF should produce Inline::Link with URL '{}', found URLs: {:?}",
        EXPECTED_URL,
        urls
    );

    let link_texts = collect_link_texts(&doc);
    assert!(
        link_texts.iter().any(|t| t.contains("our site")),
        "ODF hyperlink should preserve link text 'our site', found: {:?}",
        link_texts,
    );

    // Verify relationships overlay is populated.
    let rels = doc
        .relationships
        .as_ref()
        .expect("ODF with hyperlinks should have relationships overlay");
    assert!(
        rels.hyperlinks().iter().any(|u| u == EXPECTED_URL),
        "ODF relationships.hyperlinks should contain '{}', found: {:?}",
        EXPECTED_URL,
        rels.hyperlinks(),
    );
}

// ---------------------------------------------------------------------------
// PDF: hyperlinks via /Annots are tested at the backend level
// (udoc-pdf) but not through the facade. Synthetic PDF construction
// requires valid cross-reference tables and stream objects, which is
// impractical to build in-memory without a PDF writer. This test is
// marked #[ignore] to document the gap.
// ---------------------------------------------------------------------------

// Superseded by pdf_hyperlink_extracted_via_facade below which uses a real
// synthetic PDF builder.

// ---------------------------------------------------------------------------
// RTF: hyperlinks via \field{\*\fldinst HYPERLINK "url"}{\fldrslt text}
// ---------------------------------------------------------------------------

#[test]
fn rtf_hyperlink_produces_inline_link() {
    let rtf_bytes = br#"{\rtf1 Visit {\field{\*\fldinst HYPERLINK "https://example.com"}{\fldrslt Click here}} today.}"#;
    let config = udoc::Config::new().format(udoc::Format::Rtf);
    let doc = udoc::extract_bytes_with(rtf_bytes, config).expect("RTF extract should succeed");

    let urls = collect_link_urls(&doc);
    assert!(
        urls.iter().any(|u| u == EXPECTED_URL),
        "RTF should produce Inline::Link with URL '{}', found URLs: {:?}",
        EXPECTED_URL,
        urls
    );

    // Verify that link text content is preserved inside the Inline::Link.
    let link_texts = collect_link_texts(&doc);
    assert!(
        link_texts.iter().any(|t| t.contains("Click here")),
        "RTF hyperlink should preserve link text 'Click here', found: {:?}",
        link_texts,
    );

    // Verify relationships overlay is populated.
    let rels = doc
        .relationships
        .as_ref()
        .expect("RTF with hyperlinks should have relationships overlay");
    assert!(
        rels.hyperlinks().iter().any(|u| u == EXPECTED_URL),
        "RTF relationships.hyperlinks should contain '{}', found: {:?}",
        EXPECTED_URL,
        rels.hyperlinks(),
    );
}

// ---------------------------------------------------------------------------
// DOCX: hyperlinks inside table cells are extracted
// ---------------------------------------------------------------------------

fn make_docx_with_table_hyperlink() -> Vec<u8> {
    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
    <w:body>
        <w:tbl>
            <w:tr>
                <w:tc>
                    <w:p>
                        <w:hyperlink r:id="rId1">
                            <w:r><w:t>Cell link</w:t></w:r>
                        </w:hyperlink>
                    </w:p>
                </w:tc>
            </w:tr>
        </w:tbl>
    </w:body>
</w:document>"#;

    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink"
        Target="https://example.com"
        TargetMode="External"/>
</Relationships>"#;

    build_stored_zip(&[
        (
            "[Content_Types].xml",
            udoc_containers::test_util::DOCX_CONTENT_TYPES,
        ),
        ("_rels/.rels", udoc_containers::test_util::DOCX_PACKAGE_RELS),
        ("word/document.xml", document_xml),
        ("word/_rels/document.xml.rels", doc_rels),
    ])
}

#[test]
fn docx_hyperlink_in_table_cell_text_preserved() {
    let data = make_docx_with_table_hyperlink();
    let doc = udoc::extract_bytes(&data).expect("DOCX extract should succeed");

    // Verify that table cell hyperlinks produce Inline::Link nodes.
    let urls = collect_link_urls(&doc);
    assert!(
        urls.iter().any(|u| u == "https://example.com"),
        "DOCX table cell should produce Inline::Link with URL 'https://example.com', found URLs: {:?}",
        urls
    );

    // Verify that link display text is preserved inside the Inline::Link.
    let link_texts = collect_link_texts(&doc);
    assert!(
        link_texts.iter().any(|t| t.contains("Cell link")),
        "DOCX table cell link text should contain 'Cell link', found: {:?}",
        link_texts
    );

    // Verify that the URL is also in the relationships overlay.
    let rels = doc
        .relationships
        .as_ref()
        .expect("relationships overlay should be present");
    assert!(
        rels.hyperlinks().iter().any(|u| u == "https://example.com"),
        "relationships overlay should contain 'https://example.com', found: {:?}",
        rels.hyperlinks()
    );
}

// ---------------------------------------------------------------------------
// PDF: hyperlinks via /Annots extracted through facade
// ---------------------------------------------------------------------------

/// Build a minimal valid PDF with a /Link annotation.
fn build_pdf_with_link() -> Vec<u8> {
    let content = b"BT /F0 12 Tf 100 700 Td (Hello) Tj ET";
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
          /Annots [<< /Type /Annot /Subtype /Link /Rect [0 0 200 20] \
          /A << /S /URI /URI (https://example.com) >> >>] \
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
fn pdf_hyperlink_extracted_via_facade() {
    let data = build_pdf_with_link();
    let doc = udoc::extract_bytes(&data).expect("PDF extract should succeed");

    // PDF links are stored in the relationships overlay (not as Inline::Link).
    let rels = doc
        .relationships
        .as_ref()
        .expect("PDF with /Link annotations should have relationships overlay");
    assert!(
        rels.hyperlinks().iter().any(|u| u == EXPECTED_URL),
        "PDF relationships.hyperlinks should contain '{}', found: {:?}",
        EXPECTED_URL,
        rels.hyperlinks(),
    );
}

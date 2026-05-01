//! Golden file tests for DOCX text extraction.
//!
//! Run with `BLESS=1 cargo test -p udoc-docx --test golden_files` to update expected files.

use std::path::PathBuf;
use udoc_containers::test_util::{
    build_stored_zip, DOCX_CONTENT_TYPES, DOCX_PACKAGE_RELS, DOCX_PACKAGE_RELS_WITH_CORE,
};
use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_core::test_harness::assert_golden;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

// ---------------------------------------------------------------------------
// basic.docx -- 3 paragraphs of plain text
// ---------------------------------------------------------------------------

fn build_basic_docx() -> Vec<u8> {
    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>First paragraph of plain text.</w:t></w:r></w:p>
        <w:p><w:r><w:t>Second paragraph with more content.</w:t></w:r></w:p>
        <w:p><w:r><w:t>Third and final paragraph.</w:t></w:r></w:p>
    </w:body>
</w:document>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", DOCX_CONTENT_TYPES),
        ("_rels/.rels", DOCX_PACKAGE_RELS),
        ("word/document.xml", document_xml),
    ])
}

#[test]
fn golden_basic_text() {
    let data = build_basic_docx();
    let mut doc = udoc_docx::DocxDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("basic_text", &text, &golden_dir());
}

// ---------------------------------------------------------------------------
// headings.docx -- heading 1, heading 2, body paragraph
// ---------------------------------------------------------------------------

fn build_headings_docx() -> Vec<u8> {
    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p>
            <w:pPr>
                <w:pStyle w:val="Heading1"/>
                <w:outlineLvl w:val="0"/>
            </w:pPr>
            <w:r><w:t>Chapter One</w:t></w:r>
        </w:p>
        <w:p>
            <w:pPr>
                <w:pStyle w:val="Heading2"/>
                <w:outlineLvl w:val="1"/>
            </w:pPr>
            <w:r><w:t>Section 1.1</w:t></w:r>
        </w:p>
        <w:p><w:r><w:t>This is body text under the heading.</w:t></w:r></w:p>
    </w:body>
</w:document>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", DOCX_CONTENT_TYPES),
        ("_rels/.rels", DOCX_PACKAGE_RELS),
        ("word/document.xml", document_xml),
    ])
}

#[test]
fn golden_headings_text() {
    let data = build_headings_docx();
    let mut doc = udoc_docx::DocxDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("headings_text", &text, &golden_dir());
}

#[test]
fn headings_style_detection() {
    let data = build_headings_docx();
    let doc = udoc_docx::DocxDocument::from_bytes(&data).expect("from_bytes");
    let body = doc.body();

    // First element should be a paragraph with Heading1 style.
    match &body[0] {
        udoc_docx::BodyElement::Paragraph(para) => {
            assert_eq!(para.style_id.as_deref(), Some("Heading1"));
            assert_eq!(para.outline_level, Some(0));
        }
        other => panic!("expected Paragraph, got: {other:?}"),
    }

    // Second element should be Heading2.
    match &body[1] {
        udoc_docx::BodyElement::Paragraph(para) => {
            assert_eq!(para.style_id.as_deref(), Some("Heading2"));
            assert_eq!(para.outline_level, Some(1));
        }
        other => panic!("expected Paragraph, got: {other:?}"),
    }

    // Third should be plain body text (no style, no outline level).
    match &body[2] {
        udoc_docx::BodyElement::Paragraph(para) => {
            assert_eq!(para.style_id, None);
            assert_eq!(para.outline_level, None);
        }
        other => panic!("expected Paragraph, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// table.docx -- simple 3x2 table with text cells
// ---------------------------------------------------------------------------

fn build_table_docx() -> Vec<u8> {
    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Before the table.</w:t></w:r></w:p>
        <w:tbl>
            <w:tr>
                <w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc>
                <w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc>
                <w:tc><w:p><w:r><w:t>C1</w:t></w:r></w:p></w:tc>
            </w:tr>
            <w:tr>
                <w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc>
                <w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc>
                <w:tc><w:p><w:r><w:t>C2</w:t></w:r></w:p></w:tc>
            </w:tr>
        </w:tbl>
        <w:p><w:r><w:t>After the table.</w:t></w:r></w:p>
    </w:body>
</w:document>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", DOCX_CONTENT_TYPES),
        ("_rels/.rels", DOCX_PACKAGE_RELS),
        ("word/document.xml", document_xml),
    ])
}

#[test]
fn golden_table_text() {
    let data = build_table_docx();
    let mut doc = udoc_docx::DocxDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("table_text", &text, &golden_dir());
}

#[test]
fn table_extraction() {
    let data = build_table_docx();
    let mut doc = udoc_docx::DocxDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let tables = page.tables().expect("tables()");
    assert_eq!(tables.len(), 1, "expected 1 table");
    assert_eq!(tables[0].rows.len(), 2, "expected 2 rows");
    assert_eq!(tables[0].rows[0].cells.len(), 3, "expected 3 cells per row");
    assert_eq!(tables[0].rows[0].cells[0].text, "A1");
    assert_eq!(tables[0].rows[1].cells[2].text, "C2");
}

// ---------------------------------------------------------------------------
// metadata.docx -- document with title, author, subject in docProps/core.xml
// ---------------------------------------------------------------------------

fn build_metadata_docx() -> Vec<u8> {
    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Document with metadata.</w:t></w:r></w:p>
    </w:body>
</w:document>"#;

    let core_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties
    xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:dcterms="http://purl.org/dc/terms/">
    <dc:title>Test Document Title</dc:title>
    <dc:creator>Test Author Name</dc:creator>
    <dc:subject>Test Subject Line</dc:subject>
</cp:coreProperties>"#;

    // Content types that also include the core properties part.
    let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
    <Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>
</Types>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", DOCX_PACKAGE_RELS_WITH_CORE),
        ("word/document.xml", document_xml),
        ("docProps/core.xml", core_xml),
    ])
}

#[test]
fn golden_metadata_text() {
    let data = build_metadata_docx();
    let mut doc = udoc_docx::DocxDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("metadata_text", &text, &golden_dir());
}

#[test]
fn golden_metadata_fields() {
    let data = build_metadata_docx();
    let doc = udoc_docx::DocxDocument::from_bytes(&data).expect("from_bytes");
    let meta = doc.metadata();

    let output = format!(
        "title: {}\nauthor: {}\nsubject: {}\npage_count: {}",
        meta.title.as_deref().unwrap_or("(none)"),
        meta.author.as_deref().unwrap_or("(none)"),
        meta.subject.as_deref().unwrap_or("(none)"),
        meta.page_count,
    );
    assert_golden("metadata", &output, &golden_dir());
}

// ---------------------------------------------------------------------------
// style_inherited_bold.docx -- paragraph style with basedOn chain providing
// bold/italic, run itself has no rPr
// ---------------------------------------------------------------------------

fn build_style_inherited_bold_docx() -> Vec<u8> {
    let styles_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="BoldBase">
    <w:name w:val="Bold Base"/>
    <w:rPr><w:b/></w:rPr>
  </w:style>
  <w:style w:type="paragraph" w:styleId="BoldItalicChild">
    <w:name w:val="Bold Italic Child"/>
    <w:basedOn w:val="BoldBase"/>
    <w:rPr><w:i/></w:rPr>
  </w:style>
</w:styles>"#;

    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p>
            <w:pPr><w:pStyle w:val="BoldBase"/></w:pPr>
            <w:r><w:t>Inherited bold text.</w:t></w:r>
        </w:p>
        <w:p>
            <w:pPr><w:pStyle w:val="BoldItalicChild"/></w:pPr>
            <w:r><w:t>Inherited bold and italic text.</w:t></w:r>
        </w:p>
        <w:p>
            <w:r><w:t>Plain text with no style.</w:t></w:r>
        </w:p>
    </w:body>
</w:document>"#;

    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles"
        Target="styles.xml"/>
</Relationships>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", DOCX_CONTENT_TYPES),
        ("_rels/.rels", DOCX_PACKAGE_RELS),
        ("word/document.xml", document_xml),
        ("word/styles.xml", styles_xml),
        ("word/_rels/document.xml.rels", doc_rels),
    ])
}

#[test]
fn golden_style_inherited_bold_text() {
    let data = build_style_inherited_bold_docx();
    let mut doc = udoc_docx::DocxDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("style_inherited_bold_text", &text, &golden_dir());
}

#[test]
fn style_inherited_bold_conversion() {
    use udoc_core::diagnostics::NullDiagnostics;
    use udoc_core::document::{Block, Inline};

    let data = build_style_inherited_bold_docx();
    let mut doc = udoc_docx::DocxDocument::from_bytes(&data).expect("from_bytes");
    let diag = NullDiagnostics;
    let result = udoc_docx::docx_to_document(&mut doc, &diag, usize::MAX).expect("convert");

    assert_eq!(result.content.len(), 3, "expected 3 paragraphs");

    // First paragraph: style BoldBase provides bold. Run has no rPr,
    // so bold should come from style inheritance via resolve_bold.
    match &result.content[0] {
        Block::Paragraph { content, .. } => {
            assert_eq!(content.len(), 1);
            match &content[0] {
                Inline::Text { text, style, .. } => {
                    assert_eq!(text, "Inherited bold text.");
                    assert!(style.bold, "run under BoldBase should inherit bold");
                    assert!(!style.italic, "run under BoldBase should not be italic");
                }
                other => panic!("expected Text, got: {other:?}"),
            }
        }
        other => panic!("expected Paragraph, got: {other:?}"),
    }

    // Second paragraph: BoldItalicChild basedOn BoldBase.
    // Italic defined directly on child style, bold inherited from BoldBase.
    match &result.content[1] {
        Block::Paragraph { content, .. } => {
            assert_eq!(content.len(), 1);
            match &content[0] {
                Inline::Text { text, style, .. } => {
                    assert_eq!(text, "Inherited bold and italic text.");
                    assert!(
                        style.bold,
                        "run under BoldItalicChild should inherit bold from BoldBase"
                    );
                    assert!(style.italic, "run under BoldItalicChild should have italic");
                }
                other => panic!("expected Text, got: {other:?}"),
            }
        }
        other => panic!("expected Paragraph, got: {other:?}"),
    }

    // Third paragraph: no style, should be neither bold nor italic.
    match &result.content[2] {
        Block::Paragraph { content, .. } => {
            assert_eq!(content.len(), 1);
            match &content[0] {
                Inline::Text { text, style, .. } => {
                    assert_eq!(text, "Plain text with no style.");
                    assert!(!style.bold, "run with no style should not be bold");
                    assert!(!style.italic, "run with no style should not be italic");
                }
                other => panic!("expected Text, got: {other:?}"),
            }
        }
        other => panic!("expected Paragraph, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// headers_footers.docx -- document with header and footer parts
// ---------------------------------------------------------------------------

fn build_headers_footers_docx() -> Vec<u8> {
    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Body paragraph one.</w:t></w:r></w:p>
        <w:p><w:r><w:t>Body paragraph two.</w:t></w:r></w:p>
    </w:body>
</w:document>"#;

    let header_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:p><w:r><w:t>Company Header Text</w:t></w:r></w:p>
</w:hdr>"#;

    let footer_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:p><w:r><w:t>Page 1 of 1</w:t></w:r></w:p>
</w:ftr>"#;

    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header"
        Target="header1.xml"/>
    <Relationship Id="rId2"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer"
        Target="footer1.xml"/>
</Relationships>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", DOCX_CONTENT_TYPES),
        ("_rels/.rels", DOCX_PACKAGE_RELS),
        ("word/document.xml", document_xml),
        ("word/header1.xml", header_xml),
        ("word/footer1.xml", footer_xml),
        ("word/_rels/document.xml.rels", doc_rels),
    ])
}

#[test]
fn golden_headers_footers_text() {
    let data = build_headers_footers_docx();
    let mut doc = udoc_docx::DocxDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("headers_footers_text", &text, &golden_dir());
}

#[test]
fn headers_footers_conversion() {
    use udoc_core::diagnostics::NullDiagnostics;
    use udoc_core::document::{Block, SectionRole};

    let data = build_headers_footers_docx();
    let mut doc = udoc_docx::DocxDocument::from_bytes(&data).expect("from_bytes");
    let diag = NullDiagnostics;
    let result = udoc_docx::docx_to_document(&mut doc, &diag, usize::MAX).expect("convert");

    // Should have: Header section, 2 body paragraphs, Footer section.
    assert!(
        result.content.len() >= 4,
        "expected at least 4 blocks (header section + 2 paragraphs + footer section), got {}",
        result.content.len()
    );

    // First block: Header section.
    match &result.content[0] {
        Block::Section { role, children, .. } => {
            assert_eq!(*role, Some(SectionRole::Header));
            assert!(!children.is_empty(), "header section should have children");
        }
        other => panic!("expected Section with Header role, got: {other:?}"),
    }

    // Last block: Footer section.
    let last = result.content.last().expect("content not empty");
    match last {
        Block::Section { role, children, .. } => {
            assert_eq!(*role, Some(SectionRole::Footer));
            assert!(!children.is_empty(), "footer section should have children");
        }
        other => panic!("expected Section with Footer role, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// list_last_element.docx -- numbered list as the last body element
// (tests the pending-list flush-at-end-of-document path)
// ---------------------------------------------------------------------------

fn build_list_last_element_docx() -> Vec<u8> {
    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Introduction paragraph.</w:t></w:r></w:p>
        <w:p>
            <w:pPr>
                <w:numPr><w:numId w:val="1"/><w:ilvl w:val="0"/></w:numPr>
            </w:pPr>
            <w:r><w:t>First list item</w:t></w:r>
        </w:p>
        <w:p>
            <w:pPr>
                <w:numPr><w:numId w:val="1"/><w:ilvl w:val="0"/></w:numPr>
            </w:pPr>
            <w:r><w:t>Second list item</w:t></w:r>
        </w:p>
        <w:p>
            <w:pPr>
                <w:numPr><w:numId w:val="1"/><w:ilvl w:val="0"/></w:numPr>
            </w:pPr>
            <w:r><w:t>Third list item</w:t></w:r>
        </w:p>
    </w:body>
</w:document>"#;

    let numbering_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:abstractNum w:abstractNumId="0">
        <w:lvl w:ilvl="0">
            <w:start w:val="1"/>
            <w:numFmt w:val="decimal"/>
            <w:lvlText w:val="%1."/>
        </w:lvl>
    </w:abstractNum>
    <w:num w:numId="1">
        <w:abstractNumId w:val="0"/>
    </w:num>
</w:numbering>"#;

    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering"
        Target="numbering.xml"/>
</Relationships>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", DOCX_CONTENT_TYPES),
        ("_rels/.rels", DOCX_PACKAGE_RELS),
        ("word/document.xml", document_xml),
        ("word/numbering.xml", numbering_xml),
        ("word/_rels/document.xml.rels", doc_rels),
    ])
}

#[test]
fn golden_list_last_element_text() {
    let data = build_list_last_element_docx();
    let mut doc = udoc_docx::DocxDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("list_last_element_text", &text, &golden_dir());
}

#[test]
fn list_last_element_conversion() {
    use udoc_core::diagnostics::NullDiagnostics;
    use udoc_core::document::{Block, ListKind};

    let data = build_list_last_element_docx();
    let mut doc = udoc_docx::DocxDocument::from_bytes(&data).expect("from_bytes");
    let diag = NullDiagnostics;
    let result = udoc_docx::docx_to_document(&mut doc, &diag, usize::MAX).expect("convert");

    // Should have: 1 intro paragraph + 1 list block (flushed at end of body).
    assert_eq!(
        result.content.len(),
        2,
        "expected 2 blocks (paragraph + list), got {}",
        result.content.len()
    );

    // First block: intro paragraph.
    match &result.content[0] {
        Block::Paragraph { content, .. } => {
            assert_eq!(content.len(), 1);
        }
        other => panic!("expected Paragraph, got: {other:?}"),
    }

    // Second block: ordered list with 3 items, flushed at end-of-document.
    match &result.content[1] {
        Block::List {
            items, kind, start, ..
        } => {
            assert_eq!(*kind, ListKind::Ordered);
            assert_eq!(*start, 1);
            assert_eq!(items.len(), 3, "expected 3 list items");
        }
        other => panic!("expected List, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// nested_table.docx -- table inside a table cell
// ---------------------------------------------------------------------------

fn build_nested_table_docx() -> Vec<u8> {
    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Outer paragraph.</w:t></w:r></w:p>
        <w:tbl>
            <w:tr>
                <w:tc>
                    <w:p><w:r><w:t>Outer cell A1</w:t></w:r></w:p>
                    <w:tbl>
                        <w:tr>
                            <w:tc><w:p><w:r><w:t>Inner A1</w:t></w:r></w:p></w:tc>
                            <w:tc><w:p><w:r><w:t>Inner B1</w:t></w:r></w:p></w:tc>
                        </w:tr>
                        <w:tr>
                            <w:tc><w:p><w:r><w:t>Inner A2</w:t></w:r></w:p></w:tc>
                            <w:tc><w:p><w:r><w:t>Inner B2</w:t></w:r></w:p></w:tc>
                        </w:tr>
                    </w:tbl>
                </w:tc>
                <w:tc>
                    <w:p><w:r><w:t>Outer cell B1</w:t></w:r></w:p>
                </w:tc>
            </w:tr>
        </w:tbl>
        <w:p><w:r><w:t>Trailing paragraph.</w:t></w:r></w:p>
    </w:body>
</w:document>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", DOCX_CONTENT_TYPES),
        ("_rels/.rels", DOCX_PACKAGE_RELS),
        ("word/document.xml", document_xml),
    ])
}

#[test]
fn golden_nested_table_text() {
    let data = build_nested_table_docx();
    let mut doc = udoc_docx::DocxDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("nested_table_text", &text, &golden_dir());
}

#[test]
fn nested_table_extraction() {
    let data = build_nested_table_docx();
    let mut doc = udoc_docx::DocxDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let tables = page.tables().expect("tables()");
    // The outer table and the inner table should both be returned.
    assert!(
        !tables.is_empty(),
        "expected at least 1 table, got {}",
        tables.len()
    );
    // Outer table has 1 row with 2 cells.
    assert_eq!(tables[0].rows.len(), 1, "outer table should have 1 row");
    assert_eq!(
        tables[0].rows[0].cells.len(),
        2,
        "outer row should have 2 cells"
    );
}

// ---------------------------------------------------------------------------
// multi_section.docx -- document with multiple sections separated by page breaks
// ---------------------------------------------------------------------------

fn build_multi_section_docx() -> Vec<u8> {
    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Section one, paragraph one.</w:t></w:r></w:p>
        <w:p><w:r><w:t>Section one, paragraph two.</w:t></w:r></w:p>
        <w:p>
            <w:r><w:br w:type="page"/></w:r>
            <w:r><w:t>Section two, paragraph one.</w:t></w:r>
        </w:p>
        <w:p><w:r><w:t>Section two, paragraph two.</w:t></w:r></w:p>
        <w:p>
            <w:r><w:br w:type="page"/></w:r>
            <w:r><w:t>Section three content.</w:t></w:r>
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
fn golden_multi_section_text() {
    let data = build_multi_section_docx();
    let mut doc = udoc_docx::DocxDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert_golden("multi_section_text", &text, &golden_dir());
}

#[test]
fn multi_section_contains_all_content() {
    let data = build_multi_section_docx();
    let mut doc = udoc_docx::DocxDocument::from_bytes(&data).expect("from_bytes");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text()");
    assert!(
        text.contains("Section one, paragraph one"),
        "missing section 1 paragraph 1"
    );
    assert!(
        text.contains("Section two, paragraph one"),
        "missing section 2 paragraph 1"
    );
    assert!(
        text.contains("Section three content"),
        "missing section 3 content"
    );
}

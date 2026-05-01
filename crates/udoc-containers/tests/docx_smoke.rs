//! End-to-end DOCX smoke test for the udoc-containers crate.
//!
//! Builds a realistic DOCX file in memory (ZIP containing proper OOXML parts)
//! and verifies the full stack: ZIP parse -> decompress -> XML parse ->
//! namespace resolution -> OPC navigation -> text extraction.

use std::sync::Arc;

use udoc_containers::opc::{rel_types, OpcPackage, TargetMode};
use udoc_containers::test_util::build_stored_zip as build_zip;
use udoc_containers::xml::{ns, XmlEvent, XmlReader};
use udoc_core::diagnostics::NullDiagnostics;

// ---------------------------------------------------------------------------
// Realistic DOCX fixture data
// ---------------------------------------------------------------------------

/// [Content_Types].xml with proper OOXML content type overrides.
const CONTENT_TYPES_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/word/document.xml"
        ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
    <Override PartName="/word/styles.xml"
        ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
    <Override PartName="/word/numbering.xml"
        ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>
    <Override PartName="/docProps/core.xml"
        ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>
</Types>"#;

/// Package-level relationships: officeDocument + core properties.
const PACKAGE_RELS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
        Target="word/document.xml"/>
    <Relationship Id="rId2"
        Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties"
        Target="docProps/core.xml"/>
</Relationships>"#;

/// A realistic word/document.xml with multiple paragraphs, formatting runs
/// (bold, italic), xml:space="preserve", and a hyperlink run.
const DOCUMENT_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
    <w:body>
        <w:p>
            <w:pPr><w:pStyle w:val="Heading1"/></w:pPr>
            <w:r>
                <w:rPr><w:b/></w:rPr>
                <w:t>Document Title</w:t>
            </w:r>
        </w:p>
        <w:p>
            <w:r>
                <w:t>This is the first paragraph with </w:t>
            </w:r>
            <w:r>
                <w:rPr><w:b/></w:rPr>
                <w:t>bold</w:t>
            </w:r>
            <w:r>
                <w:t xml:space="preserve"> and </w:t>
            </w:r>
            <w:r>
                <w:rPr><w:i/></w:rPr>
                <w:t>italic</w:t>
            </w:r>
            <w:r>
                <w:t> text.</w:t>
            </w:r>
        </w:p>
        <w:p>
            <w:r>
                <w:t>Second paragraph with a </w:t>
            </w:r>
            <w:hyperlink r:id="rId3">
                <w:r>
                    <w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr>
                    <w:t>link</w:t>
                </w:r>
            </w:hyperlink>
            <w:r>
                <w:t>.</w:t>
            </w:r>
        </w:p>
        <w:p>
            <w:r>
                <w:t>Entities: &amp; &lt; &gt; &quot;</w:t>
            </w:r>
        </w:p>
    </w:body>
</w:document>"#;

/// Per-part relationships for word/document.xml: styles, numbering, hyperlink.
const DOCUMENT_RELS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles"
        Target="styles.xml"/>
    <Relationship Id="rId2"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering"
        Target="numbering.xml"/>
    <Relationship Id="rId3"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink"
        Target="https://example.com/docs"
        TargetMode="External"/>
</Relationships>"#;

/// A minimal styles.xml with one paragraph style.
const STYLES_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:style w:type="paragraph" w:styleId="Heading1">
        <w:name w:val="heading 1"/>
        <w:pPr><w:outlineLvl w:val="0"/></w:pPr>
        <w:rPr><w:b/><w:sz w:val="32"/></w:rPr>
    </w:style>
    <w:style w:type="character" w:styleId="Hyperlink">
        <w:name w:val="Hyperlink"/>
        <w:rPr><w:color w:val="0563C1"/></w:rPr>
    </w:style>
</w:styles>"#;

/// A minimal numbering.xml (no actual numbering definitions, just the wrapper).
const NUMBERING_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
</w:numbering>"#;

/// Core properties (docProps/core.xml).
const CORE_PROPS_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"
                   xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Test Document</dc:title>
    <dc:creator>udoc test suite</dc:creator>
</cp:coreProperties>"#;

fn make_realistic_docx() -> Vec<u8> {
    build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML),
        ("_rels/.rels", PACKAGE_RELS),
        ("word/document.xml", DOCUMENT_XML),
        ("word/_rels/document.xml.rels", DOCUMENT_RELS),
        ("word/styles.xml", STYLES_XML),
        ("word/numbering.xml", NUMBERING_XML),
        ("docProps/core.xml", CORE_PROPS_XML),
    ])
}

// ---------------------------------------------------------------------------
// Helper: extract all w:t text from a document.xml byte slice
// ---------------------------------------------------------------------------

/// Walk XML events, collecting text content from all w:t elements.
/// Returns a Vec of strings, one per w:t element encountered.
fn extract_wt_texts(xml_bytes: &[u8]) -> Vec<String> {
    let mut reader = XmlReader::new(xml_bytes).unwrap();
    let mut texts = Vec::new();
    let mut inside_wt = false;
    let mut current_text = String::new();

    loop {
        match reader.next_event().unwrap() {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                ..
            }
                // Match w:t elements by local name "t" in the WML namespace.
                if local_name == "t" && namespace_uri.as_deref() == Some(ns::WML) => {
                    inside_wt = true;
                    current_text.clear();
                }
            XmlEvent::EndElement {
                local_name,
                namespace_uri,
                ..
            }
                if local_name == "t" && namespace_uri.as_deref() == Some(ns::WML) && inside_wt => {
                    texts.push(current_text.clone());
                    inside_wt = false;
                }
            XmlEvent::Text(s)
                if inside_wt => {
                    current_text.push_str(&s);
                }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    texts
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Full end-to-end test: open DOCX as OpcPackage, navigate to document.xml
/// via package rels, parse XML, extract text from w:t elements.
#[test]
fn docx_full_stack_text_extraction() {
    let zip_bytes = make_realistic_docx();
    let pkg = OpcPackage::new(&zip_bytes, Arc::new(NullDiagnostics)).unwrap();

    // Step 1: find the officeDocument relationship
    let doc_rel = pkg
        .find_package_rel_by_type(rel_types::OFFICE_DOCUMENT)
        .expect("should find officeDocument relationship");

    assert_eq!(doc_rel.target, "word/document.xml");
    assert_eq!(doc_rel.target_mode, TargetMode::Internal);

    // Step 2: read document.xml
    let doc_xml = pkg.read_part_string(&doc_rel.target).unwrap();

    // Step 3: parse and extract text from w:t elements
    let texts = extract_wt_texts(doc_xml.as_bytes());

    // Verify expected text fragments
    assert!(
        texts.contains(&"Document Title".to_string()),
        "should find title text, got: {texts:?}"
    );
    assert!(
        texts.contains(&"bold".to_string()),
        "should find bold text, got: {texts:?}"
    );
    assert!(
        texts.contains(&"italic".to_string()),
        "should find italic text, got: {texts:?}"
    );
    assert!(
        texts.contains(&"link".to_string()),
        "should find hyperlink text, got: {texts:?}"
    );

    // Verify entity decoding in text: the w:t containing "Entities: & < > \""
    let entity_text = texts
        .iter()
        .find(|t| t.starts_with("Entities:"))
        .expect("should find entity text");
    assert_eq!(
        entity_text, "Entities: & < > \"",
        "XML entities should be decoded in extracted text"
    );

    // Concatenated text should form readable paragraphs
    let all_text = texts.join("");
    assert!(
        all_text.contains("first paragraph with bold and italic text."),
        "concatenated runs should form readable text, got: {all_text}"
    );
    assert!(
        all_text.contains("Second paragraph with a link."),
        "second paragraph text should be present, got: {all_text}"
    );
}

/// Verify that namespace resolution works correctly throughout the document:
/// all w:* elements should resolve to the WML namespace URI.
#[test]
fn docx_namespace_resolution_throughout_document() {
    let zip_bytes = make_realistic_docx();
    let pkg = OpcPackage::new(&zip_bytes, Arc::new(NullDiagnostics)).unwrap();

    let doc_xml = pkg.read_part_string("word/document.xml").unwrap();
    let mut reader = XmlReader::new(doc_xml.as_bytes()).unwrap();

    let mut w_prefixed_count = 0;
    let mut r_prefixed_count = 0;

    loop {
        match reader.next_event().unwrap() {
            XmlEvent::StartElement {
                prefix,
                namespace_uri,
                ..
            } => {
                if prefix == "w" {
                    assert_eq!(
                        namespace_uri.as_deref(),
                        Some(ns::WML),
                        "all w: elements should resolve to WML namespace"
                    );
                    w_prefixed_count += 1;
                }
                if prefix == "r" {
                    assert_eq!(
                        namespace_uri.as_deref(),
                        Some("http://schemas.openxmlformats.org/officeDocument/2006/relationships"),
                        "r: prefix should resolve to relationships namespace"
                    );
                    r_prefixed_count += 1;
                }
            }
            XmlEvent::EndElement {
                prefix,
                namespace_uri,
                ..
            } if prefix == "w" => {
                assert_eq!(
                    namespace_uri.as_deref(),
                    Some(ns::WML),
                    "all w: end elements should resolve to WML namespace"
                );
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    // We should have seen a good number of w: elements
    assert!(
        w_prefixed_count > 10,
        "expected many w: elements, got {w_prefixed_count}"
    );
    // The hyperlink element uses r:id attribute, but w:hyperlink is a w: element.
    // r: prefix appears in attributes, not element names in our fixture.
    // No r:-prefixed elements expected here.
    assert_eq!(
        r_prefixed_count, 0,
        "r: prefix should only appear in attributes, not element names"
    );
}

/// Verify content type lookup for all parts in the DOCX package.
#[test]
fn docx_content_type_for_all_parts() {
    let zip_bytes = make_realistic_docx();
    let pkg = OpcPackage::new(&zip_bytes, Arc::new(NullDiagnostics)).unwrap();

    // Override content types (exact PartName match)
    assert_eq!(
        pkg.content_type("/word/document.xml"),
        Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"),
    );
    assert_eq!(
        pkg.content_type("/word/styles.xml"),
        Some("application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"),
    );
    assert_eq!(
        pkg.content_type("/word/numbering.xml"),
        Some("application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"),
    );
    assert_eq!(
        pkg.content_type("/docProps/core.xml"),
        Some("application/vnd.openxmlformats-package.core-properties+xml"),
    );

    // Default content types (by extension)
    assert_eq!(
        pkg.content_type("/_rels/.rels"),
        Some("application/vnd.openxmlformats-package.relationships+xml"),
    );
    assert_eq!(
        pkg.content_type("/word/_rels/document.xml.rels"),
        Some("application/vnd.openxmlformats-package.relationships+xml"),
    );
}

/// Per-part relationships: verify styles, numbering, and hyperlink rels
/// on word/document.xml, and verify that styles.xml has no rels.
#[test]
fn docx_per_part_relationships() {
    let zip_bytes = make_realistic_docx();
    let pkg = OpcPackage::new(&zip_bytes, Arc::new(NullDiagnostics)).unwrap();

    // document.xml should have 3 relationships
    let doc_rels = pkg.part_rels("/word/document.xml");
    assert_eq!(
        doc_rels.len(),
        3,
        "document.xml should have 3 rels (styles + numbering + hyperlink)"
    );

    // Check styles rel
    let styles_rel = doc_rels
        .iter()
        .find(|r| r.rel_type == rel_types::STYLES)
        .expect("should have styles relationship");
    assert_eq!(styles_rel.target, "styles.xml");
    assert_eq!(styles_rel.target_mode, TargetMode::Internal);
    assert_eq!(styles_rel.id, "rId1");

    // Check numbering rel
    let numbering_rel = doc_rels
        .iter()
        .find(|r| r.rel_type == rel_types::NUMBERING)
        .expect("should have numbering relationship");
    assert_eq!(numbering_rel.target, "numbering.xml");
    assert_eq!(numbering_rel.target_mode, TargetMode::Internal);

    // Check hyperlink rel (external)
    let hyperlink_rel = doc_rels
        .iter()
        .find(|r| r.rel_type == rel_types::HYPERLINK)
        .expect("should have hyperlink relationship");
    assert_eq!(hyperlink_rel.target, "https://example.com/docs");
    assert_eq!(hyperlink_rel.target_mode, TargetMode::External);

    // styles.xml has no .rels file, so it should return empty
    let styles_rels = pkg.part_rels("/word/styles.xml");
    assert!(
        styles_rels.is_empty(),
        "styles.xml should have no relationships"
    );
}

/// URI resolution from document.xml to sibling parts and parent-traversal.
#[test]
fn docx_uri_resolution() {
    let zip_bytes = make_realistic_docx();
    let pkg = OpcPackage::new(&zip_bytes, Arc::new(NullDiagnostics)).unwrap();

    // Relative from document.xml to styles.xml (same directory)
    let styles_uri = pkg.resolve_uri("/word/document.xml", "styles.xml");
    assert_eq!(styles_uri, "/word/styles.xml");

    // Relative from document.xml to numbering.xml (same directory)
    let numbering_uri = pkg.resolve_uri("/word/document.xml", "numbering.xml");
    assert_eq!(numbering_uri, "/word/numbering.xml");

    // Parent traversal from word/document.xml to docProps/core.xml
    let core_uri = pkg.resolve_uri("/word/document.xml", "../docProps/core.xml");
    assert_eq!(core_uri, "/docProps/core.xml");

    // Absolute target is returned as-is (normalized)
    let abs_uri = pkg.resolve_uri("/word/document.xml", "/word/styles.xml");
    assert_eq!(abs_uri, "/word/styles.xml");
}

/// Navigate from package rels -> document -> styles via URI resolution,
/// then read and parse styles.xml to verify the styles content.
#[test]
fn docx_navigate_to_styles_via_rels_and_uri() {
    let zip_bytes = make_realistic_docx();
    let pkg = OpcPackage::new(&zip_bytes, Arc::new(NullDiagnostics)).unwrap();

    // 1. Find document via package rels
    let doc_rel = pkg
        .find_package_rel_by_type(rel_types::OFFICE_DOCUMENT)
        .unwrap();
    let doc_part = format!("/{}", doc_rel.target);

    // 2. Get document rels and find styles
    let styles_rel = pkg
        .find_part_rel_by_type(&doc_part, rel_types::STYLES)
        .expect("should find styles rel");

    // 3. Resolve styles URI relative to document
    let styles_uri = pkg.resolve_uri(&doc_part, &styles_rel.target);
    assert_eq!(styles_uri, "/word/styles.xml");

    // 4. Read and parse styles.xml
    let styles_xml = pkg.read_part_string(&styles_uri).unwrap();
    let mut reader = XmlReader::new(styles_xml.as_bytes()).unwrap();

    // First element should be w:styles in WML namespace
    let ev = reader.next_element().unwrap();
    match &ev {
        XmlEvent::StartElement {
            local_name,
            prefix,
            namespace_uri,
            ..
        } => {
            assert_eq!(local_name, "styles");
            assert_eq!(prefix, "w");
            assert_eq!(namespace_uri.as_deref(), Some(ns::WML));
        }
        other => panic!("expected <w:styles>, got: {other:?}"),
    }

    // Walk to find style definitions
    let mut style_ids = Vec::new();
    loop {
        match reader.next_element().unwrap() {
            XmlEvent::StartElement {
                local_name,
                attributes,
                namespace_uri,
                ..
            } if local_name == "style" && namespace_uri.as_deref() == Some(ns::WML) => {
                // w:styleId is a w:-prefixed attribute
                if let Some(id_attr) = attributes.iter().find(|a| a.local_name == "styleId") {
                    style_ids.push(id_attr.value.to_string());
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    assert!(
        style_ids.contains(&"Heading1".to_string()),
        "should find Heading1 style, got: {style_ids:?}"
    );
    assert!(
        style_ids.contains(&"Hyperlink".to_string()),
        "should find Hyperlink style, got: {style_ids:?}"
    );
}

/// Core properties: navigate from package rels to docProps/core.xml
/// and verify title and creator.
#[test]
fn docx_core_properties() {
    let zip_bytes = make_realistic_docx();
    let pkg = OpcPackage::new(&zip_bytes, Arc::new(NullDiagnostics)).unwrap();

    // Find core properties via package rels
    let core_rel = pkg
        .find_package_rel_by_type(rel_types::CORE_PROPERTIES)
        .expect("should find core-properties relationship");
    assert_eq!(core_rel.target, "docProps/core.xml");

    let core_xml = pkg.read_part_string(&core_rel.target).unwrap();

    // Parse and extract dc:title and dc:creator
    let mut reader = XmlReader::new(core_xml.as_bytes()).unwrap();
    let mut title = None;
    let mut creator = None;
    let mut current_element = String::new();

    loop {
        match reader.next_event().unwrap() {
            XmlEvent::StartElement { local_name, .. } => {
                current_element = local_name.into_owned();
            }
            XmlEvent::Text(s) => {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    if current_element == "title" {
                        title = Some(trimmed.to_string());
                    } else if current_element == "creator" {
                        creator = Some(trimmed.to_string());
                    }
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    assert_eq!(
        title.as_deref(),
        Some("Test Document"),
        "dc:title should be 'Test Document'"
    );
    assert_eq!(
        creator.as_deref(),
        Some("udoc test suite"),
        "dc:creator should be 'udoc test suite'"
    );
}

/// Verify that reading parts works with both leading-slash and bare paths.
#[test]
fn docx_read_parts_slash_variants() {
    let zip_bytes = make_realistic_docx();
    let pkg = OpcPackage::new(&zip_bytes, Arc::new(NullDiagnostics)).unwrap();

    let with_slash = pkg.read_part_string("/word/document.xml").unwrap();
    let without_slash = pkg.read_part_string("word/document.xml").unwrap();
    assert_eq!(with_slash, without_slash, "leading slash should not matter");
}

/// Verify that requesting a nonexistent part produces a clear error.
#[test]
fn docx_missing_part_error() {
    let zip_bytes = make_realistic_docx();
    let pkg = OpcPackage::new(&zip_bytes, Arc::new(NullDiagnostics)).unwrap();

    let result = pkg.read_part("word/footer1.xml");
    assert!(result.is_err(), "missing part should be an error");

    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("not found"),
        "error should mention 'not found', got: {msg}"
    );
}

/// Verify the full text extraction pipeline produces the expected combined
/// text when concatenated with paragraph breaks.
#[test]
fn docx_text_extraction_paragraph_reconstruction() {
    let zip_bytes = make_realistic_docx();
    let pkg = OpcPackage::new(&zip_bytes, Arc::new(NullDiagnostics)).unwrap();

    let doc_xml = pkg.read_part_string("word/document.xml").unwrap();
    let mut reader = XmlReader::new(doc_xml.as_bytes()).unwrap();

    // Collect text per paragraph: group w:t text by w:p boundaries.
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current_para = String::new();
    let mut inside_wt = false;
    let mut para_depth = 0;

    loop {
        match reader.next_event().unwrap() {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                ..
            } if namespace_uri.as_deref() == Some(ns::WML) => {
                if local_name == "p" {
                    para_depth += 1;
                    if para_depth == 1 {
                        current_para.clear();
                    }
                } else if local_name == "t" {
                    inside_wt = true;
                }
            }
            XmlEvent::EndElement {
                local_name,
                namespace_uri,
                ..
            } if namespace_uri.as_deref() == Some(ns::WML) => {
                if local_name == "t" {
                    inside_wt = false;
                } else if local_name == "p" {
                    para_depth -= 1;
                    if para_depth == 0 && !current_para.is_empty() {
                        paragraphs.push(current_para.clone());
                    }
                }
            }
            XmlEvent::Text(s) if inside_wt => {
                current_para.push_str(&s);
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    assert_eq!(
        paragraphs.len(),
        4,
        "should have 4 paragraphs: {paragraphs:?}"
    );
    assert_eq!(paragraphs[0], "Document Title");
    assert_eq!(
        paragraphs[1],
        "This is the first paragraph with bold and italic text."
    );
    assert_eq!(paragraphs[2], "Second paragraph with a link.");
    assert_eq!(paragraphs[3], "Entities: & < > \"");
}

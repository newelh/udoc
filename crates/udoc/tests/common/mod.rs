//! Shared test fixture builders for integration tests.
//!
//! Imported via `mod common;` from individual test files to avoid
//! duplicating synthetic document construction code.

use udoc_containers::test_util::{build_stored_zip, DOCX_CONTENT_TYPES, DOCX_PACKAGE_RELS};

/// Build a minimal DOCX with a single paragraph containing a hyperlink
/// to `https://example.com` with text "Click here".
pub fn make_docx_with_hyperlink() -> Vec<u8> {
    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
    <w:body>
        <w:p>
            <w:hyperlink r:id="rId1">
                <w:r><w:t>Click here</w:t></w:r>
            </w:hyperlink>
        </w:p>
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
        ("[Content_Types].xml", DOCX_CONTENT_TYPES),
        ("_rels/.rels", DOCX_PACKAGE_RELS),
        ("word/document.xml", document_xml),
        ("word/_rels/document.xml.rels", doc_rels),
    ])
}

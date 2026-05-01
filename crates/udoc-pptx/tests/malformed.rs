//! Tests for PPTX malformed file recovery.
//!
//! Each test verifies that a malformed PPTX file can be handled without
//! panicking. Some return Err, some return Ok with partial content.

use std::sync::Arc;

use udoc_containers::test_util::build_stored_zip;
use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_core::diagnostics::CollectingDiagnostics;
use udoc_pptx::PptxDocument;

#[test]
fn truncated_zip_no_panic() {
    // Random bytes that are not a valid ZIP
    let garbage = b"This is not a ZIP file at all, just random garbage bytes \x00\x01\x02\xFF";
    let result = PptxDocument::from_bytes(garbage);
    assert!(result.is_err(), "truncated/garbage data should return Err");
}

#[test]
fn missing_slide_xml() {
    // Valid ZIP with OPC structure but the slide XML referenced in
    // presentation.xml does not exist in the archive.
    let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
</Types>"#;

    let package_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"#;

    let presentation = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
                xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:sldIdLst>
    <p:sldId id="256" r:id="rId2"/>
  </p:sldIdLst>
</p:presentation>"#;

    // presentation.xml.rels points to slides/slide1.xml which does NOT exist
    let pres_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
</Relationships>"#;

    let data = build_stored_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", package_rels),
        ("ppt/presentation.xml", presentation),
        ("ppt/_rels/presentation.xml.rels", pres_rels),
        // No ppt/slides/slide1.xml -- it is missing!
    ]);

    // Should not panic. Should return Ok with 1 slide (empty content) or
    // handle gracefully.
    let diag = Arc::new(CollectingDiagnostics::new());
    let result = PptxDocument::from_bytes_with_diag(&data, diag.clone());
    match result {
        Ok(mut doc) => {
            // The document may report 1 slide with empty content (recovery)
            assert!(doc.page_count() <= 1);
            if doc.page_count() == 1 {
                let mut page = doc.page(0).expect("page 0");
                let text = page.text().expect("text");
                // Empty is fine -- the slide data was missing
                assert!(
                    text.is_empty(),
                    "missing slide should produce empty text, got: {text}"
                );
            }
            assert!(
                !diag.warnings().is_empty(),
                "missing slide XML should produce at least one warning"
            );
        }
        Err(_) => {
            // Also acceptable: returning an error for missing slide
        }
    }
}

#[test]
fn empty_presentation_no_slides() {
    // Valid OPC structure but presentation.xml has an empty sldIdLst.
    let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
</Types>"#;

    let package_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"#;

    let presentation = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
                xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:sldIdLst/>
</p:presentation>"#;

    let pres_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;

    let data = build_stored_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", package_rels),
        ("ppt/presentation.xml", presentation),
        ("ppt/_rels/presentation.xml.rels", pres_rels),
    ]);

    let result = PptxDocument::from_bytes(&data);
    match result {
        Ok(doc) => {
            assert_eq!(
                doc.page_count(),
                0,
                "empty presentation should have 0 slides"
            );
        }
        Err(_) => {
            // Also acceptable
        }
    }
}

#[test]
fn missing_content_types_returns_err() {
    // ZIP with no [Content_Types].xml at all
    let package_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"#;

    let presentation = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:sldIdLst/>
</p:presentation>"#;

    let data = build_stored_zip(&[
        ("_rels/.rels", package_rels),
        ("ppt/presentation.xml", presentation),
    ]);

    let result = PptxDocument::from_bytes(&data);
    assert!(
        result.is_err(),
        "missing [Content_Types].xml should return Err"
    );
}

#[test]
fn missing_presentation_rel_returns_err() {
    // Valid ZIP + [Content_Types].xml but no officeDocument relationship
    let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
</Types>"#;

    let package_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;

    let data = build_stored_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", package_rels),
    ]);

    let result = PptxDocument::from_bytes(&data);
    assert!(
        result.is_err(),
        "missing officeDocument relationship should return Err"
    );
}

#[test]
fn corrupted_slide_xml_recovers() {
    // Valid OPC structure but slide1.xml contains invalid XML
    let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
  <Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
</Types>"#;

    let package_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"#;

    let presentation = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
                xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:sldIdLst>
    <p:sldId id="256" r:id="rId2"/>
  </p:sldIdLst>
</p:presentation>"#;

    let pres_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
</Relationships>"#;

    // Invalid XML content for the slide
    let bad_slide = b"<<<NOT VALID XML>>> this will fail to parse {{{";

    let slide_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;

    let data = build_stored_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", package_rels),
        ("ppt/presentation.xml", presentation),
        ("ppt/_rels/presentation.xml.rels", pres_rels),
        ("ppt/slides/slide1.xml", bad_slide),
        ("ppt/slides/_rels/slide1.xml.rels", slide_rels),
    ]);

    // Should not panic. The backend should recover with empty slide content.
    let diag = Arc::new(CollectingDiagnostics::new());
    let result = PptxDocument::from_bytes_with_diag(&data, diag.clone());
    match result {
        Ok(mut doc) => {
            assert_eq!(doc.page_count(), 1);
            let mut page = doc.page(0).expect("page 0");
            let text = page.text().expect("text");
            // Empty is fine -- the slide XML was corrupt
            assert!(
                text.is_empty(),
                "corrupted slide XML should produce empty text, got: {text}"
            );
            assert!(
                !diag.warnings().is_empty(),
                "corrupted slide XML should produce at least one warning"
            );
        }
        Err(_) => {
            // Also acceptable if the whole document fails
        }
    }
}

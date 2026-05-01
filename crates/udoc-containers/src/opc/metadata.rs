//! Shared OOXML core properties (Dublin Core) metadata parser.
//!
//! All OOXML formats (DOCX, XLSX, PPTX) store metadata identically in
//! `docProps/core.xml` using Dublin Core elements. This module provides
//! a single parser that all backends share.

use udoc_core::backend::DocumentMetadata;

use crate::xml::namespace::ns;
use crate::xml::{XmlEvent, XmlReader};

/// Which namespace family the current element belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MetaNs {
    /// Dublin Core elements (dc:title, dc:creator, dc:subject, dc:description)
    Dc,
    /// Dublin Core terms (dcterms:created, dcterms:modified)
    DcTerms,
    /// OPC core properties (cp:lastModifiedBy)
    CoreProps,
    /// Unknown namespace, ignore element text
    Other,
}

/// Parse Dublin Core metadata from a `docProps/core.xml` byte slice.
///
/// Extracts `dc:title`, `dc:creator`, `dc:subject`, `dc:description`,
/// `dcterms:created`, `dcterms:modified`, and `cp:lastModifiedBy`.
/// Missing or empty elements are silently skipped (returns partial metadata).
/// Malformed XML is handled gracefully (returns whatever was parsed so far).
///
/// Only elements in the expected Dublin Core / OPC namespaces are matched.
/// Elements with the same local name in other namespaces are ignored.
///
/// The returned `DocumentMetadata` has `page_count = 0`. Callers should
/// set `page_count` themselves after calling this function.
pub fn parse_core_properties(xml_bytes: &[u8]) -> DocumentMetadata {
    let mut meta = DocumentMetadata::default();

    let mut reader = match XmlReader::new(xml_bytes) {
        Ok(r) => r,
        Err(_) => return meta,
    };

    let mut current_element: Option<(String, MetaNs)> = None;

    loop {
        match reader.next_event() {
            Ok(XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ..
            }) => {
                let ns_kind = match namespace_uri.as_deref() {
                    Some(ns::DC_ELEMENTS) => MetaNs::Dc,
                    Some(ns::DC_TERMS) => MetaNs::DcTerms,
                    Some(ns::CORE_PROPERTIES) => MetaNs::CoreProps,
                    _ => MetaNs::Other,
                };
                current_element = Some((local_name.to_string(), ns_kind));
            }
            Ok(XmlEvent::Text(ref text)) => {
                if let Some((ref elem, ns_kind)) = current_element {
                    let text = text.trim();
                    if !text.is_empty() {
                        match (elem.as_str(), ns_kind) {
                            ("title", MetaNs::Dc) => meta.title = Some(text.to_string()),
                            ("creator", MetaNs::Dc) => {
                                meta.author = Some(text.to_string());
                                meta.creator = Some(text.to_string());
                            }
                            ("subject", MetaNs::Dc) => meta.subject = Some(text.to_string()),
                            ("description", MetaNs::Dc) => {
                                meta.properties
                                    .insert("description".to_string(), text.to_string());
                            }
                            ("created", MetaNs::DcTerms) => {
                                meta.creation_date = Some(text.to_string());
                            }
                            ("modified", MetaNs::DcTerms) => {
                                meta.modification_date = Some(text.to_string());
                            }
                            ("lastModifiedBy", MetaNs::CoreProps) => {
                                meta.properties
                                    .insert("lastModifiedBy".to_string(), text.to_string());
                            }
                            _ => {}
                        }
                    }
                }
            }
            Ok(XmlEvent::EndElement { .. }) => {
                current_element = None;
            }
            Ok(XmlEvent::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }

    meta
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_core_properties() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"
                   xmlns:dc="http://purl.org/dc/elements/1.1/"
                   xmlns:dcterms="http://purl.org/dc/terms/">
  <dc:title>Test Document</dc:title>
  <dc:creator>Jane Doe</dc:creator>
  <dc:subject>Testing</dc:subject>
  <dc:description>A test document for metadata parsing</dc:description>
  <dcterms:created>2024-01-15T10:30:00Z</dcterms:created>
  <dcterms:modified>2024-01-16T14:00:00Z</dcterms:modified>
  <cp:lastModifiedBy>John Smith</cp:lastModifiedBy>
</cp:coreProperties>"#;

        let meta = parse_core_properties(xml);
        assert_eq!(meta.title.as_deref(), Some("Test Document"));
        assert_eq!(meta.author.as_deref(), Some("Jane Doe"));
        assert_eq!(meta.creator.as_deref(), Some("Jane Doe"));
        assert_eq!(meta.subject.as_deref(), Some("Testing"));
        assert_eq!(meta.creation_date.as_deref(), Some("2024-01-15T10:30:00Z"));
        assert_eq!(
            meta.modification_date.as_deref(),
            Some("2024-01-16T14:00:00Z")
        );
        assert_eq!(
            meta.properties.get("description").map(|s| s.as_str()),
            Some("A test document for metadata parsing")
        );
        assert_eq!(
            meta.properties.get("lastModifiedBy").map(|s| s.as_str()),
            Some("John Smith")
        );
        // page_count defaults to 0; callers set it themselves
        assert_eq!(meta.page_count, 0);
    }

    #[test]
    fn parse_empty_core_properties() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties">
</cp:coreProperties>"#;

        let meta = parse_core_properties(xml);
        assert!(meta.title.is_none());
        assert!(meta.author.is_none());
        assert!(meta.subject.is_none());
        assert!(meta.creator.is_none());
        assert!(meta.creation_date.is_none());
        assert!(meta.modification_date.is_none());
        assert!(meta.properties.is_empty());
    }

    #[test]
    fn parse_partial_core_properties() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"
                   xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:title>Only Title</dc:title>
</cp:coreProperties>"#;

        let meta = parse_core_properties(xml);
        assert_eq!(meta.title.as_deref(), Some("Only Title"));
        assert!(meta.author.is_none());
        assert!(meta.subject.is_none());
    }

    #[test]
    fn parse_whitespace_only_values_ignored() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"
                   xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:title>   </dc:title>
  <dc:creator>Real Author</dc:creator>
</cp:coreProperties>"#;

        let meta = parse_core_properties(xml);
        assert!(meta.title.is_none());
        assert_eq!(meta.author.as_deref(), Some("Real Author"));
    }

    #[test]
    fn parse_malformed_xml_returns_partial() {
        // Truncated XML -- should return whatever was parsed before the error
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"
                   xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:title>Got This</dc:title>
  <dc:creator>Also This</dc:creator>
  <BROKEN"#;

        let meta = parse_core_properties(xml);
        assert_eq!(meta.title.as_deref(), Some("Got This"));
        assert_eq!(meta.author.as_deref(), Some("Also This"));
    }

    #[test]
    fn parse_invalid_xml_returns_default() {
        let meta = parse_core_properties(b"not xml at all <<<>>>");
        assert!(meta.title.is_none());
        assert!(meta.author.is_none());
    }

    #[test]
    fn creator_sets_both_author_and_creator() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"
                   xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:creator>Both Fields</dc:creator>
</cp:coreProperties>"#;

        let meta = parse_core_properties(xml);
        assert_eq!(meta.author.as_deref(), Some("Both Fields"));
        assert_eq!(meta.creator.as_deref(), Some("Both Fields"));
    }

    #[test]
    fn wrong_namespace_elements_ignored() {
        // Elements with matching local names but wrong namespaces must not be captured.
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"
                   xmlns:dc="http://purl.org/dc/elements/1.1/"
                   xmlns:custom="http://example.com/custom">
  <dc:title>Real Title</dc:title>
  <custom:title>Fake Title</custom:title>
  <custom:creator>Fake Creator</custom:creator>
</cp:coreProperties>"#;

        let meta = parse_core_properties(xml);
        assert_eq!(meta.title.as_deref(), Some("Real Title"));
        assert!(meta.author.is_none());
        assert!(meta.creator.is_none());
    }

    #[test]
    fn parse_unknown_elements_ignored() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"
                   xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:title>Known</dc:title>
  <dc:language>en-US</dc:language>
  <cp:category>Report</cp:category>
</cp:coreProperties>"#;

        let meta = parse_core_properties(xml);
        assert_eq!(meta.title.as_deref(), Some("Known"));
        // language and category are not mapped
        assert!(!meta.properties.contains_key("language"));
    }
}

//! `[Content_Types].xml` parser for OPC packages.
//!
//! Maps part names to MIME content types via `Default` (by extension) and
//! `Override` (by full part name) elements. Lookups are case-insensitive
//! per ECMA-376 Part 2 section 10.1.3.

use std::collections::HashMap;

use crate::error::Result;
use crate::xml::{attr_value, XmlEvent, XmlReader};

/// Parsed content types from `[Content_Types].xml`.
#[derive(Debug)]
pub(crate) struct ContentTypes {
    /// Extension-based defaults: extension_lowercase -> content_type.
    defaults: HashMap<String, String>,
    /// Part-name overrides: part_name_lowercase -> content_type.
    overrides: HashMap<String, String>,
}

impl ContentTypes {
    /// Parse `[Content_Types].xml` from its raw bytes.
    pub(crate) fn parse(data: &[u8]) -> Result<Self> {
        let mut reader = XmlReader::new(data)?;
        let mut defaults = HashMap::new();
        let mut overrides = HashMap::new();

        loop {
            match reader.next_element()? {
                XmlEvent::StartElement {
                    local_name,
                    attributes,
                    ..
                } => {
                    if local_name == "Default" {
                        let ext = attr_value(&attributes, "Extension");
                        let ct = attr_value(&attributes, "ContentType");
                        if let (Some(ext), Some(ct)) = (ext, ct) {
                            defaults.insert(ext.to_ascii_lowercase(), ct.to_string());
                        }
                    } else if local_name == "Override" {
                        let pn = attr_value(&attributes, "PartName");
                        let ct = attr_value(&attributes, "ContentType");
                        if let (Some(pn), Some(ct)) = (pn, ct) {
                            overrides.insert(pn.to_ascii_lowercase(), ct.to_string());
                        }
                    }
                }
                XmlEvent::Eof => break,
                _ => {}
            }
        }

        Ok(ContentTypes {
            defaults,
            overrides,
        })
    }

    /// Look up the content type for a part name.
    ///
    /// Checks overrides first (case-insensitive), then defaults by extension.
    pub(crate) fn content_type(&self, part_name: &str) -> Option<&str> {
        let lower = part_name.to_ascii_lowercase();

        // Check overrides first (O(1) lookup)
        if let Some(ct) = self.overrides.get(&lower) {
            return Some(ct);
        }

        // Fall back to extension-based defaults (O(1) lookup)
        let ext = lower.rsplit('.').next()?;
        self.defaults.get(ext).map(|ct| ct.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_content_types() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

        let ct = ContentTypes::parse(xml).unwrap();
        assert_eq!(ct.defaults.len(), 2, "defaults: {:#?}", ct.defaults);
        assert_eq!(ct.overrides.len(), 1, "overrides: {:#?}", ct.overrides);
    }

    #[test]
    fn content_type_override() {
        let xml = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

        let ct = ContentTypes::parse(xml).unwrap();
        let result = ct.content_type("/word/document.xml");
        assert_eq!(
            result,
            Some(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"
            )
        );
    }

    #[test]
    fn content_type_default_extension() {
        let xml = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="xml" ContentType="application/xml"/>
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
</Types>"#;

        let ct = ContentTypes::parse(xml).unwrap();
        assert_eq!(ct.content_type("/some/random.xml"), Some("application/xml"));
        assert_eq!(
            ct.content_type("/_rels/.rels"),
            Some("application/vnd.openxmlformats-package.relationships+xml")
        );
    }

    #[test]
    fn content_type_case_insensitive() {
        let xml = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Override PartName="/word/document.xml" ContentType="text/xml"/>
</Types>"#;

        let ct = ContentTypes::parse(xml).unwrap();
        // OPC part names are case-insensitive
        assert_eq!(ct.content_type("/Word/Document.XML"), Some("text/xml"));
    }

    #[test]
    fn content_type_not_found() {
        let xml = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="xml" ContentType="application/xml"/>
</Types>"#;

        let ct = ContentTypes::parse(xml).unwrap();
        assert_eq!(ct.content_type("/file.png"), None);
    }

    #[test]
    fn override_takes_precedence_over_default() {
        let xml = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/special.xml" ContentType="text/special"/>
</Types>"#;

        let ct = ContentTypes::parse(xml).unwrap();
        // Override wins
        assert_eq!(ct.content_type("/special.xml"), Some("text/special"));
        // Default for other .xml files
        assert_eq!(ct.content_type("/other.xml"), Some("application/xml"));
    }
}

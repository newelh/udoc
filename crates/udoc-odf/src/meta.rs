//! ODF meta.xml parser for Dublin Core metadata.
//!
//! Extracts dc:title, dc:creator, dc:date, meta:creation-date, meta:keyword,
//! and meta:document-statistic from the meta.xml file.

use std::sync::Arc;

use udoc_containers::xml::namespace::ns;
use udoc_containers::xml::{attr_value, XmlEvent, XmlReader};
use udoc_core::backend::DocumentMetadata;

use crate::error::{Result, ResultExt};

/// Maximum number of keywords to collect (safety limit).
const MAX_KEYWORDS: usize = 1_000;

/// Parse meta.xml and return DocumentMetadata.
pub(crate) fn parse_meta(data: &[u8]) -> Result<DocumentMetadata> {
    let mut reader = XmlReader::new(data).context("initializing XML parser for meta.xml")?;

    let mut meta = DocumentMetadata::default();
    let mut current_element: Option<String> = None;
    let mut current_ns: Option<Arc<str>> = None;
    let mut text_buf = String::new();
    let mut keywords: Vec<String> = Vec::new();
    // Track depth relative to office:meta so nested elements are harmless.
    // 0 = inside office:meta, 1 = direct child, 2+ = nested inside a child.
    let mut meta_depth: usize = 0;
    let mut in_meta = false;

    loop {
        let event = reader.next_event().context("parsing meta.xml")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                let name = local_name.as_ref();

                if ns_str == ns::OFFICE && name == "meta" {
                    in_meta = true;
                    meta_depth = 0;
                    continue;
                }

                if in_meta {
                    meta_depth += 1;
                }

                // meta:document-statistic is a self-contained element with attributes.
                if ns_str == ns::META && name == "document-statistic" {
                    if let Some(pages) = attr_value(&attributes, "page-count") {
                        if let Ok(n) = pages.parse::<usize>() {
                            meta.page_count = n;
                        }
                    }
                }

                // Only track direct children of office:meta (meta_depth == 1).
                if in_meta && meta_depth == 1 {
                    current_element = Some(name.to_string());
                    current_ns = namespace_uri;
                    text_buf.clear();
                }
            }
            XmlEvent::Text(text) | XmlEvent::CData(text) => {
                // Only capture text at meta_depth 1 (direct children of office:meta).
                if in_meta && meta_depth == 1 {
                    text_buf.push_str(text.as_ref());
                }
            }
            XmlEvent::EndElement {
                local_name,
                namespace_uri,
                ..
            } => {
                let ns_str = namespace_uri.as_deref().unwrap_or("");

                if ns_str == ns::OFFICE && local_name.as_ref() == "meta" {
                    in_meta = false;
                    continue;
                }

                if in_meta {
                    // Harvest text when closing a direct child (meta_depth goes from 1 to 0).
                    if meta_depth == 1 {
                        if let (Some(ref name), Some(ref ns_str)) = (&current_element, &current_ns)
                        {
                            let val = text_buf.trim().to_string();
                            if !val.is_empty() {
                                match (ns_str.as_ref(), name.as_str()) {
                                    (ns::DC_ELEMENTS, "title") => {
                                        meta.title = Some(val);
                                    }
                                    (ns::DC_ELEMENTS, "creator") => {
                                        meta.author = Some(val.clone());
                                        meta.creator = Some(val);
                                    }
                                    (ns::DC_ELEMENTS, "date") => {
                                        meta.modification_date = Some(val);
                                    }
                                    (ns::DC_ELEMENTS, "subject") => {
                                        meta.subject = Some(val);
                                    }
                                    (ns::META, "creation-date") => {
                                        meta.creation_date = Some(val);
                                    }
                                    (ns::META, "keyword") if keywords.len() < MAX_KEYWORDS => {
                                        keywords.push(val);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        current_element = None;
                        current_ns = None;
                        text_buf.clear();
                    }
                    meta_depth = meta_depth.saturating_sub(1);
                }
            }
            XmlEvent::Eof => break,
        }
    }

    if !keywords.is_empty() {
        meta.properties
            .insert("keywords".to_string(), keywords.join(", "));
    }

    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_metadata() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-meta
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0">
  <office:meta>
    <dc:title>Test Document</dc:title>
    <dc:creator>Alice</dc:creator>
    <dc:date>2025-06-02T15:30:00</dc:date>
    <dc:subject>Testing</dc:subject>
    <meta:creation-date>2025-06-01T09:00:00</meta:creation-date>
    <meta:keyword>test</meta:keyword>
    <meta:keyword>odf</meta:keyword>
    <meta:document-statistic meta:page-count="5"/>
  </office:meta>
</office:document-meta>"#;

        let meta = parse_meta(xml).unwrap();
        assert_eq!(meta.title.as_deref(), Some("Test Document"));
        assert_eq!(meta.author.as_deref(), Some("Alice"));
        assert_eq!(meta.creator.as_deref(), Some("Alice"));
        assert_eq!(
            meta.modification_date.as_deref(),
            Some("2025-06-02T15:30:00")
        );
        assert_eq!(meta.subject.as_deref(), Some("Testing"));
        assert_eq!(meta.creation_date.as_deref(), Some("2025-06-01T09:00:00"));
        assert_eq!(meta.page_count, 5);
        assert_eq!(
            meta.properties.get("keywords").map(|s| s.as_str()),
            Some("test, odf")
        );
    }

    #[test]
    fn parse_empty_metadata() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-meta
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0">
  <office:meta/>
</office:document-meta>"#;

        let meta = parse_meta(xml).unwrap();
        assert!(meta.title.is_none());
        assert!(meta.author.is_none());
        assert_eq!(meta.page_count, 0);
    }

    #[test]
    fn parse_missing_fields() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-meta
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:dc="http://purl.org/dc/elements/1.1/">
  <office:meta>
    <dc:title>Only Title</dc:title>
  </office:meta>
</office:document-meta>"#;

        let meta = parse_meta(xml).unwrap();
        assert_eq!(meta.title.as_deref(), Some("Only Title"));
        assert!(meta.author.is_none());
        assert!(meta.creation_date.is_none());
    }
}

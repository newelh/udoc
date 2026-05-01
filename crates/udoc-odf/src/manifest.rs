//! META-INF/manifest.xml parser for ODF packages.
//!
//! The manifest lists every file in the ODF package along with its media type.
//! The root entry (full-path="/") declares the document's overall type.

use udoc_containers::xml::{attr_value, XmlEvent, XmlReader};

use crate::error::{Result, ResultExt};

/// MIME type for ODF text documents (.odt).
pub(crate) const ODF_TEXT_MIME: &str = "application/vnd.oasis.opendocument.text";
/// MIME type for ODF spreadsheet documents (.ods).
pub(crate) const ODF_SPREADSHEET_MIME: &str = "application/vnd.oasis.opendocument.spreadsheet";
/// MIME type for ODF presentation documents (.odp).
pub(crate) const ODF_PRESENTATION_MIME: &str = "application/vnd.oasis.opendocument.presentation";

/// Detected ODF document subformat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OdfType {
    Text,
    Spreadsheet,
    Presentation,
}

/// Parsed manifest information.
#[derive(Debug)]
pub(crate) struct ManifestInfo {
    /// Detected subformat from the root entry's media type.
    pub doc_type: Option<OdfType>,
}

/// Parse META-INF/manifest.xml to extract the document type and file entries.
pub(crate) fn parse_manifest(data: &[u8]) -> Result<ManifestInfo> {
    let mut reader = XmlReader::new(data).context("initializing XML parser for manifest.xml")?;

    let mut doc_type = None;

    loop {
        let event = reader.next_element().context("parsing manifest.xml")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                attributes,
                ..
            } if local_name.as_ref() == "file-entry" => {
                let full_path = attr_value(&attributes, "full-path").unwrap_or("");
                let media_type = attr_value(&attributes, "media-type").unwrap_or("");

                // The root entry declares the overall document type.
                if full_path == "/" {
                    doc_type = detect_type_from_mime(media_type);
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(ManifestInfo { doc_type })
}

/// Detect ODF subformat from MIME type string.
fn detect_type_from_mime(mime: &str) -> Option<OdfType> {
    match mime {
        ODF_TEXT_MIME => Some(OdfType::Text),
        ODF_SPREADSHEET_MIME => Some(OdfType::Spreadsheet),
        ODF_PRESENTATION_MIME => Some(OdfType::Presentation),
        _ => None,
    }
}

/// Detect ODF subformat from the mimetype file content (the first file in the ZIP).
pub(crate) fn detect_type_from_mimetype_file(content: &str) -> Option<OdfType> {
    detect_type_from_mime(content.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_manifest() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0">
  <manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.text"/>
  <manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/>
  <manifest:file-entry manifest:full-path="styles.xml" manifest:media-type="text/xml"/>
  <manifest:file-entry manifest:full-path="meta.xml" manifest:media-type="text/xml"/>
</manifest:manifest>"#;

        let info = parse_manifest(xml).unwrap();
        assert_eq!(info.doc_type, Some(OdfType::Text));
    }

    #[test]
    fn parse_spreadsheet_manifest() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0">
  <manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.spreadsheet"/>
</manifest:manifest>"#;

        let info = parse_manifest(xml).unwrap();
        assert_eq!(info.doc_type, Some(OdfType::Spreadsheet));
    }

    #[test]
    fn parse_presentation_manifest() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0">
  <manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.presentation"/>
</manifest:manifest>"#;

        let info = parse_manifest(xml).unwrap();
        assert_eq!(info.doc_type, Some(OdfType::Presentation));
    }

    #[test]
    fn missing_root_entry() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0">
  <manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/>
</manifest:manifest>"#;

        let info = parse_manifest(xml).unwrap();
        assert_eq!(info.doc_type, None);
    }

    #[test]
    fn mimetype_file_detection() {
        assert_eq!(
            detect_type_from_mimetype_file("application/vnd.oasis.opendocument.text"),
            Some(OdfType::Text)
        );
        assert_eq!(
            detect_type_from_mimetype_file("application/vnd.oasis.opendocument.spreadsheet\n"),
            Some(OdfType::Spreadsheet)
        );
        assert_eq!(
            detect_type_from_mimetype_file("application/vnd.oasis.opendocument.presentation"),
            Some(OdfType::Presentation)
        );
        assert_eq!(detect_type_from_mimetype_file("text/plain"), None);
    }
}

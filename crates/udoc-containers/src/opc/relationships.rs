//! OPC relationship (.rels) file parser.
//!
//! Parses `_rels/.rels` (package-level) and per-part `.rels` files.
//! Each relationship has an Id, Type URI, Target URI, and optional TargetMode.

use std::sync::Arc;

use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use crate::error::Result;
use crate::xml::{attr_value, XmlEvent, XmlReader};

/// Target mode for a relationship.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetMode {
    /// Target is a part within the package.
    Internal,
    /// Target is an external resource (URL).
    External,
}

/// A single OPC relationship.
#[derive(Debug, Clone)]
pub struct Relationship {
    /// Relationship identifier (e.g., "rId1").
    pub id: String,
    /// Relationship type URI.
    pub rel_type: String,
    /// Target URI (relative for internal, absolute for external).
    pub target: String,
    /// Whether the target is internal or external.
    pub target_mode: TargetMode,
}

/// Maximum relationships parsed from a single `.rels` file
/// (SEC-ALLOC-CLAMP #62, CFB-F1).
///
/// The XML parser already caps element nesting depth and attribute
/// count per element, but nothing prevented a malicious `.rels` XML
/// with millions of sibling `<Relationship>` elements at depth 1 --
/// each allocating id/rel_type/target strings (~200 bytes each).
/// 10K is ~2 orders of magnitude above real-world OOXML (typical
/// document.xml.rels has <50 relationships).
const MAX_RELATIONSHIPS_PER_RELS: usize = 10_000;

/// Parse a `.rels` XML file into a list of relationships.
pub(crate) fn parse_rels(
    data: &[u8],
    diag: &Arc<dyn DiagnosticsSink>,
) -> Result<Vec<Relationship>> {
    let mut reader = XmlReader::new(data)?;
    let mut rels = Vec::new();
    let mut truncated = false;

    loop {
        match reader.next_element()? {
            XmlEvent::StartElement {
                local_name,
                attributes,
                ..
            } if local_name == "Relationship" => {
                if rels.len() >= MAX_RELATIONSHIPS_PER_RELS {
                    if !truncated {
                        diag.warning(Warning::new(
                            "OpcRelationshipLimit",
                            format!(
                                ".rels parse: {MAX_RELATIONSHIPS_PER_RELS} relationship cap hit, \
                                 further entries ignored"
                            ),
                        ));
                        truncated = true;
                    }
                    continue;
                }
                let id = match attr_value(&attributes, "Id") {
                    Some(v) if !v.is_empty() => v.to_string(),
                    _ => {
                        diag.warning(Warning::new(
                            "OpcMalformedRelationship",
                            "Relationship element missing or empty Id attribute",
                        ));
                        continue;
                    }
                };
                let rel_type = match attr_value(&attributes, "Type") {
                    Some(v) if !v.is_empty() => v.to_string(),
                    _ => {
                        diag.warning(Warning::new(
                            "OpcMalformedRelationship",
                            format!("Relationship {id} missing or empty Type attribute"),
                        ));
                        continue;
                    }
                };
                let target = match attr_value(&attributes, "Target") {
                    Some(v) => v.to_string(),
                    None => {
                        diag.warning(Warning::new(
                            "OpcMalformedRelationship",
                            format!("Relationship {id} missing Target attribute"),
                        ));
                        continue;
                    }
                };
                let target_mode = match attr_value(&attributes, "TargetMode") {
                    Some(m) if m.eq_ignore_ascii_case("External") => TargetMode::External,
                    _ => TargetMode::Internal,
                };

                rels.push(Relationship {
                    id,
                    rel_type,
                    target,
                    target_mode,
                });
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(rels)
}

/// Well-known OPC relationship type URIs (Transitional namespace).
///
/// Backends use these constants with `find_package_rel_by_type` /
/// `find_part_rel_by_type`, which automatically match both Transitional
/// and Strict OOXML URIs (see [`rel_type_matches`]).
pub mod rel_types {
    /// Main document part (DOCX).
    pub const OFFICE_DOCUMENT: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
    /// Shared strings (XLSX).
    pub const SHARED_STRINGS: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings";
    /// Styles.
    pub const STYLES: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles";
    /// Numbering definitions (DOCX).
    pub const NUMBERING: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";
    /// Worksheet (XLSX).
    pub const WORKSHEET: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet";
    /// Slide master (PPTX).
    pub const SLIDE_MASTER: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster";
    /// Slide (PPTX).
    pub const SLIDE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide";
    /// Theme.
    pub const THEME: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme";
    /// Image.
    pub const IMAGE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
    /// Hyperlink (external).
    pub const HYPERLINK: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";
    /// Core properties.
    pub const CORE_PROPERTIES: &str =
        "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties";
    /// Document settings (DOCX).
    pub const DOCUMENT_SETTINGS: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings";
    /// Font table (DOCX).
    pub const FONT_TABLE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/fontTable";
    /// Header (DOCX).
    pub const HEADER: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
    /// Footer (DOCX).
    pub const FOOTER: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";
    /// Footnotes (DOCX).
    pub const FOOTNOTES: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes";
    /// Endnotes (DOCX).
    pub const ENDNOTES: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes";
    /// Comments (DOCX).
    pub const COMMENTS: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";
    /// Notes slide (PPTX).
    pub const NOTES_SLIDE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide";
}

/// Transitional and Strict OOXML use different URI prefixes for the same
/// relationship types. This function returns true if `actual` matches
/// `expected` in either namespace.
///
/// Transitional prefix: `http://schemas.openxmlformats.org/officeDocument/2006/relationships/`
/// Strict prefix: `http://purl.oclc.org/ooxml/officeDocument/relationships/`
///
/// Package-level core-properties uses a different Transitional prefix
/// (`http://schemas.openxmlformats.org/package/2006/relationships/`) but
/// the Strict form uses the same `purl.oclc.org` base.
pub fn rel_type_matches(actual: &str, expected: &str) -> bool {
    if actual == expected {
        return true;
    }

    const TRANSITIONAL_DOC: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/";
    const TRANSITIONAL_PKG: &str = "http://schemas.openxmlformats.org/package/2006/relationships/";
    const STRICT: &str = "http://purl.oclc.org/ooxml/officeDocument/relationships/";

    // Extract the suffix from `expected`, then check if `actual` has the
    // same suffix under the other namespace prefix. Zero-alloc: uses
    // strip_prefix on both sides instead of building a temporary String.
    if let Some(suffix) = expected.strip_prefix(TRANSITIONAL_DOC) {
        actual.strip_prefix(STRICT) == Some(suffix)
    } else if let Some(suffix) = expected.strip_prefix(TRANSITIONAL_PKG) {
        actual.strip_prefix(STRICT) == Some(suffix)
    } else if let Some(suffix) = expected.strip_prefix(STRICT) {
        actual.strip_prefix(TRANSITIONAL_DOC) == Some(suffix)
            || actual.strip_prefix(TRANSITIONAL_PKG) == Some(suffix)
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use udoc_core::diagnostics::{CollectingDiagnostics, NullDiagnostics};

    use super::*;

    fn null_diag() -> Arc<dyn DiagnosticsSink> {
        Arc::new(NullDiagnostics)
    }

    #[test]
    fn parse_package_rels() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
        Target="word/document.xml"/>
    <Relationship Id="rId2"
        Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties"
        Target="docProps/core.xml"/>
</Relationships>"#;

        let rels = parse_rels(xml, &null_diag()).unwrap();
        assert_eq!(rels.len(), 2);
        assert_eq!(rels[0].id, "rId1");
        assert_eq!(rels[0].rel_type, rel_types::OFFICE_DOCUMENT);
        assert_eq!(rels[0].target, "word/document.xml");
        assert_eq!(rels[0].target_mode, TargetMode::Internal);
    }

    #[test]
    fn parse_external_target() {
        let xml = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink"
        Target="https://example.com" TargetMode="External"/>
</Relationships>"#;

        let rels = parse_rels(xml, &null_diag()).unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].target_mode, TargetMode::External);
        assert_eq!(rels[0].target, "https://example.com");
    }

    #[test]
    fn parse_per_part_rels() {
        let xml = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles"
        Target="styles.xml"/>
    <Relationship Id="rId2"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering"
        Target="numbering.xml"/>
</Relationships>"#;

        let rels = parse_rels(xml, &null_diag()).unwrap();
        assert_eq!(rels.len(), 2);
        assert_eq!(rels[0].rel_type, rel_types::STYLES);
        assert_eq!(rels[1].rel_type, rel_types::NUMBERING);
    }

    #[test]
    fn empty_rels() {
        let xml = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;

        let rels = parse_rels(xml, &null_diag()).unwrap();
        assert!(rels.is_empty());
    }

    #[test]
    fn malformed_rels_missing_attrs_skipped_with_warnings() {
        let xml = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1" Type="http://example.com/type" Target="good.xml"/>
    <Relationship Type="http://example.com/type" Target="no_id.xml"/>
    <Relationship Id="" Type="http://example.com/type" Target="empty_id.xml"/>
    <Relationship Id="rId4" Target="no_type.xml"/>
    <Relationship Id="rId5" Type="http://example.com/type"/>
    <Relationship Id="rId6" Type="http://example.com/type" Target=""/>
</Relationships>"#;

        let collecting = Arc::new(CollectingDiagnostics::new());
        let diag: Arc<dyn DiagnosticsSink> = collecting.clone();
        let rels = parse_rels(xml, &diag).unwrap();
        // rId1 (valid) and rId6 (empty Target is allowed) survive.
        // Missing Id, empty Id, missing Type, and missing Target are all skipped.
        assert_eq!(rels.len(), 2);
        assert_eq!(rels[0].id, "rId1");
        assert_eq!(rels[1].id, "rId6");
        assert_eq!(rels[1].target, "");

        // 4 malformed entries should produce 4 warnings
        let warnings = collecting.warnings();
        let mal_count = warnings
            .iter()
            .filter(|w| w.kind == "OpcMalformedRelationship")
            .count();
        assert_eq!(
            mal_count, 4,
            "expected 4 malformed relationship warnings, got {mal_count}: {warnings:?}"
        );
    }

    #[test]
    fn duplicate_relationship_ids_both_kept() {
        // OPC doesn't strictly forbid duplicate IDs in the wild. Some producers
        // emit duplicates. We keep all entries and let the consumer decide.
        let xml = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1" Type="http://example.com/type1" Target="a.xml"/>
    <Relationship Id="rId1" Type="http://example.com/type2" Target="b.xml"/>
</Relationships>"#;

        let rels = parse_rels(xml, &null_diag()).unwrap();
        assert_eq!(rels.len(), 2, "both duplicate-ID entries should be kept");
        assert_eq!(rels[0].target, "a.xml");
        assert_eq!(rels[1].target, "b.xml");
    }

    #[test]
    fn default_target_mode_is_internal() {
        let xml = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1" Type="http://example.com/type" Target="part.xml"/>
</Relationships>"#;

        let rels = parse_rels(xml, &null_diag()).unwrap();
        assert_eq!(rels[0].target_mode, TargetMode::Internal);
    }

    #[test]
    fn rel_type_matches_exact() {
        assert!(rel_type_matches(
            rel_types::OFFICE_DOCUMENT,
            rel_types::OFFICE_DOCUMENT
        ));
        assert!(!rel_type_matches(
            rel_types::OFFICE_DOCUMENT,
            rel_types::STYLES
        ));
    }

    #[test]
    fn rel_type_matches_strict_to_transitional() {
        // Strict URI should match when looking for Transitional constant
        let strict_office_doc =
            "http://purl.oclc.org/ooxml/officeDocument/relationships/officeDocument";
        assert!(rel_type_matches(
            strict_office_doc,
            rel_types::OFFICE_DOCUMENT
        ));

        let strict_styles = "http://purl.oclc.org/ooxml/officeDocument/relationships/styles";
        assert!(rel_type_matches(strict_styles, rel_types::STYLES));
    }

    #[test]
    fn rel_type_matches_strict_core_properties() {
        // Core properties uses a different Transitional prefix (package/ not officeDocument/)
        let strict_core =
            "http://purl.oclc.org/ooxml/officeDocument/relationships/metadata/core-properties";
        assert!(rel_type_matches(strict_core, rel_types::CORE_PROPERTIES));
    }

    #[test]
    fn rel_type_matches_unrelated_uris() {
        assert!(!rel_type_matches(
            "http://example.com/foo",
            rel_types::OFFICE_DOCUMENT
        ));
        assert!(!rel_type_matches("", rel_types::OFFICE_DOCUMENT));
    }
}

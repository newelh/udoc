//! OPC (Open Packaging Conventions) package navigator.
//!
//! Provides Content_Types.xml parsing, relationship resolution, and
//! part URI navigation over a ZIP-backed OOXML package.
//!
//! # Usage
//!
//! ```no_run
//! use std::sync::Arc;
//! use udoc_core::diagnostics::NullDiagnostics;
//! use udoc_containers::opc::OpcPackage;
//!
//! let data = std::fs::read("document.docx").unwrap();
//! let pkg = OpcPackage::new(&data, Arc::new(NullDiagnostics)).unwrap();
//!
//! // Find the main document part via package relationships
//! let doc_rel = pkg.find_package_rel_by_type(
//!     udoc_containers::opc::rel_types::OFFICE_DOCUMENT,
//! ).unwrap();
//! let doc_xml = pkg.read_part_string(&doc_rel.target).unwrap();
//! ```

mod content_types;
pub mod metadata;
pub mod parts;
pub mod relationships;

pub use relationships::rel_types;
pub use relationships::{Relationship, TargetMode};

use std::collections::HashMap;
use std::sync::Arc;

use udoc_core::diagnostics::DiagnosticsSink;

use crate::error::{Error, Result, ResultExt};
use crate::zip::ZipArchive;

use self::content_types::ContentTypes;
use self::relationships::parse_rels;

/// Well-known OPC package paths.
pub mod paths {
    /// Content types declaration (required in every OPC package).
    pub const CONTENT_TYPES: &str = "[Content_Types].xml";
    /// Package-level relationships file.
    pub const PACKAGE_RELS: &str = "_rels/.rels";
    /// Dublin Core metadata part (conventional location).
    pub const CORE_PROPERTIES: &str = "docProps/core.xml";
}

/// An OPC package backed by a ZIP archive.
///
/// Provides content type lookup, relationship navigation, and part reading.
/// All `.rels` files are eagerly parsed during construction so that
/// `part_rels()` can take `&self` instead of `&mut self`.
///
/// **Performance note:** Eager `.rels` parsing means construction cost scales
/// with the number of `.rels` files in the archive. For typical OOXML documents
/// (1-5 `.rels` files) this is negligible. Very large packages with hundreds of
/// parts (e.g., PPTX with many embedded images) will pay upfront. If this
/// becomes a bottleneck, switch to lazy parsing with interior mutability.
pub struct OpcPackage<'a> {
    zip: ZipArchive<'a>,
    content_types: ContentTypes,
    package_rels: Vec<Relationship>,
    /// Pre-parsed per-part relationships, keyed by part name (lowercase).
    part_rels_map: HashMap<String, Vec<Relationship>>,
}

impl<'a> OpcPackage<'a> {
    /// Open an OPC package from raw bytes.
    ///
    /// Eagerly parses `[Content_Types].xml` and `_rels/.rels`.
    pub fn new(data: &'a [u8], diag: Arc<dyn DiagnosticsSink>) -> Result<Self> {
        let zip = ZipArchive::new(data, Arc::clone(&diag))?;

        // Parse [Content_Types].xml (required)
        let ct_entry = zip
            .find_ci(paths::CONTENT_TYPES)
            .ok_or_else(|| Error::opc("missing [Content_Types].xml"))?;
        let ct_data = zip.read(ct_entry).context("reading [Content_Types].xml")?;
        let content_types = ContentTypes::parse(&ct_data).context("parsing [Content_Types].xml")?;

        // Parse package-level relationships (optional but expected)
        let package_rels = match zip
            .find(paths::PACKAGE_RELS)
            .or_else(|| zip.find_ci(paths::PACKAGE_RELS))
        {
            Some(entry) => {
                let rels_data = zip.read(entry).context("reading _rels/.rels")?;
                parse_rels(&rels_data, &diag).context("parsing _rels/.rels")?
            }
            None => Vec::new(),
        };

        // Eagerly parse all per-part .rels files so part_rels() can take &self.
        let mut part_rels_map = HashMap::new();
        for entry in zip.entries() {
            let is_rels = entry.name.ends_with(".rels")
                && entry.name != "_rels/.rels"
                && (entry.name.contains("/_rels/") || entry.name.starts_with("_rels/"));
            if is_rels {
                let source = rels_source_part(&entry.name);
                let data = zip.read(entry).context(format!("reading {}", entry.name))?;
                let rels = parse_rels(&data, &diag).context(format!("parsing {}", entry.name))?;
                part_rels_map.insert(source, rels);
            }
        }

        Ok(Self {
            zip,
            content_types,
            package_rels,
            part_rels_map,
        })
    }

    /// Get the content type for a part name.
    pub fn content_type(&self, part_name: &str) -> Option<&str> {
        self.content_types.content_type(part_name)
    }

    /// Get the package-level relationships.
    pub fn package_rels(&self) -> &[Relationship] {
        &self.package_rels
    }

    /// Find a package-level relationship by its type URI.
    ///
    /// Returns the first match. Use [`Self::find_all_package_rels_by_type`] when
    /// multiple relationships of the same type exist (e.g., PPTX slides).
    pub fn find_package_rel_by_type(&self, rel_type: &str) -> Option<&Relationship> {
        self.package_rels
            .iter()
            .find(|r| relationships::rel_type_matches(&r.rel_type, rel_type))
    }

    /// Find all package-level relationships matching a type URI.
    ///
    /// Useful for formats with multiple relationships of the same type,
    /// e.g., PPTX slides, XLSX worksheets, DOCX headers/footers.
    pub fn find_all_package_rels_by_type(&self, rel_type: &str) -> Vec<&Relationship> {
        self.package_rels
            .iter()
            .filter(|r| relationships::rel_type_matches(&r.rel_type, rel_type))
            .collect()
    }

    /// Get relationships for a specific part.
    ///
    /// All `.rels` files are eagerly parsed during package construction,
    /// so this lookup is O(1).
    ///
    /// Returns an empty slice if no `.rels` file exists for this part.
    pub fn part_rels(&self, source_part: &str) -> &[Relationship] {
        let key = source_part.trim_start_matches('/').to_ascii_lowercase();
        self.part_rels_map
            .get(&key)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Find a per-part relationship by type.
    ///
    /// Returns the first match. Use [`Self::find_all_part_rels_by_type`] when
    /// multiple relationships of the same type exist.
    pub fn find_part_rel_by_type(
        &self,
        source_part: &str,
        rel_type: &str,
    ) -> Option<&Relationship> {
        self.part_rels(source_part)
            .iter()
            .find(|r| relationships::rel_type_matches(&r.rel_type, rel_type))
    }

    /// Find all per-part relationships matching a type URI.
    pub fn find_all_part_rels_by_type(
        &self,
        source_part: &str,
        rel_type: &str,
    ) -> Vec<&Relationship> {
        self.part_rels(source_part)
            .iter()
            .filter(|r| relationships::rel_type_matches(&r.rel_type, rel_type))
            .collect()
    }

    /// Read a part's raw bytes by part name.
    ///
    /// The part name can be with or without a leading `/`.
    pub fn read_part(&self, part_name: &str) -> Result<Vec<u8>> {
        let entry = self.find_part_entry(part_name)?;
        self.zip
            .read(entry)
            .context(format!("reading part {part_name}"))
    }

    /// Read a part as a UTF-8 string.
    pub fn read_part_string(&self, part_name: &str) -> Result<String> {
        let entry = self.find_part_entry(part_name)?;
        self.zip
            .read_string(entry)
            .context(format!("reading part {part_name} as string"))
    }

    /// Look up a ZIP entry by part name (case-insensitive fallback).
    fn find_part_entry(&self, part_name: &str) -> Result<&crate::zip::ZipEntry> {
        let name = part_name.trim_start_matches('/');
        self.zip
            .find(name)
            .or_else(|| self.zip.find_ci(name))
            .ok_or_else(|| Error::opc(format!("part not found: {part_name}")))
    }

    /// Resolve a relative URI from a source part.
    pub fn resolve_uri(&self, source_part: &str, target: &str) -> String {
        parts::resolve_uri(source_part, target)
    }
}

/// Derive the source part name from a `.rels` file path.
///
/// For `word/_rels/document.xml.rels`, returns `word/document.xml` (lowercase).
/// For `_rels/something.rels`, returns `something` (lowercase).
///
/// If the path doesn't follow the expected `_rels/` pattern (malformed package),
/// falls through to returning the path as-is (lowercase). This should not happen
/// in well-formed OPC packages since we only call this for paths matching the
/// `_rels/*.rels` filter in the constructor.
fn rels_source_part(rels_path: &str) -> String {
    // Strip the `_rels/` directory and `.rels` suffix to recover the source part.
    // e.g. "word/_rels/document.xml.rels" -> "word/document.xml"
    let path = rels_path.trim_start_matches('/');

    // Find the `_rels/` segment
    if let Some(rels_dir_pos) = path.find("/_rels/") {
        let dir = &path[..rels_dir_pos];
        let file_with_rels = &path[rels_dir_pos + 7..]; // skip "/_rels/"
        let file = file_with_rels
            .strip_suffix(".rels")
            .unwrap_or(file_with_rels);
        format!("{dir}/{file}").to_ascii_lowercase()
    } else if let Some(file_with_rels) = path.strip_prefix("_rels/") {
        // Root-level rels (e.g., "_rels/[Content_Types].xml.rels")
        let file = file_with_rels
            .strip_suffix(".rels")
            .unwrap_or(file_with_rels);
        file.to_ascii_lowercase()
    } else {
        // Malformed: no _rels/ segment found. Return path as-is.
        path.to_ascii_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use udoc_core::diagnostics::NullDiagnostics;

    use super::*;

    use crate::test_util::build_stored_zip;

    /// Build a minimal DOCX-like ZIP for testing.
    fn make_docx_zip() -> Vec<u8> {
        let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

        let package_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
        Target="word/document.xml"/>
</Relationships>"#;

        let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body><w:p><w:r><w:t>Hello World</w:t></w:r></w:p></w:body>
</w:document>"#;

        let document_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles"
        Target="styles.xml"/>
</Relationships>"#;

        let styles_xml =
            b"<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"/>";

        build_stored_zip(&[
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", package_rels),
            ("word/document.xml", document_xml),
            ("word/_rels/document.xml.rels", document_rels),
            ("word/styles.xml", styles_xml),
        ])
    }

    #[test]
    fn rels_source_part_standard() {
        assert_eq!(
            rels_source_part("word/_rels/document.xml.rels"),
            "word/document.xml"
        );
    }

    #[test]
    fn rels_source_part_root_level() {
        // "_rels/.rels" -> strips "_rels/" prefix -> ".rels" -> strips ".rels" suffix -> ""
        // This is the package-level rels file, which is filtered out in the constructor
        // (entry.name != "_rels/.rels"), so this path is never actually reached.
        assert_eq!(rels_source_part("_rels/.rels"), "");
    }

    #[test]
    fn rels_source_part_root_level_non_package() {
        // Root-level per-part rels file (not _rels/.rels).
        // e.g., "_rels/[Content_Types].xml.rels" -> "[content_types].xml"
        assert_eq!(
            rels_source_part("_rels/[Content_Types].xml.rels"),
            "[content_types].xml"
        );
    }

    #[test]
    fn rels_source_part_malformed_passthrough() {
        // Malformed path with no _rels/ segment: returns as-is (lowercase)
        assert_eq!(rels_source_part("weird/path.rels"), "weird/path.rels");
    }

    #[test]
    fn open_docx_package() {
        let zip = make_docx_zip();
        let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();
        assert!(!pkg.package_rels().is_empty());
    }

    #[test]
    fn find_office_document_rel() {
        let zip = make_docx_zip();
        let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();
        let rel = pkg
            .find_package_rel_by_type(rel_types::OFFICE_DOCUMENT)
            .expect("should find officeDocument relationship");
        assert_eq!(rel.target, "word/document.xml");
        assert_eq!(rel.target_mode, TargetMode::Internal);
    }

    #[test]
    fn content_type_lookup() {
        let zip = make_docx_zip();
        let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();

        // Override
        assert_eq!(
            pkg.content_type("/word/document.xml"),
            Some(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"
            )
        );

        // Default by extension
        assert_eq!(
            pkg.content_type("/_rels/.rels"),
            Some("application/vnd.openxmlformats-package.relationships+xml")
        );
    }

    #[test]
    fn read_part_string() {
        let zip = make_docx_zip();
        let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();
        let doc = pkg.read_part_string("word/document.xml").unwrap();
        assert!(doc.contains("Hello World"));
    }

    #[test]
    fn read_part_with_leading_slash() {
        let zip = make_docx_zip();
        let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();
        let doc = pkg.read_part_string("/word/document.xml").unwrap();
        assert!(doc.contains("Hello World"));
    }

    #[test]
    fn per_part_relationships() {
        let zip = make_docx_zip();
        let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();
        let rels = pkg.part_rels("/word/document.xml");
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].rel_type, rel_types::STYLES);
        assert_eq!(rels[0].target, "styles.xml");
    }

    #[test]
    fn per_part_rels_idempotent() {
        let zip = make_docx_zip();
        let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();
        // Call twice; same data each time (eagerly parsed)
        let rels1 = pkg.part_rels("/word/document.xml");
        assert_eq!(rels1.len(), 1);
        let rels2 = pkg.part_rels("/word/document.xml");
        assert_eq!(rels2.len(), 1);
    }

    #[test]
    fn missing_rels_is_empty_not_error() {
        let zip = make_docx_zip();
        let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();
        // styles.xml has no .rels file
        let rels = pkg.part_rels("/word/styles.xml");
        assert!(rels.is_empty());
    }

    #[test]
    fn resolve_uri_from_document() {
        let zip = make_docx_zip();
        let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();
        let resolved = pkg.resolve_uri("/word/document.xml", "styles.xml");
        assert_eq!(resolved, "/word/styles.xml");
    }

    #[test]
    fn missing_part_is_error() {
        let zip = make_docx_zip();
        let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();
        let result = pkg.read_part("nonexistent.xml");
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("not found"), "got: {err}");
    }

    #[test]
    fn missing_content_types_is_error() {
        // ZIP with no [Content_Types].xml
        let zip = build_stored_zip(&[("_rels/.rels", b"<Relationships/>")]);
        let result = OpcPackage::new(&zip, Arc::new(NullDiagnostics));
        assert!(result.is_err());
        let err = result.err().expect("should be an error");
        let msg = format!("{err}");
        assert!(msg.contains("Content_Types"), "got: {msg}");
    }

    #[test]
    fn navigate_full_docx_path() {
        let zip = make_docx_zip();
        let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();

        // 1. Find main document via package rels
        let doc_rel = pkg
            .find_package_rel_by_type(rel_types::OFFICE_DOCUMENT)
            .unwrap();
        let doc_target = &doc_rel.target;

        // 2. Read the document
        let doc = pkg.read_part_string(doc_target).unwrap();
        assert!(doc.contains("Hello World"));

        // 3. Find styles via per-part rels (no &mut needed)
        let doc_part = format!("/{doc_target}");
        let styles_rel = pkg
            .find_part_rel_by_type(&doc_part, rel_types::STYLES)
            .expect("should find styles rel");

        // 4. Resolve styles URI relative to document
        let styles_uri = parts::resolve_uri(&doc_part, &styles_rel.target);
        let styles = pkg.read_part_string(&styles_uri).unwrap();
        assert!(styles.contains("styles"));
    }

    #[test]
    fn find_all_rels_by_type() {
        let zip = make_docx_zip();
        let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics)).unwrap();

        // Should find exactly one officeDocument rel
        let doc_rels = pkg.find_all_package_rels_by_type(rel_types::OFFICE_DOCUMENT);
        assert_eq!(doc_rels.len(), 1);

        // Non-existent type returns empty
        let none = pkg.find_all_package_rels_by_type("http://nonexistent");
        assert!(none.is_empty());
    }

    /// Round-2 audit finding OOXML-F2 (CVSS 7.5) flagged OPC relationship
    /// cycles as a potential infinite-recursion DoS. This test proves the
    /// design is safe: `OpcPackage::new` does a single linear scan of ZIP
    /// entries and parses each `.rels` into a flat `HashMap<part, Vec<Rel>>`.
    /// It never follows relationship targets during construction, so a cycle
    /// in the relationship graph cannot loop the parser.
    ///
    /// The malicious package below has two parts whose `.rels` files point
    /// at each other (a -> b -> a -> b ...). Construction must terminate
    /// in O(parts), and the resulting map must contain both relationship
    /// vectors verbatim without any traversal.
    #[test]
    fn relationship_cycle_does_not_loop_parser() {
        let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

        let package_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
        Target="word/document.xml"/>
</Relationships>"#;

        // a.xml -> b.xml
        let a_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1" Type="http://x/cycle" Target="b.xml"/>
</Relationships>"#;

        // b.xml -> a.xml (cycle)
        let b_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1" Type="http://x/cycle" Target="a.xml"/>
</Relationships>"#;

        let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body/>
</w:document>"#;

        let zip = build_stored_zip(&[
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", package_rels),
            ("word/document.xml", document_xml),
            ("a.xml", b"<a/>"),
            ("b.xml", b"<b/>"),
            ("_rels/a.xml.rels", a_rels),
            ("_rels/b.xml.rels", b_rels),
        ]);

        // Construction must terminate; the parser does not follow targets.
        let pkg = OpcPackage::new(&zip, Arc::new(NullDiagnostics))
            .expect("opc package with cyclic relationship targets must parse");

        // Both rels are stored as flat data; targets are NOT chased.
        let a_rels_parsed = pkg.part_rels("/a.xml");
        assert_eq!(a_rels_parsed.len(), 1);
        assert_eq!(a_rels_parsed[0].target, "b.xml");

        let b_rels_parsed = pkg.part_rels("/b.xml");
        assert_eq!(b_rels_parsed.len(), 1);
        assert_eq!(b_rels_parsed[0].target, "a.xml");
    }
}

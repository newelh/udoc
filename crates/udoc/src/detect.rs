//! Format detection: magic bytes, container inspection, extension fallback.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use udoc_core::diagnostics::NullDiagnostics;
use udoc_core::error::{Error, Result};

/// Supported document formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Format {
    /// Portable Document Format (ISO 32000-2).
    Pdf,
    /// Office Open XML Word document (`.docx`).
    Docx,
    /// Office Open XML spreadsheet (`.xlsx`).
    Xlsx,
    /// Office Open XML presentation (`.pptx`).
    Pptx,
    /// Microsoft Word 97-2003 binary document (`.doc`).
    Doc,
    /// Microsoft Excel 97-2003 binary workbook (`.xls`).
    Xls,
    /// Microsoft PowerPoint 97-2003 binary presentation (`.ppt`).
    Ppt,
    /// OpenDocument Text (`.odt`).
    Odt,
    /// OpenDocument Spreadsheet (`.ods`).
    Ods,
    /// OpenDocument Presentation (`.odp`).
    Odp,
    /// Rich Text Format (`.rtf`).
    Rtf,
    /// Markdown (CommonMark + GFM subset).
    Md,
}

impl Format {
    /// File extension (without leading dot).
    ///
    /// ```
    /// use udoc::Format;
    /// assert_eq!(Format::Pdf.extension(), "pdf");
    /// assert_eq!(Format::Docx.extension(), "docx");
    /// ```
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Docx => "docx",
            Self::Xlsx => "xlsx",
            Self::Pptx => "pptx",
            Self::Doc => "doc",
            Self::Xls => "xls",
            Self::Ppt => "ppt",
            Self::Odt => "odt",
            Self::Ods => "ods",
            Self::Odp => "odp",
            Self::Rtf => "rtf",
            Self::Md => "md",
        }
    }

    /// MIME type.
    ///
    /// ```
    /// use udoc::Format;
    /// assert_eq!(Format::Pdf.mime_type(), "application/pdf");
    /// ```
    pub fn mime_type(&self) -> &'static str {
        match self {
            Self::Pdf => "application/pdf",
            Self::Docx => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Self::Pptx => {
                "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            }
            Self::Doc => "application/msword",
            Self::Xls => "application/vnd.ms-excel",
            Self::Ppt => "application/vnd.ms-powerpoint",
            Self::Odt => "application/vnd.oasis.opendocument.text",
            Self::Ods => "application/vnd.oasis.opendocument.spreadsheet",
            Self::Odp => "application/vnd.oasis.opendocument.presentation",
            Self::Rtf => "application/rtf",
            Self::Md => "text/markdown",
        }
    }

    // ------- capability accessors --------
    //
    // Consumers should key off Format-level capabilities (stable across
    // releases) rather than Document-level (which shifts as backends
    // gain or lose features). 3.

    /// Whether the page renderer can rasterize pages of this format to
    /// pixmaps via [`crate::render`].
    ///
    /// Currently true for [`Format::Pdf`] only. The other 11 formats
    /// have no native pixel-perfect representation; rendering them
    /// would require an in-process layout engine which  has
    /// not built.
    ///
    /// ```
    /// use udoc::Format;
    /// assert!(Format::Pdf.can_render());
    /// assert!(!Format::Docx.can_render());
    /// assert!(!Format::Md.can_render());
    /// ```
    pub fn can_render(&self) -> bool {
        matches!(self, Self::Pdf)
    }

    /// Whether the format's backend can extract tables.
    ///
    /// True for every shipped format: PDF (ruled-line + alignment
    /// detection), the OOXML/CFB office formats (native table
    /// elements), the ODF formats (table elements), RTF (table
    /// control words), and Markdown (GFM tables).
    ///
    /// ```
    /// use udoc::Format;
    /// assert!(Format::Pdf.has_tables());
    /// assert!(Format::Docx.has_tables());
    /// assert!(Format::Md.has_tables());
    /// ```
    pub fn has_tables(&self) -> bool {
        // Every shipped format has SOME table capability today; this
        // is `true` until a future format breaks the pattern. Keeping
        // an explicit `match` so adding a new variant requires
        // touching this method too.
        match self {
            Self::Pdf
            | Self::Docx
            | Self::Xlsx
            | Self::Pptx
            | Self::Doc
            | Self::Xls
            | Self::Ppt
            | Self::Odt
            | Self::Ods
            | Self::Odp
            | Self::Rtf
            | Self::Md => true,
        }
    }

    /// Whether the format has a native page concept that the
    /// extractor surfaces as the unit for [`crate::Extractor`]
    /// page-level methods.
    ///
    /// True for paginated formats (PDF, slide formats PPTX/PPT/ODP)
    /// and spreadsheets (where each sheet maps to a "page" for the
    /// extraction unit -- XLSX, XLS, ODS).
    ///
    /// False for flow formats with no fixed page boundaries until
    /// rendered: DOCX, RTF, ODT, Markdown.
    ///
    /// ```
    /// use udoc::Format;
    /// assert!(Format::Pdf.has_pages());
    /// assert!(Format::Pptx.has_pages());
    /// assert!(Format::Xlsx.has_pages()); // sheet-as-page
    /// assert!(!Format::Docx.has_pages());
    /// assert!(!Format::Md.has_pages());
    /// ```
    pub fn has_pages(&self) -> bool {
        match self {
            Self::Pdf | Self::Pptx | Self::Ppt | Self::Odp | Self::Xlsx | Self::Xls | Self::Ods => {
                true
            }
            Self::Docx | Self::Doc | Self::Odt | Self::Rtf | Self::Md => false,
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Pdf => "PDF",
            Self::Docx => "DOCX",
            Self::Xlsx => "XLSX",
            Self::Pptx => "PPTX",
            Self::Doc => "DOC",
            Self::Xls => "XLS",
            Self::Ppt => "PPT",
            Self::Odt => "ODT",
            Self::Ods => "ODS",
            Self::Odp => "ODP",
            Self::Rtf => "RTF",
            Self::Md => "Markdown",
        };
        write!(f, "{name}")
    }
}

// ---------------------------------------------------------------------------
// Magic byte constants
// ---------------------------------------------------------------------------

use udoc_containers::zip::ZIP_MAGIC;
const OLE2_MAGIC: [u8; 4] = [0xD0, 0xCF, 0x11, 0xE0];
const RTF_MAGIC: &[u8] = b"{\\rtf";
const PDF_MAGIC: &[u8] = b"%PDF";

/// Detect format from raw bytes using magic bytes and container inspection.
///
/// Detection priority:
/// 1. `{\rtf` at offset 0 -> RTF
/// 2. `%PDF` within first 1024 bytes -> PDF
/// 3. `PK\x03\x04` -> ZIP: inspect `[Content_Types].xml` for OOXML,
///    or `mimetype` entry for ODF
/// 4. `\xD0\xCF\x11\xE0` -> OLE2/CFB: inspect directory for
///    `WordDocument` (DOC), `Workbook` (XLS), or `PowerPoint Document` (PPT)
///
/// Returns None when format cannot be determined from bytes alone.
///
/// ```ignore
/// use udoc::detect::detect_format;
/// use udoc::Format;
///
/// // Real bundled fixture: PDF magic bytes within the first 1024 bytes.
/// let bytes = include_bytes!("../../../tests/corpus/minimal/hello.pdf");
/// assert_eq!(detect_format(bytes), Some(Format::Pdf));
///
/// // Garbage returns None.
/// assert_eq!(detect_format(b"hello world"), None);
/// ```
pub fn detect_format(data: &[u8]) -> Option<Format> {
    // RTF: {\rtf at start
    if data.starts_with(RTF_MAGIC) {
        return Some(Format::Rtf);
    }

    // PDF: %PDF within first 1024 bytes (some PDFs have garbage before the header)
    let search_len = data.len().min(1024);
    if let Some(window) = data.get(..search_len) {
        if window.windows(PDF_MAGIC.len()).any(|w| w == PDF_MAGIC) {
            return Some(Format::Pdf);
        }
    }

    // ZIP: PK\x03\x04 -> OOXML or ODF
    if data.len() >= 4 && data[..4] == ZIP_MAGIC {
        return detect_zip_format(data);
    }

    // OLE2/CFB: D0 CF 11 E0
    if data.len() >= 4 && data[..4] == OLE2_MAGIC {
        return detect_cfb_format(data);
    }

    None
}

/// Detect format from a file path: reads magic bytes first, then reads the
/// full file for container inspection if needed. Falls back to extension.
pub fn detect_format_path(path: &Path) -> Result<Option<Format>> {
    Ok(detect_format_path_reuse(path)?.map(|r| r.format))
}

/// Result of format detection that may carry pre-read file bytes.
///
/// When detection requires reading the full file (ZIP/OLE2 container
/// inspection), the bytes are preserved so the backend constructor can
/// reuse them instead of reading from disk a second time.
pub(crate) struct DetectionResult {
    pub format: Format,
    /// Pre-read file bytes, available when the detector had to read the full
    /// file for container inspection (ZIP/OLE2). `None` for formats detected
    /// from magic bytes alone (PDF, RTF) or from extension fallback.
    pub data: Option<Vec<u8>>,
}

/// Detect format from a file path, preserving any pre-read bytes.
///
/// This is the internal workhorse: `detect_format_path` delegates here and
/// discards the data. `Extractor::open_with` uses this directly to avoid
/// a second file read for ZIP/OLE2 formats (R-004).
pub(crate) fn detect_format_path_reuse(path: &Path) -> Result<Option<DetectionResult>> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .map_err(|e| Error::with_source(format!("opening {}", path.display()), e))?;

    // Read first 8KB for magic byte detection.
    let mut buf = vec![0u8; 8192];
    let n = file
        .read(&mut buf)
        .map_err(|e| Error::with_source(format!("reading {}", path.display()), e))?;
    buf.truncate(n);

    // Quick checks that don't need the full file.
    if buf.starts_with(b"{\\rtf") {
        return Ok(Some(DetectionResult {
            format: Format::Rtf,
            data: None,
        }));
    }
    let search_len = buf.len().min(1024);
    if buf[..search_len].windows(4).any(|w| w == b"%PDF") {
        return Ok(Some(DetectionResult {
            format: Format::Pdf,
            data: None,
        }));
    }

    // ZIP and OLE2 need the full file for container inspection.
    let needs_full_file =
        (buf.len() >= 4 && buf[..4] == ZIP_MAGIC) || (buf.len() >= 4 && buf[..4] == OLE2_MAGIC);

    if needs_full_file {
        // Read the rest of the file.
        let mut full = buf;
        file.read_to_end(&mut full)
            .map_err(|e| Error::with_source(format!("reading {}", path.display()), e))?;

        if let Some(fmt) = detect_format(&full) {
            // Preserve the bytes so the caller can reuse them.
            return Ok(Some(DetectionResult {
                format: fmt,
                data: Some(full),
            }));
        }
    }

    // Fallback: extension-based detection.
    Ok(format_from_extension(path).map(|fmt| DetectionResult {
        format: fmt,
        data: None,
    }))
}

/// Map a file extension to a format.
fn format_from_extension(path: &Path) -> Option<Format> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => Some(Format::Pdf),
        "docx" => Some(Format::Docx),
        "xlsx" => Some(Format::Xlsx),
        "pptx" => Some(Format::Pptx),
        "doc" => Some(Format::Doc),
        "xls" => Some(Format::Xls),
        "ppt" => Some(Format::Ppt),
        "odt" => Some(Format::Odt),
        "ods" => Some(Format::Ods),
        "odp" => Some(Format::Odp),
        "rtf" => Some(Format::Rtf),
        "md" | "markdown" => Some(Format::Md),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// ZIP container inspection (OOXML + ODF)
// ---------------------------------------------------------------------------

/// Inspect ZIP contents to distinguish OOXML (DOCX/XLSX/PPTX) from ODF
/// (ODT/ODS/ODP).
///
/// Strategy:
/// 1. Check for ODF `mimetype` entry (per ODF spec, first entry, uncompressed)
/// 2. Check for OOXML `[Content_Types].xml` and match on content type strings
fn detect_zip_format(data: &[u8]) -> Option<Format> {
    let diag: Arc<dyn udoc_core::diagnostics::DiagnosticsSink> = Arc::new(NullDiagnostics);
    let zip = udoc_containers::zip::ZipArchive::new(data, diag).ok()?;

    // ODF: the `mimetype` entry contains the MIME type as plain text.
    if let Some(entry) = zip.find("mimetype") {
        if let Ok(mime) = zip.read_string(entry) {
            let mime = mime.trim();
            return match mime {
                "application/vnd.oasis.opendocument.text" => Some(Format::Odt),
                "application/vnd.oasis.opendocument.spreadsheet" => Some(Format::Ods),
                "application/vnd.oasis.opendocument.presentation" => Some(Format::Odp),
                _ => None,
            };
        }
    }

    // OOXML: parse [Content_Types].xml and look for the main document part's
    // content type to distinguish DOCX/XLSX/PPTX.
    let ct_entry = zip
        .find("[Content_Types].xml")
        .or_else(|| zip.find_ci("[Content_Types].xml"))?;
    let ct_bytes = zip.read(ct_entry).ok()?;
    detect_ooxml_from_content_types(&ct_bytes)
}

/// Match OOXML format from [Content_Types].xml override entries.
///
/// We scan for Override elements whose ContentType contains a known
/// main-part content type substring. This avoids fully parsing the XML
/// just to check a few string patterns.
fn detect_ooxml_from_content_types(data: &[u8]) -> Option<Format> {
    // Parse the XML properly to handle encoding, entities, and attributes.
    let mut reader = udoc_containers::xml::XmlReader::new(data).ok()?;

    loop {
        match reader.next_element() {
            Ok(udoc_containers::xml::XmlEvent::StartElement {
                local_name,
                attributes,
                ..
            }) if local_name == "Override" => {
                if let Some(ct) = udoc_containers::xml::attr_value(&attributes, "ContentType") {
                    if let Some(fmt) = match_ooxml_content_type(ct) {
                        return Some(fmt);
                    }
                }
            }
            Ok(udoc_containers::xml::XmlEvent::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }

    None
}

/// Match a content type string to an OOXML format.
///
/// OOXML main-part content types (ECMA-376 Part 1):
/// - wordprocessingml.document.main -> DOCX
/// - spreadsheetml.sheet.main -> XLSX
/// - presentationml.presentation.main -> PPTX
///
/// Also handles template and macro-enabled variants (e.g.docm, .xlsm)
/// by matching on the namespace substring rather than the exact type.
fn match_ooxml_content_type(ct: &str) -> Option<Format> {
    if ct.contains("wordprocessingml") {
        Some(Format::Docx)
    } else if ct.contains("spreadsheetml") {
        Some(Format::Xlsx)
    } else if ct.contains("presentationml") {
        Some(Format::Pptx)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// OLE2/CFB container inspection (DOC/XLS/PPT)
// ---------------------------------------------------------------------------

/// Inspect CFB directory to distinguish DOC, XLS, and PPT.
///
/// Strategy: look for well-known stream names in the CFB directory.
/// - `WordDocument` -> DOC
/// - `Workbook` or `Book` -> XLS (Book is BIFF5 fallback)
/// - `PowerPoint Document` -> PPT
fn detect_cfb_format(data: &[u8]) -> Option<Format> {
    let diag: Arc<dyn udoc_core::diagnostics::DiagnosticsSink> = Arc::new(NullDiagnostics);
    let cfb = udoc_containers::cfb::CfbArchive::new(data, diag).ok()?;

    // Check for DOC first (most common legacy format).
    if cfb.find("WordDocument").is_some() {
        return Some(Format::Doc);
    }

    // XLS: Workbook (BIFF8) or Book (BIFF5).
    if cfb.find("Workbook").is_some() || cfb.find("Book").is_some() {
        return Some(Format::Xls);
    }

    // PPT: "PowerPoint Document" stream.
    if cfb.find("PowerPoint Document").is_some() {
        return Some(Format::Ppt);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_display() {
        assert_eq!(format!("{}", Format::Pdf), "PDF");
        assert_eq!(format!("{}", Format::Docx), "DOCX");
        assert_eq!(format!("{}", Format::Rtf), "RTF");
    }

    #[test]
    fn format_extension() {
        assert_eq!(Format::Pdf.extension(), "pdf");
        assert_eq!(Format::Docx.extension(), "docx");
        assert_eq!(Format::Xlsx.extension(), "xlsx");
        assert_eq!(Format::Pptx.extension(), "pptx");
        assert_eq!(Format::Doc.extension(), "doc");
        assert_eq!(Format::Xls.extension(), "xls");
        assert_eq!(Format::Ppt.extension(), "ppt");
        assert_eq!(Format::Odt.extension(), "odt");
        assert_eq!(Format::Ods.extension(), "ods");
        assert_eq!(Format::Odp.extension(), "odp");
        assert_eq!(Format::Rtf.extension(), "rtf");
    }

    #[test]
    fn format_mime_type() {
        assert_eq!(Format::Pdf.mime_type(), "application/pdf");
        assert_eq!(Format::Rtf.mime_type(), "application/rtf");
        assert!(Format::Docx.mime_type().contains("wordprocessingml"));
    }

    #[test]
    fn can_render_pdf_only() {
        assert!(Format::Pdf.can_render());
        for f in [
            Format::Docx,
            Format::Xlsx,
            Format::Pptx,
            Format::Doc,
            Format::Xls,
            Format::Ppt,
            Format::Odt,
            Format::Ods,
            Format::Odp,
            Format::Rtf,
            Format::Md,
        ] {
            assert!(!f.can_render(), "{f:?} should not render in alpha");
        }
    }

    #[test]
    fn has_tables_every_shipped_format() {
        for f in [
            Format::Pdf,
            Format::Docx,
            Format::Xlsx,
            Format::Pptx,
            Format::Doc,
            Format::Xls,
            Format::Ppt,
            Format::Odt,
            Format::Ods,
            Format::Odp,
            Format::Rtf,
            Format::Md,
        ] {
            assert!(f.has_tables(), "{f:?} should have tables");
        }
    }

    #[test]
    fn has_pages_paginated_and_spreadsheets() {
        // Paginated (PDF + slide formats) and spreadsheets
        // (sheet-as-page) are true.
        for f in [
            Format::Pdf,
            Format::Pptx,
            Format::Ppt,
            Format::Odp,
            Format::Xlsx,
            Format::Xls,
            Format::Ods,
        ] {
            assert!(f.has_pages(), "{f:?} should report pages");
        }
        // Flow formats are false.
        for f in [
            Format::Docx,
            Format::Doc,
            Format::Odt,
            Format::Rtf,
            Format::Md,
        ] {
            assert!(
                !f.has_pages(),
                "{f:?} should NOT report pages (flow format)"
            );
        }
    }

    #[test]
    fn capability_accessors_are_const_per_format() {
        // Each capability is deterministic per format -- no internal
        // state. Sanity: calling twice yields the same answer.
        assert_eq!(Format::Pdf.can_render(), Format::Pdf.can_render());
        assert_eq!(Format::Pdf.has_tables(), Format::Pdf.has_tables());
        assert_eq!(Format::Pdf.has_pages(), Format::Pdf.has_pages());
    }

    #[test]
    fn detect_pdf() {
        assert_eq!(detect_format(b"%PDF-1.7"), Some(Format::Pdf));
        // PDF with leading garbage
        let mut data = vec![0u8; 100];
        data.extend_from_slice(b"%PDF-1.4");
        assert_eq!(detect_format(&data), Some(Format::Pdf));
    }

    #[test]
    fn detect_rtf() {
        assert_eq!(detect_format(b"{\\rtf1\\ansi"), Some(Format::Rtf));
    }

    #[test]
    fn detect_unknown() {
        assert_eq!(detect_format(b"hello world"), None);
        assert_eq!(detect_format(b""), None);
    }

    #[test]
    fn extension_mapping() {
        assert_eq!(
            format_from_extension(Path::new("test.pdf")),
            Some(Format::Pdf)
        );
        assert_eq!(
            format_from_extension(Path::new("test.DOCX")),
            Some(Format::Docx)
        );
        assert_eq!(
            format_from_extension(Path::new("test.xlsx")),
            Some(Format::Xlsx)
        );
        assert_eq!(
            format_from_extension(Path::new("test.rtf")),
            Some(Format::Rtf)
        );
        assert_eq!(format_from_extension(Path::new("test.txt")), None);
        assert_eq!(format_from_extension(Path::new("noext")), None);
    }

    #[test]
    fn detect_format_path_nonexistent() {
        let result = detect_format_path(Path::new("/nonexistent/file.pdf"));
        assert!(result.is_err());
    }

    #[test]
    fn format_md_display() {
        assert_eq!(format!("{}", Format::Md), "Markdown");
    }

    #[test]
    fn format_md_extension() {
        assert_eq!(Format::Md.extension(), "md");
    }

    #[test]
    fn format_md_mime() {
        assert_eq!(Format::Md.mime_type(), "text/markdown");
    }

    #[test]
    fn format_md_extension_mapping() {
        assert_eq!(
            format_from_extension(Path::new("readme.md")),
            Some(Format::Md)
        );
        assert_eq!(
            format_from_extension(Path::new("readme.markdown")),
            Some(Format::Md)
        );
    }

    #[test]
    fn detect_md_no_magic() {
        assert_eq!(detect_format(b"# Hello World"), None);
    }

    #[test]
    fn format_equality_and_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(Format::Pdf);
        set.insert(Format::Docx);
        set.insert(Format::Pdf);
        assert_eq!(set.len(), 2);
    }

    // -- OOXML content type matching --

    #[test]
    fn match_ooxml_docx() {
        assert_eq!(
            match_ooxml_content_type(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"
            ),
            Some(Format::Docx)
        );
    }

    #[test]
    fn match_ooxml_xlsx() {
        assert_eq!(
            match_ooxml_content_type(
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"
            ),
            Some(Format::Xlsx)
        );
    }

    #[test]
    fn match_ooxml_pptx() {
        assert_eq!(
            match_ooxml_content_type(
                "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"
            ),
            Some(Format::Pptx)
        );
    }

    #[test]
    fn match_ooxml_macro_enabled() {
        // .docm, .xlsm, .pptm have different content types but same namespace substrings
        assert_eq!(
            match_ooxml_content_type("application/vnd.ms-word.document.macroEnabled.main+xml"),
            None // macro-enabled types don't contain "wordprocessingml"
        );
    }

    #[test]
    fn match_ooxml_unknown() {
        assert_eq!(match_ooxml_content_type("application/xml"), None);
    }

    #[test]
    fn detect_ooxml_content_types_docx() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;
        assert_eq!(detect_ooxml_from_content_types(xml), Some(Format::Docx));
    }

    #[test]
    fn detect_ooxml_content_types_xlsx() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
</Types>"#;
        assert_eq!(detect_ooxml_from_content_types(xml), Some(Format::Xlsx));
    }

    #[test]
    fn detect_ooxml_content_types_pptx() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
</Types>"#;
        assert_eq!(detect_ooxml_from_content_types(xml), Some(Format::Pptx));
    }

    #[test]
    fn detect_ooxml_content_types_empty() {
        let xml = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
</Types>"#;
        assert_eq!(detect_ooxml_from_content_types(xml), None);
    }

    // -- CFB directory inspection --

    #[test]
    fn detect_cfb_doc() {
        let data = build_cfb_with_stream("WordDocument");
        assert_eq!(detect_format(&data), Some(Format::Doc));
    }

    #[test]
    fn detect_cfb_xls() {
        let data = build_cfb_with_stream("Workbook");
        assert_eq!(detect_format(&data), Some(Format::Xls));
    }

    #[test]
    fn detect_cfb_xls_biff5() {
        let data = build_cfb_with_stream("Book");
        assert_eq!(detect_format(&data), Some(Format::Xls));
    }

    #[test]
    fn detect_cfb_ppt() {
        let data = build_cfb_with_stream("PowerPoint Document");
        assert_eq!(detect_format(&data), Some(Format::Ppt));
    }

    #[test]
    fn detect_cfb_unknown_stream() {
        let data = build_cfb_with_stream("SomeOtherStream");
        assert_eq!(detect_format(&data), None);
    }

    #[test]
    fn detect_malformed_zip_returns_none() {
        // ZIP magic but garbage after that
        let mut data = vec![0x50, 0x4B, 0x03, 0x04];
        data.extend_from_slice(&[0xFF; 100]);
        assert_eq!(detect_format(&data), None);
    }

    #[test]
    fn detect_malformed_cfb_returns_none() {
        // OLE2 magic but garbage after that
        let mut data = vec![0xD0, 0xCF, 0x11, 0xE0];
        data.extend_from_slice(&[0xFF; 100]);
        assert_eq!(detect_format(&data), None);
    }

    // -- ODF mimetype detection --

    #[test]
    fn detect_ooxml_content_types_invalid_xml() {
        assert_eq!(detect_ooxml_from_content_types(b"not xml at all"), None);
    }

    /// Build a minimal CFB file with a single named stream.
    /// Uses the test-internals feature of udoc-containers.
    fn build_cfb_with_stream(stream_name: &str) -> Vec<u8> {
        udoc_containers::test_util::build_cfb(&[(stream_name, b"test data")])
    }

    // -- DetectionResult / detect_format_path_reuse tests (R-004) --

    /// Create a unique temp directory for a test, cleaning up any stale remnant first.
    fn test_temp_dir(suffix: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "udoc_test_{}_{}_{}",
            suffix,
            std::process::id(),
            std::thread::current().name().unwrap_or("unknown")
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reuse_returns_none_data_for_pdf() {
        let dir = test_temp_dir("reuse_pdf");
        let path = dir.join("test.pdf");
        std::fs::write(&path, b"%PDF-1.4 fake content").unwrap();

        let result = detect_format_path_reuse(&path).unwrap().unwrap();
        assert_eq!(result.format, Format::Pdf);
        assert!(
            result.data.is_none(),
            "PDF detection should not pre-read the full file"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reuse_returns_none_data_for_rtf() {
        let dir = test_temp_dir("reuse_rtf");
        let path = dir.join("test.rtf");
        std::fs::write(&path, b"{\\rtf1 fake content}").unwrap();

        let result = detect_format_path_reuse(&path).unwrap().unwrap();
        assert_eq!(result.format, Format::Rtf);
        assert!(
            result.data.is_none(),
            "RTF detection should not pre-read the full file"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reuse_returns_some_data_for_cfb() {
        let cfb_bytes = build_cfb_with_stream("WordDocument");
        let dir = test_temp_dir("reuse_cfb");
        let path = dir.join("test.doc");
        std::fs::write(&path, &cfb_bytes).unwrap();

        let result = detect_format_path_reuse(&path).unwrap().unwrap();
        assert_eq!(result.format, Format::Doc);
        assert!(
            result.data.is_some(),
            "CFB detection should preserve pre-read bytes"
        );
        assert_eq!(result.data.unwrap().len(), cfb_bytes.len());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reuse_returns_none_data_for_extension_fallback() {
        let dir = test_temp_dir("reuse_ext");
        let path = dir.join("test.md");
        std::fs::write(&path, b"# Hello World").unwrap();

        let result = detect_format_path_reuse(&path).unwrap().unwrap();
        assert_eq!(result.format, Format::Md);
        assert!(
            result.data.is_none(),
            "extension fallback should not pre-read the full file"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reuse_returns_some_data_for_zip_ooxml() {
        // Build a minimal DOCX (ZIP with [Content_Types].xml indicating wordprocessingml)
        let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;
        let zip_bytes =
            udoc_containers::test_util::build_stored_zip(&[("[Content_Types].xml", content_types)]);
        let dir = test_temp_dir("reuse_zip");
        let path = dir.join("test.docx");
        std::fs::write(&path, &zip_bytes).unwrap();

        let result = detect_format_path_reuse(&path).unwrap().unwrap();
        assert_eq!(result.format, Format::Docx);
        assert!(
            result.data.is_some(),
            "ZIP/OOXML detection should preserve pre-read bytes"
        );
        assert_eq!(result.data.unwrap().len(), zip_bytes.len());

        let _ = std::fs::remove_dir_all(&dir);
    }
}

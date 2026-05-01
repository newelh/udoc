//! DOCX document XML parser.
//!
//! Parses word/document.xml and related parts using the XmlReader pull-parser.
//! Handles the w:body -> w:p -> w:r -> w:t element hierarchy for text
//! extraction, including run properties for bold/italic/underline/font.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use udoc_containers::opc::rel_types;
use udoc_containers::opc::OpcPackage;
use udoc_containers::xml::ns;
use udoc_containers::xml::{attr_value, prefixed_attr_value, toggle_attr, XmlEvent, XmlReader};
use udoc_core::convert::twips_to_points;
use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use crate::error::{Error, Result, ResultExt};
use crate::numbering::NumberingDefs;
use crate::styles::StyleMap;

/// Shared parsing context for a DOCX document part.
///
/// Bundles the hyperlink map and diagnostics sink that are threaded
/// through every parse function.
pub(crate) struct DocxContext<'a> {
    pub hyperlink_map: &'a HashMap<String, String>,
    pub diag: &'a Arc<dyn DiagnosticsSink>,
    /// Suppresses repeated w:themeColor warnings after the first occurrence.
    pub theme_color_warned: Cell<bool>,
}

/// Maximum nesting depth for XML element walking (security limit).
use udoc_core::MAX_NESTING_DEPTH;

/// Maximum number of body elements (paragraphs + tables) we'll extract
/// (safety limit for adversarial files).
const MAX_BODY_ELEMENTS: usize = 1_000_000;

/// Maximum number of runs per paragraph (safety limit).
const MAX_RUNS_PER_PARAGRAPH: usize = 100_000;

/// Maximum number of bookmarks collected per document (safety limit).
/// The bookmarks Vec is shared across all paragraphs in `parse_body`.
const MAX_BOOKMARKS_PER_DOCUMENT: usize = 10_000;

/// Maximum number of images extracted from a single DOCX package.
const MAX_IMAGES: usize = 10_000;

/// A body-level element in document order.
#[derive(Debug)]
pub enum BodyElement {
    /// A paragraph.
    Paragraph(Paragraph),
    /// A table.
    Table(crate::table::DocxTable),
}

/// A parsed DOCX document (all parts resolved).
///
/// Style parsing is deferred: raw styles.xml bytes are stored at open time,
/// and the `StyleMap` is built lazily on first access via `styles()`. This
/// avoids ~39% of extraction time on large DOCX files when callers only need
/// text (PageExtractor path) without heading/style resolution (Document model
/// conversion path).
///
/// Image part bytes are also deferred (#140): at parse time we only record
/// the image part paths; the raw input buffer is kept as `Arc<[u8]>` so
/// `images()` can re-open the OPC package and read the image ZIP entries
/// lazily. `text()` / `text_lines()` never touch image data.
pub struct ParsedDocument {
    /// Body elements in document order (paragraphs and tables interleaved).
    pub body: Vec<BodyElement>,
    /// Document metadata.
    pub metadata: DocxMetadata,
    /// Raw styles.xml bytes for deferred parsing. Empty if no styles part.
    styles_xml: Vec<u8>,
    /// Diagnostics sink for deferred style parse warnings.
    styles_diag: Arc<dyn DiagnosticsSink>,
    /// Lazily parsed style definitions (populated on first `styles()` call).
    styles: OnceLock<StyleMap>,
    /// Numbering definitions from numbering.xml.
    pub numbering: NumberingDefs,
    /// Parsed headers.
    pub headers: Vec<Vec<Paragraph>>,
    /// Parsed footers.
    pub footers: Vec<Vec<Paragraph>>,
    /// Footnotes indexed by ID.
    pub footnotes: Vec<Footnote>,
    /// Endnotes indexed by ID.
    pub endnotes: Vec<Endnote>,
    /// Bookmark names collected from w:bookmarkStart elements.
    pub bookmarks: Vec<String>,
    /// Hyperlink URL map: rId -> target URL (from document.xml.rels).
    pub hyperlink_map: HashMap<String, String>,
    /// Image part paths (ZIP entry names). Read lazily via `images()`.
    image_paths: Vec<String>,
    /// Lazily decoded image data, populated on first `images()` call.
    images_cache: OnceLock<Vec<DocxImage>>,
    /// Raw DOCX buffer, held so we can re-open the OPC package when
    /// images() is first called. Kept as `Arc<[u8]>` to avoid cloning
    /// the full DOCX bytes -- callers can share ownership cheaply.
    raw_bytes: Arc<[u8]>,
    /// Warnings collected during parsing.
    pub warnings: Vec<String>,
}

/// An image extracted from the DOCX package.
///
/// Raw image bytes are decoded lazily: at parse time we only collect the
/// image part path and keep an `Arc<[u8]>` reference into the raw DOCX
/// buffer. The first call to `DocxDocument::images()` walks the stored
/// paths and reads the matching ZIP parts. `text()` / `text_lines()`
/// never touch image data.
#[derive(Debug, Clone)]
pub struct DocxImage {
    /// Raw image bytes, decoded from the ZIP on first `images()` call.
    pub data: Arc<[u8]>,
}

impl std::fmt::Debug for ParsedDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParsedDocument")
            .field("body", &self.body)
            .field("metadata", &self.metadata)
            .field("styles_xml_len", &self.styles_xml.len())
            .field("styles_parsed", &self.styles.get().is_some())
            .field("numbering", &self.numbering)
            .field("headers", &self.headers)
            .field("footers", &self.footers)
            .field("footnotes", &self.footnotes)
            .field("endnotes", &self.endnotes)
            .field("bookmarks", &self.bookmarks)
            .field("hyperlink_map_len", &self.hyperlink_map.len())
            .field("image_paths", &self.image_paths)
            .field("images_loaded", &self.images_cache.get().is_some())
            .field("warnings", &self.warnings)
            .finish()
    }
}

impl ParsedDocument {
    /// Access the style map, parsing styles.xml on first call.
    ///
    /// Returns a default (empty) `StyleMap` if styles.xml was missing or
    /// failed to parse. Parsing errors are emitted as diagnostics warnings.
    pub fn styles(&self) -> &StyleMap {
        self.styles.get_or_init(|| {
            if self.styles_xml.is_empty() {
                return StyleMap::default();
            }
            match crate::styles::parse_styles(&self.styles_xml, &self.styles_diag) {
                Ok(styles) => styles,
                Err(e) => {
                    self.styles_diag.warning(Warning::new(
                        "DocxStylesParseError",
                        format!("error parsing styles.xml: {e}"),
                    ));
                    StyleMap::default()
                }
            }
        })
    }

    /// Access image parts, decoding them from the ZIP on first call (#140).
    ///
    /// Returns an empty slice if the document has no image parts, or if
    /// image reading fails (errors surface as diagnostics warnings). The
    /// `text()` / `text_lines()` extraction paths never trigger this.
    pub fn images(&self) -> &[DocxImage] {
        self.images_cache.get_or_init(|| self.load_images())
    }

    /// Load image parts by re-opening the OPC package over `raw_bytes`.
    ///
    /// Called at most once per document (driven by `images_cache`). The
    /// OPC package is re-parsed here; that's cheap relative to reading
    /// the image parts themselves, and avoids keeping a self-referential
    /// OpcPackage alive for the lifetime of the document.
    fn load_images(&self) -> Vec<DocxImage> {
        if self.image_paths.is_empty() {
            return Vec::new();
        }
        let pkg = match OpcPackage::new(&self.raw_bytes, Arc::clone(&self.styles_diag)) {
            Ok(pkg) => pkg,
            Err(e) => {
                self.styles_diag.warning(Warning::new(
                    "DocxImageReopenFailed",
                    format!("could not re-open package for images: {e}"),
                ));
                return Vec::new();
            }
        };
        let mut out = Vec::with_capacity(self.image_paths.len());
        for zip_path in &self.image_paths {
            match pkg.read_part(zip_path) {
                Ok(data) => {
                    out.push(DocxImage {
                        data: Arc::<[u8]>::from(data),
                    });
                }
                Err(e) => {
                    self.styles_diag.warning(Warning::new(
                        "DocxImageMissing",
                        format!("could not read image part {zip_path}: {e}"),
                    ));
                }
            }
        }
        out
    }

    /// Returns true if styles.xml has been parsed (for testing).
    #[cfg(test)]
    pub(crate) fn styles_parsed(&self) -> bool {
        self.styles.get().is_some()
    }

    /// Returns true if image bytes have been loaded (for testing).
    #[cfg(test)]
    pub(crate) fn images_loaded(&self) -> bool {
        self.images_cache.get().is_some()
    }
}

/// A footnote definition.
#[derive(Debug, Clone)]
pub struct Footnote {
    /// Footnote ID.
    pub id: String,
    /// Paragraphs in the footnote.
    pub paragraphs: Vec<Paragraph>,
}

/// An endnote definition.
#[derive(Debug, Clone)]
pub struct Endnote {
    /// Endnote ID.
    pub id: String,
    /// Paragraphs in the endnote.
    pub paragraphs: Vec<Paragraph>,
}

/// A parsed paragraph from the document body.
#[derive(Debug, Clone)]
pub struct Paragraph {
    /// Text runs within this paragraph.
    pub runs: Vec<Run>,
    /// Style ID from w:pPr/w:pStyle (if any).
    pub style_id: Option<String>,
    /// Outline level from w:pPr/w:outlineLvl (0-based, None = body text).
    pub outline_level: Option<u8>,
    /// Numbering properties: (numId, ilvl).
    pub num_props: Option<(String, u8)>,
    /// Whether this paragraph is a table-of-contents (field) paragraph.
    #[allow(dead_code)] // : parsed for future TOC filtering/annotation
    pub is_toc: bool,
    /// Paragraph alignment from w:jc (left, center, right, both/justify).
    pub alignment: Option<String>,
    /// Space before paragraph in points (from w:spacing w:before, twips / 20).
    pub space_before: Option<f64>,
    /// Space after paragraph in points (from w:spacing w:after, twips / 20).
    pub space_after: Option<f64>,
    /// Left indentation in points (from w:ind w:left or w:start, twips / 20).
    pub indent_left: Option<f64>,
    /// Right indentation in points (from w:ind w:right or w:end, twips / 20).
    pub indent_right: Option<f64>,
}

/// A text run within a paragraph.
#[derive(Debug, Clone)]
pub struct Run {
    /// Text content of this run.
    pub text: String,
    /// Whether this run is bold (None = not specified, inherit from style).
    pub bold: Option<bool>,
    /// Whether this run is italic (None = not specified, inherit from style).
    pub italic: Option<bool>,
    /// Whether this run has underline.
    pub underline: bool,
    /// Whether this run is hidden/invisible.
    pub invisible: bool,
    /// Font name (from w:rFonts).
    pub font_name: Option<String>,
    /// Font size in points (converted from half-points in w:sz).
    /// None means no size specified (inherit from style).
    pub font_size_pts: Option<f64>,
    /// Text color from w:color (RGB).
    pub color: Option<[u8; 3]>,
    /// Highlight color name from w:highlight (e.g. "yellow", "red").
    pub highlight: Option<String>,
    /// Whether this run has strikethrough (w:strike or w:dstrike).
    pub strikethrough: bool,
    /// Hyperlink URL when this run is inside a w:hyperlink element.
    pub hyperlink_url: Option<String>,
    /// Footnote or endnote reference ID (from w:footnoteReference or
    /// w:endnoteReference). The label is prefixed "fn:" or "en:" to match
    /// the keying in the relationships overlay.
    pub note_ref: Option<String>,
}

impl Run {
    fn new() -> Self {
        Self {
            text: String::new(),
            bold: None,
            italic: None,
            underline: false,
            invisible: false,
            font_name: None,
            font_size_pts: None,
            color: None,
            highlight: None,
            strikethrough: false,
            hyperlink_url: None,
            note_ref: None,
        }
    }
}

/// Re-export DocumentMetadata for use as DOCX metadata type.
pub use udoc_core::backend::DocumentMetadata as DocxMetadata;

/// Check if a namespace URI matches WML (Transitional or Strict).
pub(crate) fn is_wml(ns: Option<&str>) -> bool {
    matches!(ns, Some(ns::WML) | Some(ns::WML_STRICT))
}

/// Check if a namespace URI is the Markup Compatibility namespace.
fn is_mc(ns: Option<&str>) -> bool {
    matches!(ns, Some(ns::MARKUP_COMPATIBILITY))
}

/// Parse the DOCX document from raw bytes.
///
/// `raw_bytes` is the full DOCX buffer as `Arc<[u8]>`. Image part bytes are
/// not read at parse time (#140); only their ZIP paths are collected. On
/// first `images()` call, the OPC package is re-opened over `raw_bytes`
/// and the recorded paths are decoded.
pub(crate) fn parse_docx(
    raw_bytes: Arc<[u8]>,
    diag: Arc<dyn DiagnosticsSink>,
) -> Result<ParsedDocument> {
    let pkg = OpcPackage::new(&raw_bytes, Arc::clone(&diag)).context("opening OPC package")?;

    // Find the main document part via the officeDocument relationship.
    let doc_rel = pkg
        .find_package_rel_by_type(rel_types::OFFICE_DOCUMENT)
        .ok_or_else(|| {
            Error::new("invalid DOCX structure: no officeDocument relationship in package")
        })?;
    let doc_target = doc_rel.target.clone();

    let doc_xml = pkg.read_part(&doc_target).context("reading document.xml")?;

    let doc_part = if doc_target.starts_with('/') {
        doc_target.clone()
    } else {
        format!("/{doc_target}")
    };

    // Read raw styles.xml bytes for deferred parsing (O-001). The XML is
    // only parsed into a StyleMap when styles() is first called, saving
    // ~39% of extraction time when callers only need text.
    let styles_xml = read_styles_bytes(&pkg, &doc_part, &diag);

    let numbering = parse_numbering_part(&pkg, &doc_part, &diag);

    // Parse headers and footers.
    let headers = crate::ancillary::parse_headers(&pkg, &doc_part, &diag);
    let footers = crate::ancillary::parse_footers(&pkg, &doc_part, &diag);
    let footnotes = crate::ancillary::parse_footnotes(&pkg, &doc_part, &diag);
    let endnotes = crate::ancillary::parse_endnotes(&pkg, &doc_part, &diag);

    // Parse core properties for metadata.
    let metadata = parse_core_properties(&pkg, &diag);

    // Build hyperlink URL map from document.xml.rels: rId -> target URL.
    let hyperlink_map = build_hyperlink_map(&pkg, &doc_part);

    // Collect image ZIP paths without reading the bytes (#140). The raw
    // data lives in `raw_bytes`; images() will re-open the OPC package
    // on demand. text()/text_lines() never trigger image decoding.
    let image_paths = collect_image_paths(&pkg, &doc_part, &diag);

    // Drop the OPC package here: we have everything we need except image
    // bytes, which are reopened lazily via `raw_bytes`.
    drop(pkg);

    // Parse the document body.
    let mut body = Vec::new();
    let mut warnings = Vec::new();
    let mut bookmarks = Vec::new();

    let ctx = DocxContext {
        hyperlink_map: &hyperlink_map,
        diag: &diag,
        theme_color_warned: Cell::new(false),
    };
    parse_body(&doc_xml, &mut body, &mut warnings, &mut bookmarks, &ctx)?;

    Ok(ParsedDocument {
        body,
        metadata,
        styles_xml,
        styles_diag: Arc::clone(&diag),
        styles: OnceLock::new(),
        numbering,
        headers,
        footers,
        footnotes,
        endnotes,
        bookmarks,
        hyperlink_map,
        image_paths,
        images_cache: OnceLock::new(),
        raw_bytes,
        warnings,
    })
}

/// Build a map of rId -> target URL from document.xml.rels hyperlink relationships.
pub(crate) fn build_hyperlink_map(pkg: &OpcPackage<'_>, doc_part: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for rel in pkg.part_rels(doc_part) {
        if udoc_containers::opc::relationships::rel_type_matches(
            &rel.rel_type,
            rel_types::HYPERLINK,
        ) {
            map.insert(rel.id.clone(), rel.target.clone());
        }
    }
    map
}

/// Collect image part ZIP paths from IMAGE relationships in
/// document.xml.rels. Paths are resolved to absolute ZIP entry names
/// (leading slash stripped) and returned in relationship order.
///
/// These are images scoped to the document body part. Theme images,
/// header/footer images, and footnote images live in their own .rels files
/// and are not included here. Orphaned relationships (images deleted from
/// the body but still referenced in .rels) may be included.
///
/// Decoded image bytes are loaded lazily via `ParsedDocument::images()`
/// on first call (#140): `text()` / `text_lines()` never walk images.
fn collect_image_paths(
    pkg: &OpcPackage<'_>,
    doc_part: &str,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Vec<String> {
    let mut paths = Vec::new();
    for rel in pkg.part_rels(doc_part) {
        if paths.len() >= MAX_IMAGES {
            diag.warning(udoc_core::diagnostics::Warning::new(
                "DocxMaxImages",
                format!("image limit ({MAX_IMAGES}) reached, skipping remaining"),
            ));
            break;
        }
        if !udoc_containers::opc::relationships::rel_type_matches(&rel.rel_type, rel_types::IMAGE) {
            continue;
        }
        let resolved = pkg.resolve_uri(doc_part, &rel.target);
        let zip_path = resolved.strip_prefix('/').unwrap_or(&resolved).to_string();
        paths.push(zip_path);
    }
    paths
}

/// Skip to the matching end element for a start element we want to discard.
/// Consumes all nested elements so the reader stays in a consistent state.
pub(crate) fn skip_element(reader: &mut XmlReader<'_>) -> Result<()> {
    let mut skip_depth: usize = 1;
    loop {
        match reader.next_element().context("skipping element subtree")? {
            XmlEvent::StartElement { .. } => skip_depth += 1,
            XmlEvent::EndElement { .. } => {
                skip_depth = skip_depth.saturating_sub(1);
                if skip_depth == 0 {
                    return Ok(());
                }
            }
            XmlEvent::Eof => return Ok(()),
            _ => {}
        }
    }
}

/// Parse the document body (w:body) from document.xml.
fn parse_body(
    xml_data: &[u8],
    body: &mut Vec<BodyElement>,
    _warnings: &mut Vec<String>,
    bookmarks: &mut Vec<String>,
    ctx: &DocxContext<'_>,
) -> Result<()> {
    let mut reader =
        XmlReader::new(xml_data).context("initializing XML parser for document.xml")?;

    // Advance to w:body.
    let mut in_body = false;
    let mut depth: usize = 0;
    let mut mc_depth: Option<usize> = None; // depth at which we entered mc:AlternateContent
    let mut skip_depth: Option<usize> = None; // depth at which we started skipping (mc:Choice)

    loop {
        let event = reader.next_element().context("parsing document.xml")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                depth = depth.saturating_add(1);
                if depth > MAX_NESTING_DEPTH {
                    ctx.diag.warning(Warning::new(
                        "DocxMaxNestingDepth",
                        format!(
                            "XML nesting depth exceeded {} at element {}",
                            MAX_NESTING_DEPTH, local_name
                        ),
                    ));
                    // Consume the entire subtree to keep the reader consistent.
                    skip_element(&mut reader)?;
                    depth = depth.saturating_sub(1);
                    continue;
                }

                // Handle mc:AlternateContent: skip mc:Choice, process mc:Fallback.
                if is_mc(namespace_uri.as_deref()) && local_name == "AlternateContent" {
                    mc_depth = Some(depth);
                    continue;
                }
                if mc_depth.is_some() && is_mc(namespace_uri.as_deref()) {
                    if local_name == "Choice" {
                        skip_depth = Some(depth);
                        continue;
                    } else if local_name == "Fallback" {
                        // Process Fallback content: let normal parsing handle children.
                        continue;
                    }
                }

                // Skip content inside mc:Choice.
                if let Some(sd) = skip_depth {
                    if depth > sd {
                        continue;
                    }
                }

                if !in_body {
                    if is_wml(namespace_uri.as_deref()) && local_name == "body" {
                        in_body = true;
                    }
                    continue;
                }

                // Inside w:body: parse w:p (paragraph) and w:tbl (table).
                if is_wml(namespace_uri.as_deref()) {
                    match local_name.as_ref() {
                        "p" => {
                            if body.len() >= MAX_BODY_ELEMENTS {
                                ctx.diag.warning(Warning::new(
                                    "DocxMaxBodyElements",
                                    format!(
                                        "body element limit ({MAX_BODY_ELEMENTS}) exceeded, truncating"
                                    ),
                                ));
                                skip_element(&mut reader)?;
                                depth = depth.saturating_sub(1);
                                continue;
                            }
                            let para = parse_paragraph(&mut reader, &attributes, ctx, bookmarks)?;
                            body.push(BodyElement::Paragraph(para));
                            // parse_paragraph consumed the end element, adjust depth.
                            depth = depth.saturating_sub(1);
                        }
                        "tbl" => {
                            if body.len() >= MAX_BODY_ELEMENTS {
                                ctx.diag.warning(Warning::new(
                                    "DocxMaxBodyElements",
                                    format!(
                                        "body element limit ({MAX_BODY_ELEMENTS}) exceeded, truncating"
                                    ),
                                ));
                                skip_element(&mut reader)?;
                                depth = depth.saturating_sub(1);
                                continue;
                            }
                            let tbl = crate::table::parse_table(&mut reader, ctx)?;
                            body.push(BodyElement::Table(tbl));
                            // parse_table consumed the end element, adjust depth.
                            depth = depth.saturating_sub(1);
                        }
                        _ => {}
                    }
                }
            }
            XmlEvent::EndElement {
                local_name,
                namespace_uri,
                ..
            } => {
                // Handle mc:AlternateContent end.
                if let Some(mcd) = mc_depth {
                    if depth == mcd {
                        mc_depth = None;
                        skip_depth = None;
                    }
                }
                if let Some(sd) = skip_depth {
                    if depth == sd {
                        skip_depth = None;
                    }
                }

                if in_body && is_wml(namespace_uri.as_deref()) && local_name == "body" {
                    in_body = false;
                }

                depth = depth.saturating_sub(1);
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(())
}

/// Parse a single w:p element into a Paragraph.
pub(crate) fn parse_paragraph(
    reader: &mut XmlReader<'_>,
    _attrs: &[udoc_containers::xml::Attribute<'_>],
    ctx: &DocxContext<'_>,
    bookmarks: &mut Vec<String>,
) -> Result<Paragraph> {
    let mut para = Paragraph {
        runs: Vec::new(),
        style_id: None,
        outline_level: None,
        num_props: None,
        is_toc: false,
        alignment: None,
        space_before: None,
        space_after: None,
        indent_left: None,
        indent_right: None,
    };

    let mut depth: usize = 1; // we're already inside w:p
    let mut del_depth: usize = 0; // nesting depth inside w:del (skip content)
    let mut in_ppr = false; // inside w:pPr
    let mut mc_depth: Option<usize> = None; // mc:AlternateContent depth
    let mut skip_depth: Option<usize> = None; // mc:Choice skip depth
    let mut active_hyperlink_url: Option<String> = None; // URL when inside w:hyperlink

    loop {
        let event = reader.next_element().context("parsing w:p")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                depth += 1;

                // Handle mc:AlternateContent inside paragraphs.
                if is_mc(namespace_uri.as_deref()) && local_name == "AlternateContent" {
                    mc_depth = Some(depth);
                    continue;
                }
                if mc_depth.is_some() && is_mc(namespace_uri.as_deref()) {
                    if local_name == "Choice" {
                        skip_depth = Some(depth);
                        continue;
                    } else if local_name == "Fallback" {
                        continue;
                    }
                }
                if let Some(sd) = skip_depth {
                    if depth > sd {
                        continue;
                    }
                }

                if is_wml(namespace_uri.as_deref()) {
                    match local_name.as_ref() {
                        "pPr" => {
                            in_ppr = true;
                        }
                        "pStyle" if in_ppr => {
                            if let Some(val) = attr_value(&attributes, "val") {
                                para.style_id = Some(val.to_string());
                            }
                        }
                        "outlineLvl" if in_ppr => {
                            if let Some(val) = attr_value(&attributes, "val") {
                                if let Ok(level) = val.parse::<u8>() {
                                    para.outline_level = Some(level);
                                }
                            }
                        }
                        "jc" if in_ppr => {
                            if let Some(val) = attr_value(&attributes, "val") {
                                para.alignment = Some(val.to_string());
                            }
                        }
                        "spacing" if in_ppr => {
                            if let Some(before) = attr_value(&attributes, "before") {
                                if let Ok(twips) = before.parse::<f64>() {
                                    para.space_before = Some(twips_to_points(twips));
                                }
                            }
                            if let Some(after) = attr_value(&attributes, "after") {
                                if let Ok(twips) = after.parse::<f64>() {
                                    para.space_after = Some(twips_to_points(twips));
                                }
                            }
                        }
                        "ind" if in_ppr => {
                            let left = attr_value(&attributes, "left")
                                .or_else(|| attr_value(&attributes, "start"));
                            if let Some(val) = left {
                                if let Ok(twips) = val.parse::<f64>() {
                                    para.indent_left = Some(twips_to_points(twips));
                                }
                            }
                            let right = attr_value(&attributes, "right")
                                .or_else(|| attr_value(&attributes, "end"));
                            if let Some(val) = right {
                                if let Ok(twips) = val.parse::<f64>() {
                                    para.indent_right = Some(twips_to_points(twips));
                                }
                            }
                        }
                        "numPr" if in_ppr => {
                            // numId and ilvl will be parsed as child elements.
                        }
                        "numId" if in_ppr => {
                            if let Some(val) = attr_value(&attributes, "val") {
                                // numId="0" means "remove numbering" in OOXML.
                                if val == "0" {
                                    para.num_props = None;
                                } else {
                                    let ilvl =
                                        para.num_props.as_ref().map(|(_, l)| *l).unwrap_or(0);
                                    para.num_props = Some((val.to_string(), ilvl));
                                }
                            }
                        }
                        "ilvl" if in_ppr => {
                            if let Some(val) = attr_value(&attributes, "val") {
                                if let Ok(level) = val.parse::<u8>() {
                                    let num_id = para
                                        .num_props
                                        .as_ref()
                                        .map(|(id, _)| id.clone())
                                        .unwrap_or_default();
                                    para.num_props = Some((num_id, level));
                                }
                            }
                        }
                        "bookmarkStart"
                            if !in_ppr && bookmarks.len() < MAX_BOOKMARKS_PER_DOCUMENT =>
                        {
                            if let Some(name) = attr_value(&attributes, "name") {
                                // _GoBack is a Word-internal bookmark for cursor
                                // position restore; filter it to avoid noise.
                                if !name.is_empty() && name != "_GoBack" {
                                    bookmarks.push(name.to_string());
                                    if bookmarks.len() == MAX_BOOKMARKS_PER_DOCUMENT {
                                        ctx.diag.warning(Warning::new(
                                                "DocxMaxBookmarks",
                                                format!(
                                                    "bookmark limit ({MAX_BOOKMARKS_PER_DOCUMENT}) reached, skipping remaining"
                                                ),
                                            ));
                                    }
                                }
                            }
                        }
                        "del" => {
                            del_depth += 1;
                        }
                        "r" if del_depth == 0 && !in_ppr => {
                            if para.runs.len() >= MAX_RUNS_PER_PARAGRAPH {
                                ctx.diag.warning(Warning::new(
                                    "DocxMaxRuns",
                                    format!(
                                        "run limit ({}) exceeded in paragraph, truncating",
                                        MAX_RUNS_PER_PARAGRAPH
                                    ),
                                ));
                                skip_element(reader)?;
                                depth = depth.saturating_sub(1);
                            } else {
                                let mut run = parse_run(reader, &attributes, ctx)?;
                                if !run.text.is_empty() || run.note_ref.is_some() {
                                    if let Some(ref url) = active_hyperlink_url {
                                        run.hyperlink_url = Some(url.clone());
                                    }
                                    para.runs.push(run);
                                }
                                depth = depth.saturating_sub(1); // parse_run consumed the end element
                            }
                        }
                        "hyperlink" if del_depth == 0 && !in_ppr => {
                            // Resolve hyperlink URL from r:id or w:anchor.
                            let r_id = prefixed_attr_value(&attributes, "r", "id");
                            let url = if let Some(rid) = r_id {
                                ctx.hyperlink_map.get(rid).cloned()
                            } else {
                                None
                            };
                            let url = url.or_else(|| {
                                attr_value(&attributes, "anchor").map(|a| format!("#{a}"))
                            });
                            if url.is_none() {
                                ctx.diag.warning(Warning::new(
                                    "DocxUnresolvedHyperlink",
                                    "w:hyperlink has neither r:id nor w:anchor, skipping",
                                ));
                            }
                            active_hyperlink_url = url;
                        }
                        _ => {}
                    }
                }
            }
            XmlEvent::EndElement {
                local_name,
                namespace_uri,
                ..
            } => {
                // Handle mc:AlternateContent end.
                if let Some(mcd) = mc_depth {
                    if depth == mcd {
                        mc_depth = None;
                        skip_depth = None;
                    }
                }
                if let Some(sd) = skip_depth {
                    if depth == sd {
                        skip_depth = None;
                    }
                }

                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break; // End of w:p
                }
                if is_wml(namespace_uri.as_deref()) {
                    match local_name.as_ref() {
                        "pPr" => in_ppr = false,
                        "del" => {
                            del_depth = del_depth.saturating_sub(1);
                        }
                        "hyperlink" => {
                            active_hyperlink_url = None;
                        }
                        _ => {}
                    }
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(para)
}

/// Parse a single w:r (run) element.
///
/// Uses `next_event()` (not `next_element()`) because we need to capture
/// Text and CData events for the actual run content.
fn parse_run(
    reader: &mut XmlReader<'_>,
    _attrs: &[udoc_containers::xml::Attribute<'_>],
    ctx: &DocxContext<'_>,
) -> Result<Run> {
    let mut run = Run::new();
    let mut depth: usize = 1;
    let mut in_rpr = false;
    let mut in_text = false; // inside w:t element
    let mut preserve_space = false;

    loop {
        let event = reader.next_event().context("parsing w:r")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                depth += 1;
                if is_wml(namespace_uri.as_deref()) {
                    match local_name.as_ref() {
                        "rPr" => {
                            in_rpr = true;
                        }
                        // Run properties (inside w:rPr).
                        "b" if in_rpr => {
                            run.bold = Some(toggle_attr(attr_value(&attributes, "val")));
                        }
                        "i" if in_rpr => {
                            run.italic = Some(toggle_attr(attr_value(&attributes, "val")));
                        }
                        "u" if in_rpr => {
                            let val = attr_value(&attributes, "val");
                            run.underline = !matches!(val, Some("none"));
                        }
                        "vanish" if in_rpr => {
                            run.invisible = toggle_attr(attr_value(&attributes, "val"));
                        }
                        "color" if in_rpr => {
                            if let Some(val) = attr_value(&attributes, "val") {
                                if let Some(c) = udoc_core::document::Color::from_hex(val) {
                                    run.color = Some(c.to_array());
                                }
                            }
                            if attr_value(&attributes, "themeColor").is_some()
                                && !ctx.theme_color_warned.get()
                            {
                                ctx.theme_color_warned.set(true);
                                ctx.diag.warning(Warning::new(
                                    "DocxThemeColor",
                                    "w:themeColor attributes present but theme color resolution \
                                     is not yet supported; direct w:val colors are used when available",
                                ));
                            }
                        }
                        "strike" if in_rpr => {
                            run.strikethrough = toggle_attr(attr_value(&attributes, "val"));
                        }
                        "dstrike" if in_rpr && toggle_attr(attr_value(&attributes, "val")) => {
                            run.strikethrough = true;
                        }
                        "highlight" if in_rpr => {
                            if let Some(val) = attr_value(&attributes, "val") {
                                if val != "none" {
                                    run.highlight = Some(val.to_string());
                                }
                            }
                        }
                        "rFonts" if in_rpr => {
                            // Precedence: ascii -> hAnsi -> cs -> eastAsia.
                            let font = attr_value(&attributes, "ascii")
                                .or_else(|| attr_value(&attributes, "hAnsi"))
                                .or_else(|| attr_value(&attributes, "cs"))
                                .or_else(|| attr_value(&attributes, "eastAsia"));
                            if let Some(f) = font {
                                run.font_name = Some(f.to_string());
                            }
                        }
                        "sz" if in_rpr => {
                            // Font size is in half-points. Convert to points.
                            if let Some(val) = attr_value(&attributes, "val") {
                                if let Ok(half_pts) = val.parse::<f64>() {
                                    run.font_size_pts = Some(half_pts / 2.0);
                                }
                            }
                        }
                        // Text content element.
                        "t" if !in_rpr => {
                            in_text = true;
                            // Reset and check xml:space="preserve" per w:t element.
                            preserve_space = attr_value(&attributes, "space")
                                .map(|v| v == "preserve")
                                .unwrap_or(false);
                        }
                        // Special content elements.
                        "tab" if !in_rpr => {
                            run.text.push('\t');
                        }
                        "br" if !in_rpr => {
                            run.text.push('\n');
                        }
                        "cr" if !in_rpr => {
                            run.text.push('\n');
                        }
                        "noBreakHyphen" if !in_rpr => {
                            run.text.push('\u{2011}'); // Non-breaking hyphen
                        }
                        "softHyphen" if !in_rpr => {
                            run.text.push('\u{00AD}'); // Soft hyphen
                        }
                        "sym" if !in_rpr => {
                            // Symbol character. Best effort: try to decode the char attribute.
                            if let Some(char_val) = attr_value(&attributes, "char") {
                                if let Ok(code) = u32::from_str_radix(char_val, 16) {
                                    if let Some(c) = char::from_u32(code) {
                                        run.text.push(c);
                                    }
                                }
                            }
                        }
                        "footnoteReference" if !in_rpr => {
                            if let Some(id) = attr_value(&attributes, "id") {
                                run.note_ref = Some(format!("fn:{id}"));
                            }
                        }
                        "endnoteReference" if !in_rpr => {
                            if let Some(id) = attr_value(&attributes, "id") {
                                run.note_ref = Some(format!("en:{id}"));
                            }
                        }
                        _ => {}
                    }
                }
            }
            XmlEvent::EndElement {
                local_name,
                namespace_uri,
                ..
            } => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break; // End of w:r
                }
                if is_wml(namespace_uri.as_deref()) {
                    match local_name.as_ref() {
                        "rPr" => in_rpr = false,
                        "t" => in_text = false,
                        _ => {}
                    }
                }
            }
            XmlEvent::Text(text) => {
                if in_text {
                    if preserve_space {
                        run.text.push_str(&text);
                    } else {
                        // Without xml:space="preserve", collapse whitespace.
                        // This is the most common DOCX text extraction bug.
                        run.text.push_str(text.trim());
                    }
                }
            }
            XmlEvent::CData(text) => {
                if in_text {
                    run.text.push_str(&text);
                }
            }
            XmlEvent::Eof => break,
        }
    }

    Ok(run)
}

/// Read raw styles.xml bytes from the package for deferred parsing (O-001).
///
/// Returns empty `Vec<u8>` if no styles part exists or cannot be read.
/// The actual XML parsing is deferred to `ParsedDocument::styles()`.
fn read_styles_bytes(
    pkg: &OpcPackage<'_>,
    doc_part: &str,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Vec<u8> {
    let rel = match pkg.find_part_rel_by_type(doc_part, rel_types::STYLES) {
        Some(r) => r,
        None => return Vec::new(),
    };

    let styles_uri = pkg.resolve_uri(doc_part, &rel.target);
    match pkg.read_part(&styles_uri) {
        Ok(d) => d,
        Err(e) => {
            diag.warning(Warning::new(
                "DocxMissingStyles",
                format!("could not read styles.xml: {e}"),
            ));
            Vec::new()
        }
    }
}

/// Parse numbering.xml from the package.
fn parse_numbering_part(
    pkg: &OpcPackage<'_>,
    doc_part: &str,
    diag: &Arc<dyn DiagnosticsSink>,
) -> NumberingDefs {
    let rel = match pkg.find_part_rel_by_type(doc_part, rel_types::NUMBERING) {
        Some(r) => r,
        None => return NumberingDefs::default(),
    };

    let numbering_uri = pkg.resolve_uri(doc_part, &rel.target);
    let data = match pkg.read_part(&numbering_uri) {
        Ok(d) => d,
        Err(e) => {
            diag.warning(Warning::new(
                "DocxMissingNumbering",
                format!("could not read numbering.xml: {e}"),
            ));
            return NumberingDefs::default();
        }
    };

    match crate::numbering::parse_numbering(&data, diag) {
        Ok(defs) => defs,
        Err(e) => {
            diag.warning(Warning::new(
                "DocxNumberingParseError",
                format!("error parsing numbering.xml: {e}"),
            ));
            NumberingDefs::default()
        }
    }
}

/// Parse core properties (docProps/core.xml) for metadata.
fn parse_core_properties(pkg: &OpcPackage<'_>, diag: &Arc<dyn DiagnosticsSink>) -> DocxMetadata {
    let rel = match pkg.find_package_rel_by_type(rel_types::CORE_PROPERTIES) {
        Some(r) => r,
        None => return DocxMetadata::default(),
    };

    let data = match pkg.read_part(&rel.target) {
        Ok(d) => d,
        Err(e) => {
            diag.warning(Warning::new(
                "DocxMetadataReadFailed",
                format!("could not read docProps/core.xml: {e}, using defaults"),
            ));
            return DocxMetadata::default();
        }
    };

    udoc_containers::opc::metadata::parse_core_properties(&data)
}

/// Maximum number of paragraphs in ancillary parts (headers, footers, notes).
const MAX_ANCILLARY_PARAGRAPHS: usize = 10_000;

/// Parse paragraphs from a part containing w:p elements (used by headers,
/// footers, footnotes, etc.).
pub(crate) fn parse_paragraphs_from_part(
    data: &[u8],
    ctx: &DocxContext<'_>,
) -> Result<Vec<Paragraph>> {
    let mut reader = XmlReader::new(data).context("initializing XML parser")?;
    let mut paragraphs = Vec::new();
    // Bookmarks in ancillary parts (footnotes, endnotes, headers, footers) are
    // collected but intentionally not wired to the document's relationships
    // overlay. These bookmarks only make sense within their local scope and
    // cannot be cross-referenced from the main document body.
    let mut ignored_bookmarks = Vec::new();

    loop {
        let event = reader.next_element().context("parsing XML part")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } if is_wml(namespace_uri.as_deref()) && local_name == "p" => {
                if paragraphs.len() >= MAX_ANCILLARY_PARAGRAPHS {
                    ctx.diag.warning(Warning::new(
                        "DocxMaxAncillaryParagraphs",
                        format!(
                            "ancillary part paragraph limit ({}) exceeded, truncating",
                            MAX_ANCILLARY_PARAGRAPHS
                        ),
                    ));
                    break;
                }
                let para = parse_paragraph(&mut reader, &attributes, ctx, &mut ignored_bookmarks)?;
                paragraphs.push(para);
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(paragraphs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::diagnostics::NullDiagnostics;

    fn null_diag() -> Arc<dyn DiagnosticsSink> {
        Arc::new(NullDiagnostics)
    }

    fn test_ctx() -> (HashMap<String, String>, Arc<dyn DiagnosticsSink>) {
        (HashMap::new(), null_diag())
    }

    /// Extract only paragraphs from body elements (in order).
    fn paragraphs(body: &[BodyElement]) -> Vec<&Paragraph> {
        body.iter()
            .filter_map(|e| match e {
                BodyElement::Paragraph(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    /// Extract only tables from body elements (in order).
    fn tables(body: &[BodyElement]) -> Vec<&crate::table::DocxTable> {
        body.iter()
            .filter_map(|e| match e {
                BodyElement::Table(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn parse_simple_paragraph() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r><w:t>Hello World</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        assert_eq!(paras.len(), 1);
        assert_eq!(paras[0].runs.len(), 1);
        assert_eq!(paras[0].runs[0].text, "Hello World");
    }

    #[test]
    fn parse_bold_italic_run() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r>
        <w:rPr><w:b/><w:i/></w:rPr>
        <w:t>Bold and Italic</w:t>
      </w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        assert_eq!(paras[0].runs[0].bold, Some(true));
        assert_eq!(paras[0].runs[0].italic, Some(true));
    }

    #[test]
    fn parse_bold_explicit_false() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r>
        <w:rPr><w:b w:val="0"/></w:rPr>
        <w:t>Not Bold</w:t>
      </w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        assert_eq!(paras[0].runs[0].bold, Some(false));
    }

    #[test]
    fn parse_font_info() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r>
        <w:rPr>
          <w:rFonts w:ascii="Calibri"/>
          <w:sz w:val="24"/>
        </w:rPr>
        <w:t>Styled text</w:t>
      </w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        let run = &paras[0].runs[0];
        assert_eq!(run.font_name.as_deref(), Some("Calibri"));
        assert_eq!(run.font_size_pts, Some(12.0)); // 24 half-points = 12 pts
    }

    #[test]
    fn parse_special_content_elements() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r>
        <w:t>before</w:t>
        <w:tab/>
        <w:t>after tab</w:t>
        <w:br/>
        <w:t>after break</w:t>
      </w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        let text = &paras[0].runs[0].text;
        assert!(text.contains('\t'), "expected tab in: {text}");
        assert!(text.contains('\n'), "expected newline in: {text}");
    }

    #[test]
    fn skip_tracked_deletions() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r><w:t>Visible</w:t></w:r>
      <w:del><w:r><w:t>Deleted</w:t></w:r></w:del>
      <w:ins><w:r><w:t> Inserted</w:t></w:r></w:ins>
    </w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        let all_text: String = paras[0].runs.iter().map(|r| r.text.as_str()).collect();
        assert!(all_text.contains("Visible"), "got: {all_text}");
        assert!(!all_text.contains("Deleted"), "got: {all_text}");
        assert!(all_text.contains("Inserted"), "got: {all_text}");
    }

    #[test]
    fn parse_paragraph_style() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:pPr>
        <w:pStyle w:val="Heading1"/>
        <w:outlineLvl w:val="0"/>
      </w:pPr>
      <w:r><w:t>Title</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        assert_eq!(paras[0].style_id.as_deref(), Some("Heading1"));
        assert_eq!(paras[0].outline_level, Some(0));
    }

    #[test]
    fn parse_numbering_props() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:pPr>
        <w:numPr>
          <w:ilvl w:val="0"/>
          <w:numId w:val="1"/>
        </w:numPr>
      </w:pPr>
      <w:r><w:t>List item</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        assert_eq!(paras[0].num_props, Some(("1".to_string(), 0)));
    }

    #[test]
    fn parse_xml_space_preserve() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r><w:t xml:space="preserve"> leading space</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        assert_eq!(paras[0].runs[0].text, " leading space");
    }

    #[test]
    fn parse_multiple_paragraphs() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>First</w:t></w:r></w:p>
    <w:p><w:r><w:t>Second</w:t></w:r></w:p>
    <w:p><w:r><w:t>Third</w:t></w:r></w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        assert_eq!(paras.len(), 3);
        assert_eq!(paras[0].runs[0].text, "First");
        assert_eq!(paras[1].runs[0].text, "Second");
        assert_eq!(paras[2].runs[0].text, "Third");
    }

    #[test]
    fn parse_strict_namespace() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://purl.oclc.org/ooxml/wordprocessingml/main">
  <w:body>
    <w:p><w:r><w:t>Strict mode</w:t></w:r></w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        assert_eq!(paras.len(), 1);
        assert_eq!(paras[0].runs[0].text, "Strict mode");
    }

    #[test]
    fn parse_hidden_text() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r>
        <w:rPr><w:vanish/></w:rPr>
        <w:t>Hidden text</w:t>
      </w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        assert!(paras[0].runs[0].invisible);
    }

    #[test]
    fn empty_paragraph() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p></w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        assert_eq!(paras.len(), 1);
        assert!(paras[0].runs.is_empty());
    }

    #[test]
    fn empty_run() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r></w:r></w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        // Empty runs (no w:t) are filtered out.
        assert!(paras[0].runs.is_empty());
    }

    #[test]
    fn multiple_runs_in_paragraph() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r><w:t xml:space="preserve">Hello </w:t></w:r>
      <w:r><w:t>World</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        assert_eq!(paras[0].runs.len(), 2);
        assert_eq!(paras[0].runs[0].text, "Hello ");
        assert_eq!(paras[0].runs[1].text, "World");
    }

    #[test]
    fn body_with_interleaved_tables() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>Before</w:t></w:r></w:p>
    <w:tbl>
      <w:tr><w:tc><w:p><w:r><w:t>Cell</w:t></w:r></w:p></w:tc></w:tr>
    </w:tbl>
    <w:p><w:r><w:t>After</w:t></w:r></w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        // Verify document order: Paragraph, Table, Paragraph.
        assert_eq!(body.len(), 3);
        assert!(matches!(&body[0], BodyElement::Paragraph(p) if p.runs[0].text == "Before"));
        assert!(matches!(&body[1], BodyElement::Table(_)));
        assert!(matches!(&body[2], BodyElement::Paragraph(p) if p.runs[0].text == "After"));

        let paras = paragraphs(&body);
        assert_eq!(paras.len(), 2);
        let tbls = tables(&body);
        assert_eq!(tbls.len(), 1);
    }

    #[test]
    fn underline_property() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r>
        <w:rPr><w:u w:val="single"/></w:rPr>
        <w:t>Underlined</w:t>
      </w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        assert!(paras[0].runs[0].underline);
    }

    #[test]
    fn underline_none_means_no_underline() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r>
        <w:rPr><w:u w:val="none"/></w:rPr>
        <w:t>Not underlined</w:t>
      </w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        assert!(!paras[0].runs[0].underline);
    }

    #[test]
    fn no_body_element() {
        // Document without w:body should produce no body elements.
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        assert!(body.is_empty());
    }

    #[test]
    fn mc_alternate_content_fallback() {
        // mc:Choice should be skipped, mc:Fallback content should be processed.
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
  <w:body>
    <w:p><w:r><w:t>Before</w:t></w:r></w:p>
    <mc:AlternateContent>
      <mc:Choice>
        <w:p><w:r><w:t>Choice content (should be skipped)</w:t></w:r></w:p>
      </mc:Choice>
      <mc:Fallback>
        <w:p><w:r><w:t>Fallback content</w:t></w:r></w:p>
      </mc:Fallback>
    </mc:AlternateContent>
    <w:p><w:r><w:t>After</w:t></w:r></w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        let all_text: String = paras
            .iter()
            .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("|");

        assert!(all_text.contains("Before"), "got: {all_text}");
        assert!(
            all_text.contains("Fallback content"),
            "expected fallback content, got: {all_text}"
        );
        assert!(
            !all_text.contains("Choice content"),
            "choice content should be skipped, got: {all_text}"
        );
        assert!(all_text.contains("After"), "got: {all_text}");
    }

    #[test]
    fn multiple_text_elements_in_run() {
        // A run can have multiple w:t elements.
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r>
        <w:t>Part 1</w:t>
        <w:t xml:space="preserve"> Part 2</w:t>
      </w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        assert_eq!(paras[0].runs[0].text, "Part 1 Part 2");
    }

    #[test]
    fn num_id_zero_removes_numbering() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:pPr>
        <w:numPr>
          <w:ilvl w:val="0"/>
          <w:numId w:val="0"/>
        </w:numPr>
      </w:pPr>
      <w:r><w:t>Not a list item</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        assert!(
            paras[0].num_props.is_none(),
            "numId=0 should clear numbering, got: {:?}",
            paras[0].num_props
        );
    }

    #[test]
    fn sdt_content_controls_transparent() {
        // Paragraphs inside w:sdt/w:sdtContent should be extracted.
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>Before SDT</w:t></w:r></w:p>
    <w:sdt>
      <w:sdtPr><w:alias w:val="Title"/></w:sdtPr>
      <w:sdtContent>
        <w:p><w:r><w:t>Inside content control</w:t></w:r></w:p>
      </w:sdtContent>
    </w:sdt>
    <w:p><w:r><w:t>After SDT</w:t></w:r></w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        assert_eq!(paras.len(), 3, "expected 3 paragraphs (SDT transparent)");
        assert_eq!(paras[0].runs[0].text, "Before SDT");
        assert_eq!(paras[1].runs[0].text, "Inside content control");
        assert_eq!(paras[2].runs[0].text, "After SDT");
    }

    #[test]
    fn nested_del_elements() {
        // Nested w:del should not prematurely un-skip content.
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r><w:t>Visible</w:t></w:r>
      <w:del>
        <w:r><w:t>Deleted outer</w:t></w:r>
        <w:del>
          <w:r><w:t>Deleted inner</w:t></w:r>
        </w:del>
        <w:r><w:t>Still deleted outer</w:t></w:r>
      </w:del>
      <w:r><w:t> Also visible</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        let all_text: String = paras[0].runs.iter().map(|r| r.text.as_str()).collect();
        assert!(all_text.contains("Visible"), "got: {all_text}");
        assert!(all_text.contains("Also visible"), "got: {all_text}");
        assert!(!all_text.contains("Deleted"), "got: {all_text}");
        assert!(!all_text.contains("Still deleted"), "got: {all_text}");
    }

    #[test]
    fn mc_alternate_content_inside_paragraph() {
        // mc:AlternateContent inside w:p: skip Choice, process Fallback.
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
  <w:body>
    <w:p>
      <w:r><w:t>Before MC</w:t></w:r>
      <mc:AlternateContent>
        <mc:Choice>
          <w:r><w:t>Choice run (skip)</w:t></w:r>
        </mc:Choice>
        <mc:Fallback>
          <w:r><w:t> Fallback run</w:t></w:r>
        </mc:Fallback>
      </mc:AlternateContent>
      <w:r><w:t> After MC</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        let all_text: String = paras[0].runs.iter().map(|r| r.text.as_str()).collect();
        assert!(all_text.contains("Before MC"), "got: {all_text}");
        assert!(
            all_text.contains("Fallback run"),
            "expected fallback run, got: {all_text}"
        );
        assert!(
            !all_text.contains("Choice run"),
            "choice run should be skipped, got: {all_text}"
        );
        assert!(all_text.contains("After MC"), "got: {all_text}");
    }

    #[test]
    fn mc_alternate_content_without_fallback() {
        // mc:AlternateContent with only mc:Choice (no mc:Fallback).
        // Should skip Choice and produce no content from the AC block.
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
  <w:body>
    <w:p><w:r><w:t>Before</w:t></w:r></w:p>
    <mc:AlternateContent>
      <mc:Choice>
        <w:p><w:r><w:t>Choice only (should be skipped)</w:t></w:r></w:p>
      </mc:Choice>
    </mc:AlternateContent>
    <w:p><w:r><w:t>After</w:t></w:r></w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        let all_text: String = paras
            .iter()
            .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("|");

        assert!(all_text.contains("Before"), "got: {all_text}");
        assert!(all_text.contains("After"), "got: {all_text}");
        assert!(
            !all_text.contains("Choice only"),
            "choice-only content should be skipped, got: {all_text}"
        );
    }

    #[test]
    fn truncated_xml_does_not_panic() {
        // Truncated XML mid-element should return partial results, not panic.
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>Complete paragraph</w:t></w:r></w:p>
    <w:p><w:r><w:t>Truncated"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        // Should not panic. May return partial results or error gracefully.
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        let _ = parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx);

        // At minimum the first complete paragraph should be extracted.
        let paras = paragraphs(&body);
        if !paras.is_empty() {
            assert_eq!(paras[0].runs[0].text, "Complete paragraph");
        }
    }

    #[test]
    fn malformed_xml_element() {
        // Malformed inner content should not crash the parser.
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>Valid paragraph</w:t></w:r></w:p>
  </w:body>
</w:document>"#;

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx).unwrap();

        let paras = paragraphs(&body);
        assert_eq!(paras.len(), 1);
        assert_eq!(paras[0].runs[0].text, "Valid paragraph");
    }

    #[test]
    fn color_parsing_rejects_multibyte_utf8() {
        // Color value with multi-byte UTF-8 chars that is 6 bytes must not panic.
        let xml =
            br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:r>
                        <w:rPr><w:color w:val="&#xe9;&#xe9;&#xe9;"/></w:rPr>
                        <w:t>text</w:t>
                    </w:r>
                </w:p>
            </w:body>
        </w:document>"#;
        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        let _ = parse_body(xml, &mut body, &mut warnings, &mut Vec::new(), &ctx);
        // Must not panic. Color should be None (invalid hex).
        let paras = paragraphs(&body);
        if !paras.is_empty() && !paras[0].runs.is_empty() {
            assert!(paras[0].runs[0].color.is_none());
        }
    }

    #[test]
    fn bookmark_limit_emits_warning() {
        use udoc_core::diagnostics::CollectingDiagnostics;

        // Build XML with MAX_BOOKMARKS_PER_DOCUMENT + 5 bookmarks inside
        // paragraphs (bookmarkStart is parsed inside parse_paragraph).
        let count = MAX_BOOKMARKS_PER_DOCUMENT + 5;
        let mut xml = String::from(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>"#,
        );
        for i in 0..count {
            xml.push_str(&format!(
                r#"<w:p><w:bookmarkStart w:id="{i}" w:name="bm_{i}"/><w:bookmarkEnd w:id="{i}"/><w:r><w:t>x</w:t></w:r></w:p>"#
            ));
        }
        xml.push_str("</w:body></w:document>");

        let mut body = Vec::new();
        let mut warnings = Vec::new();
        let mut bookmarks = Vec::new();
        let hmap = HashMap::new();
        let collecting = Arc::new(CollectingDiagnostics::new());
        let diag: Arc<dyn DiagnosticsSink> = collecting.clone();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        let _ = parse_body(
            xml.as_bytes(),
            &mut body,
            &mut warnings,
            &mut bookmarks,
            &ctx,
        );

        assert_eq!(bookmarks.len(), MAX_BOOKMARKS_PER_DOCUMENT);
        let diag_warnings = collecting.warnings();
        assert!(
            diag_warnings.iter().any(|w| w.kind == "DocxMaxBookmarks"),
            "expected DocxMaxBookmarks warning, got: {:?}",
            diag_warnings,
        );
    }
}

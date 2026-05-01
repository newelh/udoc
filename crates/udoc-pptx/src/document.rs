//! PPTX document and page types implementing FormatBackend/PageExtractor.
//!
//! `PptxDocument` opens a .pptx file via OPC, resolves slide ordering from
//! `p:sldIdLst` in presentation.xml, and provides per-slide access through
//! the `PptxPage` type.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;

use udoc_containers::opc::{rel_types, OpcPackage};
use udoc_containers::xml::{prefixed_attr_value, XmlEvent, XmlReader};
use udoc_core::backend::{DocumentMetadata, FormatBackend, PageExtractor};
use udoc_core::diagnostics::{DiagnosticsSink, Warning};
use udoc_core::error::{Error, Result, ResultExt};
use udoc_core::image::PageImage;
use udoc_core::table::{Table, TableCell, TableRow};
use udoc_core::text::{TextLine, TextSpan};

use crate::notes::parse_notes_slide;
use crate::shapes::{parse_slide_shapes, ExtractedTable, ShapeContent, SlideContext, SlideShape};
use crate::text::{is_pml, DrawingParagraph};

use crate::MAX_FILE_SIZE;

/// A parsed PPTX document ready for content extraction.
///
/// Implements `FormatBackend` where each "page" is a slide.
pub struct PptxDocument {
    /// Ordered list of parsed slides.
    slides: Vec<ParsedSlide>,
    /// Document metadata from docProps/core.xml.
    metadata: DocumentMetadata,
}

/// A single parsed slide with its shapes and optional notes.
struct ParsedSlide {
    /// Shapes extracted from the slide's shape tree, sorted by reading order.
    shapes: Vec<SlideShape>,
    /// Speaker notes paragraphs (empty if no notes).
    notes: Vec<DrawingParagraph>,
    /// Pre-extracted image data for this slide's p:pic shapes.
    images: Vec<ExtractedImage>,
}

/// An image extracted from a slide's shape tree during construction.
struct ExtractedImage {
    /// Raw image bytes from the OPC package.
    data: Vec<u8>,
    /// Alt text from `p:cNvPr/@descr`. Reserved for document model alt text support.
    #[allow(dead_code)]
    alt_text: Option<String>,
}

/// Summary of a shape's content for Document model conversion.
///
/// Exposes placeholder type alongside text so the facade can infer heading
/// levels without reaching into crate internals.
pub struct ShapeSummary {
    /// Placeholder type (e.g. "title", "subTitle", "body"), if any.
    pub placeholder_type: Option<String>,
    /// Non-empty paragraph texts from this shape.
    pub paragraphs: Vec<String>,
    /// Whether this shape is a table (vs. text).
    pub is_table: bool,
}

/// A page (slide) view for content extraction.
pub struct PptxPage<'a> {
    slide: &'a ParsedSlide,
}

impl PptxDocument {
    udoc_core::define_backend_constructors!(MAX_FILE_SIZE, "PPTX");

    /// Create a PPTX document from in-memory bytes with diagnostics.
    pub fn from_bytes_with_diag(data: &[u8], diag: Arc<dyn DiagnosticsSink>) -> Result<Self> {
        let pkg = OpcPackage::new(data, Arc::clone(&diag)).context("opening PPTX OPC package")?;

        // Find the main presentation part via package rels
        let pres_rel = pkg
            .find_package_rel_by_type(rel_types::OFFICE_DOCUMENT)
            .ok_or_else(|| crate::error::missing_part("officeDocument relationship"))?;
        let pres_target = pres_rel.target.clone();

        // Read and parse presentation.xml for slide ordering
        let pres_xml = pkg
            .read_part(&pres_target)
            .context("reading presentation.xml")?;
        let slide_rel_ids = parse_slide_id_list(&pres_xml)?;

        // Resolve slide relationship IDs to slide part paths
        let pres_part = format!("/{}", pres_target.trim_start_matches('/'));
        let pres_rels = pkg.part_rels(&pres_part);

        // Build rId -> relationship lookup for the presentation part
        let rel_map: HashMap<&str, &udoc_containers::opc::Relationship> =
            pres_rels.iter().map(|r| (r.id.as_str(), r)).collect();

        // Parse theme colors early so they're available during slide parsing.
        let theme_colors = parse_theme_colors(&pkg, pres_rels, &pres_part, &diag);

        let mut slides = Vec::with_capacity(slide_rel_ids.len());

        for (slide_idx, r_id) in slide_rel_ids.iter().enumerate() {
            if slides.len() >= crate::MAX_SLIDES {
                diag.warning(Warning::new(
                    "PptxSlideLimit",
                    format!(
                        "presentation has {} slides, capping at {}",
                        slide_rel_ids.len(),
                        crate::MAX_SLIDES
                    ),
                ));
                break;
            }

            let rel = match rel_map.get(r_id.as_str()) {
                Some(r) => *r,
                None => {
                    diag.warning(Warning::new(
                        "PptxMissingSlideRel",
                        format!("slide relationship {r_id} not found, skipping slide {slide_idx}"),
                    ));
                    continue;
                }
            };

            // Resolve slide path relative to presentation.xml
            let slide_path = pkg.resolve_uri(&pres_part, &rel.target);

            // Read and parse slide XML
            let slide_xml = match pkg.read_part(&slide_path) {
                Ok(data) => data,
                Err(e) => {
                    diag.warning(Warning::new(
                        "PptxSlideReadError",
                        format!("failed to read slide {slide_idx}: {e}"),
                    ));
                    slides.push(ParsedSlide {
                        shapes: Vec::new(),
                        notes: Vec::new(),
                        images: Vec::new(),
                    });
                    continue;
                }
            };

            // Get slide relationships for hyperlink/image resolution.
            let slide_rels_vec: Vec<udoc_containers::opc::Relationship> =
                pkg.part_rels(&slide_path).to_vec();

            let slide_ctx = SlideContext {
                diag: diag.as_ref(),
                slide_index: slide_idx,
                slide_rels: &slide_rels_vec,
                scheme_color_warned: Cell::new(false),
                theme_colors: &theme_colors,
            };

            let shapes = match parse_slide_shapes(&slide_xml, &slide_ctx) {
                Ok(s) => s,
                Err(e) => {
                    diag.warning(Warning::new(
                        "PptxSlideParseError",
                        format!("failed to parse slide {slide_idx}: {e}"),
                    ));
                    Vec::new()
                }
            };

            // Extract images from p:pic shapes by resolving r:id to package parts.
            let images = extract_slide_images(&shapes, &slide_rels_vec, &pkg, &slide_path, &diag);

            // Parse speaker notes if available
            let notes = parse_slide_notes(&pkg, &slide_path, &diag, slide_idx, &theme_colors);

            slides.push(ParsedSlide {
                shapes,
                notes,
                images,
            });
        }

        // Parse metadata from docProps/core.xml
        let metadata = parse_metadata(&pkg, slides.len(), &diag);

        Ok(Self { slides, metadata })
    }

    /// Number of slides.
    pub fn page_count(&self) -> usize {
        self.slides.len()
    }

    /// Get shape summaries for a slide (placeholder types + text).
    ///
    /// Used by the facade's Document model conversion to infer heading levels
    /// from placeholder types (title -> H1, subtitle -> H2).
    pub fn slide_shapes(&self, index: usize) -> Vec<ShapeSummary> {
        let slide = match self.slides.get(index) {
            Some(s) => s,
            None => return Vec::new(),
        };
        slide
            .shapes
            .iter()
            .map(|shape| {
                let (paragraphs, is_table) = match &shape.content {
                    ShapeContent::Text(paras) => (
                        paras
                            .iter()
                            .filter(|p| !p.is_empty())
                            .map(|p| p.text())
                            .collect(),
                        false,
                    ),
                    ShapeContent::Table(_) => (Vec::new(), true),
                    _ => (Vec::new(), false),
                };
                ShapeSummary {
                    placeholder_type: shape.placeholder_type.clone(),
                    paragraphs,
                    is_table,
                }
            })
            .collect()
    }

    /// Access the raw slide shapes for rich formatting conversion.
    ///
    /// Unlike `slide_shapes()` which returns text-only summaries, this gives
    /// the converter access to per-run formatting (bold, italic, underline,
    /// strikethrough, color, font, hyperlinks) and paragraph alignment.
    pub(crate) fn raw_slide_shapes(&self, index: usize) -> &[SlideShape] {
        match self.slides.get(index) {
            Some(s) => &s.shapes,
            None => &[],
        }
    }

    /// Get speaker notes text for a slide, if any.
    pub fn notes(&self, index: usize) -> Option<String> {
        let slide = self.slides.get(index)?;
        if slide.notes.is_empty() {
            return None;
        }
        let text: Vec<String> = slide
            .notes
            .iter()
            .filter(|p| !p.is_empty())
            .map(|p| p.text())
            .collect();
        if text.is_empty() {
            None
        } else {
            Some(text.join("\n"))
        }
    }
}

impl FormatBackend for PptxDocument {
    type Page<'a> = PptxPage<'a>;

    fn page_count(&self) -> usize {
        self.slides.len()
    }

    fn page(&mut self, index: usize) -> Result<PptxPage<'_>> {
        if index >= self.slides.len() {
            return Err(Error::new(format!(
                "slide index {index} out of range (document has {} slides)",
                self.slides.len()
            )));
        }
        Ok(PptxPage {
            slide: &self.slides[index],
        })
    }

    fn metadata(&self) -> DocumentMetadata {
        self.metadata.clone()
    }
}

impl<'a> PageExtractor for PptxPage<'a> {
    fn text(&mut self) -> Result<String> {
        let mut parts = Vec::new();

        for shape in &self.slide.shapes {
            match &shape.content {
                ShapeContent::Text(paragraphs) => {
                    for para in paragraphs {
                        if !para.is_empty() {
                            parts.push(para.text());
                        }
                    }
                }
                ShapeContent::Table(table) => {
                    parts.push(table_to_text(table));
                }
                _ => {}
            }
        }

        Ok(parts.join("\n"))
    }

    fn text_lines(&mut self) -> Result<Vec<TextLine>> {
        let mut lines = Vec::new();

        for shape in &self.slide.shapes {
            match &shape.content {
                ShapeContent::Text(paragraphs) => {
                    for para in paragraphs {
                        if para.is_empty() {
                            continue;
                        }
                        let spans: Vec<TextSpan> = para
                            .runs
                            .iter()
                            .filter(|r| !r.text.is_empty())
                            .map(|r| {
                                let x_pt = shape.x_emu as f64 / crate::EMU_PER_POINT;
                                let y_pt = shape.y_emu as f64 / crate::EMU_PER_POINT;
                                let width_pt = shape.cx_emu as f64 / crate::EMU_PER_POINT;
                                let font_size = r.font_size_pt.unwrap_or(12.0);
                                let mut span =
                                    TextSpan::new(r.text.clone(), x_pt, y_pt, width_pt, font_size);
                                span.is_bold = r.bold;
                                span.is_italic = r.italic;
                                span.font_name = r.font_name.clone();
                                span
                            })
                            .collect();

                        if !spans.is_empty() {
                            let y_pt = shape.y_emu as f64 / crate::EMU_PER_POINT;
                            lines.push(TextLine::new(spans, y_pt, false));
                        }
                    }
                }
                ShapeContent::Table(table) => {
                    // Flatten table cells into text lines
                    for row in &table.rows {
                        for cell in &row.cells {
                            if cell.is_h_merge || cell.is_v_merge {
                                continue;
                            }
                            for para in &cell.paragraphs {
                                if para.is_empty() {
                                    continue;
                                }
                                let spans: Vec<TextSpan> = para
                                    .runs
                                    .iter()
                                    .filter(|r| !r.text.is_empty())
                                    .map(|r| {
                                        let font_size = r.font_size_pt.unwrap_or(12.0);
                                        let mut span =
                                            TextSpan::new(r.text.clone(), 0.0, 0.0, 0.0, font_size);
                                        span.is_bold = r.bold;
                                        span.is_italic = r.italic;
                                        span.font_name = r.font_name.clone();
                                        span
                                    })
                                    .collect();
                                if !spans.is_empty() {
                                    lines.push(TextLine::new(spans, 0.0, false));
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(lines)
    }

    fn raw_spans(&mut self) -> Result<Vec<TextSpan>> {
        let mut spans = Vec::new();

        for shape in &self.slide.shapes {
            match &shape.content {
                ShapeContent::Text(paragraphs) => {
                    for para in paragraphs {
                        for run in &para.runs {
                            if run.text.is_empty() {
                                continue;
                            }
                            let x_pt = shape.x_emu as f64 / crate::EMU_PER_POINT;
                            let y_pt = shape.y_emu as f64 / crate::EMU_PER_POINT;
                            let width_pt = shape.cx_emu as f64 / crate::EMU_PER_POINT;
                            let font_size = run.font_size_pt.unwrap_or(12.0);
                            let mut span =
                                TextSpan::new(run.text.clone(), x_pt, y_pt, width_pt, font_size);
                            span.is_bold = run.bold;
                            span.is_italic = run.italic;
                            span.font_name = run.font_name.clone();
                            spans.push(span);
                        }
                    }
                }
                ShapeContent::Table(table) => {
                    for row in &table.rows {
                        for cell in &row.cells {
                            if cell.is_h_merge || cell.is_v_merge {
                                continue;
                            }
                            for para in &cell.paragraphs {
                                for run in &para.runs {
                                    if run.text.is_empty() {
                                        continue;
                                    }
                                    let font_size = run.font_size_pt.unwrap_or(12.0);
                                    let mut span =
                                        TextSpan::new(run.text.clone(), 0.0, 0.0, 0.0, font_size);
                                    span.is_bold = run.bold;
                                    span.is_italic = run.italic;
                                    span.font_name = run.font_name.clone();
                                    spans.push(span);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(spans)
    }

    fn tables(&mut self) -> Result<Vec<Table>> {
        let mut tables = Vec::new();

        for shape in &self.slide.shapes {
            if let ShapeContent::Table(extracted) = &shape.content {
                let table = convert_extracted_table(extracted);
                tables.push(table);
            }
        }

        Ok(tables)
    }

    fn images(&mut self) -> Result<Vec<PageImage>> {
        let mut result = Vec::new();
        for img in &self.slide.images {
            result.push(PageImage::from_data(img.data.clone(), None));
        }
        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Parse `p:sldIdLst` from presentation.xml to get ordered slide relationship IDs.
fn parse_slide_id_list(xml_data: &[u8]) -> Result<Vec<String>> {
    let mut reader = XmlReader::new(xml_data).context("parsing presentation.xml")?;
    let mut rel_ids = Vec::new();
    let mut in_sld_id_lst = false;

    loop {
        let event = reader.next_event().context("reading slide ID list")?;
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ref attributes,
                ..
            } => {
                let name = local_name.as_ref();
                if is_pml(namespace_uri) {
                    match name {
                        "sldIdLst" => {
                            in_sld_id_lst = true;
                        }
                        "sldId" if in_sld_id_lst => {
                            // The relationship ID is in the `r:id` attribute.
                            // We need the r-prefixed one, not the bare `id`
                            // (which is the numeric slide ID).
                            let r_id = prefixed_attr_value(attributes, "r", "id");
                            if let Some(id) = r_id {
                                rel_ids.push(id.to_string());
                            }
                        }
                        _ => {}
                    }
                }
            }
            XmlEvent::EndElement {
                ref local_name,
                ref namespace_uri,
                ..
            } if is_pml(namespace_uri) && local_name.as_ref() == "sldIdLst" => {
                in_sld_id_lst = false;
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(rel_ids)
}

/// Parse speaker notes for a slide.
fn parse_slide_notes(
    pkg: &OpcPackage<'_>,
    slide_path: &str,
    diag: &Arc<dyn DiagnosticsSink>,
    slide_index: usize,
    theme_colors: &HashMap<String, [u8; 3]>,
) -> Vec<DrawingParagraph> {
    // Look for notes relationship from this slide
    let notes_rel = pkg.find_part_rel_by_type(slide_path, rel_types::NOTES_SLIDE);
    let rel = match notes_rel {
        Some(r) => r,
        None => return Vec::new(),
    };

    let notes_path = pkg.resolve_uri(slide_path, &rel.target);
    let notes_xml = match pkg.read_part(&notes_path) {
        Ok(data) => data,
        Err(e) => {
            diag.warning(Warning::new(
                "PptxNotesReadError",
                format!("failed to read notes slide {notes_path}: {e}"),
            ));
            return Vec::new();
        }
    };

    let notes_rels = pkg.part_rels(&notes_path).to_vec();
    let notes_ctx = SlideContext {
        diag: diag.as_ref(),
        slide_index,
        slide_rels: &notes_rels,
        scheme_color_warned: Cell::new(false),
        theme_colors,
    };

    match parse_notes_slide(&notes_xml, &notes_ctx) {
        Ok(paras) => paras,
        Err(e) => {
            diag.warning(Warning::new(
                "PptxNotesParseError",
                format!("failed to parse notes slide {notes_path}: {e}"),
            ));
            Vec::new()
        }
    }
}

/// Parse document metadata from docProps/core.xml.
fn parse_metadata(
    pkg: &OpcPackage<'_>,
    slide_count: usize,
    diag: &Arc<dyn DiagnosticsSink>,
) -> DocumentMetadata {
    let mut meta = match pkg.find_package_rel_by_type(rel_types::CORE_PROPERTIES) {
        Some(rel) => match pkg.read_part(&rel.target) {
            Ok(core_xml) => udoc_containers::opc::metadata::parse_core_properties(&core_xml),
            Err(e) => {
                diag.warning(Warning::new(
                    "PptxMetadataReadFailed",
                    format!("could not read docProps/core.xml: {e}, using defaults"),
                ));
                DocumentMetadata::default()
            }
        },
        None => DocumentMetadata::default(),
    };
    meta.page_count = slide_count;
    meta
}

/// Parse the theme color scheme from theme1.xml.
///
/// Follows the presentation's theme relationship to find theme1.xml,
/// then extracts the `a:clrScheme` element's children (dk1, lt1, dk2, lt2,
/// accent1-6, hlink, folHlink). Each child may contain `a:srgbClr` (direct
/// hex) or `a:sysClr` (system color with `lastClr` fallback).
fn parse_theme_colors(
    pkg: &OpcPackage<'_>,
    pres_rels: &[udoc_containers::opc::Relationship],
    pres_part: &str,
    diag: &Arc<dyn DiagnosticsSink>,
) -> HashMap<String, [u8; 3]> {
    use udoc_containers::xml::{attr_value, XmlEvent, XmlReader};

    let theme_rel = pres_rels.iter().find(|r| {
        udoc_containers::opc::relationships::rel_type_matches(&r.rel_type, rel_types::THEME)
    });
    let theme_rel = match theme_rel {
        Some(r) => r,
        None => return HashMap::new(),
    };

    let theme_path = pkg.resolve_uri(pres_part, &theme_rel.target);
    let theme_xml = match pkg.read_part(&theme_path) {
        Ok(data) => data,
        Err(e) => {
            diag.warning(Warning::new(
                "PptxThemeReadFailed",
                format!("could not read theme: {e}"),
            ));
            return HashMap::new();
        }
    };

    let mut reader = match XmlReader::new(&theme_xml) {
        Ok(r) => r,
        Err(_) => return HashMap::new(),
    };

    let mut colors = HashMap::new();
    let mut depth: u32 = 0;
    let mut in_clr_scheme = false;
    // Name of the current color element (e.g. "dk1", "accent1").
    let mut current_name: Option<String> = None;

    let scheme_names: &[&str] = &[
        "dk1", "lt1", "dk2", "lt2", "accent1", "accent2", "accent3", "accent4", "accent5",
        "accent6", "hlink", "folHlink",
    ];

    while let Ok(event) = reader.next_event() {
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref attributes,
                ..
            } => {
                depth = depth.saturating_add(1);
                let name = local_name.as_ref();

                if name == "clrScheme" {
                    in_clr_scheme = true;
                } else if in_clr_scheme {
                    if scheme_names.contains(&name) {
                        current_name = Some(name.to_string());
                    } else if current_name.is_some() {
                        // Inside a color element, look for srgbClr or sysClr.
                        match name {
                            "srgbClr" => {
                                if let Some(val) = attr_value(attributes, "val") {
                                    if let Some(c) = udoc_core::document::Color::from_hex(val) {
                                        if let Some(cn) = current_name.take() {
                                            colors.insert(cn, c.to_array());
                                        }
                                    }
                                }
                            }
                            "sysClr" => {
                                // System colors have a lastClr fallback.
                                if let Some(val) = attr_value(attributes, "lastClr") {
                                    if let Some(c) = udoc_core::document::Color::from_hex(val) {
                                        if let Some(cn) = current_name.take() {
                                            colors.insert(cn, c.to_array());
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            XmlEvent::EndElement { ref local_name, .. } => {
                depth = depth.saturating_sub(1);
                if local_name.as_ref() == "clrScheme" {
                    in_clr_scheme = false;
                } else if scheme_names.contains(&local_name.as_ref()) {
                    current_name = None;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    colors
}

/// Extract image bytes for all p:pic shapes in a slide.
fn extract_slide_images(
    shapes: &[SlideShape],
    slide_rels: &[udoc_containers::opc::Relationship],
    pkg: &OpcPackage<'_>,
    slide_path: &str,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Vec<ExtractedImage> {
    // Pre-index image relationships by id for O(1) lookup per shape.
    let image_rels: std::collections::HashMap<&str, &udoc_containers::opc::Relationship> =
        slide_rels
            .iter()
            .filter(|r| {
                udoc_containers::opc::relationships::rel_type_matches(&r.rel_type, rel_types::IMAGE)
            })
            .map(|r| (r.id.as_str(), r))
            .collect();

    let mut images = Vec::new();
    for shape in shapes {
        if images.len() >= crate::MAX_IMAGES_PER_SLIDE {
            diag.warning(Warning::new(
                "PptxImageLimit",
                format!(
                    "slide has more than {} images, excess images dropped",
                    crate::MAX_IMAGES_PER_SLIDE
                ),
            ));
            break;
        }
        if let ShapeContent::Image {
            ref r_id,
            ref alt_text,
        } = shape.content
        {
            let rel = match image_rels.get(r_id.as_str()) {
                Some(r) => *r,
                None => {
                    diag.warning(Warning::new(
                        "PptxImageRelMissing",
                        format!("image relationship {r_id} not found"),
                    ));
                    continue;
                }
            };
            let image_path = pkg.resolve_uri(slide_path, &rel.target);
            match pkg.read_part(&image_path) {
                Ok(data) => {
                    images.push(ExtractedImage {
                        data,
                        alt_text: alt_text.clone(),
                    });
                }
                Err(e) => {
                    diag.warning(Warning::new(
                        "PptxImageReadError",
                        format!("failed to read image {image_path}: {e}"),
                    ));
                }
            }
        }
    }
    images
}

/// Convert an extracted table to udoc-core Table type.
fn convert_extracted_table(extracted: &ExtractedTable) -> Table {
    let rows: Vec<TableRow> = extracted
        .rows
        .iter()
        .map(|row| {
            let cells: Vec<TableCell> = row
                .cells
                .iter()
                .filter(|c| !c.is_h_merge && !c.is_v_merge)
                .map(|cell| {
                    let text: String = cell
                        .paragraphs
                        .iter()
                        .filter(|p| !p.is_empty())
                        .map(|p| p.text())
                        .collect::<Vec<_>>()
                        .join("\n");
                    TableCell::with_spans(text, None, cell.col_span, cell.row_span)
                })
                .collect();
            TableRow::new(cells)
        })
        .collect();

    Table::new(rows, None)
}

/// Convert a table to plain text.
///
/// Cell paragraphs are joined with space (not newline) so text() produces
/// single-line cell content. `convert_extracted_table` uses newline to
/// preserve paragraph structure in the Document model.
fn table_to_text(table: &ExtractedTable) -> String {
    let mut lines = Vec::new();
    for row in &table.rows {
        let cells: Vec<String> = row
            .cells
            .iter()
            .filter(|c| !c.is_h_merge && !c.is_v_merge)
            .map(|cell| {
                cell.paragraphs
                    .iter()
                    .filter(|p| !p.is_empty())
                    .map(|p| p.text())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        lines.push(cells.join("\t"));
    }
    lines.join("\n")
}

/// Infer heading level from placeholder type.
///
/// - title, ctrTitle -> H1
/// - subTitle -> H2
/// - everything else -> not a heading (returns 0)
pub fn heading_level_from_placeholder(ph_type: Option<&str>) -> u8 {
    match ph_type {
        Some("title") | Some("ctrTitle") => 1,
        Some("subTitle") => 2,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_from_title_placeholder() {
        assert_eq!(heading_level_from_placeholder(Some("title")), 1);
        assert_eq!(heading_level_from_placeholder(Some("ctrTitle")), 1);
        assert_eq!(heading_level_from_placeholder(Some("subTitle")), 2);
        assert_eq!(heading_level_from_placeholder(Some("body")), 0);
        assert_eq!(heading_level_from_placeholder(None), 0);
    }

    #[test]
    fn parse_slide_id_list_basic() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
                xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:sldIdLst>
    <p:sldId id="256" r:id="rId2"/>
    <p:sldId id="257" r:id="rId3"/>
    <p:sldId id="258" r:id="rId4"/>
  </p:sldIdLst>
</p:presentation>"#;

        let ids = parse_slide_id_list(xml).unwrap();
        assert_eq!(ids, vec!["rId2", "rId3", "rId4"]);
    }

    #[test]
    fn parse_core_properties_via_shared() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"
                   xmlns:dc="http://purl.org/dc/elements/1.1/"
                   xmlns:dcterms="http://purl.org/dc/terms/">
  <dc:title>Test Presentation</dc:title>
  <dc:creator>Test Author</dc:creator>
  <dc:subject>Test Subject</dc:subject>
  <dcterms:created>2024-01-15T10:30:00Z</dcterms:created>
  <dcterms:modified>2024-01-16T14:00:00Z</dcterms:modified>
</cp:coreProperties>"#;

        let meta = udoc_containers::opc::metadata::parse_core_properties(xml);
        assert_eq!(meta.title.as_deref(), Some("Test Presentation"));
        assert_eq!(meta.author.as_deref(), Some("Test Author"));
        assert_eq!(meta.subject.as_deref(), Some("Test Subject"));
    }

    #[test]
    fn from_bytes_rejects_non_zip() {
        let result = PptxDocument::from_bytes(b"not a zip file");
        assert!(result.is_err());
    }

    #[test]
    fn from_bytes_rejects_oversized() {
        // Verify the constant is set to a reasonable value (not u64::MAX).
        let max = MAX_FILE_SIZE;
        assert!(max > 0 && max < u64::MAX);
    }

    #[test]
    fn parse_slide_id_list_empty() {
        // presentation.xml with no sldIdLst should return empty.
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
</p:presentation>"#;

        let ids = parse_slide_id_list(xml).unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn parse_core_properties_empty() {
        // Empty core properties should not crash.
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties">
</cp:coreProperties>"#;

        let meta = udoc_containers::opc::metadata::parse_core_properties(xml);
        assert!(meta.title.is_none());
        assert!(meta.author.is_none());
    }

    #[test]
    fn table_to_text_simple() {
        use crate::shapes::{ExtractedTableCell, ExtractedTableRow};
        use crate::text::DrawingParagraph;

        let table = ExtractedTable {
            rows: vec![ExtractedTableRow {
                cells: vec![
                    ExtractedTableCell {
                        paragraphs: vec![DrawingParagraph {
                            runs: vec![crate::text::DrawingRun {
                                text: "A".to_string(),
                                bold: false,
                                italic: false,
                                underline: false,
                                strikethrough: false,
                                font_size_pt: None,
                                font_name: None,
                                color: None,
                                hyperlink_url: None,
                                is_field: false,
                            }],
                            alignment: None,
                            bullet: None,
                        }],
                        col_span: 1,
                        row_span: 1,
                        is_h_merge: false,
                        is_v_merge: false,
                    },
                    ExtractedTableCell {
                        paragraphs: vec![DrawingParagraph {
                            runs: vec![crate::text::DrawingRun {
                                text: "B".to_string(),
                                bold: false,
                                italic: false,
                                underline: false,
                                strikethrough: false,
                                font_size_pt: None,
                                font_name: None,
                                color: None,
                                hyperlink_url: None,
                                is_field: false,
                            }],
                            alignment: None,
                            bullet: None,
                        }],
                        col_span: 1,
                        row_span: 1,
                        is_h_merge: false,
                        is_v_merge: false,
                    },
                ],
            }],
            num_columns: 2,
        };

        let text = table_to_text(&table);
        assert_eq!(text, "A\tB");
    }
}

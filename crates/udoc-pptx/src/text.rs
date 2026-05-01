//! DrawingML text extraction from PPTX shape bodies.
//!
//! Parses `p:txBody` / `a:txBody` elements to extract text paragraphs
//! with formatting (bold, italic, font size).

use std::sync::Arc;

#[cfg(test)]
use std::cell::Cell;
use udoc_containers::opc::relationships::rel_type_matches;
use udoc_containers::opc::{rel_types, Relationship};
use udoc_containers::xml::namespace::ns;
use udoc_containers::xml::{attr_value, prefixed_attr_value, Attribute, XmlEvent, XmlReader};
#[cfg(test)]
use udoc_core::diagnostics::NullDiagnostics;
use udoc_core::diagnostics::Warning;
use udoc_core::error::{Result, ResultExt};

use crate::shapes::SlideContext;

/// Bullet type for a DrawingML paragraph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BulletType {
    /// Character bullet (`a:buChar`), e.g. "-", "\u{2022}".
    Char(String),
    /// Auto-numbered bullet (`a:buAutoNum`).
    AutoNum,
    /// Explicitly no bullet (`a:buNone`), overrides inherited bullets.
    None,
}

/// A paragraph extracted from a DrawingML text body.
#[derive(Debug, Clone)]
pub(crate) struct DrawingParagraph {
    pub runs: Vec<DrawingRun>,
    /// Paragraph alignment ("l", "ctr", "r", "just"), if specified.
    pub alignment: Option<String>,
    /// Bullet type, if specified in paragraph properties.
    pub bullet: Option<BulletType>,
}

impl DrawingParagraph {
    /// Concatenate all run text into a single string.
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }

    /// Whether this paragraph has any non-empty text.
    pub fn is_empty(&self) -> bool {
        self.runs.iter().all(|r| r.text.is_empty())
    }
}

/// A text run within a DrawingML paragraph.
#[derive(Debug, Clone)]
pub(crate) struct DrawingRun {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub font_size_pt: Option<f64>,
    pub font_name: Option<String>,
    pub color: Option<[u8; 3]>,
    pub hyperlink_url: Option<String>,
    pub is_field: bool,
}

impl DrawingRun {
    fn new() -> Self {
        Self {
            text: String::new(),
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            font_size_pt: None,
            font_name: None,
            color: None,
            hyperlink_url: None,
            is_field: false,
        }
    }
}

/// Parse text body content from a `txBody` element using an XmlReader.
///
/// The reader should be positioned just after a `StartElement` for
/// `txBody` (either `p:txBody` or `a:txBody`). This function reads
/// until the matching `EndElement`.
///
/// Returns a list of paragraphs with their text runs and formatting.
#[cfg(test)]
pub(crate) fn parse_text_body(reader: &mut XmlReader<'_>) -> Result<Vec<DrawingParagraph>> {
    let mut depth: u32 = 1;
    let empty_theme = std::collections::HashMap::new();
    let ctx = SlideContext {
        diag: &NullDiagnostics,
        slide_index: 0,
        slide_rels: &[],
        scheme_color_warned: Cell::new(false),
        theme_colors: &empty_theme,
    };
    parse_text_body_with_depth(reader, &mut depth, &ctx)
}

/// Parse text body content, using a caller-provided depth tracker.
///
/// When called from shape parsing code that maintains its own depth counter,
/// use this variant so the caller's depth stays in sync.
///
/// `slide_rels` are the OPC relationships for the containing slide, used to
/// resolve hyperlink `r:id` references to URLs.
pub(crate) fn parse_text_body_with_depth(
    reader: &mut XmlReader<'_>,
    depth: &mut u32,
    ctx: &SlideContext<'_>,
) -> Result<Vec<DrawingParagraph>> {
    let mut paragraphs = Vec::new();
    let start_depth = *depth;
    let mut para_limit_warned = false;

    loop {
        let event = reader.next_event().context("reading text body")?;
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ..
            } => {
                *depth = depth.saturating_add(1);

                if is_drawingml(namespace_uri) && local_name.as_ref() == "p" {
                    if paragraphs.len() >= crate::MAX_PARAGRAPHS {
                        if !para_limit_warned {
                            ctx.diag.warning(Warning::new(
                                "PptxParagraphLimit",
                                format!(
                                    "text body exceeds {} paragraph limit",
                                    crate::MAX_PARAGRAPHS
                                ),
                            ));
                            para_limit_warned = true;
                        }
                        skip_element(reader, depth)?;
                    } else {
                        let para = parse_paragraph(reader, depth, ctx)?;
                        paragraphs.push(para);
                    }
                }
            }
            XmlEvent::EndElement { .. } => {
                *depth = depth.saturating_sub(1);
                if *depth < start_depth {
                    break;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(paragraphs)
}

/// Parse a single `a:p` paragraph element.
fn parse_paragraph(
    reader: &mut XmlReader<'_>,
    depth: &mut u32,
    ctx: &SlideContext<'_>,
) -> Result<DrawingParagraph> {
    let mut runs = Vec::new();
    let mut alignment = None;
    let mut bullet = None;
    let para_start_depth = *depth;

    loop {
        let event = reader.next_event().context("reading paragraph")?;
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ref attributes,
                ..
            } => {
                *depth = depth.saturating_add(1);
                let name = local_name.as_ref();

                if is_drawingml(namespace_uri) {
                    match name {
                        "pPr" => {
                            // Parse paragraph properties for alignment.
                            if let Some(algn) = attr_value(attributes, "algn") {
                                match algn {
                                    "l" | "ctr" | "r" | "just" => {
                                        alignment = Some(algn.to_string());
                                    }
                                    _ => {}
                                }
                            }
                            // Parse child elements for bullet type (buChar, buAutoNum,
                            // buNone) and consume the rest (lnSpc, spcBef, etc.).
                            bullet = parse_ppr_children(reader, depth)?;
                        }
                        "r" => {
                            let run = parse_run(reader, depth, false, ctx)?;
                            runs.push(run);
                        }
                        "fld" => {
                            // Field element (slide number, date, etc.)
                            let run = parse_run(reader, depth, true, ctx)?;
                            runs.push(run);
                        }
                        "br" => {
                            // Line break within paragraph
                            let mut run = DrawingRun::new();
                            run.text = "\n".to_string();
                            runs.push(run);
                            skip_element(reader, depth)?;
                        }
                        _ => {}
                    }
                }
            }
            XmlEvent::EndElement { .. } => {
                *depth = depth.saturating_sub(1);
                if *depth < para_start_depth {
                    break;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(DrawingParagraph {
        runs,
        alignment,
        bullet,
    })
}

/// Parse children of `a:pPr` to extract bullet type.
///
/// Returns `Some(BulletType)` if a bullet element (`a:buChar`, `a:buAutoNum`,
/// `a:buNone`) is found, `None` otherwise. Consumes all children of `a:pPr`.
fn parse_ppr_children(reader: &mut XmlReader<'_>, depth: &mut u32) -> Result<Option<BulletType>> {
    let start_depth = *depth;
    let mut bullet = None;

    loop {
        let event = reader
            .next_event()
            .context("reading paragraph properties")?;
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ref attributes,
                ..
            } => {
                *depth = depth.saturating_add(1);

                if is_drawingml(namespace_uri) {
                    match local_name.as_ref() {
                        "buChar" => {
                            let ch = attr_value(attributes, "char").unwrap_or("-").to_string();
                            bullet = Some(BulletType::Char(ch));
                            skip_element(reader, depth)?;
                        }
                        "buAutoNum" => {
                            bullet = Some(BulletType::AutoNum);
                            skip_element(reader, depth)?;
                        }
                        "buNone" => {
                            bullet = Some(BulletType::None);
                            skip_element(reader, depth)?;
                        }
                        _ => {}
                    }
                }
            }
            XmlEvent::EndElement { .. } => {
                *depth = depth.saturating_sub(1);
                if *depth < start_depth {
                    break;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(bullet)
}

/// Parse a single `a:r` (run) or `a:fld` (field) element.
fn parse_run(
    reader: &mut XmlReader<'_>,
    depth: &mut u32,
    is_field: bool,
    ctx: &SlideContext<'_>,
) -> Result<DrawingRun> {
    let mut run = DrawingRun::new();
    run.is_field = is_field;
    let run_start_depth = *depth;

    loop {
        let event = reader.next_event().context("reading text run")?;
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ref attributes,
                ..
            } => {
                *depth = depth.saturating_add(1);
                let name = local_name.as_ref();

                if is_drawingml(namespace_uri) {
                    match name {
                        "rPr" => {
                            parse_run_properties(attributes, &mut run);
                            // Parse child elements for font name, color, hyperlink
                            parse_rpr_children(reader, depth, &mut run, ctx)?;
                        }
                        "t" => {
                            // Collect text content
                            run.text.push_str(&collect_text(reader, depth)?);
                        }
                        _ => {}
                    }
                }
            }
            XmlEvent::EndElement { .. } => {
                *depth = depth.saturating_sub(1);
                if *depth < run_start_depth {
                    break;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(run)
}

/// Extract formatting from `a:rPr` attributes.
fn parse_run_properties(attrs: &[Attribute<'_>], run: &mut DrawingRun) {
    if let Some(b) = attr_value(attrs, "b") {
        run.bold = b == "1" || b == "true";
    }
    if let Some(i) = attr_value(attrs, "i") {
        run.italic = i == "1" || i == "true";
    }
    if let Some(sz) = attr_value(attrs, "sz") {
        if let Ok(hundredths) = sz.parse::<f64>() {
            run.font_size_pt = Some(hundredths / crate::FONT_SIZE_DIVISOR);
        }
    }
    // Underline: any value other than "none" means underlined.
    if let Some(u) = attr_value(attrs, "u") {
        run.underline = u != "none";
    }
    // Strikethrough: "sngStrike" or "dblStrike" means strikethrough.
    if let Some(strike) = attr_value(attrs, "strike") {
        run.strikethrough = strike != "noStrike";
    }
}

/// Parse children of `a:rPr` to extract font name, color, and hyperlinks.
fn parse_rpr_children(
    reader: &mut XmlReader<'_>,
    depth: &mut u32,
    run: &mut DrawingRun,
    ctx: &SlideContext<'_>,
) -> Result<()> {
    let start_depth = *depth;

    loop {
        let event = reader.next_event().context("reading run properties")?;
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ref attributes,
                ..
            } => {
                *depth = depth.saturating_add(1);

                if is_drawingml(namespace_uri) {
                    match local_name.as_ref() {
                        // a:latin is the primary font for Latin text.
                        // a:ea is for East Asian, a:cs for Complex Script.
                        // Take the first typeface we find.
                        "latin" | "ea" | "cs" => {
                            if run.font_name.is_none() {
                                run.font_name =
                                    attr_value(attributes, "typeface").map(|s| s.to_string());
                            }
                            skip_element(reader, depth)?;
                        }
                        "solidFill" => {
                            // Parse solid fill for text color.
                            run.color = parse_solid_fill(reader, depth, ctx)?;
                        }
                        "hlinkClick" => {
                            // Hyperlink: resolve r:id to URL via slide rels.
                            let r_id = prefixed_attr_value(attributes, "r", "id");
                            if let Some(id) = r_id {
                                run.hyperlink_url = resolve_hyperlink(id, ctx.slide_rels);
                            }
                            skip_element(reader, depth)?;
                        }
                        _ => {}
                    }
                }
            }
            XmlEvent::EndElement { .. } => {
                *depth = depth.saturating_sub(1);
                if *depth < start_depth {
                    break;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(())
}

/// Parse `a:solidFill` children to extract sRGB color.
///
/// Supports `a:srgbClr` (six-hex-digit color). Emits a diagnostic for
/// `a:schemeClr` which requires theme resolution we don't implement.
/// The warning is emitted only once per text body to avoid flooding
/// diagnostics on slides with many theme-colored runs.
fn parse_solid_fill(
    reader: &mut XmlReader<'_>,
    depth: &mut u32,
    ctx: &SlideContext<'_>,
) -> Result<Option<[u8; 3]>> {
    let start_depth = *depth;
    let mut color = None;

    loop {
        let event = reader.next_event().context("reading solidFill")?;
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ref attributes,
                ..
            } => {
                *depth = depth.saturating_add(1);

                if is_drawingml(namespace_uri) {
                    match local_name.as_ref() {
                        "srgbClr" => {
                            if let Some(val) = attr_value(attributes, "val") {
                                color = parse_hex_color(val);
                            }
                            skip_element(reader, depth)?;
                        }
                        "schemeClr" => {
                            if let Some(val) = attr_value(attributes, "val") {
                                color = resolve_scheme_color(val, ctx);
                            }
                            skip_element(reader, depth)?;
                        }
                        _ => {}
                    }
                }
            }
            XmlEvent::EndElement { .. } => {
                *depth = depth.saturating_sub(1);
                if *depth < start_depth {
                    break;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(color)
}

/// Parse a 6-hex-digit color string (e.g. "FF0000") into [r, g, b].
fn parse_hex_color(hex: &str) -> Option<[u8; 3]> {
    udoc_core::document::Color::from_hex(hex).map(|c| c.to_array())
}

/// Resolve a scheme color name to RGB using the parsed theme, falling back
/// to safe defaults for dk1/lt1 when no theme is available.
///
/// Alias mapping: tx1 -> dk1, bg1 -> lt1 (OOXML spec: Text 1 = Dark 1,
/// Background 1 = Light 1).
fn resolve_scheme_color(name: &str, ctx: &SlideContext<'_>) -> Option<[u8; 3]> {
    // Map aliases to canonical names.
    let canonical = match name {
        "tx1" => "dk1",
        "bg1" => "lt1",
        "tx2" => "dk2",
        "bg2" => "lt2",
        other => other,
    };

    // Look up in parsed theme first.
    if let Some(rgb) = ctx.theme_colors.get(canonical) {
        return Some(*rgb);
    }

    // Fallback for when no theme was parsed.
    match canonical {
        "dk1" => Some([0x00, 0x00, 0x00]),
        "lt1" => Some([0xFF, 0xFF, 0xFF]),
        _ => {
            if !ctx.scheme_color_warned.get() {
                ctx.scheme_color_warned.set(true);
                ctx.diag.warning(Warning::new(
                    "PptxSchemeColor",
                    format!("scheme color '{name}' not found in theme and has no safe default"),
                ));
            }
            None
        }
    }
}

/// Resolve a hyperlink relationship ID to a URL using slide rels.
fn resolve_hyperlink(r_id: &str, slide_rels: &[Relationship]) -> Option<String> {
    slide_rels
        .iter()
        .find(|r| r.id == r_id && rel_type_matches(&r.rel_type, rel_types::HYPERLINK))
        .map(|r| r.target.clone())
}

/// Collect text content from inside an `a:t` element until its EndElement.
fn collect_text(reader: &mut XmlReader<'_>, depth: &mut u32) -> Result<String> {
    let mut text = String::new();
    let start_depth = *depth;

    loop {
        let event = reader.next_event().context("reading text content")?;
        match event {
            XmlEvent::Text(t) | XmlEvent::CData(t) => {
                // Silently cap at MAX_TEXT_LENGTH. A single <a:t> element
                // exceeding 10 MB is effectively impossible in real PPTX files;
                // the implicit MAX_FILE_SIZE bound (256 MB) also limits this.
                if text.len().saturating_add(t.len()) <= crate::MAX_TEXT_LENGTH {
                    text.push_str(&t);
                }
            }
            XmlEvent::StartElement { .. } => {
                *depth = depth.saturating_add(1);
            }
            XmlEvent::EndElement { .. } => {
                *depth = depth.saturating_sub(1);
                if *depth < start_depth {
                    break;
                }
            }
            XmlEvent::Eof => break,
        }
    }

    Ok(text)
}

/// Skip over an element and all its children.
pub(crate) fn skip_element(reader: &mut XmlReader<'_>, depth: &mut u32) -> Result<()> {
    let start_depth = *depth;

    loop {
        let event = reader.next_event().context("skipping element")?;
        match event {
            XmlEvent::StartElement { .. } => {
                *depth = depth.saturating_add(1);
            }
            XmlEvent::EndElement { .. } => {
                *depth = depth.saturating_sub(1);
                if *depth < start_depth {
                    break;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(())
}

/// Check if a namespace URI is DrawingML (Transitional or Strict).
pub(crate) fn is_drawingml(ns: &Option<Arc<str>>) -> bool {
    match ns {
        Some(uri) => uri.as_ref() == ns::DRAWINGML || uri.as_ref() == ns::DRAWINGML_STRICT,
        None => false,
    }
}

/// Check if a namespace URI is PresentationML (Transitional or Strict).
pub(crate) fn is_pml(ns: &Option<Arc<str>>) -> bool {
    match ns {
        Some(uri) => uri.as_ref() == ns::PML || uri.as_ref() == ns::PML_STRICT,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_text_body_from_xml(xml: &[u8]) -> Vec<DrawingParagraph> {
        let mut reader = XmlReader::new(xml).unwrap();
        // Advance past the root txBody element
        loop {
            match reader.next_event().unwrap() {
                XmlEvent::StartElement { local_name, .. } if local_name.as_ref() == "txBody" => {
                    break;
                }
                XmlEvent::Eof => panic!("no txBody found"),
                _ => {}
            }
        }
        parse_text_body(&mut reader).unwrap()
    }

    #[test]
    fn simple_text_extraction() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:r><a:t>Hello World</a:t></a:r>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert_eq!(paras.len(), 1);
        assert_eq!(paras[0].text(), "Hello World");
    }

    #[test]
    fn multiple_runs() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:r><a:t>Hello </a:t></a:r>
                <a:r><a:t>World</a:t></a:r>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert_eq!(paras.len(), 1);
        assert_eq!(paras[0].text(), "Hello World");
        assert_eq!(paras[0].runs.len(), 2);
    }

    #[test]
    fn bold_italic_formatting() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:r>
                    <a:rPr b="1" i="1" sz="2400"/>
                    <a:t>Bold Italic</a:t>
                </a:r>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert!(paras[0].runs[0].bold);
        assert!(paras[0].runs[0].italic);
        assert_eq!(paras[0].runs[0].font_size_pt, Some(24.0));
    }

    #[test]
    fn multiple_paragraphs() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p><a:r><a:t>First</a:t></a:r></a:p>
            <a:p><a:r><a:t>Second</a:t></a:r></a:p>
            <a:p><a:r><a:t>Third</a:t></a:r></a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert_eq!(paras.len(), 3);
        assert_eq!(paras[0].text(), "First");
        assert_eq!(paras[1].text(), "Second");
        assert_eq!(paras[2].text(), "Third");
    }

    #[test]
    fn line_break() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:r><a:t>Line one</a:t></a:r>
                <a:br/>
                <a:r><a:t>Line two</a:t></a:r>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert_eq!(paras[0].text(), "Line one\nLine two");
    }

    #[test]
    fn field_element() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:fld type="slidenum"><a:t>42</a:t></a:fld>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert_eq!(paras[0].runs[0].text, "42");
        assert!(paras[0].runs[0].is_field);
    }

    #[test]
    fn empty_paragraph() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p><a:endParaRPr/></a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert_eq!(paras.len(), 1);
        assert!(paras[0].is_empty());
    }

    #[test]
    fn strict_namespace() {
        let xml = br#"<p:txBody xmlns:a="http://purl.oclc.org/ooxml/drawingml/main"
                                xmlns:p="http://purl.oclc.org/ooxml/presentationml/main">
            <a:bodyPr/>
            <a:p><a:r><a:t>Strict mode</a:t></a:r></a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert_eq!(paras.len(), 1);
        assert_eq!(paras[0].text(), "Strict mode");
    }

    #[test]
    fn font_name_from_latin() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:r>
                    <a:rPr sz="1800">
                        <a:latin typeface="Calibri"/>
                    </a:rPr>
                    <a:t>Calibri text</a:t>
                </a:r>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert_eq!(paras[0].runs[0].font_name.as_deref(), Some("Calibri"));
        assert_eq!(paras[0].runs[0].font_size_pt, Some(18.0));
    }

    #[test]
    fn font_name_ea_fallback() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:r>
                    <a:rPr>
                        <a:ea typeface="MS Gothic"/>
                    </a:rPr>
                    <a:t>East Asian</a:t>
                </a:r>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert_eq!(paras[0].runs[0].font_name.as_deref(), Some("MS Gothic"));
    }

    #[test]
    fn underline_attribute() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:r>
                    <a:rPr u="sng"/>
                    <a:t>Underlined</a:t>
                </a:r>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert!(paras[0].runs[0].underline);
    }

    #[test]
    fn underline_none_not_underlined() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:r>
                    <a:rPr u="none"/>
                    <a:t>Not underlined</a:t>
                </a:r>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert!(!paras[0].runs[0].underline);
    }

    #[test]
    fn strikethrough_single() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:r>
                    <a:rPr strike="sngStrike"/>
                    <a:t>Struck</a:t>
                </a:r>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert!(paras[0].runs[0].strikethrough);
    }

    #[test]
    fn strikethrough_no_strike() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:r>
                    <a:rPr strike="noStrike"/>
                    <a:t>Not struck</a:t>
                </a:r>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert!(!paras[0].runs[0].strikethrough);
    }

    #[test]
    fn solid_fill_srgb_color() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:r>
                    <a:rPr>
                        <a:solidFill><a:srgbClr val="FF0000"/></a:solidFill>
                    </a:rPr>
                    <a:t>Red text</a:t>
                </a:r>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert_eq!(paras[0].runs[0].color, Some([255, 0, 0]));
    }

    #[test]
    fn paragraph_alignment() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:pPr algn="ctr"/>
                <a:r><a:t>Centered</a:t></a:r>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert_eq!(paras[0].alignment.as_deref(), Some("ctr"));
    }

    #[test]
    fn hyperlink_resolved() {
        use udoc_containers::opc::{Relationship, TargetMode};

        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
                                xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
            <a:bodyPr/>
            <a:p>
                <a:r>
                    <a:rPr>
                        <a:hlinkClick r:id="rId5"/>
                    </a:rPr>
                    <a:t>Click here</a:t>
                </a:r>
            </a:p>
        </p:txBody>"#;

        let rels = vec![Relationship {
            id: "rId5".to_string(),
            rel_type:
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink"
                    .to_string(),
            target: "https://example.com".to_string(),
            target_mode: TargetMode::External,
        }];

        let mut reader = XmlReader::new(xml.as_ref()).unwrap();
        loop {
            match reader.next_event().unwrap() {
                XmlEvent::StartElement { local_name, .. } if local_name.as_ref() == "txBody" => {
                    break;
                }
                XmlEvent::Eof => panic!("no txBody found"),
                _ => {}
            }
        }
        let mut depth: u32 = 1;
        let empty_theme = std::collections::HashMap::new();
        let ctx = SlideContext {
            diag: &NullDiagnostics,
            slide_index: 0,
            slide_rels: &rels,
            scheme_color_warned: Cell::new(false),
            theme_colors: &empty_theme,
        };
        let paras = parse_text_body_with_depth(&mut reader, &mut depth, &ctx).unwrap();
        assert_eq!(paras[0].runs[0].text, "Click here");
        assert_eq!(
            paras[0].runs[0].hyperlink_url.as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn font_forwarded_with_color_and_style() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:r>
                    <a:rPr b="1" u="sng" strike="sngStrike" sz="2000">
                        <a:solidFill><a:srgbClr val="00FF00"/></a:solidFill>
                        <a:latin typeface="Arial"/>
                    </a:rPr>
                    <a:t>Styled text</a:t>
                </a:r>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        let run = &paras[0].runs[0];
        assert!(run.bold);
        assert!(run.underline);
        assert!(run.strikethrough);
        assert_eq!(run.font_size_pt, Some(20.0));
        assert_eq!(run.color, Some([0, 255, 0]));
        assert_eq!(run.font_name.as_deref(), Some("Arial"));
    }

    #[test]
    fn parse_hex_color_rejects_multibyte_utf8() {
        assert_eq!(parse_hex_color("FF0000"), Some([255, 0, 0]));
        assert_eq!(parse_hex_color("000000"), Some([0, 0, 0]));
        assert_eq!(parse_hex_color("short"), None);
        assert_eq!(parse_hex_color(""), None);
        // Multi-byte UTF-8 that is 6 bytes must not panic.
        assert_eq!(parse_hex_color("\u{00e9}\u{00e9}\u{00e9}"), None);
    }

    #[test]
    fn scheme_color_defaults_without_theme() {
        // Without a theme, only dk1/tx1 and lt1/bg1 return safe defaults.
        let empty_theme = std::collections::HashMap::new();
        let ctx = SlideContext {
            diag: &NullDiagnostics,
            slide_index: 0,
            slide_rels: &[],
            scheme_color_warned: Cell::new(false),
            theme_colors: &empty_theme,
        };
        assert_eq!(resolve_scheme_color("dk1", &ctx), Some([0x00, 0x00, 0x00]));
        assert_eq!(resolve_scheme_color("tx1", &ctx), Some([0x00, 0x00, 0x00]));
        assert_eq!(resolve_scheme_color("lt1", &ctx), Some([0xFF, 0xFF, 0xFF]));
        assert_eq!(resolve_scheme_color("bg1", &ctx), Some([0xFF, 0xFF, 0xFF]));
        // Without theme: accent/hyperlink colors return None.
        assert_eq!(resolve_scheme_color("accent1", &ctx), None);
        assert_eq!(resolve_scheme_color("unknownScheme", &ctx), None);
    }

    #[test]
    fn scheme_color_resolved_from_theme() {
        let mut theme = std::collections::HashMap::new();
        theme.insert("accent1".to_string(), [0x44, 0x72, 0xC4]);
        theme.insert("dk1".to_string(), [0x1F, 0x49, 0x7D]);
        theme.insert("dk2".to_string(), [0x33, 0x33, 0x33]);
        let ctx = SlideContext {
            diag: &NullDiagnostics,
            slide_index: 0,
            slide_rels: &[],
            scheme_color_warned: Cell::new(false),
            theme_colors: &theme,
        };
        // Theme overrides hardcoded defaults.
        assert_eq!(resolve_scheme_color("dk1", &ctx), Some([0x1F, 0x49, 0x7D]));
        assert_eq!(
            resolve_scheme_color("accent1", &ctx),
            Some([0x44, 0x72, 0xC4])
        );
        // tx1 aliases to dk1.
        assert_eq!(resolve_scheme_color("tx1", &ctx), Some([0x1F, 0x49, 0x7D]));
        // tx2 aliases to dk2.
        assert_eq!(resolve_scheme_color("tx2", &ctx), Some([0x33, 0x33, 0x33]));
    }

    #[test]
    fn scheme_color_dk1_produces_black() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:r>
                    <a:rPr>
                        <a:solidFill><a:schemeClr val="dk1"/></a:solidFill>
                    </a:rPr>
                    <a:t>Dark text</a:t>
                </a:r>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert_eq!(paras[0].runs[0].color, Some([0x00, 0x00, 0x00]));
    }

    #[test]
    fn scheme_color_accent_returns_none() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:r>
                    <a:rPr>
                        <a:solidFill><a:schemeClr val="accent1"/></a:solidFill>
                    </a:rPr>
                    <a:t>Theme colored</a:t>
                </a:r>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        // accent1 varies by theme -- we don't guess.
        assert_eq!(paras[0].runs[0].color, None);
    }

    #[test]
    fn bullet_char_parsed() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:pPr>
                    <a:buChar char="-"/>
                </a:pPr>
                <a:r><a:t>Bullet item</a:t></a:r>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert_eq!(paras.len(), 1);
        assert_eq!(paras[0].bullet, Some(BulletType::Char("-".to_string())));
        assert_eq!(paras[0].text(), "Bullet item");
    }

    #[test]
    fn bullet_autonum_parsed() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:pPr>
                    <a:buAutoNum type="arabicPeriod"/>
                </a:pPr>
                <a:r><a:t>Numbered item</a:t></a:r>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert_eq!(paras[0].bullet, Some(BulletType::AutoNum));
    }

    #[test]
    fn bullet_none_parsed() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:pPr>
                    <a:buNone/>
                </a:pPr>
                <a:r><a:t>No bullet</a:t></a:r>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert_eq!(paras[0].bullet, Some(BulletType::None));
    }

    #[test]
    fn no_bullet_element_means_none_bullet() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:pPr algn="ctr"/>
                <a:r><a:t>Plain text</a:t></a:r>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert_eq!(paras[0].bullet, None);
        assert_eq!(paras[0].alignment.as_deref(), Some("ctr"));
    }

    #[test]
    fn bullet_char_with_alignment() {
        let xml = br#"<p:txBody xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
            <a:bodyPr/>
            <a:p>
                <a:pPr algn="l">
                    <a:buChar char="&#x2022;"/>
                </a:pPr>
                <a:r><a:t>Bullet with alignment</a:t></a:r>
            </a:p>
        </p:txBody>"#;

        let paras = parse_text_body_from_xml(xml);
        assert_eq!(
            paras[0].bullet,
            Some(BulletType::Char("\u{2022}".to_string()))
        );
        assert_eq!(paras[0].alignment.as_deref(), Some("l"));
    }
}

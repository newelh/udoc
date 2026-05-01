//! Shape tree parser for PPTX slides.
//!
//! Recursively walks the `p:spTree` element to extract shapes with their
//! text content, positions, and placeholder types. Group shapes (`p:grpSp`)
//! are walked recursively with depth limiting.
//!
//! Reading order: shapes are sorted by Y-position (top-to-bottom
//! bands) then X-position (left-to-right) using EMU coordinates from
//! `a:xfrm/a:off`.

use std::cell::Cell;

use udoc_containers::opc::Relationship;
use udoc_containers::xml::{attr_value, Attribute, XmlEvent, XmlReader};
use udoc_core::diagnostics::{DiagnosticsSink, Warning, WarningContext};
use udoc_core::error::{Result, ResultExt};

use crate::table::parse_table;
use crate::text::{
    is_drawingml, is_pml, parse_text_body_with_depth, skip_element, DrawingParagraph,
};

/// Shared parsing context for a PPTX slide.
///
/// Bundles the diagnostics sink, slide index, and slide relationships
/// that are threaded through every shape/table/text parse function.
pub(crate) struct SlideContext<'a> {
    pub diag: &'a dyn DiagnosticsSink,
    pub slide_index: usize,
    pub slide_rels: &'a [Relationship],
    /// Per-slide dedup flag for scheme color diagnostics.
    /// Uses `Cell` for interior mutability through `&SlideContext`.
    pub scheme_color_warned: Cell<bool>,
    /// Theme color scheme (scheme name -> RGB). Empty if theme not available.
    pub theme_colors: &'a std::collections::HashMap<String, [u8; 3]>,
}

/// A shape extracted from a slide's shape tree.
#[derive(Debug, Clone)]
pub(crate) struct SlideShape {
    /// X offset in EMU.
    pub x_emu: i64,
    /// Y offset in EMU.
    pub y_emu: i64,
    /// Width in EMU.
    pub cx_emu: i64,
    /// Height in EMU.
    pub cy_emu: i64,
    /// The kind of content this shape holds.
    pub content: ShapeContent,
    /// Placeholder type, if this shape is a placeholder.
    pub placeholder_type: Option<String>,
    /// Placeholder index for inheritance matching.
    pub placeholder_idx: Option<u32>,
    /// Shape name from `p:cNvPr`.
    pub name: String,
}

/// Content extracted from a shape.
#[derive(Debug, Clone)]
pub(crate) enum ShapeContent {
    /// Text paragraphs from `p:txBody`.
    Text(Vec<DrawingParagraph>),
    /// A table from `a:tbl` inside a `p:graphicFrame`.
    Table(ExtractedTable),
    /// An image reference (relationship id).
    #[allow(dead_code)] // parsed from shape tree; reserved for image extraction support
    Image {
        r_id: String,
        alt_text: Option<String>,
    },
    /// No extractable content (connectors, charts, etc.).
    Empty,
}

/// A table extracted from a graphic frame.
#[derive(Debug, Clone)]
pub(crate) struct ExtractedTable {
    pub rows: Vec<ExtractedTableRow>,
    #[allow(dead_code)] // parsed from a:tblGrid; reserved for column-span calculations
    pub num_columns: usize,
}

/// A row in an extracted table.
#[derive(Debug, Clone)]
pub(crate) struct ExtractedTableRow {
    pub cells: Vec<ExtractedTableCell>,
}

/// A cell in an extracted table row.
#[derive(Debug, Clone)]
pub(crate) struct ExtractedTableCell {
    pub paragraphs: Vec<DrawingParagraph>,
    pub col_span: usize,
    pub row_span: usize,
    pub is_h_merge: bool,
    pub is_v_merge: bool,
}

/// Parse a slide's shape tree from XML bytes.
///
/// The `xml_data` should be the full slide XML. This function finds the
/// `p:spTree` element and walks it recursively.
///
/// `slide_rels` are the OPC relationships for this slide, used to resolve
/// hyperlink `r:id` references to URLs.
pub(crate) fn parse_slide_shapes(
    xml_data: &[u8],
    ctx: &SlideContext<'_>,
) -> Result<Vec<SlideShape>> {
    let mut reader = XmlReader::new(xml_data).context("parsing slide XML")?;
    let mut shapes = Vec::new();

    // Find p:spTree
    let mut found_sp_tree = false;
    loop {
        match reader.next_event().context("scanning for shape tree")? {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ..
            } if is_pml(namespace_uri) && local_name.as_ref() == "spTree" => {
                found_sp_tree = true;
                break;
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    if !found_sp_tree {
        return Ok(shapes);
    }

    // Parse shape tree children
    parse_shape_tree_children(&mut reader, &mut shapes, ctx, 0)?;

    // Cap shapes per slide to prevent unbounded allocation
    if shapes.len() > crate::MAX_SHAPES_PER_SLIDE {
        ctx.diag.warning(
            Warning::new(
                "PptxShapeLimit",
                format!(
                    "slide {} has {} shapes, capping at {}",
                    ctx.slide_index,
                    shapes.len(),
                    crate::MAX_SHAPES_PER_SLIDE
                ),
            )
            .with_context({
                let mut wctx = WarningContext::default();
                wctx.page_index = Some(ctx.slide_index);
                wctx
            }),
        );
        shapes.truncate(crate::MAX_SHAPES_PER_SLIDE);
    }

    // Sort shapes by reading order:
    // Primary: Y-position ascending (top-to-bottom)
    // Secondary: X-position ascending (left-to-right)
    shapes.sort_by(|a, b| a.y_emu.cmp(&b.y_emu).then_with(|| a.x_emu.cmp(&b.x_emu)));

    Ok(shapes)
}

/// Parse children of a shape tree or group shape.
fn parse_shape_tree_children(
    reader: &mut XmlReader<'_>,
    shapes: &mut Vec<SlideShape>,
    ctx: &SlideContext<'_>,
    depth: usize,
) -> Result<()> {
    if depth > crate::MAX_SHAPE_DEPTH {
        ctx.diag.warning(
            Warning::new(
                "PptxShapeDepthLimit",
                format!(
                    "shape tree recursion depth exceeded {}",
                    crate::MAX_SHAPE_DEPTH
                ),
            )
            .with_context({
                let mut wctx = WarningContext::default();
                wctx.page_index = Some(ctx.slide_index);
                wctx
            }),
        );
        // Safe to return without consuming: the only call site is
        // parse_slide_shapes (depth=0), so this branch is unreachable.
        // Recursive group shapes go through parse_group_shape which
        // handles depth limits with skip_element.
        return Ok(());
    }

    let mut tree_depth: u32 = 1;

    loop {
        let event = reader.next_event().context("reading shape tree")?;
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ..
            } => {
                tree_depth = tree_depth.saturating_add(1);
                let name = local_name.as_ref();

                if is_pml(namespace_uri) {
                    match name {
                        "sp" => {
                            // Regular shape (text box, placeholder, etc.)
                            if let Some(shape) = parse_shape(reader, &mut tree_depth, ctx)? {
                                shapes.push(shape);
                            }
                        }
                        "pic" => {
                            // Picture
                            if let Some(shape) = parse_picture(reader, &mut tree_depth, ctx)? {
                                shapes.push(shape);
                            }
                        }
                        "graphicFrame" => {
                            // Table, chart, or other graphic
                            if let Some(shape) = parse_graphic_frame(reader, &mut tree_depth, ctx)?
                            {
                                shapes.push(shape);
                            }
                        }
                        "grpSp" => {
                            // Group shape: recurse
                            parse_group_shape(reader, &mut tree_depth, shapes, ctx, depth + 1)?;
                        }
                        "cxnSp" => {
                            // Connector: skip (no text content)
                            skip_element(reader, &mut tree_depth)?;
                        }
                        _ => {}
                    }
                } else if local_name.as_ref() == "AlternateContent" {
                    // mc:AlternateContent: parse fallback
                    parse_alternate_content(reader, &mut tree_depth, shapes, ctx, depth)?;
                }
            }
            XmlEvent::EndElement { .. } => {
                tree_depth = tree_depth.saturating_sub(1);
                if tree_depth == 0 {
                    break;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(())
}

/// Parse a `p:sp` (shape) element.
fn parse_shape(
    reader: &mut XmlReader<'_>,
    depth: &mut u32,
    ctx: &SlideContext<'_>,
) -> Result<Option<SlideShape>> {
    let start_depth = *depth;
    let mut shape = SlideShape {
        x_emu: 0,
        y_emu: 0,
        cx_emu: 0,
        cy_emu: 0,
        content: ShapeContent::Empty,
        placeholder_type: None,
        placeholder_idx: None,
        name: String::new(),
    };
    let mut has_text = false;

    loop {
        let event = reader.next_event().context("reading shape element")?;
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ref attributes,
                ..
            } => {
                *depth = depth.saturating_add(1);
                let name = local_name.as_ref();

                if is_pml(namespace_uri) {
                    match name {
                        "cNvPr" => {
                            shape.name = attr_value(attributes, "name").unwrap_or("").to_string();
                            skip_element(reader, depth)?;
                        }
                        "nvPr" => {
                            parse_nv_pr(reader, depth, &mut shape)?;
                        }
                        "txBody" => {
                            let paragraphs = parse_text_body_with_depth(reader, depth, ctx)?;
                            shape.content = ShapeContent::Text(paragraphs);
                            has_text = true;
                        }
                        "spPr" => {
                            parse_sp_pr(reader, depth, &mut shape)?;
                        }
                        _ => {}
                    }
                } else if is_drawingml(namespace_uri) && name == "xfrm" {
                    parse_xfrm(reader, depth, attributes, &mut shape)?;
                } else if name == "spPr" {
                    // spPr can be unprefixed in some PPTX producers
                    parse_sp_pr(reader, depth, &mut shape)?;
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

    if has_text {
        Ok(Some(shape))
    } else {
        Ok(None)
    }
}

/// Parse a `p:pic` (picture) element.
fn parse_picture(
    reader: &mut XmlReader<'_>,
    depth: &mut u32,
    _ctx: &SlideContext<'_>,
) -> Result<Option<SlideShape>> {
    let start_depth = *depth;
    let mut shape = SlideShape {
        x_emu: 0,
        y_emu: 0,
        cx_emu: 0,
        cy_emu: 0,
        content: ShapeContent::Empty,
        placeholder_type: None,
        placeholder_idx: None,
        name: String::new(),
    };
    let mut r_id = None;
    let mut alt_text = None;

    loop {
        let event = reader.next_event().context("reading picture element")?;
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ref attributes,
                ..
            } => {
                *depth = depth.saturating_add(1);
                let name = local_name.as_ref();

                if is_pml(namespace_uri) && name == "cNvPr" {
                    shape.name = attr_value(attributes, "name").unwrap_or("").to_string();
                    alt_text = attr_value(attributes, "descr").map(|s| s.to_string());
                    skip_element(reader, depth)?;
                } else if is_drawingml(namespace_uri) && name == "blip" {
                    // r:embed attribute has the relationship ID
                    r_id = attr_value(attributes, "embed").map(|s| s.to_string());
                    skip_element(reader, depth)?;
                } else if is_drawingml(namespace_uri) && name == "xfrm" {
                    parse_xfrm(reader, depth, attributes, &mut shape)?;
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

    if let Some(id) = r_id {
        shape.content = ShapeContent::Image { r_id: id, alt_text };
        Ok(Some(shape))
    } else {
        Ok(None)
    }
}

/// Parse a `p:graphicFrame` element (tables, charts, etc.).
fn parse_graphic_frame(
    reader: &mut XmlReader<'_>,
    depth: &mut u32,
    ctx: &SlideContext<'_>,
) -> Result<Option<SlideShape>> {
    let start_depth = *depth;
    let mut shape = SlideShape {
        x_emu: 0,
        y_emu: 0,
        cx_emu: 0,
        cy_emu: 0,
        content: ShapeContent::Empty,
        placeholder_type: None,
        placeholder_idx: None,
        name: String::new(),
    };

    loop {
        let event = reader.next_event().context("reading graphic frame")?;
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ref attributes,
                ..
            } => {
                *depth = depth.saturating_add(1);
                let name = local_name.as_ref();

                if is_pml(namespace_uri) && name == "cNvPr" {
                    shape.name = attr_value(attributes, "name").unwrap_or("").to_string();
                    skip_element(reader, depth)?;
                } else if name == "xfrm" {
                    parse_xfrm(reader, depth, attributes, &mut shape)?;
                } else if is_drawingml(namespace_uri) && name == "graphicData" {
                    let uri = attr_value(attributes, "uri").unwrap_or("");
                    if uri.contains("/table") {
                        // Table content
                        let table = parse_table(reader, depth, ctx)?;
                        shape.content = ShapeContent::Table(table);
                    } else {
                        // Chart, SmartArt, etc.: skip
                        skip_element(reader, depth)?;
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

    match shape.content {
        ShapeContent::Empty => Ok(None),
        _ => Ok(Some(shape)),
    }
}

/// Parse a `p:grpSp` (group shape) element.
fn parse_group_shape(
    reader: &mut XmlReader<'_>,
    depth: &mut u32,
    shapes: &mut Vec<SlideShape>,
    ctx: &SlideContext<'_>,
    recursion_depth: usize,
) -> Result<()> {
    let start_depth = *depth;
    let mut group_x: i64 = 0;
    let mut group_y: i64 = 0;
    // Track child offset/extent for coordinate mapping
    let mut ch_off_x: i64 = 0;
    let mut ch_off_y: i64 = 0;

    let initial_shape_count = shapes.len();

    // Parse group transform and child shapes
    loop {
        let event = reader.next_event().context("reading group shape")?;
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ..
            } => {
                *depth = depth.saturating_add(1);
                let name = local_name.as_ref();

                if name == "grpSpPr" {
                    // Parse group shape properties for transform
                    parse_grp_sp_pr(
                        reader,
                        depth,
                        &mut group_x,
                        &mut group_y,
                        &mut ch_off_x,
                        &mut ch_off_y,
                    )?;
                } else if is_pml(namespace_uri) {
                    match name {
                        "sp" => {
                            if let Some(shape) = parse_shape(reader, depth, ctx)? {
                                shapes.push(shape);
                            }
                        }
                        "pic" => {
                            if let Some(shape) = parse_picture(reader, depth, ctx)? {
                                shapes.push(shape);
                            }
                        }
                        "graphicFrame" => {
                            if let Some(shape) = parse_graphic_frame(reader, depth, ctx)? {
                                shapes.push(shape);
                            }
                        }
                        "grpSp" => {
                            if recursion_depth < crate::MAX_SHAPE_DEPTH {
                                parse_group_shape(reader, depth, shapes, ctx, recursion_depth + 1)?;
                            } else {
                                ctx.diag.warning(Warning::new(
                                    "PptxShapeDepthLimit",
                                    "nested group shape depth exceeded limit",
                                ));
                                skip_element(reader, depth)?;
                            }
                        }
                        "cxnSp" => {
                            skip_element(reader, depth)?;
                        }
                        _ => {}
                    }
                } else if name == "AlternateContent" {
                    parse_alternate_content(reader, depth, shapes, ctx, recursion_depth)?;
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

    // Adjust child shape coordinates to parent coordinate space.
    // Child shapes use the group's child coordinate system (chOff/chExt).
    // We offset them by (group_x - ch_off_x, group_y - ch_off_y) for
    // a simple translation. Full scaling (chExt != ext) is a nice-to-have.
    let dx = group_x.saturating_sub(ch_off_x);
    let dy = group_y.saturating_sub(ch_off_y);
    for shape in shapes.iter_mut().skip(initial_shape_count) {
        shape.x_emu = shape.x_emu.saturating_add(dx);
        shape.y_emu = shape.y_emu.saturating_add(dy);
    }

    Ok(())
}

/// Parse group shape properties to extract transform.
fn parse_grp_sp_pr(
    reader: &mut XmlReader<'_>,
    depth: &mut u32,
    group_x: &mut i64,
    group_y: &mut i64,
    ch_off_x: &mut i64,
    ch_off_y: &mut i64,
) -> Result<()> {
    let start_depth = *depth;

    loop {
        let event = reader
            .next_event()
            .context("reading group shape properties")?;
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
                        "off" => {
                            *group_x = parse_emu_attr(attributes, "x");
                            *group_y = parse_emu_attr(attributes, "y");
                            skip_element(reader, depth)?;
                        }
                        "chOff" => {
                            *ch_off_x = parse_emu_attr(attributes, "x");
                            *ch_off_y = parse_emu_attr(attributes, "y");
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

/// Parse `mc:AlternateContent`: use mc:Fallback content.
fn parse_alternate_content(
    reader: &mut XmlReader<'_>,
    depth: &mut u32,
    shapes: &mut Vec<SlideShape>,
    ctx: &SlideContext<'_>,
    recursion_depth: usize,
) -> Result<()> {
    let start_depth = *depth;
    let mut in_fallback = false;

    loop {
        let event = reader.next_event().context("reading AlternateContent")?;
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ..
            } => {
                *depth = depth.saturating_add(1);
                let name = local_name.as_ref();

                match name {
                    "Fallback" => {
                        in_fallback = true;
                    }
                    "Choice" => {
                        // Skip Choice content (uses newer namespaces we may not support)
                        skip_element(reader, depth)?;
                    }
                    _ if in_fallback && is_pml(namespace_uri) => {
                        // Process shapes within Fallback
                        match name {
                            "sp" => {
                                if let Some(shape) = parse_shape(reader, depth, ctx)? {
                                    shapes.push(shape);
                                }
                            }
                            "pic" => {
                                if let Some(shape) = parse_picture(reader, depth, ctx)? {
                                    shapes.push(shape);
                                }
                            }
                            "graphicFrame" => {
                                if let Some(shape) = parse_graphic_frame(reader, depth, ctx)? {
                                    shapes.push(shape);
                                }
                            }
                            "grpSp" => {
                                parse_group_shape(reader, depth, shapes, ctx, recursion_depth + 1)?;
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            XmlEvent::EndElement { ref local_name, .. } => {
                *depth = depth.saturating_sub(1);
                if local_name.as_ref() == "Fallback" {
                    in_fallback = false;
                }
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

/// Parse `p:nvPr` (non-visual properties) to extract placeholder type/idx.
fn parse_nv_pr(reader: &mut XmlReader<'_>, depth: &mut u32, shape: &mut SlideShape) -> Result<()> {
    let start_depth = *depth;

    loop {
        let event = reader
            .next_event()
            .context("reading non-visual properties")?;
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ref attributes,
                ..
            } => {
                *depth = depth.saturating_add(1);
                let name = local_name.as_ref();

                if is_pml(namespace_uri) && name == "ph" {
                    shape.placeholder_type = attr_value(attributes, "type").map(|s| s.to_string());
                    shape.placeholder_idx =
                        attr_value(attributes, "idx").and_then(|s| s.parse::<u32>().ok());
                    skip_element(reader, depth)?;
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

/// Parse `p:spPr` (shape properties) to extract transform.
fn parse_sp_pr(reader: &mut XmlReader<'_>, depth: &mut u32, shape: &mut SlideShape) -> Result<()> {
    let start_depth = *depth;

    loop {
        let event = reader.next_event().context("reading shape properties")?;
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ref attributes,
                ..
            } => {
                *depth = depth.saturating_add(1);

                if is_drawingml(namespace_uri) && local_name.as_ref() == "xfrm" {
                    parse_xfrm(reader, depth, attributes, shape)?;
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

/// Parse `a:xfrm` transform element.
fn parse_xfrm(
    reader: &mut XmlReader<'_>,
    depth: &mut u32,
    _xfrm_attrs: &[Attribute<'_>],
    shape: &mut SlideShape,
) -> Result<()> {
    let start_depth = *depth;

    loop {
        let event = reader.next_event().context("reading shape transform")?;
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
                        "off" => {
                            shape.x_emu = parse_emu_attr(attributes, "x");
                            shape.y_emu = parse_emu_attr(attributes, "y");
                            skip_element(reader, depth)?;
                        }
                        "ext" => {
                            shape.cx_emu = parse_emu_attr(attributes, "cx");
                            shape.cy_emu = parse_emu_attr(attributes, "cy");
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

/// Parse an EMU value from an XML attribute.
pub(crate) fn parse_emu_attr(attrs: &[Attribute<'_>], name: &str) -> i64 {
    attr_value(attrs, name)
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::diagnostics::NullDiagnostics;

    fn test_ctx() -> SlideContext<'static> {
        static EMPTY_THEME: std::sync::LazyLock<std::collections::HashMap<String, [u8; 3]>> =
            std::sync::LazyLock::new(std::collections::HashMap::new);
        SlideContext {
            diag: &NullDiagnostics,
            slide_index: 0,
            slide_rels: &[],
            scheme_color_warned: Cell::new(false),
            theme_colors: &EMPTY_THEME,
        }
    }

    #[test]
    fn parse_simple_shape() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:cSld>
    <p:spTree>
      <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/></a:xfrm></p:grpSpPr>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Title 1"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="title"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr>
          <a:xfrm><a:off x="457200" y="274638"/><a:ext cx="8229600" cy="1143000"/></a:xfrm>
        </p:spPr>
        <p:txBody>
          <a:bodyPr/>
          <a:p><a:r><a:t>Hello PPTX</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let ctx = test_ctx();
        let shapes = parse_slide_shapes(xml, &ctx).unwrap();
        assert_eq!(shapes.len(), 1);
        assert_eq!(shapes[0].placeholder_type.as_deref(), Some("title"));
        assert_eq!(shapes[0].x_emu, 457200);
        assert_eq!(shapes[0].y_emu, 274638);

        if let ShapeContent::Text(ref paras) = shapes[0].content {
            assert_eq!(paras[0].text(), "Hello PPTX");
        } else {
            panic!("expected text content");
        }
    }

    #[test]
    fn reading_order_y_then_x() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr/>
      <p:sp>
        <p:nvSpPr><p:cNvPr id="3" name="Bottom"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
        <p:spPr><a:xfrm><a:off x="0" y="5000000"/><a:ext cx="1000" cy="1000"/></a:xfrm></p:spPr>
        <p:txBody><a:bodyPr/><a:p><a:r><a:t>Bottom</a:t></a:r></a:p></p:txBody>
      </p:sp>
      <p:sp>
        <p:nvSpPr><p:cNvPr id="2" name="Top"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
        <p:spPr><a:xfrm><a:off x="0" y="100000"/><a:ext cx="1000" cy="1000"/></a:xfrm></p:spPr>
        <p:txBody><a:bodyPr/><a:p><a:r><a:t>Top</a:t></a:r></a:p></p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let ctx = test_ctx();
        let shapes = parse_slide_shapes(xml, &ctx).unwrap();
        assert_eq!(shapes.len(), 2);
        // Sorted by Y: Top first, Bottom second
        if let ShapeContent::Text(ref p) = shapes[0].content {
            assert_eq!(p[0].text(), "Top");
        }
        if let ShapeContent::Text(ref p) = shapes[1].content {
            assert_eq!(p[0].text(), "Bottom");
        }
    }

    #[test]
    fn empty_slide() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr/>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let ctx = test_ctx();
        let shapes = parse_slide_shapes(xml, &ctx).unwrap();
        assert!(shapes.is_empty());
    }

    #[test]
    fn group_shape_coordinate_offset() {
        // Group at (1000000, 2000000) with child offset (0, 0).
        // Child shape at (500000, 300000) should become (1500000, 2300000).
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr/>
      <p:grpSp>
        <p:nvGrpSpPr><p:cNvPr id="2" name="Group"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
        <p:grpSpPr>
          <a:xfrm>
            <a:off x="1000000" y="2000000"/>
            <a:ext cx="5000000" cy="3000000"/>
            <a:chOff x="0" y="0"/>
            <a:chExt cx="5000000" cy="3000000"/>
          </a:xfrm>
        </p:grpSpPr>
        <p:sp>
          <p:nvSpPr><p:cNvPr id="3" name="Child"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
          <p:spPr><a:xfrm><a:off x="500000" y="300000"/><a:ext cx="1000" cy="1000"/></a:xfrm></p:spPr>
          <p:txBody><a:bodyPr/><a:p><a:r><a:t>Grouped text</a:t></a:r></a:p></p:txBody>
        </p:sp>
      </p:grpSp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let ctx = test_ctx();
        let shapes = parse_slide_shapes(xml, &ctx).unwrap();
        assert_eq!(shapes.len(), 1);
        // Child offset adjusted by group position
        assert_eq!(shapes[0].x_emu, 1500000);
        assert_eq!(shapes[0].y_emu, 2300000);
        if let ShapeContent::Text(ref p) = shapes[0].content {
            assert_eq!(p[0].text(), "Grouped text");
        } else {
            panic!("expected text content");
        }
    }

    #[test]
    fn alternate_content_fallback() {
        // mc:AlternateContent should use mc:Fallback content
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
  <p:cSld>
    <p:spTree>
      <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr/>
      <mc:AlternateContent>
        <mc:Choice Requires="p14">
          <p:sp>
            <p:nvSpPr><p:cNvPr id="5" name="Choice"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
            <p:spPr/>
            <p:txBody><a:bodyPr/><a:p><a:r><a:t>New format</a:t></a:r></a:p></p:txBody>
          </p:sp>
        </mc:Choice>
        <mc:Fallback>
          <p:sp>
            <p:nvSpPr><p:cNvPr id="6" name="Fallback"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
            <p:spPr/>
            <p:txBody><a:bodyPr/><a:p><a:r><a:t>Fallback text</a:t></a:r></a:p></p:txBody>
          </p:sp>
        </mc:Fallback>
      </mc:AlternateContent>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let ctx = test_ctx();
        let shapes = parse_slide_shapes(xml, &ctx).unwrap();
        // Should get the Fallback content, not the Choice
        assert_eq!(shapes.len(), 1);
        if let ShapeContent::Text(ref p) = shapes[0].content {
            assert_eq!(p[0].text(), "Fallback text");
        } else {
            panic!("expected text content");
        }
    }

    #[test]
    fn no_sp_tree_returns_empty() {
        // Slide XML without a p:spTree should return empty, not error.
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld/>
</p:sld>"#;
        let ctx = test_ctx();
        let shapes = parse_slide_shapes(xml, &ctx).unwrap();
        assert!(shapes.is_empty());
    }

    #[test]
    fn connector_shapes_skipped() {
        // Connector shapes (p:cxnSp) should not produce content.
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr/>
      <p:cxnSp>
        <p:nvCxnSpPr><p:cNvPr id="2" name="Connector"/><p:cNvCxnSpPr/><p:nvPr/></p:nvCxnSpPr>
        <p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="1000" cy="1000"/></a:xfrm></p:spPr>
      </p:cxnSp>
      <p:sp>
        <p:nvSpPr><p:cNvPr id="3" name="Text"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
        <p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="1000" cy="1000"/></a:xfrm></p:spPr>
        <p:txBody><a:bodyPr/><a:p><a:r><a:t>Real text</a:t></a:r></a:p></p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let ctx = test_ctx();
        let shapes = parse_slide_shapes(xml, &ctx).unwrap();
        assert_eq!(shapes.len(), 1);
        if let ShapeContent::Text(ref p) = shapes[0].content {
            assert_eq!(p[0].text(), "Real text");
        } else {
            panic!("expected text content");
        }
    }

    #[test]
    fn shape_without_text_body_filtered() {
        // A p:sp with only spPr but no txBody should not produce a shape.
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr/>
      <p:sp>
        <p:nvSpPr><p:cNvPr id="2" name="NoText"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
        <p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="1000" cy="1000"/></a:xfrm></p:spPr>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let ctx = test_ctx();
        let shapes = parse_slide_shapes(xml, &ctx).unwrap();
        assert!(shapes.is_empty());
    }

    #[test]
    fn text_and_table_interleaved() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr/>
      <p:sp>
        <p:nvSpPr><p:cNvPr id="2" name="Title"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
        <p:spPr><a:xfrm><a:off x="0" y="100000"/><a:ext cx="1000" cy="1000"/></a:xfrm></p:spPr>
        <p:txBody><a:bodyPr/><a:p><a:r><a:t>Title text</a:t></a:r></a:p></p:txBody>
      </p:sp>
      <p:graphicFrame>
        <p:nvGraphicFramePr><p:cNvPr id="3" name="Table"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr>
        <p:xfrm><a:off x="0" y="2000000"/><a:ext cx="5000" cy="5000"/></p:xfrm>
        <a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table">
          <a:tbl>
            <a:tblGrid><a:gridCol w="1000"/></a:tblGrid>
            <a:tr h="100"><a:tc><a:txBody><a:bodyPr/><a:p><a:r><a:t>Cell</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc></a:tr>
          </a:tbl>
        </a:graphicData></a:graphic>
      </p:graphicFrame>
      <p:sp>
        <p:nvSpPr><p:cNvPr id="4" name="Footer"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
        <p:spPr><a:xfrm><a:off x="0" y="8000000"/><a:ext cx="1000" cy="1000"/></a:xfrm></p:spPr>
        <p:txBody><a:bodyPr/><a:p><a:r><a:t>Footer text</a:t></a:r></a:p></p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let ctx = test_ctx();
        let shapes = parse_slide_shapes(xml, &ctx).unwrap();
        assert_eq!(shapes.len(), 3);

        // Y-sorted: Title (100000) < Table (2000000) < Footer (8000000)
        assert!(matches!(shapes[0].content, ShapeContent::Text(_)));
        assert!(matches!(shapes[1].content, ShapeContent::Table(_)));
        assert!(matches!(shapes[2].content, ShapeContent::Text(_)));
    }

    #[test]
    fn deeply_nested_groups_are_depth_limited() {
        // Build a slide with groups nested beyond limits. Each group adds
        // multiple XML child elements (nvGrpSpPr, grpSpPr), so the XML
        // parser's own nesting limit (256 elements) kicks in well before
        // MAX_SHAPE_DEPTH. Either way, deeply nested groups must not crash.
        let mut xml = String::from(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr/>"#,
        );

        // 100 levels deep: each group has ~3 XML child elements so this
        // hits ~300+ XML depth, triggering the XML parser depth limit.
        let depth = 100;
        for i in 0..depth {
            let id = i + 10;
            xml.push_str(&format!(
                r#"<p:grpSp>
<p:nvGrpSpPr><p:cNvPr id="{id}" name="G{i}"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
<p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="100" cy="100"/><a:chOff x="0" y="0"/><a:chExt cx="100" cy="100"/></a:xfrm></p:grpSpPr>"#
            ));
        }
        // Innermost shape
        xml.push_str(
            r#"<p:sp>
<p:nvSpPr><p:cNvPr id="999" name="Leaf"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
<p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="100" cy="100"/></a:xfrm></p:spPr>
<p:txBody><a:bodyPr/><a:p><a:r><a:t>deep</a:t></a:r></a:p></p:txBody>
</p:sp>"#,
        );
        for _ in 0..depth {
            xml.push_str("</p:grpSp>");
        }
        xml.push_str("</p:spTree></p:cSld></p:sld>");

        let ctx = test_ctx();
        // Deep nesting should error (XML depth limit) or succeed with
        // truncated shapes. Either outcome is safe; crashing is not.
        match parse_slide_shapes(xml.as_bytes(), &ctx) {
            Ok(shapes) => {
                // If parsing succeeds, deeply nested content may be dropped.
                assert!(shapes.len() <= 1);
            }
            Err(e) => {
                // XML parser depth limit is the expected first defense.
                let msg = format!("{e}");
                assert!(
                    msg.contains("depth") || msg.contains("nesting"),
                    "expected depth-related error, got: {msg}"
                );
            }
        }
    }
}

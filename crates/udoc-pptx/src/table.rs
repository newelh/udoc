//! DrawingML table extraction from PPTX graphic frames.
//!
//! Parses `a:tbl` elements within `a:graphicData` to extract table
//! structure with cell merging (gridSpan, rowSpan, hMerge, vMerge).

use udoc_containers::xml::{attr_value, XmlEvent, XmlReader};
use udoc_core::diagnostics::Warning;
use udoc_core::error::{Result, ResultExt};

use crate::shapes::{ExtractedTable, ExtractedTableCell, ExtractedTableRow, SlideContext};
use crate::text::{is_drawingml, parse_text_body_with_depth, skip_element};

/// Parse a `a:tbl` element from within a `a:graphicData`.
///
/// The reader should be positioned after the `a:graphicData` StartElement.
/// This function reads until the graphicData EndElement.
pub(crate) fn parse_table(
    reader: &mut XmlReader<'_>,
    depth: &mut u32,
    ctx: &SlideContext<'_>,
) -> Result<ExtractedTable> {
    let start_depth = *depth;
    let mut rows = Vec::new();
    let mut num_columns: usize = 0;
    let mut row_limit_warned = false;

    loop {
        let event = reader.next_event().context("reading table")?;
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ..
            } => {
                *depth = depth.saturating_add(1);
                let name = local_name.as_ref();

                if is_drawingml(namespace_uri) {
                    match name {
                        "tblGrid" => {
                            // Count grid columns
                            num_columns = parse_table_grid(reader, depth)?;
                        }
                        "tr" => {
                            if rows.len() >= crate::MAX_TABLE_ROWS {
                                if !row_limit_warned {
                                    ctx.diag.warning(Warning::new(
                                        "PptxTableRowLimit",
                                        format!(
                                            "table exceeds {} row limit",
                                            crate::MAX_TABLE_ROWS
                                        ),
                                    ));
                                    row_limit_warned = true;
                                }
                                skip_element(reader, depth)?;
                            } else {
                                let row = parse_table_row(reader, depth, ctx)?;
                                rows.push(row);
                            }
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

    // If tblGrid wasn't present, compute from row cell counts
    if num_columns == 0 {
        num_columns = rows
            .iter()
            .map(|r| r.cells.iter().map(|c| c.col_span).sum())
            .max()
            .unwrap_or(0);
    }

    Ok(ExtractedTable { rows, num_columns })
}

/// Count columns from `a:tblGrid/a:gridCol` elements.
fn parse_table_grid(reader: &mut XmlReader<'_>, depth: &mut u32) -> Result<usize> {
    let start_depth = *depth;
    let mut count = 0;

    loop {
        let event = reader.next_event().context("reading table grid")?;
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ..
            } => {
                *depth = depth.saturating_add(1);
                if is_drawingml(namespace_uri) && local_name.as_ref() == "gridCol" {
                    count += 1;
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

    Ok(count)
}

/// Parse a `a:tr` table row element.
fn parse_table_row(
    reader: &mut XmlReader<'_>,
    depth: &mut u32,
    ctx: &SlideContext<'_>,
) -> Result<ExtractedTableRow> {
    let start_depth = *depth;
    let mut cells = Vec::new();
    let mut cell_limit_warned = false;

    loop {
        let event = reader.next_event().context("reading table row")?;
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ref attributes,
                ..
            } => {
                *depth = depth.saturating_add(1);

                if is_drawingml(namespace_uri) && local_name.as_ref() == "tc" {
                    if cells.len() >= crate::MAX_CELLS_PER_ROW {
                        if !cell_limit_warned {
                            ctx.diag.warning(Warning::new(
                                "PptxTableCellLimit",
                                format!(
                                    "table row exceeds {} cell limit",
                                    crate::MAX_CELLS_PER_ROW
                                ),
                            ));
                            cell_limit_warned = true;
                        }
                        skip_element(reader, depth)?;
                    } else {
                        let cell = parse_table_cell(reader, depth, attributes, ctx)?;
                        cells.push(cell);
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

    Ok(ExtractedTableRow { cells })
}

/// Parse a `a:tc` table cell element.
fn parse_table_cell(
    reader: &mut XmlReader<'_>,
    depth: &mut u32,
    attrs: &[udoc_containers::xml::Attribute<'_>],
    ctx: &SlideContext<'_>,
) -> Result<ExtractedTableCell> {
    let start_depth = *depth;

    let col_span = attr_value(attrs, "gridSpan")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, crate::MAX_CELLS_PER_ROW);
    let row_span = attr_value(attrs, "rowSpan")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, crate::MAX_TABLE_ROWS);
    let is_h_merge = attr_value(attrs, "hMerge")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    let is_v_merge = attr_value(attrs, "vMerge")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);

    let mut paragraphs = Vec::new();

    loop {
        let event = reader.next_event().context("reading table cell")?;
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ..
            } => {
                *depth = depth.saturating_add(1);

                if is_drawingml(namespace_uri) && local_name.as_ref() == "txBody" {
                    paragraphs = parse_text_body_with_depth(reader, depth, ctx)?;
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

    Ok(ExtractedTableCell {
        paragraphs,
        col_span,
        row_span,
        is_h_merge,
        is_v_merge,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn parse_table_from_xml(xml: &[u8]) -> ExtractedTable {
        use udoc_core::diagnostics::NullDiagnostics;
        let mut reader = XmlReader::new(xml).unwrap();
        // Advance past the graphicData element
        loop {
            match reader.next_event().unwrap() {
                XmlEvent::StartElement { local_name, .. }
                    if local_name.as_ref() == "graphicData" =>
                {
                    break;
                }
                XmlEvent::Eof => panic!("no graphicData found"),
                _ => {}
            }
        }
        let mut depth: u32 = 1;
        let empty_theme = std::collections::HashMap::new();
        let ctx = SlideContext {
            diag: &NullDiagnostics,
            slide_index: 0,
            slide_rels: &[],
            scheme_color_warned: Cell::new(false),
            theme_colors: &empty_theme,
        };
        parse_table(&mut reader, &mut depth, &ctx).unwrap()
    }

    #[test]
    fn simple_2x2_table() {
        let xml = br#"<a:graphicData xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                     uri="http://schemas.openxmlformats.org/drawingml/2006/table">
          <a:tbl>
            <a:tblGrid>
              <a:gridCol w="3048000"/>
              <a:gridCol w="3048000"/>
            </a:tblGrid>
            <a:tr h="370840">
              <a:tc><a:txBody><a:bodyPr/><a:p><a:r><a:t>A1</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc>
              <a:tc><a:txBody><a:bodyPr/><a:p><a:r><a:t>B1</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc>
            </a:tr>
            <a:tr h="370840">
              <a:tc><a:txBody><a:bodyPr/><a:p><a:r><a:t>A2</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc>
              <a:tc><a:txBody><a:bodyPr/><a:p><a:r><a:t>B2</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc>
            </a:tr>
          </a:tbl>
        </a:graphicData>"#;

        let table = parse_table_from_xml(xml);
        assert_eq!(table.num_columns, 2);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0].cells.len(), 2);
        assert_eq!(table.rows[0].cells[0].paragraphs[0].text(), "A1");
        assert_eq!(table.rows[0].cells[1].paragraphs[0].text(), "B1");
        assert_eq!(table.rows[1].cells[0].paragraphs[0].text(), "A2");
        assert_eq!(table.rows[1].cells[1].paragraphs[0].text(), "B2");
    }

    #[test]
    fn merged_cells() {
        let xml = br#"<a:graphicData xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                                     uri="http://schemas.openxmlformats.org/drawingml/2006/table">
          <a:tbl>
            <a:tblGrid>
              <a:gridCol w="1000"/>
              <a:gridCol w="1000"/>
            </a:tblGrid>
            <a:tr h="100">
              <a:tc gridSpan="2"><a:txBody><a:bodyPr/><a:p><a:r><a:t>Merged</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc>
              <a:tc hMerge="1"><a:txBody><a:bodyPr/><a:p><a:endParaRPr/></a:p></a:txBody><a:tcPr/></a:tc>
            </a:tr>
          </a:tbl>
        </a:graphicData>"#;

        let table = parse_table_from_xml(xml);
        assert_eq!(table.rows[0].cells[0].col_span, 2);
        assert_eq!(table.rows[0].cells[0].paragraphs[0].text(), "Merged");
        assert!(table.rows[0].cells[1].is_h_merge);
    }
}

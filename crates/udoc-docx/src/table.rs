//! DOCX table (w:tbl) parser.
//!
//! Handles w:tbl/w:tr/w:tc structure including gridSpan (column merge),
//! vMerge (vertical merge), and nested tables. Maps to udoc_core Table types.

use udoc_containers::xml::{attr_value, toggle_attr, XmlEvent, XmlReader};
use udoc_core::diagnostics::Warning;

use crate::error::{Result, ResultExt};
use crate::parser::is_wml;
use crate::parser::DocxContext;
// Table cell parsing re-uses crate::parser::parse_paragraph for in-cell w:p.

/// Maximum number of rows in a single table (safety limit).
const MAX_ROWS: usize = 100_000;

/// Maximum number of cells per row (safety limit).
const MAX_CELLS_PER_ROW: usize = 10_000;

/// Maximum number of paragraphs per table cell (safety limit).
const MAX_PARAGRAPHS_PER_CELL: usize = 10_000;

/// Maximum grid column width for vMerge resolution (safety limit).
/// A gridSpan can widen the grid, so cap the total grid width to prevent
/// quadratic iteration in resolve_vmerge.
const MAX_GRID_COLUMNS: usize = 10_000;

/// A parsed DOCX table.
#[derive(Debug)]
pub struct DocxTable {
    /// Rows in the table.
    pub rows: Vec<DocxTableRow>,
}

/// A parsed table row.
#[derive(Debug)]
pub struct DocxTableRow {
    /// Cells in the row.
    pub cells: Vec<DocxTableCell>,
    /// Whether this is a header row.
    pub is_header: bool,
}

/// A parsed table cell.
#[derive(Debug)]
pub struct DocxTableCell {
    /// Text content of the cell (paragraphs joined by newlines).
    pub text: String,
    /// Column span (from w:gridSpan, default 1).
    pub col_span: usize,
    /// Row span (computed from vMerge, default 1).
    pub row_span: usize,
    /// vMerge state: Restart starts a new merge group, Continue extends it.
    pub v_merge: VMergeState,
    /// Bookmark names found inside this cell's paragraphs.
    pub bookmarks: Vec<String>,
    /// Hyperlinks found in this cell's runs: (display_text, url) pairs.
    pub hyperlinks: Vec<(String, String)>,
}

/// Vertical merge state for a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VMergeState {
    /// Not part of a vertical merge.
    None,
    /// Starts a new vertical merge group.
    Restart,
    /// Continues a vertical merge from the row above.
    Continue,
}

/// Parse a w:tbl element into a DocxTable.
pub(crate) fn parse_table(reader: &mut XmlReader<'_>, ctx: &DocxContext<'_>) -> Result<DocxTable> {
    let mut rows = Vec::new();
    let mut depth: usize = 1;

    loop {
        let event = reader.next_element().context("parsing w:tbl")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                ..
            } => {
                depth += 1;
                if is_wml(namespace_uri.as_deref()) && local_name == "tr" {
                    if rows.len() >= MAX_ROWS {
                        ctx.diag.warning(Warning::new(
                            "DocxMaxTableRows",
                            format!("table row limit ({MAX_ROWS}) exceeded, truncating"),
                        ));
                        // Consume the row's subtree to keep the reader consistent.
                        crate::parser::skip_element(reader)?;
                        depth = depth.saturating_sub(1);
                        continue;
                    }
                    let row = parse_table_row(reader, ctx)?;
                    rows.push(row);
                    depth = depth.saturating_sub(1); // parse_table_row consumed the end element
                }
            }
            XmlEvent::EndElement { .. } => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    // Resolve vMerge: compute row_span for Restart cells.
    resolve_vmerge(&mut rows);

    Ok(DocxTable { rows })
}

/// Parse a w:tr element.
fn parse_table_row(reader: &mut XmlReader<'_>, ctx: &DocxContext<'_>) -> Result<DocxTableRow> {
    let mut cells = Vec::new();
    let mut is_header = false;
    let mut depth: usize = 1;
    let mut in_trpr = false;

    loop {
        let event = reader.next_element().context("parsing w:tr")?;

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
                        "trPr" => {
                            in_trpr = true;
                        }
                        "tblHeader" if in_trpr => {
                            is_header = toggle_attr(attr_value(&attributes, "val"));
                        }
                        "tc" if !in_trpr => {
                            if cells.len() >= MAX_CELLS_PER_ROW {
                                ctx.diag.warning(Warning::new(
                                    "DocxMaxTableCells",
                                    format!("cell limit ({MAX_CELLS_PER_ROW}) exceeded in row"),
                                ));
                                // Consume the cell's subtree to keep the reader consistent.
                                crate::parser::skip_element(reader)?;
                                depth = depth.saturating_sub(1);
                                continue;
                            }
                            let cell = parse_table_cell(reader, ctx)?;
                            cells.push(cell);
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
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
                if is_wml(namespace_uri.as_deref()) && local_name == "trPr" {
                    in_trpr = false;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(DocxTableRow { cells, is_header })
}

/// Parse a w:tc element.
fn parse_table_cell(reader: &mut XmlReader<'_>, ctx: &DocxContext<'_>) -> Result<DocxTableCell> {
    let mut text_parts: Vec<String> = Vec::new();
    let mut col_span: usize = 1;
    let mut v_merge = VMergeState::None;
    let mut depth: usize = 1;
    let mut in_tcpr = false;
    let mut cell_bookmarks: Vec<String> = Vec::new();
    let mut cell_hyperlinks: Vec<(String, String)> = Vec::new();

    loop {
        let event = reader.next_element().context("parsing w:tc")?;

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
                        "tcPr" => {
                            in_tcpr = true;
                        }
                        "gridSpan" if in_tcpr => {
                            if let Some(val) = attr_value(&attributes, "val") {
                                if let Ok(span) = val.parse::<usize>() {
                                    if span == 0 {
                                        ctx.diag.warning(Warning::new(
                                            "DocxInvalidGridSpan",
                                            "gridSpan=0 is invalid, clamping to 1",
                                        ));
                                    }
                                    col_span = span.clamp(1, MAX_CELLS_PER_ROW);
                                }
                            }
                        }
                        "vMerge" if in_tcpr => {
                            let val = attr_value(&attributes, "val");
                            v_merge = match val {
                                Some("restart") => VMergeState::Restart,
                                // Absent val or empty val means Continue.
                                _ => VMergeState::Continue,
                            };
                        }
                        "p" if !in_tcpr => {
                            if text_parts.len() >= MAX_PARAGRAPHS_PER_CELL {
                                ctx.diag.warning(Warning::new(
                                    "DocxMaxParagraphsPerCell",
                                    format!(
                                        "paragraph limit ({MAX_PARAGRAPHS_PER_CELL}) exceeded in table cell, truncating"
                                    ),
                                ));
                                crate::parser::skip_element(reader)?;
                                depth = depth.saturating_sub(1);
                                continue;
                            }
                            let para = crate::parser::parse_paragraph(
                                reader,
                                &attributes,
                                ctx,
                                &mut cell_bookmarks,
                            )
                            .context("parsing paragraph in table cell")?;
                            let para_text: String = para
                                .runs
                                .iter()
                                .filter(|r| !r.invisible)
                                .map(|r| r.text.as_str())
                                .collect();
                            text_parts.push(para_text);
                            for run in &para.runs {
                                if !run.invisible {
                                    if let Some(ref url) = run.hyperlink_url {
                                        cell_hyperlinks.push((run.text.clone(), url.clone()));
                                    }
                                }
                            }
                            depth = depth.saturating_sub(1);
                        }
                        "tbl" if !in_tcpr => {
                            // Nested table: flatten text with a warning.
                            ctx.diag.warning(Warning::new(
                                "DocxNestedTable",
                                "nested table flattened into parent cell text",
                            ));
                            let nested = parse_table(reader, ctx)?;
                            let nested_text = flatten_table_text(&nested);
                            text_parts.push(nested_text);
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
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
                if is_wml(namespace_uri.as_deref()) && local_name == "tcPr" {
                    in_tcpr = false;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    let text = text_parts.join("\n");

    Ok(DocxTableCell {
        text,
        col_span,
        row_span: 1, // Will be resolved by resolve_vmerge.
        v_merge,
        bookmarks: cell_bookmarks,
        hyperlinks: cell_hyperlinks,
    })
}

/// Flatten a nested table's text into a single string.
/// Filters out vMerge Continue cells, consistent with all other extraction paths.
fn flatten_table_text(table: &DocxTable) -> String {
    let mut parts = Vec::new();
    for row in &table.rows {
        let row_text: Vec<&str> = row
            .cells
            .iter()
            .filter(|c| c.v_merge != VMergeState::Continue)
            .map(|c| c.text.as_str())
            .collect();
        parts.push(row_text.join("\t"));
    }
    parts.join("\n")
}

/// Resolve vMerge: compute row_span for Restart cells by counting consecutive
/// Continue cells below each Restart cell in the same grid column.
///
/// Uses grid column positions (accounting for gridSpan) rather than cell
/// array indices, so vMerge works correctly when rows have different numbers
/// of cells due to column spanning.
fn resolve_vmerge(rows: &mut [DocxTableRow]) {
    if rows.is_empty() {
        return;
    }

    // Compute the maximum grid column across all rows (saturating to avoid overflow).
    let max_grid_col = rows
        .iter()
        .map(|r| {
            r.cells
                .iter()
                .fold(0usize, |acc, c| acc.saturating_add(c.col_span))
        })
        .max()
        .unwrap_or(0);

    // Cap grid width to prevent quadratic iteration on adversarial gridSpan values.
    // If the grid is wider than MAX_GRID_COLUMNS, columns beyond the limit won't
    // get vMerge resolution (they keep row_span=1), which is an acceptable degradation.
    let effective_grid_cols = max_grid_col.min(MAX_GRID_COLUMNS);

    // For each grid column, walk rows and resolve vMerge.
    for grid_col in 0..effective_grid_cols {
        // Track the restart cell location: (row_idx, cell_idx).
        let mut restart: Option<(usize, usize)> = None;
        let mut span_count: usize = 1;

        for row_idx in 0..rows.len() {
            // Find which cell (if any) starts at this grid column.
            let cell_at_col = find_cell_at_grid_col(&rows[row_idx], grid_col);

            match cell_at_col {
                Some(cell_idx) => {
                    match rows[row_idx].cells[cell_idx].v_merge {
                        VMergeState::Restart => {
                            // Finalize previous merge group.
                            if let Some((rr, rc)) = restart {
                                rows[rr].cells[rc].row_span = span_count;
                            }
                            restart = Some((row_idx, cell_idx));
                            span_count = 1;
                        }
                        VMergeState::Continue => {
                            if restart.is_some() {
                                span_count += 1;
                            }
                        }
                        VMergeState::None => {
                            if let Some((rr, rc)) = restart {
                                rows[rr].cells[rc].row_span = span_count;
                            }
                            restart = None;
                            span_count = 1;
                        }
                    }
                }
                None => {
                    // No cell at this grid column in this row. Finalize.
                    if let Some((rr, rc)) = restart {
                        rows[rr].cells[rc].row_span = span_count;
                    }
                    restart = None;
                    span_count = 1;
                }
            }
        }

        // Finalize last merge group.
        if let Some((rr, rc)) = restart {
            rows[rr].cells[rc].row_span = span_count;
        }
    }
}

/// Find the cell index in a row whose starting grid column matches `target_col`.
/// Returns None if no cell starts at that grid column.
fn find_cell_at_grid_col(row: &DocxTableRow, target_col: usize) -> Option<usize> {
    let mut grid_col = 0;
    for (cell_idx, cell) in row.cells.iter().enumerate() {
        if grid_col == target_col {
            return Some(cell_idx);
        }
        grid_col = grid_col.saturating_add(cell.col_span);
        if grid_col > target_col {
            return None; // target_col is spanned over by a previous cell
        }
    }
    None
}

/// Convert a DocxTable to a core Table type.
///
/// vMerge Continue cells are filtered out (they are covered by the Restart
/// cell's row_span). This means rows with vertical merges have fewer explicit
/// cells, matching the HTML rowspan model: num_columns reflects the grid width,
/// and rows under a rowspan simply have fewer cells.
pub fn convert_table(table: &DocxTable) -> udoc_core::table::Table {
    let rows: Vec<udoc_core::table::TableRow> = table
        .rows
        .iter()
        .map(|row| {
            let cells: Vec<udoc_core::table::TableCell> = row
                .cells
                .iter()
                .filter(|c| c.v_merge != VMergeState::Continue)
                .map(|cell| {
                    udoc_core::table::TableCell::with_spans(
                        cell.text.clone(),
                        None, // DOCX tables have no page geometry.
                        cell.col_span,
                        cell.row_span,
                    )
                })
                .collect();
            udoc_core::table::TableRow::with_header(cells, row.is_header)
        })
        .collect();

    udoc_core::table::Table::new(rows, None)
}

// Make parse_paragraph accessible from table.rs for in-cell paragraph parsing.
// We re-export it as pub(crate) from parser.rs.

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::sync::Arc;
    use udoc_core::diagnostics::{DiagnosticsSink, NullDiagnostics};

    fn null_diag() -> Arc<dyn DiagnosticsSink> {
        Arc::new(NullDiagnostics)
    }

    fn test_ctx() -> (HashMap<String, String>, Arc<dyn DiagnosticsSink>) {
        (HashMap::new(), null_diag())
    }

    #[test]
    fn parse_simple_table() {
        let xml =
            br#"<w:tbl xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:tr>
    <w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc>
    <w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc>
  </w:tr>
  <w:tr>
    <w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc>
    <w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc>
  </w:tr>
</w:tbl>"#;

        let mut reader = XmlReader::new(xml).unwrap();
        // Advance past the StartElement for w:tbl.
        loop {
            match reader.next_element().unwrap() {
                XmlEvent::StartElement { local_name, .. } if local_name == "tbl" => break,
                _ => {}
            }
        }
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        let table = parse_table(&mut reader, &ctx).unwrap();

        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0].cells.len(), 2);
        assert_eq!(table.rows[0].cells[0].text, "A1");
        assert_eq!(table.rows[0].cells[1].text, "B1");
        assert_eq!(table.rows[1].cells[0].text, "A2");
    }

    #[test]
    fn parse_col_span() {
        let xml =
            br#"<w:tbl xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:tr>
    <w:tc>
      <w:tcPr><w:gridSpan w:val="2"/></w:tcPr>
      <w:p><w:r><w:t>Merged</w:t></w:r></w:p>
    </w:tc>
  </w:tr>
</w:tbl>"#;

        let mut reader = XmlReader::new(xml).unwrap();
        loop {
            match reader.next_element().unwrap() {
                XmlEvent::StartElement { local_name, .. } if local_name == "tbl" => break,
                _ => {}
            }
        }
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        let table = parse_table(&mut reader, &ctx).unwrap();

        assert_eq!(table.rows[0].cells[0].col_span, 2);
        assert_eq!(table.rows[0].cells[0].text, "Merged");
    }

    #[test]
    fn parse_vmerge() {
        let xml =
            br#"<w:tbl xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:tr>
    <w:tc>
      <w:tcPr><w:vMerge w:val="restart"/></w:tcPr>
      <w:p><w:r><w:t>Top</w:t></w:r></w:p>
    </w:tc>
    <w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc>
  </w:tr>
  <w:tr>
    <w:tc>
      <w:tcPr><w:vMerge/></w:tcPr>
      <w:p/>
    </w:tc>
    <w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc>
  </w:tr>
  <w:tr>
    <w:tc>
      <w:tcPr><w:vMerge/></w:tcPr>
      <w:p/>
    </w:tc>
    <w:tc><w:p><w:r><w:t>B3</w:t></w:r></w:p></w:tc>
  </w:tr>
</w:tbl>"#;

        let mut reader = XmlReader::new(xml).unwrap();
        loop {
            match reader.next_element().unwrap() {
                XmlEvent::StartElement { local_name, .. } if local_name == "tbl" => break,
                _ => {}
            }
        }
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        let table = parse_table(&mut reader, &ctx).unwrap();

        // First cell of first row spans 3 rows.
        assert_eq!(table.rows[0].cells[0].row_span, 3);
        assert_eq!(table.rows[0].cells[0].v_merge, VMergeState::Restart);
        assert_eq!(table.rows[1].cells[0].v_merge, VMergeState::Continue);
        assert_eq!(table.rows[2].cells[0].v_merge, VMergeState::Continue);
    }

    #[test]
    fn parse_header_row() {
        let xml =
            br#"<w:tbl xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:tr>
    <w:trPr><w:tblHeader/></w:trPr>
    <w:tc><w:p><w:r><w:t>Header</w:t></w:r></w:p></w:tc>
  </w:tr>
</w:tbl>"#;

        let mut reader = XmlReader::new(xml).unwrap();
        loop {
            match reader.next_element().unwrap() {
                XmlEvent::StartElement { local_name, .. } if local_name == "tbl" => break,
                _ => {}
            }
        }
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        let table = parse_table(&mut reader, &ctx).unwrap();

        assert!(table.rows[0].is_header);
    }

    #[test]
    fn convert_table_to_core() {
        let table = DocxTable {
            rows: vec![DocxTableRow {
                cells: vec![
                    DocxTableCell {
                        text: "A".to_string(),
                        col_span: 1,
                        row_span: 1,
                        v_merge: VMergeState::None,
                        bookmarks: Vec::new(),
                        hyperlinks: Vec::new(),
                    },
                    DocxTableCell {
                        text: "B".to_string(),
                        col_span: 1,
                        row_span: 1,
                        v_merge: VMergeState::None,
                        bookmarks: Vec::new(),
                        hyperlinks: Vec::new(),
                    },
                ],
                is_header: true,
            }],
        };

        let core_table = convert_table(&table);
        assert_eq!(core_table.rows.len(), 1);
        assert_eq!(core_table.num_columns, 2);
        assert!(core_table.rows[0].is_header);
        assert_eq!(core_table.rows[0].cells[0].text, "A");
    }

    #[test]
    fn invisible_text_filtered_from_cells() {
        let xml =
            br#"<w:tbl xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:tr>
    <w:tc>
      <w:p>
        <w:r><w:t>Visible</w:t></w:r>
        <w:r>
          <w:rPr><w:vanish/></w:rPr>
          <w:t>Hidden</w:t>
        </w:r>
      </w:p>
    </w:tc>
  </w:tr>
</w:tbl>"#;

        let mut reader = XmlReader::new(xml).unwrap();
        loop {
            match reader.next_element().unwrap() {
                XmlEvent::StartElement { local_name, .. } if local_name == "tbl" => break,
                _ => {}
            }
        }
        let (hmap, diag) = test_ctx();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        let table = parse_table(&mut reader, &ctx).unwrap();

        assert_eq!(table.rows[0].cells[0].text, "Visible");
    }

    #[test]
    fn hyperlinks_preserved_in_table_cells() {
        let xml =
            br#"<w:tbl xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                       xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:tr>
    <w:tc>
      <w:p>
        <w:r><w:t xml:space="preserve">Plain text </w:t></w:r>
        <w:hyperlink r:id="rId1">
          <w:r><w:t>Click here</w:t></w:r>
        </w:hyperlink>
      </w:p>
    </w:tc>
  </w:tr>
</w:tbl>"#;

        let mut reader = XmlReader::new(xml).unwrap();
        loop {
            match reader.next_element().unwrap() {
                XmlEvent::StartElement { local_name, .. } if local_name == "tbl" => break,
                _ => {}
            }
        }
        let mut hmap = HashMap::new();
        hmap.insert("rId1".to_string(), "https://example.com".to_string());
        let diag = null_diag();
        let ctx = DocxContext {
            hyperlink_map: &hmap,
            diag: &diag,
            theme_color_warned: Cell::new(false),
        };
        let table = parse_table(&mut reader, &ctx).unwrap();

        assert_eq!(table.rows[0].cells[0].text, "Plain text Click here");
        assert_eq!(table.rows[0].cells[0].hyperlinks.len(), 1);
        assert_eq!(table.rows[0].cells[0].hyperlinks[0].0, "Click here");
        assert_eq!(
            table.rows[0].cells[0].hyperlinks[0].1,
            "https://example.com"
        );
    }
}

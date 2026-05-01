//! RTF table extraction.
//!
//! Converts parsed table data from the RTF parser into udoc-core Table types.

use udoc_core::table::{Table, TableCell, TableRow};

use crate::parser::{CellMerge, ParsedTable};

/// Collect all text from a cell's runs into a single string.
fn cell_text(cell: &crate::parser::ParsedTableCell) -> String {
    cell.runs.iter().map(|r| r.text.as_str()).collect()
}

/// Convert a `ParsedTable` to an udoc-core `Table`.
///
/// Handles horizontal merges: `clmgf` starts a merge group and `clmrg`
/// continues it. Text from continuation cells is appended to the merge-first
/// cell, and its `col_span` is incremented. RTF is a flow format, so bbox
/// is always None.
pub fn convert_table(parsed: &ParsedTable) -> Table {
    let rows: Vec<TableRow> = parsed
        .rows
        .iter()
        .map(|row| {
            let mut out_cells: Vec<TableCell> = Vec::new();

            for (i, cell) in row.cells.iter().enumerate() {
                let merge = row.merge_flags.get(i).copied().unwrap_or(CellMerge::None);
                let text = cell_text(cell);

                match merge {
                    CellMerge::First => {
                        // Start a new merged cell.
                        out_cells.push(TableCell::with_spans(text, None, 1, 1));
                    }
                    CellMerge::Continue => {
                        // Append text to the previous merge-first cell.
                        // If there's no preceding cell (orphaned Continue),
                        // treat it as a standalone cell to avoid data loss.
                        if let Some(prev) = out_cells.last_mut() {
                            if !text.is_empty() {
                                prev.text.push_str(&text);
                            }
                            prev.col_span += 1;
                        } else {
                            out_cells.push(TableCell::new(text, None));
                        }
                    }
                    CellMerge::None => {
                        out_cells.push(TableCell::new(text, None));
                    }
                }
            }

            TableRow::new(out_cells)
        })
        .collect();

    Table::new(rows, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{ParsedTableCell, ParsedTableRow, TextRun};

    fn run(text: &str) -> TextRun {
        TextRun {
            text: text.to_string(),
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            superscript: false,
            subscript: false,
            invisible: false,
            font_name: None,
            font_size_pts: 12.0,
            color: None,
            bg_color: None,
            hyperlink_url: None,
        }
    }

    fn cell(text: &str) -> ParsedTableCell {
        ParsedTableCell {
            runs: vec![run(text)],
        }
    }

    #[test]
    fn simple_table_no_merges() {
        let parsed = ParsedTable {
            rows: vec![
                ParsedTableRow {
                    cells: vec![cell("A"), cell("B"), cell("C")],
                    cell_boundaries: vec![3000, 6000, 9000],
                    merge_flags: vec![CellMerge::None, CellMerge::None, CellMerge::None],
                },
                ParsedTableRow {
                    cells: vec![cell("1"), cell("2"), cell("3")],
                    cell_boundaries: vec![3000, 6000, 9000],
                    merge_flags: vec![CellMerge::None, CellMerge::None, CellMerge::None],
                },
            ],
        };

        let table = convert_table(&parsed);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.num_columns, 3);
        assert!(table.bbox.is_none());

        assert_eq!(table.rows[0].cells[0].text, "A");
        assert_eq!(table.rows[0].cells[1].text, "B");
        assert_eq!(table.rows[0].cells[2].text, "C");
        assert_eq!(table.rows[1].cells[0].text, "1");

        // All cells should be 1x1.
        for row in &table.rows {
            for c in &row.cells {
                assert_eq!(c.col_span, 1);
                assert_eq!(c.row_span, 1);
            }
        }
    }

    #[test]
    fn horizontal_merge() {
        let parsed = ParsedTable {
            rows: vec![ParsedTableRow {
                cells: vec![cell("Merged"), cell("Extra"), cell("Solo")],
                cell_boundaries: vec![3000, 6000, 9000],
                merge_flags: vec![CellMerge::First, CellMerge::Continue, CellMerge::None],
            }],
        };

        let table = convert_table(&parsed);
        assert_eq!(table.rows.len(), 1);
        // Merge collapses First+Continue into one cell, plus Solo = 2 cells.
        assert_eq!(table.rows[0].cells.len(), 2);
        assert_eq!(table.rows[0].cells[0].text, "MergedExtra");
        assert_eq!(table.rows[0].cells[0].col_span, 2);
        assert_eq!(table.rows[0].cells[1].text, "Solo");
        assert_eq!(table.rows[0].cells[1].col_span, 1);
    }

    #[test]
    fn orphaned_continue_becomes_standalone_cell() {
        let parsed = ParsedTable {
            rows: vec![ParsedTableRow {
                cells: vec![cell("Orphan")],
                cell_boundaries: vec![5000],
                merge_flags: vec![CellMerge::Continue],
            }],
        };

        let table = convert_table(&parsed);
        assert_eq!(table.rows[0].cells.len(), 1);
        assert_eq!(table.rows[0].cells[0].text, "Orphan");
        assert_eq!(table.rows[0].cells[0].col_span, 1);
    }

    #[test]
    fn merge_with_empty_continuation() {
        let parsed = ParsedTable {
            rows: vec![ParsedTableRow {
                cells: vec![cell("Header"), cell("")],
                cell_boundaries: vec![4000, 8000],
                merge_flags: vec![CellMerge::First, CellMerge::Continue],
            }],
        };

        let table = convert_table(&parsed);
        assert_eq!(table.rows[0].cells.len(), 1);
        assert_eq!(table.rows[0].cells[0].text, "Header");
        assert_eq!(table.rows[0].cells[0].col_span, 2);
    }
}

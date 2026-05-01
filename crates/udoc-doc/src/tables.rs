//! Table detection from paragraph properties and cell/row marks.
//!
//! DOC tables are encoded as runs of paragraphs with `in_table` and
//! `table_row_end` flags set in their PAPX properties. Cell boundaries
//! are marked by \x07 characters in the paragraph text. This module
//! walks paragraphs and their properties, extracts tables, and returns
//! the remaining non-table paragraphs in order.
//!
//! Reference: MS-DOC 2.4.3 (table text)

use udoc_core::table::{Table, TableCell, TableRow};

use crate::properties::ParagraphProperties;
use crate::text::DocParagraph;

/// Detect tables from paragraph properties and extract them.
///
/// Returns `(tables, remaining_paragraphs)` where tables are fully
/// assembled from in-table paragraph runs, and remaining_paragraphs
/// are the non-table paragraphs in their original order.
pub fn detect_tables(
    paragraphs: &[DocParagraph],
    para_props: &[ParagraphProperties],
) -> (Vec<Table>, Vec<DocParagraph>) {
    let mut tables = Vec::new();
    let mut remaining = Vec::new();

    // Current table accumulation state
    let mut current_cells: Vec<TableCell> = Vec::new();
    let mut current_rows: Vec<TableRow> = Vec::new();

    for para in paragraphs {
        let props = find_props_for_paragraph(para, para_props);

        let in_table = props.map(|p| p.in_table).unwrap_or(false);
        let is_row_end = props.map(|p| p.table_row_end).unwrap_or(false);

        if !in_table {
            // Flush any in-progress table
            flush_table(&mut current_cells, &mut current_rows, &mut tables);
            remaining.push(para.clone());
            continue;
        }

        // Inside a table: paragraph text may contain \x07 cell marks.
        // Each \x07-terminated segment is a cell. If the paragraph has
        // no \x07, treat the whole text as one cell.
        if is_row_end {
            // Row-end paragraph: finalize any pending cells, then
            // close the row. The row-end paragraph itself is a
            // terminator (its text is typically just \x07\r).
            // Add any accumulated cells as a row.
            if !current_cells.is_empty() {
                current_rows.push(TableRow::new(std::mem::take(&mut current_cells)));
            }
        } else {
            // Regular in-table paragraph: extract cells from \x07 marks
            let cell_texts = split_cell_text(&para.text);
            for text in cell_texts {
                current_cells.push(TableCell::new(text, None));
            }
        }
    }

    // Flush any remaining table at end of paragraphs
    flush_table(&mut current_cells, &mut current_rows, &mut tables);

    (tables, remaining)
}

/// Split paragraph text on \x07 cell marks, stripping the marks.
///
/// "cell1\x07cell2\x07" -> ["cell1", "cell2"]
/// "no marks" -> ["no marks"]
fn split_cell_text(text: &str) -> Vec<String> {
    if !text.contains('\x07') {
        return vec![text.to_string()];
    }

    text.split('\x07')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// Find the ParagraphProperties entry whose CP range overlaps this paragraph.
fn find_props_for_paragraph<'a>(
    para: &DocParagraph,
    props: &'a [ParagraphProperties],
) -> Option<&'a ParagraphProperties> {
    // Properties are sorted by cp_start. Find the first one that overlaps
    // the paragraph's CP range.
    props.iter().find(|p| {
        // Overlap: para starts before prop ends AND para ends after prop starts
        para.cp_start < p.cp_end && para.cp_end > p.cp_start
    })
}

/// Flush accumulated cells/rows into a Table if there's anything to emit.
fn flush_table(cells: &mut Vec<TableCell>, rows: &mut Vec<TableRow>, tables: &mut Vec<Table>) {
    // If there are leftover cells (no row-end seen), flush them as a row
    if !cells.is_empty() {
        rows.push(TableRow::new(std::mem::take(cells)));
    }
    if !rows.is_empty() {
        tables.push(Table::new(std::mem::take(rows), None));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(text: &str, cp_start: u32, cp_end: u32) -> DocParagraph {
        DocParagraph {
            text: text.to_string(),
            cp_start,
            cp_end,
        }
    }

    fn pprop(
        cp_start: u32,
        cp_end: u32,
        in_table: bool,
        table_row_end: bool,
    ) -> ParagraphProperties {
        ParagraphProperties {
            cp_start,
            cp_end,
            istd: 0,
            in_table,
            table_row_end,
        }
    }

    #[test]
    fn no_tables_all_paragraphs_pass_through() {
        let paras = vec![para("Hello", 0, 5), para("World", 6, 11)];
        let props = vec![pprop(0, 5, false, false), pprop(6, 11, false, false)];

        let (tables, remaining) = detect_tables(&paras, &props);
        assert!(tables.is_empty());
        assert_eq!(remaining.len(), 2);
        assert_eq!(remaining[0].text, "Hello");
        assert_eq!(remaining[1].text, "World");
    }

    #[test]
    fn single_row_table() {
        // Two cell paragraphs followed by a row-end paragraph
        let paras = vec![
            para("A\x07", 0, 2),
            para("B\x07", 2, 4),
            para("\x07", 4, 5), // row end
        ];
        let props = vec![
            pprop(0, 2, true, false),
            pprop(2, 4, true, false),
            pprop(4, 5, true, true),
        ];

        let (tables, remaining) = detect_tables(&paras, &props);
        assert!(remaining.is_empty());
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].rows.len(), 1);
        assert_eq!(tables[0].rows[0].cells.len(), 2);
        assert_eq!(tables[0].rows[0].cells[0].text, "A");
        assert_eq!(tables[0].rows[0].cells[1].text, "B");
    }

    #[test]
    fn two_row_table() {
        let paras = vec![
            para("A\x07", 0, 2),
            para("B\x07", 2, 4),
            para("\x07", 4, 5),
            para("C\x07", 5, 7),
            para("D\x07", 7, 9),
            para("\x07", 9, 10),
        ];
        let props = vec![
            pprop(0, 2, true, false),
            pprop(2, 4, true, false),
            pprop(4, 5, true, true),
            pprop(5, 7, true, false),
            pprop(7, 9, true, false),
            pprop(9, 10, true, true),
        ];

        let (tables, remaining) = detect_tables(&paras, &props);
        assert!(remaining.is_empty());
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].rows.len(), 2);
        assert_eq!(tables[0].rows[0].cells.len(), 2);
        assert_eq!(tables[0].rows[1].cells.len(), 2);
        assert_eq!(tables[0].rows[0].cells[0].text, "A");
        assert_eq!(tables[0].rows[1].cells[0].text, "C");
    }

    #[test]
    fn table_between_paragraphs() {
        let paras = vec![
            para("Before", 0, 6),
            para("X\x07", 7, 9),
            para("\x07", 9, 10),
            para("After", 11, 16),
        ];
        let props = vec![
            pprop(0, 6, false, false),
            pprop(7, 9, true, false),
            pprop(9, 10, true, true),
            pprop(11, 16, false, false),
        ];

        let (tables, remaining) = detect_tables(&paras, &props);
        assert_eq!(tables.len(), 1);
        assert_eq!(remaining.len(), 2);
        assert_eq!(remaining[0].text, "Before");
        assert_eq!(remaining[1].text, "After");
    }

    #[test]
    fn no_props_treats_all_as_non_table() {
        let paras = vec![para("Hello", 0, 5)];
        let props: Vec<ParagraphProperties> = vec![];

        let (tables, remaining) = detect_tables(&paras, &props);
        assert!(tables.is_empty());
        assert_eq!(remaining.len(), 1);
    }

    #[test]
    fn cell_text_without_mark() {
        // If a paragraph has no \x07, the entire text becomes a cell
        let paras = vec![para("full cell", 0, 9), para("\x07", 9, 10)];
        let props = vec![pprop(0, 9, true, false), pprop(9, 10, true, true)];

        let (tables, remaining) = detect_tables(&paras, &props);
        assert!(remaining.is_empty());
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].rows[0].cells[0].text, "full cell");
    }

    #[test]
    fn multiple_cells_in_single_paragraph() {
        // Single paragraph with multiple cell marks
        let paras = vec![para("A\x07B\x07C\x07", 0, 6), para("\x07", 6, 7)];
        let props = vec![pprop(0, 6, true, false), pprop(6, 7, true, true)];

        let (tables, remaining) = detect_tables(&paras, &props);
        assert!(remaining.is_empty());
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].rows[0].cells.len(), 3);
        assert_eq!(tables[0].rows[0].cells[0].text, "A");
        assert_eq!(tables[0].rows[0].cells[1].text, "B");
        assert_eq!(tables[0].rows[0].cells[2].text, "C");
    }

    #[test]
    fn table_bbox_is_none() {
        // DOC tables have no geometry
        let paras = vec![para("X\x07", 0, 2), para("\x07", 2, 3)];
        let props = vec![pprop(0, 2, true, false), pprop(2, 3, true, true)];

        let (tables, _) = detect_tables(&paras, &props);
        assert!(tables[0].bbox.is_none());
    }

    #[test]
    fn in_table_without_row_end_flushes_at_boundary() {
        // Table paragraphs followed by non-table, without explicit row-end
        let paras = vec![
            para("A\x07", 0, 2),
            para("B\x07", 2, 4),
            para("After", 5, 10),
        ];
        let props = vec![
            pprop(0, 2, true, false),
            pprop(2, 4, true, false),
            pprop(5, 10, false, false),
        ];

        let (tables, remaining) = detect_tables(&paras, &props);
        assert_eq!(tables.len(), 1);
        // Without row-end, all cells flush into one row
        assert_eq!(tables[0].rows.len(), 1);
        assert_eq!(tables[0].rows[0].cells.len(), 2);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].text, "After");
    }

    #[test]
    fn split_cell_text_basic() {
        assert_eq!(split_cell_text("A\x07B\x07"), vec!["A", "B"]);
        assert_eq!(split_cell_text("no marks"), vec!["no marks"]);
        assert_eq!(split_cell_text("\x07"), Vec::<String>::new());
        assert_eq!(split_cell_text("only\x07"), vec!["only"]);
    }
}

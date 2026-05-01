//! Table output types for document extraction.
//!
//! These types represent tables extracted from any document format.
//! PDF tables are detected from geometry; DOCX/XLSX tables are parsed
//! from markup. The output representation is format-agnostic.

use std::fmt;

use crate::geometry::BoundingBox;

/// A table extracted from a document page.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Table {
    /// Bounding box of the table on the page. None for formats without
    /// page geometry (e.g., DOCX tables have no absolute position).
    pub bbox: Option<BoundingBox>,
    /// Rows in the table, ordered top-to-bottom.
    pub rows: Vec<TableRow>,
    /// Number of columns (maximum cell count across all rows).
    pub num_columns: usize,
    /// Number of header rows at the top of the table.
    pub header_row_count: usize,
    /// Whether this table may continue from the previous page.
    pub may_continue_from_previous: bool,
    /// Whether this table may continue to the next page.
    pub may_continue_to_next: bool,
}

impl Table {
    /// Create a new table. Computes `num_columns` from the rows,
    /// accounting for col_span on merged cells.
    pub fn new(rows: Vec<TableRow>, bbox: Option<BoundingBox>) -> Self {
        let num_columns = Self::compute_num_columns(&rows);
        let header_row_count = rows.iter().take_while(|r| r.is_header).count();
        Self {
            bbox,
            rows,
            num_columns,
            header_row_count,
            may_continue_from_previous: false,
            may_continue_to_next: false,
        }
    }

    /// Create a table with all fields specified.
    pub fn with_continuation(
        rows: Vec<TableRow>,
        bbox: Option<BoundingBox>,
        may_continue_from_previous: bool,
        may_continue_to_next: bool,
    ) -> Self {
        let num_columns = Self::compute_num_columns(&rows);
        let header_row_count = rows.iter().take_while(|r| r.is_header).count();
        Self {
            bbox,
            rows,
            num_columns,
            header_row_count,
            may_continue_from_previous,
            may_continue_to_next,
        }
    }

    /// Compute num_columns as the max sum of col_spans across all rows.
    /// This correctly handles merged cells (col_span > 1).
    fn compute_num_columns(rows: &[TableRow]) -> usize {
        rows.iter()
            .map(|r| r.cells.iter().map(|c| c.col_span).sum())
            .max()
            .unwrap_or(0)
    }

    /// Number of rows in the table.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Number of columns in the table.
    pub fn col_count(&self) -> usize {
        self.num_columns
    }
}

/// A row within a table.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TableRow {
    /// Cells in this row, ordered left-to-right.
    pub cells: Vec<TableCell>,
    /// Whether this row is a header row.
    pub is_header: bool,
}

impl TableRow {
    /// Create a new non-header row.
    pub fn new(cells: Vec<TableCell>) -> Self {
        Self {
            cells,
            is_header: false,
        }
    }

    /// Create a row with explicit header flag.
    pub fn with_header(cells: Vec<TableCell>, is_header: bool) -> Self {
        Self { cells, is_header }
    }
}

/// A cell within a table row.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TableCell {
    /// Text content of the cell.
    pub text: String,
    /// Bounding box of the cell. None for formats without geometry.
    pub bbox: Option<BoundingBox>,
    /// Number of columns this cell spans (1 for normal cells).
    pub col_span: usize,
    /// Number of rows this cell spans (1 for normal cells).
    pub row_span: usize,
}

impl TableCell {
    /// Create a new cell with default spans (1x1).
    pub fn new(text: String, bbox: Option<BoundingBox>) -> Self {
        Self {
            text,
            bbox,
            col_span: 1,
            row_span: 1,
        }
    }

    /// Create a cell with explicit span values.
    pub fn with_spans(
        text: String,
        bbox: Option<BoundingBox>,
        col_span: usize,
        row_span: usize,
    ) -> Self {
        Self {
            text,
            bbox,
            col_span,
            row_span,
        }
    }
}

impl fmt::Display for Table {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Table ({}x{})", self.rows.len(), self.num_columns)
    }
}

impl fmt::Display for TableRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cells: Vec<&str> = self.cells.iter().map(|c| c.text.as_str()).collect();
        if self.is_header {
            write!(f, "[header] {}", cells.join(" | "))
        } else {
            write!(f, "{}", cells.join(" | "))
        }
    }
}

impl fmt::Display for TableCell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.col_span > 1 || self.row_span > 1 {
            write!(
                f,
                "{} (span {}x{})",
                self.text, self.col_span, self.row_span
            )
        } else {
            write!(f, "{}", self.text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_new_computes_columns() {
        let table = Table::new(
            vec![
                TableRow::new(vec![
                    TableCell::new("A".into(), None),
                    TableCell::new("B".into(), None),
                    TableCell::new("C".into(), None),
                ]),
                TableRow::new(vec![
                    TableCell::new("1".into(), None),
                    TableCell::new("2".into(), None),
                ]),
            ],
            None,
        );
        assert_eq!(table.num_columns, 3);
        assert_eq!(table.row_count(), 2);
        assert_eq!(table.header_row_count, 0);
    }

    #[test]
    fn table_header_count() {
        let mut header = TableRow::new(vec![TableCell::new("H".into(), None)]);
        header.is_header = true;
        let table = Table::new(
            vec![
                header,
                TableRow::new(vec![TableCell::new("D".into(), None)]),
            ],
            None,
        );
        assert_eq!(table.header_row_count, 1);
    }

    #[test]
    fn cell_display() {
        let simple = TableCell::new("value".into(), None);
        assert_eq!(format!("{simple}"), "value");

        let spanned = TableCell {
            text: "merged".into(),
            bbox: None,
            col_span: 2,
            row_span: 3,
        };
        assert_eq!(format!("{spanned}"), "merged (span 2x3)");
    }

    #[test]
    fn table_display() {
        let table = Table::new(
            vec![TableRow::new(vec![
                TableCell::new("A".into(), None),
                TableCell::new("B".into(), None),
            ])],
            None,
        );
        assert_eq!(format!("{table}"), "Table (1x2)");
    }
}

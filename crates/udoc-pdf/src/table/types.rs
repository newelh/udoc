//! Public types for table extraction from PDF pages.
//!
//! These types represent tables detected by analyzing ruled lines and
//! text alignment patterns in PDF content streams.

use std::fmt;

use crate::geometry::BoundingBox;

/// A detected table on a PDF page.
///
/// Tables are detected from ruled lines (drawn paths) or text alignment
/// patterns. Each table contains rows of cells, with column span and
/// row span support for merged cells.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Table {
    /// Bounding box of the entire table in page coordinates.
    pub bbox: BoundingBox,
    /// Rows in the table, ordered top-to-bottom.
    pub rows: Vec<TableRow>,
    /// Number of columns in the table (maximum cell count across all rows).
    pub num_columns: usize,
    /// How this table was detected.
    pub detection_method: TableDetectionMethod,
    /// X coordinates of column boundaries, sorted ascending. Semantics vary
    /// by detector: lattice tables store all grid X positions (N+1 boundaries),
    /// h-line tables store gap-derived boundaries, text-alignment stores left
    /// edges only. Useful for multi-page table merging where column structure
    /// must match.
    pub column_positions: Vec<f64>,
    /// Whether this table's bbox touches the top page margin,
    /// suggesting it may be a continuation from the previous page.
    pub may_continue_from_previous: bool,
    /// Whether this table's bbox touches the bottom page margin,
    /// suggesting it may continue on the next page.
    pub may_continue_to_next: bool,
}

impl Table {
    /// Create a new table with the given rows and detection method.
    /// Computes `num_columns` from the rows. Sets default values for
    /// continuation flags and column positions.
    pub fn new(bbox: BoundingBox, rows: Vec<TableRow>, method: TableDetectionMethod) -> Self {
        let num_columns = rows.iter().map(|r| r.cells.len()).max().unwrap_or(0);
        Self {
            bbox,
            rows,
            num_columns,
            detection_method: method,
            column_positions: Vec::new(),
            may_continue_from_previous: false,
            may_continue_to_next: false,
        }
    }
}

/// A row within a detected table.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TableRow {
    /// Cells in this row, ordered left-to-right.
    pub cells: Vec<TableCell>,
    /// Whether this row appears to be a header row (e.g., bold text,
    /// top of table, separated by a heavier ruled line).
    pub is_header: bool,
}

impl TableRow {
    /// Create a new non-header row with the given cells.
    pub fn new(cells: Vec<TableCell>) -> Self {
        Self {
            cells,
            is_header: false,
        }
    }
}

/// A cell within a table row.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TableCell {
    /// Text content of the cell.
    pub text: String,
    /// Bounding box of the cell in page coordinates.
    pub bbox: BoundingBox,
    /// Number of columns this cell spans (1 for normal cells).
    pub col_span: usize,
    /// Number of rows this cell spans (1 for normal cells).
    pub row_span: usize,
}

impl TableCell {
    /// Create a new cell with the given text and bounding box.
    /// Column span and row span default to 1.
    pub fn new(text: String, bbox: BoundingBox) -> Self {
        Self {
            text,
            bbox,
            col_span: 1,
            row_span: 1,
        }
    }
}

/// How a table was detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TableDetectionMethod {
    /// Detected from horizontal and vertical ruled lines (drawn paths)
    /// forming a full grid pattern (lattice detector).
    RuledLine,
    /// Detected from horizontal rules combined with text-based column
    /// detection. Rows may be coarse (mega-rows that need splitting).
    HLine,
    /// Detected from aligned text columns without explicit ruled lines.
    TextAlignment,
}

/// A clipping region captured from a W/W* operator.
///
/// Emitted by the content interpreter when a path is marked as a clipping
/// boundary. The polygon vertices are in device coordinates (already
/// multiplied by the CTM snapshot taken at the time of the W/W* op).
/// Curves are pre-flattened to line segments with the same tolerance as
/// visible fills.
///
/// Consumers intersect the list of `ClipPathIR` entries attached to a
/// subsequent paint op to produce the effective clip mask per
/// ISO 32000-2 §8.5.4.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipPathIR {
    /// Subpaths in device coordinates. Each subpath is implicitly closed.
    pub subpaths: Vec<Vec<(f64, f64)>>,
    /// Fill rule selected by the clip operator (`W` -> `NonZeroWinding`,
    /// `W*` -> `EvenOdd`).
    pub fill_rule: FillRule,
}

/// A path segment captured from the content stream.
///
/// Used by the table detector to find ruled lines that form table grids.
/// Paths are captured during content stream interpretation when path
/// painting operators (S, s, f, F, f*, B, B*, b, b*) are encountered.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PathSegment {
    /// The geometric shape of this path segment.
    pub kind: PathSegmentKind,
    /// Whether this path was stroked (S, s, B, B*, b, b*).
    pub stroked: bool,
    /// Whether this path was filled (f, F, f*, B, B*, b, b*).
    pub filled: bool,
    /// Stroke line width in device-space units (scaled by CTM).
    pub line_width: f64,
    /// Stroking color as RGB at the time the path was painted.
    pub stroke_color: [u8; 3],
    /// Non-stroking (fill) color as RGB at the time the path was painted.
    pub fill_color: [u8; 3],
    /// Content stream render order for z-ordering.
    pub z_index: u32,
    /// Fill opacity (0-255). 255 = fully opaque.
    pub fill_alpha: u8,
    /// Stroke opacity (0-255). 255 = fully opaque.
    pub stroke_alpha: u8,
    /// Active clip regions at the moment this path was painted.
    /// Empty = no clipping. Non-empty = all regions must be intersected to
    /// form the effective clip mask for this path (ISO 32000-2 §8.5.4).
    pub active_clips: Vec<ClipPathIR>,
}

impl PathSegment {
    /// Create a line path segment.
    pub fn line(x1: f64, y1: f64, x2: f64, y2: f64, line_width: f64) -> Self {
        Self {
            kind: PathSegmentKind::Line { x1, y1, x2, y2 },
            stroked: true,
            filled: false,
            line_width,
            stroke_color: [0, 0, 0],
            fill_color: [0, 0, 0],
            z_index: 0,
            fill_alpha: 255,
            stroke_alpha: 255,
            active_clips: Vec::new(),
        }
    }

    /// Create a rectangle path segment.
    pub fn rect(x: f64, y: f64, width: f64, height: f64, line_width: f64) -> Self {
        Self {
            kind: PathSegmentKind::Rect {
                x,
                y,
                width,
                height,
            },
            stroked: true,
            filled: false,
            line_width,
            stroke_color: [0, 0, 0],
            fill_color: [0, 0, 0],
            z_index: 0,
            fill_alpha: 255,
            stroke_alpha: 255,
            active_clips: Vec::new(),
        }
    }
}

/// Fill rule for polygon rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRule {
    NonZeroWinding,
    EvenOdd,
}

/// The geometric shape of a path segment.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum PathSegmentKind {
    /// A line from (x1, y1) to (x2, y2).
    Line { x1: f64, y1: f64, x2: f64, y2: f64 },
    /// A rectangle with origin (x, y), width, and height.
    Rect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    /// An arbitrary closed polygon. Each subpath is a list of (x, y)
    /// vertices in device coordinates, implicitly closed.
    Polygon {
        subpaths: Vec<Vec<(f64, f64)>>,
        fill_rule: FillRule,
    },
}

impl fmt::Display for Table {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let method = match self.detection_method {
            TableDetectionMethod::RuledLine => "ruled",
            TableDetectionMethod::HLine => "hline",
            TableDetectionMethod::TextAlignment => "alignment",
        };
        write!(
            f,
            "Table ({}x{}, {})",
            self.rows.len(),
            self.num_columns,
            method
        )
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

impl fmt::Display for TableDetectionMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TableDetectionMethod::RuledLine => write!(f, "RuledLine"),
            TableDetectionMethod::HLine => write!(f, "HLine"),
            TableDetectionMethod::TextAlignment => write!(f, "TextAlignment"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_bbox() -> BoundingBox {
        BoundingBox::new(0.0, 0.0, 100.0, 50.0)
    }

    #[test]
    fn test_table_cell_new() {
        let cell = TableCell::new("hello".to_string(), sample_bbox());
        assert_eq!(cell.text, "hello");
        assert_eq!(cell.col_span, 1);
        assert_eq!(cell.row_span, 1);
    }

    #[test]
    fn test_table_display() {
        let table = Table {
            bbox: BoundingBox::new(0.0, 0.0, 500.0, 300.0),
            rows: vec![
                TableRow {
                    cells: vec![
                        TableCell::new("A".to_string(), sample_bbox()),
                        TableCell::new("B".to_string(), sample_bbox()),
                    ],
                    is_header: true,
                },
                TableRow {
                    cells: vec![
                        TableCell::new("1".to_string(), sample_bbox()),
                        TableCell::new("2".to_string(), sample_bbox()),
                    ],
                    is_header: false,
                },
            ],
            num_columns: 2,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![0.0, 50.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        };
        assert_eq!(format!("{}", table), "Table (2x2, ruled)");
    }

    #[test]
    fn test_table_display_alignment() {
        let table = Table {
            bbox: sample_bbox(),
            rows: vec![],
            num_columns: 3,
            detection_method: TableDetectionMethod::TextAlignment,
            column_positions: vec![],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        };
        assert_eq!(format!("{}", table), "Table (0x3, alignment)");
    }

    #[test]
    fn test_table_row_display() {
        let header = TableRow {
            cells: vec![
                TableCell::new("Name".to_string(), sample_bbox()),
                TableCell::new("Age".to_string(), sample_bbox()),
            ],
            is_header: true,
        };
        assert_eq!(format!("{}", header), "[header] Name | Age");

        let row = TableRow {
            cells: vec![
                TableCell::new("Alice".to_string(), sample_bbox()),
                TableCell::new("30".to_string(), sample_bbox()),
            ],
            is_header: false,
        };
        assert_eq!(format!("{}", row), "Alice | 30");
    }

    #[test]
    fn test_table_cell_display() {
        let simple = TableCell::new("value".to_string(), sample_bbox());
        assert_eq!(format!("{}", simple), "value");

        let spanned = TableCell {
            text: "merged".to_string(),
            bbox: sample_bbox(),
            col_span: 2,
            row_span: 3,
        };
        assert_eq!(format!("{}", spanned), "merged (span 2x3)");
    }

    #[test]
    fn test_detection_method_display() {
        assert_eq!(format!("{}", TableDetectionMethod::RuledLine), "RuledLine");
        assert_eq!(
            format!("{}", TableDetectionMethod::TextAlignment),
            "TextAlignment"
        );
    }

    #[test]
    fn test_detection_method_eq() {
        assert_eq!(
            TableDetectionMethod::RuledLine,
            TableDetectionMethod::RuledLine
        );
        assert_ne!(
            TableDetectionMethod::RuledLine,
            TableDetectionMethod::TextAlignment
        );
    }

    #[test]
    fn test_path_segment_line() {
        let seg = PathSegment {
            kind: PathSegmentKind::Line {
                x1: 0.0,
                y1: 0.0,
                x2: 100.0,
                y2: 0.0,
            },
            stroked: true,
            filled: false,
            line_width: 0.5,
            stroke_color: [0, 0, 0],
            fill_color: [0, 0, 0],
            z_index: 0,
            fill_alpha: 255,
            stroke_alpha: 255,
            active_clips: Vec::new(),
        };
        assert!(seg.stroked);
        assert!(!seg.filled);
    }

    #[test]
    fn test_path_segment_rect() {
        let seg = PathSegment {
            kind: PathSegmentKind::Rect {
                x: 10.0,
                y: 20.0,
                width: 100.0,
                height: 50.0,
            },
            stroked: false,
            filled: true,
            line_width: 1.0,
            stroke_color: [0, 0, 0],
            fill_color: [0, 0, 0],
            z_index: 0,
            fill_alpha: 255,
            stroke_alpha: 255,
            active_clips: Vec::new(),
        };
        assert!(!seg.stroked);
        assert!(seg.filled);
    }

    #[test]
    fn test_table_continuation_flags() {
        let table = Table {
            bbox: sample_bbox(),
            rows: vec![],
            num_columns: 1,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![0.0],
            may_continue_from_previous: true,
            may_continue_to_next: true,
        };
        assert!(table.may_continue_from_previous);
        assert!(table.may_continue_to_next);
    }
}

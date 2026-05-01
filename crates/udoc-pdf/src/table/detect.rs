//! Ruled-line table detection from PDF path segments.
//!
//! Implements the "lattice" approach: extract horizontal and vertical line
//! segments from stroked/filled paths, snap to a grid, find intersections,
//! and build `Table` structs from the resulting grid cells.

use std::collections::HashMap;

use crate::diagnostics::{DiagnosticsSink, Warning, WarningContext, WarningKind, WarningLevel};
use crate::geometry::BoundingBox;

use super::text_edge::detect_columns_text_edge_with_support;
use super::types::{
    PathSegment, PathSegmentKind, Table, TableCell, TableDetectionMethod, TableRow,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Tolerance in points for snapping line endpoints to a grid.
const SNAP_TOLERANCE: f64 = 3.0;

/// Lines shorter than this (in points) are discarded as noise.
const MIN_LINE_LENGTH: f64 = 5.0;

/// Maximum gap (in points) between collinear line segments that still allows
/// merging. Larger than SNAP_TOLERANCE to handle per-cell rect drawing where
/// adjacent cells produce line segments with small gaps between them.
const LINE_MERGE_GAP: f64 = 6.0;

/// Minimum number of distinct Y positions (rows of intersections) to form a table.
const MIN_GRID_ROWS: usize = 2;

/// Minimum number of distinct X positions (columns of intersections) to form a table.
const MIN_GRID_COLS: usize = 2;

/// Distance from page edge (in points, 0.5 inch) within which a table is
/// considered to possibly continue across page boundaries.
const PAGE_EDGE_TOLERANCE: f64 = 36.0;

// Security limits -- prevent pathological inputs from consuming unbounded memory.
// Follow the pattern from content/interpreter.rs (MAX_SPANS_PER_STREAM, MAX_IMAGES).

/// Maximum number of line segments to process. Beyond this, bail early.
const MAX_TABLE_LINES: usize = 10_000;

/// Maximum number of rows in a single detected table.
const MAX_TABLE_ROWS: usize = 1_000;

/// Maximum number of columns in a single detected table.
const MAX_TABLE_COLS: usize = 500;

/// Maximum total cells across all tables on a page.
const MAX_TABLE_CELLS: usize = 50_000;

/// Maximum number of tables detected on a single page.
const MAX_TABLES_PER_PAGE: usize = 50;

/// Maximum number of line intersections to process. Pathological inputs
/// with dense grids can produce O(h_lines * v_lines) intersections.
const MAX_INTERSECTIONS: usize = 100_000;

// ---------------------------------------------------------------------------
// Internal line representation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum LineDir {
    Horizontal,
    Vertical,
}

/// A normalized, axis-aligned line segment.
#[derive(Debug, Clone, Copy)]
struct LineSeg {
    dir: LineDir,
    /// For horizontal: the Y coordinate. For vertical: the X coordinate.
    fixed: f64,
    /// Start of the varying coordinate (always <= end).
    start: f64,
    /// End of the varying coordinate.
    end: f64,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Detect tables formed by ruled lines (stroked/filled paths).
///
/// Takes path segments captured during content stream interpretation and the
/// page bounding box. Returns zero or more `Table` values with empty cell
/// text (text assignment is handled separately in TE-004).
pub fn detect_tables(
    paths: &[PathSegment],
    page_bbox: &BoundingBox,
    diagnostics: &dyn DiagnosticsSink,
) -> Vec<Table> {
    if paths.is_empty() {
        return Vec::new();
    }

    // Step 1-2: Extract and classify line segments.
    let mut raw_lines = extract_lines(paths);
    if raw_lines.is_empty() {
        return Vec::new();
    }

    // Security: cap line count to prevent pathological inputs.
    if raw_lines.len() > MAX_TABLE_LINES {
        diagnostics.warning(Warning {
            offset: None,
            kind: WarningKind::ResourceLimit,
            level: WarningLevel::Warning,
            context: WarningContext::default(),
            message: format!(
                "table detection: {} line segments exceeds limit of {}, truncating",
                raw_lines.len(),
                MAX_TABLE_LINES
            ),
        });
        raw_lines.truncate(MAX_TABLE_LINES);
    }

    // Step 3: Snap endpoints to grid.
    let snapped: Vec<LineSeg> = raw_lines.into_iter().map(snap_line).collect();

    // Step 4: Deduplicate / merge overlapping lines.
    let (h_lines, v_lines) = dedup_and_merge(snapped);

    if h_lines.is_empty() || v_lines.is_empty() {
        return Vec::new();
    }

    // Step 5: Find intersections.
    let intersections = find_intersections(&h_lines, &v_lines);
    if intersections.is_empty() {
        return Vec::new();
    }

    // Step 6-7: Build grids from intersection clusters, then validate.
    let grids = cluster_intersections(&intersections, &h_lines, &v_lines);

    let mut tables = Vec::new();
    let mut total_cells: usize = 0;
    for grid in grids {
        if tables.len() >= MAX_TABLES_PER_PAGE {
            diagnostics.warning(Warning {
                offset: None,
                kind: WarningKind::ResourceLimit,
                level: WarningLevel::Warning,
                context: WarningContext::default(),
                message: format!(
                    "table detection: {} tables per page limit reached, skipping remaining",
                    MAX_TABLES_PER_PAGE
                ),
            });
            break;
        }
        if let Some(table) = build_table_from_grid(&grid, &h_lines, &v_lines, page_bbox) {
            let cell_count: usize = table.rows.iter().map(|r| r.cells.len()).sum();
            total_cells = total_cells.saturating_add(cell_count);
            if total_cells > MAX_TABLE_CELLS {
                diagnostics.warning(Warning {
                    offset: None,
                    kind: WarningKind::ResourceLimit,
                    level: WarningLevel::Warning,
                    context: WarningContext::default(),
                    message: format!(
                        "table detection: total cell count {} exceeds limit of {}, skipping remaining tables",
                        total_cells, MAX_TABLE_CELLS
                    ),
                });
                break;
            }
            tables.push(table);
        }
    }

    tables
}

// ---------------------------------------------------------------------------
// Post-detection: merge adjacent lattice fragments
// ---------------------------------------------------------------------------

/// Merge adjacent RuledLine table fragments into unified tables.
///
/// Per-cell border rects (iText, Word) produce separate table fragments
/// instead of one unified grid when the union-find clustering doesn't
/// connect all cells. This post-detection pass merges fragments that are
/// spatially adjacent and have compatible column structures.
///
/// Only merges RuledLine tables. HLine and TextAlignment tables are left alone.
pub(crate) fn merge_adjacent_tables(tables: &mut Vec<Table>) {
    if tables.len() < 2 {
        return;
    }

    // Adjacency threshold: tables within this gap (in points) are candidates.
    let merge_gap = SNAP_TOLERANCE * 2.0;

    let mut merged = true;
    while merged {
        merged = false;

        // Sort by bbox top-to-bottom (descending Y in PDF coords), then left-to-right.
        tables.sort_by(|a, b| {
            b.bbox
                .y_max
                .total_cmp(&a.bbox.y_max)
                .then(a.bbox.x_min.total_cmp(&b.bbox.x_min))
        });

        // Try to merge each pair. When a merge happens, restart the scan.
        'outer: for i in 0..tables.len() {
            if tables[i].detection_method != TableDetectionMethod::RuledLine {
                continue;
            }
            for j in (i + 1)..tables.len() {
                if tables[j].detection_method != TableDetectionMethod::RuledLine {
                    continue;
                }
                if let Some(combined) = try_merge_tables(&tables[i], &tables[j], merge_gap) {
                    // Replace table[i] with the merged result, remove table[j].
                    tables[i] = combined;
                    tables.remove(j);
                    merged = true;
                    break 'outer;
                }
            }
        }
    }
}

/// Try to merge two RuledLine tables. Returns Some(merged) if they're
/// spatially adjacent and have compatible column structures.
fn try_merge_tables(a: &Table, b: &Table, gap: f64) -> Option<Table> {
    let a_bb = &a.bbox;
    let b_bb = &b.bbox;

    // Check spatial adjacency: tables must be close in at least one direction.
    // Vertical adjacency: one table directly above the other.
    let vertically_adjacent =
        (a_bb.y_min - b_bb.y_max).abs() < gap || (b_bb.y_min - a_bb.y_max).abs() < gap;
    // Horizontal adjacency: one table directly beside the other.
    let horizontally_adjacent =
        (a_bb.x_max - b_bb.x_min).abs() < gap || (b_bb.x_max - a_bb.x_min).abs() < gap;

    // Tables must overlap in the perpendicular dimension to be fragments of
    // the same table (not two separate tables side by side on a page).
    let x_overlap = a_bb.x_min < b_bb.x_max + gap && b_bb.x_min < a_bb.x_max + gap;
    let y_overlap = a_bb.y_min < b_bb.y_max + gap && b_bb.y_min < a_bb.y_max + gap;

    if !((vertically_adjacent && x_overlap) || (horizontally_adjacent && y_overlap)) {
        return None;
    }

    // Check column compatibility: column boundaries should align within SNAP_TOLERANCE.
    if !columns_compatible(&a.column_positions, &b.column_positions) {
        return None;
    }

    // Merge: union the grid positions and rebuild.
    let mut all_xs: Vec<f64> = Vec::new();
    let mut all_ys: Vec<f64> = Vec::new();

    // Collect grid positions from both tables.
    for x in &a.column_positions {
        insert_unique(&mut all_xs, *x);
    }
    for x in &b.column_positions {
        insert_unique(&mut all_xs, *x);
    }

    // Collect Y positions from row boundaries.
    collect_row_ys(a, &mut all_ys);
    collect_row_ys(b, &mut all_ys);

    all_xs.sort_by(f64::total_cmp);
    all_ys.sort_by(f64::total_cmp);

    let num_cell_cols = if all_xs.len() >= 2 {
        all_xs.len() - 1
    } else {
        return None;
    };
    let num_cell_rows = if all_ys.len() >= 2 {
        all_ys.len() - 1
    } else {
        return None;
    };

    if num_cell_rows > MAX_TABLE_ROWS || num_cell_cols > MAX_TABLE_COLS {
        return None;
    }

    // Build rows (top-to-bottom = reverse Y order).
    let mut rows = Vec::with_capacity(num_cell_rows);
    for vr in 0..num_cell_rows {
        let row_top = all_ys[num_cell_rows - vr];
        let row_bottom = all_ys[num_cell_rows - 1 - vr];
        let mut cells = Vec::with_capacity(num_cell_cols);
        for col in 0..num_cell_cols {
            let cell_bbox = BoundingBox::new(all_xs[col], row_bottom, all_xs[col + 1], row_top);
            cells.push(TableCell::new(String::new(), cell_bbox));
        }
        rows.push(TableRow {
            cells,
            is_header: false,
        });
    }

    let merged_bbox = BoundingBox::new(
        all_xs[0],
        all_ys[0],
        all_xs[all_xs.len() - 1],
        all_ys[all_ys.len() - 1],
    );

    Some(Table {
        bbox: merged_bbox,
        rows,
        num_columns: num_cell_cols,
        detection_method: TableDetectionMethod::RuledLine,
        column_positions: all_xs,
        may_continue_from_previous: a.may_continue_from_previous || b.may_continue_from_previous,
        may_continue_to_next: a.may_continue_to_next || b.may_continue_to_next,
    })
}

/// Check if two sets of column positions are compatible (each position in
/// one set has a corresponding position in the other within SNAP_TOLERANCE).
fn columns_compatible(a: &[f64], b: &[f64]) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    // At least half of the positions in the smaller set must match.
    let (smaller, larger) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let mut matches = 0;
    for &pos in smaller {
        if larger
            .iter()
            .any(|&other| (pos - other).abs() < SNAP_TOLERANCE)
        {
            matches += 1;
        }
    }
    // Require >= 50% match rate on the smaller set.
    matches * 2 >= smaller.len()
}

/// Collect Y positions from a table's row boundaries.
fn collect_row_ys(table: &Table, ys: &mut Vec<f64>) {
    for row in &table.rows {
        for cell in &row.cells {
            insert_unique(ys, cell.bbox.y_min);
            insert_unique(ys, cell.bbox.y_max);
        }
    }
}

// ---------------------------------------------------------------------------
// Horizontal-only table detection
// ---------------------------------------------------------------------------

/// Minimum number of horizontal lines to consider h-line-only detection.
const MIN_HLINES_FOR_HLINE_TABLE: usize = 2;

/// Maximum number of vertical lines allowed within the h-line extent
/// before bailing out of h-line detection. V-lines indicate per-cell
/// border rects (iText/Word) where the lattice detector should handle
/// the table instead.
const MAX_VLINES_FOR_HLINE_TABLE: usize = 1;

/// Detect tables from horizontal rules only (no vertical lines).
///
/// Common in LaTeX tables (`\toprule`, `\midrule`, `\bottomrule`) and many
/// financial reports that use horizontal rules to separate rows but rely on
/// text alignment for columns. Returns tables where rows are bounded by
/// h-lines and columns are inferred from text span X positions.
/// Maximum line width (in points) for a line to be considered a table border.
/// Lines thicker than this are decorative separators (section dividers, etc.).
const DECORATIVE_LINE_WIDTH: f64 = 3.0;

/// Fraction of page width beyond which a thick line is decorative
/// (section dividers, page-spanning rules) rather than a table border.
const DECORATIVE_PAGE_WIDTH_FRACTION: f64 = 0.80;

pub fn detect_hline_tables(
    paths: &[PathSegment],
    spans: &[crate::text::types::TextSpan],
    page_bbox: &BoundingBox,
    diagnostics: &dyn DiagnosticsSink,
) -> Vec<Table> {
    if paths.is_empty() || spans.is_empty() {
        return Vec::new();
    }

    // Filter out decorative thick lines and thick filled rects in a single pass.
    let page_w = page_bbox.x_max - page_bbox.x_min;
    let filtered_paths: Vec<PathSegment> = paths
        .iter()
        .filter(|seg| {
            match &seg.kind {
                PathSegmentKind::Line { x1, x2, .. } => {
                    // Keep lines with normal width.
                    if seg.line_width <= DECORATIVE_LINE_WIDTH {
                        return true;
                    }
                    // Thick line: filter if it spans most of the page width.
                    let line_len = (x2 - x1).abs();
                    !(page_w > 0.0 && line_len / page_w > DECORATIVE_PAGE_WIDTH_FRACTION)
                }
                PathSegmentKind::Rect { width, height, .. } => {
                    let w = width.abs();
                    let h = height.abs();
                    // Filter decorative thick h-rule rects spanning most of page width.
                    !(h > DECORATIVE_LINE_WIDTH
                        && h < SNAP_TOLERANCE * 3.0
                        && w >= MIN_LINE_LENGTH
                        && page_w > 0.0
                        && w / page_w > DECORATIVE_PAGE_WIDTH_FRACTION)
                }
                // Polygons are not relevant for table detection.
                _ => false,
            }
        })
        .cloned()
        .collect();

    let raw_lines = extract_lines(&filtered_paths);
    if raw_lines.is_empty() {
        return Vec::new();
    }

    let snapped: Vec<LineSeg> = raw_lines.into_iter().map(snap_line).collect();

    // Extract column boundaries from gaps between per-cell h-line segments
    // BEFORE merging. Per-cell rect drawing (pdfTeX, iText) creates separate
    // h-line segments per cell on the same Y. The gaps between them encode
    // column boundaries directly.
    let gap_columns = detect_columns_from_hline_gaps(&snapped);

    let (h_lines, v_lines) = dedup_and_merge(snapped);

    // Group h-lines into clusters by similar Y position (within SNAP_TOLERANCE).
    // Computed early so we can decide whether to use v-lines.
    let mut y_positions: Vec<f64> = h_lines.iter().map(|h| h.fixed).collect();
    y_positions.sort_by(f64::total_cmp);
    dedup_with_tolerance(&mut y_positions);

    // Use v-lines for column detection only when there are too few h-lines
    // for the lattice detector to form a proper grid (< 3 unique Y positions).
    // When a full grid exists (3+ h-lines), the lattice detector handles it
    // and we don't want h-line detection to create false positives.
    let use_vlines = v_lines.len() >= 2 && y_positions.len() < 3;

    // V-line gate: only count v-lines within the h-line horizontal extent.
    // Decorative v-lines outside the table region (charts, boxes) should not
    // block h-line table detection.
    if !use_vlines {
        let hx_min = h_lines
            .iter()
            .map(|h| h.start)
            .fold(f64::INFINITY, f64::min);
        let hx_max = h_lines
            .iter()
            .map(|h| h.end)
            .fold(f64::NEG_INFINITY, f64::max);
        let relevant_vlines = v_lines
            .iter()
            .filter(|v| v.fixed >= hx_min - SNAP_TOLERANCE && v.fixed <= hx_max + SNAP_TOLERANCE)
            .count();
        if relevant_vlines > MAX_VLINES_FOR_HLINE_TABLE {
            return Vec::new();
        }
    }

    let min_hlines = if use_vlines {
        1
    } else {
        MIN_HLINES_FOR_HLINE_TABLE
    };
    if h_lines.len() < min_hlines || y_positions.len() < min_hlines {
        return Vec::new();
    }

    // When only 1 h-line but v-lines exist, synthesize table vertical extent
    // from v-line endpoints. The v-line start/end Y values mark the table's
    // top and bottom.
    if y_positions.len() == 1 && use_vlines {
        let vline_y_min = v_lines
            .iter()
            .map(|v| v.start)
            .fold(f64::INFINITY, f64::min);
        let vline_y_max = v_lines
            .iter()
            .map(|v| v.end)
            .fold(f64::NEG_INFINITY, f64::max);
        // Add synthetic boundaries at the v-line extent (avoiding duplicates).
        if (vline_y_min - y_positions[0]).abs() > SNAP_TOLERANCE {
            y_positions.push(vline_y_min);
        }
        if (vline_y_max - y_positions[0]).abs() > SNAP_TOLERANCE {
            y_positions.push(vline_y_max);
        }
        y_positions.sort_by(f64::total_cmp);
    }

    // Determine the horizontal extent of the h-lines (table width).
    let table_x_min = h_lines
        .iter()
        .map(|h| h.start)
        .fold(f64::INFINITY, f64::min);
    let table_x_max = h_lines
        .iter()
        .map(|h| h.end)
        .fold(f64::NEG_INFINITY, f64::max);

    if table_x_max - table_x_min < MIN_LINE_LENGTH {
        return Vec::new();
    }

    // Collect text spans within the h-line extent. Filter to spans whose
    // center X falls within [table_x_min, table_x_max] and whose baseline
    // falls between the topmost and bottommost h-lines.
    let y_top = y_positions[y_positions.len() - 1];
    let y_bottom = y_positions[0];

    let relevant_spans: Vec<(f64, f64, f64)> = spans
        .iter()
        .filter_map(|s| {
            if s.text.trim().is_empty() {
                return None;
            }
            let left_x = s.x;
            let right_x = s.x + s.width;
            let center_x = (left_x + right_x) / 2.0;
            let baseline = s.y;
            if center_x >= table_x_min - SNAP_TOLERANCE
                && center_x <= table_x_max + SNAP_TOLERANCE
                && baseline >= y_bottom - SNAP_TOLERANCE * 3.0
                && baseline <= y_top + SNAP_TOLERANCE * 3.0
            {
                Some((left_x, right_x, baseline))
            } else {
                None
            }
        })
        .collect();

    if relevant_spans.is_empty() {
        return Vec::new();
    }

    // Infer column boundaries. Strategy depends on available signals:
    // 1. V-lines within the table region (most reliable, explicit grid).
    // 2. H-line gap columns: gaps between per-cell h-line segments encode
    //    column boundaries directly (structural signal from PDF drawing).
    // 3. Nurminen text-edge algorithm.
    // 4. Fallback: simple left-edge clustering.
    let vline_cols: Vec<f64> = {
        let mut xs: Vec<f64> = v_lines
            .iter()
            .filter(|v| {
                v.fixed >= table_x_min - SNAP_TOLERANCE
                    && v.fixed <= table_x_max + SNAP_TOLERANCE
                    && v.start <= y_top + SNAP_TOLERANCE
                    && v.end >= y_bottom - SNAP_TOLERANCE
            })
            .map(|v| v.fixed)
            .collect();
        xs.sort_by(f64::total_cmp);
        xs.dedup_by(|a, b| (*a - *b).abs() < SNAP_TOLERANCE);
        xs
    };

    // Filter gap_columns to those within the table X extent.
    let gap_cols: Vec<f64> = gap_columns
        .iter()
        .filter(|&&x| x > table_x_min + SNAP_TOLERANCE && x < table_x_max - SNAP_TOLERANCE)
        .copied()
        .collect();

    // Determine column source and boundary mode.
    // "boundary" mode: positions are column boundaries (N positions = N-1 columns).
    // "left-edge" mode: positions are column left-edges (N positions = N columns).
    enum ColSource {
        Boundaries(Vec<f64>),
        LeftEdges(Vec<f64>),
    }

    let col_source = if use_vlines && vline_cols.len() >= 2 {
        ColSource::Boundaries(vline_cols)
    } else if !gap_cols.is_empty() {
        // H-line gaps give column boundaries. Add table edges.
        let mut boundaries = Vec::with_capacity(gap_cols.len() + 2);
        boundaries.push(table_x_min);
        boundaries.extend_from_slice(&gap_cols);
        boundaries.push(table_x_max);
        ColSource::Boundaries(boundaries)
    } else {
        // Text-based column detection.
        let hline_bbox = BoundingBox::new(
            table_x_min - SNAP_TOLERANCE,
            y_bottom - SNAP_TOLERANCE * 3.0,
            table_x_max + SNAP_TOLERANCE,
            y_top + SNAP_TOLERANCE * 3.0,
        );
        let num_text_rows = {
            let mut ys: Vec<f64> = relevant_spans.iter().map(|(_, _, y)| *y).collect();
            ys.sort_by(f64::total_cmp);
            ys.dedup_by(|a, b| (*a - *b).abs() < SNAP_TOLERANCE);
            ys.len()
        };
        // Clamp support to [2, 4]: at least 2 for noise resistance, at most 4
        // to avoid over-filtering short tables.
        let adaptive_support = (num_text_rows / 2).clamp(2, 4);
        let mut cols =
            detect_columns_text_edge_with_support(spans, &hline_bbox, Some(adaptive_support));
        // Fallback chain: text-edge -> gap-based -> left-edge clustering.
        if cols.len() < 3 {
            let gap_cols = detect_columns_gap_based(&relevant_spans, num_text_rows);
            if gap_cols.len() > cols.len() {
                cols = gap_cols;
            }
        }
        if cols.len() < 2 {
            cols = cluster_left_edges_simple(&relevant_spans);
        }
        ColSource::LeftEdges(cols)
    };

    // Build column boundaries for cell bbox computation.
    let (col_boundaries, num_cols) = match col_source {
        ColSource::Boundaries(boundaries) => {
            if boundaries.len() < 2 {
                return Vec::new();
            }
            let n_cols = boundaries.len() - 1;
            (boundaries, n_cols)
        }
        ColSource::LeftEdges(column_left_xs) => {
            if column_left_xs.len() < 2 {
                return Vec::new();
            }
            let n_cols = column_left_xs.len();
            let column_right_xs: Vec<f64> = column_left_xs
                .iter()
                .map(|&lx| {
                    let col_tolerance = 5.0;
                    relevant_spans
                        .iter()
                        .filter(|(slx, _, _)| (*slx - lx).abs() < col_tolerance)
                        .map(|(_, rx, _)| *rx)
                        .fold(lx, f64::max)
                })
                .collect();

            let mut boundaries: Vec<f64> = Vec::with_capacity(n_cols + 1);
            boundaries.push(table_x_min);
            for i in 0..n_cols - 1 {
                let mid = (column_right_xs[i] + column_left_xs[i + 1]) / 2.0;
                boundaries.push(mid);
            }
            boundaries.push(table_x_max);
            (boundaries, n_cols)
        }
    };

    // Build row bands from consecutive y-position pairs.
    // y_positions sorted ascending; rows go from bottom to top in PDF coords.
    let num_rows = y_positions.len() - 1;

    if num_rows < 1 || num_cols < 2 {
        return Vec::new();
    }
    if num_rows > MAX_TABLE_ROWS || num_cols > MAX_TABLE_COLS {
        diagnostics.warning(Warning {
            offset: None,
            kind: WarningKind::ResourceLimit,
            level: WarningLevel::Warning,
            context: WarningContext::default(),
            message: format!(
                "h-line table detection: {}x{} exceeds limits, skipping",
                num_rows, num_cols
            ),
        });
        return Vec::new();
    }

    // Build rows (top-to-bottom in visual order = reverse Y order).
    let mut rows = Vec::with_capacity(num_rows);
    for vr in 0..num_rows {
        let row_top = y_positions[num_rows - vr]; // higher Y = top
        let row_bottom = y_positions[num_rows - 1 - vr]; // lower Y = bottom

        let mut cells = Vec::with_capacity(num_cols);
        for col in 0..num_cols {
            let cell_bbox = BoundingBox::new(
                col_boundaries[col],
                row_bottom,
                col_boundaries[col + 1],
                row_top,
            );
            cells.push(TableCell::new(String::new(), cell_bbox));
        }
        rows.push(TableRow {
            cells,
            is_header: false,
        });
    }

    let table_bbox = BoundingBox::new(table_x_min, y_bottom, table_x_max, y_top);

    let may_continue_from_previous =
        (table_bbox.y_max - page_bbox.y_max).abs() < PAGE_EDGE_TOLERANCE;
    let may_continue_to_next = (table_bbox.y_min - page_bbox.y_min).abs() < PAGE_EDGE_TOLERANCE;

    vec![Table {
        bbox: table_bbox,
        rows,
        num_columns: num_cols,
        detection_method: TableDetectionMethod::HLine,
        column_positions: col_boundaries,
        may_continue_from_previous,
        may_continue_to_next,
    }]
}

/// Minimum number of Y-levels a gap must appear at to be a column boundary.
const MIN_GAP_Y_SUPPORT: usize = 2;

/// Detect column boundaries from gaps between per-cell h-line segments.
///
/// When PDF generators draw tables with per-cell filled rectangles (pdfTeX,
/// iText, etc.), each cell produces a separate h-line segment. The gaps between
/// consecutive segments on the same Y level directly encode column boundaries.
///
/// Takes pre-merge snapped lines. Returns sorted column boundary X positions
/// (gap midpoints that recur across multiple Y levels).
fn detect_columns_from_hline_gaps(snapped_lines: &[LineSeg]) -> Vec<f64> {
    // Collect only horizontal lines.
    let mut h_lines: Vec<&LineSeg> = snapped_lines
        .iter()
        .filter(|l| l.dir == LineDir::Horizontal)
        .collect();

    if h_lines.len() < 2 {
        return Vec::new();
    }

    // Sort by fixed Y, then by start X.
    h_lines.sort_by(|a, b| {
        a.fixed
            .total_cmp(&b.fixed)
            .then(a.start.total_cmp(&b.start))
    });

    // Group by Y (within SNAP_TOLERANCE), find gaps within each group.
    let mut all_gap_xs: Vec<f64> = Vec::new();
    let mut y_level_count: usize = 0;

    let mut i = 0;
    while i < h_lines.len() {
        let y = h_lines[i].fixed;
        let mut j = i + 1;
        while j < h_lines.len() && (h_lines[j].fixed - y).abs() < SNAP_TOLERANCE {
            j += 1;
        }
        // h_lines[i.j] are all on the same Y level.
        let group = &h_lines[i..j];
        if group.len() >= 2 {
            y_level_count += 1;
            // Find gaps between consecutive segments.
            for pair in group.windows(2) {
                let gap_start = pair[0].end;
                let gap_end = pair[1].start;
                let gap = gap_end - gap_start;
                // Only consider gaps above a noise floor and within the merge
                // range. Gaps larger than 3x LINE_MERGE_GAP are likely between
                // separate tables, not column boundaries.
                const MIN_GAP: f64 = 0.5;
                const MAX_GAP_FACTOR: f64 = 3.0;
                if gap > MIN_GAP && gap < LINE_MERGE_GAP * MAX_GAP_FACTOR {
                    let gap_mid = (gap_start + gap_end) / 2.0;
                    all_gap_xs.push(gap_mid);
                }
            }
        }
        i = j;
    }

    if all_gap_xs.is_empty() || y_level_count < MIN_GAP_Y_SUPPORT {
        return Vec::new();
    }

    // Cluster gap X positions (within SNAP_TOLERANCE) and require recurrence.
    all_gap_xs.sort_by(f64::total_cmp);

    let mut clusters: Vec<(f64, usize)> = Vec::new(); // (sum_x, count)
    for &x in &all_gap_xs {
        if let Some(last) = clusters.last_mut() {
            let avg = last.0 / last.1 as f64;
            if (x - avg).abs() < SNAP_TOLERANCE {
                last.0 += x;
                last.1 += 1;
                continue;
            }
        }
        clusters.push((x, 1));
    }

    // Keep clusters that appear at enough Y levels.
    let min_support = MIN_GAP_Y_SUPPORT.min(y_level_count);
    let result: Vec<f64> = clusters
        .into_iter()
        .filter(|(_, count)| *count >= min_support)
        .map(|(sum, count)| sum / count as f64)
        .collect();

    result
}

/// Simple greedy left-edge clustering fallback for column detection.
/// Used when Nurminen text-edge returns too few columns (e.g., very few rows).
///
/// O(n * m * n) where n = spans, m = columns. Capped at 2000 spans to
/// prevent quadratic blowup on pathological inputs.
fn cluster_left_edges_simple(spans: &[(f64, f64, f64)]) -> Vec<f64> {
    const MAX_SPANS_FOR_CLUSTERING: usize = 2_000;
    let col_tolerance = 5.0;
    let min_gap = 6.0;

    // Cap input to prevent quadratic blowup.
    let spans = if spans.len() > MAX_SPANS_FOR_CLUSTERING {
        &spans[..MAX_SPANS_FOR_CLUSTERING]
    } else {
        spans
    };

    let mut left_xs: Vec<f64> = spans.iter().map(|(lx, _, _)| *lx).collect();
    left_xs.sort_by(f64::total_cmp);

    let mut column_left_xs: Vec<f64> = Vec::new();
    let mut column_right_xs: Vec<f64> = Vec::new();
    for &lx in &left_xs {
        let mut found = false;
        for (i, cx) in column_left_xs.iter().enumerate() {
            if (lx - cx).abs() < col_tolerance {
                found = true;
                if let Some(rx) = spans
                    .iter()
                    .filter(|(slx, _, _)| (*slx - lx).abs() < col_tolerance)
                    .map(|(_, rx, _)| *rx)
                    .reduce(f64::max)
                {
                    if rx > column_right_xs[i] {
                        column_right_xs[i] = rx;
                    }
                }
                break;
            }
        }
        if !found {
            let max_right = spans
                .iter()
                .filter(|(slx, _, _)| (*slx - lx).abs() < col_tolerance)
                .map(|(_, rx, _)| *rx)
                .fold(lx, f64::max);
            column_left_xs.push(lx);
            column_right_xs.push(max_right);
        }
    }

    // Validate at least one column pair has a gap.
    let has_gap = column_left_xs.windows(2).enumerate().any(|(i, w)| {
        let gap = w[1] - column_right_xs[i];
        gap >= min_gap
    });
    if !has_gap {
        return Vec::new();
    }

    column_left_xs
}

/// Detect column left-edge positions from persistent whitespace gaps.
///
/// For each text row within the table region, compute gap intervals where
/// no text exists. Cluster gap intervals across rows by X-position proximity.
/// Keep gaps persistent in >= 50% of rows and wider than min_gap. Return
/// gap midpoints as column boundary left-edge X positions.
///
/// This catches SEC financial tables and other layouts where text-edge
/// elimination kills too many edges (wide headers straddle column boundaries).
fn detect_columns_gap_based(
    relevant_spans: &[(f64, f64, f64)], // (left_x, right_x, baseline)
    num_text_rows: usize,
) -> Vec<f64> {
    if relevant_spans.len() < 4 || num_text_rows < 2 {
        return Vec::new();
    }

    // Group spans into rows by Y proximity.
    let mut sorted: Vec<(f64, f64, f64)> = relevant_spans.to_vec();
    sorted.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.total_cmp(&b.0)));

    // Compute median font-size proxy from span widths / char count.
    // Use average span width as a rough font size proxy.
    let avg_span_width: f64 =
        sorted.iter().map(|(l, r, _)| r - l).sum::<f64>() / sorted.len().max(1) as f64;
    let min_gap = (avg_span_width * 0.15).max(6.0);

    // Group into rows by Y proximity. We lack per-span font sizes here,
    // so reuse min_gap as a row tolerance proxy (correlates with text size
    // via avg_span_width). This is conservative: min_gap >= 6pts, so rows
    // with very tight leading could be merged, but that's acceptable for
    // a fallback column detector.
    let mut rows: Vec<Vec<(f64, f64)>> = Vec::new(); // Vec of row, each is Vec of (left, right)
    let mut current_row: Vec<(f64, f64)> = vec![(sorted[0].0, sorted[0].1)];
    let mut row_y = sorted[0].2;
    let row_tolerance = min_gap;

    for &(lx, rx, y) in &sorted[1..] {
        if (row_y - y).abs() <= row_tolerance {
            current_row.push((lx, rx));
        } else {
            if current_row.len() >= 2 {
                rows.push(current_row);
            }
            current_row = vec![(lx, rx)];
            row_y = y;
        }
    }
    if current_row.len() >= 2 {
        rows.push(current_row);
    }

    if rows.len() < 2 {
        return Vec::new();
    }

    // For each row, find gaps between consecutive spans.
    let mut all_gaps: Vec<f64> = Vec::new(); // gap midpoints across all rows
    for row in &rows {
        let mut spans_sorted = row.clone();
        spans_sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
        for pair in spans_sorted.windows(2) {
            let gap_start = pair[0].1;
            let gap_end = pair[1].0;
            let gap_width = gap_end - gap_start;
            if gap_width >= min_gap {
                let gap_mid = (gap_start + gap_end) / 2.0;
                all_gaps.push(gap_mid);
            }
        }
    }

    if all_gaps.is_empty() {
        return Vec::new();
    }

    // Cluster gaps by X proximity.
    all_gaps.sort_by(f64::total_cmp);
    let cluster_tolerance = min_gap;
    let mut clusters: Vec<(f64, usize)> = Vec::new(); // (sum_x, count)
    for &x in &all_gaps {
        if let Some(last) = clusters.last_mut() {
            let avg = last.0 / last.1 as f64;
            if (x - avg).abs() < cluster_tolerance {
                last.0 += x;
                last.1 += 1;
                continue;
            }
        }
        clusters.push((x, 1));
    }

    // Keep gaps present in >= 50% of rows, minimum 2 for noise resistance.
    let min_support = (rows.len() / 2).max(2);
    let gap_positions: Vec<f64> = clusters
        .into_iter()
        .filter(|(_, count)| *count >= min_support)
        .map(|(sum, count)| sum / count as f64)
        .collect();

    if gap_positions.is_empty() {
        return Vec::new();
    }

    // Convert gap midpoints to column left-edge positions.
    // The leftmost column starts at the leftmost span. Each gap midpoint
    // marks where a new column begins (the right side of the gap).
    let leftmost = sorted
        .iter()
        .map(|(lx, _, _)| *lx)
        .fold(f64::INFINITY, f64::min);
    let mut col_left_edges = vec![leftmost];
    for &gap_x in &gap_positions {
        // Find the leftmost span to the right of this gap.
        let right_of_gap = sorted
            .iter()
            .filter(|(lx, _, _)| *lx > gap_x - min_gap)
            .map(|(lx, _, _)| *lx)
            .fold(f64::INFINITY, f64::min);
        if right_of_gap < f64::INFINITY
            && !col_left_edges
                .iter()
                .any(|&existing| (existing - right_of_gap).abs() < min_gap)
        {
            col_left_edges.push(right_of_gap);
        }
    }

    col_left_edges.sort_by(f64::total_cmp);
    col_left_edges
}

// ---------------------------------------------------------------------------
// Step 1-2: Extract and classify
// ---------------------------------------------------------------------------

fn extract_lines(paths: &[PathSegment]) -> Vec<LineSeg> {
    // Two-pass approach: first collect lines from stroked paths and thin
    // rects. Large filled-only rects (cell backgrounds) are only used as
    // a fallback when there aren't enough border lines from the first pass.
    let mut lines = Vec::new();
    let mut filled_only_rects: Vec<(f64, f64, f64, f64)> = Vec::new();

    for seg in paths {
        // Only consider stroked or filled segments.
        if !seg.stroked && !seg.filled {
            continue;
        }

        match &seg.kind {
            PathSegmentKind::Line { x1, y1, x2, y2 } => {
                if let Some(line) = classify_line(*x1, *y1, *x2, *y2) {
                    lines.push(line);
                }
            }
            PathSegmentKind::Rect {
                x,
                y,
                width,
                height,
            } => {
                // Normalize negative dimensions.
                let (rx, rw) = if *width < 0.0 {
                    (x + width, -width)
                } else {
                    (*x, *width)
                };
                let (ry, rh) = if *height < 0.0 {
                    (y + height, -height)
                } else {
                    (*y, *height)
                };

                // Very thin rects are treated as single lines.
                if rh < SNAP_TOLERANCE && rw >= MIN_LINE_LENGTH {
                    // Thin horizontal rect -> horizontal line at the vertical midpoint.
                    let mid_y = ry + rh / 2.0;
                    lines.push(LineSeg {
                        dir: LineDir::Horizontal,
                        fixed: mid_y,
                        start: rx,
                        end: rx + rw,
                    });
                } else if rw < SNAP_TOLERANCE && rh >= MIN_LINE_LENGTH {
                    // Thin vertical rect -> vertical line at the horizontal midpoint.
                    let mid_x = rx + rw / 2.0;
                    lines.push(LineSeg {
                        dir: LineDir::Vertical,
                        fixed: mid_x,
                        start: ry,
                        end: ry + rh,
                    });
                } else if seg.filled && !seg.stroked {
                    // Large filled-only rect: defer. These are cell
                    // backgrounds when drawn alongside border lines, but
                    // may be the actual table structure if no borders exist.
                    filled_only_rects.push((rx, ry, rw, rh));
                } else {
                    // Normal rect: decompose into 4 edge lines.
                    let x_min = rx;
                    let x_max = rx + rw;
                    let y_min = ry;
                    let y_max = ry + rh;

                    // Bottom edge (horizontal)
                    if rw >= MIN_LINE_LENGTH {
                        lines.push(LineSeg {
                            dir: LineDir::Horizontal,
                            fixed: y_min,
                            start: x_min,
                            end: x_max,
                        });
                        // Top edge
                        lines.push(LineSeg {
                            dir: LineDir::Horizontal,
                            fixed: y_max,
                            start: x_min,
                            end: x_max,
                        });
                    }
                    // Left edge (vertical)
                    if rh >= MIN_LINE_LENGTH {
                        lines.push(LineSeg {
                            dir: LineDir::Vertical,
                            fixed: x_min,
                            start: y_min,
                            end: y_max,
                        });
                        // Right edge
                        lines.push(LineSeg {
                            dir: LineDir::Vertical,
                            fixed: x_max,
                            start: y_min,
                            end: y_max,
                        });
                    }
                }
            }
            // Polygons are not relevant for table detection.
            _ => {}
        }
    }

    // Decide whether filled-only rects are redundant backgrounds or
    // structural elements. When a PDF has both drawn borders AND cell-fill
    // rects at similar positions (iText pattern), the fills create phantom
    // grid lines. But when fills contribute new positions (per-cell tiled
    // rects, form fields), they're structural and should be kept.
    //
    // Heuristic: check if each fill rect's edges are near existing border
    // lines. If most fills are "covered" by existing borders, skip them all.
    if !filled_only_rects.is_empty() {
        let edge_tol = SNAP_TOLERANCE * 2.0;
        let covered_count = filled_only_rects
            .iter()
            .filter(|(rx, ry, rw, rh)| {
                // Check if all 4 edges are near existing border lines.
                let edges_h = [*ry, ry + rh];
                let edges_v = [*rx, rx + rw];
                let h_covered = edges_h.iter().all(|ey| {
                    lines
                        .iter()
                        .any(|l| l.dir == LineDir::Horizontal && (l.fixed - ey).abs() < edge_tol)
                });
                let v_covered = edges_v.iter().all(|ex| {
                    lines
                        .iter()
                        .any(|l| l.dir == LineDir::Vertical && (l.fixed - ex).abs() < edge_tol)
                });
                h_covered && v_covered
            })
            .count();
        let skip_fills = covered_count * 2 >= filled_only_rects.len();
        if !skip_fills {
            for (rx, ry, rw, rh) in filled_only_rects {
                let x_min = rx;
                let x_max = rx + rw;
                let y_min = ry;
                let y_max = ry + rh;

                if rw >= MIN_LINE_LENGTH {
                    lines.push(LineSeg {
                        dir: LineDir::Horizontal,
                        fixed: y_min,
                        start: x_min,
                        end: x_max,
                    });
                    lines.push(LineSeg {
                        dir: LineDir::Horizontal,
                        fixed: y_max,
                        start: x_min,
                        end: x_max,
                    });
                }
                if rh >= MIN_LINE_LENGTH {
                    lines.push(LineSeg {
                        dir: LineDir::Vertical,
                        fixed: x_min,
                        start: y_min,
                        end: y_max,
                    });
                    lines.push(LineSeg {
                        dir: LineDir::Vertical,
                        fixed: x_max,
                        start: y_min,
                        end: y_max,
                    });
                }
            }
        }
    }

    lines
}

/// Classify a line as horizontal or vertical, discarding diagonals and
/// segments shorter than `MIN_LINE_LENGTH`.
fn classify_line(x1: f64, y1: f64, x2: f64, y2: f64) -> Option<LineSeg> {
    let dx = (x2 - x1).abs();
    let dy = (y2 - y1).abs();

    if dy < SNAP_TOLERANCE && dx >= MIN_LINE_LENGTH {
        // Horizontal
        let (sx, ex) = if x1 <= x2 { (x1, x2) } else { (x2, x1) };
        Some(LineSeg {
            dir: LineDir::Horizontal,
            fixed: (y1 + y2) / 2.0,
            start: sx,
            end: ex,
        })
    } else if dx < SNAP_TOLERANCE && dy >= MIN_LINE_LENGTH {
        // Vertical
        let (sy, ey) = if y1 <= y2 { (y1, y2) } else { (y2, y1) };
        Some(LineSeg {
            dir: LineDir::Vertical,
            fixed: (x1 + x2) / 2.0,
            start: sy,
            end: ey,
        })
    } else {
        // Diagonal or too short.
        None
    }
}

// ---------------------------------------------------------------------------
// Step 3: Snap to grid
// ---------------------------------------------------------------------------

fn snap(val: f64) -> f64 {
    (val / SNAP_TOLERANCE).round() * SNAP_TOLERANCE
}

fn snap_line(mut line: LineSeg) -> LineSeg {
    line.fixed = snap(line.fixed);
    line.start = snap(line.start);
    line.end = snap(line.end);
    // Ensure start <= end after snapping.
    if line.start > line.end {
        std::mem::swap(&mut line.start, &mut line.end);
    }
    line
}

// ---------------------------------------------------------------------------
// Step 4: Deduplicate and merge overlapping segments
// ---------------------------------------------------------------------------

fn dedup_and_merge(lines: Vec<LineSeg>) -> (Vec<LineSeg>, Vec<LineSeg>) {
    let mut h_groups: Vec<LineSeg> = Vec::new();
    let mut v_groups: Vec<LineSeg> = Vec::new();

    for line in lines {
        match line.dir {
            LineDir::Horizontal => h_groups.push(line),
            LineDir::Vertical => v_groups.push(line),
        }
    }

    let h_merged = merge_lines(h_groups);
    let v_merged = merge_lines(v_groups);
    (h_merged, v_merged)
}

/// Group lines by their fixed coordinate, then merge overlapping segments
/// within each group.
fn merge_lines(mut lines: Vec<LineSeg>) -> Vec<LineSeg> {
    if lines.is_empty() {
        return Vec::new();
    }

    // Sort by fixed coordinate, then by start.
    lines.sort_by(|a, b| {
        a.fixed
            .total_cmp(&b.fixed)
            .then(a.start.total_cmp(&b.start))
    });

    let mut result = Vec::new();
    let mut current = lines[0];

    for line in lines.iter().skip(1) {
        // Same fixed coordinate (within tolerance)?
        if (line.fixed - current.fixed).abs() < SNAP_TOLERANCE {
            // Overlapping or adjacent (within LINE_MERGE_GAP)? Merge.
            // The larger gap tolerance handles per-cell rect drawing where
            // adjacent cells create line segments with small gaps.
            if line.start <= current.end + LINE_MERGE_GAP {
                current.end = current.end.max(line.end);
            } else {
                result.push(current);
                current = *line;
            }
        } else {
            result.push(current);
            current = *line;
        }
    }
    result.push(current);
    result
}

// ---------------------------------------------------------------------------
// Step 5: Find intersections
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Intersection {
    x: f64,
    y: f64,
}

fn find_intersections(h_lines: &[LineSeg], v_lines: &[LineSeg]) -> Vec<Intersection> {
    if h_lines.is_empty() || v_lines.is_empty() {
        return Vec::new();
    }

    // Sort h_lines by Y (fixed) for binary search. Each v_line covers a Y
    // range [start-tol, end+tol]; binary search finds candidate h_lines whose
    // Y falls in that range, reducing O(h*v) to O((h+v)*log(h) + hits).
    let mut h_sorted: Vec<&LineSeg> = h_lines.iter().collect();
    h_sorted.sort_by(|a, b| a.fixed.total_cmp(&b.fixed));

    let mut result = Vec::new();

    for v in v_lines {
        let ix = v.fixed;
        let y_lo = v.start - SNAP_TOLERANCE;
        let y_hi = v.end + SNAP_TOLERANCE;

        // Binary search for first h_line with Y >= y_lo.
        let start = h_sorted.partition_point(|h| h.fixed < y_lo);

        for &h in &h_sorted[start..] {
            if h.fixed > y_hi {
                break;
            }
            // h.fixed is in [y_lo, y_hi], so v contains h's Y.
            // Check if h contains v's X.
            if ix >= h.start - SNAP_TOLERANCE && ix <= h.end + SNAP_TOLERANCE {
                result.push(Intersection { x: ix, y: h.fixed });
                if result.len() >= MAX_INTERSECTIONS {
                    return result;
                }
            }
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Step 6: Cluster intersections into separate grids
// ---------------------------------------------------------------------------

/// A grid defined by sorted X and Y positions.
struct Grid {
    xs: Vec<f64>,
    ys: Vec<f64>,
}

/// Cluster intersections into independent grids by line connectivity.
///
/// Two intersections belong to the same table only if an actual line segment
/// connects them: same Y with a horizontal line spanning both X positions,
/// or same X with a vertical line spanning both Y positions. This correctly
/// separates tables that share X or Y coordinates but are spatially disjoint.
fn cluster_intersections(
    intersections: &[Intersection],
    h_lines: &[LineSeg],
    v_lines: &[LineSeg],
) -> Vec<Grid> {
    let n = intersections.len();
    if n == 0 {
        return Vec::new();
    }

    let mut parent: Vec<usize> = (0..n).collect();

    // For each pair of intersections on the same Y, check if a horizontal
    // line actually connects them. Use snapped coordinates for sorting so
    // that intersections at nearly-the-same Y are grouped together and
    // windows(2) correctly sees them as consecutive.
    let mut by_y: Vec<usize> = (0..n).collect();
    by_y.sort_by(|&a, &b| {
        let ay = snap(intersections[a].y);
        let by_ = snap(intersections[b].y);
        ay.total_cmp(&by_)
            .then(snap(intersections[a].x).total_cmp(&snap(intersections[b].x)))
    });
    for window in by_y.windows(2) {
        let (ia, ib) = (window[0], window[1]);
        let (a, b) = (&intersections[ia], &intersections[ib]);
        if (snap(a.y) - snap(b.y)).abs() < f64::EPSILON {
            // Check that a horizontal line spans from a.x to b.x at this y.
            let x_lo = a.x.min(b.x);
            let x_hi = a.x.max(b.x);
            let connected = h_lines.iter().any(|h| {
                (h.fixed - a.y).abs() < SNAP_TOLERANCE
                    && h.start <= x_lo + SNAP_TOLERANCE
                    && h.end >= x_hi - SNAP_TOLERANCE
            });
            if connected {
                union(&mut parent, ia, ib);
            }
        }
    }

    // For each pair of intersections on the same X, check if a vertical
    // line actually connects them. Same snapped-sort strategy as horizontal.
    let mut by_x: Vec<usize> = (0..n).collect();
    by_x.sort_by(|&a, &b| {
        let ax = snap(intersections[a].x);
        let bx = snap(intersections[b].x);
        ax.total_cmp(&bx)
            .then(snap(intersections[a].y).total_cmp(&snap(intersections[b].y)))
    });
    for window in by_x.windows(2) {
        let (ia, ib) = (window[0], window[1]);
        let (a, b) = (&intersections[ia], &intersections[ib]);
        if (snap(a.x) - snap(b.x)).abs() < f64::EPSILON {
            // Check that a vertical line spans from a.y to b.y at this x.
            let y_lo = a.y.min(b.y);
            let y_hi = a.y.max(b.y);
            let connected = v_lines.iter().any(|v| {
                (v.fixed - a.x).abs() < SNAP_TOLERANCE
                    && v.start <= y_lo + SNAP_TOLERANCE
                    && v.end >= y_hi - SNAP_TOLERANCE
            });
            if connected {
                union(&mut parent, ia, ib);
            }
        }
    }

    // Collect clusters.
    let mut cluster_map: std::collections::HashMap<usize, Vec<usize>> =
        std::collections::HashMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        cluster_map.entry(root).or_default().push(i);
    }

    // Build Grid for each cluster.
    // Push all coordinates, then sort+dedup with tolerance (O(n log n)
    // instead of O(n^2) from insert_unique per element).
    let mut grids = Vec::new();
    for (_root, members) in cluster_map {
        let mut gx: Vec<f64> = Vec::with_capacity(members.len());
        let mut gy: Vec<f64> = Vec::with_capacity(members.len());
        for &idx in &members {
            gx.push(intersections[idx].x);
            gy.push(intersections[idx].y);
        }
        gx.sort_by(f64::total_cmp);
        gy.sort_by(f64::total_cmp);
        dedup_with_tolerance(&mut gx);
        dedup_with_tolerance(&mut gy);

        if gx.len() >= MIN_GRID_COLS && gy.len() >= MIN_GRID_ROWS {
            grids.push(Grid { xs: gx, ys: gy });
        }
    }

    grids
}

// ---------------------------------------------------------------------------
// Step 7-8: Build Table from a validated grid
// ---------------------------------------------------------------------------

fn build_table_from_grid(
    grid: &Grid,
    h_lines: &[LineSeg],
    v_lines: &[LineSeg],
    page_bbox: &BoundingBox,
) -> Option<Table> {
    let num_cell_rows = grid.ys.len() - 1;
    let num_cell_cols = grid.xs.len() - 1;

    if num_cell_rows == 0 || num_cell_cols == 0 {
        return None;
    }

    // Security: cap grid dimensions.
    if num_cell_rows > MAX_TABLE_ROWS || num_cell_cols > MAX_TABLE_COLS {
        return None;
    }

    // Build spatial indices for O(1) line lookups instead of O(H+V) per cell.
    let h_index = LineIndex::build(h_lines);
    let v_index = LineIndex::build(v_lines);

    // Validate each cell: at least one bounding line segment must exist.
    // Be lenient: require that the cell has at least one bounding segment
    // on any side. If zero edges exist, the cell is bogus.
    let mut valid_cells = vec![false; num_cell_rows * num_cell_cols];
    let mut any_valid = false;

    for row in 0..num_cell_rows {
        for col in 0..num_cell_cols {
            let x_min = grid.xs[col];
            let x_max = grid.xs[col + 1];
            let y_min = grid.ys[row];
            let y_max = grid.ys[row + 1];

            let has_edge = h_index.has_line(y_min, x_min, x_max)
                || h_index.has_line(y_max, x_min, x_max)
                || v_index.has_line(x_min, y_min, y_max)
                || v_index.has_line(x_max, y_min, y_max);

            if has_edge {
                valid_cells[row * num_cell_cols + col] = true;
                any_valid = true;
            }
        }
    }

    if !any_valid {
        return None;
    }

    // Build rows and cells with merged cell detection.
    // PDF Y increases upward, so we process in visual order (top-to-bottom =
    // reverse grid Y). Greedily merge adjacent cells when internal separating
    // lines are missing.
    //
    // consumed tracks visual positions already claimed by a merged cell.
    let mut consumed = vec![false; num_cell_rows * num_cell_cols];
    let mut rows = Vec::with_capacity(num_cell_rows);

    for vr in 0..num_cell_rows {
        let gr = num_cell_rows - 1 - vr; // grid row (Y-ascending index)
        let mut cells = Vec::with_capacity(num_cell_cols);

        for vc in 0..num_cell_cols {
            if consumed[vr * num_cell_cols + vc] || !valid_cells[gr * num_cell_cols + vc] {
                continue;
            }

            // Expand right: merge while internal vertical separator is missing.
            // Use spans_vertical (midpoint check) so a line merely touching
            // the cell edge doesn't prevent merging.
            let mut cs = 1;
            while vc + cs < num_cell_cols {
                if consumed[vr * num_cell_cols + (vc + cs)]
                    || !valid_cells[gr * num_cell_cols + (vc + cs)]
                {
                    break;
                }
                if v_index.spans_midpoint(grid.xs[vc + cs], grid.ys[gr], grid.ys[gr + 1]) {
                    break;
                }
                cs += 1;
            }

            // Expand down (decreasing grid row): merge while internal horizontal
            // separator is missing across ALL columns in the span.
            let mut rs = 1;
            while vr + rs < num_cell_rows {
                let next_gr = num_cell_rows - 1 - (vr + rs);
                // The horizontal separator between these two visual rows.
                let h_y = grid.ys[next_gr + 1];

                let mut can_merge = true;
                for c in vc..vc + cs {
                    if consumed[(vr + rs) * num_cell_cols + c]
                        || !valid_cells[next_gr * num_cell_cols + c]
                    {
                        can_merge = false;
                        break;
                    }
                    if h_index.spans_midpoint(h_y, grid.xs[c], grid.xs[c + 1]) {
                        can_merge = false;
                        break;
                    }
                }
                if !can_merge {
                    break;
                }
                rs += 1;
            }

            // Mark all cells in the merged area as consumed.
            for dr in 0..rs {
                for dc in 0..cs {
                    consumed[(vr + dr) * num_cell_cols + (vc + dc)] = true;
                }
            }

            // Merged cell bbox spans from top-left to bottom-right in page coords.
            let bottom_gr = num_cell_rows - 1 - (vr + rs - 1);
            let cell_bbox = BoundingBox::new(
                grid.xs[vc],
                grid.ys[bottom_gr],
                grid.xs[vc + cs],
                grid.ys[gr + 1],
            );

            let mut cell = TableCell::new(String::new(), cell_bbox);
            cell.col_span = cs;
            cell.row_span = rs;
            cells.push(cell);
        }

        if !cells.is_empty() {
            rows.push(TableRow {
                cells,
                is_header: false,
            });
        }
    }

    if rows.is_empty() {
        return None;
    }

    let table_bbox = BoundingBox::new(
        grid.xs[0],
        grid.ys[0],
        grid.xs[grid.xs.len() - 1],
        grid.ys[grid.ys.len() - 1],
    );

    let column_positions: Vec<f64> = grid.xs.clone();

    let may_continue_from_previous =
        (table_bbox.y_max - page_bbox.y_max).abs() < PAGE_EDGE_TOLERANCE;
    let may_continue_to_next = (table_bbox.y_min - page_bbox.y_min).abs() < PAGE_EDGE_TOLERANCE;

    let num_columns = num_cell_cols;

    Some(Table {
        bbox: table_bbox,
        rows,
        num_columns,
        detection_method: TableDetectionMethod::RuledLine,
        column_positions,
        may_continue_from_previous,
        may_continue_to_next,
    })
}

// ---------------------------------------------------------------------------
// Helpers: line index, sorted unique insert, union-find
// ---------------------------------------------------------------------------

/// Spatial index for line segments keyed by their snapped `fixed` coordinate.
/// Turns O(H) or O(V) linear scans into O(bucket_size) lookups.
struct LineIndex<'a> {
    by_fixed: HashMap<i64, Vec<&'a LineSeg>>,
}

impl<'a> LineIndex<'a> {
    fn build(lines: &'a [LineSeg]) -> Self {
        let mut by_fixed: HashMap<i64, Vec<&'a LineSeg>> = HashMap::new();
        for line in lines {
            let key = snap_to_key(line.fixed);
            by_fixed.entry(key).or_default().push(line);
        }
        Self { by_fixed }
    }

    /// Check if any line at `fixed` overlaps the range [range_lo, range_hi].
    fn has_line(&self, fixed: f64, range_lo: f64, range_hi: f64) -> bool {
        let key = snap_to_key(fixed);
        // Check the exact key and neighbors to handle snapping boundary.
        // Use saturating arithmetic to avoid overflow on extreme coordinates.
        for k in [key.saturating_sub(1), key, key.saturating_add(1)] {
            if let Some(lines) = self.by_fixed.get(&k) {
                if lines.iter().any(|l| {
                    (l.fixed - fixed).abs() < SNAP_TOLERANCE
                        && l.start <= range_hi + SNAP_TOLERANCE
                        && l.end >= range_lo - SNAP_TOLERANCE
                }) {
                    return true;
                }
            }
        }
        false
    }

    /// Check if any line at `fixed` actually spans through the midpoint
    /// of [range_lo, range_hi]. Used for merged cell detection.
    fn spans_midpoint(&self, fixed: f64, range_lo: f64, range_hi: f64) -> bool {
        let mid = (range_lo + range_hi) / 2.0;
        let key = snap_to_key(fixed);
        for k in [key.saturating_sub(1), key, key.saturating_add(1)] {
            if let Some(lines) = self.by_fixed.get(&k) {
                if lines.iter().any(|l| {
                    (l.fixed - fixed).abs() < SNAP_TOLERANCE
                        && l.start <= mid + SNAP_TOLERANCE
                        && l.end >= mid - SNAP_TOLERANCE
                }) {
                    return true;
                }
            }
        }
        false
    }
}

/// Convert a coordinate to a bucket key for the line index.
fn snap_to_key(v: f64) -> i64 {
    (v / SNAP_TOLERANCE).round() as i64
}

fn insert_unique(vec: &mut Vec<f64>, val: f64) {
    if !vec.iter().any(|v| (*v - val).abs() <= SNAP_TOLERANCE) {
        vec.push(val);
    }
}

/// Remove near-duplicate values from a sorted Vec using SNAP_TOLERANCE.
/// Keeps the first value in each cluster. Vec must be sorted.
fn dedup_with_tolerance(vec: &mut Vec<f64>) {
    if vec.len() <= 1 {
        return;
    }
    let mut write = 1;
    for read in 1..vec.len() {
        if (vec[read] - vec[write - 1]).abs() > SNAP_TOLERANCE {
            vec[write] = vec[read];
            write += 1;
        }
    }
    vec.truncate(write);
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]]; // path compression
        i = parent[i];
    }
    i
}

fn union(parent: &mut [usize], a: usize, b: usize) {
    let ra = find(parent, a);
    let rb = find(parent, b);
    if ra != rb {
        parent[rb] = ra;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::NullDiagnostics;

    fn page_bbox() -> BoundingBox {
        BoundingBox::new(0.0, 0.0, 612.0, 792.0) // US Letter
    }

    fn make_line(x1: f64, y1: f64, x2: f64, y2: f64) -> PathSegment {
        PathSegment {
            kind: PathSegmentKind::Line { x1, y1, x2, y2 },
            stroked: true,
            filled: false,
            line_width: 0.5,
            stroke_color: [0, 0, 0],
            fill_color: [0, 0, 0],
            z_index: 0,
            fill_alpha: 255,
            stroke_alpha: 255,
            active_clips: Vec::new(),
        }
    }

    fn make_rect(x: f64, y: f64, width: f64, height: f64) -> PathSegment {
        PathSegment {
            kind: PathSegmentKind::Rect {
                x,
                y,
                width,
                height,
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
        }
    }

    /// Build a 3x3 grid (2 rows x 2 cols of cells) from 4 rects sharing edges.
    /// Each rect is one cell of the table.
    #[test]
    fn test_3x3_grid_from_rects() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        // 2x2 cells, each 100x50
        //   (100,200)---(200,200)---(300,200)
        //       |    cell    |    cell    |
        //   (100,150)---(200,150)---(300,150)
        //       |    cell    |    cell    |
        //   (100,100)---(200,100)---(300,100)
        let paths = vec![
            make_rect(100.0, 150.0, 100.0, 50.0), // top-left cell
            make_rect(200.0, 150.0, 100.0, 50.0), // top-right cell
            make_rect(100.0, 100.0, 100.0, 50.0), // bottom-left cell
            make_rect(200.0, 100.0, 100.0, 50.0), // bottom-right cell
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1, "expected 1 table, got {}", tables.len());

        let table = &tables[0];
        assert_eq!(table.num_columns, 2);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.detection_method, TableDetectionMethod::RuledLine);

        // First row (top, y=200 to y=150) should have 2 cells.
        assert_eq!(table.rows[0].cells.len(), 2);
        // Second row (bottom, y=150 to y=100) should have 2 cells.
        assert_eq!(table.rows[1].cells.len(), 2);

        // Column positions should be [100, 200, 300].
        assert_eq!(table.column_positions.len(), 3);
    }

    /// Build a 3x3 grid from individual line segments.
    #[test]
    fn test_3x3_grid_from_lines() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        // 3 horizontal lines at y=100, y=150, y=200
        // 3 vertical lines at x=100, x=200, x=300
        let paths = vec![
            // Horizontal
            make_line(100.0, 100.0, 300.0, 100.0),
            make_line(100.0, 150.0, 300.0, 150.0),
            make_line(100.0, 200.0, 300.0, 200.0),
            // Vertical
            make_line(100.0, 100.0, 100.0, 200.0),
            make_line(200.0, 100.0, 200.0, 200.0),
            make_line(300.0, 100.0, 300.0, 200.0),
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1);

        let table = &tables[0];
        assert_eq!(table.num_columns, 2);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0].cells.len(), 2);
        assert_eq!(table.rows[1].cells.len(), 2);
    }

    /// Empty input produces no tables.
    #[test]
    fn test_empty_input() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();
        let tables = detect_tables(&[], &bbox, &diag);
        assert!(tables.is_empty());
    }

    /// No qualifying lines (diagonal only) produces no tables.
    #[test]
    fn test_no_qualifying_lines() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        // Diagonal lines: neither horizontal nor vertical.
        let paths = vec![
            make_line(100.0, 100.0, 200.0, 200.0),
            make_line(200.0, 100.0, 300.0, 200.0),
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert!(tables.is_empty());
    }

    /// A single box (4 lines forming a rectangle) is a valid 1-cell table.
    #[test]
    fn test_single_cell_table() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        let paths = vec![make_rect(100.0, 100.0, 200.0, 100.0)];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1);

        let table = &tables[0];
        assert_eq!(table.num_columns, 1);
        assert_eq!(table.rows.len(), 1);
        assert_eq!(table.rows[0].cells.len(), 1);
    }

    /// Overlapping/duplicate lines from border-sharing cells are deduplicated.
    #[test]
    fn test_overlapping_lines_deduplicated() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        // Two cells sharing a border at x=200. The shared vertical line
        // appears twice (once as right edge of left cell, once as left
        // edge of right cell).
        let paths = vec![
            // Left cell border lines
            make_line(100.0, 100.0, 300.0, 100.0), // bottom
            make_line(100.0, 200.0, 300.0, 200.0), // top
            make_line(100.0, 100.0, 100.0, 200.0), // left
            make_line(200.0, 100.0, 200.0, 200.0), // shared border
            make_line(200.0, 100.0, 200.0, 200.0), // duplicate of shared border
            make_line(300.0, 100.0, 300.0, 200.0), // right
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1);

        let table = &tables[0];
        assert_eq!(table.num_columns, 2);
        assert_eq!(table.rows.len(), 1);
    }

    /// A very thin rect (height < SNAP_TOLERANCE) is treated as a horizontal line.
    #[test]
    fn test_thin_rect_as_line() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        // Build a table using thin rects for horizontal lines and normal
        // lines for vertical lines.
        let paths = vec![
            // Thin horizontal rects (height = 1.0 < SNAP_TOLERANCE)
            PathSegment {
                kind: PathSegmentKind::Rect {
                    x: 100.0,
                    y: 99.5,
                    width: 200.0,
                    height: 1.0,
                },
                stroked: false,
                filled: true,
                line_width: 0.0,
                stroke_color: [0, 0, 0],
                fill_color: [0, 0, 0],
                z_index: 0,
                fill_alpha: 255,
                stroke_alpha: 255,
                active_clips: Vec::new(),
            },
            PathSegment {
                kind: PathSegmentKind::Rect {
                    x: 100.0,
                    y: 149.5,
                    width: 200.0,
                    height: 1.0,
                },
                stroked: false,
                filled: true,
                line_width: 0.0,
                stroke_color: [0, 0, 0],
                fill_color: [0, 0, 0],
                z_index: 0,
                fill_alpha: 255,
                stroke_alpha: 255,
                active_clips: Vec::new(),
            },
            PathSegment {
                kind: PathSegmentKind::Rect {
                    x: 100.0,
                    y: 199.5,
                    width: 200.0,
                    height: 1.0,
                },
                stroked: false,
                filled: true,
                line_width: 0.0,
                stroke_color: [0, 0, 0],
                fill_color: [0, 0, 0],
                z_index: 0,
                fill_alpha: 255,
                stroke_alpha: 255,
                active_clips: Vec::new(),
            },
            // Vertical lines
            make_line(100.0, 100.0, 100.0, 200.0),
            make_line(200.0, 100.0, 200.0, 200.0),
            make_line(300.0, 100.0, 300.0, 200.0),
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1);

        let table = &tables[0];
        assert_eq!(table.num_columns, 2);
        assert_eq!(table.rows.len(), 2);
    }

    /// Negative-dimension rects are normalized.
    #[test]
    fn test_negative_dimension_rect() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        // Rect with negative width and height. Origin is the top-right corner.
        // Equivalent to rect(100, 100, 200, 100).
        let paths = vec![make_rect(300.0, 200.0, -200.0, -100.0)];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1);

        let table = &tables[0];
        assert_eq!(table.num_columns, 1);
        assert_eq!(table.rows.len(), 1);
    }

    /// Unstroked, unfilled paths are ignored.
    #[test]
    fn test_unstroked_unfilled_ignored() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        let paths = vec![PathSegment {
            kind: PathSegmentKind::Rect {
                x: 100.0,
                y: 100.0,
                width: 200.0,
                height: 100.0,
            },
            stroked: false,
            filled: false,
            line_width: 0.5,
            stroke_color: [0, 0, 0],
            fill_color: [0, 0, 0],
            z_index: 0,
            fill_alpha: 255,
            stroke_alpha: 255,
            active_clips: Vec::new(),
        }];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert!(tables.is_empty());
    }

    /// Table near the top edge sets may_continue_from_previous.
    #[test]
    fn test_continuation_flags() {
        let diag = NullDiagnostics;
        let bbox = BoundingBox::new(0.0, 0.0, 612.0, 792.0);

        // Table at the very top of the page.
        let paths = vec![make_rect(100.0, 760.0, 200.0, 30.0)];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1);
        assert!(
            tables[0].may_continue_from_previous,
            "table near top edge should set may_continue_from_previous"
        );

        // Table at the very bottom of the page.
        let paths_bottom = vec![make_rect(100.0, 2.0, 200.0, 30.0)];
        let tables_bottom = detect_tables(&paths_bottom, &bbox, &diag);
        assert_eq!(tables_bottom.len(), 1);
        assert!(
            tables_bottom[0].may_continue_to_next,
            "table near bottom edge should set may_continue_to_next"
        );
    }

    /// Two separate tables on the same page.
    #[test]
    fn test_two_separate_tables() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        // Table 1: top of page
        let mut paths = vec![
            make_line(50.0, 700.0, 250.0, 700.0),
            make_line(50.0, 650.0, 250.0, 650.0),
            make_line(50.0, 600.0, 250.0, 600.0),
            make_line(50.0, 600.0, 50.0, 700.0),
            make_line(150.0, 600.0, 150.0, 700.0),
            make_line(250.0, 600.0, 250.0, 700.0),
        ];

        // Table 2: bottom of page (well separated)
        paths.extend(vec![
            make_line(50.0, 200.0, 250.0, 200.0),
            make_line(50.0, 150.0, 250.0, 150.0),
            make_line(50.0, 100.0, 250.0, 100.0),
            make_line(50.0, 100.0, 50.0, 200.0),
            make_line(150.0, 100.0, 150.0, 200.0),
            make_line(250.0, 100.0, 250.0, 200.0),
        ]);

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 2, "expected 2 separate tables");

        // Both should be 2x2 cell tables.
        for table in &tables {
            assert_eq!(table.num_columns, 2);
            assert_eq!(table.rows.len(), 2);
        }
    }

    /// Lines too short are discarded.
    #[test]
    fn test_short_lines_discarded() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        // Lines shorter than MIN_LINE_LENGTH.
        let paths = vec![
            make_line(100.0, 100.0, 103.0, 100.0), // 3pt horizontal
            make_line(100.0, 100.0, 100.0, 103.0), // 3pt vertical
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert!(tables.is_empty());
    }

    /// Filled rects (not stroked) are still picked up.
    #[test]
    fn test_filled_rect_detected() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        let paths = vec![PathSegment {
            kind: PathSegmentKind::Rect {
                x: 100.0,
                y: 100.0,
                width: 200.0,
                height: 100.0,
            },
            stroked: false,
            filled: true,
            line_width: 0.0,
            stroke_color: [0, 0, 0],
            fill_color: [0, 0, 0],
            z_index: 0,
            fill_alpha: 255,
            stroke_alpha: 255,
            active_clips: Vec::new(),
        }];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1);
    }

    /// Verify cell bounding boxes are correct and rows are top-to-bottom.
    #[test]
    fn test_cell_bbox_and_row_order() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        let paths = vec![
            make_line(100.0, 100.0, 300.0, 100.0),
            make_line(100.0, 200.0, 300.0, 200.0),
            make_line(100.0, 300.0, 300.0, 300.0),
            make_line(100.0, 100.0, 100.0, 300.0),
            make_line(300.0, 100.0, 300.0, 300.0),
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1);

        let table = &tables[0];
        assert_eq!(table.rows.len(), 2);

        // First row is the top row (y=200..300).
        let top_cell = &table.rows[0].cells[0];
        assert!((top_cell.bbox.y_max - 300.0).abs() < SNAP_TOLERANCE + 1.0);
        assert!((top_cell.bbox.y_min - 200.0).abs() < SNAP_TOLERANCE + 1.0);

        // Second row is the bottom row (y=100..200).
        let bottom_cell = &table.rows[1].cells[0];
        assert!((bottom_cell.bbox.y_max - 200.0).abs() < SNAP_TOLERANCE + 1.0);
        assert!((bottom_cell.bbox.y_min - 100.0).abs() < SNAP_TOLERANCE + 1.0);
    }

    #[test]
    fn test_snap_function() {
        // Snap rounds to the nearest multiple of SNAP_TOLERANCE (3.0).
        // 99.0 / 3.0 = 33.0, rounds to 33, * 3 = 99.0
        assert!((snap(99.0) - 99.0).abs() < f64::EPSILON);
        // 100.0 / 3.0 = 33.33, rounds to 33, * 3 = 99.0
        assert!((snap(100.0) - 99.0).abs() < f64::EPSILON);
        // 100.0 and 99.0 both snap to 99.0
        assert!((snap(100.0) - snap(99.0)).abs() < f64::EPSILON);
        // Values far apart stay apart.
        assert!((snap(100.0) - snap(110.0)).abs() > SNAP_TOLERANCE);
    }

    #[test]
    fn test_classify_line_diagonal() {
        // A 45-degree line is neither horizontal nor vertical.
        assert!(classify_line(0.0, 0.0, 100.0, 100.0).is_none());
    }

    #[test]
    fn test_classify_line_horizontal() {
        let line = classify_line(10.0, 50.0, 200.0, 50.0);
        assert!(line.is_some());
        let line = line.unwrap();
        assert_eq!(line.dir, LineDir::Horizontal);
        assert!((line.fixed - 50.0).abs() < f64::EPSILON);
        assert!((line.start - 10.0).abs() < f64::EPSILON);
        assert!((line.end - 200.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_classify_line_vertical() {
        let line = classify_line(50.0, 10.0, 50.0, 200.0);
        assert!(line.is_some());
        let line = line.unwrap();
        assert_eq!(line.dir, LineDir::Vertical);
        assert!((line.fixed - 50.0).abs() < f64::EPSILON);
    }

    /// Merge overlapping horizontal lines on the same Y.
    #[test]
    fn test_merge_overlapping_lines() {
        let lines = vec![
            LineSeg {
                dir: LineDir::Horizontal,
                fixed: 100.0,
                start: 0.0,
                end: 150.0,
            },
            LineSeg {
                dir: LineDir::Horizontal,
                fixed: 100.0,
                start: 100.0,
                end: 300.0,
            },
        ];
        let merged = merge_lines(lines);
        assert_eq!(merged.len(), 1);
        assert!((merged[0].start - 0.0).abs() < f64::EPSILON);
        assert!((merged[0].end - 300.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_line_count_security_limit() {
        // Generate more than MAX_TABLE_LINES line segments.
        // Each rect decomposes into up to 4 lines, so we need > MAX_TABLE_LINES / 4 rects
        // that produce valid lines. Use simple rects spread across the page.
        let mut paths = Vec::new();
        for i in 0..(MAX_TABLE_LINES + 100) {
            let y = (i as f64) * 0.1;
            paths.push(make_line(0.0, y, 100.0, y));
        }
        let diag = NullDiagnostics;
        // Should not panic or OOM; just truncates and returns.
        let tables = detect_tables(&paths, &page_bbox(), &diag);
        // With only horizontal lines (no vertical), no tables form.
        assert!(tables.is_empty());
    }

    #[test]
    fn test_tables_per_page_limit() {
        // MAX_TABLES_PER_PAGE is 50. Create many small independent grids.
        let mut paths = Vec::new();
        for i in 0..60 {
            let x = (i as f64) * 100.0;
            let y = 0.0;
            let w = 30.0;
            let h = 30.0;
            paths.push(make_rect(x, y, w, h));
        }
        let diag = NullDiagnostics;
        let tables = detect_tables(&paths, &page_bbox(), &diag);
        assert!(tables.len() <= MAX_TABLES_PER_PAGE);
    }

    // -----------------------------------------------------------------------
    // Additional tests (TE-009)
    // -----------------------------------------------------------------------

    /// 5x5 grid (6 horizontal lines, 6 vertical lines) produces a table
    /// with 5 columns and 5 rows of cells (25 cells total).
    #[test]
    fn test_5x5_large_grid() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        // 6 horizontal lines at y = 100, 150, 200, 250, 300, 350
        // 6 vertical lines at x = 50, 100, 150, 200, 250, 300
        let mut paths = Vec::new();
        for i in 0..6 {
            let y = 100.0 + (i as f64) * 50.0;
            paths.push(make_line(50.0, y, 300.0, y));
        }
        for i in 0..6 {
            let x = 50.0 + (i as f64) * 50.0;
            paths.push(make_line(x, 100.0, x, 350.0));
        }

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1, "expected 1 table from 5x5 grid");

        let table = &tables[0];
        assert_eq!(table.num_columns, 5, "expected 5 columns");
        assert_eq!(table.rows.len(), 5, "expected 5 rows");
        // Every row should have 5 cells.
        for (i, row) in table.rows.iter().enumerate() {
            assert_eq!(row.cells.len(), 5, "row {} should have 5 cells", i);
        }
    }

    /// L-shaped / partial grid: 3 horizontal lines but only 2 vertical
    /// lines on part of the range. Should detect whatever valid sub-grid
    /// exists (at least a 1-column table from the 2 vertical lines).
    #[test]
    fn test_partial_grid_l_shaped() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        // 3 horizontal lines spanning x=50..300
        let mut paths = vec![
            make_line(50.0, 100.0, 300.0, 100.0),
            make_line(50.0, 150.0, 300.0, 150.0),
            make_line(50.0, 200.0, 300.0, 200.0),
        ];
        // Only 2 vertical lines (no right boundary at x=300)
        paths.push(make_line(50.0, 100.0, 50.0, 200.0));
        paths.push(make_line(150.0, 100.0, 150.0, 200.0));
        // Missing vertical at x=300 means we get a single-column table.

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1, "should detect at least one table");
        let table = &tables[0];
        assert_eq!(table.num_columns, 1, "only 2 verticals = 1 column");
        assert_eq!(table.rows.len(), 2, "3 horizontals = 2 rows");
    }

    /// Filled rectangles (stroked=false, filled=true) forming a 2x2 grid
    /// should be detected as a table.
    #[test]
    fn test_filled_rects_form_table() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        let make_filled_rect = |x: f64, y: f64, w: f64, h: f64| PathSegment {
            kind: PathSegmentKind::Rect {
                x,
                y,
                width: w,
                height: h,
            },
            stroked: false,
            filled: true,
            line_width: 0.0,
            stroke_color: [0, 0, 0],
            fill_color: [0, 0, 0],
            z_index: 0,
            fill_alpha: 255,
            stroke_alpha: 255,
            active_clips: Vec::new(),
        };

        let paths = vec![
            make_filled_rect(100.0, 150.0, 100.0, 50.0),
            make_filled_rect(200.0, 150.0, 100.0, 50.0),
            make_filled_rect(100.0, 100.0, 100.0, 50.0),
            make_filled_rect(200.0, 100.0, 100.0, 50.0),
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].num_columns, 2);
        assert_eq!(tables[0].rows.len(), 2);
    }

    /// Mixed stroked and filled paths contributing to a single table.
    /// Top row cells are stroked rects, bottom row cells are filled rects.
    #[test]
    fn test_mixed_stroked_and_filled_paths() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        let stroked_rect = |x: f64, y: f64, w: f64, h: f64| PathSegment {
            kind: PathSegmentKind::Rect {
                x,
                y,
                width: w,
                height: h,
            },
            stroked: true,
            filled: false,
            line_width: 1.0,
            stroke_color: [0, 0, 0],
            fill_color: [0, 0, 0],
            z_index: 0,
            fill_alpha: 255,
            stroke_alpha: 255,
            active_clips: Vec::new(),
        };
        let filled_rect = |x: f64, y: f64, w: f64, h: f64| PathSegment {
            kind: PathSegmentKind::Rect {
                x,
                y,
                width: w,
                height: h,
            },
            stroked: false,
            filled: true,
            line_width: 0.0,
            stroke_color: [0, 0, 0],
            fill_color: [0, 0, 0],
            z_index: 0,
            fill_alpha: 255,
            stroke_alpha: 255,
            active_clips: Vec::new(),
        };

        let paths = vec![
            stroked_rect(100.0, 150.0, 100.0, 50.0), // top-left
            stroked_rect(200.0, 150.0, 100.0, 50.0), // top-right
            filled_rect(100.0, 100.0, 100.0, 50.0),  // bottom-left
            filled_rect(200.0, 100.0, 100.0, 50.0),  // bottom-right
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1, "mixed stroked/filled should form 1 table");
        assert_eq!(tables[0].num_columns, 2);
        assert_eq!(tables[0].rows.len(), 2);
    }

    /// Filled-only rects are skipped when border lines already cover their
    /// edges (iText cell-background pattern). This prevents phantom grid lines.
    #[test]
    fn test_cell_background_rects_skipped_when_borders_exist() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        // Thin-rect borders forming a 2x2 grid (3 H lines, 3 V lines).
        let thin_h = |y: f64| PathSegment {
            kind: PathSegmentKind::Rect {
                x: 100.0,
                y,
                width: 200.0,
                height: 0.5,
            },
            stroked: false,
            filled: true,
            line_width: 0.0,
            stroke_color: [0, 0, 0],
            fill_color: [0, 0, 0],
            z_index: 0,
            fill_alpha: 255,
            stroke_alpha: 255,
            active_clips: Vec::new(),
        };
        let thin_v = |x: f64| PathSegment {
            kind: PathSegmentKind::Rect {
                x,
                y: 100.0,
                width: 0.5,
                height: 100.0,
            },
            stroked: false,
            filled: true,
            line_width: 0.0,
            stroke_color: [0, 0, 0],
            fill_color: [0, 0, 0],
            z_index: 0,
            fill_alpha: 255,
            stroke_alpha: 255,
            active_clips: Vec::new(),
        };

        // Cell-fill rects at similar positions to borders.
        let fill = |x: f64, y: f64, w: f64, h: f64| PathSegment {
            kind: PathSegmentKind::Rect {
                x,
                y,
                width: w,
                height: h,
            },
            stroked: false,
            filled: true,
            line_width: 0.0,
            stroke_color: [0, 0, 0],
            fill_color: [0, 0, 0],
            z_index: 0,
            fill_alpha: 255,
            stroke_alpha: 255,
            active_clips: Vec::new(),
        };

        let paths = vec![
            // Borders
            thin_h(100.0),
            thin_h(150.0),
            thin_h(200.0),
            thin_v(100.0),
            thin_v(200.0),
            thin_v(300.0),
            // Cell-fill backgrounds (slightly offset, as iText does).
            fill(101.0, 101.0, 98.0, 48.0),
            fill(201.0, 101.0, 98.0, 48.0),
            fill(101.0, 151.0, 98.0, 48.0),
            fill(201.0, 151.0, 98.0, 48.0),
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1);
        // Should be 2x2 from the border lines, not more.
        assert_eq!(
            tables[0].num_columns, 2,
            "fills should not add extra columns"
        );
        assert_eq!(tables[0].rows.len(), 2, "fills should not add extra rows");
    }

    /// A single rect (1x1 cell table) is detected.
    #[test]
    fn test_very_small_1x1_table() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        // A single small rect that is big enough (both dims > MIN_LINE_LENGTH)
        let paths = vec![make_rect(200.0, 300.0, 50.0, 30.0)];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].num_columns, 1);
        assert_eq!(tables[0].rows.len(), 1);
        assert_eq!(tables[0].rows[0].cells.len(), 1);
    }

    /// Verify column_positions contains the correct sorted X values
    /// for a 3-column table.
    #[test]
    fn test_column_positions_correctness() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        // 3 columns: x=50..150, 150..250, 250..350
        let paths = vec![
            // 4 vertical lines
            make_line(50.0, 100.0, 50.0, 200.0),
            make_line(150.0, 100.0, 150.0, 200.0),
            make_line(250.0, 100.0, 250.0, 200.0),
            make_line(350.0, 100.0, 350.0, 200.0),
            // 2 horizontal lines
            make_line(50.0, 100.0, 350.0, 100.0),
            make_line(50.0, 200.0, 350.0, 200.0),
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].num_columns, 3);

        // column_positions should have 4 X values (boundaries for 3 columns).
        let cols = &tables[0].column_positions;
        assert_eq!(cols.len(), 4, "3 columns need 4 boundary X values");

        // Values should be approximately 50, 150, 250, 350 (after snapping).
        let expected = [51.0, 150.0, 249.0, 351.0]; // snap(50)=51, snap(150)=150, etc.
        for (actual, exp) in cols.iter().zip(expected.iter()) {
            assert!(
                (actual - exp).abs() < SNAP_TOLERANCE + 1.0,
                "column position {} not near expected {}",
                actual,
                exp
            );
        }

        // They must be sorted ascending.
        for w in cols.windows(2) {
            assert!(w[0] <= w[1], "column positions must be sorted");
        }
    }

    /// Thin borders (0.5pt line_width) and thick borders (2pt) both work.
    /// The detection algorithm does not filter by line_width.
    #[test]
    fn test_different_line_widths() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        let make_line_with_width = |x1: f64, y1: f64, x2: f64, y2: f64, lw: f64| PathSegment {
            kind: PathSegmentKind::Line { x1, y1, x2, y2 },
            stroked: true,
            filled: false,
            line_width: lw,
            stroke_color: [0, 0, 0],
            fill_color: [0, 0, 0],
            z_index: 0,
            fill_alpha: 255,
            stroke_alpha: 255,
            active_clips: Vec::new(),
        };

        let paths = vec![
            // Thin (0.5pt) horizontal lines
            make_line_with_width(100.0, 100.0, 300.0, 100.0, 0.5),
            make_line_with_width(100.0, 200.0, 300.0, 200.0, 0.5),
            // Thick (2pt) horizontal line in the middle
            make_line_with_width(100.0, 150.0, 300.0, 150.0, 2.0),
            // Vertical lines with mixed widths
            make_line_with_width(100.0, 100.0, 100.0, 200.0, 0.5),
            make_line_with_width(200.0, 100.0, 200.0, 200.0, 2.0),
            make_line_with_width(300.0, 100.0, 300.0, 200.0, 1.0),
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(
            tables.len(),
            1,
            "mixed line widths should still form a table"
        );
        assert_eq!(tables[0].num_columns, 2);
        assert_eq!(tables[0].rows.len(), 2);
    }

    /// Horizontal-only lines (no verticals) produce no tables.
    #[test]
    fn test_horizontal_only_no_table() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        let paths = vec![
            make_line(50.0, 100.0, 300.0, 100.0),
            make_line(50.0, 150.0, 300.0, 150.0),
            make_line(50.0, 200.0, 300.0, 200.0),
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert!(
            tables.is_empty(),
            "horizontal-only lines should not form a table"
        );
    }

    /// Vertical-only lines (no horizontals) produce no tables.
    #[test]
    fn test_vertical_only_no_table() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        let paths = vec![
            make_line(100.0, 50.0, 100.0, 300.0),
            make_line(200.0, 50.0, 200.0, 300.0),
            make_line(300.0, 50.0, 300.0, 300.0),
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert!(
            tables.is_empty(),
            "vertical-only lines should not form a table"
        );
    }

    /// A 4x3 grid (4 rows, 3 columns) verifies non-square grids work.
    #[test]
    fn test_4x3_non_square_grid() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        // 5 horizontal lines at y = 100, 150, 200, 250, 300
        let mut paths = Vec::new();
        for i in 0..5 {
            let y = 100.0 + (i as f64) * 50.0;
            paths.push(make_line(50.0, y, 200.0, y));
        }
        // 4 vertical lines at x = 50, 100, 150, 200
        for i in 0..4 {
            let x = 50.0 + (i as f64) * 50.0;
            paths.push(make_line(x, 100.0, x, 300.0));
        }

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].num_columns, 3, "4 verticals = 3 columns");
        assert_eq!(tables[0].rows.len(), 4, "5 horizontals = 4 rows");
    }

    /// Reversed-direction lines (x2 < x1 or y2 < y1) are still detected.
    #[test]
    fn test_reversed_direction_lines() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        // Draw a 2x2 grid but with reversed line directions
        let paths = vec![
            // Horizontal lines drawn right-to-left
            make_line(300.0, 100.0, 100.0, 100.0),
            make_line(300.0, 200.0, 100.0, 200.0),
            make_line(300.0, 300.0, 100.0, 300.0),
            // Vertical lines drawn top-to-bottom (reversed Y)
            make_line(100.0, 300.0, 100.0, 100.0),
            make_line(200.0, 300.0, 200.0, 100.0),
            make_line(300.0, 300.0, 300.0, 100.0),
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1, "reversed lines should still form a table");
        assert_eq!(tables[0].num_columns, 2);
        assert_eq!(tables[0].rows.len(), 2);
    }

    /// Table bbox should encompass all cells correctly.
    #[test]
    fn test_table_bbox_spans_entire_grid() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        let paths = vec![
            make_line(100.0, 100.0, 400.0, 100.0),
            make_line(100.0, 300.0, 400.0, 300.0),
            make_line(100.0, 100.0, 100.0, 300.0),
            make_line(400.0, 100.0, 400.0, 300.0),
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1);

        let table_bbox = &tables[0].bbox;
        // After snapping, values should be near 100, 300, 400 etc.
        assert!(
            (table_bbox.x_min - snap(100.0)).abs() < SNAP_TOLERANCE + 1.0,
            "table x_min should be near 100"
        );
        assert!(
            (table_bbox.x_max - snap(400.0)).abs() < SNAP_TOLERANCE + 1.0,
            "table x_max should be near 400"
        );
        assert!(
            (table_bbox.y_min - snap(100.0)).abs() < SNAP_TOLERANCE + 1.0,
            "table y_min should be near 100"
        );
        assert!(
            (table_bbox.y_max - snap(300.0)).abs() < SNAP_TOLERANCE + 1.0,
            "table y_max should be near 300"
        );
    }

    /// Lines that nearly align (within SNAP_TOLERANCE) are snapped together,
    /// forming a valid grid even if coordinates are slightly off.
    #[test]
    fn test_near_aligned_lines_snap_to_grid() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        // Horizontal lines at y=100.5, y=101.0, y=200.0 should snap
        // the first two together, giving a single horizontal row boundary.
        let paths = vec![
            make_line(50.0, 100.5, 250.0, 100.5),
            make_line(50.0, 200.0, 250.0, 200.0),
            make_line(50.0, 300.0, 250.0, 300.0),
            make_line(50.0, 100.5, 50.0, 300.0),
            make_line(250.0, 100.5, 250.0, 300.0),
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1);
        // Should get 2 rows (3 distinct Y values after snapping).
        assert_eq!(tables[0].rows.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Merged cell tests (TE-005)
    // -----------------------------------------------------------------------

    /// Two columns merged in the top row (col_span=2), two normal cells below.
    ///
    /// +----------+----------+
    /// |     A (colspan=2)   |   <- no vertical at x=200 between y=200..300
    /// +----------+----------+
    /// |    B     |    C     |   <- vertical at x=200 between y=100..200
    /// +----------+----------+
    #[test]
    fn test_merged_cells_colspan() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        let paths = vec![
            // Horizontal lines at y=100, 200, 300
            make_line(100.0, 100.0, 300.0, 100.0),
            make_line(100.0, 200.0, 300.0, 200.0),
            make_line(100.0, 300.0, 300.0, 300.0),
            // Vertical lines: left and right edges full height
            make_line(100.0, 100.0, 100.0, 300.0),
            make_line(300.0, 100.0, 300.0, 300.0),
            // Internal vertical at x=200 only in bottom row (y=100..200)
            make_line(200.0, 100.0, 200.0, 200.0),
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1);
        let table = &tables[0];

        // Visual row 0 (top, y=200..300): merged cell with col_span=2
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0].cells.len(), 1);
        assert_eq!(table.rows[0].cells[0].col_span, 2);
        assert_eq!(table.rows[0].cells[0].row_span, 1);

        // Visual row 1 (bottom, y=100..200): two normal cells
        assert_eq!(table.rows[1].cells.len(), 2);
        assert_eq!(table.rows[1].cells[0].col_span, 1);
        assert_eq!(table.rows[1].cells[1].col_span, 1);
    }

    /// Two rows merged in the left column (row_span=2), two normal cells on right.
    ///
    /// +----------+----------+
    /// |          |    B     |   <- h-line at y=200 only spans col 1
    /// |   A      +----------+
    /// | (rs=2)   |    C     |
    /// +----------+----------+
    #[test]
    fn test_merged_cells_rowspan() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        let paths = vec![
            // Horizontal: full-width top and bottom
            make_line(100.0, 100.0, 300.0, 100.0),
            make_line(100.0, 300.0, 300.0, 300.0),
            // Horizontal at y=200: only from x=200 to x=300 (right column)
            make_line(200.0, 200.0, 300.0, 200.0),
            // Vertical: full-height left, right, and center
            make_line(100.0, 100.0, 100.0, 300.0),
            make_line(200.0, 100.0, 200.0, 300.0),
            make_line(300.0, 100.0, 300.0, 300.0),
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1);
        let table = &tables[0];

        // Visual row 0 (top): cell A (row_span=2) + cell B
        assert_eq!(table.rows[0].cells.len(), 2);
        assert_eq!(table.rows[0].cells[0].col_span, 1);
        assert_eq!(table.rows[0].cells[0].row_span, 2);
        assert_eq!(table.rows[0].cells[1].col_span, 1);
        assert_eq!(table.rows[0].cells[1].row_span, 1);

        // Visual row 1 (bottom): only cell C (left cell consumed by row_span)
        assert_eq!(table.rows[1].cells.len(), 1);
        assert_eq!(table.rows[1].cells[0].col_span, 1);
        assert_eq!(table.rows[1].cells[0].row_span, 1);
    }

    /// 2x2 merged cell (col_span=2, row_span=2) in top-left, surrounded by
    /// normal cells.
    ///
    /// +-----+-----+-----+
    /// |           |  D  |
    /// |   A       +-----+
    /// | (2x2)     |  E  |
    /// +-----+-----+-----+
    /// |  F  |  G  |  H  |
    /// +-----+-----+-----+
    #[test]
    fn test_merged_cells_block() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        let paths = vec![
            // H-lines: full width at y=100, y=200, y=300, y=400
            make_line(100.0, 100.0, 400.0, 100.0),
            make_line(100.0, 200.0, 400.0, 200.0),
            // H-line at y=300: only right column (x=300..400)
            make_line(300.0, 300.0, 400.0, 300.0),
            make_line(100.0, 400.0, 400.0, 400.0),
            // V-lines: full height at x=100, x=400
            make_line(100.0, 100.0, 100.0, 400.0),
            make_line(400.0, 100.0, 400.0, 400.0),
            // V-line at x=200: only bottom row (y=100..200)
            make_line(200.0, 100.0, 200.0, 200.0),
            // V-line at x=300: full height
            make_line(300.0, 100.0, 300.0, 400.0),
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1);
        let table = &tables[0];

        assert_eq!(table.rows.len(), 3);
        assert_eq!(table.num_columns, 3);

        // Row 0 (top, y=300..400): A (2x2) + D
        assert_eq!(table.rows[0].cells.len(), 2);
        assert_eq!(table.rows[0].cells[0].col_span, 2);
        assert_eq!(table.rows[0].cells[0].row_span, 2);
        assert_eq!(table.rows[0].cells[1].col_span, 1);
        assert_eq!(table.rows[0].cells[1].row_span, 1);

        // Row 1 (middle, y=200..300): only E (A consumed left two columns)
        assert_eq!(table.rows[1].cells.len(), 1);
        assert_eq!(table.rows[1].cells[0].col_span, 1);
        assert_eq!(table.rows[1].cells[0].row_span, 1);

        // Row 2 (bottom, y=100..200): F, G, H
        assert_eq!(table.rows[2].cells.len(), 3);
        for cell in &table.rows[2].cells {
            assert_eq!(cell.col_span, 1);
            assert_eq!(cell.row_span, 1);
        }
    }

    /// No merging when all internal separators are present (regression test).
    #[test]
    fn test_no_merge_when_all_separators_present() {
        let diag = NullDiagnostics;
        let bbox = page_bbox();

        // Standard 2x2 grid with all internal lines.
        let paths = vec![
            make_line(100.0, 100.0, 300.0, 100.0),
            make_line(100.0, 200.0, 300.0, 200.0),
            make_line(100.0, 300.0, 300.0, 300.0),
            make_line(100.0, 100.0, 100.0, 300.0),
            make_line(200.0, 100.0, 200.0, 300.0),
            make_line(300.0, 100.0, 300.0, 300.0),
        ];

        let tables = detect_tables(&paths, &bbox, &diag);
        assert_eq!(tables.len(), 1);
        let table = &tables[0];

        assert_eq!(table.rows.len(), 2);
        for row in &table.rows {
            assert_eq!(row.cells.len(), 2);
            for cell in &row.cells {
                assert_eq!(cell.col_span, 1);
                assert_eq!(cell.row_span, 1);
            }
        }
    }

    // -----------------------------------------------------------------------
    // H-line gap column detection tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_hline_gap_columns_basic() {
        // 3 columns: per-cell h-line segments at 2 Y levels, with gaps at
        // x=200 and x=300.
        let lines = vec![
            // Y=100: three segments [100..198, 202..298, 302..400]
            LineSeg {
                dir: LineDir::Horizontal,
                fixed: 100.0,
                start: 100.0,
                end: 198.0,
            },
            LineSeg {
                dir: LineDir::Horizontal,
                fixed: 100.0,
                start: 202.0,
                end: 298.0,
            },
            LineSeg {
                dir: LineDir::Horizontal,
                fixed: 100.0,
                start: 302.0,
                end: 400.0,
            },
            // Y=150: same pattern
            LineSeg {
                dir: LineDir::Horizontal,
                fixed: 150.0,
                start: 100.0,
                end: 198.0,
            },
            LineSeg {
                dir: LineDir::Horizontal,
                fixed: 150.0,
                start: 202.0,
                end: 298.0,
            },
            LineSeg {
                dir: LineDir::Horizontal,
                fixed: 150.0,
                start: 302.0,
                end: 400.0,
            },
        ];
        let cols = detect_columns_from_hline_gaps(&lines);
        assert_eq!(cols.len(), 2, "expected 2 gap columns, got {:?}", cols);
        assert!((cols[0] - 200.0).abs() < 3.0);
        assert!((cols[1] - 300.0).abs() < 3.0);
    }

    #[test]
    fn test_hline_gap_columns_no_gaps() {
        // Full-width h-lines (no gaps).
        let lines = vec![
            LineSeg {
                dir: LineDir::Horizontal,
                fixed: 100.0,
                start: 100.0,
                end: 400.0,
            },
            LineSeg {
                dir: LineDir::Horizontal,
                fixed: 150.0,
                start: 100.0,
                end: 400.0,
            },
        ];
        let cols = detect_columns_from_hline_gaps(&lines);
        assert!(cols.is_empty(), "no gaps should produce no columns");
    }

    #[test]
    fn test_hline_gap_columns_single_y() {
        // Only one Y level: below MIN_GAP_Y_SUPPORT.
        let lines = vec![
            LineSeg {
                dir: LineDir::Horizontal,
                fixed: 100.0,
                start: 100.0,
                end: 198.0,
            },
            LineSeg {
                dir: LineDir::Horizontal,
                fixed: 100.0,
                start: 202.0,
                end: 300.0,
            },
        ];
        let cols = detect_columns_from_hline_gaps(&lines);
        assert!(cols.is_empty(), "single Y level should not produce columns");
    }

    #[test]
    fn test_double_border_merged_by_insert_unique() {
        // Two values exactly SNAP_TOLERANCE apart should be merged.
        let mut vec = Vec::new();
        insert_unique(&mut vec, 240.0);
        insert_unique(&mut vec, 243.0); // exactly 3.0 apart
        assert_eq!(vec.len(), 1, "values {} apart should merge", SNAP_TOLERANCE);
    }

    #[test]
    fn test_merge_adjacent_tables_vertical() {
        // Two fragments stacked vertically with compatible columns.
        let top = Table {
            bbox: BoundingBox::new(100.0, 150.0, 300.0, 200.0),
            rows: vec![TableRow {
                cells: vec![
                    TableCell::new(String::new(), BoundingBox::new(100.0, 150.0, 200.0, 200.0)),
                    TableCell::new(String::new(), BoundingBox::new(200.0, 150.0, 300.0, 200.0)),
                ],
                is_header: false,
            }],
            num_columns: 2,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![100.0, 200.0, 300.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        };
        let bottom = Table {
            bbox: BoundingBox::new(100.0, 100.0, 300.0, 150.0),
            rows: vec![TableRow {
                cells: vec![
                    TableCell::new(String::new(), BoundingBox::new(100.0, 100.0, 200.0, 150.0)),
                    TableCell::new(String::new(), BoundingBox::new(200.0, 100.0, 300.0, 150.0)),
                ],
                is_header: false,
            }],
            num_columns: 2,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![100.0, 200.0, 300.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        };
        let mut tables = vec![top, bottom];
        merge_adjacent_tables(&mut tables);
        assert_eq!(tables.len(), 1, "fragments should merge into 1 table");
        assert_eq!(tables[0].num_columns, 2);
        assert_eq!(tables[0].rows.len(), 2);
    }

    #[test]
    fn test_merge_skips_distant_tables() {
        // Two tables far apart vertically should NOT merge.
        let a = Table {
            bbox: BoundingBox::new(100.0, 500.0, 300.0, 550.0),
            rows: vec![TableRow {
                cells: vec![
                    TableCell::new(String::new(), BoundingBox::new(100.0, 500.0, 200.0, 550.0)),
                    TableCell::new(String::new(), BoundingBox::new(200.0, 500.0, 300.0, 550.0)),
                ],
                is_header: false,
            }],
            num_columns: 2,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![100.0, 200.0, 300.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        };
        let b = Table {
            bbox: BoundingBox::new(100.0, 100.0, 300.0, 150.0),
            rows: vec![TableRow {
                cells: vec![
                    TableCell::new(String::new(), BoundingBox::new(100.0, 100.0, 200.0, 150.0)),
                    TableCell::new(String::new(), BoundingBox::new(200.0, 100.0, 300.0, 150.0)),
                ],
                is_header: false,
            }],
            num_columns: 2,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![100.0, 200.0, 300.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        };
        let mut tables = vec![a, b];
        merge_adjacent_tables(&mut tables);
        assert_eq!(tables.len(), 2, "distant tables should not merge");
    }

    #[test]
    fn test_merge_skips_non_ruled_line() {
        // HLine tables should not be merged.
        let a = Table {
            bbox: BoundingBox::new(100.0, 150.0, 300.0, 200.0),
            rows: vec![TableRow {
                cells: vec![TableCell::new(
                    String::new(),
                    BoundingBox::new(100.0, 150.0, 300.0, 200.0),
                )],
                is_header: false,
            }],
            num_columns: 1,
            detection_method: TableDetectionMethod::HLine,
            column_positions: vec![100.0, 300.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        };
        let b = Table {
            bbox: BoundingBox::new(100.0, 100.0, 300.0, 150.0),
            rows: vec![TableRow {
                cells: vec![TableCell::new(
                    String::new(),
                    BoundingBox::new(100.0, 100.0, 300.0, 150.0),
                )],
                is_header: false,
            }],
            num_columns: 1,
            detection_method: TableDetectionMethod::HLine,
            column_positions: vec![100.0, 300.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        };
        let mut tables = vec![a, b];
        merge_adjacent_tables(&mut tables);
        assert_eq!(tables.len(), 2, "HLine tables should not merge");
    }

    #[test]
    fn test_gap_based_column_detection() {
        // 4 rows of 3-column data with clear gaps.
        let spans: Vec<(f64, f64, f64)> = (0..4)
            .flat_map(|row| {
                let y = 500.0 - row as f64 * 20.0;
                vec![
                    (50.0, 120.0, y),  // col 1
                    (180.0, 280.0, y), // col 2
                    (340.0, 420.0, y), // col 3
                ]
            })
            .collect();
        let cols = detect_columns_gap_based(&spans, 4);
        assert!(
            cols.len() >= 3,
            "expected >= 3 columns, got {}: {:?}",
            cols.len(),
            cols
        );
    }

    #[test]
    fn test_gap_based_too_few_rows() {
        // Only 1 row: should return empty.
        let spans = vec![
            (50.0, 120.0, 500.0),
            (180.0, 280.0, 500.0),
            (340.0, 420.0, 500.0),
        ];
        let cols = detect_columns_gap_based(&spans, 1);
        assert!(cols.is_empty(), "too few rows should return empty");
    }
}

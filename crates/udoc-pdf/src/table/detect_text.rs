//! Text-alignment table detection (borderless fallback).
//!
//! Detects tables from text alignment patterns when no ruled lines exist.
//! This is the "stream" mode approach (inspired by pdfplumber): cluster
//! span positions into rows and columns, validate grid consistency, and
//! build `Table` structs from the resulting alignment grid.
//!
//! This detector has LOWER confidence than ruled-line detection. It requires
//! clear column alignment (>= 3 rows, >= 2 columns, consistent column
//! occupancy) to avoid false positives on paragraph text.

use crate::diagnostics::{DiagnosticsSink, Warning, WarningContext, WarningKind, WarningLevel};
use crate::geometry::BoundingBox;
use crate::text::types::TextSpan;

use super::types::{Table, TableCell, TableDetectionMethod, TableRow};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Minimum number of rows to qualify as a table.
/// Some tables are just header + 1 data row.
const MIN_ROWS: usize = 2;

/// Minimum number of columns to qualify as a table.
const MIN_COLUMNS: usize = 2;

/// Minimum gap floor (in points) between columns. The actual threshold
/// is adaptive: max(MIN_COLUMN_GAP_FLOOR, median_font_size * 0.5).
const MIN_COLUMN_GAP_FLOOR: f64 = 6.0;

/// Fraction of rows that must have spans in at least 2 columns for the
/// grid to be considered consistent. Rows that fail are dropped, but if
/// too many fail we reject the whole candidate. Set to 0.4 to allow
/// tables embedded in narrative text (common in financial filings).
const MIN_ROW_CONSISTENCY: f64 = 0.4;

// Security limits

/// Maximum number of rows in a text-alignment table.
const MAX_ROWS: usize = 200;

/// Maximum number of columns in a text-alignment table.
const MAX_COLUMNS: usize = 50;

/// Maximum number of text-alignment tables per page.
const MAX_TABLES_PER_PAGE: usize = 5;

/// Tolerance (in points) for snapping spans to column boundaries.
const COL_TOLERANCE: f64 = 5.0;

/// Tolerance (in points) for assigning spans to the last (rightmost) column.
/// More generous than inter-column tolerance since there is no right neighbor.
const LAST_COLUMN_TOLERANCE: f64 = 50.0;

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

/// A span's position data extracted for clustering.
struct SpanPosition {
    left_x: f64,
    right_x: f64,
    baseline_y: f64,
    font_size: f64,
    /// Index into the original span slice, used by dot-leader filtering.
    span_index: usize,
}

/// A cluster of Y positions representing one row.
struct RowCluster {
    baseline: f64,
    span_indices: Vec<usize>, // indices into SpanPosition vec
}

/// A cluster of X positions representing one column.
#[derive(Clone)]
struct ColumnCluster {
    left_x: f64,
    /// Rightmost right_x of any span in this column.
    right_x: f64,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Detect tables from text alignment patterns (borderless table fallback).
///
/// Analyzes span positions to find grid-like alignment patterns. Requires
/// at least 2 rows and 2 columns with consistent occupancy. Supports
/// multiple tables per page by splitting into Y-gap-separated regions.
/// Returns at most `MAX_TABLES_PER_PAGE` tables.
///
/// This should be called independently of `detect_tables` (ruled-line).
/// The caller is responsible for merging results and avoiding overlaps.
pub fn detect_text_tables(
    spans: &[TextSpan],
    page_bbox: &BoundingBox,
    diagnostics: &dyn DiagnosticsSink,
) -> Vec<Table> {
    // Collect non-empty span positions.
    let positions = collect_positions(spans);
    if positions.len() < MIN_ROWS * MIN_COLUMNS {
        return Vec::new();
    }

    // Compute a representative font size for tolerances.
    let median_font_size = median_font_size(&positions);
    let row_tolerance = median_font_size * 0.3;

    // Cluster Y positions into rows.
    let mut rows = cluster_y_positions(&positions, row_tolerance);

    // Security limit: too many rows.
    if rows.len() > MAX_ROWS {
        diagnostics.warning(Warning {
            offset: None,
            kind: WarningKind::ResourceLimit,
            level: WarningLevel::Warning,
            context: WarningContext::default(),
            message: format!(
                "text-alignment table detection: {} rows exceeds limit {}, truncating",
                rows.len(),
                MAX_ROWS
            ),
        });
        rows.truncate(MAX_ROWS);
    }

    if rows.len() < MIN_ROWS {
        return Vec::new();
    }

    // Split rows into regions separated by large Y gaps.
    // A gap > 3x the median row spacing indicates a new region.
    let regions = split_into_regions(&rows, median_font_size);

    let mut result: Vec<Table> = Vec::new();

    for region_rows in regions {
        if result.len() >= MAX_TABLES_PER_PAGE {
            diagnostics.warning(Warning {
                offset: None,
                kind: WarningKind::ResourceLimit,
                level: WarningLevel::Warning,
                context: WarningContext::default(),
                message: format!(
                    "text-alignment table detection: {} tables limit reached, skipping remaining",
                    MAX_TABLES_PER_PAGE
                ),
            });
            break;
        }

        if let Some(table) = try_detect_region(
            &region_rows,
            &positions,
            spans,
            median_font_size,
            page_bbox,
            diagnostics,
        ) {
            result.push(table);
        }
    }

    result
}

/// Split rows into regions separated by large Y gaps.
/// Returns groups of row references, each group is a candidate table region.
fn split_into_regions(rows: &[RowCluster], median_font_size: f64) -> Vec<Vec<&RowCluster>> {
    if rows.is_empty() {
        return Vec::new();
    }

    // Rows are sorted top-to-bottom (descending Y). A "gap" is when the
    // distance between consecutive row baselines exceeds the threshold.
    // 2x font size separates distinct table regions on the page.
    let gap_threshold = median_font_size * 2.0;

    let mut regions: Vec<Vec<&RowCluster>> = Vec::new();
    let mut current: Vec<&RowCluster> = vec![&rows[0]];

    for i in 1..rows.len() {
        let prev_y = rows[i - 1].baseline;
        let curr_y = rows[i].baseline;
        let gap = (prev_y - curr_y).abs(); // prev_y > curr_y since sorted desc

        if gap > gap_threshold {
            regions.push(current);
            current = Vec::new();
        }
        current.push(&rows[i]);
    }
    if !current.is_empty() {
        regions.push(current);
    }

    regions
}

/// Try to detect a text-alignment table from a region of rows.
fn try_detect_region(
    region_rows: &[&RowCluster],
    positions: &[SpanPosition],
    spans: &[TextSpan],
    median_font_size: f64,
    page_bbox: &BoundingBox,
    diagnostics: &dyn DiagnosticsSink,
) -> Option<Table> {
    if region_rows.len() < MIN_ROWS {
        return None;
    }

    // Collect positions only from spans in this region.
    let region_span_indices: Vec<usize> = region_rows
        .iter()
        .flat_map(|r| r.span_indices.iter().copied())
        .collect();

    let region_positions: Vec<&SpanPosition> =
        region_span_indices.iter().map(|&i| &positions[i]).collect();

    // Compute region bounding box for text-edge column detection.
    let region_bbox = {
        let x_min = region_positions
            .iter()
            .map(|p| p.left_x)
            .fold(f64::INFINITY, f64::min);
        let x_max = region_positions
            .iter()
            .map(|p| p.right_x)
            .fold(f64::NEG_INFINITY, f64::max);
        let y_min = region_positions
            .iter()
            .map(|p| p.baseline_y)
            .fold(f64::INFINITY, f64::min);
        let y_max = region_positions
            .iter()
            .map(|p| p.baseline_y)
            .fold(f64::NEG_INFINITY, f64::max);
        let margin = median_font_size;
        BoundingBox::new(
            x_min - margin,
            y_min - margin,
            x_max + margin,
            y_max + margin,
        )
    };

    // Try Nurminen text-edge column detection first, fall back to simple clustering.
    let text_edge_cols = super::text_edge::detect_columns_text_edge(spans, &region_bbox);

    let columns = if text_edge_cols.len() >= MIN_COLUMNS {
        // Convert text-edge left-x positions to ColumnCluster structs.
        // Compute right_x extent from non-leader spans, capped at the next
        // column boundary. Dot-leader spans ("......") stretch across columns,
        // inflating right_x and killing inter-column gaps.
        let col_tolerance = COL_TOLERANCE;
        let mut cols: Vec<ColumnCluster> = text_edge_cols
            .iter()
            .enumerate()
            .map(|(ci, &lx)| {
                // Cap right_x at midpoint to next column (or page right edge).
                let max_right = if ci + 1 < text_edge_cols.len() {
                    (lx + text_edge_cols[ci + 1]) / 2.0
                } else {
                    region_bbox.x_max
                };
                let right_x = region_positions
                    .iter()
                    .filter(|p| (p.left_x - lx).abs() < col_tolerance)
                    .filter(|p| !is_dot_leader_span(p, spans))
                    .map(|p| p.right_x.min(max_right))
                    .fold(lx, f64::max);
                ColumnCluster {
                    left_x: lx,
                    right_x,
                }
            })
            .collect();
        // For the last column, use uncapped right_x (no next column boundary).
        if let Some(last) = cols.last_mut() {
            let lx = last.left_x;
            let right_x = region_positions
                .iter()
                .filter(|p| (p.left_x - lx).abs() < col_tolerance)
                .filter(|p| !is_dot_leader_span(p, spans))
                .map(|p| p.right_x)
                .fold(lx, f64::max);
            last.right_x = right_x;
        }
        cols
    } else {
        // Fallback: X-extent filtered simple clustering (original algorithm).
        let filtered_positions: Vec<&SpanPosition> = if region_positions.len() >= 4 {
            let mut left_xs: Vec<f64> = region_positions.iter().map(|p| p.left_x).collect();
            left_xs.sort_by(f64::total_cmp);
            let p10_idx = (left_xs.len() as f64 * 0.10) as usize;
            let p90_idx = ((left_xs.len() as f64 * 0.90) as usize).min(left_xs.len() - 1);
            let x_lo = left_xs[p10_idx];
            let x_hi = left_xs[p90_idx];
            let margin = median_font_size;
            region_positions
                .iter()
                .copied()
                .filter(|p| p.left_x >= x_lo - margin && p.left_x <= x_hi + margin)
                .collect()
        } else {
            region_positions.to_vec()
        };

        let x_tolerance = COL_TOLERANCE;
        let region_pos_for_clustering: Vec<SpanPosition> = filtered_positions
            .iter()
            .enumerate()
            .map(|(i, p)| SpanPosition {
                left_x: p.left_x,
                right_x: p.right_x,
                baseline_y: p.baseline_y,
                font_size: p.font_size,
                span_index: i,
            })
            .collect();
        cluster_x_positions(&region_pos_for_clustering, x_tolerance)
    };

    if columns.len() < MIN_COLUMNS {
        return None;
    }

    if columns.len() > MAX_COLUMNS {
        diagnostics.warning(Warning {
            offset: None,
            kind: WarningKind::ResourceLimit,
            level: WarningLevel::Warning,
            context: WarningContext::default(),
            message: format!(
                "text-alignment table detection: {} columns exceeds limit {}, skipping",
                columns.len(),
                MAX_COLUMNS
            ),
        });
        return None;
    }

    // Merge columns that are too close together, then validate gaps.
    let min_gap = MIN_COLUMN_GAP_FLOOR.max(median_font_size * 0.5);
    let columns = merge_close_columns(columns, min_gap);
    if columns.len() < MIN_COLUMNS {
        return None;
    }

    // Validate grid consistency: use original positions for column assignment.
    let valid_rows = validate_rows_region(region_rows, positions, &columns);
    if valid_rows.len() < MIN_ROWS {
        return None;
    }

    let consistency = valid_rows.len() as f64 / region_rows.len() as f64;
    if consistency < MIN_ROW_CONSISTENCY {
        return None;
    }

    Some(build_table(&valid_rows, positions, &columns, page_bbox))
}

/// Validate rows within a region, using original position indices.
fn validate_rows_region<'a>(
    rows: &[&'a RowCluster],
    positions: &[SpanPosition],
    columns: &[ColumnCluster],
) -> Vec<&'a RowCluster> {
    let mut valid = Vec::new();
    for &row in rows {
        let cols_used = count_columns_used(row, positions, columns);
        if cols_used >= MIN_COLUMNS {
            valid.push(row);
        }
    }
    valid
}

// ---------------------------------------------------------------------------
// Step 1: Collect span positions
// ---------------------------------------------------------------------------

fn collect_positions(spans: &[TextSpan]) -> Vec<SpanPosition> {
    let mut positions = Vec::new();
    for (i, span) in spans.iter().enumerate() {
        if span.text.trim().is_empty() {
            continue;
        }
        // Skip rotated or vertical text; they don't participate in
        // horizontal table alignment.
        if span.rotation.abs() > 1.0 || span.is_vertical {
            continue;
        }
        positions.push(SpanPosition {
            left_x: span.x,
            right_x: span.x + span.width,
            baseline_y: span.y,
            font_size: span.font_size,
            span_index: i,
        });
    }
    positions
}

// ---------------------------------------------------------------------------
// Step 2: Cluster Y positions into rows
// ---------------------------------------------------------------------------

/// Group spans by baseline Y. Spans within `tolerance` of each other share
/// a row. Uses a simple greedy sweep: sort by Y descending (top to bottom
/// in PDF coords), then assign each span to the current cluster or start a
/// new one.
fn cluster_y_positions(positions: &[SpanPosition], tolerance: f64) -> Vec<RowCluster> {
    if positions.is_empty() {
        return Vec::new();
    }

    // Sort by Y descending (top-to-bottom in PDF space).
    let mut sorted_indices: Vec<usize> = (0..positions.len()).collect();
    sorted_indices.sort_by(|&a, &b| positions[b].baseline_y.total_cmp(&positions[a].baseline_y));

    let mut clusters: Vec<RowCluster> = Vec::new();

    for &idx in &sorted_indices {
        let y = positions[idx].baseline_y;
        // Try to find an existing cluster within tolerance.
        let mut found = false;
        for cluster in &mut clusters {
            if (cluster.baseline - y).abs() <= tolerance {
                cluster.span_indices.push(idx);
                found = true;
                break;
            }
        }
        if !found {
            clusters.push(RowCluster {
                baseline: y,
                span_indices: vec![idx],
            });
        }
    }

    // Sort clusters top-to-bottom (descending Y in PDF space).
    clusters.sort_by(|a, b| b.baseline.total_cmp(&a.baseline));

    clusters
}

// ---------------------------------------------------------------------------
// Step 3: Cluster X positions into columns
// ---------------------------------------------------------------------------

/// Find distinct left-edge X positions across all spans and cluster them.
/// Uses 1D greedy clustering: sort left_x values, merge those within
/// `tolerance` of each other.
fn cluster_x_positions(positions: &[SpanPosition], tolerance: f64) -> Vec<ColumnCluster> {
    if positions.is_empty() {
        return Vec::new();
    }

    // Collect (left_x, right_x) pairs sorted by left_x.
    let mut xs: Vec<(f64, f64)> = positions.iter().map(|p| (p.left_x, p.right_x)).collect();
    xs.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut clusters: Vec<ColumnCluster> = Vec::new();

    for (lx, rx) in xs {
        if let Some(last) = clusters.last_mut() {
            if (lx - last.left_x).abs() <= tolerance {
                // Same column cluster: update right extent.
                if rx > last.right_x {
                    last.right_x = rx;
                }
                continue;
            }
        }
        clusters.push(ColumnCluster {
            left_x: lx,
            right_x: rx,
        });
    }

    clusters
}

// ---------------------------------------------------------------------------
// Step 3b: Dot-leader detection
// ---------------------------------------------------------------------------

/// Check if a span is a typographic dot-leader ("........." or similar).
/// These spans bridge column gaps in financial tables, tables of contents,
/// and exhibit indices, inflating column right_x measurements.
fn is_dot_leader_span(pos: &SpanPosition, spans: &[TextSpan]) -> bool {
    let text = &spans[pos.span_index].text;
    if text.len() < 4 {
        return false;
    }
    let dot_count = text
        .chars()
        .filter(|&c| c == '.' || c == '\u{00B7}')
        .count();
    let total = text.chars().count();
    // More than 60% dots with at least 4 characters -> dot leader.
    total >= 4 && dot_count * 10 > total * 6
}

// ---------------------------------------------------------------------------
// Step 4: Validate column gaps
// ---------------------------------------------------------------------------

/// Merge adjacent columns whose gap is below the minimum threshold.
/// When two clusters are too close (e.g., "2007" and "2006" year columns
/// at x=494 and x=509 with only 5pt gap), they belong to the same column.
fn merge_close_columns(columns: Vec<ColumnCluster>, min_gap: f64) -> Vec<ColumnCluster> {
    if columns.is_empty() {
        return columns;
    }
    let mut merged: Vec<ColumnCluster> = Vec::with_capacity(columns.len());
    merged.push(columns[0].clone());
    for col in columns.into_iter().skip(1) {
        let last_idx = merged.len() - 1; // always >= 0: pushed above
        let gap = col.left_x - merged[last_idx].right_x;
        if gap < min_gap {
            merged[last_idx].right_x = merged[last_idx].right_x.max(col.right_x);
        } else {
            merged.push(col);
        }
    }
    merged
}

// ---------------------------------------------------------------------------
// Step 5: Validate grid consistency
// ---------------------------------------------------------------------------

/// Count how many distinct columns are occupied by spans in this row.
fn count_columns_used(
    row: &RowCluster,
    positions: &[SpanPosition],
    columns: &[ColumnCluster],
) -> usize {
    let mut used = vec![false; columns.len()];
    for &span_idx in &row.span_indices {
        let pos = &positions[span_idx];
        if let Some(col_idx) = find_column(pos.left_x, columns) {
            used[col_idx] = true;
        }
    }
    used.iter().filter(|&&u| u).count()
}

/// Find which column a span's left_x belongs to. Returns the index of the
/// column whose left_x is closest (within a reasonable tolerance derived
/// from column spacing).
fn find_column(left_x: f64, columns: &[ColumnCluster]) -> Option<usize> {
    let mut best_idx = None;
    let mut best_dist = f64::MAX;

    for (i, col) in columns.iter().enumerate() {
        let dist = (left_x - col.left_x).abs();
        // Allow assignment if the span's left_x falls between this column's
        // left edge and the next column's left edge (or page end).
        let max_range = if i + 1 < columns.len() {
            (columns[i + 1].left_x - col.left_x) / 2.0
        } else {
            LAST_COLUMN_TOLERANCE
        };
        if dist < best_dist && dist <= max_range {
            best_dist = dist;
            best_idx = Some(i);
        }
    }

    best_idx
}

// ---------------------------------------------------------------------------
// Step 6: Build Table struct
// ---------------------------------------------------------------------------

/// Build a `Table` from validated rows and column clusters.
fn build_table(
    rows: &[&RowCluster],
    positions: &[SpanPosition],
    columns: &[ColumnCluster],
    page_bbox: &BoundingBox,
) -> Table {
    let num_columns = columns.len();

    // Compute column boundaries. Each column's left boundary is its
    // cluster left_x. The right boundary extends to the next column's
    // left_x (with some padding) or the page right edge for the last column.
    let mut col_bounds: Vec<(f64, f64)> = Vec::with_capacity(num_columns);
    for (i, col) in columns.iter().enumerate() {
        let left = col.left_x;
        let right = if i + 1 < columns.len() {
            // Midpoint between this column's right extent and next column's left.
            (col.right_x + columns[i + 1].left_x) / 2.0
        } else {
            // Last column: extend to page right or column's own right extent,
            // whichever is larger (with a small margin).
            col.right_x.max(left + 10.0).min(page_bbox.x_max)
        };
        col_bounds.push((left, right));
    }

    // Build rows top-to-bottom (rows are already sorted descending Y).
    let median_font = median_font_size(positions);
    let row_height = median_font * 1.5; // approximate row height

    let mut table_rows: Vec<TableRow> = Vec::with_capacity(rows.len());

    for row in rows {
        let mut cells: Vec<TableCell> = Vec::with_capacity(num_columns);
        let row_y_center = row.baseline;

        for &(col_left, col_right) in col_bounds.iter() {
            // Cell bbox: column boundaries horizontally, row baseline +/- half row height vertically.
            let cell_bbox = BoundingBox::new(
                col_left,
                row_y_center - row_height / 2.0,
                col_right,
                row_y_center + row_height / 2.0,
            );

            // Cell text will be filled later by fill_table_text.
            cells.push(TableCell::new(String::new(), cell_bbox));
        }

        table_rows.push(TableRow {
            cells,
            is_header: false,
        });
    }

    // Compute overall table bounding box.
    let table_left = col_bounds.first().map(|b| b.0).unwrap_or(0.0);
    let table_right = col_bounds.last().map(|b| b.1).unwrap_or(0.0);
    let table_top = rows
        .first()
        .map(|r| r.baseline + row_height / 2.0)
        .unwrap_or(0.0);
    let table_bottom = rows
        .last()
        .map(|r| r.baseline - row_height / 2.0)
        .unwrap_or(0.0);

    let table_bbox = BoundingBox::new(table_left, table_bottom, table_right, table_top);

    // Column positions (left edges).
    let column_positions: Vec<f64> = columns.iter().map(|c| c.left_x).collect();

    Table {
        bbox: table_bbox,
        rows: table_rows,
        num_columns,
        detection_method: TableDetectionMethod::TextAlignment,
        column_positions,
        may_continue_from_previous: false,
        may_continue_to_next: false,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute the median font size from span positions.
/// Returns a reasonable default (12.0) if no positions are available.
fn median_font_size(positions: &[SpanPosition]) -> f64 {
    if positions.is_empty() {
        return 12.0;
    }
    let mut sizes: Vec<f64> = positions.iter().map(|p| p.font_size).collect();
    sizes.sort_by(f64::total_cmp);
    let mid = sizes.len() / 2;
    if sizes.len().is_multiple_of(2) && sizes.len() >= 2 {
        (sizes[mid - 1] + sizes[mid]) / 2.0
    } else {
        sizes[mid]
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::NullDiagnostics;

    fn make_span(text: &str, x: f64, y: f64, width: f64) -> TextSpan {
        TextSpan::new(text.to_string(), x, y, width, "Helvetica".to_string(), 12.0)
    }

    fn page_bbox() -> BoundingBox {
        BoundingBox::new(0.0, 0.0, 612.0, 792.0)
    }

    /// Simple 3-column, 4-row aligned text -> detects 1 table.
    #[test]
    fn test_simple_3col_4row_table() {
        let diag = NullDiagnostics;
        let page = page_bbox();

        // 3 columns at x=50, x=200, x=350. 4 rows at y=700, 680, 660, 640.
        let spans = vec![
            // Row 1
            make_span("Name", 50.0, 700.0, 40.0),
            make_span("Age", 200.0, 700.0, 30.0),
            make_span("City", 350.0, 700.0, 35.0),
            // Row 2
            make_span("Alice", 50.0, 680.0, 45.0),
            make_span("30", 200.0, 680.0, 20.0),
            make_span("NYC", 350.0, 680.0, 30.0),
            // Row 3
            make_span("Bob", 50.0, 660.0, 30.0),
            make_span("25", 200.0, 660.0, 20.0),
            make_span("LA", 350.0, 660.0, 20.0),
            // Row 4
            make_span("Carol", 50.0, 640.0, 42.0),
            make_span("35", 200.0, 640.0, 20.0),
            make_span("SF", 350.0, 640.0, 18.0),
        ];

        let tables = detect_text_tables(&spans, &page, &diag);
        assert_eq!(tables.len(), 1, "should detect exactly 1 table");

        let table = &tables[0];
        assert_eq!(table.num_columns, 3);
        assert_eq!(table.rows.len(), 4);
        assert_eq!(table.detection_method, TableDetectionMethod::TextAlignment);
        assert_eq!(table.column_positions.len(), 3);
    }

    /// Paragraph text (no column alignment) -> detects 0 tables.
    #[test]
    fn test_paragraph_text_no_table() {
        let diag = NullDiagnostics;
        let page = page_bbox();

        // Paragraph: each line starts at x=72 (single column).
        let spans = vec![
            make_span("The quick brown fox jumps", 72.0, 700.0, 200.0),
            make_span("over the lazy dog.", 72.0, 686.0, 150.0),
            make_span("Pack my box with five", 72.0, 672.0, 180.0),
            make_span("dozen liquor jugs.", 72.0, 658.0, 140.0),
        ];

        let tables = detect_text_tables(&spans, &page, &diag);
        assert_eq!(tables.len(), 0, "paragraph text should not be a table");
    }

    /// Two-column layout with consistent alignment -> could detect a table
    /// if alignment is consistent and gap is large enough.
    #[test]
    fn test_two_column_layout() {
        let diag = NullDiagnostics;
        let page = page_bbox();

        // Two columns at x=50 and x=320 with sufficient gap.
        // Only 2 columns with 4 rows.
        let spans = vec![
            make_span("Label", 50.0, 700.0, 40.0),
            make_span("Value", 320.0, 700.0, 45.0),
            make_span("Name", 50.0, 680.0, 35.0),
            make_span("Alice", 320.0, 680.0, 42.0),
            make_span("Age", 50.0, 660.0, 25.0),
            make_span("30", 320.0, 660.0, 20.0),
            make_span("City", 50.0, 640.0, 30.0),
            make_span("NYC", 320.0, 640.0, 30.0),
        ];

        let tables = detect_text_tables(&spans, &page, &diag);
        // With 2 columns and 4 rows this should detect a table.
        assert_eq!(
            tables.len(),
            1,
            "2-col 4-row aligned text should be a table"
        );
        assert_eq!(tables[0].num_columns, 2);
        assert_eq!(tables[0].rows.len(), 4);
    }

    /// Mixed: some rows with 3 columns, some with 2 -> validates consistency.
    /// If enough rows have 2+ columns, the table is detected (inconsistent
    /// rows are dropped).
    #[test]
    fn test_mixed_column_count() {
        let diag = NullDiagnostics;
        let page = page_bbox();

        // 5 rows total: 3 have 3 columns, 2 have only 1 column.
        // With MIN_ROW_CONSISTENCY=0.6, we need 3/5=0.6 valid rows. Exactly at threshold.
        let spans = vec![
            // Full rows (3 columns)
            make_span("A", 50.0, 700.0, 20.0),
            make_span("B", 200.0, 700.0, 20.0),
            make_span("C", 350.0, 700.0, 20.0),
            make_span("D", 50.0, 680.0, 20.0),
            make_span("E", 200.0, 680.0, 20.0),
            make_span("F", 350.0, 680.0, 20.0),
            make_span("G", 50.0, 660.0, 20.0),
            make_span("H", 200.0, 660.0, 20.0),
            make_span("I", 350.0, 660.0, 20.0),
            // Sparse rows (only 1 column each)
            make_span("J", 50.0, 640.0, 20.0),
            make_span("K", 50.0, 620.0, 20.0),
        ];

        let tables = detect_text_tables(&spans, &page, &diag);
        // 3 valid rows out of 5 = 0.6 >= MIN_ROW_CONSISTENCY.
        // The 3 valid rows have 3 columns >= MIN_COLUMNS.
        assert_eq!(tables.len(), 1);
        // Only the 3 consistent rows survive.
        assert_eq!(tables[0].rows.len(), 3);
        assert_eq!(tables[0].num_columns, 3);
    }

    /// Security limit: more than MAX_ROWS rows get truncated.
    #[test]
    fn test_security_limit_max_rows() {
        let diag = NullDiagnostics;
        let page = page_bbox();

        // Generate 250 rows (> MAX_ROWS=200) with 3 columns each.
        let mut spans = Vec::new();
        for row in 0..250 {
            let y = 700.0 - (row as f64) * 20.0; // well-spaced rows
            spans.push(make_span("A", 50.0, y, 20.0));
            spans.push(make_span("B", 200.0, y, 20.0));
            spans.push(make_span("C", 350.0, y, 20.0));
        }

        let tables = detect_text_tables(&spans, &page, &diag);
        assert_eq!(tables.len(), 1);
        // Rows should be capped at MAX_ROWS (some may be dropped by
        // consistency validation, but the raw cluster count is capped).
        assert!(
            tables[0].rows.len() <= MAX_ROWS,
            "rows should be capped at MAX_ROWS, got {}",
            tables[0].rows.len()
        );
    }

    /// Two rows with 2 columns -> now detects a table (MIN_ROWS=2).
    #[test]
    fn test_two_row_table() {
        let diag = NullDiagnostics;
        let page = page_bbox();

        let spans = vec![
            make_span("A", 50.0, 700.0, 20.0),
            make_span("B", 200.0, 700.0, 20.0),
            make_span("C", 50.0, 680.0, 20.0),
            make_span("D", 200.0, 680.0, 20.0),
        ];

        let tables = detect_text_tables(&spans, &page, &diag);
        assert_eq!(tables.len(), 1, "2 rows with 2 columns should be a table");
        assert_eq!(tables[0].rows.len(), 2);
    }

    /// Only 1 row -> no table detected.
    #[test]
    fn test_too_few_rows() {
        let diag = NullDiagnostics;
        let page = page_bbox();

        let spans = vec![
            make_span("A", 50.0, 700.0, 20.0),
            make_span("B", 200.0, 700.0, 20.0),
        ];

        let tables = detect_text_tables(&spans, &page, &diag);
        assert_eq!(tables.len(), 0, "1 row should not form a table");
    }

    /// Too few columns (only 1 column) -> no table detected.
    #[test]
    fn test_too_few_columns() {
        let diag = NullDiagnostics;
        let page = page_bbox();

        // All spans at the same X position.
        let spans = vec![
            make_span("Line 1", 72.0, 700.0, 100.0),
            make_span("Line 2", 72.0, 680.0, 100.0),
            make_span("Line 3", 72.0, 660.0, 100.0),
            make_span("Line 4", 72.0, 640.0, 100.0),
        ];

        let tables = detect_text_tables(&spans, &page, &diag);
        assert_eq!(tables.len(), 0, "single column should not be a table");
    }

    /// Columns too close together (gap < MIN_COLUMN_GAP) -> no table.
    #[test]
    fn test_columns_too_close() {
        let diag = NullDiagnostics;
        let page = page_bbox();

        // Two "columns" but the right edge of column 1 nearly touches column 2.
        // Width 95 at x=50 means right edge at 145, column 2 starts at 150.
        // Gap = 150 - 145 = 5 < min_gap (adaptive, at least MIN_COLUMN_GAP_FLOOR=6).
        let spans = vec![
            make_span("Hello world foo", 50.0, 700.0, 95.0),
            make_span("bar baz", 150.0, 700.0, 60.0),
            make_span("Hello world foo", 50.0, 680.0, 95.0),
            make_span("bar baz", 150.0, 680.0, 60.0),
            make_span("Hello world foo", 50.0, 660.0, 95.0),
            make_span("bar baz", 150.0, 660.0, 60.0),
        ];

        let tables = detect_text_tables(&spans, &page, &diag);
        assert_eq!(
            tables.len(),
            0,
            "columns with insufficient gap should not form a table"
        );
    }

    /// Empty span list -> no table.
    #[test]
    fn test_empty_spans() {
        let diag = NullDiagnostics;
        let page = page_bbox();
        let tables = detect_text_tables(&[], &page, &diag);
        assert_eq!(tables.len(), 0);
    }

    /// Whitespace-only spans are ignored.
    #[test]
    fn test_whitespace_only_spans_ignored() {
        let diag = NullDiagnostics;
        let page = page_bbox();

        let spans = vec![
            make_span("  ", 50.0, 700.0, 20.0),
            make_span("\t", 200.0, 700.0, 20.0),
            make_span("", 350.0, 700.0, 20.0),
        ];

        let tables = detect_text_tables(&spans, &page, &diag);
        assert_eq!(tables.len(), 0);
    }

    /// Table bounding box encompasses all cells.
    #[test]
    fn test_table_bbox_coverage() {
        let diag = NullDiagnostics;
        let page = page_bbox();

        let spans = vec![
            make_span("A", 50.0, 700.0, 20.0),
            make_span("B", 200.0, 700.0, 20.0),
            make_span("C", 350.0, 700.0, 20.0),
            make_span("D", 50.0, 680.0, 20.0),
            make_span("E", 200.0, 680.0, 20.0),
            make_span("F", 350.0, 680.0, 20.0),
            make_span("G", 50.0, 660.0, 20.0),
            make_span("H", 200.0, 660.0, 20.0),
            make_span("I", 350.0, 660.0, 20.0),
        ];

        let tables = detect_text_tables(&spans, &page, &diag);
        assert_eq!(tables.len(), 1);

        let table = &tables[0];
        // Table bbox should contain all cell bboxes.
        for row in &table.rows {
            for cell in &row.cells {
                assert!(
                    table.bbox.x_min <= cell.bbox.x_min,
                    "table left {} should be <= cell left {}",
                    table.bbox.x_min,
                    cell.bbox.x_min
                );
                assert!(
                    table.bbox.x_max >= cell.bbox.x_max,
                    "table right {} should be >= cell right {}",
                    table.bbox.x_max,
                    cell.bbox.x_max
                );
                assert!(
                    table.bbox.y_min <= cell.bbox.y_min,
                    "table bottom {} should be <= cell bottom {}",
                    table.bbox.y_min,
                    cell.bbox.y_min
                );
                assert!(
                    table.bbox.y_max >= cell.bbox.y_max,
                    "table top {} should be >= cell top {}",
                    table.bbox.y_max,
                    cell.bbox.y_max
                );
            }
        }
    }

    /// Internal: median font size computation.
    #[test]
    fn test_median_font_size() {
        let positions = vec![
            SpanPosition {
                left_x: 0.0,
                right_x: 10.0,
                baseline_y: 0.0,
                font_size: 10.0,
                span_index: 0,
            },
            SpanPosition {
                left_x: 0.0,
                right_x: 10.0,
                baseline_y: 0.0,
                font_size: 12.0,
                span_index: 1,
            },
            SpanPosition {
                left_x: 0.0,
                right_x: 10.0,
                baseline_y: 0.0,
                font_size: 14.0,
                span_index: 2,
            },
        ];
        let median = median_font_size(&positions);
        assert!((median - 12.0).abs() < f64::EPSILON);
    }

    /// Internal: empty positions give default font size.
    #[test]
    fn test_median_font_size_empty() {
        let median = median_font_size(&[]);
        assert!((median - 12.0).abs() < f64::EPSILON);
    }

    /// Y clustering groups close baselines together.
    #[test]
    fn test_y_clustering() {
        let positions = vec![
            SpanPosition {
                left_x: 0.0,
                right_x: 10.0,
                baseline_y: 700.0,
                font_size: 12.0,
                span_index: 0,
            },
            SpanPosition {
                left_x: 0.0,
                right_x: 10.0,
                baseline_y: 700.5,
                font_size: 12.0,
                span_index: 1,
            },
            SpanPosition {
                left_x: 0.0,
                right_x: 10.0,
                baseline_y: 680.0,
                font_size: 12.0,
                span_index: 2,
            },
        ];
        let clusters = cluster_y_positions(&positions, 3.6);
        assert_eq!(
            clusters.len(),
            2,
            "700 and 700.5 should merge, 680 separate"
        );
        assert_eq!(clusters[0].span_indices.len(), 2);
        assert_eq!(clusters[1].span_indices.len(), 1);
    }

    /// X clustering groups close left edges together.
    #[test]
    fn test_x_clustering() {
        let positions = vec![
            SpanPosition {
                left_x: 50.0,
                right_x: 90.0,
                baseline_y: 700.0,
                font_size: 12.0,
                span_index: 0,
            },
            SpanPosition {
                left_x: 52.0,
                right_x: 95.0,
                baseline_y: 680.0,
                font_size: 12.0,
                span_index: 1,
            },
            SpanPosition {
                left_x: 200.0,
                right_x: 240.0,
                baseline_y: 700.0,
                font_size: 12.0,
                span_index: 2,
            },
        ];
        let clusters = cluster_x_positions(&positions, 5.0);
        assert_eq!(clusters.len(), 2, "50 and 52 should merge, 200 separate");
        assert!((clusters[0].left_x - 50.0).abs() < f64::EPSILON);
        assert!((clusters[1].left_x - 200.0).abs() < f64::EPSILON);
    }
}

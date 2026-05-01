//! Cell content extraction: assign text spans to table cells.
//!
//! Given table geometry (cell bounding boxes from the detect module) and text
//! spans from the content interpreter, assigns spans to cells based on center
//! point containment. Spans within each cell are ordered by baseline then X.

use crate::diagnostics::DiagnosticsSink;
use crate::geometry::BoundingBox;
use crate::table::types::PathSegment;
use crate::text::types::TextSpan;

use super::types::{Table, TableCell, TableDetectionMethod, TableRow};

/// Bucket size (in points) for the Y-axis spatial index used by `fill_table_text`.
const SPAN_BUCKET_SIZE: f64 = 50.0;

/// Populate table cell text from text spans.
///
/// For each cell in each table, collects spans whose center point falls within
/// the cell bbox, orders them by baseline Y then X, and joins them with spaces
/// (within a line) or newlines (between lines).
///
/// Uses a Y-bucket spatial index so each cell only examines spans in
/// overlapping Y buckets instead of scanning all spans (O(S*C) -> O(S + C*B)).
///
/// Baseline grouping uses a tolerance of half the first span's font size in
/// each cell. Spans on the "same line" (Y difference < tolerance) are joined
/// with spaces; different lines are separated by newlines.
pub fn fill_table_text(tables: &mut [Table], spans: &[TextSpan]) {
    if spans.is_empty() {
        return;
    }

    // Build Y-bucket spatial index over non-empty spans.
    let (buckets, min_y) = build_y_buckets(spans);

    for table in tables.iter_mut() {
        // Track which span indices have been assigned within this table
        // to prevent duplicate assignment at shared cell boundaries.
        let mut assigned: Vec<bool> = vec![false; spans.len()];

        for row in &mut table.rows {
            for cell in &mut row.cells {
                // Collect spans whose center falls within this cell's bbox,
                // only scanning Y-buckets that overlap the cell. Skip spans
                // already assigned to another cell in this table.
                let mut hits: Vec<&TextSpan> =
                    collect_spans_in_bbox_unique(&buckets, min_y, &cell.bbox, spans, &mut assigned);

                if hits.is_empty() {
                    continue;
                }

                // Sort by Y descending (top-to-bottom in PDF coords where Y
                // increases upward), then X ascending (left to right).
                hits.sort_by(|a, b| b.y.total_cmp(&a.y).then(a.x.total_cmp(&b.x)));

                // Group by baseline: spans within tolerance of the first span
                // in a group share a line.
                let tolerance = hits[0].font_size * 0.5;
                // Use a minimum tolerance to avoid degenerate zero-font-size spans
                // collapsing distinct lines.
                let tolerance = if tolerance > 0.0 { tolerance } else { 1.0 };

                let mut lines: Vec<Vec<&TextSpan>> = Vec::new();
                let mut current_line: Vec<&TextSpan> = vec![hits[0]];
                let mut current_y = hits[0].y;

                for span in &hits[1..] {
                    if (current_y - span.y).abs() < tolerance {
                        // Same baseline group.
                        current_line.push(span);
                    } else {
                        // New baseline group.
                        lines.push(current_line);
                        current_line = vec![span];
                        current_y = span.y;
                    }
                }
                lines.push(current_line);

                // Sort each line's spans by X (left to right). The initial
                // sort groups by Y, but within a baseline group the X order
                // may be disrupted when Y values differ slightly.
                for line in &mut lines {
                    line.sort_by(|a, b| a.x.total_cmp(&b.x));
                }

                // Build the cell text: use gap detection to decide whether to
                // insert a space between consecutive spans on the same line.
                let line_texts: Vec<String> = lines
                    .iter()
                    .map(|line| {
                        let mut text = String::new();
                        for (i, span) in line.iter().enumerate() {
                            if i > 0 {
                                let prev = line[i - 1];
                                let gap = span.x - (prev.x + prev.width);
                                // Insert space only if there's a visible gap.
                                // A gap smaller than ~30% of the font size is
                                // likely kerning or glyph adjacency.
                                let threshold = span.font_size * 0.15;
                                if gap > threshold {
                                    text.push(' ');
                                }
                            }
                            text.push_str(&span.text);
                        }
                        text
                    })
                    .collect();

                cell.text = line_texts.join("\n");
            }
        }
    }
}

/// Build a Y-bucket index: each bucket covers `SPAN_BUCKET_SIZE` points of Y
/// range and contains indices into the original `spans` slice for non-empty spans.
fn build_y_buckets(spans: &[TextSpan]) -> (Vec<Vec<usize>>, f64) {
    if spans.is_empty() {
        return (Vec::new(), 0.0);
    }
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for s in spans {
        if s.text.is_empty() {
            continue;
        }
        if s.y < min_y {
            min_y = s.y;
        }
        if s.y > max_y {
            max_y = s.y;
        }
    }
    if !min_y.is_finite() || !max_y.is_finite() {
        return (Vec::new(), 0.0);
    }
    // Use saturating_add to avoid overflow when span coordinates are extreme.
    let raw = ((max_y - min_y) / SPAN_BUCKET_SIZE).ceil();
    let n_buckets = if raw.is_finite() && (0.0..100_000.0).contains(&raw) {
        (raw as usize).saturating_add(1)
    } else {
        100_000
    };
    let mut buckets = vec![Vec::new(); n_buckets];
    for (i, s) in spans.iter().enumerate() {
        if s.text.is_empty() {
            continue;
        }
        let bi = ((s.y - min_y) / SPAN_BUCKET_SIZE) as usize;
        let bi = bi.min(n_buckets - 1);
        buckets[bi].push(i);
    }
    (buckets, min_y)
}

/// Compute bucket range for a Y interval, clamped to valid indices.
fn bucket_range(y_min: f64, y_max: f64, min_y: f64, n_buckets: usize) -> (usize, usize) {
    let lo = ((y_min - min_y) / SPAN_BUCKET_SIZE).floor() as isize;
    let hi = ((y_max - min_y) / SPAN_BUCKET_SIZE).ceil() as isize;
    let lo = (lo.max(0) as usize).min(n_buckets - 1);
    let hi = (hi.max(0) as usize).min(n_buckets - 1);
    (lo, hi)
}

/// Collect spans whose center point falls within `bbox`, using the Y-bucket index
/// for fast spatial lookup. Marks collected spans in `assigned` to prevent
/// duplicate assignment across cells within the same table.
fn collect_spans_in_bbox_unique<'a>(
    buckets: &[Vec<usize>],
    min_y: f64,
    bbox: &BoundingBox,
    spans: &'a [TextSpan],
    assigned: &mut [bool],
) -> Vec<&'a TextSpan> {
    if buckets.is_empty() {
        return Vec::new();
    }
    let n_buckets = buckets.len();
    let (lo, hi) = bucket_range(bbox.y_min, bbox.y_max, min_y, n_buckets);

    let mut hits = Vec::new();
    for bucket in buckets.iter().take(hi + 1).skip(lo) {
        for &si in bucket {
            if assigned[si] {
                continue;
            }
            let span = &spans[si];
            let center_x = span.x + span.width / 2.0;
            let center_y = span.y;
            if bbox.contains_point(center_x, center_y) {
                hits.push(span);
                assigned[si] = true;
            }
        }
    }
    hits
}

/// Split mega-rows that contain multiple baselines of text into sub-rows.
///
/// When h-line detection produces a row band spanning a large Y range (e.g.,
/// a LaTeX table with rules only at top/middle/bottom), all data rows between
/// two h-lines collapse into one mega-row. This function detects distinct
/// text baselines within each row and splits into sub-rows so each sub-row
/// contains one visual row of text.
///
/// Must be called BEFORE `fill_table_text` so that the split sub-rows get
/// proper cell bboxes and text assignment works correctly.
pub(crate) fn split_mega_rows(tables: &mut [Table], spans: &[TextSpan]) {
    if spans.is_empty() {
        return;
    }

    // Build a Y-bucket spatial index once for all tables.
    let (buckets, min_y) = build_y_buckets(spans);
    if buckets.is_empty() {
        return;
    }
    let n_buckets = buckets.len();

    for table in tables.iter_mut() {
        let mut new_rows: Vec<TableRow> = Vec::new();
        let mut changed = false;

        for row in &table.rows {
            if row.cells.is_empty() {
                new_rows.push(row.clone());
                continue;
            }

            // Compute the row's bounding box from its cell bboxes.
            let row_y_min = row
                .cells
                .iter()
                .map(|c| c.bbox.y_min)
                .fold(f64::INFINITY, f64::min);
            let row_y_max = row
                .cells
                .iter()
                .map(|c| c.bbox.y_max)
                .fold(f64::NEG_INFINITY, f64::max);
            let row_x_min = row
                .cells
                .iter()
                .map(|c| c.bbox.x_min)
                .fold(f64::INFINITY, f64::min);
            let row_x_max = row
                .cells
                .iter()
                .map(|c| c.bbox.x_max)
                .fold(f64::NEG_INFINITY, f64::max);

            // Collect baselines using the Y-bucket index instead of scanning all spans.
            let (lo, hi) = bucket_range(row_y_min, row_y_max, min_y, n_buckets);
            let mut baselines: Vec<(f64, f64)> = Vec::new(); // (y, font_size)
            let mut rotated_count = 0_usize;
            let mut total_count = 0_usize;
            for bucket in buckets.iter().take(hi + 1).skip(lo) {
                for &si in bucket {
                    let span = &spans[si];
                    if span.text.trim().is_empty() {
                        continue;
                    }
                    let center_x = span.x + span.width / 2.0;
                    if center_x >= row_x_min
                        && center_x <= row_x_max
                        && span.y >= row_y_min
                        && span.y <= row_y_max
                    {
                        baselines.push((span.y, span.font_size));
                        total_count += 1;
                        if span.rotation.abs() > 45.0 {
                            rotated_count += 1;
                        }
                    }
                }
            }

            // Skip splitting when most text is rotated. Rotated text
            // has vertical baselines that look like many "rows" but
            // actually represent columns in the visual layout.
            if total_count > 0 && rotated_count * 2 > total_count {
                new_rows.push(row.clone());
                continue;
            }

            if baselines.len() <= 1 {
                new_rows.push(row.clone());
                continue;
            }

            // Sort by Y descending (top-to-bottom in visual order).
            baselines.sort_by(|a, b| b.0.total_cmp(&a.0));

            // Compute median font size for gap threshold.
            let mut font_sizes: Vec<f64> = baselines.iter().map(|(_, fs)| *fs).collect();
            font_sizes.sort_by(f64::total_cmp);
            let median_fs = font_sizes[font_sizes.len() / 2];
            let gap_threshold = (median_fs * 0.6).max(2.0);

            // Cluster baselines: spans within gap_threshold of each other share a group.
            // groups is always non-empty (initialized with one element) and each
            // inner Vec always has at least one element, so direct indexing is safe.
            let mut groups: Vec<Vec<f64>> = vec![vec![baselines[0].0]];
            for &(y, _) in &baselines[1..] {
                let gi = groups.len() - 1;
                let last_y = groups[gi][groups[gi].len() - 1];
                if (last_y - y).abs() > gap_threshold {
                    groups.push(vec![y]);
                } else {
                    let gi = groups.len() - 1;
                    groups[gi].push(y);
                }
            }

            if groups.len() <= 1 {
                new_rows.push(row.clone());
                continue;
            }

            // We have multiple baseline groups: split into sub-rows.
            changed = true;

            // Compute Y boundaries between groups using midpoints.
            let group_centers: Vec<f64> = groups
                .iter()
                .map(|g| g.iter().sum::<f64>() / g.len() as f64)
                .collect();

            let mut y_boundaries: Vec<f64> = Vec::with_capacity(groups.len() + 1);
            y_boundaries.push(row_y_max); // top of row
            for i in 0..group_centers.len() - 1 {
                let mid = (group_centers[i] + group_centers[i + 1]) / 2.0;
                y_boundaries.push(mid);
            }
            y_boundaries.push(row_y_min); // bottom of row

            // Create sub-rows. y_boundaries[0] is top, y_boundaries[last] is bottom.
            // group[0] is topmost (highest Y), so sub-row 0 goes [y_boundaries[1], y_boundaries[0]].
            for i in 0..groups.len() {
                let sub_top = y_boundaries[i];
                let sub_bottom = y_boundaries[i + 1];

                let mut cells = Vec::with_capacity(row.cells.len());
                for cell in &row.cells {
                    let sub_bbox =
                        BoundingBox::new(cell.bbox.x_min, sub_bottom, cell.bbox.x_max, sub_top);
                    cells.push(TableCell::new(String::new(), sub_bbox));
                }
                new_rows.push(TableRow {
                    cells,
                    is_header: row.is_header && i == 0,
                });
            }
        }

        if changed {
            table.rows = new_rows;
        }
    }
}

/// Remove rows where all cells are empty (no text content).
///
/// Extra h-lines (boundary edges, duplicate rules) can create phantom rows
/// that contain no text. These inflate the row count and hurt structure
/// matching. Must be called after `fill_table_text`.
pub(crate) fn eliminate_empty_rows(tables: &mut [Table]) {
    for table in tables.iter_mut() {
        let orig_len = table.rows.len();
        table
            .rows
            .retain(|row| row.cells.iter().any(|c| !c.text.trim().is_empty()));
        if table.rows.len() < orig_len && !table.rows.is_empty() {
            // Shrink table bbox to fit remaining rows.
            let y_min = table
                .rows
                .iter()
                .flat_map(|r| r.cells.iter())
                .map(|c| c.bbox.y_min)
                .fold(f64::INFINITY, f64::min);
            let y_max = table
                .rows
                .iter()
                .flat_map(|r| r.cells.iter())
                .map(|c| c.bbox.y_max)
                .fold(f64::NEG_INFINITY, f64::max);
            table.bbox = BoundingBox::new(table.bbox.x_min, y_min, table.bbox.x_max, y_max);
            table.num_columns = table.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0);
        }
    }
}

/// Remove columns where all cells are empty (no text content).
///
/// Per-cell border rects can create phantom narrow columns at grid positions
/// where borders from adjacent cells don't perfectly align. These columns
/// contain no text and inflate the column count. Must be called after
/// `fill_table_text`.
pub(crate) fn eliminate_empty_columns(tables: &mut [Table]) {
    for table in tables.iter_mut() {
        if table.rows.is_empty() {
            continue;
        }
        let num_cols = table.rows[0].cells.len();
        if num_cols <= 1 {
            continue;
        }

        // Find columns where ALL rows have empty text.
        let mut empty_cols: Vec<bool> = vec![true; num_cols];
        for row in &table.rows {
            for (col, cell) in row.cells.iter().enumerate() {
                if col < num_cols && !cell.text.trim().is_empty() {
                    empty_cols[col] = false;
                }
            }
        }

        let empty_count = empty_cols.iter().filter(|&&e| e).count();
        if empty_count == 0 {
            continue;
        }
        // Don't eliminate all columns.
        if num_cols - empty_count < 1 {
            continue;
        }

        // Remove empty columns from each row and merge with adjacent cells.
        // When removing column i, expand the previous non-empty column's bbox
        // to include the removed column's X range.
        for row in &mut table.rows {
            let mut new_cells: Vec<TableCell> = Vec::with_capacity(num_cols - empty_count);
            for (col, cell) in row.cells.iter().enumerate() {
                if col < num_cols && empty_cols[col] {
                    // Empty column: expand previous cell's bbox to cover this space.
                    if let Some(prev) = new_cells.last_mut() {
                        prev.bbox = BoundingBox::new(
                            prev.bbox.x_min,
                            prev.bbox.y_min,
                            cell.bbox.x_max,
                            prev.bbox.y_max,
                        );
                    }
                    // If no previous cell yet, the space will be absorbed by the next cell.
                } else {
                    let mut new_cell = cell.clone();
                    // If leading empty columns existed, expand this cell's bbox leftward.
                    if new_cells.is_empty() && col > 0 {
                        new_cell.bbox = BoundingBox::new(
                            table.bbox.x_min,
                            new_cell.bbox.y_min,
                            new_cell.bbox.x_max,
                            new_cell.bbox.y_max,
                        );
                    }
                    new_cells.push(new_cell);
                }
            }
            row.cells = new_cells;
        }
        table.num_columns = table.rows.first().map_or(0, |r| r.cells.len());

        // Update column_positions.
        let mut new_positions: Vec<f64> = Vec::new();
        for (col, &empty) in empty_cols.iter().enumerate() {
            if !empty && col < table.column_positions.len() {
                new_positions.push(table.column_positions[col]);
            }
        }
        if let Some(&last) = table.column_positions.last() {
            if new_positions
                .last()
                .is_none_or(|p| (*p - last).abs() > 1e-6)
            {
                new_positions.push(last);
            }
        }
        table.column_positions = new_positions;
    }
}

/// Quality metrics for a filled table, used for false positive filtering.
pub(crate) struct TableQuality {
    /// Fraction of cells that have non-empty text (0.0 to 1.0).
    pub cell_occupancy: f64,
    /// Fraction of rows where >= 2 columns have content (0.0 to 1.0).
    pub multi_column_row_fraction: f64,
    /// Number of data rows (excluding header if detected).
    pub data_row_count: usize,
    /// Ratio of text-occupied area to table bbox area.
    pub text_density: f64,
    /// Number of cells with non-empty text.
    pub non_empty_cells: usize,
}

/// Compute quality metrics for a table after text filling.
///
/// These metrics help distinguish real tables from false positives
/// (footnotes, disclaimers, columnar prose). Must be called after
/// `fill_table_text`.
pub(crate) fn compute_table_quality(table: &Table) -> TableQuality {
    let total_cells: usize = table.rows.iter().map(|r| r.cells.len()).sum();
    let non_empty_cells = table
        .rows
        .iter()
        .flat_map(|r| &r.cells)
        .filter(|c| !c.text.trim().is_empty())
        .count();

    let cell_occupancy = if total_cells > 0 {
        non_empty_cells as f64 / total_cells as f64
    } else {
        0.0
    };

    // Count rows where at least 2 columns have content.
    let multi_col_rows = table
        .rows
        .iter()
        .filter(|r| {
            let filled = r.cells.iter().filter(|c| !c.text.trim().is_empty()).count();
            filled >= 2
        })
        .count();
    let multi_column_row_fraction = if !table.rows.is_empty() {
        multi_col_rows as f64 / table.rows.len() as f64
    } else {
        0.0
    };

    let data_row_count = table.rows.iter().filter(|r| !r.is_header).count();

    // Text density: approximate text area vs bbox area.
    // Use character count * avg char width as a rough area proxy.
    let table_area = table.bbox.area();
    let text_area: f64 = table
        .rows
        .iter()
        .flat_map(|r| &r.cells)
        .filter(|c| !c.text.trim().is_empty())
        .map(|c| {
            // Approximate: use cell bbox height * text width as text area.
            // Width from cell bbox, height proportional to number of text lines.
            let text_lines = c.text.lines().count().max(1);
            let cell_h = c.bbox.y_max - c.bbox.y_min;
            let line_h = cell_h / text_lines.max(1) as f64;
            let text_w = c.bbox.x_max - c.bbox.x_min;
            line_h * text_w * text_lines as f64 * 0.5 // ~50% fill factor
        })
        .sum();
    let text_density = if table_area > 0.0 {
        (text_area / table_area).min(1.0)
    } else {
        0.0
    };

    TableQuality {
        cell_occupancy,
        multi_column_row_fraction,
        data_row_count,
        text_density,
        non_empty_cells,
    }
}

/// Detect header rows by checking font characteristics.
///
/// Marks the first row of each table as a header if all its non-empty cells
/// contain text set in a bold font (font_name contains "Bold"). This is a
/// simple heuristic that works for most professionally typeset tables.
///
/// Must be called after `fill_table_text` so cell text is populated.
pub fn detect_header_rows(tables: &mut [Table], spans: &[TextSpan]) {
    if spans.is_empty() {
        return;
    }

    // Build a Y-bucket spatial index once for all tables.
    let (buckets, min_y) = build_y_buckets(spans);
    if buckets.is_empty() {
        return;
    }
    let n_buckets = buckets.len();

    for table in tables.iter_mut() {
        if table.rows.is_empty() {
            continue;
        }

        let first_row = &table.rows[0];

        // Count non-empty cells and bold cells in the first row.
        let mut non_empty = 0;
        let mut bold_count = 0;

        for cell in &first_row.cells {
            if cell.text.is_empty() {
                continue;
            }
            non_empty += 1;

            // Check if ALL spans landing in this cell use a bold font.
            // Use Y-bucket lookup instead of scanning all spans.
            let (lo, hi) = bucket_range(cell.bbox.y_min, cell.bbox.y_max, min_y, n_buckets);
            let mut all_bold = true;
            let mut found_any = false;
            for bucket in buckets.iter().take(hi + 1).skip(lo) {
                for &si in bucket {
                    let s = &spans[si];
                    if s.text.is_empty() {
                        continue;
                    }
                    if cell.bbox.contains_point(s.x + s.width / 2.0, s.y) {
                        found_any = true;
                        if !s.font_name.contains("Bold") && !s.font_name.contains("bold") {
                            all_bold = false;
                        }
                    }
                }
            }

            if found_any && all_bold {
                bold_count += 1;
            }
        }

        // Mark as header if we have at least one non-empty cell and all
        // non-empty cells use bold fonts.
        if non_empty > 0 && bold_count == non_empty {
            table.rows[0].is_header = true;
        }
    }
}

/// Minimum fraction of page area for a table to be kept (0.5% of page).
const MIN_TABLE_AREA_FRACTION: f64 = 0.005;

/// Minimum cell occupancy (fraction of non-empty cells) for a table to pass
/// quality filtering. Below this, the table is mostly empty cells (likely a
/// false positive from stray lines).
const MIN_CELL_OCCUPANCY: f64 = 0.20;

/// Minimum fraction of rows with 2+ filled columns for text-alignment tables.
/// Below this, the "table" is probably a two-column layout or side note.
const MIN_MULTI_COL_ROW_FRACTION: f64 = 0.30;

/// Minimum text density (text area / table area) to keep. Tables below this
/// are mostly whitespace, typically false positives from decorative boxes.
const MIN_TEXT_DENSITY: f64 = 0.03;

/// Minimum data rows for text-alignment tables. With fewer rows, the signal
/// is too weak to distinguish a table from coincidental column alignment.
const MIN_TEXT_ALIGN_DATA_ROWS: usize = 3;

/// IoU / containment threshold for deduplicating tables across detectors.
/// A candidate is "dominated" if overlap exceeds this threshold.
pub(crate) const OVERLAP_THRESHOLD: f64 = 0.5;

/// Run the full table extraction pipeline on pre-interpreted page content.
///
/// Takes paths and spans from a content stream interpretation pass, runs
/// all three detectors (lattice, h-line, text-alignment), deduplicates,
/// and applies post-processing (mega-row splitting, text filling, empty
/// row/column elimination, quality filtering, header detection).
///
/// `glyph_spans` are the original per-glyph spans from the interpreter
/// (used for header detection font analysis). If you only have merged
/// spans, pass the same slice for both.
///
/// This is the same pipeline that [`Page::tables()`](crate::Page::tables) runs internally,
/// exposed for users who want to combine it with [`Page::extract_all()`](crate::Page::extract_all)
/// to avoid re-interpreting the content stream.
pub fn extract_tables(
    paths: &[PathSegment],
    glyph_spans: &[TextSpan],
    page_bbox: &BoundingBox,
    diagnostics: &dyn DiagnosticsSink,
) -> Vec<Table> {
    let merged = super::span_merge::merge_spans_for_table(glyph_spans);

    let mut tables = super::detect::detect_tables(paths, page_bbox, diagnostics);
    super::detect::merge_adjacent_tables(&mut tables);
    let hline_tables = super::detect::detect_hline_tables(paths, &merged, page_bbox, diagnostics);
    let text_tables = super::detect_text::detect_text_tables(&merged, page_bbox, diagnostics);

    // Deduplicate: merge results from all detectors. A candidate is
    // "dominated" if it overlaps an existing table (IoU > 50% or
    // containment > 50%). Exception: if the candidate has 2x+ more
    // columns (>= 3), it replaces the existing table.
    for candidate in hline_tables.into_iter().chain(text_tables) {
        let candidate_area = candidate.bbox.area();
        let mut replace_idx = None;
        let mut dominated = false;
        for (idx, t) in tables.iter().enumerate() {
            let iou = t.bbox.iou(&candidate.bbox);
            let containment = if candidate_area > 0.0 {
                t.bbox.intersection_area(&candidate.bbox) / candidate_area
            } else {
                0.0
            };
            if iou > OVERLAP_THRESHOLD || containment > OVERLAP_THRESHOLD {
                if t.detection_method == TableDetectionMethod::RuledLine
                    && candidate.num_columns >= t.num_columns * 2
                    && candidate.num_columns >= 3
                {
                    replace_idx = Some(idx);
                } else {
                    dominated = true;
                }
                break;
            }
        }
        if let Some(idx) = replace_idx {
            tables[idx] = candidate;
        } else if !dominated {
            tables.push(candidate);
        }
    }

    split_mega_rows(&mut tables, &merged);
    fill_table_text(&mut tables, &merged);
    eliminate_empty_rows(&mut tables);
    eliminate_empty_columns(&mut tables);

    filter_phantom_tables(&mut tables, page_bbox.area());

    detect_header_rows(&mut tables, glyph_spans);
    tables
}

/// Remove phantom tables: tiny fragments, all-empty tables, and structurally
/// invalid detections. Called after text filling and empty row/column elimination.
fn filter_phantom_tables(tables: &mut Vec<Table>, page_area: f64) {
    let min_table_area = page_area * MIN_TABLE_AREA_FRACTION;
    tables.retain(|t| {
        if t.bbox.area() < min_table_area {
            return false;
        }
        let has_text = t
            .rows
            .iter()
            .any(|r| r.cells.iter().any(|c| !c.text.trim().is_empty()));
        if !has_text {
            return false;
        }

        let quality = compute_table_quality(t);
        let actual_cols = t.rows.first().map_or(0, |r| r.cells.len());

        if t.detection_method == TableDetectionMethod::RuledLine {
            let actual_rows = t.rows.len();
            if actual_cols < 2 && actual_rows < 3 {
                return false;
            }
            if actual_rows < 4 && actual_cols < 4 && quality.non_empty_cells < 4 {
                return false;
            }
        }

        if quality.cell_occupancy < MIN_CELL_OCCUPANCY {
            return false;
        }

        if t.detection_method == TableDetectionMethod::TextAlignment
            && quality.multi_column_row_fraction < MIN_MULTI_COL_ROW_FRACTION
        {
            return false;
        }

        if t.detection_method == TableDetectionMethod::TextAlignment
            && quality.data_row_count < MIN_TEXT_ALIGN_DATA_ROWS
        {
            return false;
        }

        if quality.text_density < MIN_TEXT_DENSITY {
            return false;
        }

        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::BoundingBox;
    use crate::table::types::{Table, TableCell, TableDetectionMethod, TableRow};
    use crate::text::types::TextSpan;

    /// Helper: build a 2x2 table with given cell bboxes.
    fn make_2x2_table(tl: BoundingBox, tr: BoundingBox, bl: BoundingBox, br: BoundingBox) -> Table {
        let table_bbox = BoundingBox::new(tl.x_min, bl.y_min, tr.x_max, tl.y_max);
        Table {
            bbox: table_bbox,
            rows: vec![
                TableRow {
                    cells: vec![
                        TableCell::new(String::new(), tl),
                        TableCell::new(String::new(), tr),
                    ],
                    is_header: false,
                },
                TableRow {
                    cells: vec![
                        TableCell::new(String::new(), bl),
                        TableCell::new(String::new(), br),
                    ],
                    is_header: false,
                },
            ],
            num_columns: 2,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![tl.x_min, tr.x_min, tr.x_max],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        }
    }

    fn span(text: &str, x: f64, y: f64, width: f64, font_size: f64) -> TextSpan {
        TextSpan::new(
            text.to_string(),
            x,
            y,
            width,
            "Helvetica".to_string(),
            font_size,
        )
    }

    /// 2x2 grid: spans correctly assigned to the right cells.
    #[test]
    fn test_spans_assigned_to_2x2_cells() {
        // Table layout (PDF coords, Y up):
        //   top row: y=150..200, bottom row: y=100..150
        //   left col: x=0..100, right col: x=100..200
        let tl = BoundingBox::new(0.0, 150.0, 100.0, 200.0);
        let tr = BoundingBox::new(100.0, 150.0, 200.0, 200.0);
        let bl = BoundingBox::new(0.0, 100.0, 100.0, 150.0);
        let br = BoundingBox::new(100.0, 100.0, 200.0, 150.0);
        let mut tables = vec![make_2x2_table(tl, tr, bl, br)];

        let spans = vec![
            span("A", 20.0, 170.0, 30.0, 12.0),  // top-left cell
            span("B", 120.0, 170.0, 30.0, 12.0), // top-right cell
            span("C", 20.0, 120.0, 30.0, 12.0),  // bottom-left cell
            span("D", 120.0, 120.0, 30.0, 12.0), // bottom-right cell
        ];

        fill_table_text(&mut tables, &spans);

        assert_eq!(tables[0].rows[0].cells[0].text, "A");
        assert_eq!(tables[0].rows[0].cells[1].text, "B");
        assert_eq!(tables[0].rows[1].cells[0].text, "C");
        assert_eq!(tables[0].rows[1].cells[1].text, "D");
    }

    /// Spans outside all cells are ignored.
    #[test]
    fn test_spans_outside_cells_ignored() {
        let tl = BoundingBox::new(0.0, 150.0, 100.0, 200.0);
        let tr = BoundingBox::new(100.0, 150.0, 200.0, 200.0);
        let bl = BoundingBox::new(0.0, 100.0, 100.0, 150.0);
        let br = BoundingBox::new(100.0, 100.0, 200.0, 150.0);
        let mut tables = vec![make_2x2_table(tl, tr, bl, br)];

        let spans = vec![
            span("Inside", 20.0, 170.0, 30.0, 12.0),   // in top-left
            span("Outside", 300.0, 500.0, 30.0, 12.0), // nowhere near the table
            span("Above", 50.0, 250.0, 30.0, 12.0),    // above the table
        ];

        fill_table_text(&mut tables, &spans);

        assert_eq!(tables[0].rows[0].cells[0].text, "Inside");
        assert_eq!(tables[0].rows[0].cells[1].text, "");
        assert_eq!(tables[0].rows[1].cells[0].text, "");
        assert_eq!(tables[0].rows[1].cells[1].text, "");
    }

    /// Multi-line cell content: spans at different baselines within one cell.
    #[test]
    fn test_multiline_cell_content() {
        let cell_bbox = BoundingBox::new(0.0, 100.0, 200.0, 200.0);
        let mut tables = vec![Table {
            bbox: cell_bbox,
            rows: vec![TableRow {
                cells: vec![TableCell::new(String::new(), cell_bbox)],
                is_header: false,
            }],
            num_columns: 1,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![0.0, 200.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        }];

        // Two lines of text: "First" at y=180, "Second" at y=150.
        // Font size 12, tolerance = 6. These are 30 apart, so different lines.
        let spans = vec![
            span("First", 10.0, 180.0, 40.0, 12.0),
            span("Second", 10.0, 150.0, 50.0, 12.0),
        ];

        fill_table_text(&mut tables, &spans);

        assert_eq!(tables[0].rows[0].cells[0].text, "First\nSecond");
    }

    /// Empty cells: no spans within bbox -> text stays empty.
    #[test]
    fn test_empty_cells() {
        let cell_bbox = BoundingBox::new(0.0, 100.0, 200.0, 200.0);
        let mut tables = vec![Table {
            bbox: cell_bbox,
            rows: vec![TableRow {
                cells: vec![TableCell::new(String::new(), cell_bbox)],
                is_header: false,
            }],
            num_columns: 1,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![0.0, 200.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        }];

        // No spans at all.
        fill_table_text(&mut tables, &[]);
        assert_eq!(tables[0].rows[0].cells[0].text, "");

        // Spans exist but outside the cell.
        let spans = vec![span("Far away", 500.0, 500.0, 30.0, 12.0)];
        fill_table_text(&mut tables, &spans);
        assert_eq!(tables[0].rows[0].cells[0].text, "");
    }

    /// Span ordering: left-to-right within a line, top-to-bottom across lines.
    #[test]
    fn test_span_ordering() {
        let cell_bbox = BoundingBox::new(0.0, 100.0, 300.0, 200.0);
        let mut tables = vec![Table {
            bbox: cell_bbox,
            rows: vec![TableRow {
                cells: vec![TableCell::new(String::new(), cell_bbox)],
                is_header: false,
            }],
            num_columns: 1,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![0.0, 300.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        }];

        // Insert spans in scrambled order.
        let spans = vec![
            span("C", 200.0, 180.0, 20.0, 12.0), // top line, rightmost
            span("D", 10.0, 140.0, 20.0, 12.0),  // bottom line, left
            span("A", 10.0, 180.0, 20.0, 12.0),  // top line, leftmost
            span("B", 100.0, 180.0, 20.0, 12.0), // top line, middle
            span("E", 100.0, 140.0, 20.0, 12.0), // bottom line, right
        ];

        fill_table_text(&mut tables, &spans);

        // Top line: A B C (left to right), bottom line: D E
        assert_eq!(tables[0].rows[0].cells[0].text, "A B C\nD E");
    }

    /// Spans on the same baseline (within tolerance) are grouped on one line.
    #[test]
    fn test_baseline_tolerance_grouping() {
        let cell_bbox = BoundingBox::new(0.0, 100.0, 200.0, 200.0);
        let mut tables = vec![Table {
            bbox: cell_bbox,
            rows: vec![TableRow {
                cells: vec![TableCell::new(String::new(), cell_bbox)],
                is_header: false,
            }],
            num_columns: 1,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![0.0, 200.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        }];

        // Two spans with Y values 2 points apart. Font size 12 gives tolerance
        // of 6, so they should be on the same line.
        let spans = vec![
            span("Hello", 10.0, 170.0, 40.0, 12.0),
            span("World", 80.0, 172.0, 40.0, 12.0),
        ];

        fill_table_text(&mut tables, &spans);

        // Both on same line, joined with space.
        assert_eq!(tables[0].rows[0].cells[0].text, "Hello World");
    }

    /// Multiple tables: spans are distributed correctly across tables.
    #[test]
    fn test_multiple_tables() {
        let cell1 = BoundingBox::new(0.0, 100.0, 100.0, 200.0);
        let cell2 = BoundingBox::new(300.0, 100.0, 400.0, 200.0);

        let mut tables = vec![
            Table {
                bbox: cell1,
                rows: vec![TableRow {
                    cells: vec![TableCell::new(String::new(), cell1)],
                    is_header: false,
                }],
                num_columns: 1,
                detection_method: TableDetectionMethod::RuledLine,
                column_positions: vec![0.0, 100.0],
                may_continue_from_previous: false,
                may_continue_to_next: false,
            },
            Table {
                bbox: cell2,
                rows: vec![TableRow {
                    cells: vec![TableCell::new(String::new(), cell2)],
                    is_header: false,
                }],
                num_columns: 1,
                detection_method: TableDetectionMethod::RuledLine,
                column_positions: vec![300.0, 400.0],
                may_continue_from_previous: false,
                may_continue_to_next: false,
            },
        ];

        let spans = vec![
            span("Table1", 20.0, 150.0, 40.0, 12.0),
            span("Table2", 320.0, 150.0, 40.0, 12.0),
        ];

        fill_table_text(&mut tables, &spans);

        assert_eq!(tables[0].rows[0].cells[0].text, "Table1");
        assert_eq!(tables[1].rows[0].cells[0].text, "Table2");
    }

    /// Empty span text is skipped.
    #[test]
    fn test_empty_span_text_skipped() {
        let cell_bbox = BoundingBox::new(0.0, 100.0, 200.0, 200.0);
        let mut tables = vec![Table {
            bbox: cell_bbox,
            rows: vec![TableRow {
                cells: vec![TableCell::new(String::new(), cell_bbox)],
                is_header: false,
            }],
            num_columns: 1,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![0.0, 200.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        }];

        let spans = vec![
            span("", 10.0, 170.0, 0.0, 12.0),      // empty, should be skipped
            span("Real", 50.0, 170.0, 40.0, 12.0), // actual content
        ];

        fill_table_text(&mut tables, &spans);

        assert_eq!(tables[0].rows[0].cells[0].text, "Real");
    }

    /// Zero font size uses fallback tolerance of 1.0.
    #[test]
    fn test_zero_font_size_fallback_tolerance() {
        let cell_bbox = BoundingBox::new(0.0, 100.0, 200.0, 200.0);
        let mut tables = vec![Table {
            bbox: cell_bbox,
            rows: vec![TableRow {
                cells: vec![TableCell::new(String::new(), cell_bbox)],
                is_header: false,
            }],
            num_columns: 1,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![0.0, 200.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        }];

        // Font size 0 would give tolerance 0, but we clamp to 1.0.
        // Spans 0.5 apart should be on the same line.
        let spans = vec![
            span("A", 10.0, 170.0, 20.0, 0.0),
            span("B", 50.0, 170.5, 20.0, 0.0),
        ];

        fill_table_text(&mut tables, &spans);

        assert_eq!(tables[0].rows[0].cells[0].text, "A B");
    }

    // -------------------------------------------------------------------
    // Additional tests (TE-009)
    // -------------------------------------------------------------------

    /// Helper to build a single-cell table.
    fn make_1x1_table(cell_bbox: BoundingBox) -> Table {
        Table {
            bbox: cell_bbox,
            rows: vec![TableRow {
                cells: vec![TableCell::new(String::new(), cell_bbox)],
                is_header: false,
            }],
            num_columns: 1,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![cell_bbox.x_min, cell_bbox.x_max],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        }
    }

    /// Cell with 10+ spans verifies they are all collected and ordered correctly.
    #[test]
    fn test_cell_with_many_spans() {
        let cell_bbox = BoundingBox::new(0.0, 0.0, 500.0, 200.0);
        let mut tables = vec![make_1x1_table(cell_bbox)];

        // 12 spans on two lines: first line at y=150, second at y=100.
        // 6 spans per line, placed left to right but inserted in scrambled order.
        let spans = vec![
            span("F", 250.0, 150.0, 20.0, 12.0),
            span("A", 10.0, 150.0, 20.0, 12.0),
            span("L", 250.0, 100.0, 20.0, 12.0),
            span("D", 150.0, 150.0, 20.0, 12.0),
            span("H", 50.0, 100.0, 20.0, 12.0),
            span("C", 100.0, 150.0, 20.0, 12.0),
            span("J", 150.0, 100.0, 20.0, 12.0),
            span("B", 50.0, 150.0, 20.0, 12.0),
            span("G", 10.0, 100.0, 20.0, 12.0),
            span("K", 200.0, 100.0, 20.0, 12.0),
            span("E", 200.0, 150.0, 20.0, 12.0),
            span("I", 100.0, 100.0, 20.0, 12.0),
        ];

        fill_table_text(&mut tables, &spans);

        // Line 1 (y=150, top): A B C D E F
        // Line 2 (y=100, bottom): G H I J K L
        assert_eq!(tables[0].rows[0].cells[0].text, "A B C D E F\nG H I J K L");
    }

    /// Span whose center falls exactly on a shared cell boundary is assigned
    /// to only one cell (first-match wins in iteration order).
    #[test]
    fn test_span_on_exact_cell_boundary() {
        let left = BoundingBox::new(0.0, 100.0, 100.0, 200.0);
        let right = BoundingBox::new(100.0, 100.0, 200.0, 200.0);
        let table_bbox = BoundingBox::new(0.0, 100.0, 200.0, 200.0);

        let mut tables = vec![Table {
            bbox: table_bbox,
            rows: vec![TableRow {
                cells: vec![
                    TableCell::new(String::new(), left),
                    TableCell::new(String::new(), right),
                ],
                is_header: false,
            }],
            num_columns: 2,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![0.0, 100.0, 200.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        }];

        // Span center at x=100.0 (the shared boundary). Width=0 so center_x = 100.0.
        // Both cells geometrically contain x=100.0, but the assigned-tracking
        // ensures the span goes to only the first matching cell (left).
        let spans = vec![span("Edge", 100.0, 150.0, 0.0, 12.0)];

        fill_table_text(&mut tables, &spans);

        // First-match wins: left cell claims it, right cell is empty.
        assert_eq!(tables[0].rows[0].cells[0].text, "Edge");
        assert!(tables[0].rows[0].cells[1].text.is_empty());
    }

    /// Tables with no text spans at all: all cells remain empty.
    #[test]
    fn test_tables_with_no_text_all_empty() {
        let tl = BoundingBox::new(0.0, 50.0, 100.0, 100.0);
        let tr = BoundingBox::new(100.0, 50.0, 200.0, 100.0);
        let bl = BoundingBox::new(0.0, 0.0, 100.0, 50.0);
        let br = BoundingBox::new(100.0, 0.0, 200.0, 50.0);
        let mut tables = vec![make_2x2_table(tl, tr, bl, br)];

        // Zero spans
        fill_table_text(&mut tables, &[]);

        for row in &tables[0].rows {
            for cell in &row.cells {
                assert_eq!(cell.text, "", "all cells should be empty with no spans");
            }
        }
    }

    /// End-to-end test: detect tables from paths, then fill with text spans.
    #[test]
    fn test_end_to_end_detect_then_fill() {
        use crate::diagnostics::NullDiagnostics;
        use crate::table::detect::detect_tables;
        use crate::table::types::{PathSegment, PathSegmentKind};

        let diag = NullDiagnostics;
        let page = BoundingBox::new(0.0, 0.0, 612.0, 792.0);

        // Build a 2x2 grid from line segments.
        let paths: Vec<PathSegment> = vec![
            PathSegment {
                kind: PathSegmentKind::Line {
                    x1: 100.0,
                    y1: 400.0,
                    x2: 300.0,
                    y2: 400.0,
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
            },
            PathSegment {
                kind: PathSegmentKind::Line {
                    x1: 100.0,
                    y1: 450.0,
                    x2: 300.0,
                    y2: 450.0,
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
            },
            PathSegment {
                kind: PathSegmentKind::Line {
                    x1: 100.0,
                    y1: 500.0,
                    x2: 300.0,
                    y2: 500.0,
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
            },
            PathSegment {
                kind: PathSegmentKind::Line {
                    x1: 100.0,
                    y1: 400.0,
                    x2: 100.0,
                    y2: 500.0,
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
            },
            PathSegment {
                kind: PathSegmentKind::Line {
                    x1: 200.0,
                    y1: 400.0,
                    x2: 200.0,
                    y2: 500.0,
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
            },
            PathSegment {
                kind: PathSegmentKind::Line {
                    x1: 300.0,
                    y1: 400.0,
                    x2: 300.0,
                    y2: 500.0,
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
            },
        ];

        let mut tables = detect_tables(&paths, &page, &diag);
        assert_eq!(tables.len(), 1, "should detect 1 table");
        assert_eq!(tables[0].num_columns, 2);
        assert_eq!(tables[0].rows.len(), 2);

        // Place text in each cell.
        let spans = vec![
            span("Name", 120.0, 480.0, 40.0, 12.0),  // top-left
            span("Age", 220.0, 480.0, 30.0, 12.0),   // top-right
            span("Alice", 120.0, 420.0, 40.0, 12.0), // bottom-left
            span("30", 220.0, 420.0, 20.0, 12.0),    // bottom-right
        ];

        fill_table_text(&mut tables, &spans);

        assert_eq!(tables[0].rows[0].cells[0].text, "Name");
        assert_eq!(tables[0].rows[0].cells[1].text, "Age");
        assert_eq!(tables[0].rows[1].cells[0].text, "Alice");
        assert_eq!(tables[0].rows[1].cells[1].text, "30");
    }

    /// Two tables on the same page, each receiving its own text.
    #[test]
    fn test_multi_table_text_assignment() {
        use crate::diagnostics::NullDiagnostics;
        use crate::table::detect::detect_tables;
        use crate::table::types::{PathSegment, PathSegmentKind};

        let diag = NullDiagnostics;
        let page = BoundingBox::new(0.0, 0.0, 612.0, 792.0);

        let make_line_seg = |x1: f64, y1: f64, x2: f64, y2: f64| PathSegment {
            kind: PathSegmentKind::Line { x1, y1, x2, y2 },
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

        // Table 1 at top (y=500..600, x=50..200)
        let mut paths = vec![
            make_line_seg(50.0, 500.0, 200.0, 500.0),
            make_line_seg(50.0, 600.0, 200.0, 600.0),
            make_line_seg(50.0, 500.0, 50.0, 600.0),
            make_line_seg(200.0, 500.0, 200.0, 600.0),
        ];
        // Table 2 at bottom (y=100..200, x=300..500)
        paths.extend(vec![
            make_line_seg(300.0, 100.0, 500.0, 100.0),
            make_line_seg(300.0, 200.0, 500.0, 200.0),
            make_line_seg(300.0, 100.0, 300.0, 200.0),
            make_line_seg(500.0, 100.0, 500.0, 200.0),
        ]);

        let mut tables = detect_tables(&paths, &page, &diag);
        assert_eq!(tables.len(), 2, "should detect 2 separate tables");

        // Spans for table 1
        let spans = vec![
            span("Top", 80.0, 550.0, 30.0, 12.0),
            span("Bottom", 350.0, 150.0, 50.0, 12.0),
            span("Outside", 400.0, 400.0, 40.0, 12.0), // not in any table
        ];

        fill_table_text(&mut tables, &spans);

        // One table should have "Top", the other "Bottom", and "Outside"
        // should not appear in either. We check by collecting all cell texts.
        let all_texts: Vec<&str> = tables
            .iter()
            .flat_map(|t| t.rows.iter())
            .flat_map(|r| r.cells.iter())
            .map(|c| c.text.as_str())
            .collect();
        assert!(
            all_texts.contains(&"Top"),
            "Top span should be in one table"
        );
        assert!(
            all_texts.contains(&"Bottom"),
            "Bottom span should be in one table"
        );
        assert!(
            !all_texts.contains(&"Outside"),
            "Outside span should not be in any table"
        );
    }

    /// Wide span straddling two cells: assigned based on center point.
    #[test]
    fn test_wide_span_assigned_by_center() {
        let left = BoundingBox::new(0.0, 100.0, 100.0, 200.0);
        let right = BoundingBox::new(100.0, 100.0, 200.0, 200.0);
        let table_bbox = BoundingBox::new(0.0, 100.0, 200.0, 200.0);

        let mut tables = vec![Table {
            bbox: table_bbox,
            rows: vec![TableRow {
                cells: vec![
                    TableCell::new(String::new(), left),
                    TableCell::new(String::new(), right),
                ],
                is_header: false,
            }],
            num_columns: 2,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![0.0, 100.0, 200.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        }];

        // Span starts at x=70, width=80 (so it extends from x=70 to x=150).
        // Center: x = 70 + 80/2 = 110, which is in the RIGHT cell.
        let spans = vec![span("Wide", 70.0, 150.0, 80.0, 12.0)];

        fill_table_text(&mut tables, &spans);

        assert_eq!(
            tables[0].rows[0].cells[0].text, "",
            "left cell should be empty"
        );
        assert_eq!(
            tables[0].rows[0].cells[1].text, "Wide",
            "right cell gets the span based on center"
        );
    }

    /// Three lines of text in one cell, verifying multi-line ordering.
    #[test]
    fn test_three_lines_in_cell() {
        let cell_bbox = BoundingBox::new(0.0, 0.0, 300.0, 300.0);
        let mut tables = vec![make_1x1_table(cell_bbox)];

        // Three distinct baselines (font_size=12, tolerance=6).
        // y=250, y=200, y=150 are 50 apart, so 3 separate lines.
        let spans = vec![
            span("Line3", 10.0, 150.0, 40.0, 12.0), // bottom
            span("Line1", 10.0, 250.0, 40.0, 12.0), // top
            span("Line2", 10.0, 200.0, 40.0, 12.0), // middle
        ];

        fill_table_text(&mut tables, &spans);

        assert_eq!(
            tables[0].rows[0].cells[0].text, "Line1\nLine2\nLine3",
            "three lines should be ordered top to bottom"
        );
    }

    // -----------------------------------------------------------------------
    // Header detection tests (TE-006)
    // -----------------------------------------------------------------------

    fn bold_span(text: &str, x: f64, y: f64, width: f64, font_size: f64) -> TextSpan {
        TextSpan::new(
            text.to_string(),
            x,
            y,
            width,
            "Helvetica-Bold".to_string(),
            font_size,
        )
    }

    /// First row all bold -> marked as header.
    #[test]
    fn test_header_detection_bold_first_row() {
        use super::detect_header_rows;

        let tl = BoundingBox::new(0.0, 150.0, 100.0, 200.0);
        let tr = BoundingBox::new(100.0, 150.0, 200.0, 200.0);
        let bl = BoundingBox::new(0.0, 100.0, 100.0, 150.0);
        let br = BoundingBox::new(100.0, 100.0, 200.0, 150.0);
        let mut tables = vec![make_2x2_table(tl, tr, bl, br)];

        let spans = vec![
            bold_span("Name", 20.0, 170.0, 40.0, 12.0), // top-left, bold
            bold_span("Age", 120.0, 170.0, 30.0, 12.0), // top-right, bold
            span("Alice", 20.0, 120.0, 40.0, 12.0),     // bottom-left, regular
            span("30", 120.0, 120.0, 20.0, 12.0),       // bottom-right, regular
        ];

        fill_table_text(&mut tables, &spans);
        detect_header_rows(&mut tables, &spans);

        assert!(tables[0].rows[0].is_header, "first row should be header");
        assert!(
            !tables[0].rows[1].is_header,
            "second row should not be header"
        );
    }

    /// First row not bold -> not marked as header.
    #[test]
    fn test_header_detection_no_bold() {
        use super::detect_header_rows;

        let tl = BoundingBox::new(0.0, 150.0, 100.0, 200.0);
        let tr = BoundingBox::new(100.0, 150.0, 200.0, 200.0);
        let bl = BoundingBox::new(0.0, 100.0, 100.0, 150.0);
        let br = BoundingBox::new(100.0, 100.0, 200.0, 150.0);
        let mut tables = vec![make_2x2_table(tl, tr, bl, br)];

        let spans = vec![
            span("Name", 20.0, 170.0, 40.0, 12.0),
            span("Age", 120.0, 170.0, 30.0, 12.0),
            span("Alice", 20.0, 120.0, 40.0, 12.0),
            span("30", 120.0, 120.0, 20.0, 12.0),
        ];

        fill_table_text(&mut tables, &spans);
        detect_header_rows(&mut tables, &spans);

        assert!(
            !tables[0].rows[0].is_header,
            "first row should not be header"
        );
    }

    /// Mixed bold/regular in first row -> not a header.
    #[test]
    fn test_header_detection_mixed_fonts() {
        use super::detect_header_rows;

        let tl = BoundingBox::new(0.0, 150.0, 100.0, 200.0);
        let tr = BoundingBox::new(100.0, 150.0, 200.0, 200.0);
        let bl = BoundingBox::new(0.0, 100.0, 100.0, 150.0);
        let br = BoundingBox::new(100.0, 100.0, 200.0, 150.0);
        let mut tables = vec![make_2x2_table(tl, tr, bl, br)];

        let spans = vec![
            bold_span("Name", 20.0, 170.0, 40.0, 12.0), // bold
            span("Age", 120.0, 170.0, 30.0, 12.0),      // regular
            span("Alice", 20.0, 120.0, 40.0, 12.0),
            span("30", 120.0, 120.0, 20.0, 12.0),
        ];

        fill_table_text(&mut tables, &spans);
        detect_header_rows(&mut tables, &spans);

        assert!(
            !tables[0].rows[0].is_header,
            "mixed fonts should not be header"
        );
    }

    // -----------------------------------------------------------------------
    // split_mega_rows tests
    // -----------------------------------------------------------------------

    /// Helper: build a 1-row table with num_cols columns spanning the given bbox.
    fn make_mega_row_table(
        num_cols: usize,
        x_min: f64,
        x_max: f64,
        y_min: f64,
        y_max: f64,
    ) -> Table {
        let col_width = (x_max - x_min) / num_cols as f64;
        let cells: Vec<TableCell> = (0..num_cols)
            .map(|i| {
                let cx_min = x_min + i as f64 * col_width;
                let cx_max = cx_min + col_width;
                TableCell::new(
                    String::new(),
                    BoundingBox::new(cx_min, y_min, cx_max, y_max),
                )
            })
            .collect();
        Table {
            bbox: BoundingBox::new(x_min, y_min, x_max, y_max),
            rows: vec![TableRow {
                cells,
                is_header: false,
            }],
            num_columns: num_cols,
            detection_method: TableDetectionMethod::HLine,
            column_positions: Vec::new(),
            may_continue_from_previous: false,
            may_continue_to_next: false,
        }
    }

    /// Mega-row with 3 baselines splits into 3 sub-rows.
    #[test]
    fn test_split_mega_rows_three_baselines() {
        // One mega-row: 2 cols, Y range 100..400
        let mut tables = vec![make_mega_row_table(2, 0.0, 200.0, 100.0, 400.0)];

        // 3 baselines at y=350, y=250, y=150 with font_size=12
        let spans = vec![
            span("A", 20.0, 350.0, 30.0, 12.0),
            span("B", 120.0, 350.0, 30.0, 12.0),
            span("C", 20.0, 250.0, 30.0, 12.0),
            span("D", 120.0, 250.0, 30.0, 12.0),
            span("E", 20.0, 150.0, 30.0, 12.0),
            span("F", 120.0, 150.0, 30.0, 12.0),
        ];

        split_mega_rows(&mut tables, &spans);

        assert_eq!(tables[0].rows.len(), 3, "should split into 3 sub-rows");
        assert_eq!(tables[0].rows[0].cells.len(), 2);
        assert_eq!(tables[0].rows[1].cells.len(), 2);
        assert_eq!(tables[0].rows[2].cells.len(), 2);

        // Now fill text to verify cell bbox correctness.
        fill_table_text(&mut tables, &spans);
        assert_eq!(tables[0].rows[0].cells[0].text, "A");
        assert_eq!(tables[0].rows[0].cells[1].text, "B");
        assert_eq!(tables[0].rows[1].cells[0].text, "C");
        assert_eq!(tables[0].rows[1].cells[1].text, "D");
        assert_eq!(tables[0].rows[2].cells[0].text, "E");
        assert_eq!(tables[0].rows[2].cells[1].text, "F");
    }

    /// Single baseline: no splitting occurs.
    #[test]
    fn test_split_mega_rows_single_baseline_no_split() {
        let mut tables = vec![make_mega_row_table(2, 0.0, 200.0, 100.0, 200.0)];
        let spans = vec![
            span("A", 20.0, 150.0, 30.0, 12.0),
            span("B", 120.0, 150.0, 30.0, 12.0),
        ];

        split_mega_rows(&mut tables, &spans);

        assert_eq!(tables[0].rows.len(), 1, "single baseline should not split");
    }

    /// No spans in row: no splitting occurs.
    #[test]
    fn test_split_mega_rows_no_spans() {
        let mut tables = vec![make_mega_row_table(2, 0.0, 200.0, 100.0, 200.0)];
        let spans: Vec<TextSpan> = Vec::new();

        split_mega_rows(&mut tables, &spans);

        assert_eq!(tables[0].rows.len(), 1, "no spans should not split");
    }

    /// Table with 2 existing rows where only one is a mega-row.
    #[test]
    fn test_split_mega_rows_preserves_normal_rows() {
        // 2-row table: top row is normal (narrow Y), bottom row is a mega-row (wide Y).
        let table_bbox = BoundingBox::new(0.0, 100.0, 200.0, 500.0);
        let mut tables = vec![Table {
            bbox: table_bbox,
            rows: vec![
                // Normal row (y=400..500)
                TableRow {
                    cells: vec![
                        TableCell::new(String::new(), BoundingBox::new(0.0, 400.0, 100.0, 500.0)),
                        TableCell::new(String::new(), BoundingBox::new(100.0, 400.0, 200.0, 500.0)),
                    ],
                    is_header: false,
                },
                // Mega-row (y=100..400) with 2 baselines
                TableRow {
                    cells: vec![
                        TableCell::new(String::new(), BoundingBox::new(0.0, 100.0, 100.0, 400.0)),
                        TableCell::new(String::new(), BoundingBox::new(100.0, 100.0, 200.0, 400.0)),
                    ],
                    is_header: false,
                },
            ],
            num_columns: 2,
            detection_method: TableDetectionMethod::HLine,
            column_positions: Vec::new(),
            may_continue_from_previous: false,
            may_continue_to_next: false,
        }];

        let spans = vec![
            // Normal row text
            span("H1", 20.0, 450.0, 30.0, 12.0),
            span("H2", 120.0, 450.0, 30.0, 12.0),
            // Mega-row: 2 baselines at y=300 and y=200
            span("R1C1", 20.0, 300.0, 40.0, 12.0),
            span("R1C2", 120.0, 300.0, 40.0, 12.0),
            span("R2C1", 20.0, 200.0, 40.0, 12.0),
            span("R2C2", 120.0, 200.0, 40.0, 12.0),
        ];

        split_mega_rows(&mut tables, &spans);

        assert_eq!(
            tables[0].rows.len(),
            3,
            "normal row + split mega-row = 3 rows"
        );

        fill_table_text(&mut tables, &spans);
        assert_eq!(tables[0].rows[0].cells[0].text, "H1");
        assert_eq!(tables[0].rows[0].cells[1].text, "H2");
        assert_eq!(tables[0].rows[1].cells[0].text, "R1C1");
        assert_eq!(tables[0].rows[1].cells[1].text, "R1C2");
        assert_eq!(tables[0].rows[2].cells[0].text, "R2C1");
        assert_eq!(tables[0].rows[2].cells[1].text, "R2C2");
    }

    /// Baselines close together (within tolerance) are not split.
    #[test]
    fn test_split_mega_rows_close_baselines_no_split() {
        let mut tables = vec![make_mega_row_table(2, 0.0, 200.0, 100.0, 200.0)];
        // Two baselines 3 points apart with font_size=12 (threshold = 12*0.6 = 7.2)
        let spans = vec![
            span("A", 20.0, 153.0, 30.0, 12.0),
            span("B", 120.0, 150.0, 30.0, 12.0),
        ];

        split_mega_rows(&mut tables, &spans);

        assert_eq!(tables[0].rows.len(), 1, "close baselines should not split");
    }

    // -------------------------------------------------------------------
    // eliminate_empty_rows tests
    // -------------------------------------------------------------------

    #[test]
    fn test_eliminate_empty_rows_removes_all_empty() {
        let bb = BoundingBox::new(0.0, 0.0, 200.0, 300.0);
        let mut tables = vec![Table {
            bbox: bb,
            rows: vec![
                TableRow {
                    cells: vec![TableCell::new(
                        String::new(),
                        BoundingBox::new(0.0, 200.0, 200.0, 300.0),
                    )],
                    is_header: false,
                },
                TableRow {
                    cells: vec![TableCell::new(
                        "data".to_string(),
                        BoundingBox::new(0.0, 100.0, 200.0, 200.0),
                    )],
                    is_header: false,
                },
                TableRow {
                    cells: vec![TableCell::new(
                        String::new(),
                        BoundingBox::new(0.0, 0.0, 200.0, 100.0),
                    )],
                    is_header: false,
                },
            ],
            num_columns: 1,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![0.0, 200.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        }];

        eliminate_empty_rows(&mut tables);

        assert_eq!(tables[0].rows.len(), 1);
        assert_eq!(tables[0].rows[0].cells[0].text, "data");
        // Bbox should shrink to fit remaining row.
        assert!((tables[0].bbox.y_min - 100.0).abs() < 0.1);
        assert!((tables[0].bbox.y_max - 200.0).abs() < 0.1);
    }

    #[test]
    fn test_eliminate_empty_rows_keeps_nonempty() {
        let bb = BoundingBox::new(0.0, 0.0, 200.0, 200.0);
        let mut tables = vec![Table {
            bbox: bb,
            rows: vec![
                TableRow {
                    cells: vec![TableCell::new(
                        "A".to_string(),
                        BoundingBox::new(0.0, 100.0, 200.0, 200.0),
                    )],
                    is_header: false,
                },
                TableRow {
                    cells: vec![TableCell::new(
                        "B".to_string(),
                        BoundingBox::new(0.0, 0.0, 200.0, 100.0),
                    )],
                    is_header: false,
                },
            ],
            num_columns: 1,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![0.0, 200.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        }];

        eliminate_empty_rows(&mut tables);

        assert_eq!(tables[0].rows.len(), 2);
    }

    #[test]
    fn test_eliminate_empty_rows_whitespace_only() {
        let bb = BoundingBox::new(0.0, 0.0, 200.0, 200.0);
        let mut tables = vec![Table {
            bbox: bb,
            rows: vec![
                TableRow {
                    cells: vec![TableCell::new(
                        "  \t ".to_string(),
                        BoundingBox::new(0.0, 100.0, 200.0, 200.0),
                    )],
                    is_header: false,
                },
                TableRow {
                    cells: vec![TableCell::new(
                        "real".to_string(),
                        BoundingBox::new(0.0, 0.0, 200.0, 100.0),
                    )],
                    is_header: false,
                },
            ],
            num_columns: 1,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![0.0, 200.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        }];

        eliminate_empty_rows(&mut tables);

        assert_eq!(tables[0].rows.len(), 1);
        assert_eq!(tables[0].rows[0].cells[0].text, "real");
    }

    #[test]
    fn test_eliminate_empty_columns_basic() {
        // 2 rows x 3 cols, middle column always empty.
        let mut tables = vec![Table {
            bbox: BoundingBox::new(0.0, 0.0, 300.0, 200.0),
            rows: vec![
                TableRow {
                    cells: vec![
                        TableCell::new("A".to_string(), BoundingBox::new(0.0, 100.0, 100.0, 200.0)),
                        TableCell::new(String::new(), BoundingBox::new(100.0, 100.0, 110.0, 200.0)),
                        TableCell::new(
                            "B".to_string(),
                            BoundingBox::new(110.0, 100.0, 300.0, 200.0),
                        ),
                    ],
                    is_header: false,
                },
                TableRow {
                    cells: vec![
                        TableCell::new("C".to_string(), BoundingBox::new(0.0, 0.0, 100.0, 100.0)),
                        TableCell::new(String::new(), BoundingBox::new(100.0, 0.0, 110.0, 100.0)),
                        TableCell::new("D".to_string(), BoundingBox::new(110.0, 0.0, 300.0, 100.0)),
                    ],
                    is_header: false,
                },
            ],
            num_columns: 3,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![0.0, 100.0, 110.0, 300.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        }];

        eliminate_empty_columns(&mut tables);

        assert_eq!(tables[0].num_columns, 2);
        assert_eq!(tables[0].rows[0].cells.len(), 2);
        assert_eq!(tables[0].rows[0].cells[0].text, "A");
        assert_eq!(tables[0].rows[0].cells[1].text, "B");
        assert_eq!(tables[0].rows[1].cells[0].text, "C");
        assert_eq!(tables[0].rows[1].cells[1].text, "D");
        // Empty column's space absorbed by adjacent cell.
        assert!((tables[0].rows[0].cells[0].bbox.x_max - 110.0).abs() < 0.01);
    }

    #[test]
    fn test_eliminate_empty_columns_no_empty() {
        let mut tables = vec![Table {
            bbox: BoundingBox::new(0.0, 0.0, 200.0, 100.0),
            rows: vec![TableRow {
                cells: vec![
                    TableCell::new("A".to_string(), BoundingBox::new(0.0, 0.0, 100.0, 100.0)),
                    TableCell::new("B".to_string(), BoundingBox::new(100.0, 0.0, 200.0, 100.0)),
                ],
                is_header: false,
            }],
            num_columns: 2,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![0.0, 100.0, 200.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        }];

        eliminate_empty_columns(&mut tables);

        assert_eq!(tables[0].num_columns, 2);
        assert_eq!(tables[0].rows[0].cells.len(), 2);
    }

    // -----------------------------------------------------------------------
    // compute_table_quality tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_quality_empty_table() {
        let table = Table {
            bbox: BoundingBox::new(0.0, 0.0, 100.0, 100.0),
            rows: vec![],
            num_columns: 0,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        };
        let q = compute_table_quality(&table);
        assert_eq!(q.cell_occupancy, 0.0);
        assert_eq!(q.multi_column_row_fraction, 0.0);
        assert_eq!(q.data_row_count, 0);
        assert_eq!(q.non_empty_cells, 0);
    }

    #[test]
    fn test_quality_all_empty_cells() {
        let cell = BoundingBox::new(0.0, 0.0, 50.0, 50.0);
        let table = Table {
            bbox: BoundingBox::new(0.0, 0.0, 100.0, 100.0),
            rows: vec![
                TableRow {
                    cells: vec![
                        TableCell::new(String::new(), cell),
                        TableCell::new(String::new(), cell),
                    ],
                    is_header: false,
                },
                TableRow {
                    cells: vec![
                        TableCell::new(String::new(), cell),
                        TableCell::new(String::new(), cell),
                    ],
                    is_header: false,
                },
            ],
            num_columns: 2,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![0.0, 50.0, 100.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        };
        let q = compute_table_quality(&table);
        assert_eq!(q.cell_occupancy, 0.0);
        assert_eq!(q.non_empty_cells, 0);
        assert_eq!(q.data_row_count, 2);
    }

    #[test]
    fn test_quality_single_cell_with_text() {
        let cell = BoundingBox::new(0.0, 0.0, 100.0, 50.0);
        let table = Table {
            bbox: BoundingBox::new(0.0, 0.0, 100.0, 50.0),
            rows: vec![TableRow {
                cells: vec![TableCell::new("hello".to_string(), cell)],
                is_header: false,
            }],
            num_columns: 1,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![0.0, 100.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        };
        let q = compute_table_quality(&table);
        assert_eq!(q.cell_occupancy, 1.0);
        assert_eq!(q.non_empty_cells, 1);
        assert_eq!(q.data_row_count, 1);
        // Single column, so no row has 2+ filled columns.
        assert_eq!(q.multi_column_row_fraction, 0.0);
    }

    #[test]
    fn test_quality_mixed_occupancy() {
        let cell = BoundingBox::new(0.0, 0.0, 50.0, 50.0);
        let table = Table {
            bbox: BoundingBox::new(0.0, 0.0, 100.0, 100.0),
            rows: vec![
                TableRow {
                    cells: vec![
                        TableCell::new("A".to_string(), cell),
                        TableCell::new("B".to_string(), cell),
                    ],
                    is_header: true,
                },
                TableRow {
                    cells: vec![
                        TableCell::new("C".to_string(), cell),
                        TableCell::new(String::new(), cell),
                    ],
                    is_header: false,
                },
            ],
            num_columns: 2,
            detection_method: TableDetectionMethod::RuledLine,
            column_positions: vec![0.0, 50.0, 100.0],
            may_continue_from_previous: false,
            may_continue_to_next: false,
        };
        let q = compute_table_quality(&table);
        // 3 of 4 cells have text.
        assert!((q.cell_occupancy - 0.75).abs() < 1e-10);
        assert_eq!(q.non_empty_cells, 3);
        // 1 data row (the non-header one).
        assert_eq!(q.data_row_count, 1);
        // Row 0 has 2 filled columns, row 1 has 1. So 1/2 = 0.5.
        assert!((q.multi_column_row_fraction - 0.5).abs() < 1e-10);
    }
}

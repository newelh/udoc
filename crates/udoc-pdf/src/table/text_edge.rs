//! Nurminen text-edge column detection for tables.
//!
//! Implements column boundary detection based on the text-edge algorithm from:
//!
//!   Nurminen, A. (2013). "Algorithmic Extraction of Data in Tables in
//!   PDF Documents." Master's thesis, Tampere University of Technology.
//!   <https://trepo.tuni.fi/bitstream/handle/123456789/21520/Nurminen.pdf>
//!
//! The key insight: table columns create **persistent vertical text edges**
//! (left, right, or center x-positions) that recur across multiple rows.
//! A text chunk whose bounding box straddles an edge position eliminates
//! that edge as a column boundary (it must be interior to a cell).
//!
//! This algorithm is the consensus approach for borderless/h-line-only
//! table column detection, used (with variations) by Tabula, pdfplumber,
//! and Camelot.

use std::collections::BTreeMap;

use crate::geometry::BoundingBox;
use crate::text::types::TextSpan;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default minimum number of distinct text rows an edge must span to be
/// considered a valid column boundary. Matches Tabula-java's
/// REQUIRED_TEXT_LINES_FOR_EDGE. Callers can override via
/// `detect_columns_text_edge_with_support`.
const DEFAULT_MIN_ROW_SUPPORT: usize = 4;

/// Snap tolerance (in points) for merging nearby edge positions.
const EDGE_SNAP_TOLERANCE: f64 = 3.0;

/// Minimum distance from a chunk's midpoint before a MID edge can be
/// considered interrupted. Prevents a chunk from killing its own mid edge.
const MIN_MID_INTERRUPT_DISTANCE: i32 = 2;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Which alignment edge of a text chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeType {
    Left,
    Right,
    Mid,
}

/// A finalized text edge that survived interruption checks.
#[derive(Debug)]
struct FinalizedEdge {
    x: i32,
    row_count: usize,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Detect column boundary x-positions using the Nurminen text-edge algorithm.
///
/// Takes word-level spans (pre-merged by `merge_spans_for_table`) and a
/// bounding box to constrain detection. Returns sorted column left-edge
/// x-positions, or an empty vec if no valid columns are found.
///
/// The algorithm:
/// 1. Filters spans to those within `region_bbox`
/// 2. Groups spans into text rows by Y proximity
/// 3. Tracks LEFT/RIGHT/MID edge accumulators keyed by snapped x-coordinate
/// 4. Eliminates "interrupted" edges (where a chunk bbox straddles the edge)
/// 5. Selects the edge type with the strongest signal
/// 6. Snaps nearby surviving edges together
pub fn detect_columns_text_edge(spans: &[TextSpan], region_bbox: &BoundingBox) -> Vec<f64> {
    detect_columns_text_edge_with_support(spans, region_bbox, None)
}

/// Like `detect_columns_text_edge` but with an explicit minimum row support
/// override. Use when detecting columns within a known table region (e.g.,
/// bounded by h-lines) where a lower threshold is safe.
pub fn detect_columns_text_edge_with_support(
    spans: &[TextSpan],
    region_bbox: &BoundingBox,
    min_support_override: Option<usize>,
) -> Vec<f64> {
    // Step 1: Filter spans to those within the region bbox.
    let filtered: Vec<&TextSpan> = spans
        .iter()
        .filter(|s| {
            s.x >= region_bbox.x_min - 1.0
                && s.x + s.width <= region_bbox.x_max + 1.0
                && s.y >= region_bbox.y_min - 1.0
                && s.y <= region_bbox.y_max + 1.0
        })
        .collect();

    if filtered.len() < 4 {
        return Vec::new();
    }

    // Step 2: Group spans into rows by Y proximity.
    let rows = group_into_rows(&filtered);
    if rows.len() < 2 {
        return Vec::new();
    }

    // Row support threshold. When an explicit override is given (e.g., by
    // the h-line detector for a known table region), use it. Otherwise,
    // default to 4 (Tabula's standard for full-page detection).
    let min_row_support = min_support_override.unwrap_or(DEFAULT_MIN_ROW_SUPPORT);

    // Steps 3-4: Build edge accumulators with interruption elimination.
    let (finalized_left, finalized_right, finalized_mid) =
        build_and_eliminate_edges(&rows, min_row_support);

    // Step 5: Select the relevant edge type.
    let selected = select_relevant_edges(&finalized_left, &finalized_right, &finalized_mid);

    if selected.is_empty() {
        return Vec::new();
    }

    // Step 6: Snap nearby edges together.
    snap_edges(selected)
}

// ---------------------------------------------------------------------------
// Step 2: Row grouping
// ---------------------------------------------------------------------------

/// Group spans into rows by Y proximity. Returns rows sorted top-to-bottom
/// (descending Y in PDF coords), each row's spans sorted left-to-right.
fn group_into_rows<'a>(spans: &[&'a TextSpan]) -> Vec<Vec<&'a TextSpan>> {
    if spans.is_empty() {
        return Vec::new();
    }

    // Sort by Y descending (top first), then X ascending.
    let mut sorted: Vec<&TextSpan> = spans.to_vec();
    sorted.sort_by(|a, b| b.y.total_cmp(&a.y).then(a.x.total_cmp(&b.x)));

    let mut rows: Vec<Vec<&'a TextSpan>> = Vec::new();
    let mut current_row: Vec<&TextSpan> = vec![sorted[0]];
    let mut row_y = sorted[0].y;

    for &span in &sorted[1..] {
        let tolerance = (span.font_size * 0.3).max(1.5);
        if (row_y - span.y).abs() <= tolerance {
            current_row.push(span);
        } else {
            rows.push(current_row);
            current_row = vec![span];
            row_y = span.y;
        }
    }
    if !current_row.is_empty() {
        rows.push(current_row);
    }

    rows
}

// ---------------------------------------------------------------------------
// Steps 3-4: Edge accumulation with interruption elimination
// ---------------------------------------------------------------------------

/// Process all rows, building edge accumulators and eliminating interrupted edges.
///
/// An edge at position `key` is "interrupted" by a text chunk if the chunk's
/// bounding box spans across `key` (i.e., `chunk.left < key < chunk.right`).
/// This means the edge falls inside a cell, not at a column boundary.
fn build_and_eliminate_edges(
    rows: &[Vec<&TextSpan>],
    min_row_support: usize,
) -> (Vec<FinalizedEdge>, Vec<FinalizedEdge>, Vec<FinalizedEdge>) {
    // Active edge accumulators: x-coord -> set of row indices that contributed.
    let mut active_left: BTreeMap<i32, Vec<usize>> = BTreeMap::new();
    let mut active_right: BTreeMap<i32, Vec<usize>> = BTreeMap::new();
    let mut active_mid: BTreeMap<i32, Vec<usize>> = BTreeMap::new();

    // Finalized edges (survived interruption with enough row support).
    let mut final_left: Vec<FinalizedEdge> = Vec::new();
    let mut final_right: Vec<FinalizedEdge> = Vec::new();
    let mut final_mid: Vec<FinalizedEdge> = Vec::new();

    for (row_idx, row) in rows.iter().enumerate() {
        for span in row {
            let left = span.x.floor() as i32;
            let right = (span.x + span.width).floor() as i32;
            let mid = left + (right - left) / 2;

            // Add this span's edges to the active accumulators.
            add_to_accumulator(&mut active_left, left, row_idx);
            add_to_accumulator(&mut active_right, right, row_idx);
            add_to_accumulator(&mut active_mid, mid, row_idx);

            // Eliminate interrupted LEFT edges.
            eliminate_interrupted(
                &mut active_left,
                &mut final_left,
                left,
                right,
                None,
                min_row_support,
            );

            // Eliminate interrupted RIGHT edges.
            eliminate_interrupted(
                &mut active_right,
                &mut final_right,
                left,
                right,
                None,
                min_row_support,
            );

            // Eliminate interrupted MID edges (with extra distance check).
            eliminate_interrupted(
                &mut active_mid,
                &mut final_mid,
                left,
                right,
                Some(mid),
                min_row_support,
            );
        }
    }

    // Finalize remaining active edges.
    finalize_remaining(&active_left, &mut final_left, min_row_support);
    finalize_remaining(&active_right, &mut final_right, min_row_support);
    finalize_remaining(&active_mid, &mut final_mid, min_row_support);

    (final_left, final_right, final_mid)
}

/// Add a row index to an edge accumulator, deduplicating within the same row.
fn add_to_accumulator(acc: &mut BTreeMap<i32, Vec<usize>>, key: i32, row_idx: usize) {
    let entry = acc.entry(key).or_default();
    if entry.last() != Some(&row_idx) {
        entry.push(row_idx);
    }
}

/// Check all active edges and eliminate those interrupted by the span [left, right].
///
/// For MID edges, `own_mid` is the current chunk's midpoint; edges within
/// MIN_MID_INTERRUPT_DISTANCE of it are not interrupted (to prevent a chunk
/// from killing its own mid edge).
fn eliminate_interrupted(
    active: &mut BTreeMap<i32, Vec<usize>>,
    finalized: &mut Vec<FinalizedEdge>,
    left: i32,
    right: i32,
    own_mid: Option<i32>,
    min_row_support: usize,
) {
    if left + 1 >= right {
        return;
    }
    // Use BTreeMap range query to find only keys in (left, right).
    let to_remove: Vec<i32> = active
        .range((left + 1)..right)
        .filter_map(|(&key, _rows)| {
            if let Some(mid) = own_mid {
                if (key - mid).abs() <= MIN_MID_INTERRUPT_DISTANCE {
                    return None;
                }
            }
            Some(key)
        })
        .collect();

    for key in to_remove {
        if let Some(rows) = active.remove(&key) {
            if rows.len() >= min_row_support {
                finalized.push(FinalizedEdge {
                    x: key,
                    row_count: rows.len(),
                });
            }
        }
    }
}

/// Move all remaining active edges with sufficient row support to finalized.
fn finalize_remaining(
    active: &BTreeMap<i32, Vec<usize>>,
    finalized: &mut Vec<FinalizedEdge>,
    min_row_support: usize,
) {
    for (&key, rows) in active {
        if rows.len() >= min_row_support {
            finalized.push(FinalizedEdge {
                x: key,
                row_count: rows.len(),
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Step 5: Select relevant edge type
// ---------------------------------------------------------------------------

/// Select the edge type with the strongest signal.
///
/// Priority: LEFT (preferred on ties) > RIGHT > MID (all need 2+ edges).
/// Among candidates, prefer the type where the maximum row_count is highest.
fn select_relevant_edges(
    left: &[FinalizedEdge],
    right: &[FinalizedEdge],
    mid: &[FinalizedEdge],
) -> Vec<f64> {
    // Build per-type summaries: (edge_count, max_row_count, edges as f64 positions).
    let left_info = edge_summary(left);
    let right_info = edge_summary(right);
    let mid_info = edge_summary(mid);

    // All edge types need 2+ edges. LEFT is preferred when scores tie
    // (most tables are left-aligned). Tabula uses 3+ for LEFT in table
    // *detection* on full pages, but we're detecting columns within an
    // already-known table region, so 2+ is sufficient.
    let candidates: Vec<(EdgeType, usize, usize, Vec<f64>)> = vec![
        (EdgeType::Left, left_info.0, left_info.1, left_info.2),
        (EdgeType::Right, right_info.0, right_info.1, right_info.2),
        (EdgeType::Mid, mid_info.0, mid_info.1, mid_info.2),
    ];

    let mut best: Option<(EdgeType, Vec<f64>)> = None;
    let mut best_score: (usize, usize) = (0, 0); // (max_row_count, edge_count)

    for (etype, count, max_rows, positions) in &candidates {
        if *count < 2 {
            continue;
        }
        // Score: (max_row_count, priority_bonus). LEFT gets a small bonus for ties.
        let priority_bonus = if *etype == EdgeType::Left { 1 } else { 0 };
        let score = (*max_rows, *count + priority_bonus);
        if score > best_score {
            best_score = score;
            best = Some((*etype, positions.clone()));
        }
    }

    best.map(|(_, positions)| positions).unwrap_or_default()
}

/// Summarize edges: (count, max_row_count, sorted x-positions).
fn edge_summary(edges: &[FinalizedEdge]) -> (usize, usize, Vec<f64>) {
    let count = edges.len();
    let max_rows = edges.iter().map(|e| e.row_count).max().unwrap_or(0);
    let mut positions: Vec<f64> = edges.iter().map(|e| e.x as f64).collect();
    positions.sort_by(f64::total_cmp);
    (count, max_rows, positions)
}

// ---------------------------------------------------------------------------
// Step 6: Snap nearby edges
// ---------------------------------------------------------------------------

/// Merge edge positions that are within EDGE_SNAP_TOLERANCE of each other
/// by averaging their x-coordinates.
fn snap_edges(mut positions: Vec<f64>) -> Vec<f64> {
    if positions.is_empty() {
        return positions;
    }

    positions.sort_by(f64::total_cmp);

    let mut snapped: Vec<f64> = Vec::new();
    let mut group_sum = positions[0];
    let mut group_count = 1.0_f64;

    for &pos in &positions[1..] {
        if pos - (group_sum / group_count) <= EDGE_SNAP_TOLERANCE {
            group_sum += pos;
            group_count += 1.0;
        } else {
            snapped.push(group_sum / group_count);
            group_sum = pos;
            group_count = 1.0;
        }
    }
    snapped.push(group_sum / group_count);

    snapped
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_span(text: &str, x: f64, y: f64, width: f64, font_size: f64) -> TextSpan {
        TextSpan::new(
            text.to_string(),
            x,
            y,
            width,
            "TestFont".to_string(),
            font_size,
        )
    }

    fn make_bbox(x_min: f64, y_min: f64, x_max: f64, y_max: f64) -> BoundingBox {
        BoundingBox::new(x_min, y_min, x_max, y_max)
    }

    #[test]
    fn test_basic_three_column_table() {
        // 5 rows, 3 columns with consistent left-edge alignment
        let bbox = make_bbox(50.0, 400.0, 500.0, 600.0);
        let mut spans = Vec::new();
        for row in 0..6 {
            let y = 580.0 - row as f64 * 20.0;
            spans.push(make_span("Col1", 100.0, y, 60.0, 12.0));
            spans.push(make_span("Col2", 220.0, y, 80.0, 12.0));
            spans.push(make_span("Col3", 370.0, y, 50.0, 12.0));
        }

        let cols = detect_columns_text_edge(&spans, &bbox);
        assert!(
            cols.len() >= 3,
            "expected >= 3 columns, got {}: {:?}",
            cols.len(),
            cols
        );
        // Check approximate positions
        assert!((cols[0] - 100.0).abs() < 5.0, "col 0: {}", cols[0]);
        assert!((cols[1] - 220.0).abs() < 5.0, "col 1: {}", cols[1]);
        assert!((cols[2] - 370.0).abs() < 5.0, "col 2: {}", cols[2]);
    }

    #[test]
    fn test_interior_edge_eliminated() {
        // Table where column 1 has short text ("A") and column 2 has wide text
        // ("Very Long Text") that spans across column 1's right edge.
        // The right edge of col1 should NOT become a column boundary.
        let bbox = make_bbox(50.0, 400.0, 500.0, 600.0);
        let mut spans = Vec::new();
        for row in 0..6 {
            let y = 580.0 - row as f64 * 20.0;
            spans.push(make_span("A", 100.0, y, 10.0, 12.0));
            spans.push(make_span("Very Long Text", 200.0, y, 120.0, 12.0));
        }

        let cols = detect_columns_text_edge(&spans, &bbox);
        // Should detect 2 columns at ~100 and ~200, not at intermediate positions
        assert_eq!(cols.len(), 2, "expected 2 columns, got {:?}", cols);
        assert!((cols[0] - 100.0).abs() < 5.0);
        assert!((cols[1] - 200.0).abs() < 5.0);
    }

    #[test]
    fn test_too_few_rows() {
        // Only 2 spans on 1 row: not enough for column detection
        let bbox = make_bbox(50.0, 490.0, 500.0, 510.0);
        let spans = vec![
            make_span("A", 100.0, 500.0, 30.0, 12.0),
            make_span("B", 200.0, 500.0, 30.0, 12.0),
        ];
        let cols = detect_columns_text_edge(&spans, &bbox);
        assert!(cols.is_empty(), "expected no columns from 1 row");
    }

    #[test]
    fn test_snap_nearby_edges() {
        let positions = vec![100.0, 101.5, 102.0, 200.0, 201.0, 350.0];
        let snapped = snap_edges(positions);
        assert_eq!(snapped.len(), 3);
        assert!((snapped[0] - 101.17).abs() < 0.5);
        assert!((snapped[1] - 200.5).abs() < 0.5);
        assert!((snapped[2] - 350.0).abs() < 0.1);
    }

    #[test]
    fn test_group_into_rows() {
        let spans = [
            make_span("A", 100.0, 500.0, 20.0, 12.0),
            make_span("B", 200.0, 500.0, 20.0, 12.0),
            make_span("C", 100.0, 480.0, 20.0, 12.0),
            make_span("D", 200.0, 480.0, 20.0, 12.0),
            make_span("E", 100.0, 460.0, 20.0, 12.0),
            make_span("F", 200.0, 460.0, 20.0, 12.0),
        ];
        let refs: Vec<&TextSpan> = spans.iter().collect();
        let rows = group_into_rows(&refs);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].len(), 2);
        assert_eq!(rows[1].len(), 2);
        assert_eq!(rows[2].len(), 2);
    }

    #[test]
    fn test_empty_input() {
        let bbox = make_bbox(0.0, 0.0, 100.0, 100.0);
        let cols = detect_columns_text_edge(&[], &bbox);
        assert!(cols.is_empty());
    }

    #[test]
    fn test_five_column_table() {
        // Simulate a SciTSR-style table with 5 columns across 8 rows
        let bbox = make_bbox(50.0, 200.0, 550.0, 600.0);
        let mut spans = Vec::new();
        let col_xs = [80.0, 160.0, 260.0, 360.0, 450.0];
        let col_widths = [50.0, 70.0, 60.0, 55.0, 65.0];

        for row in 0..8 {
            let y = 580.0 - row as f64 * 20.0;
            for (i, (&cx, &cw)) in col_xs.iter().zip(col_widths.iter()).enumerate() {
                let text = format!("c{}r{}", i, row);
                spans.push(make_span(&text, cx, y, cw, 10.0));
            }
        }

        let cols = detect_columns_text_edge(&spans, &bbox);
        assert!(
            cols.len() >= 5,
            "expected >= 5 columns, got {}: {:?}",
            cols.len(),
            cols
        );
    }
}

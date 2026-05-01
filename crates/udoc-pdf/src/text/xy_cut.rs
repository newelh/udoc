//! X-Y cut algorithm: pre-masking + recursive whitespace-based partitioning.
//!
//! Implements column detection and reading-order partitioning for multi-column
//! PDF layouts. Separated from `order.rs` to keep the reading-order module
//! focused on the tiered cascade orchestration.
//!
//! Algorithm overview:
//! 1. Pre-mask full-width spans (headers, footers, titles)
//! 2. Recursively partition spans using vertical and horizontal whitespace gaps
//! 3. Reorder leaf partitions using Breuel's spatial ordering rules
//! 4. Reinsert pre-masked spans at their correct Y positions

use super::order::{order_single_column, OrderDiagnostics, BASELINE_TOLERANCE};
use super::types::{TextLine, TextSpan};
use crate::content::marked_content::PageStructureOrder;
use crate::diagnostics::{Warning, WarningContext, WarningKind, WarningLevel};

// -- X-Y cut algorithm constants --

/// Maximum recursion depth for X-Y cut. Prevents degenerate cases where
/// every span ends up in its own partition. 4 levels handles up to 16
/// leaf regions, more than enough for any reasonable document layout.
const XY_CUT_MAX_DEPTH: usize = 4;

/// Minimum fraction of content height a vertical whitespace gap must span
/// to qualify as a column boundary. 0.3 means the gap must extend across
/// at least 30% of the page's text content height. Lowered from 0.40 to
/// catch column gaps partially blocked by figures or headings. Stacked-gap
/// detection compensates for the reduced threshold by combining multiple
/// shorter gaps at the same X position.
const MIN_VERTICAL_GAP_HEIGHT_FRACTION: f64 = 0.30;

/// Minimum fraction of content width a horizontal whitespace gap must span
/// to qualify as a row boundary. Used by find_best_horizontal_cut at depth
/// >= 1 in the recursive X-Y cut step.
const MIN_HORIZONTAL_GAP_WIDTH_FRACTION: f64 = 0.50;

/// Minimum vertical gap width in points to consider as a column boundary.
/// Poppler uses 0.7x font size (~7pt for 10pt text). We use 15pt as a
/// safe floor that avoids splitting on wide word spacing while still
/// catching real column gutters (typically 36-72pt).
const MIN_XY_CUT_GAP_PT: f64 = 15.0;

/// Fallback fraction of content width above which a span is considered
/// "full-width". Used when there are too few lines to compute an adaptive
/// threshold. The adaptive threshold (1.3x median line width, per XY-Cut++)
/// is preferred when enough lines exist.
const FULL_WIDTH_THRESHOLD_FALLBACK: f64 = 0.80;

/// Multiplier for adaptive full-width threshold (XY-Cut++, Li et al. 2025).
/// Lines wider than median_line_width * this multiplier are pre-masked.
const FULL_WIDTH_ADAPTIVE_MULTIPLIER: f64 = 1.3;

/// Minimum number of spans in a partition for X-Y cut to attempt splitting.
/// Partitions smaller than this are treated as leaf nodes.
const MIN_SPANS_FOR_CUT: usize = 2;

/// Minimum fraction of content height a whitespace rectangle must cover
/// to qualify as a column divider. More lenient than the X-Y cut threshold
/// because whitespace rectangles are a fallback for partially blocked gutters.
const MIN_WRECT_HEIGHT_FRACTION: f64 = 0.40;

/// Minimum width in points for a whitespace rectangle to qualify as a
/// column divider.
const MIN_WRECT_WIDTH_PT: f64 = 10.0;

/// Resolution for X-axis discretization in the whitespace grid (in points).
/// Smaller values find narrower gutters but increase computation.
const WRECT_X_RESOLUTION: f64 = 2.0;

/// Maximum number of columns in the whitespace occupancy grid.
/// Prevents pathological memory allocation from malformed PDFs with
/// extreme coordinates (grid is n_rows * n_cols bools).
const MAX_GRID_COLS: usize = 10_000;

/// Maximum number of rows (baselines) in the whitespace occupancy grid.
/// Prevents pathological memory allocation when adversarial PDFs contain
/// millions of spans at unique Y coordinates.
const MAX_GRID_ROWS: usize = 10_000;

/// Maximum number of unique baselines tracked during clustering.
/// Prevents O(n*m) baseline lookup from becoming O(n^2) when adversarial
/// PDFs contain millions of spans at unique Y coordinates. Beyond this
/// limit, new spans are assigned to their nearest existing baseline.
pub(super) const MAX_BASELINES: usize = 10_000;

/// Minimum average words per line for a column to be considered "flowing text"
/// rather than table data. Table columns typically have 1-2 short entries per
/// row (numbers, labels), while real text columns have sentences.
const MIN_AVG_WORDS_FOR_TEXT_COLUMN: f64 = 4.0;

// ---------------------------------------------------------------------------
// X-Y cut algorithm: pre-masking + recursive whitespace-based partitioning
// ---------------------------------------------------------------------------

/// Compute the bounding box of a set of spans.
/// Returns (min_x, min_y, max_x, max_y). Uses max(0.0) on width to handle
/// negative widths from malformed PDFs.
fn spans_bbox(spans: &[TextSpan]) -> (f64, f64, f64, f64) {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for s in spans {
        let right = s.x + s.width.max(0.0);
        if s.x < min_x {
            min_x = s.x;
        }
        if right > max_x {
            max_x = right;
        }
        if s.y < min_y {
            min_y = s.y;
        }
        if s.y > max_y {
            max_y = s.y;
        }
    }
    (min_x, min_y, max_x, max_y)
}

/// Separate full-width spans from the input for pre-masking.
///
/// Full-width spans (headers, footers, titles) span > FULL_WIDTH_THRESHOLD
/// of the content width. They block vertical column gaps from being detected,
/// so we remove them before X-Y cut and reinsert at their Y position later.
///
/// Returns the extracted full-width spans. The input vec is modified in place
/// to contain only the remaining (non-full-width) spans.
pub(super) fn separate_full_width_spans(spans: &mut Vec<TextSpan>) -> Vec<TextSpan> {
    if spans.len() < 3 {
        // Need at least some spans for meaningful content width calculation.
        // With < 3 spans, pre-masking would be unreliable.
        return Vec::new();
    }

    let (min_x, _min_y, max_x, _max_y) = spans_bbox(spans);
    let content_width = max_x - min_x;
    if content_width <= 0.0 || !content_width.is_finite() {
        return Vec::new();
    }

    // Cluster spans into lines by baseline, then check if any line's total
    // extent is full-width. Individual spans might be narrow (e.g., bold title
    // split into multiple Tj operations), but the line they form is full-width.
    let mut line_baselines: Vec<f64> = Vec::new();
    let mut line_indices: Vec<Vec<usize>> = Vec::new();

    for (idx, span) in spans.iter().enumerate() {
        let target = line_baselines
            .iter()
            .position(|&bl| (bl - span.y).abs() <= BASELINE_TOLERANCE);
        match target {
            Some(li) => line_indices[li].push(idx),
            None => {
                if line_baselines.len() < MAX_BASELINES {
                    line_baselines.push(span.y);
                    line_indices.push(vec![idx]);
                }
                // Beyond MAX_BASELINES: skip span for full-width analysis.
                // This is a pre-masking optimization, so missing a few spans
                // from adversarial input is harmless.
            }
        }
    }

    // Compute adaptive full-width threshold (XY-Cut++, Li et al. 2025).
    // Use median individual span width * 1.3 as the threshold. Individual
    // span widths are more stable than line extents in multi-column layouts
    // because line extents include column gaps (a two-column line extends
    // from left margin to right margin, appearing "wider" than a true
    // full-width title). Individual spans in column text are column-width,
    // while a full-width title is a single wide span.
    let mut span_widths: Vec<f64> = spans
        .iter()
        .map(|s| s.width.max(0.0))
        .filter(|&w| w > 0.0)
        .collect();

    let full_width_cutoff = if span_widths.len() >= 3 {
        span_widths.sort_by(|a, b| a.total_cmp(b));
        let median_width = span_widths[span_widths.len() / 2];
        let adaptive = median_width * FULL_WIDTH_ADAPTIVE_MULTIPLIER;
        // Don't let adaptive threshold drop below a reasonable floor.
        adaptive.max(content_width * 0.50)
    } else {
        content_width * FULL_WIDTH_THRESHOLD_FALLBACK
    };

    // For each line, check if it's a single contiguous full-width element.
    // A line is full-width if:
    // 1. Its total extent > full_width_cutoff, AND
    // 2. It has no large internal gaps (which would indicate separate columns)
    let gap_threshold = MIN_XY_CUT_GAP_PT;

    let mut full_width_indices: Vec<usize> = Vec::new();
    for indices in &line_indices {
        let mut line_min_x = f64::INFINITY;
        let mut line_max_x = f64::NEG_INFINITY;
        for &idx in indices {
            let s = &spans[idx];
            if s.x < line_min_x {
                line_min_x = s.x;
            }
            let right = s.x + s.width.max(0.0);
            if right > line_max_x {
                line_max_x = right;
            }
        }
        let line_width = line_max_x - line_min_x;
        if line_width <= full_width_cutoff {
            continue;
        }

        // Check for large internal gaps using left-edge analysis.
        // In a two-column layout, a "full-width" line is often two separate
        // column entries whose advance widths bridge the gutter. Detect this
        // by finding an anomalously large gap between consecutive span left
        // edges: the gutter gap is much larger than typical within-column
        // span-to-span gaps.
        let mut sorted_lefts: Vec<f64> = indices.iter().map(|&idx| spans[idx].x).collect();
        sorted_lefts.sort_by(|a, b| a.total_cmp(b));

        let has_large_gap = if sorted_lefts.len() >= 2 {
            let inter_gaps: Vec<f64> = sorted_lefts.windows(2).map(|w| w[1] - w[0]).collect();
            let max_gap = inter_gaps.iter().copied().fold(0.0f64, f64::max);
            if inter_gaps.len() <= 2 {
                // Few spans: just check if the gap exceeds the threshold.
                // Two or three spans with a large gap between left edges
                // indicates separate column entries, not a title.
                max_gap > gap_threshold
            } else {
                // Many spans: use relative check. The gutter gap should
                // be significantly larger than typical within-column gaps.
                let mut sorted_gaps = inter_gaps.clone();
                sorted_gaps.sort_by(|a, b| a.total_cmp(b));
                let median_gap = sorted_gaps[sorted_gaps.len() / 2];
                max_gap > median_gap * 3.0 && max_gap > gap_threshold
            }
        } else {
            false
        };

        if !has_large_gap {
            full_width_indices.extend(indices);
        }
    }

    if full_width_indices.is_empty() {
        return Vec::new();
    }

    // Sort ascending; iterate in reverse to remove from highest index first
    full_width_indices.sort_unstable();
    full_width_indices.dedup();

    // Perf note: Vec::remove is O(n) per call, making this O(k*n) where k is the
    // number of full-width spans and n is total spans. For typical PDFs (k < 10,
    // n < 1000) this is negligible. If profiling shows otherwise, switch to a
    // partition-based approach (swap_remove + sort, or retain + collect).
    let mut extracted = Vec::with_capacity(full_width_indices.len());
    for &idx in full_width_indices.iter().rev() {
        extracted.push(spans.remove(idx));
    }
    extracted.reverse(); // restore original relative order
    extracted
}

/// Tolerance for grouping gaps at similar X positions in stacked-gap detection.
/// Gaps whose left and right edges are both within this distance are considered
/// "stacked" (the same column gutter, partially blocked by a figure or heading).
const STACKED_GAP_X_TOLERANCE: f64 = 20.0;

/// Find the best vertical cut (split left/right) in a set of spans.
///
/// Uses left-edge-only gap detection: finds gaps between
/// consecutive span left edges rather than between advance-width-based
/// right edges. This avoids the advance width problem where font metrics
/// extend span right edges into the column gutter, eliminating whitespace
/// gaps that traditional X-Y cut depends on.
///
/// Height fraction measures what fraction of baselines have content on
/// BOTH sides of each gap, ensuring the gap divides actual columns rather
/// than sitting at a margin or being a one-off indentation gap.
///
/// Includes stacked-gap detection for adjacent gaps that individually
/// fall below the height threshold (e.g., a figure label breaking one
/// gutter into two shorter gaps).
///
/// Returns `Some((gap_center_x, gap_width, gap_height_fraction))` for the
/// best cut, or `None` if no qualifying gap exists.
fn find_best_vertical_cut(spans: &[TextSpan]) -> Option<(f64, f64, f64)> {
    if spans.len() < MIN_SPANS_FOR_CUT {
        return None;
    }

    let (min_x, min_y, max_x, max_y) = spans_bbox(spans);
    let content_height = max_y - min_y;
    let content_width = max_x - min_x;
    if content_height <= 0.0 || content_width <= 0.0 {
        return None;
    }
    if !content_height.is_finite() || !content_width.is_finite() {
        return None;
    }

    // Left-edge-only gap detection. We use span start positions (text
    // matrix X) which are precise, unlike advance-width-based right edges
    // that can extend into or across the column gutter.
    let mut left_edges: Vec<(f64, f64)> = spans.iter().map(|s| (s.x, s.y)).collect();
    left_edges.sort_by(|a, b| a.0.total_cmp(&b.0));

    // Find gaps between consecutive left edges exceeding the threshold.
    let mut gaps: Vec<(f64, f64)> = Vec::new();
    for pair in left_edges.windows(2) {
        let gap_width = pair[1].0 - pair[0].0;
        if gap_width >= MIN_XY_CUT_GAP_PT {
            gaps.push((pair[0].0, pair[1].0));
        }
    }

    if gaps.is_empty() {
        return None;
    }

    // Pre-compute baseline clusters for height fraction analysis.
    // Cap at MAX_BASELINES to prevent O(n*m) blowup on adversarial input.
    let mut baselines: Vec<f64> = Vec::new();
    for &(_, y) in &left_edges {
        if baselines.len() >= MAX_BASELINES {
            break;
        }
        if !baselines
            .iter()
            .any(|&bl| (bl - y).abs() <= BASELINE_TOLERANCE)
        {
            baselines.push(y);
        }
    }
    let total_baselines = baselines.len();
    if total_baselines == 0 {
        return None;
    }

    // For each gap, compute height fraction: what fraction of baselines
    // have content on BOTH sides of the gap center? A real column gutter
    // divides content on most baselines; an indentation gap or margin
    // only affects a few.
    struct GapInfo {
        left: f64,
        right: f64,
        width: f64,
        height_fraction: f64,
    }

    // Pre-compute per-baseline X extremes so height fraction is O(baselines)
    // per gap instead of O(baselines * left_edges).
    struct BaselineXRange {
        min_x: f64,
        max_x: f64,
    }
    let baseline_ranges: Vec<BaselineXRange> = baselines
        .iter()
        .map(|&bl| {
            let mut min_x = f64::INFINITY;
            let mut max_x = f64::NEG_INFINITY;
            for &(x, y) in &left_edges {
                if (y - bl).abs() <= BASELINE_TOLERANCE {
                    if x < min_x {
                        min_x = x;
                    }
                    if x > max_x {
                        max_x = x;
                    }
                }
            }
            BaselineXRange { min_x, max_x }
        })
        .collect();

    let compute_height_fraction = |gap_center: f64| -> f64 {
        let both_count = baseline_ranges
            .iter()
            .filter(|r| r.min_x < gap_center && r.max_x > gap_center)
            .count();
        both_count as f64 / total_baselines as f64
    };

    let mut gap_infos: Vec<GapInfo> = gaps
        .iter()
        .map(|&(left, right)| {
            let center = (left + right) / 2.0;
            GapInfo {
                left,
                right,
                width: right - left,
                height_fraction: compute_height_fraction(center),
            }
        })
        .collect();

    // First pass: find the best individual gap meeting the height threshold.
    // Score by area (width * height_fraction) per XY-Cut++ (Li et al. 2025).
    // A narrow gap spanning 90% of baselines is better than a wide gap
    // spanning only 35%.
    let mut best: Option<(f64, f64, f64)> = None;
    let mut best_score: f64 = 0.0;

    for info in &gap_infos {
        if info.height_fraction < MIN_VERTICAL_GAP_HEIGHT_FRACTION {
            continue;
        }
        let score = info.width * info.height_fraction;
        let gap_center = (info.left + info.right) / 2.0;
        if score > best_score {
            best_score = score;
            best = Some((gap_center, info.width, info.height_fraction));
        }
    }

    if best.is_some() {
        return best;
    }

    // Second pass: stacked-gap detection. Merge adjacent or nearby gaps
    // whose combined span might exceed the height threshold. A figure
    // label with a left edge inside the gutter can break one gap into
    // two adjacent smaller ones.
    gap_infos.sort_by(|a, b| a.left.total_cmp(&b.left));

    for i in 0..gap_infos.len() {
        let mut combined_left = gap_infos[i].left;
        let mut combined_right = gap_infos[i].right;

        for (j, other) in gap_infos.iter().enumerate() {
            if i == j {
                continue;
            }
            // Sorted by left edge, so once a gap starts beyond our combined
            // right + tolerance, all subsequent gaps are too far right.
            if other.left > combined_right + STACKED_GAP_X_TOLERANCE {
                break;
            }
            // Merge gaps that are adjacent or close together
            if other.right >= combined_left - STACKED_GAP_X_TOLERANCE {
                combined_left = combined_left.min(other.left);
                combined_right = combined_right.max(other.right);
            }
        }

        let combined_width = combined_right - combined_left;
        if combined_width < MIN_XY_CUT_GAP_PT {
            continue;
        }

        let combined_center = (combined_left + combined_right) / 2.0;
        let combined_fraction = compute_height_fraction(combined_center);
        if combined_fraction < MIN_VERTICAL_GAP_HEIGHT_FRACTION {
            continue;
        }

        let score = combined_width * combined_fraction;
        if score > best_score {
            best_score = score;
            best = Some((combined_center, combined_width, combined_fraction));
        }
    }

    best
}

/// Find the best horizontal cut (split top/bottom) in a set of spans.
///
/// Projects all spans onto the Y axis and finds the widest horizontal
/// whitespace gap that spans a sufficient fraction of the content width.
///
/// Returns `Some((gap_center_y, gap_height, gap_width_fraction))` for the
/// best cut, or `None` if no qualifying gap exists.
///
/// Used at depth >= 1 in the recursive X-Y cut step to detect section
/// breaks within columns (e.g., headings or figures separating text blocks).
fn find_best_horizontal_cut(spans: &[TextSpan]) -> Option<(f64, f64, f64)> {
    if spans.len() < MIN_SPANS_FOR_CUT {
        return None;
    }

    let (min_x, min_y, max_x, max_y) = spans_bbox(spans);
    let content_height = max_y - min_y;
    let content_width = max_x - min_x;
    if content_height <= 0.0 || content_width <= 0.0 {
        return None;
    }
    if !content_height.is_finite() || !content_width.is_finite() {
        return None;
    }

    // Cluster spans by baseline Y to find horizontal whitespace gaps
    let mut baselines: Vec<(f64, f64, f64)> = Vec::new(); // (y, min_x, max_x)
    for s in spans {
        let right = s.x + s.width.max(0.0);
        let target = baselines
            .iter_mut()
            .find(|(bl, _, _)| (*bl - s.y).abs() <= BASELINE_TOLERANCE);
        match target {
            Some(entry) => {
                if s.x < entry.1 {
                    entry.1 = s.x;
                }
                if right > entry.2 {
                    entry.2 = right;
                }
            }
            None => {
                if baselines.len() >= MAX_BASELINES {
                    continue;
                }
                baselines.push((s.y, s.x, right));
            }
        }
    }

    if baselines.len() < 2 {
        return None;
    }

    // Sort by Y (ascending in PDF coords, bottom to top)
    baselines.sort_by(|a, b| a.0.total_cmp(&b.0));

    // Find gaps between consecutive baselines.
    // Score by area (height * width_fraction) per XY-Cut++ (Li et al. 2025).
    let mut best: Option<(f64, f64, f64)> = None;
    let mut best_score: f64 = 0.0;
    let median_font_size = compute_median_font_size(spans);

    for i in 1..baselines.len() {
        let gap_bottom = baselines[i - 1].0; // lower Y
        let gap_top = baselines[i].0; // higher Y
        let gap_height = gap_top - gap_bottom;

        // Typical line spacing is ~12-16pt. A horizontal cut needs a gap
        // noticeably larger than normal line spacing.
        if gap_height < median_font_size * 1.5 {
            continue;
        }

        // Check what fraction of the content width this gap spans.
        // A real section break spans the full width; a column break doesn't.
        // We check that lines above and below the gap both span enough width.
        let width_above = baselines[i - 1].2 - baselines[i - 1].1;
        let width_below = baselines[i].2 - baselines[i].1;
        let width_fraction = width_above.min(width_below) / content_width;

        if width_fraction < MIN_HORIZONTAL_GAP_WIDTH_FRACTION {
            continue;
        }

        let gap_center = (gap_bottom + gap_top) / 2.0;
        // Score by area (height * width_fraction) per XY-Cut++.
        let score = gap_height * width_fraction;
        if score > best_score {
            best_score = score;
            best = Some((gap_center, gap_height, width_fraction));
        }
    }

    best
}

/// Compute median font size across spans. Used by find_best_horizontal_cut.
fn compute_median_font_size(spans: &[TextSpan]) -> f64 {
    let mut sizes: Vec<f64> = spans
        .iter()
        .filter(|s| s.font_size > 0.0 && s.font_size.is_finite())
        .map(|s| s.font_size)
        .collect();
    if sizes.is_empty() {
        return 12.0; // reasonable default
    }
    sizes.sort_by(|a, b| a.total_cmp(b));
    sizes[sizes.len() / 2]
}

/// Reorder leaf partitions using Breuel's (2003) spatial ordering rules.
///
/// Instead of relying on tree-traversal order (left-before-right, top-before-
/// bottom at each recursive split level), this evaluates all pairwise spatial
/// relationships between partition bounding boxes and topologically sorts them.
///
/// Rules (for left-to-right, top-to-bottom reading):
/// 1. A before B if A's center is above B's center AND their X ranges overlap
///    (same column, read top-to-bottom)
/// 2. A before B if A's center is left of B's center AND their Y ranges overlap
///    (same row, read left-to-right)
///
/// Builds a DAG from these rules and performs topological sort (Kahn's algorithm).
/// Falls back to the input order if a cycle is detected (shouldn't happen with
/// non-overlapping partitions, but defensive).
pub(super) fn reorder_partitions_breuel(
    partitions: Vec<Vec<TextSpan>>,
    diag: Option<&OrderDiagnostics<'_>>,
) -> Vec<Vec<TextSpan>> {
    let n = partitions.len();
    if n <= 1 {
        return partitions;
    }

    // Compute bounding box for each partition
    let bboxes: Vec<(f64, f64, f64, f64)> = partitions.iter().map(|p| spans_bbox(p)).collect();

    // Build adjacency list DAG: edges[i] = set of j where i must come before j
    let mut in_degree = vec![0usize; n];
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); n];

    for i in 0..n {
        let (ai_min_x, ai_min_y, ai_max_x, ai_max_y) = bboxes[i];
        let ai_cx = (ai_min_x + ai_max_x) / 2.0;
        let ai_cy = (ai_min_y + ai_max_y) / 2.0;

        for j in (i + 1)..n {
            let (aj_min_x, aj_min_y, aj_max_x, aj_max_y) = bboxes[j];
            let aj_cx = (aj_min_x + aj_max_x) / 2.0;
            let aj_cy = (aj_min_y + aj_max_y) / 2.0;

            // Check horizontal overlap (same column)
            let x_overlap = ai_min_x < aj_max_x && aj_min_x < ai_max_x;

            let mut i_before_j = false;
            let mut j_before_i = false;

            if x_overlap {
                // Same column: order top-to-bottom (higher Y first in PDF coords)
                if ai_cy > aj_cy {
                    i_before_j = true;
                } else if aj_cy > ai_cy {
                    j_before_i = true;
                }
            } else {
                // Different columns: complete left column before right.
                // This ensures column-first reading order regardless of
                // Y positions of sub-partitions within each column.
                if ai_cx < aj_cx {
                    i_before_j = true;
                } else if aj_cx < ai_cx {
                    j_before_i = true;
                }
            }

            // Fallback for equal centers: use top-left priority
            if !i_before_j && !j_before_i {
                if ai_cy > aj_cy || (ai_cy == aj_cy && ai_cx < aj_cx) {
                    i_before_j = true;
                } else {
                    j_before_i = true;
                }
            }

            if i_before_j {
                edges[i].push(j);
                in_degree[j] += 1;
            } else if j_before_i {
                edges[j].push(i);
                in_degree[i] += 1;
            }
        }
    }

    // Kahn's algorithm for topological sort
    let mut queue: std::collections::VecDeque<usize> =
        (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut order: Vec<usize> = Vec::with_capacity(n);

    while let Some(node) = queue.pop_front() {
        order.push(node);
        for &next in &edges[node] {
            in_degree[next] -= 1;
            if in_degree[next] == 0 {
                queue.push_back(next);
            }
        }
    }

    if order.len() != n {
        // Cycle detected (shouldn't happen with well-formed partitions).
        // Fall back to input order.
        if let Some(d) = diag {
            d.sink.warning(Warning {
                offset: None,
                kind: WarningKind::ReadingOrder,
                level: WarningLevel::Warning,
                context: WarningContext {
                    page_index: Some(d.page_index),
                    ..Default::default()
                },
                message: "topological sort detected cycle in reading order, \
                          falling back to input order"
                    .into(),
            });
        }
        return partitions;
    }

    // Reorder partitions according to topological sort
    let mut indexed: Vec<Option<Vec<TextSpan>>> = partitions.into_iter().map(Some).collect();
    order
        .into_iter()
        .map(|i| indexed[i].take().unwrap_or_default())
        .collect()
}

/// Find a vertical cut using Breuel's maximal whitespace rectangle approach.
///
/// Fallback segmentation when the left-edge gap method finds no cut (
/// step 2b). Builds a 2D occupancy grid from span bounding boxes, finds
/// maximal empty rectangles using the histogram method, and selects the
/// best tall narrow rectangle as a column divider.
///
/// Returns `Some((cut_x, rect_width, height_fraction))` or `None`.
fn find_whitespace_vertical_cut(
    spans: &[TextSpan],
    diag: Option<&OrderDiagnostics<'_>>,
) -> Option<(f64, f64, f64)> {
    if spans.len() < MIN_SPANS_FOR_CUT {
        return None;
    }

    let (min_x, min_y, max_x, max_y) = spans_bbox(spans);
    let content_width = max_x - min_x;
    let content_height = max_y - min_y;
    if content_width <= 0.0 || content_height <= 0.0 {
        return None;
    }
    if !content_width.is_finite() || !content_height.is_finite() {
        return None;
    }

    // Discretize X axis into columns at WRECT_X_RESOLUTION granularity.
    // Cap at MAX_GRID_COLS to prevent pathological memory allocation from
    // malformed PDFs with extreme coordinates (grid is n_rows * n_cols bools).
    let raw_cols = (content_width / WRECT_X_RESOLUTION).ceil() as usize;
    let n_cols = raw_cols.clamp(1, MAX_GRID_COLS);
    if raw_cols > MAX_GRID_COLS {
        if let Some(d) = diag {
            d.sink.warning(Warning {
                offset: None,
                kind: WarningKind::ResourceLimit,
                level: WarningLevel::Warning,
                context: WarningContext {
                    page_index: Some(d.page_index),
                    ..Default::default()
                },
                message: format!("whitespace grid capped at {} columns", MAX_GRID_COLS),
            });
        }
    }
    if n_cols < 3 {
        return None; // Content too narrow for column detection
    }

    // Collect baseline rows (horizontal spans only). Cap at MAX_GRID_ROWS to prevent O(n*m) blowup.
    let mut baselines: Vec<f64> = Vec::new();
    let mut capped = false;
    for s in spans {
        if s.is_vertical {
            continue;
        }
        if baselines.len() >= MAX_GRID_ROWS {
            capped = true;
            break;
        }
        if !baselines
            .iter()
            .any(|&bl| (bl - s.y).abs() <= BASELINE_TOLERANCE)
        {
            baselines.push(s.y);
        }
    }
    baselines.sort_by(|a, b| a.total_cmp(b));
    if capped {
        if let Some(d) = diag {
            d.sink.warning(Warning {
                offset: None,
                kind: WarningKind::ResourceLimit,
                level: WarningLevel::Warning,
                context: WarningContext {
                    page_index: Some(d.page_index),
                    ..Default::default()
                },
                message: format!("whitespace grid capped at {} rows", MAX_GRID_ROWS),
            });
        }
    }
    let n_rows = baselines.len();
    if n_rows < 2 {
        return None;
    }

    // Build occupancy grid: grid[row][col] = true if a span occupies that cell.
    // Use span left edge to estimated visual right edge. For visual width,
    // use text length * (font_size * 0.5) as an approximation, capped at
    // the advance width. This avoids the advance-width-into-gutter problem.
    let mut grid = vec![vec![false; n_cols]; n_rows];

    for s in spans {
        // Skip vertical CJK spans; they don't participate in horizontal
        // column detection and would corrupt the occupancy grid.
        if s.is_vertical {
            continue;
        }

        // Find which baseline row this span belongs to
        let row = match baselines
            .iter()
            .position(|&bl| (bl - s.y).abs() <= BASELINE_TOLERANCE)
        {
            Some(r) => r,
            None => continue,
        };

        // Estimate visual width from glyph advances.
        // CJK full-width glyphs have per-char advance >= 0.8 * font_size;
        // for those, trust s.width directly (it already reflects the font's
        // /W array or /DW default). For Latin proportional text, use the
        // conservative 0.5 * font_size estimate to avoid advance-width
        // bleeding into gutters.
        let font_sz = if s.font_size > 0.0 && s.font_size.is_finite() {
            s.font_size
        } else {
            12.0
        };
        let char_count = s.text.chars().count().max(1) as f64;
        let per_char_advance = s.width / char_count;
        let visual_width = if per_char_advance >= 0.8 * font_sz {
            // CJK full-width or monospace: trust actual advance widths
            s.width
        } else {
            // Latin proportional: use conservative estimate, capped at advance width
            (char_count * font_sz * 0.5).min(s.width.max(0.0))
        };
        let visual_width = visual_width.max(font_sz * 0.5); // at least half a char

        let x_left = s.x;
        let x_right = s.x + visual_width;

        // Mark grid cells as occupied
        let col_start = ((x_left - min_x) / WRECT_X_RESOLUTION).floor() as usize;
        let col_end = ((x_right - min_x) / WRECT_X_RESOLUTION).ceil() as usize;
        let col_start = col_start.min(n_cols - 1);
        let col_end = col_end.min(n_cols);

        for cell in &mut grid[row][col_start..col_end] {
            *cell = true;
        }
    }

    // Find maximal empty rectangles using histogram method.
    // For each column, compute the height of consecutive empty cells above.
    let mut heights = vec![0usize; n_cols];
    let mut best_rect: Option<(usize, usize, usize)> = None; // (col, width_cols, height_rows)
    let mut best_score: f64 = 0.0;

    for grid_row in &grid {
        // Update heights: if cell is empty, increment; if occupied, reset to 0
        for (h, &occupied) in heights.iter_mut().zip(grid_row.iter()) {
            if !occupied {
                *h += 1;
            } else {
                *h = 0;
            }
        }

        // Find the largest rectangle in this histogram row using a stack.
        // We only care about rectangles that are tall and narrow (gutters).
        // The loop goes to n_cols+1 (sentinel) so we use index-based iteration.
        let mut stack: Vec<(usize, usize)> = Vec::new(); // (col_start, height)

        #[allow(clippy::needless_range_loop)]
        for col in 0..=n_cols {
            let h = if col < n_cols { heights[col] } else { 0 };
            let mut start = col;

            while let Some(&(s_col, s_h)) = stack.last() {
                if s_h <= h {
                    break;
                }
                stack.pop();
                start = s_col;
                let rect_width_cols = col - s_col;
                let rect_height_rows = s_h;
                let rect_width_pt = rect_width_cols as f64 * WRECT_X_RESOLUTION;
                let height_frac = rect_height_rows as f64 / n_rows as f64;

                // Score: favor tall narrow rectangles (column gutters).
                // height * log(1 + width) gives diminishing returns for width
                // while strongly rewarding height coverage.
                if rect_width_pt >= MIN_WRECT_WIDTH_PT && height_frac >= MIN_WRECT_HEIGHT_FRACTION {
                    let score = height_frac * (1.0 + rect_width_pt).ln();
                    if score > best_score {
                        best_score = score;
                        best_rect = Some((s_col, rect_width_cols, rect_height_rows));
                    }
                }
            }

            stack.push((start, h));
        }
    }

    // Convert best rectangle to a cut position
    let (col, width_cols, height_rows) = best_rect?;
    let rect_x_start = min_x + col as f64 * WRECT_X_RESOLUTION;
    let rect_width = width_cols as f64 * WRECT_X_RESOLUTION;
    let cut_x = rect_x_start + rect_width / 2.0;
    let height_frac = height_rows as f64 / n_rows as f64;

    // Validate: the cut must have span left edges on BOTH sides.
    // A rectangle in the right margin of single-column content is not a gutter.
    let margin = WRECT_X_RESOLUTION;
    let has_left = spans.iter().any(|s| s.x < rect_x_start - margin);
    let has_right = spans.iter().any(|s| s.x >= rect_x_start + rect_width);
    if !has_left || !has_right {
        return None;
    }

    Some((cut_x, rect_width, height_frac))
}

/// Recursively partition spans into reading-order leaf regions using X-Y cut.
///
/// Uses density-aware axis selection (XY-Cut++, Li et al. 2025):
/// - Depth 0: always try vertical first (column detection is the primary goal)
/// - Depth >= 1: choose axis based on content density. If the content has many
///   more distinct baselines than distinct X clusters, it's vertically organized
///   and horizontal cuts should be tried first.
///
/// Returns leaf partitions in reading order: left before right, top before bottom.
pub(super) fn xy_cut_recursive(
    spans: Vec<TextSpan>,
    depth: usize,
    diag: Option<&OrderDiagnostics<'_>>,
) -> Vec<Vec<TextSpan>> {
    if depth >= XY_CUT_MAX_DEPTH || spans.len() < MIN_SPANS_FOR_CUT {
        return vec![spans];
    }

    // Density-aware axis selection (XY-Cut++, Li et al. 2025). At depth 0,
    // always try vertical first: column interleaving is the primary error.
    // At depth >= 1, evaluate both cuts and take the one with the higher
    // area score (gap_size * coverage_fraction). This avoids the problem
    // of sequential try-first logic where an inferior cut on the first axis
    // blocks a superior cut on the second axis.
    if depth >= 1 {
        // Evaluate left-edge vertical, whitespace rectangle vertical, and horizontal.
        // Pick the single best cut by area score, then execute it (consuming spans).
        // This avoids both the redundant recomputation and the sequential fallback
        // chain where an inferior cut on the first axis blocks a superior one.
        let v_cut = find_best_vertical_cut(&spans);
        let w_cut = if v_cut.is_none() {
            find_whitespace_vertical_cut(&spans, diag)
        } else {
            None
        };
        let h_cut = find_best_horizontal_cut(&spans);

        let v_score = v_cut.map(|(_, w, hf)| w * hf).unwrap_or(0.0);
        let w_score = w_cut.map(|(_, w, hf)| w * hf).unwrap_or(0.0);
        let h_score = h_cut.map(|(_, h, wf)| h * wf).unwrap_or(0.0);
        let vert_score = v_score.max(w_score);

        if vert_score > 0.0 || h_score > 0.0 {
            if vert_score >= h_score {
                // Use the pre-computed cut directly, no recomputation
                let cut_x = if v_score >= w_score {
                    v_cut.map(|c| c.0)
                } else {
                    w_cut.map(|c| c.0)
                };
                if let Some(x) = cut_x {
                    return try_vertical_cut_at(spans, x, depth, diag);
                }
            } else if let Some(cut) = h_cut {
                return try_horizontal_cut_at(spans, cut.0, depth, diag);
            }
        }
    } else {
        // Depth 0: always vertical first (left-edge, then whitespace fallback)
        if let Some(cut) = find_best_vertical_cut(&spans) {
            return try_vertical_cut_at(spans, cut.0, depth, diag);
        } else if let Some(cut) = find_whitespace_vertical_cut(&spans, diag) {
            return try_vertical_cut_at(spans, cut.0, depth, diag);
        }
    }

    // No cut found: this is a leaf partition
    vec![spans]
}

/// Execute a vertical cut at a pre-computed X position.
/// Takes ownership of spans to avoid cloning (TextSpan contains Strings).
fn try_vertical_cut_at(
    spans: Vec<TextSpan>,
    cut_x: f64,
    depth: usize,
    diag: Option<&OrderDiagnostics<'_>>,
) -> Vec<Vec<TextSpan>> {
    let mut left = Vec::new();
    let mut right = Vec::new();
    for span in spans {
        let center_x = span.x + span.width.max(0.0) / 2.0;
        if center_x < cut_x {
            left.push(span);
        } else {
            right.push(span);
        }
    }

    // Table detection: if ANY partition has very short text per line,
    // this is a table layout, not real text columns. Reject the cut
    // and treat as a single region (row-by-row reading).
    if is_partition_table(&left) || is_partition_table(&right) {
        left.extend(right);
        return vec![left];
    }

    // Recurse on each side. Left partitions come first (LTR reading order).
    let mut result = Vec::new();
    if !left.is_empty() {
        result.extend(xy_cut_recursive(left, depth + 1, diag));
    }
    if !right.is_empty() {
        result.extend(xy_cut_recursive(right, depth + 1, diag));
    }
    if result.is_empty() {
        // Degenerate cut: all spans on one side (shouldn't happen with valid cut_x)
        return vec![Vec::new()];
    }
    result
}

/// Execute a horizontal cut at a pre-computed Y position.
/// Takes ownership of spans to avoid cloning (TextSpan contains Strings).
fn try_horizontal_cut_at(
    spans: Vec<TextSpan>,
    cut_y: f64,
    depth: usize,
    diag: Option<&OrderDiagnostics<'_>>,
) -> Vec<Vec<TextSpan>> {
    let mut top = Vec::new();
    let mut bottom = Vec::new();
    for s in spans {
        if s.y >= cut_y {
            top.push(s);
        } else {
            bottom.push(s);
        }
    }

    // Top before bottom (higher Y = earlier in reading order)
    let mut result = Vec::new();
    if !top.is_empty() {
        result.extend(xy_cut_recursive(top, depth + 1, diag));
    }
    if !bottom.is_empty() {
        result.extend(xy_cut_recursive(bottom, depth + 1, diag));
    }
    if result.is_empty() {
        return vec![Vec::new()];
    }
    result
}

/// Count the number of words in a string (split by whitespace).
fn count_words(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Check if a partition looks like table data (short text per line).
///
/// Tables have 1-2 short entries per row, while real text columns have
/// flowing sentences. Returns true if the average words per line is below
/// the text column threshold.
fn is_partition_table(spans: &[TextSpan]) -> bool {
    if spans.is_empty() {
        return false;
    }

    // Cluster spans into lines by baseline
    let mut line_word_counts: Vec<usize> = Vec::new();
    let mut line_baselines: Vec<f64> = Vec::new();

    for span in spans {
        let target = line_baselines
            .iter()
            .position(|&bl| (bl - span.y).abs() <= BASELINE_TOLERANCE);
        match target {
            Some(idx) => {
                line_word_counts[idx] += count_words(&span.text);
            }
            None => {
                if line_baselines.len() < MAX_BASELINES {
                    line_baselines.push(span.y);
                    line_word_counts.push(count_words(&span.text));
                }
                // Beyond cap: skip for table detection (conservative, won't
                // false-positive since missing lines only reduce avg words).
            }
        }
    }

    if line_word_counts.is_empty() {
        return false;
    }

    let total_words: usize = line_word_counts.iter().sum();
    let avg_words = total_words as f64 / line_word_counts.len() as f64;
    avg_words < MIN_AVG_WORDS_FOR_TEXT_COLUMN
}

/// Reinsert full-width spans into an ordered list of lines at their correct
/// Y positions. Full-width spans are ordered among themselves top-to-bottom,
/// then merged into the line list by Y coordinate.
pub(super) fn reinsert_full_width_lines(
    lines: &mut Vec<TextLine>,
    full_width_spans: Vec<TextSpan>,
    structure_order: Option<&PageStructureOrder>,
) {
    if full_width_spans.is_empty() {
        return;
    }

    // Process full-width spans through single-column ordering to get proper
    // line grouping, word spaces, and merging.
    let fw_lines = order_single_column(full_width_spans, structure_order);

    // Merge fw_lines into the existing lines by baseline Y (descending).
    // Both lists should already be sorted top-to-bottom.
    let existing = std::mem::take(lines);
    let mut merged = Vec::with_capacity(existing.len() + fw_lines.len());
    let mut main_iter = existing.into_iter().peekable();
    let mut fw_iter = fw_lines.into_iter().peekable();

    loop {
        match (main_iter.peek(), fw_iter.peek()) {
            (Some(main_line), Some(fw_line)) => {
                // Higher Y = earlier in reading order (top of page)
                if fw_line.baseline >= main_line.baseline {
                    if let Some(line) = fw_iter.next() {
                        merged.push(line);
                    }
                } else if let Some(line) = main_iter.next() {
                    merged.push(line);
                }
            }
            (Some(_), None) => {
                merged.extend(main_iter);
                break;
            }
            (None, Some(_)) => {
                merged.extend(fw_iter);
                break;
            }
            (None, None) => break,
        }
    }

    *lines = merged;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use udoc_core::text::FontResolution;

    fn span(text: &str, x: f64, y: f64, width: f64, font_size: f64) -> TextSpan {
        TextSpan {
            text: text.to_string(),
            x,
            y,
            width,
            font_name: Arc::from("Helvetica"),
            font_size,
            rotation: 0.0,
            is_vertical: false,
            mcid: None,
            space_width: None,
            has_font_metrics: false,
            is_invisible: false,
            is_annotation: false,
            color: None,
            letter_spacing: None,
            is_superscript: false,
            is_subscript: false,
            char_advances: None,
            advance_scale: 1.0,
            char_codes: None,
            char_gids: None,
            z_index: 0,
            font_id: None,
            font_resolution: FontResolution::Exact,
            glyph_bboxes: None,
            active_clips: Vec::new(),
        }
    }

    /// Build a two-column layout: left column at x~72, right column at x~320,
    /// with a large gap (~180pt) between them across multiple lines.
    /// Uses realistic flowing text (5+ words per line) to distinguish from tables.
    fn two_column_spans() -> Vec<TextSpan> {
        // Simulates a disordered content stream where left and right column
        // spans are NOT in a coherent Y-descending order. This triggers Tier 2
        // (X-Y cut) column detection. Spans are emitted in shuffled order:
        // right column bottom-to-top, then left column bottom-to-top.
        // This gives low coherence because Y increases within each group.
        let font_size = 10.0;
        let char_w = 4.5;
        let mut spans = Vec::new();

        let left_texts = [
            "The quick brown fox jumps over the lazy dog today",
            "Meanwhile the rain continued falling on the fields",
            "Several researchers proposed new approaches to solve",
            "In the following section we describe the methods used",
            "The experimental results clearly show an improvement",
            "Furthermore the analysis reveals several key trends",
        ];
        let right_texts = [
            "On the other hand some critics argued against this",
            "Nevertheless the evidence supports the original claim",
            "Additional experiments were conducted to verify results",
            "The data collected from multiple sources confirms the",
            "These findings have significant implications for future",
            "In conclusion we have demonstrated a novel technique",
        ];

        // Emit right column bottom-to-top (reverse Y order = low coherence)
        for i in (0..6).rev() {
            let y = 700.0 - (i as f64 * 14.0);
            let right_text = right_texts[i];
            let right_w = right_text.len() as f64 * char_w;
            spans.push(TextSpan {
                text: right_text.to_string(),
                x: 340.0,
                y,
                width: right_w,
                font_name: Arc::from("Helvetica"),
                font_size,
                rotation: 0.0,
                is_vertical: false,
                mcid: None,
                space_width: None,
                has_font_metrics: false,
                is_invisible: false,
                is_annotation: false,
                color: None,
                letter_spacing: None,
                is_superscript: false,
                is_subscript: false,
                char_advances: None,
                advance_scale: 1.0,
                char_codes: None,
                char_gids: None,
                z_index: 0,
                font_id: None,
                font_resolution: FontResolution::Exact,
                glyph_bboxes: None,
                active_clips: Vec::new(),
            });
        }

        // Emit left column bottom-to-top (reverse Y order = low coherence)
        for i in (0..6).rev() {
            let y = 700.0 - (i as f64 * 14.0);
            let left_text = left_texts[i];
            let left_w = left_text.len() as f64 * char_w;
            spans.push(TextSpan {
                text: left_text.to_string(),
                x: 72.0,
                y,
                width: left_w,
                font_name: Arc::from("Helvetica"),
                font_size,
                rotation: 0.0,
                is_vertical: false,
                mcid: None,
                space_width: None,
                has_font_metrics: false,
                is_invisible: false,
                is_annotation: false,
                color: None,
                letter_spacing: None,
                is_superscript: false,
                is_subscript: false,
                char_advances: None,
                advance_scale: 1.0,
                char_codes: None,
                char_gids: None,
                z_index: 0,
                font_id: None,
                font_resolution: FontResolution::Exact,
                glyph_bboxes: None,
                active_clips: Vec::new(),
            });
        }

        spans
    }

    #[test]
    fn test_count_words() {
        assert_eq!(count_words("hello world"), 2);
        assert_eq!(count_words("one"), 1);
        assert_eq!(count_words(""), 0);
        assert_eq!(count_words("  spaced  out  "), 2);
        assert_eq!(count_words("the quick brown fox jumps"), 5);
    }

    #[test]
    fn test_spans_bbox_basic() {
        let spans = vec![
            span("A", 10.0, 100.0, 20.0, 12.0),
            span("B", 50.0, 200.0, 30.0, 12.0),
            span("C", 5.0, 150.0, 10.0, 12.0),
        ];
        let (min_x, min_y, max_x, max_y) = spans_bbox(&spans);
        assert!((min_x - 5.0).abs() < 0.01, "min_x={min_x}");
        assert!((min_y - 100.0).abs() < 0.01, "min_y={min_y}");
        assert!((max_x - 80.0).abs() < 0.01, "max_x={max_x}");
        assert!((max_y - 200.0).abs() < 0.01, "max_y={max_y}");
    }

    #[test]
    fn test_spans_bbox_negative_width() {
        // Negative widths should be clamped to 0
        let spans = vec![span("A", 10.0, 100.0, -5.0, 12.0)];
        let (min_x, _min_y, max_x, _max_y) = spans_bbox(&spans);
        assert!((min_x - 10.0).abs() < 0.01);
        assert!((max_x - 10.0).abs() < 0.01); // x + max(0, -5) = x + 0
    }

    #[test]
    fn test_find_best_vertical_cut_two_columns() {
        let spans = two_column_spans();
        let cut = find_best_vertical_cut(&spans);
        assert!(
            cut.is_some(),
            "should find vertical cut in two-column layout"
        );
        let (center, width, height_frac) = cut.expect("verified above");
        // Left-edge-only: gap is between left edges at 72 and 340, center ~ 206
        assert!(
            center > 100.0 && center < 300.0,
            "cut center {center:.1} should be between left-edge groups"
        );
        assert!(width > 15.0, "gap width {width:.1} should be significant");
        assert!(
            height_frac >= MIN_VERTICAL_GAP_HEIGHT_FRACTION,
            "height fraction {height_frac:.2} should meet minimum"
        );
    }

    #[test]
    fn test_find_best_vertical_cut_single_column() {
        let spans = vec![
            span("Line one text here", 72.0, 700.0, 108.0, 10.0),
            span("Line two text here", 72.0, 686.0, 108.0, 10.0),
            span("Line three here", 72.0, 672.0, 90.0, 10.0),
            span("Line four here too", 72.0, 658.0, 108.0, 10.0),
        ];
        let cut = find_best_vertical_cut(&spans);
        assert!(
            cut.is_none(),
            "single-column layout should not find vertical cut, got: {:?}",
            cut
        );
    }

    #[test]
    fn test_find_best_vertical_cut_single_span() {
        let spans = vec![span("Hello", 72.0, 700.0, 30.0, 12.0)];
        assert!(find_best_vertical_cut(&spans).is_none());
    }

    #[test]
    fn test_find_best_vertical_cut_empty() {
        let spans: Vec<TextSpan> = vec![];
        assert!(find_best_vertical_cut(&spans).is_none());
    }

    #[test]
    fn test_find_whitespace_vertical_cut_two_columns() {
        // Two-column layout with visual widths that fill the gutter
        // (simulating the advance width problem). The left-edge-only
        // method should catch this, but whitespace rectangles should too
        // since the visual width estimate is narrower than advance width.
        let spans = two_column_spans();
        let cut = find_whitespace_vertical_cut(&spans, None);
        assert!(
            cut.is_some(),
            "should find whitespace vertical cut in two-column layout"
        );
        let (center, width, height_frac) = cut.unwrap();
        // Whitespace rect finds the gap between visual right edges (~292)
        // and right column left edges (340). Center should be in 280-340 range.
        assert!(
            center > 200.0 && center < 340.0,
            "cut center {center:.1} should be between columns"
        );
        assert!(
            width >= MIN_WRECT_WIDTH_PT,
            "width {width:.1} should meet minimum"
        );
        assert!(
            height_frac >= MIN_WRECT_HEIGHT_FRACTION,
            "height fraction {height_frac:.2} should meet minimum"
        );
    }

    #[test]
    fn test_find_whitespace_vertical_cut_single_column() {
        // Single-column layout should not produce a whitespace cut
        let spans = vec![
            span("Line one text here that is wide", 72.0, 700.0, 200.0, 10.0),
            span("Line two text here also is wide", 72.0, 686.0, 200.0, 10.0),
            span("Line three text here wide again", 72.0, 672.0, 200.0, 10.0),
            span(
                "Line four text that is still wide",
                72.0,
                658.0,
                200.0,
                10.0,
            ),
        ];
        let cut = find_whitespace_vertical_cut(&spans, None);
        assert!(
            cut.is_none(),
            "single-column layout should not find whitespace cut, got: {:?}",
            cut
        );
    }

    #[test]
    fn test_separate_full_width_spans_no_headers() {
        // Two-column layout: no spans should be extracted as full-width
        let mut spans = two_column_spans();
        let fw = separate_full_width_spans(&mut spans);
        assert!(
            fw.is_empty(),
            "two-column layout should have no full-width spans, got {}",
            fw.len()
        );
        assert_eq!(spans.len(), 12); // all spans remain
    }

    #[test]
    fn test_separate_full_width_spans_with_header() {
        let font_size = 10.0;
        let char_w = 4.5;
        let mut spans = Vec::new();

        // Full-width title at top: must span > 80% of content width.
        // Body spans from x=72 to ~591 (content width ~519).
        // Title needs width > 519 * 0.8 = 415. Use explicit wide width.
        let title = "This Is A Full Width Title Spanning Both Columns Of The Entire Page Layout";
        spans.push(TextSpan {
            text: title.to_string(),
            x: 72.0,
            y: 750.0,
            width: 480.0, // wider than 0.8 * content_width
            font_name: Arc::from("Helvetica"),
            font_size: 14.0,
            rotation: 0.0,
            is_vertical: false,
            mcid: None,
            space_width: None,
            has_font_metrics: false,
            is_invisible: false,
            is_annotation: false,
            color: None,
            letter_spacing: None,
            is_superscript: false,
            is_subscript: false,
            char_advances: None,
            advance_scale: 1.0,
            char_codes: None,
            char_gids: None,
            z_index: 0,
            font_id: None,
            font_resolution: FontResolution::Exact,
            glyph_bboxes: None,
            active_clips: Vec::new(),
        });

        // Two-column body
        for i in 0..5 {
            let y = 720.0 - (i as f64 * 14.0);
            let left = "The left column has flowing text here";
            spans.push(TextSpan {
                text: left.to_string(),
                x: 72.0,
                y,
                width: left.len() as f64 * char_w,
                font_name: Arc::from("Helvetica"),
                font_size,
                rotation: 0.0,
                is_vertical: false,
                mcid: None,
                space_width: None,
                has_font_metrics: false,
                is_invisible: false,
                is_annotation: false,
                color: None,
                letter_spacing: None,
                is_superscript: false,
                is_subscript: false,
                char_advances: None,
                advance_scale: 1.0,
                char_codes: None,
                char_gids: None,
                z_index: 0,
                font_id: None,
                font_resolution: FontResolution::Exact,
                glyph_bboxes: None,
                active_clips: Vec::new(),
            });
            let right = "Meanwhile the right has different text";
            spans.push(TextSpan {
                text: right.to_string(),
                x: 420.0,
                y,
                width: right.len() as f64 * char_w,
                font_name: Arc::from("Helvetica"),
                font_size,
                rotation: 0.0,
                is_vertical: false,
                mcid: None,
                space_width: None,
                has_font_metrics: false,
                is_invisible: false,
                is_annotation: false,
                color: None,
                letter_spacing: None,
                is_superscript: false,
                is_subscript: false,
                char_advances: None,
                advance_scale: 1.0,
                char_codes: None,
                char_gids: None,
                z_index: 0,
                font_id: None,
                font_resolution: FontResolution::Exact,
                glyph_bboxes: None,
                active_clips: Vec::new(),
            });
        }

        let total_before = spans.len();
        let fw = separate_full_width_spans(&mut spans);
        assert_eq!(fw.len(), 1, "should extract 1 full-width span (title)");
        assert!(
            fw[0].text.contains("Full Width Title"),
            "extracted span should be the title"
        );
        assert_eq!(
            spans.len(),
            total_before - 1,
            "remaining spans should be body text"
        );
    }

    #[test]
    fn test_separate_full_width_too_few_spans() {
        // With < 3 spans, pre-masking should be skipped
        let mut spans = vec![
            span("Hello", 72.0, 700.0, 200.0, 12.0),
            span("World", 72.0, 680.0, 200.0, 12.0),
        ];
        let fw = separate_full_width_spans(&mut spans);
        assert!(fw.is_empty(), "should not extract from < 3 spans");
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn test_xy_cut_recursive_two_columns() {
        let spans = two_column_spans();
        let partitions = xy_cut_recursive(spans, 0, None);
        assert_eq!(
            partitions.len(),
            2,
            "two-column layout should produce 2 partitions, got {}",
            partitions.len()
        );
        assert_eq!(partitions[0].len(), 6, "left partition should have 6 spans");
        assert_eq!(
            partitions[1].len(),
            6,
            "right partition should have 6 spans"
        );

        // Left partition should have lower X values
        let left_max_x = partitions[0]
            .iter()
            .map(|s| s.x)
            .fold(f64::NEG_INFINITY, f64::max);
        let right_min_x = partitions[1]
            .iter()
            .map(|s| s.x)
            .fold(f64::INFINITY, f64::min);
        assert!(
            left_max_x < right_min_x,
            "left partition max_x ({left_max_x}) should be < right partition min_x ({right_min_x})"
        );
    }

    #[test]
    fn test_xy_cut_recursive_single_column() {
        let spans = vec![
            span("Line one text here", 72.0, 700.0, 108.0, 10.0),
            span("Line two text here", 72.0, 686.0, 108.0, 10.0),
            span("Line three here", 72.0, 672.0, 90.0, 10.0),
        ];
        let partitions = xy_cut_recursive(spans, 0, None);
        assert_eq!(
            partitions.len(),
            1,
            "single-column layout should produce 1 partition"
        );
    }

    #[test]
    fn test_xy_cut_recursive_max_depth() {
        // At max depth, should return input as a single partition
        let spans = two_column_spans();
        let partitions = xy_cut_recursive(spans, XY_CUT_MAX_DEPTH, None);
        assert_eq!(
            partitions.len(),
            1,
            "at max depth, should return single partition"
        );
    }

    #[test]
    fn test_xy_cut_table_not_split() {
        // Table layout should not be split into columns by X-Y cut
        let font_size = 10.0;
        let char_w = 6.0;
        let mut spans = Vec::new();

        let col1 = ["Process", "gcc", "clang", "rustc"];
        let col2 = ["200k", "1.2s", "1.1s", "0.9s"];

        for i in 0..4 {
            let y = 700.0 - (i as f64 * 14.0);
            spans.push(TextSpan {
                text: col1[i].to_string(),
                x: 72.0,
                y,
                width: col1[i].len() as f64 * char_w,
                font_name: Arc::from("Helvetica"),
                font_size,
                rotation: 0.0,
                is_vertical: false,
                mcid: None,
                space_width: None,
                has_font_metrics: false,
                is_invisible: false,
                is_annotation: false,
                color: None,
                letter_spacing: None,
                is_superscript: false,
                is_subscript: false,
                char_advances: None,
                advance_scale: 1.0,
                char_codes: None,
                char_gids: None,
                z_index: 0,
                font_id: None,
                font_resolution: FontResolution::Exact,
                glyph_bboxes: None,
                active_clips: Vec::new(),
            });
            spans.push(TextSpan {
                text: col2[i].to_string(),
                x: 300.0,
                y,
                width: col2[i].len() as f64 * char_w,
                font_name: Arc::from("Helvetica"),
                font_size,
                rotation: 0.0,
                is_vertical: false,
                mcid: None,
                space_width: None,
                has_font_metrics: false,
                is_invisible: false,
                is_annotation: false,
                color: None,
                letter_spacing: None,
                is_superscript: false,
                is_subscript: false,
                char_advances: None,
                advance_scale: 1.0,
                char_codes: None,
                char_gids: None,
                z_index: 0,
                font_id: None,
                font_resolution: FontResolution::Exact,
                glyph_bboxes: None,
                active_clips: Vec::new(),
            });
        }

        let partitions = xy_cut_recursive(spans, 0, None);
        assert_eq!(
            partitions.len(),
            1,
            "table should not be split into columns, got {} partitions",
            partitions.len()
        );
    }

    #[test]
    fn test_is_partition_table_short_text() {
        let spans = vec![
            span("Hello", 72.0, 700.0, 30.0, 10.0),
            span("World", 72.0, 686.0, 30.0, 10.0),
            span("Test", 72.0, 672.0, 24.0, 10.0),
        ];
        assert!(
            is_partition_table(&spans),
            "1 word per line should be detected as table"
        );
    }

    #[test]
    fn test_is_partition_table_flowing_text() {
        let spans = vec![
            span(
                "The quick brown fox jumps over the lazy dog",
                72.0,
                700.0,
                200.0,
                10.0,
            ),
            span(
                "Meanwhile the rain continued to fall heavily",
                72.0,
                686.0,
                200.0,
                10.0,
            ),
        ];
        assert!(
            !is_partition_table(&spans),
            "flowing text should not be detected as table"
        );
    }

    #[test]
    fn test_reinsert_full_width_lines_basic() {
        // Lines at y=600 and y=400, full-width line at y=700 (above both)
        let mut lines = vec![
            TextLine {
                baseline: 600.0,
                spans: vec![span("Middle", 72.0, 600.0, 50.0, 12.0)],
                is_vertical: false,
            },
            TextLine {
                baseline: 400.0,
                spans: vec![span("Bottom", 72.0, 400.0, 50.0, 12.0)],
                is_vertical: false,
            },
        ];
        let fw_spans = vec![span("Title", 72.0, 700.0, 200.0, 14.0)];
        reinsert_full_width_lines(&mut lines, fw_spans, None);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text(), "Title");
        assert_eq!(lines[1].text(), "Middle");
        assert_eq!(lines[2].text(), "Bottom");
    }

    #[test]
    fn test_reinsert_full_width_lines_between() {
        // Full-width line should be inserted between existing lines
        let mut lines = vec![
            TextLine {
                baseline: 700.0,
                spans: vec![span("Top", 72.0, 700.0, 30.0, 12.0)],
                is_vertical: false,
            },
            TextLine {
                baseline: 400.0,
                spans: vec![span("Bottom", 72.0, 400.0, 50.0, 12.0)],
                is_vertical: false,
            },
        ];
        let fw_spans = vec![span("Section Break", 72.0, 550.0, 200.0, 14.0)];
        reinsert_full_width_lines(&mut lines, fw_spans, None);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text(), "Top");
        assert_eq!(lines[1].text(), "Section Break");
        assert_eq!(lines[2].text(), "Bottom");
    }

    #[test]
    fn test_reinsert_full_width_lines_empty() {
        let mut lines = vec![TextLine {
            baseline: 700.0,
            spans: vec![span("Only", 72.0, 700.0, 30.0, 12.0)],
            is_vertical: false,
        }];
        let original_len = lines.len();
        reinsert_full_width_lines(&mut lines, vec![], None);
        assert_eq!(
            lines.len(),
            original_len,
            "empty fw should not change lines"
        );
    }

    #[test]
    fn test_gap_threshold_at_30_percent() {
        // Two columns with a gap that spans ~35% of baselines (above 0.30
        // but below old 0.40 threshold). Should now detect columns.
        let mut spans = Vec::new();
        // Left column: 10 lines
        for i in 0..10 {
            spans.push(span(
                &format!("L{i}"),
                72.0,
                700.0 - (i as f64) * 20.0,
                150.0,
                12.0,
            ));
        }
        // Right column: only 4 lines (occupying 4 out of 10 baselines = 40%,
        // but the gap is clear for only 4/10 = 40% on the right side).
        // Place right column spans at baselines 0..3 only.
        for i in 0..4 {
            spans.push(span(
                &format!("R{i}"),
                350.0,
                700.0 - (i as f64) * 20.0,
                150.0,
                12.0,
            ));
        }
        // Add wide spans that cross the gap for baselines 4..9 to block those.
        // This creates a scenario: gap is clear for baselines 0..3 (40%),
        // blocked for baselines 4..9 (60%). At old 0.40 threshold this is
        // borderline; at new 0.30 it should pass.
        // Actually, the gap IS clear where the right column has no spans.
        // Baselines 4..9 have only left column spans, so nothing crosses the
        // gap on those baselines. The height fraction is 10/10 = 100%.
        // Let me instead make a test with explicit blocking.

        // Reset and do it properly: create wide spans that cross the gap
        // at some baselines to reduce clear fraction to ~35%.
        let mut spans2 = Vec::new();
        for i in 0..20 {
            let y = 700.0 - (i as f64) * 15.0;
            // Left column
            spans2.push(span(
                &format!("Left column line {i} with enough words here"),
                72.0,
                y,
                150.0,
                12.0,
            ));
            // Right column
            spans2.push(span(
                &format!("Right column line {i} with enough words here"),
                350.0,
                y,
                150.0,
                12.0,
            ));
        }
        // Add wide spans that cross the gap on 13 of the 20 baselines,
        // leaving only 7/20 = 35% clear. Above 0.30 but below old 0.40.
        for i in 0..13 {
            let y = 700.0 - (i as f64) * 15.0;
            spans2.push(span(
                &format!("Wide blocking span number {i}"),
                200.0,
                y,
                120.0,
                10.0,
            ));
        }

        let result = find_best_vertical_cut(&spans2);
        assert!(
            result.is_some(),
            "should detect column gap at 35% height fraction (above 0.30 threshold)"
        );
    }

    #[test]
    fn test_stacked_gap_detection() {
        // Two columns where blocking spans reduce clear baselines to near
        // the 30% threshold. Tests that the lowered threshold catches this.
        let mut spans = Vec::new();

        // 20 baselines
        for i in 0..20 {
            let y = 700.0 - (i as f64) * 15.0;
            // Left column
            spans.push(span(
                &format!("Left column line {i} with enough words for detection"),
                72.0,
                y,
                140.0,
                12.0,
            ));
            // Right column
            spans.push(span(
                &format!("Right column line {i} with enough words for detection"),
                360.0,
                y,
                140.0,
                12.0,
            ));
        }

        // Block the gap on baselines 0..13 with wide spans, leaving
        // 7/20 = 35% clear (above 0.30 threshold)
        for i in 0..13 {
            let y = 700.0 - (i as f64) * 15.0;
            spans.push(span(
                &format!("Blocking element {i}"),
                200.0,
                y,
                130.0,
                10.0,
            ));
        }

        let result = find_best_vertical_cut(&spans);
        assert!(
            result.is_some(),
            "should detect column gap at 35% clear baselines (above 0.30 threshold)"
        );
    }

    #[test]
    fn test_gap_figure_blocking_some_baselines() {
        // Two columns where a narrow figure fills the gap on some baselines.
        // The figure sits entirely within the gap (doesn't extend into
        // either column), so the gap sweep still finds a gap interval.
        // The per-baseline check filters baselines where the figure crosses.
        let mut spans = Vec::new();

        // Create 20 baselines with two column layout.
        // Left: [72, 207], Right: [365, 500]. Gap: [207, 365].
        for i in 0..20 {
            let y = 700.0 - (i as f64) * 15.0;
            spans.push(span(
                &format!("Left column line number {i} with enough words"),
                72.0,
                y,
                135.0,
                12.0,
            ));
            spans.push(span(
                &format!("Right column line number {i} with enough words"),
                365.0,
                y,
                135.0,
                12.0,
            ));
        }

        // Place a figure in the gap on baselines 0..12 (13 baselines).
        // The figure sits at [240, 340], inside the gap [207, 365].
        // It crosses the gap (splits it into [207,240] and [340,365]).
        // The sweep finds two sub-gaps; the per-baseline check marks
        // these baselines as crossed. 7/20 = 35% baselines clear.
        for i in 0..13 {
            let y = 700.0 - (i as f64) * 15.0;
            spans.push(span(&format!("Figure element {i}"), 240.0, y, 100.0, 12.0));
        }

        let result = find_best_vertical_cut(&spans);
        // The sub-gaps [207,240] and [340,365] are both < MIN_XY_CUT_GAP_PT (15pt),
        // so they won't pass. But the full gap [207,365] doesn't exist in the
        // sweep because the figure breaks it. Let me check: gap [207,240] is 33pt,
        // gap [340,365] is 25pt. Both > 15pt. So they should be found.
        // The question is whether enough baselines are clear for each sub-gap.
        // For gap [207,240]: the figure [240,340] doesn't cross this gap.
        // For gap [340,365]: the figure [240,340] doesn't cross this gap.
        // Both sub-gaps are clear on ALL 20 baselines (the figure sits between them).
        // But wait: the figure at [240,340] crosses into neither sub-gap, so
        // both sub-gaps have 100% clear baselines.
        assert!(
            result.is_some(),
            "should detect sub-gap outside figure with 100% clear baselines"
        );
    }

    #[test]
    fn test_horizontal_cut_not_at_depth_0() {
        // Single-column document with a large vertical gap (simulating
        // a section break). At depth 0, horizontal cuts should NOT happen,
        // so xy_cut_recursive should return a single partition.
        let mut spans = Vec::new();

        // Top section: 5 lines
        for i in 0..5 {
            let y = 700.0 - (i as f64) * 15.0;
            spans.push(span(
                &format!("Top section line {i} with enough words in it"),
                72.0,
                y,
                400.0,
                12.0,
            ));
        }
        // Big gap (simulated by skipping Y values)
        // Bottom section: 5 lines starting much lower
        for i in 0..5 {
            let y = 400.0 - (i as f64) * 15.0;
            spans.push(span(
                &format!("Bottom section line {i} with enough words in it"),
                72.0,
                y,
                400.0,
                12.0,
            ));
        }

        let partitions = xy_cut_recursive(spans, 0, None);
        // At depth 0, no vertical cut should be found (single column),
        // and no horizontal cut should be attempted. Single partition.
        assert_eq!(
            partitions.len(),
            1,
            "depth 0 should not apply horizontal cuts"
        );
    }

    #[test]
    fn test_horizontal_cut_at_depth_1() {
        // Two-column layout where one column has a section break.
        // After vertical cut (depth 0 -> 1), horizontal cut should
        // split the section break within the column.
        //
        // Build a left column with a big gap and a right column.
        // Use text with enough words to avoid table detection.
        let mut spans = Vec::new();

        // Left column top: 5 lines
        for i in 0..5 {
            let y = 700.0 - (i as f64) * 15.0;
            spans.push(span(
                &format!("Left top section line {i} with enough flowing text words"),
                72.0,
                y,
                150.0,
                12.0,
            ));
        }
        // Left column bottom (big gap, 200pt below): 5 lines
        for i in 0..5 {
            let y = 400.0 - (i as f64) * 15.0;
            spans.push(span(
                &format!("Left bottom section line {i} with enough flowing text words"),
                72.0,
                y,
                150.0,
                12.0,
            ));
        }
        // Right column: 10 lines, continuous
        for i in 0..10 {
            let y = 700.0 - (i as f64) * 15.0;
            spans.push(span(
                &format!("Right column line number {i} with enough flowing text words"),
                350.0,
                y,
                150.0,
                12.0,
            ));
        }

        let partitions = xy_cut_recursive(spans, 0, None);
        // Should find vertical cut (left vs right), then potentially
        // horizontal cut within left column at depth 1.
        // We expect at least 2 partitions (left and right).
        assert!(
            partitions.len() >= 2,
            "should have at least 2 partitions (left/right columns), got {}",
            partitions.len()
        );
    }

    #[test]
    fn test_reorder_partitions_breuel_left_right() {
        // Two side-by-side partitions: right is listed first, should reorder to left-first.
        let right = vec![
            span("Right col line 1", 300.0, 700.0, 100.0, 12.0),
            span("Right col line 2", 300.0, 686.0, 100.0, 12.0),
        ];
        let left = vec![
            span("Left col line 1", 50.0, 700.0, 100.0, 12.0),
            span("Left col line 2", 50.0, 686.0, 100.0, 12.0),
        ];
        let partitions = vec![right, left];
        let result = reorder_partitions_breuel(partitions, None);
        assert_eq!(result.len(), 2);
        assert!(
            result[0][0].x < result[1][0].x,
            "left partition (x={}) should come before right (x={})",
            result[0][0].x,
            result[1][0].x
        );
    }

    #[test]
    fn test_reorder_partitions_breuel_top_bottom() {
        // Two vertically stacked partitions at same X: bottom listed first.
        let bottom = vec![
            span("Bottom line 1", 50.0, 300.0, 200.0, 12.0),
            span("Bottom line 2", 50.0, 286.0, 200.0, 12.0),
        ];
        let top = vec![
            span("Top line 1", 50.0, 700.0, 200.0, 12.0),
            span("Top line 2", 50.0, 686.0, 200.0, 12.0),
        ];
        let partitions = vec![bottom, top];
        let result = reorder_partitions_breuel(partitions, None);
        assert_eq!(result.len(), 2);
        // Top partition (higher Y) should come first
        assert!(
            result[0][0].y > result[1][0].y,
            "top partition (y={}) should come before bottom (y={})",
            result[0][0].y,
            result[1][0].y
        );
    }

    #[test]
    fn test_reorder_partitions_breuel_single() {
        let partitions = vec![vec![span("Only", 50.0, 700.0, 100.0, 12.0)]];
        let result = reorder_partitions_breuel(partitions, None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0][0].text, "Only");
    }

    #[test]
    fn test_reorder_partitions_breuel_empty() {
        let partitions: Vec<Vec<TextSpan>> = vec![];
        let result = reorder_partitions_breuel(partitions, None);
        assert!(result.is_empty());
    }

    #[test]
    fn test_reorder_partitions_breuel_three_columns() {
        // Three columns given in reverse order.
        let col3 = vec![span("Col3", 500.0, 700.0, 80.0, 12.0)];
        let col1 = vec![span("Col1", 50.0, 700.0, 80.0, 12.0)];
        let col2 = vec![span("Col2", 275.0, 700.0, 80.0, 12.0)];
        let partitions = vec![col3, col1, col2];
        let result = reorder_partitions_breuel(partitions, None);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0][0].text, "Col1");
        assert_eq!(result[1][0].text, "Col2");
        assert_eq!(result[2][0].text, "Col3");
    }

    #[test]
    fn test_cjk_visual_width_full_width_glyphs() {
        // Pure CJK span: 3 chars at 12pt, each full-width (advance = font_size).
        // s.width = 3 * 12.0 = 36.0, per_char_advance = 12.0 >= 0.8 * 12.0.
        // The grid should use s.width (36.0), not the old 0.5 cap (18.0).
        let font_size = 12.0;
        let cjk_width = 3.0 * font_size; // 36pt, full-width advances
        let s = TextSpan {
            text: "\u{4e2d}\u{6587}\u{5b57}".to_string(), // 3 CJK chars
            x: 72.0,
            y: 700.0,
            width: cjk_width,
            font_name: Arc::from("SimSun"),
            font_size,
            rotation: 0.0,
            is_vertical: false,
            mcid: None,
            space_width: None,
            has_font_metrics: true,
            is_invisible: false,
            is_annotation: false,
            color: None,
            letter_spacing: None,
            is_superscript: false,
            is_subscript: false,
            char_advances: None,
            advance_scale: 1.0,
            char_codes: None,
            char_gids: None,
            z_index: 0,
            font_id: None,
            font_resolution: FontResolution::Exact,
            glyph_bboxes: None,
            active_clips: Vec::new(),
        };

        let char_count = s.text.chars().count().max(1) as f64;
        let per_char_advance = s.width / char_count;
        let visual_width = if per_char_advance >= 0.8 * font_size {
            s.width
        } else {
            (char_count * font_size * 0.5).min(s.width.max(0.0))
        };

        assert!(
            (visual_width - 36.0).abs() < 0.01,
            "CJK full-width visual_width should be ~36pt, got {visual_width:.1}"
        );
    }

    #[test]
    fn test_latin_visual_width_uses_half_cap() {
        // Latin proportional: 10 chars at 12pt, advance width = 60pt.
        // per_char_advance = 6.0 < 0.8 * 12.0 = 9.6, so use 0.5 cap.
        // Conservative: 10 * 12 * 0.5 = 60, capped at s.width = 60 -> 60.
        let font_size = 12.0;
        let s = span("HelloWorld", 72.0, 700.0, 60.0, font_size);

        let char_count = s.text.chars().count().max(1) as f64;
        let per_char_advance = s.width / char_count;
        let visual_width = if per_char_advance >= 0.8 * font_size {
            s.width
        } else {
            (char_count * font_size * 0.5).min(s.width.max(0.0))
        };

        // 10 * 12 * 0.5 = 60, min(60, 60) = 60
        assert!(
            (visual_width - 60.0).abs() < 0.01,
            "Latin visual_width should use 0.5 cap, got {visual_width:.1}"
        );

        // With an advance width larger than the conservative estimate,
        // the cap should limit it. Use a realistic Latin scenario:
        // "Hello" at 12pt, advance width 40pt (8pt/char, typical proportional).
        // per_char = 8.0 < 9.6, so conservative cap: 5 * 12 * 0.5 = 30, min(30, 40) = 30.
        let s2 = span("Hello", 72.0, 700.0, 40.0, font_size);
        let char_count2 = s2.text.chars().count().max(1) as f64;
        let per_char_advance2 = s2.width / char_count2;
        let visual_width2 = if per_char_advance2 >= 0.8 * font_size {
            s2.width
        } else {
            (char_count2 * font_size * 0.5).min(s2.width.max(0.0))
        };
        // 5 * 12 * 0.5 = 30, min(30, 40) = 30
        assert!(
            (visual_width2 - 30.0).abs() < 0.01,
            "Latin text should be capped at char_count * 0.5 * font_size, got {visual_width2:.1}"
        );
    }

    #[test]
    fn test_mixed_cjk_latin_trusts_width() {
        // Mixed CJK+Latin span: text with a mix, s.width reflects actual advances.
        // If overall per-char advance is high enough, trust s.width.
        let font_size = 12.0;
        // 2 CJK chars (12pt each) + 2 Latin chars (6pt each) = 36pt total, 4 chars
        // per_char = 9.0, threshold = 0.8 * 12 = 9.6 -> just below, uses cap.
        // But 3 CJK + 1 Latin: 36 + 6 = 42, 4 chars, per_char = 10.5 >= 9.6 -> trusts.
        let s = TextSpan {
            text: "\u{4e2d}\u{6587}\u{5b57}A".to_string(), // 3 CJK + 1 Latin
            x: 72.0,
            y: 700.0,
            width: 42.0, // 3*12 + 1*6
            font_name: Arc::from("SimSun"),
            font_size,
            rotation: 0.0,
            is_vertical: false,
            mcid: None,
            space_width: None,
            has_font_metrics: true,
            is_invisible: false,
            is_annotation: false,
            color: None,
            letter_spacing: None,
            is_superscript: false,
            is_subscript: false,
            char_advances: None,
            advance_scale: 1.0,
            char_codes: None,
            char_gids: None,
            z_index: 0,
            font_id: None,
            font_resolution: FontResolution::Exact,
            glyph_bboxes: None,
            active_clips: Vec::new(),
        };

        let char_count = s.text.chars().count().max(1) as f64;
        let per_char_advance = s.width / char_count;
        let visual_width = if per_char_advance >= 0.8 * font_size {
            s.width
        } else {
            (char_count * font_size * 0.5).min(s.width.max(0.0))
        };

        // per_char = 42/4 = 10.5 >= 9.6, trusts s.width = 42
        assert!(
            (visual_width - 42.0).abs() < 0.01,
            "mixed CJK+Latin should trust s.width when per_char >= 0.8*fs, got {visual_width:.1}"
        );
    }

    #[test]
    fn test_cjk_dw_default_width_not_capped() {
        // CJK font with /DW 1000 (default width = full em).
        // 5 CJK chars at 10pt, each advance = 10pt.
        // s.width = 50.0, per_char = 10.0 >= 0.8 * 10.0 = 8.0.
        // Formula should use s.width = 50, not cap to 5*10*0.5 = 25.
        let font_size = 10.0;
        let s = TextSpan {
            text: "\u{3042}\u{3044}\u{3046}\u{3048}\u{304a}".to_string(), // 5 hiragana
            x: 72.0,
            y: 700.0,
            width: 50.0,
            font_name: Arc::from("MS-Mincho"),
            font_size,
            rotation: 0.0,
            is_vertical: false,
            mcid: None,
            space_width: None,
            has_font_metrics: true,
            is_invisible: false,
            is_annotation: false,
            color: None,
            letter_spacing: None,
            is_superscript: false,
            is_subscript: false,
            char_advances: None,
            advance_scale: 1.0,
            char_codes: None,
            char_gids: None,
            z_index: 0,
            font_id: None,
            font_resolution: FontResolution::Exact,
            glyph_bboxes: None,
            active_clips: Vec::new(),
        };

        let char_count = s.text.chars().count().max(1) as f64;
        let per_char_advance = s.width / char_count;
        let visual_width = if per_char_advance >= 0.8 * font_size {
            s.width
        } else {
            (char_count * font_size * 0.5).min(s.width.max(0.0))
        };

        assert!(
            (visual_width - 50.0).abs() < 0.01,
            "CJK DW=1000 visual_width should be 50pt (not capped to 25pt), got {visual_width:.1}"
        );
    }

    #[test]
    fn test_vertical_spans_excluded_from_whitespace_grid() {
        // Vertical CJK spans should be skipped in the whitespace grid.
        // Build a two-column layout, then add a vertical span that would
        // break the grid if not filtered. The cut should still be found.
        let mut spans = two_column_spans();

        // Add a vertical span crossing the gutter
        spans.push(TextSpan {
            text: "\u{7e26}\u{66f8}\u{304d}".to_string(),
            x: 300.0, // right in the gutter
            y: 700.0,
            width: 36.0,
            font_name: Arc::from("SimSun"),
            font_size: 12.0,
            rotation: 0.0,
            is_vertical: true,
            mcid: None,
            space_width: None,
            has_font_metrics: true,
            is_invisible: false,
            is_annotation: false,
            color: None,
            letter_spacing: None,
            is_superscript: false,
            is_subscript: false,
            char_advances: None,
            advance_scale: 1.0,
            char_codes: None,
            char_gids: None,
            z_index: 0,
            font_id: None,
            font_resolution: FontResolution::Exact,
            glyph_bboxes: None,
            active_clips: Vec::new(),
        });

        let cut = find_whitespace_vertical_cut(&spans, None);
        assert!(
            cut.is_some(),
            "vertical span in gutter should not block whitespace cut detection"
        );
    }

    #[test]
    fn test_stacked_gap_merge_combines_short_gaps() {
        // Exercises the second pass (stacked-gap merge) in find_best_vertical_cut.
        // Creates a layout where every individual gap between consecutive left
        // edges has height_fraction < 30%, but merging nearby gaps produces a
        // combined gap that exceeds the threshold.
        //
        // Layout (10 baselines):
        //   bl 0-1: left edges at x=100 and x=130 (group A)
        //   bl 2-3: left edges at x=120 and x=150 (group B, shifted right)
        //   bl 4-9: left edge at x=50 only (diluter baselines)
        //
        // Sorted unique left edges: 50, 100, 120, 130, 150.
        // Gaps >= 15pt: [50,100]=50, [100,120]=20, [130,150]=20.
        //
        // First pass (individual gaps):
        //   [50,100] center=75: no baseline has both sides -> 0%. FAIL.
        //   [100,120] center=110: only bl 0-1 straddle -> 20%. FAIL.
        //   [130,150] center=140: only bl 2-3 straddle -> 20%. FAIL.
        //
        // Second pass (merge starting from i=2, gap [130,150]):
        //   Skips gap [50,100] (right=100 < combined_left-tol=110).
        //   Merges gap [100,120] -> combined [100,150], center=125.
        //   At center 125: bl 0-1 (100<125, 130>125) and bl 2-3 (120<125, 150>125)
        //   both straddle -> 4/10 = 40% > 30%. PASSES.

        let font_size = 10.0;
        let mut test_spans = Vec::new();

        // bl 0-1: edges at x=100 and x=130
        for i in 0..2 {
            let y = 700.0 - (i as f64 * 15.0);
            test_spans.push(span("Left group A content", 100.0, y, 25.0, font_size));
            test_spans.push(span("Right group A content", 130.0, y, 25.0, font_size));
        }

        // bl 2-3: edges at x=120 and x=150
        for i in 2..4 {
            let y = 700.0 - (i as f64 * 15.0);
            test_spans.push(span("Left group B content", 120.0, y, 25.0, font_size));
            test_spans.push(span("Right group B content", 150.0, y, 25.0, font_size));
        }

        // bl 4-9: edge at x=50 only (diluter, one side only)
        for i in 4..10 {
            let y = 700.0 - (i as f64 * 15.0);
            test_spans.push(span("Far left only content", 50.0, y, 30.0, font_size));
        }

        let result = find_best_vertical_cut(&test_spans);
        assert!(
            result.is_some(),
            "stacked gap merge should find a combined vertical cut"
        );

        let (center, width, fraction) = result.unwrap();
        assert!(
            center > 110.0 && center < 140.0,
            "combined cut center should be between gap groups, got {center:.1}"
        );
        assert!(
            width >= 15.0,
            "combined width should meet minimum, got {width:.1}"
        );
        assert!(
            fraction >= MIN_VERTICAL_GAP_HEIGHT_FRACTION,
            "combined fraction should meet threshold, got {fraction:.2}"
        );
    }

    #[test]
    fn test_reorder_partitions_breuel_cycle_detection() {
        // Create three partitions whose spatial relationships form a cycle.
        // With Breuel's rules:
        //   - X overlap -> higher Y center first
        //   - No X overlap -> left center first
        //
        // Partition C: x=[100,250], cy=844. Overlaps A on X, C above A -> C before A.
        // Partition A: x=[50,200], cy=744. Overlaps B on X, A above B -> A before B.
        // Partition B: x=[0,100], cy=644. No X overlap with C, B left of C -> B before C.
        // Cycle: C -> A -> B -> C.
        let part_c = vec![
            span("C content line one", 100.0, 850.0, 150.0, 12.0),
            span("C content line two", 100.0, 838.0, 150.0, 12.0),
        ];
        let part_a = vec![
            span("A content line one", 50.0, 750.0, 150.0, 12.0),
            span("A content line two", 50.0, 738.0, 150.0, 12.0),
        ];
        let part_b = vec![
            span("B content line one", 0.0, 650.0, 100.0, 12.0),
            span("B content line two", 0.0, 638.0, 100.0, 12.0),
        ];

        use crate::diagnostics::CollectingDiagnostics;
        let diag_sink = CollectingDiagnostics::new();
        let diag = OrderDiagnostics {
            sink: &diag_sink,
            page_index: 7,
        };

        let partitions = vec![part_c, part_a, part_b];
        let result = reorder_partitions_breuel(partitions, Some(&diag));

        // When a cycle is detected, the function falls back to input order
        // and emits a warning.
        let warnings = diag_sink.warnings();
        let cycle_warnings: Vec<_> = warnings
            .iter()
            .filter(|w| w.kind == WarningKind::ReadingOrder && w.message.contains("cycle"))
            .collect();
        assert_eq!(
            cycle_warnings.len(),
            1,
            "should emit exactly one cycle warning, got {} warnings: {:?}",
            cycle_warnings.len(),
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
        assert_eq!(
            cycle_warnings[0].context.page_index,
            Some(7),
            "cycle warning should carry page_index=7"
        );
        assert_eq!(
            cycle_warnings[0].level,
            WarningLevel::Warning,
            "cycle warning should be Warning level"
        );

        // Fallback: input order preserved
        assert_eq!(result.len(), 3);
        assert_eq!(result[0][0].text, "C content line one");
        assert_eq!(result[1][0].text, "A content line one");
        assert_eq!(result[2][0].text, "B content line one");
    }

    #[test]
    fn test_reorder_partitions_breuel_cycle_without_diagnostics() {
        // Same cycle scenario without diagnostics. Should not panic
        // and should return input order.
        let part_c = vec![
            span("C line", 100.0, 850.0, 150.0, 12.0),
            span("C line 2", 100.0, 838.0, 150.0, 12.0),
        ];
        let part_a = vec![
            span("A line", 50.0, 750.0, 150.0, 12.0),
            span("A line 2", 50.0, 738.0, 150.0, 12.0),
        ];
        let part_b = vec![
            span("B line", 0.0, 650.0, 100.0, 12.0),
            span("B line 2", 0.0, 638.0, 100.0, 12.0),
        ];

        let partitions = vec![part_c, part_a, part_b];
        let result = reorder_partitions_breuel(partitions, None);
        // Cycle detected, falls back to input order
        assert_eq!(result.len(), 3);
        assert_eq!(result[0][0].text, "C line");
        assert_eq!(result[1][0].text, "A line");
        assert_eq!(result[2][0].text, "B line");
    }

    #[test]
    fn test_reorder_partitions_breuel_with_diagnostics_no_cycle() {
        // Normal reordering (no cycle) with diagnostics.
        // Verifies the function works correctly with diagnostics attached
        // and does NOT emit a cycle warning.
        use crate::diagnostics::CollectingDiagnostics;
        let diag_sink = CollectingDiagnostics::new();
        let diag = OrderDiagnostics {
            sink: &diag_sink,
            page_index: 2,
        };

        let right = vec![span(
            "Right col text with enough words",
            300.0,
            700.0,
            100.0,
            12.0,
        )];
        let left = vec![span(
            "Left col text with enough words",
            50.0,
            700.0,
            100.0,
            12.0,
        )];
        let partitions = vec![right, left];
        let result = reorder_partitions_breuel(partitions, Some(&diag));

        assert_eq!(result.len(), 2);
        // Left before right
        assert!(result[0][0].x < result[1][0].x);

        // No cycle warning should be emitted
        let warnings = diag_sink.warnings();
        let cycle_warnings: Vec<_> = warnings
            .iter()
            .filter(|w| w.kind == WarningKind::ReadingOrder && w.message.contains("cycle"))
            .collect();
        assert!(
            cycle_warnings.is_empty(),
            "no cycle warning expected for non-cyclic partitions"
        );
    }

    #[test]
    fn test_whitespace_grid_col_cap_diagnostic() {
        // Create spans with extreme X range that would exceed MAX_GRID_COLS.
        // At WRECT_X_RESOLUTION=2.0, need width > 2 * 10_000 = 20_000pt.
        use crate::diagnostics::CollectingDiagnostics;
        let diag_sink = CollectingDiagnostics::new();
        let diag = OrderDiagnostics {
            sink: &diag_sink,
            page_index: 1,
        };

        let spans = vec![
            span("Left content at origin", 0.0, 700.0, 100.0, 12.0),
            span("Right content far away", 25000.0, 700.0, 100.0, 12.0),
            span("Middle content line two", 0.0, 688.0, 100.0, 12.0),
            span("Right content line two", 25000.0, 688.0, 100.0, 12.0),
        ];

        let _result = find_whitespace_vertical_cut(&spans, Some(&diag));

        let warnings = diag_sink.warnings();
        let cap_warnings: Vec<_> = warnings
            .iter()
            .filter(|w| w.kind == WarningKind::ResourceLimit && w.message.contains("columns"))
            .collect();
        assert_eq!(cap_warnings.len(), 1, "should emit grid column cap warning");
        assert_eq!(cap_warnings[0].context.page_index, Some(1));
    }

    #[test]
    fn test_whitespace_grid_row_cap_diagnostic() {
        // Create spans with more than MAX_GRID_ROWS unique baselines.
        // MAX_GRID_ROWS = 10_000. We need > 10K unique Y values.
        use crate::diagnostics::CollectingDiagnostics;
        let diag_sink = CollectingDiagnostics::new();
        let diag = OrderDiagnostics {
            sink: &diag_sink,
            page_index: 4,
        };

        // 10_001 spans at unique Y values (spaced > BASELINE_TOLERANCE apart)
        let mut spans = Vec::new();
        for i in 0..10_001 {
            spans.push(span("x", 0.0, (i as f64) * 5.0, 10.0, 12.0));
            spans.push(span("y", 100.0, (i as f64) * 5.0, 10.0, 12.0));
        }

        let _result = find_whitespace_vertical_cut(&spans, Some(&diag));

        let warnings = diag_sink.warnings();
        let cap_warnings: Vec<_> = warnings
            .iter()
            .filter(|w| w.kind == WarningKind::ResourceLimit && w.message.contains("rows"))
            .collect();
        assert_eq!(
            cap_warnings.len(),
            1,
            "should emit grid row cap warning, got warnings: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
        assert_eq!(cap_warnings[0].context.page_index, Some(4));
    }
}

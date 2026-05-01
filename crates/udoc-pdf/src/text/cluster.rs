//! Spatial clustering, gap detection, and bbox filter helpers for PDF text.
//!
//! Centralises the baseline-clustering / bbox / gap-finding patterns that
//! diagnostic callers and the reading-order pipeline both need; exposing
//! them here saves callers from re-implementing and keeps the "what counts
//! as the same line" tolerance consistent with the rest of
//! `udoc_pdf::text` (the `BASELINE_TOLERANCE` constant in `super::order`).
//!
//! These helpers operate on PDF-native [`TextSpan`] / [`TextLine`] (with
//! `f64` page coordinates, y-up origin) and live in `udoc-pdf` rather than
//! `udoc-core` because the clustering thresholds are tuned to PDF point
//! geometry. Other backends compute their layout differently (DOCX has no
//! geometry; OOXML `EMUs` are integer; etc.) and would not share these
//! tolerances.

use std::ops::Range;

use udoc_core::geometry::BoundingBox;

use super::types::{TextLine, TextSpan};

/// Cluster a slice of text spans into groups that share approximately the
/// same baseline (y-coordinate within `tolerance` points).
///
/// Whitespace-only spans are skipped: they have no glyph extent worth
/// grouping and add noise to baseline detection. Each returned group
/// preserves the original input order of its spans.
///
/// Tolerance picks: pass `2.0` (the `BASELINE_TOLERANCE` constant in
/// `super::order`) for behavior matching the reading-order pipeline.
/// Looser values (3-5 pt) help cluster mixed-leading text; tighter values
/// risk splitting genuine lines that have minor baseline jitter from
/// CID-font glyph metrics.
///
/// Complexity is O(n * k) where n is span count and k is the number of
/// distinct baselines. For typical PDFs (n < 5_000, k < 200) this is
/// negligible. Adversarial input with thousands of distinct baselines
/// degrades; callers concerned with that should pre-filter or cap.
///
/// # Example
///
/// ```
/// use udoc_pdf::TextSpan;
/// use udoc_pdf::cluster::cluster_by_baseline;
///
/// let spans = vec![
///     TextSpan::new("hello".into(), 0.0, 100.0, 30.0, "Helvetica", 12.0),
///     TextSpan::new("world".into(), 40.0, 100.5, 30.0, "Helvetica", 12.0),
///     TextSpan::new("next".into(), 0.0, 80.0, 25.0, "Helvetica", 12.0),
/// ];
/// let groups = cluster_by_baseline(&spans, 2.0);
/// assert_eq!(groups.len(), 2);
/// assert_eq!(groups[0].len(), 2); // hello + world share baseline ~100
/// assert_eq!(groups[1].len(), 1); // next on baseline 80
/// ```
pub fn cluster_by_baseline<'a>(spans: &'a [TextSpan], tolerance: f64) -> Vec<Vec<&'a TextSpan>> {
    let mut baselines: Vec<f64> = Vec::new();
    let mut groups: Vec<Vec<&'a TextSpan>> = Vec::new();

    for span in spans {
        if span.text.chars().all(char::is_whitespace) {
            continue;
        }
        let target = baselines
            .iter()
            .position(|&bl| (bl - span.y).abs() <= tolerance);
        match target {
            Some(idx) => groups[idx].push(span),
            None => {
                baselines.push(span.y);
                groups.push(vec![span]);
            }
        }
    }

    groups
}

/// Detect vertical-gap regions in a sequence of [`TextLine`]s sorted by
/// reading order (top-to-bottom: descending baseline in PDF y-up
/// coordinates). Returns ranges of line indices, where each range covers
/// the lines BELOW a gap and runs to the next gap (or end of input).
///
/// The first range always starts at 0. Each consecutive returned range
/// represents a logically separated section of the page. If no gap >
/// `min_gap` exists, the result is a single range covering all lines.
///
/// `min_gap` is in points (1/72 inch). A typical page-section gap is
/// 1.5x to 2x the body-text leading (e.g. 18-24 pt for 12 pt body).
///
/// Lines must be pre-sorted top-to-bottom for the result to be meaningful.
/// Empty input returns an empty vector. Single-line input returns one
/// range `0..1`.
///
/// # Example
///
/// Pass [`TextLine`] values produced by `Page::text_lines()` (or any other
/// caller-built collection sorted top-to-bottom). For four lines whose
/// baselines are `[700.0, 688.0, 640.0, 628.0]` with `min_gap` 20, the
/// 48-point gap between index 1 (y=688) and index 2 (y=640) splits the
/// page into two sections, returning `vec![0..2, 2..4]`.
pub fn detect_gaps(lines: &[TextLine], min_gap: f64) -> Vec<Range<usize>> {
    if lines.is_empty() {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    if lines.len() == 1 {
        ranges.push(0..1);
        return ranges;
    }

    let mut section_start = 0usize;

    for i in 1..lines.len() {
        // Top-to-bottom in PDF y-up coordinates: previous baseline is
        // ABOVE current, i.e. previous.baseline > current.baseline.
        let gap = lines[i - 1].baseline - lines[i].baseline;
        if gap > min_gap {
            ranges.push(section_start..i);
            section_start = i;
        }
    }
    ranges.push(section_start..lines.len());

    ranges
}

/// Return references to spans whose position lies within `bbox`.
///
/// "Within" means the span's anchor point `(x, y)` is inside the bbox via
/// [`BoundingBox::contains_point`]. This matches how span extraction
/// reports positions (`x` is the left edge, `y` is the baseline) and is
/// the cheap test most callers want for "is this span on this row of the
/// page". Spans whose anchor is in but whose width extends outside are
/// included; spans whose anchor is outside but whose extent intrudes are
/// excluded. For full geometric containment use a manual loop over
/// [`TextSpan::glyph_bboxes`].
///
/// Inverted bboxes are normalized at construction by [`BoundingBox::new`],
/// so callers cannot accidentally pass an empty inverted region.
///
/// # Example
///
/// ```
/// use udoc_core::geometry::BoundingBox;
/// use udoc_pdf::TextSpan;
/// use udoc_pdf::cluster::filter_bbox;
///
/// let spans = vec![
///     TextSpan::new("in".into(), 50.0, 50.0, 10.0, "F", 10.0),
///     TextSpan::new("out".into(), 200.0, 200.0, 10.0, "F", 10.0),
/// ];
/// let bbox = BoundingBox::new(0.0, 0.0, 100.0, 100.0);
/// let kept = filter_bbox(&spans, bbox);
/// assert_eq!(kept.len(), 1);
/// assert_eq!(kept[0].text, "in");
/// ```
pub fn filter_bbox(spans: &[TextSpan], bbox: BoundingBox) -> Vec<&TextSpan> {
    spans
        .iter()
        .filter(|s| bbox.contains_point(s.x, s.y))
        .collect()
}

/// Compute the axis-aligned bounding box of a slice of spans.
///
/// Width is clamped at `max(0.0)` to handle malformed PDFs that emit
/// negative-width glyphs. Returns `None` for empty input or for input
/// whose extent is non-finite (all-NaN positions).
///
/// This is the union of all span anchor-rects: x extends from `min(x)`
/// to `max(x + width)`; y is `min(y)` to `max(y)` (y here is the baseline,
/// not the glyph top, so the result tracks baseline span rather than
/// rendered glyph extent). For glyph-extent bboxes use
/// [`TextSpan::glyph_bboxes`] per span and union those.
///
/// # Example
///
/// ```
/// use udoc_pdf::TextSpan;
/// use udoc_pdf::cluster::spans_bbox;
///
/// let spans = vec![
///     TextSpan::new("a".into(), 10.0, 100.0, 5.0, "F", 10.0),
///     TextSpan::new("b".into(), 50.0, 200.0, 8.0, "F", 10.0),
/// ];
/// let bbox = spans_bbox(&spans).unwrap();
/// assert!((bbox.x_min - 10.0).abs() < 1e-9);
/// assert!((bbox.x_max - 58.0).abs() < 1e-9); // 50 + 8
/// assert!((bbox.y_min - 100.0).abs() < 1e-9);
/// assert!((bbox.y_max - 200.0).abs() < 1e-9);
/// ```
pub fn spans_bbox(spans: &[TextSpan]) -> Option<BoundingBox> {
    if spans.is_empty() {
        return None;
    }
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
    if !min_x.is_finite() || !max_x.is_finite() || !min_y.is_finite() || !max_y.is_finite() {
        return None;
    }
    Some(BoundingBox::new(min_x, min_y, max_x, max_y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn span(text: &str, x: f64, y: f64, width: f64) -> TextSpan {
        TextSpan::new(text.into(), x, y, width, Arc::<str>::from("F"), 10.0)
    }

    fn line(baseline: f64) -> TextLine {
        TextLine {
            spans: vec![span("x", 0.0, baseline, 5.0)],
            baseline,
            is_vertical: false,
        }
    }

    // ---- cluster_by_baseline ----

    #[test]
    fn cluster_empty() {
        let spans: Vec<TextSpan> = Vec::new();
        assert!(cluster_by_baseline(&spans, 2.0).is_empty());
    }

    #[test]
    fn cluster_single_span() {
        let spans = vec![span("hi", 0.0, 100.0, 10.0)];
        let groups = cluster_by_baseline(&spans, 2.0);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 1);
        assert_eq!(groups[0][0].text, "hi");
    }

    #[test]
    fn cluster_two_spans_same_baseline() {
        let spans = vec![span("a", 0.0, 100.0, 10.0), span("b", 20.0, 100.5, 10.0)];
        let groups = cluster_by_baseline(&spans, 2.0);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
    }

    #[test]
    fn cluster_two_spans_different_baselines() {
        let spans = vec![span("a", 0.0, 100.0, 10.0), span("b", 0.0, 80.0, 10.0)];
        let groups = cluster_by_baseline(&spans, 2.0);
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn cluster_tolerance_just_above() {
        // Distance = 2.001, tolerance 2.0 -> different lines.
        let spans = vec![span("a", 0.0, 100.0, 10.0), span("b", 0.0, 102.001, 10.0)];
        let groups = cluster_by_baseline(&spans, 2.0);
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn cluster_tolerance_just_below() {
        // Distance = 1.999, tolerance 2.0 -> same line.
        let spans = vec![span("a", 0.0, 100.0, 10.0), span("b", 0.0, 101.999, 10.0)];
        let groups = cluster_by_baseline(&spans, 2.0);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
    }

    #[test]
    fn cluster_three_baselines_preserves_order() {
        let spans = vec![
            span("a", 0.0, 100.0, 5.0),
            span("b", 0.0, 80.0, 5.0),
            span("c", 0.0, 60.0, 5.0),
            span("d", 10.0, 100.0, 5.0),
            span("e", 10.0, 80.0, 5.0),
        ];
        let groups = cluster_by_baseline(&spans, 2.0);
        assert_eq!(groups.len(), 3);
        // First-seen baseline determines group index.
        assert_eq!(groups[0][0].text, "a");
        assert_eq!(groups[0][1].text, "d");
        assert_eq!(groups[1][0].text, "b");
        assert_eq!(groups[1][1].text, "e");
        assert_eq!(groups[2][0].text, "c");
    }

    #[test]
    fn cluster_skips_whitespace_only_spans() {
        let spans = vec![
            span("a", 0.0, 100.0, 5.0),
            span(" ", 5.0, 100.0, 2.0),
            span("\t\n", 7.0, 100.0, 2.0),
            span("b", 9.0, 100.0, 5.0),
        ];
        let groups = cluster_by_baseline(&spans, 2.0);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2); // only "a" and "b"
    }

    #[test]
    fn cluster_zero_tolerance_requires_exact_match() {
        let spans = vec![span("a", 0.0, 100.0, 5.0), span("b", 0.0, 100.0, 5.0)];
        let groups = cluster_by_baseline(&spans, 0.0);
        assert_eq!(groups.len(), 1);
    }

    #[test]
    fn cluster_zero_tolerance_splits_on_any_jitter() {
        let spans = vec![span("a", 0.0, 100.0, 5.0), span("b", 0.0, 100.0001, 5.0)];
        let groups = cluster_by_baseline(&spans, 0.0);
        assert_eq!(groups.len(), 2);
    }

    // ---- detect_gaps ----

    #[test]
    fn gaps_empty() {
        let lines: Vec<TextLine> = Vec::new();
        assert!(detect_gaps(&lines, 10.0).is_empty());
    }

    #[test]
    fn gaps_single_line() {
        let lines = vec![line(100.0)];
        assert_eq!(detect_gaps(&lines, 10.0), vec![0..1]);
    }

    #[test]
    fn gaps_no_gap() {
        // Even spacing of 12 pt; min_gap 20 -> no breaks.
        let lines = vec![line(700.0), line(688.0), line(676.0), line(664.0)];
        assert_eq!(detect_gaps(&lines, 20.0), vec![0..4]);
    }

    #[test]
    fn gaps_one_gap() {
        // Gap of 50 between idx 1 and 2.
        let lines = vec![line(700.0), line(688.0), line(638.0), line(626.0)];
        assert_eq!(detect_gaps(&lines, 20.0), vec![0..2, 2..4]);
    }

    #[test]
    fn gaps_multiple_gaps() {
        let lines = vec![
            line(700.0), // section 1
            line(640.0), // gap 60 -> section 2
            line(630.0),
            line(560.0), // gap 70 -> section 3
            line(548.0),
        ];
        assert_eq!(detect_gaps(&lines, 20.0), vec![0..1, 1..3, 3..5]);
    }

    #[test]
    fn gaps_threshold_just_above_real_gap() {
        // Real gap is 30; threshold 30.0 -> NOT a break (strictly greater).
        let lines = vec![line(700.0), line(670.0)];
        assert_eq!(detect_gaps(&lines, 30.0), vec![0..2]);
    }

    #[test]
    fn gaps_threshold_just_below_real_gap() {
        // Real gap 30; threshold 29.9 -> break.
        let lines = vec![line(700.0), line(670.0)];
        assert_eq!(detect_gaps(&lines, 29.9), vec![0..1, 1..2]);
    }

    // ---- filter_bbox ----

    #[test]
    fn filter_empty() {
        let spans: Vec<TextSpan> = Vec::new();
        let bbox = BoundingBox::new(0.0, 0.0, 100.0, 100.0);
        assert!(filter_bbox(&spans, bbox).is_empty());
    }

    #[test]
    fn filter_no_overlap() {
        let spans = vec![span("out", 200.0, 200.0, 10.0)];
        let bbox = BoundingBox::new(0.0, 0.0, 100.0, 100.0);
        assert!(filter_bbox(&spans, bbox).is_empty());
    }

    #[test]
    fn filter_full_inside() {
        let spans = vec![span("in", 50.0, 50.0, 10.0)];
        let bbox = BoundingBox::new(0.0, 0.0, 100.0, 100.0);
        let kept = filter_bbox(&spans, bbox);
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn filter_on_boundary_is_inside() {
        let spans = vec![span("edge", 0.0, 0.0, 5.0)];
        let bbox = BoundingBox::new(0.0, 0.0, 100.0, 100.0);
        assert_eq!(filter_bbox(&spans, bbox).len(), 1);
    }

    #[test]
    fn filter_anchor_outside_extent_inside_excluded() {
        // Anchor at x=-10 is outside the bbox even though width crosses
        // into it. filter_bbox uses anchor-point semantics by design.
        let spans = vec![span("partial", -10.0, 50.0, 50.0)];
        let bbox = BoundingBox::new(0.0, 0.0, 100.0, 100.0);
        assert_eq!(filter_bbox(&spans, bbox).len(), 0);
    }

    #[test]
    fn filter_zero_area_bbox() {
        // Degenerate bbox at a single point matches only spans whose
        // anchor sits exactly on it.
        let bbox = BoundingBox::new(50.0, 50.0, 50.0, 50.0);
        let spans = vec![span("hit", 50.0, 50.0, 5.0), span("miss", 51.0, 50.0, 5.0)];
        let kept = filter_bbox(&spans, bbox);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].text, "hit");
    }

    #[test]
    fn filter_inverted_bbox_is_normalized() {
        // BoundingBox::new normalizes inverted coords; filter still works.
        let bbox = BoundingBox::new(100.0, 100.0, 0.0, 0.0);
        let spans = vec![span("in", 50.0, 50.0, 5.0)];
        assert_eq!(filter_bbox(&spans, bbox).len(), 1);
    }

    #[test]
    fn filter_preserves_input_order() {
        let spans = vec![
            span("a", 10.0, 10.0, 5.0),
            span("skip", 200.0, 200.0, 5.0),
            span("b", 20.0, 10.0, 5.0),
            span("c", 30.0, 10.0, 5.0),
        ];
        let bbox = BoundingBox::new(0.0, 0.0, 100.0, 100.0);
        let kept = filter_bbox(&spans, bbox);
        assert_eq!(kept.len(), 3);
        assert_eq!(kept[0].text, "a");
        assert_eq!(kept[1].text, "b");
        assert_eq!(kept[2].text, "c");
    }

    // ---- spans_bbox ----

    #[test]
    fn spans_bbox_empty() {
        let spans: Vec<TextSpan> = Vec::new();
        assert!(spans_bbox(&spans).is_none());
    }

    #[test]
    fn spans_bbox_single_span() {
        let spans = vec![span("x", 10.0, 100.0, 8.0)];
        let bbox = spans_bbox(&spans).unwrap();
        assert!((bbox.x_min - 10.0).abs() < 1e-9);
        assert!((bbox.x_max - 18.0).abs() < 1e-9);
        assert!((bbox.y_min - 100.0).abs() < 1e-9);
        assert!((bbox.y_max - 100.0).abs() < 1e-9);
    }

    #[test]
    fn spans_bbox_clamps_negative_width() {
        let spans = vec![span("a", 10.0, 100.0, -5.0), span("b", 20.0, 100.0, 5.0)];
        let bbox = spans_bbox(&spans).unwrap();
        // Negative width clamped to 0; max extent = 25 (b right edge).
        assert!((bbox.x_max - 25.0).abs() < 1e-9);
    }

    #[test]
    fn spans_bbox_union() {
        let spans = vec![
            span("a", 10.0, 100.0, 5.0),
            span("b", 50.0, 200.0, 8.0),
            span("c", 30.0, 50.0, 5.0),
        ];
        let bbox = spans_bbox(&spans).unwrap();
        assert!((bbox.x_min - 10.0).abs() < 1e-9);
        assert!((bbox.x_max - 58.0).abs() < 1e-9);
        assert!((bbox.y_min - 50.0).abs() < 1e-9);
        assert!((bbox.y_max - 200.0).abs() < 1e-9);
    }

    #[test]
    fn spans_bbox_nan_returns_none() {
        let spans = vec![span("x", f64::NAN, f64::NAN, 5.0)];
        assert!(spans_bbox(&spans).is_none());
    }

    // ---- Integration-style smoke tests ----

    #[test]
    fn integration_cluster_then_filter() {
        // Build a tiny "page": three lines, four spans each. Filter to
        // the upper half via filter_bbox, then cluster the result.
        let mut spans = Vec::new();
        for &y in &[700.0, 600.0, 500.0] {
            for x in [0.0, 50.0, 100.0, 150.0] {
                spans.push(span("w", x, y, 30.0));
            }
        }
        let upper = BoundingBox::new(0.0, 599.0, 200.0, 800.0);
        let kept = filter_bbox(&spans, upper);
        assert_eq!(kept.len(), 8); // top two rows

        // Re-cluster the kept refs through a copy (cluster takes &[TextSpan]).
        let owned: Vec<TextSpan> = kept.into_iter().cloned().collect();
        let groups = cluster_by_baseline(&owned, 2.0);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 4);
        assert_eq!(groups[1].len(), 4);
    }

    #[test]
    fn integration_lines_then_gaps() {
        // Build TextLines for a page with two visual sections.
        let lines = vec![
            line(720.0),
            line(708.0),
            line(696.0),
            // gap of 80 here
            line(616.0),
            line(604.0),
        ];
        let sections = detect_gaps(&lines, 50.0);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0], 0..3);
        assert_eq!(sections[1], 3..5);
    }
}

//! Edge detection from segments.
//!
//! Edges are groups of nearby parallel segments at approximately the same
//! position. They represent the visible boundaries of strokes (stem edges,
//! serifs, etc.) and are the units that get grid-fitted.

use super::segments::{AnalysisContour, Dimension, Segment};

/// Edge flags for classification.
pub(crate) const EDGE_STRONG: u8 = 0x01;
pub(crate) const EDGE_SERIF: u8 = 0x02;

/// An edge: a group of aligned segments at approximately the same position.
#[derive(Debug, Clone)]
pub struct Edge {
    /// Position in font units (along the controlling axis).
    pub pos: f64,
    /// Dimension this edge controls.
    pub dim: Dimension,
    /// Fitted position after grid-fitting (set by fitting phase).
    pub fitted_pos: f64,
    /// Whether this edge has been grid-fitted.
    pub fitted: bool,
    /// Blue zone this edge touches, if any. Index into GlobalMetrics.blue_zones.
    pub blue_zone: Option<usize>,
    /// Linked edge (opposite side of a stem). Index into edge array.
    pub link: Option<usize>,
    /// Serif base edge. Index into edge array.
    pub serif: Option<usize>,
    /// Classification flags (EDGE_STRONG, EDGE_SERIF).
    pub flags: u8,
    /// Number of segments in this edge.
    pub segment_count: usize,
    /// FreeType `AF_EDGE_ROUND`. An edge is round when at least half of
    /// its segments are round (curve-derived). Stems of curves (e.g. the
    /// left and right apices of an 'o') get marked round and the fitter
    /// uses a softer positioning rule that preserves curvature rather
    /// than snapping apex positions to integer pixels. Derived from
    /// aflatin.c:2442-2446.
    pub is_round: bool,
    /// Semantic crossbar flag. Set when this horizontal edge bounds a
    /// horizontal stroke that has glyph ink both above and below, within
    /// the stroke's x-extent. Examples: the crossbars of 'e', 'f', 't',
    /// 'A', 'H', 'e' (at any angle), the middle arm of 'E'. The top of
    /// 'o' is NOT a crossbar (no ink above); neither is the baseline of
    /// 'n' (no ink below). Used by the fitter to apply a size-gated
    /// stem-darkening bonus (+1 device pixel of width) so the crossbar
    /// survives binarization at body-text ppem. Populated by
    /// `classify_crossbars` after edge linking. Only ever set on
    /// `Dimension::Horizontal` edges.
    pub is_crossbar: bool,
}

/// Detect edges from a list of segments.
///
/// Groups nearby segments into edges, links edges via segment links,
/// and classifies edges as strong, weak, or serif.
pub fn detect_edges(segments: &mut [Segment], dim: Dimension, scale: f64) -> Vec<Edge> {
    let mut edges = Vec::new();
    let mut dim_indices = Vec::new();
    let mut link_counts = Vec::new();
    let mut serif_counts = Vec::new();
    detect_edges_into(
        segments,
        dim,
        scale,
        &mut edges,
        &mut dim_indices,
        &mut link_counts,
        &mut serif_counts,
    );
    edges
}

/// Like [`detect_edges`] but writes into caller-supplied buffers so the
/// hot path reuses memory across glyph calls.
///
/// `edges`, `dim_indices`, `link_counts`, and `serif_counts` are cleared
/// on entry; capacity is preserved.
pub(crate) fn detect_edges_into(
    segments: &mut [Segment],
    dim: Dimension,
    scale: f64,
    edges: &mut Vec<Edge>,
    dim_indices: &mut Vec<usize>,
    link_counts: &mut Vec<usize>,
    serif_counts: &mut Vec<usize>,
) {
    edges.clear();
    dim_indices.clear();
    link_counts.clear();
    serif_counts.clear();
    if segments.is_empty() {
        return;
    }

    // Filter segments for this dimension.
    dim_indices.extend((0..segments.len()).filter(|&i| segments[i].dim == dim));

    if dim_indices.is_empty() {
        return;
    }

    // Sort by position.
    dim_indices.sort_by(|&a, &b| {
        segments[a]
            .pos
            .partial_cmp(&segments[b].pos)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Group segments within threshold into edges.
    // FreeType uses 0.25 pixels converted to font units.
    let threshold = if scale > 0.0 { 0.25 / scale } else { 10.0 };

    for &seg_idx in dim_indices.iter() {
        let seg_pos = segments[seg_idx].pos;

        // Check if this segment belongs to the last edge.
        let belongs_to_last = if let Some(last_edge) = edges.last() {
            (seg_pos - last_edge.pos).abs() <= threshold
        } else {
            false
        };

        if belongs_to_last {
            let edge = edges.last_mut().unwrap();
            // Update edge position as weighted average.
            let total = edge.segment_count as f64;
            edge.pos = (edge.pos * total + seg_pos) / (total + 1.0);
            edge.segment_count += 1;
        } else {
            let edge_idx = edges.len();
            edges.push(Edge {
                pos: seg_pos,
                dim,
                fitted_pos: seg_pos, // will be overwritten by fitting
                fitted: false,
                blue_zone: None,
                link: None,
                serif: None,
                flags: 0,
                segment_count: 1,
                is_round: false,    // populated below after grouping is done
                is_crossbar: false, // populated by classify_crossbars
            });
            segments[seg_idx].edge = Some(edge_idx);
        }

        // Record which edge this segment belongs to.
        let edge_idx = edges.len() - 1;
        segments[seg_idx].edge = Some(edge_idx);
    }

    // Propagate round flag: an edge is round when the majority of its
    // constituent segments are round (aflatin.c:2442-2446). Curve
    // apexes produce round segments; stem sides do not.
    propagate_round_flag(edges, segments);

    // Link edges via segment links. Reuses scratch count buffers.
    link_edges_with(edges, segments, link_counts, serif_counts);

    // Classify edges.
    classify_edges(edges, segments);
}

/// Aggregate per-segment `is_round` flags onto each edge. Mirrors
/// `edge->flags |= AF_EDGE_ROUND` in aflatin.c around line 2446.
fn propagate_round_flag(edges: &mut [Edge], segments: &[Segment]) {
    for (edge_idx, edge) in edges.iter_mut().enumerate() {
        let mut round = 0usize;
        let mut straight = 0usize;
        for seg in segments.iter() {
            if seg.edge == Some(edge_idx) {
                if seg.is_round {
                    round += 1;
                } else {
                    straight += 1;
                }
            }
        }
        // FreeType: "if (is_round > 0 && is_round >= is_straight)".
        edge.is_round = round > 0 && round >= straight;
    }
}

/// Link edges based on segment-level links and serif refs.
///
/// If most of an edge's segments link to segments in edge B, edge->link = B.
/// If segments have a `serif` ref instead (non-mutual link demoted in
/// `link_segments`) and no stem link survives, edge->serif and
/// EDGE_SERIF flag get set. Mirrors FT's edge-link resolution
/// (aflatin.c:2388-2436) at the tally level.
///
/// Takes caller-supplied `link_counts` and `serif_counts` buffers so the
/// per-edge inner loop does not allocate. Previously each edge allocated
/// two fresh `Vec<usize>` of length `n`, producing an `O(n)`-allocation
/// burst on every glyph fitting call.
pub(crate) fn link_edges_with(
    edges: &mut [Edge],
    segments: &[Segment],
    link_counts: &mut Vec<usize>,
    serif_counts: &mut Vec<usize>,
) {
    let n = edges.len();
    if n < 2 {
        return;
    }

    // Allocate/resize scratch once for all edges.
    link_counts.clear();
    link_counts.resize(n, 0);
    serif_counts.clear();
    serif_counts.resize(n, 0);

    #[allow(clippy::needless_range_loop)]
    for edge_idx in 0..n {
        // Zero the per-edge scratch. These are Vec<usize> of length n, so
        // fill is a single memset.
        for c in link_counts.iter_mut() {
            *c = 0;
        }
        for c in serif_counts.iter_mut() {
            *c = 0;
        }

        for seg in segments.iter() {
            if seg.edge != Some(edge_idx) {
                continue;
            }
            if let Some(linked_seg) = seg.link {
                if linked_seg < segments.len() {
                    if let Some(linked_edge) = segments[linked_seg].edge {
                        if linked_edge < n && linked_edge != edge_idx {
                            link_counts[linked_edge] += 1;
                        }
                    }
                }
            } else if let Some(serif_seg) = seg.serif {
                if serif_seg < segments.len() {
                    if let Some(serif_edge) = segments[serif_seg].edge {
                        if serif_edge < n && serif_edge != edge_idx {
                            serif_counts[serif_edge] += 1;
                        }
                    }
                }
            }
        }

        let (best_edge, best_count) = link_counts
            .iter()
            .enumerate()
            .max_by_key(|&(_, &count)| count)
            .unwrap_or((0, &0));
        if *best_count > 0 {
            edges[edge_idx].link = Some(best_edge);
        }

        // Only attach a serif ref when we don't have a stem link.
        if edges[edge_idx].link.is_none() {
            let (best_serif, best_serif_count) = serif_counts
                .iter()
                .enumerate()
                .max_by_key(|&(_, &count)| count)
                .unwrap_or((0, &0));
            if *best_serif_count > 0 {
                edges[edge_idx].serif = Some(best_serif);
                edges[edge_idx].flags |= EDGE_SERIF;
            }
        }
    }
}

/// Populate `Edge::is_crossbar` by semantic analysis of the outline topology.
///
/// A horizontal edge is a "crossbar" candidate when it bounds a horizontal
/// stroke (linked pair of horizontal edges) and, within the stroke's x-extent,
/// glyph ink exists both above the top edge and below the bottom edge. This
/// captures the mid-stroke of letters like 'e', 'f', 't', 'A', 'H', the middle
/// arm of 'E', etc. It rejects the top of 'o' (no ink above), the baseline of
/// 'n' (no ink below), and isolated horizontal strokes like the hyphen.
///
/// Only operates on `Dimension::Horizontal` edges. V-dim edges (X-axis stems)
/// are left unchanged.
///
/// Implementation notes:
/// - Uses outline points directly (not segment midpoints) so off-curve bezier
///   control points still count as ink.
/// - Requires at least one point strictly above/below the stroke, separated
///   by ~1 device pixel in font units, to suppress noise from jitter at the
///   stroke's own endpoints.
pub(crate) fn classify_crossbars(
    edges: &mut [Edge],
    segments: &[Segment],
    contours: &[AnalysisContour],
    scale: f64,
) {
    if edges.is_empty() || scale <= 0.0 {
        return;
    }
    // Require ~1 device pixel of vertical separation in font units so that
    // points sharing the stroke's own y-range don't count as "above/below".
    let eps_fu = if scale > 0.0 { 1.0 / scale } else { 1.0 };

    // Compute glyph x-extent once. Used below to require that a crossbar
    // stroke span a substantial fraction of the glyph. This rejects 'a'
    // (whose mid-bowl hook spans ~46% of glyph width and would otherwise
    // trigger e-like darkening, producing an a->e OCR regression) while
    // accepting 'e', 'E', 'H', 'f', 't' (which all span >= 55%).
    let (glyph_xmin, glyph_xmax) = {
        let mut lo = f64::MAX;
        let mut hi = f64::MIN;
        for c in contours {
            for p in &c.points {
                if p.x < lo {
                    lo = p.x;
                }
                if p.x > hi {
                    hi = p.x;
                }
            }
        }
        (lo, hi)
    };
    let glyph_w = (glyph_xmax - glyph_xmin).max(1.0);
    const MIN_CROSSBAR_SPAN_FRACTION: f64 = 0.55;

    let n = edges.len();
    for i in 0..n {
        if edges[i].dim != Dimension::Horizontal {
            continue;
        }
        // A crossbar is a linked pair. Only process each pair once: take the
        // bottom edge (smaller y).
        let j = match edges[i].link {
            Some(j) if j < n => j,
            _ => continue,
        };
        if edges[j].dim != Dimension::Horizontal {
            continue;
        }
        if edges[i].pos > edges[j].pos {
            // i is the top edge; handle from j's side.
            continue;
        }
        if (edges[i].pos - edges[j].pos).abs() < 0.5 {
            continue; // degenerate
        }
        // Serif-flagged edges: not crossbar candidates.
        if edges[i].flags & EDGE_SERIF != 0 || edges[j].flags & EDGE_SERIF != 0 {
            continue;
        }

        // Round-on-both-sides pairs are curve apices, not horizontal strokes
        // (e.g. the top of 'o', the bottom of 'g' where the bowl meets the
        // descender). A real crossbar has a flat stroke; at least one edge
        // must be non-round. This protects curved features from being
        // over-darkened.
        if edges[i].is_round && edges[j].is_round {
            continue;
        }

        // Union x-extent from segments belonging to either edge.
        let mut x_min = f64::MAX;
        let mut x_max = f64::MIN;
        for seg in segments.iter() {
            match seg.edge {
                Some(e) if e == i || e == j => {
                    if seg.min_coord < x_min {
                        x_min = seg.min_coord;
                    }
                    if seg.max_coord > x_max {
                        x_max = seg.max_coord;
                    }
                }
                _ => {}
            }
        }
        if !(x_min.is_finite() && x_max.is_finite()) || x_max - x_min <= 0.0 {
            continue;
        }

        // Crossbar must span a substantial fraction of the glyph width.
        // Rejects localised horizontal strokes like the upper-right hook of
        // 'a' (46% of glyph width) while accepting real crossbars of
        // 'e'/'E'/'H'/'f'/'t' (>= 55%).
        if (x_max - x_min) < glyph_w * MIN_CROSSBAR_SPAN_FRACTION {
            continue;
        }

        let bot_y = edges[i].pos; // smaller
        let top_y = edges[j].pos; // larger

        // Scan outline points for ink above top_y and below bot_y within
        // [x_min, x_max]. Both must exist to flag as crossbar.
        let mut has_above = false;
        let mut has_below = false;
        for contour in contours {
            for p in &contour.points {
                if p.x < x_min || p.x > x_max {
                    continue;
                }
                if !has_above && p.y > top_y + eps_fu {
                    has_above = true;
                }
                if !has_below && p.y < bot_y - eps_fu {
                    has_below = true;
                }
                if has_above && has_below {
                    break;
                }
            }
            if has_above && has_below {
                break;
            }
        }

        if has_above && has_below {
            edges[i].is_crossbar = true;
            edges[j].is_crossbar = true;
        }
    }
}

/// Classify edges as strong, weak, or serif.
fn classify_edges(edges: &mut [Edge], segments: &[Segment]) {
    for edge_idx in 0..edges.len() {
        // Strong: edge has segments from multiple contours. Scan segments
        // once, tracking only the first contour seen and whether any other
        // contour shows up. This avoids a per-edge `HashSet` allocation
        // that used to dominate the hinter's heap traffic on glyphs with
        // many edges.
        let mut first_contour: Option<usize> = None;
        let mut multi_contour = false;
        for seg in segments.iter() {
            if seg.edge != Some(edge_idx) {
                continue;
            }
            match first_contour {
                None => first_contour = Some(seg.contour),
                Some(c) if c != seg.contour => {
                    multi_contour = true;
                    break;
                }
                _ => {}
            }
        }
        if multi_contour {
            edges[edge_idx].flags |= EDGE_STRONG;
        }

        // Serif: single-segment edge that links to another edge which also
        // links elsewhere. This pattern indicates a serif attachment.
        if edges[edge_idx].segment_count == 1 {
            if let Some(linked) = edges[edge_idx].link {
                if let Some(other_link) = edges[linked].link {
                    if other_link != edge_idx {
                        edges[edge_idx].serif = Some(linked);
                        edges[edge_idx].flags |= EDGE_SERIF;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto_hinter::segments;

    #[test]
    fn detect_two_edges_from_rectangle() {
        let pts: Vec<(f64, f64, bool)> = vec![
            (0.0, 0.0, true),
            (0.0, 700.0, true),
            (100.0, 700.0, true),
            (100.0, 0.0, true),
        ];
        let contours = segments::tuples_to_contours(&[pts]);
        let mut segs = segments::detect_segments(&contours, Dimension::Vertical, 1000);
        let scale = 0.04; // ~40px font size at 1000 UPM
        let edges = detect_edges(&mut segs, Dimension::Vertical, scale);

        assert!(
            edges.len() >= 2,
            "expected at least 2 vertical edges, got {}",
            edges.len()
        );

        // Check that edges are linked (opposite sides of the stem).
        let has_link = edges.iter().any(|e| e.link.is_some());
        assert!(has_link, "expected at least one linked edge pair");

        // Straight rectangle stem: no edge should be round.
        assert!(
            edges.iter().all(|e| !e.is_round),
            "straight-stem edges must not be flagged round"
        );
    }

    /// A linked pair of horizontal edges at y=330 (bottom) and y=370 (top)
    /// with outline ink both above y=370 (stems extending to the cap line)
    /// and below y=330 (stems extending to the baseline) must flag both
    /// edges as crossbars. This is the core shape of an 'H' mid-stroke,
    /// 'e' crossbar, middle arm of 'E', etc.
    #[test]
    fn crossbar_classifier_flags_mid_stroke_with_ink_above_and_below() {
        use crate::auto_hinter::segments::{tuples_to_contours, Dimension, Segment};

        // Mid-stroke's two horizontal edges as directly-constructed
        // segments, so we don't fight segment detection on the toy test
        // fixture. Each segment represents the bottom (y=330) or top
        // (y=370) of the bar, spanning x in [0, 240].
        let mut segs = vec![
            Segment {
                dir: crate::auto_hinter::segments::Direction::Left,
                dim: Dimension::Horizontal,
                contour: 0,
                pos: 330.0,
                min_coord: 0.0,
                max_coord: 240.0,
                link: Some(1),
                score: 0.0,
                edge: None,
                is_round: false,
                serif: None,
            },
            Segment {
                dir: crate::auto_hinter::segments::Direction::Right,
                dim: Dimension::Horizontal,
                contour: 0,
                pos: 370.0,
                min_coord: 0.0,
                max_coord: 240.0,
                link: Some(0),
                score: 0.0,
                edge: None,
                is_round: false,
                serif: None,
            },
        ];
        let scale = 0.016; // 16 ppem at 1000 UPM
        let mut edges = detect_edges(&mut segs, Dimension::Horizontal, scale);
        assert_eq!(edges.len(), 2, "expected 2 edges (top + bottom of bar)");

        // Outline points representing left+right stems extending above and
        // below the bar. Classifier only cares about point topology within
        // x in [0, 240]; we provide points strictly above 370 and strictly
        // below 330.
        let outline: Vec<(f64, f64, bool)> = vec![
            // Left stem going up from bar top to cap.
            (0.0, 500.0, true),
            (40.0, 500.0, true),
            // Right stem going up from bar top to cap.
            (200.0, 500.0, true),
            (240.0, 500.0, true),
            // Left stem going down from bar bottom to baseline.
            (0.0, 100.0, true),
            (40.0, 100.0, true),
            // Right stem going down from bar bottom to baseline.
            (200.0, 100.0, true),
            (240.0, 100.0, true),
        ];
        let contours = tuples_to_contours(&[outline]);

        classify_crossbars(&mut edges, &segs, &contours, scale);

        assert!(
            edges.iter().all(|e| e.is_crossbar),
            "both edges should be flagged, got: {:?}",
            edges
                .iter()
                .map(|e| (e.pos, e.is_crossbar))
                .collect::<Vec<_>>()
        );
    }

    /// A linked pair of horizontal edges with ink only below (e.g. top of
    /// 'o': outer top at y=500 and inner top at y=400, ink only below)
    /// must NOT flag as crossbar.
    #[test]
    fn crossbar_classifier_rejects_top_of_o() {
        use crate::auto_hinter::segments::{tuples_to_contours, Dimension, Segment};

        let mut segs = vec![
            Segment {
                dir: crate::auto_hinter::segments::Direction::Left,
                dim: Dimension::Horizontal,
                contour: 0,
                pos: 400.0, // inner-top of ring
                min_coord: 50.0,
                max_coord: 150.0,
                link: Some(1),
                score: 0.0,
                edge: None,
                is_round: false,
                serif: None,
            },
            Segment {
                dir: crate::auto_hinter::segments::Direction::Right,
                dim: Dimension::Horizontal,
                contour: 0,
                pos: 500.0, // outer-top of ring
                min_coord: 0.0,
                max_coord: 200.0,
                link: Some(0),
                score: 0.0,
                edge: None,
                is_round: false,
                serif: None,
            },
        ];
        let scale = 0.016;
        let mut edges = detect_edges(&mut segs, Dimension::Horizontal, scale);

        // Outline points: ring has ink below the top (outer sides down to
        // baseline) but nothing above the outer top.
        let outline: Vec<(f64, f64, bool)> = vec![
            (0.0, 250.0, true), // outer-left mid
            (0.0, 0.0, true),   // outer-left bottom
            (200.0, 0.0, true), // outer-right bottom
            (200.0, 250.0, true),
            (50.0, 100.0, true), // inner-left bottom
            (150.0, 100.0, true),
        ];
        let contours = tuples_to_contours(&[outline]);

        classify_crossbars(&mut edges, &segs, &contours, scale);

        assert!(
            edges.iter().all(|e| !e.is_crossbar),
            "'o'-top edges must not flag as crossbars, got: {:?}",
            edges
                .iter()
                .map(|e| (e.pos, e.is_crossbar))
                .collect::<Vec<_>>()
        );
    }

    /// A linked pair with ink above but not below (e.g. baseline edge of
    /// an 'n': baseline at y=0 links to something just above; stems go up
    /// from there but nothing is below the baseline) must NOT flag as
    /// crossbar.
    #[test]
    fn crossbar_classifier_rejects_baseline() {
        use crate::auto_hinter::segments::{tuples_to_contours, Dimension, Segment};

        // Linked pair at y=0 (baseline) and y=40 (just above).
        let mut segs = vec![
            Segment {
                dir: crate::auto_hinter::segments::Direction::Left,
                dim: Dimension::Horizontal,
                contour: 0,
                pos: 0.0,
                min_coord: 0.0,
                max_coord: 100.0,
                link: Some(1),
                score: 0.0,
                edge: None,
                is_round: false,
                serif: None,
            },
            Segment {
                dir: crate::auto_hinter::segments::Direction::Right,
                dim: Dimension::Horizontal,
                contour: 0,
                pos: 40.0,
                min_coord: 0.0,
                max_coord: 100.0,
                link: Some(0),
                score: 0.0,
                edge: None,
                is_round: false,
                serif: None,
            },
        ];
        let scale = 0.016;
        let mut edges = detect_edges(&mut segs, Dimension::Horizontal, scale);

        // Ink above only (stems extending up).
        let outline: Vec<(f64, f64, bool)> = vec![(0.0, 500.0, true), (100.0, 500.0, true)];
        let contours = tuples_to_contours(&[outline]);

        classify_crossbars(&mut edges, &segs, &contours, scale);

        assert!(
            edges.iter().all(|e| !e.is_crossbar),
            "baseline edge-pair must not flag as crossbar, got: {:?}",
            edges
                .iter()
                .map(|e| (e.pos, e.is_crossbar))
                .collect::<Vec<_>>()
        );
    }

    /// An approximate 'o' outer-left curve apex (three-point control cluster)
    /// should yield a round edge once segments are grouped. Mirrors the
    /// round-classification path for the whole pipeline.
    #[test]
    fn round_flag_propagates_segments_to_edge() {
        let pts: Vec<(f64, f64, bool)> = vec![
            (200.0, 500.0, true),
            (100.0, 500.0, false),
            (50.0, 400.0, false),
            (50.0, 250.0, true),
            (50.0, 100.0, false),
            (100.0, 0.0, false),
            (200.0, 0.0, true),
        ];
        let contours = segments::tuples_to_contours(&[pts]);
        let mut segs = segments::detect_segments(&contours, Dimension::Vertical, 1000);
        let scale = 0.04;
        let edges = detect_edges(&mut segs, Dimension::Vertical, scale);

        // At least one round edge should be present for the curve apex.
        assert!(
            edges.iter().any(|e| e.is_round),
            "expected at least one round edge from curve apex, got: {edges:?}"
        );
    }
}

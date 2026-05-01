//! Segment detection from glyph outline topology.
//!
//! FreeType-compatible approach: computes out_dir (tangent direction to next
//! point) on every outline point including off-curve control points. Segments
//! are runs of consecutive points with the same axis-aligned out_dir.
//!
//! Segments are linked into stems using FreeType's scoring formula and fill
//! convention checks. Delta filtering removes false-positive segments.

use super::latin;

/// Axis along which a segment is aligned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    /// Segment runs horizontally (contributes to horizontal stems).
    Horizontal,
    /// Segment runs vertically (contributes to vertical stems).
    Vertical,
}

/// Direction of travel along a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Upward (increasing Y).
    Up,
    /// Downward (decreasing Y).
    Down,
    /// Leftward (decreasing X).
    Left,
    /// Rightward (increasing X).
    Right,
    /// Below the straight-angle threshold, treated as undirected.
    None,
}

impl Direction {
    fn opposite(self) -> Self {
        match self {
            Direction::Up => Direction::Down,
            Direction::Down => Direction::Up,
            Direction::Left => Direction::Right,
            Direction::Right => Direction::Left,
            Direction::None => Direction::None,
        }
    }

    fn matches_dimension(self, dim: Dimension) -> bool {
        match dim {
            Dimension::Vertical => matches!(self, Direction::Up | Direction::Down),
            Dimension::Horizontal => matches!(self, Direction::Left | Direction::Right),
        }
    }
}

/// A point with computed tangent direction for segment analysis.
/// FreeType's AF_PointRec equivalent (simplified).
#[derive(Debug, Clone)]
pub struct AnalysisPoint {
    /// X coordinate (font units).
    pub x: f64,
    /// Y coordinate (font units).
    pub y: f64,
    /// Direction of the tangent vector to the NEXT point.
    pub out_dir: Direction,
    /// True when this is an on-curve point, false for bezier control points.
    /// FreeType's `AF_FLAG_CONTROL` is the inverse. Tracked to implement
    /// the `AF_EDGE_ROUND` heuristic: segments whose extreme points are
    /// control points get flagged as round.
    pub on_curve: bool,
}

/// A detected segment.
#[derive(Debug, Clone)]
pub struct Segment {
    /// Direction of travel along the segment.
    pub dir: Direction,
    /// Controlling dimension (Horizontal or Vertical).
    pub dim: Dimension,
    /// Index of the originating contour.
    pub contour: usize,
    /// Position along the controlling axis (midpoint of min/max).
    pub pos: f64,
    /// Low extreme along the cross axis.
    pub min_coord: f64,
    /// High extreme along the cross axis.
    pub max_coord: f64,
    /// Index of the linked partner segment (stem pair), if any.
    pub link: Option<usize>,
    /// Linking score (FreeType-style); lower is better.
    pub score: f64,
    /// Edge assignment produced by the edge-grouping pass.
    pub edge: Option<usize>,
    /// FreeType `AF_EDGE_ROUND` flag: set when one or both extreme points
    /// on this segment are bezier control points and the on-curve coord
    /// span is below the UPM/14 flat threshold. Curve bulges get flagged
    /// as round; straight stem sides do not. Anchor fitting treats round
    /// edges differently so curve apexes don't get pixel-snapped like
    /// stems. Ported from aflatin.c:1698-1700.
    pub is_round: bool,
    /// Serif-attachment ref (aflatin.c:2107-2120). Populated when our
    /// `link` is cleared by non-mutual-link demotion (the other side's
    /// link points elsewhere, meaning we're a serif attachment to the
    /// other side's real stem partner). Edge classification uses this to
    /// attach us as a serif instead of promoting a bogus long-distance
    /// link into a phantom stem pair.
    pub serif: Option<usize>,
}

/// A contour with pre-computed tangent directions at each point.
pub struct AnalysisContour {
    /// Points in traversal order with per-point tangent metadata.
    pub points: Vec<AnalysisPoint>,
}

/// Classify a tangent vector into a direction.
/// FreeType threshold: ~4.1 degrees (14x ratio).
fn classify_tangent(dx: f64, dy: f64) -> Direction {
    let ax = dx.abs();
    let ay = dy.abs();
    if ax < 0.5 && ay < 0.5 {
        return Direction::None;
    }
    if ay > ax * latin::DIRECTION_RATIO {
        return if dy > 0.0 {
            Direction::Up
        } else {
            Direction::Down
        };
    }
    if ax > ay * latin::DIRECTION_RATIO {
        return if dx > 0.0 {
            Direction::Right
        } else {
            Direction::Left
        };
    }
    Direction::None
}

/// Build analysis contours from GlyphOutline contours.
/// Computes out_dir on every point (including off-curve control points)
/// using the tangent vector to the next point. No bezier flattening.
pub(crate) fn outline_to_contours(contours: &[udoc_font::ttf::Contour]) -> Vec<AnalysisContour> {
    contours
        .iter()
        .map(|c| build_analysis_contour(&c.points))
        .collect()
}

/// Convert (x, y, on_curve) tuples to analysis contours.
///
/// Allocates a fresh outer Vec plus inner point Vecs. The hinter hot path
/// prefers `tuples_to_contours_into` which reuses buffers from
/// `super::scratch::ScratchBuffers`.
pub fn tuples_to_contours(contours: &[Vec<(f64, f64, bool)>]) -> Vec<AnalysisContour> {
    contours
        .iter()
        .map(|c| {
            let mut pts = Vec::with_capacity(c.len());
            fill_analysis_points_from_tuples(c, &mut pts);
            AnalysisContour { points: pts }
        })
        .collect()
}

/// Fill scratch `AnalysisContour` slots from the tuple contours without
/// allocating fresh inner Vecs.
///
/// Reuses the point Vecs already sitting in `scratch.analysis_contours`; the
/// outer Vec grows only when the glyph has more contours than any previously
/// processed glyph on this thread. Returns the number of live contours so
/// callers can slice `&scratch.analysis_contours[..n]`.
pub(crate) fn tuples_to_contours_into(
    contours: &[Vec<(f64, f64, bool)>],
    scratch: &mut super::scratch::ScratchBuffers,
) -> usize {
    for (idx, c) in contours.iter().enumerate() {
        let slot = scratch.ensure_contour_slot(idx);
        // `reset()` already cleared `slot.points`; capacity is preserved.
        fill_analysis_points_from_tuples(c, &mut slot.points);
    }
    contours.len()
}

/// Populate an `AnalysisPoint` buffer from `(x, y, on_curve)` tuples,
/// computing `out_dir` via the tangent to the next point. `out` must be
/// empty on entry; caller ensures adequate capacity.
fn fill_analysis_points_from_tuples(contour: &[(f64, f64, bool)], out: &mut Vec<AnalysisPoint>) {
    let n = contour.len();
    if n < 2 {
        out.extend(contour.iter().map(|&(x, y, on_curve)| AnalysisPoint {
            x,
            y,
            out_dir: Direction::None,
            on_curve,
        }));
        return;
    }
    out.reserve(n);
    for i in 0..n {
        let next = (i + 1) % n;
        let (x, y, on_curve) = contour[i];
        let (nx, ny, _) = contour[next];
        out.push(AnalysisPoint {
            x,
            y,
            out_dir: classify_tangent(nx - x, ny - y),
            on_curve,
        });
    }
}

/// Build an analysis contour with per-point tangent directions.
/// Works directly on all outline points (on-curve and off-curve).
fn build_analysis_contour(points: &[udoc_font::ttf::OutlinePoint]) -> AnalysisContour {
    let n = points.len();
    if n < 2 {
        return AnalysisContour {
            points: points
                .iter()
                .map(|p| AnalysisPoint {
                    x: p.x,
                    y: p.y,
                    out_dir: Direction::None,
                    on_curve: p.on_curve,
                })
                .collect(),
        };
    }

    let mut result: Vec<AnalysisPoint> = Vec::with_capacity(n);
    for i in 0..n {
        let next = (i + 1) % n;
        let dx = points[next].x - points[i].x;
        let dy = points[next].y - points[i].y;
        result.push(AnalysisPoint {
            x: points[i].x,
            y: points[i].y,
            out_dir: classify_tangent(dx, dy),
            on_curve: points[i].on_curve,
        });
    }

    AnalysisContour { points: result }
}

/// Context passed to [`emit_segment`] for the run of points currently being
/// walked on a single contour. Grouped into a struct so the emit helper
/// stays below clippy's too_many_arguments lint threshold.
struct EmitCtx<'a> {
    pts: &'a [AnalysisPoint],
    contour: usize,
    dim: Dimension,
    min_segment_length: f64,
    /// UPM/14 flat threshold for `AF_EDGE_ROUND` classification. A segment
    /// is round when its on-curve-point coord span is below this value and
    /// at least one extreme point is a control point. See aflatin.c:38.
    flat_threshold: f64,
}

/// Detect segments in contours along the given dimension.
///
/// A segment starts when a point's out_dir matches the dimension's axis.
/// It continues while subsequent points maintain that direction.
/// Sub-threshold segments (shorter than `min_segment_length(units_per_em)`)
/// are filtered out.
///
/// `units_per_em` is threaded through so that length / distance thresholds
/// scale correctly for non-1000-UPM fonts.
pub fn detect_segments(
    contours: &[AnalysisContour],
    dim: Dimension,
    units_per_em: u16,
) -> Vec<Segment> {
    let mut segments = Vec::new();
    detect_segments_into(contours, dim, units_per_em, &mut segments);
    segments
}

/// Like [`detect_segments`] but writes into a caller-supplied buffer so the
/// hot path avoids per-call allocation.
///
/// The `out` vec is cleared on entry; capacity is preserved.
pub(crate) fn detect_segments_into(
    contours: &[AnalysisContour],
    dim: Dimension,
    units_per_em: u16,
    segments: &mut Vec<Segment>,
) {
    segments.clear();
    let min_len = latin::min_segment_length(units_per_em);
    // UPM/14 matches FreeType's FLAT_THRESHOLD macro (aflatin.c:38).
    let flat_threshold = if units_per_em == 0 {
        1000.0 / 14.0
    } else {
        units_per_em as f64 / 14.0
    };

    for (contour_idx, contour) in contours.iter().enumerate() {
        let pts = &contour.points;
        if pts.len() < 3 {
            continue;
        }

        let ctx = EmitCtx {
            pts,
            contour: contour_idx,
            dim,
            min_segment_length: min_len,
            flat_threshold,
        };
        let n = pts.len();
        let mut seg_start: Option<usize> = None;
        let mut seg_dir = Direction::None;

        // Forward pass over points in contour order. Preserves the
        // original segment boundaries for all points that don't straddle
        // a wrap. Runs that span the contour start (wrap boundary) are
        // handled below via a cyclic continuation so their "missing tail"
        // gets emitted without disturbing any forward-contained segment.
        for (i, pt) in pts.iter().enumerate().take(n) {
            let dir = pt.out_dir;

            if dir.matches_dimension(dim) {
                if seg_start.is_none() {
                    seg_start = Some(i);
                    seg_dir = dir;
                } else if dir != seg_dir {
                    emit_segment(&mut *segments, &ctx, seg_dir, seg_start.unwrap(), i);
                    seg_start = Some(i);
                    seg_dir = dir;
                }
            } else if seg_start.is_some() {
                emit_segment(&mut *segments, &ctx, seg_dir, seg_start.unwrap(), i);
                seg_start = None;
                seg_dir = Direction::None;
            }
        }

        // Wrap-around heal: if a segment is still open at end of forward
        // iteration (rare, but happens on glyphs like Liberation Sans 'e'
        // where the top of the crossbar is traced across the contour's
        // start index), continue virtually past index 0 until the
        // direction changes OR we loop all the way back to `start`. Emit
        // the recovered segment with `start > end` so `emit_segment`'s
        // wrap branch walks the right index range. All segments that
        // were already fully contained in [0, n) are unaffected, so
        // existing hinting for glyphs like 'a' (no wrap-open segment)
        // stays exactly as it was.
        if let Some(start) = seg_start {
            let mut end_wrap = start; // no-op sentinel
            for (idx, pt) in pts.iter().enumerate().take(n) {
                if idx == start {
                    break;
                }
                let dir = pt.out_dir;
                if !dir.matches_dimension(dim) || dir != seg_dir {
                    end_wrap = idx;
                    break;
                }
            }
            if end_wrap != start {
                emit_segment(&mut *segments, &ctx, seg_dir, start, end_wrap);
            }
        }
    }

    // Link segments using FreeType's scoring (plain dist + len_score).
    link_segments(segments, units_per_em);
}

/// Emit a segment with delta computation and filtering.
fn emit_segment(
    segments: &mut Vec<Segment>,
    ctx: &EmitCtx<'_>,
    dir: Direction,
    start: usize,
    end: usize,
) {
    let pts = ctx.pts;
    let contour = ctx.contour;
    let dim = ctx.dim;
    let min_segment_length = ctx.min_segment_length;

    if start >= pts.len() || end >= pts.len() || start == end {
        return;
    }

    let mut min_pos = f64::MAX;
    let mut max_pos = f64::MIN;
    let mut min_coord = f64::MAX;
    let mut max_coord = f64::MIN;
    // Track whether the points sitting at min_coord / max_coord are
    // control points: this mirrors FreeType's min_flags / max_flags.
    let mut min_coord_is_control = false;
    let mut max_coord_is_control = false;
    // Coord range covered by on-curve points only; used for the round
    // heuristic's flat_threshold check (aflatin.c:1698-1700).
    let mut min_on_coord = f64::MAX;
    let mut max_on_coord = f64::MIN;
    let mut count = 0usize;

    // Walk point indices without allocating a Vec. Covers either the
    // forward range `start.=end` or, when wrapping, `start.n` followed
    // by `0..=end`.
    let mut visit = |idx: usize| {
        let p = &pts[idx];
        let (ctrl, cross) = match dim {
            Dimension::Vertical => (p.x, p.y),
            Dimension::Horizontal => (p.y, p.x),
        };
        min_pos = min_pos.min(ctrl);
        max_pos = max_pos.max(ctrl);
        if cross < min_coord {
            min_coord = cross;
            min_coord_is_control = !p.on_curve;
        }
        if cross > max_coord {
            max_coord = cross;
            max_coord_is_control = !p.on_curve;
        }
        if p.on_curve {
            min_on_coord = min_on_coord.min(cross);
            max_on_coord = max_on_coord.max(cross);
        }
        count += 1;
    };
    if start <= end {
        for idx in start..=end {
            visit(idx);
        }
    } else {
        for idx in start..pts.len() {
            visit(idx);
        }
        for idx in 0..=end {
            visit(idx);
        }
    }

    if count == 0 {
        return;
    }

    let extent = max_coord - min_coord;
    if extent < min_segment_length {
        return;
    }

    // FreeType's AF_EDGE_ROUND rule (aflatin.c:1698-1700):
    //
    //   round if (min_flags | max_flags) & AF_FLAG_CONTROL
    //         && (max_on_coord - min_on_coord) < flat_threshold
    //
    // Interpretation: the segment's extent is bounded by a control point
    // (= curve) on one side, and the on-curve span within the segment is
    // short enough that the curve dominates the shape. A straight stem
    // has on-curve endpoints, so the first clause fails and is_round
    // stays false. Curve apices have off-curve extremes with small
    // on-curve span in the middle -> is_round true.
    let on_span = if max_on_coord > f64::MIN && min_on_coord < f64::MAX {
        max_on_coord - min_on_coord
    } else {
        // No on-curve points in segment at all: treat as round (pure curve).
        0.0
    };
    let is_round = (min_coord_is_control || max_coord_is_control) && on_span < ctx.flat_threshold;

    segments.push(Segment {
        dir,
        dim,
        contour,
        pos: (min_pos + max_pos) / 2.0,
        min_coord,
        max_coord,
        link: None,
        score: f64::MAX,
        edge: None,
        is_round,
        serif: None,
    });
}

/// Link segments using FreeType's scoring formula.
///
/// Mirrors `af_latin_hints_link_segments` in FreeType 2.13 (aflatin.c):
///
/// ```text
/// score = dist + len_score
/// len_score = 6000 / overlap_len
/// ```
///
/// A candidate replaces the current best only on **strict less-than**
/// (`dist + len_score < best_score`). This matches FreeType's behaviour
/// and keeps link selection deterministic when multiple candidates tie.
///
/// We previously used `distance_penalty + overlap_reward` where the
/// distance penalty was quadratic-in-ratio against `dominant_width` and
/// the reward divided by `overlap * coord_span`. That form rejected
/// valid pairings on glyphs like 'W' and 'M' where stems have asymmetric
/// widths, producing wrong cross-links. The plain FreeType form picks
/// the geometrically-nearest paired segment, which is what FT targets.
///
/// `units_per_em` is used for UPM-aware `MAX_STEM_WIDTH` scaling so that
/// the maximum-plausible-stem filter works equivalently on 2048-UPM
/// TrueType fonts as it does on 1000-UPM CFF fonts.
fn link_segments(segments: &mut [Segment], units_per_em: u16) {
    let n = segments.len();
    if n < 2 {
        return;
    }

    let max_stem_width = latin::max_stem_width(units_per_em);

    for i in 0..n {
        let mut best_score = f64::MAX;
        let mut best_link: Option<usize> = None;

        for j in 0..n {
            if i == j {
                continue;
            }
            if segments[i].dim != segments[j].dim {
                continue;
            }
            if segments[j].dir != segments[i].dir.opposite() {
                continue;
            }

            // Fill convention check disabled: CFF fonts don't consistently
            // use counter-clockwise outer contours. The check was rejecting
            // valid stem pairings, causing 0.75x ratio (worse than no check).

            // Cross-axis overlap between segments -- segments that don't
            // overlap at all in the cross-axis direction can't form a stem.
            let overlap_min = segments[i].min_coord.max(segments[j].min_coord);
            let overlap_max = segments[i].max_coord.min(segments[j].max_coord);
            let overlap = overlap_max - overlap_min;
            if overlap <= 0.0 {
                continue;
            }

            let dist = (segments[i].pos - segments[j].pos).abs();
            if !(1.0..=max_stem_width).contains(&dist) {
                continue;
            }

            // FreeType plain scoring: dist + len_score, with len_score
            // inversely proportional to overlap. Shorter overlaps are
            // penalised (a tiny kissing overlap shouldn't outweigh a
            // solid one); closer pairings outscore farther ones.
            let len_score = 6000.0 / overlap;
            let score = dist + len_score;

            // Strict less-than required to replace the current best --
            // matches FreeType and keeps ties deterministic (first-seen
            // wins, which respects contour traversal order).
            if score < best_score {
                best_score = score;
                best_link = Some(j);
            }
        }

        if let Some(link) = best_link {
            segments[i].link = Some(link);
            segments[i].score = best_score;
        }
    }

    // FT non-mutual-link demotion (aflatin.c:2107-2120).
    //
    // A segment that links to another segment whose own link doesn't
    // point back is not a stem: it's a serif attachment. Clear the
    // one-way link and record the far segment's real partner as our
    // serif target. Without this demotion, glyphs like 'n' (in X-axis
    // hinting) form a bogus long-distance "stem" across the glyph
    // counter and the anchor-fit grips that 4-5 px counter as if it
    // were a real stem.
    //
    // Applied only to Vertical-dim segments (X-axis stems). The Y-axis
    // (Horizontal-dim) cascade fit depends on the existing one-way
    // links for serif-to-stem propagation -- demoting them there
    // regresses Y-axis golden renders on characters like CJK 中.
    for i in 0..n {
        if segments[i].dim != Dimension::Vertical {
            continue;
        }
        if let Some(j) = segments[i].link {
            match segments[j].link {
                Some(k) if k == i => { /* mutual stem link, keep */ }
                _ => {
                    segments[i].link = None;
                    segments[i].serif = segments[j].link;
                }
            }
        }
    }
}

/// Find mutually linked segment pairs.
pub(crate) fn find_linked_pairs(segments: &[Segment]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    for i in 0..segments.len() {
        if let Some(j) = segments[i].link {
            if j > i {
                if let Some(k) = segments[j].link {
                    if k == i {
                        pairs.push((i, j));
                    }
                }
            }
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_contour(points: &[(f64, f64, bool)]) -> AnalysisContour {
        let pts: Vec<udoc_font::ttf::OutlinePoint> = points
            .iter()
            .map(|&(x, y, oc)| udoc_font::ttf::OutlinePoint { x, y, on_curve: oc })
            .collect();
        build_analysis_contour(&pts)
    }

    /// Default 1000 UPM for tests unless a case specifically exercises UPM scaling.
    const UPM_1000: u16 = 1000;

    #[test]
    fn detect_vertical_segments_rectangle() {
        let contour = make_contour(&[
            (0.0, 0.0, true),
            (0.0, 700.0, true),
            (100.0, 700.0, true),
            (100.0, 0.0, true),
        ]);
        let segs = detect_segments(&[contour], Dimension::Vertical, UPM_1000);
        assert!(segs.len() >= 2, "expected >= 2, got {}", segs.len());
    }

    #[test]
    fn detect_horizontal_segments_rectangle() {
        let contour = make_contour(&[
            (0.0, 0.0, true),
            (500.0, 0.0, true),
            (500.0, 100.0, true),
            (0.0, 100.0, true),
        ]);
        let segs = detect_segments(&[contour], Dimension::Horizontal, UPM_1000);
        assert!(segs.len() >= 2, "expected >= 2, got {}", segs.len());
    }

    #[test]
    fn linked_pairs_stem() {
        let contour = make_contour(&[
            (0.0, 0.0, true),
            (0.0, 700.0, true),
            (100.0, 700.0, true),
            (100.0, 0.0, true),
        ]);
        let segs = detect_segments(&[contour], Dimension::Vertical, UPM_1000);
        let pairs = find_linked_pairs(&segs);
        assert!(!pairs.is_empty(), "expected linked pair for stem");
    }

    #[test]
    fn fill_convention_prevents_counter_linking() {
        // Two rectangles side by side (like 'H' without crossbar).
        // Left stem: x=0-80, Right stem: x=300-380.
        // Counter gap: 80-300. Should NOT link across the counter.
        let left = make_contour(&[
            (0.0, 0.0, true),
            (0.0, 700.0, true),
            (80.0, 700.0, true),
            (80.0, 0.0, true),
        ]);
        let right = make_contour(&[
            (300.0, 0.0, true),
            (300.0, 700.0, true),
            (380.0, 700.0, true),
            (380.0, 0.0, true),
        ]);
        let segs = detect_segments(&[left, right], Dimension::Vertical, UPM_1000);

        // Each stem should link internally (0-80, 300-380), not across counter.
        for seg in &segs {
            if let Some(link) = seg.link {
                let dist = (seg.pos - segs[link].pos).abs();
                assert!(
                    dist < 100.0,
                    "linked across counter: dist={dist:.0}, expected < 100"
                );
            }
        }
    }

    #[test]
    fn segments_detected_from_curved_outline() {
        // Curve with off-curve control points.
        let contour = make_contour(&[
            (100.0, 0.0, true),
            (100.0, 200.0, false),
            (100.0, 400.0, false),
            (100.0, 600.0, true),
            (200.0, 600.0, true),
            (200.0, 0.0, true),
        ]);
        let segs = detect_segments(&[contour], Dimension::Vertical, UPM_1000);
        assert!(!segs.is_empty(), "expected segments from curved outline");
    }

    /// `is_round` should stay false on a straight rectangular stem (all
    /// on-curve corners, no bezier control points at the extremes).
    #[test]
    fn is_round_false_on_straight_stem() {
        let contour = make_contour(&[
            (0.0, 0.0, true),
            (0.0, 700.0, true),
            (100.0, 700.0, true),
            (100.0, 0.0, true),
        ]);
        let segs = detect_segments(&[contour], Dimension::Vertical, UPM_1000);
        assert!(!segs.is_empty());
        assert!(
            segs.iter().all(|s| !s.is_round),
            "straight stem segments must not be marked round: {segs:?}"
        );
    }

    /// `is_round` should be true on a curve apex (extreme points are bezier
    /// control points and the on-curve span within the segment is tiny).
    /// Mirrors the leftmost curve apex of an 'o'.
    #[test]
    fn is_round_true_on_curve_apex() {
        // Three-point segment pattern that appears at the leftmost vertical
        // apex of an 'o': off-curve, on-curve apex, off-curve. All at the
        // same x coordinate; the control points flank the apex in y.
        let contour = make_contour(&[
            // Top-right to start the loop heading left and down.
            (200.0, 500.0, true),
            // Outer-left curve top control.
            (100.0, 500.0, false),
            (50.0, 400.0, false),
            // Outer-left apex (on-curve).
            (50.0, 250.0, true),
            (50.0, 100.0, false),
            (100.0, 0.0, false),
            // Back across the bottom.
            (200.0, 0.0, true),
        ]);
        let segs = detect_segments(&[contour], Dimension::Vertical, UPM_1000);
        assert!(!segs.is_empty());
        // The segment at the leftmost x (near pos=50) should be flagged
        // round: both extremes in y (the two off-curve controls) are
        // control points, and the on-curve span within the segment is 0.
        let left_seg = segs
            .iter()
            .min_by(|a, b| a.pos.partial_cmp(&b.pos).unwrap())
            .unwrap();
        assert!(
            left_seg.is_round,
            "curve apex segment at pos={} should be round: {left_seg:?}",
            left_seg.pos
        );
    }

    // ---- Domain-expert-specified X-axis re-enable tests (M-03) ----

    /// 'n'-shaped glyph has two vertical stems that should pair to each
    /// other, not to the shoulder at the top of the glyph. Our previous
    /// quadratic-distance scoring could pair stem-to-shoulder on glyphs
    /// with asymmetric side-bearings.
    #[test]
    fn x_axis_n_glyph_pairs_correct_stems() {
        // Approximate 'n' outline: two vertical rectangles (stems) joined
        // at the top by a shoulder. Left stem x=50..130, right stem
        // x=320..400, shoulder spans the top between them.
        //
        // Single contour (counter-clockwise outer):
        //   start at bottom-left of left stem
        //   up the left side of left stem, across the top shoulder, down
        //   the right side of right stem, back along the bottom.
        let outline = make_contour(&[
            // left stem outer (left edge, going up)
            (50.0, 0.0, true),
            (50.0, 600.0, true),
            // shoulder top
            (130.0, 680.0, true),
            (320.0, 680.0, true),
            // right stem outer (right edge, going down)
            (400.0, 600.0, true),
            (400.0, 0.0, true),
            // bottom of right stem
            (320.0, 0.0, true),
            // right stem inner (going up from bottom to where shoulder joins)
            (320.0, 580.0, true),
            // inside of shoulder (going left)
            (130.0, 580.0, true),
            // left stem inner (going down)
            (130.0, 0.0, true),
        ]);
        let segs = detect_segments(&[outline], Dimension::Vertical, UPM_1000);
        let pairs = find_linked_pairs(&segs);

        // Two mutually-linked stem pairs are expected: left-outer<->left-inner
        // and right-inner<->right-outer. Widths both ~80 font units.
        assert!(
            pairs.len() >= 2,
            "expected >= 2 stem pairs on 'n'-glyph, got {}: {:?}",
            pairs.len(),
            pairs
        );
        for (a, b) in &pairs {
            let width = (segs[*a].pos - segs[*b].pos).abs();
            assert!(
                (70.0..=100.0).contains(&width),
                "stem pair width {width} outside expected 70..100 range (counter was 190)"
            );
        }
    }

    /// 'W' has four vertical stems of varying widths. With our old quadratic
    /// scoring against dominant_h_width, stems of non-dominant width were
    /// cross-linked instead of pairing with their true partners. FreeType's
    /// plain dist+len_score form pairs nearest-neighbours correctly.
    #[test]
    fn x_axis_w_glyph_asymmetric_stems() {
        // Four near-vertical stems at x = 50, 150 (pair 1, width 100) and
        // x = 400, 480 (pair 2, width 80). The cross-pair distance (250+)
        // is much larger than any within-pair distance, so a sane scorer
        // MUST pair (0,1) and (2,3).
        let stems = vec![
            // stem 0: x=50 going up (contour 0 left edge)
            make_contour(&[
                (50.0, 0.0, true),
                (50.0, 700.0, true),
                (150.0, 700.0, true),
                (150.0, 0.0, true),
            ]),
            // stem 2/3: x=400..480
            make_contour(&[
                (400.0, 0.0, true),
                (400.0, 700.0, true),
                (480.0, 700.0, true),
                (480.0, 0.0, true),
            ]),
        ];

        let segs = detect_segments(&stems, Dimension::Vertical, UPM_1000);
        let pairs = find_linked_pairs(&segs);

        assert!(
            pairs.len() >= 2,
            "expected 2 mutually-linked stem pairs on 'W'-like outline, got {pairs:?}"
        );
        for (a, b) in &pairs {
            let dist = (segs[*a].pos - segs[*b].pos).abs();
            assert!(
                dist < 200.0,
                "cross-linked stems across gap: pair ({a},{b}) dist={dist:.0} -- \
                 quadratic scoring regression"
            );
        }
    }

    /// The old scoring rewarded stems whose width matched `dominant_width`,
    /// so a far partner at dominant_width=180 beat a near partner at 100.
    /// FreeType's plain `dist + len_score` picks the geometrically nearer
    /// one regardless of dominant width. This test drives `link_segments`
    /// directly with hand-constructed segments so the input is unambiguous.
    #[test]
    fn link_scoring_prefers_nearest_not_dominant_width_match() {
        // Three hand-built vertical segments:
        //   target (pos=0, dir=Up), covers y=0..500
        //   near   (pos=100, dir=Down), covers y=0..500   -> dist=100
        //   far    (pos=180, dir=Down), covers y=0..500   -> dist=180
        //
        // Both near and far have identical cross-axis overlap with target,
        // so len_score is equal. Under FreeType's formula target MUST pick
        // near (dist 100 < dist 180). Under the old quadratic-ratio scoring
        // calibrated against dominant_width=180, target would pick far.
        //
        // We call `link_segments` directly because the old formula depended
        // on `dominant_width`; the new formula does not, so the crucial
        // thing is just that dist strictly dominates the link choice.
        let mut segs = vec![
            Segment {
                dir: Direction::Up,
                dim: Dimension::Vertical,
                contour: 0,
                pos: 0.0,
                min_coord: 0.0,
                max_coord: 500.0,
                link: None,
                score: f64::MAX,
                edge: None,
                is_round: false,
                serif: None,
            },
            Segment {
                dir: Direction::Down,
                dim: Dimension::Vertical,
                contour: 1,
                pos: 100.0,
                min_coord: 0.0,
                max_coord: 500.0,
                link: None,
                score: f64::MAX,
                edge: None,
                is_round: false,
                serif: None,
            },
            Segment {
                dir: Direction::Down,
                dim: Dimension::Vertical,
                contour: 2,
                pos: 180.0,
                min_coord: 0.0,
                max_coord: 500.0,
                link: None,
                score: f64::MAX,
                edge: None,
                is_round: false,
                serif: None,
            },
        ];

        link_segments(&mut segs, UPM_1000);

        let linked = segs[0].link.expect("target (seg 0) should link");
        assert_eq!(
            linked, 1,
            "target linked to seg {linked} (pos={}), expected seg 1 (pos=100, nearest). \
             Old quadratic-ratio scoring against dominant_width=180 would pick seg 2.",
            segs[linked].pos
        );

        // Mutual: near should link back to target.
        let near_linked = segs[1].link.expect("near should link");
        assert_eq!(near_linked, 0, "near's partner should be target");
    }

    /// UPM-aware thresholds: a segment just short of MIN_SEGMENT_LENGTH at
    /// 1000 UPM (9 font units) is rejected at UPM=1000, but an equivalent
    /// segment at UPM=2048 must be 2.048x longer. The scaled threshold
    /// MUST change with UPM.
    #[test]
    fn thresholds_scale_with_upm() {
        use latin::{max_stem_width, min_segment_length};

        // Scale is linear to UPM.
        let min_1000 = min_segment_length(1000);
        let min_2048 = min_segment_length(2048);
        let expected_2048 = min_1000 * 2.048;
        assert!(
            (min_2048 - expected_2048).abs() < 0.01,
            "min_segment_length(2048)={min_2048:.3}, expected {expected_2048:.3}"
        );

        let max_1000 = max_stem_width(1000);
        let max_2048 = max_stem_width(2048);
        let expected_max_2048 = max_1000 * 2.048;
        assert!(
            (max_2048 - expected_max_2048).abs() < 0.01,
            "max_stem_width(2048)={max_2048:.3}, expected {expected_max_2048:.3}"
        );

        // Zero / tiny UPM must not blow up -- falls back to reference value.
        assert!(min_segment_length(0) > 0.0);
    }

    /// DIRECTION_RATIO is 14.0. Tangent vectors exactly at the 1:14 boundary
    /// should classify as None (no axis dominance); at 1:15 and beyond they
    /// should classify as axis-aligned. Prevents false-positive segments on
    /// near-diagonal strokes.
    #[test]
    fn direction_classification_boundary() {
        // dx=1, dy=14 -- ratio exactly 14x. FT uses strict-greater-than so
        // this is NOT axis-aligned.
        let d = classify_tangent(1.0, 14.0);
        assert_eq!(
            d,
            Direction::None,
            "1:14 ratio must not be classified axis-aligned"
        );

        // dx=1, dy=15 -- exceeds threshold, classifies as Up.
        let d = classify_tangent(1.0, 15.0);
        assert_eq!(d, Direction::Up, "1:15 ratio must classify as Up");

        // Horizontal equivalents.
        let d = classify_tangent(14.0, 1.0);
        assert_eq!(
            d,
            Direction::None,
            "14:1 ratio must not be classified axis-aligned"
        );
        let d = classify_tangent(15.0, 1.0);
        assert_eq!(d, Direction::Right, "15:1 ratio must classify as Right");

        // Very small vectors below absolute cutoff -> None.
        let d = classify_tangent(0.2, 0.2);
        assert_eq!(d, Direction::None);
    }

    /// CFF fonts are sometimes drawn with clockwise outer contours (opposite
    /// of FreeType's assumption that outers are counter-clockwise). Our
    /// fill-convention check was rejecting valid stem links on such fonts,
    /// so it's currently disabled. This test locks in that clockwise-outer
    /// CFF-style outlines still produce a valid stem link.
    ///
    /// NOTE: The contour is deliberately started at a point where the first
    /// edge is horizontal. Segment detection has a known wraparound limitation
    /// when a matching-dimension segment spans the index-0 boundary
    /// (tracked separately; not in M-03 scope). Starting on a horizontal
    /// edge keeps both vertical edges strictly inside the point sequence.
    #[test]
    fn fill_convention_cff_winding_bypass() {
        // Clockwise outer rectangle, starting at the TOP-MIDDLE of the top
        // edge so the first motion is horizontal and both Down (right edge)
        // and Up (left edge) segments are interior to the loop.
        let cw_rect = make_contour(&[
            (50.0, 700.0, true),  // start mid-top (heading right)
            (100.0, 700.0, true), // top-right corner
            (100.0, 0.0, true),   // bottom-right (down)
            (0.0, 0.0, true),     // bottom-left (left)
            (0.0, 700.0, true),   // top-left (up)
        ]);
        let segs = detect_segments(&[cw_rect], Dimension::Vertical, UPM_1000);

        // Both vertical edges should be detected.
        assert!(
            segs.len() >= 2,
            "expected >= 2 vertical segments on clockwise rect, got {}",
            segs.len()
        );

        let pairs = find_linked_pairs(&segs);
        assert!(
            !pairs.is_empty(),
            "clockwise-outer rectangle produced no stem link -- \
             fill-convention check is over-rejecting. segs={segs:?}"
        );
    }
}

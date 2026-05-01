//! "Discovered" stem detection for Type1 / CFF glyph outlines.
//!
//! FreeType's PostScript hinter includes a pass
//! (`ps_hinter_table_build` in `src/pshinter/pshalgo.c`, and the
//! edge-synthesis path in `src/psaux/psobjs.c::ps_builder_add_stem`)
//! that scans the outline for stem candidates when the font's declared
//! hstem / vstem hints are sparse. Many Type1 fonts, especially the
//! Computer Modern family, declare only the "primary" stem of a glyph
//! and leave secondary edges (left/right side of `H`-crossbars, the
//! diagonal-to-vertical transitions inside numerals like `1`) implicit
//! in the outline itself. The rasterizer then relies on the outline's
//! edge geometry alone, which at very small pixel sizes (ppem 6..16
//! for body text) loses the sub-pixel information needed to snap the
//! edge to the pixel grid. FreeType compensates by finding the
//! additional stems from outline geometry before grid-fitting.
//!
//! This module reproduces the idea at a much smaller surface area:
//! it walks contours, finds pairs of near-vertical edges that are
//! parallel, at a consistent X distance (typical stem width for
//! Latin body text), with ink on both sides, and returns them as
//! additional `(x_position, width)` stem candidates that the PS hint
//! fitter can consume alongside the declared `v_stems`.
//!
//! The algorithm is intentionally conservative:
//!   * only vertical stems (the axis where Type1 sparsity hurts us most)
//!   * only nearly-vertical edges (abs(dx) < 1/5 of |dy|)
//!   * Y-overlap >= `min_y_overlap_fu` (default 50 font units)
//!   * stem width within `[min_width_fu, max_width_fu]`, typical 30..160
//!   * opposite traversal-direction sign on the two edges (one up,
//!     one down); matches how a single closed outer contour walks the
//!     two sides of a real stem, and rejects pairings of two
//!     "outer-going-same-way" edges from two different paths that
//!     happen to sit at a consistent X distance
//!   * de-duplicated against declared `v_stems` within 1 font unit
//!   * cap at `max_discovered_count` (default 8) to avoid false positives
//!     on decorative outlines (ornaments, italic swashes)
//!
//! Callers typically merge the output with declared stems at the very
//! start of the stem list so the declared hints take precedence during
//! the PS hint fitter's "closest wins" snap, and the discovered stems
//! only contribute where declared coverage is missing.

use crate::ttf::Contour;

/// Tuning parameters for the discovered-stem scanner.
///
/// Defaults target Latin body-text Type1 fonts (CMR, CMBX, NimbusRom,
/// LM* at 1000 UPM). Non-Latin, italic, and display-size variants can
/// pass custom limits.
#[derive(Debug, Clone, Copy)]
pub struct StemLimits {
    /// Minimum stem width in font units; stems thinner than this
    /// are ignored (likely hairline details, not body strokes).
    pub min_width_fu: f64,
    /// Maximum stem width in font units; stems wider than this are
    /// ignored (likely slab serifs or counter edges, not real stems).
    pub max_width_fu: f64,
    /// Minimum vertical overlap between the two edges in font units.
    /// Two edges must span at least this much Y in common to be
    /// considered a stem pair.
    pub min_y_overlap_fu: f64,
    /// Maximum per-segment dx-to-dy ratio for an edge to count as
    /// "near-vertical". 0.2 matches FreeType's classify_tangent
    /// threshold for DIR_UP / DIR_DOWN at the `DIRECTION_RATIO`
    /// default of 14.
    pub near_vertical_ratio: f64,
    /// Cap on the number of discovered stems returned. Prevents
    /// decorative outlines from polluting the hint table.
    pub max_discovered_count: usize,
    /// Dedupe tolerance against declared v_stems, in font units.
    /// A discovered stem whose (pos, width) is within this
    /// of a declared one is dropped.
    pub dedupe_tolerance_fu: f64,
}

impl Default for StemLimits {
    fn default() -> Self {
        Self {
            min_width_fu: 30.0,
            max_width_fu: 180.0,
            min_y_overlap_fu: 50.0,
            near_vertical_ratio: 0.20,
            max_discovered_count: 8,
            dedupe_tolerance_fu: 1.0,
        }
    }
}

/// A single near-vertical edge discovered in the outline: the X
/// midpoint, the Y-span it covers, and a sign indicating whether ink
/// is to the RIGHT of the edge (when traversed in the contour's
/// direction) or to the LEFT. This is just the sign of `dy`: for a
/// counter-clockwise contour (the usual "outer" direction in Type1)
/// a downward edge puts ink on the left, an upward edge puts ink on
/// the right. The stem pairing logic uses this to reject "bar"
/// candidates where both edges have ink on the same side.
#[derive(Debug, Clone, Copy)]
struct VerticalEdge {
    x: f64,
    y_min: f64,
    y_max: f64,
    /// +1 if traversal is upward (ink on right), -1 if downward (ink on left).
    /// 0 if the edge is degenerate; such edges are never emitted.
    ink_side: i8,
}

/// Scan an outline's contours for vertical stem candidates whose
/// declared counterparts are missing from the hint table.
///
/// Returns a list of `(x_left, width)` pairs compatible with the
/// `StemHints::v_stems` shape. `declared` is inspected for dedupe;
/// any candidate matching a declared entry within
/// `limits.dedupe_tolerance_fu` on both position and width is
/// dropped. The returned stems are sorted by x-position ascending
/// and capped at `limits.max_discovered_count`.
pub fn discover_vertical_stems(
    contours: &[Contour],
    declared: &[(f64, f64)],
    limits: StemLimits,
) -> Vec<(f64, f64)> {
    let edges = collect_vertical_edges(contours, limits.near_vertical_ratio);
    if edges.len() < 2 {
        return Vec::new();
    }

    let mut candidates: Vec<(f64, f64)> = Vec::new();
    for i in 0..edges.len() {
        for j in (i + 1)..edges.len() {
            let a = edges[i];
            let b = edges[j];
            let (left, right) = if a.x < b.x { (a, b) } else { (b, a) };
            let width = right.x - left.x;
            if !(limits.min_width_fu..=limits.max_width_fu).contains(&width) {
                continue;
            }

            // Y-overlap test: the two edges must share at least
            // `min_y_overlap_fu` of Y span.
            let y_lo = left.y_min.max(right.y_min);
            let y_hi = left.y_max.min(right.y_max);
            if y_hi - y_lo < limits.min_y_overlap_fu {
                continue;
            }

            // Ink-on-both-sides: a real stem has the left edge
            // traversed downward (ink on the right) and the right
            // edge traversed upward (ink on the left) -- so their
            // ink_side signs sum to zero. A "bar" (like the inside
            // of a counter) has matching signs.
            if left.ink_side == right.ink_side {
                continue;
            }
            if left.ink_side == 0 || right.ink_side == 0 {
                continue;
            }

            candidates.push((left.x, width));
        }
    }

    // Sort then dedupe overlapping candidates within dedupe_tolerance.
    candidates.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    });
    candidates.dedup_by(|a, b| {
        (a.0 - b.0).abs() < limits.dedupe_tolerance_fu
            && (a.1 - b.1).abs() < limits.dedupe_tolerance_fu
    });

    // Drop candidates that coincide with a declared stem.
    candidates.retain(|c| {
        !declared.iter().any(|d| {
            (c.0 - d.0).abs() < limits.dedupe_tolerance_fu
                && (c.1 - d.1).abs() < limits.dedupe_tolerance_fu
        })
    });

    if candidates.len() > limits.max_discovered_count {
        candidates.truncate(limits.max_discovered_count);
    }
    candidates
}

/// Walk all contours and extract near-vertical edges.
///
/// An "edge" here is one segment between two consecutive on-curve
/// points. Off-curve (quadratic bezier control) points are treated
/// as ordinary vertices -- the flattening resolution is coarse
/// enough at this scale (stem detection, not rasterization) that
/// skipping curve flattening is acceptable. If accuracy mattered
/// more, we'd flatten curves first.
fn collect_vertical_edges(contours: &[Contour], near_vertical_ratio: f64) -> Vec<VerticalEdge> {
    let mut out = Vec::new();
    for contour in contours {
        let n = contour.points.len();
        if n < 2 {
            continue;
        }
        for i in 0..n {
            let p0 = contour.points[i];
            let p1 = contour.points[(i + 1) % n];
            let dx = p1.x - p0.x;
            let dy = p1.y - p0.y;
            let adx = dx.abs();
            let ady = dy.abs();
            if ady < 1.0 {
                continue; // not enough Y extent to be a stem edge
            }
            if adx > ady * near_vertical_ratio {
                continue; // not near-vertical
            }
            let (y_min, y_max) = if p0.y < p1.y {
                (p0.y, p1.y)
            } else {
                (p1.y, p0.y)
            };
            let ink_side: i8 = if dy > 0.0 {
                1
            } else if dy < 0.0 {
                -1
            } else {
                0
            };
            out.push(VerticalEdge {
                x: (p0.x + p1.x) * 0.5,
                y_min,
                y_max,
                ink_side,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ttf::{Contour, OutlinePoint};

    fn mkpt(x: f64, y: f64) -> OutlinePoint {
        OutlinePoint {
            x,
            y,
            on_curve: true,
        }
    }

    /// Simple rectangle (stem-shaped): left edge at x=100, right
    /// edge at x=180, height 400. Counter-clockwise winding so ink
    /// is on the interior. One discovered stem at (100, 80).
    #[test]
    fn rectangle_yields_one_stem() {
        let contour = Contour {
            points: vec![
                // CCW: bottom-left, bottom-right, top-right, top-left.
                mkpt(100.0, 0.0),
                mkpt(180.0, 0.0),
                mkpt(180.0, 400.0),
                mkpt(100.0, 400.0),
            ],
        };
        let stems = discover_vertical_stems(&[contour], &[], StemLimits::default());
        assert_eq!(stems.len(), 1);
        let (pos, width) = stems[0];
        assert!((pos - 100.0).abs() < 0.5);
        assert!((width - 80.0).abs() < 0.5);
    }

    /// Capital 'H' approximation: two stems (left + right verticals)
    /// plus a crossbar. Expected discovered stems: the two side
    /// verticals at widths ~80.
    #[test]
    fn h_glyph_yields_two_stems() {
        // Two separate rectangles (two contours) for simplicity.
        let left = Contour {
            points: vec![
                mkpt(100.0, 0.0),
                mkpt(180.0, 0.0),
                mkpt(180.0, 700.0),
                mkpt(100.0, 700.0),
            ],
        };
        let right = Contour {
            points: vec![
                mkpt(500.0, 0.0),
                mkpt(580.0, 0.0),
                mkpt(580.0, 700.0),
                mkpt(500.0, 700.0),
            ],
        };
        let stems = discover_vertical_stems(&[left, right], &[], StemLimits::default());
        assert_eq!(stems.len(), 2, "got {:?}", stems);
        assert!((stems[0].0 - 100.0).abs() < 0.5);
        assert!((stems[0].1 - 80.0).abs() < 0.5);
        assert!((stems[1].0 - 500.0).abs() < 0.5);
        assert!((stems[1].1 - 80.0).abs() < 0.5);
    }

    /// Declared v_stem at (100, 80) should dedupe the rectangle
    /// stem so the result is empty.
    #[test]
    fn declared_stem_dedupes() {
        let contour = Contour {
            points: vec![
                mkpt(100.0, 0.0),
                mkpt(180.0, 0.0),
                mkpt(180.0, 400.0),
                mkpt(100.0, 400.0),
            ],
        };
        let stems = discover_vertical_stems(&[contour], &[(100.0, 80.0)], StemLimits::default());
        assert!(stems.is_empty(), "expected dedupe, got {:?}", stems);
    }

    /// A clockwise-wound rectangle traverses the left edge DOWNWARD
    /// (ink on left, outside) and the right edge UPWARD (ink on right,
    /// outside) -- which from the fill rule is the exterior of the
    /// path, not a stem. The ink_side signs should sum to zero anyway,
    /// so the pair-up will still succeed. This test documents that
    /// this algorithm can't tell a cut-out counter from a stem without
    /// a winding analysis; the higher-level `discover_vertical_stems`
    /// callers must rely on the width filter + declared-dedupe to
    /// suppress false positives.
    #[test]
    fn clockwise_rectangle_still_pairs() {
        // CW order: bottom-left, top-left, top-right, bottom-right.
        // Left edge goes up , right edge goes down (-). ink_side
        // signs sum to zero, so the pair is accepted.
        let contour = Contour {
            points: vec![
                mkpt(100.0, 0.0),
                mkpt(100.0, 400.0),
                mkpt(180.0, 400.0),
                mkpt(180.0, 0.0),
            ],
        };
        let stems = discover_vertical_stems(&[contour], &[], StemLimits::default());
        assert_eq!(stems.len(), 1, "got {:?}", stems);
    }

    /// Two separate CCW rectangles both wound the same way produce
    /// FOUR valid stem candidates: the two rectangles themselves plus
    /// any cross-rectangle pairing within the width range. The
    /// `max_discovered_count` cap + declared dedupe keep this from
    /// polluting the hint table in practice. This test pins the
    /// current (conservative) behavior: at least the two "real"
    /// rectangle stems are present, and false-positive cross pairs
    /// are limited by the Y-overlap + width filters plus the cap.
    #[test]
    fn two_adjacent_rectangles_find_the_real_stems() {
        let a = Contour {
            points: vec![
                mkpt(100.0, 0.0),
                mkpt(180.0, 0.0),
                mkpt(180.0, 400.0),
                mkpt(100.0, 400.0),
            ],
        };
        let b = Contour {
            points: vec![
                mkpt(400.0, 0.0),
                mkpt(480.0, 0.0),
                mkpt(480.0, 400.0),
                mkpt(400.0, 400.0),
            ],
        };
        let stems = discover_vertical_stems(&[a, b], &[], StemLimits::default());
        // The two real stems MUST be present.
        assert!(
            stems
                .iter()
                .any(|&(p, w)| (p - 100.0).abs() < 0.5 && (w - 80.0).abs() < 0.5),
            "missing first real stem, got {:?}",
            stems
        );
        assert!(
            stems
                .iter()
                .any(|&(p, w)| (p - 400.0).abs() < 0.5 && (w - 80.0).abs() < 0.5),
            "missing second real stem, got {:?}",
            stems
        );
    }

    /// Width outside the allowed range should be rejected.
    #[test]
    fn width_filtered() {
        // Width 10 (too thin) and width 400 (too wide) both rejected.
        let too_thin = Contour {
            points: vec![
                mkpt(100.0, 0.0),
                mkpt(110.0, 0.0),
                mkpt(110.0, 400.0),
                mkpt(100.0, 400.0),
            ],
        };
        let too_wide = Contour {
            points: vec![
                mkpt(100.0, 0.0),
                mkpt(500.0, 0.0),
                mkpt(500.0, 400.0),
                mkpt(100.0, 400.0),
            ],
        };
        assert!(discover_vertical_stems(&[too_thin], &[], StemLimits::default()).is_empty());
        assert!(discover_vertical_stems(&[too_wide], &[], StemLimits::default()).is_empty());
    }

    /// A stem with only 20 FU of vertical overlap should be rejected
    /// by default (min_y_overlap_fu = 50).
    #[test]
    fn insufficient_y_overlap_rejected() {
        let contour = Contour {
            points: vec![
                mkpt(100.0, 0.0),
                mkpt(180.0, 0.0),
                mkpt(180.0, 20.0),
                mkpt(100.0, 20.0),
            ],
        };
        let stems = discover_vertical_stems(&[contour], &[], StemLimits::default());
        assert!(stems.is_empty());
    }

    /// The cap on discovered stems must hold: if we feed a forest
    /// of stem rectangles, at most `max_discovered_count` come out.
    #[test]
    fn max_count_cap_holds() {
        let contours: Vec<Contour> = (0..20)
            .map(|i| {
                let x0 = (i as f64) * 200.0;
                Contour {
                    points: vec![
                        mkpt(x0, 0.0),
                        mkpt(x0 + 80.0, 0.0),
                        mkpt(x0 + 80.0, 400.0),
                        mkpt(x0, 400.0),
                    ],
                }
            })
            .collect();
        let lim = StemLimits {
            max_discovered_count: 5,
            ..StemLimits::default()
        };
        let stems = discover_vertical_stems(&contours, &[], lim);
        assert_eq!(stems.len(), 5);
    }
}

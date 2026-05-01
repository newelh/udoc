//! Stem-level diagnostic probe for auto_hinter ().
//!
//! closed the delta-emission half of #194 by folding FT's
//! advance-rounding (zero-edge rows match byte-exact). Stem-rich rows still
//! diverge because `fit_stem_edges_anchor` emits different fitted-edge
//! positions than FT's `af_latin_hints_compute_stems`. The FT port slated
//! for needs a structured view of *which* stems
//! diverge and by how much, not a "this glyph regressed" signal.
//!
//! This module exposes the full stem list after edge-fitting (before advance
//! emission) so `tools/cursor-pair-diagnose --dump stems` can diff every
//! stem against FT's fitted-outline positions.
//!
//! Read-only. Gated on the `cursor-diag` Cargo feature so the module never
//! links into default builds and the hinter's observable behavior is
//! unchanged.
//!
//! Terminology mirrors FreeType (`src/autofit/aflatin.c`):
//!   - `pos_fu`       = fitted edge position in font units (`AF_Edge::pos`).
//!   - `opos_fu`      = original (pre-fit) edge position (`AF_Edge::opos`).
//!   - `fitted_pos_fu`= alias of `pos_fu` exposed explicitly for the probe,
//!                      so downstream tooling doesn't have to re-derive it.
//!   - `width_fu`     = signed font-unit distance from this stem edge to
//!                      its linked partner (positive when the link sits to
//!                      the right / above).
//!   - `flags`        = AF_EDGE_* flag bits (round, serif, etc.).

#![cfg(feature = "cursor-diag")]

use super::edges::{self, Edge, EDGE_SERIF, EDGE_STRONG};
use super::fitting::{self, AnchorMode};
use super::metrics::GlobalMetrics;
use super::segments::{self, Dimension};

/// Per-edge stem state captured after X-axis fitting.
///
/// `pos_fu` / `opos_fu` match FT's `AF_Edge::pos` / `AF_Edge::opos`
/// exactly (post-fit and pre-fit font-unit positions, respectively).
/// `width_fu` is the signed distance to the linked partner. `linked_idx`
/// points at the partner inside the same `Vec<StemProbe>` when a link
/// exists; `None` on serifs and unlinked edges.
#[derive(Debug, Clone, PartialEq)]
pub struct StemProbe {
    /// Index of this edge within `v_edges` at the time of probing.
    /// Matches `--detail` trailer indexing so diagnostic output composes.
    pub edge_idx: usize,
    /// Pre-fit edge position, font units. FT `AF_Edge::opos`.
    pub opos_fu: f64,
    /// Post-fit edge position, font units. FT `AF_Edge::pos`.
    pub pos_fu: f64,
    /// Alias of `pos_fu` for callers that want to read "fitted_pos"
    /// explicitly when scanning a table. Always equal to `pos_fu`.
    pub fitted_pos_fu: f64,
    /// Signed font-unit distance to the linked partner. Positive when
    /// the link is at a higher pos. `None` when this edge has no link
    /// (lone stem, serif, unlinked contour).
    pub width_fu: Option<f64>,
    /// Linked-partner index within the same probe slice. Preserves the
    /// original `Edge::link` topology so tooling can pair up stems.
    pub linked_idx: Option<usize>,
    /// Raw flag byte (EDGE_STRONG | EDGE_SERIF). Same bits as `Edge::flags`.
    pub flags: u8,
    /// FT `AF_EDGE_ROUND` equivalent; set when the edge is derived from
    /// a curve (apex of 'o', etc.).
    pub is_round: bool,
    /// FT-equivalent: was this edge grid-fit during the pass?
    pub fitted: bool,
    /// Number of segments backing this edge. Thin proxy for FT's
    /// `AF_Edge::num_linked`.
    pub segment_count: usize,
}

/// Run the full auto-hinter X-axis pipeline and capture the stem list
/// immediately after edge fitting.
///
/// `contours`: glyph outline in font units (x, y, on_curve).
/// `metrics`:  font-level auto-hinter metrics (blue zones, stem widths,
///              UPM).
/// `scale`:    font units to pixels (`ppem / units_per_em`).
///
/// Returns stems in detected-edge order. Empty vec when the glyph has no
/// v-dimension edges (space glyphs, zero-outline characters).
pub fn probe_stems(
    contours: &[Vec<(f64, f64, bool)>],
    metrics: &GlobalMetrics,
    scale: f64,
) -> Vec<StemProbe> {
    if contours.is_empty() || scale <= 0.0 {
        return Vec::new();
    }

    // Mirror the exact pipeline in `auto_hinter::hint_with_scratch` for the
    // X-axis (vertical-dimension) edges only. This matches what
    // `auto_hint_glyph_axes(_, _, _, HintAxes::Both)` produces modulo the
    // probe returning edges as first-class data instead of bbox-folded
    // deltas.
    let analysis = segments::tuples_to_contours(contours);
    let total_oncurve: usize = analysis.iter().map(|c| c.points.len()).sum();
    if total_oncurve < 4 {
        return Vec::new();
    }

    let mut v_segments =
        segments::detect_segments(&analysis, Dimension::Vertical, metrics.units_per_em);
    let mut v_edges = edges::detect_edges(&mut v_segments, Dimension::Vertical, scale);

    if v_edges.is_empty() {
        return Vec::new();
    }

    // Honour the env-var override the `probe_glyph_cursor` cursor-pair
    // probe honours, so a single test run can diff cascade vs anchor.
    let x_mode = match std::env::var("UDOC_XAXIS_FIT_MODE").ok().as_deref() {
        Some("cascade") => AnchorMode::Cascade,
        _ => AnchorMode::Anchor,
    };
    fitting::fit_edges_with(&mut v_edges, metrics, scale, x_mode);

    v_edges
        .iter()
        .enumerate()
        .map(|(idx, e)| edge_to_probe(idx, e, &v_edges))
        .collect()
}

fn edge_to_probe(idx: usize, edge: &Edge, all: &[Edge]) -> StemProbe {
    let (width_fu, linked_idx) = match edge.link {
        Some(link) if link < all.len() => {
            let other = &all[link];
            // Report the SIGNED font-unit distance so downstream tooling
            // doesn't have to remember which side of a stem pair it is
            // looking at. Negative width == partner is to the left/below.
            let width = other.pos - edge.pos;
            (Some(width), Some(link))
        }
        _ => (None, None),
    };
    StemProbe {
        edge_idx: idx,
        opos_fu: edge.pos,
        pos_fu: edge.fitted_pos,
        fitted_pos_fu: edge.fitted_pos,
        width_fu,
        linked_idx,
        flags: edge.flags & (EDGE_STRONG | EDGE_SERIF),
        is_round: edge.is_round,
        fitted: edge.fitted,
        segment_count: edge.segment_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_metrics() -> GlobalMetrics {
        GlobalMetrics {
            units_per_em: 1000,
            blue_zones: Vec::new(),
            dominant_h_width: 80.0,
            dominant_v_width: 80.0,
        }
    }

    #[test]
    fn probe_stems_empty_contours_returns_empty() {
        let m = plain_metrics();
        assert!(probe_stems(&[], &m, 0.04).is_empty());
    }

    #[test]
    fn probe_stems_rectangle_reports_two_linked_stems() {
        // 'I'-like rectangle with two vertical stems.
        let contours = vec![vec![
            (50.0, 0.0, true),
            (50.0, 700.0, true),
            (150.0, 700.0, true),
            (150.0, 0.0, true),
        ]];
        let stems = probe_stems(&contours, &plain_metrics(), 0.04);
        assert_eq!(stems.len(), 2, "rectangle should produce two v-edges");
        // Both stems linked to each other.
        assert!(stems.iter().all(|s| s.linked_idx.is_some()));
        // Width reciprocity: stem[i] width == -stem[link] width.
        for s in &stems {
            let link = s.linked_idx.unwrap();
            let partner = &stems[link];
            let sum = s.width_fu.unwrap_or(0.0) + partner.width_fu.unwrap_or(0.0);
            assert!(
                sum.abs() < 1e-9,
                "stem widths should be reciprocal, got {} + {}",
                s.width_fu.unwrap(),
                partner.width_fu.unwrap()
            );
        }
        // All edges fitted (scale=0.04 is body-text ppem).
        assert!(stems.iter().all(|s| s.fitted));
    }

    #[test]
    fn probe_stems_fitted_pos_is_pos_fu_alias() {
        let contours = vec![vec![
            (50.0, 0.0, true),
            (50.0, 700.0, true),
            (150.0, 700.0, true),
            (150.0, 0.0, true),
        ]];
        let stems = probe_stems(&contours, &plain_metrics(), 0.04);
        for s in &stems {
            assert_eq!(s.fitted_pos_fu, s.pos_fu);
        }
    }
}

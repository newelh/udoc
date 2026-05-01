//! FreeType-style auto-hinter for glyph outline grid-fitting.
//!
//! Analyzes glyph outline topology to detect stems (segments and edges),
//! computes global font metrics (blue zones, standard stem widths) from
//! reference glyphs, and grid-fits edges to pixel boundaries. Produces
//! hinted contours that the rasterizer renders with sharper, more consistent
//! anti-aliasing.
//!
//! The auto-hinter works with all font formats (CFF, Type1, TrueType) and
//! doesn't require declared stem hints from the font's Private DICT,
//! though it uses them as input when available.

#[cfg(feature = "test-internals")]
pub mod edges;
#[cfg(not(feature = "test-internals"))]
pub(crate) mod edges;
#[cfg(feature = "test-internals")]
pub mod fitting;
#[cfg(not(feature = "test-internals"))]
pub(crate) mod fitting;
/// Re-export the diagnostic probe module at the auto_hinter level for
/// tool convenience. Gated on `cursor-diag`.
#[cfg(feature = "cursor-diag")]
pub use fitting::probe;
/// Stem-level diagnostic probe for. Exposes the
/// stem list after edge-fitting, before advance emission. Read-only,
/// cursor-diag-gated.
#[cfg(feature = "cursor-diag")]
pub mod diag;
#[cfg(feature = "test-internals")]
pub mod latin;
#[cfg(not(feature = "test-internals"))]
pub(crate) mod latin;
#[cfg(feature = "test-internals")]
pub mod metrics;
#[cfg(not(feature = "test-internals"))]
pub(crate) mod metrics;
pub(crate) mod scratch;
#[cfg(feature = "test-internals")]
pub mod segments;
#[cfg(not(feature = "test-internals"))]
pub(crate) mod segments;

use super::ps_hints::HintAxis;
use edges::Edge;
use metrics::GlobalMetrics;
use segments::Dimension;

/// Which axes to auto-hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintAxes {
    /// Only fit horizontal features (baselines, x-height, stem heights).
    Y,
    /// Fit both axes. The X-axis hint requires caller to consume
    /// `HintedOutline::lsb_delta_fu` / `rsb_delta_fu` so advance-cursor drift
    /// can be compensated (emulates FreeType `lsb_delta`/`rsb_delta`).
    Both,
}

/// Result of auto-hinting: fitted contours plus advance-side deltas.
///
/// `lsb_delta_fu` / `rsb_delta_fu` are the font-unit shifts applied to the
/// outline's left/right bounding-box edges by the X-axis fit. They're the
/// analog of FreeType's `lsb_delta` / `rsb_delta` (but in font units, not
/// 26.6 pixels). Callers apply them to the advance cursor like FreeType does:
///   `pen_x += lsb_delta_fu`  (before composite)
///   `pen_x += advance + rsb_delta_fu`  (after composite)
#[derive(Debug, Clone)]
pub struct HintedOutline {
    /// Hinted contours as `(x, y, on_curve)` tuples in font units.
    pub contours: Vec<Vec<(f64, f64, bool)>>,
    /// Left-side-bearing delta in font units (FreeType advance-compensation).
    pub lsb_delta_fu: f64,
    /// Right-side-bearing delta in font units (FreeType advance-compensation).
    pub rsb_delta_fu: f64,
}

/// Auto-hint a glyph outline using topology-based analysis.
///
/// This is the main entry point. It:
/// 1. Detects segments from the outline geometry
/// 2. Groups segments into edges
/// 3. Grid-fits edges using blue zones and stem width rounding
/// 4. Applies fitted edge positions to all outline points via interpolation
///
/// `contours`: glyph outline points in font units (x, y, on_curve).
/// `metrics`: global font metrics (blue zones, stem widths).
/// `scale`: font units to pixels (font_size_px / units_per_em).
///
/// Returns hinted contours in the same format, with point positions adjusted.
/// Y-axis only; see `auto_hint_glyph_axes` for both-axis hinting with lsb/rsb
/// deltas.
pub fn auto_hint_glyph(
    contours: &[Vec<(f64, f64, bool)>],
    metrics: &GlobalMetrics,
    scale: f64,
) -> Vec<Vec<(f64, f64, bool)>> {
    auto_hint_glyph_axes(contours, metrics, scale, HintAxes::Y).contours
}

/// Auto-hint a glyph outline, selecting Y-only or both-axis fitting.
///
/// Returns `HintedOutline` with the fitted contours and font-unit
/// lsb/rsb deltas. When `axes == HintAxes::Y`, deltas are always 0.
pub fn auto_hint_glyph_axes(
    contours: &[Vec<(f64, f64, bool)>],
    metrics: &GlobalMetrics,
    scale: f64,
    axes: HintAxes,
) -> HintedOutline {
    if contours.is_empty() || scale <= 0.0 {
        return HintedOutline {
            contours: contours.to_vec(),
            lsb_delta_fu: 0.0,
            rsb_delta_fu: 0.0,
        };
    }

    scratch::with_scratch(|sb| hint_with_scratch(contours, metrics, scale, axes, sb))
}

/// Body of `auto_hint_glyph_axes` run against caller-supplied scratch
/// buffers. Keeps the allocator quiet on the per-glyph hot path; after the
/// first few glyphs every working Vec is capacity-preserved and reused.
fn hint_with_scratch(
    contours: &[Vec<(f64, f64, bool)>],
    metrics: &GlobalMetrics,
    scale: f64,
    axes: HintAxes,
    sb: &mut scratch::ScratchBuffers,
) -> HintedOutline {
    // Convert contours to analysis format, flattening bezier curves.
    // This is essential for CFF fonts where the shape is entirely curves
    // with few on-curve points -- segment detection needs the actual curve path.
    let live_contours = segments::tuples_to_contours_into(contours, sb);
    let analysis = &sb.analysis_contours[..live_contours];

    // If very few on-curve points available, segment detection is unreliable.
    let total_oncurve: usize = analysis.iter().map(|c| c.points.len()).sum();
    if total_oncurve < 4 {
        return HintedOutline {
            contours: contours.to_vec(),
            lsb_delta_fu: 0.0,
            rsb_delta_fu: 0.0,
        };
    }

    // Detect segments and edges in both dimensions. UPM is threaded
    // through so that font-unit thresholds scale for non-1000-UPM fonts.
    segments::detect_segments_into(
        analysis,
        Dimension::Horizontal,
        metrics.units_per_em,
        &mut sb.h_segments,
    );
    segments::detect_segments_into(
        analysis,
        Dimension::Vertical,
        metrics.units_per_em,
        &mut sb.v_segments,
    );

    edges::detect_edges_into(
        &mut sb.h_segments,
        Dimension::Horizontal,
        scale,
        &mut sb.h_edges,
        &mut sb.dim_indices,
        &mut sb.link_counts,
        &mut sb.serif_counts,
    );
    edges::detect_edges_into(
        &mut sb.v_segments,
        Dimension::Vertical,
        scale,
        &mut sb.v_edges,
        &mut sb.dim_indices,
        &mut sb.link_counts,
        &mut sb.serif_counts,
    );

    // Semantic classification of crossbars (e.g. mid-stroke of 'e', 'A', 'H',
    // middle arm of 'E'). The fitter uses this flag to bump crossbar stem
    // widths by +1 device pixel at body-text ppem so the crossbar survives
    // rasterizer coverage / OCR binarization. Outline topology is still
    // available here; doing this at the edge layer avoids the false
    // positives that a bitmap-only post-rasterize check hits on 'e' vs 'o'
    // at 12 ppem (M-26 v1).
    edges::classify_crossbars(&mut sb.h_edges, &sb.h_segments, analysis, scale);

    // If no edges detected, return the original contours unchanged.
    if sb.h_edges.is_empty() && sb.v_edges.is_empty() {
        return HintedOutline {
            contours: contours.to_vec(),
            lsb_delta_fu: 0.0,
            rsb_delta_fu: 0.0,
        };
    }

    // Fit edges to pixel grid. Y-axis (h_edges) cascades from blue zones.
    // X-axis (v_edges) uses FreeType's anchor model for better preservation
    // of inter-stem spacing. Use env var to toggle cascade for comparison.
    let x_mode = match std::env::var("UDOC_XAXIS_FIT_MODE").ok().as_deref() {
        Some("cascade") => fitting::AnchorMode::Cascade,
        _ => fitting::AnchorMode::Anchor,
    };
    fitting::fit_edges_with_scratch(
        &mut sb.h_edges,
        metrics,
        scale,
        fitting::AnchorMode::Cascade,
        &mut sb.zone_fits,
        &mut sb.order,
        &mut sb.fitted_pairs,
    );
    fitting::fit_edges_with_scratch(
        &mut sb.v_edges,
        metrics,
        scale,
        x_mode,
        &mut sb.zone_fits,
        &mut sb.order,
        &mut sb.fitted_pairs,
    );

    // Populate axis-map scratch in place. Reset() has already cleared them.
    populate_axis_from_edges(&mut sb.y_axis, &sb.h_edges, scale);
    let use_x_axis = matches!(axes, HintAxes::Both);
    if use_x_axis {
        populate_axis_from_edges(&mut sb.x_axis, &sb.v_edges, scale);
    }

    // Compute outline bbox (original) to derive lsb/rsb deltas.
    let (mut x_min, mut x_max) = (f64::MAX, f64::MIN);
    for contour in contours {
        for &(x, _, _) in contour {
            if x < x_min {
                x_min = x;
            }
            if x > x_max {
                x_max = x;
            }
        }
    }

    let y_axis = &sb.y_axis;
    let x_axis = &sb.x_axis;
    let hinted_contours: Vec<Vec<(f64, f64, bool)>> = contours
        .iter()
        .map(|contour| {
            contour
                .iter()
                .map(|&(x, y, on_curve)| {
                    let hinted_y = y_axis.interpolate(y);
                    let hinted_x = if use_x_axis { x_axis.interpolate(x) } else { x };
                    (hinted_x, hinted_y, on_curve)
                })
                .collect()
        })
        .collect();

    // lsb/rsb deltas measure how much the X-axis fit shifted the outline's
    // left/right edges (font units). Callers apply to advance cursor.
    let (lsb_delta_fu, rsb_delta_fu) = if use_x_axis {
        let new_xmin = x_axis.interpolate(x_min);
        let new_xmax = x_axis.interpolate(x_max);
        (new_xmin - x_min, new_xmax - x_max)
    } else {
        (0.0, 0.0)
    };

    HintedOutline {
        contours: hinted_contours,
        lsb_delta_fu,
        rsb_delta_fu,
    }
}

/// Fill `axis` in place with the (original, fitted) pairs from fitted
/// edges, sorting and equalizing counters. Caller clears `axis` first;
/// the auto-hinter scratch pool does this via `ScratchBuffers::reset`.
fn populate_axis_from_edges(axis: &mut HintAxis, edges: &[Edge], scale: f64) {
    for edge in edges {
        if edge.fitted {
            axis.add(edge.pos, edge.fitted_pos);
        }
    }
    axis.sort();
    axis.equalize_counters(scale);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_hint_empty() {
        let metrics = GlobalMetrics {
            units_per_em: 1000,
            blue_zones: Vec::new(),
            dominant_h_width: 0.0,
            dominant_v_width: 0.0,
        };
        let result = auto_hint_glyph(&[], &metrics, 0.04);
        assert!(result.is_empty());
    }

    #[test]
    fn auto_hint_produces_aligned_stems() {
        // A simple rectangle glyph (like 'I') with vertical stems at x=50 and x=150.
        let contours = vec![vec![
            (50.0, 0.0, true),
            (50.0, 700.0, true),
            (150.0, 700.0, true),
            (150.0, 0.0, true),
        ]];

        let metrics = GlobalMetrics {
            units_per_em: 1000,
            blue_zones: Vec::new(),
            dominant_h_width: 0.0,
            dominant_v_width: 100.0,
        };

        let scale = 0.04; // 40px at 1000 UPM
        let hinted = auto_hint_glyph(&contours, &metrics, scale);

        assert_eq!(hinted.len(), 1);
        assert_eq!(hinted[0].len(), 4);

        // Check that the vertical edges are on pixel boundaries.
        let left_x = hinted[0][0].0;
        let right_x = hinted[0][2].0;
        let left_px = left_x * scale;
        let right_px = right_x * scale;

        assert!(
            (left_px - left_px.round()).abs() < 0.05,
            "left edge at {:.2} px should be grid-aligned",
            left_px
        );
        assert!(
            (right_px - right_px.round()).abs() < 0.05,
            "right edge at {:.2} px should be grid-aligned",
            right_px
        );

        // Stem width should be an integer number of pixels.
        let width_px = right_px - left_px;
        assert!(
            (width_px - width_px.round()).abs() < 0.05,
            "stem width {:.2} px should be integer",
            width_px
        );
    }

    #[test]
    fn auto_hint_preserves_point_count() {
        // Glyph with on-curve and off-curve points.
        let contours = vec![vec![
            (0.0, 0.0, true),
            (250.0, 500.0, false), // off-curve control point
            (500.0, 0.0, true),
        ]];

        let metrics = GlobalMetrics {
            units_per_em: 1000,
            blue_zones: Vec::new(),
            dominant_h_width: 0.0,
            dominant_v_width: 0.0,
        };

        let hinted = auto_hint_glyph(&contours, &metrics, 0.04);
        assert_eq!(hinted[0].len(), 3);
        // Off-curve flag preserved.
        assert!(!hinted[0][1].2);
    }
}

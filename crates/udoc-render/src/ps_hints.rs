//! PostScript hint interpreter for Type1 and CFF font grid-fitting.
//!
//! Implements the FreeType-equivalent algorithm for snapping glyph outlines
//! to the pixel grid using blue zones (alignment zones) and stem hints.
//! Produces wider, more consistent strokes by building a unified
//! (original, snapped) position mapping and interpolating all points through it.

use udoc_font::ttf::StemHints;
use udoc_font::type1::Type1HintValues;

/// Round stem width to integer pixels. Sub-pixel stems (< 1px) are forced
/// to 1px to prevent disappearing. All other stems use normal rounding.
fn round_stem_width(width_px: f64) -> f64 {
    if width_px < 1.0 {
        // Sub-pixel stems: force to 1px minimum visibility.
        1.0
    } else {
        // Visible stems: normal rounding.
        width_px.round().max(1.0)
    }
}

/// A (original, snapped) position pair for the hint mapping.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SnapPair {
    pub original: f64,
    pub snapped: f64,
}

/// Sorted hint axis: a list of snapped reference positions.
/// Used by both the PS hint interpreter and the auto-hinter for
/// proportional interpolation of outline points.
#[derive(Default)]
pub(crate) struct HintAxis {
    pairs: Vec<SnapPair>,
}

impl HintAxis {
    pub(crate) fn new() -> Self {
        Self { pairs: Vec::new() }
    }

    /// Clear out existing pairs while preserving the inner Vec's capacity.
    /// Used by the auto-hinter scratch pool to reuse HintAxis instances
    /// across per-glyph calls without dropping/realloc.
    pub(crate) fn clear(&mut self) {
        self.pairs.clear();
    }

    pub(crate) fn add(&mut self, original: f64, snapped: f64) {
        self.pairs.push(SnapPair { original, snapped });
    }

    pub(crate) fn sort(&mut self) {
        self.pairs.sort_by(|a, b| {
            a.original
                .partial_cmp(&b.original)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        // Deduplicate overlapping pairs (keep the first).
        self.pairs
            .dedup_by(|a, b| (a.original - b.original).abs() < 0.5);
    }

    /// Equalize counter spaces between adjacent snap pairs. After individual
    /// stems are fitted, scan for adjacent pairs and round the gap between
    /// them to an integer pixel count. This prevents asymmetric spacing
    /// inside characters like 'H', 'n', 'm'.
    pub(crate) fn equalize_counters(&mut self, scale: f64) {
        if self.pairs.len() < 2 || scale <= 0.0 {
            return;
        }
        for i in 1..self.pairs.len() {
            let prev_snapped = self.pairs[i - 1].snapped;
            let curr_snapped = self.pairs[i].snapped;
            let counter_fu = curr_snapped - prev_snapped;
            let counter_px = counter_fu * scale;
            // Only adjust counters that are at least 1px wide (skip stem
            // edge pairs that are part of the same stem).
            if counter_px > 0.5 {
                let rounded_px = counter_px.round();
                let target_fu = rounded_px / scale;
                self.pairs[i].snapped = prev_snapped + target_fu;
            }
        }
    }

    /// Snap a coordinate to the nearest reference if close, otherwise
    /// interpolate proportionally between bracketing references.
    pub(crate) fn interpolate(&self, coord: f64) -> f64 {
        if self.pairs.is_empty() {
            return coord;
        }

        // Find nearest reference.
        let idx = self.pairs.partition_point(|p| p.original < coord);

        // Check if close to a specific reference (within threshold).
        let threshold = 15.0; // font units
        let mut best_dist = f64::MAX;
        let mut best_delta = 0.0;
        for &check in &[idx.wrapping_sub(1), idx] {
            if check < self.pairs.len() {
                let dist = (self.pairs[check].original - coord).abs();
                if dist < best_dist {
                    best_dist = dist;
                    best_delta = self.pairs[check].snapped - self.pairs[check].original;
                }
            }
        }

        if best_dist <= threshold {
            // Close to a reference: snap directly.
            return coord + best_delta;
        }

        // Between two references: interpolate proportionally.
        if idx == 0 {
            coord + (self.pairs[0].snapped - self.pairs[0].original)
        } else if idx >= self.pairs.len() {
            let last = &self.pairs[self.pairs.len() - 1];
            coord + (last.snapped - last.original)
        } else {
            let lo = &self.pairs[idx - 1];
            let hi = &self.pairs[idx];
            let span = hi.original - lo.original;
            if span.abs() < 0.001 {
                coord + (lo.snapped - lo.original)
            } else {
                let t = (coord - lo.original) / span;
                lo.snapped + t * (hi.snapped - lo.snapped)
            }
        }
    }
}

/// Apply PostScript hint grid-fitting to glyph outline points.
///
/// Builds a unified (original, snapped) position mapping from blue zones
/// and stem hints, then interpolates all points through it. This produces
/// wider, more consistent strokes by coordinating all grid-fitting in a
/// single proportional pass.
pub(crate) fn ps_hint_glyph(
    contours: &[Vec<(f64, f64, bool)>],
    stem_hints: Option<&StemHints>,
    hint_values: Option<&Type1HintValues>,
    scale: f64,
) -> Vec<Vec<(f64, f64, bool)>> {
    if scale <= 0.0 {
        return contours.to_vec();
    }

    let stems = stem_hints.cloned().unwrap_or_default();
    let hv = hint_values;

    // Build Y-axis mapping (horizontal features: baselines, x-height, stems).
    let y_axis = build_y_axis(&stems.h_stems, hv, scale);

    // Build X-axis mapping (vertical features: stems only, no blue zones).
    let x_axis = build_x_axis(&stems.v_stems, hv, scale);

    // Apply the mapping to all outline points.
    if y_axis.pairs.is_empty() && x_axis.pairs.is_empty() {
        return contours.to_vec();
    }

    contours
        .iter()
        .map(|contour| {
            contour
                .iter()
                .map(|&(x, y, on_curve)| {
                    let hinted_x = x_axis.interpolate(x);
                    let hinted_y = y_axis.interpolate(y);
                    (hinted_x, hinted_y, on_curve)
                })
                .collect()
        })
        .collect()
}

/// Build the Y-axis hint mapping from horizontal stems and blue zones.
fn build_y_axis(
    h_stems: &[(f64, f64)],
    hint_values: Option<&Type1HintValues>,
    scale: f64,
) -> HintAxis {
    let mut axis = HintAxis::new();

    // Blue zone alignment with overshoot suppression.
    if let Some(hv) = hint_values {
        // Determine if overshoot should be suppressed at this size.
        // Type1 fonts are 1000 UPM, so ppem = scale * 1000.
        let ppem = scale * 1000.0;
        let suppress_overshoot = hv.blue_scale > 0.0 && ppem < 1.0 / (2.0 * hv.blue_scale);

        // Top zones (baseline and above): snap reference edge to grid.
        for &(bottom, top) in &hv.blue_values {
            let ref_edge = if bottom >= 0.0 { bottom } else { top };
            let ref_px = ref_edge * scale;
            let snapped_px = ref_px.round();
            let delta = (snapped_px - ref_px) / scale;

            if suppress_overshoot {
                // At small sizes, pull both edges to the reference edge
                // position. This prevents round characters (o, e, c) from
                // appearing taller than flat characters (x, z).
                axis.add(bottom, ref_edge + delta);
                axis.add(top, ref_edge + delta);
            } else {
                // Normal: preserve zone height, shift both edges equally.
                axis.add(bottom, bottom + delta);
                axis.add(top, top + delta);
            }
        }

        // Bottom zones (descenders): snap top edge to grid.
        for &(bottom, top) in &hv.other_blues {
            let ref_px = top * scale;
            let snapped_px = ref_px.round();
            let delta = (snapped_px - ref_px) / scale;

            if suppress_overshoot {
                axis.add(bottom, top + delta);
                axis.add(top, top + delta);
            } else {
                axis.add(bottom, bottom + delta);
                axis.add(top, top + delta);
            }
        }
    }

    // Stem fitting with ceiling rounding for thin stems.
    let std_hw = hint_values.map(|hv| hv.std_hw).unwrap_or(0.0);
    let std_hw_px = if std_hw > 0.0 {
        round_stem_width(std_hw * scale)
    } else {
        0.0
    };

    for &(pos, height) in h_stems {
        let bottom = pos;
        let top = pos + height;
        let width_px = height.abs() * scale;

        let target_width =
            if std_hw_px > 0.0 && (width_px - std_hw * scale).abs() < std_hw * scale * 0.3 {
                std_hw_px
            } else {
                round_stem_width(width_px)
            };

        // Snap stem edges to pixel grid. FreeType's auto-hinter aligns
        // edges to exact pixel boundaries for sharp rendering. Snap the
        // bottom edge to the nearest pixel, then place the top edge at
        // bottom + target_width to maintain the rounded stem width.
        let bottom_px = bottom * scale;
        let snapped_bottom_px = bottom_px.round();
        let snapped_top_px = snapped_bottom_px + target_width;
        let top_px = top * scale;

        axis.add(bottom, bottom + (snapped_bottom_px - bottom_px) / scale);
        axis.add(top, top + (snapped_top_px - top_px) / scale);
    }

    axis.sort();
    // Equalize counter spaces between stems.
    axis.equalize_counters(scale);
    axis
}

/// Build the X-axis hint mapping from vertical stems.
fn build_x_axis(
    v_stems: &[(f64, f64)],
    hint_values: Option<&Type1HintValues>,
    scale: f64,
) -> HintAxis {
    let mut axis = HintAxis::new();
    let std_vw = hint_values.map(|hv| hv.std_vw).unwrap_or(0.0);
    let std_vw_px = if std_vw > 0.0 {
        round_stem_width(std_vw * scale)
    } else {
        0.0
    };

    for &(pos, width) in v_stems {
        let left = pos;
        let right = pos + width;
        let width_px = width.abs() * scale;

        let target_width =
            if std_vw_px > 0.0 && (width_px - std_vw * scale).abs() < std_vw * scale * 0.3 {
                std_vw_px
            } else {
                round_stem_width(width_px)
            };

        // Snap stem edges to pixel grid. Left edge rounds to nearest pixel,
        // right edge placed at left + target_width for consistent stem width.
        let left_px = left * scale;
        let snapped_left = left_px.round();
        let snapped_right = snapped_left + target_width;
        let right_px = right * scale;

        axis.add(left, left + (snapped_left - left_px) / scale);
        axis.add(right, right + (snapped_right - right_px) / scale);
    }

    axis.sort();
    // Equalize counter spaces between stems.
    axis.equalize_counters(scale);
    axis
}

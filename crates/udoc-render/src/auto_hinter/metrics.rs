//! Global metrics computation from reference glyph analysis.
//!
//! Scans specific reference glyphs to detect blue zones (alignment zones)
//! and standard stem widths. These metrics are computed once per font and
//! cached for use during per-glyph auto-hinting.

use super::latin::{self, BlueZoneType};
use super::segments;
use crate::font_cache::FontCache;

/// A blue zone: an alignment region detected from reference glyph extrema.
#[derive(Debug, Clone)]
pub struct BlueZone {
    /// Reference (flat) edge position in font units.
    pub reference: f64,
    /// Overshoot (round) edge position in font units.
    pub overshoot: f64,
    /// Whether this zone is the x-height top zone. FreeType tags this zone
    /// (`AF_LATIN_BLUE_ADJUSTMENT` via `AF_LATIN_IS_X_HEIGHT_BLUE`) and uses
    /// its overshoot to bump the y-scale so round small-letter tops align to
    /// the pixel grid (`aflatin.c` `af_latin_metrics_scale_dim`). Without
    /// this bump, round-topped glyphs like 'o' at ppem=12 render 1 px
    /// shorter than their flat-topped siblings; see issue #175.
    pub is_x_height: bool,
}

/// Global metrics for auto-hinting, computed once per font.
#[derive(Debug, Clone)]
pub struct GlobalMetrics {
    /// Units per em for this font.
    pub units_per_em: u16,
    /// Detected blue zones from reference glyph analysis.
    pub blue_zones: Vec<BlueZone>,
    /// Dominant horizontal stem width (font units).
    pub dominant_h_width: f64,
    /// Dominant vertical stem width (font units).
    pub dominant_v_width: f64,
}

/// Compute global metrics for a font by analyzing reference glyphs.
///
/// Falls back to Private DICT values (blue_values, StdHW, StdVW) when
/// reference glyphs are unavailable (common in subset fonts).
pub(crate) fn compute_global_metrics(
    font_cache: &mut FontCache,
    font_name: &str,
) -> Option<GlobalMetrics> {
    let units_per_em = font_cache.units_per_em(font_name);

    // Compute blue zones from reference glyph extrema.
    let mut blue_zones = Vec::new();
    for &(zone_type, chars) in latin::BLUE_ZONE_CHARS {
        if let Some(zone) = compute_blue_zone(font_cache, font_name, zone_type, chars) {
            blue_zones.push(zone);
        }
    }

    // If we got no blue zones from glyph analysis, try Private DICT values.
    if blue_zones.is_empty() {
        if let Some(hv) = font_cache.ps_hint_values(font_name) {
            // Type1 convention: blue_values[0] is the baseline pair,
            // blue_values[1] is the x-height pair (first TOP zone). Tag
            // that one so x-height scale alignment kicks in.
            let mut top_zone_idx = 0usize;
            for &(bottom, top) in &hv.blue_values {
                let is_top = bottom >= 0.0;
                let is_x_height = is_top && top_zone_idx == 0;
                if is_top {
                    top_zone_idx += 1;
                }
                blue_zones.push(BlueZone {
                    reference: if is_top { bottom } else { top },
                    overshoot: if is_top { top } else { bottom },
                    is_x_height,
                });
            }
            for &(bottom, top) in &hv.other_blues {
                blue_zones.push(BlueZone {
                    reference: top,
                    overshoot: bottom,
                    is_x_height: false,
                });
            }
        }
    }

    // Compute dominant stem widths from reference glyph analysis.
    let (dom_h, dom_v) = compute_dominant_widths(font_cache, font_name);

    // Fall back to Private DICT stem widths if analysis found nothing.
    let mut dominant_h_width = if dom_h > 0.0 {
        dom_h
    } else {
        font_cache
            .ps_hint_values(font_name)
            .map(|hv| hv.std_hw)
            .unwrap_or(0.0)
    };
    let mut dominant_v_width = if dom_v > 0.0 {
        dom_v
    } else {
        font_cache
            .ps_hint_values(font_name)
            .map(|hv| hv.std_vw)
            .unwrap_or(0.0)
    };

    // Prefer Private DICT stem widths when available. They are authoritative.
    // Our auto-detected widths can pick up counters (gaps) instead of stems
    // when the segment linker pairs across features.
    if let Some(hv) = font_cache.ps_hint_values(font_name) {
        if hv.std_hw > 0.0 {
            dominant_h_width = hv.std_hw;
        }
        if hv.std_vw > 0.0 {
            dominant_v_width = hv.std_vw;
        }
    }

    // Need at least some metrics to be useful.
    if blue_zones.is_empty() && dominant_h_width <= 0.0 && dominant_v_width <= 0.0 {
        return None;
    }

    Some(GlobalMetrics {
        units_per_em,
        blue_zones,
        dominant_h_width,
        dominant_v_width,
    })
}

/// Compute a single blue zone from reference glyph extrema.
fn compute_blue_zone(
    font_cache: &mut FontCache,
    font_name: &str,
    zone_type: BlueZoneType,
    chars: &[char],
) -> Option<BlueZone> {
    let is_top = latin::zone_is_top(zone_type);
    let is_bottom_zone = latin::zone_is_bottom_zone(zone_type);

    let mut flat_positions: Vec<f64> = Vec::new();
    let mut round_positions: Vec<f64> = Vec::new();

    for &ch in chars {
        let outline = font_cache.glyph_outline(font_name, ch);
        let outline = match outline {
            Some(o) if !o.contours.is_empty() => o,
            _ => continue,
        };

        // Find Y extrema across all contours.
        let (y_min, y_max) = glyph_y_extrema(&outline.contours);

        if is_top {
            // For top zones, the relevant position is y_max.
            // Flat reference = extremum from flat-topped glyphs (T, H, E, Z, L, I)
            // Round overshoot = extremum from round glyphs (O, C, Q, S, o, e, s, c)
            let is_round = matches!(ch, 'O' | 'C' | 'Q' | 'S' | 'o' | 'e' | 's' | 'c');
            if is_round {
                round_positions.push(y_max);
            } else {
                flat_positions.push(y_max);
            }
        } else {
            // For bottom zones, the relevant position is y_min (or y_max for baseline).
            if is_bottom_zone && !matches!(zone_type, BlueZoneType::Descender) {
                // Baseline: use y_min of letters that sit on the baseline.
                let is_round = matches!(ch, 'o' | 'e' | 's' | 'c');
                if is_round {
                    round_positions.push(y_min);
                } else {
                    flat_positions.push(y_min);
                }
            } else {
                // Descender: use y_min.
                let is_round = matches!(ch, 'g' | 'j');
                if is_round {
                    round_positions.push(y_min);
                } else {
                    flat_positions.push(y_min);
                }
            }
        }
    }

    // Need at least one reference to define the zone.
    if flat_positions.is_empty() && round_positions.is_empty() {
        return None;
    }

    // Use median of flat positions as reference, median of round as overshoot.
    let reference = if !flat_positions.is_empty() {
        median(&mut flat_positions)
    } else {
        median(&mut round_positions)
    };

    let overshoot = if !round_positions.is_empty() {
        median(&mut round_positions)
    } else {
        reference
    };

    Some(BlueZone {
        reference,
        overshoot,
        is_x_height: matches!(zone_type, BlueZoneType::SmallTop),
    })
}

/// Compute dominant horizontal and vertical stem widths from reference glyphs.
fn compute_dominant_widths(font_cache: &mut FontCache, font_name: &str) -> (f64, f64) {
    let mut h_widths: Vec<f64> = Vec::new();
    let mut v_widths: Vec<f64> = Vec::new();

    let upm = font_cache.units_per_em(font_name);
    let max_stem_width = latin::max_stem_width(upm);

    for &ch in latin::STEM_REFERENCE_CHARS {
        let outline = match font_cache.glyph_outline(font_name, ch) {
            Some(o) if !o.contours.is_empty() => o,
            _ => continue,
        };

        // Use declared stem hints if available.
        for &(_, height) in &outline.stem_hints.h_stems {
            let w = height.abs();
            if w > 1.0 && w < max_stem_width {
                h_widths.push(w);
            }
        }
        for &(_, width) in &outline.stem_hints.v_stems {
            let w = width.abs();
            if w > 1.0 && w < max_stem_width {
                v_widths.push(w);
            }
        }

        // Also detect stems from outline geometry.
        let contours = segments::outline_to_contours(&outline.contours);
        let h_segs = segments::detect_segments(&contours, segments::Dimension::Horizontal, upm);
        let v_segs = segments::detect_segments(&contours, segments::Dimension::Vertical, upm);

        for pair in segments::find_linked_pairs(&h_segs) {
            let w = (h_segs[pair.0].pos - h_segs[pair.1].pos).abs();
            if w > 1.0 && w < max_stem_width {
                h_widths.push(w);
            }
        }
        for pair in segments::find_linked_pairs(&v_segs) {
            let w = (v_segs[pair.0].pos - v_segs[pair.1].pos).abs();
            if w > 1.0 && w < max_stem_width {
                v_widths.push(w);
            }
        }
    }

    let dom_h = dominant_width(&mut h_widths);
    let dom_v = dominant_width(&mut v_widths);
    (dom_h, dom_v)
}

/// Find the most common width in a list (using histogram binning).
fn dominant_width(widths: &mut [f64]) -> f64 {
    if widths.is_empty() {
        return 0.0;
    }
    widths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // Bin widths: group values within 10% of each other.
    let mut best_count = 0;
    let mut best_width = widths[0];
    let mut i = 0;
    while i < widths.len() {
        let ref_width = widths[i];
        let threshold = ref_width * 0.1;
        let mut count = 0;
        let mut sum = 0.0;
        let mut j = i;
        while j < widths.len() && (widths[j] - ref_width).abs() <= threshold {
            count += 1;
            sum += widths[j];
            j += 1;
        }
        if count > best_count {
            best_count = count;
            best_width = sum / count as f64;
        }
        i = j;
    }
    best_width
}

/// Compute Y extrema across all contours of a glyph.
fn glyph_y_extrema(contours: &[udoc_font::ttf::Contour]) -> (f64, f64) {
    let mut y_min = f64::MAX;
    let mut y_max = f64::MIN;
    for contour in contours {
        for pt in &contour.points {
            y_min = y_min.min(pt.y);
            y_max = y_max.max(pt.y);
        }
    }
    (y_min, y_max)
}

/// Compute the median of a mutable slice (sorts in place).
fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

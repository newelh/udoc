//! Latin-script reference glyph tables and constants.
//!
//! FreeType's auto-hinter uses specific characters to detect blue zones
//! (alignment zones like baseline, x-height, cap height). Each zone type
//! has a set of reference glyphs whose extrema define the zone boundaries.

/// Blue zone types for Latin script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlueZoneType {
    CapitalTop,
    CapitalBottom,
    SmallTop,
    SmallBottom,
    Ascender,
    Descender,
}

/// Reference characters for each blue zone type.
/// FreeType uses these to detect alignment positions from glyph outlines.
pub(crate) const BLUE_ZONE_CHARS: &[(BlueZoneType, &[char])] = &[
    (
        BlueZoneType::CapitalTop,
        &['T', 'H', 'E', 'Z', 'O', 'C', 'Q', 'S'],
    ),
    (BlueZoneType::CapitalBottom, &['H', 'E', 'Z', 'L', 'T', 'I']),
    (BlueZoneType::SmallTop, &['x', 'z', 'r', 'o', 'e', 's', 'c']),
    (
        BlueZoneType::SmallBottom,
        &['x', 'z', 'r', 'o', 'e', 's', 'c'],
    ),
    (BlueZoneType::Ascender, &['b', 'd', 'f', 'h', 'k', 'l']),
    (BlueZoneType::Descender, &['p', 'q', 'g', 'j', 'y']),
];

/// Reference characters for standard stem width analysis.
pub(crate) const STEM_REFERENCE_CHARS: &[char] = &['i', 'l', 'I', 'T', 'H', 'o', 'O'];

/// Whether a zone type measures from the top or bottom of glyphs.
pub(crate) fn zone_is_top(zone_type: BlueZoneType) -> bool {
    matches!(
        zone_type,
        BlueZoneType::CapitalTop | BlueZoneType::SmallTop | BlueZoneType::Ascender
    )
}

/// Whether a zone type uses round (curved) reference or flat reference.
/// Top zones use flat top + round overshoot above.
/// Bottom zones use flat bottom + round overshoot below.
pub(crate) fn zone_is_bottom_zone(zone_type: BlueZoneType) -> bool {
    matches!(
        zone_type,
        BlueZoneType::CapitalBottom | BlueZoneType::SmallBottom | BlueZoneType::Descender
    )
}

/// Threshold for direction classification. FreeType uses 14x ratio
/// (~4.1 degrees from axis). A point's movement is axis-aligned if
/// the on-axis component is > 14x the off-axis component.
/// Previous value (0.25 = 4x ratio, ~14 degrees) was too loose,
/// detecting diagonal features as axis-aligned segments.
pub(crate) const DIRECTION_RATIO: f64 = 14.0;

/// Reference units-per-em that our hardcoded font-unit thresholds were
/// calibrated against. Thresholds below scale linearly with the actual
/// font's UPM via [`scale_to_upm`] so that non-1000-UPM fonts (common
/// TrueType: 2048, some CFF subsets: 1024) get equivalent treatment.
pub(crate) const REFERENCE_UPM: f64 = 1000.0;

/// Minimum segment length at 1000 UPM. Use [`min_segment_length`] to
/// obtain a UPM-scaled value.
const MIN_SEGMENT_LENGTH_AT_1000: f64 = 10.0;

/// Maximum distance at 1000 UPM for two segments to be considered part
/// of the same stem. Use [`max_stem_width`] to obtain a UPM-scaled
/// value.
const MAX_STEM_WIDTH_AT_1000: f64 = 300.0;

/// Scale a 1000-UPM font-unit threshold to the actual font's UPM.
/// Zero or tiny UPM falls back to the unscaled value.
pub(crate) fn scale_to_upm(threshold_at_1000: f64, units_per_em: u16) -> f64 {
    let upm = units_per_em as f64;
    if upm < 1.0 {
        return threshold_at_1000;
    }
    threshold_at_1000 * (upm / REFERENCE_UPM)
}

/// Minimum segment length in font units, UPM-scaled from 10.0 at 1000 UPM.
pub(crate) fn min_segment_length(units_per_em: u16) -> f64 {
    scale_to_upm(MIN_SEGMENT_LENGTH_AT_1000, units_per_em)
}

/// Maximum stem-pair distance in font units, UPM-scaled from 300.0 at 1000 UPM.
pub(crate) fn max_stem_width(units_per_em: u16) -> f64 {
    scale_to_upm(MAX_STEM_WIDTH_AT_1000, units_per_em)
}

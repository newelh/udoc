//! Cell style and number format parser for XLSX.
//!
//! Parses `xl/styles.xml` to build a mapping from style index (the `s`
//! attribute on `<c>` elements) to number format code, font info, fill
//! colors, and alignment.

use std::collections::HashMap;
use std::sync::Arc;

use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use crate::error::{Result, ResultExt};
use udoc_containers::xml::{attr_value, toggle_attr, XmlEvent, XmlReader};

/// Maximum number of cellXfs entries we'll process (safety limit).
const MAX_XF_ENTRIES: usize = 100_000;

/// Maximum number of custom number format entries (safety limit).
const MAX_CUSTOM_FORMATS: usize = 10_000;

/// Maximum number of font entries we'll process (safety limit).
const MAX_FONT_ENTRIES: usize = 100_000;

/// Maximum number of fill entries we'll process (safety limit).
const MAX_FILL_ENTRIES: usize = 100_000;

/// Maximum number of border entries we'll process (safety limit).
const MAX_BORDER_ENTRIES: usize = 100_000;

/// A parsed border entry from the `<borders>` section of styles.xml.
/// Tracks presence only (not color/style/width).
#[derive(Debug, Clone, Default)]
pub(crate) struct BorderEntry {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

impl BorderEntry {
    /// Returns true if any border side is present.
    #[allow(dead_code)]
    pub fn any(&self) -> bool {
        self.left || self.right || self.top || self.bottom
    }
}

/// A parsed font entry from the `<fonts>` section of styles.xml.
#[derive(Debug, Clone, Default)]
pub(crate) struct FontEntry {
    pub color: Option<[u8; 3]>,
    pub name: Option<String>,
    pub size: Option<f64>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

/// A parsed stylesheet mapping style IDs to format codes, fonts, fills,
/// and alignment.
#[derive(Debug, Clone, Default)]
pub(crate) struct StyleSheet {
    /// Maps style index (xf position in cellXfs) to numFmtId.
    xf_to_num_fmt_id: Vec<u32>,
    /// Custom number format codes keyed by numFmtId.
    custom_formats: HashMap<u32, String>,
    /// Parsed font entries from `<fonts>`.
    fonts: Vec<FontEntry>,
    /// Maps style index to font index (from `<cellXfs><xf fontId="N"/>`).
    xf_to_font_id: Vec<u32>,
    /// Parsed fill colors from `<fills>`.
    fills: Vec<Option<[u8; 3]>>,
    /// Maps style index to fill index (from `<cellXfs><xf fillId="N"/>`).
    xf_to_fill_id: Vec<u32>,
    /// Maps style index to horizontal alignment string.
    xf_to_alignment: Vec<Option<String>>,
    /// Parsed border entries from `<borders>`.
    #[allow(dead_code)]
    borders: Vec<BorderEntry>,
    /// Maps style index to border index (from `<cellXfs><xf borderId="N"/>`).
    #[allow(dead_code)]
    xf_to_border_id: Vec<u32>,
}

impl StyleSheet {
    /// Look up the number format code for a cell style index.
    ///
    /// Returns `None` if the style index is out of range or maps to
    /// the General format (numFmtId 0).
    pub(crate) fn format_code(&self, style_index: usize) -> Option<&str> {
        let num_fmt_id = self.xf_to_num_fmt_id.get(style_index).copied()?;
        // Built-in ID 0 is "General" (no formatting)
        if num_fmt_id == 0 {
            return None;
        }
        // Check custom formats first, then built-in.
        if let Some(code) = self.custom_formats.get(&num_fmt_id) {
            return Some(code.as_str());
        }
        builtin_format_code(num_fmt_id)
    }

    /// Get the raw numFmtId for a style index.
    pub(crate) fn num_fmt_id(&self, style_index: usize) -> Option<u32> {
        self.xf_to_num_fmt_id.get(style_index).copied()
    }

    /// Look up the font entry for a cell style index.
    ///
    /// Returns `None` if the style index or font index is out of range.
    pub(crate) fn font_entry(&self, style_index: u32) -> Option<&FontEntry> {
        let font_id = self.xf_to_font_id.get(style_index as usize).copied()?;
        self.fonts.get(font_id as usize)
    }

    /// Look up the fill color for a cell style index.
    ///
    /// Returns `None` if the style index or fill index is out of range,
    /// or if the fill has no foreground color.
    pub(crate) fn fill_color(&self, style_index: u32) -> Option<[u8; 3]> {
        let fill_id = self.xf_to_fill_id.get(style_index as usize).copied()?;
        self.fills.get(fill_id as usize).copied().flatten()
    }

    /// Look up the horizontal alignment for a cell style index.
    ///
    /// Returns `None` if the style index is out of range or no alignment set.
    pub(crate) fn alignment(&self, style_index: u32) -> Option<&str> {
        self.xf_to_alignment
            .get(style_index as usize)
            .and_then(|a| a.as_deref())
    }

    /// Look up the border entry for a cell style index.
    ///
    /// Returns `None` if the style index or border index is out of range.
    #[cfg(test)]
    pub(crate) fn border_entry(&self, style_index: u32) -> Option<&BorderEntry> {
        let border_id = self.xf_to_border_id.get(style_index as usize).copied()?;
        self.borders.get(border_id as usize)
    }
}

/// Parse an ARGB hex string (e.g., "FFFF0000") into an RGB [u8; 3].
/// Skips the first two hex chars (alpha channel). Returns None on invalid input.
pub(crate) fn parse_argb_color(s: &str) -> Option<[u8; 3]> {
    udoc_core::document::Color::from_argb_hex(s).map(|c| c.to_array())
}

/// Parse `xl/styles.xml` into a StyleSheet.
pub(crate) fn parse_styles(data: &[u8], diag: &Arc<dyn DiagnosticsSink>) -> Result<StyleSheet> {
    let mut reader = XmlReader::new(data).context("creating XML reader for styles")?;
    let mut custom_formats: HashMap<u32, String> = HashMap::new();
    let mut xf_list: Vec<u32> = Vec::new();
    let mut xf_font_ids: Vec<u32> = Vec::new();
    let mut xf_fill_ids: Vec<u32> = Vec::new();
    let mut xf_alignments: Vec<Option<String>> = Vec::new();
    let mut in_cell_xfs = false;
    let mut in_num_fmts = false;
    let mut xf_truncated = false;
    let mut fmt_truncated = false;

    // Font parsing state
    let mut fonts: Vec<FontEntry> = Vec::new();
    let mut in_fonts = false;
    let mut in_font = false;
    let mut current_font = FontEntry::default();
    let mut fonts_truncated = false;

    // Fill parsing state
    let mut fills: Vec<Option<[u8; 3]>> = Vec::new();
    let mut in_fills = false;
    let mut in_fill = false;
    let mut in_pattern_fill = false;
    let mut current_fill_color: Option<[u8; 3]> = None;
    let mut fills_truncated = false;

    // Border parsing state
    let mut borders: Vec<BorderEntry> = Vec::new();
    let mut in_borders = false;
    let mut in_border = false;
    let mut current_border = BorderEntry::default();
    let mut borders_truncated = false;

    // Theme color diagnostic: emit once per stylesheet, not per element.
    let mut theme_color_warned = false;

    // cellXfs child element tracking: need to capture <alignment> inside <xf>
    let mut in_xf = false;
    let mut current_xf_alignment: Option<String> = None;
    let mut xf_border_ids: Vec<u32> = Vec::new();

    loop {
        match reader.next_event().context("reading styles XML")? {
            XmlEvent::StartElement {
                local_name,
                attributes,
                ..
            } => match local_name.as_ref() {
                "numFmts" => {
                    in_num_fmts = true;
                }
                "numFmt" if in_num_fmts => {
                    if custom_formats.len() >= MAX_CUSTOM_FORMATS {
                        if !fmt_truncated {
                            fmt_truncated = true;
                            diag.warning(Warning::new(
                                "XlsxNumFmtLimit",
                                format!(
                                    "more than {MAX_CUSTOM_FORMATS} custom number formats, \
                                     truncating"
                                ),
                            ));
                        }
                    } else {
                        let id =
                            attr_value(&attributes, "numFmtId").and_then(|s| s.parse::<u32>().ok());
                        let code = attr_value(&attributes, "formatCode").map(|s| s.to_string());
                        if let (Some(id), Some(code)) = (id, code) {
                            custom_formats.insert(id, code);
                        }
                    }
                }
                // --- Fonts section ---
                "fonts" => {
                    in_fonts = true;
                }
                "font" if in_fonts => {
                    in_font = true;
                    current_font = FontEntry::default();
                }
                "b" if in_font => {
                    current_font.bold = toggle_attr(attr_value(&attributes, "val"));
                }
                "i" if in_font => {
                    current_font.italic = toggle_attr(attr_value(&attributes, "val"));
                }
                "u" if in_font => {
                    let val = attr_value(&attributes, "val");
                    current_font.underline = val.is_none_or(|v| v != "none");
                }
                "strike" if in_font => {
                    current_font.strikethrough = toggle_attr(attr_value(&attributes, "val"));
                }
                "sz" if in_font => {
                    if let Some(v) = attr_value(&attributes, "val") {
                        current_font.size = v.parse::<f64>().ok();
                    }
                }
                "name" if in_font => {
                    if let Some(v) = attr_value(&attributes, "val") {
                        current_font.name = Some(v.to_string());
                    }
                }
                "color" if in_font => {
                    if let Some(rgb) = attr_value(&attributes, "rgb") {
                        current_font.color = parse_argb_color(rgb);
                    } else if attr_value(&attributes, "theme").is_some() && !theme_color_warned {
                        theme_color_warned = true;
                        diag.warning(Warning::new(
                            "XlsxThemeColor",
                            "theme-based colors in styles are not resolved; \
                             direct RGB colors are extracted",
                        ));
                    }
                }
                // --- Fills section ---
                "fills" => {
                    in_fills = true;
                }
                "fill" if in_fills => {
                    in_fill = true;
                    current_fill_color = None;
                }
                "patternFill" if in_fill => {
                    in_pattern_fill = true;
                }
                "fgColor" if in_pattern_fill => {
                    if let Some(rgb) = attr_value(&attributes, "rgb") {
                        current_fill_color = parse_argb_color(rgb);
                    } else if attr_value(&attributes, "theme").is_some() && !theme_color_warned {
                        theme_color_warned = true;
                        diag.warning(Warning::new(
                            "XlsxThemeColor",
                            "theme-based colors in styles are not resolved; \
                             direct RGB colors are extracted",
                        ));
                    }
                }
                // --- Borders section ---
                "borders" => {
                    in_borders = true;
                }
                "border" if in_borders => {
                    in_border = true;
                    current_border = BorderEntry::default();
                }
                "left" if in_border => {
                    current_border.left = has_border_style(&attributes);
                }
                "right" if in_border => {
                    current_border.right = has_border_style(&attributes);
                }
                "top" if in_border => {
                    current_border.top = has_border_style(&attributes);
                }
                "bottom" if in_border => {
                    current_border.bottom = has_border_style(&attributes);
                }
                // --- cellXfs section ---
                "cellXfs" => {
                    in_cell_xfs = true;
                }
                "xf" if in_cell_xfs => {
                    in_xf = true;
                    current_xf_alignment = None;
                    if xf_list.len() >= MAX_XF_ENTRIES {
                        if !xf_truncated {
                            xf_truncated = true;
                            diag.warning(Warning::new(
                                "XlsxStyleLimit",
                                format!("more than {MAX_XF_ENTRIES} cell styles, truncating"),
                            ));
                        }
                    } else {
                        let num_fmt_id = attr_value(&attributes, "numFmtId")
                            .and_then(|s| s.parse::<u32>().ok())
                            .unwrap_or(0);
                        xf_list.push(num_fmt_id);

                        let font_id = attr_value(&attributes, "fontId")
                            .and_then(|s| s.parse::<u32>().ok())
                            .unwrap_or(0);
                        xf_font_ids.push(font_id);

                        let fill_id = attr_value(&attributes, "fillId")
                            .and_then(|s| s.parse::<u32>().ok())
                            .unwrap_or(0);
                        xf_fill_ids.push(fill_id);

                        let border_id = attr_value(&attributes, "borderId")
                            .and_then(|s| s.parse::<u32>().ok())
                            .unwrap_or(0);
                        xf_border_ids.push(border_id);
                    }
                }
                "alignment" if in_cell_xfs && in_xf => {
                    if let Some(h) = attr_value(&attributes, "horizontal") {
                        current_xf_alignment = Some(h.to_string());
                    }
                }
                _ => {}
            },
            XmlEvent::EndElement { local_name, .. } => match local_name.as_ref() {
                "numFmts" => in_num_fmts = false,
                "cellXfs" => in_cell_xfs = false,
                "fonts" => in_fonts = false,
                "font" if in_font => {
                    if fonts.len() < MAX_FONT_ENTRIES {
                        fonts.push(std::mem::take(&mut current_font));
                    } else if !fonts_truncated {
                        fonts_truncated = true;
                        diag.warning(Warning::new(
                            "XlsxFontLimit",
                            format!("more than {MAX_FONT_ENTRIES} font entries, truncating"),
                        ));
                    }
                    in_font = false;
                }
                "fills" => in_fills = false,
                "borders" => in_borders = false,
                "border" if in_border => {
                    if borders.len() < MAX_BORDER_ENTRIES {
                        borders.push(std::mem::take(&mut current_border));
                    } else if !borders_truncated {
                        borders_truncated = true;
                        diag.warning(Warning::new(
                            "XlsxBorderLimit",
                            format!("more than {MAX_BORDER_ENTRIES} border entries, truncating"),
                        ));
                    }
                    in_border = false;
                }
                "fill" if in_fill => {
                    if fills.len() < MAX_FILL_ENTRIES {
                        fills.push(current_fill_color.take());
                    } else if !fills_truncated {
                        fills_truncated = true;
                        diag.warning(Warning::new(
                            "XlsxFillLimit",
                            format!("more than {MAX_FILL_ENTRIES} fill entries, truncating"),
                        ));
                    }
                    in_fill = false;
                    in_pattern_fill = false;
                }
                "patternFill" => in_pattern_fill = false,
                "xf" if in_cell_xfs => {
                    in_xf = false;
                    let alignment = current_xf_alignment.take();
                    if !xf_truncated {
                        // Push alignment collected during xf child parsing.
                        xf_alignments.push(alignment);
                    }
                }
                _ => {}
            },
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(StyleSheet {
        xf_to_num_fmt_id: xf_list,
        custom_formats,
        fonts,
        xf_to_font_id: xf_font_ids,
        fills,
        xf_to_fill_id: xf_fill_ids,
        xf_to_alignment: xf_alignments,
        borders,
        xf_to_border_id: xf_border_ids,
    })
}

/// Built-in number format codes (ECMA-376 Part 1, 18.8.30).
/// Returns the format code string for a built-in numFmtId.
fn builtin_format_code(id: u32) -> Option<&'static str> {
    match id {
        0 => Some("General"),
        1 => Some("0"),
        2 => Some("0.00"),
        3 => Some("#,##0"),
        4 => Some("#,##0.00"),
        5 => Some("$#,##0_);($#,##0)"),
        6 => Some("$#,##0_);[Red]($#,##0)"),
        7 => Some("$#,##0.00_);($#,##0.00)"),
        8 => Some("$#,##0.00_);[Red]($#,##0.00)"),
        9 => Some("0%"),
        10 => Some("0.00%"),
        11 => Some("0.00E+00"),
        12 => Some("# ?/?"),
        13 => Some("# ??/??"),
        14 => Some("mm-dd-yy"),
        15 => Some("d-mmm-yy"),
        16 => Some("d-mmm"),
        17 => Some("mmm-yy"),
        18 => Some("h:mm AM/PM"),
        19 => Some("h:mm:ss AM/PM"),
        20 => Some("h:mm"),
        21 => Some("h:mm:ss"),
        22 => Some("m/d/yy h:mm"),
        37 => Some("#,##0_);(#,##0)"),
        38 => Some("#,##0_);[Red](#,##0)"),
        39 => Some("#,##0.00_);(#,##0.00)"),
        40 => Some("#,##0.00_);[Red](#,##0.00)"),
        45 => Some("mm:ss"),
        46 => Some("[h]:mm:ss"),
        47 => Some("mmss.0"),
        48 => Some("##0.0E+0"),
        49 => Some("@"),
        _ => None,
    }
}

/// Check if an XML element's `style` attribute indicates a visible border.
/// A border side is present if the element has a `style` attribute
/// that is not "none".
fn has_border_style(attributes: &[udoc_containers::xml::Attribute<'_>]) -> bool {
    match attr_value(attributes, "style") {
        Some(s) => s != "none",
        None => false,
    }
}

/// Check if a numFmtId corresponds to a date/time format.
pub(crate) fn is_date_format_id(id: u32) -> bool {
    matches!(id, 14..=22 | 45..=47)
}

/// Check if a format code string looks like a date format.
/// Heuristic: contains multi-character date/time tokens (yy, mm, dd, hh, ss)
/// rather than single characters, to avoid false positives on text like
/// "0.0 shares" or "yes"/"no" conditional formats.
pub(crate) fn is_date_format_code(code: &str) -> bool {
    // Strip bracketed sections like [Red], [$$-en-US], [$-F800]
    let cleaned = strip_bracketed_sections(code);
    // Strip quoted literal strings (e.g., "hours", "days") so their
    // characters don't trigger false positives.
    let unquoted = strip_quoted_literals(&cleaned);
    let lower = unquoted.to_ascii_lowercase();

    // Look for multi-character date/time tokens to avoid false positives.
    let has_year = lower.contains("yy");
    let has_day = lower.contains("dd") || has_day_token(&lower);
    let has_hour = lower.contains("hh") || has_hour_token(&lower);
    let has_second = lower.contains("ss");
    let has_month = lower.contains("mm") || lower.contains("mmm");

    // "m" alone is ambiguous (month vs minute). It's a date if:
    //   - m appears with year or day context (month)
    //   - m appears with hour or second context (minute)
    let has_single_m = lower.contains('m');
    let has_date_context = has_year || has_day;
    let has_time_context = has_hour || has_second;

    has_year
        || has_day
        || has_hour
        || has_second
        || has_month
        || (has_single_m && (has_date_context || has_time_context))
}

/// Check for standalone "d" token (not part of a word).
/// Matches "d" surrounded by non-letter chars or at string boundaries.
fn has_day_token(lower: &str) -> bool {
    for (i, ch) in lower.char_indices() {
        if ch == 'd' {
            let prev_alpha = i > 0 && lower.as_bytes()[i - 1].is_ascii_alphabetic();
            let next_alpha = i + 1 < lower.len() && lower.as_bytes()[i + 1].is_ascii_alphabetic();
            if !prev_alpha && !next_alpha {
                return true;
            }
        }
    }
    false
}

/// Check for standalone "h" token.
fn has_hour_token(lower: &str) -> bool {
    for (i, ch) in lower.char_indices() {
        if ch == 'h' {
            let prev_alpha = i > 0 && lower.as_bytes()[i - 1].is_ascii_alphabetic();
            let next_alpha = i + 1 < lower.len() && lower.as_bytes()[i + 1].is_ascii_alphabetic();
            if !prev_alpha && !next_alpha {
                return true;
            }
        }
    }
    false
}

/// Strip double-quoted literal strings from format codes.
/// E.g., `0.00" hours"` becomes `0.00`.
fn strip_quoted_literals(code: &str) -> String {
    let mut result = String::with_capacity(code.len());
    let mut in_quote = false;
    for ch in code.chars() {
        if ch == '"' {
            in_quote = !in_quote;
        } else if !in_quote {
            result.push(ch);
        }
    }
    result
}

/// Strip bracketed sections from format codes (e.g., [Red], [$-409]).
fn strip_bracketed_sections(code: &str) -> String {
    let mut result = String::with_capacity(code.len());
    let mut depth = 0usize;
    for ch in code.chars() {
        match ch {
            '[' => depth += 1,
            ']' if depth > 0 => depth -= 1,
            _ if depth == 0 => result.push(ch),
            _ => {}
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use udoc_core::diagnostics::NullDiagnostics;

    use super::*;

    fn null_diag() -> Arc<dyn DiagnosticsSink> {
        Arc::new(NullDiagnostics)
    }

    #[test]
    fn parse_basic_styles() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <numFmts count="1">
        <numFmt numFmtId="164" formatCode="yyyy-mm-dd"/>
    </numFmts>
    <cellXfs count="3">
        <xf numFmtId="0"/>
        <xf numFmtId="14"/>
        <xf numFmtId="164"/>
    </cellXfs>
</styleSheet>"#;

        let ss = parse_styles(xml, &null_diag()).unwrap();
        assert_eq!(ss.num_fmt_id(0), Some(0)); // General
        assert_eq!(ss.num_fmt_id(1), Some(14)); // Built-in date
        assert_eq!(ss.num_fmt_id(2), Some(164)); // Custom date

        assert_eq!(ss.format_code(0), None); // General returns None
        assert_eq!(ss.format_code(1), Some("mm-dd-yy"));
        assert_eq!(ss.format_code(2), Some("yyyy-mm-dd"));
    }

    #[test]
    fn parse_empty_styles() {
        let xml =
            br#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <cellXfs count="0"/>
</styleSheet>"#;

        let ss = parse_styles(xml, &null_diag()).unwrap();
        assert_eq!(ss.format_code(0), None);
    }

    #[test]
    fn builtin_format_codes() {
        assert_eq!(builtin_format_code(0), Some("General"));
        assert_eq!(builtin_format_code(1), Some("0"));
        assert_eq!(builtin_format_code(14), Some("mm-dd-yy"));
        assert_eq!(builtin_format_code(49), Some("@"));
        assert_eq!(builtin_format_code(100), None);
    }

    #[test]
    fn is_date_format_detection() {
        assert!(is_date_format_id(14));
        assert!(is_date_format_id(22));
        assert!(!is_date_format_id(0));
        assert!(!is_date_format_id(1));
        assert!(!is_date_format_id(49));
    }

    #[test]
    fn date_format_code_detection() {
        assert!(is_date_format_code("yyyy-mm-dd"));
        assert!(is_date_format_code("d-mmm-yy"));
        assert!(is_date_format_code("h:mm:ss"));
        assert!(is_date_format_code("m/d/yy h:mm"));
        assert!(!is_date_format_code("0.00"));
        assert!(!is_date_format_code("#,##0"));
        assert!(!is_date_format_code("General"));
    }

    #[test]
    fn date_format_code_no_false_positives() {
        // Text containing date-like single characters should NOT match.
        assert!(!is_date_format_code(r#"0.0" shares""#));
        assert!(!is_date_format_code(r#"0" days""#));
        assert!(!is_date_format_code(r#"0" hours""#));
    }

    #[test]
    fn strip_bracketed() {
        assert_eq!(strip_bracketed_sections("[Red]0.00"), "0.00");
        assert_eq!(strip_bracketed_sections("[$-409]d-mmm-yy"), "d-mmm-yy");
        assert_eq!(strip_bracketed_sections("no brackets"), "no brackets");
    }

    #[test]
    fn parse_fonts_and_fills() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <fonts count="2">
        <font>
            <sz val="11"/>
            <name val="Calibri"/>
        </font>
        <font>
            <b/>
            <i/>
            <u/>
            <strike/>
            <sz val="14"/>
            <color rgb="FFFF0000"/>
            <name val="Arial"/>
        </font>
    </fonts>
    <fills count="2">
        <fill><patternFill patternType="none"/></fill>
        <fill><patternFill patternType="solid"><fgColor rgb="FF00FF00"/></patternFill></fill>
    </fills>
    <cellXfs count="2">
        <xf numFmtId="0" fontId="0" fillId="0"/>
        <xf numFmtId="0" fontId="1" fillId="1">
            <alignment horizontal="center"/>
        </xf>
    </cellXfs>
</styleSheet>"#;

        let ss = parse_styles(xml, &null_diag()).unwrap();

        // Font 0: default
        let f0 = ss.font_entry(0).unwrap();
        assert_eq!(f0.name.as_deref(), Some("Calibri"));
        assert_eq!(f0.size, Some(11.0));
        assert!(!f0.bold);
        assert!(!f0.italic);
        assert!(f0.color.is_none());

        // Font 1: bold, italic, underline, strikethrough, red, Arial 14pt
        let f1 = ss.font_entry(1).unwrap();
        assert!(f1.bold);
        assert!(f1.italic);
        assert!(f1.underline);
        assert!(f1.strikethrough);
        assert_eq!(f1.size, Some(14.0));
        assert_eq!(f1.color, Some([255, 0, 0]));
        assert_eq!(f1.name.as_deref(), Some("Arial"));

        // Fill: style 1 has green fill
        assert!(ss.fill_color(0).is_none());
        assert_eq!(ss.fill_color(1), Some([0, 255, 0]));

        // Alignment: style 1 is centered
        assert!(ss.alignment(0).is_none());
        assert_eq!(ss.alignment(1), Some("center"));
    }

    #[test]
    fn font_entry_out_of_range() {
        let ss = StyleSheet::default();
        assert!(ss.font_entry(0).is_none());
        assert!(ss.font_entry(999).is_none());
    }

    #[test]
    fn fill_color_out_of_range() {
        let ss = StyleSheet::default();
        assert!(ss.fill_color(0).is_none());
        assert!(ss.fill_color(999).is_none());
    }

    #[test]
    fn alignment_out_of_range() {
        let ss = StyleSheet::default();
        assert!(ss.alignment(0).is_none());
        assert!(ss.alignment(999).is_none());
    }

    #[test]
    fn parse_argb_valid() {
        assert_eq!(parse_argb_color("FFFF0000"), Some([255, 0, 0]));
        assert_eq!(parse_argb_color("FF00FF00"), Some([0, 255, 0]));
        assert_eq!(parse_argb_color("FF0000FF"), Some([0, 0, 255]));
        assert_eq!(parse_argb_color("00FFFFFF"), Some([255, 255, 255]));
    }

    #[test]
    fn parse_argb_invalid() {
        assert_eq!(parse_argb_color("short"), None);
        assert_eq!(parse_argb_color(""), None);
        assert_eq!(parse_argb_color("FFXXYYZZ"), None);
        // Multi-byte UTF-8 that reaches 8+ bytes must not panic.
        assert_eq!(parse_argb_color("\u{00e9}\u{00e9}\u{00e9}\u{00e9}"), None);
    }

    #[test]
    fn parse_borders_with_styles() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <borders count="3">
        <border>
            <left/><right/><top/><bottom/><diagonal/>
        </border>
        <border>
            <left style="thin"><color rgb="FF000000"/></left>
            <right style="thin"><color rgb="FF000000"/></right>
            <top style="thin"><color rgb="FF000000"/></top>
            <bottom style="thin"><color rgb="FF000000"/></bottom>
            <diagonal/>
        </border>
        <border>
            <left style="none"/>
            <right style="medium"><color rgb="FF0000FF"/></right>
            <top/><bottom style="dashed"/>
        </border>
    </borders>
    <cellXfs count="3">
        <xf borderId="0"/>
        <xf borderId="1"/>
        <xf borderId="2"/>
    </cellXfs>
</styleSheet>"#;

        let ss = parse_styles(xml, &null_diag()).unwrap();

        // Border 0: no styles on any side
        let b0 = ss.border_entry(0).unwrap();
        assert!(!b0.left);
        assert!(!b0.right);
        assert!(!b0.any());

        // Border 1: all sides thin
        let b1 = ss.border_entry(1).unwrap();
        assert!(b1.left);
        assert!(b1.right);
        assert!(b1.top);
        assert!(b1.bottom);
        assert!(b1.any());

        // Border 2: mixed (left=none, right=medium, top=none, bottom=dashed)
        let b2 = ss.border_entry(2).unwrap();
        assert!(!b2.left);
        assert!(b2.right);
        assert!(!b2.top);
        assert!(b2.bottom);
        assert!(b2.any());
    }

    #[test]
    fn parse_no_borders_section() {
        let xml =
            br#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <cellXfs count="1"><xf borderId="0"/></cellXfs>
</styleSheet>"#;
        let ss = parse_styles(xml, &null_diag()).unwrap();
        assert!(ss.border_entry(0).is_none());
    }
}

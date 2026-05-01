//! Number and date formatting for XLSX cell values.
//!
//! Converts raw numeric values to display strings based on numFmtId
//! codes (both built-in and custom). Handles the Excel serial date
//! system including the 1900 Lotus bug.

use crate::styles::{is_date_format_code, is_date_format_id, StyleSheet};

/// Format a cell's numeric value using its style.
///
/// If the style maps to a date format, converts the serial number to
/// an ISO 8601 date string. Otherwise returns the raw number string
/// (possibly with formatting applied).
pub(crate) fn format_cell_value(
    value_text: &str,
    stylesheet: &StyleSheet,
    style_index: Option<usize>,
    date_1904: bool,
) -> String {
    let style_index = match style_index {
        Some(i) => i,
        None => return format_number_string(value_text),
    };

    let num_fmt_id = match stylesheet.num_fmt_id(style_index) {
        Some(id) => id,
        None => return format_number_string(value_text),
    };

    // Check if this is a date format.
    let is_date = if is_date_format_id(num_fmt_id) {
        true
    } else if let Some(code) = stylesheet.format_code(style_index) {
        is_date_format_code(code)
    } else {
        false
    };

    if is_date {
        if let Ok(serial) = value_text.parse::<f64>() {
            return serial_to_iso_date(serial, date_1904);
        }
    }

    // Check for percentage format (numFmtId 9 or 10, or custom with %).
    let format_code = stylesheet.format_code(style_index);
    let is_percentage =
        matches!(num_fmt_id, 9 | 10) || format_code.map(|c| c.contains('%')).unwrap_or(false);

    if is_percentage {
        if let Ok(n) = value_text.parse::<f64>() {
            let pct = n * 100.0;
            // Determine decimal places from format code.
            // Built-in 9 = "0%" (0 decimals), 10 = "0.00%" (2 decimals).
            // Custom: count digits after '.' before '%'.
            let decimals = if num_fmt_id == 9 {
                0
            } else if num_fmt_id == 10 {
                2
            } else if let Some(code) = format_code {
                pct_decimal_places(code)
            } else {
                2
            };
            return match decimals {
                0 => format!("{:.0}%", pct),
                d => format!("{:.prec$}%", pct, prec = d),
            };
        }
    }

    format_number_string(value_text)
}

/// Count decimal places in a percentage format code.
/// E.g., "0%" -> 0, "0.00%" -> 2, "0.0%" -> 1.
fn pct_decimal_places(code: &str) -> usize {
    // Find the '%' and look backwards for decimal digits.
    if let Some(pct_pos) = code.find('%') {
        let before = &code[..pct_pos];
        if let Some(dot_pos) = before.rfind('.') {
            // Count '0' and '#' chars between dot and end (before %)
            return before[dot_pos + 1..]
                .chars()
                .filter(|&c| c == '0' || c == '#')
                .count();
        }
    }
    0
}

/// Clean up a raw number string for display.
/// Strips unnecessary trailing zeros after decimal point.
fn format_number_string(value: &str) -> String {
    // If it parses as an integer (no decimal point), return as-is.
    if !value.contains('.') {
        return value.to_string();
    }

    // Parse as f64 and format to strip trailing zeros.
    if let Ok(n) = value.parse::<f64>() {
        // Check if it's actually an integer value.
        if n.fract() == 0.0 && n.abs() < i64::MAX as f64 {
            return format!("{}", n as i64);
        }
        // Format with enough precision but strip trailing zeros.
        return format!("{}", n);
    }

    value.to_string()
}

/// Convert an Excel serial date number to an ISO 8601 date string.
///
/// Excel uses a serial date system where:
/// - 1900 epoch (default): 1 = January 1, 1900
/// - 1904 epoch (Mac): 1 = January 2, 1904
///
/// The 1900 epoch has the famous Lotus 1-2-3 bug: it incorrectly treats
/// 1900 as a leap year, so serial 60 = February 29, 1900 (which doesn't
/// exist). We preserve this behavior for compatibility.
pub(crate) fn serial_to_iso_date(serial: f64, epoch_1904: bool) -> String {
    if serial < 0.0 {
        return format!("{serial}");
    }

    let (date_part, time_frac) = if epoch_1904 {
        serial_to_ymd_1904(serial)
    } else {
        serial_to_ymd_1900(serial)
    };

    match date_part {
        Some((year, month, day)) => {
            if time_frac > 1e-10 {
                let total_seconds = ((time_frac * 86400.0).round() as u32).min(86399);
                let hours = total_seconds / 3600;
                let minutes = (total_seconds % 3600) / 60;
                let seconds = total_seconds % 60;
                format!(
                    "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
                    year, month, day, hours, minutes, seconds
                )
            } else {
                format!("{:04}-{:02}-{:02}", year, month, day)
            }
        }
        None => format!("{serial}"),
    }
}

/// Convert serial to (year, month, day) using 1900 epoch.
/// Returns (Some((y,m,d)), time_fraction) or (None, 0.0) on failure.
fn serial_to_ymd_1900(serial: f64) -> (Option<(i32, u32, u32)>, f64) {
    let day_num = serial.floor() as i64;
    let time_frac = serial - serial.floor();

    if day_num < 1 {
        return (None, 0.0);
    }

    // Handle the Lotus 1-2-3 bug: serial 60 = Feb 29, 1900 (nonexistent).
    if day_num == 60 {
        return (Some((1900, 2, 29)), time_frac);
    }

    // Adjust for the Lotus bug: days after 60 are off by one.
    let adjusted = if day_num > 60 { day_num - 1 } else { day_num };

    // Convert to days since epoch. Serial 1 = Jan 1, 1900.
    // We use a simple algorithm: convert to a Julian Day Number then to
    // Gregorian date.
    let days_since_1900_jan_1 = adjusted - 1; // 0 = Jan 1, 1900

    // Base Julian Day for Jan 1, 1900 = 2415021
    let jdn = 2415021 + days_since_1900_jan_1;
    let (y, m, d) = jdn_to_gregorian(jdn);

    (Some((y, m, d)), time_frac)
}

/// Convert serial to (year, month, day) using 1904 epoch.
fn serial_to_ymd_1904(serial: f64) -> (Option<(i32, u32, u32)>, f64) {
    let day_num = serial.floor() as i64;
    let time_frac = serial - serial.floor();

    if day_num < 0 {
        return (None, 0.0);
    }

    // 1904 epoch: serial 0 = Jan 1, 1904
    // Base Julian Day for Jan 1, 1904 = 2416481
    let jdn = 2416481 + day_num;
    let (y, m, d) = jdn_to_gregorian(jdn);

    (Some((y, m, d)), time_frac)
}

/// Convert Julian Day Number to Gregorian (year, month, day).
/// Algorithm from Richards (2013) via Wikipedia.
fn jdn_to_gregorian(jdn: i64) -> (i32, u32, u32) {
    let a = jdn + 32044;
    let b = (4 * a + 3) / 146097;
    let c = a - (146097 * b) / 4;
    let d = (4 * c + 3) / 1461;
    let e = c - (1461 * d) / 4;
    let m = (5 * e + 2) / 153;

    let day = (e - (153 * m + 2) / 5 + 1) as u32;
    let month = (m + 3 - 12 * (m / 10)) as u32;
    let year = (100 * b + d - 4800 + m / 10) as i32;

    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- serial_to_iso_date tests (1900 epoch) --

    #[test]
    fn serial_1_is_jan_1_1900() {
        assert_eq!(serial_to_iso_date(1.0, false), "1900-01-01");
    }

    #[test]
    fn serial_2_is_jan_2_1900() {
        assert_eq!(serial_to_iso_date(2.0, false), "1900-01-02");
    }

    #[test]
    fn serial_59_is_feb_28_1900() {
        assert_eq!(serial_to_iso_date(59.0, false), "1900-02-28");
    }

    #[test]
    fn serial_60_is_lotus_bug_feb_29_1900() {
        // This date doesn't exist, but Excel/Lotus say it does.
        assert_eq!(serial_to_iso_date(60.0, false), "1900-02-29");
    }

    #[test]
    fn serial_61_is_mar_1_1900() {
        assert_eq!(serial_to_iso_date(61.0, false), "1900-03-01");
    }

    #[test]
    fn serial_43831_is_jan_1_2020() {
        assert_eq!(serial_to_iso_date(43831.0, false), "2020-01-01");
    }

    #[test]
    fn serial_44197_is_jan_1_2021() {
        assert_eq!(serial_to_iso_date(44197.0, false), "2021-01-01");
    }

    #[test]
    fn serial_with_time() {
        // 43831.5 = Jan 1, 2020 at noon
        assert_eq!(serial_to_iso_date(43831.5, false), "2020-01-01T12:00:00");
    }

    #[test]
    fn serial_negative_returns_raw() {
        assert_eq!(serial_to_iso_date(-1.0, false), "-1");
    }

    // -- 1904 epoch tests --

    #[test]
    fn serial_0_1904_is_jan_1_1904() {
        assert_eq!(serial_to_iso_date(0.0, true), "1904-01-01");
    }

    #[test]
    fn serial_1_1904_is_jan_2_1904() {
        assert_eq!(serial_to_iso_date(1.0, true), "1904-01-02");
    }

    #[test]
    fn serial_time_clamped_to_23_59_59() {
        // A serial where the fractional part rounds to exactly 86400 seconds.
        // time_frac = 0.999999999... should clamp to 23:59:59, not 24:00:00.
        assert_eq!(
            serial_to_iso_date(43831.9999999, false),
            "2020-01-01T23:59:59"
        );
    }

    // -- format_number_string tests --

    #[test]
    fn format_integer_string() {
        assert_eq!(format_number_string("42"), "42");
    }

    #[test]
    fn format_decimal_string() {
        assert_eq!(format_number_string("3.14"), "3.14");
    }

    #[test]
    fn format_trailing_zeros_stripped() {
        assert_eq!(format_number_string("42.0"), "42");
    }

    // -- format_cell_value tests --

    #[test]
    fn format_with_no_style() {
        let ss = StyleSheet::default();
        assert_eq!(format_cell_value("42", &ss, None, false), "42");
    }

    #[test]
    fn format_percentage() {
        // Build a stylesheet with numFmtId 9 (0%) at index 0
        let xml =
            br#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <cellXfs count="1">
        <xf numFmtId="9"/>
    </cellXfs>
</styleSheet>"#;
        let diag = std::sync::Arc::new(udoc_core::diagnostics::NullDiagnostics);
        let ss = crate::styles::parse_styles(xml, &(diag as _)).unwrap();

        assert_eq!(format_cell_value("0.75", &ss, Some(0), false), "75%");
    }

    // -- Julian Day Number tests --

    #[test]
    fn jdn_epoch() {
        // Julian Day 2451545 = January 1, 2000 (J2000.0 epoch)
        let (y, m, d) = jdn_to_gregorian(2451545);
        assert_eq!((y, m, d), (2000, 1, 1));
    }

    #[test]
    fn jdn_known_date() {
        // Julian Day 2459581 = January 1, 2022
        let (y, m, d) = jdn_to_gregorian(2459581);
        assert_eq!((y, m, d), (2022, 1, 1));
    }
}

//! Number format evaluation for XLS/BIFF8 cells.
//!
//! Converts numeric cell values to display strings using the XF -> FORMAT
//! lookup chain. Date-formatted cells are converted from Excel serial numbers
//! to ISO 8601 date strings.

use std::collections::HashMap;

// -- Built-in date format IDs -----------------------------------------------

/// Built-in format IDs that are always date/time formats.
///
/// From the BIFF8 spec, section 2.5.165 (IFmt).
const BUILTIN_DATE_FORMATS: &[u16] = &[14, 15, 16, 17, 18, 19, 20, 21, 22, 45, 46, 47];

// -- is_date_format ---------------------------------------------------------

/// Return true if the given format index represents a date/time format.
///
/// Built-in date format IDs (14-22, 45-47) are always dates. For custom
/// formats, we scan the format string for date/time tokens: y, m, d, h, s
/// (case-insensitive). Escaped characters and quoted sections are skipped.
/// Only the first semicolon-separated section is examined, because subsequent
/// sections handle negative/zero/text values and may contain unrelated tokens.
///
/// The `m` token is ambiguous between "month" and "minutes". We treat a
/// format as a date format when `m` appears alongside at least one of y, d,
/// h, or s.
pub fn is_date_format(ifmt: u16, custom_formats: &HashMap<u16, String>) -> bool {
    // Fast path: well-known built-in date IDs.
    if BUILTIN_DATE_FORMATS.contains(&ifmt) {
        return true;
    }

    // Look up a format string: either from the custom map or (for the few
    // built-in non-date formats) there is nothing to scan.
    let fmt_str = match custom_formats.get(&ifmt) {
        Some(s) => s.as_str(),
        None => return false,
    };

    scan_format_string_for_date_tokens(fmt_str)
}

/// Scan a format string for date tokens, respecting escape and quote rules.
///
/// Only the first ';'-separated section is examined.
fn scan_format_string_for_date_tokens(fmt: &str) -> bool {
    let chars: Vec<char> = fmt.chars().collect();
    let len = chars.len();
    let mut i = 0;

    let mut has_y = false;
    let mut has_d = false;
    let mut has_hs = false; // h or s

    while i < len {
        let c = chars[i];

        match c {
            // Section separator: stop after the first section.
            ';' => break,

            // Backslash escape: skip the next character.
            '\\' => {
                i += 2;
                continue;
            }

            // Quoted literal section: skip to the closing '"'.
            '"' => {
                i += 1;
                while i < len && chars[i] != '"' {
                    i += 1;
                }
                // skip closing '"'
                i += 1;
                continue;
            }

            // Bracket section [COLOR.] or [condition]: skip entirely.
            '[' => {
                i += 1;
                while i < len && chars[i] != ']' {
                    i += 1;
                }
                i += 1; // skip ']'
                continue;
            }

            // Underscore: skip next character (padding width specifier).
            '_' => {
                i += 2;
                continue;
            }

            // Asterisk: skip next character (fill character specifier).
            '*' => {
                i += 2;
                continue;
            }

            _ => {
                let lc = c.to_ascii_lowercase();
                match lc {
                    'y' => has_y = true,
                    'd' => has_d = true,
                    // 'm' alone is ambiguous (month vs minutes); it is only a
                    // date token when y, d, h, or s also appear. Since those
                    // are all handled separately, we do not need to track 'm'.
                    'm' => {}
                    'h' | 's' => has_hs = true,
                    _ => {}
                }
                i += 1;
            }
        }
    }

    // 'y' or 'd' on their own are unambiguously date tokens.
    // 'm' is only a date token when combined with y, d, or h/s.
    // 'h' or 's' alone could be time-only -- still treat as date-like since
    // Excel's time values are a subset of the date serial space and we want
    // ISO output for them.
    if has_y || has_d || has_hs {
        return true;
    }
    // 'm' alone is ambiguous; treat as date only if accompanied.
    false
}

// -- serial_to_iso_date -----------------------------------------------------

/// Convert an Excel date serial number to an ISO 8601 string.
///
/// Returns `None` if `serial` is negative or greater than 2958465
/// (which corresponds to 9999-12-31).
///
/// # 1900 epoch (date_1904 = false)
///
/// Excel serial 1 = 1900-01-01. Excel incorrectly treats 1900 as a leap year
/// (the "Lotus bug"), so serial 60 = 1900-02-29. To compensate, for serials
/// >= 61 we subtract 1 from the day count before computing the Gregorian date.
///
/// # 1904 epoch (date_1904 = true)
///
/// Excel serial 0 = 1904-01-01. No Lotus bug.
///
/// # Fractional part
///
/// If the serial has a non-zero fractional component, the time of day is
/// appended as HH:MM:SS.
pub fn serial_to_iso_date(serial: f64, date_1904: bool) -> Option<String> {
    if !(0.0..=2_958_465.0).contains(&serial) {
        return None;
    }

    let day_serial = serial.floor() as u32;
    let frac = serial - serial.floor();

    let (year, month, day) = serial_to_ymd(day_serial, date_1904)?;

    if frac < 1e-9 {
        Some(format!("{year:04}-{month:02}-{day:02}"))
    } else {
        let total_secs = (frac * 86_400.0).round() as u32;
        let hh = total_secs / 3_600;
        let mm = (total_secs % 3_600) / 60;
        let ss = total_secs % 60;
        Some(format!(
            "{year:04}-{month:02}-{day:02} {hh:02}:{mm:02}:{ss:02}"
        ))
    }
}

/// Convert a day serial to (year, month, day).
fn serial_to_ymd(serial: u32, date_1904: bool) -> Option<(i32, u32, u32)> {
    // Number of days since the Gregorian epoch (0001-01-01) for each epoch.
    // We work in "days since the proleptic Gregorian calendar day 0".
    //
    // The reference point we use internally is the Julian Day Number (JDN)
    // of the Unix epoch is irrelevant; instead we convert Excel serial -> civil
    // date directly.

    if date_1904 {
        // Serial 0 = 1904-01-01
        // Compute absolute day number: days since some epoch + serial.
        let abs = serial_to_absolute_day_1904(serial);
        absolute_day_to_ymd(abs)
    } else {
        // Serial 1 = 1900-01-01 (not serial 0)
        // Serial 60 = 1900-02-29 (phantom leap day, Lotus bug)
        // Serial 61 = 1900-03-01
        if serial == 0 {
            // Serial 0 is sometimes used as a null/empty date in Excel.
            // Return None to indicate invalid.
            return None;
        }
        if serial == 60 {
            // The phantom Lotus Feb 29, 1900.
            return Some((1900, 2, 29));
        }
        // For serial >= 61, subtract 1 to skip the phantom day.
        let adjusted = if serial >= 61 { serial - 1 } else { serial };
        // Serial 1 -> adjusted 1 -> 1900-01-01
        let abs = serial_to_absolute_day_1900(adjusted);
        absolute_day_to_ymd(abs)
    }
}

/// Convert a 1900-epoch serial (after Lotus adjustment) to an absolute day
/// count where day 1 = 1900-01-01.
///
/// We use the algorithm: count days from 1900-01-01 = JDN 2415021.
/// absolute day = (JDN of 1900-01-01 - 1) + serial
/// Then pass to a standard JDN->Gregorian converter.
fn serial_to_absolute_day_1900(serial: u32) -> u32 {
    // JDN of 1900-01-01 = 2415021.
    // We define absolute_day such that absolute_day 1 = JDN 2415021.
    // absolute_day = serial (serial 1 = absolute 1 = 1900-01-01).
    serial
}

/// Convert a 1904-epoch serial to an absolute day count relative to 1900-01-01.
///
/// Serial 0 = 1904-01-01. 1904-01-01 is 1461 days after 1900-01-01 (4 years,
/// 1 leap year 1904 counts here -- actually 1900-01-01 to 1904-01-01 is
/// 1461 days: 1900(365)+1901(365)+1902(365)+1903(365) -- wait, 1904 is the
/// leap year, not 1900. Let's count properly:
/// 1900: 365, 1901: 365, 1902: 365, 1903: 365 = 1460 days from 1900-01-01
/// to 1904-01-01. So serial 0 (1904 epoch) = absolute day 1461 (1900 epoch
/// counting from 1 for 1900-01-01, i.e. 1 + 1460 = 1461).
fn serial_to_absolute_day_1904(serial: u32) -> u32 {
    // 1904-01-01 in our 1900-epoch absolute scale = day 1461
    // (1900-01-01 = day 1, plus 1460 days).
    serial + 1461
}

/// Convert an absolute day number (day 1 = 1900-01-01) to (year, month, day).
///
/// Uses the standard algorithm via Julian Day Number.
fn absolute_day_to_ymd(abs: u32) -> Option<(i32, u32, u32)> {
    if abs == 0 {
        return None;
    }
    // Convert absolute day (1-based from 1900-01-01) to JDN.
    // JDN of 1900-01-01 = 2415021.
    let jdn = abs as i64 + 2415020; // abs=1 -> jdn=2415021

    // Standard JDN to Gregorian algorithm.
    let a = jdn + 32044;
    let b = (4 * a + 3) / 146097;
    let c = a - (146097 * b) / 4;
    let d = (4 * c + 3) / 1461;
    let e = c - (1461 * d) / 4;
    let m = (5 * e + 2) / 153;

    let day = e - (153 * m + 2) / 5 + 1;
    let month = m + 3 - 12 * (m / 10);
    let year = 100 * b + d - 4800 + m / 10;

    Some((year as i32, month as u32, day as u32))
}

// -- decode_rk --------------------------------------------------------------

/// Decode a BIFF8 RK value to f64.
///
/// Layout of the 32-bit RK field:
/// - Bit 0 (`fX100`): if set, divide the final result by 100.
/// - Bit 1 (`fInt`): if set, treat bits 2-31 as a 30-bit signed integer.
///   If clear, treat bits 2-31 as the high 30 bits of an IEEE 754 f64.
/// - Bits 2-31: the numeric payload.
pub(crate) fn decode_rk(rk: u32) -> f64 {
    let f_x100 = (rk & 0x01) != 0;
    let f_int = (rk & 0x02) != 0;

    let value = if f_int {
        // Arithmetic right shift to sign-extend the 30-bit integer.
        // Cast to i32 first so the shift propagates the sign bit.
        let signed = (rk as i32) >> 2;
        signed as f64
    } else {
        // The high 30 bits of an f64 mantissa/exponent. Place them in bits
        // 62-32 of a u64 (the top 30 bits of the 64-bit float), with the
        // low 32 bits set to zero.
        let bits: u64 = ((rk & 0xFFFF_FFFC) as u64) << 32;
        f64::from_bits(bits)
    };

    if f_x100 {
        value / 100.0
    } else {
        value
    }
}

// -- format_cell_value ------------------------------------------------------

/// Format a numeric cell value as a display string.
///
/// If the format index is a date format and the value is in a valid date
/// range, returns an ISO 8601 date string. Otherwise, formats the value as
/// a number, trimming trailing zeros after the decimal point.
pub fn format_cell_value(
    value: f64,
    ifmt: u16,
    custom_formats: &HashMap<u16, String>,
    date_1904: bool,
) -> String {
    if is_date_format(ifmt, custom_formats) {
        if let Some(date_str) = serial_to_iso_date(value, date_1904) {
            return date_str;
        }
    }
    format_number(value)
}

/// Format a floating-point number as a clean string.
///
/// Uses standard Display formatting, then strips trailing zeros and a
/// trailing decimal point for readability.
fn format_number(value: f64) -> String {
    // Use Rust's default Display for f64 which avoids scientific notation for
    // most values and is already fairly clean.
    let s = value.to_string();

    // If the string contains a decimal point, trim trailing zeros.
    if s.contains('.') {
        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
        trimmed.to_string()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_formats() -> HashMap<u16, String> {
        HashMap::new()
    }

    fn custom(pairs: &[(u16, &str)]) -> HashMap<u16, String> {
        pairs.iter().map(|(k, v)| (*k, v.to_string())).collect()
    }

    // -- is_date_format tests -----------------------------------------------

    #[test]
    fn test_is_date_format_builtin_14() {
        assert!(is_date_format(14, &empty_formats()));
    }

    #[test]
    fn test_is_date_format_builtin_0_is_false() {
        assert!(!is_date_format(0, &empty_formats()));
    }

    #[test]
    fn test_is_date_format_custom_yyyy_mm_dd() {
        let fmts = custom(&[(164, "yyyy-mm-dd")]);
        assert!(is_date_format(164, &fmts));
    }

    #[test]
    fn test_is_date_format_custom_number_is_false() {
        let fmts = custom(&[(165, "#,##0.00")]);
        assert!(!is_date_format(165, &fmts));
    }

    #[test]
    fn test_is_date_format_custom_escaped_chars_is_false() {
        // "\\d\\a\\t\\e" -- every letter is escaped, no bare date tokens.
        let fmts = custom(&[(166, "\\d\\a\\t\\e")]);
        assert!(!is_date_format(166, &fmts));
    }

    #[test]
    fn test_is_date_format_quoted_section_is_false() {
        // All tokens are inside a quoted literal.
        let fmts = custom(&[(167, "\"yyyy-mm-dd\"")]);
        assert!(!is_date_format(167, &fmts));
    }

    #[test]
    fn test_is_date_format_second_section_ignored() {
        // Date tokens only appear after the ';' -- should be false.
        let fmts = custom(&[(168, "#,##0;yyyy-mm-dd")]);
        assert!(!is_date_format(168, &fmts));
    }

    #[test]
    fn test_is_date_format_builtin_all_date_ids() {
        let date_ids = [14u16, 15, 16, 17, 18, 19, 20, 21, 22, 45, 46, 47];
        for id in date_ids {
            assert!(
                is_date_format(id, &empty_formats()),
                "ID {id} should be a date format"
            );
        }
    }

    #[test]
    fn test_is_date_format_m_alone_is_false() {
        // 'm' alone is ambiguous (minutes vs month) -- treat as non-date.
        let fmts = custom(&[(169, "mm")]);
        assert!(!is_date_format(169, &fmts));
    }

    #[test]
    fn test_is_date_format_m_with_h_is_true() {
        // 'h' and 'm' together -- time format, treat as date-like.
        let fmts = custom(&[(170, "hh:mm:ss")]);
        assert!(is_date_format(170, &fmts));
    }

    // -- serial_to_iso_date tests -------------------------------------------

    #[test]
    fn test_serial_1_1900_epoch() {
        assert_eq!(
            serial_to_iso_date(1.0, false),
            Some("1900-01-01".to_string())
        );
    }

    #[test]
    fn test_serial_60_lotus_bug() {
        // Serial 60 = 1900-02-29 (the phantom Lotus date).
        assert_eq!(
            serial_to_iso_date(60.0, false),
            Some("1900-02-29".to_string())
        );
    }

    #[test]
    fn test_serial_61_1900_epoch() {
        // Serial 61 = 1900-03-01 (day after the phantom Feb 29).
        assert_eq!(
            serial_to_iso_date(61.0, false),
            Some("1900-03-01".to_string())
        );
    }

    #[test]
    fn test_serial_0_1904_epoch() {
        assert_eq!(
            serial_to_iso_date(0.0, true),
            Some("1904-01-01".to_string())
        );
    }

    #[test]
    fn test_serial_with_fractional_part() {
        // Serial 1.5 = 1900-01-01 12:00:00
        let result = serial_to_iso_date(1.5, false);
        assert!(result.is_some());
        let s = result.unwrap();
        assert!(
            s.starts_with("1900-01-01 "),
            "expected date prefix, got: {s}"
        );
        assert!(s.contains("12:00:00"), "expected 12:00:00 time, got: {s}");
    }

    #[test]
    fn test_serial_negative_returns_none() {
        assert_eq!(serial_to_iso_date(-1.0, false), None);
    }

    #[test]
    fn test_serial_too_large_returns_none() {
        assert_eq!(serial_to_iso_date(2_958_466.0, false), None);
    }

    #[test]
    fn test_serial_known_date_2023_01_15() {
        // 2023-01-15: days from 1900-01-01.
        // Excel serial for 2023-01-15 = 44941.
        assert_eq!(
            serial_to_iso_date(44941.0, false),
            Some("2023-01-15".to_string())
        );
    }

    #[test]
    fn test_serial_0_1900_epoch_returns_none() {
        // Serial 0 in 1900 mode is not a valid date.
        assert_eq!(serial_to_iso_date(0.0, false), None);
    }

    // -- format_cell_value tests --------------------------------------------

    #[test]
    fn test_format_cell_value_date_format() {
        let fmts = custom(&[(164, "yyyy-mm-dd")]);
        let result = format_cell_value(44941.0, 164, &fmts, false);
        assert_eq!(result, "2023-01-15");
    }

    #[test]
    fn test_format_cell_value_number_format() {
        let fmts = custom(&[(165, "#,##0.00")]);
        let result = format_cell_value(42.5, 165, &fmts, false);
        assert_eq!(result, "42.5");
    }

    #[test]
    fn test_format_cell_value_integer_no_trailing_dot() {
        let result = format_cell_value(100.0, 0, &empty_formats(), false);
        assert_eq!(result, "100");
    }

    #[test]
    fn test_format_cell_value_builtin_date_14() {
        // Built-in date format ID 14 should produce ISO date output.
        let result = format_cell_value(44941.0, 14, &empty_formats(), false);
        assert_eq!(result, "2023-01-15");
    }

    #[test]
    fn test_format_cell_value_negative_date_serial_falls_back_to_number() {
        // Negative serial is out of date range -- fall back to number formatting.
        let result = format_cell_value(-5.0, 14, &empty_formats(), false);
        assert_eq!(result, "-5");
    }
}

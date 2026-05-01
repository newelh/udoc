//! Cell reference parsing for XLSX.
//!
//! Converts Excel-style cell references like "AA100" to (row, col) pairs.
//! Column letters are 1-based in Excel (A=0 internally, B=1, ..., Z=25, AA=26).
//! Row numbers are 1-based in XML (parsed to 0-based internally).

use crate::error::{Error, Result};

/// Maximum column index we'll accept (XFD = 16383, Excel's limit).
const MAX_COL: usize = 16_383;

/// Maximum row index we'll accept (1,048,576 rows in Excel, 0-based = 1,048,575).
const MAX_ROW: usize = 1_048_575;

/// Parse a cell reference string like "A1", "AA100", "XFD1048576" into
/// a 0-based (row, col) pair.
///
/// Returns an error if the reference is malformed or exceeds Excel limits.
pub(crate) fn parse_cell_ref(cell_ref: &str) -> Result<(usize, usize)> {
    let bytes = cell_ref.as_bytes();
    if bytes.is_empty() {
        return Err(Error::new("empty cell reference"));
    }

    // Split into letter prefix and digit suffix.
    let split = bytes
        .iter()
        .position(|b| b.is_ascii_digit())
        .ok_or_else(|| Error::new(format!("cell reference has no row number: {cell_ref}")))?;

    if split == 0 {
        return Err(Error::new(format!(
            "cell reference has no column letters: {cell_ref}"
        )));
    }

    let col_part = &bytes[..split];
    let row_part = &bytes[split..];

    // Validate all column chars are ASCII uppercase letters.
    if !col_part.iter().all(|b| b.is_ascii_uppercase()) {
        // Try case-insensitive parse.
        if !col_part.iter().all(|b| b.is_ascii_alphabetic()) {
            return Err(Error::new(format!(
                "cell reference has invalid column letters: {cell_ref}"
            )));
        }
    }

    // Validate all row chars are digits.
    if !row_part.iter().all(|b| b.is_ascii_digit()) {
        return Err(Error::new(format!(
            "cell reference has non-digit row chars: {cell_ref}"
        )));
    }

    // Parse column: A=0, B=1, ..., Z=25, AA=26, AB=27, ...
    let col = col_letters_to_index(col_part);
    if col > MAX_COL {
        return Err(Error::new(format!(
            "column index {col} exceeds maximum {MAX_COL}: {cell_ref}"
        )));
    }

    // Parse row (1-based in XML -> 0-based internally).
    let row_str = std::str::from_utf8(row_part)
        .map_err(|_| Error::new(format!("invalid UTF-8 in row number: {cell_ref}")))?;
    let row_1based: usize = row_str
        .parse()
        .map_err(|_| Error::new(format!("invalid row number: {cell_ref}")))?;
    if row_1based == 0 {
        return Err(Error::new(format!("row number must be >= 1: {cell_ref}")));
    }
    let row = row_1based - 1;
    if row > MAX_ROW {
        return Err(Error::new(format!(
            "row index {row} exceeds maximum {MAX_ROW}: {cell_ref}"
        )));
    }

    Ok((row, col))
}

/// Convert column letters to 0-based index.
/// A=0, B=1, ..., Z=25, AA=26, AB=27, ...
fn col_letters_to_index(letters: &[u8]) -> usize {
    let mut col: usize = 0;
    for &b in letters {
        let ch = b.to_ascii_uppercase();
        col = col
            .saturating_mul(26)
            .saturating_add((ch - b'A') as usize + 1);
    }
    // Convert from 1-based (A=1) to 0-based (A=0).
    col.saturating_sub(1)
}

/// Convert a 0-based column index to Excel column letters.
/// 0=A, 1=B, ..., 25=Z, 26=AA, 27=AB, ...
#[cfg(test)]
fn col_index_to_letters(mut col: usize) -> String {
    let mut letters = Vec::new();
    col += 1; // 0-based to 1-based
    while col > 0 {
        col -= 1;
        letters.push(b'A' + (col % 26) as u8);
        col /= 26;
    }
    letters.reverse();
    // SAFETY: letters only contains bytes from (b'A' + offset), always valid ASCII/UTF-8.
    String::from_utf8(letters).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_a1() {
        assert_eq!(parse_cell_ref("A1").unwrap(), (0, 0));
    }

    #[test]
    fn parse_b2() {
        assert_eq!(parse_cell_ref("B2").unwrap(), (1, 1));
    }

    #[test]
    fn parse_z1() {
        assert_eq!(parse_cell_ref("Z1").unwrap(), (0, 25));
    }

    #[test]
    fn parse_aa1() {
        assert_eq!(parse_cell_ref("AA1").unwrap(), (0, 26));
    }

    #[test]
    fn parse_az1() {
        assert_eq!(parse_cell_ref("AZ1").unwrap(), (0, 51));
    }

    #[test]
    fn parse_ba1() {
        assert_eq!(parse_cell_ref("BA1").unwrap(), (0, 52));
    }

    #[test]
    fn parse_xfd1() {
        // XFD = 16383 (Excel max column)
        assert_eq!(parse_cell_ref("XFD1").unwrap(), (0, 16383));
    }

    #[test]
    fn parse_large_row() {
        assert_eq!(parse_cell_ref("A1048576").unwrap(), (1_048_575, 0));
    }

    #[test]
    fn parse_empty_is_error() {
        assert!(parse_cell_ref("").is_err());
    }

    #[test]
    fn parse_no_row_is_error() {
        assert!(parse_cell_ref("AB").is_err());
    }

    #[test]
    fn parse_no_col_is_error() {
        assert!(parse_cell_ref("123").is_err());
    }

    #[test]
    fn parse_row_zero_is_error() {
        assert!(parse_cell_ref("A0").is_err());
    }

    #[test]
    fn col_round_trip() {
        for i in 0..=100 {
            let letters = col_index_to_letters(i);
            let (_, col) = parse_cell_ref(&format!("{letters}1")).unwrap();
            assert_eq!(col, i, "round-trip failed for col {i} = {letters}");
        }
    }

    #[test]
    fn parse_lowercase_refs() {
        assert_eq!(parse_cell_ref("a1").unwrap(), (0, 0));
        assert_eq!(parse_cell_ref("aa1").unwrap(), (0, 26));
        assert_eq!(parse_cell_ref("Az1").unwrap(), (0, 51));
    }

    #[test]
    fn col_round_trip_boundaries() {
        // Z=25, AA=26, AZ=51, BA=52
        assert_eq!(col_index_to_letters(0), "A");
        assert_eq!(col_index_to_letters(25), "Z");
        assert_eq!(col_index_to_letters(26), "AA");
        assert_eq!(col_index_to_letters(51), "AZ");
        assert_eq!(col_index_to_letters(52), "BA");
        assert_eq!(col_index_to_letters(16383), "XFD");
    }
}

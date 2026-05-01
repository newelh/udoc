//! sfnt table-directory helpers.
//!
//! TrueType and OpenType/CFF (OTF) both use the same sfnt container format:
//! a 12-byte header followed by a table directory of 16-byte records. This
//! module exposes a minimal bounds-checked walker so callers can peel a
//! specific table (e.g. `CFF `) out of a raw sfnt blob without depending on
//! a full TrueType or CFF parser.
//!
//! Used by:
//! - `udoc-render::font_cache` to extract the `CFF ` table from bundled
//!   LatinModern .otf assets so `CffFont::from_bytes` can parse them (the
//!   PDF FontFile3 path already receives raw CFF and skips the wrapper).
//! - Future consumers that need to read auxiliary tables from raw sfnt
//!   data without parsing the full font program.
//!
//! See issue #206 for the hoist rationale.

use crate::error::{Error, Result};

/// sfnt magic for TrueType fonts (`\0\1\0\0`). Some old fonts use `"true"`
/// instead; both layouts share the same table directory.
pub const SFNT_MAGIC_TRUETYPE: &[u8; 4] = &[0x00, 0x01, 0x00, 0x00];
/// sfnt magic for TrueType fonts using the legacy `"true"` tag.
pub const SFNT_MAGIC_TRUE: &[u8; 4] = b"true";
/// sfnt magic for OpenType/CFF fonts (`"OTTO"`).
pub const SFNT_MAGIC_OTTO: &[u8; 4] = b"OTTO";

/// Offset and length of a table within an sfnt font.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableEntry {
    /// Byte offset of the table's contents from the start of the font.
    pub offset: usize,
    /// Length of the table's contents in bytes.
    pub length: usize,
}

/// Locate a table by 4-byte tag within an sfnt table directory.
///
/// Walks the header at the front of `data` and returns the directory record
/// whose tag matches `tag`. Bounds-checked: returns `None` if the header or
/// directory is truncated rather than panicking.
///
/// Does not inspect the sfnt magic. Callers that need to reject mixed
/// TrueType/OpenType data should check the first 4 bytes of `data` against
/// one of the `SFNT_MAGIC_*` constants before calling this.
pub fn find_table(data: &[u8], tag: &[u8; 4]) -> Option<TableEntry> {
    if data.len() < 12 {
        return None;
    }
    let num_tables = u16::from_be_bytes([data[4], data[5]]) as usize;
    let dir_end = 12usize.checked_add(num_tables.checked_mul(16)?)?;
    if data.len() < dir_end {
        return None;
    }
    for i in 0..num_tables {
        let rec = 12 + i * 16;
        if &data[rec..rec + 4] == tag {
            let offset =
                u32::from_be_bytes([data[rec + 8], data[rec + 9], data[rec + 10], data[rec + 11]])
                    as usize;
            let length = u32::from_be_bytes([
                data[rec + 12],
                data[rec + 13],
                data[rec + 14],
                data[rec + 15],
            ]) as usize;
            return Some(TableEntry { offset, length });
        }
    }
    None
}

/// Extract a table's raw bytes from an sfnt font, bounds-checked.
///
/// Returns a slice pointing into `data`. Errors out if the table is missing,
/// its offset+length overflows, or the range would extend past the end of
/// `data`.
///
/// Prefer this over calling [`find_table`] plus manual slicing: it folds the
/// bounds checks into the `Result` so callers can `?`-propagate.
pub fn table_data<'a>(data: &'a [u8], tag: &[u8; 4]) -> Result<&'a [u8]> {
    let entry = find_table(data, tag).ok_or_else(|| {
        Error::new(format!(
            "sfnt table '{}' not found",
            String::from_utf8_lossy(tag)
        ))
    })?;
    let end = entry
        .offset
        .checked_add(entry.length)
        .ok_or_else(|| Error::new("sfnt table offset+length overflow"))?;
    if end > data.len() {
        return Err(Error::new(format!(
            "sfnt table '{}' extends past end of data ({} + {} > {})",
            String::from_utf8_lossy(tag),
            entry.offset,
            entry.length,
            data.len()
        )));
    }
    Ok(&data[entry.offset..end])
}

/// Extract the `CFF ` table from an OpenType/CFF container.
///
/// Convenience wrapper over [`table_data`] that also verifies the sfnt
/// magic matches `OTTO`. Use when the caller specifically wants to route
/// a bundled `.otf` asset (or a FontFile3 blob wrapped in OpenType) into a
/// CFF parser.
pub fn extract_cff_table(data: &[u8]) -> Result<&[u8]> {
    if data.len() < 12 {
        return Err(Error::new("OTF data too short for sfnt header"));
    }
    if &data[0..4] != SFNT_MAGIC_OTTO {
        return Err(Error::new("expected OpenType/CFF container magic (OTTO)"));
    }
    table_data(data, b"CFF ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_table_on_truncated_header_returns_none() {
        assert!(find_table(&[0u8; 4], b"CFF ").is_none());
        assert!(find_table(&[0u8; 11], b"CFF ").is_none());
    }

    #[test]
    fn find_table_rejects_truncated_directory() {
        // Header claims 10 tables but only room for 1.
        let mut data = vec![b'O', b'T', b'T', b'O', 0x00, 0x0A];
        data.extend_from_slice(&[0u8; 6]); // rest of header
        data.extend_from_slice(&[0u8; 16]); // one record only
        assert!(find_table(&data, b"CFF ").is_none());
    }

    #[test]
    fn extract_cff_table_rejects_non_otto_magic() {
        let mut data = vec![0x00, 0x01, 0x00, 0x00]; // TrueType magic
        data.extend_from_slice(&[0u8; 8]); // rest of header
        let err = extract_cff_table(&data).unwrap_err();
        assert!(err.to_string().contains("OTTO"));
    }

    #[test]
    fn extract_cff_table_from_real_otf() {
        // Round-trip against the bundled LatinModernMath-Subset.otf asset.
        // Parse it as a sfnt and pull the CFF table; sanity-check that the
        // returned slice looks like a CFF header (first byte is major
        // version 1 per the CFF spec).
        let data = include_bytes!("../assets/LatinModernMath-Subset.otf");
        let cff = extract_cff_table(data).expect("LM Math OTF should yield CFF table");
        assert!(!cff.is_empty());
        assert_eq!(cff[0], 0x01, "CFF major version should be 1");
    }
}

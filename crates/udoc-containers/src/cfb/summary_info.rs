//! OLE SummaryInformation property set parser.
//!
//! Parses the "\x05SummaryInformation" stream found in CFB containers
//! (DOC, PPT, XLS) to extract document metadata: title, subject, author.
//!
//! Format reference: [MS-OLEPS] OLE Property Set Data Structures.

use udoc_core::codepage::encoding_for_codepage;

use crate::error::{Error, Result, ResultExt};

/// Metadata extracted from the SummaryInformation property set.
#[derive(Debug, Clone, Default)]
pub struct SummaryInfo {
    pub title: Option<String>,
    pub subject: Option<String>,
    pub author: Option<String>,
}

/// The stream name for SummaryInformation in a CFB container.
/// The 0x05 prefix is part of the OLE spec, not a display artifact.
pub const SUMMARY_INFO_STREAM_NAME: &str = "\x05SummaryInformation";

/// Property IDs from [MS-OLEPS] for the SummaryInformation set.
const PIDSI_TITLE: u32 = 0x0002;
const PIDSI_SUBJECT: u32 = 0x0003;
const PIDSI_AUTHOR: u32 = 0x0004;

/// VT_LPSTR type tag per [MS-OLEPS].
const VT_LPSTR: u32 = 0x001E;

/// Read a u16 LE from `data` at `offset`, or None if out of bounds.
fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    data.get(offset..offset + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
}

/// Read a u32 LE from `data` at `offset`, or None if out of bounds.
fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    data.get(offset..offset + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Parse a VT_LPSTR property value at the given offset within the property
/// set data. Returns None if the type tag is not VT_LPSTR or the data is
/// truncated.
fn parse_vt_lpstr(data: &[u8], offset: usize) -> Option<String> {
    let type_tag = read_u32(data, offset)?;
    if type_tag != VT_LPSTR {
        return None;
    }
    let byte_count = read_u32(data, offset + 4)? as usize;
    if byte_count == 0 {
        return Some(String::new());
    }
    let str_start = offset + 8;
    let str_end = str_start.checked_add(byte_count)?;
    if str_end > data.len() {
        return None;
    }
    let raw = &data[str_start..str_end];
    // Strip null terminator(s) if present.
    let trimmed = match raw.iter().position(|&b| b == 0) {
        Some(pos) => &raw[..pos],
        None => raw,
    };
    // SummaryInformation strings are typically Windows-1252. A full
    // implementation would read the codepage property (PIDSI 0x0001) to
    // pick the right decoder, but CP1252 covers the vast majority of
    // real-world DOC metadata.
    let cp1252 = encoding_for_codepage(1252);
    let (decoded, _, _) = cp1252.decode(trimmed);
    Some(decoded.into_owned())
}

/// Parse the SummaryInformation property set from raw stream bytes.
///
/// Lenient: returns partial results when individual properties are malformed
/// or missing. Only returns Err for completely unparseable data (e.g.,
/// truncated header).
pub fn parse_summary_information(data: &[u8]) -> Result<SummaryInfo> {
    // Property Set Header: 28 bytes minimum.
    // u16 byte_order, u16 version, u32 os_version, 16 bytes class_id, u32 num_sets
    if data.len() < 28 {
        return Err(Error::cfb(format!(
            "SummaryInformation stream too short ({} bytes, need at least 28)",
            data.len()
        )));
    }

    let byte_order = read_u16(data, 0).unwrap_or(0);
    if byte_order != 0xFFFE {
        return Err(Error::cfb(format!(
            "SummaryInformation bad byte order marker: 0x{byte_order:04X} (expected 0xFFFE)"
        )));
    }

    let num_sets = read_u32(data, 24).unwrap_or(0);
    if num_sets == 0 {
        return Ok(SummaryInfo::default());
    }

    // Property Set Entry (offset 28): 16 bytes FMTID + u32 offset = 20 bytes.
    if data.len() < 48 {
        return Err(Error::cfb(
            "SummaryInformation stream too short for property set entry",
        ));
    }

    let set_offset = read_u32(data, 44).unwrap_or(0) as usize;
    if set_offset == 0 || set_offset >= data.len() {
        return Err(Error::cfb(format!(
            "SummaryInformation property set offset {set_offset} out of range (stream len {})",
            data.len()
        )));
    }

    parse_property_set(data, set_offset).context("parsing SummaryInformation property set")
}

/// Parse a single property set at the given offset within the stream.
fn parse_property_set(data: &[u8], base: usize) -> Result<SummaryInfo> {
    // Property set header: u32 size, u32 num_properties
    if base + 8 > data.len() {
        return Err(Error::cfb("property set header truncated"));
    }

    let num_props = read_u32(data, base + 4).unwrap_or(0) as usize;

    // Cap to a sane limit to avoid huge allocations on malformed data.
    let num_props = num_props.min(1024);

    // Property ID/offset pairs start at base + 8, each is 8 bytes.
    let pairs_start = base + 8;
    let pairs_end = pairs_start + num_props * 8;
    if pairs_end > data.len() {
        // Truncated property array. Parse what we can.
        let available = (data.len().saturating_sub(pairs_start)) / 8;
        return parse_props_from_pairs(data, base, pairs_start, available);
    }

    parse_props_from_pairs(data, base, pairs_start, num_props)
}

/// Extract title/subject/author from the property ID/offset pairs.
fn parse_props_from_pairs(
    data: &[u8],
    base: usize,
    pairs_start: usize,
    count: usize,
) -> Result<SummaryInfo> {
    let mut info = SummaryInfo::default();

    for i in 0..count {
        let pair_offset = pairs_start + i * 8;
        let prop_id = match read_u32(data, pair_offset) {
            Some(v) => v,
            None => continue,
        };
        let prop_offset = match read_u32(data, pair_offset + 4) {
            Some(v) => v as usize,
            None => continue,
        };

        // Property data is at base + prop_offset.
        let abs_offset = match base.checked_add(prop_offset) {
            Some(v) if v < data.len() => v,
            _ => continue,
        };

        match prop_id {
            PIDSI_TITLE => {
                if let Some(s) = parse_vt_lpstr(data, abs_offset) {
                    if !s.is_empty() {
                        info.title = Some(s);
                    }
                }
            }
            PIDSI_SUBJECT => {
                if let Some(s) = parse_vt_lpstr(data, abs_offset) {
                    if !s.is_empty() {
                        info.subject = Some(s);
                    }
                }
            }
            PIDSI_AUTHOR => {
                if let Some(s) = parse_vt_lpstr(data, abs_offset) {
                    if !s.is_empty() {
                        info.author = Some(s);
                    }
                }
            }
            _ => {
                // Other properties (keywords, comments, etc.) -- skip for now.
            }
        }
    }

    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid SummaryInformation stream with the given properties.
    /// Each property is (id, string_value).
    fn build_summary_info(props: &[(u32, &str)]) -> Vec<u8> {
        // Property Set Header (28 bytes)
        let mut buf = Vec::new();
        buf.extend_from_slice(&0xFFFEu16.to_le_bytes()); // byte_order
        buf.extend_from_slice(&0x0000u16.to_le_bytes()); // version
        buf.extend_from_slice(&0u32.to_le_bytes()); // os_version
        buf.extend_from_slice(&[0u8; 16]); // class_id
        buf.extend_from_slice(&1u32.to_le_bytes()); // num_property_sets

        // Property Set Entry (20 bytes): FMTID + offset
        buf.extend_from_slice(&[0u8; 16]); // FMTID (don't care for parsing)
        let set_offset = 48u32; // right after this entry
        buf.extend_from_slice(&set_offset.to_le_bytes());

        // Property Set Data starts at offset 48.
        // Layout: u32 size, u32 num_properties, then (id, offset) pairs,
        // then property values.

        let num_props = props.len() as u32;

        // Compute offsets for property values.
        // Property data starts after the pairs array.
        let pairs_size = (props.len() * 8) as u32;
        let values_base = 8 + pairs_size; // relative to set start

        // Build property values and compute their offsets.
        let mut values = Vec::new();
        let mut prop_offsets: Vec<u32> = Vec::new();
        for &(_, text) in props {
            prop_offsets.push(values_base + values.len() as u32);
            // VT_LPSTR: u32 type + u32 byte_count + bytes (with null terminator)
            values.extend_from_slice(&VT_LPSTR.to_le_bytes());
            let byte_count = (text.len() + 1) as u32; // include null terminator
            values.extend_from_slice(&byte_count.to_le_bytes());
            values.extend_from_slice(text.as_bytes());
            values.push(0); // null terminator
                            // Pad to 4-byte alignment.
            while values.len() % 4 != 0 {
                values.push(0);
            }
        }

        let set_size = 8 + pairs_size as usize + values.len();

        // Write property set header.
        buf.extend_from_slice(&(set_size as u32).to_le_bytes()); // size
        buf.extend_from_slice(&num_props.to_le_bytes()); // num_properties

        // Write property ID/offset pairs.
        for (i, &(id, _)) in props.iter().enumerate() {
            buf.extend_from_slice(&id.to_le_bytes());
            buf.extend_from_slice(&prop_offsets[i].to_le_bytes());
        }

        // Write property values.
        buf.extend_from_slice(&values);

        buf
    }

    #[test]
    fn parse_title_and_author() {
        let data = build_summary_info(&[(PIDSI_TITLE, "My Document"), (PIDSI_AUTHOR, "Jane Doe")]);
        let info = parse_summary_information(&data).expect("should parse");
        assert_eq!(info.title.as_deref(), Some("My Document"));
        assert_eq!(info.author.as_deref(), Some("Jane Doe"));
        assert!(info.subject.is_none());
    }

    #[test]
    fn parse_all_three_fields() {
        let data = build_summary_info(&[
            (PIDSI_TITLE, "Title Here"),
            (PIDSI_SUBJECT, "Subject Here"),
            (PIDSI_AUTHOR, "Author Here"),
        ]);
        let info = parse_summary_information(&data).expect("should parse");
        assert_eq!(info.title.as_deref(), Some("Title Here"));
        assert_eq!(info.subject.as_deref(), Some("Subject Here"));
        assert_eq!(info.author.as_deref(), Some("Author Here"));
    }

    #[test]
    fn parse_missing_properties() {
        // Only subject, no title or author.
        let data = build_summary_info(&[(PIDSI_SUBJECT, "Just Subject")]);
        let info = parse_summary_information(&data).expect("should parse");
        assert!(info.title.is_none());
        assert_eq!(info.subject.as_deref(), Some("Just Subject"));
        assert!(info.author.is_none());
    }

    #[test]
    fn parse_empty_stream_errors() {
        let result = parse_summary_information(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn parse_truncated_header_errors() {
        // Less than 28 bytes.
        let data = vec![0xFE, 0xFF, 0x00, 0x00];
        let result = parse_summary_information(&data);
        assert!(result.is_err());
    }

    #[test]
    fn parse_bad_byte_order_errors() {
        let mut data = build_summary_info(&[(PIDSI_TITLE, "test")]);
        // Corrupt the byte order marker.
        data[0] = 0x00;
        data[1] = 0x00;
        let result = parse_summary_information(&data);
        assert!(result.is_err());
    }

    #[test]
    fn parse_no_properties_returns_empty() {
        let data = build_summary_info(&[]);
        let info = parse_summary_information(&data).expect("should parse");
        assert!(info.title.is_none());
        assert!(info.subject.is_none());
        assert!(info.author.is_none());
    }

    #[test]
    fn parse_unknown_property_ids_ignored() {
        // Property ID 0x0010 is not title/subject/author -- should be skipped.
        let data = build_summary_info(&[(0x0010, "Unknown"), (PIDSI_TITLE, "Known Title")]);
        let info = parse_summary_information(&data).expect("should parse");
        assert_eq!(info.title.as_deref(), Some("Known Title"));
        assert!(info.subject.is_none());
        assert!(info.author.is_none());
    }

    #[test]
    fn parse_empty_string_value_yields_none() {
        // Empty string properties should not populate fields.
        let data = build_summary_info(&[(PIDSI_TITLE, "")]);
        let info = parse_summary_information(&data).expect("should parse");
        assert!(
            info.title.is_none(),
            "empty title string should not set the field"
        );
    }

    #[test]
    fn stream_name_constant() {
        assert_eq!(SUMMARY_INFO_STREAM_NAME.as_bytes()[0], 0x05);
        assert!(SUMMARY_INFO_STREAM_NAME.ends_with("SummaryInformation"));
    }
}

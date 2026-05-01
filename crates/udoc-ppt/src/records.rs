//! TLV record parser for PPT binary streams.
//!
//! PPT files store data as a tree of records, each with an 8-byte header:
//! - Bytes 0-1: recVer (4 bits) + recInstance (12 bits)
//! - Bytes 2-3: recType (u16 LE)
//! - Bytes 4-7: recLen (u32 LE, length of data after header)
//!
//! Container records (recVer == 0xF) hold child records. Atom records hold
//! raw data.

use crate::error::{Error, Result};

/// Size of a PPT record header in bytes.
pub const HEADER_SIZE: usize = 8;

/// Record type constants from the MS-PPT spec.
///
/// Complete catalog of record types used in text extraction. Some constants
/// are only referenced in tests or reserved for future use (font collection,
/// table detection). Suppressing dead_code since this is a spec constant module.
#[allow(dead_code)]
pub mod rt {
    pub const DOCUMENT: u16 = 0x03E8;
    pub const DOCUMENT_ATOM: u16 = 0x03E9;
    pub const END_DOCUMENT_ATOM: u16 = 0x03EA;
    pub const SLIDE: u16 = 0x03EE;
    pub const SLIDE_ATOM: u16 = 0x03EF;
    pub const NOTES: u16 = 0x03F0;
    pub const NOTES_ATOM: u16 = 0x03F1;
    pub const SLIDE_PERSIST_ATOM: u16 = 0x03F3;
    pub const LIST: u16 = 0x07D0;
    pub const SLIDE_LIST_WITH_TEXT: u16 = 0x0FF0;
    pub const TEXT_HEADER_ATOM: u16 = 0x0F9F;
    pub const TEXT_CHARS_ATOM: u16 = 0x0FA0;
    pub const STYLE_TEXT_PROP_ATOM: u16 = 0x0FA1;
    pub const TEXT_BYTES_ATOM: u16 = 0x0FA8;
    pub const CURRENT_USER_ATOM: u16 = 0x0FF6;
    pub const USER_EDIT_ATOM: u16 = 0x0FF5;
    pub const PERSIST_DIRECTORY_ATOM: u16 = 0x1772;
    pub const FONT_COLLECTION: u16 = 0x07D5;
    pub const FONT_ENTITY_ATOM: u16 = 0x0FB7;
    pub const TEXT_SPEC_INFO_ATOM: u16 = 0x0FAA;
    pub const CSTRING_ATOM: u16 = 0x0FBA;
}

/// Parsed PPT record header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordHeader {
    /// Record version (4 bits, 0-15).
    pub rec_ver: u8,
    /// Record instance (12 bits, 0-4095).
    pub rec_instance: u16,
    /// Record type identifier.
    pub rec_type: u16,
    /// Length of the record data (not including the 8-byte header).
    pub rec_len: u32,
}

impl RecordHeader {
    /// Whether this record is a container (holds child records).
    /// Container records have rec_ver == 0xF.
    pub fn is_container(&self) -> bool {
        self.rec_ver == 0xF
    }
}

/// Read a record header from `data` at the given byte offset.
///
/// Returns an error if there aren't enough bytes for the 8-byte header.
pub fn read_record_header(data: &[u8], offset: usize) -> Result<RecordHeader> {
    let end = offset
        .checked_add(HEADER_SIZE)
        .ok_or_else(|| Error::new(format!("record header offset overflow at {offset}")))?;
    let hdr = data.get(offset..end).ok_or_else(|| {
        Error::new(format!(
            "truncated record header at offset {offset}: need {HEADER_SIZE} bytes, have {}",
            data.len().saturating_sub(offset)
        ))
    })?;

    let ver_inst = u16::from_le_bytes([hdr[0], hdr[1]]);
    let rec_ver = (ver_inst & 0x000F) as u8;
    let rec_instance = ver_inst >> 4;
    let rec_type = u16::from_le_bytes([hdr[2], hdr[3]]);
    let rec_len = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);

    Ok(RecordHeader {
        rec_ver,
        rec_instance,
        rec_type,
        rec_len,
    })
}

/// Iterator over sibling records in a byte slice.
///
/// Yields `(offset, RecordHeader)` pairs for each record encountered.
/// Stops when there aren't enough bytes for another header, or when a
/// record's data extends past the end of the slice.
pub struct RecordIter<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> RecordIter<'a> {
    /// Create an iterator over records in `data`, starting at byte 0.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }
}

impl<'a> Iterator for RecordIter<'a> {
    /// Yields `(absolute_offset, header)` for each record.
    type Item = Result<(usize, RecordHeader)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset.checked_add(HEADER_SIZE)? > self.data.len() {
            return None;
        }

        let hdr = match read_record_header(self.data, self.offset) {
            Ok(h) => h,
            Err(e) => return Some(Err(e)),
        };

        let current_offset = self.offset;

        // Advance past header + data, using checked arithmetic
        let data_end = match self
            .offset
            .checked_add(HEADER_SIZE)
            .and_then(|o| o.checked_add(hdr.rec_len as usize))
        {
            Some(end) => end,
            None => {
                return Some(Err(Error::new(format!(
                    "record length overflow at offset {current_offset}: rec_len={}",
                    hdr.rec_len
                ))));
            }
        };

        if data_end > self.data.len() {
            // Record extends past available data. Yield the header (it's
            // valid) but advance to EOF so we stop on the next call.
            self.offset = self.data.len();
        } else {
            self.offset = data_end;
        }

        Some(Ok((current_offset, hdr)))
    }
}

/// Iterate over the child records of a container record.
///
/// `data` is the full stream, `parent_offset` is where the container
/// header starts, and `parent_header` is the already-parsed header.
///
/// Returns an iterator bounded to the parent's data region.
pub fn children<'a>(
    data: &'a [u8],
    parent_offset: usize,
    parent_header: &RecordHeader,
) -> Result<RecordIter<'a>> {
    if !parent_header.is_container() {
        return Err(Error::new(format!(
            "record at offset {parent_offset} is not a container (rec_ver={})",
            parent_header.rec_ver
        )));
    }

    let data_start = parent_offset.checked_add(HEADER_SIZE).ok_or_else(|| {
        Error::new(format!(
            "container data start overflow at offset {parent_offset}"
        ))
    })?;
    let data_end = data_start
        .checked_add(parent_header.rec_len as usize)
        .ok_or_else(|| {
            Error::new(format!(
                "container data end overflow at offset {parent_offset}: rec_len={}",
                parent_header.rec_len
            ))
        })?;

    // Clamp to available data
    let actual_end = data_end.min(data.len());
    let child_slice = data.get(data_start..actual_end).ok_or_else(|| {
        Error::new(format!(
            "container data region [{data_start}..{actual_end}] out of bounds (data len={})",
            data.len()
        ))
    })?;

    Ok(RecordIter::new(child_slice))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::*;

    #[test]
    fn empty_stream_yields_no_records() {
        let iter = RecordIter::new(&[]);
        let records: Vec<_> = iter.collect();
        assert!(records.is_empty());
    }

    #[test]
    fn single_atom_record() {
        let data = build_atom(rt::TEXT_BYTES_ATOM, b"hello");
        let hdr = read_record_header(&data, 0).unwrap();
        assert_eq!(hdr.rec_type, rt::TEXT_BYTES_ATOM);
        assert_eq!(hdr.rec_ver, 0);
        assert_eq!(hdr.rec_instance, 0);
        assert_eq!(hdr.rec_len, 5);
        assert!(!hdr.is_container());
    }

    #[test]
    fn nested_container_with_children() {
        let child1 = build_atom(rt::SLIDE_ATOM, &[0x01, 0x02]);
        let child2 = build_atom(rt::NOTES_ATOM, &[0x03]);
        let mut children_data = Vec::new();
        children_data.extend_from_slice(&child1);
        children_data.extend_from_slice(&child2);
        let container = build_container(rt::SLIDE, &children_data);

        let hdr = read_record_header(&container, 0).unwrap();
        assert!(hdr.is_container());
        assert_eq!(hdr.rec_type, rt::SLIDE);

        let child_iter = children(&container, 0, &hdr).unwrap();
        let child_records: Vec<_> = child_iter
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(child_records.len(), 2);
        assert_eq!(child_records[0].1.rec_type, rt::SLIDE_ATOM);
        assert_eq!(child_records[0].1.rec_len, 2);
        assert_eq!(child_records[1].1.rec_type, rt::NOTES_ATOM);
        assert_eq!(child_records[1].1.rec_len, 1);
    }

    #[test]
    fn truncated_header_returns_error() {
        let data = [0u8; 5]; // less than 8 bytes
        let result = read_record_header(&data, 0);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("truncated"));
    }

    #[test]
    fn record_length_exceeding_data_bounds() {
        // Build a record header claiming 1000 bytes of data, but only provide 4
        let mut data = Vec::new();
        let ver_inst: u16 = 0; // ver=0, instance=0
        data.extend_from_slice(&ver_inst.to_le_bytes());
        data.extend_from_slice(&rt::TEXT_BYTES_ATOM.to_le_bytes());
        data.extend_from_slice(&1000u32.to_le_bytes());
        data.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]); // only 4 bytes of "data"

        // Header parses fine (it reports what the file claims)
        let hdr = read_record_header(&data, 0).unwrap();
        assert_eq!(hdr.rec_len, 1000);

        // Iterator yields the record but then stops
        let records: Vec<_> = RecordIter::new(&data)
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn zero_length_atom() {
        let data = build_atom(rt::END_DOCUMENT_ATOM, &[]);
        let hdr = read_record_header(&data, 0).unwrap();
        assert_eq!(hdr.rec_len, 0);
        assert_eq!(hdr.rec_type, rt::END_DOCUMENT_ATOM);

        let records: Vec<_> = RecordIter::new(&data)
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn rec_instance_extraction() {
        // rec_ver = 2 (0x2), rec_instance = 0xABC
        // ver_inst = (0xABC << 4) | 0x2 = 0xABC2
        let data = build_record(2, 0xABC, rt::TEXT_HEADER_ATOM, &[0x00]);
        let hdr = read_record_header(&data, 0).unwrap();
        assert_eq!(hdr.rec_ver, 2);
        assert_eq!(hdr.rec_instance, 0xABC);
        assert_eq!(hdr.rec_type, rt::TEXT_HEADER_ATOM);
    }

    #[test]
    fn container_children_bounded_by_parent_length() {
        // Container claims 10 bytes of children, but the stream has more
        // data after the container. Children iterator must not read past
        // the parent's declared length.
        let child = build_atom(rt::SLIDE_ATOM, &[0x01, 0x02]); // 8 + 2 = 10 bytes
        let container = build_container(rt::SLIDE, &child);

        // Append extra garbage after the container
        let mut data = container.clone();
        data.extend_from_slice(&build_atom(rt::NOTES_ATOM, &[0xFF]));

        let hdr = read_record_header(&data, 0).unwrap();
        let child_records: Vec<_> = children(&data, 0, &hdr)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        // Should only see the one child, not the trailing NOTES_ATOM
        assert_eq!(child_records.len(), 1);
        assert_eq!(child_records[0].1.rec_type, rt::SLIDE_ATOM);
    }

    #[test]
    fn multiple_sibling_records() {
        let mut data = Vec::new();
        data.extend_from_slice(&build_atom(rt::TEXT_HEADER_ATOM, &[0x00]));
        data.extend_from_slice(&build_atom(rt::TEXT_CHARS_ATOM, &[b'A', 0, b'B', 0]));
        data.extend_from_slice(&build_atom(rt::TEXT_BYTES_ATOM, b"C"));

        let records: Vec<_> = RecordIter::new(&data)
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].1.rec_type, rt::TEXT_HEADER_ATOM);
        assert_eq!(records[1].1.rec_type, rt::TEXT_CHARS_ATOM);
        assert_eq!(records[2].1.rec_type, rt::TEXT_BYTES_ATOM);
    }

    #[test]
    fn children_of_non_container_returns_error() {
        let data = build_atom(rt::SLIDE_ATOM, &[0x01]);
        let hdr = read_record_header(&data, 0).unwrap();
        let result = children(&data, 0, &hdr);
        assert!(result.is_err());
    }
}

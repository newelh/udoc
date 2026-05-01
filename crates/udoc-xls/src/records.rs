//! BIFF8 record reader with transparent CONTINUE record reassembly.
//!
//! The Workbook stream is a flat sequence of BIFF8 records. Each record has
//! a 4-byte header: record_type (u16 LE) + record_len (u16 LE), followed by
//! record_len bytes of data. When a record's data exceeds 8,224 bytes, it is
//! split across CONTINUE records (type 0x003C) that immediately follow.
//!
//! The [`BiffReader`] transparently reassembles CONTINUE records and tracks
//! segment boundaries via [`BiffRecord::continue_offsets`] for consumers
//! that need to handle encoding changes at boundaries (notably the SST parser).

use crate::error::{Error, Result};
use crate::{MAX_RECORDS, MAX_RECORD_SIZE};
use udoc_core::diagnostics::Warning;

// -- Record type constants --------------------------------------------------

pub const RT_BOF: u16 = 0x0809;
pub const RT_EOF: u16 = 0x000A;
pub const RT_BOUNDSHEET8: u16 = 0x0085;
pub const RT_CONTINUE: u16 = 0x003C;

pub const RT_SST: u16 = 0x00FC;
pub const RT_EXTSST: u16 = 0x00FF;
pub const RT_LABELSST: u16 = 0x00FD;
pub const RT_LABEL: u16 = 0x0204;

pub const RT_NUMBER: u16 = 0x0203;
pub const RT_RK: u16 = 0x027E;
pub const RT_MULRK: u16 = 0x00BD;
pub const RT_MULBLANK: u16 = 0x00BE;
pub const RT_BLANK: u16 = 0x0201;
pub const RT_BOOLERR: u16 = 0x0205;
pub const RT_FORMULA: u16 = 0x0006;
pub const RT_STRING: u16 = 0x0207;

pub const RT_FORMAT: u16 = 0x041E;
pub const RT_XF: u16 = 0x00E0;
pub const RT_CODEPAGE: u16 = 0x0042;
pub const RT_DATEMODE: u16 = 0x0022;
pub const RT_MERGEDCELLS: u16 = 0x00E5;
/// FILEPASS record (encryption header). When present in the globals
/// substream, the workbook is encrypted and cannot be parsed plaintext.
pub const RT_FILEPASS: u16 = 0x002F;

/// Header size: 2 bytes record type + 2 bytes record length.
const HEADER_SIZE: usize = 4;

/// A logical BIFF8 record with CONTINUE segments already joined.
///
/// If the original record was split across CONTINUE records,
/// `continue_offsets` lists the byte offset within `data` where each
/// CONTINUE segment begins. The SST parser uses these to detect flag-byte
/// re-injection points. All other consumers can ignore the field.
#[derive(Debug, Clone)]
pub struct BiffRecord {
    /// The record type (opcode).
    pub record_type: u16,
    /// The assembled record data (all CONTINUE segments concatenated).
    pub data: Vec<u8>,
    /// Byte offsets within `data` where each CONTINUE segment starts.
    /// Empty if the record was not split across CONTINUE records.
    pub continue_offsets: Vec<u32>,
}

/// Reads BIFF8 records from a byte slice, transparently joining CONTINUE
/// records and tracking segment boundaries.
pub struct BiffReader<'a> {
    data: &'a [u8],
    pos: usize,
    record_count: usize,
    diagnostics: &'a dyn udoc_core::diagnostics::DiagnosticsSink,
}

impl<'a> BiffReader<'a> {
    /// Create a new reader over a Workbook stream byte slice.
    pub fn new(
        data: &'a [u8],
        diagnostics: &'a dyn udoc_core::diagnostics::DiagnosticsSink,
    ) -> Self {
        Self {
            data,
            pos: 0,
            record_count: 0,
            diagnostics,
        }
    }

    /// Current byte position in the stream.
    #[allow(dead_code)] // used in tests; reserved for seek-relative API
    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    /// Seek to an absolute position in the stream.
    pub fn seek(&mut self, pos: usize) {
        self.pos = pos;
    }

    /// Read the next raw record header (type + length) without CONTINUE joining.
    fn read_raw_header(&mut self) -> Result<Option<(u16, u16)>> {
        if self.pos + HEADER_SIZE > self.data.len() {
            if self.pos < self.data.len() {
                self.diagnostics.warning(
                    Warning::new(
                        "truncated_record_header",
                        format!(
                            "truncated record header at offset {} ({} bytes remaining, need {})",
                            self.pos,
                            self.data.len() - self.pos,
                            HEADER_SIZE
                        ),
                    )
                    .at_offset(self.pos as u64),
                );
            }
            return Ok(None);
        }

        let record_type = u16::from_le_bytes([self.data[self.pos], self.data[self.pos + 1]]);
        let record_len = u16::from_le_bytes([self.data[self.pos + 2], self.data[self.pos + 3]]);
        self.pos += HEADER_SIZE;

        Ok(Some((record_type, record_len)))
    }

    /// Read the next logical record, joining any trailing CONTINUE records.
    pub fn next_record(&mut self) -> Result<Option<BiffRecord>> {
        // Read the base record header.
        let (record_type, record_len) = match self.read_raw_header()? {
            Some(h) => h,
            None => return Ok(None),
        };

        self.record_count += 1;
        if self.record_count > MAX_RECORDS {
            return Err(Error::new(format!(
                "exceeded maximum record count ({MAX_RECORDS})"
            )));
        }

        let body_len = record_len as usize;

        // Clamp body to available data.
        let available = self.data.len().saturating_sub(self.pos);
        let actual_len = body_len.min(available);
        if actual_len < body_len {
            self.diagnostics.warning(
                Warning::new(
                    "record_length_exceeds_data",
                    format!(
                        "record type {record_type:#06x} at offset {} has length {body_len} but only {available} bytes remain",
                        self.pos.saturating_sub(HEADER_SIZE)
                    ),
                )
                .at_offset(self.pos.saturating_sub(HEADER_SIZE) as u64),
            );
        }

        let mut data = Vec::with_capacity(actual_len);
        data.extend_from_slice(&self.data[self.pos..self.pos + actual_len]);
        self.pos += actual_len;

        let mut continue_offsets = Vec::new();

        // Consume trailing CONTINUE records.
        loop {
            // Peek at the next header without advancing permanently.
            if self.pos + HEADER_SIZE > self.data.len() {
                break;
            }
            let next_type = u16::from_le_bytes([self.data[self.pos], self.data[self.pos + 1]]);
            if next_type != RT_CONTINUE {
                break;
            }
            let cont_len =
                u16::from_le_bytes([self.data[self.pos + 2], self.data[self.pos + 3]]) as usize;
            self.pos += HEADER_SIZE;

            self.record_count += 1;
            if self.record_count > MAX_RECORDS {
                return Err(Error::new(format!(
                    "exceeded maximum record count ({MAX_RECORDS})"
                )));
            }

            // Track the offset where this CONTINUE segment starts in the
            // assembled data buffer.
            let segment_start = data.len() as u32;
            continue_offsets.push(segment_start);

            // Enforce MAX_RECORD_SIZE incrementally.
            let new_total = data.len().saturating_add(cont_len);
            if new_total > MAX_RECORD_SIZE {
                self.diagnostics.warning(
                    Warning::new(
                        "record_size_exceeded",
                        format!(
                            "reassembled record type {record_type:#06x} exceeds {MAX_RECORD_SIZE} bytes, truncating"
                        ),
                    )
                    .at_offset(self.pos.saturating_sub(HEADER_SIZE) as u64),
                );
                // Skip this CONTINUE segment's body.
                let skip = cont_len.min(self.data.len().saturating_sub(self.pos));
                self.pos += skip;
                // Consume any trailing CONTINUE records for this logical record.
                loop {
                    if self.pos + HEADER_SIZE > self.data.len() {
                        break;
                    }
                    let peek_type =
                        u16::from_le_bytes([self.data[self.pos], self.data[self.pos + 1]]);
                    if peek_type != RT_CONTINUE {
                        break;
                    }
                    let peek_len =
                        u16::from_le_bytes([self.data[self.pos + 2], self.data[self.pos + 3]])
                            as usize;
                    self.pos += HEADER_SIZE;
                    let s = peek_len.min(self.data.len().saturating_sub(self.pos));
                    self.pos += s;
                }
                break;
            }

            let available = self.data.len().saturating_sub(self.pos);
            let actual = cont_len.min(available);
            if actual < cont_len {
                self.diagnostics.warning(
                    Warning::new(
                        "continue_truncated",
                        format!(
                            "CONTINUE record truncated: expected {cont_len} bytes, have {available}"
                        ),
                    )
                    .at_offset(self.pos.saturating_sub(HEADER_SIZE) as u64),
                );
            }

            data.extend_from_slice(&self.data[self.pos..self.pos + actual]);
            self.pos += actual;
        }

        Ok(Some(BiffRecord {
            record_type,
            data,
            continue_offsets,
        }))
    }

    /// Iterate all records from the current position.
    #[allow(dead_code)] // used in tests; reserved for batch-processing API
    pub(crate) fn records(self) -> BiffRecordIter<'a> {
        BiffRecordIter { reader: self }
    }
}

/// Iterator adapter over [`BiffReader`].
#[allow(dead_code)] // used in tests via records()
pub(crate) struct BiffRecordIter<'a> {
    reader: BiffReader<'a>,
}

impl<'a> Iterator for BiffRecordIter<'a> {
    type Item = Result<BiffRecord>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.reader.next_record() {
            Ok(Some(rec)) => Some(Ok(rec)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::diagnostics::CollectingDiagnostics;

    /// Build a raw BIFF8 record from type and data.
    fn build_record(record_type: u16, data: &[u8]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(HEADER_SIZE + data.len());
        buf.extend_from_slice(&record_type.to_le_bytes());
        buf.extend_from_slice(&(data.len() as u16).to_le_bytes());
        buf.extend_from_slice(data);
        buf
    }

    #[test]
    fn empty_stream_yields_no_records() {
        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&[], &diag);
        assert!(reader.next_record().unwrap().is_none());
    }

    #[test]
    fn single_record_exact_parse() {
        let data = build_record(RT_BOF, &[0x00, 0x06, 0x05, 0x00, 0x00, 0x00]);
        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&data, &diag);

        let rec = reader.next_record().unwrap().unwrap();
        assert_eq!(rec.record_type, RT_BOF);
        assert_eq!(rec.data, &[0x00, 0x06, 0x05, 0x00, 0x00, 0x00]);
        assert!(rec.continue_offsets.is_empty());

        assert!(reader.next_record().unwrap().is_none());
    }

    #[test]
    fn multiple_sequential_records() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&build_record(RT_BOF, &[0x00, 0x06]));
        stream.extend_from_slice(&build_record(RT_CODEPAGE, &[0xE4, 0x04]));
        stream.extend_from_slice(&build_record(RT_EOF, &[]));

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);

        let r1 = reader.next_record().unwrap().unwrap();
        assert_eq!(r1.record_type, RT_BOF);
        let r2 = reader.next_record().unwrap().unwrap();
        assert_eq!(r2.record_type, RT_CODEPAGE);
        assert_eq!(r2.data, &[0xE4, 0x04]);
        let r3 = reader.next_record().unwrap().unwrap();
        assert_eq!(r3.record_type, RT_EOF);
        assert!(r3.data.is_empty());

        assert!(reader.next_record().unwrap().is_none());
    }

    #[test]
    fn record_with_zero_length_body() {
        let data = build_record(RT_EOF, &[]);
        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&data, &diag);

        let rec = reader.next_record().unwrap().unwrap();
        assert_eq!(rec.record_type, RT_EOF);
        assert!(rec.data.is_empty());
        assert!(rec.continue_offsets.is_empty());
    }

    #[test]
    fn truncated_header_warns() {
        // Only 2 bytes -- not enough for a full 4-byte header.
        let data = [0x09, 0x08];
        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&data, &diag);

        assert!(reader.next_record().unwrap().is_none());
        assert_eq!(diag.warnings().len(), 1);
        assert!(diag.warnings()[0].kind.as_str().contains("truncated"));
    }

    #[test]
    fn record_length_past_stream_end_warns() {
        // Header says 100 bytes but only 4 bytes of body available.
        let mut data = Vec::new();
        data.extend_from_slice(&RT_BOF.to_le_bytes());
        data.extend_from_slice(&100u16.to_le_bytes());
        data.extend_from_slice(&[0xAA; 4]);

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&data, &diag);

        let rec = reader.next_record().unwrap().unwrap();
        assert_eq!(rec.record_type, RT_BOF);
        assert_eq!(rec.data.len(), 4); // Clamped to available.
        assert_eq!(diag.warnings().len(), 1);
        assert!(diag.warnings()[0].kind.as_str().contains("exceeds"));
    }

    #[test]
    fn single_continue_after_base_record() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&build_record(RT_SST, &[0x01, 0x02, 0x03]));
        stream.extend_from_slice(&build_record(RT_CONTINUE, &[0x04, 0x05]));
        stream.extend_from_slice(&build_record(RT_EOF, &[]));

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);

        let rec = reader.next_record().unwrap().unwrap();
        assert_eq!(rec.record_type, RT_SST);
        assert_eq!(rec.data, &[0x01, 0x02, 0x03, 0x04, 0x05]);
        assert_eq!(rec.continue_offsets, &[3]); // CONTINUE starts at byte 3.

        let eof = reader.next_record().unwrap().unwrap();
        assert_eq!(eof.record_type, RT_EOF);
    }

    #[test]
    fn multiple_continue_chain() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&build_record(RT_SST, &[0xAA; 4]));
        stream.extend_from_slice(&build_record(RT_CONTINUE, &[0xBB; 3]));
        stream.extend_from_slice(&build_record(RT_CONTINUE, &[0xCC; 2]));
        stream.extend_from_slice(&build_record(RT_EOF, &[]));

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);

        let rec = reader.next_record().unwrap().unwrap();
        assert_eq!(rec.record_type, RT_SST);
        assert_eq!(rec.data.len(), 9); // 4 + 3 + 2
        assert_eq!(&rec.data[0..4], &[0xAA; 4]);
        assert_eq!(&rec.data[4..7], &[0xBB; 3]);
        assert_eq!(&rec.data[7..9], &[0xCC; 2]);
        assert_eq!(rec.continue_offsets, &[4, 7]);
    }

    #[test]
    fn continue_after_unknown_record_type_is_joined_correctly() {
        // Unknown record type 0xBEEF followed by CONTINUE.
        let mut stream = Vec::new();
        stream.extend_from_slice(&build_record(0xBEEF, &[0x01]));
        stream.extend_from_slice(&build_record(RT_CONTINUE, &[0x02]));
        stream.extend_from_slice(&build_record(RT_EOF, &[]));

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);

        let rec = reader.next_record().unwrap().unwrap();
        assert_eq!(rec.record_type, 0xBEEF);
        assert_eq!(rec.data, &[0x01, 0x02]);
        assert_eq!(rec.continue_offsets, &[1]);
    }

    #[test]
    fn continue_offsets_populated_at_correct_positions() {
        let mut stream = Vec::new();
        let base_data = vec![0x11; 10];
        let cont1_data = vec![0x22; 5];
        let cont2_data = vec![0x33; 8];
        stream.extend_from_slice(&build_record(RT_SST, &base_data));
        stream.extend_from_slice(&build_record(RT_CONTINUE, &cont1_data));
        stream.extend_from_slice(&build_record(RT_CONTINUE, &cont2_data));

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);

        let rec = reader.next_record().unwrap().unwrap();
        assert_eq!(rec.data.len(), 23); // 10 + 5 + 8
        assert_eq!(rec.continue_offsets, &[10, 15]); // offsets 10 and 15
    }

    #[test]
    fn empty_continue_record_is_valid() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&build_record(RT_SST, &[0x01]));
        stream.extend_from_slice(&build_record(RT_CONTINUE, &[]));
        stream.extend_from_slice(&build_record(RT_EOF, &[]));

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);

        let rec = reader.next_record().unwrap().unwrap();
        assert_eq!(rec.record_type, RT_SST);
        assert_eq!(rec.data, &[0x01]);
        assert_eq!(rec.continue_offsets, &[1]); // Boundary at offset 1.
    }

    #[test]
    fn iterator_adapter_works() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&build_record(RT_BOF, &[0x00, 0x06]));
        stream.extend_from_slice(&build_record(RT_EOF, &[]));

        let diag = CollectingDiagnostics::new();
        let reader = BiffReader::new(&stream, &diag);
        let records: Vec<_> = reader
            .records()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].record_type, RT_BOF);
        assert_eq!(records[1].record_type, RT_EOF);
    }

    #[test]
    fn seek_and_resume_reading() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&build_record(RT_BOF, &[0x00, 0x06]));
        let second_pos = stream.len();
        stream.extend_from_slice(&build_record(RT_CODEPAGE, &[0xE4, 0x04]));
        stream.extend_from_slice(&build_record(RT_EOF, &[]));

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);

        // Seek past the BOF record.
        reader.seek(second_pos);
        let rec = reader.next_record().unwrap().unwrap();
        assert_eq!(rec.record_type, RT_CODEPAGE);
    }
}

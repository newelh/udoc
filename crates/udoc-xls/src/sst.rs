//! Shared String Table (SST) parser with CONTINUE-boundary flag-byte handling.
//!
//! The SST record (0x00FC) contains all unique strings referenced by LABELSST
//! cells. When the SST spans multiple CONTINUE records, strings that cross
//! segment boundaries have their encoding re-declared by a flag byte at the
//! start of each CONTINUE segment. This module handles that re-injection
//! transparently via `SstCursor`.

use crate::error::{Error, Result};
use crate::records::BiffRecord;
use crate::MAX_SST_ENTRIES;
use udoc_core::diagnostics::{DiagnosticsSink, Warning};

/// Parse the Shared String Table from an SST BiffRecord.
///
/// Returns a Vec of unique strings. LABELSST cells reference these by index.
pub fn parse_sst(record: &BiffRecord, diag: &dyn DiagnosticsSink) -> Result<Vec<String>> {
    if record.data.len() < 8 {
        return Err(Error::new("SST record too short (need at least 8 bytes)"));
    }

    let _cst_total = u32::from_le_bytes([
        record.data[0],
        record.data[1],
        record.data[2],
        record.data[3],
    ]);
    let cst_unique = u32::from_le_bytes([
        record.data[4],
        record.data[5],
        record.data[6],
        record.data[7],
    ]) as usize;

    if cst_unique > MAX_SST_ENTRIES {
        diag.warning(Warning::new(
            "sst_too_large",
            format!("SST claims {cst_unique} unique strings, capping at {MAX_SST_ENTRIES}"),
        ));
    }
    let target_count = cst_unique.min(MAX_SST_ENTRIES);

    let mut cursor = SstCursor::new(&record.data, &record.continue_offsets);
    cursor.skip(8); // Skip cstTotal + cstUnique.

    let mut strings = Vec::with_capacity(target_count.min(65536));

    for i in 0..target_count {
        match cursor.read_xl_unicode_string(diag) {
            Ok(s) => strings.push(s),
            Err(e) => {
                diag.warning(Warning::new(
                    "sst_string_truncated",
                    format!("SST string {i} truncated: {e}"),
                ));
                break;
            }
        }
    }

    Ok(strings)
}

// ---------------------------------------------------------------------------
// SstCursor -- continuation-aware reader for SST data
// ---------------------------------------------------------------------------

/// A cursor over SST record data that handles CONTINUE segment boundaries.
///
/// When reading character data mid-string, crossing a CONTINUE boundary
/// consumes a flag byte that re-declares the encoding (compressed vs UTF-16LE).
/// When NOT mid-string (between strings), no flag byte is consumed.
struct SstCursor<'a> {
    data: &'a [u8],
    pos: usize,
    /// Sorted byte offsets where CONTINUE segments begin.
    continue_offsets: Vec<usize>,
    /// Index into continue_offsets for the next unprocessed boundary.
    /// Avoids O(n) linear scan per next_continue_boundary() call.
    boundary_idx: usize,
}

impl<'a> SstCursor<'a> {
    fn new(data: &'a [u8], continue_offsets: &[u32]) -> Self {
        Self {
            data,
            pos: 0,
            continue_offsets: continue_offsets.iter().map(|&o| o as usize).collect(),
            boundary_idx: 0,
        }
    }

    fn skip(&mut self, n: usize) {
        self.pos = (self.pos + n).min(self.data.len());
    }

    fn read_u8(&mut self) -> Result<u8> {
        if self.pos >= self.data.len() {
            return Err(Error::new("unexpected end of SST data"));
        }
        let v = self.data[self.pos];
        self.pos += 1;
        Ok(v)
    }

    fn read_u16(&mut self) -> Result<u16> {
        if self.pos + 2 > self.data.len() {
            return Err(Error::new("unexpected end of SST data reading u16"));
        }
        let v = u16::from_le_bytes([self.data[self.pos], self.data[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    fn read_u32(&mut self) -> Result<u32> {
        if self.pos + 4 > self.data.len() {
            return Err(Error::new("unexpected end of SST data reading u32"));
        }
        let v = u32::from_le_bytes([
            self.data[self.pos],
            self.data[self.pos + 1],
            self.data[self.pos + 2],
            self.data[self.pos + 3],
        ]);
        self.pos += 4;
        Ok(v)
    }

    /// Check if the current position is exactly at a CONTINUE boundary.
    fn at_continue_boundary(&mut self) -> bool {
        // Advance boundary_idx past boundaries we have already passed.
        while self.boundary_idx < self.continue_offsets.len()
            && self.continue_offsets[self.boundary_idx] < self.pos
        {
            self.boundary_idx += 1;
        }
        self.boundary_idx < self.continue_offsets.len()
            && self.continue_offsets[self.boundary_idx] == self.pos
    }

    /// Read an XLUnicodeRichExtendedString.
    ///
    /// Layout:
    /// - cch: u16 (character count)
    /// - grbit: u8 (bit 0 = fHighByte, bit 2 = fExtSt, bit 3 = fRichSt)
    /// - [cRun: u16] if fRichSt
    /// - [cbExtRst: u32] if fExtSt
    /// - chars: cch bytes (compressed) or cch*2 bytes (UTF-16LE)
    /// - [rgRun: cRun*4 bytes] if fRichSt
    /// - [ExtRst: cbExtRst bytes] if fExtSt
    fn read_xl_unicode_string(&mut self, diag: &dyn DiagnosticsSink) -> Result<String> {
        let raw_cch = self.read_u16()? as usize;
        let cch = if raw_cch > crate::MAX_STRING_LENGTH {
            diag.warning(Warning::new(
                "sst_string_length_exceeded",
                format!(
                    "SST string claims {} chars, capping at {}",
                    raw_cch,
                    crate::MAX_STRING_LENGTH
                ),
            ));
            crate::MAX_STRING_LENGTH
        } else {
            raw_cch
        };
        let grbit = self.read_u8()?;

        let high_byte = (grbit & 0x01) != 0;
        let f_ext_st = (grbit & 0x04) != 0;
        let f_rich_st = (grbit & 0x08) != 0;

        let c_run = if f_rich_st {
            self.read_u16()? as usize
        } else {
            0
        };
        let cb_ext_rst = if f_ext_st {
            self.read_u32()? as usize
        } else {
            0
        };

        // Read the character data, handling CONTINUE boundaries.
        let text = self.read_chars(cch, high_byte, diag)?;

        // Skip rich text formatting runs (4 bytes each).
        if c_run > 0 {
            self.skip_bytes_across_boundaries(c_run.saturating_mul(4));
        }

        // Skip ExtRst phonetic data.
        if cb_ext_rst > 0 {
            self.skip_bytes_across_boundaries(cb_ext_rst);
        }

        Ok(text)
    }

    /// Read `count` characters, handling CONTINUE boundary encoding changes.
    ///
    /// At each CONTINUE boundary crossed while reading characters, the first
    /// byte of the new segment is a flag byte that re-declares the encoding:
    /// 0x00 = compressed (1 byte/char), 0x01 = UTF-16LE (2 bytes/char).
    fn read_chars(
        &mut self,
        count: usize,
        initial_high_byte: bool,
        _diag: &dyn DiagnosticsSink,
    ) -> Result<String> {
        if count == 0 {
            return Ok(String::new());
        }

        let mut result = String::with_capacity(count);
        let mut chars_read = 0;
        let mut high_byte = initial_high_byte;

        while chars_read < count {
            // Check if we hit a CONTINUE boundary while mid-string.
            if self.at_continue_boundary() && chars_read > 0 {
                // Consume the flag byte that re-declares encoding.
                let flag = self.read_u8()?;
                high_byte = (flag & 0x01) != 0;
            }

            // How many chars can we read before the next CONTINUE boundary?
            let next_boundary = self.next_continue_boundary();
            let bytes_until_boundary = next_boundary.saturating_sub(self.pos);

            let chars_remaining = count - chars_read;
            let bytes_per_char = if high_byte { 2 } else { 1 };
            // bytes_per_char is always 1 (compressed) or 2 (UTF-16LE).
            let chars_in_segment = bytes_until_boundary / bytes_per_char;
            let chars_to_read = chars_remaining.min(chars_in_segment);

            if chars_to_read == 0 && chars_read < count {
                // We are at a boundary but haven't started reading chars yet
                // in this segment. Advance past the boundary.
                if self.at_continue_boundary() {
                    let flag = self.read_u8()?;
                    high_byte = (flag & 0x01) != 0;
                    continue;
                }
                // Not enough data.
                return Err(Error::new(format!(
                    "SST string truncated: read {chars_read} of {count} chars"
                )));
            }

            // Read the character bytes.
            if high_byte {
                for _ in 0..chars_to_read {
                    if self.pos + 2 > self.data.len() {
                        return Err(Error::new(format!(
                            "SST string truncated reading UTF-16: read {chars_read} of {count}"
                        )));
                    }
                    let code_unit =
                        u16::from_le_bytes([self.data[self.pos], self.data[self.pos + 1]]);
                    self.pos += 2;
                    // Handle surrogate pairs later if needed; for now, treat
                    // each code unit as a BMP character.
                    if let Some(ch) = char::from_u32(code_unit as u32) {
                        result.push(ch);
                    } else {
                        result.push('\u{FFFD}');
                    }
                }
            } else {
                for _ in 0..chars_to_read {
                    if self.pos >= self.data.len() {
                        return Err(Error::new(format!(
                            "SST string truncated reading compressed: read {chars_read} of {count}"
                        )));
                    }
                    let byte = self.data[self.pos];
                    self.pos += 1;
                    // Compressed strings use Latin-1 (ISO 8859-1) encoding.
                    result.push(byte as char);
                }
            }

            chars_read += chars_to_read;
        }

        Ok(result)
    }

    /// Find the next CONTINUE boundary offset strictly after the current position.
    fn next_continue_boundary(&self) -> usize {
        // Start from boundary_idx which is already advanced past earlier boundaries.
        for i in self.boundary_idx..self.continue_offsets.len() {
            if self.continue_offsets[i] > self.pos {
                return self.continue_offsets[i];
            }
        }
        self.data.len() // No more boundaries, extend to end of data.
    }

    /// Skip `n` bytes across CONTINUE boundaries without consuming flag bytes.
    /// Used for rgRun and ExtRst data which are raw bytes, not character data.
    fn skip_bytes_across_boundaries(&mut self, n: usize) {
        let target = (self.pos + n).min(self.data.len());
        self.pos = target;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records;
    use udoc_core::diagnostics::CollectingDiagnostics;

    /// Build a minimal SST record body with the given strings.
    /// Each string is encoded as XLUnicodeRichExtendedString.
    fn build_sst_body(strings: &[&str], high_byte: bool) -> Vec<u8> {
        let count = strings.len() as u32;
        let mut body = Vec::new();
        // cstTotal (= cstUnique for simplicity).
        body.extend_from_slice(&count.to_le_bytes());
        // cstUnique.
        body.extend_from_slice(&count.to_le_bytes());

        for s in strings {
            let chars: Vec<char> = s.chars().collect();
            let cch = chars.len() as u16;
            body.extend_from_slice(&cch.to_le_bytes());
            let grbit: u8 = if high_byte { 0x01 } else { 0x00 };
            body.push(grbit);
            if high_byte {
                for &ch in &chars {
                    let code_unit = ch as u16;
                    body.extend_from_slice(&code_unit.to_le_bytes());
                }
            } else {
                for &ch in &chars {
                    body.push(ch as u8);
                }
            }
        }
        body
    }

    fn make_sst_record(body: Vec<u8>, continue_offsets: Vec<u32>) -> BiffRecord {
        BiffRecord {
            record_type: records::RT_SST,
            data: body,
            continue_offsets,
        }
    }

    #[test]
    fn sst_zero_strings() {
        let body = build_sst_body(&[], false);
        let record = make_sst_record(body, vec![]);
        let diag = CollectingDiagnostics::new();
        let strings = parse_sst(&record, &diag).unwrap();
        assert!(strings.is_empty());
    }

    #[test]
    fn sst_one_ascii_string() {
        let body = build_sst_body(&["Hello"], false);
        let record = make_sst_record(body, vec![]);
        let diag = CollectingDiagnostics::new();
        let strings = parse_sst(&record, &diag).unwrap();
        assert_eq!(strings, vec!["Hello"]);
    }

    #[test]
    fn sst_one_utf16_string() {
        let body = build_sst_body(&["World"], true);
        let record = make_sst_record(body, vec![]);
        let diag = CollectingDiagnostics::new();
        let strings = parse_sst(&record, &diag).unwrap();
        assert_eq!(strings, vec!["World"]);
    }

    #[test]
    fn sst_multiple_strings() {
        let body = build_sst_body(&["foo", "bar", "baz"], false);
        let record = make_sst_record(body, vec![]);
        let diag = CollectingDiagnostics::new();
        let strings = parse_sst(&record, &diag).unwrap();
        assert_eq!(strings, vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn sst_string_at_exact_continue_boundary() {
        // Two strings: "AB" ends exactly at the CONTINUE boundary,
        // "CD" starts at the boundary. No flag byte needed.
        let mut body = Vec::new();
        body.extend_from_slice(&2u32.to_le_bytes()); // cstTotal
        body.extend_from_slice(&2u32.to_le_bytes()); // cstUnique

        // String 1: "AB" compressed (cch=2, grbit=0, 2 bytes)
        body.extend_from_slice(&2u16.to_le_bytes());
        body.push(0x00); // grbit: compressed
        body.push(b'A');
        body.push(b'B');

        let boundary = body.len();

        // String 2: "CD" compressed (cch=2, grbit=0, 2 bytes)
        body.extend_from_slice(&2u16.to_le_bytes());
        body.push(0x00);
        body.push(b'C');
        body.push(b'D');

        let record = make_sst_record(body, vec![boundary as u32]);
        let diag = CollectingDiagnostics::new();
        let strings = parse_sst(&record, &diag).unwrap();
        assert_eq!(strings, vec!["AB", "CD"]);
    }

    #[test]
    fn sst_string_split_across_continue_same_encoding() {
        // One 4-char compressed string "ABCD" split across CONTINUE.
        // First segment has "AB" (2 bytes), CONTINUE has flag + "CD".
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes()); // cstTotal
        body.extend_from_slice(&1u32.to_le_bytes()); // cstUnique

        // cch=4, grbit=0 (compressed).
        body.extend_from_slice(&4u16.to_le_bytes());
        body.push(0x00);
        body.push(b'A');
        body.push(b'B');

        let boundary = body.len();

        // CONTINUE flag byte: 0x00 (still compressed), then "CD".
        body.push(0x00); // flag: compressed
        body.push(b'C');
        body.push(b'D');

        let record = make_sst_record(body, vec![boundary as u32]);
        let diag = CollectingDiagnostics::new();
        let strings = parse_sst(&record, &diag).unwrap();
        assert_eq!(strings, vec!["ABCD"]);
    }

    #[test]
    fn sst_string_split_with_encoding_change_compressed_to_utf16() {
        // One 4-char string starts compressed ("AB"), then CONTINUE
        // switches to UTF-16LE for "CD".
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes()); // cstTotal
        body.extend_from_slice(&1u32.to_le_bytes()); // cstUnique

        // cch=4, grbit=0 (compressed).
        body.extend_from_slice(&4u16.to_le_bytes());
        body.push(0x00);
        body.push(b'A');
        body.push(b'B');

        let boundary = body.len();

        // CONTINUE flag byte: 0x01 (UTF-16LE), then "CD" as UTF-16LE.
        body.push(0x01); // flag: high byte
        body.extend_from_slice(&(b'C' as u16).to_le_bytes());
        body.extend_from_slice(&(b'D' as u16).to_le_bytes());

        let record = make_sst_record(body, vec![boundary as u32]);
        let diag = CollectingDiagnostics::new();
        let strings = parse_sst(&record, &diag).unwrap();
        assert_eq!(strings, vec!["ABCD"]);
    }

    #[test]
    fn sst_string_split_with_encoding_change_utf16_to_compressed() {
        // One 4-char string starts UTF-16LE ("AB"), then CONTINUE
        // switches to compressed for "CD".
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes()); // cstTotal
        body.extend_from_slice(&1u32.to_le_bytes()); // cstUnique

        // cch=4, grbit=1 (UTF-16LE).
        body.extend_from_slice(&4u16.to_le_bytes());
        body.push(0x01);
        body.extend_from_slice(&(b'A' as u16).to_le_bytes());
        body.extend_from_slice(&(b'B' as u16).to_le_bytes());

        let boundary = body.len();

        // CONTINUE flag byte: 0x00 (compressed), then "CD" as single bytes.
        body.push(0x00); // flag: compressed
        body.push(b'C');
        body.push(b'D');

        let record = make_sst_record(body, vec![boundary as u32]);
        let diag = CollectingDiagnostics::new();
        let strings = parse_sst(&record, &diag).unwrap();
        assert_eq!(strings, vec!["ABCD"]);
    }

    #[test]
    fn sst_string_with_rich_text_runs() {
        // One string with fRichSt=1, cRun=2 (8 bytes of run data to skip).
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes()); // cstTotal
        body.extend_from_slice(&1u32.to_le_bytes()); // cstUnique

        // cch=3, grbit=0x08 (fRichSt, compressed), cRun=2.
        body.extend_from_slice(&3u16.to_le_bytes());
        body.push(0x08); // fRichSt
        body.extend_from_slice(&2u16.to_le_bytes()); // cRun=2
        body.push(b'A');
        body.push(b'B');
        body.push(b'C');
        // 2 formatting runs, 4 bytes each (8 bytes total) -- dummy data.
        body.extend_from_slice(&[0x00; 8]);

        let record = make_sst_record(body, vec![]);
        let diag = CollectingDiagnostics::new();
        let strings = parse_sst(&record, &diag).unwrap();
        assert_eq!(strings, vec!["ABC"]);
    }

    #[test]
    fn sst_string_with_ext_rst() {
        // One string with fExtSt=1, cbExtRst=6 (6 bytes of phonetic data).
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes()); // cstTotal
        body.extend_from_slice(&1u32.to_le_bytes()); // cstUnique

        // cch=2, grbit=0x04 (fExtSt, compressed), cbExtRst=6.
        body.extend_from_slice(&2u16.to_le_bytes());
        body.push(0x04); // fExtSt
        body.extend_from_slice(&6u32.to_le_bytes()); // cbExtRst=6
        body.push(b'H');
        body.push(b'i');
        // 6 bytes of ExtRst dummy data.
        body.extend_from_slice(&[0xFF; 6]);

        let record = make_sst_record(body, vec![]);
        let diag = CollectingDiagnostics::new();
        let strings = parse_sst(&record, &diag).unwrap();
        assert_eq!(strings, vec!["Hi"]);
    }

    #[test]
    fn sst_string_with_rich_text_and_ext_rst() {
        // One string with both fRichSt and fExtSt.
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes());

        // cch=2, grbit=0x0C (fRichSt + fExtSt), cRun=1, cbExtRst=4.
        body.extend_from_slice(&2u16.to_le_bytes());
        body.push(0x0C); // fRichSt | fExtSt
        body.extend_from_slice(&1u16.to_le_bytes()); // cRun=1
        body.extend_from_slice(&4u32.to_le_bytes()); // cbExtRst=4
        body.push(b'O');
        body.push(b'K');
        // 1 formatting run (4 bytes).
        body.extend_from_slice(&[0x00; 4]);
        // 4 bytes of ExtRst.
        body.extend_from_slice(&[0xFF; 4]);

        let record = make_sst_record(body, vec![]);
        let diag = CollectingDiagnostics::new();
        let strings = parse_sst(&record, &diag).unwrap();
        assert_eq!(strings, vec!["OK"]);
    }

    #[test]
    fn sst_many_strings_no_quadratic() {
        // 500 short strings, verify it completes quickly.
        let strs: Vec<&str> = (0..500).map(|_| "x").collect();
        let body = build_sst_body(&strs, false);
        let record = make_sst_record(body, vec![]);
        let diag = CollectingDiagnostics::new();
        let strings = parse_sst(&record, &diag).unwrap();
        assert_eq!(strings.len(), 500);
        assert!(strings.iter().all(|s| s == "x"));
    }

    #[test]
    fn sst_truncated_string_warns() {
        // SST claims 1 string with cch=100 but body is too short.
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&100u16.to_le_bytes()); // cch=100
        body.push(0x00); // compressed
        body.extend_from_slice(&[b'A'; 5]); // Only 5 chars of 100.

        let record = make_sst_record(body, vec![]);
        let diag = CollectingDiagnostics::new();
        let strings = parse_sst(&record, &diag).unwrap();
        // Should have warned and returned partial results.
        assert!(strings.is_empty() || strings.len() <= 1);
        assert!(!diag.warnings().is_empty());
    }

    #[test]
    fn sst_ext_rst_crossing_continue_boundary() {
        // String with ExtRst that crosses a CONTINUE boundary.
        // The ExtRst skip should work across boundaries without
        // consuming a flag byte (ExtRst is raw byte data).
        let mut body = Vec::new();
        body.extend_from_slice(&2u32.to_le_bytes()); // cstTotal
        body.extend_from_slice(&2u32.to_le_bytes()); // cstUnique

        // String 1: "Hi" with ExtRst of 6 bytes.
        body.extend_from_slice(&2u16.to_le_bytes()); // cch=2
        body.push(0x04); // fExtSt
        body.extend_from_slice(&6u32.to_le_bytes()); // cbExtRst=6
        body.push(b'H');
        body.push(b'i');
        // Only 3 bytes of ExtRst before boundary.
        body.extend_from_slice(&[0xEE; 3]);

        let boundary = body.len();

        // Remaining 3 bytes of ExtRst after boundary.
        body.extend_from_slice(&[0xEE; 3]);

        // String 2: "OK" (simple, no extensions).
        body.extend_from_slice(&2u16.to_le_bytes()); // cch=2
        body.push(0x00); // compressed, no extensions
        body.push(b'O');
        body.push(b'K');

        let record = make_sst_record(body, vec![boundary as u32]);
        let diag = CollectingDiagnostics::new();
        let strings = parse_sst(&record, &diag).unwrap();
        assert_eq!(strings, vec!["Hi", "OK"]);
    }
}

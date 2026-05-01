//! Piece table parser for DOC binary format.
//!
//! Parses the CLX structure from the Table stream and reassembles text
//! from the WordDocument stream. The CLX contains a PlcPcd (Plex of PCDs)
//! which maps character positions (CPs) to byte offsets in the WordDocument
//! stream, along with encoding information (CP1252 compressed vs UTF-16LE).
//!
//! Reference: MS-DOC 2.8.35 (Clx), 2.8.36 (Pcdt), 2.8.37 (PlcPcd)

use udoc_core::codepage::encoding_for_codepage;
use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use crate::error::{Error, Result};
use crate::MAX_PIECES;

/// A single piece descriptor mapping a CP range to a byte range.
#[derive(Debug, Clone)]
pub(crate) struct Piece {
    /// Starting character position (inclusive).
    pub(crate) cp_start: u32,
    /// Ending character position (exclusive).
    pub(crate) cp_end: u32,
    /// Byte offset in the WordDocument stream.
    pub(crate) byte_offset: u32,
    /// True if text is CP1252-encoded (compressed), false if UTF-16LE.
    pub(crate) is_compressed: bool,
}

/// Parsed piece table from the CLX structure.
#[derive(Debug)]
pub struct PieceTable {
    pub(crate) pieces: Vec<Piece>,
}

impl PieceTable {
    /// Parse the piece table from the Table stream.
    ///
    /// `table_stream` is the full Table stream (0Table or 1Table).
    /// `fc_clx` and `lcb_clx` are from the FIB, pointing to the CLX data.
    pub fn parse(table_stream: &[u8], fc_clx: u32, lcb_clx: u32) -> Result<Self> {
        let start = fc_clx as usize;
        let end = start
            .checked_add(lcb_clx as usize)
            .ok_or_else(|| Error::new("CLX end offset overflow"))?;

        let clx_data = table_stream.get(start..end).ok_or_else(|| {
            Error::new(format!(
                "CLX data out of bounds: offset {start}..{end}, table stream length {}",
                table_stream.len()
            ))
        })?;

        if clx_data.is_empty() {
            return Err(Error::new("CLX data is empty"));
        }

        // Skip any Prc entries (prefix byte 0x01)
        let mut pos = 0;
        while pos < clx_data.len() && clx_data[pos] == 0x01 {
            // Prc: 0x01 + cbGrpprl (u16) + grpprl data
            if pos + 3 > clx_data.len() {
                return Err(Error::new(format!(
                    "truncated Prc entry at CLX offset {pos}"
                )));
            }
            let cb_grpprl = u16::from_le_bytes([clx_data[pos + 1], clx_data[pos + 2]]) as usize;
            let skip = 3usize
                .checked_add(cb_grpprl)
                .ok_or_else(|| Error::new("Prc size overflow"))?;
            pos = pos
                .checked_add(skip)
                .ok_or_else(|| Error::new("Prc skip overflow"))?;
        }

        // Next byte must be 0x02 (Pcdt marker)
        if pos >= clx_data.len() {
            return Err(Error::new("CLX data ends before Pcdt marker"));
        }
        if clx_data[pos] != 0x02 {
            return Err(Error::new(format!(
                "expected Pcdt marker 0x02 at CLX offset {pos}, found 0x{:02X}",
                clx_data[pos]
            )));
        }
        pos += 1;

        // Read lcbPlcPcd (u32)
        if pos + 4 > clx_data.len() {
            return Err(Error::new("truncated Pcdt: missing lcbPlcPcd"));
        }
        let lcb_plc_pcd = u32::from_le_bytes([
            clx_data[pos],
            clx_data[pos + 1],
            clx_data[pos + 2],
            clx_data[pos + 3],
        ]) as usize;
        pos += 4;

        // PlcPcd data
        let plc_pcd_end = pos
            .checked_add(lcb_plc_pcd)
            .ok_or_else(|| Error::new("PlcPcd end offset overflow"))?;
        let plc_pcd_data = clx_data.get(pos..plc_pcd_end).ok_or_else(|| {
            Error::new(format!(
                "PlcPcd data out of bounds: need {} bytes at offset {pos}, have {} remaining",
                lcb_plc_pcd,
                clx_data.len().saturating_sub(pos)
            ))
        })?;

        // PlcPcd structure: (n+1) CPs (u32 each) + n PCDs (8 bytes each)
        // Total = (n+1)*4 + n*8 = 4 + 12*n
        // So n = (lcbPlcPcd - 4) / 12
        if lcb_plc_pcd < 4 {
            return Err(Error::new(format!(
                "PlcPcd too small: {lcb_plc_pcd} bytes, need at least 4"
            )));
        }
        let remainder = lcb_plc_pcd - 4;
        if !remainder.is_multiple_of(12) {
            return Err(Error::new(format!(
                "PlcPcd size mismatch: (lcbPlcPcd - 4) = {remainder} is not divisible by 12"
            )));
        }
        let n = remainder / 12;

        if n > MAX_PIECES {
            return Err(Error::new(format!(
                "too many pieces: {n}, maximum is {MAX_PIECES}"
            )));
        }

        let mut pieces = Vec::with_capacity(n);

        for i in 0..n {
            // Read CP[i] and CP[i+1]
            let cp_start = read_plc_u32(plc_pcd_data, i * 4, "CP[start]")?;
            let cp_end = read_plc_u32(plc_pcd_data, (i + 1) * 4, "CP[end]")?;

            // PCD starts after (n+1) CPs
            let pcd_base = (n + 1) * 4 + i * 8;
            // PCD layout: bytes 0-1 (igrpprl, unused), bytes 2-5 (fc), bytes 6-7 (prm, unused)
            let fc_raw = read_plc_u32(plc_pcd_data, pcd_base + 2, "PCD fc")?;

            // Bit 30 = fCompressed
            let is_compressed = (fc_raw >> 30) & 1 != 0;
            let raw_offset = fc_raw & 0x3FFF_FFFF;

            let byte_offset = if is_compressed {
                // For compressed text, the actual byte offset is fc/2
                raw_offset / 2
            } else {
                raw_offset
            };

            pieces.push(Piece {
                cp_start,
                cp_end,
                byte_offset,
                is_compressed,
            });
        }

        Ok(PieceTable { pieces })
    }

    /// Assemble text from the WordDocument stream for a CP range.
    ///
    /// Iterates through pieces that overlap `[cp_start, cp_end)`,
    /// clips each piece to the requested range, reads the appropriate
    /// bytes from `word_doc_stream`, and decodes according to the piece's
    /// encoding (CP1252 for compressed, UTF-16LE for uncompressed).
    pub fn assemble_text(
        &self,
        word_doc_stream: &[u8],
        cp_start: u32,
        cp_end: u32,
    ) -> Result<String> {
        if cp_start >= cp_end {
            return Ok(String::new());
        }

        let mut result = String::new();
        let cp1252 = encoding_for_codepage(1252);

        for piece in &self.pieces {
            // Skip pieces entirely before or after our range
            if piece.cp_end <= cp_start || piece.cp_start >= cp_end {
                continue;
            }

            // Clip to requested range
            let clip_start = piece.cp_start.max(cp_start);
            let clip_end = piece.cp_end.min(cp_end);
            let offset_within_piece = clip_start - piece.cp_start;
            let char_count = clip_end - clip_start;

            if piece.is_compressed {
                // CP1252: 1 byte per character
                let byte_start = (piece.byte_offset as usize)
                    .checked_add(offset_within_piece as usize)
                    .ok_or_else(|| Error::new("compressed byte offset overflow"))?;
                let byte_end = byte_start
                    .checked_add(char_count as usize)
                    .ok_or_else(|| Error::new("compressed byte end overflow"))?;

                let bytes = word_doc_stream.get(byte_start..byte_end).ok_or_else(|| {
                    Error::new(format!(
                        "piece byte range {byte_start}..{byte_end} exceeds WordDocument stream length {}",
                        word_doc_stream.len()
                    ))
                })?;

                let (decoded, _, _) = cp1252.decode(bytes);
                result.push_str(&decoded);
            } else {
                // UTF-16LE: 2 bytes per character
                let byte_start = (piece.byte_offset as usize)
                    .checked_add(
                        (offset_within_piece as usize)
                            .checked_mul(2)
                            .ok_or_else(|| Error::new("UTF-16 offset overflow"))?,
                    )
                    .ok_or_else(|| Error::new("UTF-16 byte start overflow"))?;
                let byte_end = byte_start
                    .checked_add(
                        (char_count as usize)
                            .checked_mul(2)
                            .ok_or_else(|| Error::new("UTF-16 byte count overflow"))?,
                    )
                    .ok_or_else(|| Error::new("UTF-16 byte end overflow"))?;

                let bytes = word_doc_stream.get(byte_start..byte_end).ok_or_else(|| {
                    Error::new(format!(
                        "piece byte range {byte_start}..{byte_end} exceeds WordDocument stream length {}",
                        word_doc_stream.len()
                    ))
                })?;

                // Decode UTF-16LE
                let (decoded, _, _) = encoding_rs::UTF_16LE.decode(bytes);
                result.push_str(&decoded);
            }
        }

        Ok(result)
    }

    /// Convert an FKP file-character position to a CP.
    ///
    /// FKP pages store "file character" positions (FCs) that are byte offsets
    /// into the WordDocument stream with encoding metadata. For compressed
    /// pieces, the FC has bit 30 set and stores `byte_offset * 2`. For
    /// uncompressed pieces, the FC is the raw byte offset.
    ///
    /// Returns `None` if the FC does not fall within any piece.
    pub fn fc_to_cp(&self, fc: u32) -> Option<u32> {
        let is_compressed_fc = fc & 0x4000_0000 != 0;
        let raw_fc = fc & 0x3FFF_FFFF;

        for piece in &self.pieces {
            if piece.is_compressed {
                // Compressed piece: FKP FCs are (byte_offset * 2) | 0x40000000
                if !is_compressed_fc {
                    continue;
                }
                // raw_fc = byte_offset * 2, so actual byte pos = raw_fc / 2
                let byte_pos = raw_fc / 2;
                let piece_byte_start = piece.byte_offset;
                let piece_byte_end = piece.byte_offset + (piece.cp_end - piece.cp_start);
                if byte_pos >= piece_byte_start && byte_pos <= piece_byte_end {
                    let offset_in_piece = byte_pos - piece_byte_start;
                    return Some(piece.cp_start + offset_in_piece);
                }
            } else {
                // Uncompressed piece: FKP FCs are raw byte offsets (no bit 30)
                if is_compressed_fc {
                    continue;
                }
                let piece_byte_start = piece.byte_offset;
                let piece_byte_end = piece.byte_offset + (piece.cp_end - piece.cp_start) * 2;
                if raw_fc >= piece_byte_start && raw_fc <= piece_byte_end {
                    let byte_offset_in_piece = raw_fc - piece_byte_start;
                    return Some(piece.cp_start + byte_offset_in_piece / 2);
                }
            }
        }
        None
    }

    /// Parse the piece table, falling back to a synthetic single piece for fast-save files.
    ///
    /// Fast-save .doc files produced by some applications leave the CLX empty
    /// (lcbClx == 0) or omit the Pcdt section entirely. In that case the entire
    /// document text is stored contiguously in the WordDocument stream starting at
    /// `fc_min`, encoded as UTF-16LE.
    ///
    /// When the CLX is empty or has no Pcdt marker, this function emits a
    /// diagnostic warning and constructs a single synthetic piece covering
    /// [0, total_ccp) with `is_compressed = false` (UTF-16LE). All other callers
    /// are unaffected: when CLX is present and valid, `parse` is called normally.
    ///
    /// `total_ccp` should be `ccpText + ccpFtn + ccpHdd + ccpAtn + ccpEdn + 1`.
    pub fn parse_with_fast_save_fallback(
        table_stream: &[u8],
        fc_clx: u32,
        lcb_clx: u32,
        fc_min: u32,
        total_ccp: u32,
        diag: &dyn DiagnosticsSink,
    ) -> Result<Self> {
        // If lcbClx is zero the Table stream has no CLX data at all.
        if lcb_clx == 0 {
            diag.warning(Warning::new(
                "DocFastSaveFallback",
                "CLX is empty (lcbClx == 0), using fast-save text extraction fallback",
            ));
            return Ok(Self::synthetic_fast_save(fc_min, total_ccp));
        }

        // Attempt normal parse. If it fails because the CLX has no Pcdt marker,
        // treat it as a fast-save file and fall back. Re-raise other errors.
        match Self::parse(table_stream, fc_clx, lcb_clx) {
            Ok(pt) => Ok(pt),
            Err(ref e)
                if e.to_string().contains("Pcdt")
                    || e.to_string().contains("CLX data is empty") =>
            {
                diag.warning(Warning::new(
                    "DocFastSaveFallback",
                    format!(
                        "CLX has no Pcdt section ({}), using fast-save text extraction fallback",
                        e
                    ),
                ));
                Ok(Self::synthetic_fast_save(fc_min, total_ccp))
            }
            Err(e) => Err(e),
        }
    }

    /// Build a synthetic single-piece PieceTable for fast-save files.
    ///
    /// Covers CP range [0, total_ccp) with a single UTF-16LE piece whose
    /// byte offset in the WordDocument stream is `fc_min`.
    fn synthetic_fast_save(fc_min: u32, total_ccp: u32) -> Self {
        let pieces = if total_ccp == 0 {
            Vec::new()
        } else {
            vec![Piece {
                cp_start: 0,
                cp_end: total_ccp,
                byte_offset: fc_min,
                is_compressed: false,
            }]
        };
        PieceTable { pieces }
    }

    /// Total character count covered by all pieces.
    #[cfg(test)]
    fn total_cps(&self) -> u32 {
        self.pieces
            .last()
            .map(|p| p.cp_end)
            .unwrap_or(0)
            .saturating_sub(self.pieces.first().map(|p| p.cp_start).unwrap_or(0))
    }

    /// Number of pieces in the table.
    #[cfg(test)]
    fn piece_count(&self) -> usize {
        self.pieces.len()
    }
}

/// Read a u32 from a byte slice at a given offset (for PlcPcd parsing).
fn read_plc_u32(data: &[u8], offset: usize, field: &str) -> Result<u32> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| Error::new(format!("offset overflow reading {field}")))?;
    let bytes = data.get(offset..end).ok_or_else(|| {
        Error::new(format!(
            "truncated PlcPcd: need 4 bytes for {field} at offset {offset}, have {} total",
            data.len()
        ))
    })?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ResultExt;
    use crate::test_util::*;

    #[test]
    fn single_piece_compressed() {
        let text = b"Hello World";
        let clx = build_clx(&[(0, text.len() as u32, text, true)]);

        // Build table stream with CLX at offset 0
        let table_stream = clx.clone();
        let pt = PieceTable::parse(&table_stream, 0, clx.len() as u32)
            .context("parsing piece table")
            .unwrap();

        assert_eq!(pt.piece_count(), 1);
        assert_eq!(pt.total_cps(), text.len() as u32);

        // WordDocument stream: text at offset 0
        let result = pt
            .assemble_text(text, 0, text.len() as u32)
            .context("assembling text")
            .unwrap();
        assert_eq!(result, "Hello World");
    }

    #[test]
    fn single_piece_utf16() {
        let text_str = "Hello";
        let utf16_bytes: Vec<u8> = text_str
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();

        let clx = build_clx(&[(0, text_str.len() as u32, &utf16_bytes, false)]);
        let table_stream = clx.clone();
        let pt = PieceTable::parse(&table_stream, 0, clx.len() as u32).unwrap();

        assert_eq!(pt.piece_count(), 1);

        let result = pt
            .assemble_text(&utf16_bytes, 0, text_str.len() as u32)
            .unwrap();
        assert_eq!(result, "Hello");
    }

    #[test]
    fn mixed_encoding_pieces() {
        // Piece 1: "Hello " compressed (CP1252), Piece 2: "World" UTF-16LE
        let text1 = b"Hello ";
        let text2_str = "World";
        let text2_utf16: Vec<u8> = text2_str
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();

        // Build WordDocument stream: compressed at offset 0, UTF-16 after that
        let mut word_doc = Vec::new();
        word_doc.extend_from_slice(text1);
        let utf16_offset = word_doc.len();
        word_doc.extend_from_slice(&text2_utf16);

        let clx = build_clx_with_offsets(&[
            (0, 6, 0, true),                     // "Hello " at byte offset 0
            (6, 11, utf16_offset as u32, false), // "World" at utf16_offset
        ]);
        let table_stream = clx.clone();
        let pt = PieceTable::parse(&table_stream, 0, clx.len() as u32).unwrap();

        assert_eq!(pt.piece_count(), 2);

        let result = pt.assemble_text(&word_doc, 0, 11).unwrap();
        assert_eq!(result, "Hello World");
    }

    #[test]
    fn empty_document() {
        // Zero pieces, just one CP boundary at 0
        let clx = build_clx(&[]);
        let table_stream = clx.clone();
        let pt = PieceTable::parse(&table_stream, 0, clx.len() as u32).unwrap();

        assert_eq!(pt.piece_count(), 0);

        let result = pt.assemble_text(&[], 0, 0).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn cp_range_extraction() {
        // Full text "Hello World", extract just "World" (CP 6..11)
        let text = b"Hello World";
        let clx = build_clx(&[(0, text.len() as u32, text, true)]);
        let table_stream = clx.clone();
        let pt = PieceTable::parse(&table_stream, 0, clx.len() as u32).unwrap();

        let result = pt.assemble_text(text, 6, 11).unwrap();
        assert_eq!(result, "World");
    }

    #[test]
    fn piece_past_word_doc_end_errors() {
        // Piece claims byte_offset 1000, but WordDocument is only 10 bytes
        let clx = build_clx_with_offsets(&[(0, 5, 1000, true)]);
        let table_stream = clx.clone();
        let pt = PieceTable::parse(&table_stream, 0, clx.len() as u32).unwrap();

        let word_doc = vec![0u8; 10];
        let err = pt.assemble_text(&word_doc, 0, 5).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("exceeds"), "got: {msg}");
    }

    #[test]
    fn prc_entries_skipped() {
        // Build CLX with a Prc entry before the Pcdt
        let text = b"Test";
        let inner_clx = build_clx(&[(0, text.len() as u32, text, true)]);

        // Prepend a Prc entry: 0x01 + cbGrpprl (u16) + grpprl data
        let mut clx_with_prc = Vec::new();
        clx_with_prc.push(0x01); // Prc marker
        let grpprl = vec![0xAA, 0xBB, 0xCC]; // 3 bytes of dummy data
        clx_with_prc.extend_from_slice(&(grpprl.len() as u16).to_le_bytes());
        clx_with_prc.extend_from_slice(&grpprl);
        clx_with_prc.extend_from_slice(&inner_clx);

        let table_stream = clx_with_prc.clone();
        let pt = PieceTable::parse(&table_stream, 0, clx_with_prc.len() as u32).unwrap();

        assert_eq!(pt.piece_count(), 1);
        let result = pt.assemble_text(text, 0, text.len() as u32).unwrap();
        assert_eq!(result, "Test");
    }

    #[test]
    fn cross_piece_boundary_extraction() {
        // Two pieces: "Hel" (CP 0..3) and "lo" (CP 3..5)
        let text1 = b"Hel";
        let text2 = b"lo";
        let mut word_doc = Vec::new();
        word_doc.extend_from_slice(text1);
        let offset2 = word_doc.len();
        word_doc.extend_from_slice(text2);

        let clx = build_clx_with_offsets(&[(0, 3, 0, true), (3, 5, offset2 as u32, true)]);
        let table_stream = clx.clone();
        let pt = PieceTable::parse(&table_stream, 0, clx.len() as u32).unwrap();

        // Extract full range spanning both pieces
        let result = pt.assemble_text(&word_doc, 0, 5).unwrap();
        assert_eq!(result, "Hello");

        // Extract partial range within second piece
        let result = pt.assemble_text(&word_doc, 3, 5).unwrap();
        assert_eq!(result, "lo");

        // Extract partial range clipping into both pieces
        let result = pt.assemble_text(&word_doc, 1, 4).unwrap();
        assert_eq!(result, "ell");
    }

    #[test]
    fn clx_at_nonzero_offset() {
        let text = b"Offset";
        let clx = build_clx(&[(0, text.len() as u32, text, true)]);

        // Put some padding before the CLX in the table stream
        let mut table_stream = vec![0xAA; 100];
        let fc_clx = table_stream.len() as u32;
        table_stream.extend_from_slice(&clx);

        let pt = PieceTable::parse(&table_stream, fc_clx, clx.len() as u32).unwrap();
        let result = pt.assemble_text(text, 0, text.len() as u32).unwrap();
        assert_eq!(result, "Offset");
    }

    #[test]
    fn bad_pcdt_marker_rejected() {
        // Build CLX data that starts with 0xFF instead of 0x02
        let bad_clx = vec![0xFF, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00];
        let err = PieceTable::parse(&bad_clx, 0, bad_clx.len() as u32).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("Pcdt marker"), "got: {msg}");
    }

    #[test]
    fn cp1252_special_chars() {
        // Test CP1252 characters outside ASCII (e.g. 0xE9 = e-acute)
        let text = &[0xE9u8]; // e with acute in CP1252
        let clx = build_clx(&[(0, 1, text, true)]);
        let table_stream = clx.clone();
        let pt = PieceTable::parse(&table_stream, 0, clx.len() as u32).unwrap();

        let result = pt.assemble_text(text, 0, 1).unwrap();
        assert_eq!(result, "\u{00E9}"); // e-acute in UTF-8
    }

    // ---------------------------------------------------------------
    // Fast-save fallback tests
    // ---------------------------------------------------------------

    /// A DiagnosticsSink that collects warning kind strings for assertion.
    struct CaptureDiag {
        warnings: std::sync::Mutex<Vec<String>>,
    }
    impl CaptureDiag {
        fn new() -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                warnings: std::sync::Mutex::new(Vec::new()),
            })
        }
        fn warnings(&self) -> Vec<String> {
            self.warnings.lock().unwrap().clone()
        }
    }
    impl udoc_core::diagnostics::DiagnosticsSink for CaptureDiag {
        fn warning(&self, w: udoc_core::diagnostics::Warning) {
            self.warnings
                .lock()
                .unwrap()
                .push(w.kind.as_str().to_string());
        }
    }

    #[test]
    fn fast_save_fallback_activates_when_lcb_clx_is_zero() {
        // lcbClx == 0: table stream is empty, fallback must activate.
        let diag = CaptureDiag::new();
        let pt = PieceTable::parse_with_fast_save_fallback(
            &[],   // empty table stream
            0,     // fc_clx
            0,     // lcb_clx = 0  -> triggers fallback
            0x100, // fc_min (text starts at offset 256)
            5,     // total_ccp
            &*diag,
        )
        .unwrap();

        // One synthetic piece covering [0, 5) at byte_offset 0x100, UTF-16LE
        assert_eq!(pt.pieces.len(), 1);
        assert_eq!(pt.pieces[0].cp_start, 0);
        assert_eq!(pt.pieces[0].cp_end, 5);
        assert_eq!(pt.pieces[0].byte_offset, 0x100);
        assert!(!pt.pieces[0].is_compressed);

        // Diagnostic warning emitted
        let warnings = diag.warnings();
        assert_eq!(warnings.len(), 1, "expected one warning, got: {warnings:?}");
        assert_eq!(warnings[0], "DocFastSaveFallback");
    }

    #[test]
    fn fast_save_fallback_activates_when_no_pcdt_marker() {
        // CLX data present but missing Pcdt marker (0xFF instead of 0x02).
        let bad_clx = vec![0xFF, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00];
        let diag = CaptureDiag::new();
        let pt = PieceTable::parse_with_fast_save_fallback(
            &bad_clx,
            0,
            bad_clx.len() as u32,
            0x200,
            10,
            &*diag,
        )
        .unwrap();

        assert_eq!(pt.pieces.len(), 1);
        assert_eq!(pt.pieces[0].cp_end, 10);
        assert_eq!(pt.pieces[0].byte_offset, 0x200);
        assert!(!pt.pieces[0].is_compressed);

        let warnings = diag.warnings();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0], "DocFastSaveFallback");
    }

    #[test]
    fn fast_save_fallback_text_extraction() {
        // Build a UTF-16LE stream that the synthetic piece should decode.
        let text_str = "Hello";
        let utf16_bytes: Vec<u8> = text_str
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();

        // Fake WordDocument stream: some FIB bytes, then the UTF-16LE text.
        let fc_min: u32 = 50;
        let mut word_doc = vec![0u8; fc_min as usize];
        word_doc.extend_from_slice(&utf16_bytes);

        let diag = CaptureDiag::new();
        let total_ccp = text_str.len() as u32 + 1; // +1 for terminator
        let pt = PieceTable::parse_with_fast_save_fallback(
            &[], // empty table stream
            0,
            0, // lcbClx = 0
            fc_min,
            total_ccp,
            &*diag,
        )
        .unwrap();

        // Assemble the body text (cp 0..text_str.len())
        let result = pt
            .assemble_text(&word_doc, 0, text_str.len() as u32)
            .unwrap();
        assert_eq!(result, "Hello");
    }

    #[test]
    fn fast_save_fallback_does_not_activate_for_valid_clx() {
        // Normal CLX: fallback must NOT activate, no warnings emitted.
        let text = b"ValidDoc";
        let clx = build_clx(&[(0, text.len() as u32, text, true)]);
        let diag = CaptureDiag::new();

        let pt = PieceTable::parse_with_fast_save_fallback(
            &clx,
            0,
            clx.len() as u32,
            0, // fc_min (not used for valid CLX)
            text.len() as u32,
            &*diag,
        )
        .unwrap();

        // Normal parse: multiple-pieces or correct piece count, no warnings.
        assert_eq!(pt.piece_count(), 1);
        assert!(diag.warnings().is_empty(), "should not warn for valid CLX");

        let result = pt.assemble_text(text, 0, text.len() as u32).unwrap();
        assert_eq!(result, "ValidDoc");
    }

    #[test]
    fn fast_save_fallback_zero_total_ccp_produces_empty_table() {
        // Fast-save with total_ccp == 0: empty document, no pieces.
        let diag = CaptureDiag::new();
        let pt = PieceTable::parse_with_fast_save_fallback(&[], 0, 0, 0x100, 0, &*diag).unwrap();

        assert_eq!(pt.pieces.len(), 0);
        let result = pt.assemble_text(&[], 0, 0).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn fast_save_story_boundaries_work_with_synthetic_piece() {
        // Verify that story boundary extraction works when using the synthetic piece.
        // Layout: body "Hi\r" (3 cp) + footnote "Fn\r" (3 cp), total_ccp = 7 (6+1).
        let body_utf16: Vec<u8> = "Hi\r"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        let ftn_utf16: Vec<u8> = "Fn\r"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();

        let fc_min: u32 = 20;
        let mut word_doc = vec![0u8; fc_min as usize];
        word_doc.extend_from_slice(&body_utf16);
        word_doc.extend_from_slice(&ftn_utf16);

        let diag = CaptureDiag::new();
        let total_ccp: u32 = 7; // 3 + 3 + 1 terminator
        let pt = PieceTable::parse_with_fast_save_fallback(&[], 0, 0, fc_min, total_ccp, &*diag)
            .unwrap();

        // Body: cp 0..3, footnote: cp 3..6
        let body_text = pt.assemble_text(&word_doc, 0, 3).unwrap();
        assert_eq!(body_text, "Hi\r");

        let ftn_text = pt.assemble_text(&word_doc, 3, 6).unwrap();
        assert_eq!(ftn_text, "Fn\r");
    }
}

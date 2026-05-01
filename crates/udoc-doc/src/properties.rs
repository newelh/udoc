//! Paragraph and character property resolution via FKP (Formatted Disk Pages).
//!
//! Parses PlcfBtePapx and PlcfBteChpx from the Table stream to resolve
//! paragraph-level (istd, in-table, row-end) and character-level (bold,
//! italic, font size, font index) properties using SPRM opcodes.
//!
//! Reference: MS-DOC 2.8.26 (PlcBtePapx), 2.8.25 (PlcBteChpx),
//!            2.9.252 (Sprm), 2.8.34 (PapxFkp), 2.8.9 (ChpxFkp)

use crate::error::{Error, Result, ResultExt};
use crate::fib::Fib;
use crate::piece_table::PieceTable;

/// FKP page size in bytes (always 512).
const FKP_PAGE_SIZE: usize = 512;

/// Safety limit on the number of BTE entries we'll process.
const MAX_BTE_ENTRIES: usize = 100_000;

/// Safety limit on crun within a single FKP page.
const MAX_CRUN: usize = 127; // 511 / 4 = 127 max

// -- SPRM opcodes we care about --

/// sprmPFInTable: paragraph is inside a table. u8 operand, 1 = true.
const SPRM_PF_IN_TABLE: u16 = 0x2416;

/// sprmPFTtp: paragraph is a table row terminator. u8 operand, 1 = true.
const SPRM_PF_TTP: u16 = 0x2417;

/// sprmCFBold: character bold. u8 operand, 1 = bold.
const SPRM_CF_BOLD: u16 = 0x0835;

/// sprmCFItalic: character italic. u8 operand, 1 = italic.
const SPRM_CF_ITALIC: u16 = 0x0836;

/// sprmCHps: character font size in half-points. u16 operand.
const SPRM_C_HPS: u16 = 0x4A43;

/// sprmCRgFtc0: character font index (ASCII). u16 operand.
const SPRM_C_RG_FTC0: u16 = 0x4A4F;

/// Paragraph-level properties for a CP range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParagraphProperties {
    /// Starting character position (inclusive).
    pub cp_start: u32,
    /// Ending character position (exclusive).
    pub cp_end: u32,
    /// Style index (istd) from the PAPX header.
    pub istd: u16,
    /// Whether the paragraph is inside a table.
    pub in_table: bool,
    /// Whether the paragraph is a table row terminator.
    pub table_row_end: bool,
}

/// Character-level properties for a CP range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterProperties {
    /// Starting character position (inclusive).
    pub cp_start: u32,
    /// Ending character position (exclusive).
    pub cp_end: u32,
    /// Bold (None if not specified in this run).
    pub bold: Option<bool>,
    /// Italic (None if not specified in this run).
    pub italic: Option<bool>,
    /// Font size in half-points (None if not specified).
    pub font_size_half_pts: Option<u16>,
    /// Font index into the font table (None if not specified).
    pub font_index: Option<u16>,
}

/// Iterator over Single Property Modifiers (SPRMs) in a grpprl byte sequence.
///
/// Each SPRM has a 2-byte opcode followed by a variable-length operand.
/// The operand size is determined by bits 13-15 (spra) of the opcode.
pub struct SprmIter<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> SprmIter<'a> {
    /// Create a new SPRM iterator over the given byte slice.
    pub fn new(data: &'a [u8]) -> Self {
        SprmIter { data, pos: 0 }
    }
}

impl<'a> Iterator for SprmIter<'a> {
    type Item = (u16, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos + 2 > self.data.len() {
            return None;
        }

        let opcode = u16::from_le_bytes([self.data[self.pos], self.data[self.pos + 1]]);
        self.pos += 2;

        let spra = (opcode >> 13) & 0x07;
        let operand_size = match spra {
            0 | 1 => 1,     // toggle / byte
            2 | 4 | 5 => 2, // word
            3 => 4,         // dword
            7 => 3,         // 3 bytes
            6 => {
                // Variable length: first byte is count, then count bytes
                if self.pos >= self.data.len() {
                    return None;
                }
                let count = self.data[self.pos] as usize;
                // The count byte is part of the operand data we return
                1 + count
            }
            _ => {
                // Unknown spra, can't determine size. Stop iteration.
                return None;
            }
        };

        let operand_end = self.pos + operand_size;
        if operand_end > self.data.len() {
            // Truncated operand, stop iteration gracefully
            return None;
        }

        let operand = &self.data[self.pos..operand_end];
        self.pos = operand_end;

        Some((opcode, operand))
    }
}

/// Parse paragraph properties from PlcfBtePapx via FKP pages.
///
/// Reads the PlcfBtePapx PLC from the table stream, resolves each BTE to an
/// FKP page in the WordDocument stream, then extracts paragraph boundaries
/// and SPRM properties, converting FKP FCs to CPs via the piece table.
pub fn parse_paragraph_properties(
    table_stream: &[u8],
    word_doc_stream: &[u8],
    fib: &Fib,
    piece_table: &PieceTable,
) -> Result<Vec<ParagraphProperties>> {
    let bte_entries = parse_plcf_bte(
        table_stream,
        fib.fc_plcf_bte_papx as usize,
        fib.lcb_plcf_bte_papx as usize,
    )
    .context("parsing PlcfBtePapx")?;

    let mut results = Vec::new();

    for (bte_idx, page_number) in bte_entries.iter().enumerate() {
        let page_offset = (*page_number as usize)
            .checked_mul(FKP_PAGE_SIZE)
            .ok_or_else(|| Error::new("PAPX FKP page offset overflow"))?;

        parse_papx_fkp(word_doc_stream, page_offset, piece_table, &mut results).context(
            format!("parsing PapxFkp page {bte_idx} at offset 0x{page_offset:X}"),
        )?;
    }

    // Sort by cp_start for consistent ordering
    results.sort_by_key(|p| p.cp_start);

    Ok(results)
}

/// Parse character properties from PlcfBteChpx via FKP pages.
///
/// Similar to paragraph properties but reads character formatting (bold,
/// italic, font size, font index).
pub fn parse_character_properties(
    table_stream: &[u8],
    word_doc_stream: &[u8],
    fib: &Fib,
    piece_table: &PieceTable,
) -> Result<Vec<CharacterProperties>> {
    let bte_entries = parse_plcf_bte(
        table_stream,
        fib.fc_plcf_bte_chpx as usize,
        fib.lcb_plcf_bte_chpx as usize,
    )
    .context("parsing PlcfBteChpx")?;

    let mut results = Vec::new();

    for (bte_idx, page_number) in bte_entries.iter().enumerate() {
        let page_offset = (*page_number as usize)
            .checked_mul(FKP_PAGE_SIZE)
            .ok_or_else(|| Error::new("CHPX FKP page offset overflow"))?;

        parse_chpx_fkp(word_doc_stream, page_offset, piece_table, &mut results).context(
            format!("parsing ChpxFkp page {bte_idx} at offset 0x{page_offset:X}"),
        )?;
    }

    // Sort by cp_start
    results.sort_by_key(|c| c.cp_start);

    Ok(results)
}

/// Parse a PlcfBte (PLC of BTE entries) from the table stream.
///
/// Returns a vector of page numbers. Each page number * 512 = offset of
/// the corresponding FKP page in the WordDocument stream.
///
/// PLC format: (n+1) FCs (u32 each) + n BTEs (4 bytes each = page number u32).
/// Total = (n+1)*4 + n*4 = 4 + 8*n, so n = (lcb - 4) / 8.
fn parse_plcf_bte(table_stream: &[u8], offset: usize, size: usize) -> Result<Vec<u32>> {
    if size == 0 {
        return Ok(Vec::new());
    }

    if size < 4 {
        return Err(Error::new(format!(
            "PlcfBte too small: {size} bytes, need at least 4"
        )));
    }

    let end = offset
        .checked_add(size)
        .ok_or_else(|| Error::new("PlcfBte end offset overflow"))?;

    let data = table_stream.get(offset..end).ok_or_else(|| {
        Error::new(format!(
            "PlcfBte out of bounds: offset {offset}..{end}, table stream length {}",
            table_stream.len()
        ))
    })?;

    let remainder = size - 4;
    if !remainder.is_multiple_of(8) {
        return Err(Error::new(format!(
            "PlcfBte size mismatch: (size - 4) = {remainder} is not divisible by 8"
        )));
    }

    let n = remainder / 8;
    if n > MAX_BTE_ENTRIES {
        return Err(Error::new(format!(
            "too many BTE entries: {n}, maximum is {MAX_BTE_ENTRIES}"
        )));
    }

    // BTE entries start after (n+1) FCs
    let bte_base = (n + 1) * 4;
    let mut pages = Vec::with_capacity(n);

    for i in 0..n {
        let bte_off = bte_base + i * 4;
        let page_num = read_u32_at(data, bte_off, "BTE page number")?;
        pages.push(page_num);
    }

    Ok(pages)
}

/// Parse a PapxFkp page from the WordDocument stream.
///
/// PapxFkp layout (512 bytes):
/// - byte 511: crun (number of paragraph runs)
/// - bytes 0..(crun+1)*4: rgfc (FC boundaries, u32 each)
/// - after rgfc: crun BX entries (13 bytes each for PAPX):
///   - byte 0: bxOffset (multiply by 2 for byte position in page)
///   - bytes 1-12: reserved/padding
/// - PAPX data at the positions indicated by bxOffset
fn parse_papx_fkp(
    word_doc_stream: &[u8],
    page_offset: usize,
    piece_table: &PieceTable,
    results: &mut Vec<ParagraphProperties>,
) -> Result<()> {
    let page_end = page_offset
        .checked_add(FKP_PAGE_SIZE)
        .ok_or_else(|| Error::new("FKP page end overflow"))?;

    let page = word_doc_stream.get(page_offset..page_end).ok_or_else(|| {
        Error::new(format!(
            "PapxFkp page out of bounds: offset {page_offset}..{page_end}, \
             WordDocument stream length {}",
            word_doc_stream.len()
        ))
    })?;

    let crun = page[511] as usize;
    if crun == 0 {
        return Ok(());
    }
    if crun > MAX_CRUN {
        return Err(Error::new(format!(
            "PapxFkp crun too large: {crun}, maximum is {MAX_CRUN}"
        )));
    }

    // Read FC boundaries: (crun+1) u32 values
    let rgfc_end = (crun + 1) * 4;
    if rgfc_end > 511 {
        return Err(Error::new(format!(
            "PapxFkp rgfc exceeds page: need {} bytes for {} FCs",
            rgfc_end,
            crun + 1
        )));
    }

    // BX entries start after rgfc. Each BX for PAPX is 13 bytes:
    // 1 byte bxOffset + 12 bytes reserved.
    let bx_base = rgfc_end;

    for i in 0..crun {
        let fc_start = read_u32_at(page, i * 4, "PapxFkp FC[start]")?;
        let fc_end = read_u32_at(page, (i + 1) * 4, "PapxFkp FC[end]")?;

        // Convert FCs to CPs
        let cp_start = match piece_table.fc_to_cp(fc_start) {
            Some(cp) => cp,
            None => continue, // FC not in any piece, skip
        };
        let cp_end = match piece_table.fc_to_cp(fc_end) {
            Some(cp) => cp,
            None => continue,
        };

        if cp_start >= cp_end {
            continue;
        }

        // Read BX entry: byte at bx_base + i * 13
        let bx_off = bx_base + i * 13;
        if bx_off >= 511 {
            continue; // BX entry would be past the crun byte
        }
        let bx_offset = page[bx_off] as usize * 2;
        if bx_offset == 0 {
            // No PAPX data for this run, use defaults
            results.push(ParagraphProperties {
                cp_start,
                cp_end,
                istd: 0,
                in_table: false,
                table_row_end: false,
            });
            continue;
        }

        // Parse PAPX at bx_offset within the page
        let (istd, in_table, table_row_end) = parse_papx_data(page, bx_offset)?;

        results.push(ParagraphProperties {
            cp_start,
            cp_end,
            istd,
            in_table,
            table_row_end,
        });
    }

    Ok(())
}

/// Parse PAPX data from within an FKP page.
///
/// PAPX format:
/// - byte 0: cb (byte count). If cb == 0, the next byte is the low byte of a u16 cb.
///   The actual cb value includes the istd (2 bytes), so sprm data = cb*2 - 2 bytes
///   (when cb from single byte) or cb - 2 bytes (when cb from u16 fallback).
///   Actually: single-byte cb means total grpprl size is cb*2 - 2 bytes after istd.
///   Wait, let me re-read the spec more carefully.
///
/// Per MS-DOC: if cb is non-zero, total PAPX size = cb * 2 bytes (including istd).
/// If cb == 0, next two bytes are a u16 size (total PAPX size including istd).
/// In both cases, first 2 bytes of PAPX content are istd, rest are SPRMs.
fn parse_papx_data(page: &[u8], offset: usize) -> Result<(u16, bool, bool)> {
    if offset >= page.len() {
        return Ok((0, false, false));
    }

    let cb = page[offset] as usize;
    let (istd_offset, sprm_len) = if cb == 0 {
        // Fallback: next byte is part of a u16 size
        if offset + 3 > page.len() {
            return Ok((0, false, false));
        }
        // Actually per spec: if first byte is 0, then a u16 at offset+1 gives
        // the total byte count of the PAPX (including istd).
        let total = u16::from_le_bytes([page[offset + 1], page[offset + 2]]) as usize;
        let istd_off = offset + 3;
        // sprm data length = total - 2 (for istd)
        let sprm_sz = total.saturating_sub(2);
        (istd_off, sprm_sz)
    } else {
        // cb is non-zero: total PAPX bytes (including istd) = cb * 2
        let total = cb * 2;
        let istd_off = offset + 1;
        // sprm data length = total - 2 (for istd)
        let sprm_sz = total.saturating_sub(2);
        (istd_off, sprm_sz)
    };

    // Read istd (2 bytes)
    if istd_offset + 2 > page.len() {
        return Ok((0, false, false));
    }
    let istd = u16::from_le_bytes([page[istd_offset], page[istd_offset + 1]]);

    // SPRMs start after istd
    let sprm_start = istd_offset + 2;
    let sprm_end = sprm_start + sprm_len;
    let sprm_end = sprm_end.min(page.len());

    if sprm_start >= sprm_end {
        return Ok((istd, false, false));
    }

    let sprm_data = &page[sprm_start..sprm_end];

    let mut in_table = false;
    let mut table_row_end = false;

    for (opcode, operand) in SprmIter::new(sprm_data) {
        match opcode {
            SPRM_PF_IN_TABLE if !operand.is_empty() => {
                in_table = operand[0] != 0;
            }
            SPRM_PF_TTP if !operand.is_empty() => {
                table_row_end = operand[0] != 0;
            }
            _ => {} // Skip SPRMs we don't care about
        }
    }

    Ok((istd, in_table, table_row_end))
}

/// Parse a ChpxFkp page from the WordDocument stream.
///
/// ChpxFkp layout (512 bytes):
/// - byte 511: crun
/// - bytes 0..(crun+1)*4: rgfc (FC boundaries, u32 each)
/// - after rgfc: crun BX entries (1 byte each for CHPX):
///   - byte 0: bxOffset (multiply by 2 for byte position in page)
/// - CHPX data at the positions indicated by bxOffset
fn parse_chpx_fkp(
    word_doc_stream: &[u8],
    page_offset: usize,
    piece_table: &PieceTable,
    results: &mut Vec<CharacterProperties>,
) -> Result<()> {
    let page_end = page_offset
        .checked_add(FKP_PAGE_SIZE)
        .ok_or_else(|| Error::new("CHPX FKP page end overflow"))?;

    let page = word_doc_stream.get(page_offset..page_end).ok_or_else(|| {
        Error::new(format!(
            "ChpxFkp page out of bounds: offset {page_offset}..{page_end}, \
             WordDocument stream length {}",
            word_doc_stream.len()
        ))
    })?;

    let crun = page[511] as usize;
    if crun == 0 {
        return Ok(());
    }
    if crun > MAX_CRUN {
        return Err(Error::new(format!(
            "ChpxFkp crun too large: {crun}, maximum is {MAX_CRUN}"
        )));
    }

    let rgfc_end = (crun + 1) * 4;
    if rgfc_end > 511 {
        return Err(Error::new(format!(
            "ChpxFkp rgfc exceeds page: need {} bytes for {} FCs",
            rgfc_end,
            crun + 1
        )));
    }

    // BX entries for CHPX are 1 byte each (just the offset byte)
    let bx_base = rgfc_end;

    for i in 0..crun {
        let fc_start = read_u32_at(page, i * 4, "ChpxFkp FC[start]")?;
        let fc_end = read_u32_at(page, (i + 1) * 4, "ChpxFkp FC[end]")?;

        let cp_start = match piece_table.fc_to_cp(fc_start) {
            Some(cp) => cp,
            None => continue,
        };
        let cp_end = match piece_table.fc_to_cp(fc_end) {
            Some(cp) => cp,
            None => continue,
        };

        if cp_start >= cp_end {
            continue;
        }

        // BX entry: single byte at bx_base + i
        let bx_off = bx_base + i;
        if bx_off >= 511 {
            continue;
        }
        let bx_offset = page[bx_off] as usize * 2;
        if bx_offset == 0 {
            // No CHPX data, use defaults
            results.push(CharacterProperties {
                cp_start,
                cp_end,
                bold: None,
                italic: None,
                font_size_half_pts: None,
                font_index: None,
            });
            continue;
        }

        let chpx = parse_chpx_data(page, bx_offset)?;
        results.push(CharacterProperties {
            cp_start,
            cp_end,
            bold: chpx.bold,
            italic: chpx.italic,
            font_size_half_pts: chpx.font_size_half_pts,
            font_index: chpx.font_index,
        });
    }

    Ok(())
}

/// Intermediate result from parsing CHPX sprm data (avoids complex tuple return).
struct ChpxResult {
    bold: Option<bool>,
    italic: Option<bool>,
    font_size_half_pts: Option<u16>,
    font_index: Option<u16>,
}

impl ChpxResult {
    fn empty() -> Self {
        ChpxResult {
            bold: None,
            italic: None,
            font_size_half_pts: None,
            font_index: None,
        }
    }
}

/// Parse CHPX data from within an FKP page.
///
/// CHPX format: 1 byte cb (byte count of grpprl), then cb bytes of SPRMs.
fn parse_chpx_data(page: &[u8], offset: usize) -> Result<ChpxResult> {
    if offset >= page.len() {
        return Ok(ChpxResult::empty());
    }

    let cb = page[offset] as usize;
    if cb == 0 {
        return Ok(ChpxResult::empty());
    }

    let sprm_start = offset + 1;
    let sprm_end = (sprm_start + cb).min(page.len());
    if sprm_start >= sprm_end {
        return Ok(ChpxResult::empty());
    }

    let sprm_data = &page[sprm_start..sprm_end];

    let mut result = ChpxResult::empty();

    for (opcode, operand) in SprmIter::new(sprm_data) {
        match opcode {
            SPRM_CF_BOLD if !operand.is_empty() => {
                result.bold = Some(operand[0] != 0);
            }
            SPRM_CF_ITALIC if !operand.is_empty() => {
                result.italic = Some(operand[0] != 0);
            }
            SPRM_C_HPS if operand.len() >= 2 => {
                result.font_size_half_pts = Some(u16::from_le_bytes([operand[0], operand[1]]));
            }
            SPRM_C_RG_FTC0 if operand.len() >= 2 => {
                result.font_index = Some(u16::from_le_bytes([operand[0], operand[1]]));
            }
            _ => {}
        }
    }

    Ok(result)
}

/// Read a little-endian u32 from a byte slice at a given offset.
fn read_u32_at(data: &[u8], offset: usize, field: &str) -> Result<u32> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| Error::new(format!("offset overflow reading {field}")))?;
    let bytes = data.get(offset..end).ok_or_else(|| {
        Error::new(format!(
            "truncated data: need 4 bytes for {field} at offset {offset}, have {} total",
            data.len()
        ))
    })?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::*;

    // -- SprmIter tests --

    #[test]
    fn sprm_iter_known_and_unknown() {
        // Build a grpprl with:
        // 1) sprmCFBold (0x0835) with operand 0x01 (bold=true)
        //    spra = (0x0835 >> 13) & 7 = 0, so 1-byte operand
        // 2) An unknown sprm 0x0837 with 1-byte operand 0xFF
        //    spra = 0, so 1-byte operand
        // 3) sprmCFItalic (0x0836) with operand 0x01
        let mut data = Vec::new();
        data.extend_from_slice(&0x0835u16.to_le_bytes()); // bold opcode
        data.push(0x01); // bold = true
        data.extend_from_slice(&0x0837u16.to_le_bytes()); // unknown opcode
        data.push(0xFF); // unknown operand
        data.extend_from_slice(&0x0836u16.to_le_bytes()); // italic opcode
        data.push(0x01); // italic = true

        let sprms: Vec<(u16, &[u8])> = SprmIter::new(&data).collect();
        assert_eq!(sprms.len(), 3);
        assert_eq!(sprms[0].0, 0x0835);
        assert_eq!(sprms[0].1, &[0x01]);
        assert_eq!(sprms[1].0, 0x0837);
        assert_eq!(sprms[1].1, &[0xFF]);
        assert_eq!(sprms[2].0, 0x0836);
        assert_eq!(sprms[2].1, &[0x01]);
    }

    #[test]
    fn sprm_iter_empty_data() {
        let data: &[u8] = &[];
        let sprms: Vec<(u16, &[u8])> = SprmIter::new(data).collect();
        assert!(sprms.is_empty());
    }

    #[test]
    fn sprm_iter_variable_length() {
        // spra=6 means variable length: first byte = count, then count bytes
        // Build an opcode with spra=6: bits 13-15 = 110 = 6
        // opcode = (6 << 13) | 0x0001 = 0xC001
        let mut data = Vec::new();
        data.extend_from_slice(&0xC001u16.to_le_bytes());
        data.push(3); // count = 3
        data.push(0xAA);
        data.push(0xBB);
        data.push(0xCC);

        let sprms: Vec<(u16, &[u8])> = SprmIter::new(&data).collect();
        assert_eq!(sprms.len(), 1);
        assert_eq!(sprms[0].0, 0xC001);
        assert_eq!(sprms[0].1, &[3, 0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn sprm_iter_truncated_stops_gracefully() {
        // opcode for a 2-byte operand (spra=2: bits 13-15 = 010)
        // opcode = (2 << 13) | 0x0001 = 0x4001
        // But only provide 1 byte of operand instead of 2
        let mut data = Vec::new();
        data.extend_from_slice(&0x4001u16.to_le_bytes());
        data.push(0xAA); // only 1 byte, need 2

        let sprms: Vec<(u16, &[u8])> = SprmIter::new(&data).collect();
        assert!(sprms.is_empty()); // should stop gracefully
    }

    #[test]
    fn sprm_iter_word_and_dword_operands() {
        let mut data = Vec::new();

        // spra=2 (2-byte operand): opcode = (2 << 13) | 0x0001 = 0x4001
        data.extend_from_slice(&0x4001u16.to_le_bytes());
        data.extend_from_slice(&0x1234u16.to_le_bytes());

        // spra=3 (4-byte operand): opcode = (3 << 13) | 0x0002 = 0x6002
        data.extend_from_slice(&0x6002u16.to_le_bytes());
        data.extend_from_slice(&0xDEADBEEFu32.to_le_bytes());

        // spra=7 (3-byte operand): opcode = (7 << 13) | 0x0003 = 0xE003
        data.extend_from_slice(&0xE003u16.to_le_bytes());
        data.push(0x11);
        data.push(0x22);
        data.push(0x33);

        let sprms: Vec<(u16, &[u8])> = SprmIter::new(&data).collect();
        assert_eq!(sprms.len(), 3);

        assert_eq!(sprms[0].0, 0x4001);
        assert_eq!(sprms[0].1, &[0x34, 0x12]);

        assert_eq!(sprms[1].0, 0x6002);
        assert_eq!(sprms[1].1, &[0xEF, 0xBE, 0xAD, 0xDE]);

        assert_eq!(sprms[2].0, 0xE003);
        assert_eq!(sprms[2].1, &[0x11, 0x22, 0x33]);
    }

    // -- Helper to build FKP page and supporting structures --

    /// Build a PapxFkp page (512 bytes) with the given runs.
    ///
    /// Each run is (fc_start, fc_end, istd, sprm_bytes).
    /// The final fc_end of the last run is used as the last FC boundary.
    fn build_papx_fkp(runs: &[(u32, u32, u16, &[u8])]) -> [u8; FKP_PAGE_SIZE] {
        let mut page = [0u8; FKP_PAGE_SIZE];
        let crun = runs.len();
        assert!(crun <= MAX_CRUN);

        page[511] = crun as u8;

        // Write FCs: (crun+1) u32 values
        for (i, &(fc_start, _, _, _)) in runs.iter().enumerate() {
            let off = i * 4;
            page[off..off + 4].copy_from_slice(&fc_start.to_le_bytes());
        }
        if let Some(&(_, fc_end, _, _)) = runs.last() {
            let off = crun * 4;
            page[off..off + 4].copy_from_slice(&fc_end.to_le_bytes());
        }

        // Write BX entries (13 bytes each for PAPX) and PAPX data
        let bx_base = (crun + 1) * 4;
        // Place PAPX data starting from the end of the page, working backwards
        // (before the crun byte at 511)
        let mut papx_pos = 510; // start placing data just before byte 511

        for (i, &(_, _, istd, sprms)) in runs.iter().enumerate() {
            // Build PAPX: cb (1 byte) + istd (2 bytes) + sprms
            let total_with_istd = 2 + sprms.len();
            // cb = ceil(total_with_istd / 2)
            let cb = total_with_istd.div_ceil(2);
            let papx_total = 1 + 2 + sprms.len(); // cb byte + istd + sprms

            papx_pos -= papx_total;
            // Ensure papx_pos is even (bxOffset * 2 must reach it)
            if papx_pos % 2 != 0 {
                papx_pos -= 1;
            }

            page[papx_pos] = cb as u8;
            page[papx_pos + 1..papx_pos + 3].copy_from_slice(&istd.to_le_bytes());
            if !sprms.is_empty() {
                page[papx_pos + 3..papx_pos + 3 + sprms.len()].copy_from_slice(sprms);
            }

            // BX entry: bxOffset = papx_pos / 2
            let bx_off = bx_base + i * 13;
            page[bx_off] = (papx_pos / 2) as u8;
        }

        page
    }

    /// Build a ChpxFkp page (512 bytes) with the given runs.
    ///
    /// Each run is (fc_start, fc_end, sprm_bytes).
    fn build_chpx_fkp(runs: &[(u32, u32, &[u8])]) -> [u8; FKP_PAGE_SIZE] {
        let mut page = [0u8; FKP_PAGE_SIZE];
        let crun = runs.len();
        assert!(crun <= MAX_CRUN);

        page[511] = crun as u8;

        // Write FCs
        for (i, &(fc_start, _, _)) in runs.iter().enumerate() {
            let off = i * 4;
            page[off..off + 4].copy_from_slice(&fc_start.to_le_bytes());
        }
        if let Some(&(_, fc_end, _)) = runs.last() {
            let off = crun * 4;
            page[off..off + 4].copy_from_slice(&fc_end.to_le_bytes());
        }

        // BX entries for CHPX are 1 byte each
        let bx_base = (crun + 1) * 4;
        let mut chpx_pos = 510;

        for (i, &(_, _, sprms)) in runs.iter().enumerate() {
            // CHPX: cb (1 byte) + sprms
            let chpx_total = 1 + sprms.len();
            chpx_pos -= chpx_total;
            if chpx_pos % 2 != 0 {
                chpx_pos -= 1;
            }

            page[chpx_pos] = sprms.len() as u8;
            if !sprms.is_empty() {
                page[chpx_pos + 1..chpx_pos + 1 + sprms.len()].copy_from_slice(sprms);
            }

            // BX entry: 1 byte offset
            let bx_off = bx_base + i;
            page[bx_off] = (chpx_pos / 2) as u8;
        }

        page
    }

    /// Build a PlcfBte from page numbers.
    ///
    /// FCs are synthetic: [0, 1, 2, ...] just to satisfy the PLC structure.
    fn build_plcf_bte(page_numbers: &[u32]) -> Vec<u8> {
        let n = page_numbers.len();
        let mut data = Vec::new();

        // (n+1) FCs
        for i in 0..=n {
            data.extend_from_slice(&(i as u32).to_le_bytes());
        }
        // n BTE entries (page numbers)
        for &pn in page_numbers {
            data.extend_from_slice(&pn.to_le_bytes());
        }

        data
    }

    /// Build a Fib with the specified PlcfBtePapx and PlcfBteChpx offsets.
    fn build_fib_with_bte(
        ccp_text: u32,
        fc_clx: u32,
        lcb_clx: u32,
        fc_papx: u32,
        lcb_papx: u32,
        fc_chpx: u32,
        lcb_chpx: u32,
    ) -> Vec<u8> {
        let mut buf = build_fib(ccp_text, fc_clx, lcb_clx, false);
        // The FIB's fc/lcb pairs are at the end of the buffer.
        // We need to patch pairs 72 (PlcfBtePapx) and 73 (PlcfBteChpx).
        let fc_lcb_base = buf.len() - 74 * 8;

        buf[fc_lcb_base + 72 * 8..fc_lcb_base + 72 * 8 + 4].copy_from_slice(&fc_papx.to_le_bytes());
        buf[fc_lcb_base + 72 * 8 + 4..fc_lcb_base + 72 * 8 + 8]
            .copy_from_slice(&lcb_papx.to_le_bytes());
        buf[fc_lcb_base + 73 * 8..fc_lcb_base + 73 * 8 + 4].copy_from_slice(&fc_chpx.to_le_bytes());
        buf[fc_lcb_base + 73 * 8 + 4..fc_lcb_base + 73 * 8 + 8]
            .copy_from_slice(&lcb_chpx.to_le_bytes());

        buf
    }

    // -- Integration tests: paragraph properties --

    #[test]
    fn paragraph_properties_in_table() {
        // Set up: one compressed piece at CP 0..10, byte offset at 1024
        let text_offset = 1024u32;
        let cp_count = 10u32;
        let clx = build_clx_with_offsets(&[(0, cp_count, text_offset, true)]);
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();

        // Build PAPX FKP with one run.
        // For compressed piece at byte_offset=1024, CP 0..10:
        // FKP FC = (1024 * 2) | 0x40000000 = 0x40000800
        let fc_base = (text_offset * 2) | 0x4000_0000;
        let fc_end = ((text_offset + cp_count) * 2) | 0x4000_0000;

        // Build sprm for sprmPFInTable: opcode 0x2416, operand 0x01
        let mut sprms = Vec::new();
        sprms.extend_from_slice(&SPRM_PF_IN_TABLE.to_le_bytes());
        sprms.push(0x01);

        let fkp_page = build_papx_fkp(&[(fc_base, fc_end, 42, &sprms)]);

        // Place FKP page at page_number=2 (offset 1024) in WordDocument stream.
        // We also need text data at offset 1024.
        let mut word_doc = vec![0u8; 2 * FKP_PAGE_SIZE];
        word_doc[FKP_PAGE_SIZE..FKP_PAGE_SIZE + FKP_PAGE_SIZE].copy_from_slice(&fkp_page);
        // Extend to include text area (already covered since 2 * 512 = 1024 = text_offset)
        word_doc.resize((text_offset as usize) + (cp_count as usize), 0);

        // Build PlcfBtePapx pointing to page 1 (offset 512)
        let plcf_papx = build_plcf_bte(&[1]);

        // Build table stream: CLX at 0, PlcfBtePapx after it
        let mut table_stream = clx.clone();
        let papx_offset = table_stream.len() as u32;
        table_stream.extend_from_slice(&plcf_papx);

        // Build FIB
        let fib_data = build_fib_with_bte(
            cp_count,
            0,
            clx.len() as u32,
            papx_offset,
            plcf_papx.len() as u32,
            0,
            0,
        );
        let fib = crate::fib::parse_fib(&fib_data).unwrap();

        let props = parse_paragraph_properties(&table_stream, &word_doc, &fib, &pt).unwrap();

        assert_eq!(props.len(), 1);
        assert_eq!(props[0].cp_start, 0);
        assert_eq!(props[0].cp_end, 10);
        assert_eq!(props[0].istd, 42);
        assert!(props[0].in_table);
        assert!(!props[0].table_row_end);
    }

    #[test]
    fn character_properties_bold() {
        let text_offset = 1024u32;
        let cp_count = 5u32;
        let clx = build_clx_with_offsets(&[(0, cp_count, text_offset, true)]);
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();

        let fc_base = (text_offset * 2) | 0x4000_0000;
        let fc_end = ((text_offset + cp_count) * 2) | 0x4000_0000;

        // Bold sprm
        let mut sprms = Vec::new();
        sprms.extend_from_slice(&SPRM_CF_BOLD.to_le_bytes());
        sprms.push(0x01);

        let fkp_page = build_chpx_fkp(&[(fc_base, fc_end, &sprms)]);

        let mut word_doc = vec![0u8; FKP_PAGE_SIZE];
        word_doc.extend_from_slice(&fkp_page);
        word_doc.resize((text_offset as usize) + (cp_count as usize), 0);

        let plcf_chpx = build_plcf_bte(&[1]);
        let mut table_stream = clx.clone();
        let chpx_offset = table_stream.len() as u32;
        table_stream.extend_from_slice(&plcf_chpx);

        let fib_data = build_fib_with_bte(
            cp_count,
            0,
            clx.len() as u32,
            0,
            0,
            chpx_offset,
            plcf_chpx.len() as u32,
        );
        let fib = crate::fib::parse_fib(&fib_data).unwrap();

        let props = parse_character_properties(&table_stream, &word_doc, &fib, &pt).unwrap();

        assert_eq!(props.len(), 1);
        assert_eq!(props[0].cp_start, 0);
        assert_eq!(props[0].cp_end, 5);
        assert_eq!(props[0].bold, Some(true));
        assert_eq!(props[0].italic, None);
    }

    #[test]
    fn character_properties_italic() {
        let text_offset = 1024u32;
        let cp_count = 5u32;
        let clx = build_clx_with_offsets(&[(0, cp_count, text_offset, true)]);
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();

        let fc_base = (text_offset * 2) | 0x4000_0000;
        let fc_end = ((text_offset + cp_count) * 2) | 0x4000_0000;

        let mut sprms = Vec::new();
        sprms.extend_from_slice(&SPRM_CF_ITALIC.to_le_bytes());
        sprms.push(0x01);

        let fkp_page = build_chpx_fkp(&[(fc_base, fc_end, &sprms)]);

        let mut word_doc = vec![0u8; FKP_PAGE_SIZE];
        word_doc.extend_from_slice(&fkp_page);
        word_doc.resize((text_offset as usize) + (cp_count as usize), 0);

        let plcf_chpx = build_plcf_bte(&[1]);
        let mut table_stream = clx.clone();
        let chpx_offset = table_stream.len() as u32;
        table_stream.extend_from_slice(&plcf_chpx);

        let fib_data = build_fib_with_bte(
            cp_count,
            0,
            clx.len() as u32,
            0,
            0,
            chpx_offset,
            plcf_chpx.len() as u32,
        );
        let fib = crate::fib::parse_fib(&fib_data).unwrap();

        let props = parse_character_properties(&table_stream, &word_doc, &fib, &pt).unwrap();

        assert_eq!(props.len(), 1);
        assert_eq!(props[0].italic, Some(true));
        assert_eq!(props[0].bold, None);
    }

    #[test]
    fn istd_extraction_from_papx() {
        let text_offset = 1024u32;
        let cp_count = 8u32;
        let clx = build_clx_with_offsets(&[(0, cp_count, text_offset, true)]);
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();

        let fc_base = (text_offset * 2) | 0x4000_0000;
        let fc_end = ((text_offset + cp_count) * 2) | 0x4000_0000;

        // No SPRMs, just testing istd
        let fkp_page = build_papx_fkp(&[(fc_base, fc_end, 99, &[])]);

        let mut word_doc = vec![0u8; FKP_PAGE_SIZE];
        word_doc.extend_from_slice(&fkp_page);
        word_doc.resize((text_offset as usize) + (cp_count as usize), 0);

        let plcf_papx = build_plcf_bte(&[1]);
        let mut table_stream = clx.clone();
        let papx_offset = table_stream.len() as u32;
        table_stream.extend_from_slice(&plcf_papx);

        let fib_data = build_fib_with_bte(
            cp_count,
            0,
            clx.len() as u32,
            papx_offset,
            plcf_papx.len() as u32,
            0,
            0,
        );
        let fib = crate::fib::parse_fib(&fib_data).unwrap();

        let props = parse_paragraph_properties(&table_stream, &word_doc, &fib, &pt).unwrap();

        assert_eq!(props.len(), 1);
        assert_eq!(props[0].istd, 99);
        assert!(!props[0].in_table);
        assert!(!props[0].table_row_end);
    }

    #[test]
    fn fkp_page_with_crun_1() {
        // Minimal FKP: just 1 run
        let text_offset = 1024u32;
        let cp_count = 3u32;
        let clx = build_clx_with_offsets(&[(0, cp_count, text_offset, true)]);
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();

        let fc_base = (text_offset * 2) | 0x4000_0000;
        let fc_end = ((text_offset + cp_count) * 2) | 0x4000_0000;

        // Single run with no special properties
        let fkp_page = build_chpx_fkp(&[(fc_base, fc_end, &[])]);

        // Page 1 is at offset 512, need at least 1024 bytes + text area
        let mut word_doc = vec![0u8; FKP_PAGE_SIZE];
        word_doc.extend_from_slice(&fkp_page);
        word_doc.resize((text_offset as usize) + (cp_count as usize), 0);

        let plcf_chpx = build_plcf_bte(&[1]);
        let mut table_stream = clx.clone();
        let chpx_offset = table_stream.len() as u32;
        table_stream.extend_from_slice(&plcf_chpx);

        let fib_data = build_fib_with_bte(
            cp_count,
            0,
            clx.len() as u32,
            0,
            0,
            chpx_offset,
            plcf_chpx.len() as u32,
        );
        let fib = crate::fib::parse_fib(&fib_data).unwrap();

        let props = parse_character_properties(&table_stream, &word_doc, &fib, &pt).unwrap();

        // Empty sprm data yields a record with all None
        assert_eq!(props.len(), 1);
        assert_eq!(props[0].cp_start, 0);
        assert_eq!(props[0].cp_end, 3);
        assert_eq!(props[0].bold, None);
        assert_eq!(props[0].italic, None);
        assert_eq!(props[0].font_size_half_pts, None);
        assert_eq!(props[0].font_index, None);
    }

    #[test]
    fn empty_plcf_bte_returns_empty() {
        let clx = build_clx_with_offsets(&[(0, 5, 0, true)]);
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();

        let fib_data = build_fib_with_bte(5, 0, clx.len() as u32, 0, 0, 0, 0);
        let fib = crate::fib::parse_fib(&fib_data).unwrap();

        let table_stream = clx;
        let word_doc = vec![0u8; 512];

        let pprops = parse_paragraph_properties(&table_stream, &word_doc, &fib, &pt).unwrap();
        assert!(pprops.is_empty());

        let cprops = parse_character_properties(&table_stream, &word_doc, &fib, &pt).unwrap();
        assert!(cprops.is_empty());
    }

    #[test]
    fn character_properties_font_size_and_index() {
        let text_offset = 1024u32;
        let cp_count = 5u32;
        let clx = build_clx_with_offsets(&[(0, cp_count, text_offset, true)]);
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();

        let fc_base = (text_offset * 2) | 0x4000_0000;
        let fc_end = ((text_offset + cp_count) * 2) | 0x4000_0000;

        // Build sprms with font size (24pt = 48 half-pts) and font index 3
        let mut sprms = Vec::new();
        sprms.extend_from_slice(&SPRM_C_HPS.to_le_bytes());
        sprms.extend_from_slice(&48u16.to_le_bytes());
        sprms.extend_from_slice(&SPRM_C_RG_FTC0.to_le_bytes());
        sprms.extend_from_slice(&3u16.to_le_bytes());

        let fkp_page = build_chpx_fkp(&[(fc_base, fc_end, &sprms)]);

        let mut word_doc = vec![0u8; FKP_PAGE_SIZE];
        word_doc.extend_from_slice(&fkp_page);
        word_doc.resize((text_offset as usize) + (cp_count as usize), 0);

        let plcf_chpx = build_plcf_bte(&[1]);
        let mut table_stream = clx.clone();
        let chpx_offset = table_stream.len() as u32;
        table_stream.extend_from_slice(&plcf_chpx);

        let fib_data = build_fib_with_bte(
            cp_count,
            0,
            clx.len() as u32,
            0,
            0,
            chpx_offset,
            plcf_chpx.len() as u32,
        );
        let fib = crate::fib::parse_fib(&fib_data).unwrap();

        let props = parse_character_properties(&table_stream, &word_doc, &fib, &pt).unwrap();

        assert_eq!(props.len(), 1);
        assert_eq!(props[0].font_size_half_pts, Some(48));
        assert_eq!(props[0].font_index, Some(3));
        assert_eq!(props[0].bold, None);
        assert_eq!(props[0].italic, None);
    }

    #[test]
    fn paragraph_table_row_end() {
        let text_offset = 1024u32;
        let cp_count = 5u32;
        let clx = build_clx_with_offsets(&[(0, cp_count, text_offset, true)]);
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();

        let fc_base = (text_offset * 2) | 0x4000_0000;
        let fc_end = ((text_offset + cp_count) * 2) | 0x4000_0000;

        // Both in-table and row-end
        let mut sprms = Vec::new();
        sprms.extend_from_slice(&SPRM_PF_IN_TABLE.to_le_bytes());
        sprms.push(0x01);
        sprms.extend_from_slice(&SPRM_PF_TTP.to_le_bytes());
        sprms.push(0x01);

        let fkp_page = build_papx_fkp(&[(fc_base, fc_end, 0, &sprms)]);

        let mut word_doc = vec![0u8; FKP_PAGE_SIZE];
        word_doc.extend_from_slice(&fkp_page);
        word_doc.resize((text_offset as usize) + (cp_count as usize), 0);

        let plcf_papx = build_plcf_bte(&[1]);
        let mut table_stream = clx.clone();
        let papx_offset = table_stream.len() as u32;
        table_stream.extend_from_slice(&plcf_papx);

        let fib_data = build_fib_with_bte(
            cp_count,
            0,
            clx.len() as u32,
            papx_offset,
            plcf_papx.len() as u32,
            0,
            0,
        );
        let fib = crate::fib::parse_fib(&fib_data).unwrap();

        let props = parse_paragraph_properties(&table_stream, &word_doc, &fib, &pt).unwrap();

        assert_eq!(props.len(), 1);
        assert!(props[0].in_table);
        assert!(props[0].table_row_end);
    }
}

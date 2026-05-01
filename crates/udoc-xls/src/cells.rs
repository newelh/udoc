//! Cell record parsing for BIFF8 sheet substreams.
//!
//! After seeking to a sheet's BOF (via BoundSheet8 lbPlyPos) and consuming
//! the BOF record, call [`parse_sheet_cells`] to extract all cell values
//! from the sheet substream until the sheet EOF record.
//!
//! Supported cell record types: LABELSST, NUMBER, RK, MULRK, FORMULA,
//! STRING (following a string-result FORMULA), BOOLERR, BLANK, MULBLANK.

use crate::error::{Error, Result};
use crate::records::{
    BiffReader, RT_BLANK, RT_BOOLERR, RT_EOF, RT_FORMULA, RT_LABEL, RT_LABELSST, RT_MERGEDCELLS,
    RT_MULBLANK, RT_MULRK, RT_NUMBER, RT_RK, RT_STRING,
};
use udoc_core::diagnostics::{DiagnosticsSink, Warning};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// The value extracted from a single spreadsheet cell.
#[derive(Debug, Clone)]
pub enum CellValue {
    /// A string value (from SST lookup, FORMULA string result, or similar).
    String(String),
    /// A numeric value (NUMBER, RK, MULRK, or f64 FORMULA result).
    Number(f64),
    /// A boolean value (BOOLERR with fError=0, or FORMULA boolean result).
    Bool(bool),
    /// An error code byte (BOOLERR with fError=1, or FORMULA error result).
    Error(u8),
    /// A cell with no meaningful value (e.g. BLANK cells with only formatting).
    Empty,
}

/// A single parsed cell with its grid position, XF index, and value.
#[derive(Debug, Clone)]
pub struct Cell {
    /// Zero-based row index.
    pub row: u16,
    /// Zero-based column index.
    pub col: u16,
    /// Index into the XF (Extended Format) table for cell formatting.
    pub ixfe: u16,
    /// The cell's value.
    pub value: CellValue,
}

/// A merged cell range from a MERGEDCELLS record (MS-XLS 2.4.168).
///
/// The top-left cell of the range carries the content; all other cells in the
/// range are empty BLANK records in practice. Row and column indices are
/// zero-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedRange {
    /// First (top) row index of the merged area (inclusive).
    pub first_row: u16,
    /// Last (bottom) row index of the merged area (inclusive).
    pub last_row: u16,
    /// First (left) column index of the merged area (inclusive).
    pub first_col: u16,
    /// Last (right) column index of the merged area (inclusive).
    pub last_col: u16,
}

impl MergedRange {
    /// Number of columns spanned by this range (minimum 1).
    pub fn col_span(&self) -> usize {
        (self.last_col.saturating_sub(self.first_col) as usize).saturating_add(1)
    }

    /// Number of rows spanned by this range (minimum 1).
    pub fn row_span(&self) -> usize {
        (self.last_row.saturating_sub(self.first_row) as usize).saturating_add(1)
    }
}

// ---------------------------------------------------------------------------
// RK decoder
// ---------------------------------------------------------------------------

use crate::formats::decode_rk;

// ---------------------------------------------------------------------------
// Main parser
// ---------------------------------------------------------------------------

/// Parse cell records from a sheet substream.
///
/// The `reader` must be positioned immediately after the sheet's BOF record.
/// Reads records until the sheet EOF record (or end of data) and returns all
/// cells that carry a value. BLANK and MULBLANK records are silently skipped.
///
/// String-result FORMULA cells are completed when the subsequent STRING record
/// is consumed. If no STRING record follows immediately, the cell is stored
/// with an empty string and a diagnostic warning is emitted.
///
/// Returns `(cells, merged_ranges)`. A sheet may contain multiple MERGEDCELLS
/// records (Excel splits them at 1,026 ranges per record); all ranges are
/// collected into a single flat Vec.
pub fn parse_sheet_cells(
    reader: &mut BiffReader,
    sst: &[String],
    diag: &dyn DiagnosticsSink,
) -> Result<(Vec<Cell>, Vec<MergedRange>)> {
    let mut cells: Vec<Cell> = Vec::new();
    let mut merged_ranges: Vec<MergedRange> = Vec::new();

    // When a FORMULA cell produces a string result, we store a placeholder
    // here and fill it in when the next STRING record arrives.
    let mut expecting_formula_string: Option<usize> = None;

    loop {
        if cells.len() >= crate::MAX_CELLS_PER_SHEET {
            diag.warning(Warning::new(
                "cell_limit_exceeded",
                format!(
                    "sheet exceeds {} cells, stopping cell extraction",
                    crate::MAX_CELLS_PER_SHEET
                ),
            ));
            break;
        }

        let record = match reader.next_record()? {
            Some(r) => r,
            None => break,
        };

        match record.record_type {
            RT_EOF => break,

            // ------------------------------------------------------------------
            // LABELSST (0x00FD): row/col/ixfe + u32 SST index
            // ------------------------------------------------------------------
            RT_LABELSST => {
                // If a previous FORMULA expected a STRING, it won't get one now.
                flush_pending_formula_string(&mut expecting_formula_string, &mut cells, diag);

                if record.data.len() < 10 {
                    diag.warning(Warning::new(
                        "labelsst_truncated",
                        format!(
                            "LABELSST record too short: {} bytes (need 10)",
                            record.data.len()
                        ),
                    ));
                    continue;
                }

                let row = u16::from_le_bytes([record.data[0], record.data[1]]);
                let col = u16::from_le_bytes([record.data[2], record.data[3]]);
                let ixfe = u16::from_le_bytes([record.data[4], record.data[5]]);
                let isst = u32::from_le_bytes([
                    record.data[6],
                    record.data[7],
                    record.data[8],
                    record.data[9],
                ]) as usize;

                let value = if isst < sst.len() {
                    CellValue::String(sst[isst].clone())
                } else {
                    diag.warning(Warning::new(
                        "labelsst_out_of_bounds",
                        format!(
                            "LABELSST isst={isst} is out of bounds (SST has {} entries)",
                            sst.len()
                        ),
                    ));
                    CellValue::Empty
                };

                cells.push(Cell {
                    row,
                    col,
                    ixfe,
                    value,
                });
            }

            // ------------------------------------------------------------------
            // LABEL (0x0204): inline string cell (pre-SST, rare in BIFF8)
            // ------------------------------------------------------------------
            RT_LABEL => {
                flush_pending_formula_string(&mut expecting_formula_string, &mut cells, diag);

                if record.data.len() < 8 {
                    diag.warning(Warning::new(
                        "label_truncated",
                        format!(
                            "LABEL record too short: {} bytes (need 8+)",
                            record.data.len()
                        ),
                    ));
                    continue;
                }

                let row = u16::from_le_bytes([record.data[0], record.data[1]]);
                let col = u16::from_le_bytes([record.data[2], record.data[3]]);
                let ixfe = u16::from_le_bytes([record.data[4], record.data[5]]);

                // XLUnicodeString: cch (u16), grbit (u8), chars
                let cch = u16::from_le_bytes([record.data[6], record.data[7]]) as usize;
                if record.data.len() < 9 {
                    continue;
                }
                let grbit = record.data[8];
                let high_byte = (grbit & 0x01) != 0;

                let text = if high_byte {
                    let byte_len = cch.saturating_mul(2);
                    let end = (9 + byte_len).min(record.data.len());
                    let mut s = String::with_capacity(cch);
                    let mut i = 9;
                    while i + 1 < end {
                        let code_unit = u16::from_le_bytes([record.data[i], record.data[i + 1]]);
                        if let Some(ch) = char::from_u32(code_unit as u32) {
                            s.push(ch);
                        } else {
                            s.push('\u{FFFD}');
                        }
                        i += 2;
                    }
                    s
                } else {
                    let end = (9 + cch).min(record.data.len());
                    record.data[9..end].iter().map(|&b| b as char).collect()
                };

                cells.push(Cell {
                    row,
                    col,
                    ixfe,
                    value: CellValue::String(text),
                });
            }

            // ------------------------------------------------------------------
            // NUMBER (0x0203): row/col/ixfe + f64
            // ------------------------------------------------------------------
            RT_NUMBER => {
                flush_pending_formula_string(&mut expecting_formula_string, &mut cells, diag);

                if record.data.len() < 14 {
                    diag.warning(Warning::new(
                        "number_truncated",
                        format!(
                            "NUMBER record too short: {} bytes (need 14)",
                            record.data.len()
                        ),
                    ));
                    continue;
                }

                let row = u16::from_le_bytes([record.data[0], record.data[1]]);
                let col = u16::from_le_bytes([record.data[2], record.data[3]]);
                let ixfe = u16::from_le_bytes([record.data[4], record.data[5]]);
                let num_bytes: [u8; 8] = record.data[6..14]
                    .try_into()
                    .map_err(|_| Error::new("NUMBER: failed to read f64 bytes"))?;
                let num = f64::from_le_bytes(num_bytes);

                cells.push(Cell {
                    row,
                    col,
                    ixfe,
                    value: CellValue::Number(num),
                });
            }

            // ------------------------------------------------------------------
            // RK (0x027E): row/col/ixfe + u32 RK value
            // ------------------------------------------------------------------
            RT_RK => {
                flush_pending_formula_string(&mut expecting_formula_string, &mut cells, diag);

                if record.data.len() < 10 {
                    diag.warning(Warning::new(
                        "rk_truncated",
                        format!("RK record too short: {} bytes (need 10)", record.data.len()),
                    ));
                    continue;
                }

                let row = u16::from_le_bytes([record.data[0], record.data[1]]);
                let col = u16::from_le_bytes([record.data[2], record.data[3]]);
                let ixfe = u16::from_le_bytes([record.data[4], record.data[5]]);
                let rk = u32::from_le_bytes([
                    record.data[6],
                    record.data[7],
                    record.data[8],
                    record.data[9],
                ]);

                cells.push(Cell {
                    row,
                    col,
                    ixfe,
                    value: CellValue::Number(decode_rk(rk)),
                });
            }

            // ------------------------------------------------------------------
            // MULRK (0x00BD): row/colFirst + N*(ixfe/rk) pairs + colLast
            // ------------------------------------------------------------------
            RT_MULRK => {
                flush_pending_formula_string(&mut expecting_formula_string, &mut cells, diag);

                // Minimum: row(2) + colFirst(2) + one pair(6) + colLast(2) = 12
                if record.data.len() < 12 {
                    diag.warning(Warning::new(
                        "mulrk_truncated",
                        format!(
                            "MULRK record too short: {} bytes (need at least 12)",
                            record.data.len()
                        ),
                    ));
                    continue;
                }

                let row = u16::from_le_bytes([record.data[0], record.data[1]]);
                let col_first = u16::from_le_bytes([record.data[2], record.data[3]]);
                let col_last = u16::from_le_bytes([
                    record.data[record.data.len() - 2],
                    record.data[record.data.len() - 1],
                ]);

                // Number of (ixfe, rk) pairs determined by byte count, not col range.
                let pairs_len = record.data.len().saturating_sub(6); // strip row+colFirst+colLast
                let pair_count = pairs_len / 6;

                let expected_count = (col_last as usize).saturating_sub(col_first as usize) + 1;
                if pair_count != expected_count {
                    diag.warning(Warning::new(
                        "mulrk_count_mismatch",
                        format!(
                            "MULRK colFirst={col_first} colLast={col_last} implies {expected_count} pairs but byte count gives {pair_count}"
                        ),
                    ));
                }

                for i in 0..pair_count {
                    let offset = 4 + i * 6; // skip row(2)+colFirst(2), then 6 bytes per pair
                    if offset + 6 > record.data.len().saturating_sub(2) {
                        break;
                    }
                    let ixfe = u16::from_le_bytes([record.data[offset], record.data[offset + 1]]);
                    let rk = u32::from_le_bytes([
                        record.data[offset + 2],
                        record.data[offset + 3],
                        record.data[offset + 4],
                        record.data[offset + 5],
                    ]);
                    let col = col_first.saturating_add(i as u16);
                    cells.push(Cell {
                        row,
                        col,
                        ixfe,
                        value: CellValue::Number(decode_rk(rk)),
                    });
                }
            }

            // ------------------------------------------------------------------
            // FORMULA (0x0006): row/col/ixfe + 8-byte value field
            // ------------------------------------------------------------------
            RT_FORMULA => {
                flush_pending_formula_string(&mut expecting_formula_string, &mut cells, diag);

                if record.data.len() < 20 {
                    diag.warning(Warning::new(
                        "formula_truncated",
                        format!(
                            "FORMULA record too short: {} bytes (need at least 20)",
                            record.data.len()
                        ),
                    ));
                    continue;
                }

                let row = u16::from_le_bytes([record.data[0], record.data[1]]);
                let col = u16::from_le_bytes([record.data[2], record.data[3]]);
                let ixfe = u16::from_le_bytes([record.data[4], record.data[5]]);

                // Bytes 6-13 are the cached formula result value.
                let val = &record.data[6..14];

                let value = if val[6] == 0xFF && val[7] == 0xFF {
                    // Special non-numeric result -- discriminated by val[0].
                    match val[0] {
                        0x00 => {
                            // String result: value will come in the next STRING record.
                            // Push a placeholder and record its index.
                            let placeholder_idx = cells.len();
                            cells.push(Cell {
                                row,
                                col,
                                ixfe,
                                value: CellValue::String(String::new()),
                            });
                            expecting_formula_string = Some(placeholder_idx);
                            continue;
                        }
                        0x01 => {
                            // Boolean result: val[2] is 0 or 1.
                            CellValue::Bool(val[2] != 0)
                        }
                        0x02 => {
                            // Error result: val[2] is the error code byte.
                            CellValue::Error(val[2])
                        }
                        _ => {
                            // Unknown special type; store as empty.
                            diag.warning(Warning::new(
                                "formula_unknown_special",
                                format!(
                                    "FORMULA special result type {:#04x} at row={row} col={col}",
                                    val[0]
                                ),
                            ));
                            CellValue::Empty
                        }
                    }
                } else {
                    // Numeric f64 result.
                    let num_bytes: [u8; 8] = val
                        .try_into()
                        .map_err(|_| Error::new("FORMULA: failed to read f64 bytes"))?;
                    CellValue::Number(f64::from_le_bytes(num_bytes))
                };

                cells.push(Cell {
                    row,
                    col,
                    ixfe,
                    value,
                });
            }

            // ------------------------------------------------------------------
            // STRING (0x0207): completes a preceding string-result FORMULA
            // ------------------------------------------------------------------
            RT_STRING => {
                if let Some(idx) = expecting_formula_string.take() {
                    let s = parse_string_record(&record.data, diag);
                    cells[idx].value = CellValue::String(s);
                } else {
                    // STRING without a preceding string-formula; ignore.
                    diag.warning(Warning::new(
                        "string_record_unexpected",
                        "STRING record appeared without a preceding string-result FORMULA",
                    ));
                }
            }

            // ------------------------------------------------------------------
            // BOOLERR (0x0205): boolean or error cell
            // ------------------------------------------------------------------
            RT_BOOLERR => {
                flush_pending_formula_string(&mut expecting_formula_string, &mut cells, diag);

                if record.data.len() < 8 {
                    diag.warning(Warning::new(
                        "boolerr_truncated",
                        format!(
                            "BOOLERR record too short: {} bytes (need 8)",
                            record.data.len()
                        ),
                    ));
                    continue;
                }

                let row = u16::from_le_bytes([record.data[0], record.data[1]]);
                let col = u16::from_le_bytes([record.data[2], record.data[3]]);
                let ixfe = u16::from_le_bytes([record.data[4], record.data[5]]);
                let b_bool_err = record.data[6];
                let f_error = record.data[7];

                let value = if f_error == 0 {
                    CellValue::Bool(b_bool_err != 0)
                } else {
                    CellValue::Error(b_bool_err)
                };

                cells.push(Cell {
                    row,
                    col,
                    ixfe,
                    value,
                });
            }

            // ------------------------------------------------------------------
            // BLANK / MULBLANK: cells with only formatting, no value
            // ------------------------------------------------------------------
            RT_BLANK | RT_MULBLANK => {
                flush_pending_formula_string(&mut expecting_formula_string, &mut cells, diag);
                // Silently skip; no cell value to emit.
            }

            // ------------------------------------------------------------------
            // MERGEDCELLS (0x00E5): one or more CellRangeAddress entries
            // ------------------------------------------------------------------
            RT_MERGEDCELLS => {
                flush_pending_formula_string(&mut expecting_formula_string, &mut cells, diag);
                parse_mergedcells_record(&record.data, &mut merged_ranges, diag);
            }

            // ------------------------------------------------------------------
            // All other records: ignore and continue
            // ------------------------------------------------------------------
            _ => {
                // Flush any pending formula-string state on unrelated records.
                // STRING must immediately follow its FORMULA, so anything else
                // terminates the wait.
                flush_pending_formula_string(&mut expecting_formula_string, &mut cells, diag);
            }
        }
    }

    // Handle a string-formula at the very end of the stream.
    flush_pending_formula_string(&mut expecting_formula_string, &mut cells, diag);

    Ok((cells, merged_ranges))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Maximum number of merged ranges we will accept across all MERGEDCELLS
/// records in a single sheet. Beyond this we warn and truncate.
const MAX_MERGED_RANGES: usize = 100_000;

/// Parse a MERGEDCELLS record body and append ranges to `out`.
///
/// The record body is: cmcs (u16) + cmcs * CellRangeAddress (8 bytes each).
/// Each CellRangeAddress: first_row(u16) last_row(u16) first_col(u16) last_col(u16).
///
/// Truncated entries at the end of a short record are silently skipped (one
/// warning is emitted for the truncation). Ranges that would push `out` past
/// `MAX_MERGED_RANGES` are truncated with a warning.
fn parse_mergedcells_record(data: &[u8], out: &mut Vec<MergedRange>, diag: &dyn DiagnosticsSink) {
    if data.len() < 2 {
        diag.warning(Warning::new(
            "mergedcells_truncated",
            format!(
                "MERGEDCELLS record too short: {} bytes (need at least 2)",
                data.len()
            ),
        ));
        return;
    }

    let cmcs = u16::from_le_bytes([data[0], data[1]]) as usize;
    let body = &data[2..];

    // Each CellRangeAddress is 8 bytes. If the body is shorter than cmcs*8,
    // we still parse as many complete entries as are available and warn once.
    let available = body.len() / 8;
    if available < cmcs {
        diag.warning(Warning::new(
            "mergedcells_count_exceeds_data",
            format!(
                "MERGEDCELLS cmcs={cmcs} but only {available} complete entries fit in the record data"
            ),
        ));
    }
    let to_parse = cmcs.min(available);

    for i in 0..to_parse {
        // Security: cap total merged ranges across all MERGEDCELLS records.
        if out.len() >= MAX_MERGED_RANGES {
            diag.warning(Warning::new(
                "mergedcells_limit_exceeded",
                format!(
                    "sheet exceeds {MAX_MERGED_RANGES} merged ranges; truncating remaining entries"
                ),
            ));
            return;
        }

        let offset = i * 8;
        let first_row = u16::from_le_bytes([body[offset], body[offset + 1]]);
        let last_row = u16::from_le_bytes([body[offset + 2], body[offset + 3]]);
        let first_col = u16::from_le_bytes([body[offset + 4], body[offset + 5]]);
        let last_col = u16::from_le_bytes([body[offset + 6], body[offset + 7]]);

        out.push(MergedRange {
            first_row,
            last_row,
            first_col,
            last_col,
        });
    }
}

/// If we were waiting for a STRING record to complete a FORMULA cell,
/// emit a warning and leave the cell with the empty-string placeholder.
fn flush_pending_formula_string(
    expecting: &mut Option<usize>,
    cells: &mut Vec<Cell>,
    diag: &dyn DiagnosticsSink,
) {
    if let Some(idx) = expecting.take() {
        diag.warning(Warning::new(
            "formula_string_missing",
            format!("FORMULA at cells[{idx}] expected a following STRING record but none arrived"),
        ));
        // Leave the cell value as CellValue::String("") -- already set.
        let _ = cells; // suppress unused-variable warning
    }
}

/// Parse a STRING record body into a Rust String.
///
/// Layout: cch (u16) + grbit (u8) + chars.
/// grbit bit 0: 0 = compressed (1 byte/char, Latin-1), 1 = UTF-16LE (2 bytes/char).
fn parse_string_record(data: &[u8], diag: &dyn DiagnosticsSink) -> String {
    if data.len() < 3 {
        diag.warning(Warning::new(
            "string_record_truncated",
            format!(
                "STRING record too short: {} bytes (need at least 3)",
                data.len()
            ),
        ));
        return String::new();
    }

    let cch = u16::from_le_bytes([data[0], data[1]]) as usize;
    let grbit = data[2];
    let high_byte = (grbit & 0x01) != 0;

    let char_data = &data[3..];

    if high_byte {
        // UTF-16LE: 2 bytes per character.
        let needed = cch * 2;
        if char_data.len() < needed {
            diag.warning(Warning::new(
                "string_record_chars_truncated",
                format!(
                    "STRING UTF-16 body too short: have {} bytes, need {needed}",
                    char_data.len()
                ),
            ));
        }
        let available_chars = char_data.len() / 2;
        let chars_to_read = cch.min(available_chars);
        let mut result = String::with_capacity(chars_to_read);
        for i in 0..chars_to_read {
            let code_unit = u16::from_le_bytes([char_data[i * 2], char_data[i * 2 + 1]]);
            if let Some(ch) = char::from_u32(code_unit as u32) {
                result.push(ch);
            } else {
                result.push('\u{FFFD}');
            }
        }
        result
    } else {
        // Compressed: 1 byte per character (Latin-1).
        let chars_to_read = cch.min(char_data.len());
        if chars_to_read < cch {
            diag.warning(Warning::new(
                "string_record_chars_truncated",
                format!(
                    "STRING compressed body too short: have {} bytes, need {cch}",
                    char_data.len()
                ),
            ));
        }
        char_data[..chars_to_read]
            .iter()
            .map(|&b| b as char)
            .collect()
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

    /// Build a 4-byte BIFF8 record header.
    fn header(record_type: u16, len: u16) -> Vec<u8> {
        let mut v = Vec::with_capacity(4);
        v.extend_from_slice(&record_type.to_le_bytes());
        v.extend_from_slice(&len.to_le_bytes());
        v
    }

    /// Append a complete BIFF8 record (header + body) to a stream buffer.
    fn push_record(buf: &mut Vec<u8>, record_type: u16, body: &[u8]) {
        buf.extend_from_slice(&header(record_type, body.len() as u16));
        buf.extend_from_slice(body);
    }

    /// Append an EOF record.
    fn push_eof(buf: &mut Vec<u8>) {
        push_record(buf, records::RT_EOF, &[]);
    }

    /// Build a LABELSST body: row/col/ixfe/isst (10 bytes).
    fn labelsst_body(row: u16, col: u16, ixfe: u16, isst: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity(10);
        v.extend_from_slice(&row.to_le_bytes());
        v.extend_from_slice(&col.to_le_bytes());
        v.extend_from_slice(&ixfe.to_le_bytes());
        v.extend_from_slice(&isst.to_le_bytes());
        v
    }

    /// Build a NUMBER body: row/col/ixfe/f64 (14 bytes).
    fn number_body(row: u16, col: u16, ixfe: u16, num: f64) -> Vec<u8> {
        let mut v = Vec::with_capacity(14);
        v.extend_from_slice(&row.to_le_bytes());
        v.extend_from_slice(&col.to_le_bytes());
        v.extend_from_slice(&ixfe.to_le_bytes());
        v.extend_from_slice(&num.to_le_bytes());
        v
    }

    /// Build an RK body: row/col/ixfe/rk (10 bytes).
    fn rk_body(row: u16, col: u16, ixfe: u16, rk: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity(10);
        v.extend_from_slice(&row.to_le_bytes());
        v.extend_from_slice(&col.to_le_bytes());
        v.extend_from_slice(&ixfe.to_le_bytes());
        v.extend_from_slice(&rk.to_le_bytes());
        v
    }

    /// Build a FORMULA body with an f64 cached result (20 bytes minimum).
    fn formula_body_f64(row: u16, col: u16, ixfe: u16, value: f64) -> Vec<u8> {
        let mut v = Vec::with_capacity(20);
        v.extend_from_slice(&row.to_le_bytes());
        v.extend_from_slice(&col.to_le_bytes());
        v.extend_from_slice(&ixfe.to_le_bytes());
        v.extend_from_slice(&value.to_le_bytes()); // 8 bytes, val[6]!=FF or val[7]!=FF
                                                   // Remaining required bytes (fAlwaysCalc etc.) -- 4 bytes padding + 4 bytes chn
        v.extend_from_slice(&[0u8; 8]);
        v
    }

    /// Build a FORMULA body with a string-result cached value.
    fn formula_body_string(row: u16, col: u16, ixfe: u16) -> Vec<u8> {
        // val[0]=0x00 (string), val[1..5]=any, val[6]=0xFF, val[7]=0xFF
        let mut v = Vec::with_capacity(20);
        v.extend_from_slice(&row.to_le_bytes());
        v.extend_from_slice(&col.to_le_bytes());
        v.extend_from_slice(&ixfe.to_le_bytes());
        v.extend_from_slice(&[0x00u8, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF]); // val
        v.extend_from_slice(&[0u8; 8]); // padding
        v
    }

    /// Build a FORMULA body with a boolean-result cached value.
    fn formula_body_bool(row: u16, col: u16, ixfe: u16, value: bool) -> Vec<u8> {
        // val[0]=0x01, val[2]=0 or 1, val[6]=0xFF, val[7]=0xFF
        let mut v = Vec::with_capacity(20);
        v.extend_from_slice(&row.to_le_bytes());
        v.extend_from_slice(&col.to_le_bytes());
        v.extend_from_slice(&ixfe.to_le_bytes());
        v.extend_from_slice(&[
            0x01u8,
            0x00,
            if value { 1 } else { 0 },
            0x00,
            0x00,
            0x00,
            0xFF,
            0xFF,
        ]);
        v.extend_from_slice(&[0u8; 8]);
        v
    }

    /// Build a STRING record body: cch(u16) + grbit(u8) + chars (compressed).
    fn string_body_compressed(s: &str) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&(s.len() as u16).to_le_bytes());
        v.push(0x00); // grbit: compressed
        v.extend_from_slice(s.as_bytes());
        v
    }

    /// Build a BOOLERR body (8 bytes).
    fn boolerr_body(row: u16, col: u16, ixfe: u16, b_bool_err: u8, f_error: u8) -> Vec<u8> {
        let mut v = Vec::with_capacity(8);
        v.extend_from_slice(&row.to_le_bytes());
        v.extend_from_slice(&col.to_le_bytes());
        v.extend_from_slice(&ixfe.to_le_bytes());
        v.push(b_bool_err);
        v.push(f_error);
        v
    }

    // -----------------------------------------------------------------------
    // Test 1: Single LABELSST cell
    // -----------------------------------------------------------------------

    #[test]
    fn single_labelsst_cell() {
        let sst = vec!["hello".to_string(), "world".to_string()];
        let mut stream = Vec::new();
        push_record(
            &mut stream,
            records::RT_LABELSST,
            &labelsst_body(0, 0, 0, 1),
        );
        push_eof(&mut stream);

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let (cells, _merges) = parse_sheet_cells(&mut reader, &sst, &diag).unwrap();

        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].row, 0);
        assert_eq!(cells[0].col, 0);
        match &cells[0].value {
            CellValue::String(s) => assert_eq!(s, "world"),
            other => panic!("expected String, got {other:?}"),
        }
        assert!(diag.warnings().is_empty());
    }

    // -----------------------------------------------------------------------
    // Test 2: Single NUMBER cell
    // -----------------------------------------------------------------------

    #[test]
    fn single_number_cell() {
        let sst: Vec<String> = vec![];
        let mut stream = Vec::new();
        push_record(
            &mut stream,
            records::RT_NUMBER,
            &number_body(2, 3, 0, 3.125),
        );
        push_eof(&mut stream);

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let (cells, _merges) = parse_sheet_cells(&mut reader, &sst, &diag).unwrap();

        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].row, 2);
        assert_eq!(cells[0].col, 3);
        match cells[0].value {
            CellValue::Number(n) => assert!((n - 3.125).abs() < 1e-12),
            ref other => panic!("expected Number, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Test 3: RK with fX100=0, fInt=0 (pure float high bits)
    // -----------------------------------------------------------------------

    #[test]
    fn rk_float_no_x100() {
        // Encode 1.0 as an RK value: IEEE 754 bits of 1.0 = 0x3FF0_0000_0000_0000.
        // High 30 bits occupy bits 32-61 of u64 = 0x3FF0_0000 >> 2 = 0x0FFC_0000 in u32 bits.
        // Actually: top 30 bits of 1.0's u64 = bits 62..32 = upper 32 bits >> 2 ...
        // Simpler: build the RK as decode_rk inverts.
        // For fInt=0, fX100=0: bits = (rk & 0xFFFF_FFFC) << 32 as u64.
        // We want f64::from_bits(bits) = 1.0.
        // 1.0 bits = 0x3FF0_0000_0000_0000.
        // (rk & 0xFFFF_FFFC) << 32 = 0x3FF0_0000_0000_0000
        // => rk & 0xFFFF_FFFC = 0x3FF0_0000
        // => rk = 0x3FF0_0000 (bits 0,1 = 0 already)
        let rk: u32 = 0x3FF0_0000;
        let value = decode_rk(rk);
        assert!((value - 1.0).abs() < 1e-15, "expected 1.0 got {value}");

        let sst: Vec<String> = vec![];
        let mut stream = Vec::new();
        push_record(&mut stream, records::RT_RK, &rk_body(0, 0, 0, rk));
        push_eof(&mut stream);

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let (cells, _merges) = parse_sheet_cells(&mut reader, &sst, &diag).unwrap();

        assert_eq!(cells.len(), 1);
        match cells[0].value {
            CellValue::Number(n) => assert!((n - 1.0).abs() < 1e-15),
            ref other => panic!("expected Number, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Test 4: RK with fX100=1, fInt=1 (integer / 100)
    // -----------------------------------------------------------------------

    #[test]
    fn rk_integer_with_x100() {
        // Encode integer 42 with fInt=1, fX100=1: rk = (42 << 2) | 0b11 = 168 | 3 = 171
        let rk: u32 = (42u32 << 2) | 0b11;
        let value = decode_rk(rk);
        assert!((value - 0.42).abs() < 1e-10, "expected 0.42 got {value}");
    }

    // -----------------------------------------------------------------------
    // Test 5: RK with negative integer
    // -----------------------------------------------------------------------

    #[test]
    fn rk_negative_integer() {
        // Encode integer -5 with fInt=1, fX100=0: rk = ((-5i32 << 2) as u32) | 0b10
        let rk: u32 = ((-5i32 << 2) as u32) | 0b10;
        let value = decode_rk(rk);
        assert_eq!(value, -5.0, "expected -5.0 got {value}");
    }

    // -----------------------------------------------------------------------
    // Test 6: MULRK with 3 cells
    // -----------------------------------------------------------------------

    #[test]
    fn mulrk_three_cells() {
        // Row 1, cols 0-2. Three RK values: integers 10, 20, 30 (fInt=1, fX100=0).
        let row: u16 = 1;
        let col_first: u16 = 0;
        let col_last: u16 = 2;

        let rk10: u32 = (10u32 << 2) | 0b10;
        let rk20: u32 = (20u32 << 2) | 0b10;
        let rk30: u32 = (30u32 << 2) | 0b10;

        let mut body = Vec::new();
        body.extend_from_slice(&row.to_le_bytes());
        body.extend_from_slice(&col_first.to_le_bytes());
        // Pair 0: ixfe=0, rk=rk10
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&rk10.to_le_bytes());
        // Pair 1: ixfe=0, rk=rk20
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&rk20.to_le_bytes());
        // Pair 2: ixfe=0, rk=rk30
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&rk30.to_le_bytes());
        body.extend_from_slice(&col_last.to_le_bytes());

        let sst: Vec<String> = vec![];
        let mut stream = Vec::new();
        push_record(&mut stream, records::RT_MULRK, &body);
        push_eof(&mut stream);

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let (cells, _merges) = parse_sheet_cells(&mut reader, &sst, &diag).unwrap();

        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0].col, 0);
        assert_eq!(cells[1].col, 1);
        assert_eq!(cells[2].col, 2);

        let expected = [10.0, 20.0, 30.0];
        for (i, cell) in cells.iter().enumerate() {
            match cell.value {
                CellValue::Number(n) => assert!((n - expected[i]).abs() < 1e-10),
                ref other => panic!("cell {i}: expected Number, got {other:?}"),
            }
        }
        assert!(diag.warnings().is_empty());
    }

    // -----------------------------------------------------------------------
    // Test 7: FORMULA with f64 result
    // -----------------------------------------------------------------------

    #[test]
    fn formula_f64_result() {
        let sst: Vec<String> = vec![];
        let mut stream = Vec::new();
        push_record(
            &mut stream,
            records::RT_FORMULA,
            &formula_body_f64(0, 0, 0, 2.625),
        );
        push_eof(&mut stream);

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let (cells, _merges) = parse_sheet_cells(&mut reader, &sst, &diag).unwrap();

        assert_eq!(cells.len(), 1);
        match cells[0].value {
            CellValue::Number(n) => assert!((n - 2.625).abs() < 1e-12),
            ref other => panic!("expected Number, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Test 8: FORMULA with string result (STRING follows)
    // -----------------------------------------------------------------------

    #[test]
    fn formula_string_result_with_string_record() {
        let sst: Vec<String> = vec![];
        let mut stream = Vec::new();
        push_record(
            &mut stream,
            records::RT_FORMULA,
            &formula_body_string(0, 0, 0),
        );
        push_record(
            &mut stream,
            records::RT_STRING,
            &string_body_compressed("hello"),
        );
        push_eof(&mut stream);

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let (cells, _merges) = parse_sheet_cells(&mut reader, &sst, &diag).unwrap();

        assert_eq!(cells.len(), 1);
        match &cells[0].value {
            CellValue::String(s) => assert_eq!(s, "hello"),
            other => panic!("expected String, got {other:?}"),
        }
        assert!(diag.warnings().is_empty());
    }

    // -----------------------------------------------------------------------
    // Test 9: FORMULA with boolean result
    // -----------------------------------------------------------------------

    #[test]
    fn formula_boolean_result() {
        let sst: Vec<String> = vec![];
        let mut stream = Vec::new();
        push_record(
            &mut stream,
            records::RT_FORMULA,
            &formula_body_bool(1, 2, 0, true),
        );
        push_eof(&mut stream);

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let (cells, _merges) = parse_sheet_cells(&mut reader, &sst, &diag).unwrap();

        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].row, 1);
        assert_eq!(cells[0].col, 2);
        match cells[0].value {
            CellValue::Bool(b) => assert!(b),
            ref other => panic!("expected Bool(true), got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Test 10: LABELSST with isst out of bounds (warning emitted)
    // -----------------------------------------------------------------------

    #[test]
    fn labelsst_isst_out_of_bounds_warns() {
        let sst = vec!["only_entry".to_string()];
        let mut stream = Vec::new();
        // isst=5 but SST only has 1 entry.
        push_record(
            &mut stream,
            records::RT_LABELSST,
            &labelsst_body(0, 0, 0, 5),
        );
        push_eof(&mut stream);

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let (cells, _merges) = parse_sheet_cells(&mut reader, &sst, &diag).unwrap();

        assert_eq!(cells.len(), 1);
        match cells[0].value {
            CellValue::Empty => {}
            ref other => panic!("expected Empty, got {other:?}"),
        }

        let warnings = diag.warnings();
        assert!(!warnings.is_empty());
        assert!(warnings.iter().any(|w| w.kind == "labelsst_out_of_bounds"));
    }

    // -----------------------------------------------------------------------
    // Test 11: BOOLERR cell
    // -----------------------------------------------------------------------

    #[test]
    fn boolerr_boolean_cell() {
        let sst: Vec<String> = vec![];
        let mut stream = Vec::new();
        // fError=0, bBoolErr=0 => Bool(false)
        push_record(
            &mut stream,
            records::RT_BOOLERR,
            &boolerr_body(3, 4, 0, 0, 0),
        );
        // fError=0, bBoolErr=1 => Bool(true)
        push_record(
            &mut stream,
            records::RT_BOOLERR,
            &boolerr_body(3, 5, 0, 1, 0),
        );
        // fError=1, bBoolErr=7 => Error(7)
        push_record(
            &mut stream,
            records::RT_BOOLERR,
            &boolerr_body(3, 6, 0, 7, 1),
        );
        push_eof(&mut stream);

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let (cells, _merges) = parse_sheet_cells(&mut reader, &sst, &diag).unwrap();

        assert_eq!(cells.len(), 3);

        match cells[0].value {
            CellValue::Bool(b) => assert!(!b),
            ref other => panic!("expected Bool(false), got {other:?}"),
        }
        match cells[1].value {
            CellValue::Bool(b) => assert!(b),
            ref other => panic!("expected Bool(true), got {other:?}"),
        }
        match cells[2].value {
            CellValue::Error(e) => assert_eq!(e, 7),
            ref other => panic!("expected Error(7), got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Test 12: FORMULA with error result
    // -----------------------------------------------------------------------

    #[test]
    fn formula_error_result() {
        let sst: Vec<String> = vec![];
        let mut stream = Vec::new();

        // Build a FORMULA with error result: val[0]=0x02, val[2]=error_code,
        // val[6..7]=0xFFFF.
        let mut body = Vec::with_capacity(20);
        body.extend_from_slice(&0u16.to_le_bytes()); // row
        body.extend_from_slice(&0u16.to_le_bytes()); // col
        body.extend_from_slice(&0u16.to_le_bytes()); // ixfe
        body.extend_from_slice(&[0x02, 0x00, 0x07, 0x00, 0x00, 0x00, 0xFF, 0xFF]); // val: error, code=7
        body.extend_from_slice(&[0u8; 8]); // padding

        push_record(&mut stream, records::RT_FORMULA, &body);
        push_eof(&mut stream);

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let (cells, _merges) = parse_sheet_cells(&mut reader, &sst, &diag).unwrap();

        assert_eq!(cells.len(), 1);
        match cells[0].value {
            CellValue::Error(e) => assert_eq!(e, 7),
            ref other => panic!("expected Error(7), got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Helpers for MERGEDCELLS tests
    // -----------------------------------------------------------------------

    /// Build a MERGEDCELLS record body: cmcs(u16) + N * CellRangeAddress(8 bytes).
    fn mergedcells_body(ranges: &[(u16, u16, u16, u16)]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&(ranges.len() as u16).to_le_bytes());
        for (first_row, last_row, first_col, last_col) in ranges {
            body.extend_from_slice(&first_row.to_le_bytes());
            body.extend_from_slice(&last_row.to_le_bytes());
            body.extend_from_slice(&first_col.to_le_bytes());
            body.extend_from_slice(&last_col.to_le_bytes());
        }
        body
    }

    // -----------------------------------------------------------------------
    // MERGEDCELLS Test 1: No MERGEDCELLS record -- empty merge list
    // -----------------------------------------------------------------------

    #[test]
    fn mergedcells_absent_yields_empty_merge_list() {
        let sst: Vec<String> = vec![];
        let mut stream = Vec::new();
        push_record(&mut stream, records::RT_NUMBER, &number_body(0, 0, 0, 1.0));
        push_eof(&mut stream);

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let (cells, merges) = parse_sheet_cells(&mut reader, &sst, &diag).unwrap();

        assert_eq!(cells.len(), 1);
        assert!(
            merges.is_empty(),
            "expected no merged ranges, got {merges:?}"
        );
        assert!(diag.warnings().is_empty());
    }

    // -----------------------------------------------------------------------
    // MERGEDCELLS Test 2: Single 2x2 merged range
    // -----------------------------------------------------------------------

    #[test]
    fn mergedcells_single_2x2_range() {
        let sst: Vec<String> = vec![];
        let mut stream = Vec::new();
        // MERGEDCELLS: one range, rows 0-1, cols 0-1
        push_record(
            &mut stream,
            records::RT_MERGEDCELLS,
            &mergedcells_body(&[(0, 1, 0, 1)]),
        );
        push_eof(&mut stream);

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let (_cells, merges) = parse_sheet_cells(&mut reader, &sst, &diag).unwrap();

        assert_eq!(merges.len(), 1);
        assert_eq!(merges[0].first_row, 0);
        assert_eq!(merges[0].last_row, 1);
        assert_eq!(merges[0].first_col, 0);
        assert_eq!(merges[0].last_col, 1);
        assert_eq!(merges[0].col_span(), 2);
        assert_eq!(merges[0].row_span(), 2);
        assert!(diag.warnings().is_empty());
    }

    // -----------------------------------------------------------------------
    // MERGEDCELLS Test 3: Multiple ranges across two MERGEDCELLS records
    // -----------------------------------------------------------------------

    #[test]
    fn mergedcells_multiple_ranges_two_records() {
        let sst: Vec<String> = vec![];
        let mut stream = Vec::new();
        // First MERGEDCELLS record: two ranges
        push_record(
            &mut stream,
            records::RT_MERGEDCELLS,
            &mergedcells_body(&[(0, 0, 0, 2), (1, 3, 0, 0)]),
        );
        // Second MERGEDCELLS record: one more range
        push_record(
            &mut stream,
            records::RT_MERGEDCELLS,
            &mergedcells_body(&[(5, 5, 1, 4)]),
        );
        push_eof(&mut stream);

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let (_cells, merges) = parse_sheet_cells(&mut reader, &sst, &diag).unwrap();

        assert_eq!(merges.len(), 3);
        // First range: row 0, cols 0-2 (col_span=3, row_span=1)
        assert_eq!(
            merges[0],
            MergedRange {
                first_row: 0,
                last_row: 0,
                first_col: 0,
                last_col: 2
            }
        );
        assert_eq!(merges[0].col_span(), 3);
        assert_eq!(merges[0].row_span(), 1);
        // Second range: rows 1-3, col 0
        assert_eq!(
            merges[1],
            MergedRange {
                first_row: 1,
                last_row: 3,
                first_col: 0,
                last_col: 0
            }
        );
        assert_eq!(merges[1].row_span(), 3);
        // Third range from second record
        assert_eq!(
            merges[2],
            MergedRange {
                first_row: 5,
                last_row: 5,
                first_col: 1,
                last_col: 4
            }
        );
        assert!(diag.warnings().is_empty());
    }

    // -----------------------------------------------------------------------
    // MERGEDCELLS Test 4: Count exceeds available data (truncated record)
    // -----------------------------------------------------------------------

    #[test]
    fn mergedcells_count_exceeds_data_warns() {
        let sst: Vec<String> = vec![];
        let mut stream = Vec::new();
        // cmcs=3 but only 1 complete CellRangeAddress follows (8 bytes).
        let mut body = Vec::new();
        body.extend_from_slice(&3u16.to_le_bytes()); // cmcs=3
                                                     // Only one complete entry: rows 0-0, cols 0-1
        body.extend_from_slice(&0u16.to_le_bytes()); // first_row
        body.extend_from_slice(&0u16.to_le_bytes()); // last_row
        body.extend_from_slice(&0u16.to_le_bytes()); // first_col
        body.extend_from_slice(&1u16.to_le_bytes()); // last_col
        push_record(&mut stream, records::RT_MERGEDCELLS, &body);
        push_eof(&mut stream);

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let (_cells, merges) = parse_sheet_cells(&mut reader, &sst, &diag).unwrap();

        // Should have parsed the one complete entry.
        assert_eq!(merges.len(), 1);
        assert_eq!(merges[0].last_col, 1);
        // Should have warned about the truncation.
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.kind == "mergedcells_count_exceeds_data"),
            "expected mergedcells_count_exceeds_data warning, got: {warnings:?}"
        );
    }

    // -----------------------------------------------------------------------
    // MERGEDCELLS Test 5: Range at sheet boundary (max row/col values)
    // -----------------------------------------------------------------------

    #[test]
    fn mergedcells_boundary_values() {
        let sst: Vec<String> = vec![];
        let mut stream = Vec::new();
        // Use max u16 values to ensure no overflow in span computation.
        push_record(
            &mut stream,
            records::RT_MERGEDCELLS,
            &mergedcells_body(&[(0xFFFE, 0xFFFF, 0xFFFE, 0xFFFF)]),
        );
        push_eof(&mut stream);

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let (_cells, merges) = parse_sheet_cells(&mut reader, &sst, &diag).unwrap();

        assert_eq!(merges.len(), 1);
        let r = &merges[0];
        assert_eq!(r.first_row, 0xFFFE);
        assert_eq!(r.last_row, 0xFFFF);
        assert_eq!(r.first_col, 0xFFFE);
        assert_eq!(r.last_col, 0xFFFF);
        // col_span and row_span should be 2, not overflow.
        assert_eq!(r.col_span(), 2);
        assert_eq!(r.row_span(), 2);
        assert!(diag.warnings().is_empty());
    }

    // -----------------------------------------------------------------------
    // MERGEDCELLS Test 6: Empty MERGEDCELLS record (cmcs=0) is valid
    // -----------------------------------------------------------------------

    #[test]
    fn mergedcells_zero_count_is_valid() {
        let sst: Vec<String> = vec![];
        let mut stream = Vec::new();
        push_record(
            &mut stream,
            records::RT_MERGEDCELLS,
            &mergedcells_body(&[]), // cmcs=0, no entries
        );
        push_eof(&mut stream);

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let (_cells, merges) = parse_sheet_cells(&mut reader, &sst, &diag).unwrap();

        assert!(merges.is_empty());
        assert!(diag.warnings().is_empty());
    }
}

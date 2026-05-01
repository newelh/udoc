//! Workbook globals substream parser.
//!
//! The Workbook stream starts with a globals substream: a BOF record with
//! dt=0x0005 followed by workbook-level records and a matching EOF. This
//! module parses the globals and returns everything sheet parsers need:
//! sheet offsets, number formats, XF table, codepage, and date epoch.

use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::records::{
    BiffReader, RT_BOF, RT_BOUNDSHEET8, RT_CODEPAGE, RT_DATEMODE, RT_EOF, RT_FILEPASS, RT_FORMAT,
    RT_XF,
};
use crate::{MAX_FORMAT_RECORDS, MAX_SHEETS, MAX_XF_RECORDS};
use udoc_core::diagnostics::{DiagnosticsSink, Warning};

// -- BIFF8 BOF field values -------------------------------------------------

/// BIFF8 `vers` field value indicating this is a BIFF8 file.
const BIFF8_VERS: u16 = 0x0600;
/// BIFF5 `vers` field value -- not supported.
const BIFF5_VERS: u16 = 0x0500;

/// BOF `dt` field: globals substream.
const BOF_DT_GLOBALS: u16 = 0x0005;
/// BOF `dt` field: worksheet substream.
pub const BOF_DT_WORKSHEET: u16 = 0x0010;

// -- BOUNDSHEET8 sheet type codes -------------------------------------------

/// Sheet type: regular worksheet.
const SHEET_TYPE_WORKSHEET: u8 = 0x00;

// -- Public types -----------------------------------------------------------

/// Info about one sheet, extracted from a BOUNDSHEET8 record.
#[derive(Debug, Clone)]
pub struct SheetInfo {
    /// Sheet display name.
    pub name: String,
    /// Byte offset of the sheet BOF record within the Workbook stream.
    pub offset: u32,
    /// Sheet is hidden (hsState == 1).
    #[allow(dead_code)] // used in tests; reserved for filter-by-visibility feature
    pub hidden: bool,
    /// Sheet is very hidden (hsState == 2).
    #[allow(dead_code)] // used in tests; reserved for filter-by-visibility feature
    pub very_hidden: bool,
    /// Sheet type byte: 0=worksheet, 2=chart, 6=VBA.
    #[allow(dead_code)] // used in tests; reserved for sheet-type filtering feature
    pub sheet_type: u8,
}

/// One entry from the XF (Extended Format) table.
///
/// We only keep the number format index; all other XF fields are irrelevant
/// for text extraction.
#[derive(Debug, Clone)]
pub struct XfEntry {
    /// Index into the FORMAT table (or a built-in format number).
    pub ifmt: u16,
}

/// All workbook-level data parsed from the globals substream.
#[derive(Debug)]
pub struct WorkbookGlobals {
    /// Sheets in workbook order.
    pub sheets: Vec<SheetInfo>,
    /// Number formats keyed by format index.
    pub formats: HashMap<u16, String>,
    /// Extended format table, indexed by xf index from cell records.
    pub xf_table: Vec<XfEntry>,
    /// Windows codepage. Defaults to 1252 (CP_ACP Western Europe) if absent.
    pub codepage: u16,
    /// True if the workbook uses the 1904 date epoch instead of 1900.
    pub date_1904: bool,
}

// -- Globals parser ---------------------------------------------------------

/// Parse the globals substream from a BIFF8 Workbook stream.
///
/// The reader must be positioned at the very start of the stream (i.e. the
/// first BOF record). Parsing stops when the matching EOF record is reached
/// or the stream ends.
pub fn parse_globals(
    reader: &mut BiffReader,
    diag: &dyn DiagnosticsSink,
) -> Result<WorkbookGlobals> {
    // Read and validate the first record, which must be a BIFF8 BOF for the
    // globals substream.
    let bof = reader
        .next_record()?
        .ok_or_else(|| Error::new("Workbook stream is empty, expected BOF"))?;

    if bof.record_type != RT_BOF {
        return Err(Error::new(format!(
            "expected BOF record ({RT_BOF:#06x}) as first record, got {:#06x}",
            bof.record_type
        )));
    }

    parse_bof_globals(&bof.data)?;

    let mut globals = WorkbookGlobals {
        sheets: Vec::new(),
        formats: HashMap::new(),
        xf_table: Vec::new(),
        codepage: 1252,
        date_1904: false,
    };

    loop {
        let rec = match reader.next_record()? {
            Some(r) => r,
            None => break,
        };

        match rec.record_type {
            RT_EOF => break,

            // FILEPASS appears in the globals substream when the workbook is
            // encrypted. We have no decryption support, so reject explicitly
            // rather than continuing to extract garbled cipher bytes as text
            // ( round-4: silent extraction of encrypted files looked
            // like plain text in our output to downstream consumers).
            RT_FILEPASS => {
                return Err(Error::new(
                    "XLS file is encrypted (FILEPASS record present); decryption is not supported",
                ));
            }

            RT_BOUNDSHEET8 => {
                match parse_boundsheet8(&rec.data, diag) {
                    Ok(Some(sheet)) => {
                        if globals.sheets.len() >= MAX_SHEETS {
                            diag.warning(Warning::new(
                                "too_many_sheets",
                                format!(
                                    "workbook has more than {MAX_SHEETS} sheets, ignoring excess"
                                ),
                            ));
                        } else {
                            globals.sheets.push(sheet);
                        }
                    }
                    Ok(None) => {
                        // Non-worksheet sheet (chart, VBA); skip silently.
                    }
                    Err(e) => {
                        diag.warning(Warning::new(
                            "boundsheet_parse_error",
                            format!("failed to parse BOUNDSHEET8 record: {e}"),
                        ));
                    }
                }
            }

            RT_FORMAT => match parse_format_record(&rec.data, diag) {
                Ok(Some((ifmt, fmt_str))) => {
                    if globals.formats.len() >= MAX_FORMAT_RECORDS {
                        diag.warning(Warning::new(
                                "too_many_formats",
                                format!("workbook has more than {MAX_FORMAT_RECORDS} FORMAT records, ignoring excess"),
                            ));
                    } else {
                        globals.formats.insert(ifmt, fmt_str);
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    diag.warning(Warning::new(
                        "format_parse_error",
                        format!("failed to parse FORMAT record: {e}"),
                    ));
                }
            },

            RT_XF => match parse_xf_record(&rec.data, diag) {
                Ok(xf) => {
                    if globals.xf_table.len() >= MAX_XF_RECORDS {
                        diag.warning(Warning::new(
                                "too_many_xf_records",
                                format!(
                                    "workbook has more than {MAX_XF_RECORDS} XF records, ignoring excess"
                                ),
                            ));
                    } else {
                        globals.xf_table.push(xf);
                    }
                }
                Err(e) => {
                    diag.warning(Warning::new(
                        "xf_parse_error",
                        format!("failed to parse XF record: {e}"),
                    ));
                }
            },

            RT_CODEPAGE => {
                if rec.data.len() >= 2 {
                    globals.codepage = u16::from_le_bytes([rec.data[0], rec.data[1]]);
                } else {
                    diag.warning(Warning::new(
                        "codepage_truncated",
                        format!(
                            "CODEPAGE record is {} bytes, expected 2; using default CP1252",
                            rec.data.len()
                        ),
                    ));
                }
            }

            RT_DATEMODE => {
                if rec.data.len() >= 2 {
                    let f1904 = u16::from_le_bytes([rec.data[0], rec.data[1]]);
                    globals.date_1904 = f1904 != 0;
                } else {
                    diag.warning(Warning::new(
                        "datemode_truncated",
                        format!(
                            "DATEMODE record is {} bytes, expected 2; using default 1900 epoch",
                            rec.data.len()
                        ),
                    ));
                }
            }

            _ => {
                // Unknown or irrelevant record for globals parsing -- skip.
            }
        }
    }

    Ok(globals)
}

// -- BOF validation ---------------------------------------------------------

/// Validate a BOF record body for BIFF8 and globals substream.
fn parse_bof_globals(data: &[u8]) -> Result<()> {
    if data.len() < 4 {
        return Err(Error::new(format!(
            "BOF record too short: {} bytes (need at least 4)",
            data.len()
        )));
    }

    let vers = u16::from_le_bytes([data[0], data[1]]);
    let dt = u16::from_le_bytes([data[2], data[3]]);

    if vers == BIFF5_VERS {
        return Err(Error::new("BIFF5 not supported"));
    }
    if vers != BIFF8_VERS {
        return Err(Error::new(format!(
            "unsupported BIFF version {vers:#06x} (expected {BIFF8_VERS:#06x})"
        )));
    }
    if dt != BOF_DT_GLOBALS {
        return Err(Error::new(format!(
            "first BOF is not a globals substream (dt={dt:#06x}, expected {BOF_DT_GLOBALS:#06x})"
        )));
    }

    Ok(())
}

// -- BOUNDSHEET8 parsing ----------------------------------------------------

/// Parse one BOUNDSHEET8 record body.
///
/// Returns `Ok(None)` for non-worksheet sheet types (charts, VBA modules).
/// Returns `Ok(Some(sheet))` for visible or hidden worksheets.
fn parse_boundsheet8(data: &[u8], diag: &dyn DiagnosticsSink) -> Result<Option<SheetInfo>> {
    // Minimum: 4 (lbPlyPos) + 2 (grbit) + 1 (cch) + 1 (grbit for name) = 8 bytes.
    if data.len() < 8 {
        return Err(Error::new(format!(
            "BOUNDSHEET8 record too short: {} bytes (need at least 8)",
            data.len()
        )));
    }

    let offset = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let grbit = u16::from_le_bytes([data[4], data[5]]);

    let hs_state = (grbit & 0x0003) as u8;
    let dt = ((grbit >> 8) & 0x00FF) as u8;

    // Only process worksheets; silently skip charts, VBA modules, etc.
    if dt != SHEET_TYPE_WORKSHEET {
        return Ok(None);
    }

    let hidden = hs_state == 1;
    let very_hidden = hs_state == 2;

    // ShortXLUnicodeString starts at byte 6.
    let name = parse_short_xl_unicode_string(&data[6..], diag)?;

    Ok(Some(SheetInfo {
        name,
        offset,
        hidden,
        very_hidden,
        sheet_type: dt,
    }))
}

/// Parse a ShortXLUnicodeString from a byte slice.
///
/// Layout: cch (u8), grbit (u8, bit 0 = fHighByte), then `cch` chars.
/// If fHighByte=0, each char is 1 byte (Latin-1). If fHighByte=1, 2 bytes (UTF-16LE).
fn parse_short_xl_unicode_string(data: &[u8], diag: &dyn DiagnosticsSink) -> Result<String> {
    if data.len() < 2 {
        return Err(Error::new(format!(
            "ShortXLUnicodeString too short: {} bytes (need at least 2 for header)",
            data.len()
        )));
    }

    let cch = data[0] as usize;
    let grbit = data[1];
    let high_byte = (grbit & 0x01) != 0;

    let char_data = &data[2..];
    let bytes_needed = if high_byte {
        cch.saturating_mul(2)
    } else {
        cch
    };

    if char_data.len() < bytes_needed {
        diag.warning(Warning::new(
            "sheet_name_truncated",
            format!(
                "sheet name truncated: expected {bytes_needed} bytes, have {}",
                char_data.len()
            ),
        ));
    }

    let available_bytes = char_data.len().min(bytes_needed);
    let mut name = String::with_capacity(cch);

    if high_byte {
        let pairs = available_bytes / 2;
        for i in 0..pairs {
            let code_unit = u16::from_le_bytes([char_data[i * 2], char_data[i * 2 + 1]]);
            if let Some(ch) = char::from_u32(code_unit as u32) {
                name.push(ch);
            } else {
                name.push('\u{FFFD}');
            }
        }
    } else {
        for &byte in &char_data[..available_bytes] {
            name.push(byte as char);
        }
    }

    Ok(name)
}

// -- FORMAT record parsing --------------------------------------------------

/// Parse a FORMAT record body.
///
/// Returns `Ok(None)` on truncation (warns separately).
fn parse_format_record(data: &[u8], diag: &dyn DiagnosticsSink) -> Result<Option<(u16, String)>> {
    // ifmt: 2 bytes, then XLUnicodeString (cch u16, grbit u8, chars).
    if data.len() < 5 {
        diag.warning(Warning::new(
            "format_record_truncated",
            format!(
                "FORMAT record too short: {} bytes (need at least 5)",
                data.len()
            ),
        ));
        return Ok(None);
    }

    let ifmt = u16::from_le_bytes([data[0], data[1]]);
    let cch = u16::from_le_bytes([data[2], data[3]]) as usize;
    let grbit = data[4];
    let high_byte = (grbit & 0x01) != 0;

    let char_data = &data[5..];
    let bytes_needed = if high_byte {
        cch.saturating_mul(2)
    } else {
        cch
    };

    if char_data.len() < bytes_needed {
        diag.warning(Warning::new(
            "format_string_truncated",
            format!(
                "FORMAT record string truncated: expected {bytes_needed} bytes, have {}",
                char_data.len()
            ),
        ));
    }

    let available = char_data.len().min(bytes_needed);
    let mut fmt_str = String::with_capacity(cch);

    if high_byte {
        let pairs = available / 2;
        for i in 0..pairs {
            let code_unit = u16::from_le_bytes([char_data[i * 2], char_data[i * 2 + 1]]);
            if let Some(ch) = char::from_u32(code_unit as u32) {
                fmt_str.push(ch);
            } else {
                fmt_str.push('\u{FFFD}');
            }
        }
    } else {
        for &byte in &char_data[..available] {
            fmt_str.push(byte as char);
        }
    }

    Ok(Some((ifmt, fmt_str)))
}

// -- XF record parsing ------------------------------------------------------

/// Parse a XF record body.
///
/// XF records are 20 bytes. We only need ifmt at bytes 2-3.
fn parse_xf_record(data: &[u8], diag: &dyn DiagnosticsSink) -> Result<XfEntry> {
    if data.len() < 4 {
        diag.warning(Warning::new(
            "xf_record_truncated",
            format!(
                "XF record too short: {} bytes (need at least 4 for ifmt)",
                data.len()
            ),
        ));
        return Ok(XfEntry { ifmt: 0 });
    }

    let ifmt = u16::from_le_bytes([data[2], data[3]]);
    Ok(XfEntry { ifmt })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records;
    use udoc_core::diagnostics::CollectingDiagnostics;

    // -- Record builders ------------------------------------------------

    /// Build a minimal 4-byte BIFF8 header.
    fn biff_record(record_type: u16, data: &[u8]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(4 + data.len());
        buf.extend_from_slice(&record_type.to_le_bytes());
        buf.extend_from_slice(&(data.len() as u16).to_le_bytes());
        buf.extend_from_slice(data);
        buf
    }

    /// Build a minimal BIFF8 BOF record body for globals.
    fn bof_globals_body() -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&BIFF8_VERS.to_le_bytes()); // vers
        body.extend_from_slice(&BOF_DT_GLOBALS.to_le_bytes()); // dt
        body.extend_from_slice(&0u16.to_le_bytes()); // rupBuild (ignored)
        body.extend_from_slice(&0u16.to_le_bytes()); // rupYear  (ignored)
        body
    }

    /// Build a BIFF5 BOF record body.
    fn bof_biff5_body() -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&BIFF5_VERS.to_le_bytes()); // vers = BIFF5
        body.extend_from_slice(&BOF_DT_GLOBALS.to_le_bytes()); // dt
        body
    }

    /// Build a BOUNDSHEET8 record body for a worksheet with a Latin-1 name.
    fn boundsheet_body(offset: u32, hs_state: u8, sheet_type: u8, name: &str) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&offset.to_le_bytes()); // lbPlyPos
                                                       // grbit: bits 0-1 = hsState, bits 8-9 = dt
        let grbit: u16 = (hs_state as u16) | ((sheet_type as u16) << 8);
        body.extend_from_slice(&grbit.to_le_bytes());
        // ShortXLUnicodeString: cch, grbit (0=compressed), chars
        body.push(name.len() as u8); // cch
        body.push(0x00); // grbit: compressed
        body.extend_from_slice(name.as_bytes());
        body
    }

    /// Build a FORMAT record body with a Latin-1 format string.
    fn format_body(ifmt: u16, fmt: &str) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&ifmt.to_le_bytes()); // ifmt
        body.extend_from_slice(&(fmt.len() as u16).to_le_bytes()); // cch
        body.push(0x00); // grbit: compressed
        body.extend_from_slice(fmt.as_bytes());
        body
    }

    /// Build an XF record body (20 bytes). Only ifmt at bytes 2-3 matters.
    fn xf_body(ifmt: u16) -> Vec<u8> {
        let mut body = vec![0u8; 20];
        body[2] = (ifmt & 0xFF) as u8;
        body[3] = (ifmt >> 8) as u8;
        body
    }

    // -- Minimal stream: BOF + EOF ------------------------------------------

    #[test]
    fn minimal_globals_empty_sheets_formats_xf() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&biff_record(records::RT_BOF, &bof_globals_body()));
        stream.extend_from_slice(&biff_record(records::RT_EOF, &[]));

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let globals = parse_globals(&mut reader, &diag).unwrap();

        assert!(globals.sheets.is_empty());
        assert!(globals.formats.is_empty());
        assert!(globals.xf_table.is_empty());
        assert_eq!(globals.codepage, 1252);
        assert!(!globals.date_1904);
        assert!(diag.warnings().is_empty());
    }

    #[test]
    fn encrypted_workbook_returns_clear_error() {
        // A FILEPASS record in the globals substream marks an encrypted
        // workbook. Without explicit detection ( round-4), we would
        // skip it as "unknown record" and continue extracting cipher bytes
        // as if they were plaintext cells.
        let mut stream = Vec::new();
        stream.extend_from_slice(&biff_record(records::RT_BOF, &bof_globals_body()));
        // FILEPASS body is encryption-method-specific; its presence alone
        // is enough for us to refuse parsing.
        stream.extend_from_slice(&biff_record(records::RT_FILEPASS, &[0x01, 0x00]));
        stream.extend_from_slice(&biff_record(records::RT_EOF, &[]));

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let err = parse_globals(&mut reader, &diag).expect_err("expected encrypted error");
        let msg = format!("{err}");
        assert!(
            msg.to_lowercase().contains("encrypted"),
            "error should mention encryption: {msg}"
        );
    }

    // -- BOUNDSHEET8: single visible worksheet -----------------------------

    #[test]
    fn single_visible_worksheet_boundsheet() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&biff_record(records::RT_BOF, &bof_globals_body()));
        stream.extend_from_slice(&biff_record(
            records::RT_BOUNDSHEET8,
            &boundsheet_body(0x1000, 0, 0, "Sheet1"),
        ));
        stream.extend_from_slice(&biff_record(records::RT_EOF, &[]));

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let globals = parse_globals(&mut reader, &diag).unwrap();

        assert_eq!(globals.sheets.len(), 1);
        let sheet = &globals.sheets[0];
        assert_eq!(sheet.name, "Sheet1");
        assert_eq!(sheet.offset, 0x1000);
        assert!(!sheet.hidden);
        assert!(!sheet.very_hidden);
        assert_eq!(sheet.sheet_type, 0);
        assert!(diag.warnings().is_empty());
    }

    // -- BOUNDSHEET8: three sheets (2 visible, 1 hidden) ------------------

    #[test]
    fn three_sheets_two_visible_one_hidden() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&biff_record(records::RT_BOF, &bof_globals_body()));
        stream.extend_from_slice(&biff_record(
            records::RT_BOUNDSHEET8,
            &boundsheet_body(0x0100, 0, 0, "Alpha"),
        ));
        stream.extend_from_slice(&biff_record(
            records::RT_BOUNDSHEET8,
            &boundsheet_body(0x0200, 1, 0, "Beta"), // hidden
        ));
        stream.extend_from_slice(&biff_record(
            records::RT_BOUNDSHEET8,
            &boundsheet_body(0x0300, 0, 0, "Gamma"),
        ));
        stream.extend_from_slice(&biff_record(records::RT_EOF, &[]));

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let globals = parse_globals(&mut reader, &diag).unwrap();

        assert_eq!(globals.sheets.len(), 3);
        assert_eq!(globals.sheets[0].name, "Alpha");
        assert!(!globals.sheets[0].hidden);
        assert_eq!(globals.sheets[1].name, "Beta");
        assert!(globals.sheets[1].hidden);
        assert!(!globals.sheets[1].very_hidden);
        assert_eq!(globals.sheets[2].name, "Gamma");
        assert!(!globals.sheets[2].hidden);
    }

    // -- FORMAT record parsing ---------------------------------------------

    #[test]
    fn format_record_round_trip() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&biff_record(records::RT_BOF, &bof_globals_body()));
        stream.extend_from_slice(&biff_record(
            records::RT_FORMAT,
            &format_body(164, "dd/mm/yyyy"),
        ));
        stream.extend_from_slice(&biff_record(
            records::RT_FORMAT,
            &format_body(165, "#,##0.00"),
        ));
        stream.extend_from_slice(&biff_record(records::RT_EOF, &[]));

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let globals = parse_globals(&mut reader, &diag).unwrap();

        assert_eq!(globals.formats.len(), 2);
        assert_eq!(globals.formats[&164], "dd/mm/yyyy");
        assert_eq!(globals.formats[&165], "#,##0.00");
        assert!(diag.warnings().is_empty());
    }

    // -- XF record parsing ------------------------------------------------

    #[test]
    fn xf_table_built_from_xf_records() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&biff_record(records::RT_BOF, &bof_globals_body()));
        stream.extend_from_slice(&biff_record(records::RT_XF, &xf_body(0))); // General
        stream.extend_from_slice(&biff_record(records::RT_XF, &xf_body(14))); // built-in date
        stream.extend_from_slice(&biff_record(records::RT_XF, &xf_body(164))); // custom
        stream.extend_from_slice(&biff_record(records::RT_EOF, &[]));

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let globals = parse_globals(&mut reader, &diag).unwrap();

        assert_eq!(globals.xf_table.len(), 3);
        assert_eq!(globals.xf_table[0].ifmt, 0);
        assert_eq!(globals.xf_table[1].ifmt, 14);
        assert_eq!(globals.xf_table[2].ifmt, 164);
        assert!(diag.warnings().is_empty());
    }

    // -- CODEPAGE record --------------------------------------------------

    #[test]
    fn codepage_record_sets_codepage() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&biff_record(records::RT_BOF, &bof_globals_body()));
        // 0x04E4 = 1252 (CP_ACP, already the default, but we test parsing)
        stream.extend_from_slice(&biff_record(records::RT_CODEPAGE, &0x04E4u16.to_le_bytes()));
        stream.extend_from_slice(&biff_record(records::RT_EOF, &[]));

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let globals = parse_globals(&mut reader, &diag).unwrap();

        assert_eq!(globals.codepage, 0x04E4);
        assert!(diag.warnings().is_empty());
    }

    #[test]
    fn codepage_default_when_absent() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&biff_record(records::RT_BOF, &bof_globals_body()));
        stream.extend_from_slice(&biff_record(records::RT_EOF, &[]));

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let globals = parse_globals(&mut reader, &diag).unwrap();

        // Default is CP1252.
        assert_eq!(globals.codepage, 1252);
    }

    // -- DATEMODE record --------------------------------------------------

    #[test]
    fn datemode_1904_epoch_set() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&biff_record(records::RT_BOF, &bof_globals_body()));
        stream.extend_from_slice(&biff_record(
            records::RT_DATEMODE,
            &1u16.to_le_bytes(), // f1904 = 1
        ));
        stream.extend_from_slice(&biff_record(records::RT_EOF, &[]));

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let globals = parse_globals(&mut reader, &diag).unwrap();

        assert!(globals.date_1904);
        assert!(diag.warnings().is_empty());
    }

    // -- BIFF5 rejection --------------------------------------------------

    #[test]
    fn biff5_bof_rejected_with_clean_error() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&biff_record(records::RT_BOF, &bof_biff5_body()));
        stream.extend_from_slice(&biff_record(records::RT_EOF, &[]));

        let diag = CollectingDiagnostics::new();
        let mut reader = BiffReader::new(&stream, &diag);
        let err = parse_globals(&mut reader, &diag).unwrap_err();

        assert!(
            err.to_string().contains("BIFF5"),
            "error should mention BIFF5, got: {err}"
        );
    }
}

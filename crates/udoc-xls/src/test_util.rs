//! Test helpers for building synthetic BIFF8/XLS data.
//!
//! These helpers construct minimal but valid XLS files (CFB containers with
//! a Workbook stream) for use in unit and integration tests. Real-world XLS
//! files are far more complex, but these cover the parsing paths exercised
//! by the test suite.
//!
//! Available when the `test-internals` Cargo feature is enabled, which is
//! set by default in dev-dependencies.

use crate::records::{
    RT_BOF, RT_BOOLERR, RT_BOUNDSHEET8, RT_CODEPAGE, RT_EOF, RT_FORMAT, RT_LABELSST,
    RT_MERGEDCELLS, RT_NUMBER, RT_SST, RT_XF,
};

// ---------------------------------------------------------------------------
// Low-level record builder
// ---------------------------------------------------------------------------

/// Build a raw BIFF8 record: 4-byte header (type LE, length LE) + data.
pub fn build_biff_record(record_type: u16, data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(4 + data.len());
    buf.extend_from_slice(&record_type.to_le_bytes());
    buf.extend_from_slice(&(data.len() as u16).to_le_bytes());
    buf.extend_from_slice(data);
    buf
}

// ---------------------------------------------------------------------------
// BIFF8 component builders
// ---------------------------------------------------------------------------

/// Build a minimal BIFF8 BOF record body for the globals substream.
pub fn build_bof_globals_body() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0x0600u16.to_le_bytes()); // vers = BIFF8
    body.extend_from_slice(&0x0005u16.to_le_bytes()); // dt = globals
    body.extend_from_slice(&0u16.to_le_bytes()); // rupBuild
    body.extend_from_slice(&0u16.to_le_bytes()); // rupYear
    body
}

/// Build a minimal BIFF8 BOF record body for a worksheet substream.
pub fn build_bof_worksheet_body() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0x0600u16.to_le_bytes()); // vers = BIFF8
    body.extend_from_slice(&0x0010u16.to_le_bytes()); // dt = worksheet
    body.extend_from_slice(&0u16.to_le_bytes()); // rupBuild
    body.extend_from_slice(&0u16.to_le_bytes()); // rupYear
    body
}

/// Build a BOUNDSHEET8 record body for a worksheet with a Latin-1 name.
///
/// `offset` is the byte offset of the sheet's BOF record within the Workbook
/// stream. `hs_state` is 0 (visible), 1 (hidden), or 2 (very hidden).
pub fn build_boundsheet_body(offset: u32, name: &str) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&offset.to_le_bytes()); // lbPlyPos
                                                   // grbit: bits 0-1 = hsState(0=visible), bits 8-9 = dt(0=worksheet)
    body.extend_from_slice(&0u16.to_le_bytes());
    // ShortXLUnicodeString: cch, grbit (0=compressed), chars
    body.push(name.len() as u8);
    body.push(0x00); // grbit: compressed
    body.extend_from_slice(name.as_bytes());
    body
}

/// Build an SST record body containing the given compressed (Latin-1) strings.
pub fn build_sst_body(strings: &[&str]) -> Vec<u8> {
    let count = strings.len() as u32;
    let mut body = Vec::new();
    body.extend_from_slice(&count.to_le_bytes()); // cstTotal
    body.extend_from_slice(&count.to_le_bytes()); // cstUnique

    for s in strings {
        let cch = s.len() as u16;
        body.extend_from_slice(&cch.to_le_bytes());
        body.push(0x00); // grbit: compressed (Latin-1)
        body.extend_from_slice(s.as_bytes());
    }
    body
}

/// Build an XF record body. Only the ifmt field at bytes 2-3 is significant
/// for formatting; everything else is zero.
pub fn build_xf_body(ifmt: u16) -> Vec<u8> {
    let mut body = vec![0u8; 20];
    body[2] = (ifmt & 0xFF) as u8;
    body[3] = (ifmt >> 8) as u8;
    body
}

/// Build a LABELSST record body for a string cell.
///
/// `row` and `col` are zero-based. `ixfe` is the XF (format) index.
/// `isst` is the index into the SST.
pub fn build_labelsst_body(row: u16, col: u16, ixfe: u16, isst: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&row.to_le_bytes());
    body.extend_from_slice(&col.to_le_bytes());
    body.extend_from_slice(&ixfe.to_le_bytes());
    body.extend_from_slice(&isst.to_le_bytes());
    body
}

/// Build a NUMBER record body for a floating-point cell (14 bytes).
///
/// `row` and `col` are zero-based. `ixfe` is the XF (format) index.
/// `num` is the IEEE 754 double-precision value.
pub fn build_number_body(row: u16, col: u16, ixfe: u16, num: f64) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&row.to_le_bytes());
    body.extend_from_slice(&col.to_le_bytes());
    body.extend_from_slice(&ixfe.to_le_bytes());
    body.extend_from_slice(&num.to_le_bytes());
    body
}

/// Build a BOOLERR record body for a boolean or error cell (8 bytes).
///
/// `row` and `col` are zero-based. `ixfe` is the XF (format) index.
/// `b_bool_err` is the boolean value (0 or 1) or error code byte.
/// `f_error` is 0 for a boolean cell, 1 for an error cell.
pub fn build_boolerr_body(row: u16, col: u16, ixfe: u16, b_bool_err: u8, f_error: u8) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&row.to_le_bytes());
    body.extend_from_slice(&col.to_le_bytes());
    body.extend_from_slice(&ixfe.to_le_bytes());
    body.push(b_bool_err);
    body.push(f_error);
    body
}

/// Build a FORMAT record body for a custom number format string.
///
/// `ifmt` is the format index (use values >= 164 to avoid colliding with
/// built-in formats). `fmt` is the format string (Latin-1, compressed).
pub fn build_format_body(ifmt: u16, fmt: &str) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&ifmt.to_le_bytes()); // ifmt
    body.extend_from_slice(&(fmt.len() as u16).to_le_bytes()); // cch
    body.push(0x00); // grbit: compressed (Latin-1)
    body.extend_from_slice(fmt.as_bytes());
    body
}

// ---------------------------------------------------------------------------
// High-level: build_minimal_xls
// ---------------------------------------------------------------------------

/// Build a complete, minimal XLS (BIFF8) file as a CFB container.
///
/// `sst_strings` -- the unique strings in the Shared String Table.
/// `sheets` -- each sheet is `(name, cells)` where each cell is
/// `(row, col, sst_string)` referencing a string in `sst_strings`.
///
/// The resulting file contains:
/// - A globals substream with BOF, CODEPAGE, SST, one XF (General), one
///   BOUNDSHEET8 per sheet, and EOF.
/// - One worksheet substream per sheet with BOF, LABELSST cells, and EOF.
///
/// Sheet BOF offsets are computed correctly so the document parser can seek
/// to them.
#[allow(clippy::type_complexity)]
pub fn build_minimal_xls(sst_strings: &[&str], sheets: &[(&str, &[(u16, u16, &str)])]) -> Vec<u8> {
    // Build the Workbook stream in two passes:
    //
    // Pass 1: Build the globals substream with dummy BOUNDSHEET8 offsets.
    //         Compute the size of the globals substream.
    // Pass 2: Build the sheet substreams, knowing their absolute offsets.
    //         Rebuild the globals substream with correct offsets.

    // Build all sheet substreams upfront to know their sizes.
    let sheet_streams: Vec<Vec<u8>> = sheets
        .iter()
        .map(|(_, cells)| {
            let mut stream = Vec::new();
            stream.extend_from_slice(&build_biff_record(RT_BOF, &build_bof_worksheet_body()));
            for (row, col, text) in *cells {
                // Find the index of this text in sst_strings.
                let isst = sst_strings.iter().position(|s| s == text).unwrap_or(0) as u32;
                stream.extend_from_slice(&build_biff_record(
                    RT_LABELSST,
                    &build_labelsst_body(*row, *col, 0, isst),
                ));
            }
            stream.extend_from_slice(&build_biff_record(RT_EOF, &[]));
            stream
        })
        .collect();

    // Build the globals substream (BOF to EOF).
    let globals_stream = build_globals_substream(sst_strings, sheets, &sheet_streams);
    let globals_len = globals_stream.len();

    // Compute the absolute offset of each sheet BOF within the full Workbook stream.
    // The Workbook stream is: globals_stream | sheet_streams[0] | sheet_streams[1] | ...
    let mut sheet_offsets: Vec<u32> = Vec::with_capacity(sheets.len());
    let mut running_offset = globals_len;
    for ss in &sheet_streams {
        sheet_offsets.push(running_offset as u32);
        running_offset += ss.len();
    }

    // Rebuild globals with the correct BOUNDSHEET8 offsets.
    let globals_stream = build_globals_substream_with_offsets(sst_strings, sheets, &sheet_offsets);

    // Concatenate all streams into the Workbook stream.
    let mut workbook = globals_stream;
    for ss in &sheet_streams {
        workbook.extend_from_slice(ss);
    }

    // Wrap in a CFB container.
    udoc_containers::test_util::build_cfb(&[("Workbook", &workbook)])
}

/// Build the globals substream with placeholder (zero) BOUNDSHEET8 offsets.
#[allow(clippy::type_complexity)]
fn build_globals_substream(
    sst_strings: &[&str],
    sheets: &[(&str, &[(u16, u16, &str)])],
    _sheet_streams: &[Vec<u8>],
) -> Vec<u8> {
    build_globals_substream_with_offsets(sst_strings, sheets, &vec![0u32; sheets.len()])
}

/// Build the globals substream with the given per-sheet BOUNDSHEET8 offsets.
#[allow(clippy::type_complexity)]
fn build_globals_substream_with_offsets(
    sst_strings: &[&str],
    sheets: &[(&str, &[(u16, u16, &str)])],
    offsets: &[u32],
) -> Vec<u8> {
    let mut stream = Vec::new();

    // BOF (globals)
    stream.extend_from_slice(&build_biff_record(RT_BOF, &build_bof_globals_body()));

    // CODEPAGE (CP1252)
    stream.extend_from_slice(&build_biff_record(RT_CODEPAGE, &1252u16.to_le_bytes()));

    // SST (if non-empty)
    if !sst_strings.is_empty() {
        stream.extend_from_slice(&build_biff_record(RT_SST, &build_sst_body(sst_strings)));
    }

    // XF record (General, ifmt=0)
    stream.extend_from_slice(&build_biff_record(RT_XF, &build_xf_body(0)));

    // BOUNDSHEET8 records
    for (i, (name, _)) in sheets.iter().enumerate() {
        let offset = offsets.get(i).copied().unwrap_or(0);
        stream.extend_from_slice(&build_biff_record(
            RT_BOUNDSHEET8,
            &build_boundsheet_body(offset, name),
        ));
    }

    // EOF
    stream.extend_from_slice(&build_biff_record(RT_EOF, &[]));

    stream
}

// ---------------------------------------------------------------------------
// MERGEDCELLS builder
// ---------------------------------------------------------------------------

/// Build a MERGEDCELLS record body.
///
/// Each entry in `ranges` is `(first_row, last_row, first_col, last_col)`.
pub fn build_mergedcells_body(ranges: &[(u16, u16, u16, u16)]) -> Vec<u8> {
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

/// Build a complete minimal XLS file with one sheet that has both cell data
/// and MERGEDCELLS ranges.
///
/// `sst_strings` -- the unique strings for the SST.
/// `cells` -- `(row, col, sst_index)` entries (using SST index directly, not string value).
/// `merged_ranges` -- `(first_row, last_row, first_col, last_col)` ranges.
pub fn build_minimal_xls_with_merges(
    sst_strings: &[&str],
    cells: &[(u16, u16, u32)],
    merged_ranges: &[(u16, u16, u16, u16)],
) -> Vec<u8> {
    // Build the sheet substream.
    let sheet_stream: Vec<u8> = {
        let mut stream = Vec::new();
        stream.extend_from_slice(&build_biff_record(RT_BOF, &build_bof_worksheet_body()));
        for (row, col, isst) in cells {
            stream.extend_from_slice(&build_biff_record(
                RT_LABELSST,
                &build_labelsst_body(*row, *col, 0, *isst),
            ));
        }
        if !merged_ranges.is_empty() {
            stream.extend_from_slice(&build_biff_record(
                RT_MERGEDCELLS,
                &build_mergedcells_body(merged_ranges),
            ));
        }
        stream.extend_from_slice(&build_biff_record(RT_EOF, &[]));
        stream
    };

    // Build globals with one BOUNDSHEET8.
    // First pass: compute globals size with a placeholder offset.
    let placeholder_globals = build_globals_one_sheet(sst_strings, "Sheet1", 0);
    let globals_len = placeholder_globals.len();
    let sheet_offset = globals_len as u32;

    // Second pass: correct offset.
    let globals = build_globals_one_sheet(sst_strings, "Sheet1", sheet_offset);

    let mut workbook = globals;
    workbook.extend_from_slice(&sheet_stream);

    udoc_containers::test_util::build_cfb(&[("Workbook", &workbook)])
}

/// Build a globals substream for one worksheet with the given SST strings.
fn build_globals_one_sheet(sst_strings: &[&str], sheet_name: &str, sheet_offset: u32) -> Vec<u8> {
    let mut stream = Vec::new();
    stream.extend_from_slice(&build_biff_record(RT_BOF, &build_bof_globals_body()));
    stream.extend_from_slice(&build_biff_record(RT_CODEPAGE, &1252u16.to_le_bytes()));
    if !sst_strings.is_empty() {
        stream.extend_from_slice(&build_biff_record(RT_SST, &build_sst_body(sst_strings)));
    }
    stream.extend_from_slice(&build_biff_record(RT_XF, &build_xf_body(0)));
    stream.extend_from_slice(&build_biff_record(
        RT_BOUNDSHEET8,
        &build_boundsheet_body(sheet_offset, sheet_name),
    ));
    stream.extend_from_slice(&build_biff_record(RT_EOF, &[]));
    stream
}

/// Build a globals substream for one worksheet with a custom FORMAT record
/// and an XF record pointing at that format.
///
/// This is used to test date-formatted NUMBER cells. `ifmt` must be >= 164 to
/// avoid colliding with built-in format IDs. The returned globals include a
/// FORMAT record, one XF (at index 0) pointing at ifmt, and the sheet BOF.
fn build_globals_one_sheet_with_format(
    sheet_name: &str,
    sheet_offset: u32,
    ifmt: u16,
    fmt_string: &str,
) -> Vec<u8> {
    let mut stream = Vec::new();
    stream.extend_from_slice(&build_biff_record(RT_BOF, &build_bof_globals_body()));
    stream.extend_from_slice(&build_biff_record(RT_CODEPAGE, &1252u16.to_le_bytes()));
    // FORMAT record for the custom format.
    stream.extend_from_slice(&build_biff_record(
        RT_FORMAT,
        &build_format_body(ifmt, fmt_string),
    ));
    // XF at index 0 uses ifmt.
    stream.extend_from_slice(&build_biff_record(RT_XF, &build_xf_body(ifmt)));
    stream.extend_from_slice(&build_biff_record(
        RT_BOUNDSHEET8,
        &build_boundsheet_body(sheet_offset, sheet_name),
    ));
    stream.extend_from_slice(&build_biff_record(RT_EOF, &[]));
    stream
}

/// Build a complete minimal XLS file with NUMBER cells.
///
/// `cells` -- `(row, col, ixfe, value)` entries. `ixfe` 0 uses the default
/// "General" XF. `extra_xf` -- additional XF records inserted after the first
/// (General) XF, as raw bodies from `build_xf_body`.
/// `extra_formats` -- custom FORMAT record bodies prepended before the XFs.
///
/// This is a lower-level builder that lets golden tests exercise NUMBER cells
/// without the overhead of a full SST.
pub fn build_minimal_xls_with_numbers(cells: &[(u16, u16, u16, f64)]) -> Vec<u8> {
    // Sheet substream: BOF + NUMBER records + EOF.
    let sheet_stream: Vec<u8> = {
        let mut stream = Vec::new();
        stream.extend_from_slice(&build_biff_record(RT_BOF, &build_bof_worksheet_body()));
        for &(row, col, ixfe, value) in cells {
            stream.extend_from_slice(&build_biff_record(
                RT_NUMBER,
                &build_number_body(row, col, ixfe, value),
            ));
        }
        stream.extend_from_slice(&build_biff_record(RT_EOF, &[]));
        stream
    };

    // Globals: single XF (General, ifmt=0), one sheet.
    let placeholder_globals = build_globals_one_sheet(&[], "Sheet1", 0);
    let sheet_offset = placeholder_globals.len() as u32;
    let globals = build_globals_one_sheet(&[], "Sheet1", sheet_offset);

    let mut workbook = globals;
    workbook.extend_from_slice(&sheet_stream);
    udoc_containers::test_util::build_cfb(&[("Workbook", &workbook)])
}

/// Build a minimal XLS with NUMBER cells using a date format XF.
///
/// `cells` -- `(row, col, value)` entries. All cells use XF index 0 which
/// points at a built-in date format (ifmt=14, "m/d/yy").
pub fn build_minimal_xls_with_date_numbers(cells: &[(u16, u16, f64)]) -> Vec<u8> {
    // Built-in date format 14 ("m/d/yy") is recognized by the date detector.
    const DATE_IFMT: u16 = 14;

    // Sheet substream.
    let sheet_stream: Vec<u8> = {
        let mut stream = Vec::new();
        stream.extend_from_slice(&build_biff_record(RT_BOF, &build_bof_worksheet_body()));
        for &(row, col, value) in cells {
            // ixfe=0 -- the single XF in globals uses DATE_IFMT.
            stream.extend_from_slice(&build_biff_record(
                RT_NUMBER,
                &build_number_body(row, col, 0, value),
            ));
        }
        stream.extend_from_slice(&build_biff_record(RT_EOF, &[]));
        stream
    };

    // Globals: FORMAT + XF(14) + BOUNDSHEET8.
    let placeholder_globals = build_globals_one_sheet_with_format("Sheet1", 0, DATE_IFMT, "m/d/yy");
    let sheet_offset = placeholder_globals.len() as u32;
    let globals = build_globals_one_sheet_with_format("Sheet1", sheet_offset, DATE_IFMT, "m/d/yy");

    let mut workbook = globals;
    workbook.extend_from_slice(&sheet_stream);
    udoc_containers::test_util::build_cfb(&[("Workbook", &workbook)])
}

/// Build a minimal XLS file with BOOLERR cells.
///
/// `cells` -- `(row, col, b_bool_err, f_error)` entries.
/// - `f_error=0`: boolean cell; `b_bool_err=0` is FALSE, `b_bool_err=1` is TRUE.
/// - `f_error=1`: error cell; `b_bool_err` is the error code byte (e.g. 7 = #DIV/0!).
pub fn build_minimal_xls_with_boolerr(cells: &[(u16, u16, u8, u8)]) -> Vec<u8> {
    let sheet_stream: Vec<u8> = {
        let mut stream = Vec::new();
        stream.extend_from_slice(&build_biff_record(RT_BOF, &build_bof_worksheet_body()));
        for &(row, col, b_bool_err, f_error) in cells {
            stream.extend_from_slice(&build_biff_record(
                RT_BOOLERR,
                &build_boolerr_body(row, col, 0, b_bool_err, f_error),
            ));
        }
        stream.extend_from_slice(&build_biff_record(RT_EOF, &[]));
        stream
    };

    let placeholder_globals = build_globals_one_sheet(&[], "Sheet1", 0);
    let sheet_offset = placeholder_globals.len() as u32;
    let globals = build_globals_one_sheet(&[], "Sheet1", sheet_offset);

    let mut workbook = globals;
    workbook.extend_from_slice(&sheet_stream);
    udoc_containers::test_util::build_cfb(&[("Workbook", &workbook)])
}

// ---------------------------------------------------------------------------
// Tests for the test helpers themselves
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::XlsDocument;
    use std::sync::Arc;
    use udoc_core::backend::{FormatBackend, PageExtractor};
    use udoc_core::diagnostics::NullDiagnostics;

    fn null_diag() -> Arc<dyn udoc_core::diagnostics::DiagnosticsSink> {
        Arc::new(NullDiagnostics)
    }

    #[test]
    fn build_minimal_xls_empty_parses() {
        let data = build_minimal_xls(&[], &[]);
        let doc = XlsDocument::from_bytes_with_diag(&data, null_diag()).unwrap();
        assert_eq!(FormatBackend::page_count(&doc), 0);
    }

    #[test]
    fn build_minimal_xls_one_sheet_one_cell() {
        let data = build_minimal_xls(&["hello"], &[("Sheet1", &[(0, 0, "hello")])]);
        let mut doc = XlsDocument::from_bytes_with_diag(&data, null_diag()).unwrap();
        assert_eq!(FormatBackend::page_count(&doc), 1);

        let mut page = doc.page(0).unwrap();
        let text = page.text().unwrap();
        assert_eq!(text, "hello");
    }

    #[test]
    fn build_minimal_xls_two_sheets() {
        let data = build_minimal_xls(
            &["alpha", "beta"],
            &[
                ("Sheet1", &[(0, 0, "alpha")]),
                ("Sheet2", &[(0, 0, "beta")]),
            ],
        );
        let mut doc = XlsDocument::from_bytes_with_diag(&data, null_diag()).unwrap();
        assert_eq!(FormatBackend::page_count(&doc), 2);

        let mut p0 = doc.page(0).unwrap();
        assert_eq!(p0.text().unwrap(), "alpha");

        let mut p1 = doc.page(1).unwrap();
        assert_eq!(p1.text().unwrap(), "beta");
    }

    #[test]
    fn build_biff_record_length() {
        let rec = build_biff_record(0xABCD, b"hello");
        assert_eq!(rec.len(), 4 + 5);
        assert_eq!(u16::from_le_bytes([rec[0], rec[1]]), 0xABCD);
        assert_eq!(u16::from_le_bytes([rec[2], rec[3]]), 5);
        assert_eq!(&rec[4..], b"hello");
    }
}

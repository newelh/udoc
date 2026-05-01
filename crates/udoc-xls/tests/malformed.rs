//! Malformed-input robustness tests for the XLS backend.
//!
//! Every test here must NOT panic. Returning an `Err` is fine. The key
//! invariant is that no malformed input triggers a panic, out-of-bounds
//! access, or unwrap failure in the parsing code.

use std::sync::Arc;

use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_core::diagnostics::NullDiagnostics;
use udoc_xls::document::XlsDocument;
use udoc_xls::test_util::build_minimal_xls;

fn null_diag() -> Arc<dyn udoc_core::diagnostics::DiagnosticsSink> {
    Arc::new(NullDiagnostics)
}

// ---------------------------------------------------------------------------
// Malformed 1: completely empty byte slice
// ---------------------------------------------------------------------------

#[test]
fn malformed_empty_bytes() {
    let result = XlsDocument::from_bytes_with_diag(&[], null_diag());
    // Must return Err, not panic.
    assert!(
        result.is_err(),
        "empty bytes must return Err, got Ok with page_count={}",
        result.as_ref().map(FormatBackend::page_count).unwrap_or(0)
    );
}

// ---------------------------------------------------------------------------
// Malformed 2: non-CFB data (random bytes)
// ---------------------------------------------------------------------------

#[test]
fn malformed_random_bytes() {
    // Not a CFB magic header -- just ASCII text.
    let garbage = b"This is definitely not an OLE2 container file. Random junk follows: \
                    \x00\x01\x02\x03\xFF\xFE\xFD";
    let result = XlsDocument::from_bytes_with_diag(garbage, null_diag());
    assert!(result.is_err(), "non-CFB data must return Err");
}

// ---------------------------------------------------------------------------
// Malformed 3: truncated CFB header
// ---------------------------------------------------------------------------

#[test]
fn malformed_truncated_cfb() {
    // The CFB magic is 8 bytes. Feed only 4 bytes of it.
    let partial_magic: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0];
    let result = XlsDocument::from_bytes_with_diag(partial_magic, null_diag());
    assert!(result.is_err(), "truncated CFB must return Err");
}

// ---------------------------------------------------------------------------
// Malformed 4: valid CFB container with a Workbook stream containing only
//              the CFB magic (no BIFF records at all)
// ---------------------------------------------------------------------------

#[test]
fn malformed_cfb_with_empty_workbook_stream() {
    // Build a CFB with a Workbook stream that has zero bytes of BIFF data.
    let cfb_data = udoc_containers::test_util::build_cfb(&[("Workbook", &[])]);
    // May succeed or fail, but must NOT panic.
    match XlsDocument::from_bytes_with_diag(&cfb_data, null_diag()) {
        Ok(doc) => {
            // If it parses, page_count should be 0 (no sheets parsed).
            assert_eq!(FormatBackend::page_count(&doc), 0);
        }
        Err(_) => {
            // Returning Err is also acceptable.
        }
    }
}

// ---------------------------------------------------------------------------
// Malformed 5: valid CFB with Workbook stream containing truncated BIFF data
// ---------------------------------------------------------------------------

#[test]
fn malformed_cfb_with_truncated_biff() {
    // A BIFF record header says length=100 but only 3 bytes of body follow.
    // record_type=0x0809 (BOF), length=0x0064 (100), body=3 bytes
    let mut workbook = Vec::new();
    workbook.extend_from_slice(&0x0809u16.to_le_bytes()); // BOF record type
    workbook.extend_from_slice(&100u16.to_le_bytes()); // claims 100 bytes of body
    workbook.extend_from_slice(&[0x00u8, 0x06u8, 0x05u8]); // only 3 bytes provided

    let cfb_data = udoc_containers::test_util::build_cfb(&[("Workbook", &workbook)]);
    // Must not panic.
    if let Ok(doc) = XlsDocument::from_bytes_with_diag(&cfb_data, null_diag()) {
        assert_eq!(FormatBackend::page_count(&doc), 0);
    }
}

// ---------------------------------------------------------------------------
// Malformed 6: BOUNDSHEET8 with an out-of-bounds lbPlyPos offset
// ---------------------------------------------------------------------------

#[test]
fn malformed_boundsheet_out_of_bounds_offset() {
    // Build a valid XLS but then corrupt the BOUNDSHEET8 offset to point
    // past the end of the Workbook stream.
    //
    // We build normally then doctor the raw workbook bytes.  Because this
    // is complex to do at the CFB layer, we use a simpler approach:
    // build_minimal_xls gives us correct offsets; just verify that a
    // document with a sheet having no actual cells still works robustly.
    //
    // Instead, build a CFB whose Workbook stream has a BOF (globals),
    // a BOUNDSHEET8 pointing to offset 0xFFFFFFFF, and an EOF.
    use udoc_xls::test_util::{
        build_biff_record, build_bof_globals_body, build_boundsheet_body, build_xf_body,
    };

    let rt_codepage: u16 = 0x0042;
    let rt_eof: u16 = 0x000A;
    let rt_bof: u16 = 0x0809;
    let rt_boundsheet8: u16 = 0x0085;
    let rt_xf: u16 = 0x00E0;

    let mut workbook = Vec::new();
    workbook.extend_from_slice(&build_biff_record(rt_bof, &build_bof_globals_body()));
    workbook.extend_from_slice(&build_biff_record(rt_codepage, &1252u16.to_le_bytes()));
    workbook.extend_from_slice(&build_biff_record(rt_xf, &build_xf_body(0)));
    // BOUNDSHEET8 with offset 0xFFFFFFFF (way past end of stream)
    workbook.extend_from_slice(&build_biff_record(
        rt_boundsheet8,
        &build_boundsheet_body(0xFFFF_FFFFu32, "BadSheet"),
    ));
    workbook.extend_from_slice(&build_biff_record(rt_eof, &[]));

    let cfb_data = udoc_containers::test_util::build_cfb(&[("Workbook", &workbook)]);
    let result = XlsDocument::from_bytes_with_diag(&cfb_data, null_diag());

    // May succeed (with 1 sheet, 0 cells) or fail -- must NOT panic.
    if let Ok(mut doc) = result {
        assert_eq!(FormatBackend::page_count(&doc), 1);
        // page() should succeed but return empty content, not panic.
        if let Ok(mut page) = doc.page(0) {
            let _ = page.text();
            let _ = page.tables();
        }
    }
}

// ---------------------------------------------------------------------------
// Malformed 7: CFB that has no Workbook or Book stream
// ---------------------------------------------------------------------------

#[test]
fn malformed_cfb_no_workbook_stream() {
    // Build a valid CFB container but name the stream something else.
    let cfb_data = udoc_containers::test_util::build_cfb(&[("NotAWorkbook", b"some bytes here")]);
    let result = XlsDocument::from_bytes_with_diag(&cfb_data, null_diag());
    assert!(
        result.is_err(),
        "CFB without Workbook/Book stream must return Err"
    );
}

// ---------------------------------------------------------------------------
// Malformed 8: SST record with claimed count larger than actual string data
// ---------------------------------------------------------------------------

#[test]
fn malformed_sst_count_overflow() {
    use udoc_xls::test_util::{build_biff_record, build_bof_globals_body, build_xf_body};

    let rt_codepage: u16 = 0x0042;
    let rt_eof: u16 = 0x000A;
    let rt_bof: u16 = 0x0809;
    let rt_sst: u16 = 0x00FC;
    let rt_xf: u16 = 0x00E0;

    // SST body: claims cstTotal=1000 and cstUnique=1000 but has data for only 1 string.
    let mut sst_body = Vec::new();
    sst_body.extend_from_slice(&1000u32.to_le_bytes()); // cstTotal
    sst_body.extend_from_slice(&1000u32.to_le_bytes()); // cstUnique
                                                        // Only one XLUnicodeString: cch=5, grbit=0 (compressed), "hello"
    sst_body.extend_from_slice(&5u16.to_le_bytes());
    sst_body.push(0x00);
    sst_body.extend_from_slice(b"hello");

    let mut workbook = Vec::new();
    workbook.extend_from_slice(&build_biff_record(rt_bof, &build_bof_globals_body()));
    workbook.extend_from_slice(&build_biff_record(rt_codepage, &1252u16.to_le_bytes()));
    workbook.extend_from_slice(&build_biff_record(rt_sst, &sst_body));
    workbook.extend_from_slice(&build_biff_record(rt_xf, &build_xf_body(0)));
    workbook.extend_from_slice(&build_biff_record(rt_eof, &[]));

    let cfb_data = udoc_containers::test_util::build_cfb(&[("Workbook", &workbook)]);
    // Must not panic. Err or Ok(doc with partial SST) are both acceptable.
    // Parsed successfully or Err -- both are acceptable. Must not panic.
    let _ = XlsDocument::from_bytes_with_diag(&cfb_data, null_diag());
}

// ---------------------------------------------------------------------------
// Malformed 9: page() call on a valid document with an out-of-range index
// ---------------------------------------------------------------------------

#[test]
fn malformed_page_out_of_range() {
    let data = build_minimal_xls(&["x"], &[("Sheet1", &[(0, 0, "x")])]);
    let mut doc =
        XlsDocument::from_bytes_with_diag(&data, null_diag()).expect("should parse minimal XLS");
    // page_count is 1, so index 99 must return Err.
    assert!(
        doc.page(99).is_err(),
        "out-of-range page index must return Err"
    );
}

//! Malformed input recovery tests for DOC backend.
//!
//! Verifies that the DOC parser handles broken input gracefully:
//! returns errors with context (not panics), emits diagnostic warnings,
//! and never crashes on garbage data.

use std::sync::Arc;
use udoc_containers::test_util::build_cfb;
use udoc_core::diagnostics::{CollectingDiagnostics, DiagnosticsSink};
use udoc_doc::test_util::{build_clx_with_offsets, build_fib};
use udoc_doc::DocDocument;

// ---------------------------------------------------------------------------
// 1. Truncated WordDocument stream (< 32 bytes, too short for FIB)
// ---------------------------------------------------------------------------

#[test]
fn malformed_truncated_worddocument() {
    // A valid CFB with a WordDocument stream that's too short for a FIB.
    let short_wd = vec![0xEC, 0xA5, 0x00, 0x00]; // Just magic, nothing else
    let cfb_data = build_cfb(&[("WordDocument", &short_wd), ("0Table", &[0u8; 16])]);

    let diag = Arc::new(CollectingDiagnostics::new());
    let result =
        DocDocument::from_bytes_with_diag(&cfb_data, diag.clone() as Arc<dyn DiagnosticsSink>);

    assert!(
        result.is_err(),
        "truncated WordDocument should return an error, not panic"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(!err_msg.is_empty(), "error message should not be empty");
}

// ---------------------------------------------------------------------------
// 2. Bad FIB magic (0xBEEF instead of 0xA5EC)
// ---------------------------------------------------------------------------

#[test]
fn malformed_bad_fib_magic() {
    // Build a minimal FIB then corrupt the magic.
    let mut fib_data = build_fib(10, 0, 0, false);
    // Overwrite wIdent at offset 0x00 with 0xBEEF.
    fib_data[0] = 0xEF;
    fib_data[1] = 0xBE;

    let cfb_data = build_cfb(&[("WordDocument", &fib_data), ("0Table", &[0u8; 16])]);

    let result = DocDocument::from_bytes(&cfb_data);

    assert!(
        result.is_err(),
        "bad FIB magic should return an error, not panic"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("0xBEEF") || err_msg.contains("magic") || err_msg.contains("Word"),
        "error should mention bad magic: {err_msg}"
    );
}

// ---------------------------------------------------------------------------
// 3. Missing table stream (WordDocument present, 0Table/1Table absent)
// ---------------------------------------------------------------------------

#[test]
fn malformed_missing_table_stream() {
    let fib_data = build_fib(5, 0, 10, false);

    // CFB has only WordDocument, no table stream at all.
    let cfb_data = build_cfb(&[("WordDocument", &fib_data)]);

    let result = DocDocument::from_bytes(&cfb_data);

    assert!(
        result.is_err(),
        "missing table stream should return an error, not panic"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("0Table") || err_msg.contains("Table") || err_msg.contains("not found"),
        "error should mention missing table stream: {err_msg}"
    );
}

// ---------------------------------------------------------------------------
// 4. Random bytes (no CFB structure at all)
// ---------------------------------------------------------------------------

#[test]
fn malformed_random_bytes_no_panic() {
    let garbage = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0xFF, 0x42, 0x13, 0x99, 0xAA];
    let result = DocDocument::from_bytes(&garbage);
    assert!(
        result.is_err(),
        "random bytes should return an error, not panic"
    );
}

// ---------------------------------------------------------------------------
// 5. Piece FC overflow (piece descriptor pointing past WordDocument end)
// ---------------------------------------------------------------------------

#[test]
fn malformed_piece_fc_overflow() {
    let text = "valid text";
    let text_bytes = text.as_bytes();
    let ccp_text = text_bytes.len() as u32;

    let placeholder_fib = build_fib(ccp_text, 0, 0, false);
    let _fib_size = placeholder_fib.len();

    // Build a CLX where the piece byte_offset points way past the end
    // of the WordDocument stream.
    let overflow_offset = 0x0FFF_FFFF_u32;
    let clx = build_clx_with_offsets(&[(0, ccp_text, overflow_offset, true)]);
    let fc_clx = 0u32;
    let lcb_clx = clx.len() as u32;

    let real_fib = build_fib(ccp_text, fc_clx, lcb_clx, false);
    let mut word_doc = real_fib;
    word_doc.extend_from_slice(text_bytes);

    let cfb_data = build_cfb(&[("WordDocument", &word_doc), ("0Table", &clx)]);

    let diag = Arc::new(CollectingDiagnostics::new());
    let result =
        DocDocument::from_bytes_with_diag(&cfb_data, diag.clone() as Arc<dyn DiagnosticsSink>);

    // Should error (piece points past end), not panic.
    assert!(
        result.is_err(),
        "piece FC overflow should return an error, not panic"
    );
}

// ---------------------------------------------------------------------------
// 6. Encrypted DOC (fEncrypted bit set in FIB flags)
// ---------------------------------------------------------------------------

#[test]
fn malformed_encrypted_doc() {
    let mut fib_data = build_fib(10, 0, 10, false);
    // Set bit 8 of the flags at offset 0x0A (fEncrypted).
    let flags = u16::from_le_bytes([fib_data[0x0A], fib_data[0x0B]]);
    let encrypted_flags = flags | (1 << 8);
    fib_data[0x0A..0x0C].copy_from_slice(&encrypted_flags.to_le_bytes());

    let cfb_data = build_cfb(&[("WordDocument", &fib_data), ("0Table", &[0u8; 16])]);

    let result = DocDocument::from_bytes(&cfb_data);

    assert!(
        result.is_err(),
        "encrypted DOC should return an error, not panic"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("encrypted"),
        "error should mention encryption: {err_msg}"
    );
}

// ---------------------------------------------------------------------------
// 7. Empty WordDocument stream (0 bytes)
// ---------------------------------------------------------------------------

#[test]
fn malformed_empty_worddocument() {
    let cfb_data = build_cfb(&[("WordDocument", &[]), ("0Table", &[0u8; 16])]);

    let result = DocDocument::from_bytes(&cfb_data);

    assert!(
        result.is_err(),
        "empty WordDocument stream should return an error, not panic"
    );
}

// ---------------------------------------------------------------------------
// 8. Corrupted CLX (bad Pcdt marker)
// ---------------------------------------------------------------------------

#[test]
fn malformed_bad_clx_marker() {
    let text = "some text";
    let text_bytes = text.as_bytes();
    let ccp_text = text_bytes.len() as u32;

    let placeholder_fib = build_fib(ccp_text, 0, 0, false);
    let fib_size = placeholder_fib.len();
    let text_offset = fib_size as u32;

    // Build a valid CLX then corrupt the Pcdt marker (byte 0).
    let mut clx = build_clx_with_offsets(&[(0, ccp_text, text_offset, true)]);
    if !clx.is_empty() {
        clx[0] = 0xFF; // Corrupt: should be 0x02
    }

    let fc_clx = 0u32;
    let lcb_clx = clx.len() as u32;
    let real_fib = build_fib(ccp_text, fc_clx, lcb_clx, false);

    let mut word_doc = real_fib;
    word_doc.extend_from_slice(text_bytes);

    let cfb_data = build_cfb(&[("WordDocument", &word_doc), ("0Table", &clx)]);

    let result = DocDocument::from_bytes(&cfb_data);

    // A bad CLX marker triggers the fast-save fallback, which attempts to read
    // UTF-16LE text at the FIB end offset. Because the stream was built with
    // compressed (CP1252) text at that offset rather than UTF-16LE, the fallback
    // produces garbled text or fails with a bounds error -- but it must not panic.
    // We accept either success (garbled text) or an error; the only requirement
    // is no panic.
    let _ = result;
}

//! Malformed input recovery tests for PPT backend.
//!
//! Verifies that the PPT parser handles broken input gracefully:
//! returns errors with context (not panics), emits diagnostic warnings,
//! and recovers from partial/corrupt data where possible.

use std::sync::Arc;
use udoc_containers::test_util::build_cfb;
use udoc_core::backend::FormatBackend;
use udoc_core::diagnostics::{CollectingDiagnostics, DiagnosticsSink};
use udoc_ppt::records::rt;
use udoc_ppt::test_util::*;
use udoc_ppt::PptDocument;

// ---------------------------------------------------------------------------
// 1. Truncated PowerPoint Document stream
// ---------------------------------------------------------------------------

#[test]
fn malformed_truncated_ppt_stream() {
    // Build a valid PPT structure but truncate the PowerPoint Document stream
    // to only 4 bytes (less than even one record header).
    let truncated_ppt_stream = &[0xFFu8; 4];

    // Still need a valid UserEditAtom chain pointing somewhere.
    // The persist directory will point to offset 0 in the (truncated) stream.
    let persist_atom = build_persist_directory_atom(&[(0, &[0])]);
    let persist_offset = truncated_ppt_stream.len() as u32;

    let user_edit = build_user_edit_atom(0, persist_offset);
    let user_edit_offset = persist_offset + persist_atom.len() as u32;

    let mut ppt_stream = Vec::new();
    ppt_stream.extend_from_slice(truncated_ppt_stream);
    ppt_stream.extend_from_slice(&persist_atom);
    ppt_stream.extend_from_slice(&user_edit);

    let current_user = build_current_user(user_edit_offset);

    let cfb_data = build_cfb(&[
        ("PowerPoint Document", &ppt_stream),
        ("Current User", &current_user),
    ]);

    let diag = Arc::new(CollectingDiagnostics::new());
    let result =
        PptDocument::from_bytes_with_diag(&cfb_data, diag.clone() as Arc<dyn DiagnosticsSink>);

    // Should error (truncated data can't be parsed as DocumentContainer)
    // but must NOT panic.
    assert!(
        result.is_err(),
        "truncated PPT stream should return an error, not panic"
    );
}

// ---------------------------------------------------------------------------
// 2. Circular UserEditAtom chain
// ---------------------------------------------------------------------------

#[test]
fn malformed_circular_user_edit_chain() {
    // Build a DocumentContainer with a single slide.
    let mut slwt = Vec::new();
    slwt.extend_from_slice(&build_slide_persist_atom(1));
    slwt.extend_from_slice(&build_text_header_atom(0));
    slwt.extend_from_slice(&build_text_chars_atom("Hello"));

    let doc_container = build_ppt_stream_with_slwts(&slwt, &[]);
    let doc_len = doc_container.len() as u32;

    // Persist directory at offset doc_len.
    let persist_atom = build_persist_directory_atom(&[(0, &[0])]);
    let persist_offset = doc_len;
    let persist_len = persist_atom.len() as u32;

    // UserEditAtom at offset doc_len + persist_len.
    // Set offsetLastEdit to point BACK to itself (circular).
    let user_edit_offset = doc_len + persist_len;

    // Build the UserEditAtom manually with self-referential offset.
    let mut ue_data = Vec::new();
    ue_data.extend_from_slice(&0u32.to_le_bytes()); // lastSlideIdRef
    ue_data.extend_from_slice(&0u16.to_le_bytes()); // minorVersion
    ue_data.extend_from_slice(&3u16.to_le_bytes()); // majorVersion
    ue_data.extend_from_slice(&user_edit_offset.to_le_bytes()); // offsetLastEdit -> self!
    ue_data.extend_from_slice(&persist_offset.to_le_bytes()); // offsetPersistDir
    ue_data.resize(28, 0);

    let ver_inst: u16 = 0;
    let rec_len = ue_data.len() as u32;
    let mut user_edit = Vec::new();
    user_edit.extend_from_slice(&ver_inst.to_le_bytes());
    user_edit.extend_from_slice(&rt::USER_EDIT_ATOM.to_le_bytes());
    user_edit.extend_from_slice(&rec_len.to_le_bytes());
    user_edit.extend_from_slice(&ue_data);

    let mut ppt_stream = Vec::new();
    ppt_stream.extend_from_slice(&doc_container);
    ppt_stream.extend_from_slice(&persist_atom);
    ppt_stream.extend_from_slice(&user_edit);

    let current_user = build_current_user(user_edit_offset);

    let cfb_data = build_cfb(&[
        ("PowerPoint Document", &ppt_stream),
        ("Current User", &current_user),
    ]);

    let diag = Arc::new(CollectingDiagnostics::new());
    let result =
        PptDocument::from_bytes_with_diag(&cfb_data, diag.clone() as Arc<dyn DiagnosticsSink>);

    // The circular chain should be detected and the parser should either:
    // - Return an error, or
    // - Succeed with a warning about the circular chain.
    // Either way, it must NOT hang or panic.
    match result {
        Ok(_doc) => {
            let warnings = diag.warnings();
            assert!(
                !warnings.is_empty(),
                "circular edit chain should emit at least one warning"
            );
        }
        Err(e) => {
            let err_msg = format!("{e}");
            // Error is acceptable for circular chains.
            assert!(
                !err_msg.is_empty(),
                "error message should not be empty: {err_msg}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 3. Record with length exceeding stream bounds
// ---------------------------------------------------------------------------

#[test]
fn malformed_record_length_exceeds_stream() {
    // Build a SLWT containing a TextCharsAtom whose declared length
    // exceeds the available data. The parser should handle this gracefully.
    let mut slwt = Vec::new();
    slwt.extend_from_slice(&build_slide_persist_atom(1));
    slwt.extend_from_slice(&build_text_header_atom(0));

    // Manually build a TextCharsAtom with length claiming 1000 bytes,
    // but only supply 10 bytes of actual data.
    let declared_len: u32 = 1000;
    let actual_data = &[0x41u8, 0x00, 0x42, 0x00, 0x43, 0x00, 0x44, 0x00, 0x45, 0x00]; // "ABCDE" in UTF-16LE
    let ver_inst: u16 = 0;
    slwt.extend_from_slice(&ver_inst.to_le_bytes());
    slwt.extend_from_slice(&rt::TEXT_CHARS_ATOM.to_le_bytes());
    slwt.extend_from_slice(&declared_len.to_le_bytes());
    slwt.extend_from_slice(actual_data);

    let doc_container = build_ppt_stream_with_slwts(&slwt, &[]);
    let doc_len = doc_container.len() as u32;

    let persist_atom = build_persist_directory_atom(&[(0, &[0])]);
    let persist_offset = doc_len;

    let user_edit = build_user_edit_atom(0, persist_offset);
    let user_edit_offset = doc_len + persist_atom.len() as u32;

    let mut ppt_stream = Vec::new();
    ppt_stream.extend_from_slice(&doc_container);
    ppt_stream.extend_from_slice(&persist_atom);
    ppt_stream.extend_from_slice(&user_edit);

    let current_user = build_current_user(user_edit_offset);

    let cfb_data = build_cfb(&[
        ("PowerPoint Document", &ppt_stream),
        ("Current User", &current_user),
    ]);

    let diag = Arc::new(CollectingDiagnostics::new());
    let result =
        PptDocument::from_bytes_with_diag(&cfb_data, diag.clone() as Arc<dyn DiagnosticsSink>);

    // Must NOT panic. May succeed with partial data or return an error.
    match result {
        Ok(doc) => {
            // If it succeeds, it should have extracted some partial content.
            assert!(
                doc.page_count() <= 1,
                "should have at most 1 slide from partial data"
            );
        }
        Err(_) => {
            // Error is also acceptable for severely malformed data.
        }
    }
}

// ---------------------------------------------------------------------------
// 4. Non-CFB data (random bytes)
// ---------------------------------------------------------------------------

#[test]
fn malformed_random_bytes_no_panic() {
    let garbage = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0xFF, 0x42, 0x13, 0x99, 0xAA];
    let result = PptDocument::from_bytes(&garbage);
    assert!(
        result.is_err(),
        "random bytes should return an error, not panic"
    );
}

// ---------------------------------------------------------------------------
// 5. Valid CFB but missing Current User stream
// ---------------------------------------------------------------------------

#[test]
fn malformed_missing_current_user() {
    let doc_container = build_container(rt::DOCUMENT, &[]);
    let cfb_data = build_cfb(&[("PowerPoint Document", &doc_container)]);

    let diag = Arc::new(CollectingDiagnostics::new());
    let result =
        PptDocument::from_bytes_with_diag(&cfb_data, diag.clone() as Arc<dyn DiagnosticsSink>);

    assert!(
        result.is_err(),
        "missing Current User should return an error"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("Current User"),
        "error should mention missing stream: {err_msg}"
    );
}

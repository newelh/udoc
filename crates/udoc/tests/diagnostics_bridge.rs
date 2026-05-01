//! regression tests for the PDF -> facade diagnostics
//! bridge.
//!
//! Before this sprint, `udoc::open_pdf_path` ignored
//! `Config::diagnostics` entirely, so PDF warnings emitted while opening
//! a file from disk were silently dropped. The bytes-based path bridged
//! diagnostics but lost the structured kind by formatting it through
//! `format!("{:?}", warn.kind)`.
//!
//! Post-fix: the facade installs a `CoreDiagBridge` on both code paths,
//! and `udoc_core::diagnostics::WarningKind` carries the PDF variants
//! as typed enum entries (no String round-trip). Tests below trigger a
//! `WarningKind::StreamLengthMismatch` via the public facade and assert
//! the typed variant survives the bridge.

use std::path::PathBuf;
use std::sync::Arc;

use udoc::{CollectingDiagnostics, Config, WarningKind};

fn wrong_length_pdf() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../udoc-pdf/tests/corpus/minimal/wrong_length.pdf")
}

#[test]
fn pdf_warnings_bridge_through_facade_path_api() {
    // wrong_length.pdf has a stream whose declared /Length disagrees with
    // the actual data. The PDF parser recovers via endstream-scanning and
    // emits WarningKind::StreamLengthMismatch.
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().diagnostics(diag.clone());
    let _doc = udoc::extract_with(wrong_length_pdf(), config)
        .expect("extract_with should recover from bad /Length");

    let warnings = diag.warnings();
    assert!(
        !warnings.is_empty(),
        "expected at least one bridged warning from PDF parse"
    );

    // Typed-variant assertion: pre-fix the kind would have been a String
    // produced by `format!("{:?}", warn.kind)`, which still happened to
    // contain "StreamLengthMismatch" but was opaque to pattern matching.
    // Post-fix `kind` is the typed core enum.
    let stream_len_hits = warnings
        .iter()
        .filter(|w| w.kind == WarningKind::StreamLengthMismatch)
        .count();
    assert!(
        stream_len_hits >= 1,
        "expected StreamLengthMismatch (typed) in warnings: {:?}",
        warnings.iter().map(|w| w.kind.as_str()).collect::<Vec<_>>()
    );
}

#[test]
fn pdf_warnings_bridge_through_facade_bytes_api() {
    // Same scenario via the in-memory entry point.
    let bytes = std::fs::read(wrong_length_pdf()).expect("read corpus pdf");
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().diagnostics(diag.clone());
    let _doc = udoc::extract_bytes_with(&bytes, config)
        .expect("extract_bytes_with should recover from bad /Length");

    let warnings = diag.warnings();
    assert!(
        warnings
            .iter()
            .any(|w| w.kind == WarningKind::StreamLengthMismatch),
        "expected StreamLengthMismatch (typed) via bytes API: {:?}",
        warnings.iter().map(|w| w.kind.as_str()).collect::<Vec<_>>()
    );
}

#[test]
fn typed_kind_supports_legacy_string_comparison() {
    // The existing tests in the workspace compare `kind == "FooBar"`. The
    // typed enum implements PartialEq<&str> against the canonical name,
    // so those tests keep working without churn.
    let bytes = std::fs::read(wrong_length_pdf()).expect("read corpus pdf");
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().diagnostics(diag.clone());
    let _doc = udoc::extract_bytes_with(&bytes, config).expect("extract_bytes_with");

    let warnings = diag.warnings();
    assert!(
        warnings.iter().any(|w| w.kind == "StreamLengthMismatch"),
        "PartialEq<&str> compat path should match the typed variant"
    );
}

#[test]
fn warning_context_page_index_survives_bridge() {
    // The bridge must propagate `WarningContext::page_index`. Some PDF
    // warnings carry it; even when None the field round-trips.
    let bytes = std::fs::read(wrong_length_pdf()).expect("read corpus pdf");
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().diagnostics(diag.clone());
    let _doc = udoc::extract_bytes_with(&bytes, config).expect("extract_bytes_with");

    // We don't pin a specific page_index value (the warning may fire
    // during xref/trailer parsing before any page is loaded); we just
    // verify the bridged warning exposes the structured WarningContext
    // type rather than an opaque blob.
    let warnings = diag.warnings();
    let bridged = warnings
        .iter()
        .find(|w| w.kind == WarningKind::StreamLengthMismatch)
        .expect("StreamLengthMismatch present");
    // Field exists and is Option<usize>; this just exercises the path.
    let _maybe_page: Option<usize> = bridged.context.page_index;
}

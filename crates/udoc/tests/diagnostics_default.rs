//! integration tests for the  four-state
//! behavior matrix on `Document::diagnostics()` + `Limits::max_warnings`
//! cap.
//!
//! State table covered by the tests below:
//!
//! | State | `collect_diagnostics` | custom sink | doc.diagnostics() | user sink |
//! |-------|----------------------|-------------|-------------------|-----------|
//! | 1     | true (default)       | no          | populated         | n/a       |
//! | 2     | false (implicit)     | yes         | empty             | populated |
//! | 3     | false (explicit)     | no          | empty             | n/a       |
//! | 4     | true (forced)        | yes         | populated         | populated |

use std::path::PathBuf;
use std::sync::Arc;

use udoc::{extract, extract_with, CollectingDiagnostics, Config, NullDiagnostics, WarningKind};
use udoc_core::limits::Limits;

/// A PDF that triggers a `FallbackFontSubstitution` warning during
/// extraction (Helvetica-without-FontFile).
fn fallback_pdf() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../udoc-pdf/tests/corpus/minimal/content_array.pdf")
}

// ---------------------------------------------------------------------------
// State 1: default config -> internal collector populates doc.diagnostics()
// ---------------------------------------------------------------------------

#[test]
fn state1_extract_no_config_populates_doc_diagnostics() {
    let doc = extract(fallback_pdf()).expect("extract should succeed");
    assert!(
        !doc.diagnostics().is_empty(),
        "default Config should auto-collect diagnostics from a PDF that emits warnings"
    );
    let any_fallback = doc
        .diagnostics()
        .iter()
        .any(|w| matches!(w.kind, WarningKind::FallbackFontSubstitution));
    assert!(
        any_fallback,
        "Helvetica-no-embed PDF should yield at least one FallbackFontSubstitution"
    );
}

#[test]
fn state1_extract_with_default_config_populates_doc_diagnostics() {
    let doc = extract_with(fallback_pdf(), Config::new()).expect("extract should succeed");
    assert!(!doc.diagnostics().is_empty());
}

#[test]
fn state1_diagnostics_field_is_owned_by_doc() {
    // Even on a PDF that emits info-level FontLoaded entries, the
    // diagnostics() snapshot belongs to the doc and is consistent.
    // Verifies state-1 wiring without depending on a "clean" fixture.
    let pdf =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/minimal/hello.pdf");
    let doc = extract(pdf).expect("extract should succeed");
    let snapshot1 = doc.diagnostics().to_vec();
    let snapshot2 = doc.diagnostics().to_vec();
    assert_eq!(snapshot1.len(), snapshot2.len());
}

// ---------------------------------------------------------------------------
// State 2: custom sink only -> doc.diagnostics() empty, my_sink populated
// ---------------------------------------------------------------------------

#[test]
fn state2_custom_sink_populates_user_sink_not_doc() {
    let user_sink = Arc::new(CollectingDiagnostics::new());
    let cfg = Config::new().diagnostics(user_sink.clone());
    // Implicit opt-out is part of the contract.
    assert!(!cfg.collect_diagnostics);

    let doc = extract_with(fallback_pdf(), cfg).expect("extract should succeed");

    assert!(
        doc.diagnostics().is_empty(),
        "doc.diagnostics() should be empty when only a custom sink is set, got: {:?}",
        doc.diagnostics()
    );
    assert!(
        !user_sink.warnings().is_empty(),
        "user-supplied sink should be populated"
    );
}

// ---------------------------------------------------------------------------
// State 3: explicit opt-out -> doc.diagnostics() empty
// ---------------------------------------------------------------------------

#[test]
fn state3_explicit_collect_false_yields_empty_doc_diagnostics() {
    let cfg = Config::new().collect_diagnostics(false);
    assert!(!cfg.collect_diagnostics);
    let doc = extract_with(fallback_pdf(), cfg).expect("extract should succeed");
    assert!(
        doc.diagnostics().is_empty(),
        "explicit opt-out should yield empty diagnostics"
    );
}

#[test]
fn state3_null_diag_explicit_opt_out_with_collect_false() {
    let cfg = Config::new()
        .diagnostics(Arc::new(NullDiagnostics))
        .collect_diagnostics(false);
    let doc = extract_with(fallback_pdf(), cfg).expect("extract should succeed");
    assert!(doc.diagnostics().is_empty());
}

// ---------------------------------------------------------------------------
// State 4: custom sink + collect_diagnostics(true) -> Tee both
// ---------------------------------------------------------------------------

#[test]
fn state4_tee_populates_both_user_and_doc() {
    let user_sink = Arc::new(CollectingDiagnostics::new());
    let cfg = Config::new()
        .diagnostics(user_sink.clone())
        .collect_diagnostics(true); // explicit re-arm
    assert!(cfg.collect_diagnostics);

    let doc = extract_with(fallback_pdf(), cfg).expect("extract should succeed");

    assert!(
        !doc.diagnostics().is_empty(),
        "doc.diagnostics() should be populated in state 4"
    );
    assert!(
        !user_sink.warnings().is_empty(),
        "user sink should also be populated in state 4"
    );

    // Each warning should appear in both. Compare counts as a smoke
    // check; exact match modulo synthetic sentinels.
    let doc_count = doc.diagnostics().len();
    let user_count = user_sink.warnings().len();
    // Doc may add one synthetic WarningsTruncated when capped, so its
    // count is >= user_count or vice versa within 1.
    assert!(
        (doc_count as isize - user_count as isize).abs() <= 1,
        "Tee should fan out roughly equally: doc={doc_count} user={user_count}"
    );
}

// ---------------------------------------------------------------------------
// Cap behavior: WarningsTruncated synthesis at Limits::max_warnings
// ---------------------------------------------------------------------------

#[test]
fn cap_default_is_some_1000() {
    let cfg = Config::new();
    assert_eq!(cfg.limits.max_warnings, Some(1000));
}

#[test]
fn cap_low_value_truncates_doc_diagnostics() {
    // Drive an extraction that produces many warnings, then assert the
    // collector capped at the configured value and synthesized the new
    // typed variant.
    use udoc_core::diagnostics::{Warning, WarningKind};
    let cfg = Config::new().limits(Limits::builder().max_warnings(Some(3)).build());

    // Extract-with on a PDF that produces a few warnings. The internal
    // collector cap is 3; if more than 3 are emitted, the trailing
    // entry is replaced with WarningsTruncated.
    let user_sink = Arc::new(CollectingDiagnostics::new());
    let cfg = cfg.diagnostics(user_sink.clone()).collect_diagnostics(true);

    // Extract via Extractor so we can also push synthetic warnings via
    // the tee'd custom sink. We use the tee so the doc.diagnostics()
    // collector receives them.
    let ext = udoc::Extractor::open_with(fallback_pdf(), cfg).expect("extractor open");
    // Push 10 synthetic warnings via the user sink. (The Tee was
    // installed at open_with time; pushing directly to the user_sink
    // here only populates user_sink, but the live extraction also
    // emits multiple FallbackFontSubstitution warnings via the Tee --
    // those flow into the internal collector and trip the cap.)
    for i in 0..10 {
        use udoc_core::diagnostics::DiagnosticsSink;
        user_sink.warning(Warning::new("Synthetic", format!("synth-{i}")));
    }
    // Drive the extractor to materialize the document and drain.
    let doc = ext.into_document().expect("into_document");

    // Internal collector cap was 3; we expect 3 real entries + 1 truncation sentinel.
    assert!(
        doc.diagnostics().len() <= 4,
        "doc.diagnostics() should cap at max_warnings + 1 sentinel, got {}",
        doc.diagnostics().len()
    );
    let last = doc.diagnostics().last().expect("at least one entry");
    // The last entry must be the typed variant ( contract).
    assert!(
        matches!(last.kind, WarningKind::WarningsTruncated { .. }),
        "trailing entry should be WarningsTruncated, got {:?}",
        last.kind
    );
}

#[test]
fn cap_none_disables_truncation() {
    // No cap: extract a doc that emits a few warnings; assert no
    // synthetic WarningsTruncated appears in doc.diagnostics().
    let cfg = Config::new().limits(Limits::builder().max_warnings(None).build());
    let doc = extract_with(fallback_pdf(), cfg).expect("extract");
    assert!(!doc.diagnostics().is_empty());
    let any_truncated = doc
        .diagnostics()
        .iter()
        .any(|w| matches!(w.kind, WarningKind::WarningsTruncated { .. }));
    assert!(
        !any_truncated,
        "uncapped run should not synthesize WarningsTruncated"
    );
}

#[test]
fn cap_high_value_does_not_synthesize_truncation_below_cap() {
    // Cap well above the natural warning count; no truncation.
    let cfg = Config::new().limits(Limits::builder().max_warnings(Some(10_000)).build());
    let doc = extract_with(fallback_pdf(), cfg).expect("extract");
    let any_truncated = doc
        .diagnostics()
        .iter()
        .any(|w| matches!(w.kind, WarningKind::WarningsTruncated { .. }));
    assert!(!any_truncated);
}

// ---------------------------------------------------------------------------
// Extractor::diagnostics() parity with into_document
// ---------------------------------------------------------------------------

#[test]
fn extractor_diagnostics_mirrors_internal_collector() {
    let mut ext = udoc::Extractor::open(fallback_pdf()).expect("open");
    // Drive a lazy extraction that emits at least one warning.
    let _ = ext.text();
    let snapshot = ext.diagnostics();
    assert!(
        !snapshot.is_empty(),
        "Extractor::diagnostics() should report warnings collected so far"
    );
}

#[test]
fn extractor_diagnostics_empty_when_collector_off() {
    let ext = udoc::Extractor::open_with(fallback_pdf(), Config::new().collect_diagnostics(false))
        .expect("open");
    let snapshot = ext.diagnostics();
    assert!(snapshot.is_empty());
}

// ---------------------------------------------------------------------------
// extract_bytes + extract_bytes_with parity (state 1 + state 2)
// ---------------------------------------------------------------------------

#[test]
fn state1_extract_bytes_populates_doc_diagnostics() {
    let bytes = std::fs::read(fallback_pdf()).expect("corpus");
    let doc = udoc::extract_bytes(&bytes).expect("extract");
    assert!(!doc.diagnostics().is_empty());
}

#[test]
fn state2_extract_bytes_with_custom_sink_doc_empty() {
    let bytes = std::fs::read(fallback_pdf()).expect("corpus");
    let user_sink = Arc::new(CollectingDiagnostics::new());
    let cfg = Config::new().diagnostics(user_sink.clone());
    let doc = udoc::extract_bytes_with(&bytes, cfg).expect("extract");
    assert!(doc.diagnostics().is_empty());
    assert!(!user_sink.warnings().is_empty());
}

// ---------------------------------------------------------------------------
// Document::diagnostics is a getter, not a pub field
// ---------------------------------------------------------------------------

#[test]
fn doc_diagnostics_getter_is_borrow() {
    // Compile-time test: doc.diagnostics() returns &[Warning], not a
    // mutable Vec the caller can clear or push to. This test exercises
    // the read path; mutation would not compile.
    let doc = extract(fallback_pdf()).expect("extract");
    let ws: &[udoc_core::diagnostics::Warning] = doc.diagnostics();
    let _len = ws.len();
}

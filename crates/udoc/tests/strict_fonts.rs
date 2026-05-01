//! M-32b: facade-level tests for `AssetConfig::strict_fonts`.
//!
//! Covers the two documented modes:
//! - Default (`assets.strict_fonts = false`): extraction succeeds despite
//!   font substitution; downstream consumers see
//!   `FallbackFontSubstitution` warnings.
//! - Strict (`assets.strict_fonts = true`): extraction aborts with a
//!   [`Error::font_fallback_required`] payload on the first non-Exact
//!   [`FontResolution`] observed.
//!
//! The flag moved from `Config::strict_fonts` to
//! `AssetConfig::strict_fonts` in.

use std::path::PathBuf;
use std::sync::Arc;

use udoc::{AssetConfig, Config, Error};
use udoc_core::diagnostics::CollectingDiagnostics;

/// A minimal PDF that references /Helvetica (a standard-14 font) without
/// embedding a FontFile. The PDF backend substitutes the built-in metrics
/// and marks every span with `FontResolution::Substituted { NameRouted }`.
fn helvetica_no_embed_pdf() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../udoc-pdf/tests/corpus/minimal/content_array.pdf")
}

#[test]
fn strict_fonts_false_allows_fallback_extraction() {
    // Use extract_bytes_with so the facade diagnostics sink is bridged
    // through to the PDF backend (open_pdf_path currently does not bridge).
    let bytes = std::fs::read(helvetica_no_embed_pdf()).expect("corpus file");
    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::new().diagnostics(diag.clone());
    // strict_fonts defaults to false; extraction must succeed and emit
    // fallback warnings rather than erroring out.
    assert!(!config.assets.strict_fonts);

    let doc = udoc::extract_bytes_with(&bytes, config)
        .expect("default config should not fail on fallback");

    assert!(!doc.content.is_empty(), "expected some extracted content");

    let warnings = diag.warnings();
    let fallbacks: Vec<_> = warnings
        .iter()
        .filter(|w| w.kind == "FallbackFontSubstitution")
        .collect();
    assert!(
        !fallbacks.is_empty(),
        "expected at least one FallbackFontSubstitution warning, got {:?}",
        warnings.iter().map(|w| &w.kind).collect::<Vec<_>>()
    );
}

#[test]
fn strict_fonts_true_returns_font_fallback_required_error() {
    let config = Config::new().assets(AssetConfig::default().strict_fonts(true));
    assert!(config.assets.strict_fonts);

    let err = udoc::extract_with(helvetica_no_embed_pdf(), config)
        .expect_err("strict mode should abort when the PDF forces a fallback");

    let info = err
        .font_fallback_info()
        .expect("error should carry a FontFallbackRequired payload");
    assert!(
        info.requested.contains("Helvetica"),
        "payload should name the requested font, got {:?}",
        info.requested
    );

    // Message should surface the requested font + reason token so callers
    // who only render `{err}` still see useful context.
    let msg = format!("{err}");
    assert!(msg.contains("Helvetica"), "message: {msg}");
    assert!(msg.contains("NameRouted"), "message: {msg}");
    assert!(
        msg.contains("extracting page"),
        "message should include page context, got {msg}"
    );
}

#[test]
fn strict_fonts_true_on_all_exact_document_succeeds() {
    // simpletype3font.pdf is Exact per the existing font_resolution tests
    // (Type3 fonts define glyphs inline; no fallback needed). Strict mode
    // must not fail for that case.
    let pdf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../udoc-pdf/tests/corpus/minimal/simpletype3font.pdf");
    let config = Config::new().assets(AssetConfig::default().strict_fonts(true));
    let doc =
        udoc::extract_with(pdf, config).expect("strict mode should accept Exact-only documents");
    assert!(
        !doc.content.is_empty(),
        "Type3 doc should still produce content"
    );
}

#[test]
fn font_fallback_info_none_for_unrelated_errors() {
    // Constructing an unrelated error via the public Error API and confirming
    // that `font_fallback_info()` does not yield false positives.
    let err = Error::new("something else");
    assert!(err.font_fallback_info().is_none());
}

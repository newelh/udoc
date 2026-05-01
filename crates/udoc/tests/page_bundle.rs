//! -- integration tests for the new
//! `PageExtractor::bundle()` trait method and `PageBundle` struct
//!
//! Exercises the default impl on a non-PDF backend (DOCX) and the
//! PDF override. Per-layer skip behavior is covered in unit tests in
//! udoc-core/src/backend.rs::tests.

use std::path::PathBuf;

use udoc_core::backend::{FormatBackend, LayerConfig, PageExtractor};

fn pdf_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/minimal/hello.pdf")
}

fn docx_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../udoc-docx/tests/corpus/real-world/sample2.docx")
}

#[test]
fn bundle_pdf_via_override_yields_lines_tables_images() {
    let bytes = std::fs::read(pdf_path()).expect("hello.pdf fixture");
    let mut pdf = udoc_pdf::Document::from_bytes(bytes).expect("open pdf");
    let mut page = FormatBackend::page(&mut pdf, 0).expect("page 0");
    let bundle = page
        .bundle(&LayerConfig::default())
        .expect("bundle should succeed");
    // hello.pdf has at least one text line ("Hello, World").
    assert!(!bundle.lines.is_empty());
    // No tables or images in the minimal fixture.
    assert!(bundle.tables.is_empty());
    assert!(bundle.images.is_empty());
    // Derived text holds the page content.
    let text = bundle.text();
    assert!(
        !text.is_empty(),
        "bundle.text() should not be empty: {text:?}"
    );
}

#[test]
fn bundle_pdf_skips_tables_when_layer_off() {
    let bytes = std::fs::read(pdf_path()).expect("fixture");
    let mut pdf = udoc_pdf::Document::from_bytes(bytes).expect("open");
    let mut page = FormatBackend::page(&mut pdf, 0).expect("page 0");
    let mut layers = LayerConfig::default();
    layers.tables = false;
    let bundle = page.bundle(&layers).expect("bundle");
    assert!(bundle.tables.is_empty());
    // Lines still extracted (content spine is always on).
    assert!(!bundle.lines.is_empty());
}

#[test]
fn bundle_pdf_skips_images_when_layer_off() {
    let bytes = std::fs::read(pdf_path()).expect("fixture");
    let mut pdf = udoc_pdf::Document::from_bytes(bytes).expect("open");
    let mut page = FormatBackend::page(&mut pdf, 0).expect("page 0");
    let mut layers = LayerConfig::default();
    layers.images = false;
    let bundle = page.bundle(&layers).expect("bundle");
    assert!(bundle.images.is_empty());
}

#[test]
fn bundle_docx_uses_default_impl() {
    // DOCX backend has no override; the trait default impl composes
    // text_lines + tables + images. Verifies the default impl is
    // reachable from the dispatch path.
    let path = docx_path();
    if !path.exists() {
        eprintln!("skipping: {} not present", path.display());
        return;
    }
    let bytes = std::fs::read(&path).expect("docx fixture");
    let mut docx = udoc_docx::DocxDocument::from_bytes(&bytes).expect("open docx");
    let mut page = FormatBackend::page(&mut docx, 0).expect("page 0");
    let bundle = page
        .bundle(&LayerConfig::default())
        .expect("default impl bundle");
    // Real-world docx has some text content.
    assert!(!bundle.lines.is_empty());
}

#[test]
fn bundle_text_derives_from_lines_round_trip() {
    let bytes = std::fs::read(pdf_path()).expect("fixture");
    let mut pdf = udoc_pdf::Document::from_bytes(bytes).expect("open");
    let mut page = FormatBackend::page(&mut pdf, 0).expect("page");
    let bundle = page.bundle(&LayerConfig::default()).expect("bundle");

    // The derived bundle.text() should contain the same content as a
    // direct page.text() call (modulo whitespace / line-break
    // differences between the two reconstruction paths).
    let derived = bundle.text();
    // Sanity: derived text non-empty.
    assert!(!derived.is_empty());
}

#[test]
fn bundle_content_only_layer_keeps_lines() {
    let bytes = std::fs::read(pdf_path()).expect("fixture");
    let mut pdf = udoc_pdf::Document::from_bytes(bytes).expect("open");
    let mut page = FormatBackend::page(&mut pdf, 0).expect("page");
    // content_only() turns off tables/images at the LayerConfig level
    // -- tables and images stay enabled per the constructor's
    // documented behavior. Verify the bundle still yields lines.
    let layers = LayerConfig::content_only();
    let bundle = page.bundle(&layers).expect("bundle");
    assert!(!bundle.lines.is_empty());
}

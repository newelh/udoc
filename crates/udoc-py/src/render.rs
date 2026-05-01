//! W2-RENDER: factored `render_page` logic shared by `PyDocument` (and,
//! once `Page` carries a document handle, by `PyPage::render`).
//!
//! The actual rasterization is done by the `udoc-render` crate. This
//! module is a thin wrapper that:
//!   1. Checks the format supports rendering (PDF only at ).
//!   2. Bounds-checks the page index against `metadata.page_count`.
//!   3. Drops the GIL and calls `udoc::render::render_page`.
//!   4. Maps any rasterizer error into the right Python exception type.
//!   5. Returns the encoded PNG bytes via `PyBytes`.
//!
//! Centralising the gate + GIL release here means new render call sites
//! (Page.render once it carries a doc handle, Corpus.render_pages, the
//! W3-PYTEST tests) get the same capability check and error mapping for
//! free.

use pyo3::exceptions::PyIndexError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyModule};

use udoc_facade::Document;

use crate::errors::{udoc_error_to_py, UnsupportedOperationError};
use crate::types::PyFormat;

/// Default DPI for `render_page` / `Page.render`. Matches
/// `udoc_render::DEFAULT_DPI` and the W1-METHODS-DOCUMENT signature.
pub const DEFAULT_DPI: u32 = 150;

/// Render a single page of a Document as PNG bytes.
///
/// `format` is the format the Python wrapper detected/forced. Only
/// [`PyFormat::Pdf`] is supported; every other format raises
/// [`UnsupportedOperationError`]. `format = None` is also unsupported
/// because we can't claim PDF without evidence (the udoc-render crate
/// would happily attempt to render any Document but only the PDF backend
/// populates the presentation overlay with the geometry the rasterizer
/// needs, and silently emitting a blank PNG would mislead callers).
///
/// `index` is the 0-based page index. Out-of-range raises `IndexError`,
/// matching the `Document.__getitem__` semantics.
///
/// `dpi` is the output resolution. Pass [`DEFAULT_DPI`] (150) for the
/// W1-METHODS-DOCUMENT default.
///
/// The actual rasterizer call is wrapped in `py.detach` so the GIL is
/// released for the duration -- a 300 DPI page can take 100 ms+ and we
/// don't want to block other Python threads on it.
pub fn render_page_bytes(
    py: Python<'_>,
    doc: &Document,
    format: Option<PyFormat>,
    index: usize,
    dpi: u32,
) -> PyResult<Py<PyBytes>> {
    if !format_can_render(format) {
        let format_name = format.map(format_name).unwrap_or("unknown");
        return Err(UnsupportedOperationError::new_err(format!(
            "render_page: format {format_name} does not support rendering; \
             only PDF is supported in this release (check format.can_render \
             before calling)"
        )));
    }
    let page_count = doc.metadata.page_count;
    if index >= page_count {
        return Err(PyIndexError::new_err(format!(
            "render_page: index {index} out of range (page_count={page_count})"
        )));
    }
    let bytes = py
        .detach(|| {
            // FontCache is per-call: pages in a hot loop will re-parse
            // shared fonts. The Corpus.render_pages path (W1-CORPUS) is
            // where a long-lived cache pays off; for one-shot
            // render_page calls the parse cost is amortized over the
            // whole-page rasterization which dwarfs it.
            let mut cache = udoc_facade::render::font_cache::FontCache::new(&doc.assets);
            udoc_facade::render::render_page(doc, index, dpi, &mut cache)
        })
        .map_err(udoc_error_to_py)?;
    Ok(PyBytes::new(py, &bytes).unbind())
}

/// `true` iff the format supports page rendering today. Mirrors
/// `PyFormat::can_render` (which is the public Python-facing accessor)
/// but takes `Option<PyFormat>` so callers can check both "no format
/// known" and "format is X" in one call.
pub fn format_can_render(format: Option<PyFormat>) -> bool {
    matches!(format, Some(PyFormat::Pdf))
}

/// Best-effort lowercase string for a `PyFormat`, used in error messages.
/// Duplicated from `document.rs` (where it was defined alongside the
/// inline render_page implementation) so this module compiles standalone;
/// `document.rs` will lose its copy when its render_page delegates here.
fn format_name(f: PyFormat) -> &'static str {
    match f {
        PyFormat::Pdf => "pdf",
        PyFormat::Docx => "docx",
        PyFormat::Xlsx => "xlsx",
        PyFormat::Pptx => "pptx",
        PyFormat::Doc => "doc",
        PyFormat::Xls => "xls",
        PyFormat::Ppt => "ppt",
        PyFormat::Odt => "odt",
        PyFormat::Ods => "ods",
        PyFormat::Odp => "odp",
        PyFormat::Rtf => "rtf",
        PyFormat::Md => "md",
    }
}

/// Module-level register hook. The W0 lib.rs scaffolding wires every
/// submodule through a `register(m)` call; this module exposes no
/// Python-visible names of its own (the helper is a Rust internal called
/// from `document.rs` / `types.rs`), so register is a no-op. Keeping the
/// hook here avoids special-casing the `#[pymodule]` registration order
/// in `lib.rs`.
pub fn register(_m: &Bound<'_, PyModule>) -> PyResult<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(all(test, not(feature = "extension-module")))]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use udoc_facade::{Block, Document, Inline, NodeId, SpanStyle};

    /// Path to the canonical 1-page PDF in the corpus. The cargo test
    /// harness runs from the workspace root, so the relative path is the
    /// same as in scripts that resolve from the repo root.
    fn hello_pdf_path() -> PathBuf {
        // CARGO_MANIFEST_DIR points at crates/udoc-py; the corpus lives
        // at the workspace root, two levels up.
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest
            .parent() // crates/
            .and_then(|p| p.parent()) // workspace root
            .expect("CARGO_MANIFEST_DIR has parents")
            .join("tests/corpus/minimal/hello.pdf")
    }

    /// Build a tiny non-PDF Document for unsupported-format tests.
    /// Format is intentionally unset; the renderer's gate also rejects
    /// `None` (we can't render without knowing the backend wrote the
    /// presentation overlay we need).
    fn make_docx_shaped_doc() -> Document {
        let mut doc = Document::new();
        doc.metadata.page_count = 1;
        doc.content.push(Block::Paragraph {
            id: NodeId::new(1),
            content: vec![Inline::Text {
                id: NodeId::new(2),
                text: "Hi".into(),
                style: SpanStyle::default(),
            }],
        });
        doc
    }

    #[test]
    fn test_render_page_pdf_returns_png_bytes() {
        // Open the canonical 1-page PDF, render page 0, and confirm the
        // result is non-empty PNG bytes (magic header check). Skips
        // gracefully if the corpus file isn't present in this build
        // tree, so the test passes in environments where the corpus is
        // not vendored (CI minimal image, etc).
        let path = hello_pdf_path();
        if !path.exists() {
            eprintln!(
                "skipping test_render_page_pdf_returns_png_bytes: \
                 corpus fixture not found at {path:?}"
            );
            return;
        }
        Python::initialize();
        Python::attach(|py| {
            let doc = udoc_facade::extract(&path).expect("extract hello.pdf");
            let png = render_page_bytes(py, &doc, Some(PyFormat::Pdf), 0, DEFAULT_DPI)
                .expect("render_page_bytes succeeds for PDF");
            let bytes = png.bind(py).as_bytes();
            assert!(!bytes.is_empty(), "PNG bytes were empty");
            // PNG magic: 89 50 4E 47 0D 0A 1A 0A.
            assert_eq!(
                &bytes[..8],
                &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
                "result is not a PNG"
            );
        });
    }

    #[test]
    fn test_render_page_unsupported_format_raises() {
        // DOCX (or any non-PDF format) must raise
        // UnsupportedOperationError -- we don't silently emit a blank
        // PNG because the Document tree has no presentation overlay
        // populated by a non-PDF backend.
        Python::initialize();
        Python::attach(|py| {
            let doc = make_docx_shaped_doc();
            let err = render_page_bytes(py, &doc, Some(PyFormat::Docx), 0, DEFAULT_DPI)
                .expect_err("DOCX render_page must raise");
            assert!(
                err.is_instance_of::<crate::errors::UnsupportedOperationError>(py),
                "expected UnsupportedOperationError, got {err:?}"
            );
            let msg = err.value(py).to_string();
            assert!(
                msg.contains("docx"),
                "error message should name the format: {msg}"
            );
        });
    }

    #[test]
    fn test_render_page_dpi_default_150() {
        // The Python-side default DPI is 150 -- a deliberate divergence
        // from the udoc-render crate's DEFAULT_DPI (300, tuned for OCR).
        // 150 is the W1-METHODS-DOCUMENT signature default and matches
        // the phase-16 plan §6.2.3 spec ("dpi=150"); 300 was a poor fit
        // for a default Python user (a single A4 page renders to ~5 MB
        // of PNG at 300 DPI, vs ~1.5 MB at 150 DPI).
        //
        // This test pins both numbers so the divergence is intentional
        // and visible: bumping the Python default changes a public API
        // contract and should be a deliberate edit here, not silent.
        assert_eq!(DEFAULT_DPI, 150, "Python default DPI is 150");
        assert_eq!(
            udoc_facade::render::DEFAULT_DPI,
            300,
            "udoc-render DEFAULT_DPI is 300 (OCR-tuned); \
             Python wrapper deliberately defaults to 150 for friendlier \
             default file sizes"
        );
    }

    #[test]
    fn test_render_page_invalid_index_raises() {
        // Out-of-range page index must raise IndexError, matching the
        // Document.__getitem__ contract. We test on a non-PDF Document
        // here; the index check fires *after* the format gate, so we
        // build a PDF-shaped Document (format = Pdf, page_count = 1)
        // without actually parsing one. The renderer is never called
        // because the index check rejects first.
        Python::initialize();
        Python::attach(|py| {
            let doc = make_docx_shaped_doc(); // page_count = 1
            let err = render_page_bytes(py, &doc, Some(PyFormat::Pdf), 99, DEFAULT_DPI)
                .expect_err("out-of-range index must raise");
            assert!(
                err.is_instance_of::<pyo3::exceptions::PyIndexError>(py),
                "expected IndexError, got {err:?}"
            );
            let msg = err.value(py).to_string();
            assert!(
                msg.contains("99") && msg.contains("page_count"),
                "error message should mention the bad index and page count: {msg}"
            );
        });
    }
}

#![allow(clippy::vec_init_then_push)]
//! W1-METHODS-DOCUMENT: Python `Document` class.
//!
//! Holds an owned Rust `udoc::Document` plus lazy caches for the
//! materialized page / block / table / image vectors. NOT frozen --
//! it carries mutable cache state.
//!
//! W1-FOUNDATION landed the struct definition + register() + a `#[new]`
//! constructor used by `convert::document_to_py`. W1-METHODS-DOCUMENT
//! (this file) fills the `#[pymethods]` block: dunders, iterators,
//! materializers, render hook.

use std::sync::OnceLock;

use pyo3::exceptions::{PyIndexError, PyRuntimeError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyList, PyTuple};
use pyo3::BoundObject;

use crate::types::{PyBlock, PyDocumentMetadata, PyFormat, PyImage, PyPage, PyTable, PyWarning};

/// The result of an extraction. Mirrors the public Rust `udoc::Document`
/// surface plus pythonic affordances.
#[pyclass(name = "Document")]
pub struct PyDocument {
    /// The owned Rust Document tree. `pub(crate)` so convert.rs can
    /// poke at it for lazy overlay lookups; not exposed to Python.
    pub(crate) inner: udoc_facade::Document,
    /// Source path the doc was extracted from, if any (None for
    /// extract_bytes()). Set by `convert::document_to_py`.
    pub(crate) source: Option<std::path::PathBuf>,
    /// Detected/forced format, if known.
    pub(crate) format: Option<PyFormat>,
    /// Cached page handles. Materialized lazily by `pages()` /
    /// `__iter__`. None until first access.
    pub(crate) cached_pages: OnceLock<Vec<Py<PyPage>>>,
    /// Cached block handles (top-level walk). Materialized lazily.
    pub(crate) cached_blocks: OnceLock<Vec<Py<PyBlock>>>,
    /// Cached warnings.
    pub(crate) cached_warnings: OnceLock<Vec<Py<PyWarning>>>,
    /// Cached metadata.
    pub(crate) cached_metadata: OnceLock<Py<PyDocumentMetadata>>,
}

impl PyDocument {
    /// Materialize page handles on first access; return cached slice on
    /// subsequent calls. Walks via `convert::page_to_py`.
    fn ensure_pages(&self, py: Python<'_>) -> PyResult<&Vec<Py<PyPage>>> {
        if let Some(p) = self.cached_pages.get() {
            return Ok(p);
        }
        let count = self.inner.metadata.page_count;
        let mut pages = Vec::with_capacity(count);
        for idx in 0..count {
            pages.push(crate::convert::page_to_py(py, &self.inner, idx)?);
        }
        // OnceLock::set returns Err if another thread won the race; that
        // race can't happen under the GIL but we tolerate it by reading
        // back the winning value.
        let _ = self.cached_pages.set(pages);
        Ok(self
            .cached_pages
            .get()
            .expect("OnceLock populated above or by racing GIL holder"))
    }

    /// Materialize top-level block handles on first access.
    fn ensure_blocks(&self, py: Python<'_>) -> PyResult<&Vec<Py<PyBlock>>> {
        if let Some(b) = self.cached_blocks.get() {
            return Ok(b);
        }
        let mut blocks = Vec::with_capacity(self.inner.content.len());
        for block in &self.inner.content {
            blocks.push(crate::convert::block_to_py(py, block, &self.inner.assets)?);
        }
        let _ = self.cached_blocks.set(blocks);
        Ok(self.cached_blocks.get().expect("OnceLock populated above"))
    }

    /// Materialize warning handles on first access.
    fn ensure_warnings(&self, py: Python<'_>) -> PyResult<&Vec<Py<PyWarning>>> {
        if let Some(w) = self.cached_warnings.get() {
            return Ok(w);
        }
        let diags = self.inner.diagnostics();
        let mut out = Vec::with_capacity(diags.len());
        for w in diags {
            out.push(crate::convert::warning_to_py(py, w)?);
        }
        let _ = self.cached_warnings.set(out);
        Ok(self
            .cached_warnings
            .get()
            .expect("OnceLock populated above"))
    }

    /// Materialize metadata handle on first access.
    fn ensure_metadata(&self, py: Python<'_>) -> PyResult<&Py<PyDocumentMetadata>> {
        if let Some(m) = self.cached_metadata.get() {
            return Ok(m);
        }
        let m = crate::convert::metadata_to_py(py, &self.inner.metadata)?;
        let _ = self.cached_metadata.set(m);
        Ok(self
            .cached_metadata
            .get()
            .expect("OnceLock populated above"))
    }
}

#[pymethods]
impl PyDocument {
    // -----------------------------------------------------------------
    // Properties.
    // -----------------------------------------------------------------

    /// Document-level metadata (title, author, page_count, ...).
    #[getter]
    fn metadata(&self, py: Python<'_>) -> PyResult<Py<PyDocumentMetadata>> {
        let m = self.ensure_metadata(py)?;
        Ok(m.clone_ref(py))
    }

    /// Detected/forced format, or None if unknown.
    #[getter]
    fn format(&self) -> Option<PyFormat> {
        self.format
    }

    /// Source path the document was extracted from, or None for in-memory.
    #[getter]
    fn source(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        match &self.source {
            Some(p) => {
                // Build a pathlib.Path so callers get the idiomatic
                // Python type rather than a raw str.
                let pathlib = py.import("pathlib")?;
                let path_cls = pathlib.getattr("Path")?;
                let s = p.to_string_lossy().into_owned();
                Ok(path_cls.call1((s,))?.unbind())
            }
            None => Ok(py.None()),
        }
    }

    /// Diagnostic warnings emitted during extraction.
    #[getter]
    fn warnings(&self, py: Python<'_>) -> PyResult<Py<PyList>> {
        let ws = self.ensure_warnings(py)?;
        let list = PyList::empty(py);
        for w in ws {
            list.append(w.clone_ref(py))?;
        }
        Ok(list.unbind())
    }

    /// `true` iff the source document declared encryption (regardless of
    /// whether decryption succeeded). Backed by the
    /// Rust accessor on `udoc::Document`.
    #[getter]
    fn is_encrypted(&self) -> bool {
        self.inner.is_encrypted()
    }

    // -----------------------------------------------------------------
    // Iteration.
    // -----------------------------------------------------------------

    /// Iterate pages in order. The full page list is materialized on the
    /// first call and cached; subsequent calls reuse the cached handles.
    fn pages(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let pages = self.ensure_pages(py)?;
        let list = PyList::empty(py);
        for p in pages {
            list.append(p.clone_ref(py))?;
        }
        Ok(list.as_any().try_iter()?.unbind().into_any())
    }

    /// Iterate the top-level blocks of the content spine.
    fn blocks(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let blocks = self.ensure_blocks(py)?;
        let list = PyList::empty(py);
        for b in blocks {
            list.append(b.clone_ref(py))?;
        }
        Ok(list.as_any().try_iter()?.unbind().into_any())
    }

    /// Iterate every `Block::Table` in the content spine, returning the
    /// underlying `Table` payload (not the wrapping block).
    fn tables(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let blocks = self.ensure_blocks(py)?;
        let list = PyList::empty(py);
        for b in blocks {
            let bref: PyRef<'_, PyBlock> = b.bind(py).borrow();
            if bref.kind == "table" {
                if let Some(ref t) = bref.table {
                    let t_clone: Py<PyTable> = t.clone_ref(py);
                    list.append(t_clone)?;
                }
            }
        }
        Ok(list.as_any().try_iter()?.unbind().into_any())
    }

    /// Iterate every image asset in the document. Each `Image` carries
    /// the encoded bytes plus dimensions; the bbox overlay is not
    /// resolved here (W1-METHODS-DOCUMENT scope keeps this O(images),
    /// not O(blocks * overlay-lookups)).
    fn images(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let list = PyList::empty(py);
        for (idx, asset) in self.inner.assets.images().iter().enumerate() {
            let py_img: Py<PyImage> = crate::convert::image_asset_to_py(
                py, /* node_id */ 0, idx, asset, /* alt_text */ None,
                /* bbox */ None,
            )?;
            list.append(py_img)?;
        }
        Ok(list.as_any().try_iter()?.unbind().into_any())
    }

    /// Yield retrieval-sized chunks over the document content. Strategy
    /// selected by `by`: `"page"`, `"heading"`, `"section"`, `"size"`,
    /// or `"semantic"`. `size` is the soft target chunk size in chars
    /// (only enforced for `"size"` and `"semantic"` strategies).
    ///
    /// Delegates to `crate::chunks::chunk_document` (W1-METHODS-CHUNKS).
    /// Unknown `by` strategies raise `ValueError`.
    #[pyo3(signature = (*, by = "heading", size = 2000))]
    fn text_chunks(&self, py: Python<'_>, by: &str, size: usize) -> PyResult<Py<PyAny>> {
        let chunks = crate::chunks::chunk_document(py, &self.inner, by, size)?;
        let list = PyList::new(py, chunks)?;
        Ok(list.as_any().try_iter()?.unbind().into_any())
    }

    // -----------------------------------------------------------------
    // Materialization.
    // -----------------------------------------------------------------

    /// Plain-text reduction of the document. Each top-level block's
    /// `Block::text()` is joined by a blank line.
    fn text(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(self.inner.content.len());
        for block in &self.inner.content {
            parts.push(block.text());
        }
        parts.join("\n\n")
    }

    /// Render the document to markdown. With `with_anchors=True`
    /// (default) every block is preceded by a citation comment so
    /// downstream chunkers can recover provenance; pass `False` for
    /// human-readable output.
    #[pyo3(signature = (*, with_anchors = true))]
    fn to_markdown(&self, py: Python<'_>, with_anchors: bool) -> String {
        // Markdown emission walks the entire document; release the GIL
        // so other Python threads keep going during the write.
        py.detach(|| {
            if with_anchors {
                udoc_facade::output::markdown::markdown_with_anchors(&self.inner)
            } else {
                udoc_facade::output::markdown::markdown(&self.inner)
            }
        })
    }

    /// Convert the document to a Python dict. Foundation implementation
    /// uses a serde_json -> json.loads roundtrip; a direct PyObject
    /// walker lands in a follow-up. The roundtrip is
    /// observable: callers see the same shape `to_json()` produces, just
    /// as a dict, and `data` bytes are replaced with `data_length` for
    /// images per the udoc-core ImageAsset serde policy.
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        // TODO(): replace the json.loads roundtrip with a direct
        // PyObject visitor over the Document tree once W1-METHODS-CONVERT
        // grows a `to_pyobject` analogue. Acceptable alpha per
        // the W1-METHODS-DOCUMENT charter.
        let json = py
            .detach(|| serde_json::to_string(&self.inner))
            .map_err(|e| PyRuntimeError::new_err(format!("to_dict: serialize: {e}")))?;
        let json_mod = py.import("json")?;
        let value = json_mod.getattr("loads")?.call1((json,))?;
        Ok(value.unbind())
    }

    /// Convert the document to a JSON string. With `pretty=True` the
    /// output is indented for human inspection; otherwise compact.
    #[pyo3(signature = (*, pretty = false))]
    fn to_json(&self, py: Python<'_>, pretty: bool) -> PyResult<String> {
        py.detach(|| {
            if pretty {
                serde_json::to_string_pretty(&self.inner)
            } else {
                serde_json::to_string(&self.inner)
            }
        })
        .map_err(|e| PyRuntimeError::new_err(format!("to_json: serialize: {e}")))
    }

    // -----------------------------------------------------------------
    // Rendering -- PDF only.
    // -----------------------------------------------------------------

    /// Render a single page to PNG bytes at the given DPI. PDF only;
    /// other formats raise `UnsupportedOperationError`.
    #[pyo3(signature = (index, *, dpi = 150))]
    fn render_page(&self, py: Python<'_>, index: usize, dpi: u32) -> PyResult<Py<PyBytes>> {
        // Capability check. We only render PDFs today: the renderer
        // operates on the udoc Document tree, but only the PDF backend
        // populates the presentation overlay with the geometry the
        // rasterizer needs. Other formats raise UnsupportedOperationError.
        let can_render = matches!(self.format, Some(PyFormat::Pdf));
        // TODO(W1-METHODS-TYPES): switch to PyFormat::can_render(self.format)
        // when that classmethod lands; until then the Pdf-only check above
        // is the documented capability gate.
        if !can_render {
            let format_name = self
                .format
                .map(format_name)
                .unwrap_or_else(|| "unknown".to_string());
            return Err(crate::errors::UnsupportedOperationError::new_err(format!(
                "render_page: format {format_name} does not support rendering"
            )));
        }
        if index >= self.inner.metadata.page_count {
            return Err(PyIndexError::new_err(format!(
                "render_page: index {index} out of range (page_count={})",
                self.inner.metadata.page_count
            )));
        }
        let bytes = py
            .detach(|| {
                let mut cache = udoc_facade::render::font_cache::FontCache::new(&self.inner.assets);
                udoc_facade::render::render_page(&self.inner, index, dpi, &mut cache)
            })
            .map_err(|e| PyRuntimeError::new_err(format!("render_page: {e}")))?;
        Ok(PyBytes::new(py, &bytes).unbind())
    }

    // -----------------------------------------------------------------
    // Convenience dunders.
    // -----------------------------------------------------------------

    /// `len(doc)` -- page count.
    fn __len__(&self) -> usize {
        self.inner.metadata.page_count
    }

    /// `doc[i]` -- page at index. Negative indices supported per Python
    /// convention. Out-of-range raises IndexError.
    fn __getitem__(&self, py: Python<'_>, idx: isize) -> PyResult<Py<PyPage>> {
        let count = self.inner.metadata.page_count;
        let resolved = if idx < 0 {
            let len = count as isize;
            let v = idx + len;
            if v < 0 {
                return Err(PyIndexError::new_err(format!(
                    "Document index {idx} out of range (len={count})"
                )));
            }
            v as usize
        } else {
            idx as usize
        };
        if resolved >= count {
            return Err(PyIndexError::new_err(format!(
                "Document index {idx} out of range (len={count})"
            )));
        }
        let pages = self.ensure_pages(py)?;
        Ok(pages[resolved].clone_ref(py))
    }

    /// `iter(doc)` -- iterate pages. Equivalent to `iter(doc.pages())`
    /// but spelled the dunder way so `for page in doc:` works.
    fn __iter__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.pages(py)
    }

    /// `repr(doc)` -- a one-line summary. Carries the format, page count,
    /// and source path if any. Title is included only when present so the
    /// repr stays readable for documents with no metadata.
    fn __repr__(&self) -> String {
        let format_str = self
            .format
            .map(format_name)
            .unwrap_or_else(|| "unknown".to_string());
        let source_str = self
            .source
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<bytes>".to_string());
        let page_count = self.inner.metadata.page_count;
        match &self.inner.metadata.title {
            Some(t) if !t.is_empty() => format!(
                "Document(format={format_str:?}, pages={page_count}, source={source_str:?}, title={t:?})"
            ),
            _ => format!(
                "Document(format={format_str:?}, pages={page_count}, source={source_str:?})"
            ),
        }
    }

    /// Rich repr protocol -- yields (name, value) tuples for the rich
    /// pretty-printer (`rich.print(doc)` / `rich.repr(doc)`). Includes
    /// format, page count, source path, encryption flag, warning count,
    /// and the embedded `DocumentMetadata` so callers can drill in.
    fn __rich_repr__<'py>(
        slf: PyRef<'py, Self>,
        py: Python<'py>,
    ) -> PyResult<Vec<Bound<'py, PyTuple>>> {
        let format_str = slf
            .format
            .map(format_name)
            .unwrap_or_else(|| "unknown".to_string());
        let source_str: Option<String> = slf.source.as_ref().map(|p| p.display().to_string());
        let warning_count = slf.inner.diagnostics().len();
        let metadata = slf.ensure_metadata(py)?.clone_ref(py);
        let mut out: Vec<Bound<'py, PyTuple>> = Vec::new();
        out.push(PyTuple::new(
            py,
            [
                "format".into_pyobject(py)?.into_any().into_bound(),
                format_str.into_pyobject(py)?.into_any().into_bound(),
            ],
        )?);
        out.push(PyTuple::new(
            py,
            [
                "pages".into_pyobject(py)?.into_any().into_bound(),
                slf.inner
                    .metadata
                    .page_count
                    .into_pyobject(py)?
                    .into_any()
                    .into_bound(),
            ],
        )?);
        out.push(PyTuple::new(
            py,
            [
                "source".into_pyobject(py)?.into_any().into_bound(),
                source_str.into_pyobject(py)?.into_any().into_bound(),
            ],
        )?);
        out.push(PyTuple::new(
            py,
            [
                "is_encrypted".into_pyobject(py)?.into_any().into_bound(),
                slf.inner
                    .is_encrypted()
                    .into_pyobject(py)?
                    .into_any()
                    .into_bound(),
            ],
        )?);
        out.push(PyTuple::new(
            py,
            [
                "warnings".into_pyobject(py)?.into_any().into_bound(),
                warning_count.into_pyobject(py)?.into_any().into_bound(),
            ],
        )?);
        out.push(PyTuple::new(
            py,
            [
                "metadata".into_pyobject(py)?.into_any().into_bound(),
                metadata.into_pyobject(py)?.into_any().into_bound(),
            ],
        )?);
        Ok(out)
    }
}

/// Best-effort string for a `PyFormat` for error messages. `PyFormat`
/// itself does not derive Debug (per  frozen pyclass shape) so we
/// hand-roll the mapping here; W1-METHODS-TYPES owns the canonical
/// `__str__` / `extension()` accessors.
fn format_name(f: PyFormat) -> String {
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
    .to_string()
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyDocument>()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests.
//
// Tests run under cargo test (the rlib half of the cdylib+rlib crate). They
// rely on `Python::initialize()` to bring up an embedded interpreter; with
// the `extension-module` feature on, libpython is dlopened lazily by the
// CPython runner that loads the .so at runtime, but for unit tests cargo
// links libpython through the rlib build path.
// ---------------------------------------------------------------------------

#[cfg(all(test, not(feature = "extension-module")))]
mod tests {
    use super::*;
    use crate::convert::document_to_py;
    use udoc_facade::{Block, Document, Inline, NodeId, SpanStyle};

    /// Build a tiny Document fixture: one heading, one paragraph, one
    /// page. No PDF parsing; deterministic shape for unit tests.
    fn make_doc() -> Document {
        let mut doc = Document::new();
        // DocumentMetadata is #[non_exhaustive]; mutate fields on the
        // default instance instead of struct-expressing it.
        doc.metadata.title = Some("Test Doc".into());
        doc.metadata.page_count = 1;
        doc.content.push(Block::Heading {
            id: NodeId::new(1),
            level: 1,
            content: vec![Inline::Text {
                id: NodeId::new(2),
                text: "Hello".into(),
                style: SpanStyle::default(),
            }],
        });
        doc.content.push(Block::Paragraph {
            id: NodeId::new(3),
            content: vec![Inline::Text {
                id: NodeId::new(4),
                text: "World".into(),
                style: SpanStyle::default(),
            }],
        });
        doc
    }

    #[test]
    fn test_document_page_count() {
        Python::initialize();
        Python::attach(|py| {
            let doc = make_doc();
            let py_doc = document_to_py(py, doc, None, None).unwrap();
            let bound = py_doc.bind(py).borrow();
            assert_eq!(bound.__len__(), 1);
        });
    }

    #[test]
    fn test_document_metadata_passthrough() {
        Python::initialize();
        Python::attach(|py| {
            let doc = make_doc();
            let py_doc = document_to_py(py, doc, None, None).unwrap();
            let bound = py_doc.bind(py).borrow();
            let meta = bound.metadata(py).unwrap();
            let meta_ref = meta.bind(py).borrow();
            assert_eq!(meta_ref.title.as_deref(), Some("Test Doc"));
            assert_eq!(meta_ref.page_count, 1);
        });
    }

    #[test]
    fn test_document_iter() {
        Python::initialize();
        Python::attach(|py| {
            let doc = make_doc();
            let py_doc = document_to_py(py, doc, None, None).unwrap();
            let bound = py_doc.bind(py).borrow();
            // pages() returns an iterator; just confirm it builds.
            let _pages_iter = bound.pages(py).unwrap();
            // The strict count check sits in test_document_dunder_len.
        });
    }

    #[test]
    fn test_document_text() {
        Python::initialize();
        Python::attach(|py| {
            let doc = make_doc();
            let py_doc = document_to_py(py, doc, None, None).unwrap();
            let bound = py_doc.bind(py).borrow();
            let text = bound.text();
            assert!(text.contains("Hello"), "text was: {text:?}");
            assert!(text.contains("World"), "text was: {text:?}");
            // Block::text() joins with "\n\n" between top-level blocks.
            assert!(text.contains("\n\n"));
        });
    }

    #[test]
    fn test_document_to_markdown() {
        Python::initialize();
        Python::attach(|py| {
            let doc = make_doc();
            let py_doc = document_to_py(py, doc, None, None).unwrap();
            let bound = py_doc.bind(py).borrow();
            // With anchors flag flipped on or off, the heading text and
            // the structural "# " marker show up. The markdown emitter
            // suppresses anchor comments entirely when the node has no
            // presentation overlay (no page assignment, no geometry) --
            // our trivial fixture has neither, so we only assert on the
            // visible markdown shape, not on `<!-- udoc:... -->`. A test
            // exercising anchor emission lives in the integration tests
            // once W2 wires up a fixture with a presentation overlay.
            let with_anchors = bound.to_markdown(py, true);
            assert!(with_anchors.contains("# Hello"), "md was: {with_anchors}");
            assert!(with_anchors.contains("World"), "md was: {with_anchors}");
            // Without anchors must be byte-identical to with_anchors here
            // because there is no overlay data to elide.
            let plain = bound.to_markdown(py, false);
            assert!(plain.contains("# Hello"));
            assert!(plain.contains("World"));
            assert_eq!(with_anchors, plain);
        });
    }

    #[test]
    fn test_document_to_dict() {
        Python::initialize();
        Python::attach(|py| {
            let doc = make_doc();
            let py_doc = document_to_py(py, doc, None, None).unwrap();
            let bound = py_doc.bind(py).borrow();
            let dict_obj = bound.to_dict(py).unwrap();
            // The result is whatever json.loads returned -- a dict with
            // the udoc Document serde shape.
            let bound_dict = dict_obj.bind(py);
            // version field is always present per the Document serde impl.
            let v = bound_dict.get_item("version").unwrap();
            let n: i64 = v.extract().unwrap();
            assert_eq!(n, 1);
        });
    }

    #[test]
    fn test_document_to_json() {
        Python::initialize();
        Python::attach(|py| {
            let doc = make_doc();
            let py_doc = document_to_py(py, doc, None, None).unwrap();
            let bound = py_doc.bind(py).borrow();
            let compact = bound.to_json(py, false).unwrap();
            assert!(compact.starts_with('{'));
            assert!(compact.contains("\"version\":1"));
            // Pretty-printed has a newline between entries.
            let pretty = bound.to_json(py, true).unwrap();
            assert!(pretty.contains('\n'));
        });
    }

    #[test]
    fn test_document_is_encrypted_default_false() {
        Python::initialize();
        Python::attach(|py| {
            let doc = make_doc();
            let py_doc = document_to_py(py, doc, None, None).unwrap();
            let bound = py_doc.bind(py).borrow();
            assert!(!bound.is_encrypted());
        });
    }

    #[test]
    fn test_document_render_page_unsupported_for_non_pdf() {
        Python::initialize();
        Python::attach(|py| {
            // No format set -> not PDF -> render_page must raise.
            let doc = make_doc();
            let py_doc = document_to_py(py, doc, None, None).unwrap();
            let bound = py_doc.bind(py).borrow();
            let err = bound.render_page(py, 0, 150).unwrap_err();
            // Confirm the right exception type.
            assert!(err.is_instance_of::<crate::errors::UnsupportedOperationError>(py));
        });
    }

    #[test]
    fn test_document_blocks_iter() {
        Python::initialize();
        Python::attach(|py| {
            let doc = make_doc();
            let py_doc = document_to_py(py, doc, None, None).unwrap();
            let bound = py_doc.bind(py).borrow();
            let blocks = bound.ensure_blocks(py).unwrap();
            assert_eq!(blocks.len(), 2);
            let kinds: Vec<String> = blocks
                .iter()
                .map(|b| b.bind(py).borrow().kind.clone())
                .collect();
            assert_eq!(kinds, vec!["heading".to_string(), "paragraph".to_string()]);
        });
    }

    #[test]
    fn test_document_dunder_len() {
        Python::initialize();
        Python::attach(|py| {
            let doc = make_doc();
            let py_doc = document_to_py(py, doc, None, None).unwrap();
            let bound = py_doc.bind(py).borrow();
            assert_eq!(bound.__len__(), 1);
        });
    }

    #[test]
    fn test_document_dunder_getitem() {
        Python::initialize();
        Python::attach(|py| {
            let doc = make_doc();
            let py_doc = document_to_py(py, doc, None, None).unwrap();
            let bound = py_doc.bind(py).borrow();
            // Positive index.
            let p0 = bound.__getitem__(py, 0).unwrap();
            assert_eq!(p0.bind(py).borrow().index, 0);
            // Negative index (Python convention).
            let pneg = bound.__getitem__(py, -1).unwrap();
            assert_eq!(pneg.bind(py).borrow().index, 0);
            // Out of range.
            assert!(bound.__getitem__(py, 5).is_err());
            assert!(bound.__getitem__(py, -2).is_err());
        });
    }

    #[test]
    fn test_document_warnings_empty() {
        Python::initialize();
        Python::attach(|py| {
            // Fresh Document has no diagnostics.
            let doc = make_doc();
            let py_doc = document_to_py(py, doc, None, None).unwrap();
            let bound = py_doc.bind(py).borrow();
            let ws = bound.warnings(py).unwrap();
            assert_eq!(ws.bind(py).len(), 0);
        });
    }

    /// `__rich_repr__` yields the metadata + summary fields. Confirms
    /// the protocol returns at least one (name, value) pair and that
    /// the metadata pyclass is among the yielded values.
    #[test]
    fn test_document_rich_repr_yields_metadata() {
        Python::initialize();
        Python::attach(|py| {
            let doc = make_doc();
            let py_doc = document_to_py(py, doc, None, None).unwrap();
            let bound = py_doc.bind(py);
            let pairs_obj = bound.call_method0("__rich_repr__").unwrap();
            let pairs: Vec<(String, Py<PyAny>)> = pairs_obj.extract().unwrap();
            assert!(!pairs.is_empty(), "rich_repr returned no pairs");
            // First pair is the format. Subsequent pairs include "pages",
            // "source", "is_encrypted", "warnings", "metadata".
            let names: Vec<&str> = pairs.iter().map(|(n, _)| n.as_str()).collect();
            assert!(names.contains(&"pages"), "names: {names:?}");
            assert!(names.contains(&"metadata"), "names: {names:?}");
            assert!(names.contains(&"is_encrypted"), "names: {names:?}");
            // The pages value should be 1 for our fixture.
            let pages_idx = names.iter().position(|n| *n == "pages").unwrap();
            let pages_val: usize = pairs[pages_idx].1.extract(py).unwrap();
            assert_eq!(pages_val, 1);
        });
    }

    #[test]
    fn test_document_repr_contains_format_and_pages() {
        Python::initialize();
        Python::attach(|py| {
            let doc = make_doc();
            let py_doc = document_to_py(py, doc, None, None).unwrap();
            let bound = py_doc.bind(py).borrow();
            let r = bound.__repr__();
            assert!(r.starts_with("Document("), "repr was: {r}");
            assert!(r.contains("pages=1"), "repr was: {r}");
            // Title ends up included since make_doc sets it.
            assert!(r.contains("Test Doc"), "repr was: {r}");
        });
    }
}

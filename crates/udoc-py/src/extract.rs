//! W1-METHODS-EXTRACT: full `extract` / `extract_bytes` / `stream` surface.
//!
//! 2. Public Python signatures:
//!
//! ```python
//! udoc.extract(path, *, pages=None, password=None, format=None,
//!              max_file_size=None, config=None, on_warning=None) -> Document
//! udoc.extract_bytes(data, *, pages=None, password=None, format=None,
//!                    max_file_size=None, config=None, on_warning=None) -> Document
//! udoc.stream(path, *, pages=None, password=None, format=None,
//!             max_file_size=None, config=None, on_warning=None) -> ExtractionContext
//! ```
//!
//! Precedence ( + spec §6.2.2): explicit kwargs always win over the
//! corresponding field on `config`. The `pages` kwarg accepts richer types
//! than the Rust `Config::pages(&str)` -- int, range, list, str spec --
//! normalized in this module before being handed to `udoc::PageRange`.
//!
//! GIL release: `py.detach(|| udoc_facade::extract_with(path, cfg))` per
//!  so the Python interpreter can run other threads while we do
//! the long native extraction.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;

use pyo3::exceptions::{PyRuntimeError, PyStopIteration, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyList, PyRange, PyString, PyTuple};

use udoc_facade::{
    CollectingDiagnostics, Config as RustConfig, DiagnosticsSink, Extractor, Format as RustFormat,
    PageRange, Warning,
};

use crate::config::PyConfig;
use crate::convert::{document_to_py, warning_to_py};
use crate::document::PyDocument;
use crate::errors::udoc_error_to_py;
use crate::types::PyFormat;

// ---------------------------------------------------------------------------
// Helpers for normalizing the rich kwarg surface into `udoc::Config`.
// ---------------------------------------------------------------------------

/// Convert a `PyFormat` enum value or a string ("pdf"/"docx"/...) into a
/// `udoc::Format`. The string variant is case-insensitive and accepts the
/// 12 backend short names.
fn coerce_format(value: &Bound<'_, PyAny>) -> PyResult<RustFormat> {
    // PyFormat uses `skip_from_py_object` so we can't auto-extract; we
    // downcast manually instead. The enum's `Clone + Copy` lets us deref
    // the borrow before returning.
    if let Ok(borrowed) = value.cast::<PyFormat>() {
        let py_fmt: PyFormat = *borrowed.borrow();
        return Ok(py_format_to_rust(py_fmt));
    }
    if let Ok(s) = value.extract::<String>() {
        return parse_format_str(&s);
    }
    Err(PyTypeError::new_err(format!(
        "format must be a Format enum or a str, got {}",
        value.get_type().name()?
    )))
}

fn py_format_to_rust(f: PyFormat) -> RustFormat {
    match f {
        PyFormat::Pdf => RustFormat::Pdf,
        PyFormat::Docx => RustFormat::Docx,
        PyFormat::Xlsx => RustFormat::Xlsx,
        PyFormat::Pptx => RustFormat::Pptx,
        PyFormat::Doc => RustFormat::Doc,
        PyFormat::Xls => RustFormat::Xls,
        PyFormat::Ppt => RustFormat::Ppt,
        PyFormat::Odt => RustFormat::Odt,
        PyFormat::Ods => RustFormat::Ods,
        PyFormat::Odp => RustFormat::Odp,
        PyFormat::Rtf => RustFormat::Rtf,
        PyFormat::Md => RustFormat::Md,
    }
}

/// Map the Python-side short string (case-insensitive) to a `RustFormat`.
/// Accepts the 12 short names plus the friendly aliases users reach for
/// without thinking ("markdown" -> Md).
fn parse_format_str(s: &str) -> PyResult<RustFormat> {
    match s.to_ascii_lowercase().as_str() {
        "pdf" => Ok(RustFormat::Pdf),
        "docx" => Ok(RustFormat::Docx),
        "xlsx" => Ok(RustFormat::Xlsx),
        "pptx" => Ok(RustFormat::Pptx),
        "doc" => Ok(RustFormat::Doc),
        "xls" => Ok(RustFormat::Xls),
        "ppt" => Ok(RustFormat::Ppt),
        "odt" => Ok(RustFormat::Odt),
        "ods" => Ok(RustFormat::Ods),
        "odp" => Ok(RustFormat::Odp),
        "rtf" => Ok(RustFormat::Rtf),
        "md" | "markdown" => Ok(RustFormat::Md),
        other => Err(PyValueError::new_err(format!(
            "unknown format string '{other}' (expected one of: pdf, docx, xlsx, pptx, doc, xls, ppt, odt, ods, odp, rtf, md)"
        ))),
    }
}

/// Normalize the polymorphic `pages` kwarg into a `udoc::PageRange`.
///
/// Accepts: `int`, `range`, `list[int]`/`Sequence[int]`, `str` (the Rust
/// spec form like "1-10,15"), or `None`. None returns Ok(None) -- caller
/// installs no page filter.
///
/// All non-string forms are 0-based indices on the Python side (matches
/// the Pythonic convention `pages=[0, 2]`); they are converted to a 1-based
/// spec string before being parsed by `PageRange::parse`. The string form
/// passes through verbatim because users typing "1-10,15" mean 1-based
/// (it matches the CLI `--pages` flag).
fn coerce_pages(value: Option<&Bound<'_, PyAny>>) -> PyResult<Option<PageRange>> {
    let Some(v) = value else { return Ok(None) };
    if v.is_none() {
        return Ok(None);
    }

    // String form -- pass through to PageRange::parse (1-based).
    if let Ok(s) = v.cast::<PyString>() {
        let spec: String = s.extract()?;
        return PageRange::parse(&spec)
            .map(Some)
            .map_err(|e| PyValueError::new_err(format!("invalid pages spec: {e}")));
    }

    // range(...) -- materialize to indices and convert to spec.
    if let Ok(r) = v.cast::<PyRange>() {
        let start: i64 = r.getattr("start")?.extract()?;
        let stop: i64 = r.getattr("stop")?.extract()?;
        let step: i64 = r.getattr("step")?.extract()?;
        if step == 0 {
            return Err(PyValueError::new_err("range() step cannot be zero"));
        }
        let mut indices: Vec<u64> = Vec::new();
        let mut cur = start;
        if step > 0 {
            while cur < stop {
                if cur < 0 {
                    return Err(PyValueError::new_err(
                        "negative page indices are not supported",
                    ));
                }
                indices.push(cur as u64);
                cur += step;
            }
        } else {
            while cur > stop {
                if cur < 0 {
                    return Err(PyValueError::new_err(
                        "negative page indices are not supported",
                    ));
                }
                indices.push(cur as u64);
                cur += step;
            }
        }
        return indices_to_page_range(&indices).map(Some);
    }

    // bool is a subclass of int -- reject explicitly so `pages=True` doesn't
    // silently mean "page 1" (a footgun).
    if v.is_instance_of::<pyo3::types::PyBool>() {
        return Err(PyTypeError::new_err(
            "pages= cannot be a bool (did you mean an int?)",
        ));
    }

    // int -- single page (0-based on the Python side).
    if let Ok(n) = v.extract::<i64>() {
        if n < 0 {
            return Err(PyValueError::new_err(
                "negative page indices are not supported",
            ));
        }
        return indices_to_page_range(&[n as u64]).map(Some);
    }

    // Sequence -- list, tuple, or any Python iterable of ints.
    if let Ok(list) = v.cast::<PyList>() {
        let mut indices = Vec::with_capacity(list.len());
        for item in list.iter() {
            let n: i64 = item
                .extract()
                .map_err(|_| PyTypeError::new_err("pages= list must contain only ints"))?;
            if n < 0 {
                return Err(PyValueError::new_err(
                    "negative page indices are not supported",
                ));
            }
            indices.push(n as u64);
        }
        return indices_to_page_range(&indices).map(Some);
    }
    if let Ok(tup) = v.cast::<PyTuple>() {
        let mut indices = Vec::with_capacity(tup.len());
        for item in tup.iter() {
            let n: i64 = item
                .extract()
                .map_err(|_| PyTypeError::new_err("pages= tuple must contain only ints"))?;
            if n < 0 {
                return Err(PyValueError::new_err(
                    "negative page indices are not supported",
                ));
            }
            indices.push(n as u64);
        }
        return indices_to_page_range(&indices).map(Some);
    }

    // Generic iterable fallback -- iter() + extract.
    if let Ok(iter) = v.try_iter() {
        let mut indices = Vec::new();
        for item in iter {
            let item = item?;
            let n: i64 = item
                .extract()
                .map_err(|_| PyTypeError::new_err("pages= iterable must yield only ints"))?;
            if n < 0 {
                return Err(PyValueError::new_err(
                    "negative page indices are not supported",
                ));
            }
            indices.push(n as u64);
        }
        return indices_to_page_range(&indices).map(Some);
    }

    Err(PyTypeError::new_err(format!(
        "pages= must be int, range, list, tuple, str, or None (got {})",
        v.get_type().name()?
    )))
}

/// Convert a slice of 0-based indices to a 1-based spec string and parse
/// via `PageRange::parse`. We compose the spec instead of constructing
/// the `PageRange` directly because the public Rust API exposes only the
/// parser, not a from-indices constructor ( frozen surface).
fn indices_to_page_range(indices_zero_based: &[u64]) -> PyResult<PageRange> {
    if indices_zero_based.is_empty() {
        return Err(PyValueError::new_err("pages= cannot be empty"));
    }
    // Compose "1,3,5,..." (1-based). The PageRange parser handles dedup
    // and sort internally so we can stream the indices without de-duping
    // here.
    let parts: Vec<String> = indices_zero_based
        .iter()
        .map(|&i| (i.saturating_add(1)).to_string())
        .collect();
    let spec = parts.join(",");
    PageRange::parse(&spec).map_err(|e| PyValueError::new_err(format!("invalid pages: {e}")))
}

/// Convert a `PyConfig` handle into a fresh `udoc::Config`.
///
/// This is a foundation-level conversion: only the fields the underlying
/// Rust `Config` actually carries are wired through. Hooks/assets/render
/// land in W1-METHODS-CONFIG (when `PyConfig::as_rust` is filled in
/// properly). For now we read the leaf primitive fields so the kwarg
/// surface works end-to-end.
fn pyconfig_to_rust(py: Python<'_>, cfg: &Py<PyConfig>) -> PyResult<RustConfig> {
    let bound = cfg.bind(py);
    let inner = bound.borrow();
    let mut rust = RustConfig::new();
    if let Some(ref pw) = inner.password {
        rust = rust.password(pw.clone());
    }
    if let Some(ref fmt_str) = inner.format {
        rust = rust.format(parse_format_str(fmt_str)?);
    }
    // Pull max_file_size out of the limits sub-pyclass. Other limit fields
    // are not yet plumbed through (W1-METHODS-CONFIG fills them later).
    // We mutate the cloned default `Limits` in place rather than naming
    // the type directly, because `udoc_core::limits::Limits` is reachable
    // through `Config::limits` as a field but not re-exported at the
    // facade's public surface.
    let limits_bound = inner.limits.bind(py);
    let limits = limits_bound.borrow();
    let mut rust_limits = rust.limits.clone();
    rust_limits.max_file_size = limits.max_file_size;
    rust = rust.limits(rust_limits);
    rust = rust.collect_diagnostics(inner.collect_diagnostics);
    Ok(rust)
}

// ---------------------------------------------------------------------------
// `on_warning` callback bridge.
// ---------------------------------------------------------------------------

/// Build the final `udoc::Config` from the kwarg matrix.
///
/// Precedence (per spec): explicit kwargs win over `config` fields. We
/// start from `config` (or `Config::default()`) and overlay each kwarg.
#[allow(clippy::too_many_arguments)]
fn build_config(
    py: Python<'_>,
    pages: Option<&Bound<'_, PyAny>>,
    password: Option<&str>,
    format: Option<&Bound<'_, PyAny>>,
    max_file_size: Option<u64>,
    config: Option<&Py<PyConfig>>,
    install_collector: bool,
) -> PyResult<(RustConfig, Option<Arc<CollectingDiagnostics>>)> {
    let mut rust = match config {
        Some(c) => pyconfig_to_rust(py, c)?,
        None => RustConfig::default(),
    };

    if let Some(p) = password {
        rust = rust.password(p);
    }
    if let Some(f) = format {
        rust = rust.format(coerce_format(f)?);
    }
    if let Some(size) = max_file_size {
        let mut new_limits = rust.limits.clone();
        new_limits.max_file_size = size;
        rust = rust.limits(new_limits);
    }
    if let Some(range) = coerce_pages(pages)? {
        rust.page_range = Some(range);
    }

    // Install our internal collector so we can ship warnings to the
    // Python `on_warning` callback after extraction returns. This is the
    // thin wrapper called out in the W1-METHODS-EXTRACT brief --
    // a streaming sink would require holding the GIL across a worker
    // thread which fights `py.detach`.
    let collector = if install_collector {
        let c = Arc::new(CollectingDiagnostics::new());
        let dyn_sink: Arc<dyn DiagnosticsSink> = c.clone();
        rust = rust.diagnostics(dyn_sink).collect_diagnostics(true);
        Some(c)
    } else {
        None
    };

    Ok((rust, collector))
}

/// After native extraction has returned, drain the collector and call the
/// Python `on_warning` callback once per collected warning. The GIL is
/// held by the caller of this function, so callbacks run safely.
fn dispatch_warnings(
    py: Python<'_>,
    callback: &Bound<'_, PyAny>,
    warnings: &[Warning],
) -> PyResult<()> {
    for w in warnings {
        let py_w = warning_to_py(py, w)?;
        callback.call1((py_w,))?;
    }
    Ok(())
}

/// Common error mapping helper.
///
/// Routes every `udoc::Error` through the typed-exception dispatch in
/// `crate::errors::udoc_error_to_py` so callers can `except
/// udoc.PasswordRequiredError as e` instead of substring-matching.
/// (W1-METHODS-EXCEPTIONS, .)
fn map_udoc_err(err: udoc_facade::Error) -> PyErr {
    udoc_error_to_py(err)
}

// ---------------------------------------------------------------------------
// `udoc.extract(path, *, ...)`
// ---------------------------------------------------------------------------

/// Extract a document from a file path.
#[pyfunction]
#[pyo3(signature = (
    path,
    *,
    pages = None,
    password = None,
    format = None,
    max_file_size = None,
    config = None,
    on_warning = None,
))]
#[allow(clippy::too_many_arguments)]
fn extract<'py>(
    py: Python<'py>,
    path: &str,
    pages: Option<&Bound<'py, PyAny>>,
    password: Option<&str>,
    format: Option<&Bound<'py, PyAny>>,
    max_file_size: Option<u64>,
    config: Option<Py<PyConfig>>,
    on_warning: Option<Bound<'py, PyAny>>,
) -> PyResult<Py<PyDocument>> {
    let path_buf = PathBuf::from(path);
    let install_collector = on_warning.is_some();
    let (rust_cfg, collector) = build_config(
        py,
        pages,
        password,
        format,
        max_file_size,
        config.as_ref(),
        install_collector,
    )?;

    // Stash the format hint before move so we can pass it to the visitor.
    // If no explicit format was passed, fall back to magic-byte detection
    // so doc.format isn't None on the success path.
    let format_hint = rust_cfg.format.or_else(|| {
        udoc_facade::detect::detect_format_path(&path_buf)
            .ok()
            .flatten()
    });

    let doc = py
        .detach(|| udoc_facade::extract_with(&path_buf, rust_cfg))
        .map_err(map_udoc_err)?;

    if let (Some(cb), Some(c)) = (on_warning.as_ref(), collector.as_ref()) {
        let ws = c.warnings();
        dispatch_warnings(py, cb, &ws)?;
    }

    document_to_py(py, doc, Some(path_buf), format_hint)
}

// ---------------------------------------------------------------------------
// `udoc.extract_bytes(data, *, ...)`
// ---------------------------------------------------------------------------

/// Extract a document from in-memory bytes.
#[pyfunction]
#[pyo3(signature = (
    data,
    *,
    pages = None,
    password = None,
    format = None,
    max_file_size = None,
    config = None,
    on_warning = None,
))]
#[allow(clippy::too_many_arguments)]
fn extract_bytes<'py>(
    py: Python<'py>,
    data: &Bound<'py, PyBytes>,
    pages: Option<&Bound<'py, PyAny>>,
    password: Option<&str>,
    format: Option<&Bound<'py, PyAny>>,
    max_file_size: Option<u64>,
    config: Option<Py<PyConfig>>,
    on_warning: Option<Bound<'py, PyAny>>,
) -> PyResult<Py<PyDocument>> {
    let install_collector = on_warning.is_some();
    let (rust_cfg, collector) = build_config(
        py,
        pages,
        password,
        format,
        max_file_size,
        config.as_ref(),
        install_collector,
    )?;
    let owned: Vec<u8> = data.as_bytes().to_vec();
    // Fall back to magic-byte detection if no explicit format hint
    // was passed.
    let format_hint = rust_cfg
        .format
        .or_else(|| udoc_facade::detect::detect_format(&owned));

    let doc = py
        .detach(|| udoc_facade::extract_bytes_with(&owned, rust_cfg))
        .map_err(map_udoc_err)?;

    if let (Some(cb), Some(c)) = (on_warning.as_ref(), collector.as_ref()) {
        let ws = c.warnings();
        dispatch_warnings(py, cb, &ws)?;
    }

    document_to_py(py, doc, None, format_hint)
}

// ---------------------------------------------------------------------------
// `udoc.detect_format(path_or_bytes)` -- magic-byte format detection.
// ---------------------------------------------------------------------------

/// Detect the document format from a path or bytes via magic-byte sniff.
///
/// Returns the detected `udoc.Format` or raises `UnsupportedFormatError`
/// if no format matches.
///
/// Accepts:
/// - a `str` or `os.PathLike` path -- routes through the path-based
///   detector that reads up to 4096 bytes from the file.
/// - a `bytes` blob -- routes through the in-memory detector.
#[pyfunction]
fn detect_format<'py>(_py: Python<'py>, source: &Bound<'py, PyAny>) -> PyResult<PyFormat> {
    let detected: Option<RustFormat> = if let Ok(b) = source.cast::<PyBytes>() {
        udoc_facade::detect::detect_format(b.as_bytes())
    } else if let Ok(s) = source.extract::<String>() {
        udoc_facade::detect::detect_format_path(std::path::Path::new(&s)).map_err(map_udoc_err)?
    } else {
        return Err(PyTypeError::new_err(
            "detect_format expects a str path or a bytes blob",
        ));
    };
    match detected {
        Some(f) => Ok(rust_format_to_py(f)),
        None => Err(crate::errors::UnsupportedFormatError::new_err(
            "could not detect document format from input",
        )),
    }
}

fn rust_format_to_py(f: RustFormat) -> PyFormat {
    match f {
        RustFormat::Pdf => PyFormat::Pdf,
        RustFormat::Docx => PyFormat::Docx,
        RustFormat::Xlsx => PyFormat::Xlsx,
        RustFormat::Pptx => PyFormat::Pptx,
        RustFormat::Rtf => PyFormat::Rtf,
        RustFormat::Md => PyFormat::Md,
        RustFormat::Doc => PyFormat::Doc,
        RustFormat::Xls => PyFormat::Xls,
        RustFormat::Ppt => PyFormat::Ppt,
        RustFormat::Odt => PyFormat::Odt,
        RustFormat::Ods => PyFormat::Ods,
        RustFormat::Odp => PyFormat::Odp,
        // RustFormat is #[non_exhaustive]; default any future variant
        // to PDF's slot until explicit support lands.
        _ => PyFormat::Pdf,
    }
}

// ---------------------------------------------------------------------------
// `udoc.stream(path, *, ...)` -- ExtractionContext factory.
// ---------------------------------------------------------------------------

/// Open a streaming extractor. Returns a context manager; use it as
/// `with udoc.stream(path) as ext: for i in range(len(ext)): ...`.
#[pyfunction]
#[pyo3(signature = (
    path,
    *,
    pages = None,
    password = None,
    format = None,
    max_file_size = None,
    config = None,
    on_warning = None,
))]
#[allow(clippy::too_many_arguments)]
fn stream<'py>(
    py: Python<'py>,
    path: &str,
    pages: Option<&Bound<'py, PyAny>>,
    password: Option<&str>,
    format: Option<&Bound<'py, PyAny>>,
    max_file_size: Option<u64>,
    config: Option<Py<PyConfig>>,
    on_warning: Option<Bound<'py, PyAny>>,
) -> PyResult<Py<PyExtractionContext>> {
    let path_buf = PathBuf::from(path);
    let install_collector = on_warning.is_some();
    let (rust_cfg, collector) = build_config(
        py,
        pages,
        password,
        format,
        max_file_size,
        config.as_ref(),
        install_collector,
    )?;
    let extractor = py
        .detach(|| Extractor::open_with(&path_buf, rust_cfg))
        .map_err(map_udoc_err)?;

    Py::new(
        py,
        PyExtractionContext {
            inner: RefCell::new(Some(extractor)),
            on_warning: on_warning.map(|cb| cb.unbind()),
            collector,
            warnings_dispatched: RefCell::new(false),
        },
    )
}

// ---------------------------------------------------------------------------
// PyExtractionContext -- the streaming context manager.
// ---------------------------------------------------------------------------

/// The context manager returned by `udoc.stream(path, ...)`.
///
/// Wraps a `udoc::Extractor` so callers can iterate pages without
/// materializing the whole document. `udoc::Extractor` is `!Sync` (it
/// holds backend-internal mutable state), so this pyclass is marked
/// `unsendable` and lives on the thread that created it.
#[pyclass(name = "ExtractionContext", unsendable)]
pub struct PyExtractionContext {
    /// The underlying Extractor. RefCell so the page methods can take
    /// `&mut Extractor` while the pyclass exposes `&self`. Option so
    /// `__exit__` / `close()` can drop the backend explicitly without
    /// waiting for Python GC.
    inner: RefCell<Option<Extractor>>,
    /// Optional Python `on_warning` callback. Fired once when the user
    /// exits the context (lazy: streaming page reads don't drain warnings
    /// per call because the underlying collector is process-global).
    on_warning: Option<Py<PyAny>>,
    /// Internal collector installed when `on_warning` was passed. None
    /// otherwise.
    collector: Option<Arc<CollectingDiagnostics>>,
    /// Whether we've already invoked `on_warning` (idempotency guard for
    /// `__exit__` + explicit `close()` overlap).
    warnings_dispatched: RefCell<bool>,
}

impl PyExtractionContext {
    fn with_extractor<R>(&self, f: impl FnOnce(&mut Extractor) -> PyResult<R>) -> PyResult<R> {
        let mut guard = self.inner.borrow_mut();
        let ext = guard.as_mut().ok_or_else(|| {
            PyRuntimeError::new_err("ExtractionContext is closed (the `with` block has exited)")
        })?;
        f(ext)
    }

    /// Fire the optional `on_warning` callback once with the snapshot of
    /// collected warnings. Called by `__exit__` and `close()`. Idempotent.
    fn dispatch_pending_warnings(&self, py: Python<'_>) -> PyResult<()> {
        if *self.warnings_dispatched.borrow() {
            return Ok(());
        }
        if let (Some(cb), Some(c)) = (self.on_warning.as_ref(), self.collector.as_ref()) {
            let ws = c.warnings();
            let cb_bound = cb.bind(py);
            dispatch_warnings(py, cb_bound, &ws)?;
        }
        *self.warnings_dispatched.borrow_mut() = true;
        Ok(())
    }
}

#[pymethods]
impl PyExtractionContext {
    /// `__enter__` -- the context manager protocol entry. Returns self;
    /// the caller binds it via `as ext:`.
    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    /// `__exit__` -- close the context. Drops the backend, fires the
    /// optional warning callback. Always returns False so exceptions
    /// raised inside the `with` block propagate.
    #[pyo3(signature = (exc_type=None, exc_val=None, exc_tb=None))]
    fn __exit__(
        &self,
        py: Python<'_>,
        exc_type: Option<Py<PyAny>>,
        exc_val: Option<Py<PyAny>>,
        exc_tb: Option<Py<PyAny>>,
    ) -> PyResult<bool> {
        let _ = (exc_type, exc_val, exc_tb);
        // Fire warnings while the extractor's collector is still around.
        let _ = self.dispatch_pending_warnings(py);
        // Drop the extractor so any backend-held resources (mmap, file
        // handles, font caches) release immediately.
        let _ = self.inner.borrow_mut().take();
        Ok(false)
    }

    /// Explicit close (mirrors `__exit__` but callable without `with`).
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        self.dispatch_pending_warnings(py)?;
        let _ = self.inner.borrow_mut().take();
        Ok(())
    }

    /// `__len__` -- alias for `page_count()` so `len(ctx)` works.
    fn __len__(&self) -> PyResult<usize> {
        self.with_extractor(|ext| Ok(ext.page_count()))
    }

    /// Page count.
    fn page_count(&self) -> PyResult<usize> {
        self.with_extractor(|ext| Ok(ext.page_count()))
    }

    /// `__iter__` -- iterate over page indices, returning an iterator that
    /// yields page text strings. We avoid yielding full Page pyclasses to
    /// keep streaming truly bounded; callers who want richer objects use
    /// the explicit `page_*` accessors.
    fn __iter__(slf: PyRef<'_, Self>) -> PyResult<Py<PyExtractionContextIter>> {
        let count = slf.page_count()?;
        Py::new(
            slf.py(),
            PyExtractionContextIter {
                ctx: slf.into(),
                index: 0,
                end: count,
            },
        )
    }

    /// Plain text for a single page.
    fn page_text(&self, index: usize) -> PyResult<String> {
        self.with_extractor(|ext| ext.page_text(index).map_err(map_udoc_err))
    }

    /// Text lines (best-effort reading order) for a single page.
    /// Returns a list of `(text, baseline, is_vertical)` tuples to keep
    /// the streaming surface dependency-free; richer pyclass shapes land
    /// in W1-METHODS-DOCUMENT alongside the document iterator.
    fn page_lines(&self, py: Python<'_>, index: usize) -> PyResult<Py<PyAny>> {
        let lines = self.with_extractor(|ext| ext.page_lines(index).map_err(map_udoc_err))?;
        let list = PyList::empty(py);
        for line in lines {
            // TextLine carries the joined text via its .text() method.
            let text = line.text();
            let tup = (text, line.baseline, line.is_vertical);
            list.append(tup)?;
        }
        Ok(list.unbind().into())
    }

    /// Raw text spans (no reading order) for a single page. Each span is
    /// returned as `(text, x, y, width, font_size)`. TextSpan exposes
    /// position via x/y/width (not a packed bbox); see
    /// `udoc_core::text::TextSpan` for the full shape.
    fn page_spans(&self, py: Python<'_>, index: usize) -> PyResult<Py<PyAny>> {
        let spans = self.with_extractor(|ext| ext.page_spans(index).map_err(map_udoc_err))?;
        let list = PyList::empty(py);
        for span in spans {
            let tup = (span.text, span.x, span.y, span.width, span.font_size);
            list.append(tup)?;
        }
        Ok(list.unbind().into())
    }

    /// Tables for a single page. Returns a list of `list[list[str]]`
    /// (rows of cell text) for streaming use; richer pyclasses land in
    /// W1-METHODS-DOCUMENT.
    fn page_tables(&self, py: Python<'_>, index: usize) -> PyResult<Py<PyAny>> {
        let tables = self.with_extractor(|ext| ext.page_tables(index).map_err(map_udoc_err))?;
        let outer = PyList::empty(py);
        for table in tables {
            let table_py = PyList::empty(py);
            for row in &table.rows {
                let row_py = PyList::empty(py);
                for cell in &row.cells {
                    row_py.append(cell.text.clone())?;
                }
                table_py.append(row_py)?;
            }
            outer.append(table_py)?;
        }
        Ok(outer.unbind().into())
    }

    /// Images for a single page. Returns a list of dicts with keys
    /// `width`, `height`, `filter`, `data`.
    fn page_images(&self, py: Python<'_>, index: usize) -> PyResult<Py<PyAny>> {
        let images = self.with_extractor(|ext| ext.page_images(index).map_err(map_udoc_err))?;
        let list = PyList::empty(py);
        for image in images {
            let dict = pyo3::types::PyDict::new(py);
            dict.set_item("width", image.width)?;
            dict.set_item("height", image.height)?;
            dict.set_item("filter", format!("{:?}", image.filter))?;
            dict.set_item("data", PyBytes::new(py, &image.data))?;
            list.append(dict)?;
        }
        Ok(list.unbind().into())
    }
}

/// Iterator companion for `PyExtractionContext.__iter__`.
#[pyclass(unsendable)]
pub struct PyExtractionContextIter {
    ctx: Py<PyExtractionContext>,
    index: usize,
    end: usize,
}

#[pymethods]
impl PyExtractionContextIter {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&mut self, py: Python<'_>) -> PyResult<String> {
        if self.index >= self.end {
            return Err(PyStopIteration::new_err(()));
        }
        let i = self.index;
        self.index += 1;
        let ctx = self.ctx.bind(py).borrow();
        ctx.page_text(i)
    }
}

// ---------------------------------------------------------------------------
// Module registration.
// ---------------------------------------------------------------------------

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyExtractionContext>()?;
    m.add_class::<PyExtractionContextIter>()?;
    m.add_function(wrap_pyfunction!(extract, m)?)?;
    m.add_function(wrap_pyfunction!(extract_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(stream, m)?)?;
    m.add_function(wrap_pyfunction!(detect_format, m)?)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, not(feature = "extension-module")))]
mod tests {
    //! Rust-side unit tests for the kwarg normalization helpers.
    //!
    //! Caveat: the W0 scaffold ships `pyo3 = { features = ["extension-module",
    //! "abi3-py310"] }` -- `extension-module` deliberately does NOT link
    //! libpython at build time (Python loads us at runtime), so any test
    //! that constructs a `Bound<'_, PyAny>` or calls `Python::attach` fails
    //! to LINK as a unit-test binary even before it runs. The
    //! `crate-type = ["cdylib"]`-only workaround was reverted in
    //! W0-PYO3-SCAFFOLD when `rlib` was added back to support `cargo test`,
    //! and the `auto-initialize` feature that would re-enable embedded-Python
    //! linking is not yet wired (it belongs in W0).
    //!
    //! What this means for W1-METHODS-EXTRACT: end-to-end tests of
    //! `extract` / `extract_bytes` / `stream` live on the Python side
    //! (W3-PYTEST -- 85+ pytest tests against `import udoc`). Rust-side
    //! tests focus on the **GIL-free helpers**: format string parsing,
    //! 0-based-to-1-based page index conversion, and the
    //! `py_format_to_rust` mapping. These are the routines most likely to
    //! drift; pinning them at the Rust seam keeps the kwarg surface stable
    //! even when the W3 pytest gate hasn't run yet.
    //!
    //! When W0 is amended to add a `test-py = ["pyo3/auto-initialize"]`
    //! feature gate (or an explicit `dev-dependencies` re-import of pyo3
    //! without `extension-module`), the broader integration tests can
    //! land here without churning the helper set.

    // Cherry-pick only the GIL-free items. PyFormat is referenced only
    // by name (no method calls) so its Drop/PyClass machinery doesn't
    // monomorphize into the test binary.
    use super::{PageRange, RustFormat};

    // The kwarg-normalization helpers (`parse_format_str`,
    // `indices_to_page_range`, `coerce_pages`) all return `PyResult<...>`,
    // which monomorphizes `pyo3::err::err_state::raise_lazy` and brings
    // `PyErr_SetString` etc into the test binary's link surface. With the
    // current Cargo.toml (`extension-module` + abi3 + no `auto-initialize`
    // dev-dep), libpython is not linked, so those symbols stay unresolved
    // and `cargo test --lib` fails before running anything.
    //
    // We therefore mirror the helpers as test-local pure-Rust equivalents
    // and exercise THEM. The mirrored versions use `Result<T, String>`
    // instead of `PyResult`, so no pyo3 ffi symbol is pulled in. Drift
    // risk is low: each mirror is two lines of plumbing around the same
    // underlying udoc / `&str`-matching logic that the production code
    // wraps. When the W0 test scaffolding lands (`auto-initialize`
    // feature flag), the production helpers can be tested directly and
    // these mirrors deleted.

    /// Pure-Rust mirror of `parse_format_str`. Identical match arms.
    fn mirror_parse_format_str(s: &str) -> Result<RustFormat, String> {
        match s.to_ascii_lowercase().as_str() {
            "pdf" => Ok(RustFormat::Pdf),
            "docx" => Ok(RustFormat::Docx),
            "xlsx" => Ok(RustFormat::Xlsx),
            "pptx" => Ok(RustFormat::Pptx),
            "doc" => Ok(RustFormat::Doc),
            "xls" => Ok(RustFormat::Xls),
            "ppt" => Ok(RustFormat::Ppt),
            "odt" => Ok(RustFormat::Odt),
            "ods" => Ok(RustFormat::Ods),
            "odp" => Ok(RustFormat::Odp),
            "rtf" => Ok(RustFormat::Rtf),
            "md" | "markdown" => Ok(RustFormat::Md),
            other => Err(format!(
                "unknown format string '{other}' (expected one of: pdf, docx, xlsx, pptx, doc, xls, ppt, odt, ods, odp, rtf, md)"
            )),
        }
    }

    /// Pure-Rust mirror of `indices_to_page_range`.
    fn mirror_indices_to_page_range(indices: &[u64]) -> Result<PageRange, String> {
        if indices.is_empty() {
            return Err("pages= cannot be empty".into());
        }
        let parts: Vec<String> = indices
            .iter()
            .map(|&i| (i.saturating_add(1)).to_string())
            .collect();
        let spec = parts.join(",");
        PageRange::parse(&spec).map_err(|e| format!("invalid pages: {e}"))
    }

    // -- pages= normalization --------------------------------------------

    #[test]
    fn pages_int_zero_yields_page_one() {
        // 0-based int 0 -> "1" -> indices [0]. The 1-based offset is the
        // contract that the Python `pages=0` shorthand promises.
        let r = mirror_indices_to_page_range(&[0]).expect("ok");
        assert_eq!(r.len(), 1);
        assert!(r.contains(0));
        assert!(!r.contains(1));
    }

    #[test]
    fn pages_range_three_yields_first_three_pages() {
        // simulating pages=range(3): [0,1,2] -> "1,2,3".
        let r = mirror_indices_to_page_range(&[0, 1, 2]).expect("ok");
        assert_eq!(r.len(), 3);
        assert!(r.contains(0));
        assert!(r.contains(1));
        assert!(r.contains(2));
        assert!(!r.contains(3));
    }

    #[test]
    fn pages_list_explicit_indices_round_trip() {
        // pages=[0, 2, 5] -> "1,3,6".
        let r = mirror_indices_to_page_range(&[0, 2, 5]).expect("ok");
        assert_eq!(r.len(), 3);
        assert!(r.contains(0));
        assert!(!r.contains(1));
        assert!(r.contains(2));
        assert!(r.contains(5));
    }

    #[test]
    fn pages_str_form_passes_through_to_page_range() {
        // pages="1,3,5-7" goes straight to the Rust parser (1-based).
        let r = PageRange::parse("1,3,5-7").expect("ok");
        assert_eq!(r.len(), 5);
        assert!(r.contains(0)); // page 1
        assert!(!r.contains(1)); // page 2
        assert!(r.contains(2)); // page 3
        assert!(r.contains(4)); // page 5
        assert!(r.contains(5)); // page 6
        assert!(r.contains(6)); // page 7
    }

    #[test]
    fn pages_empty_indices_errors() {
        // The Python wrapper rejects empty pages= so we surface a clear
        // error rather than silently extracting "all pages".
        let err = mirror_indices_to_page_range(&[]).expect_err("empty should error");
        assert!(err.contains("empty"), "got: {err}");
    }

    // -- format= round-trip ----------------------------------------------

    #[test]
    fn format_str_round_trips_to_rust() {
        // Spot-check the three names users reach for first.
        assert_eq!(
            mirror_parse_format_str("pdf").expect("pdf"),
            RustFormat::Pdf
        );
        assert_eq!(
            mirror_parse_format_str("docx").expect("docx"),
            RustFormat::Docx
        );
        assert_eq!(mirror_parse_format_str("md").expect("md"), RustFormat::Md);
    }

    #[test]
    fn format_str_is_case_insensitive() {
        // "DOCX" and "Pdf" should both work -- Python users mix cases.
        assert_eq!(
            mirror_parse_format_str("DOCX").expect("DOCX"),
            RustFormat::Docx
        );
        assert_eq!(
            mirror_parse_format_str("Pdf").expect("Pdf"),
            RustFormat::Pdf
        );
    }

    #[test]
    fn format_str_markdown_alias_works() {
        // Domain-Expert nudge: users type `format="markdown"` because
        // that's how the format names itself everywhere else.
        assert_eq!(
            mirror_parse_format_str("markdown").expect("markdown"),
            RustFormat::Md
        );
    }

    #[test]
    fn format_str_unknown_errors() {
        let err = mirror_parse_format_str("bogus").expect_err("bogus format");
        assert!(err.contains("unknown format"), "got: {err}");
    }

    #[test]
    fn pages_dedup_and_sort() {
        // Make sure PageRange::parse dedupes and sorts the spec we hand
        // it. This also implicitly verifies that the int/list code path
        // is order-tolerant: pages=[5, 1, 3, 1] ends up with three
        // unique pages in ascending order.
        let r = mirror_indices_to_page_range(&[5, 1, 3, 1]).expect("ok");
        assert_eq!(r.len(), 3);
        let collected: Vec<usize> = r.iter().collect();
        assert_eq!(collected, vec![1, 3, 5]);
    }
}

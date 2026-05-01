//! W1-METHODS-CONFIG: Python `Config` + sub-configs + presets.
//!
//! All shapes are `#[pyclass(frozen, get_all)]` with
//! `__match_args__`, a `__dataclass_fields__` shim, and `__reduce__` so
//! workers can pickle Config across the ProcessPoolExecutor boundary
//!
//! Pickle contract: every config class implements `__reduce__` returning
//! `(cls, (kwargs_dict,))`. The matching `__new__` accepts either a
//! single positional dict (the pickle reconstruction path) or per-field
//! kwargs (the user-facing path). Round-trip is verified by the tests at
//! the bottom of this file.
//!
//! Presets land here as `#[classmethod]` constructors on `PyConfig`
//! per  §6.2.2:
//!   * `Config.default()` -- sensible defaults (mirrors `udoc::Config::default`).
//!   * `Config.agent()`   -- markdown-friendly, chunkable, all overlays on.
//!   * `Config.batch()`   -- throughput: skip images, lower limits, no rendering.
//!   * `Config.ocr()`     -- font extraction on, hooks ready, rendering on.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple, PyType};
use pyo3::BoundObject;

// ---------------------------------------------------------------------------
// __rich_repr__ helper
// ---------------------------------------------------------------------------

/// Build a Python `(name, value)` tuple for the rich library's
/// `__rich_repr__` protocol.
fn rich_pair<'py, V>(py: Python<'py>, name: &str, value: V) -> PyResult<Bound<'py, PyTuple>>
where
    V: IntoPyObject<'py>,
{
    let name_obj = name.into_pyobject(py)?.into_any().into_bound();
    let value_obj = value
        .into_pyobject(py)
        .map_err(Into::into)?
        .into_any()
        .into_bound();
    PyTuple::new(py, [name_obj, value_obj])
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Pull a value from a kwargs dict, returning `None` if the key is missing
/// OR the entry is Python `None`.
///
/// pyo3 0.28's `FromPyObject` carries an associated error type that may
/// not be `PyErr` for every implementation; the `.map_err(Into::into)`
/// boilerplate is what the trait docs recommend at every `?` site.
fn extract_kwarg<T>(state: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<T>>
where
    T: for<'a, 'py> FromPyObject<'a, 'py>,
{
    match state.get_item(key)? {
        Some(v) if v.is_none() => Ok(None),
        Some(v) => Ok(Some(v.extract::<T>().map_err(Into::into)?)),
        None => Ok(None),
    }
}

/// Attach a `__dataclass_fields__` dict to a class so
/// `dataclasses.fields(obj)` and `dataclasses.asdict(obj)` work.
///
/// `dataclasses.fields()` walks `__dataclass_fields__.values()` and keeps
/// entries whose `_field_type is dataclasses._FIELD`. We call
/// `dataclasses.field()` once per name and patch in `name`, `type`, and
/// `_field_type` afterwards (the same dance the real dataclass machinery
/// does during class creation).
///
/// the shim is attached at module-register time so importers
/// don't pay the cost on every Config construction.
fn install_dataclass_fields(cls: &Bound<'_, PyType>, field_names: &[&str]) -> PyResult<()> {
    let py = cls.py();
    let dataclasses = py.import("dataclasses")?;
    let field_factory = dataclasses.getattr("field")?;
    let missing = dataclasses.getattr("MISSING")?;
    let field_type_attr = dataclasses.getattr("_FIELD")?;

    let fields_dict = PyDict::new(py);
    for name in field_names {
        let kwargs = PyDict::new(py);
        kwargs.set_item("default", &missing)?;
        let f = field_factory.call((), Some(&kwargs))?;
        f.setattr("name", *name)?;
        f.setattr("type", "Any")?;
        f.setattr("_field_type", &field_type_attr)?;
        fields_dict.set_item(*name, f)?;
    }
    cls.setattr("__dataclass_fields__", fields_dict)?;
    Ok(())
}

/// Format one field of a `@dataclass`-style repr: `name=repr(value)`.
fn repr_field(name: &str, value: Bound<'_, PyAny>) -> PyResult<String> {
    let s = value.repr()?.extract::<String>()?;
    Ok(format!("{name}={s}"))
}

/// Parse a format string into the facade `Format` enum. Mirrors the
/// CLI's `--input-format` argument.
fn parse_format(s: &str) -> PyResult<udoc_facade::Format> {
    match s.to_ascii_lowercase().as_str() {
        "pdf" => Ok(udoc_facade::Format::Pdf),
        "docx" => Ok(udoc_facade::Format::Docx),
        "xlsx" => Ok(udoc_facade::Format::Xlsx),
        "pptx" => Ok(udoc_facade::Format::Pptx),
        "doc" => Ok(udoc_facade::Format::Doc),
        "xls" => Ok(udoc_facade::Format::Xls),
        "ppt" => Ok(udoc_facade::Format::Ppt),
        "odt" => Ok(udoc_facade::Format::Odt),
        "ods" => Ok(udoc_facade::Format::Ods),
        "odp" => Ok(udoc_facade::Format::Odp),
        "rtf" => Ok(udoc_facade::Format::Rtf),
        "md" | "markdown" => Ok(udoc_facade::Format::Md),
        other => Err(PyValueError::new_err(format!(
            "unknown format {other:?}; expected one of pdf, docx, xlsx, pptx, doc, xls, ppt, odt, ods, odp, rtf, md"
        ))),
    }
}

// ---------------------------------------------------------------------------
// PyLimits
// ---------------------------------------------------------------------------

/// Resource limits for extraction. Mirrors `udoc_core::limits::Limits`.
#[pyclass(name = "Limits", module = "udoc", frozen, get_all)]
pub struct PyLimits {
    pub max_file_size: u64,
    pub max_pages: usize,
    pub max_nesting_depth: usize,
    pub max_table_rows: usize,
    pub max_cells_per_row: usize,
    pub max_text_length: usize,
    pub max_styles: usize,
    pub max_style_depth: usize,
    pub max_images: usize,
    pub max_decompressed_size: u64,
    pub max_warnings: Option<usize>,
    pub memory_budget: Option<usize>,
}

impl PyLimits {
    const FIELDS: &'static [&'static str] = &[
        "max_file_size",
        "max_pages",
        "max_nesting_depth",
        "max_table_rows",
        "max_cells_per_row",
        "max_text_length",
        "max_styles",
        "max_style_depth",
        "max_images",
        "max_decompressed_size",
        "max_warnings",
        "memory_budget",
    ];

    fn from_rust(l: &udoc_core::limits::Limits) -> Self {
        Self {
            max_file_size: l.max_file_size,
            max_pages: l.max_pages,
            max_nesting_depth: l.max_nesting_depth,
            max_table_rows: l.max_table_rows,
            max_cells_per_row: l.max_cells_per_row,
            max_text_length: l.max_text_length,
            max_styles: l.max_styles,
            max_style_depth: l.max_style_depth,
            max_images: l.max_images,
            max_decompressed_size: l.max_decompressed_size,
            max_warnings: l.max_warnings,
            memory_budget: l.memory_budget,
        }
    }

    /// Convert to the Rust `Limits` type. `Limits` is `#[non_exhaustive]`;
    /// start from `Default::default()` and mutate each public field.
    pub(crate) fn as_rust(&self) -> udoc_core::limits::Limits {
        let mut out = udoc_core::limits::Limits::default();
        out.max_file_size = self.max_file_size;
        out.max_pages = self.max_pages;
        out.max_nesting_depth = self.max_nesting_depth;
        out.max_table_rows = self.max_table_rows;
        out.max_cells_per_row = self.max_cells_per_row;
        out.max_text_length = self.max_text_length;
        out.max_styles = self.max_styles;
        out.max_style_depth = self.max_style_depth;
        out.max_images = self.max_images;
        out.max_decompressed_size = self.max_decompressed_size;
        out.max_warnings = self.max_warnings;
        out.memory_budget = self.memory_budget;
        out
    }
}

#[pymethods]
impl PyLimits {
    #[classattr]
    const __match_args__: (&'static str, &'static str) = ("max_file_size", "max_pages");

    #[new]
    #[pyo3(signature = (
        state=None,
        /,
        *,
        max_file_size=None,
        max_pages=None,
        max_nesting_depth=None,
        max_table_rows=None,
        max_cells_per_row=None,
        max_text_length=None,
        max_styles=None,
        max_style_depth=None,
        max_images=None,
        max_decompressed_size=None,
        max_warnings=None::<Option<usize>>,
        memory_budget=None::<Option<usize>>,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        state: Option<&Bound<'_, PyAny>>,
        max_file_size: Option<u64>,
        max_pages: Option<usize>,
        max_nesting_depth: Option<usize>,
        max_table_rows: Option<usize>,
        max_cells_per_row: Option<usize>,
        max_text_length: Option<usize>,
        max_styles: Option<usize>,
        max_style_depth: Option<usize>,
        max_images: Option<usize>,
        max_decompressed_size: Option<u64>,
        max_warnings: Option<Option<usize>>,
        memory_budget: Option<Option<usize>>,
    ) -> PyResult<Self> {
        let defaults = udoc_core::limits::Limits::default();
        if let Some(state) = state {
            let dict = state
                .cast::<PyDict>()
                .map_err(|_| PyValueError::new_err("Limits state must be a dict (pickle path)"))?;
            return Ok(Self {
                max_file_size: extract_kwarg::<u64>(dict, "max_file_size")?
                    .unwrap_or(defaults.max_file_size),
                max_pages: extract_kwarg::<usize>(dict, "max_pages")?.unwrap_or(defaults.max_pages),
                max_nesting_depth: extract_kwarg::<usize>(dict, "max_nesting_depth")?
                    .unwrap_or(defaults.max_nesting_depth),
                max_table_rows: extract_kwarg::<usize>(dict, "max_table_rows")?
                    .unwrap_or(defaults.max_table_rows),
                max_cells_per_row: extract_kwarg::<usize>(dict, "max_cells_per_row")?
                    .unwrap_or(defaults.max_cells_per_row),
                max_text_length: extract_kwarg::<usize>(dict, "max_text_length")?
                    .unwrap_or(defaults.max_text_length),
                max_styles: extract_kwarg::<usize>(dict, "max_styles")?
                    .unwrap_or(defaults.max_styles),
                max_style_depth: extract_kwarg::<usize>(dict, "max_style_depth")?
                    .unwrap_or(defaults.max_style_depth),
                max_images: extract_kwarg::<usize>(dict, "max_images")?
                    .unwrap_or(defaults.max_images),
                max_decompressed_size: extract_kwarg::<u64>(dict, "max_decompressed_size")?
                    .unwrap_or(defaults.max_decompressed_size),
                max_warnings: match dict.get_item("max_warnings")? {
                    Some(v) if v.is_none() => None,
                    Some(v) => Some(v.extract::<usize>()?),
                    None => defaults.max_warnings,
                },
                memory_budget: match dict.get_item("memory_budget")? {
                    Some(v) if v.is_none() => None,
                    Some(v) => Some(v.extract::<usize>()?),
                    None => defaults.memory_budget,
                },
            });
        }
        Ok(Self {
            max_file_size: max_file_size.unwrap_or(defaults.max_file_size),
            max_pages: max_pages.unwrap_or(defaults.max_pages),
            max_nesting_depth: max_nesting_depth.unwrap_or(defaults.max_nesting_depth),
            max_table_rows: max_table_rows.unwrap_or(defaults.max_table_rows),
            max_cells_per_row: max_cells_per_row.unwrap_or(defaults.max_cells_per_row),
            max_text_length: max_text_length.unwrap_or(defaults.max_text_length),
            max_styles: max_styles.unwrap_or(defaults.max_styles),
            max_style_depth: max_style_depth.unwrap_or(defaults.max_style_depth),
            max_images: max_images.unwrap_or(defaults.max_images),
            max_decompressed_size: max_decompressed_size.unwrap_or(defaults.max_decompressed_size),
            max_warnings: max_warnings.unwrap_or(defaults.max_warnings),
            memory_budget: memory_budget.unwrap_or(defaults.memory_budget),
        })
    }

    fn __reduce__<'py>(slf: PyRef<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let dict = PyDict::new(py);
        dict.set_item("max_file_size", slf.max_file_size)?;
        dict.set_item("max_pages", slf.max_pages)?;
        dict.set_item("max_nesting_depth", slf.max_nesting_depth)?;
        dict.set_item("max_table_rows", slf.max_table_rows)?;
        dict.set_item("max_cells_per_row", slf.max_cells_per_row)?;
        dict.set_item("max_text_length", slf.max_text_length)?;
        dict.set_item("max_styles", slf.max_styles)?;
        dict.set_item("max_style_depth", slf.max_style_depth)?;
        dict.set_item("max_images", slf.max_images)?;
        dict.set_item("max_decompressed_size", slf.max_decompressed_size)?;
        dict.set_item("max_warnings", slf.max_warnings)?;
        dict.set_item("memory_budget", slf.memory_budget)?;
        let cls = py.get_type::<PyLimits>();
        let args = PyTuple::new(py, [dict.into_any()])?;
        PyTuple::new(py, [cls.into_any(), args.into_any()])
    }

    fn __repr__(slf: PyRef<'_, Self>, py: Python<'_>) -> PyResult<String> {
        let parts = [
            repr_field(
                "max_file_size",
                slf.max_file_size.into_pyobject(py)?.into_any(),
            )?,
            repr_field("max_pages", slf.max_pages.into_pyobject(py)?.into_any())?,
            repr_field(
                "max_nesting_depth",
                slf.max_nesting_depth.into_pyobject(py)?.into_any(),
            )?,
            repr_field(
                "max_table_rows",
                slf.max_table_rows.into_pyobject(py)?.into_any(),
            )?,
            repr_field(
                "max_cells_per_row",
                slf.max_cells_per_row.into_pyobject(py)?.into_any(),
            )?,
            repr_field(
                "max_text_length",
                slf.max_text_length.into_pyobject(py)?.into_any(),
            )?,
            repr_field("max_styles", slf.max_styles.into_pyobject(py)?.into_any())?,
            repr_field(
                "max_style_depth",
                slf.max_style_depth.into_pyobject(py)?.into_any(),
            )?,
            repr_field("max_images", slf.max_images.into_pyobject(py)?.into_any())?,
            repr_field(
                "max_decompressed_size",
                slf.max_decompressed_size.into_pyobject(py)?.into_any(),
            )?,
            repr_field(
                "max_warnings",
                slf.max_warnings.into_pyobject(py)?.into_any(),
            )?,
            repr_field(
                "memory_budget",
                slf.memory_budget.into_pyobject(py)?.into_any(),
            )?,
        ];
        Ok(format!("Limits({})", parts.join(", ")))
    }

    /// Rich repr protocol -- yields (name, value) tuples for `rich`'s
    /// pretty-printer. Includes every field so callers see the full
    /// limits table at a glance.
    fn __rich_repr__<'py>(
        slf: PyRef<'py, Self>,
        py: Python<'py>,
    ) -> PyResult<Vec<Bound<'py, PyTuple>>> {
        Ok(vec![
            rich_pair(py, "max_file_size", slf.max_file_size)?,
            rich_pair(py, "max_pages", slf.max_pages)?,
            rich_pair(py, "max_nesting_depth", slf.max_nesting_depth)?,
            rich_pair(py, "max_table_rows", slf.max_table_rows)?,
            rich_pair(py, "max_cells_per_row", slf.max_cells_per_row)?,
            rich_pair(py, "max_text_length", slf.max_text_length)?,
            rich_pair(py, "max_styles", slf.max_styles)?,
            rich_pair(py, "max_style_depth", slf.max_style_depth)?,
            rich_pair(py, "max_images", slf.max_images)?,
            rich_pair(py, "max_decompressed_size", slf.max_decompressed_size)?,
            rich_pair(py, "max_warnings", slf.max_warnings)?,
            rich_pair(py, "memory_budget", slf.memory_budget)?,
        ])
    }
}

// ---------------------------------------------------------------------------
// PyHooks
// ---------------------------------------------------------------------------

/// Hooks (OCR, layout, annotate) configuration.
///
/// Each phase is described by a shell command plus an optional timeout
/// (seconds). None means "no hook for this phase".
#[pyclass(name = "Hooks", module = "udoc", frozen, get_all)]
pub struct PyHooks {
    pub ocr: Option<String>,
    pub layout: Option<String>,
    pub annotate: Option<String>,
    pub timeout: Option<u64>,
}

impl PyHooks {
    const FIELDS: &'static [&'static str] = &["ocr", "layout", "annotate", "timeout"];
}

#[pymethods]
impl PyHooks {
    #[classattr]
    const __match_args__: (&'static str, &'static str, &'static str) =
        ("ocr", "layout", "annotate");

    #[new]
    #[pyo3(signature = (
        state=None,
        /,
        *,
        ocr=None::<Option<String>>,
        layout=None::<Option<String>>,
        annotate=None::<Option<String>>,
        timeout=None::<Option<u64>>,
    ))]
    fn new(
        state: Option<&Bound<'_, PyAny>>,
        ocr: Option<Option<String>>,
        layout: Option<Option<String>>,
        annotate: Option<Option<String>>,
        timeout: Option<Option<u64>>,
    ) -> PyResult<Self> {
        if let Some(state) = state {
            let dict = state
                .cast::<PyDict>()
                .map_err(|_| PyValueError::new_err("Hooks state must be a dict (pickle path)"))?;
            return Ok(Self {
                ocr: match dict.get_item("ocr")? {
                    Some(v) if v.is_none() => None,
                    Some(v) => Some(v.extract::<String>()?),
                    None => None,
                },
                layout: match dict.get_item("layout")? {
                    Some(v) if v.is_none() => None,
                    Some(v) => Some(v.extract::<String>()?),
                    None => None,
                },
                annotate: match dict.get_item("annotate")? {
                    Some(v) if v.is_none() => None,
                    Some(v) => Some(v.extract::<String>()?),
                    None => None,
                },
                timeout: match dict.get_item("timeout")? {
                    Some(v) if v.is_none() => None,
                    Some(v) => Some(v.extract::<u64>()?),
                    None => None,
                },
            });
        }
        Ok(Self {
            ocr: ocr.unwrap_or(None),
            layout: layout.unwrap_or(None),
            annotate: annotate.unwrap_or(None),
            timeout: timeout.unwrap_or(None),
        })
    }

    fn __reduce__<'py>(slf: PyRef<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let dict = PyDict::new(py);
        dict.set_item("ocr", slf.ocr.clone())?;
        dict.set_item("layout", slf.layout.clone())?;
        dict.set_item("annotate", slf.annotate.clone())?;
        dict.set_item("timeout", slf.timeout)?;
        let cls = py.get_type::<PyHooks>();
        let args = PyTuple::new(py, [dict.into_any()])?;
        PyTuple::new(py, [cls.into_any(), args.into_any()])
    }

    fn __repr__(slf: PyRef<'_, Self>, py: Python<'_>) -> PyResult<String> {
        let parts = [
            repr_field("ocr", slf.ocr.clone().into_pyobject(py)?.into_any())?,
            repr_field("layout", slf.layout.clone().into_pyobject(py)?.into_any())?,
            repr_field(
                "annotate",
                slf.annotate.clone().into_pyobject(py)?.into_any(),
            )?,
            repr_field("timeout", slf.timeout.into_pyobject(py)?.into_any())?,
        ];
        Ok(format!("Hooks({})", parts.join(", ")))
    }

    /// Rich repr protocol.
    fn __rich_repr__<'py>(
        slf: PyRef<'py, Self>,
        py: Python<'py>,
    ) -> PyResult<Vec<Bound<'py, PyTuple>>> {
        Ok(vec![
            rich_pair(py, "ocr", slf.ocr.clone())?,
            rich_pair(py, "layout", slf.layout.clone())?,
            rich_pair(py, "annotate", slf.annotate.clone())?,
            rich_pair(py, "timeout", slf.timeout)?,
        ])
    }
}

// ---------------------------------------------------------------------------
// PyAssetConfig
// ---------------------------------------------------------------------------

/// Asset extraction toggles. Mirrors `udoc::AssetConfig`.
#[pyclass(name = "AssetConfig", module = "udoc", frozen, get_all)]
pub struct PyAssetConfig {
    pub images: bool,
    pub fonts: bool,
    pub strict_fonts: bool,
}

impl PyAssetConfig {
    const FIELDS: &'static [&'static str] = &["images", "fonts", "strict_fonts"];

    fn from_rust(a: &udoc_facade::AssetConfig) -> Self {
        Self {
            images: a.images,
            fonts: a.fonts,
            strict_fonts: a.strict_fonts,
        }
    }

    pub(crate) fn as_rust(&self) -> udoc_facade::AssetConfig {
        // AssetConfig is #[non_exhaustive]; mutate fields off the default.
        let mut out = udoc_facade::AssetConfig::default();
        out.images = self.images;
        out.fonts = self.fonts;
        out.strict_fonts = self.strict_fonts;
        out
    }
}

#[pymethods]
impl PyAssetConfig {
    #[classattr]
    const __match_args__: (&'static str, &'static str, &'static str) =
        ("images", "fonts", "strict_fonts");

    #[new]
    #[pyo3(signature = (state=None, /, *, images=None, fonts=None, strict_fonts=None))]
    fn new(
        state: Option<&Bound<'_, PyAny>>,
        images: Option<bool>,
        fonts: Option<bool>,
        strict_fonts: Option<bool>,
    ) -> PyResult<Self> {
        let defaults = udoc_facade::AssetConfig::default();
        if let Some(state) = state {
            let dict = state.cast::<PyDict>().map_err(|_| {
                PyValueError::new_err("AssetConfig state must be a dict (pickle path)")
            })?;
            return Ok(Self {
                images: extract_kwarg::<bool>(dict, "images")?.unwrap_or(defaults.images),
                fonts: extract_kwarg::<bool>(dict, "fonts")?.unwrap_or(defaults.fonts),
                strict_fonts: extract_kwarg::<bool>(dict, "strict_fonts")?
                    .unwrap_or(defaults.strict_fonts),
            });
        }
        Ok(Self {
            images: images.unwrap_or(defaults.images),
            fonts: fonts.unwrap_or(defaults.fonts),
            strict_fonts: strict_fonts.unwrap_or(defaults.strict_fonts),
        })
    }

    fn __reduce__<'py>(slf: PyRef<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let dict = PyDict::new(py);
        dict.set_item("images", slf.images)?;
        dict.set_item("fonts", slf.fonts)?;
        dict.set_item("strict_fonts", slf.strict_fonts)?;
        let cls = py.get_type::<PyAssetConfig>();
        let args = PyTuple::new(py, [dict.into_any()])?;
        PyTuple::new(py, [cls.into_any(), args.into_any()])
    }

    fn __repr__(slf: PyRef<'_, Self>) -> String {
        format!(
            "AssetConfig(images={}, fonts={}, strict_fonts={})",
            if slf.images { "True" } else { "False" },
            if slf.fonts { "True" } else { "False" },
            if slf.strict_fonts { "True" } else { "False" },
        )
    }

    /// Rich repr protocol.
    fn __rich_repr__<'py>(
        slf: PyRef<'py, Self>,
        py: Python<'py>,
    ) -> PyResult<Vec<Bound<'py, PyTuple>>> {
        Ok(vec![
            rich_pair(py, "images", slf.images)?,
            rich_pair(py, "fonts", slf.fonts)?,
            rich_pair(py, "strict_fonts", slf.strict_fonts)?,
        ])
    }
}

// ---------------------------------------------------------------------------
// PyLayerConfig
// ---------------------------------------------------------------------------

/// Layer toggles. Mirrors `udoc::LayerConfig`.
///
/// W1-FOUNDATION laid down three fields (`presentation`, `relationships`,
/// `interactions`); the Rust `LayerConfig` also has `tables` and `images`
/// but those are content-spine concerns. The `as_rust()` conversion fills
/// them from `LayerConfig::default()`.
#[pyclass(name = "LayerConfig", module = "udoc", frozen, get_all)]
pub struct PyLayerConfig {
    pub presentation: bool,
    pub relationships: bool,
    pub interactions: bool,
}

impl PyLayerConfig {
    const FIELDS: &'static [&'static str] = &["presentation", "relationships", "interactions"];

    fn from_rust(l: &udoc_facade::LayerConfig) -> Self {
        Self {
            presentation: l.presentation,
            relationships: l.relationships,
            interactions: l.interactions,
        }
    }

    pub(crate) fn as_rust(&self) -> udoc_facade::LayerConfig {
        // LayerConfig is #[non_exhaustive]; mutate off the default so
        // future-added fields keep their sensible defaults.
        let mut out = udoc_facade::LayerConfig::default();
        out.presentation = self.presentation;
        out.relationships = self.relationships;
        out.interactions = self.interactions;
        out
    }
}

#[pymethods]
impl PyLayerConfig {
    #[classattr]
    const __match_args__: (&'static str, &'static str, &'static str) =
        ("presentation", "relationships", "interactions");

    #[new]
    #[pyo3(signature = (state=None, /, *, presentation=None, relationships=None, interactions=None))]
    fn new(
        state: Option<&Bound<'_, PyAny>>,
        presentation: Option<bool>,
        relationships: Option<bool>,
        interactions: Option<bool>,
    ) -> PyResult<Self> {
        let defaults = udoc_facade::LayerConfig::default();
        if let Some(state) = state {
            let dict = state.cast::<PyDict>().map_err(|_| {
                PyValueError::new_err("LayerConfig state must be a dict (pickle path)")
            })?;
            return Ok(Self {
                presentation: extract_kwarg::<bool>(dict, "presentation")?
                    .unwrap_or(defaults.presentation),
                relationships: extract_kwarg::<bool>(dict, "relationships")?
                    .unwrap_or(defaults.relationships),
                interactions: extract_kwarg::<bool>(dict, "interactions")?
                    .unwrap_or(defaults.interactions),
            });
        }
        Ok(Self {
            presentation: presentation.unwrap_or(defaults.presentation),
            relationships: relationships.unwrap_or(defaults.relationships),
            interactions: interactions.unwrap_or(defaults.interactions),
        })
    }

    fn __reduce__<'py>(slf: PyRef<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let dict = PyDict::new(py);
        dict.set_item("presentation", slf.presentation)?;
        dict.set_item("relationships", slf.relationships)?;
        dict.set_item("interactions", slf.interactions)?;
        let cls = py.get_type::<PyLayerConfig>();
        let args = PyTuple::new(py, [dict.into_any()])?;
        PyTuple::new(py, [cls.into_any(), args.into_any()])
    }

    fn __repr__(slf: PyRef<'_, Self>) -> String {
        format!(
            "LayerConfig(presentation={}, relationships={}, interactions={})",
            if slf.presentation { "True" } else { "False" },
            if slf.relationships { "True" } else { "False" },
            if slf.interactions { "True" } else { "False" },
        )
    }

    /// Rich repr protocol.
    fn __rich_repr__<'py>(
        slf: PyRef<'py, Self>,
        py: Python<'py>,
    ) -> PyResult<Vec<Bound<'py, PyTuple>>> {
        Ok(vec![
            rich_pair(py, "presentation", slf.presentation)?,
            rich_pair(py, "relationships", slf.relationships)?,
            rich_pair(py, "interactions", slf.interactions)?,
        ])
    }
}

// ---------------------------------------------------------------------------
// PyRenderConfig
// ---------------------------------------------------------------------------

/// Render pipeline configuration. Used by `Document.render_page()` and
/// `Page.render()`.
///
/// `profile` is one of `"ocr_friendly"` | `"visual"` (mirrors
/// `udoc_render::RenderingProfile`). `dpi` is the default DPI when the
/// caller doesn't pass one explicitly.
#[pyclass(name = "RenderConfig", module = "udoc", frozen, get_all)]
pub struct PyRenderConfig {
    pub dpi: u32,
    pub profile: String,
}

impl PyRenderConfig {
    const FIELDS: &'static [&'static str] = &["dpi", "profile"];
    const DEFAULT_DPI: u32 = 150;
    const DEFAULT_PROFILE: &'static str = "ocr_friendly";

    pub(crate) fn as_rust(&self) -> PyResult<udoc_facade::config::RenderingProfile> {
        match self.profile.as_str() {
            "ocr_friendly" => Ok(udoc_facade::config::RenderingProfile::OcrFriendly),
            "visual" => Ok(udoc_facade::config::RenderingProfile::Visual),
            other => Err(PyValueError::new_err(format!(
                "unknown rendering profile {other:?}; expected 'ocr_friendly' or 'visual'"
            ))),
        }
    }
}

#[pymethods]
impl PyRenderConfig {
    #[classattr]
    const __match_args__: (&'static str, &'static str) = ("dpi", "profile");

    #[new]
    #[pyo3(signature = (state=None, /, *, dpi=None, profile=None::<String>))]
    fn new(
        state: Option<&Bound<'_, PyAny>>,
        dpi: Option<u32>,
        profile: Option<String>,
    ) -> PyResult<Self> {
        if let Some(state) = state {
            let dict = state.cast::<PyDict>().map_err(|_| {
                PyValueError::new_err("RenderConfig state must be a dict (pickle path)")
            })?;
            let out = Self {
                dpi: extract_kwarg::<u32>(dict, "dpi")?.unwrap_or(Self::DEFAULT_DPI),
                profile: extract_kwarg::<String>(dict, "profile")?
                    .unwrap_or_else(|| Self::DEFAULT_PROFILE.to_string()),
            };
            // Validate even on the pickle path: unknown profile strings
            // should never reach the renderer.
            let _ = out.as_rust()?;
            return Ok(out);
        }
        let out = Self {
            dpi: dpi.unwrap_or(Self::DEFAULT_DPI),
            profile: profile.unwrap_or_else(|| Self::DEFAULT_PROFILE.to_string()),
        };
        let _ = out.as_rust()?;
        Ok(out)
    }

    fn __reduce__<'py>(slf: PyRef<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let dict = PyDict::new(py);
        dict.set_item("dpi", slf.dpi)?;
        dict.set_item("profile", slf.profile.clone())?;
        let cls = py.get_type::<PyRenderConfig>();
        let args = PyTuple::new(py, [dict.into_any()])?;
        PyTuple::new(py, [cls.into_any(), args.into_any()])
    }

    fn __repr__(slf: PyRef<'_, Self>) -> String {
        format!("RenderConfig(dpi={}, profile={:?})", slf.dpi, slf.profile)
    }

    /// Rich repr protocol.
    fn __rich_repr__<'py>(
        slf: PyRef<'py, Self>,
        py: Python<'py>,
    ) -> PyResult<Vec<Bound<'py, PyTuple>>> {
        Ok(vec![
            rich_pair(py, "dpi", slf.dpi)?,
            rich_pair(py, "profile", slf.profile.clone())?,
        ])
    }
}

// ---------------------------------------------------------------------------
// PyConfig
// ---------------------------------------------------------------------------

/// The top-level configuration bag passed to `extract`.
///
/// Mirrors the Rust `udoc::Config`. 2 the Python
/// shape is "frozen dataclass-like": fields are read-only, mutate via
/// `dataclasses.replace(cfg, ...)` or the named presets.
#[pyclass(name = "Config", module = "udoc", frozen, get_all)]
pub struct PyConfig {
    pub limits: Py<PyLimits>,
    pub hooks: Py<PyHooks>,
    pub assets: Py<PyAssetConfig>,
    pub layers: Py<PyLayerConfig>,
    pub rendering: Py<PyRenderConfig>,
    pub strict_fonts: bool,
    pub memory_budget: Option<usize>,
    /// Force a format (bypass detection). One of "pdf", "docx", ... or None.
    pub format: Option<String>,
    pub password: Option<String>,
    /// Whether the facade auto-collects diagnostics into `doc.diagnostics`
    ///. False if the caller wires their own sink.
    pub collect_diagnostics: bool,
}

impl PyConfig {
    const FIELDS: &'static [&'static str] = &[
        "limits",
        "hooks",
        "assets",
        "layers",
        "rendering",
        "strict_fonts",
        "memory_budget",
        "format",
        "password",
        "collect_diagnostics",
    ];

    /// Build a `PyConfig` from a fully-customized facade `Config`. Used
    /// by the preset classmethods.
    fn from_rust(py: Python<'_>, cfg: &udoc_facade::Config) -> PyResult<Self> {
        let limits = Py::new(py, PyLimits::from_rust(&cfg.limits))?;
        let hooks = Py::new(
            py,
            PyHooks {
                ocr: None,
                layout: None,
                annotate: None,
                timeout: None,
            },
        )?;
        let assets = Py::new(py, PyAssetConfig::from_rust(&cfg.assets))?;
        let layers = Py::new(py, PyLayerConfig::from_rust(&cfg.layers))?;
        let rendering = Py::new(
            py,
            PyRenderConfig {
                dpi: PyRenderConfig::DEFAULT_DPI,
                profile: match cfg.rendering_profile {
                    udoc_facade::config::RenderingProfile::OcrFriendly => {
                        "ocr_friendly".to_string()
                    }
                    udoc_facade::config::RenderingProfile::Visual => "visual".to_string(),
                },
            },
        )?;
        Ok(Self {
            limits,
            hooks,
            assets,
            layers,
            rendering,
            strict_fonts: cfg.assets.strict_fonts,
            memory_budget: cfg.limits.memory_budget,
            format: cfg.format.map(|f| f.extension().to_string()),
            password: cfg.password.clone(),
            collect_diagnostics: cfg.collect_diagnostics,
        })
    }

    /// Build the Rust `udoc::Config` consumed by the extractor. Single
    /// conversion point: W1-METHODS-EXTRACT calls
    /// `udoc_facade::extract_with(path, cfg.as_rust(py)?)`.
    pub(crate) fn as_rust(&self, py: Python<'_>) -> PyResult<udoc_facade::Config> {
        let mut out = udoc_facade::Config::default();
        out.limits = self.limits.borrow(py).as_rust();
        out.assets = self.assets.borrow(py).as_rust();
        // Top-level strict_fonts shortcut: more permissive of the two wins.
        if self.strict_fonts {
            out.assets.strict_fonts = true;
        }
        // Top-level memory_budget shadows the limits field. Mirrors CLI.
        if let Some(b) = self.memory_budget {
            out.limits.memory_budget = Some(b);
        }
        out.layers = self.layers.borrow(py).as_rust();
        out.rendering_profile = self.rendering.borrow(py).as_rust()?;
        out.collect_diagnostics = self.collect_diagnostics;
        if let Some(pw) = &self.password {
            out.password = Some(pw.clone());
        }
        if let Some(fmt_str) = &self.format {
            out.format = Some(parse_format(fmt_str)?);
        }
        Ok(out)
    }
}

#[pymethods]
impl PyConfig {
    #[classattr]
    const __match_args__: (&'static str, &'static str, &'static str) =
        ("limits", "hooks", "layers");

    #[new]
    #[pyo3(signature = (
        state=None,
        /,
        *,
        limits=None,
        hooks=None,
        assets=None,
        layers=None,
        rendering=None,
        strict_fonts=None,
        memory_budget=None::<Option<usize>>,
        format=None::<Option<String>>,
        password=None::<Option<String>>,
        collect_diagnostics=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        py: Python<'_>,
        state: Option<&Bound<'_, PyAny>>,
        limits: Option<Py<PyLimits>>,
        hooks: Option<Py<PyHooks>>,
        assets: Option<Py<PyAssetConfig>>,
        layers: Option<Py<PyLayerConfig>>,
        rendering: Option<Py<PyRenderConfig>>,
        strict_fonts: Option<bool>,
        memory_budget: Option<Option<usize>>,
        format: Option<Option<String>>,
        password: Option<Option<String>>,
        collect_diagnostics: Option<bool>,
    ) -> PyResult<Self> {
        if let Some(state) = state {
            let dict = state
                .cast::<PyDict>()
                .map_err(|_| PyValueError::new_err("Config state must be a dict (pickle path)"))?;
            let limits = match dict.get_item("limits")? {
                Some(v) => v.extract::<Py<PyLimits>>()?,
                None => Py::new(
                    py,
                    PyLimits::from_rust(&udoc_core::limits::Limits::default()),
                )?,
            };
            let hooks = match dict.get_item("hooks")? {
                Some(v) => v.extract::<Py<PyHooks>>()?,
                None => Py::new(
                    py,
                    PyHooks {
                        ocr: None,
                        layout: None,
                        annotate: None,
                        timeout: None,
                    },
                )?,
            };
            let assets = match dict.get_item("assets")? {
                Some(v) => v.extract::<Py<PyAssetConfig>>()?,
                None => Py::new(
                    py,
                    PyAssetConfig::from_rust(&udoc_facade::AssetConfig::default()),
                )?,
            };
            let layers = match dict.get_item("layers")? {
                Some(v) => v.extract::<Py<PyLayerConfig>>()?,
                None => Py::new(
                    py,
                    PyLayerConfig::from_rust(&udoc_facade::LayerConfig::default()),
                )?,
            };
            let rendering = match dict.get_item("rendering")? {
                Some(v) => v.extract::<Py<PyRenderConfig>>()?,
                None => Py::new(
                    py,
                    PyRenderConfig {
                        dpi: PyRenderConfig::DEFAULT_DPI,
                        profile: PyRenderConfig::DEFAULT_PROFILE.to_string(),
                    },
                )?,
            };
            return Ok(Self {
                limits,
                hooks,
                assets,
                layers,
                rendering,
                strict_fonts: extract_kwarg::<bool>(dict, "strict_fonts")?.unwrap_or(false),
                memory_budget: match dict.get_item("memory_budget")? {
                    Some(v) if v.is_none() => None,
                    Some(v) => Some(v.extract::<usize>()?),
                    None => None,
                },
                format: match dict.get_item("format")? {
                    Some(v) if v.is_none() => None,
                    Some(v) => Some(v.extract::<String>()?),
                    None => None,
                },
                password: match dict.get_item("password")? {
                    Some(v) if v.is_none() => None,
                    Some(v) => Some(v.extract::<String>()?),
                    None => None,
                },
                collect_diagnostics: extract_kwarg::<bool>(dict, "collect_diagnostics")?
                    .unwrap_or(true),
            });
        }

        // Kwargs path: fill from Config::default() and override per kwarg.
        let defaults = udoc_facade::Config::default();
        let limits = match limits {
            Some(l) => l,
            None => Py::new(py, PyLimits::from_rust(&defaults.limits))?,
        };
        let hooks = match hooks {
            Some(h) => h,
            None => Py::new(
                py,
                PyHooks {
                    ocr: None,
                    layout: None,
                    annotate: None,
                    timeout: None,
                },
            )?,
        };
        let assets = match assets {
            Some(a) => a,
            None => Py::new(py, PyAssetConfig::from_rust(&defaults.assets))?,
        };
        let layers = match layers {
            Some(l) => l,
            None => Py::new(py, PyLayerConfig::from_rust(&defaults.layers))?,
        };
        let rendering = match rendering {
            Some(r) => r,
            None => Py::new(
                py,
                PyRenderConfig {
                    dpi: PyRenderConfig::DEFAULT_DPI,
                    profile: PyRenderConfig::DEFAULT_PROFILE.to_string(),
                },
            )?,
        };
        Ok(Self {
            limits,
            hooks,
            assets,
            layers,
            rendering,
            strict_fonts: strict_fonts.unwrap_or(defaults.assets.strict_fonts),
            memory_budget: memory_budget.unwrap_or(defaults.limits.memory_budget),
            format: format.unwrap_or(None),
            password: password.unwrap_or(defaults.password.clone()),
            collect_diagnostics: collect_diagnostics.unwrap_or(defaults.collect_diagnostics),
        })
    }

    /// Sensible defaults; mirrors `udoc::Config::default`.
    #[classmethod]
    fn default(_cls: &Bound<'_, PyType>, py: Python<'_>) -> PyResult<Self> {
        Self::from_rust(py, &udoc_facade::Config::default())
    }

    /// "Agent" preset: markdown-friendly, chunkable, all overlays on.
    ///
    /// Tuned for "feed extracted docs into an LLM ingest pipeline".
    /// Defaults already match this shape ("all overlays on, images on,
    /// fonts off"); pinned as a separate function so future tweaks
    /// don't bleed into the default surface.
    #[classmethod]
    fn agent(_cls: &Bound<'_, PyType>, py: Python<'_>) -> PyResult<Self> {
        Self::from_rust(py, &udoc_facade::Config::default())
    }

    /// "Batch" preset: throughput-oriented.
    ///
    ///   * Skip image extraction (largest single cost in PDF, ~19%).
    ///   * Skip presentation / relationships / interactions overlays.
    ///   * Keep tables on (cheap, common batch use case).
    ///   * Diagnostics still auto-collected so failures surface.
    #[classmethod]
    fn batch(_cls: &Bound<'_, PyType>, py: Python<'_>) -> PyResult<Self> {
        let mut cfg = udoc_facade::Config::default();
        cfg.assets.images = false;
        cfg.layers.presentation = false;
        cfg.layers.relationships = false;
        cfg.layers.interactions = false;
        cfg.layers.images = false;
        Self::from_rust(py, &cfg)
    }

    /// "OCR" preset: hooks ready, font extraction on, rendering enabled.
    ///
    ///   * `assets.fonts = true` so the OCR hook can re-rasterize from
    ///     the embedded font program if needed.
    ///   * Rendering profile = OcrFriendly (already the default but pinned
    ///     here for clarity).
    ///   * `Hooks` left empty so the caller plugs in their own command
    ///     via `dataclasses.replace(Config.ocr(), hooks=Hooks(ocr="..."))`.
    #[classmethod]
    fn ocr(_cls: &Bound<'_, PyType>, py: Python<'_>) -> PyResult<Self> {
        let mut cfg = udoc_facade::Config::default();
        cfg.assets.fonts = true;
        cfg.rendering_profile = udoc_facade::config::RenderingProfile::OcrFriendly;
        Self::from_rust(py, &cfg)
    }

    // ---- Pickle / repr ---------------------------------------------------

    fn __reduce__<'py>(slf: PyRef<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let dict = PyDict::new(py);
        dict.set_item("limits", slf.limits.clone_ref(py))?;
        dict.set_item("hooks", slf.hooks.clone_ref(py))?;
        dict.set_item("assets", slf.assets.clone_ref(py))?;
        dict.set_item("layers", slf.layers.clone_ref(py))?;
        dict.set_item("rendering", slf.rendering.clone_ref(py))?;
        dict.set_item("strict_fonts", slf.strict_fonts)?;
        dict.set_item("memory_budget", slf.memory_budget)?;
        dict.set_item("format", slf.format.clone())?;
        dict.set_item("password", slf.password.clone())?;
        dict.set_item("collect_diagnostics", slf.collect_diagnostics)?;
        let cls = py.get_type::<PyConfig>();
        let args = PyTuple::new(py, [dict.into_any()])?;
        PyTuple::new(py, [cls.into_any(), args.into_any()])
    }

    fn __repr__(slf: PyRef<'_, Self>, py: Python<'_>) -> PyResult<String> {
        let parts = [
            repr_field(
                "limits",
                slf.limits.clone_ref(py).into_pyobject(py)?.into_any(),
            )?,
            repr_field(
                "hooks",
                slf.hooks.clone_ref(py).into_pyobject(py)?.into_any(),
            )?,
            repr_field(
                "assets",
                slf.assets.clone_ref(py).into_pyobject(py)?.into_any(),
            )?,
            repr_field(
                "layers",
                slf.layers.clone_ref(py).into_pyobject(py)?.into_any(),
            )?,
            repr_field(
                "rendering",
                slf.rendering.clone_ref(py).into_pyobject(py)?.into_any(),
            )?,
            format!(
                "strict_fonts={}",
                if slf.strict_fonts { "True" } else { "False" }
            ),
            repr_field(
                "memory_budget",
                slf.memory_budget.into_pyobject(py)?.into_any(),
            )?,
            repr_field("format", slf.format.clone().into_pyobject(py)?.into_any())?,
            // Mask password in repr to avoid leaking secrets in logs (see
            // `udoc::Config`'s Debug impl for the same policy).
            format!(
                "password={}",
                if slf.password.is_some() {
                    "'***'"
                } else {
                    "None"
                }
            ),
            format!(
                "collect_diagnostics={}",
                if slf.collect_diagnostics {
                    "True"
                } else {
                    "False"
                }
            ),
        ];
        Ok(format!("Config({})", parts.join(", ")))
    }

    /// Rich repr protocol -- yields (name, value) tuples for rich's
    /// pretty-printer. Sub-configs are passed through as their own
    /// pyclass instances so rich can recurse into them. Password is
    /// masked to `'***'` for parity with `__repr__` (DEC matches the
    /// Rust `udoc::Config` Debug impl which also masks the field).
    fn __rich_repr__<'py>(
        slf: PyRef<'py, Self>,
        py: Python<'py>,
    ) -> PyResult<Vec<Bound<'py, PyTuple>>> {
        let masked_password: Option<&'static str> = slf.password.as_ref().map(|_| "***");
        Ok(vec![
            rich_pair(py, "limits", slf.limits.clone_ref(py))?,
            rich_pair(py, "hooks", slf.hooks.clone_ref(py))?,
            rich_pair(py, "assets", slf.assets.clone_ref(py))?,
            rich_pair(py, "layers", slf.layers.clone_ref(py))?,
            rich_pair(py, "rendering", slf.rendering.clone_ref(py))?,
            rich_pair(py, "strict_fonts", slf.strict_fonts)?,
            rich_pair(py, "memory_budget", slf.memory_budget)?,
            rich_pair(py, "format", slf.format.clone())?,
            rich_pair(py, "password", masked_password)?,
            rich_pair(py, "collect_diagnostics", slf.collect_diagnostics)?,
        ])
    }
}

// ---------------------------------------------------------------------------
// Module registration
// ---------------------------------------------------------------------------

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyLimits>()?;
    m.add_class::<PyHooks>()?;
    m.add_class::<PyAssetConfig>()?;
    m.add_class::<PyLayerConfig>()?;
    m.add_class::<PyRenderConfig>()?;
    m.add_class::<PyConfig>()?;

    // Attach the `__dataclass_fields__` shim so `dataclasses.fields(obj)`
    // and `dataclasses.asdict(obj)` work on every config pyclass.
    install_dataclass_fields(
        &m.getattr("Limits")?.cast_into::<PyType>()?,
        PyLimits::FIELDS,
    )?;
    install_dataclass_fields(&m.getattr("Hooks")?.cast_into::<PyType>()?, PyHooks::FIELDS)?;
    install_dataclass_fields(
        &m.getattr("AssetConfig")?.cast_into::<PyType>()?,
        PyAssetConfig::FIELDS,
    )?;
    install_dataclass_fields(
        &m.getattr("LayerConfig")?.cast_into::<PyType>()?,
        PyLayerConfig::FIELDS,
    )?;
    install_dataclass_fields(
        &m.getattr("RenderConfig")?.cast_into::<PyType>()?,
        PyRenderConfig::FIELDS,
    )?;
    install_dataclass_fields(
        &m.getattr("Config")?.cast_into::<PyType>()?,
        PyConfig::FIELDS,
    )?;
    Ok(())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(all(test, not(feature = "extension-module")))]
mod tests {
    use super::*;
    use pyo3::ffi::c_str;

    fn import_udoc(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
        // Build a fresh PyModule and register the config classes. The
        // cdylib's Python entrypoint is exercised at the integration
        // level; here we only need the type objects + their dataclass
        // shim, so a local PyModule sidesteps the embedded-interpreter
        // import-machinery hassle.
        let m = PyModule::new(py, "udoc")?;
        register(&m)?;
        // Make pickle's qualname lookup work: pickle walks `sys.modules`
        // for the module the class advertises in `__module__` (which the
        // `module = "udoc"` arg on `#[pyclass]` already sets).
        let sys = py.import("sys")?;
        sys.getattr("modules")?.set_item("udoc", &m)?;
        Ok(m)
    }

    /// pickle.dumps -> pickle.loads roundtrip helper.
    fn pickle_roundtrip<'py>(
        py: Python<'py>,
        obj: Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let pickle = py.import("pickle")?;
        let dumped = pickle.call_method1("dumps", (obj,))?;
        pickle.call_method1("loads", (dumped,))
    }

    #[test]
    fn test_config_default_pickle_roundtrip() {
        Python::initialize();
        Python::attach(|py| {
            let m = import_udoc(py).expect("module register");
            let cfg = m
                .getattr("Config")
                .unwrap()
                .call_method0("default")
                .unwrap();
            let restored = pickle_roundtrip(py, cfg.clone()).expect("roundtrip");
            let a: bool = cfg.getattr("strict_fonts").unwrap().extract().unwrap();
            let b: bool = restored.getattr("strict_fonts").unwrap().extract().unwrap();
            assert_eq!(a, b);
            let a: bool = cfg
                .getattr("collect_diagnostics")
                .unwrap()
                .extract()
                .unwrap();
            let b: bool = restored
                .getattr("collect_diagnostics")
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(a, b);
        });
    }

    #[test]
    fn test_config_agent_preset_pickle_roundtrip() {
        Python::initialize();
        Python::attach(|py| {
            let m = import_udoc(py).expect("module register");
            let cfg = m.getattr("Config").unwrap().call_method0("agent").unwrap();
            let restored = pickle_roundtrip(py, cfg).expect("roundtrip");
            let v: bool = restored
                .getattr("collect_diagnostics")
                .unwrap()
                .extract()
                .unwrap();
            assert!(v);
        });
    }

    #[test]
    fn test_config_batch_preset_pickle_roundtrip() {
        Python::initialize();
        Python::attach(|py| {
            let m = import_udoc(py).expect("module register");
            let cfg = m.getattr("Config").unwrap().call_method0("batch").unwrap();
            let assets = cfg.getattr("assets").unwrap();
            let images: bool = assets.getattr("images").unwrap().extract().unwrap();
            assert!(!images, "batch preset should disable image extraction");
            let layers = cfg.getattr("layers").unwrap();
            let pres: bool = layers.getattr("presentation").unwrap().extract().unwrap();
            assert!(!pres, "batch preset should disable presentation overlay");

            let restored = pickle_roundtrip(py, cfg).expect("roundtrip");
            let r_assets = restored.getattr("assets").unwrap();
            let r_images: bool = r_assets.getattr("images").unwrap().extract().unwrap();
            assert!(!r_images, "image flag must survive pickle");
        });
    }

    #[test]
    fn test_config_ocr_preset_pickle_roundtrip() {
        Python::initialize();
        Python::attach(|py| {
            let m = import_udoc(py).expect("module register");
            let cfg = m.getattr("Config").unwrap().call_method0("ocr").unwrap();
            let assets = cfg.getattr("assets").unwrap();
            let fonts: bool = assets.getattr("fonts").unwrap().extract().unwrap();
            assert!(fonts, "ocr preset should enable font extraction");

            let restored = pickle_roundtrip(py, cfg).expect("roundtrip");
            let r_assets = restored.getattr("assets").unwrap();
            let r_fonts: bool = r_assets.getattr("fonts").unwrap().extract().unwrap();
            assert!(r_fonts, "fonts flag must survive pickle");
        });
    }

    #[test]
    fn test_config_dataclass_fields() {
        Python::initialize();
        Python::attach(|py| {
            let m = import_udoc(py).expect("module register");
            let cfg = m
                .getattr("Config")
                .unwrap()
                .call_method0("default")
                .unwrap();
            let dataclasses = py.import("dataclasses").unwrap();
            let fields = dataclasses.call_method1("fields", (cfg,)).unwrap();
            let count: usize = fields.len().unwrap();
            assert_eq!(
                count,
                PyConfig::FIELDS.len(),
                "dataclasses.fields() must return all Config fields"
            );
            let names: Vec<String> = fields
                .try_iter()
                .unwrap()
                .map(|f| f.unwrap().getattr("name").unwrap().extract().unwrap())
                .collect();
            for expected in PyConfig::FIELDS {
                assert!(
                    names.iter().any(|n| n == *expected),
                    "field {expected} missing from dataclasses.fields()"
                );
            }
        });
    }

    #[test]
    fn test_config_match_args() {
        Python::initialize();
        Python::attach(|py| {
            let _m = import_udoc(py).expect("module register");
            // Rust's `\` line-continuation in a string literal trims the
            // leading whitespace of the next line, so we can't use it for
            // indented Python source. Use explicit `\n` escapes in a
            // single line to preserve the indents `match` requires.
            let code = c_str!(
                "import udoc\ncfg = udoc.Config.default()\nmatch cfg:\n    case udoc.Config(limits=l, hooks=h, layers=lay):\n        result = (l is not None, h is not None, lay is not None)\n    case _:\n        result = (False, False, False)\n"
            );
            let globals = PyDict::new(py);
            py.run(code, Some(&globals), None).unwrap();
            let result: (bool, bool, bool) = globals
                .get_item("result")
                .unwrap()
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(result, (true, true, true));
        });
    }

    #[test]
    fn test_limits_default() {
        Python::initialize();
        Python::attach(|py| {
            let m = import_udoc(py).expect("module register");
            let l = m.getattr("Limits").unwrap().call0().unwrap();
            let max_pages: usize = l.getattr("max_pages").unwrap().extract().unwrap();
            let rust_default = udoc_core::limits::Limits::default();
            assert_eq!(max_pages, rust_default.max_pages);
            let mw: Option<usize> = l.getattr("max_warnings").unwrap().extract().unwrap();
            assert_eq!(mw, rust_default.max_warnings);
        });
    }

    #[test]
    fn test_limits_pickle_roundtrip() {
        Python::initialize();
        Python::attach(|py| {
            let m = import_udoc(py).expect("module register");
            let kwargs = PyDict::new(py);
            kwargs.set_item("max_file_size", 12345u64).unwrap();
            let l = m
                .getattr("Limits")
                .unwrap()
                .call((), Some(&kwargs))
                .unwrap();
            let restored = pickle_roundtrip(py, l).expect("roundtrip");
            let v: u64 = restored
                .getattr("max_file_size")
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(v, 12345);
        });
    }

    #[test]
    fn test_assets_pickle_roundtrip() {
        Python::initialize();
        Python::attach(|py| {
            let m = import_udoc(py).expect("module register");
            let kwargs = PyDict::new(py);
            kwargs.set_item("fonts", true).unwrap();
            kwargs.set_item("strict_fonts", true).unwrap();
            let a = m
                .getattr("AssetConfig")
                .unwrap()
                .call((), Some(&kwargs))
                .unwrap();
            let restored = pickle_roundtrip(py, a).expect("roundtrip");
            let f: bool = restored.getattr("fonts").unwrap().extract().unwrap();
            let sf: bool = restored.getattr("strict_fonts").unwrap().extract().unwrap();
            assert!(f);
            assert!(sf);
        });
    }

    #[test]
    fn test_render_config_invalid_profile_rejected() {
        Python::initialize();
        Python::attach(|py| {
            let m = import_udoc(py).expect("module register");
            let kwargs = PyDict::new(py);
            kwargs.set_item("profile", "nonsense").unwrap();
            let err = m.getattr("RenderConfig").unwrap().call((), Some(&kwargs));
            assert!(err.is_err(), "invalid profile must reject");
        });
    }

    #[test]
    fn test_config_repr_shape() {
        Python::initialize();
        Python::attach(|py| {
            let m = import_udoc(py).expect("module register");
            let cfg = m
                .getattr("Config")
                .unwrap()
                .call_method0("default")
                .unwrap();
            let r: String = cfg.repr().unwrap().extract().unwrap();
            assert!(r.starts_with("Config("), "repr was: {r}");
            assert!(r.contains("limits="));
            assert!(r.contains("hooks="));
            assert!(r.contains("collect_diagnostics="));
            assert!(r.contains("password=None"));
            // Sub-config's own repr is embedded.
            assert!(r.contains("Limits("));
        });
    }

    #[test]
    fn test_config_repr_masks_password() {
        Python::initialize();
        Python::attach(|py| {
            let m = import_udoc(py).expect("module register");
            let kwargs = PyDict::new(py);
            kwargs.set_item("password", "hunter2").unwrap();
            let cfg = m
                .getattr("Config")
                .unwrap()
                .call((), Some(&kwargs))
                .unwrap();
            let r: String = cfg.repr().unwrap().extract().unwrap();
            assert!(!r.contains("hunter2"), "password leaked: {r}");
            assert!(r.contains("'***'"), "expected masked password: {r}");
        });
    }

    #[test]
    fn test_config_as_rust_round_trip() {
        Python::initialize();
        Python::attach(|py| {
            let m = import_udoc(py).expect("module register");
            let cfg = m.getattr("Config").unwrap().call_method0("batch").unwrap();
            let py_cfg: PyRef<'_, PyConfig> = cfg.cast::<PyConfig>().unwrap().borrow();
            let rust = py_cfg.as_rust(py).unwrap();
            assert!(!rust.assets.images);
            assert!(!rust.layers.presentation);
            assert!(!rust.layers.relationships);
            assert!(!rust.layers.interactions);
            assert!(rust.collect_diagnostics);
        });
    }
}

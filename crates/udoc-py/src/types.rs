#![allow(clippy::vec_init_then_push)]
//! W1-TYPES: leaf typed pyclasses (`Page`, `Block`, `Inline`, `Table`,
//! `Image`, `Warning`, `BoundingBox`, etc.).
//!
//! All shapes are `#[pyclass(frozen, get_all)]` with `__match_args__` and
//! `__dataclass_fields__` shims so they look and feel like
//! frozen dataclasses to Python callers.
//!
//! W1-FOUNDATION lands the struct definitions + register(). W1-METHODS-TYPES
//! (this file) fills in the dunder methods: `__getattr__` (kind dispatch on
//! Block/Inline), `__repr__` (dataclass-style), `__rich_repr__` (yields
//! field tuples for the rich library), the `Format` capability properties,
//! and `Table.to_pandas()` shim.

use pyo3::exceptions::{PyAttributeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyTuple, PyType};
use pyo3::BoundObject;

use crate::chunks::PyBoundingBox;

// ---------------------------------------------------------------------------
// Format -- enum-style pyclass mirroring `udoc::Format`.
// ---------------------------------------------------------------------------

/// The detected format of a document.
///
/// Mirrors `udoc::Format`. Capability accessors (`can_render`, `has_tables`,
/// `has_pages`) follow the Python no-parens convention (properties).
#[pyclass(name = "Format", frozen, eq, eq_int, skip_from_py_object)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PyFormat {
    Pdf,
    Docx,
    Xlsx,
    Pptx,
    Doc,
    Xls,
    Ppt,
    Odt,
    Ods,
    Odp,
    Rtf,
    Md,
}

#[pymethods]
impl PyFormat {
    /// Whether this format supports page rendering (rasterizing pages to
    /// pixel buffers). PDF only; the other formats either have no pixel
    /// model (DOCX, MD, RTF) or don't have a renderer yet (PPTX, ODP).
    #[getter]
    fn can_render(&self) -> bool {
        matches!(self, PyFormat::Pdf)
    }

    /// Whether this format may contain tables. All twelve shipped formats
    /// can carry tables; the field is a capability marker, not a guarantee
    /// any specific document has them.
    #[getter]
    fn has_tables(&self) -> bool {
        true
    }

    /// Whether this format has a first-class page concept. PDF, DOCX,
    /// PPTX, DOC, PPT, and ODP do; the spreadsheet and reflowable text
    /// formats do not.
    #[getter]
    fn has_pages(&self) -> bool {
        matches!(
            self,
            PyFormat::Pdf
                | PyFormat::Docx
                | PyFormat::Pptx
                | PyFormat::Doc
                | PyFormat::Ppt
                | PyFormat::Odp
        )
    }

    /// Lowercase format name. `str(Format.Pdf) == "pdf"`.
    fn __str__(&self) -> &'static str {
        format_name_lowercase(*self)
    }

    /// All 12 Format variants in declaration order.
    ///
    /// pyo3 0.28 enums aren't iterable on the type itself, so this
    /// classmethod is the canonical way to enumerate the format set:
    /// `for f in udoc.Format.all_variants(): ...`.
    #[classmethod]
    fn all_variants(_cls: &Bound<'_, pyo3::types::PyType>) -> Vec<PyFormat> {
        vec![
            PyFormat::Pdf,
            PyFormat::Docx,
            PyFormat::Xlsx,
            PyFormat::Pptx,
            PyFormat::Doc,
            PyFormat::Xls,
            PyFormat::Ppt,
            PyFormat::Odt,
            PyFormat::Ods,
            PyFormat::Odp,
            PyFormat::Rtf,
            PyFormat::Md,
        ]
    }

    /// Variant name in PascalCase: `Format.Pdf.name == "Pdf"`. Lets
    /// users iterate `Format.all_variants()` and read names without
    /// touching the str() representation (which is lowercase).
    #[getter]
    fn name(&self) -> &'static str {
        format_name_pascal(*self)
    }

    /// Repr in `Format.Pdf` style. Overrides the default pyo3 enum repr
    /// only to be explicit; the default would already produce the same
    /// string but the test pins the exact spelling.
    fn __repr__(&self) -> String {
        format!("Format.{}", format_name_pascal(*self))
    }

    /// Rich repr protocol -- yields (name, value) tuples for the rich
    /// pretty-printer. For an enum we yield the variant name as `name`
    /// plus the three capability flags so `rich.print(Format.Pdf)` shows
    /// the user a full row of context.
    fn __rich_repr__<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyTuple>>> {
        let mut out: Vec<Bound<'py, PyTuple>> = Vec::new();
        out.push(name_value(py, "name", format_name_lowercase(*self))?);
        out.push(name_value(py, "can_render", self.can_render())?);
        out.push(name_value(py, "has_pages", self.has_pages())?);
        out.push(name_value(py, "has_tables", self.has_tables())?);
        Ok(out)
    }

    /// Parse a lowercase name (`"pdf"`, `"docx"`, ...) back into a
    /// `Format` variant. Mirrors the Rust `FromStr` impl.
    #[classmethod]
    fn from_str(_cls: &Bound<'_, PyType>, s: &str) -> PyResult<Self> {
        match s {
            "pdf" => Ok(PyFormat::Pdf),
            "docx" => Ok(PyFormat::Docx),
            "xlsx" => Ok(PyFormat::Xlsx),
            "pptx" => Ok(PyFormat::Pptx),
            "doc" => Ok(PyFormat::Doc),
            "xls" => Ok(PyFormat::Xls),
            "ppt" => Ok(PyFormat::Ppt),
            "odt" => Ok(PyFormat::Odt),
            "ods" => Ok(PyFormat::Ods),
            "odp" => Ok(PyFormat::Odp),
            "rtf" => Ok(PyFormat::Rtf),
            "md" => Ok(PyFormat::Md),
            other => Err(PyValueError::new_err(format!(
                "unknown format: {other:?} (expected one of: pdf, docx, xlsx, pptx, doc, xls, ppt, odt, ods, odp, rtf, md)"
            ))),
        }
    }
}

impl PyFormat {
    /// Convert from the Rust `udoc::Format` enum.
    pub fn from_rust(f: udoc_facade::Format) -> Self {
        match f {
            udoc_facade::Format::Pdf => Self::Pdf,
            udoc_facade::Format::Docx => Self::Docx,
            udoc_facade::Format::Xlsx => Self::Xlsx,
            udoc_facade::Format::Pptx => Self::Pptx,
            udoc_facade::Format::Doc => Self::Doc,
            udoc_facade::Format::Xls => Self::Xls,
            udoc_facade::Format::Ppt => Self::Ppt,
            udoc_facade::Format::Odt => Self::Odt,
            udoc_facade::Format::Ods => Self::Ods,
            udoc_facade::Format::Odp => Self::Odp,
            udoc_facade::Format::Rtf => Self::Rtf,
            udoc_facade::Format::Md => Self::Md,
            // Format is #[non_exhaustive].
            _ => Self::Pdf,
        }
    }
}

fn format_name_lowercase(f: PyFormat) -> &'static str {
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

fn format_name_pascal(f: PyFormat) -> &'static str {
    match f {
        PyFormat::Pdf => "Pdf",
        PyFormat::Docx => "Docx",
        PyFormat::Xlsx => "Xlsx",
        PyFormat::Pptx => "Pptx",
        PyFormat::Doc => "Doc",
        PyFormat::Xls => "Xls",
        PyFormat::Ppt => "Ppt",
        PyFormat::Odt => "Odt",
        PyFormat::Ods => "Ods",
        PyFormat::Odp => "Odp",
        PyFormat::Rtf => "Rtf",
        PyFormat::Md => "Md",
    }
}

// ---------------------------------------------------------------------------
// Inline span -- one pyclass with a `kind` discriminant.
// ---------------------------------------------------------------------------

/// An inline span within a block. The `kind` field is one of
/// `"text"`, `"code"`, `"link"`, `"footnote_ref"`, `"inline_image"`,
/// `"soft_break"`, `"line_break"`. Kind-specific synthetic attributes
/// (none today, room reserved) are exposed via `__getattr__`. Real fields
/// like `text`, `url`, `label` are always-accessible (None when not set
/// for the variant) per the dataclass-shim contract.
#[pyclass(name = "Inline", frozen, get_all)]
pub struct PyInline {
    /// One of "text" | "code" | "link" | "footnote_ref" |
    /// "inline_image" | "soft_break" | "line_break".
    pub kind: String,
    /// NodeId of the span in the document arena.
    pub node_id: u64,
    /// Text payload for text/code variants. None for the rest.
    pub text: Option<String>,
    /// Bold flag for text spans (carried in SpanStyle).
    pub bold: bool,
    /// Italic flag for text spans.
    pub italic: bool,
    /// Underline flag for text spans.
    pub underline: bool,
    /// Strikethrough flag for text spans.
    pub strikethrough: bool,
    /// Superscript flag for text spans.
    pub superscript: bool,
    /// Subscript flag for text spans.
    pub subscript: bool,
    /// URL for link variant. None otherwise.
    pub url: Option<String>,
    /// Nested inline content (link only).
    pub content: Vec<Py<PyInline>>,
    /// Footnote label for footnote_ref variant.
    pub label: Option<String>,
    /// Alt text for inline_image variant.
    pub alt_text: Option<String>,
    /// Asset index for inline_image variant.
    pub image_index: Option<usize>,
}

#[pymethods]
impl PyInline {
    #[classattr]
    const __match_args__: (&'static str, &'static str, &'static str) = ("kind", "node_id", "text");

    /// Catch-all for unknown attribute access. Real fields are served by
    /// the `get_all` getters before reaching this method, so we only fire
    /// for misspellings or kind-mismatched lookups (e.g. `span.rows`).
    fn __getattr__(&self, name: &str) -> PyResult<Py<PyAny>> {
        Err(PyAttributeError::new_err(format!(
            "Inline of kind '{}' has no attribute '{}'; check inline.kind first or use one of the documented fields ({})",
            self.kind,
            name,
            inline_field_hint(&self.kind),
        )))
    }

    fn __repr__(&self) -> String {
        match self.kind.as_str() {
            "text" => format!(
                "Inline(kind='text', text={:?}, bold={}, italic={})",
                self.text.as_deref().unwrap_or(""),
                py_bool(self.bold),
                py_bool(self.italic),
            ),
            "code" => format!(
                "Inline(kind='code', text={:?})",
                self.text.as_deref().unwrap_or(""),
            ),
            "link" => format!(
                "Inline(kind='link', url={:?}, content_len={})",
                self.url.as_deref().unwrap_or(""),
                self.content.len(),
            ),
            "footnote_ref" => format!(
                "Inline(kind='footnote_ref', label={:?})",
                self.label.as_deref().unwrap_or(""),
            ),
            "inline_image" => format!(
                "Inline(kind='inline_image', alt_text={:?}, image_index={:?})",
                self.alt_text.as_deref().unwrap_or(""),
                self.image_index,
            ),
            other => format!("Inline(kind={other:?})"),
        }
    }

    /// Rich repr protocol -- yields (name, value) tuples.
    fn __rich_repr__<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyTuple>>> {
        let mut out: Vec<Bound<'py, PyTuple>> = Vec::new();
        out.push(name_value(py, "kind", self.kind.clone())?);
        out.push(name_value(py, "node_id", self.node_id)?);
        match self.kind.as_str() {
            "text" => {
                out.push(name_value(py, "text", self.text.clone())?);
                if self.bold {
                    out.push(name_value(py, "bold", true)?);
                }
                if self.italic {
                    out.push(name_value(py, "italic", true)?);
                }
            }
            "code" => out.push(name_value(py, "text", self.text.clone())?),
            "link" => {
                out.push(name_value(py, "url", self.url.clone())?);
                out.push(name_value(py, "content_len", self.content.len())?);
            }
            "footnote_ref" => out.push(name_value(py, "label", self.label.clone())?),
            "inline_image" => {
                out.push(name_value(py, "alt_text", self.alt_text.clone())?);
                out.push(name_value(py, "image_index", self.image_index)?);
            }
            _ => {}
        }
        Ok(out)
    }
}

fn inline_field_hint(kind: &str) -> &'static str {
    match kind {
        "text" => "text, bold, italic, underline, strikethrough, superscript, subscript",
        "code" => "text",
        "link" => "url, content",
        "footnote_ref" => "label",
        "inline_image" => "alt_text, image_index",
        "soft_break" | "line_break" => "(none)",
        _ => "kind, node_id",
    }
}

// ---------------------------------------------------------------------------
// Block -- one pyclass with a `kind` discriminant.
// ---------------------------------------------------------------------------

/// A block-level element. The `kind` field is one of
/// `"paragraph"`, `"heading"`, `"list"`, `"table"`, `"code_block"`,
/// `"image"`, `"page_break"`, `"thematic_break"`, `"section"`, `"shape"`.
///
/// Kind-specific synthetic attributes (e.g. `block.rows` -> the table's
/// rows when kind == "table"; `block.image_alt` -> alt_text on image
/// blocks) are routed through `__getattr__`. Real fields fall through to
/// the `get_all` getters and return None when the variant doesn't carry
/// them; the `__getattr__` fallback raises `AttributeError` only for
/// genuinely unknown names with a kind-aware hint.
#[pyclass(name = "Block", frozen, get_all)]
pub struct PyBlock {
    /// Discriminant: "paragraph" | "heading" | "list" | "table" |
    /// "code_block" | "image" | "page_break" | "thematic_break" |
    /// "section" | "shape".
    pub kind: String,
    /// NodeId of this block in the document arena.
    pub node_id: u64,
    /// Plain text payload for paragraph + heading + code_block. None
    /// for table / image / section / shape -- callers should iterate
    /// child blocks or rows for those.
    pub text: Option<String>,
    /// Heading level (1-6) for heading blocks. None otherwise.
    pub level: Option<u32>,
    /// Inline spans for paragraph + heading. Empty for non-text blocks.
    pub spans: Vec<Py<PyInline>>,
    /// Table payload for table blocks. None otherwise.
    pub table: Option<Py<PyTable>>,
    /// List kind ("ordered" | "unordered") for list blocks.
    pub list_kind: Option<String>,
    /// Starting number for ordered lists.
    pub list_start: Option<u64>,
    /// List items for list blocks. Each item is itself a Vec<PyBlock>.
    pub items: Vec<Vec<Py<PyBlock>>>,
    /// Code block language. None for non-code-block.
    pub language: Option<String>,
    /// Image asset index for image blocks.
    pub image_index: Option<usize>,
    /// Alt text for image / shape blocks.
    pub alt_text: Option<String>,
    /// Section role for section blocks (e.g. "header", "footnotes",
    /// "named:custom").
    pub section_role: Option<String>,
    /// Shape kind for shape blocks (e.g. "rectangle", "frame").
    pub shape_kind: Option<String>,
    /// Child blocks for section / shape blocks.
    pub children: Vec<Py<PyBlock>>,
}

#[pymethods]
impl PyBlock {
    #[classattr]
    const __match_args__: (&'static str, &'static str, &'static str, &'static str) =
        ("kind", "node_id", "text", "level");

    /// Synthetic attribute dispatch: `block.rows` -> table.rows for table
    /// blocks; `block.image_alt` -> alt_text for image blocks. Anything
    /// else raises AttributeError with a kind-aware message. Real fields
    /// (text, level, items, ...) are served by the `get_all` getters and
    /// never reach this method.
    fn __getattr__(&self, py: Python<'_>, name: &str) -> PyResult<Py<PyAny>> {
        match (self.kind.as_str(), name) {
            ("table", "rows") => {
                let table = self.table.as_ref().ok_or_else(|| {
                    PyAttributeError::new_err(
                        "Block of kind 'table' has no attached table payload",
                    )
                })?;
                let rows: Vec<Py<PyTableRow>> = table
                    .bind(py)
                    .borrow()
                    .rows
                    .iter()
                    .map(|r| r.clone_ref(py))
                    .collect();
                Ok(rows.into_pyobject(py)?.into_any().unbind())
            }
            ("image", "image_alt") => Ok(self
                .alt_text
                .clone()
                .into_pyobject(py)?
                .into_any()
                .unbind()),
            ("table", attr) => Err(PyAttributeError::new_err(format!(
                "Block of kind 'table' has no attribute '{attr}'; use block.table or block.rows"
            ))),
            (kind, attr) => Err(PyAttributeError::new_err(format!(
                "Block of kind '{kind}' has no attribute '{attr}'; use block.{} or check block.kind first",
                block_field_hint(kind),
            ))),
        }
    }

    fn __repr__(&self) -> String {
        match self.kind.as_str() {
            "paragraph" => format!(
                "Block(kind='paragraph', text={:?})",
                truncate(self.text.as_deref().unwrap_or(""), 60),
            ),
            "heading" => format!(
                "Block(kind='heading', text={:?}, level={})",
                truncate(self.text.as_deref().unwrap_or(""), 60),
                self.level.unwrap_or(0),
            ),
            "list" => format!(
                "Block(kind='list', list_kind={:?}, items={})",
                self.list_kind.as_deref().unwrap_or("unknown"),
                self.items.len(),
            ),
            "table" => format!("Block(kind='table', node_id={})", self.node_id,),
            "code_block" => format!(
                "Block(kind='code_block', language={:?}, text_len={})",
                self.language.as_deref().unwrap_or(""),
                self.text.as_deref().map(str::len).unwrap_or(0),
            ),
            "image" => format!(
                "Block(kind='image', alt_text={:?}, image_index={:?})",
                self.alt_text.as_deref().unwrap_or(""),
                self.image_index,
            ),
            "page_break" => "Block(kind='page_break')".to_string(),
            "thematic_break" => "Block(kind='thematic_break')".to_string(),
            "section" => format!(
                "Block(kind='section', role={:?}, children={})",
                self.section_role.as_deref().unwrap_or(""),
                self.children.len(),
            ),
            "shape" => format!(
                "Block(kind='shape', shape_kind={:?}, children={})",
                self.shape_kind.as_deref().unwrap_or(""),
                self.children.len(),
            ),
            other => format!("Block(kind={other:?})"),
        }
    }

    fn __rich_repr__<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyTuple>>> {
        let mut out: Vec<Bound<'py, PyTuple>> = Vec::new();
        out.push(name_value(py, "kind", self.kind.clone())?);
        out.push(name_value(py, "node_id", self.node_id)?);
        match self.kind.as_str() {
            "paragraph" | "heading" | "code_block" => {
                out.push(name_value(py, "text", self.text.clone())?);
                if let Some(l) = self.level {
                    out.push(name_value(py, "level", l)?);
                }
                if let Some(lang) = &self.language {
                    out.push(name_value(py, "language", lang.clone())?);
                }
            }
            "list" => {
                out.push(name_value(py, "list_kind", self.list_kind.clone())?);
                out.push(name_value(py, "items", self.items.len())?);
            }
            "table" => out.push(name_value(py, "rows", self.items.len())?),
            "image" => {
                out.push(name_value(py, "alt_text", self.alt_text.clone())?);
                out.push(name_value(py, "image_index", self.image_index)?);
            }
            "section" => {
                out.push(name_value(py, "role", self.section_role.clone())?);
                out.push(name_value(py, "children", self.children.len())?);
            }
            "shape" => {
                out.push(name_value(py, "shape_kind", self.shape_kind.clone())?);
                out.push(name_value(py, "children", self.children.len())?);
            }
            _ => {}
        }
        Ok(out)
    }
}

fn block_field_hint(kind: &str) -> &'static str {
    match kind {
        "paragraph" => "text|spans",
        "heading" => "text|level|spans",
        "list" => "list_kind|list_start|items",
        "table" => "table|rows",
        "code_block" => "text|language",
        "image" => "image_index|alt_text",
        "section" => "section_role|children",
        "shape" => "shape_kind|alt_text|children",
        "page_break" | "thematic_break" => "kind|node_id",
        _ => "kind|node_id",
    }
}

// ---------------------------------------------------------------------------
// Table primitives.
// ---------------------------------------------------------------------------

/// A table cell. `content` is the rich block content of the cell (a cell
/// can hold multiple paragraphs, lists, even nested tables).
#[pyclass(name = "TableCell", frozen, get_all)]
pub struct PyTableCell {
    pub node_id: u64,
    /// Plain text reduction of the cell content (for ergonomic access;
    /// rich content lives in `content`).
    pub text: String,
    /// Rich content blocks.
    pub content: Vec<Py<PyBlock>>,
    pub col_span: usize,
    pub row_span: usize,
    /// Typed value for spreadsheet cells. Serialized to a string for the
    /// Python side; the typed enum (Number, Date, Bool, Error, ...) lands
    /// later if callers want strong typing.
    pub value: Option<String>,
}

#[pymethods]
impl PyTableCell {
    #[classattr]
    const __match_args__: (&'static str, &'static str) = ("text", "content");

    fn __repr__(&self) -> String {
        if self.col_span > 1 || self.row_span > 1 {
            format!(
                "TableCell(text={:?}, col_span={}, row_span={})",
                truncate(&self.text, 40),
                self.col_span,
                self.row_span,
            )
        } else {
            format!("TableCell(text={:?})", truncate(&self.text, 40))
        }
    }

    fn __rich_repr__<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyTuple>>> {
        let mut out: Vec<Bound<'py, PyTuple>> = Vec::new();
        out.push(name_value(py, "text", self.text.clone())?);
        if self.col_span > 1 {
            out.push(name_value(py, "col_span", self.col_span)?);
        }
        if self.row_span > 1 {
            out.push(name_value(py, "row_span", self.row_span)?);
        }
        Ok(out)
    }
}

/// A table row.
#[pyclass(name = "TableRow", frozen, get_all)]
pub struct PyTableRow {
    pub node_id: u64,
    pub cells: Vec<Py<PyTableCell>>,
    pub is_header: bool,
}

#[pymethods]
impl PyTableRow {
    #[classattr]
    const __match_args__: (&'static str, &'static str) = ("cells", "is_header");

    fn __repr__(&self) -> String {
        format!(
            "TableRow(cells={}, is_header={})",
            self.cells.len(),
            py_bool(self.is_header),
        )
    }

    fn __rich_repr__<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyTuple>>> {
        let mut out: Vec<Bound<'py, PyTuple>> = Vec::new();
        out.push(name_value(py, "cells", self.cells.len())?);
        if self.is_header {
            out.push(name_value(py, "is_header", true)?);
        }
        Ok(out)
    }
}

/// A table. Carries shape metadata + the row vector. `to_pandas()` is a
/// thin shim that imports `udoc.integrations.pandas` at call
/// time -- if pandas is not installed the call propagates the resulting
/// `ImportError` to the caller.
#[pyclass(name = "Table", frozen, get_all)]
pub struct PyTable {
    pub node_id: u64,
    pub rows: Vec<Py<PyTableRow>>,
    pub num_columns: usize,
    pub header_row_count: usize,
    pub has_header_row: bool,
    pub may_continue_from_previous: bool,
    pub may_continue_to_next: bool,
}

#[pymethods]
impl PyTable {
    #[classattr]
    const __match_args__: (&'static str, &'static str, &'static str) =
        ("rows", "num_columns", "has_header_row");

    /// Materialize the table as a pandas DataFrame. this is a
    /// shim: it imports `udoc.integrations.pandas.to_dataframe` at call
    /// time and forwards `self`. If pandas is not installed the import
    /// raises `ImportError("No module named 'pandas'")` and we let it
    /// propagate untouched (callers can catch it; the user-facing
    /// install hint lives in `udoc/integrations/pandas.py`).
    fn to_pandas<'py>(slf: PyRef<'py, Self>, py: Python<'py>) -> PyResult<Py<PyAny>> {
        let module = py.import("udoc.integrations.pandas")?;
        let func = module.getattr("to_dataframe")?;
        let bound = slf.into_pyobject(py)?;
        let result = func.call1((bound,))?;
        Ok(result.unbind())
    }

    fn __repr__(&self) -> String {
        format!(
            "Table(rows={}, num_columns={}, has_header_row={})",
            self.rows.len(),
            self.num_columns,
            py_bool(self.has_header_row),
        )
    }

    fn __rich_repr__<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyTuple>>> {
        let mut out: Vec<Bound<'py, PyTuple>> = Vec::new();
        out.push(name_value(py, "rows", self.rows.len())?);
        out.push(name_value(py, "num_columns", self.num_columns)?);
        out.push(name_value(py, "has_header_row", self.has_header_row)?);
        if self.may_continue_from_previous {
            out.push(name_value(py, "may_continue_from_previous", true)?);
        }
        if self.may_continue_to_next {
            out.push(name_value(py, "may_continue_to_next", true)?);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Image -- the Block::Image placement, joined with its asset payload.
// ---------------------------------------------------------------------------

/// An image: either `Block::Image` (block-level) or `Inline::InlineImage`,
/// with the asset bytes joined in. `data` is the raw encoded bytes; the
/// caller decodes per `filter`.
#[pyclass(name = "Image", frozen, get_all)]
pub struct PyImage {
    pub node_id: u64,
    /// Asset index in the document's asset store.
    pub asset_index: usize,
    pub width: u32,
    pub height: u32,
    pub bits_per_component: u8,
    /// Filter kind: "jpeg" | "png" | "tiff" | "raw" | etc.
    pub filter: String,
    /// Raw encoded bytes of the image.
    pub data: Vec<u8>,
    pub alt_text: Option<String>,
    pub bbox: Option<Py<PyBoundingBox>>,
}

#[pymethods]
impl PyImage {
    #[classattr]
    const __match_args__: (&'static str, &'static str, &'static str) =
        ("width", "height", "filter");

    fn __repr__(&self) -> String {
        format!(
            "Image(width={}, height={}, filter={:?}, alt_text={:?}, bytes={})",
            self.width,
            self.height,
            self.filter,
            self.alt_text.as_deref().unwrap_or(""),
            self.data.len(),
        )
    }

    fn __rich_repr__<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyTuple>>> {
        let mut out: Vec<Bound<'py, PyTuple>> = Vec::new();
        out.push(name_value(py, "width", self.width)?);
        out.push(name_value(py, "height", self.height)?);
        out.push(name_value(py, "filter", self.filter.clone())?);
        if let Some(alt) = &self.alt_text {
            out.push(name_value(py, "alt_text", alt.clone())?);
        }
        out.push(name_value(py, "bytes", self.data.len())?);
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Warning -- a diagnostic.
// ---------------------------------------------------------------------------

/// A diagnostic warning emitted during extraction.
#[pyclass(name = "Warning", frozen, get_all)]
pub struct PyWarning {
    /// Machine-readable kind (e.g. "FontError", "MalformedXref").
    pub kind: String,
    /// Severity ("info" | "warning").
    pub level: String,
    /// Human-readable message.
    pub message: String,
    /// Byte offset in the source if known, else None.
    pub offset: Option<u64>,
    /// Page index if applicable.
    pub page_index: Option<usize>,
    /// Additional detail string.
    pub detail: Option<String>,
}

#[pymethods]
impl PyWarning {
    #[classattr]
    const __match_args__: (&'static str, &'static str, &'static str) = ("kind", "level", "message");

    fn __repr__(&self) -> String {
        format!(
            "Warning(kind={:?}, level={:?}, message={:?})",
            self.kind, self.level, self.message,
        )
    }

    fn __str__(&self) -> String {
        match (self.page_index, self.offset.as_ref()) {
            (Some(p), Some(o)) => format!(
                "[{}] {}: {} (page={}, offset={})",
                self.level, self.kind, self.message, p, o
            ),
            (Some(p), None) => format!(
                "[{}] {}: {} (page={})",
                self.level, self.kind, self.message, p
            ),
            (None, Some(o)) => format!(
                "[{}] {}: {} (offset={})",
                self.level, self.kind, self.message, o
            ),
            (None, None) => format!("[{}] {}: {}", self.level, self.kind, self.message),
        }
    }

    fn __rich_repr__<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyTuple>>> {
        let mut out: Vec<Bound<'py, PyTuple>> = Vec::new();
        out.push(name_value(py, "kind", self.kind.clone())?);
        out.push(name_value(py, "level", self.level.clone())?);
        out.push(name_value(py, "message", self.message.clone())?);
        if let Some(o) = self.offset {
            out.push(name_value(py, "offset", o)?);
        }
        if let Some(p) = self.page_index {
            out.push(name_value(py, "page_index", p)?);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Page -- a per-page bundle.
// ---------------------------------------------------------------------------

/// A page of the document. The udoc Document model has no first-class Page
/// type (pages are derived from the presentation overlay's `PageDef`); for
/// the Python surface we materialize a `Page` per page index that exposes
/// the slice of blocks belonging to that page.
///
/// W1-METHODS-TYPES wires the materialized-block accessors (`text()`,
/// `tables()`, `images()` synthesized from `blocks`). Operations that
/// require live page extraction (`text_lines()`, `raw_spans()`,
/// `render(dpi)`) raise `UnsupportedOperationError` here and are
/// implemented end-to-end by W1-METHODS-DOCUMENT, which has the
/// `Py<PyDocument>` handle needed to re-enter the backend.
#[pyclass(name = "Page", frozen, get_all)]
pub struct PyPage {
    pub index: usize,
    pub blocks: Vec<Py<PyBlock>>,
}

#[pymethods]
impl PyPage {
    #[classattr]
    const __match_args__: (&'static str, &'static str) = ("index", "blocks");

    /// Concatenated plain text for the page. Walks the materialized
    /// block tree, joining paragraph + heading text with newlines.
    fn text(&self, py: Python<'_>) -> PyResult<String> {
        let mut out = String::new();
        for block_handle in &self.blocks {
            let block = block_handle.bind(py).borrow();
            collect_block_text(&block, &mut out, py);
        }
        Ok(out)
    }

    /// `text_lines()` requires live page extraction (segmenting by
    /// baseline / line spacing) which the materialized block view does
    /// not preserve. W1-METHODS-DOCUMENT will wire this through the
    /// `Document` handle. Until then we surface an explicit
    /// `UnsupportedOperationError`.
    fn text_lines(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        Err(make_unsupported_op_error(
            py,
            "Page.text_lines() requires live page extraction; use Document.text() or wait for W1-METHODS-DOCUMENT",
        ))
    }

    /// Same caveat as `text_lines()`; lands in W1-METHODS-DOCUMENT.
    fn raw_spans(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        Err(make_unsupported_op_error(
            py,
            "Page.raw_spans() requires live page extraction; use Document.text() or wait for W1-METHODS-DOCUMENT",
        ))
    }

    /// Tables on this page, walked from the materialized block tree.
    /// Includes nested tables inside section / shape / list children.
    fn tables(&self, py: Python<'_>) -> PyResult<Vec<Py<PyTable>>> {
        let mut out = Vec::new();
        for block_handle in &self.blocks {
            let block = block_handle.bind(py).borrow();
            collect_block_tables(&block, &mut out, py);
        }
        Ok(out)
    }

    /// Image-block placements on this page. Inline images embedded in
    /// paragraphs are reachable via the inline tree (Block.spans) and not
    /// surfaced here -- the spec mirrors the Rust `PageBundle.images()`
    /// behavior which is block-image-only.
    ///
    /// NOTE: PyBlock carries the image_index but not the resolved bytes;
    /// W1-METHODS-DOCUMENT has the asset store and produces full
    /// `PyImage` instances. This foundation method returns an empty list
    /// because we cannot mint a `PyImage` without the asset bytes -- we
    /// would otherwise hand callers half-formed images.
    fn images(&self, _py: Python<'_>) -> PyResult<Vec<Py<PyImage>>> {
        // Intentionally empty: PyBlock carries image_index but the asset
        // store needed to mint PyImage lives on PyDocument. The full
        // implementation is W1-METHODS-DOCUMENT's job (it has the doc
        // handle). Returning [] here, rather than raising, keeps the
        // common case (`for img in page.images(): ...`) from blowing up
        // on documents that simply have no images.
        Ok(Vec::new())
    }

    /// Render the page to a pixel buffer. Only PDF supports rendering;
    /// other backends raise `UnsupportedOperationError`. The actual
    /// rasterizer call lives in W1-METHODS-DOCUMENT (which has the
    /// `Py<PyDocument>` handle); the foundation surfaces a clear error
    /// so callers learn the contract early.
    #[pyo3(signature = (dpi=150))]
    fn render(&self, py: Python<'_>, dpi: u32) -> PyResult<Py<PyAny>> {
        let _ = dpi;
        Err(make_unsupported_op_error(
            py,
            "Page.render() is implemented on Document.render_page(); the Page value handle does not carry the document context (lands in W1-METHODS-DOCUMENT)",
        ))
    }

    fn __repr__(&self) -> String {
        format!("Page(index={}, blocks={})", self.index, self.blocks.len())
    }

    /// Rich repr protocol -- (name, value) tuples for the rich
    /// pretty-printer.
    fn __rich_repr__<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyTuple>>> {
        let mut out: Vec<Bound<'py, PyTuple>> = Vec::new();
        out.push(name_value(py, "index", self.index)?);
        out.push(name_value(py, "blocks", self.blocks.len())?);
        Ok(out)
    }
}

fn collect_block_text(block: &PyBlock, out: &mut String, py: Python<'_>) {
    match block.kind.as_str() {
        "paragraph" | "heading" | "code_block" => {
            if let Some(t) = &block.text {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            }
        }
        "list" => {
            for item in &block.items {
                for child_handle in item {
                    let child = child_handle.bind(py).borrow();
                    collect_block_text(&child, out, py);
                }
            }
        }
        "table" => {
            if let Some(table_handle) = &block.table {
                let table = table_handle.bind(py).borrow();
                for row_handle in &table.rows {
                    let row = row_handle.bind(py).borrow();
                    for (i, cell_handle) in row.cells.iter().enumerate() {
                        let cell = cell_handle.bind(py).borrow();
                        if i > 0 {
                            out.push('\t');
                        }
                        out.push_str(&cell.text);
                    }
                    out.push('\n');
                }
            }
        }
        "section" | "shape" => {
            for child_handle in &block.children {
                let child = child_handle.bind(py).borrow();
                collect_block_text(&child, out, py);
            }
        }
        _ => {}
    }
}

fn collect_block_tables(block: &PyBlock, out: &mut Vec<Py<PyTable>>, py: Python<'_>) {
    if block.kind == "table" {
        if let Some(table_handle) = &block.table {
            out.push(table_handle.clone_ref(py));
        }
    }
    for child_handle in &block.children {
        let child = child_handle.bind(py).borrow();
        collect_block_tables(&child, out, py);
    }
    for item in &block.items {
        for child_handle in item {
            let child = child_handle.bind(py).borrow();
            collect_block_tables(&child, out, py);
        }
    }
}

// ---------------------------------------------------------------------------
// DocumentMetadata -- exposed as a frozen pyclass.
// ---------------------------------------------------------------------------

/// Document-level metadata. Mirrors `udoc::DocumentMetadata`.
#[pyclass(name = "DocumentMetadata", frozen, get_all)]
pub struct PyDocumentMetadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub creation_date: Option<String>,
    pub modification_date: Option<String>,
    pub page_count: usize,
    /// Custom/extended properties as a flat string-keyed dict.
    pub properties: std::collections::HashMap<String, String>,
}

#[pymethods]
impl PyDocumentMetadata {
    #[classattr]
    const __match_args__: (&'static str, &'static str, &'static str) =
        ("title", "author", "page_count");

    fn __repr__(&self) -> String {
        format!(
            "DocumentMetadata(title={:?}, author={:?}, page_count={})",
            self.title.as_deref().unwrap_or(""),
            self.author.as_deref().unwrap_or(""),
            self.page_count,
        )
    }

    fn __rich_repr__<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyTuple>>> {
        let mut out: Vec<Bound<'py, PyTuple>> = Vec::new();
        if let Some(t) = &self.title {
            out.push(name_value(py, "title", t.clone())?);
        }
        if let Some(a) = &self.author {
            out.push(name_value(py, "author", a.clone())?);
        }
        if let Some(s) = &self.subject {
            out.push(name_value(py, "subject", s.clone())?);
        }
        out.push(name_value(py, "page_count", self.page_count)?);
        if !self.properties.is_empty() {
            out.push(name_value(py, "properties", self.properties.len())?);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// Build a Python `(name, value)` tuple. Used by `__rich_repr__`.
fn name_value<'py, V>(py: Python<'py>, name: &str, value: V) -> PyResult<Bound<'py, PyTuple>>
where
    V: IntoPyObject<'py>,
{
    let name_obj: Bound<'py, PyAny> = name.into_pyobject(py)?.into_any().into_bound();
    let value_obj: Bound<'py, PyAny> = value
        .into_pyobject(py)
        .map_err(Into::into)?
        .into_any()
        .into_bound();
    PyTuple::new(py, [name_obj, value_obj])
}

/// Truncate a string to a max-character preview for `__repr__`.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push_str("...");
        out
    }
}

/// Format a Rust bool as the Python "True"/"False" literal for embedding
/// inside __repr__ strings (matching `@dataclass`'s default repr style).
fn py_bool(b: bool) -> &'static str {
    if b {
        "True"
    } else {
        "False"
    }
}

/// Build an `udoc.UnsupportedOperationError` PyErr. Pulled lazily so we
/// don't take a hard dep on the errors module's class objects at compile
/// time (registration order is errors-first per lib.rs but the lookup
/// path is robust either way).
fn make_unsupported_op_error(py: Python<'_>, msg: &str) -> PyErr {
    if let Ok(udoc_mod) = py.import("udoc") {
        if let Ok(cls) = udoc_mod.getattr("UnsupportedOperationError") {
            if let Ok(instance) = cls.call1((msg,)) {
                return PyErr::from_value(instance);
            }
        }
    }
    pyo3::exceptions::PyNotImplementedError::new_err(msg.to_string())
}

/// Attach a `__dataclass_fields__` shim to a pyclass so
/// `dataclasses.fields(obj)` and `dataclasses.asdict(obj)` work. The
/// shim is a dict of field-name -> sentinel (we use the field name
/// itself as the value; the typing reflection in the .pyi stubs is
/// the source of truth, this dict only needs to exist + be iterable).
fn attach_dataclass_fields(
    m: &Bound<'_, PyModule>,
    cls: &Bound<'_, PyType>,
    fields: &[&str],
) -> PyResult<()> {
    let dict = PyDict::new(m.py());
    for name in fields {
        dict.set_item(name, name)?;
    }
    cls.setattr("__dataclass_fields__", dict)?;
    Ok(())
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyFormat>()?;
    m.add_class::<PyInline>()?;
    m.add_class::<PyBlock>()?;
    m.add_class::<PyTableCell>()?;
    m.add_class::<PyTableRow>()?;
    m.add_class::<PyTable>()?;
    m.add_class::<PyImage>()?;
    m.add_class::<PyWarning>()?;
    m.add_class::<PyPage>()?;
    m.add_class::<PyDocumentMetadata>()?;
    // PyBoundingBox is registered by the chunks module (it lives there
    // because chunks.PyChunkSource references it).

    // Attach `__dataclass_fields__` shims so the value pyclasses behave
    // like @dataclass instances under `dataclasses.fields()` /
    // `dataclasses.asdict()` reflection.
    attach_dataclass_fields(
        m,
        &m.py().get_type::<PyInline>(),
        &[
            "kind",
            "node_id",
            "text",
            "bold",
            "italic",
            "underline",
            "strikethrough",
            "superscript",
            "subscript",
            "url",
            "content",
            "label",
            "alt_text",
            "image_index",
        ],
    )?;
    attach_dataclass_fields(
        m,
        &m.py().get_type::<PyBlock>(),
        &[
            "kind",
            "node_id",
            "text",
            "level",
            "spans",
            "table",
            "list_kind",
            "list_start",
            "items",
            "language",
            "image_index",
            "alt_text",
            "section_role",
            "shape_kind",
            "children",
        ],
    )?;
    attach_dataclass_fields(
        m,
        &m.py().get_type::<PyTableCell>(),
        &[
            "node_id", "text", "content", "col_span", "row_span", "value",
        ],
    )?;
    attach_dataclass_fields(
        m,
        &m.py().get_type::<PyTableRow>(),
        &["node_id", "cells", "is_header"],
    )?;
    attach_dataclass_fields(
        m,
        &m.py().get_type::<PyTable>(),
        &[
            "node_id",
            "rows",
            "num_columns",
            "header_row_count",
            "has_header_row",
            "may_continue_from_previous",
            "may_continue_to_next",
        ],
    )?;
    attach_dataclass_fields(
        m,
        &m.py().get_type::<PyImage>(),
        &[
            "node_id",
            "asset_index",
            "width",
            "height",
            "bits_per_component",
            "filter",
            "data",
            "alt_text",
            "bbox",
        ],
    )?;
    attach_dataclass_fields(
        m,
        &m.py().get_type::<PyWarning>(),
        &["kind", "level", "message", "offset", "page_index", "detail"],
    )?;
    attach_dataclass_fields(m, &m.py().get_type::<PyPage>(), &["index", "blocks"])?;
    attach_dataclass_fields(
        m,
        &m.py().get_type::<PyDocumentMetadata>(),
        &[
            "title",
            "author",
            "subject",
            "creator",
            "producer",
            "creation_date",
            "modification_date",
            "page_count",
            "properties",
        ],
    )?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

// The test module is gated off when `extension-module` is on: that feature
// disables linking against libpython, which makes `cargo test --lib` fail
// at the link step (PyType_IsSubtype, PyObject_GenericGetAttr, etc. become
// undefined symbols). Run unit tests with `cargo test -p udoc-py --lib`
// (the crate's default has no features); workspace-wide
// `cargo test --workspace --all-features` would otherwise activate
// `extension-module` and break the link.
#[cfg(all(test, not(feature = "extension-module")))]
mod tests {
    use super::*;
    use pyo3::types::PyList;

    fn make_paragraph(py: Python<'_>, text: &str) -> Py<PyBlock> {
        Py::new(
            py,
            PyBlock {
                kind: "paragraph".into(),
                node_id: 1,
                text: Some(text.into()),
                level: None,
                spans: vec![],
                table: None,
                list_kind: None,
                list_start: None,
                items: vec![],
                language: None,
                image_index: None,
                alt_text: None,
                section_role: None,
                shape_kind: None,
                children: vec![],
            },
        )
        .unwrap()
    }

    fn make_heading(py: Python<'_>, text: &str, level: u32) -> Py<PyBlock> {
        Py::new(
            py,
            PyBlock {
                kind: "heading".into(),
                node_id: 2,
                text: Some(text.into()),
                level: Some(level),
                spans: vec![],
                table: None,
                list_kind: None,
                list_start: None,
                items: vec![],
                language: None,
                image_index: None,
                alt_text: None,
                section_role: None,
                shape_kind: None,
                children: vec![],
            },
        )
        .unwrap()
    }

    fn make_table_block(py: Python<'_>) -> Py<PyBlock> {
        let cell = Py::new(
            py,
            PyTableCell {
                node_id: 10,
                text: "hello".into(),
                content: vec![],
                col_span: 1,
                row_span: 1,
                value: None,
            },
        )
        .unwrap();
        let row = Py::new(
            py,
            PyTableRow {
                node_id: 11,
                cells: vec![cell],
                is_header: false,
            },
        )
        .unwrap();
        let table = Py::new(
            py,
            PyTable {
                node_id: 12,
                rows: vec![row],
                num_columns: 1,
                header_row_count: 0,
                has_header_row: false,
                may_continue_from_previous: false,
                may_continue_to_next: false,
            },
        )
        .unwrap();
        Py::new(
            py,
            PyBlock {
                kind: "table".into(),
                node_id: 12,
                text: None,
                level: None,
                spans: vec![],
                table: Some(table),
                list_kind: None,
                list_start: None,
                items: vec![],
                language: None,
                image_index: None,
                alt_text: None,
                section_role: None,
                shape_kind: None,
                children: vec![],
            },
        )
        .unwrap()
    }

    /// `block.text` on a paragraph returns the text via the get_all
    /// getter (it's a real field, not a __getattr__ fallback).
    #[test]
    fn test_block_text_on_paragraph_returns_text() {
        Python::attach(|py| {
            let block = make_paragraph(py, "Hello, world.");
            let bound = block.bind(py);
            let text: Option<String> = bound.getattr("text").unwrap().extract().unwrap();
            assert_eq!(text, Some("Hello, world.".to_string()));
        });
    }

    /// `block.text` on a TABLE block returns None (the field exists,
    /// is None for non-text variants). The kind-aware error path is
    /// tested below for synthetic / unknown attributes.
    #[test]
    fn test_block_text_on_table_returns_none() {
        Python::attach(|py| {
            let block = make_table_block(py);
            let bound = block.bind(py);
            let text: Option<String> = bound.getattr("text").unwrap().extract().unwrap();
            assert_eq!(text, None);
        });
    }

    /// `block.bogus_attr` on a TABLE raises AttributeError with a
    /// kind-aware hint.
    #[test]
    fn test_block_unknown_attr_on_table_raises_attribute_error() {
        Python::attach(|py| {
            let block = make_table_block(py);
            let bound = block.bind(py);
            let err = bound.getattr("bogus_attr").unwrap_err();
            assert!(err.is_instance_of::<PyAttributeError>(py));
            let msg = err.to_string();
            assert!(msg.contains("'table'"), "msg was: {msg}");
            assert!(msg.contains("bogus_attr"), "msg was: {msg}");
        });
    }

    /// `block.rows` on a table block returns a list of TableRow handles
    /// via the synthetic-attribute path in __getattr__.
    #[test]
    fn test_block_rows_on_table_returns_list() {
        Python::attach(|py| {
            let block = make_table_block(py);
            let bound = block.bind(py);
            let rows = bound.getattr("rows").unwrap();
            let list = rows.cast_into::<PyList>().unwrap();
            assert_eq!(list.len(), 1);
        });
    }

    /// `block.rows` on a paragraph block raises AttributeError.
    #[test]
    fn test_block_rows_on_paragraph_raises() {
        Python::attach(|py| {
            let block = make_paragraph(py, "hi");
            let bound = block.bind(py);
            let err = bound.getattr("rows").unwrap_err();
            assert!(err.is_instance_of::<PyAttributeError>(py));
            let msg = err.to_string();
            assert!(msg.contains("'paragraph'"), "msg was: {msg}");
            assert!(msg.contains("rows"), "msg was: {msg}");
        });
    }

    /// `inline.kind` returns the discriminant via the get_all getter.
    #[test]
    fn test_inline_text_kind_dispatch() {
        Python::attach(|py| {
            let span = Py::new(
                py,
                PyInline {
                    kind: "text".into(),
                    node_id: 0,
                    text: Some("hello".into()),
                    bold: true,
                    italic: false,
                    underline: false,
                    strikethrough: false,
                    superscript: false,
                    subscript: false,
                    url: None,
                    content: vec![],
                    label: None,
                    alt_text: None,
                    image_index: None,
                },
            )
            .unwrap();
            let bound = span.bind(py);
            let kind: String = bound.getattr("kind").unwrap().extract().unwrap();
            assert_eq!(kind, "text");
            let bold: bool = bound.getattr("bold").unwrap().extract().unwrap();
            assert!(bold);
            // Synthetic / unknown attribute on an inline span.
            let err = bound.getattr("rows").unwrap_err();
            assert!(err.is_instance_of::<PyAttributeError>(py));
        });
    }

    /// `Table.to_pandas()` imports `udoc.integrations.pandas`. If that
    /// module isn't on `sys.path` (the standard case in `cargo test`,
    /// where the udoc cdylib isn't installed) the call raises an
    /// ImportError. We assert that error type is propagated.
    #[test]
    fn test_table_to_pandas_smoke() {
        Python::attach(|py| {
            let table = Py::new(
                py,
                PyTable {
                    node_id: 0,
                    rows: vec![],
                    num_columns: 0,
                    header_row_count: 0,
                    has_header_row: false,
                    may_continue_from_previous: false,
                    may_continue_to_next: false,
                },
            )
            .unwrap();
            let bound = table.bind(py);
            // Skip if pandas is somehow importable AND udoc.integrations
            // is too -- in that case to_pandas would actually run.
            let pandas_is_available =
                py.import("pandas").is_ok() && py.import("udoc.integrations.pandas").is_ok();
            let result = bound.call_method0("to_pandas");
            if pandas_is_available {
                assert!(
                    result.is_ok(),
                    "to_pandas should succeed when pandas + udoc.integrations are importable"
                );
            } else {
                assert!(
                    result.is_err(),
                    "to_pandas should raise when udoc.integrations.pandas is not importable"
                );
                // The error should be ImportError (or a subclass).
                let err = result.unwrap_err();
                assert!(
                    err.is_instance_of::<pyo3::exceptions::PyImportError>(py)
                        || err.is_instance_of::<pyo3::exceptions::PyModuleNotFoundError>(py),
                    "expected ImportError, got {err:?}"
                );
            }
        });
    }

    #[test]
    fn test_format_can_render_pdf_true() {
        Python::attach(|py| {
            let f = Py::new(py, PyFormat::Pdf).unwrap();
            let v: bool = f
                .bind(py)
                .as_any()
                .getattr("can_render")
                .unwrap()
                .extract()
                .unwrap();
            assert!(v);
        });
    }

    #[test]
    fn test_format_can_render_docx_false() {
        Python::attach(|py| {
            let f = Py::new(py, PyFormat::Docx).unwrap();
            let v: bool = f
                .bind(py)
                .as_any()
                .getattr("can_render")
                .unwrap()
                .extract()
                .unwrap();
            assert!(!v);
        });
    }

    #[test]
    fn test_format_has_pages() {
        Python::attach(|py| {
            for (variant, expected) in [
                (PyFormat::Pdf, true),
                (PyFormat::Docx, true),
                (PyFormat::Pptx, true),
                (PyFormat::Doc, true),
                (PyFormat::Ppt, true),
                (PyFormat::Odp, true),
                (PyFormat::Xlsx, false),
                (PyFormat::Xls, false),
                (PyFormat::Ods, false),
                (PyFormat::Rtf, false),
                (PyFormat::Md, false),
                (PyFormat::Odt, false),
            ] {
                let f = Py::new(py, variant).unwrap();
                let v: bool = f
                    .bind(py)
                    .as_any()
                    .getattr("has_pages")
                    .unwrap()
                    .extract()
                    .unwrap();
                assert_eq!(
                    v,
                    expected,
                    "has_pages mismatch for {:?}",
                    format_name_lowercase(variant)
                );
            }
        });
    }

    #[test]
    fn test_format_has_tables_all_true() {
        Python::attach(|py| {
            for v in [
                PyFormat::Pdf,
                PyFormat::Docx,
                PyFormat::Xlsx,
                PyFormat::Pptx,
                PyFormat::Md,
            ] {
                let f = Py::new(py, v).unwrap();
                let has: bool = f
                    .bind(py)
                    .as_any()
                    .getattr("has_tables")
                    .unwrap()
                    .extract()
                    .unwrap();
                assert!(has);
            }
        });
    }

    #[test]
    fn test_format_str_and_repr() {
        Python::attach(|py| {
            let f = Py::new(py, PyFormat::Pdf).unwrap();
            let bound = f.bind(py).as_any();
            let s: String = bound.call_method0("__str__").unwrap().extract().unwrap();
            assert_eq!(s, "pdf");
            let r: String = bound.call_method0("__repr__").unwrap().extract().unwrap();
            assert_eq!(r, "Format.Pdf");
        });
    }

    #[test]
    fn test_format_from_str() {
        Python::attach(|py| {
            let cls = py.get_type::<PyFormat>();
            // PyFormat is `skip_from_py_object`, so we can't `.extract::<PyFormat>()`.
            // Instead we round-trip through Python identity equality (`is`).
            let parsed = cls.call_method1("from_str", ("pdf",)).unwrap();
            let pdf = Py::new(py, PyFormat::Pdf)
                .unwrap()
                .into_pyobject(py)
                .unwrap();
            assert!(parsed.eq(pdf).unwrap());
            let parsed_docx = cls.call_method1("from_str", ("docx",)).unwrap();
            let docx = Py::new(py, PyFormat::Docx)
                .unwrap()
                .into_pyobject(py)
                .unwrap();
            assert!(parsed_docx.eq(docx).unwrap());
            // Unknown name -> ValueError.
            let bogus = cls.call_method1("from_str", ("bogus",));
            assert!(bogus.is_err());
            let err = bogus.unwrap_err();
            assert!(err.is_instance_of::<PyValueError>(py));
        });
    }

    #[test]
    fn test_warning_repr() {
        Python::attach(|py| {
            let w = Py::new(
                py,
                PyWarning {
                    kind: "FontError".into(),
                    level: "warning".into(),
                    message: "ToUnicode missing".into(),
                    offset: Some(123),
                    page_index: Some(2),
                    detail: None,
                },
            )
            .unwrap();
            let bound = w.bind(py);
            let r: String = bound.call_method0("__repr__").unwrap().extract().unwrap();
            assert!(r.contains("FontError"));
            assert!(r.contains("warning"));
            assert!(r.contains("ToUnicode missing"));
            let s: String = bound.call_method0("__str__").unwrap().extract().unwrap();
            assert!(s.contains("page=2"));
            assert!(s.contains("offset=123"));
        });
    }

    #[test]
    fn test_image_repr() {
        Python::attach(|py| {
            let img = Py::new(
                py,
                PyImage {
                    node_id: 0,
                    asset_index: 0,
                    width: 640,
                    height: 480,
                    bits_per_component: 8,
                    filter: "jpeg".into(),
                    data: vec![0xFF, 0xD8, 0xFF],
                    alt_text: Some("a cat".into()),
                    bbox: None,
                },
            )
            .unwrap();
            let bound = img.bind(py);
            let r: String = bound.call_method0("__repr__").unwrap().extract().unwrap();
            assert!(r.contains("640"));
            assert!(r.contains("480"));
            assert!(r.contains("jpeg"));
            assert!(r.contains("a cat"));
        });
    }

    #[test]
    fn test_metadata_repr_dataclass_style() {
        Python::attach(|py| {
            let meta = Py::new(
                py,
                PyDocumentMetadata {
                    title: Some("Annual Report".into()),
                    author: Some("Author One".into()),
                    subject: None,
                    creator: None,
                    producer: None,
                    creation_date: None,
                    modification_date: None,
                    page_count: 42,
                    properties: Default::default(),
                },
            )
            .unwrap();
            let bound = meta.bind(py);
            let r: String = bound.call_method0("__repr__").unwrap().extract().unwrap();
            assert!(r.starts_with("DocumentMetadata("));
            assert!(r.contains("Annual Report"));
            assert!(r.contains("Author One"));
            assert!(r.contains("page_count=42"));
        });
    }

    #[test]
    fn test_table_repr() {
        Python::attach(|py| {
            let table = Py::new(
                py,
                PyTable {
                    node_id: 0,
                    rows: vec![],
                    num_columns: 3,
                    header_row_count: 1,
                    has_header_row: true,
                    may_continue_from_previous: false,
                    may_continue_to_next: false,
                },
            )
            .unwrap();
            let bound = table.bind(py);
            let r: String = bound.call_method0("__repr__").unwrap().extract().unwrap();
            assert!(r.contains("Table("));
            assert!(r.contains("num_columns=3"));
            assert!(r.contains("has_header_row=True"));
        });
    }

    #[test]
    fn test_page_text_concatenates_blocks() {
        Python::attach(|py| {
            let p1 = make_paragraph(py, "First paragraph.");
            let h1 = make_heading(py, "Heading One", 1);
            let page = Py::new(
                py,
                PyPage {
                    index: 0,
                    blocks: vec![p1, h1],
                },
            )
            .unwrap();
            let bound = page.bind(py);
            let text: String = bound.call_method0("text").unwrap().extract().unwrap();
            assert!(text.contains("First paragraph."));
            assert!(text.contains("Heading One"));
        });
    }

    #[test]
    fn test_page_render_raises_unsupported() {
        Python::attach(|py| {
            let page = Py::new(
                py,
                PyPage {
                    index: 0,
                    blocks: vec![],
                },
            )
            .unwrap();
            let bound = page.bind(py);
            let result = bound.call_method0("render");
            assert!(result.is_err());
        });
    }

    /// `Block.__rich_repr__` yields the kind and (for paragraph) the
    /// text payload. Confirms the protocol shape and field selection.
    #[test]
    fn test_block_rich_repr_yields_kind_and_text() {
        Python::attach(|py| {
            let block = make_paragraph(py, "hello rich");
            let bound = block.bind(py);
            let pairs_obj = bound.call_method0("__rich_repr__").unwrap();
            let pairs: Vec<(String, Py<PyAny>)> = pairs_obj.extract().unwrap();
            assert!(!pairs.is_empty());
            let names: Vec<&str> = pairs.iter().map(|(n, _)| n.as_str()).collect();
            assert!(names.contains(&"kind"), "names: {names:?}");
            assert!(names.contains(&"text"), "names: {names:?}");
            // kind value
            let kind_idx = names.iter().position(|n| *n == "kind").unwrap();
            let kind_val: String = pairs[kind_idx].1.extract(py).unwrap();
            assert_eq!(kind_val, "paragraph");
        });
    }

    /// `Table.__rich_repr__` yields rows count, num_columns, and the
    /// has_header_row flag.
    #[test]
    fn test_table_rich_repr_yields_rows_and_header_flag() {
        Python::attach(|py| {
            let table = Py::new(
                py,
                PyTable {
                    node_id: 0,
                    rows: vec![],
                    num_columns: 5,
                    header_row_count: 1,
                    has_header_row: true,
                    may_continue_from_previous: false,
                    may_continue_to_next: false,
                },
            )
            .unwrap();
            let bound = table.bind(py);
            let pairs_obj = bound.call_method0("__rich_repr__").unwrap();
            let pairs: Vec<(String, Py<PyAny>)> = pairs_obj.extract().unwrap();
            let names: Vec<&str> = pairs.iter().map(|(n, _)| n.as_str()).collect();
            assert!(names.contains(&"rows"), "names: {names:?}");
            assert!(names.contains(&"num_columns"), "names: {names:?}");
            assert!(names.contains(&"has_header_row"), "names: {names:?}");
            let cols_idx = names.iter().position(|n| *n == "num_columns").unwrap();
            let cols_val: usize = pairs[cols_idx].1.extract(py).unwrap();
            assert_eq!(cols_val, 5);
        });
    }

    /// `Format.__rich_repr__` returns the variant name and the three
    /// capability flags so `rich.print(Format.Pdf)` is informative.
    #[test]
    fn test_format_rich_repr_yields_capabilities() {
        Python::attach(|py| {
            let f = Py::new(py, PyFormat::Pdf).unwrap();
            let bound = f.bind(py);
            let pairs_obj = bound.as_any().call_method0("__rich_repr__").unwrap();
            let pairs: Vec<(String, Py<PyAny>)> = pairs_obj.extract().unwrap();
            let names: Vec<&str> = pairs.iter().map(|(n, _)| n.as_str()).collect();
            assert!(names.contains(&"name"));
            assert!(names.contains(&"can_render"));
            assert!(names.contains(&"has_pages"));
            assert!(names.contains(&"has_tables"));
            // Pdf can render.
            let cr_idx = names.iter().position(|n| *n == "can_render").unwrap();
            let cr_val: bool = pairs[cr_idx].1.extract(py).unwrap();
            assert!(cr_val);
        });
    }

    /// `Page.__rich_repr__` yields index + block count.
    #[test]
    fn test_page_rich_repr_yields_index_and_blocks() {
        Python::attach(|py| {
            let p = make_paragraph(py, "p");
            let page = Py::new(
                py,
                PyPage {
                    index: 7,
                    blocks: vec![p],
                },
            )
            .unwrap();
            let bound = page.bind(py);
            let pairs_obj = bound.call_method0("__rich_repr__").unwrap();
            let pairs: Vec<(String, Py<PyAny>)> = pairs_obj.extract().unwrap();
            let names: Vec<&str> = pairs.iter().map(|(n, _)| n.as_str()).collect();
            assert_eq!(names, vec!["index", "blocks"]);
            let idx_val: usize = pairs[0].1.extract(py).unwrap();
            assert_eq!(idx_val, 7);
            let blocks_val: usize = pairs[1].1.extract(py).unwrap();
            assert_eq!(blocks_val, 1);
        });
    }

    /// `attach_dataclass_fields` adds a `__dataclass_fields__` dict to a
    /// pyclass. Verify the helper directly, since the lib `register()` is
    /// only called from the cdylib `#[pymodule]` entry which doesn't run
    /// in the cargo test harness.
    #[test]
    fn test_dataclass_fields_attached() {
        Python::attach(|py| {
            let module = PyModule::new(py, "udoc_test").unwrap();
            module.add_class::<PyBlock>().unwrap();
            attach_dataclass_fields(
                &module,
                &py.get_type::<PyBlock>(),
                &["kind", "node_id", "text", "level"],
            )
            .unwrap();
            let cls = py.get_type::<PyBlock>();
            let fields = cls.as_any().getattr("__dataclass_fields__").unwrap();
            let dict = fields.cast_into::<PyDict>().unwrap();
            assert!(dict.contains("kind").unwrap());
            assert!(dict.contains("node_id").unwrap());
            assert!(dict.contains("text").unwrap());
            assert!(dict.contains("level").unwrap());
        });
    }
}

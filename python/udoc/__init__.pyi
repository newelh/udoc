"""Type stubs for the `udoc` document extraction toolkit.

Hand-written per S56 W2-STUBS. Mirrors the runtime surface produced by
`crates/udoc-py` (the pyo3 cdylib named `udoc.udoc`). Source of truth:

  * `phase-16-plan.md` Sec 6 (Python API surface).
  * `crates/udoc-py/src/{types,document,extract,config,corpus,chunks,errors}.rs`.

These stubs are NOT generated. The Python API diverges from the Rust shape
on purpose (DEC-145 frozen-pyclass + dataclass shim, kind-discriminant
Block/Inline, Corpus toolkit). Hand-written keeps mypy --strict honest
without forcing the Rust source to grow stub-gen annotations.

If a method shows up in `dir(udoc.SomeClass)` but is missing here, that is
a stub bug. The reverse (here but not on the runtime) is also a bug; the
`test_stubs_match_runtime` test catches both directions.
"""

from __future__ import annotations

import os
from pathlib import Path
from typing import (
    Any,
    Callable,
    Generic,
    Iterator,
    Literal,
    Optional,
    Sequence,
    TypeVar,
    Union,
    overload,
)

# ---------------------------------------------------------------------------
# Module attributes.
# ---------------------------------------------------------------------------

__version__: str

# A path-like input. Mirrors the kwarg-coercion in `extract.rs`.
PathLike = Union[str, os.PathLike[str], Path]

T = TypeVar("T")

# ---------------------------------------------------------------------------
# Format -- enum-style pyclass mirroring `udoc::Format`.
# ---------------------------------------------------------------------------

class Format:
    """The detected format of a document.

    Each variant is a class-level constant of `Format` itself (Format.Pdf,
    Format.Docx, ...). Capability accessors (`can_render`, `has_tables`,
    `has_pages`) are read-only properties.
    """

    Pdf: Format
    Docx: Format
    Xlsx: Format
    Pptx: Format
    Doc: Format
    Xls: Format
    Ppt: Format
    Odt: Format
    Ods: Format
    Odp: Format
    Rtf: Format
    Md: Format

    @property
    def can_render(self) -> bool: ...
    @property
    def has_tables(self) -> bool: ...
    @property
    def has_pages(self) -> bool: ...
    def __str__(self) -> str: ...
    def __repr__(self) -> str: ...
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...
    @classmethod
    def from_str(cls, s: str) -> Format: ...

# ---------------------------------------------------------------------------
# Geometry.
# ---------------------------------------------------------------------------

class BoundingBox:
    """An axis-aligned rectangle. Mirrors `udoc::BoundingBox`."""

    __match_args__: tuple[str, ...]
    x_min: float
    y_min: float
    x_max: float
    y_max: float
    @property
    def width(self) -> float: ...
    @property
    def height(self) -> float: ...
    @property
    def area(self) -> float: ...
    def __contains__(self, point: tuple[float, float]) -> bool: ...
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

# ---------------------------------------------------------------------------
# Inline span.
# ---------------------------------------------------------------------------

InlineKind = Literal[
    "text",
    "code",
    "link",
    "footnote_ref",
    "inline_image",
    "soft_break",
    "line_break",
]

class Inline:
    """An inline span within a block.

    The `kind` discriminant selects which fields are meaningful. Real fields
    (text, url, label, alt_text, image_index, ...) always exist on the
    pyclass and are None when the variant doesn't carry them. Misspelled
    attributes raise AttributeError with a kind-aware hint.
    """

    __match_args__: tuple[str, ...]
    @property
    def kind(self) -> InlineKind: ...
    @property
    def node_id(self) -> int: ...
    @property
    def text(self) -> Optional[str]: ...
    @property
    def bold(self) -> bool: ...
    @property
    def italic(self) -> bool: ...
    @property
    def underline(self) -> bool: ...
    @property
    def strikethrough(self) -> bool: ...
    @property
    def superscript(self) -> bool: ...
    @property
    def subscript(self) -> bool: ...
    @property
    def url(self) -> Optional[str]: ...
    @property
    def content(self) -> Sequence[Inline]: ...
    @property
    def label(self) -> Optional[str]: ...
    @property
    def alt_text(self) -> Optional[str]: ...
    @property
    def image_index(self) -> Optional[int]: ...
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...
    def __getattr__(self, name: str) -> Any: ...

# ---------------------------------------------------------------------------
# Block.
# ---------------------------------------------------------------------------

BlockKind = Literal[
    "paragraph",
    "heading",
    "list",
    "table",
    "code_block",
    "image",
    "page_break",
    "thematic_break",
    "section",
    "shape",
]

class Block:
    """A block-level element.

    The `kind` discriminant selects which fields are meaningful. Real
    fields fall through; synthetic attributes (`block.rows` for table
    blocks, `block.image_alt` for image blocks) are dispatched via
    __getattr__. Misspelled attributes raise AttributeError with a
    kind-aware hint.
    """

    __match_args__: tuple[str, ...]
    @property
    def kind(self) -> BlockKind: ...
    @property
    def node_id(self) -> int: ...
    @property
    def text(self) -> Optional[str]: ...
    @property
    def level(self) -> Optional[int]: ...
    @property
    def spans(self) -> Sequence[Inline]: ...
    @property
    def table(self) -> Optional[Table]: ...
    @property
    def list_kind(self) -> Optional[Literal["ordered", "unordered"]]: ...
    @property
    def list_start(self) -> Optional[int]: ...
    @property
    def items(self) -> Sequence[Sequence[Block]]: ...
    @property
    def language(self) -> Optional[str]: ...
    @property
    def image_index(self) -> Optional[int]: ...
    @property
    def alt_text(self) -> Optional[str]: ...
    @property
    def section_role(self) -> Optional[str]: ...
    @property
    def shape_kind(self) -> Optional[str]: ...
    @property
    def children(self) -> Sequence[Block]: ...
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...
    def __getattr__(self, name: str) -> Any: ...

# ---------------------------------------------------------------------------
# Table primitives.
# ---------------------------------------------------------------------------

class TableCell:
    """A table cell."""

    __match_args__: tuple[str, ...]
    node_id: int
    text: str
    content: Sequence[Block]
    col_span: int
    row_span: int
    value: Optional[str]
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

class TableRow:
    """A table row."""

    __match_args__: tuple[str, ...]
    node_id: int
    cells: Sequence[TableCell]
    is_header: bool
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

class Table:
    """A table. Carries shape metadata + the row vector."""

    __match_args__: tuple[str, ...]
    node_id: int
    rows: Sequence[TableRow]
    num_columns: int
    header_row_count: int
    has_header_row: bool
    may_continue_from_previous: bool
    may_continue_to_next: bool
    def to_pandas(self) -> Any:
        """Materialize the table as a pandas DataFrame. Imports
        `udoc.integrations.pandas` lazily; raises ImportError if pandas
        is not installed."""
        ...
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

# ---------------------------------------------------------------------------
# Image.
# ---------------------------------------------------------------------------

class Image:
    """An image asset with placement information."""

    __match_args__: tuple[str, ...]
    node_id: int
    asset_index: int
    width: int
    height: int
    bits_per_component: int
    filter: str
    data: bytes
    alt_text: Optional[str]
    bbox: Optional[BoundingBox]
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

# ---------------------------------------------------------------------------
# Warning.
# ---------------------------------------------------------------------------

class Warning:  # noqa: A001 (shadows builtin -- intentional, mirrors Rust)
    """A diagnostic warning emitted during extraction."""

    __match_args__: tuple[str, ...]
    kind: str
    level: str
    message: str
    offset: Optional[int]
    page_index: Optional[int]
    detail: Optional[str]
    def __repr__(self) -> str: ...
    def __str__(self) -> str: ...
    def __reduce__(self) -> Any: ...

# ---------------------------------------------------------------------------
# Page.
# ---------------------------------------------------------------------------

class Page:
    """A page of the document.

    For formats with no first-class page concept (DOCX, XLSX, MD, ...) a
    Page may aggregate the entire content spine into a single page.
    """

    __match_args__: tuple[str, ...]
    index: int
    blocks: Sequence[Block]
    def text(self) -> str: ...
    def text_lines(self) -> Sequence[Any]: ...
    def raw_spans(self) -> Sequence[Any]: ...
    def tables(self) -> Sequence[Table]: ...
    def images(self) -> Sequence[Image]: ...
    def render(self, dpi: int = 150) -> bytes: ...
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

# ---------------------------------------------------------------------------
# DocumentMetadata.
# ---------------------------------------------------------------------------

class DocumentMetadata:
    """Document-level metadata. Mirrors `udoc::DocumentMetadata`."""

    __match_args__: tuple[str, ...]
    title: Optional[str]
    author: Optional[str]
    subject: Optional[str]
    creator: Optional[str]
    producer: Optional[str]
    creation_date: Optional[str]
    modification_date: Optional[str]
    page_count: int
    properties: dict[str, str]
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

# ---------------------------------------------------------------------------
# Document.
# ---------------------------------------------------------------------------

ChunkBy = Literal["page", "heading", "section", "size", "semantic"]

class Document:
    """The result of an extraction.

    Iterators (`pages`, `blocks`, `tables`, `images`, `text_chunks`) are
    materialized lazily on first call and cached. The dunder shorthand
    `len(doc)`, `doc[0]`, `for page in doc:` walks pages.
    """

    @property
    def metadata(self) -> DocumentMetadata: ...
    @property
    def format(self) -> Optional[Format]: ...
    @property
    def source(self) -> Optional[Path]: ...
    @property
    def warnings(self) -> list[Warning]: ...
    @property
    def is_encrypted(self) -> bool: ...
    def pages(self) -> Iterator[Page]: ...
    def blocks(self) -> Iterator[Block]: ...
    def tables(self) -> Iterator[Table]: ...
    def images(self) -> Iterator[Image]: ...
    def text_chunks(
        self,
        *,
        by: ChunkBy = "heading",
        size: int = 2000,
    ) -> Iterator[Chunk]: ...
    def text(self) -> str: ...
    def to_markdown(self, *, with_anchors: bool = True) -> str: ...
    def to_dict(self) -> dict[str, Any]: ...
    def to_json(self, *, pretty: bool = False) -> str: ...
    def render_page(self, index: int, *, dpi: int = 150) -> bytes: ...
    def __len__(self) -> int: ...
    def __getitem__(self, idx: int) -> Page: ...
    def __iter__(self) -> Iterator[Page]: ...
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

# ---------------------------------------------------------------------------
# Chunk.
# ---------------------------------------------------------------------------

class ChunkSource:
    """Provenance for a chunk."""

    __match_args__: tuple[str, ...]
    page: Optional[int]
    block_ids: Sequence[int]
    bbox: Optional[BoundingBox]
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

class Chunk:
    """A chunk of text plus its provenance."""

    __match_args__: tuple[str, ...]
    text: str
    source: ChunkSource
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

# ---------------------------------------------------------------------------
# Configuration.
# ---------------------------------------------------------------------------

class Limits:
    """Resource limits for extraction. Mirrors `udoc_core::limits::Limits`."""

    __match_args__: tuple[str, ...]
    max_file_size: int
    max_pages: int
    max_nesting_depth: int
    max_table_rows: int
    max_cells_per_row: int
    max_text_length: int
    max_styles: int
    max_style_depth: int
    max_images: int
    max_decompressed_size: int
    max_warnings: Optional[int]
    memory_budget: Optional[int]
    def __init__(
        self,
        *,
        max_file_size: Optional[int] = None,
        max_pages: Optional[int] = None,
        max_nesting_depth: Optional[int] = None,
        max_table_rows: Optional[int] = None,
        max_cells_per_row: Optional[int] = None,
        max_text_length: Optional[int] = None,
        max_styles: Optional[int] = None,
        max_style_depth: Optional[int] = None,
        max_images: Optional[int] = None,
        max_decompressed_size: Optional[int] = None,
        max_warnings: Optional[int] = None,
        memory_budget: Optional[int] = None,
    ) -> None: ...
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

class Hooks:
    """Hooks (OCR, layout, annotate) configuration."""

    __match_args__: tuple[str, ...]
    ocr: Optional[str]
    layout: Optional[str]
    annotate: Optional[str]
    timeout: Optional[int]
    def __init__(
        self,
        *,
        ocr: Optional[str] = None,
        layout: Optional[str] = None,
        annotate: Optional[str] = None,
        timeout: Optional[int] = None,
    ) -> None: ...
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

class AssetConfig:
    """Asset extraction toggles."""

    __match_args__: tuple[str, ...]
    images: bool
    fonts: bool
    strict_fonts: bool
    def __init__(
        self,
        *,
        images: Optional[bool] = None,
        fonts: Optional[bool] = None,
        strict_fonts: Optional[bool] = None,
    ) -> None: ...
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

class LayerConfig:
    """Document-model layer toggles."""

    __match_args__: tuple[str, ...]
    presentation: bool
    relationships: bool
    interactions: bool
    def __init__(
        self,
        *,
        presentation: Optional[bool] = None,
        relationships: Optional[bool] = None,
        interactions: Optional[bool] = None,
    ) -> None: ...
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

class RenderConfig:
    """Render pipeline configuration."""

    __match_args__: tuple[str, ...]
    dpi: int
    profile: Literal["ocr_friendly", "visual"]
    def __init__(
        self,
        *,
        dpi: Optional[int] = None,
        profile: Optional[str] = None,
    ) -> None: ...
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

class Config:
    """The top-level configuration bag passed to `extract`.

    Frozen dataclass-like: fields are read-only. Mutate via
    `dataclasses.replace(cfg, ...)` or use the named presets.
    """

    __match_args__: tuple[str, ...]
    limits: Limits
    hooks: Hooks
    assets: AssetConfig
    layers: LayerConfig
    rendering: RenderConfig
    strict_fonts: bool
    memory_budget: Optional[int]
    format: Optional[str]
    password: Optional[str]
    collect_diagnostics: bool
    def __init__(
        self,
        *,
        limits: Optional[Limits] = None,
        hooks: Optional[Hooks] = None,
        assets: Optional[AssetConfig] = None,
        layers: Optional[LayerConfig] = None,
        rendering: Optional[RenderConfig] = None,
        strict_fonts: Optional[bool] = None,
        memory_budget: Optional[int] = None,
        format: Optional[str] = None,
        password: Optional[str] = None,
        collect_diagnostics: Optional[bool] = None,
    ) -> None: ...
    @classmethod
    def default(cls) -> Config: ...
    @classmethod
    def agent(cls) -> Config: ...
    @classmethod
    def batch(cls) -> Config: ...
    @classmethod
    def ocr(cls) -> Config: ...
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

# ---------------------------------------------------------------------------
# Streaming extractor.
# ---------------------------------------------------------------------------

class ExtractionContext:
    """The context manager returned by `udoc.stream(path, ...)`.

    Wraps a streaming extractor so callers can iterate pages without
    materializing the whole document.
    """

    def __enter__(self) -> ExtractionContext: ...
    def __exit__(
        self,
        exc_type: Optional[type[BaseException]] = None,
        exc_val: Optional[BaseException] = None,
        exc_tb: Optional[Any] = None,
    ) -> bool: ...
    def close(self) -> None: ...
    def __len__(self) -> int: ...
    def page_count(self) -> int: ...
    def __iter__(self) -> Iterator[str]: ...
    def page_text(self, index: int) -> str: ...
    def page_lines(
        self, index: int
    ) -> list[tuple[str, float, bool]]: ...
    def page_spans(
        self, index: int
    ) -> list[tuple[str, float, float, float, float]]: ...
    def page_tables(self, index: int) -> list[list[list[str]]]: ...
    def page_images(self, index: int) -> list[dict[str, Any]]: ...
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

class PyExtractionContextIter:
    """Iterator companion for `ExtractionContext.__iter__`."""

    def __iter__(self) -> PyExtractionContextIter: ...
    def __next__(self) -> str: ...
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

# ---------------------------------------------------------------------------
# Top-level functions.
# ---------------------------------------------------------------------------

PagesArg = Union[int, range, Sequence[int], str, None]

def extract(
    path: PathLike,
    *,
    pages: PagesArg = None,
    password: Optional[str] = None,
    format: Union[Format, str, None] = None,
    max_file_size: Optional[int] = None,
    config: Optional[Config] = None,
    on_warning: Optional[Callable[[Warning], None]] = None,
) -> Document:
    """Extract a document from a file path.

    Examples
    --------
    >>> doc = udoc.extract("report.pdf")
    >>> doc = udoc.extract("doc.pdf", pages=range(10))
    >>> doc = udoc.extract("doc.pdf", config=udoc.Config.agent())
    """
    ...

def extract_bytes(
    data: bytes,
    *,
    pages: PagesArg = None,
    password: Optional[str] = None,
    format: Union[Format, str, None] = None,
    max_file_size: Optional[int] = None,
    config: Optional[Config] = None,
    on_warning: Optional[Callable[[Warning], None]] = None,
) -> Document:
    """Extract a document from in-memory bytes.

    `format=` is recommended whenever the bytes lack a reliable magic
    signature.
    """
    ...

def stream(
    path: PathLike,
    *,
    pages: PagesArg = None,
    password: Optional[str] = None,
    format: Union[Format, str, None] = None,
    max_file_size: Optional[int] = None,
    config: Optional[Config] = None,
    on_warning: Optional[Callable[[Warning], None]] = None,
) -> ExtractionContext:
    """Open a streaming extractor.

    Use as a context manager:

        with udoc.stream("big.pdf") as ext:
            for i in range(len(ext)):
                process(ext.page_text(i))
    """
    ...

# ---------------------------------------------------------------------------
# Corpus toolkit.
# ---------------------------------------------------------------------------

class Sourced(Generic[T]):
    """Provenance wrapper for corpus-level aggregations.

    Returned by `Corpus.tables()`, `.images()`, `.chunks()`, `.metadata()`,
    `.warnings()`, `.render_pages()` so callers always know which file
    (and where in the file) a value came from.
    """

    __match_args__: tuple[str, ...]
    path: Path
    value: T
    page: Optional[int]
    block_id: Optional[int]
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

class Failed:
    """Per-document failure marker yielded during corpus iteration."""

    __match_args__: tuple[str, ...]
    path: Path
    error: UdocError
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

CorpusSource = Union[PathLike, Sequence[PathLike]]
CorpusMode = Literal["process", "thread"]

class Corpus:
    """A lazy iterable of Documents with batch fan-out / fan-in helpers.

    Examples
    --------
    >>> for s in udoc.Corpus("docs/").chunks(by="heading", size=2000):
    ...     index.add(text=s.value.text, source=s.path, page=s.page)
    """

    def __init__(
        self,
        source: CorpusSource,
        *,
        config: Union[Config, str, None] = None,
    ) -> None: ...
    def __iter__(self) -> Iterator[Union[Document, Failed]]: ...
    def __len__(self) -> int:
        """Always raises TypeError -- use `count()` instead. The method
        exists so `len(corpus)` produces a discoverable hint rather than
        silently materializing an O(N) directory walk."""
        ...
    def count(self) -> int:
        """Eager count of files in the source. I/O for directory sources."""
        ...
    def filter(self, predicate: Callable[[Document], bool]) -> Corpus: ...
    def with_config(self, config: Union[Config, str]) -> Corpus: ...
    def parallel(
        self,
        n_workers: int = 1,
        *,
        mode: CorpusMode = "process",
    ) -> Corpus: ...
    def text(self, *, join: str = "\n\n") -> str: ...
    def tables(self) -> Iterator[Sourced[Table]]: ...
    def images(self) -> Iterator[Sourced[Image]]: ...
    def chunks(
        self,
        *,
        by: ChunkBy = "heading",
        size: int = 2000,
    ) -> Iterator[Sourced[Chunk]]: ...
    def metadata(self) -> Iterator[Sourced[DocumentMetadata]]: ...
    def warnings(self) -> Iterator[Sourced[Warning]]: ...
    def render_pages(
        self,
        indices: Sequence[int],
        *,
        dpi: int = 150,
    ) -> Iterator[Sourced[bytes]]: ...
    def list(self) -> list[Document]: ...
    def to_jsonl(self, path: PathLike) -> int: ...
    def __repr__(self) -> str: ...
    def __reduce__(self) -> Any: ...

# ---------------------------------------------------------------------------
# Exception hierarchy (DEC-144).
# ---------------------------------------------------------------------------

class UdocError(Exception):
    """Base class for all udoc errors."""

class ExtractionError(UdocError):
    """Extraction failed for unspecified reasons. Catch-all for backend errors."""

class UnsupportedFormatError(UdocError):
    """The document format is not recognized or no backend is registered."""

class UnsupportedOperationError(UdocError):
    """The backend does not support the requested operation (e.g. render on DOCX)."""

class PasswordRequiredError(UdocError):
    """The document is encrypted and no password was provided."""

class WrongPasswordError(UdocError):
    """The provided password did not unlock the document."""

class LimitExceededError(UdocError):
    """A configured resource limit was exceeded during extraction."""

class HookError(UdocError):
    """An external hook (OCR, layout, annotate) failed."""

class IoError(UdocError):
    """An underlying I/O operation failed."""

class ParseError(UdocError):
    """The document could not be parsed; the bytes are malformed."""

class InvalidDocumentError(UdocError):
    """The document parses but its structure is invalid."""

class EncryptedDocumentError(UdocError):
    """The document is encrypted and decryption failed or is unsupported."""

# ---------------------------------------------------------------------------
# Submodule re-export -- `udoc.udoc` is the cdylib name.
# ---------------------------------------------------------------------------

# `from udoc.udoc import *` is what `udoc/__init__.py` does. The submodule
# itself is exposed for power users who want to bypass the wrapper.
udoc: Any

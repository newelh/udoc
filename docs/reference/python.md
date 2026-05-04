# Python API reference

The complete public surface of the `udoc` Python module. Mirrors
`python/udoc/__init__.pyi`; this page is the prose companion to
those stubs. For higher-level walkthroughs with worked examples,
see the [Library guide](../library.md).

The module is a PyO3 cdylib re-export: `import udoc` brings every
symbol into the `udoc` namespace. The cdylib itself is reachable as
`udoc.udoc` for callers that want to bypass the wrapper.

## Top-level functions

### `udoc.extract`

```python
def extract(
    path: PathLike,
    *,
    pages: PagesArg = None,
    password: str | None = None,
    format: Format | str | None = None,
    max_file_size: int | None = None,
    config: Config | None = None,
    on_warning: Callable[[Warning], None] | None = None,
) -> Document
```

One-shot extraction from a file path. Returns a fully-materialised
[`Document`](#document).

| Parameter       | Meaning                                                                                |
|-----------------|----------------------------------------------------------------------------------------|
| `path`          | Path to the file. `str`, `pathlib.Path`, or any `os.PathLike[str]`.                    |
| `pages`         | Page selector. `int`, `range`, sequence of ints, or a string spec like `"1,3,5-10"`.   |
| `password`      | Decryption password. PDF only; ignored for other formats.                              |
| `format`        | Force a backend instead of magic-byte detection. Accepts a `Format` or its `str` name. |
| `max_file_size` | Per-call override for the file-size limit. Falls back to `config.limits.max_file_size`.|
| `config`        | Full [`Config`](#config) bag. Wins over the per-call shortcuts above on conflict.      |
| `on_warning`    | Live callback for [`Warning`](#warning) records. Called on the extraction thread.      |

Raises one of the [exception types](#exceptions) on failure. Always
returns a `Document` even when the result is empty (e.g. a scanned
PDF without an OCR hook attached); inspect `doc.warnings` for the
recoverable issues that surfaced during extraction.

### `udoc.extract_bytes`

```python
def extract_bytes(
    data: bytes,
    *,
    pages: PagesArg = None,
    password: str | None = None,
    format: Format | str | None = None,
    max_file_size: int | None = None,
    config: Config | None = None,
    on_warning: Callable[[Warning], None] | None = None,
) -> Document
```

Same shape as [`extract`](#udocextract) but takes bytes already in
memory. Pass `format=` whenever the bytes lack a reliable magic
signature (e.g. RTF without the `{\rtf1` prefix, OOXML zips with
unusual ordering).

### `udoc.stream`

```python
def stream(
    path: PathLike,
    *,
    pages: PagesArg = None,
    password: str | None = None,
    format: Format | str | None = None,
    max_file_size: int | None = None,
    config: Config | None = None,
    on_warning: Callable[[Warning], None] | None = None,
) -> ExtractionContext
```

Open a streaming extractor. The returned
[`ExtractionContext`](#extractioncontext) is a context manager that
defers per-page work until requested. Use this for documents whose
content does not need to fit in memory at once.

```python
with udoc.stream("big.pdf") as ext:
    for i in range(len(ext)):
        text = ext.page_text(i)
```

`ExtractionContext` does not materialise a `Document`; it is the
streaming counterpart that hands out per-page text, lines, spans,
tables, and images on demand.

## Document

```python
class Document
```

The materialised result of [`extract`](#udocextract) or
[`extract_bytes`](#udocextract_bytes). Iterators
(`pages`, `blocks`, `tables`, `images`, `text_chunks`) are
materialised lazily on first call and then cached, so calling them
twice does not redo work.

| Property           | Type                                  | Notes                                                                    |
|--------------------|---------------------------------------|--------------------------------------------------------------------------|
| `metadata`         | [`DocumentMetadata`](#documentmetadata) | Always present, even when every field is `None`.                       |
| `format`           | [`Format`](#format) \| None           | None if format detection was inconclusive at this layer.                 |
| `source`           | `Path` \| None                        | `None` when extracted from `extract_bytes`.                              |
| `warnings`         | `list[Warning]`                       | Populated when `Config(collect_diagnostics=True)`, default off for `extract`. |
| `is_encrypted`     | `bool`                                | True when the source declared encryption (decrypted or not).             |

| Method                                | Notes                                                                     |
|---------------------------------------|---------------------------------------------------------------------------|
| `pages() -> Iterator[Page]`           | Iterates [`Page`](#page) objects in document order.                       |
| `blocks() -> Iterator[Block]`         | Walks the content spine recursively, yielding every [`Block`](#block).    |
| `tables() -> Iterator[Table]`         | Yields every [`Table`](#table) in document order.                         |
| `images() -> Iterator[Image]`         | Yields every [`Image`](#image) (block-level and inline).                  |
| `text() -> str`                       | Plain text reconstruction of the whole document.                          |
| `text_chunks(*, by, size) -> Iterator[Chunk]` | Yields [`Chunk`](#chunk) records sized for downstream embedding / LLM input. |
| `to_markdown(*, with_anchors)`        | Serialise to Markdown. `with_anchors=True` adds heading anchors.          |
| `to_dict() -> dict`                   | The full document model as a Python dict, ready for JSON.                 |
| `to_json(*, pretty)`                  | Same as `json.dumps(doc.to_dict())`; `pretty=True` indents.               |
| `render_page(index, *, dpi=150)`      | PNG bytes for one page. Raises `UnsupportedOperationError` on non-renderable formats. |

`len(doc)` and `doc[i]` walk pages. `for page in doc:` is
equivalent to `for page in doc.pages():`.

### `text_chunks`

```python
def text_chunks(
    self,
    *,
    by: ChunkBy = "heading",
    size: int = 2000,
) -> Iterator[Chunk]
```

`ChunkBy` is one of `"page"`, `"heading"`, `"section"`, `"size"`,
`"semantic"`. `size` is a soft target (characters) — chunks are
emitted at the natural boundary closest to the target, so they
tend to vary by ±20%. Each yielded [`Chunk`](#chunk) carries a
[`ChunkSource`](#chunk) so downstream indexers can record
provenance back to the page and originating block.

## Page

```python
class Page
```

A page of the document. For formats with no first-class page
concept (DOCX, Markdown, RTF), a `Page` aggregates the entire
content spine into a single page; treat the count as informational
in those cases.

| Property | Type             | Notes                                          |
|----------|------------------|------------------------------------------------|
| `index`  | `int`            | Zero-based page index.                         |
| `blocks` | `Sequence[Block]`| The page's content spine.                      |

| Method                          | Notes                                                       |
|---------------------------------|-------------------------------------------------------------|
| `text() -> str`                 | Reading-order text reconstruction for the page.             |
| `text_lines() -> Sequence`      | Line-broken text + per-line baseline / direction info.      |
| `raw_spans() -> Sequence`       | Positioned spans in content-stream order (PDF specifics; see [Library guide / Raw spans](../library.md#raw-spans-pdf)). |
| `tables() -> Sequence[Table]`   | Tables on this page, detected per the format's strategy.    |
| `images() -> Sequence[Image]`   | Images placed on this page.                                 |
| `render(dpi=150) -> bytes`      | PNG bytes for this page. Same renderer as `Document.render_page`. |

## Block

```python
class Block
```

A block-level element. The `kind` discriminant selects which fields
are meaningful:

| Kind             | Meaningful fields                                              |
|------------------|----------------------------------------------------------------|
| `paragraph`      | `text`, `spans`                                                |
| `heading`        | `text`, `spans`, `level` (1–6; clamp values outside)           |
| `list`           | `list_kind`, `list_start`, `items` (sequence-of-sequence-of-blocks) |
| `table`          | `table` ([`Table`](#table) handle)                             |
| `code_block`     | `text`, `language`                                             |
| `image`          | `image_index`, `alt_text`                                      |
| `page_break`     | (no extra fields)                                              |
| `thematic_break` | (no extra fields)                                              |
| `section`        | `section_role`, `children`                                     |
| `shape`          | `shape_kind`, `children`, `alt_text`                           |

`block.node_id` is the typed handle into the document's arena.
Overlay payloads (presentation, relationships, interactions) key
off this id. `block.text` returns the recursive text reconstruction
for any block kind. Misspelled attributes raise `AttributeError`
with a kind-aware hint.

`Block` is `__match_args__`-equipped, so structural matching
works:

```python
match block:
    case Block(kind="heading", level=l, text=t):
        index.add_section(level=l, title=t)
    case Block(kind="paragraph", text=t):
        index.add_paragraph(t)
```

## Inline

```python
class Inline
```

An inline span within a block. The `kind` discriminant selects
which fields are meaningful:

| Kind            | Meaningful fields                                        |
|-----------------|----------------------------------------------------------|
| `text`          | `text`, `bold`, `italic`, `underline`, `strikethrough`, `superscript`, `subscript` |
| `code`          | `text`                                                   |
| `link`          | `url`, `content` (sequence of child `Inline`)            |
| `footnote_ref`  | `label`                                                  |
| `inline_image`  | `image_index`, `alt_text`                                |
| `soft_break`    | (no extra fields — collapse to a space when reflowing)   |
| `line_break`    | (no extra fields — emit a hard newline)                  |

Style booleans are `False` (not `None`) when the variant does not
carry styling, so `inline.bold` is always safe to read.

## Table

```python
class Table
class TableRow
class TableCell
```

Table primitives. A `Table` carries shape metadata plus the row
vector; rows hold cells; cells hold content blocks (cells can
contain anything — paragraphs, lists, nested tables).

`Table` properties:

| Field                          | Notes                                                  |
|--------------------------------|--------------------------------------------------------|
| `rows`                         | Sequence of `TableRow`.                                |
| `num_columns`                  | Logical column count, after merge resolution.          |
| `header_row_count`             | How many leading rows are headers.                     |
| `has_header_row`               | `header_row_count > 0`.                                |
| `may_continue_from_previous`   | True when the table likely begins mid-flow on a page break. |
| `may_continue_to_next`         | True when the table likely continues onto the next page. |

`TableCell` properties: `text`, `content` (sequence of `Block`),
`col_span`, `row_span`, `value` (typed string for spreadsheet cells
with a normalised representation; `None` for plain text cells).

`Table.to_pandas()` materialises the table as a pandas DataFrame.
The pandas integration lives under `udoc.integrations.pandas` and
is imported lazily, so importing `udoc` does not pull in pandas.

## Image

```python
class Image
```

| Field               | Notes                                                                |
|---------------------|----------------------------------------------------------------------|
| `node_id`           | Arena handle.                                                        |
| `asset_index`       | Index into the document-wide image asset store (deduplicated).       |
| `width`, `height`   | Pixel dimensions.                                                    |
| `bits_per_component`| Bit depth.                                                           |
| `filter`            | The codec the bytes are stored in (`flate`, `dct`, `ccitt`, `jbig2`, `jpx`, ...).  |
| `data`              | Raw bytes. Decoded if the source format embedded a recognised codec. |
| `alt_text`          | Optional alt text from the source.                                   |
| `bbox`              | Optional [`BoundingBox`](#boundingbox) for placement.                |

Multiple `Image` objects can share an `asset_index` when the same
underlying bitmap is referenced from many places (slide deck logos,
DOCX repeating header images). The bytes live once in the asset
store; `Image` records carry per-placement metadata.

## DocumentMetadata

```python
class DocumentMetadata
```

| Field                  | Notes                                                  |
|------------------------|--------------------------------------------------------|
| `title`                | Document title.                                        |
| `author`               | Primary author.                                        |
| `subject`              | Subject (PDF Info dict, OOXML core).                   |
| `creator`              | Application that authored the document.                |
| `producer`             | Application that wrote the file (often a print driver).|
| `creation_date`        | ISO 8601 string when known.                            |
| `modification_date`    | ISO 8601 string when known.                            |
| `page_count`           | Logical page count. `0` for paged formats with no pages discovered. |
| `properties`           | `dict[str, str]` of format-specific extended fields.   |

Common keys in `properties`:

- `dc:creator`, `dc:subject`, `dc:description` — Dublin Core entries
  (OOXML core, ODF meta).
- `dcterms:created`, `dcterms:modified` — typed Dublin Core dates.
- `pdf:Producer`, `pdf:Creator`, `pdf:Trapped` — PDF Info dict
  entries that did not fit the structured fields above.
- `app:Application`, `app:AppVersion`, `app:Company`, `app:Pages`,
  `app:Words`, `app:Characters` — OOXML extended properties from
  `app.xml`.

The `properties` dict is open-ended. Format-specific guides list
the keys their backend writes.

## Warning

```python
class Warning
```

A typed diagnostic record from the extraction. Fields:

| Field        | Notes                                                          |
|--------------|----------------------------------------------------------------|
| `kind`       | Stable enum string (e.g. `"StreamLengthMismatch"`). Filter on this. |
| `level`      | `"info"` or `"warning"`.                                       |
| `message`    | Human-readable description. Do not parse.                      |
| `offset`     | Byte offset in the source when known.                          |
| `page_index` | Zero-based page when known.                                    |
| `detail`     | Optional supplementary string (e.g. obj reference, font name). |

Common `kind` values are listed in the
[Architecture](../architecture.md#diagnostics) page. New `kind`s
are added as backends grow; treat unknown kinds as `info`-level
unless `level` says otherwise.

The shadowing of the Python builtin `Warning` is intentional —
`udoc.Warning` mirrors the Rust shape. If you import warnings
generically alongside Python's, alias one of them: `from udoc
import Warning as UdocWarning`.

## Format

```python
class Format
```

Enum-style pyclass. Each variant is a class-level constant:

```python
udoc.Format.Pdf, Format.Docx, Format.Xlsx, Format.Pptx,
Format.Doc, Format.Xls, Format.Ppt,
Format.Odt, Format.Ods, Format.Odp,
Format.Rtf, Format.Md
```

Capability accessors (read-only properties):

| Accessor       | True when                                                     |
|----------------|---------------------------------------------------------------|
| `can_render`   | The backend implements page rasterisation (PDF only at present). |
| `has_tables`   | The backend produces `Block::Table` from native structures.   |
| `has_pages`    | The backend has a first-class page concept (PDF, PPTX, XLSX, legacy PPT/XLS, ODP, ODS). |

`Format.from_str("pdf")` parses the canonical lowercase name.
`str(format)` returns the same canonical name for round-tripping.

## BoundingBox

```python
class BoundingBox
```

Axis-aligned rectangle in PDF user space (origin lower-left,
y-axis points up). Fields: `x_min`, `y_min`, `x_max`, `y_max`,
plus computed properties `width`, `height`, `area`.

Supports `(x, y) in bbox` for point-in-rect tests. PDF page
coordinates run in points (1/72 inch).

## Chunk

```python
class Chunk
class ChunkSource
```

A chunk of text plus its provenance. `Chunk.text` is the chunk
body; `Chunk.source` is a `ChunkSource` carrying:

| Field        | Notes                                                          |
|--------------|----------------------------------------------------------------|
| `page`       | Originating page index (when the chunk is page-local).         |
| `block_ids`  | The `node_id`s the chunk was assembled from.                   |
| `bbox`       | Optional `BoundingBox` covering the chunk's source region.     |

The provenance is what makes `Document.text_chunks` useful for
RAG indexing — every chunk carries enough information to highlight
the source region in a viewer or recover the originating block
from `Document.blocks()`.

## Config

```python
class Config
```

The top-level configuration bag passed to `extract`,
`extract_bytes`, and `stream`. Frozen dataclass-like — fields are
read-only; mutate via `dataclasses.replace(cfg, ...)` or by
constructing a new `Config`.

| Field                  | Type            | Default                                              |
|------------------------|-----------------|------------------------------------------------------|
| `limits`               | [`Limits`](#limits)         | Conservative resource caps.              |
| `hooks`                | [`Hooks`](#hooks)           | No hooks attached.                       |
| `assets`               | [`AssetConfig`](#assetconfig) | Images and fonts both extracted.       |
| `layers`               | [`LayerConfig`](#layerconfig) | All overlays enabled.                  |
| `rendering`            | [`RenderConfig`](#renderconfig) | 150 DPI, viewer profile.             |
| `strict_fonts`         | `bool`          | `False`. When `True`, font fallbacks raise instead of warn. |
| `memory_budget`        | `int \| None`   | Soft per-process RSS cap. Triggers between-document cache resets. |
| `format`               | `str \| None`   | Force a backend (lowercase name).                    |
| `password`             | `str \| None`   | PDF decryption password.                             |
| `collect_diagnostics`  | `bool`          | When `True`, populates `Document.warnings`. Off for `extract` (the default `on_warning=None` already drops); on for the named presets that need warnings later. |

### Named presets

| Preset                | Tuned for                                                  |
|-----------------------|------------------------------------------------------------|
| `Config.default()`    | Interactive use, balanced defaults.                        |
| `Config.agent()`      | LLM agent loops — collects diagnostics, keeps overlays on. |
| `Config.batch()`      | Bulk ingest — disables expensive overlays, raises limits.  |
| `Config.ocr()`        | Hybrid scanned-document pipelines — pre-wires OCR-friendly render profile and detection. |

### Limits

```python
class Limits
```

Resource limits enforced during extraction. All fields default to
sensible caps and are overridden by passing keyword arguments.

| Field                  | Default unit | Purpose                                                    |
|------------------------|--------------|------------------------------------------------------------|
| `max_file_size`        | bytes        | Reject inputs larger than this.                            |
| `max_pages`            | count        | Cap per-document page count.                               |
| `max_nesting_depth`    | levels       | Recursion cap on Section/Shape/list nesting.               |
| `max_table_rows`       | count        | Per-table row cap.                                         |
| `max_cells_per_row`    | count        | Per-row cell cap.                                          |
| `max_text_length`      | chars        | Per-string cap on extracted text fragments.                |
| `max_styles`           | count        | Cap on distinct style records (defends against style-table bombs). |
| `max_style_depth`      | levels       | Cap on style cascade depth.                                |
| `max_images`           | count        | Per-document image cap.                                    |
| `max_decompressed_size`| bytes        | Cap on cumulative decompressed bytes (zip / flate bombs).  |
| `max_warnings`         | count or None| Cap on emitted diagnostics. None disables the cap.         |
| `memory_budget`        | bytes or None| Soft per-process RSS cap; cache reset trigger.             |

Hitting a limit raises [`LimitExceededError`](#exceptions).

### Hooks

```python
class Hooks
```

| Field      | Type            | Notes                                                       |
|------------|-----------------|-------------------------------------------------------------|
| `ocr`      | `str \| None`   | Path or `$PATH` name of an OCR hook executable.             |
| `layout`   | `str \| None`   | Layout-detection hook executable.                           |
| `annotate` | `str \| None`   | Annotation hook executable.                                 |
| `timeout`  | `int \| None`   | Per-request timeout in seconds. Default 60 s when omitted.  |

A hook is a long-lived subprocess that follows the
[hooks protocol](hooks.md). The hook executable receives one JSON
request per line on stdin and writes one JSON response per line on
stdout.

### AssetConfig

```python
class AssetConfig
```

| Field          | Default | Notes                                                          |
|----------------|---------|----------------------------------------------------------------|
| `images`       | `True`  | When `False`, image bytes are not loaded; references in the content tree become empty placeholders. |
| `fonts`        | `True`  | When `False`, font assets are not collected (saves memory on PDF). |
| `strict_fonts` | `False` | When `True`, missing ToUnicode CMaps raise instead of warn.    |

### LayerConfig

```python
class LayerConfig
```

| Field            | Default | Notes                                                          |
|------------------|---------|----------------------------------------------------------------|
| `presentation`   | `True`  | Geometry / fonts / colours overlay.                            |
| `relationships`  | `True`  | Footnotes / links / bookmarks overlay.                         |
| `interactions`   | `True`  | Form fields / comments / tracked-changes overlay.              |

Disabling an overlay skips the work that produces it; the content
spine is unaffected.

### RenderConfig

```python
class RenderConfig
```

| Field     | Default        | Notes                                                       |
|-----------|----------------|-------------------------------------------------------------|
| `dpi`     | `150`          | Render resolution. 300 for OCR-quality output.              |
| `profile` | `"visual"`     | One of `"visual"`, `"ocr_friendly"`. See [PDF rendering & OCR](../render.md). |

## ExtractionContext

```python
class ExtractionContext
```

Returned by [`udoc.stream`](#udocstream). A context manager that
holds a streaming extractor open for the duration of a `with`
block and surfaces per-page accessors.

| Method                        | Notes                                                          |
|-------------------------------|----------------------------------------------------------------|
| `__enter__`, `__exit__`       | Acquire / release the underlying file handle.                  |
| `close()`                     | Manual close. `__exit__` calls this.                           |
| `__len__()`, `page_count()`   | Total page count. `len(ctx)` is the idiomatic call.            |
| `__iter__()`                  | Iterates per-page reading-order text strings.                  |
| `page_text(i)`                | Reading-order text for one page.                               |
| `page_lines(i)`               | List of `(text, baseline_y, is_rtl)` tuples.                   |
| `page_spans(i)`               | List of `(text, x, y, w, h)` positioned spans.                 |
| `page_tables(i)`              | List of tables, each represented as `list[list[str]]`.         |
| `page_images(i)`              | List of dicts with image metadata + bytes for one page.        |

The streaming view trades the unified document model for memory
locality. Convert to a full `Document` with `udoc.extract(...)` if
you need block-level structure, overlays, or the richer chunking.

## Corpus

```python
class Corpus
```

A lazy iterable of `Document` instances built from a directory or a
sequence of paths. Constructed once; iterated many times. Designed
for batch ingest pipelines.

```python
udoc.Corpus("docs/")
udoc.Corpus(["a.pdf", "b.docx", "c.pptx"], config="batch")
```

`config=` accepts a `Config` instance or one of the preset names
`"default"`, `"agent"`, `"batch"`, `"ocr"`.

| Method / property               | Notes                                                                 |
|---------------------------------|-----------------------------------------------------------------------|
| `__iter__() -> Iterator[Document \| Failed]` | Yields one record per file; failures surface as [`Failed`](#failed) markers rather than raising. |
| `count() -> int`                | Eager count of files. I/O for directory sources.                      |
| `__len__()`                     | Always raises `TypeError` with a hint to call `count()`.              |
| `filter(pred)`                  | Returns a new `Corpus` with `pred(doc)` applied lazily.               |
| `with_config(cfg)`              | Returns a new `Corpus` using a different config.                      |
| `parallel(n_workers, *, mode)`  | Fans out across `n_workers`. `mode="process"` uses subprocess workers; `mode="thread"` uses a thread pool (best for I/O-bound input). |
| `text(*, join="\n\n")`          | Eager concatenation of every document's text.                         |
| `tables()`, `images()`, `chunks()`, `metadata()`, `warnings()` | Yield [`Sourced[T]`](#sourced) records with origin-file provenance attached. |
| `render_pages(indices, *, dpi=150)` | Yields `Sourced[bytes]` of rendered PNGs for the given page indices in each document. |
| `list()`                        | Materialise all documents into a list.                                |
| `to_jsonl(path)`                | Stream-write `Document.to_dict()` for each file as JSONL; returns the count written. |

### Sourced

```python
class Sourced(Generic[T])
```

Provenance wrapper around a value extracted from a corpus
iteration. Carries `path`, `value`, `page` (optional), and
`block_id` (optional). Returned by `Corpus.tables()`,
`.images()`, `.chunks()`, and friends so the caller never loses
track of which file (and which place in the file) a value came
from.

### Failed

```python
class Failed
```

Per-document failure marker yielded during corpus iteration.
Carries `path` and the originating `error` ([`UdocError`](#exceptions)
subclass). Iteration does not abort on a single failure — the
caller decides how to handle the marker.

## Async iteration (`udoc.asyncio`)

The `udoc.asyncio` module is a small wrapper that wraps the blocking
extractor in a thread pool so corpus iteration plays nicely with
`asyncio` consumers. See `python/udoc/asyncio.py` for the surface.

## Exceptions

All exceptions inherit from `udoc.UdocError`, which inherits from
`Exception`. Catch the base class to handle every udoc-originated
failure uniformly; catch the specific subclass when the caller
should react differently per cause.

| Class                          | Raised when                                                       |
|--------------------------------|-------------------------------------------------------------------|
| `UdocError`                    | Base class; never raised directly.                                |
| `ExtractionError`              | Backend-internal failure that does not fit a more specific class. |
| `UnsupportedFormatError`       | The bytes do not match a known backend (or `format=` is wrong).   |
| `UnsupportedOperationError`    | The backend cannot do what was asked (e.g. `render_page` on DOCX).|
| `PasswordRequiredError`        | Document is encrypted and no password was provided.               |
| `WrongPasswordError`           | The password did not unlock the document.                         |
| `LimitExceededError`           | A `Limits` cap was hit during extraction.                         |
| `HookError`                    | An OCR/layout/annotate hook failed fatally.                       |
| `IoError`                      | The underlying I/O operation failed.                              |
| `ParseError`                   | The bytes are malformed below the level of structured recovery.   |
| `InvalidDocumentError`         | The document parses but its structure is not coherent.            |
| `EncryptedDocumentError`       | Encryption decoding failed or is not supported.                   |

Every exception carries the same `code` attribute the CLI uses on
exit — agents can match `e.code` rather than parsing the message.
The full code list lives in [CLI reference / Exit
codes](../cli.md#exit-codes).

## Type aliases

| Alias            | Definition                                              |
|------------------|---------------------------------------------------------|
| `PathLike`       | `Union[str, os.PathLike[str], pathlib.Path]`            |
| `PagesArg`       | `Union[int, range, Sequence[int], str, None]`           |
| `ChunkBy`        | `Literal["page", "heading", "section", "size", "semantic"]` |
| `InlineKind`     | `Literal["text", "code", "link", "footnote_ref", "inline_image", "soft_break", "line_break"]` |
| `BlockKind`      | `Literal["paragraph", "heading", "list", "table", "code_block", "image", "page_break", "thematic_break", "section", "shape"]` |
| `CorpusSource`   | `Union[PathLike, Sequence[PathLike]]`                   |
| `CorpusMode`     | `Literal["process", "thread"]`                          |

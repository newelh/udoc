# Library guide

How to use udoc from Python — patterns, configuration,
trade-offs, with worked examples. This page is the prose entry
to the Reference section; the strict surface lives in the
sibling pages: [Python API](reference/python.md), [Document
model](reference/document-model.md), [Hook
protocol](reference/hooks.md), [CLI](cli.md), and [Agent
instructions](agents.md). The Rust API mirrors the Python shape
and is exposed for callers who want to skip the wheel.

## A first extraction

```python
import udoc

doc = udoc.extract("paper.pdf")

print(doc.metadata.title)
for block in doc.blocks():
    print(block.text)
```

`udoc.extract` takes a path, runs the right backend for the
detected format, and returns a fully-materialised `Document`.
Iterators on `Document` (`pages()`, `blocks()`, `tables()`,
`images()`, `text_chunks()`) are lazy — they materialise on
first call and cache, so calling them twice does not redo work.

`udoc.extract_bytes(data, format=...)` is the in-memory
counterpart for cases where the source is a buffer rather than a
file.

<details>
<summary>Rust equivalent</summary>

```rust
let doc = udoc::extract("paper.pdf")?;
println!("{}", doc.metadata.title.as_deref().unwrap_or("(untitled)"));
for block in &doc.content {
    println!("{}", block.text());
}
# Ok::<(), udoc::Error>(())
```

The Rust API exposes `doc.content` as a `Vec<Block>` directly;
the Python wrapper hides that behind iterator methods so it can
materialise spine and overlays lazily.
</details>

## The document model

```text
Document
├── metadata          DocumentMetadata
├── pages()           -> Iterator[Page]      lazy view onto the spine
├── blocks()          -> Iterator[Block]     recursive walk
├── tables()          -> Iterator[Table]
├── images()          -> Iterator[Image]
├── text()            -> str                 plain-text reconstruction
├── text_chunks(...)  -> Iterator[Chunk]     for embedding / LLM input
├── to_markdown(...)  -> str
├── to_dict() / to_json(...)
└── render_page(i, dpi=...) -> bytes
```

The spine is the text-bearing structure: a tree of `Block` nodes
(paragraphs, headings, lists, tables, images, sections, shapes)
holding `Inline` children (text spans, links, inline images,
breaks). Three overlays (presentation, relationships,
interactions) carry geometry, links, and form-field data keyed
off the same `node_id`s. Overlays are independently optional.
Disabling one skips the work that produces it without touching
the spine.

The full type list with field-level shape lives in
[Document model reference](reference/document-model.md). The
short version: pattern-match on `block.kind` for variant-specific
fields, treat `node_id` as an opaque arena handle, and trust
`block.text` to walk children recursively when you want plain
text.

```python
doc = udoc.extract("paper.pdf")

for block in doc.blocks():
    match block.kind:
        case "heading":
            print("##" * (block.level or 1), block.text)
        case "paragraph":
            print(block.text)
        case "table":
            for row in block.table.rows:
                print("\t".join(c.text for c in row.cells))
        case "image":
            img = doc.images()  # shared store; image_index is the handle
```

## Streaming large documents

`udoc.stream` defers per-page work until you ask for it. The
returned `ExtractionContext` is a context manager; the underlying
file handle and parser state are released when the `with` block
exits.

```python
import udoc

with udoc.stream("big.pdf") as ext:
    for i in range(len(ext)):
        text = ext.page_text(i)
        if "attention" in text.lower():
            print(f"page {i}: hit")
```

`ExtractionContext` exposes per-page accessors (`page_text`,
`page_lines`, `page_spans`, `page_tables`, `page_images`) that
return small typed views without materialising a full `Document`.
The trade-off: you give up overlays, chunking, and cross-page
metadata in exchange for memory locality.

Reach for `stream` when the document is large enough that holding
the whole `Document` in memory would matter. For most agent and
ETL paths, `extract` is simpler and the difference is negligible.

<details>
<summary>Rust equivalent</summary>

```rust
let mut ext = udoc::Extractor::open("big.pdf")?;
for i in 0..ext.page_count() {
    let text = ext.page_text(i)?;
    if text.to_lowercase().contains("attention") {
        println!("page {i}: hit");
    }
}
# Ok::<(), udoc::Error>(())
```
</details>

## Configuration

`udoc.Config` is the configuration bag passed to `extract`,
`extract_bytes`, and `stream`. It is a frozen dataclass — fields
are read-only; build a new `Config` rather than mutating in place.

The common shortcuts are exposed as keyword arguments on the
extraction calls themselves; reach for `Config` when you need
something the shortcuts do not cover (overlay toggles, hooks,
non-default limits).

```python
cfg = udoc.Config(
    format="pdf",                      # skip detection
    password="secret",                 # PDF encryption
    layers=udoc.LayerConfig(           # disable overlays you don't need
        presentation=False,
        relationships=False,
        interactions=False,
    ),
    rendering=udoc.RenderConfig(dpi=300, profile="ocr_friendly"),
    collect_diagnostics=True,          # surface warnings on doc.warnings
)
doc = udoc.extract("paper.pdf", config=cfg, pages="1,3,5-10")
```

The pieces that make up a `Config`:

| Piece          | Purpose                                                              |
|----------------|----------------------------------------------------------------------|
| `Limits`       | Resource caps (file size, page count, decompressed size, ...).       |
| `Hooks`        | OCR / layout / annotate hook commands.                               |
| `AssetConfig`  | Image and font extraction toggles.                                   |
| `LayerConfig`  | Presentation / relationships / interactions overlay toggles.         |
| `RenderConfig` | DPI and profile for `render_page` and the OCR pipeline.              |

See the [Python reference / Config](reference/python.md#config)
for the full field set.

### Named presets

```python
udoc.Config.default()   # interactive defaults
udoc.Config.agent()     # collects diagnostics, keeps overlays on
udoc.Config.batch()     # disables expensive overlays, raises limits
udoc.Config.ocr()       # OCR-friendly render profile + scan detection
```

The presets are the right starting point for the shape of pipeline
you are building; tweak with `dataclasses.replace(...)` if you
want a one-off override.

### What overlays cost

Roughly:

- **Presentation**: 5–15% of total extraction time on PDF,
  depending on bounding-box density. Required if you want to
  render pages or feed a layout model.
- **Relationships**: typically under 5%. Worth keeping on unless
  you know the documents have no footnotes / links / bookmarks.
- **Interactions**: zero unless the document actually has form
  fields, comments, or tracked changes.

If you only need text, set every layer to `False` via
`LayerConfig`. Tables and images live on the content spine
itself; toggle them via `AssetConfig(images=False)` and (for
PDF table detection) `Config(tables=False)`. Each saves around
15–20% of extraction time on table-heavy or image-heavy
documents.

PDF table detection runs heuristic strategies (ruled lattice plus
text-edge column detection) and is best-effort — clean for
born-digital documents with ruled tables, degraded on scans,
dense unruled tables, rotated headers, and mixed-layout pages.
For hard cases, attach a layout-detection or OCR hook before
extraction. See [PDF format guide / Table detection](formats/pdf.md#table-detection)
and [PDF rendering & OCR](render.md) for the failure modes and
the hook recipes.

## Chunking for downstream consumers

`Document.text_chunks(...)` slices the spine into chunks suitable
for embedding, RAG, or LLM input. Each chunk carries its source
provenance so a downstream indexer can recover the page and
originating block from a hit.

```python
import udoc

doc = udoc.extract("paper.pdf")

for chunk in doc.text_chunks(by="heading", size=2000):
    embed(
        text=chunk.text,
        page=chunk.source.page,
        block_ids=chunk.source.block_ids,
        bbox=chunk.source.bbox,
    )
```

The `by` strategies:

| Strategy   | Boundary                                                              |
|------------|-----------------------------------------------------------------------|
| `"page"`   | One chunk per page.                                                   |
| `"heading"`| Chunk between consecutive headings.                                   |
| `"section"`| Chunk per `Block::Section` container (when the format has them).      |
| `"size"`   | Greedy size-based packing; respects paragraph boundaries.             |
| `"semantic"` | Heuristic: heading-aware with size cap when the section is too large. |

`size` is a soft target in characters; the chunker emits at the
natural boundary nearest the target, so chunks vary by ±20%. The
default (`by="heading"`, `size=2000`) is a reasonable starting
point; tune `size` to your model's context window and `by` to
the structure your documents tend to have.

## Diagnostics

Recoverable issues during extraction (font fallback, malformed
xref, stream-length mismatch, ZIP central-directory mismatch,
hook failures) flow through a typed warnings sink. Two ways to
collect them:

```python
# 1. Live callback. Called on the extraction thread per warning.
def log(w):
    print(f"[{w.level}] {w.kind} (page {w.page_index}): {w.message}")

doc = udoc.extract("paper.pdf", on_warning=log)
```

```python
# 2. Collect on the document. Set `collect_diagnostics=True` and
#    read `doc.warnings` after extraction.
cfg = udoc.Config(collect_diagnostics=True)
doc = udoc.extract("paper.pdf", config=cfg)

for w in doc.warnings:
    if w.kind == "StreamLengthMismatch":
        ...
```

A warning carries `kind` (typed enum string), `level` (`"info"`
or `"warning"`), `message` (human-readable), `page_index`,
`offset`, and an optional `detail`. Filter on `kind` in agent
loops and CI pipelines. `message` is for humans. The full
catalogue of `kind` values lives in [Architecture /
Diagnostics](architecture.md#diagnostics).

`Config.agent()` and `Config.batch()` both pre-set
`collect_diagnostics=True`. `Config.default()` does not, on the
assumption that interactive callers either pass `on_warning=` or
do not care.

<details>
<summary>Rust equivalent</summary>

```rust
use std::sync::Arc;
use udoc::diagnostics::{CollectingDiagnostics, DiagnosticsSink};

let diag = Arc::new(CollectingDiagnostics::new());
let cfg = udoc::Config::new().diagnostics(diag.clone());
let _doc = udoc::extract_with("paper.pdf", cfg)?;

for w in diag.warnings() {
    eprintln!("[{}] {}: {}", w.level, w.kind, w.message);
}
# Ok::<(), udoc::Error>(())
```

For live emission, implement `DiagnosticsSink` directly:

```rust
struct LogSink;
impl udoc::diagnostics::DiagnosticsSink for LogSink {
    fn warning(&self, w: udoc::diagnostics::Warning) {
        log::warn!("{}: {}", w.kind, w.message);
    }
}
```
</details>

## Resource limits

`Limits` bounds per-document resource use. The defaults are
conservative; raise them when you ingest large documents and
trust the source.

```python
cfg = udoc.Config(
    limits=udoc.Limits(
        max_file_size=1_000_000_000,        # 1 GB
        max_pages=50_000,
        max_decompressed_size=2_000_000_000,
    ),
)
doc = udoc.extract("paper.pdf", config=cfg)
```

For batch workers ingesting thousands of documents in one
process, `Config(memory_budget=...)` is an opt-in soft RSS cap.
When the budget is crossed between documents, per-document caches
are reset before the next extraction begins.

Hitting a limit raises `udoc.LimitExceededError`.

The full list of fields lives in
[Reference / Limits](reference/python.md#limits); the table
there is the canonical surface.

## Hooks

OCR, layout-detection, and annotation models plug in through the
[JSONL hook protocol](hooks.md). From Python:

```python
cfg = udoc.Config(
    hooks=udoc.Hooks(
        ocr="tesseract-hook",
        layout="doclayout-yolo",
        timeout=120,                  # per-request timeout in seconds
    ),
)
doc = udoc.extract("scanned.pdf", config=cfg)
```

`tesseract-hook` and `doclayout-yolo` are executable names
resolved against `$PATH` (or absolute paths). The hook
subprocess is spawned once per extraction and reused across every
page.

**OCR fires on a subset of pages by default.** A page goes through
the OCR hook only when its extracted text has fewer than 10
whitespace-separated words; pages that already have clean text are
skipped. To force OCR on every page, pass `--ocr-all` on the CLI
(the Python `Hooks` config does not yet expose the override).
Layout and annotate phases fire on every page unconditionally.
Full firing rules and knobs in [Hooks / When does a hook
fire?](hooks.md#when-does-a-hook-fire).

The full protocol (handshake, request/response shapes, error
kinds) lives in [Reference / Hooks protocol](reference/hooks.md).
The narrative tutorial with worked hooks is in [Hooks and LLM
integration](hooks.md).

## Batch processing with `Corpus`

`udoc.Corpus` is a lazy iterable of `Document` instances built
from a directory or a list of paths. It is the right tool for
ingest pipelines, backfills, and large-scale evaluations.

```python
import udoc

corpus = udoc.Corpus("./docs", config="batch")

for doc_or_failed in corpus:
    match doc_or_failed:
        case udoc.Document() as doc:
            index_document(doc)
        case udoc.Failed(path=p, error=e):
            log_failure(p, e)
```

Failures surface as `Failed` markers rather than raising —
iteration does not abort on a single bad document. The caller
decides how to handle them.

Common ergonomic surfaces:

```python
# Stream chunks across the whole corpus, keeping origin provenance.
for sourced in corpus.chunks(by="heading", size=2000):
    index.add(text=sourced.value.text, path=sourced.path,
              page=sourced.page, block_id=sourced.block_id)

# Fan out across worker processes (CPU-bound) or threads (I/O-bound).
for doc in corpus.parallel(8, mode="process"):
    ...

# Filter lazily, then materialise.
docs = corpus.filter(lambda d: d.metadata.page_count > 5).list()

# Stream the whole thing to a JSONL file for offline analysis.
n_written = corpus.to_jsonl("corpus.jsonl")
```

The full surface (`tables()`, `images()`, `metadata()`,
`warnings()`, `render_pages()`, `count()`, `with_config()`) is
in [Reference / Corpus](reference/python.md#corpus). Each returns
a `Sourced[T]` so the caller never loses track of which file
(and which place in the file) a value came from.

## Errors

Python errors are typed exceptions inheriting from
`udoc.UdocError`:

```python
import udoc

try:
    doc = udoc.extract("missing.pdf")
except udoc.PasswordRequiredError:
    ...
except udoc.UnsupportedFormatError:
    ...
except udoc.LimitExceededError as e:
    # The cap that was hit is on `e.code`.
    ...
except udoc.UdocError as e:
    print(e.code, e)
```

Every exception carries a stable `code` attribute that matches
the [CLI exit codes](cli.md#exit-codes); agents match on `e.code`
rather than parsing prose. The full hierarchy is in
[Reference / Exceptions](reference/python.md#exceptions).

<details>
<summary>Rust: <code>udoc::Error</code></summary>

`udoc::Error` carries a context chain. The top-level message
describes what failed; chained context describes what it was
doing.

```text
Error: parsing object at offset 12345
  caused by: reading token
  caused by: I/O error: unexpected end of file
```

`Error::code()` returns the same stable error code Python sees;
agents match on that without parsing prose.
</details>

## Escape hatches

The facade is the right answer for most uses. The remaining cases
want something it does not give:

- Raw, unordered spans straight from the PDF content stream.
- A typed handle to a per-format backend that exposes
  format-specific knobs.
- A per-format `Document` type that does not pay the conversion
  cost into the unified model.

These are documented design choices, not hidden internals.

### Raw spans (PDF)

`page_spans` returns text in content-stream order with no
reading-order heuristic applied. Use this when you are doing your
own layout analysis or feeding a layout-detection model.

```python
with udoc.stream("paper.pdf") as ext:
    for text, x, y, w, h in ext.page_spans(0):
        print(f"({x:.1f}, {y:.1f}) {text!r}")
```

Trade-offs:

- **Less stable.** Raw spans expose more of the PDF content
  stream; if you depend on them you are coupled to the
  producer's content-stream habits.
- **Skips ordering work.** Faster than `page_text`, but you
  reorder spans yourself if you want reading-order text.

The reading-order pipeline that backs `page_text` is documented
in [PDF format guide / Reading order](formats/pdf.md#reading-order).

<details>
<summary>Rust: drop into <code>udoc-pdf</code> directly</summary>

```rust
let mut doc = udoc_pdf::Document::open("paper.pdf")?;
let mut page = doc.page(0)?;
for span in page.raw_spans()? {
    println!("({:.1}, {:.1}) {}", span.x, span.y, span.text);
}
# Ok::<(), udoc_pdf::Error>(())
```
</details>

### Direct backend access (Rust)

Each backend ships as a separate crate with its own `Document`
type. Useful when you know the format and want a typed handle
that does not go through the unified model:

| Crate              | Format                                      |
|--------------------|---------------------------------------------|
| `udoc_pdf`         | PDF parser API.                             |
| `udoc_docx`        | DOCX walker.                                |
| `udoc_xlsx`        | XLSX with typed cells.                      |
| `udoc_pptx`        | PPTX shape tree.                            |
| `udoc_doc`, `udoc_xls`, `udoc_ppt` | Legacy binary formats.      |
| `udoc_odf`         | ODT / ODS / ODP.                            |
| `udoc_rtf`, `udoc_markdown` | RTF, Markdown.                     |

Trade-offs:

- **Format-specific surface.** The PDF backend exposes things
  the DOCX backend does not. There is no portable code over the
  per-format APIs.
- **No automatic dispatch.** You commit to a format at the type
  level.

## Where to look next

- [Reference / Python API](reference/python.md) — the strict
  surface: every function, class, and exception with full
  signatures.
- [Reference / Document model](reference/document-model.md) —
  block and inline variants, overlays, asset store.
- [Reference / Hooks protocol](reference/hooks.md) —
  `udoc-hook-v1` wire format.
- [Hooks chapter](hooks.md) — narrative tutorial with worked
  Python hooks.
- [PDF format guide](formats/pdf.md) — reading-order tiers,
  table-detection strategies, failure modes.
- [PDF rendering & OCR](render.md) — the rasteriser and the
  OCR-detection pipeline.
- [Architecture](architecture.md) — design tenets, the document
  model, performance.


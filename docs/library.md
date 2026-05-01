# Library guide

How to use udoc as a library. Python is the primary surface; the Rust
API is the same shape, exposed for callers who want to skip the wheel.

## The facade

`udoc.extract` is the simple, opinionated entry point: one call in, a
`Document` out.

### Python

```python
import udoc

doc = udoc.extract("paper.pdf")
print(doc.metadata.title)
for block in doc.content:
    print(block.text)
```

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
</details>

## The document model

Every backend converges on the same shape:

```python
doc = udoc.extract("paper.pdf")

doc.metadata          # title, author, page_count, created, ...
doc.content           # list[Block]: paragraphs, headings, tables, lists, images
doc.presentation      # bounding boxes, fonts, colours (overlay; opt out to skip)
doc.relationships     # footnotes, links, bookmarks (overlay)
doc.interactions      # form fields, comments, tracked changes (overlay)
doc.images            # shared image store referenced by Block::Image
```

The **content spine** (`doc.content`) is the canonical text-bearing
structure: a tree of `Block` nodes (paragraphs, headings, lists, tables,
images) and inline `Inline` nodes (text, bold, italic, links, code).
Every node carries a typed `node_id` into the document's arena.

Three **overlays** carry information that is logically separate from the
spine but keyed to the same node ids: presentation (where on the page is
this paragraph? what font?), relationships (this paragraph is referenced
from a footnote on page 12), interactions (this paragraph contains a
form field).

Overlays are independently toggleable. Disabling one skips the work that
produces it; the spine of the document is unaffected.

## Streaming

For large documents, do not call `extract` — use the streaming
`Extractor` instead. It defers per-page work until you ask for it.

### Python

```python
with udoc.open("large.pdf") as ext:
    for i in range(ext.page_count):
        page = ext.page(i)
        # `page` is a small typed view: per-page text, tables, images.
        print(page.text[:80])
```

<details>
<summary>Rust equivalent</summary>

```rust
let mut ext = udoc::Extractor::open("large.pdf")?;
for i in 0..ext.page_count() {
    let text = ext.page_text(i)?;
    // ... do something with this page, then drop it
}
# Ok::<(), udoc::Error>(())
```
</details>

`Extractor` only holds the parsed structure index in memory at any time;
per-page content is allocated when requested and dropped when you move
on.

## Configuration

```python
cfg = udoc.Config(
    format=udoc.Format.PDF,        # skip detection
    password="secret",             # PDF encryption
    pages="1,3,5-10",              # subset
    presentation=False,            # skip the geometry/font overlay
    relationships=False,           # skip footnotes/links overlay
    interactions=False,            # skip form-fields overlay
    tables=True,                   # table detection (default on)
    images=True,                   # image extraction (default on)
)
doc = udoc.extract("paper.pdf", config=cfg)
```

<details>
<summary>Rust equivalent</summary>

```rust
use udoc::{Config, Format, LayerConfig};

let cfg = Config::new()
    .format(Format::Pdf)
    .password("secret")
    .pages("1,3,5-10")?
    .layers(LayerConfig::content_only());

let doc = udoc::extract_bytes_with(&bytes, cfg)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```
</details>

### What overlays cost

Roughly:

- Presentation overlay: 5-15% of total extraction time on PDF, depending
  on bounding-box density.
- Relationships overlay: typically under 5%.
- Interactions overlay: zero unless the document actually has form
  fields or tracked changes.

If you only need text, set `presentation=False, relationships=False,
interactions=False`. Tables and images are part of the content spine
itself; toggle them via `tables=False` / `images=False` (each saves
roughly 15-20% of extraction time on table-heavy or image-heavy
documents).

PDF table detection runs heuristic strategies (ruled lattice plus
text-edge column detection) and is best-effort: it handles
born-digital documents with clean ruling and standard layouts well,
and degrades on scans, dense unruled tables, rotated headers, and
mixed-layout pages. For hard cases, attach a layout-detection or OCR
hook before extraction. See the [PDF format guide / Table
detection](formats/pdf.md#table-detection) and
[PDF rendering & OCR](render.md) for the failure modes and the hook
recipes.

## Escape hatches

The facade is the right answer for ~95% of uses. The remaining 5% wants
something the facade does not give:

- Raw, unordered spans straight from the PDF content stream.
- A typed handle to the per-format backend that exposes format-specific
  knobs.
- A per-format `Document` type that does not pay the conversion cost
  into the unified model.

These are documented design choices, not hidden internals. Each backend
crate exposes a typed API; reach for it when you need to.

### Raw spans (PDF)

`raw_spans` returns text in content-stream order with no reading-order
heuristic applied. Use this when you are doing your own layout analysis
or feeding a layout-detection model.

```python
import udoc

# Python: facade exposes raw_spans on the streaming page view.
with udoc.open("paper.pdf") as ext:
    page = ext.page(0)
    for span in page.raw_spans:
        print(f"({span.x:.1f}, {span.y:.1f}) {span.text!r}")
```

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

Tradeoffs:

- **Less stable.** Raw spans expose more of the PDF content stream; if
  you depend on them you are coupled to the producer's content-stream
  habits.
- **Skips ordering work.** Faster than `text()`, but you reorder spans
  yourself if you want reading-order text.

The reading-order pipeline that backs `text()` is documented in
[per-format / PDF](formats/pdf.md#reading-order); start there if you are
deciding between `raw_spans` and the higher tiers.

### Direct backend access

Each backend ships as a separate crate with its own `Document` type.
This is useful when you know the format and want a typed handle that
does not go through the unified model:

- `udoc_pdf` — PDF parser API.
- `udoc_docx` — DOCX walker.
- `udoc_xlsx` — XLSX with typed cells.
- `udoc_pptx` — PPTX shape tree.
- `udoc_doc`, `udoc_xls`, `udoc_ppt` — legacy binary formats.
- `udoc_odf` — ODT / ODS / ODP.
- `udoc_rtf`, `udoc_markdown`.

Tradeoffs:

- **Format-specific surface.** The PDF backend exposes things the DOCX
  backend does not. There is no portable code over the per-format APIs.
- **No automatic dispatch.** You commit to a format at the type level.

## Diagnostics

Recoverable issues during extraction (font fallback, malformed xref,
stream-length mismatch, ZIP central-directory mismatch) flow through a
`DiagnosticsSink`. By default they are dropped; to collect them:

```python
import udoc

warnings = []
doc = udoc.extract("paper.pdf", diagnostics=warnings.append)

for w in warnings:
    print(f"[{w.level}] {w.kind} (page {w.page}): {w.message}")
```

A warning carries:

- `kind` — typed enum (`StreamLengthMismatch`, `ToUnicodeMissing`,
  `MalformedXref`, `UnsupportedFilter`, `DocFastSaveFallback`, ...).
- `level` — `info` or `warning`.
- `page` — the 0-indexed page where the issue surfaced (when known).
- `offset` — byte offset in the source file (when known).
- `message` — human-readable description.

Filter on `kind` in CI pipelines and agent loops; `message` is for
humans.

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

To emit warnings live, implement `DiagnosticsSink` directly:

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

`Config.limits` (Python) / `Config::limits` (Rust) bounds per-document
resource use. Defaults are conservative; raise them if you ingest large
documents and trust the source.

```python
limits = udoc.Limits(
    max_file_size=1_000_000_000,        # 1 GB
    max_page_count=50_000,
    max_decompressed_size=2_000_000_000,
)
cfg = udoc.Config(limits=limits)
```

For batch workers that ingest thousands of documents in one process,
`Config.memory_budget` provides an opt-in soft RSS cap. When the budget
is crossed between documents, per-document caches are reset.

## Errors

Python errors are typed exceptions:

```python
import udoc

try:
    doc = udoc.extract("missing.pdf")
except udoc.IOError:
    ...
except udoc.PasswordRequired:
    ...
except udoc.ExtractionError as e:
    print(e.code, e.message)
```

Each exception carries a stable `code` so agents can match without
parsing the message. The full code list is in the
[CLI reference](cli.md#exit-codes); library and CLI use the same codes.

<details>
<summary>Rust: <code>udoc::Error</code></summary>

`udoc::Error` carries a context chain. The top-level message describes
what failed; chained context describes what it was doing.

```text
Error: parsing object at offset 12345
  caused by: reading token
  caused by: I/O error: unexpected end of file
```

`Error::code()` returns the same stable error code Python sees; agents
match on that without parsing prose.
</details>

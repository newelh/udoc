# Architecture

A short tour of how udoc is put together. You do not need this to use
the library; read it when you want to understand a behaviour, contribute
a recommendation, or build something on top.

## Design tenets

1. **Lenient parsing beats strict failure.** Real documents lie. We
   try the spec, recover when it doesn't hold, and warn instead of
   abort. A partial extraction with known caveats beats an exception.

2. **Tiered APIs over false certainty.** Where layout analysis is
   hard, expose a cascade and let the caller pick the tier they
   trust. PDF text has `text()` / `text_lines()` / `raw_spans()`;
   reading order runs four tiers internally. We don't hide failure
   behind one method.

3. **Diagnostics are a feature.** Recoverable issues flow through
   `DiagnosticsSink` as typed warnings. Pipelines filter on `kind`.
   Silent recovery is its own kind of bug.

4. **Page-oriented, deferred work.** Backends operate on pages and
   defer expensive work until a caller asks for it.
   `Extractor::open(path)` doesn't interpret a content stream until
   you call `page_text(i)`.

5. **Unsafe stays isolated.** Workspace-wide `#![deny(unsafe_code)]`,
   one audited exception in `udoc_pdf::io::mmap_impl`. New unsafe
   requires a deliberate, reviewable change.

6. **Vertical ownership.** Parsers live in-tree. No subprocesses
   to system tools. When a document does something unusual, the fix
   lands in this codebase.

7. **Permissive licence.** Dual MIT / Apache-2.0.

## The 5-layer document model

Backends converge on a single `Document` shape:

```python
doc = udoc.extract("paper.pdf")

doc.content           # list[Block] — paragraphs, headings, tables, lists, images
doc.metadata          # title, author, page_count, created, ...
doc.presentation      # bounding boxes, fonts, colours (overlay)
doc.relationships     # links, footnotes, bookmarks (overlay)
doc.interactions      # form fields, comments, tracked changes (overlay)
doc.images            # shared image store referenced by Block::Image
```

**`doc.content` — content spine.** The text-bearing tree. `Block`
nodes (paragraphs, headings, lists, tables, image references, code
blocks, page breaks, sections, shapes) hold `Inline` children (text
spans, bold, italic, links, code, footnote refs, soft and hard
breaks, inline images). Each node carries a typed `node_id` into the
document's arena. Always present. This is what `udoc -t` walks for
tables, what the markdown emitter renders, and what `block.text`
returns for plain text.

**`doc.metadata` — document facts.** Title, author, subject, creator,
producer, creation and modification dates, page count, plus a
`properties` map for format-specific extended fields (Dublin Core
from OOXML / ODF cores, PDF Info dictionary entries, OOXML extended
properties from `app.xml`). Always present, even when all fields
are `None` for documents that didn't carry metadata.

**`doc.presentation` — geometry overlay.** Where things live on the
page and how they look. Bounding boxes per block, font name + size +
styling per text span, fill and stroke colours, paint paths (for PDF
rendering), page geometry (rotation, media box, crop box, page size).
Optional. Disable via `Config(presentation=False)` if you only need
text — the spine is unaffected. The PDF renderer reads from this
overlay, and downstream layout models consume the bounding boxes for
region-of-interest crops.

**`doc.relationships` — link overlay.** The connections between
content. Footnote and endnote definitions paired with their inline
references, hyperlinks (URL targets and anchor ranges), bookmark
targets, table-of-contents entries, cross-references between blocks.
Each entry references a `node_id` from the content spine and resolves
to its target. PDF link annotations, DOCX / ODF cross-refs, and
Markdown link reference definitions all flatten into this overlay.
Optional.

**`doc.interactions` — actionable overlay.** Things a viewer can act
on. Form fields (text, checkbox, radio, select, signature), comments
threaded by author, tracked changes — insertions, deletions, and
formatting revisions stamped with author and timestamp. PDF AcroForm
fields and DOCX revision marks live here. Documents without any of
these features carry an empty Interactions; the overlay is
independently optional via `Config(interactions=False)`.

**`doc.images` — shared image store.** Each `Block::Image` and
`Inline::InlineImage` carries an `ImageRef` index into `doc.images`;
the actual bitmap bytes plus metadata (width, height, MIME type,
original filter chain) live once in this `Vec`. An image referenced
N times is stored once — important for slide decks with repeated
logos and DOCX with repeated header images. The store is part of
the spine, not an overlay; if you want extraction to skip image
bytes entirely, set `Config(images=False)` and the references in
the content tree become empty placeholders.

Overlays are independently toggleable on the `Config`. Disabling one
skips the work that produces it; the spine (text, tables, image
references) is unaffected.

## The backend trait

All format-specific code lives behind one trait:

```rust
trait FormatBackend {
    type Page<'a>: PageExtractor where Self: 'a;
    fn page_count(&self) -> usize;
    fn page(&mut self, index: usize) -> Result<Self::Page<'_>>;
    fn metadata(&self) -> &DocumentMetadata;
    // ...
}

trait PageExtractor {
    fn text(&mut self) -> Result<String>;
    fn text_lines(&mut self) -> Result<Vec<TextLine>>;
    fn raw_spans(&mut self) -> Result<Vec<TextSpan>>;
    fn tables(&mut self) -> Result<Vec<Table>>;
    fn images(&mut self) -> Result<Vec<PageImage>>;
}
```

Each backend (`udoc-pdf`, `udoc-docx`, `udoc-xlsx`, ...) implements
these. The facade dispatches to the right backend based on format
detection, calls into the trait, and converts the result into the
unified `Document` model.

The macro `define_internal_backend!` wires a backend into the facade
in about seven lines per format; the conversion from format-specific
types to the core `Document` lives in each backend's `convert.rs`.

## The crates

| Crate              | Role                                                                       |
|--------------------|----------------------------------------------------------------------------|
| `udoc`             | The facade. Public API, CLI binary, format detection, conversion glue.     |
| `udoc-py`          | Python bindings (PyO3) over the same engine.                               |
| `udoc-core`        | Format-agnostic types: `Document`, `Block`, `Inline`, `NodeId`, `TextSpan`, `Table`, `PageImage`, `Error`, `DiagnosticsSink`, `FormatBackend`, `PageExtractor`. |
| `udoc-containers`  | Shared parsers: ZIP (OOXML / ODF), namespace-aware XML, CFB / OLE2 (legacy Office), OPC (OOXML packages). |
| `udoc-pdf`         | PDF parser. Layered: `io → parse → object → font → content → text → document`. |
| `udoc-font`        | Font engine. TrueType, CFF, Type 1, hinting, cmaps, ToUnicode.             |
| `udoc-image`       | Image decoders. CCITT, JBIG2, JPEG, JPEG 2000.                             |
| `udoc-render`      | PDF page rasteriser. Auto-hinter, font cache, glyph compositor.            |
| `udoc-docx`        | DOCX backend (ZIP + XML).                                                  |
| `udoc-xlsx`        | XLSX backend. Typed cells, shared strings, number-format mini-language.    |
| `udoc-pptx`        | PPTX backend. Shape tree, slide layouts, notes slides.                     |
| `udoc-doc`         | Legacy DOC backend (CFB + FIB + piece table).                              |
| `udoc-xls`         | Legacy XLS backend (BIFF8).                                                |
| `udoc-ppt`         | Legacy PPT backend (CFB + PowerPoint records + PersistDirectory).          |
| `udoc-odf`         | ODF backend covering ODT / ODS / ODP from a single crate.                  |
| `udoc-rtf`         | RTF parser. Control words, groups, codepage decoding.                      |
| `udoc-markdown`    | Markdown parser. CommonMark + GFM tables.                                  |

## Backend internals

Each backend has its own internal layering. The format guides cover
the per-backend architecture: see [PDF](formats/pdf.md#layers-within-udoc-pdf),
[DOCX](formats/docx.md#layers-within-udoc-docx),
[XLSX](formats/xlsx.md#layers-within-udoc-xlsx),
[PPTX](formats/pptx.md#layers-within-udoc-pptx),
[DOC](formats/doc.md#layers-within-udoc-doc),
[XLS](formats/xls.md#layers-within-udoc-xls),
[PPT](formats/ppt.md#layers-within-udoc-ppt),
[ODF](formats/odf.md#layers-within-udoc-odf),
[RTF](formats/rtf.md#layers-within-udoc-rtf), and
[Markdown](formats/markdown.md#layers-within-udoc-markdown).

## Performance Notes

- **Zero-copy lexer.** Tokens borrow from the input buffer; only the
  object parser converts to owned `PdfObject` types. Most objects are
  never dereferenced, so the lexer is the hot path and the parser is
  the cold path. `crates/udoc-pdf/src/parse/lexer.rs`.
- **Hash-DoS-resistant maps on attacker-controlled keys.** `ahash` on
  the PDF object resolver, font cmap tables, ToUnicode lookups, and
  ZIP central directory. SipHash by default in std is slow; `ahash`
  is fast and DoS-resistant. The audit-and-swap cut wall time roughly
  15% on a 200-document Archive.org sample.
- **Per-page (font, glyph) decode cache.** 256-entry approximate-LRU
  in `crates/udoc-pdf/src/content/decode_cache.rs`. Caches decoded
  Unicode strings by `(font_obj_ref, packed_code)`. Per-page rather
  than per-document, so multi-thousand-page reports do not balloon
  the cache. Covers Latin + CJK paragraphs at ~8 KB per page.
- **Pre-sized hot-path vectors.** Glyph count is known up front from
  the code length; `Vec::with_capacity(byte_count / code_len)` for
  bbox and advance vectors avoids incremental realloc per span.
- **Per-page move-semantics.** The raw-span → `PositionedSpan` emit
  loop consumes spans by value rather than cloning. Dropped 8 clones
  per span (text, font name, char advances, etc.) on a typical page.
- **Stream filter buffer pool.** Thread-local buffer pool in
  `udoc_pdf::object::stream` reduces large allocations the kernel
  sees, sidestepping a `mm_struct` rwsem contention point at high
  thread counts.
- **Memory budget.** `Config::memory_budget` is a soft per-process
  RSS cap that triggers between-document cache resets. Use it when
  ingesting 10K+ documents in one process to bound peak heap.
- **Reading-order tier 1 fast path.** If the content stream is
  already laid out in reading order (the case for most LaTeX, Word,
  and InDesign output), the geometric reordering pipeline is
  skipped. Coherence is detected with a Y-monotonicity check inside
  detected column regions; threshold 0.75. See [PDF format
  guide](formats/pdf.md#reading-order).

## Diagnostics

Recoverable issues during extraction are not exceptions. They flow
through a `DiagnosticsSink` trait that callers can attach. The default
sink drops warnings; the CLI's default sink prints them on stderr;
batch workers typically attach a `CollectingDiagnostics` and aggregate
results.

A warning carries:

- a structured `kind` (an enum, not a string).
- a level (`Warning` or `Info`).
- a context (`page_index`, `obj_ref`).
- an optional byte offset.
- a human message.

Common kinds you will see in the wild:

| `kind`                    | When                                                         |
|---------------------------|--------------------------------------------------------------|
| `StreamLengthMismatch`    | PDF `/Length` is wrong; recovered by scanning for `endstream`. |
| `ToUnicodeMissing`        | Font has no ToUnicode CMap; encoding-table fallback used.    |
| `MalformedXref`           | An xref entry was malformed; skipped, parse continued.       |
| `UnsupportedFilter`       | Stream uses a filter we do not implement; stream dropped.    |
| `DocFastSaveFallback`     | Word 95 fast-save piece-table fragment; text may be empty.   |
| `TierSelection`           | Reading-order tier picked for this page (Info, not Warning). |
| `HookTimeout`             | A hook exceeded its per-request timeout; page un-augmented.  |

Agents and CI pipelines filter on `kind`. Humans read the message.

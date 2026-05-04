# Rust library

The udoc workspace publishes a set of crates that share one
document model. The `udoc` crate is the facade: it dispatches to
the right backend based on format detection and emits the
unified [`Document`](document-model.md). The per-format backends
are also independently usable when you want a typed handle that
does not pay the conversion cost into the unified model.

## Status

udoc is not on crates.io for the alpha period. To use the Rust
API today, depend by git path:

```toml
[dependencies]
udoc = { git = "https://github.com/newelh/udoc", tag = "v0.1.0-alpha.1" }
```

The Rust API is alpha; expect to bump frequently. Per-crate
publishing to crates.io lands at beta, once the public API has
stabilised across at least one external integration.

## Facade surface

```rust
use udoc;

let doc = udoc::extract("paper.pdf")?;
println!("{:?}", doc.metadata.title);
for block in &doc.content {
    println!("{}", block.text());
}
# Ok::<(), udoc::Error>(())
```

The Rust facade mirrors the Python shape:

| Python                       | Rust                                                  |
|------------------------------|-------------------------------------------------------|
| `udoc.extract(path)`         | `udoc::extract(path)` -> `Result<Document>`           |
| `udoc.extract_bytes(b)`      | `udoc::extract_bytes(&bytes)` -> `Result<Document>`   |
| `udoc.stream(path)`          | `udoc::Extractor::open(path)` -> `Result<Extractor>`  |
| `udoc.Config(...)`           | `udoc::Config::new()` (builder)                       |
| `udoc.extract(p, on_warning=)` | `udoc::extract_with(path, cfg.diagnostics(sink))`   |

`Document` is `udoc_core::document::Document`. Iteration is by
direct field access (`doc.content`, `doc.metadata`, `doc.images`)
— the Python wrapper hides this behind iterator methods so it can
materialise spine and overlays lazily.

## Per-backend access

Each format backend ships as a separate crate. Reach for them
when you want format-specific structure or want to skip the
conversion step into `Document`.

| Crate              | Role                                                                       |
|--------------------|----------------------------------------------------------------------------|
| `udoc`             | The facade. Public API, format detection, conversion glue, CLI binary.     |
| `udoc-py`          | Python bindings (PyO3) over the same engine.                               |
| `udoc-core`        | Format-agnostic types: `Document`, `Block`, `Inline`, `NodeId`, `TextSpan`, `Table`, `PageImage`, `Error`, `DiagnosticsSink`, `FormatBackend`, `PageExtractor`. |
| `udoc-containers`  | Shared parsers: ZIP (OOXML / ODF), namespace-aware XML, CFB / OLE2 (legacy Office), OPC packages. |
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

The trait that backends implement:

```rust
trait FormatBackend {
    type Page<'a>: PageExtractor where Self: 'a;
    fn page_count(&self) -> usize;
    fn page(&mut self, index: usize) -> Result<Self::Page<'_>>;
    fn metadata(&self) -> &DocumentMetadata;
}

trait PageExtractor {
    fn text(&mut self) -> Result<String>;
    fn text_lines(&mut self) -> Result<Vec<TextLine>>;
    fn raw_spans(&mut self) -> Result<Vec<TextSpan>>;
    fn tables(&mut self) -> Result<Vec<Table>>;
    fn images(&mut self) -> Result<Vec<PageImage>>;
}
```

Per-backend extensions live behind each crate's own types. PDF's
extra surface includes raw object access:

```rust
let mut doc = udoc_pdf::Document::open("paper.pdf")?;
let mut page = doc.page(0)?;
for span in page.raw_spans()? {
    println!("({:.1}, {:.1}) {}", span.x, span.y, span.text);
}
# Ok::<(), udoc_pdf::Error>(())
```

The PDF backend's internal types (`PdfObject`, `PdfDictionary`,
`PdfStream`, `Lexer`, `ObjectResolver`) are public when you need
parser-level access.

## Configuration

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

Named presets:

```rust
Config::default()    // interactive defaults
Config::agent()      // collects diagnostics, keeps overlays on
Config::batch()      // disables expensive overlays, raises limits
Config::ocr()        // OCR-friendly render profile + scan detection
```

## Diagnostics

Recoverable issues flow through `DiagnosticsSink`. The default
sink drops; the CLI's default sink prints to stderr; batch
workers typically attach a `CollectingDiagnostics` and aggregate:

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

## Errors

`udoc::Error` carries a context chain. The top-level message
describes what failed; chained context describes what it was
doing.

```text
Error: parsing object at offset 12345
  caused by: reading token
  caused by: I/O error: unexpected end of file
```

`Error::code()` returns a stable code matching the [CLI exit
codes](../cli.md#exit-codes); agents and pipelines match on the
code rather than parsing prose.

## Generating rustdoc

```bash
cargo doc --workspace --no-deps --open
```

When udoc lands on crates.io, the per-crate docs will live at
`docs.rs/udoc` and the equivalent paths for each backend.

## See also

- [Architecture](../architecture.md) — design tenets, the crate
  layout, performance notes.
- [Document Model](document-model.md) — strict surface for
  `Document`, `Block`, `Inline`, overlays.
- [Hook protocol](hooks.md) — wire format for external hook
  subprocesses.
- [Compiling from source](../compiling.md) — building wheels and
  the CLI binary from a workspace checkout.

# udoc

The unified facade and CLI for the udoc document extraction toolkit.
Auto-detects format, dispatches to the right backend, and converts
results into the unified `Document` model. Ships the `udoc` command-line
binary.

## What

If you want one crate that handles every supported format, this is it.
`udoc::extract(path)` is the simple, opinionated entry point.

Supported formats: PDF, DOCX, XLSX, PPTX, DOC, XLS, PPT, ODT, ODS, ODP,
RTF, Markdown.

## Install

```bash
pip install udoc
```

`pip install udoc` ships both the `udoc` CLI binary and the `udoc`
Python module. They share one engine.

For the alpha period, distribution is via PyPI only. Per-crate
publishing to crates.io lands at beta. To use the Rust API today,
depend on the workspace by git path:

```toml
[dependencies]
udoc = { git = "https://github.com/newelh/udoc", tag = "v0.1.0-alpha.1" }
```

See [Compiling from source](https://newelh.github.io/udoc/compiling)
for the full build walkthrough.

## Examples

```rust
// One-shot extraction; format detected from magic bytes.
let doc = udoc::extract("paper.pdf")?;
println!("{}", doc.metadata.title.as_deref().unwrap_or("(untitled)"));

// Streaming page-by-page access for large documents.
let mut ext = udoc::Extractor::open("paper.pdf")?;
for i in 0..ext.page_count() {
    println!("page {i}: {}", ext.page_text(i)?);
}

// In-memory bytes with full configuration.
use udoc::{Config, Format};
let cfg = Config::new()
    .format(Format::Pdf)
    .password("secret")
    .pages("1,3,5-10")?;
let doc = udoc::extract_bytes_with(&std::fs::read("encrypted.pdf")?, cfg)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

## CLI

See [`udoc --help`](https://newelh.github.io/udoc/cli.html) for the
full reference. Common idioms:

```bash
udoc paper.pdf                  # text to stdout
udoc -j paper.pdf               # full document as JSON
udoc -J paper.pdf               # streaming JSONL
udoc -t spreadsheet.xlsx         # tables only as TSV
udoc -p 1-5 paper.pdf           # page range
udoc render paper.pdf -o ./out  # rasterise PDF pages
cat paper.pdf | udoc -          # read from stdin
```

## Hooks (OCR, layout, LLM integration)

External programs can plug in at three phases (OCR, layout, annotate)
via a JSONL subprocess protocol. See the
[hooks chapter](https://newelh.github.io/udoc/hooks.html) for the
protocol, recipes, and worked examples.

```bash
udoc --ocr "tesseract-hook" scanned.pdf
```

## Escape hatches

The facade is the right answer for ~95% of uses. When you need format-
specific surface (raw PDF spans, typed XLSX cells, etc.), reach for
the per-format crate directly: `udoc-pdf`, `udoc-docx`, `udoc-xlsx`,
`udoc-pptx`, `udoc-doc`, `udoc-xls`, `udoc-ppt`, `udoc-odf`,
`udoc-rtf`, `udoc-markdown`. Each is independently usable.

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or
[Apache-2.0](../../LICENSE-APACHE) at your option. Part of the
[udoc workspace](../../README.md).

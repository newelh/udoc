# udoc

A document extraction toolkit for PDF, DOCX, DOC, XLSX, XLS, PPTX,
PPT, ODT, ODS, ODP, RTF, and Markdown. udoc emits text, tables,
JSON, or rendered pages. A CLI and Python module wrap a pure Rust
binary. No external parsers, libraries, or system packages are required.
Provides [hooks](docs/hooks.md) for OCR, layout detection, and
entity extraction. Permissively licensed as dual MIT / Apache-2.0.

---

Try it out using [uv](https://docs.astral.sh/uv/), no install required:

```bash
curl -sL https://arxiv.org/pdf/1706.03762 | uvx udoc - | grep -A 18 '^Abstract'
```

## Install

```bash
pip install udoc
```

`uv pip install udoc` works the same way. This puts the `udoc`
command-line tool and the `udoc` Python module on your system;
both call one Rust binary. To build from source, see
[Compiling from source](docs/compiling.md).

## Highlights

- **One `Document` model across formats.** A content spine of `Block` and `Inline` nodes, plus optional [presentation, relationships, and interactions overlays](docs/reference/document-model.md). Disable any overlay via `Config`.
- **Legacy binary Office.** Native parsers for `.doc`, `.xls`, and `.ppt`. Per-format details in the [format guides](docs/formats/).
- **Streaming page-by-page.** The `Extractor` defers per-page work. A 10 GB PDF does not have to fit in memory.
- **Typed diagnostics.** Recoverable issues become structured warnings filterable by `kind`. Examples: font fallbacks, malformed `xref`, stream-length mismatches.
- **Hooks for OCR, layout, and annotation.** [JSONL protocol](docs/hooks.md) for Tesseract, cloud OCR APIs, DocLayout-YOLO, GLM-OCR, vision-language models, NER, or any subprocess that reads JSON line-by-line.
- **LLM tool use.** [Agent instructions](docs/agents.md) — a paste-into-context page describing udoc's CLI to assistants.

## CLI

```bash
udoc paper.pdf                     # text to stdout
udoc -j paper.pdf                  # full document as JSON
udoc -J paper.pdf                  # streaming JSONL (one record per page)
udoc -t spreadsheet.xlsx           # tables only as TSV
udoc -p 1-5,10 paper.pdf           # page range
udoc render paper.pdf -o ./pages   # rasterise PDF pages to PNG
cat paper.pdf | udoc -             # read from stdin
```

A few real-world piping recipes:

```bash
curl -sL https://arxiv.org/pdf/1706.03762 | udoc - | head -40
udoc paper.pdf | grep -i 'attention'
udoc -J docs/*.pdf | jq '.metadata.title'
```

Plain text on stdout. Structured output on flags. Stderr is
silent unless you pass `-v`. The full flag list lives in the
[CLI reference](docs/cli.md).

## Python

```python
import udoc

# One-shot extraction. Format detected from magic bytes.
doc = udoc.extract("paper.pdf")
print(doc.metadata.title)
for block in doc.blocks():
    print(block.text)

# Stream page by page; large documents do not have to fit in memory.
with udoc.stream("large.pdf") as ext:
    for i in range(len(ext)):
        print(f"page {i}: {ext.page_text(i)[:80]}")

# In-memory bytes with options.
with open("encrypted.pdf", "rb") as f:
    doc = udoc.extract_bytes(f.read(), password="secret")
```

PDF table detection and reading order are heuristic. Born-digital
documents with clean ruling and standard column flow extract
cleanly out of the box; the [PDF format
guide](docs/formats/pdf.md) covers the failure modes and when to
attach a [layout-detection or OCR hook](docs/formats/pdf.md#triggering-ocr).

The [Guide](docs/library.md) walks through configuration,
overlays, diagnostics, chunking, and batch processing. The
[Python Library reference](docs/reference/python.md) lists every
function, class, and exception.

## Rust

```rust
let doc = udoc::extract("paper.pdf")?;
println!("{:?}", doc.metadata.title);
for block in &doc.content {
    println!("{}", block.text());
}
# Ok::<(), udoc::Error>(())
```

The Rust facade mirrors the Python shape. `Document` is
`udoc_core::document::Document`; iteration is by direct field
access (`doc.content`, `doc.metadata`, `doc.images`). The
[Rust Library reference](docs/reference/rust.md) covers the
facade, the per-format backends, configuration presets,
diagnostics, and the trait that backends implement.

The full hosted manual lives at <https://newelh.github.io/udoc>.

## Status

This is the initial alpha release of udoc. APIs and outputs are subject
to change. Bugs, ergonomic suggestions, and format quirks are welcome on
the issue tracker.

## Security

Report vulnerabilities through
[GitHub Security Advisories](https://github.com/newelh/udoc/security/advisories/new)
(preferred) or, if that is not workable for you, email me@newel.dev.

See [SECURITY.md](SECURITY.md) for the disclosure process and
[docs/security.md](docs/security.md) for the unsafe-code policy and
audit.

## Contributing

Issues are welcome. Pull requests are not currently accepted on this
repository — udoc is solo-maintained during the alpha period. File an
issue describing the change you would like to see; if it is a good fit
it will land in a future release. The full policy is in
[CONTRIBUTING.md](CONTRIBUTING.md).

## Licence

Dual-licensed under either of:

- Apache License, Version 2.0
  ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT License
  ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

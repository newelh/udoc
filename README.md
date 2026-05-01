# udoc

A unified document extraction toolkit. `udoc paper.pdf` is to a PDF
what `cat paper.txt` is to a text file: clean text on stdout, ready
to pipe through `grep`, `less`, `wc`, or `jq`. Tables, JSON, rendered
pages, and OCR hooks come along when you reach for them. Twelve
formats are supported: PDF, Microsoft Office (including legacy `.doc` / `.xls` /
`.ppt`), OpenDocument, RTF, and Markdown.

---

Read the abstract of "Attention Is All You Need" straight from
arxiv using [uv](https://docs.astral.sh/uv/), no install required:

```bash
curl -sL https://arxiv.org/pdf/1706.03762 \
  | uvx --index-url https://newelh.github.io/udoc/simple/ udoc - \
  | grep -A 18 '^Abstract'
```

[`uvx`](https://docs.astral.sh/uv/) runs udoc in an ephemeral
environment; the rest is plain shell. The same shape works for any
document and any tool you'd reach for on a text file (`less`, `wc`,
`jq`, `sed`, `awk`); swap the URL for any PDF, DOCX, XLSX, PPTX,
ODF, RTF, or Markdown file you have at hand.

## Install

While the `udoc` name on PyPI is being secured, wheels are served
from a [PEP 503](https://peps.python.org/pep-0503/) simple index
hosted on this repo's GitHub Pages. Once `udoc` is on PyPI the
`--index-url` flag goes away.

```bash
uv pip install udoc --index-url https://newelh.github.io/udoc/simple/
```

That lays down both the `udoc` command-line tool and the `udoc`
Python module. They share one engine; the Python module is a thin
PyO3 wrapper over the same binary. Plain `pip` works too if you set
the same flag: `pip install udoc --index-url https://newelh.github.io/udoc/simple/`.

## Quick examples

### Python

```python
import udoc

# One-shot extraction. Format detected from magic bytes.
doc = udoc.extract("paper.pdf")
for block in doc.content:
    print(block.text)

# Stream page by page; large documents never need to fit in memory.
with udoc.open("large.pdf") as ext:
    for i in range(ext.page_count):
        print(f"page {i}: {ext.page_text(i)[:80]}")

# In-memory bytes with options.
with open("encrypted.pdf", "rb") as f:
    doc = udoc.extract_bytes(f.read(), password="secret")
```

### CLI

```bash
udoc paper.pdf                  # text to stdout
udoc -j paper.pdf               # full document as JSON
udoc -J paper.pdf               # streaming JSONL (one record per page)
udoc -t spreadsheet.xlsx        # tables only as TSV
udoc -p 1-5 paper.pdf           # pages 1 through 5
udoc render paper.pdf -o ./out  # rasterise PDF pages to PNG

curl -sL https://arxiv.org/pdf/1706.03762 | udoc -
udoc paper.pdf | grep -i 'attention'
udoc -J docs/*.pdf | jq '.metadata.title'
```

PDF table detection and reading order are heuristic — born-digital
documents with clean ruling and standard column layouts come through
cleanly; scans, dense unruled tables, and unusual layouts may need a
layout-detection or OCR hook. The [PDF format
guide](docs/formats/pdf.md) covers the strategies and failure modes.

## Documentation

- [Quick start](docs/quickstart.md) — five minutes from install to first extraction.
- [Compiling from source](docs/compiling.md) — for when you'd rather build the wheel yourself.
- [CLI reference](docs/cli.md) — every flag, every subcommand, piping recipes.
- [Library guide](docs/library.md) — the Python API and the document model.
- [Hooks and LLM integration](docs/hooks.md) — JSONL hook protocol with worked examples.
- [Agent instructions](docs/agents.md) — paste-into-context guide for assistants.
- [Per-format guides](docs/formats/) — quirks and escape hatches for each supported format.
- [Font engine](docs/fonts.md), [Image decoders](docs/images.md),
  [PDF rendering & OCR](docs/render.md) — the vertical primitives.
- [Architecture](docs/architecture.md) — the document model, design tenets,
  performance.
- [Security and unsafe audit](docs/security.md).

The full hosted manual lives at <https://newelh.github.io/udoc>.

## Document model

Every extracted document, regardless of format, has the same shape:

```python
doc = udoc.extract("paper.pdf")

doc.metadata          # title, author, page_count, created, ...
doc.content           # list of Block — paragraphs, headings, tables, lists, images
doc.presentation      # bounding boxes, fonts, colours (optional overlay)
doc.relationships     # footnotes, links, bookmarks (optional overlay)
doc.interactions      # form fields, comments, tracked changes (optional overlay)
doc.images            # shared image store referenced by Block::Image
```

Overlays are independently toggleable via `Config`; callers that only need
text pay nothing for geometry, styling, or relationships.

## Hooks

Hooks are external programs that participate in extraction over a JSONL
subprocess protocol. Three phases — **OCR** (text from images), **layout**
(semantic regions), and **annotate** (entity / metadata enrichment) — chain
together. Anything that can read and write JSON line by line can plug in.

```bash
udoc --ocr "tesseract-hook" scanned.pdf
```

The [hooks chapter](docs/hooks.md) has the protocol, security notes,
async/long-running patterns, and recipes for Tesseract, GLM-OCR,
DeepSeek-OCR, layout models, and entity extractors.

## What udoc does well

- **Twelve formats from one tool.** PDF, DOCX, XLSX, PPTX, DOC, XLS,
  PPT, ODT, ODS, ODP, RTF, Markdown — including the legacy 1997-era
  binary Office formats that everyone forgets are still in production
  archives.
- **Streams.** Open a 10 GB PDF, pull pages as you need them. The
  document model does not require loading everything up front.
- **Diagnostics as a feature.** Recoverable issues are reported as
  structured warnings on a `DiagnosticsSink`, not stderr noise. Filter,
  log, or fail the build on them.
- **Built for agents.** A documented JSONL hook protocol lets you wire
  OCR, layout detection, and entity extraction in front of the
  extractor. The [agent instructions](docs/agents.md) chapter is a
  paste-into-context guide for assistants using udoc as a tool.
- **Permissive licence.** Dual MIT / Apache-2.0.

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

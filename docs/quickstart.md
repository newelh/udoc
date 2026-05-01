# Quick start

Five minutes from install to first extraction. Python first.

## Install

```bash
pip install udoc --index-url https://newelh.github.io/udoc/simple/
```

The `--index-url` flag points at the PEP 503 simple index hosted on
this repo's GitHub Pages while the `udoc` name on PyPI is being
secured. Once `udoc` is on PyPI the flag drops and `pip install udoc`
will Just Work. `uv pip install` accepts the same flag.

This puts both the `udoc` command-line tool and the `udoc` Python module
on your system. They share one binary; the Python module is a thin
wrapper over the same engine.

If you would rather build from source, see
[Compiling from source](compiling.md).

## Python

```python
import udoc

# One-shot extraction. Format detected from magic bytes.
doc = udoc.extract("paper.pdf")
print(doc.metadata.title)
for block in doc.content:
    print(block.text)
```

For larger documents, stream page by page so the whole file never has
to fit in memory:

```python
with udoc.open("large.pdf") as ext:
    for i in range(ext.page_count):
        page = ext.page(i)
        print(f"page {i}: {page.text[:80]}")
```

In-memory bytes work the same way:

```python
with open("encrypted.pdf", "rb") as f:
    doc = udoc.extract_bytes(f.read(), password="secret")
```

A worked example you can paste into a REPL right now:

```python
import urllib.request
import udoc

url = "https://arxiv.org/pdf/1706.03762"   # "Attention Is All You Need"
data = urllib.request.urlopen(url).read()
doc = udoc.extract_bytes(data)

print(doc.metadata.title)
print(f"{doc.metadata.page_count} pages, {len(doc.content)} blocks")
for block in doc.content[:5]:
    print(f"  [{block.kind}] {block.text[:80]}")
```

## CLI

```bash
udoc paper.pdf                  # text to stdout
udoc -t spreadsheet.xlsx        # tables only as TSV
udoc -j paper.pdf               # full document as JSON
udoc -J paper.pdf               # streaming JSONL — one record per page
udoc -p 1-5,10 paper.pdf        # page range
udoc render paper.pdf -o ./pages   # rasterise PDF pages to PNG
cat paper.pdf | udoc -          # read from stdin

curl -sL https://arxiv.org/pdf/1706.03762 | udoc - | head -40
```

Pipe-friendly by default. Plain text on stdout, structured output on
flags, stderr is silent unless you pass `-v`.

PDF table detection and reading order are heuristic. Born-digital
documents with clean ruling and standard column flow extract cleanly
out of the box; the [PDF format guide](formats/pdf.md) covers the
strategies, failure modes, and when to attach a
[layout-detection or OCR hook](render.md) for hard cases.

## More

- [Compiling from source](compiling.md) — when you would rather build
  the wheel yourself.

## Where to next

- [CLI reference](cli.md) — every flag, every subcommand, piping recipes.
- [Library guide](library.md) — config, escape hatches, diagnostics, in
  Python and Rust.
- [Hooks and LLM integration](hooks.md) — the JSONL hook protocol.
- [Agent instructions](agents.md) — drop-in context block for assistants
  using udoc as a tool.
- [Per-format guides](formats/index.md) — quirks and escape hatches for each
  supported format.
- [Architecture](architecture.md) — design tenets, the document model,
  performance.
- [Security](security.md) — disclosure process, threat model, the unsafe
  audit.

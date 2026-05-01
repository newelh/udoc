# Examples

Runnable examples for udoc — the CLI, the Rust library, the Python
module, and the hooks system. Each is short and self-contained.

## Running

Rust examples live under this directory and the per-crate
`examples/` directories. Run them with:

```bash
cargo run --example extract_text -- path/to/file.pdf
cargo run --release --example diagnostics -- path/to/file.pdf
```

Python and shell examples are scripts; run them directly.

## Index

### Rust

| File                | What it shows                                                      |
|---------------------|--------------------------------------------------------------------|
| [`extract_text.rs`](extract_text.rs) | The simplest path: open a PDF, print its text. |
| [`extract_lines.rs`](extract_lines.rs) | Per-line extraction with positions. |
| [`diagnostics.rs`](diagnostics.rs) | Capturing structured warnings via a `DiagnosticsSink`. |

The per-crate examples under
[`crates/udoc/examples/`](../crates/udoc/examples/) cover the unified
facade:

| File                                          | What it shows                                  |
|-----------------------------------------------|------------------------------------------------|
| [`extract.rs`](../crates/udoc/examples/extract.rs)             | One-shot extraction via the facade. |
| [`extract_any.rs`](../crates/udoc/examples/extract_any.rs)     | Format auto-detection across mixed inputs. |
| [`extract_tables.rs`](../crates/udoc/examples/extract_tables.rs) | Just tables, as TSV.                |
| [`streaming.rs`](../crates/udoc/examples/streaming.rs)         | Page-by-page streaming for large documents. |
| [`hooks.rs`](../crates/udoc/examples/hooks.rs)                 | Wiring an OCR hook from Rust.       |
| [`render.rs`](../crates/udoc/examples/render.rs)               | Rasterising a PDF page to PNG.       |

### Shell

| File                          | What it shows                                  |
|-------------------------------|------------------------------------------------|
| [`cli_pipe_to_grep.sh`](cli_pipe_to_grep.sh) | One-liners showing pipe-friendly CLI use. |

### Python

| File                                       | What it shows                                  |
|--------------------------------------------|------------------------------------------------|
| [`python/basic.py`](python/basic.py)       | `import udoc` smoke usage.                     |
| [`python/streaming.py`](python/streaming.py) | Streaming a large PDF page-by-page.          |
| [`python/batch_with_llm.py`](python/batch_with_llm.py) | Walk a directory and pipe text into a generic LLM. |

### Hooks

The [`hooks/`](hooks/) subdirectory has working hook implementations
for Tesseract, GLM-OCR, DeepSeek-OCR, DocLayout-YOLO, and a couple of
NER models. See its [README](hooks/README.md) for details and
prerequisites.

# udoc-py

Python bindings for the [`udoc`](../udoc/) document extraction toolkit
via [PyO3](https://pyo3.rs/). This crate produces the wheel that
`pip install udoc` lays down on user systems.

```python
import udoc

doc = udoc.extract("report.pdf")
print(doc.metadata.title, len(doc.content), "blocks")
```

## What you get from `import udoc`

The Python module exposes the same conceptual API as the Rust crate,
adapted to Python idioms:

- `udoc.extract(path, *, format=None, password=None, **kwargs)` — one-shot
  extraction, returns a `Document`.
- `udoc.extract_bytes(bytes, **kwargs)` — same, from in-memory bytes.
- `udoc.open(path)` — context-manager streaming `Extractor`.
- `udoc.Config(...)` — typed builder for advanced options.
- `udoc.Document`, `udoc.Block`, `udoc.Inline`, `udoc.TextSpan`,
  `udoc.Table`, etc. — the typed model objects.
- `udoc.Format` — enum of supported formats.
- Typed exceptions: `udoc.IOError`, `udoc.ExtractionError`,
  `udoc.PasswordRequired`, etc.

The CLI (`udoc` on `$PATH`) and the Python module ship from the same
wheel. They are one binary.

## Install

For end users:

```bash
pip install udoc
```

For development (build from source against a local Rust workspace):

```bash
python3 -m venv .venv && source .venv/bin/activate
pip install maturin
maturin develop --release
python -c "import udoc; print(udoc.__version__)"
```

`maturin develop` compiles the Rust code in this crate into a `.so` that
Python can import. The wheel build for distribution is run via
`cibuildwheel` from the workspace root.

## Streaming

```python
with udoc.open("large.pdf") as ext:
    for i in range(ext.page_count):
        page = ext.page(i)
        print(page.text[:80])
```

`udoc.open` defers per-page work; large documents do not have to fit in
memory.

## Configuration

```python
cfg = udoc.Config(
    format=udoc.Format.PDF,
    password="secret",
    pages="1,3,5-10",
    presentation=False,    # skip the geometry/font overlay
    relationships=False,   # skip footnotes/links overlay
    interactions=False,    # skip form-fields overlay
)
doc = udoc.extract("encrypted.pdf", config=cfg)
```

## Diagnostics

```python
warnings = []
doc = udoc.extract("weird.pdf", diagnostics=warnings.append)
for w in warnings:
    print(w.level, w.kind, w.message)
```

## More

- Full library guide: <https://newelh.github.io/udoc/library.html>
- Hooks and LLM integration: <https://newelh.github.io/udoc/hooks.html>
- Format-specific notes: <https://newelh.github.io/udoc/formats/>

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or
[Apache-2.0](../../LICENSE-APACHE) at your option. Part of the
[udoc workspace](../../README.md).

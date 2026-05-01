# udoc-markdown

Markdown backend for the `udoc` toolkit.

## What

Parses Markdown documents (CommonMark plus the GFM subset: tables,
strikethrough, autolinks, fenced code blocks) and emits text, tables, and
structure using `udoc-core`'s format-agnostic types.

Public API: [`MdDocument`], the [`MdBlock`] / [`MdInline`] AST types, and
the [`parse_inlines`] / [`parse_inlines_with_warnings`] helpers.

## Why

Including Markdown in the format-coverage table means consumers can pipe
mixed corpora (Markdown notes, PDFs, DOCX manuals) through one extraction
API and get back the same `Document` model. The parser is small, lenient,
and re-uses the shared `Document` builder rather than emitting Markdown's
own AST as the public type.

## Status

This crate is part of the [udoc workspace](../../README.md). For the
alpha period distribution is via PyPI only (`pip install udoc`);
per-crate publishing to crates.io lands at beta. To use the Rust API
today, depend on the workspace by git path or build from source —
see [Compiling from source](https://newelh.github.io/udoc/compiling).

## One Example

```rust,no_run
use udoc_markdown::MdDocument;
use udoc_core::backend::{FormatBackend, PageExtractor};

let mut doc = MdDocument::open("README.md")?;
let mut page = doc.page(0)?;
println!("{}", page.text()?);
# Ok::<(), udoc_core::error::Error>(())
```

See [docs.rs/udoc-markdown](https://docs.rs/udoc-markdown) for the AST and
GFM-extension coverage.

---

This crate is part of the [udoc workspace](../../README.md). See the
[workspace README](../../README.md) for the full library overview.

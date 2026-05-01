# udoc-doc

Legacy DOC (Word 97-2003) backend for the [`udoc`](../udoc/) toolkit.
Reads `.doc` binary files natively — no LibreOffice subprocess, no
Office install required.

## What you get

- Document body text via the piece table.
- Tables.
- Headers, footers, footnotes, endnotes.
- Document metadata (title, author, created/modified).
- Style information (Heading 1-9 maps to the unified `Block::Heading`).

## Why this exists

A surprising amount of "modern" enterprise document storage still
contains 90s-era .doc files. Rather than shelling out to a converter,
this backend parses the native binary format directly: CFB / OLE2
container, FIB header, piece table for the document body, property
tables for everything else.

## Status

This crate is part of the [udoc workspace](../../README.md). For the
alpha period distribution is via PyPI only (`pip install udoc`);
per-crate publishing to crates.io lands at beta. To use the Rust API
today, depend on the workspace by git path or build from source —
see [Compiling from source](https://newelh.github.io/udoc/compiling).

## Example

```rust,no_run
use udoc_doc::DocDocument;
use udoc_core::backend::{FormatBackend, PageExtractor};

let mut doc = DocDocument::open("legacy.doc")?;
for i in 0..doc.page_count() {
    let mut page = doc.page(i)?;
    println!("{}", page.text()?);
}
# Ok::<(), udoc_doc::Error>(())
```

## Edge cases

- **Fast-save fragments.** Word 95-era fast-save can leave the body as
  fragmented piece-table entries. udoc emits a `DocFastSaveFallback`
  diagnostic when it hits this; consider routing the document through
  OCR or another extractor if your downstream pipeline depends on
  reliable text.

## See also

- For modern `.docx`, see [`udoc-docx`](../udoc-docx/).

## More

- Format notes: <https://newelh.github.io/udoc/formats/doc.html>

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or
[Apache-2.0](../../LICENSE-APACHE) at your option. Part of the
[udoc workspace](../../README.md).

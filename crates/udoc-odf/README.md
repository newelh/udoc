# udoc-odf

OpenDocument backend for the [`udoc`](../udoc/) toolkit. Handles ODT
(text), ODS (spreadsheet), and ODP (presentation) — one crate, three
formats. No LibreOffice install required.

## What you get

- ODT: paragraphs, tables, lists, headers/footers, footnotes,
  hyperlinks, embedded images, document metadata.
- ODS: typed cells, formulas, merged ranges, hyperlinks, multi-sheet
  workbooks.
- ODP: per-slide text frames, tables, speaker notes, embedded images.

## Status

This crate is part of the [udoc workspace](../../README.md). For the
alpha period distribution is via PyPI only (`pip install udoc`);
per-crate publishing to crates.io lands at beta. To use the Rust API
today, depend on the workspace by git path or build from source —
see [Compiling from source](https://newelh.github.io/udoc/compiling).

## Example

```rust,no_run
use udoc_odf::OdfDocument;
use udoc_core::backend::{FormatBackend, PageExtractor};

let mut doc = OdfDocument::open("notes.odt")?;
for i in 0..doc.page_count() {
    let mut page = doc.page(i)?;
    println!("{}", page.text()?);
}
# Ok::<(), udoc_odf::Error>(())
```

## Notes

- ODF documents declare their type via the `mimetype` archive entry
  (which by spec is the first STORED entry in the ZIP).
- Encrypted ODF documents are not supported.

## More

- Format notes: <https://newelh.github.io/udoc/formats/odf.html>

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or
[Apache-2.0](../../LICENSE-APACHE) at your option. Part of the
[udoc workspace](../../README.md).

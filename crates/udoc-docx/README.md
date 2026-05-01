# udoc-docx

DOCX backend for the [`udoc`](../udoc/) toolkit. Reads modern
Microsoft Word files (`.docx`) by walking the OOXML package directly.
No Office install required, no native libraries.

## What you get

- Paragraphs with style information (Heading 1-9, lists, code, quote).
- Tables, including merged cells and nested tables.
- Headers and footers.
- Footnotes, endnotes, and comments via the relationships overlay.
- Numbered and bulleted lists with rendered list-marker text.
- Embedded images.
- Tracked changes via the interactions overlay.
- Document metadata.

## Status

This crate is part of the [udoc workspace](../../README.md). For the
alpha period distribution is via PyPI only (`pip install udoc`);
per-crate publishing to crates.io lands at beta. To use the Rust API
today, depend on the workspace by git path or build from source —
see [Compiling from source](https://newelh.github.io/udoc/compiling).

## Example

```rust,no_run
use udoc_docx::DocxDocument;
use udoc_core::backend::{FormatBackend, PageExtractor};

let mut doc = DocxDocument::open("memo.docx")?;
for i in 0..doc.page_count() {
    let mut page = doc.page(i)?;
    println!("{}", page.text()?);
}
# Ok::<(), udoc_core::error::Error>(())
```

## See also

- For legacy `.doc` (Word 97-2003), see [`udoc-doc`](../udoc-doc/).

## More

- Format notes: <https://newelh.github.io/udoc/formats/docx.html>

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or
[Apache-2.0](../../LICENSE-APACHE) at your option. Part of the
[udoc workspace](../../README.md).

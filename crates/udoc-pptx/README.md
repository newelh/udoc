# udoc-pptx

PPTX backend for the [`udoc`](../udoc/) toolkit. Reads modern Microsoft
PowerPoint files (`.pptx`) by walking the OOXML package and the slide
shape tree. No Office install required.

## What you get

- One "page" per slide.
- Text from text boxes and placeholders, with formatted runs (bold,
  italic, hyperlinks).
- Tables with merged cells.
- Speaker notes as separate content blocks.
- Embedded images.
- Slide layout and master metadata in the presentation overlay.

## Status

This crate is part of the [udoc workspace](../../README.md). For the
alpha period distribution is via PyPI only (`pip install udoc`);
per-crate publishing to crates.io lands at beta. To use the Rust API
today, depend on the workspace by git path or build from source —
see [Compiling from source](https://newelh.github.io/udoc/compiling).

## Example

```rust,no_run
use udoc_pptx::PptxDocument;
use udoc_core::backend::{FormatBackend, PageExtractor};

let mut doc = PptxDocument::open("deck.pptx")?;
for i in 0..doc.page_count() {
    let mut page = doc.page(i)?;
    println!("{}", page.text()?);
}
# Ok::<(), udoc_core::error::Error>(())
```

## See also

- For legacy `.ppt` (PowerPoint 97-2003), see [`udoc-ppt`](../udoc-ppt/).

## More

- Format notes: <https://newelh.github.io/udoc/formats/pptx.html>

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or
[Apache-2.0](../../LICENSE-APACHE) at your option. Part of the
[udoc workspace](../../README.md).

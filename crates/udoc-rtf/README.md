# udoc-rtf

RTF (Rich Text Format) backend for the [`udoc`](../udoc/) toolkit.
Parses control words, groups, and codepage-encoded text natively.

## What you get

- Paragraph text with formatting (bold, italic, underline, font, size,
  colour).
- Tables (`\trowd...\row` groups).
- Lists.
- Embedded images (`\pict` groups).
- Document metadata via the `\info` group.
- Codepage-aware character decoding (`\ansicpg`, `\u`, `\'XX`), with
  20+ CJK codepages supported via the shared
  `udoc_core::codepage::CodepageDecoder`.

## Status

This crate is part of the [udoc workspace](../../README.md). For the
alpha period distribution is via PyPI only (`pip install udoc`);
per-crate publishing to crates.io lands at beta. To use the Rust API
today, depend on the workspace by git path or build from source —
see [Compiling from source](https://newelh.github.io/udoc/compiling).

## Example

```rust,no_run
use udoc_rtf::RtfDocument;
use udoc_core::backend::{FormatBackend, PageExtractor};

let mut doc = RtfDocument::open("notes.rtf")?;
for i in 0..doc.page_count() {
    let mut page = doc.page(i)?;
    println!("{}", page.text()?);
}
# Ok::<(), udoc_core::error::Error>(())
```

## Notes

- udoc honours the document's declared `\ansicpg` codepage when
  decoding 8-bit byte escapes. Mixed-codepage documents are decoded
  with the codepage in effect at each text run.
- Drawing-object data (`\shp`) is preserved but not rasterised.

## More

- Format notes: <https://newelh.github.io/udoc/formats/rtf.html>

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or
[Apache-2.0](../../LICENSE-APACHE) at your option. Part of the
[udoc workspace](../../README.md).

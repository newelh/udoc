# udoc-ppt

Legacy PPT (PowerPoint 97-2003) backend for the [`udoc`](../udoc/)
toolkit. Reads `.ppt` binary files natively. No Office install required.

## What you get

- One "page" per slide.
- Text from text boxes and placeholders.
- Tables (where structurally typed).
- Speaker notes.
- Slide-level metadata.

## Status

This crate is part of the [udoc workspace](../../README.md). For the
alpha period distribution is via PyPI only (`pip install udoc`);
per-crate publishing to crates.io lands at beta. To use the Rust API
today, depend on the workspace by git path or build from source —
see [Compiling from source](https://newelh.github.io/udoc/compiling).

## Example

```rust,no_run
use udoc_ppt::PptDocument;
use udoc_core::backend::{FormatBackend, PageExtractor};

let mut doc = PptDocument::open("legacy.ppt")?;
for i in 0..doc.page_count() {
    let mut page = doc.page(i)?;
    println!("{}", page.text()?);
}
# Ok::<(), udoc_ppt::Error>(())
```

## Notes

- The PowerPoint binary format is byzantine. Some very old presentations
  (PowerPoint 4.0 era) are not fully decodable; the backend warns and
  extracts what it can.

## See also

- For modern `.pptx`, see [`udoc-pptx`](../udoc-pptx/).

## More

- Format notes: <https://newelh.github.io/udoc/formats/ppt.html>

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or
[Apache-2.0](../../LICENSE-APACHE) at your option. Part of the
[udoc workspace](../../README.md).

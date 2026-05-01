# udoc-pdf

PDF backend for the [`udoc`](../udoc/) toolkit: parser, content
interpreter, text reading-order reconstruction, and table detection.
Standalone — usable on its own when you know the input is PDF.

## What

A robust, leniency-first PDF reader. Real PDFs lie about everything
(stream lengths, xref offsets, ToUnicode maps, encodings). `udoc-pdf`
warns on the diagnostics sink and recovers, rather than failing the
parse.

Architecture is layered and the layers do not skip:

```
io -> parse -> object -> font -> content -> text -> document
```

Tiered text API:

- `Page::text()` — full reading-order reconstruction.
- `Page::text_lines()` — positioned lines with baseline metadata.
- `Page::raw_spans()` — content-stream order, no ordering applied; the
  escape hatch when you want to do your own layout analysis.

## Status

This crate is part of the [udoc workspace](../../README.md). For the
alpha period distribution is via PyPI only (`pip install udoc`);
per-crate publishing to crates.io lands at beta. To use the Rust API
today, depend on the workspace by git path or build from source —
see [Compiling from source](https://newelh.github.io/udoc/compiling).

## Example

```rust,no_run
let mut doc = udoc_pdf::Document::open("report.pdf")?;
for i in 0..doc.page_count() {
    let mut page = doc.page(i)?;
    println!("{}", page.text()?);
}
# Ok::<(), udoc_pdf::Error>(())
```

## Capabilities

- Text extraction with three tiers (full reconstruction, positioned
  lines, raw spans).
- Table detection via ruled-line and text-alignment heuristics.
- Image extraction with original filter chain preserved (CCITT, JBIG2,
  JPEG, JPEG 2000, Flate).
- Encryption: standard security handler at revision 4 (AES-128) and
  revision 6 (AES-256).
- Font handling: ToUnicode resolution, encoding-table fallback, Adobe
  Glyph List fallback, Unicode replacement as a last resort.
- Diagnostics: structured warnings for malformed input that we
  recovered from.
- Streaming: `Document::page(i)` defers per-page work.

## Robustness

- Stream-length recovery (when `/Length` lies, scan for `endstream`).
- Malformed-xref tolerance (skip bad entries, parse the rest).
- Cycle detection in object resolution.
- Depth-limited graphics-state stack (256 levels) to bound recursion.
- HashDoS-resistant hashing on every map keyed by attacker-controlled
  values (object numbers, font cmap entries, ToUnicode lookups).

The lexer and object parser are continuously fuzzed.

## See also

- [udoc-font](../udoc-font/) — font program parsing and rasterisation.
- [udoc-image](../udoc-image/) — image decoders shared across backends.
- [udoc-render](../udoc-render/) — PDF page rasteriser. Not part of
  `udoc-pdf` so that consumers who only need text-and-tables do not
  link the rasteriser.

## More

- PDF format notes: <https://newelh.github.io/udoc/formats/pdf.html>
- Architecture: <https://newelh.github.io/udoc/architecture.html>

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or
[Apache-2.0](../../LICENSE-APACHE) at your option. Part of the
[udoc workspace](../../README.md).

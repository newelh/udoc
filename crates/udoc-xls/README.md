# udoc-xls

Legacy XLS (Excel 97-2003 BIFF8) backend for the [`udoc`](../udoc/)
toolkit. Reads `.xls` binary files natively — BIFF8 record stream
inside a CFB container. No Office install required.

## What you get

- Each sheet as a logical "page".
- Typed cells (number, date, boolean, error).
- Shared strings, formulas, named ranges.
- Workbook and per-sheet metadata.
- Multi-sheet workbooks.

## Status

This crate is part of the [udoc workspace](../../README.md). For the
alpha period distribution is via PyPI only (`pip install udoc`);
per-crate publishing to crates.io lands at beta. To use the Rust API
today, depend on the workspace by git path or build from source —
see [Compiling from source](https://newelh.github.io/udoc/compiling).

## Example

```rust,no_run
use udoc_xls::XlsDocument;
use udoc_core::backend::{FormatBackend, PageExtractor};

let mut doc = XlsDocument::open("legacy.xls")?;
for i in 0..doc.page_count() {
    let mut page = doc.page(i)?;
    println!("{}", page.text()?);
}
# Ok::<(), udoc_core::error::Error>(())
```

## Notes

- Encrypted workbooks (RC4 / Office Excel password protection) are
  not supported and fail with a structured error.
- BIFF8 (Excel 97-2003) is the supported revision; older BIFF5 (Excel
  5.0 / 95) workbooks are not handled.

## See also

- For modern `.xlsx`, see [`udoc-xlsx`](../udoc-xlsx/).

## More

- Format notes: <https://newelh.github.io/udoc/formats/xls.html>

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or
[Apache-2.0](../../LICENSE-APACHE) at your option. Part of the
[udoc workspace](../../README.md).

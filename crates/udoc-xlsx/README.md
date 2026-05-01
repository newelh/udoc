# udoc-xlsx

XLSX backend for the [`udoc`](../udoc/) toolkit. Reads modern Microsoft
Excel files (`.xlsx`) by walking the OOXML package directly. No Office
install required.

## What you get

- Each sheet as a logical "page".
- Typed cells (number, date, datetime, time, boolean, error, string).
- Shared strings resolved at parse time.
- Cell formulas as text (formula source, not re-evaluated).
- Merged-cell ranges.
- Hyperlinks via the relationships overlay.
- Named ranges and defined names.
- Workbook and per-sheet metadata.

## Status

This crate is part of the [udoc workspace](../../README.md). For the
alpha period distribution is via PyPI only (`pip install udoc`);
per-crate publishing to crates.io lands at beta. To use the Rust API
today, depend on the workspace by git path or build from source —
see [Compiling from source](https://newelh.github.io/udoc/compiling).

## Example

```rust,no_run
use udoc_xlsx::XlsxDocument;
use udoc_core::backend::{FormatBackend, PageExtractor};

let mut doc = XlsxDocument::open("report.xlsx")?;
for i in 0..doc.page_count() {
    let mut page = doc.page(i)?;
    println!("{}", page.text()?);
}
# Ok::<(), udoc_core::error::Error>(())
```

## Notes

- Date parsing honours the workbook's 1900 / 1904 calendar.
- Empty trailing rows and columns are not emitted; the "used range" is
  the rightmost / bottommost non-empty cell.

## See also

- For legacy `.xls` (Excel 97-2003 BIFF8), see [`udoc-xls`](../udoc-xls/).

## More

- Format notes: <https://newelh.github.io/udoc/formats/xlsx.html>

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or
[Apache-2.0](../../LICENSE-APACHE) at your option. Part of the
[udoc workspace](../../README.md).

# XLSX

The modern Microsoft Excel format: an Office Open XML package. udoc
reads every sheet, decodes shared strings, types numeric cells
(currency, date, percent, number, boolean, error), and exposes
formulas where present. No Office install required.

## Why this format is interesting

A spreadsheet is the format where "what is the value of this cell" has
a surprisingly subtle answer. XLSX cells store *raw values*, not
display strings. A cell that shows `$1,234.50 USD` to a Word user is
stored as `1234.5` plus a *number format code* like `"$"#,##0.00 "USD"`,
applied at render time. A cell that shows `2024-03-15` is stored as
`45366` (the day-count from the workbook's epoch) with a date-style
number format. A cell that shows `12.5%` is stored as `0.125` with a
percent format.

Two consequences:

1. udoc does the format application during extraction so callers get
   both the raw typed value AND the display string, and never have to
   reimplement Excel's number-format mini-language.
2. *Date detection requires inspecting both the value and the format*.
   A bare numeric cell with no date format is just a number; the same
   numeric value with `[$-409]m/d/yyyy;@` becomes a date. Cells that
   look like dates in the rendered Excel view are not dates in the XML
   — the format is the date, not the value.

## What you get

- Each sheet as a logical "page" — `udoc -J workbook.xlsx` emits one
  JSONL record per sheet.
- Typed cells. udoc emits `Number`, `Date`, `DateTime`, `Time`,
  `Boolean`, `Error`, and `String` variants, with both the raw value
  and the formatted display string available.
- Shared strings resolved at parse time so cell text is ready to use.
  XLSX stores repeated string values once in `sharedStrings.xml` and
  references them by index from each cell.
- Cell formulas as text (formula source, not re-evaluated). The
  cached result is also available.
- Merged-cell ranges, with the merge anchor identified.
- Hyperlinks via the relationships overlay.
- Named ranges and defined names (workbook + sheet scope).
- Workbook and per-sheet metadata.

## What you do not get

- Formula evaluation. udoc reads the formula text and the cached
  result; it does not re-evaluate. If you need live values, run the
  workbook through a calculation engine first. (This is a design
  decision — udoc does not execute spreadsheet logic.)
- Charts, pivot tables, sparklines, conditional formatting visuals.
  The metadata exists but visual reconstruction is not currently
  supported.
- Page rendering. Rendering for non-PDF formats is not currently
  supported.
- VBA macros. Security decision — udoc does not execute embedded
  scripts.
- External data connections (`xl/connections.xml`).

If you need any of the items marked "not currently supported", please
[open a feature request](https://github.com/newelh/udoc/issues).

## Format nuances worth knowing

### 1900 vs 1904 calendar

Excel for Mac historically used January 2, 1904 as day zero; Excel for
Windows uses January 0, 1900 (a fictitious date — Excel treats 1900
as a leap year, perpetuating a Lotus 1-2-3 bug). The workbook declares
which calendar applies via `<workbookPr date1904="1"/>`. udoc honours
the declaration; date values come out correctly regardless.

If you see dates that are off by ~4 years in your output, the most
likely cause is a tool that copied raw cell values between a 1900-
calendar and a 1904-calendar workbook without converting the day
number — that is a producer bug, not an extraction bug. udoc will
faithfully report what the workbook declares.

### Number formats are a mini-language

Excel's number format syntax (`#,##0.00`, `[$-409]m/d/yyyy`,
`[Red]-#,##0.00;-`, etc.) has positive / negative / zero / text
sub-format compartments separated by `;`, locale tags in `[$-LCID]`,
date / time codes, repetition runs, and conditional comparisons. udoc
ships a parser for the subset you actually see in the wild and falls
back to the raw value's default `Display` for formats it does not
understand, with a `NumberFormatUnsupported` warning.

### Empty cells are not stored

XLSX only stores cells that have a value. Empty rows and columns
between populated cells just do not exist in the XML. This means the
"used range" of a sheet is the bounding box of populated cells, not
`A1:end-of-grid`. udoc reports rows and columns up to the rightmost
and bottommost populated cell; pure-empty trailing rows are dropped.
If you need to preserve the apparent "shape" of a sparse sheet,
inspect `sheet.dimensions` (when set) instead.

### Shared strings are an optimisation, not a normalisation

Two cells with the same text *may* point at the same shared-string
index, or *may* each have inline `<is>` text. udoc resolves both
forms transparently; you get the string regardless. The tradeoff
the format makes is parse-time deduplication for memory savings on
large workbooks; udoc preserves the deduplication during extraction
so a 50K-row workbook does not balloon the heap.

## Layers within udoc-xlsx

```
udoc-containers  ZIP + OPC relationships + XML reader
shared_strings   sharedStrings.xml SST table reader
workbook         workbook.xml + sheet directory + 1900/1904 calendar flag
styles           styles.xml + cell XF records + number-format codes
formats          number-format mini-language parser + applier
sheet            per-sheet cell-stream parser (BLANK / NUMBER / FORMULA / ...)
cell_ref         A1 <-> (row, col) coordinate conversion
merge            merged-range reconstruction with anchor identification
convert          XLSX nodes -> unified Document model
document         public API (XlsxDocument, Sheet iteration)
```

The shared-string table parses up front and is reused across all
sheets. Style + format parsing also runs once at workbook open;
per-cell extraction looks up a small XF index rather than re-parsing
format codes. The split between `styles` (parsing the XML) and
`formats` (applying a format code to a value) lets the format
mini-language be reused by other backends if needed.

## Failure modes

- **Documents larger than 256 MB.** Rejected by default; raise via
  `Config::limits`.
- **Encrypted workbooks.** Not supported; fail with a structured
  `PasswordRequired` error.
- **Number formats outside the supported subset.** udoc emits the
  raw value and warns; the cell's `formatted` field falls back to
  `value.to_string()`.
- **Out-of-range shared-string references.** Treated as empty
  string; warning emitted.
- **Pivot caches.** Not extracted. The source data range is
  reachable via the cells themselves.

## Diagnostics

| `kind`                       | When                                                           |
|------------------------------|----------------------------------------------------------------|
| `SharedStringOutOfRange`     | A cell references a sst index past the end of the table.       |
| `NumberFormatUnsupported`    | A cell's number format uses constructs udoc does not parse.    |
| `MergedRangeOverflow`        | A merge spec extends past the sheet's used range.              |
| `DateBeforeEpoch`            | A date value is < 0 in the declared calendar.                  |

## Escape hatches

```rust,no_run
use udoc_xlsx::XlsxDocument;
use udoc_core::backend::{FormatBackend, PageExtractor};

let bytes = std::fs::read("workbook.xlsx")?;
let doc = XlsxDocument::from_bytes(&bytes)?;

for sheet in doc.sheets() {
    println!("=== {} ===", sheet.name);
    for row in sheet.rows() {
        for cell in row.cells() {
            // Both the typed value and the formatted display string.
            print!("{}\t", cell.formatted());
        }
        println!();
    }
}
# Ok::<(), udoc_core::error::Error>(())
```

`XlsxDocument` exposes the typed `Cell` enum directly so callers can
distinguish a number-that-displays-as-currency from a string. The
`Document` model collapses everything to text in the table cell.

## See also

- For legacy `.xls` (Excel 97-2003 BIFF8), see [`xls.md`](xls.md).
- XLSX shares its container plumbing with [DOCX](docx.md) and
  [PPTX](pptx.md) via `udoc-containers`.

# Legacy XLS (Excel 97-2003)

The pre-XLSX Microsoft Excel binary format. BIFF8 record stream
inside a CFB container. Still encountered in financial archives,
older bank statements, and documents from systems that have not
upgraded their export pipelines in two decades. udoc reads them
natively, without an Office install or LibreOffice subprocess.

## Why this format is interesting

XLS is BIFF8 (Binary Interchange File Format, version 8): a stream of
typed records, each with a 2-byte type code and a 2-byte length,
strung together inside a "Workbook" stream of the parent CFB
container. The record types are documented in Microsoft's
`[MS-XLS]` reference and there are well over a hundred of them.

Two structural quirks make BIFF parsing genuinely awkward:

1. **CONTINUE chains.** A single logical record can exceed the
   per-record byte budget (~8K for some records, varies). When it
   does, the producer splits the payload across the original record
   plus one or more `CONTINUE` records following it. The parser must
   stitch the fragments back together before interpreting the
   combined payload — and the boundaries are not always at convenient
   semantic points (a Unicode string can be split mid-character).
2. **Variable-length records.** Most records do not have a fixed
   layout: the byte interpretation depends on flag bits earlier in
   the same record. A `LABELSST` cell record contains a row, column,
   format index, and shared-string index (compact, twelve bytes).
   A generic `LABEL` record contains a row, column, format index,
   length, encoding flag, and length-bytes-of-text, with the
   length-encoded-or-unicode treatment driven by the flag.

Like its `.doc` sibling, XLS is essentially a tiny on-disk database
of typed records, and parsing means reading the records in order
while maintaining state about the current sheet, the active
codepage, and the running shared-string table.

## What you get

- Each sheet as a logical "page".
- Typed cells: `BLANK`, `NUMBER`, `RK` (compressed number), `MULRK`
  (run of compressed numbers), `BOOLERR`, `LABEL` / `LABELSST`
  (string-table reference), `FORMULA` with cached result.
- Shared strings via the SST records.
- Formulas as text (parsed from RPN to readable form) and the cached
  result.
- Named ranges and defined names.
- Workbook and per-sheet metadata.
- Multi-sheet workbooks.

## What you do not get

- VBA macros (`_VBA_PROJECT_CUR` stream skipped). Security decision
  — udoc does not execute embedded scripts.
- Charts. Chart records in BIFF8 are voluminous; visual
  reconstruction is not currently supported.
- BIFF5 (Excel 5.0 / Excel 95) or earlier. Only BIFF8 (Excel
  97-2003) is supported; older BIFF revisions are not currently
  supported.
- Encrypted workbooks (RC4 password protection, XOR obfuscation).
  Not currently supported.

If you need any of the items marked "not currently supported", please
[open a feature request](https://github.com/newelh/udoc/issues).

## Format nuances worth knowing

### CodePage governs string interpretation

A `CODEPAGE` record near the start of the workbook stream declares
the 8-bit codepage used for legacy `LABEL` records that pre-date the
Unicode shift. udoc honours the declaration via the shared
`CodepageDecoder` and falls back to CP-1252 (Windows Latin-1) if no
`CODEPAGE` record fires. Mixed-codepage workbooks (rare) are handled
by re-evaluating the encoding at each `LABEL` record.

### RK encoding

`RK` records pack a 32-bit integer or scaled fixed-point number into
the cell payload, saving 4 bytes versus a full IEEE 754 double. The
two low bits encode whether the value is integer or fixed-point and
whether to divide by 100. udoc decodes this transparently; you get a
regular `Cell::Number`.

### MULRK records

A run of consecutive RK-encoded cells in the same row collapses into
a single `MULRK` record carrying the row, the starting column, and
the array of RK values. udoc expands these into individual cells
during extraction so the row/column structure is preserved.

### Sheet streams are linked by BOF/EOF pairs

Each sheet's records live between a `BOF` (Begin of File) and `EOF`
record pair within the Workbook stream. The workbook-global records
(SST, codepage, defined names, sheet directory) live in the outer
stream, before the first sheet's `BOF`. The parser tracks state
transitions to know which records belong to which scope.

### Date detection requires the format index

Like XLSX, an XLS date is a number plus a format that interprets it
as a date. udoc inspects each cell's format-index reference into the
`XF` records to determine whether the cell renders as a date, and
emits the typed `Cell::Date` / `Cell::DateTime` accordingly.

The same 1900 vs 1904 calendar setting from XLSX applies. The flag
sits in a `WINDOW1` or `WORKBOOK` record.

## Layers within udoc-xls

```
udoc-containers  CFB / OLE2 reader
records          BIFF8 record-stream parser + CONTINUE chain reassembly
sst              Shared-string table reader
formats          Number-format records + format-code mini-language
workbook         Workbook directory (sheets, defined names, codepage, calendar flag)
cells            Per-record cell parsers (BLANK / NUMBER / RK / MULRK / LABEL / LABELSST / FORMULA / BOOLERR)
convert          XLS nodes -> unified Document model
document         public API
```

`records` is the boundary between bytes and typed records and
handles CONTINUE-chain stitching transparently for higher layers.
`cells` is where BIFF8's variable-length record interpretations
land — the same record type can have different field layouts based
on flag bits earlier in the same record. RK and MULRK decoding
live here too.

## Failure modes

- **Encrypted workbooks.** Not supported. RC4-encrypted XLS files
  fail with a structured `PasswordRequired` error.
- **BIFF5 (Excel 5/95).** Detected via the BIFF version in the
  `BOF` record; udoc returns a structured "unsupported BIFF
  version" error rather than producing garbage.
- **CONTINUE chain corruption.** A `CONTINUE` record claims more
  data than fits before the next record header; udoc warns and
  truncates the chain at the boundary.
- **Codepage mismatch.** A `LABEL` record's bytes do not decode
  cleanly under the declared codepage; udoc emits replacement
  characters (U+FFFD) and warns.

## Diagnostics

| `kind`                  | When                                                            |
|-------------------------|-----------------------------------------------------------------|
| `BiffVersionUnsupported`| BIFF5 or earlier detected; extraction aborted with clear error. |
| `ContinueChainTruncated`| A CONTINUE chain ran past the next record header.               |
| `CodepageDecodeFailed`  | A LABEL record could not decode under the declared codepage.    |
| `RkRecordOutOfRange`    | Compressed-number flags are inconsistent.                       |
| `EncryptionDetected`    | Workbook stream is encrypted; extraction blocked.               |

## Escape hatches

```rust,no_run
use udoc_xls::XlsDocument;
use udoc_core::backend::{FormatBackend, PageExtractor};

let bytes = std::fs::read("legacy.xls")?;
let doc = XlsDocument::from_bytes(&bytes)?;

for sheet in doc.sheets() {
    println!("=== {} ===", sheet.name);
    for (r, row) in sheet.rows().enumerate() {
        for (c, cell) in row.cells().enumerate() {
            print!("({},{}) {}\t", r, c, cell.formatted());
        }
        println!();
    }
}
# Ok::<(), udoc_core::error::Error>(())
```

## See also

- For modern `.xlsx`, see [`xlsx.md`](xlsx.md).
- XLS shares its CFB container plumbing with [DOC](doc.md) and
  [PPT](ppt.md).

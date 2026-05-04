# Legacy DOC (Word 97-2003)

The pre-DOCX Microsoft Word binary. Despite being officially superseded
in 2007, `.doc` files are still in heavy circulation: government records
archives, enterprise document management systems, decades of email
attachments. udoc parses them natively without LibreOffice, antiword,
or any other system Office install.

## Why this format is interesting

`.doc` is structurally a tiny in-process database, not a flat file. The
file is a CFB (Compound File Binary) container — basically a FAT
filesystem laid out inside one file. Word stores its document text in a
"WordDocument" stream, properties in a "1Table" or "0Table" stream,
embedded objects in their own streams, and so on.

Inside the WordDocument stream the text is *not* stored as one
contiguous block. It's stored as a *piece table*: a list of
`(file-offset, length, encoding)` tuples that, when concatenated,
yield the document body. The piece table itself lives in one of the
property tables and is reconstructed on every open. This is how Word
implemented fast undo and partial-save in the 1990s — appending an
edit only required adding a new piece-table entry pointing into freshly
written bytes, without rewriting the document. It is also where most
of the parser difficulty comes from.

The master index for everything is the **File Information Block (FIB)**
at the start of the WordDocument stream. The FIB tells the parser where
to find every other structure: the piece table, the styles table, the
formatting properties, the lists, the bookmarks, the document
properties. Get the FIB wrong and the rest of the parse is lost.

## What you get

- Document body text via piece-table reconstruction.
- Tables.
- Headers, footers, footnotes, endnotes.
- Document metadata (title, author, created/modified) from the
  `\005SummaryInformation` property stream.
- Style information. The Word 6+ heading styles (`Heading 1` through
  `Heading 9`) map to the unified `Block::Heading` levels.

## What you do not get

- Tracked changes from very old documents (Word 95 and earlier) that
  predate Word's modern revision-tracking format.
- Embedded images. The CFB streams are walked and embedded-object
  references are surfaced, but bitmap extraction from the legacy
  in-document format is not currently supported.
- Page rendering. udoc does not currently render `.doc` files.
- VBA macros (`_VBA_PROJECT_CUR` stream skipped). This is a security
  decision — udoc does not execute embedded scripts.
- Mail-merge data sources. Extracting data sources from a merge
  document is a different tool's job.

If you need any of the items marked "not currently supported", please
[open a feature request](https://github.com/newelh/udoc/issues).

## Edge cases worth knowing

### Fast-save fallback

The headline issue. Word 95 introduced a "fast save" mode that, instead
of rewriting the document on save, *appends* edits to the end of the
file and updates the piece table to point at the new bytes. Successive
fast-saves leave the document body as fragmented pieces scattered
throughout the file, with the piece table as the only canonical source
of truth.

The problem: some Word 95-era documents have empty or corrupted
piece tables. The text is somewhere in the file, but the index that
tells the parser how to read it is missing. udoc detects this case
(piece-table CLX empty or all-zero), falls back to a heuristic scan of
the WordDocument stream, and emits a `DocFastSaveFallback` warning so
downstream code knows the extracted text may be incomplete.

If you see `DocFastSaveFallback` on the diagnostics sink and the
extracted text looks wrong, the original byte layout has been lost
and udoc cannot reconstruct it from inside the format. udoc does not
currently render `.doc` files. If you need that path, please [open a
feature request](https://github.com/newelh/udoc/issues).

### Mixed codepages within one document

Word 97 introduced Unicode support (UCS-2) but kept the legacy 8-bit
codepage path for backward compatibility. A single document can carry
text fragments in multiple encodings (e.g. half in CP-1252, half in
CP-932 / Shift-JIS), with the encoding picked per fragment. udoc
honours the per-fragment encoding metadata via the shared
[`CodepageDecoder`](../fonts.md) and falls back to the document's
declared default codepage when a fragment has no explicit annotation.

### Property-stream variations

The summary metadata lives in OLE2 property streams whose layout has
changed across Word versions. udoc reads
`\005SummaryInformation` and `\005DocumentSummaryInformation` and
falls back gracefully when fields are absent — but a Word 6.0 document
saved in Word 2003 may have property layouts that udoc cannot fully
parse; metadata fields come through as `None`.

## Layers within udoc-doc

```
udoc-containers  CFB / OLE2 reader (FAT chains, mini-stream, root directory)
fib              File Information Block parser (master pointer table)
piece_table      PieceTable reconstruction + fast-save fallback heuristic
properties       \005SummaryInformation / \005DocumentSummaryInformation streams
font_table       FFN structures (font names referenced by character runs)
text             body / footnote / endnote / header / footer extraction
tables           table-cell reconstruction across piece-table boundaries
convert          DOC nodes -> unified Document model
document         public API
```

The CFB reader is shared with XLS and PPT via `udoc-containers`;
only the post-CFB Word-specific parsing lives in this crate.
`fib` runs first because every other layer needs offsets it
provides. `piece_table` is the most-tested component — fast-save
fallback, encoding-per-fragment detection, and boundary handling
across pieces are where Word 95-era files get interesting.

## Failure modes

- **Encrypted documents.** RC4-encrypted `.doc` files are not
  supported and fail with a structured `PasswordRequired` error.
- **Corrupted CFB.** The compound-file directory itself can be
  corrupt; udoc returns a structured error rather than producing
  garbage.
- **Word 4.0 / Word 6.0.** These pre-Word-97 binaries use a different
  format altogether (Word 6 binary, not BIFF/CFB). They are not
  supported.

## Diagnostics

| `kind`                  | When                                                   |
|-------------------------|--------------------------------------------------------|
| `DocFastSaveFallback`   | Empty / corrupt piece table; heuristic scan used.      |
| `DocPropertyMissing`    | A summary-info field referenced by FIB is unreadable.  |
| `EncodingFallback`      | A text fragment had no explicit encoding annotation.   |
| `EmbeddedObjectSkipped` | An OLE-embedded object was found but not decoded.      |

## Escape hatches

```rust
let bytes = std::fs::read("legacy.doc")?;
let doc = udoc_doc::DocDocument::from_bytes(&bytes)?;
println!("title: {:?}", doc.metadata().title);
for para in doc.paragraphs() {
    println!("{}", para.text);
}
# Ok::<(), udoc_doc::Error>(())
```

## See also

- For modern `.docx`, see [`docx.md`](docx.md).
- The [font engine page](../fonts.md) covers the codepage decoder
  shared with RTF and the legacy Excel backend.

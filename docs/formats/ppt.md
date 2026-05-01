# Legacy PPT (PowerPoint 97-2003)

The pre-PPTX PowerPoint binary. CFB container holding a stream of
typed PowerPoint records. udoc reads them natively. Like its
DOC and XLS siblings, the format predates the OOXML era and shows
its age in interesting ways.

## Why this format is interesting

PPT is essentially append-only at the file level. When PowerPoint
saves a deck, it does not rewrite the existing records — it appends
new versions of the changed records to the end of the file and updates
a directory of "current pointers" so the next reader knows which
version of each record is active. The format calls this directory the
**PersistDirectory**.

This means a `.ppt` file is, in some sense, a journal: every revision
saved without a "compact" operation contains all prior versions of
every record, with the active set selected by the latest
PersistDirectory at the end of the file. Parsing PPT correctly means
finding the most recent PersistDirectory, walking its pointer entries
to locate the *current* version of each record, and ignoring the
superseded older copies — even though they remain physically present
in the file.

The records themselves are organised into a tree of **atoms**
(leaf records carrying typed payload) inside **containers**
(records that hold child records). PowerPoint records run from
straightforward (`SlideAtom`, `TextCharsAtom`) to byzantine
(`PPDrawing` containers with deeply nested DrawingML predecessor
hierarchies).

## What you get

- One "page" per slide.
- Text from text boxes and placeholders. udoc finds each
  `TextCharsAtom` / `TextBytesAtom` under the slide's text-stream
  records and pulls its payload.
- Tables (where structurally typed in the binary record stream).
- Speaker notes via the corresponding notes slide records.
- Slide-level metadata (title, layout, sequence).

## What you do not get

- Slide rendering. Rendering for non-PDF formats is not currently
  supported.
- Animations and transitions. Runtime presentation behaviours, not
  extraction concerns.
- VBA macros. Security decision — udoc does not execute embedded
  scripts.
- Embedded video or audio media. The reference is surfaced; media
  decoding is not currently supported.
- DrawingML reconstruction beyond text and table boundaries is not
  currently supported.

If you need any of the items marked "not currently supported", please
[open a feature request](https://github.com/newelh/udoc/issues).

## Format nuances worth knowing

### PersistDirectory walking

Finding the current version of a record is not just "read the file
top to bottom". udoc starts by locating the
**UserEditAtom** at the end of the document stream — the last
UserEditAtom is the "head" of the active document state. From there
it walks backwards through the chain of prior UserEditAtoms until it
has the full PersistDirectory, then resolves each persist ID to the
current record offset.

If the PersistDirectory chain is corrupt — UserEditAtom pointing at
itself, persist IDs out of range, the chain not terminating — udoc
emits a warning and falls back to a forward scan that picks the *last*
occurrence of each record type. The text usually comes through; the
slide order may not.

### Records can be revised without removal

A `.ppt` file edited many times without a "compact" save can be
several times the size of its visible content, because every prior
revision of every changed record is still in the file. The active
records are the ones the PersistDirectory points at; the rest are
storage overhead. udoc reads only the active set.

### TextCharsAtom vs TextBytesAtom

Text in PPT can be stored as 16-bit Unicode (`TextCharsAtom`) or as
8-bit codepage-encoded bytes (`TextBytesAtom`). Both forms appear in
real documents — sometimes in the same slide. udoc honours the
distinction and decodes both transparently.

### Atom record numbering changed across versions

PowerPoint 97, 2000, 2002, and 2003 each added record types and
occasionally repurposed existing ones. The differences are documented
in Microsoft's `[MS-PPT]` reference. udoc supports the records
present in production-grade `.ppt` files; PowerPoint 4.0-era binaries
(pre-PPT-97) are not supported.

## Layers within udoc-ppt

```
udoc-containers  CFB / OLE2 reader
records          PowerPoint record-stream parser (atoms + containers)
persist          PersistDirectory walker — "what's the current version of each record?"
slides           Slide assembly from the active record set
styles           Text-style / paragraph-style record interpretation
convert          PPT nodes -> unified Document model
document         public API
```

`persist` is the layer that makes this format different from XLS
or DOC. PowerPoint's append-only save model means the file may
contain superseded versions of every record; finding the "current"
revision means walking the UserEditAtom chain at the file end and
resolving persist IDs to offsets. Higher layers see only the active
records, which simplifies the slide-assembly logic considerably.

## Failure modes

- **PowerPoint 4.0 / earlier.** Detected and rejected with a clear
  error.
- **Corrupt PersistDirectory.** Warning + forward-scan fallback
  (recovers text, may miss slide ordering).
- **Encrypted files.** Not supported.
- **DrawingML predecessor groups deeper than supported.** The
  enclosing slide still extracts; deeply-nested drawings emit a
  warning and have their text-bearing children extracted at the
  outer level.

## Diagnostics

| `kind`                       | When                                                       |
|------------------------------|------------------------------------------------------------|
| `PersistDirectoryCorrupt`    | UserEditAtom chain is broken; forward-scan fallback used. |
| `RecordVersionUnsupported`   | A record version is too old for udoc to interpret.         |
| `TextEncodingFallback`       | TextBytesAtom codepage decode produced replacement chars.  |
| `DrawingDepthLimit`          | DrawingML group recursion hit the configured maximum.      |

## Escape hatches

```rust,no_run
use udoc_ppt::PptDocument;
use udoc_core::backend::{FormatBackend, PageExtractor};

let bytes = std::fs::read("legacy.ppt")?;
let doc = PptDocument::from_bytes(&bytes)?;
for slide in doc.slides() {
    println!("=== slide {} ===", slide.index);
    println!("{}", slide.text());
}
# Ok::<(), udoc_ppt::Error>(())
```

## See also

- For modern `.pptx`, see [`pptx.md`](pptx.md).
- PPT shares its CFB container plumbing with [DOC](doc.md) and
  [XLS](xls.md).

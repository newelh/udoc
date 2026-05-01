# OpenDocument (ODT, ODS, ODP)

LibreOffice / OpenOffice's native formats — the ISO-standardised
counterpart to Office Open XML. udoc handles all three OpenDocument
subformats (text, spreadsheet, presentation) in one backend crate
because they share the same package layout.

## Why this format is interesting

ODF is structurally cleaner than OOXML in most respects, but it has
its own conventions worth knowing about. An ODF document is a ZIP
container with:

- `mimetype` — a single ASCII line declaring the type
  (`application/vnd.oasis.opendocument.text` for ODT,
  `…spreadsheet` for ODS, `…presentation` for ODP). The spec
  *requires* this entry to be the first STORED (uncompressed) entry
  in the archive, so a reader can detect the document type by reading
  the first ~50 bytes of the file. This is the cleanest format-
  detection mechanism in any of the formats udoc supports.
- `content.xml` — the document body. One file for ODT, one for the
  whole spreadsheet (all sheets) for ODS, one for the whole
  presentation (all slides) for ODP.
- `styles.xml` — style definitions, page geometry, master pages.
- `meta.xml` — Dublin Core + ODF-extended metadata.
- `manifest.xml` — the package manifest enumerating every part.
- `Pictures/` — embedded images.
- `settings.xml` — viewer settings (not extracted; not document content).

One backend serves all three subformats because the structural
walking is the same; only the body XML schema (`text:`, `table:`, or
`presentation:` namespace) differs. udoc dispatches on the mimetype
detected at open time.

## What you get

### ODT (text)

Roughly equivalent to DOCX:

- Paragraphs with style information; ODF heading styles map to the
  unified `Block::Heading`.
- Tables, lists, headers, footers.
- Footnotes, endnotes, hyperlinks.
- Embedded images.
- Document metadata.

### ODS (spreadsheet)

Roughly equivalent to XLSX:

- Each sheet as one "page".
- Typed cells (number, date, time, percent, currency, boolean,
  string).
- Formulas as text.
- Merged cells.
- Hyperlinks.
- Multi-sheet workbooks.

### ODP (presentation)

Roughly equivalent to PPTX:

- One "page" per slide.
- Text from text frames.
- Tables on slides.
- Speaker notes (in ODP, notes are inline within the slide XML —
  no separate part walk needed, in contrast to PPTX).
- Embedded images.

## What you do not get (any subformat)

- Page rendering. Rendering for non-PDF formats is not currently
  supported.
- Macros (`Basic` / Python scripts). Security decision — udoc does
  not execute embedded scripts.
- Embedded objects beyond images.
- Charts. The ODF chart subformat is structured but rendering it is
  not currently supported.
- Encrypted ODF documents (password-protected via the manifest's
  encryption metadata). Not currently supported.

If you need any of the items marked "not currently supported", please
[open a feature request](https://github.com/newelh/udoc/issues).

## Format nuances worth knowing

### mimetype detection

ODF's spec mandate that the `mimetype` archive entry be the first
STORED (uncompressed) entry of the ZIP makes detection trivial: read
the first ~50 bytes after the ZIP header, look for the mimetype
string, dispatch to the right subformat. udoc honours this fast path
and falls back to inspecting the inner XML root element when the
mimetype entry is missing or non-conformant — which happens with
malformed exports from third-party tools.

### Single body XML per document

Unlike DOCX (which can split sections across parts) or PPTX (one XML
per slide), ODF puts the entire document body in `content.xml`. ODS
spreadsheets with hundreds of sheets are *one* XML file. ODP decks
with hundreds of slides are *one* XML file. This is friendly for
top-down parsing but means very large documents are a single big
allocation; udoc honours the document's size limit accordingly.

### Style chains follow the family

Like DOCX, ODF styles inherit. ODF styles also belong to a *family*
(`paragraph`, `text`, `table`, `table-cell`, `graphic`, etc.) and a
style's "parent-style-name" must reference a style of the same family.
udoc resolves the chain at parse time and exposes the merged effective
style on the presentation overlay.

### ODP notes are inline

Where PPTX puts speaker notes in a separate `notesSlideN.xml`, ODP
embeds them inside the slide element as a `<presentation:notes>`
child. udoc walks this child during slide extraction; no extra part
fetch needed.

### Number formats: locale-aware

ODF number formats live in `<number:date-style>`,
`<number:currency-style>`, etc. — structured XML rather than
XLSX's compact mini-language. udoc parses the ones used in real
documents and applies them to typed cell values; unsupported
constructs warn and the cell renders the raw value's `Display`.

## Layers within udoc-odf

```
udoc-containers  ZIP container reader
manifest         manifest.xml parser (package contents, mimetype, encryption flags)
meta             meta.xml + Dublin Core + extended properties
styles           styles.xml resolver (family-scoped chains, page-master)
odt              text-specific body parser (text:* namespace)
ods              spreadsheet-specific body parser (table:* namespace)
odp              presentation-specific body parser (presentation:* namespace)
convert          ODF nodes -> unified Document model
document         public API; mimetype dispatch picks one of odt/ods/odp
```

The three subformats share manifest, metadata, and style resolution
but split into format-specific body parsers. mimetype-based
dispatch happens at `OdfDocument::open` time so the parser pipeline
is straight-line per document.

## Failure modes

- **Malformed mimetype entry.** udoc falls back to root-element
  detection; warns when the fallback fires.
- **Encrypted documents.** Not supported.
- **Very large `content.xml`.** Capped by the document size
  limit; raise via `Config::limits` if needed.
- **Mixed-version style references.** ODF 1.0 / 1.1 / 1.2 / 1.3
  documents are all accepted; cross-version style chain breaks
  warn and resolve to defaults.

## Diagnostics

| `kind`                       | When                                                          |
|------------------------------|---------------------------------------------------------------|
| `OdfMimetypeFallback`        | mimetype entry missing or malformed; root-element fallback.   |
| `StyleFamilyMismatch`        | A parent-style-name references a different family.            |
| `NumberFormatUnsupported`    | A `<number:...>` style uses constructs udoc does not parse.   |
| `EncryptionDetected`         | Manifest encryption metadata present; extraction blocked.     |

## Escape hatches

```rust,no_run
use udoc_odf::OdfDocument;
use udoc_core::backend::{FormatBackend, PageExtractor};

let mut doc = OdfDocument::open("notes.odt")?;
println!("kind: {:?}", doc.kind);  // Odt | Ods | Odp

for i in 0..doc.page_count() {
    let mut page = doc.page(i)?;
    println!("{}", page.text()?);
}
# Ok::<(), udoc_odf::Error>(())
```

## See also

- ODF shares its ZIP container with [DOCX](docx.md), [XLSX](xlsx.md),
  and [PPTX](pptx.md) via `udoc-containers`.

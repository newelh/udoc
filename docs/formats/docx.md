# DOCX

The modern Microsoft Word format: an Office Open XML package zipped
into a single file. udoc reads the body, headers, footers, footnotes,
endnotes, comments, and tracked changes. No Office install required;
the parser handles both Strict and Transitional OOXML namespaces.

## Why this format is interesting

DOCX looks straightforward — "it's just XML in a ZIP" — but the
format's reach across decades of Word releases shows up as accumulated
indirection. A DOCX package is a directory of related XML parts
(`word/document.xml`, `word/styles.xml`, `word/numbering.xml`,
`word/footnotes.xml`, plus per-image media files), connected by an
OPC relationships graph. Resolving anything user-visible (a heading,
a numbered list item, a hyperlink) usually means walking from the
body out through one or two indirections.

The XML itself is verbose. A single styled paragraph in Word becomes
a `<w:p>` element with a `<w:pPr>` paragraph-properties block, each
text run wrapped in `<w:r>` with its own `<w:rPr>` run-properties
block. Properties cascade: a paragraph's effective formatting is
the merge of its assigned style, that style's "based-on" parent
recursively, the document defaults, and any direct (`<w:rPr>`) overrides.
The "Strict" OOXML namespace also differs from "Transitional" in URI
strings the parser must accept.

## What you get

- Paragraphs with style information. Heading 1 through Heading 9 map
  to the unified `Block::Heading` with the matching level.
- Tables, including merged cells (`<w:gridSpan>` / `<w:vMerge>`) and
  nested tables.
- Headers and footers, surfaced as separate sections in the content
  spine.
- Footnotes, endnotes, and comments via the relationships overlay.
- Numbered and bulleted lists with rendered list-marker text — the
  numbering itself is reconstructed from `numbering.xml` + the
  `<w:numId>` references on each paragraph.
- Embedded images (referenced by relationship from `<w:drawing>`).
- Tracked changes (insertions, deletions, formatting changes) via the
  interactions overlay.
- Document metadata from `docProps/core.xml` (Dublin Core) and
  `docProps/app.xml` (extended properties).

## What you do not get

- Page rendering. DOCX has no canonical page boundary — pagination
  is computed by the renderer at viewing time and depends on font
  metrics, page size, margins, and section breaks. udoc reports a
  single "page" for the whole document. There is no
  `udoc render report.docx`; rendering for non-PDF formats is not
  currently supported.
- ActiveX controls and embedded objects beyond images. Security
  decision — udoc does not execute embedded controls.
- VBA macros (`vbaProject.bin`). Same security decision as above.
- Mail-merge data sources.
- Field codes evaluated dynamically (`{ TIME }`, `{ AUTHOR }`,
  `{ MERGEFIELD ... }`). The cached result text comes through; the
  field is not re-evaluated.

If you need any of the items marked "not currently supported", please
[open a feature request](https://github.com/newelh/udoc/issues).

## Format nuances worth knowing

### Numbering is convoluted

DOCX numbering uses a two-level indirection. Each paragraph in a list
references a `<w:numId>` (an instance), which references an
`<w:abstractNumId>` (a definition), which contains the level
definitions (`<w:lvl>`) — bullet character, indentation, restart rules,
override mappings. Computing what list marker to render on a given
paragraph means resolving the reference chain *and* tracking sequential
state across the document (numbered list 3.2.1 needs to know about
3.1.x preceding it).

udoc reconstructs numbering at parse time and exposes the rendered
marker text on each list-item block, so callers do not have to resolve
the chain themselves.

### Style inheritance

Paragraph style → its based-on parent → that parent's based-on → ...
all the way to the document defaults. udoc resolves the chain at parse
time and exposes the merged effective style on the
`PresentationOverlay`, but if you reach for raw spans you may need to
walk the chain yourself.

### Section properties at the document end

A DOCX section's properties live in `<w:sectPr>` blocks. Most are at
the *end* of the section's content, not the start. The last section's
properties are at the document end inside the body element itself.
Parsers that read top-down without buffering can miss the section
properties for sections they have already emitted.

### Tracked changes interleave with content

Insertions (`<w:ins>`) and deletions (`<w:del>`) are inline elements
in the document XML. By default udoc emits the *post-acceptance* view
(insertions become part of the text, deletions are dropped) and
records the original revision metadata in the interactions overlay so
callers can reconstruct what changed.

### Strict vs Transitional namespaces

The OOXML 1.0 spec ("Strict" / ECMA-376) used different XML namespace
URIs than the production "Transitional" variant Word actually emits.
udoc accepts both. If a document mixes namespace prefixes
inconsistently, the parser logs a warning and recovers.

## Layers within udoc-docx

```
udoc-containers  ZIP + OPC relationships + namespace-aware XML reader
parser           document.xml body walker (paragraphs, runs, tables, sections)
styles           styles.xml parser + based-on chain resolution
numbering        numbering.xml resolver + per-document list-state machine
table            table-grid + merge-anchor reconstruction
ancillary        headers, footers, footnotes, endnotes, comments
convert          DOCX nodes -> unified Document model
document         public API surface (DocxDocument, page() entry point)
```

The container plumbing is shared with XLSX and PPTX via
`udoc-containers`; only the body-walker and per-format conventions
live in this crate. The numbering resolver is the most stateful
component — list-marker rendering depends on running counters across
the document, restart rules per level, and override chains that can
reach the abstract definition.

## Failure modes

- **Documents larger than 256 MB.** Rejected by default to prevent
  unbounded ZIP-bomb-style decompression. Raise the limit via
  `Config::limits` if you trust the source.
- **Malformed numbering references.** `<w:numId>` pointing at an
  abstract that does not exist; falls back to a generic bullet.
- **Drawings referencing missing media parts.** Image is dropped
  with a warning; surrounding paragraph text extracts normally.
- **Field codes that depend on runtime state.** The cached value
  inside `<w:t>` comes through; the field is not re-evaluated.

## Diagnostics

| `kind`                  | When                                                            |
|-------------------------|-----------------------------------------------------------------|
| `NumberingResolveFailed`| `<w:numId>` does not resolve; generic bullet emitted.           |
| `StyleChainCycle`       | Based-on style chain has a cycle; broken at first repeat.       |
| `MediaPartMissing`      | Drawing references a part that is not in the package.           |
| `NamespaceMixed`        | Document mixes Strict + Transitional namespace URIs.            |
| `RelationshipMissing`   | A relationship target ID has no matching `<Relationship>`.      |

## Escape hatches

```rust,no_run
use udoc_docx::DocxDocument;
use udoc_core::backend::{FormatBackend, PageExtractor};

let mut doc = DocxDocument::open("memo.docx")?;
for i in 0..doc.page_count() {
    let mut page = doc.page(i)?;
    println!("{}", page.text()?);
}
# Ok::<(), udoc_core::error::Error>(())
```

The `udoc-docx` backend exposes the parsed `DocxDocument` directly when
you need format-specific structure access (section trees, raw run
properties, the resolved numbering tables) beyond what the
`Document` model exposes.

## See also

- For legacy `.doc` (Word 97-2003 binary), see [`doc.md`](doc.md).
- DOCX shares its container plumbing with [XLSX](xlsx.md) and
  [PPTX](pptx.md) via `udoc-containers`.

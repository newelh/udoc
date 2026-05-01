# PPTX

The modern Microsoft PowerPoint format: an Office Open XML package.
udoc reads each slide, walks the shape tree, extracts text from text
boxes and tables, and surfaces speaker notes from the linked notes
slides. No Office install required.

## Why this format is interesting

A PowerPoint slide is a *shape tree*, not a document flow. The slide
XML (`slideN.xml`) contains a `p:spTree` root holding shape elements
(text boxes, picture frames, group frames, content placeholders,
connectors), each with its own positioning, styling, and possibly
nested children for grouped shapes. Extracting text means walking the
tree, recognising which shapes carry text, and ordering the results in
a way that matches how a human would read the slide — which is
non-trivial because the visual order on a slide does not necessarily
match the XML order.

A second wrinkle: each slide layout (`slideLayoutN.xml`) inherits from
a slide master (`slideMasterN.xml`), and individual shapes on a slide
may inherit text body content and properties from a placeholder on the
layout. Extracting "everything on this slide" sometimes means resolving
through the layout to fill in placeholder text.

A third: slides have *speaker notes*, stored in a separate XML part
(`notesSlideN.xml`), linked from the slide via per-slide relationships.
Notes are conceptually slide content but live in a different file.

## What you get

- One "page" per slide. `udoc -J deck.pptx` emits one JSONL record
  per slide. Slide order comes from `p:sldIdLst` in `presentation.xml`,
  which is the order PowerPoint shows the user — not the filesystem
  order of the slide XML files inside the ZIP.
- Text from text boxes and placeholders, with formatted runs (bold,
  italic, hyperlinks).
- Tables on slides, with merged cells.
- Speaker notes as separate content blocks. udoc walks the
  per-slide relationship of type
  `notesSlide` to find each notes slide and extracts its body
  paragraphs.
- Embedded images.
- Slide layout and master metadata via the presentation overlay.
- Hyperlinks via the relationships overlay.

## What you do not get

- Slide rendering. Rendering for non-PDF formats is not currently
  supported. The visual fidelity required for a PowerPoint deck also
  involves theme resolution, master inheritance, animation state,
  and DrawingML rendering — none of which are implemented.
- Animations and transitions. These are runtime presentation
  behaviours, not extraction concerns.
- VBA macros. Security decision — udoc does not execute embedded
  scripts.
- Embedded video or audio media. The reference is surfaced; media
  decoding is not currently supported.
- Chart data. The chart XML is structured but rendering it is not
  currently supported.

If you need any of the items marked "not currently supported", please
[open a feature request](https://github.com/newelh/udoc/issues).

## Format nuances worth knowing

### Slide order is not filesystem order

The slide XML files inside a PPTX ZIP can be in *any* order. The
canonical slide sequence is the `<p:sldId>` list in
`presentation.xml`, which references each slide by relationship ID.
udoc honours this — slide 1 in the output is the slide PowerPoint
shows first to the user, regardless of which `slideN.xml` happens to
be alphabetically first in the archive.

This matters because tools that re-export PPTX (Google Slides export,
LibreOffice Save As, third-party converters) frequently leave the
slide files in a non-sequential filesystem order while keeping the
sldIdLst correct. A naive parser that walks the ZIP entries in order
gets the wrong slide sequence on these documents.

### Shape tree recursion

Group frames (`p:grpSp`) contain their own nested shape trees. Slides
with deeply nested groups are valid PPTX but adversarial inputs can
declare arbitrary nesting depth. udoc caps recursion at a configurable
depth limit (default: a safe single-digit number) and warns if the
limit is hit.

### Placeholder inheritance

A shape on a slide marked as a placeholder (`p:ph`) inherits text body
content from the matching placeholder on the slide's layout, which in
turn inherits from the slide master. udoc resolves the inheritance
chain at parse time so the extracted text reflects what PowerPoint
shows the viewer.

A common case: a shape on the slide has *empty* text content but the
matching layout placeholder has the actual text. Reading only the
slide's XML would miss it.

### Notes slides are separate XML parts

Speaker notes for slide N live at `ppt/notesSlides/notesSlideN.xml`,
linked from slide N via a per-slide relationship of type `notesSlide`.
The notes slide has its own shape tree with the notes paragraphs in a
notes-body placeholder. udoc walks the relationship, extracts the
notes-body text, and emits it as a `Block::Paragraph` group attached
to the slide.

If the relationships file for a slide is missing the `notesSlide`
entry, the notes are silently absent — there is no other path to the
notes XML. udoc warns when a notes slide is referenced but the target
part is missing.

## Layers within udoc-pptx

```
udoc-containers  ZIP + OPC relationships + XML reader
text             text-body + run-level paragraph extraction
shapes           shape tree walker (text frames, picture frames, group recursion)
table            slide-embedded table extraction
notes            per-slide notes-slide relationship walking + body extraction
convert          PPTX nodes -> unified Document model
document         public API (PptxDocument, slide-by-index access, page() trait)
```

The shape tree walker is the heart of the backend. Group frames
nest, and udoc caps recursion at a configurable depth so adversarial
inputs cannot exhaust the stack. Slide ordering is resolved by
reading `presentation.xml`'s `p:sldIdLst` and dispatching to the
correct relationship target — never by walking the ZIP entries in
filesystem order, which is non-canonical.

## Failure modes

- **Documents larger than 256 MB.** Rejected by default; raise via
  `Config::limits`.
- **Group recursion past the depth limit.** Inner groups are
  truncated and a warning fires.
- **Slide ID list out of sync with relationships.** A `p:sldId`
  references a relationship target that does not exist; that slide
  is skipped with a warning, the rest of the deck extracts normally.
- **Theme / scheme references.** Some slide content references
  theme XML (`ppt/theme/themeN.xml`) for colour and font choices.
  udoc reads themes for the presentation overlay; if theme parsing
  fails the slide text still extracts and the colour metadata
  defaults.

## Diagnostics

| `kind`                  | When                                                          |
|-------------------------|---------------------------------------------------------------|
| `SlideOrderMismatch`    | `p:sldIdLst` references a relationship target that is missing.|
| `NotesSlideMissing`     | Slide has no `notesSlide` relationship; notes silently absent.|
| `ShapeTreeDepthLimit`   | Group recursion hit the configured maximum.                   |
| `PlaceholderResolveFailed` | A placeholder reference does not match a layout placeholder.|

## Escape hatches

```rust,no_run
use udoc_pptx::PptxDocument;
use udoc_core::backend::{FormatBackend, PageExtractor};

let bytes = std::fs::read("deck.pptx")?;
let doc = PptxDocument::from_bytes(&bytes)?;

for slide in doc.slides() {
    println!("=== slide {} ===", slide.index);
    for shape in slide.shapes() {
        if let Some(text) = shape.text() {
            println!("  [{}] {}", shape.kind, text);
        }
    }
    if let Some(notes) = slide.notes() {
        println!("  --- notes: {}", notes);
    }
}
# Ok::<(), udoc_core::error::Error>(())
```

Reach for `PptxDocument` directly when you want to walk shapes by
type, inspect placeholder metadata, or get the resolved-but-unmerged
shape tree.

## See also

- For legacy `.ppt` (PowerPoint 97-2003 binary), see [`ppt.md`](ppt.md).
- PPTX shares its container plumbing with [DOCX](docx.md) and
  [XLSX](xlsx.md) via `udoc-containers`.

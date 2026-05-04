# Document model reference

The `Document` is the format-agnostic shape every backend
produces. This page is the strict reference for its fields,
variants, and overlays — the counterpart to the prose in
[Architecture / The 5-layer document model](../architecture.md#the-5-layer-document-model).

## Shape

```text
Document
├── metadata          DocumentMetadata
├── content           Vec<Block>            -- the content spine
├── presentation      Presentation          -- overlay (geometry / fonts / colour)
├── relationships     Relationships         -- overlay (links / footnotes / bookmarks / TOC)
├── interactions      Interactions          -- overlay (forms / comments / revisions)
└── images            AssetStore<ImageAsset> -- shared image bytes referenced by ImageRef
```

The **content spine** is the canonical text-bearing tree, always
present. The three **overlays** carry independently optional layers
keyed off the same `node_id`s the spine assigns. Disabling an
overlay via `Config(layers=...)` skips its work; the spine is
untouched.

The shared **image asset store** lives in `doc.images`. Every
`Block::Image` and `Inline::InlineImage` carries an `ImageRef`
index into the store; the same image referenced N times is stored
once.

Every node carries a typed `NodeId` allocated from the document's
arena. Overlay payloads (presentation, relationships,
interactions) key off these ids; consumers walk the spine and look
up overlay data per node when they need it.

## Block

A block-level content element. The Rust enum is `#[non_exhaustive]`;
new variants may be added without a major-version bump.

| Variant         | Fields                                        | Notes                                              |
|-----------------|-----------------------------------------------|----------------------------------------------------|
| `Heading`       | `id`, `level: u8`, `content: Vec<Inline>`     | `level` is the semantic heading depth (typically 1–6; clamp values outside the range). |
| `Paragraph`     | `id`, `content: Vec<Inline>`                  | The default text container.                        |
| `Table`         | `id`, `table: TableData`                      | See [Table](#table) below.                         |
| `List`          | `id`, `items: Vec<ListItem>`, `kind: ListKind`, `start: u64` | `kind` is `Ordered` or `Unordered`; `start` is the first ordinal for ordered lists. |
| `CodeBlock`     | `id`, `text: String`, `language: Option<String>` | Preformatted, language-tagged when known.       |
| `Image`         | `id`, `image_ref: ImageRef`, `alt_text: Option<String>` | Block-level image. The bytes live in `doc.images[image_ref]`. |
| `PageBreak`     | `id`                                          | A page / slide / sheet boundary.                   |
| `ThematicBreak` | `id`                                          | A horizontal rule.                                 |
| `Section`       | `id`, `role: Option<SectionRole>`, `children: Vec<Block>` | A semantic container (HTML `section`/`article`/`nav`/`aside`, PDF tagged structure, DOCX section). |
| `Shape`         | `id`, `kind: ShapeKind`, `children: Vec<Block>`, `alt_text: Option<String>` | A non-textual visual element (PPTX shape, SVG primitive). Shapes can nest and contain text. |

### `ListItem`

```text
ListItem { content: Vec<Block> }
```

A list item is a sequence of blocks (paragraphs, nested lists,
images, etc). The flatten-friendly shape is intentional: an item
can carry multi-block content without forcing list-specific node
types into the spine.

### `Block::text`

Every block exposes a recursive `text()` that walks its content
and produces a plain-text reconstruction. The convention used
across formats:

- Headings, paragraphs, code blocks: text concatenated in source
  order.
- Lists: items separated by `\n`; multi-block items separated by
  `\n` between blocks within the item.
- Tables: rows separated by `\n`; cells separated by `\t`; cell
  blocks separated by ` ` (single space).
- Sections / Shapes: child blocks separated by `\n`.
- Images / breaks: empty (image alt text is *not* included; pattern
  match on `Block::Image` to access it).

### Block discriminant strings

In Python (and JSON output), the variant is exposed as a
lowercase-snake-case discriminant string in the `kind` field:

```text
"paragraph", "heading", "list", "table", "code_block", "image",
"page_break", "thematic_break", "section", "shape"
```

## Inline

An inline content element within a block.

| Variant        | Fields                                         | Notes                                              |
|----------------|------------------------------------------------|----------------------------------------------------|
| `Text`         | `id`, `text: String`, `style: SpanStyle`       | The default inline. `style` carries semantic markup. |
| `Code`         | `id`, `text: String`                           | Inline `<code>`-equivalent.                        |
| `Link`         | `id`, `url: String`, `content: Vec<Inline>`    | Hyperlink. URL is content (it is part of what the document says). |
| `FootnoteRef`  | `id`, `label: String`                          | Marker for a footnote / endnote. The definition lives in the relationships overlay (`doc.relationships.footnotes[label]`). |
| `InlineImage`  | `id`, `image_ref: ImageRef`, `alt_text: Option<String>` | Inline image. Bytes in `doc.images[image_ref]`. |
| `SoftBreak`    | `id`                                           | Reflowable break (collapse to a space when wrapping). |
| `LineBreak`    | `id`                                           | Hard newline (preserve in plain-text output).      |

### `SpanStyle`

```text
SpanStyle {
    bold:          bool,
    italic:        bool,
    underline:     bool,
    strikethrough: bool,
    superscript:   bool,
    subscript:     bool,
}
```

These flags carry semantic weight (bold = emphasis; italic =
citation, in the prose sense). Extended visual styling (font name,
font size, colour) lives in the presentation overlay, not on
`SpanStyle`. Custom serde: only the fields that are `true`
serialise — empty `SpanStyle` becomes `{}`.

### Inline discriminant strings

```text
"text", "code", "link", "footnote_ref", "inline_image",
"soft_break", "line_break"
```

## Table

```text
TableData {
    rows:                       Vec<TableRow>,
    num_columns:                u32,
    header_row_count:           u32,
    has_header_row:             bool,
    may_continue_from_previous: bool,
    may_continue_to_next:       bool,
}

TableRow {
    cells: Vec<TableCell>,
}

TableCell {
    content:  Vec<Block>,
    col_span: u32,
    row_span: u32,
    value:    Option<String>,    -- typed string for spreadsheet cells
}

CellValue {
    String(String),
    Number(f64),
    Boolean(bool),
    Date(String),
    Empty,
    Error(String),
}
```

`num_columns` is the logical column count after merge resolution.
`header_row_count` records how many leading rows are headers
(commonly `1`; `0` for table-of-contents-style or borderless
tables; `>1` for spreadsheets with grouped headers).

`may_continue_from_previous` and `may_continue_to_next` are
hints, not promises: PDF detects continuations from page-break
adjacency and matching column shape; DOCX / OOXML use explicit
markers. False negatives are common on producers that do not
flag continuations explicitly.

`TableCell.value` is a normalised string for spreadsheet cells
(XLSX, ODS, XLS) — the underlying typed value formatted with the
backend's number-format engine. For document tables (PDF, DOCX,
RTF) it is `None` and `cell.text` is the only meaningful surface.

`TableCell.content` lets cells nest arbitrary blocks. A cell can
contain paragraphs, lists, even nested tables; spreadsheet
backends always emit a single `Paragraph` per cell, but document
backends preserve real structure where the source has it.

## Presentation overlay

The geometry / styling layer. Disabled with
`Config(layers=LayerConfig(presentation=False))`.

`Presentation` carries:

- **Bounding boxes** per block (where on the page is this).
- **Positioned spans** (PDF) — per-glyph or per-run placement
  with font reference, font size, baseline, advance.
- **Page geometry** — `PageDef` per page: rotation, media box,
  crop box, page size.
- **Extended text styling** (`ExtendedTextStyle`) — font name,
  font size, fill colour, stroke colour, alignment.
- **Layout info** (`LayoutInfo`) — block-level layout mode,
  flow direction, alignment, padding.
- **Paint paths** (`PaintPath`) — stroke / fill paths the
  renderer emits to reproduce the page.
- **Colour models** (`Color`) — RGB, CMYK, gray, ICC, plus
  alpha.
- **Patterns and shadings** (`PaintPattern`, `PaintShading`) —
  PDF tiling patterns and gradient fills (Type 1–7; Type 4–7
  function-based shadings are partially supported).
- **Soft masks and clip regions** — alpha-channel masks and
  page clipping.
- **Image placement** (`ImagePlacement`) — geometry per image
  reference.

The PDF renderer reads from this overlay; layout-detection hooks
consume bounding boxes for region-of-interest crops; downstream
viewers reconstruct page appearance from it.

The full type list with field-level documentation lives in
[`crates/udoc-core/src/document/presentation.rs`](https://github.com/newelh/udoc/blob/main/crates/udoc-core/src/document/presentation.rs).

## Relationships overlay

The link / cross-reference layer. Disabled with
`Config(layers=LayerConfig(relationships=False))`.

```text
Relationships {
    footnotes:      HashMap<String, FootnoteDef>,
    bookmarks:      HashMap<String, BookmarkTarget>,
    hyperlinks:     Vec<String>,
    captions:       SparseOverlay<NodeId>,
    toc_entries:    Vec<TocEntry>,
    component_refs: SparseOverlay<ComponentRef>,
}
```

| Type            | Notes                                                                |
|-----------------|----------------------------------------------------------------------|
| `FootnoteDef`   | `{ label, content: Vec<Block> }`. Pair with `Inline::FootnoteRef::label`. |
| `BookmarkTarget`| `Resolved(NodeId)` for bookmarks resolved to a node; `Positional` when the source marks a position between elements (DOCX). |
| `TocEntry`      | `{ level, text, target: Option<NodeId> }`. Level 1 = top-level entry. |
| `ComponentRef`  | `{ component_id, overrides }`. Reserved for component / template references. |

Per-node members (`captions`, `component_refs`) participate in
`relationships.has_node(node_id)`; document-wide members
(`footnotes`, `bookmarks`, `hyperlinks`, `toc_entries`) do not.

Resource caps (defending against pathological inputs):
`MAX_FOOTNOTES`, `MAX_BOOKMARKS`, `MAX_HYPERLINKS`, `MAX_CAPTIONS`,
`MAX_TOC_ENTRIES`, `MAX_COMPONENT_REFS` — defined in the
relationships module. Add operations that hit a cap return a
`*AddResult::LimitReached` rather than silently truncating.

## Interactions overlay

The actionable layer. Disabled with
`Config(layers=LayerConfig(interactions=False))`.

```text
Interactions {
    form_fields:     Vec<FormField>,
    comments:        Vec<Comment>,
    tracked_changes: Vec<TrackedChange>,
}

FormField {
    anchor:     Option<NodeId>,
    name:       String,
    field_type: FormFieldType,    -- Text | Checkbox | Radio | Dropdown | Signature | Button
    value:      Option<String>,
    bbox:       Option<BoundingBox>,
    page_index: Option<usize>,
}

Comment {
    anchor:     NodeId,
    author:     Option<String>,
    date:       Option<String>,
    text:       String,
    replies:    Vec<Comment>,         -- nested up to MAX_COMMENT_DEPTH (64)
    bbox:       Option<BoundingBox>,  -- for PDF annotations spanning a region
    page_index: Option<usize>,
}

TrackedChange {
    anchor:      NodeId,
    change_type: ChangeType,           -- Insertion | Deletion | FormatChange
    author:      Option<String>,
    date:        Option<String>,
    old_content: Option<Vec<Inline>>,  -- for Deletion / FormatChange
}
```

`FormField.anchor` is `None` for fields that were not associated
with a node (free-floating PDF AcroForm widgets). `bbox` and
`page_index` carry the geometric placement when the field has one.

Comment threading recurses through `replies`. Deserialisation caps
nesting at `MAX_COMMENT_DEPTH` (64) to defend against adversarial
JSON.

`TrackedChange.old_content` carries the inline content that was
removed for `Deletion` and `FormatChange`; `Insertion` leaves it
`None` (the new content lives on the spine).

## DocumentMetadata

```text
DocumentMetadata {
    title:             Option<String>,
    author:            Option<String>,
    subject:           Option<String>,
    creator:           Option<String>,
    producer:          Option<String>,
    creation_date:     Option<String>,
    modification_date: Option<String>,
    page_count:        usize,
    properties:        HashMap<String, String>,
}
```

Always present, even when every structured field is `None`. The
`properties` map is the open-ended bag for format-specific
extended fields; common keys:

- **OOXML core / ODF meta**: `dc:creator`, `dc:subject`,
  `dc:description`, `dc:language`, `dcterms:created`,
  `dcterms:modified`.
- **OOXML extended (`app.xml`)**: `app:Application`,
  `app:AppVersion`, `app:Company`, `app:Pages`, `app:Words`,
  `app:Characters`, `app:Lines`, `app:Paragraphs`.
- **PDF Info dict**: `pdf:Producer`, `pdf:Creator`, `pdf:Trapped`,
  plus any custom Info-dict entries the producer wrote.

`creation_date` and `modification_date` are ISO 8601 strings when
the source supplied a parseable date; otherwise the original raw
string from the source is preserved.

## NodeId and arena

Every node in the spine is allocated a typed `NodeId` from a
monotonically increasing arena. Overlay payloads key off these
ids. Consumers should treat ids as opaque handles — equality
and hashing are well-defined; arithmetic is not.

`SparseOverlay<T>` and `Overlay<T>` are the per-node payload
container types in `udoc_core::document::overlay`. `SparseOverlay`
is a hash-map-backed map for sparsely-populated layers; `Overlay`
is a vec-backed dense store for layers that approach one entry per
node.

`Document::walk()` is the canonical recursive traversal; pattern
matching on the `Block` variant is the right call when you want
variant-specific access (children, tables, lists).

## Images and the asset store

```text
ImageAsset {
    width:               u32,
    height:              u32,
    bits_per_component:  u8,
    filter:              ImageFilter,    -- Flate | Dct | Ccitt | Jbig2 | Jpx | Raw
    mime_type:           &'static str,   -- "image/jpeg", "image/png", ...
    data:                Vec<u8>,
}

ImageRef = AssetRef<ImageAsset>
```

`doc.images` is an `AssetStore<ImageAsset>`: a `Vec<ImageAsset>`
indexed by `ImageRef`. The same bitmap referenced from many places
in the spine (slide deck logos, repeating headers) is stored once.

Setting `Config(assets=AssetConfig(images=False))` skips bitmap
loading entirely; references in the spine become empty
placeholders rather than valid `ImageRef`s.

## Cross-references

| Spine site                                    | Overlay site                                                    |
|-----------------------------------------------|-----------------------------------------------------------------|
| `Inline::FootnoteRef { label }`               | `relationships.footnotes[label] -> FootnoteDef`                 |
| `Block::Image / Inline::InlineImage { image_ref }` | `doc.images[image_ref] -> ImageAsset`                       |
| Any `Block` / `Inline` with `id`              | `presentation` per-node bbox / extended style / layout info     |
| Any `Block` / `Inline` with `id`              | `relationships.captions[id] -> NodeId` (caption association)    |
| Any `Block` with `id`                         | `interactions.{form_fields, comments, tracked_changes}` keyed via `anchor == id` |

The convention: the spine is the source of truth for *what* the
document says; overlays carry *where it lives*, *what it links
to*, and *what a viewer can do with it*. A consumer that only
needs text reads the spine and ignores overlays; a consumer
building a viewer reads both.

## Source

Definitive types live in
[`crates/udoc-core/src/document/`](https://github.com/newelh/udoc/blob/main/crates/udoc-core/src/document):

- `mod.rs` — `Document`, `NodeId`, re-exports.
- `content.rs` — `Block`, `Inline`, `SpanStyle`, `ListItem`,
  `ListKind`, `SectionRole`, `ShapeKind`.
- `table.rs` — `TableData`, `TableRow`, `TableCell`, `CellValue`.
- `presentation.rs` — every presentation-overlay type listed
  above plus the paint, pattern, shading, mask, and colour
  primitives.
- `relationships.rs` — `Relationships`, `FootnoteDef`,
  `BookmarkTarget`, `TocEntry`, `ComponentRef`, plus the
  `*AddResult` enums and resource caps.
- `interactions.rs` — `Interactions`, `FormField`,
  `FormFieldType`, `Comment`, `TrackedChange`, `ChangeType`.
- `assets.rs` — `AssetStore`, `AssetRef`, `ImageAsset`,
  `FontAsset`, `FontProgramType`.
- `overlay.rs` — `Overlay`, `SparseOverlay`.

The Python types in [Python API reference](python.md) mirror
these one-for-one with kind-discriminant pyclasses.

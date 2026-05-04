# Documents are weird

Send people here when they hit a result they didn't expect and ask
"why doesn't this just work?" — why a heuristic is involved, why OCR
is sometimes the only path, why a "full toolkit" still ships hooks
for external models. The short answer is: documents are weird, and
no toolkit can paper over that without lying about its results.

## What's actually in the file

The thing called "a document" does not, on disk, contain what a
reader thinks it contains. Each format encodes the on-screen
artifact in a way that is convenient for the producer (a layout
engine, a word processor, a slide deck designer, a printer driver)
and inconvenient for anyone trying to recover meaning later.

**PDF is a layout description, not a document.** A PDF page is a
sequence of drawing instructions: "place this glyph at coordinate
(x, y) in this font at this size." There are no paragraphs. There
are no lines. There are no columns. There are no reading-order
guarantees — a producer is free to emit the right column before the
left, footnotes interleaved with body text, or the same glyph twice
because it sat in two overlapping clip regions. Recovering "the
text of this page in the order a human would read it" requires
reconstructing structure that the file never contained. That
reconstruction is heuristic. It works well on born-digital documents
from LaTeX, Word, or InDesign, and it degrades on producers that
cut corners — which is most of them.

**DOCX is XML, but the XML is a tag soup with overrides.** Word
encodes paragraphs, runs, and properties; properties cascade from
section to paragraph to run with overrides at every level; tables
nest inside cells inside tables; revision marks coexist with the
underlying text; deletions are still in the file unless rejected.
The structure is recoverable, but it requires walking the cascade
correctly, which producers regularly violate.

**XLSX cells are a grid; tables are inferred.** A spreadsheet is
a sparse cell array with formatting. The thing a human calls "the
sales table" is some contiguous rectangle the user happened to
treat as a table — possibly with merged header cells, possibly
with a totals row in bold, possibly with three blank rows between
two unrelated tables. Where one table ends and another begins is
a judgment call the file does not record.

**The legacy 1997 binary formats are worse.** `.doc`, `.xls`, and
`.ppt` are CFB / OLE2 compound files. DOC stores text in a
"piece table" data structure that supports fast-save (uncommitted
edits live in the file alongside the committed text). XLS encodes
records in a stream that interleaves cell values, formulas,
formatting, and chart definitions. PPT references slide records
through a `PersistDirectory` that survived multiple file revisions.
These formats still fill government archives, legal discovery,
insurance back-offices, and decades of corporate share drives, and
most modern tooling has quietly stopped supporting them.

**A scanned PDF contains no text at all.** A scan is a sequence of
page-sized images embedded in PDF wrapping. There is no glyph data
to extract. The only way to recover text is to render the page and
hand it to an OCR engine. There is no toolkit on earth that can
extract text from pixels without OCR; the question is whether the
toolkit makes OCR easy to attach.

**Hybrid documents are the worst of all.** A 200-page legal exhibit
is often 180 pages of digitally-generated text plus 20 inserted
scans of signed forms. Without per-page detection, you either OCR
nothing (and lose the forms) or OCR everything (and waste twenty
minutes on pages you already had clean text for).

## Why heuristics

Where the spec or the file is silent, a parser has to choose. udoc
exposes those choices as tiered APIs rather than hiding them behind
"the right answer."

**Reading order.** The PDF backend runs a four-tier cascade for
reading-order reconstruction (content-stream order on coherent
producers, X-Y cut for multi-column, region-projection for complex
spreads, layout-model override via a hook for the hardest cases).
Each tier is documented; the tier picked for a given page surfaces
on diagnostics; you can constrain or override the choice from the
config.

**Tables.** PDF table detection runs a ruled-lattice strategy and
a text-edge column strategy and merges the results. Both can be
fooled by dense unruled tables, rotated headers, or producers that
emit cell content out of grid order. When they fail, attach a
layout model via the hook protocol; the toolkit keeps the parsing
parts honest by surfacing what it did rather than silently fudging.

**Document recovery.** Real producers ship malformed `xref` tables,
wrong stream lengths, missing ToUnicode CMaps, malformed central
directories in OOXML zips, fast-saved DOC piece tables, and a
hundred other small lies. udoc's design tenet here is "lenient
parsing beats strict failure": try the spec, recover when it
doesn't hold, emit a typed warning instead of an exception. A
partial extraction with `StreamLengthMismatch` warnings beats a
full exception that aborts the pipeline.

The principle: every place the toolkit had to guess, the guess is
visible. Filter on `Diagnostics.kind` in your pipeline and you can
sort the documents the parser was confident about from the ones
where you should look at the rendering before trusting the text.

## Why OCR (even from a "full toolkit")

The frequent FAQ: "If udoc is a full document toolkit, why do I
need OCR?" Because OCR is not a parser — it is a model that
reconstructs text from pixels. No parser can replace it. The
question is whether the parser knows to ask for it.

udoc's answer:

- **Detect scans automatically.** Pages with one large image,
  fewer than five text spans, and no extractable glyph data are
  flagged as `LikelyScanned` on the diagnostics sink. The OCR
  hook fires only on those pages by default.
- **OCR is a hook, not a built-in.** Tesseract, GLM-OCR,
  DeepSeek-OCR, Textract, Document AI, Azure Form Recognizer —
  the right OCR engine depends on the document, the language, the
  hardware, the budget, and the data-egress policy. udoc does not
  ship one. The hook protocol means you wire whichever one you
  want, and your choice does not change as udoc evolves.
- **Mixed documents handled directly.** The detector runs per
  page, not per document. OCR fires on the scanned inserts and
  skips the digitally-generated body.

A scanned page hitting an OCR-less udoc invocation comes back with
empty text and a `LikelyScanned` warning rather than silently
producing a clean-looking-but-empty result. That is the diagnostic
contract, not a bug.

## Why hooks

Hooks are udoc's native extension mechanism. They exist because
the things you want to do *to* a document are open-ended in a way
that the things you want to do *with* a document model are not.

The toolkit converges every format on one document model. The
*shape* of the data is fixed: blocks, inlines, tables, presentation
overlays. But the *content* of those blocks can be enriched by an
arbitrary number of external systems — OCR engines, layout
detectors, NER models, classifiers, table reconcilers, language
detectors, redaction filters. Putting any of those in core would
be a bet on one model family in a space that turns over every
six months.

Hooks turn that into a pipeline:

```
udoc parse -> [OCR hooks] -> [layout hooks] -> [annotate hooks] -> Document
```

Each phase is optional. Anything that can read JSON line by line on
stdin and write JSON line by line on stdout can plug in. The
[hooks chapter](hooks.md) has the protocol; the
[examples directory](https://github.com/newelh/udoc/tree/main/examples/hooks)
has working hooks for Tesseract, GLM-OCR, DeepSeek-OCR, DocLayout-YOLO,
NER, and a cloud-OCR template you can adapt to the provider of your
choice.

The hook process is long-lived: udoc spawns it once per extraction
and reuses it across every page. Model setup amortises across the
document. For genuinely async backends (cloud OCR with poll-for-result
APIs), the hook owns the polling and udoc just waits.

## Why the toolkit philosophy

The design tenets that follow from the above are catalogued in
[Architecture](architecture.md). In short: udoc recovers and warns
rather than failing a parse outright, exposes heuristic layers as
tiered APIs so the caller picks the level of confidence, and
surfaces recoverable issues as typed diagnostics instead of stderr
noise. Per-page work is deferred until the caller asks for it,
and every parser is in-tree.

The thread is honesty about what documents are. Some answers are
heuristic, some inputs need OCR, and a pipeline built on top is
more robust if it reads the diagnostics than if it pretends every
extraction returned a clean answer.

## Where to next

- [Overview](index.md) — install, highlights, and quick examples for each surface.
- [Library guide](library.md) — the document model, configuration,
  diagnostics, escape hatches.
- [Hooks chapter](hooks.md) — the JSONL protocol with worked
  Python hooks.
- [PDF rendering & OCR](render.md) — the page rasteriser and the
  hook-driven OCR wiring.
- [Per-format guides](formats/index.md) — the quirks specific to
  each backend and the diagnostics they emit.
- [Architecture](architecture.md) — the document model, design
  tenets, performance notes.

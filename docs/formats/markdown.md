# Markdown

CommonMark with a useful subset of GitHub-Flavored Markdown extensions.
The simplest format udoc handles, and intentionally so: Markdown is
already plain text, so udoc's value here is producing a typed
`Document` model from it that downstream code can treat the same way
as PDF / DOCX / etc. output. Mixed-format pipelines do not need a
separate Markdown parser.

## Why this format is interesting

Markdown's appeal is that it is *almost* unambiguous. A `#` at
column 0 is always a heading; a fenced code block is always literal
content; an indented list is always a list. The hard parts are
always the corners: setext-style headings (`====` underline)
versus ATX (`#` prefix); emphasis nesting (`**this *that* this**`);
the perpetual battle between MathJax / KaTeX / Pandoc dollar-math
and CommonMark's inline-code semantics; HTML-in-Markdown.

udoc's parser implements CommonMark plus the GFM extensions most
users actually expect: pipe tables, task lists, autolinks,
strikethrough. Things outside that set come through as plain text or
the closest CommonMark equivalent, with a warning if information was
lost.

## What you get

- Headings (ATX `#` and Setext `===` / `---`).
- Paragraphs.
- Lists (bulleted, numbered, task lists).
- Tables (GFM pipe-table syntax).
- Code blocks (fenced and indented), with language hints preserved
  on `Block::CodeBlock`.
- Inline formatting: bold, italic, code, links, images.
- Blockquotes.
- Horizontal rules.
- Document metadata: `Document::metadata.title` is set from the
  first H1 heading if the file has no front-matter.

## What you do not get

- HTML-in-Markdown is treated as raw text. udoc does not attempt to
  parse embedded HTML.
- Footnotes are recognised as a GFM extension; some non-standard
  footnote dialects (Pandoc-style multi-paragraph footnotes) round-
  trip as plain text.
- Custom directive blocks (MyST, RST-style admonitions) are not
  interpreted.
- Math (`$...$`, `$$...$$`, `\(...\)`) comes through as inline code
  or paragraph text. Math parsing and rendering are not currently
  supported.
- Front-matter (YAML / TOML at the top of the file) is preserved as
  raw paragraph text. If you need it parsed, do it before passing the
  file to udoc.

If you need any of the items marked "not currently supported", please
[open a feature request](https://github.com/newelh/udoc/issues).

## Format nuances worth knowing

### One "page" only

Markdown has no inherent pagination. `Document::metadata.page_count`
is always 1; the entire file is the page. Streaming extraction works
the same way it would for any single-page document.

### Title detection

The `Document::metadata.title` is populated from the first H1
heading in the file when no other source is available. If your
Markdown opens with `# Project Name`, that becomes the title.
If your file uses Setext headings (`Project Name\n===`), the same
applies. Files without an H1 have `title = None`.

### Image links are not fetched

`![alt text](https://example.com/image.png)` produces a
`Block::Image` with the URL on the relationships overlay, but udoc
does not download remote images. If you need the bytes, fetch them
yourself; the URL is right there in the model.

Local image references (`![alt](./path.png)`) are treated the same
way — the path is preserved, the bytes are not loaded. This is a
deliberate choice: extracting Markdown should not have side effects
on the filesystem.

### GFM autolinks are heuristic

Bare URLs like `https://example.com` become `Inline::Link` nodes per
GFM rules. The detection is heuristic and matches the upstream GFM
spec where it is unambiguous; pathological strings (URLs containing
unbalanced parens, or trailing punctuation that reads like part of
the URL) may extract slightly differently than your viewer renders
them.

### Hard breaks vs soft breaks

A line ending followed by another line of text is a soft break in
CommonMark — rendered as a space, not a line break. Two trailing
spaces or a `\` at end of line is a hard break. udoc emits
`Inline::SoftBreak` and `Inline::LineBreak` accordingly so the text
writer can reproduce either reading.

## Layers within udoc-markdown

```
block      block-level CommonMark parser (paragraphs, headings, lists, code blocks, blockquotes, HRs, HTML blocks)
inline     inline parser (emphasis, links, code, autolinks, strikethrough, soft/hard breaks)
table      GFM pipe-table parser (recognised by block, dispatched to here)
document   public API (MdDocument, page() entry point)
```

The cleanest of the udoc backends, because Markdown's grammar is
unambiguous by design. Block-level parsing runs first to identify
each block's boundaries; inline parsing then runs over the
recognised text spans. Tables are recognised at the block level
(pipe pattern + alignment row) and dispatch to the table layer
for cell-level inline parsing.

## Failure modes

There is essentially no failure mode for well-formed Markdown
extraction — the CommonMark spec is closed and parseable. The
real-world issues are upstream of udoc:

- **Files that are not actually Markdown.** A `.md` file containing
  Org-mode, RST, or AsciiDoc still parses without error but produces
  surprising output (everything becomes paragraphs / inline code).
  Format detection can override.
- **Files with non-UTF-8 encoding.** udoc assumes UTF-8 by default
  and will surface decode errors at the byte boundary that fails.

## Diagnostics

| `kind`                  | When                                                              |
|-------------------------|-------------------------------------------------------------------|
| `MarkdownExtensionUnknown`| A `:::name` directive or other extension construct is not parsed.|
| `EncodingNotUtf8`       | The file is not valid UTF-8 at some byte offset.                  |

## Escape hatches

```rust,no_run
use udoc_markdown::MdDocument;
use udoc_core::backend::{FormatBackend, PageExtractor};

let mut doc = MdDocument::open("README.md")?;
for i in 0..doc.page_count() {
    let mut page = doc.page(i)?;
    println!("{}", page.text()?);
}
# Ok::<(), udoc_core::error::Error>(())
```

## See also

- For a richer Markdown parser with full math / Pandoc / MyST
  support, the Markdown ecosystem has many specialised
  implementations. udoc's parser optimises for "produce a uniform
  Document model alongside the other formats" rather than for
  Markdown-specific fidelity.

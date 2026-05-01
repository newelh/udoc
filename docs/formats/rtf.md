# RTF (Rich Text Format)

RTF predates the Web. It first shipped with Word for Windows 1.0 in
1989, has accumulated decades of producer-specific extensions, and
remains in heavy use as an interchange format for legacy office
tooling, government records systems, and email-attachment workflows
that have never moved off it. udoc parses control words, groups,
codepage-encoded text, and the embedded picture format natively.

## Why this format is interesting

RTF is plain text. A document is an outermost group `{...}` containing
control words `\rtf1`, `\ansi`, `\b`, etc., nested groups, and
literal text. The format's age is its complexity: the syntax has
been extended and re-extended for thirty years, and most extensions
were added by Microsoft Word for one version and then never removed.
What you parse against is not "the RTF spec" but "the union of every
RTF dialect Word has ever emitted".

Two structural choices that shape the parser:

1. **Control words have global scope but group-level effects.** A
   `\b` (bold-on) inside a group affects only the runs in that group;
   when the group closes, formatting reverts. This means the parser
   maintains a stack of formatting states pushed at each `{` and
   popped at each `}`. Get the bracket count wrong by one and the
   styling for the rest of the document is silently inverted.
2. **Mixed text encoding.** RTF text can come through as 7-bit
   ASCII literals, `\'XX` 8-bit codepage escapes, `\u`*N*`?` Unicode
   escapes (where `?` is a fallback character for non-Unicode
   readers), and raw bytes inside binary `\bin` blocks. A single
   paragraph may use all four. udoc's lexer handles the multiplexing
   so the higher layers see uniform text runs.

## What you get

- Paragraph text with formatting (bold, italic, underline, font,
  size, colour).
- Tables (`\trowd...\row` groups).
- Lists.
- Embedded images decoded from `\pict` groups (PNG, JPEG, EMF, WMF).
- Document metadata via the `\info` group (title, author, subject,
  creation date, etc.).
- Codepage-aware character decoding via `\ansicpg`, `\u`, `\'XX`.
  20+ CJK codepages (Shift-JIS, GB18030, Big5, EUC-KR, etc.) via
  the shared `udoc_core::codepage::CodepageDecoder`.

## What you do not get

- Drawing-object rendering. RTF can carry vector drawings via
  `\shp` / `\shpinst` containers; the drawing data is preserved in
  the document model but rasterising it is not currently supported.
- OLE-embedded objects (`\objemb`). The reference is surfaced; OLE
  payload decoding is not currently supported.
- Field codes evaluated dynamically (`{\field {\fldinst HYPERLINK
  "..."} {\fldrslt "label"}}`). The cached result text comes through;
  the field is not re-evaluated. (Design decision — udoc reports
  what is there, not what would be there at view time.)
- Form fields with runtime state.

If you need any of the items marked "not currently supported", please
[open a feature request](https://github.com/newelh/udoc/issues).

## Format nuances worth knowing

### Codepage governs 8-bit byte escapes

The `\ansicpg`*N* control word at the document start declares the
codepage for `\'XX` 8-bit byte escapes. A document that says
`\ansicpg932` decodes `\'82\'A0` as Japanese (Shift-JIS). udoc honours
the declaration and routes 8-bit escapes through the matching encoder.

If the document does not declare a codepage, udoc falls back to
CP-1252 (Windows Latin-1) — the default Word has used since the early
1990s. Documents from Word for Mac may declare `\mac` instead, in
which case Mac Roman applies.

### Mixed-codepage documents

Some documents (rare but real, especially exports from translation
tools) carry text fragments in *multiple* codepages within one file.
The codepage in effect at any given moment is the most recent
`\ansicpg` declared inside the current group scope. udoc tracks the
codepage stack alongside the formatting stack so each text fragment
decodes correctly even when the codepage changes mid-document.

### Unicode escape pairing

The `\u`*N*`?` escape carries a 16-bit signed Unicode codepoint
followed by a fallback character (the `?`) that non-Unicode-aware
readers should display instead. udoc reads the Unicode code point and
discards the fallback. For surrogate pairs (codepoints > U+FFFF), RTF
emits two consecutive `\u` escapes; udoc pairs them per UTF-16
surrogate rules.

A common producer bug: emitting a single low surrogate without a
preceding high surrogate, or vice versa. udoc treats orphan
surrogates as U+FFFD and warns.

### `\bin` directives carry binary data with explicit length

`\binN` declares that the next N bytes are raw binary, not RTF
syntax. The parser must skip those bytes literally without trying
to interpret control words inside them. udoc validates the declared
length against the actual bytes available before the next group
delimiter and warns if they disagree (a producer that miscounted, or
a truncated file).

### Group depth bounds

Pathological RTF documents can declare arbitrary nesting depth and
exhaust the parser's stack. udoc caps recursion at a configurable
limit (default safe single-digit number) and rejects documents that
exceed it with a clear error.

## Layers within udoc-rtf

```
lexer        control-word + group-marker + literal-text tokenizer
state        formatting + codepage stack pushed at { and popped at }
codepage     codepage-aware byte decoder (re-exports udoc_core::codepage)
parser       token stream -> structured groups + paragraphs + table rows
table        \trowd...\row group reconstruction into Table blocks
image        \pict + \shp + \objemb decoders (PNG/JPEG/EMF/WMF)
convert      RTF nodes -> unified Document model
document     public API
```

`state` is the layer that makes nested groups work. Every `{`
pushes a copy of the current formatting state plus the active
codepage; every `}` pops. Get the bracket balance wrong by one and
trailing text inherits the wrong styling — it is silent and easy
to miss, so the lexer asserts on `}` underflow.

## Failure modes

- **Unbalanced braces.** udoc's lexer detects mismatched
  `{` / `}` counts and warns; depending on where the imbalance
  falls, the document may extract with the wrong formatting state
  for trailing text.
- **Codepage / declaration mismatch.** Bytes do not decode under
  the declared codepage; udoc emits replacement characters and
  warns.
- **`\bin` length disagreement.** Declared length does not match
  the actual binary block length.
- **Group depth past the limit.** Document rejected with a clear
  error.

## Diagnostics

| `kind`                  | When                                                              |
|-------------------------|-------------------------------------------------------------------|
| `BraceImbalance`        | The brace counter reaches zero before EOF, or stays nonzero at EOF.|
| `CodepageDecodeFailed`  | A `\'XX` byte sequence does not decode under the active codepage. |
| `OrphanSurrogate`       | A `\uN` value is a low surrogate without a preceding high surrogate (or vice versa). |
| `BinLengthMismatch`     | A `\binN` declaration's length disagrees with the data available. |
| `GroupDepthLimit`       | Nesting depth exceeded the configured maximum.                    |

## Escape hatches

```rust,no_run
use udoc_rtf::RtfDocument;
use udoc_core::backend::{FormatBackend, PageExtractor};

let mut doc = RtfDocument::open("notes.rtf")?;
for i in 0..doc.page_count() {
    let mut page = doc.page(i)?;
    println!("{}", page.text()?);
}
# Ok::<(), udoc_core::error::Error>(())
```

## See also

- The [font engine page](../fonts.md) covers the
  `CodepageDecoder` shared with the legacy DOC and XLS backends.

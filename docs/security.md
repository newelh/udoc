# Security

This is the in-depth complement to [`SECURITY.md`](../SECURITY.md). The
short reporting policy and threat model live there; this page covers the
unsafe-code audit, the fuzzing posture, and a few hardening details that
do not fit on a one-page policy.

## Reporting

See [`SECURITY.md`](../SECURITY.md). The short version: GitHub Security
Advisories are preferred, `me@newel.dev` is the email fallback.

## Unsafe code policy

Every crate root has `#![deny(unsafe_code)]`. The compiler refuses to
build any `unsafe` block in workspace code that is not whitelisted on
the specific module.

### The one whitelisted module

`udoc-pdf::io::mmap_impl` contains a single audited `unsafe` block that
calls `memmap2::MmapOptions::map`. The safety argument is:

- The memory map's lifetime is bound to the file handle held by the
  `MappedFile` wrapper.
- The wrapper exposes only `&[u8]` views of the mapping. Callers cannot
  obtain a `&mut [u8]` and cannot keep a reference longer than the
  wrapper.
- File truncation while a mapping is alive is documented as undefined
  behaviour at the OS level (SIGBUS on Linux). Callers either trust the
  filesystem (the common case) or use one of the buffered `Source`
  impls instead of `mmap`.

The wrapper is exercised by every test that reads a real file from disk;
the safety properties are testable as observable behaviour, not just
review-only invariants.

### Other unsafe in the dependency chain

We rely on `#![deny(unsafe_code)]` for our own code. Dependencies have
their own unsafe — `flate2` (zlib bindings), `aes`, `cbc`, `md-5`,
`memmap2`, and the standard library. We do not re-audit those. If a CVE
lands against a shipped dep, we bump and ship a patch release.

## Fuzzing

Fuzz targets cover the parser surface across five clusters:

- **PDF parser** — lexer, object parser, xref, content stream.
- **PDF font** — TTF, CFF, Type 1, ToUnicode parser.
- **PDF crypto** — encryption parameter parsing (not the crypto
  primitives themselves; those are RustCrypto).
- **Containers** — ZIP, XML, CFB, OPC.
- **Image decoders** — CCITT, JBIG2, JPEG headers.
- **Format-specific** — RTF lexer, DOCX walker, XLSX shared-strings.

## HashDoS resistance

Any `HashMap` keyed by attacker-controlled values uses `ahash`. This
includes:

- The PDF object resolver's cache (keyed by `PdfRef`).
- Font cmap and ToUnicode tables (keyed by character code or glyph
  index from the font).
- ZIP central-directory lookups (keyed by entry path from the archive).

`HashMap` keyed by integers we generate ourselves (`NodeId` arena
indices, internal IDs) uses `FxHash` for speed. The split is intentional
— ahash on internally-generated keys is wasted CPU; FxHash on
attacker-controlled keys is a footgun.

## Resource budgets

Two layers of budget control extraction:

1. **Per-document `Limits`.** Hard ceilings on individual document
   parameters: max file size, max page count, max decompressed-stream
   size, max object-graph depth. Defaults are conservative; raise them
   if you trust your input.
2. **Per-process `memory_budget`.** Soft RSS cap that triggers
   between-document cache resets in long-running batch workers. Use
   when ingesting 10K+ documents in one process to bound peak heap.

Neither is a security boundary in the classical sense — they bound the
blast radius of a malicious input but do not provide isolation. For
strong isolation, run udoc in a sandbox (seccomp, container, separate
process per ingest tier).

## Crypto

PDF encryption uses RustCrypto crates: `md-5`, `aes`, `cbc`. Limited to
the standard security handler at revisions 4 (AES-128) and 6 (AES-256);
public-key handler is not implemented.

There is no homegrown cryptographic code anywhere in the workspace. We
do not roll our own block ciphers, hash functions, or KDFs. This is a
deliberate, durable policy.

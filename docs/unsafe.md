# Unsafe code audit

Every crate in the workspace has `#![deny(unsafe_code)]` at its root.
The compiler refuses to build any `unsafe` block that is not explicitly
whitelisted on a per-module basis.

This file enumerates every whitelisted `unsafe` block in the workspace,
the safety argument for each, and the test that exercises it.

## `udoc-pdf::io::mmap_impl::map_file`

**Location:** `crates/udoc-pdf/src/io/mmap_impl.rs`

**Reason:** memory-mapping a file via `memmap2::MmapOptions::map`
requires `unsafe`. The OS API (`mmap(2)`) cannot guarantee that the
backing file will not be truncated or modified by another process while
the mapping is alive; the Rust wrapper passes that guarantee to the
caller.

**Safety argument:**

1. The mapping is wrapped in a private `MappedFile` struct that holds
   the source `File` for as long as the mapping lives. Dropping the
   wrapper unmaps the memory before the file handle is closed.
2. The wrapper exposes only `&[u8]` views of the mapped bytes through
   the `RandomAccessSource` trait. Callers cannot obtain a `&mut [u8]`,
   so the mapping is effectively read-only at the type level.
3. File truncation while the mapping is alive is undefined behaviour at
   the OS level (typically SIGBUS on Linux when the truncated region is
   accessed). This is documented on the public `MappedFile::open`
   constructor; callers who cannot make the no-truncation assumption
   should use the buffered or chunked `Source` implementations instead
   of `mmap`.
4. The OS enforces page-level memory safety via the page tables.
   Out-of-bounds reads are caught by the kernel as SIGSEGV; the Rust
   side cannot read past the mapped region in a way that would corrupt
   the process.

**Tests exercising this:** every integration test that opens a PDF from
disk (`crates/udoc-pdf/tests/corpus_integration.rs`,
`crates/udoc/tests/cli.rs`, etc.). The mmap path is the default for
file-backed sources, so any disk-backed test exercises it.

## No other unsafe in the workspace

That is the complete list. Workspace-wide `#![deny(unsafe_code)]` makes
the absence enforceable; new unsafe code requires lifting the deny on a
specific module, which is a deliberate, reviewable change.

//! `udoc fonts` / `udoc images` / `udoc metadata` introspection
//! subcommands (issue #157).
//!
//! These surfaces wrap extraction so operators can answer quick
//! "what is in this file" questions from the shell without writing
//! Rust. They deliberately share the extraction pipeline used by the
//! rest of the CLI: the output is a narrow view over the same
//! [`Document`](udoc::Document) that `udoc extract` produces.
//!
//! Each subcommand emits JSON by default so the output is pipeline
//! friendly (jq, etc.) and offers a `--format text` opt-out for humans.

// pub(crate) to mirror the parent cli/mod.rs
// gating. These are bin-internal subcommand impls, not part of the
// udoc facade's public API.
pub(crate) mod fonts;
pub(crate) mod images;
pub(crate) mod metadata;

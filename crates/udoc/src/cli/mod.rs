//! CLI subcommand implementations.
//!
//! Subcommand tree:
//! - `extract` (default): the extraction pipeline. Bare `udoc <file>`
//!   dispatches here to preserve the zero-ceremony shortcut.
//! - `render`: render pages to PNG.
//! - `fonts` / `images` / `metadata`: introspection subcommands
//!   (#157).
//! - `audit-fonts`: font-resolution audit.
//! - `render-diff` / `render-inspect`: renderer-iteration
//!   tooling.
//! - `completions`: hidden helper that emits shell completion scripts.

// pub(crate) to keep subcommand impls out of any
// public surface. Reachable to `main.rs` via the `#[path]`-mounted `cli`
// module declared in `crates/udoc/src/main.rs` (the bin's crate root,
// where these modules become `crate::cli::*`).
pub(crate) mod audit;
pub(crate) mod completions;
pub(crate) mod features;
pub(crate) mod inspect;
pub(crate) mod intro;
pub(crate) mod mangen;

// Renderer-iteration debug tooling. Gated behind the `dev-tools` feature
// + : the default release binary excludes
// these subcommands so the alpha CLI surface stays small. CI builds with
// --features dev-tools to keep the render-diff / render-inspect QA flow.
#[cfg(feature = "dev-tools")]
pub(crate) mod render_diff;
#[cfg(feature = "dev-tools")]
pub(crate) mod render_inspect;

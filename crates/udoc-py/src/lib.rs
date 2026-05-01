//! Python bindings for udoc -- module entry point.
//!
//! The cdylib is named `udoc` so users `import udoc`. This file
//! is intentionally tiny: it declares the per-area submodules and wires
//! each into the `#[pymodule]` registration in a fixed order. Every
//! submodule owns a `pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()>`
//! and is filled in by a dedicated W1-1b task.
//!
//! Registration order matters:
//! - `errors` first: the exception types are referenced everywhere else.
//! - `types` early: `config` and `document` reference the typed shapes.
//! - `extract` last: it pulls in everything above.

// pyo3 0.28's #[pymodule] / #[pyfunction] macros expand to unsafe blocks.
// The workspace #![deny(unsafe_code)] policy is relaxed here because the
// unsafe is macro-generated and is the supported pyo3 contract.
#![allow(unsafe_op_in_unsafe_fn)]
// `__match_args__`, `__getattr__`, etc. are Python dunder names that
// don't follow Rust's UPPER_CASE constant convention. The pyo3 contract
// requires them at this exact spelling.
#![allow(non_upper_case_globals)]
// W1-FOUNDATION lands the convert visitor functions + value pyclasses
// before W1-METHODS-* fills in the methods that actually call them.
// The dead_code warnings are expected and clear up automatically as
// downstream tasks land. Keep the allow narrow: it only fires on items
// the downstream tasks need.
#![allow(dead_code)]

use pyo3::prelude::*;

mod asyncio; // W2-ASYNCIO (no-op stub; real adapter is python/udoc/asyncio.py)
mod chunks; // W1-CHUNKS
mod config; // W1-CONFIG
mod convert; // W1-CONVERT ( visitor)
mod corpus; // W1-CORPUS
mod document; // W1-DOCUMENT
mod errors; // W1-EXCEPTIONS
mod extract; // W1-EXTRACT
mod markdown; // W1-MARKDOWN
mod render; // W2-RENDER
mod types; // W1-TYPES

#[pymodule]
fn udoc(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    errors::register(m)?;
    types::register(m)?;
    config::register(m)?;
    document::register(m)?;
    chunks::register(m)?;
    markdown::register(m)?;
    extract::register(m)?;
    corpus::register(m)?;
    render::register(m)?;
    Ok(())
}

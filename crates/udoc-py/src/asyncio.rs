//! W2-ASYNCIO: Python-side `udoc.asyncio` adapter (stdlib executor bridge).
//!
//! Per  §6.5 and , the async story for udoc is a
//! pure-Python wrapper around the sync API: every entry point bounces
//! through `loop.run_in_executor(...)` so `await udoc.asyncio.extract(p)`
//! does not block the event loop. Per-doc work uses the default
//! `ThreadPoolExecutor` (the GIL is released around `udoc.extract` via
//! `py.detach`); `Corpus.parallel(N)` uses a `ProcessPoolExecutor` with
//! the `multiprocessing.get_context("spawn")` context.
//!
//! Implementation lives in `python/udoc/asyncio.py`. This Rust file is
//! a no-op stub kept for symmetry with the other 8 W1 modules and to
//! reserve the `asyncio.rs` slot for future native helpers (cancel
//! tokens, executor bridging hooks, etc). It exports `register(m)`
//! returning `Ok(())`; lib.rs does not currently call it because the
//! module is purely Python-side, but the file exists so the next agent
//! who needs a native helper has a slot ready.
//!
//! Why no Rust-native async machinery here:
//!
//! - We deliberately reject the `pyo3-asyncio` + tokio bridge. `udoc-py`
//!   has no tokio runtime, the workload is CPU-bound (best served by
//!   processes, not futures), and dragging in tokio would add ~5 MB to
//!   the wheel for no measurable benefit.
//! - The stdlib `loop.run_in_executor(executor, sync_call)` primitive
//!   is sufficient for the entire async surface (`extract`,
//!   `extract_bytes`, `stream`, `Corpus` + aggregators).
//! - Cancel propagation rides on
//!   `executor.shutdown(wait=False, cancel_futures=True)`; no Rust code
//!   needs to be involved.
//!
//! Tests: see `python/tests/test_asyncio.py` (W3-PYTEST). The Rust stub
//! has no surface to unit-test; `cargo test -p udoc-py --lib` count is
//! unchanged from W1-METHODS baseline.

use pyo3::prelude::*;

/// Module registration hook. No-op by design (the asyncio surface is
/// Python-side, not Rust-side). Kept to match the other 8 register
/// functions called from `lib.rs::udoc`.
#[allow(dead_code)]
pub fn register(_m: &Bound<'_, PyModule>) -> PyResult<()> {
    Ok(())
}

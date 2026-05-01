//! Hook protocol stress tests (T0-HOOKS-STRESS, sprint 55).
//!
//! Surface bugs in the  hooks protocol BEFORE the alpha freeze locks
//! the surface. Six test modules:
//!
//! - `timeout`     -- hook sleeps past the timeout window
//! - `error_exit`  -- hook exits non-zero
//! - `hung`        -- hook traps SIGTERM and refuses to die
//! - `oversized`   -- hook emits >10 MB of JSONL
//! - `malformed`   -- hook emits invalid JSON, half-lines, BOMs, NULs, etc.
//! - `concurrent`  -- coverage smoke + parallel extraction
//!
//! Tests gated to Unix because the helper scripts use `#!/bin/bash`. On
//! Windows the modules compile but contain no test functions.

#![cfg(all(unix, feature = "json"))]

mod hooks_stress {
    pub mod helpers;

    pub mod concurrent;
    pub mod error_exit;
    pub mod hung;
    pub mod malformed;
    pub mod oversized;
    pub mod timeout;
}

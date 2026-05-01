//! Shared helpers for the hooks_stress test suite.
//!
//! Builds temporary bash scripts on disk, returns their path so individual
//! tests can hand them to [`HookSpec::from_command`]. Caller is responsible
//! for keeping the returned `ScriptHandle` alive until the test ends -- it
//! cleans up the temp dir on drop.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// RAII handle to a temp script. Drops the parent directory on Drop.
pub struct ScriptHandle {
    pub path: PathBuf,
    dir: PathBuf,
}

impl Drop for ScriptHandle {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Build a bash script at a unique tmp path containing `body`.
///
/// `label` distinguishes scripts in test logs. The script is marked
/// executable. Returns a [`ScriptHandle`] that owns the temp directory.
pub fn write_script(label: &str, body: &str) -> ScriptHandle {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "udoc-hooks-stress-{}-{}-{}",
        std::process::id(),
        id,
        label
    ));
    std::fs::create_dir_all(&dir).expect("create tmp dir");

    let path = dir.join(format!("{label}.sh"));
    {
        let mut f = std::fs::File::create(&path).expect("create script");
        f.write_all(b"#!/bin/bash\n").expect("write shebang");
        f.write_all(body.as_bytes()).expect("write body");
        f.write_all(b"\n").expect("write trailing newline");
    }
    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&path)
        .status()
        .expect("chmod +x");

    ScriptHandle { path, dir }
}

/// Path to the workspace root (one above `crates/udoc`).
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/udoc -> crates")
        .parent()
        .expect("crates -> repo root")
        .to_path_buf()
}

/// Path to a small PDF fixture that always extracts cleanly.
pub fn small_pdf() -> PathBuf {
    workspace_root().join("crates/udoc-pdf/tests/corpus/minimal/two_column.pdf")
}

/// Path to a representative real-world PDF.
pub fn realworld_pdf() -> PathBuf {
    workspace_root().join("crates/udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf")
}

/// Resolve a fixture path under `crates/<crate>/tests/corpus/...`. Helper for
/// the per-format coverage smoke.
pub fn fixture(rel: &str) -> PathBuf {
    workspace_root().join(rel)
}

/// Standard no-op annotation hook handshake line. Matches  / +.
/// The hook declares `annotate` capability with `text` need so we don't have
/// to render page images to satisfy it.
pub const NOOP_HANDSHAKE: &str = r#"{"protocol":"udoc-hook-v1","capabilities":["annotate"],"needs":["text"],"provides":["labels"]}"#;

/// A minimal "no-op" hook script body. Emits the handshake then echoes an
/// empty annotation response to every page request.
pub const NOOP_HOOK_BODY: &str = r#"echo '{"protocol":"udoc-hook-v1","capabilities":["annotate"],"needs":["text"],"provides":["labels"]}'
while IFS= read -r line; do
    echo '{"annotations":[]}'
done
"#;

/// Run a hook against a path and return the resulting Document plus the
/// outcome of `runner.run()`. Falls back to extracting bytes when path-based
/// extraction fails (some formats need the path; some need bytes).
pub fn run_hook_against(
    spec: udoc::hooks::HookSpec,
    config: udoc::hooks::HookConfig,
    path: &Path,
) -> (Result<udoc::Document, udoc::Error>, Result<(), udoc::Error>) {
    let runner = match udoc::hooks::HookRunner::new(&[spec], config) {
        Ok(r) => r,
        Err(e) => return (udoc::extract(path), Err(e)),
    };
    let mut runner = runner;
    let mut doc = match udoc::extract(path) {
        Ok(d) => d,
        Err(e) => return (Err(e), Ok(())),
    };
    let outcome = runner.run(&mut doc, None);
    (Ok(doc), outcome)
}

// Some helpers are only used by certain modules; silence dead_code so
// each module's narrow imports don't trip the lint.
#[allow(dead_code)]
pub(super) fn _suppress_dead_code() {}

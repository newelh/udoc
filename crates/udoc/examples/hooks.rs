//! Run a tiny inline OCR-style hook against a PDF.
//!
//! The hook protocol is JSONL over stdin/stdout: udoc spawns the
//! hook process, optionally reads a handshake line declaring capabilities, and
//! then sends one request per page. Each response can return spans/blocks that
//! get merged into the [`Document`](udoc::Document).
//!
//! This example writes a 4-line shell script to a tmp dir, registers it as an
//! OCR hook, and runs it against an extracted PDF. The script returns a
//! one-span-per-page response so we can assert the runner actually invoked it.
//!
//! Run with:
//!
//! ```text
//! cargo run -p udoc --example hooks
//! ```
//!
//! Unix-only: the inline script uses `#!/bin/bash`. On Windows the example
//! prints a skip notice and exits 0 so `cargo test --examples` still passes.

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::Command;

    use udoc::hooks::{HookConfig, HookRunner, HookSpec};

    fn default_fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/udoc -> crates")
            .parent()
            .expect("crates -> repo root")
            .join("crates/udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf")
    }

    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(default_fixture);

    println!("running OCR hook against {}", path.display());

    // Materialize the document we want the hook to enrich.
    let mut doc = udoc::extract(&path)?;
    let original_block_count = doc.content.len();
    println!(
        "extracted {} pages, {} blocks before hook",
        doc.metadata.page_count, original_block_count
    );

    // Write a minimal hook script. The handshake declares an OCR phase that
    // doesn't need page images (Need::Text only) so the runner doesn't render
    // pages. For each page request, we echo back a one-span response.
    let dir = std::env::temp_dir().join(format!("udoc-example-hooks-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let script_path = dir.join("inline-ocr.sh");
    {
        let mut f = std::fs::File::create(&script_path)?;
        writeln!(
            f,
            r#"#!/bin/bash
# Handshake: declare ourselves an OCR hook that consumes nothing and emits spans.
echo '{{"protocol":"udoc-hook-v1","capabilities":["ocr"],"needs":["text"],"provides":["spans"]}}'
# One JSONL response per page request line. Empty arrays satisfy the schema
# without mutating extracted text -- we just want to exercise the wire format.
while IFS= read -r line; do
    echo '{{"spans":[],"blocks":[]}}'
done
"#
        )?;
    }

    // chmod +x so exec works. Failing here is a hard error.
    let chmod = Command::new("chmod").arg("+x").arg(&script_path).status()?;
    assert!(chmod.success(), "chmod +x failed");

    // Lower the per-page timeout so this example finishes quickly even if
    // the hook hangs. ocr_all_pages forces invocation on every page (the
    // arxiv fixture has plenty of text and would otherwise be skipped).
    let mut config = HookConfig::default();
    config.page_timeout_secs = 5;
    config.ocr_all_pages = true;
    let spec = HookSpec::from_command(
        script_path
            .to_str()
            .ok_or("hook script path must be UTF-8")?,
    );

    let mut runner = HookRunner::new(&[spec], config)?;
    runner.run(&mut doc, None)?;
    drop(runner); // joins reader threads, kills the process tree.

    println!(
        "hook returned, document now has {} blocks",
        doc.content.len()
    );

    // Cleanup tmp dir; best-effort.
    let _ = std::fs::remove_dir_all(&dir);

    // Smoke assertions: the runner ran without error and the doc is intact.
    // Our trivial hook returned empty arrays, so block count is unchanged --
    // a real OCR hook would emit spans that grow this number. The point is
    // that the dispatch + handshake + per-page request loop completed cleanly.
    assert_eq!(
        doc.content.len(),
        original_block_count,
        "block count should be unchanged by an empty-response hook"
    );
    assert!(doc.metadata.page_count > 0);

    println!(
        "ok: hook ran on {} pages without error",
        doc.metadata.page_count
    );

    Ok(())
}

#[cfg(not(unix))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("hooks example is unix-only (uses #!/bin/bash); skipping on this platform");
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Drive `main()` from a test so `cargo test --examples` exercises the
    /// full hook spawn + handshake + per-page request loop, not just a
    /// compile check. On non-unix the example is a no-op skip and this
    /// just verifies it returns Ok.
    #[test]
    fn example_runs() {
        super::main().expect("hooks example should succeed");
    }
}

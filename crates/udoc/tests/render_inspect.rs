//! Integration tests for `udoc render-inspect`.
//!
//! Covers all four dump modes (outlines, hints, edges, bitmap) with both
//! JSON and text output. The tests use a committed minimal-corpus PDF so
//! they run on every checkout.
//!
//! gated behind the `dev-tools` feature; the
//! subcommand is not present in the default release binary.
#![cfg(feature = "dev-tools")]

use std::path::PathBuf;
use std::process::Command;

fn udoc_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_udoc"))
}

fn test_pdf() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../udoc-pdf/tests/corpus/minimal/table_layout.pdf")
}

#[test]
fn outlines_dump_emits_schema_and_contours() {
    let output = udoc_cmd()
        .arg("render-inspect")
        .arg(test_pdf())
        .args(["--page", "1"])
        .args(["--dump", "outlines"])
        .args(["--format", "json"])
        .output()
        .expect("run udoc");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"schema_version\""), "stdout: {stdout}");
    assert!(stdout.contains("\"kind\":\"outline\""));
    assert!(stdout.contains("\"contours\":"));
    assert!(stdout.contains("\"units_per_em\""));
    assert!(stdout.contains("\"op\":\"move\""));
}

#[test]
fn hints_dump_shows_declared_and_auto() {
    let output = udoc_cmd()
        .arg("render-inspect")
        .arg(test_pdf())
        .args(["--page", "1"])
        .args(["--dump", "hints"])
        .args(["--format", "json"])
        .output()
        .expect("run udoc");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"kind\":\"hints\""), "stdout: {stdout}");
    assert!(stdout.contains("\"declared\":"));
    assert!(stdout.contains("\"auto_hinter\":"));
    assert!(stdout.contains("\"h_edges\":"));
    assert!(stdout.contains("\"v_edges\":"));
}

#[test]
fn edges_dump_has_segments_and_edges() {
    let output = udoc_cmd()
        .arg("render-inspect")
        .arg(test_pdf())
        .args(["--page", "1"])
        .args(["--dump", "edges"])
        .args(["--format", "json"])
        .args(["--compact"])
        .output()
        .expect("run udoc");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"kind\":\"edges\""), "stdout: {stdout}");
    assert!(stdout.contains("\"h_segments\":"));
    assert!(stdout.contains("\"v_segments\":"));
    assert!(stdout.contains("\"h_edges\":"));
    assert!(stdout.contains("\"v_edges\":"));
    // Compact mode: no pretty-print newlines inside the payload.
    let trailing = stdout.trim_end_matches('\n');
    assert!(
        !trailing.contains('\n'),
        "compact output should be a single line; got: {stdout}"
    );
}

#[test]
fn bitmap_ascii_dump_produces_nonempty_output() {
    let output = udoc_cmd()
        .arg("render-inspect")
        .arg(test_pdf())
        .args(["--page", "1"])
        .args(["--dump", "bitmap"])
        .args(["--ppem", "16"])
        .arg("--ascii")
        .output()
        .expect("run udoc");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // ASCII ramp is ` .:-=+*#%@`. Expect at least one non-space rendering char.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.chars().any(|c| "@#%*+=-:.".contains(c)),
        "expected ramp chars in output; got:\n{stdout}"
    );
    // Also expect newline-delimited rows.
    assert!(stdout.lines().count() > 1, "rows should be > 1: {stdout}");
}

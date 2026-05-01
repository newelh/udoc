//! Integration tests for `udoc render-diff`.
//!
//! Tests gate-pass, gate-fail, and missing-reference behaviour. The tests
//! that need `mutool` or `pdftoppm` on PATH are skipped (not failed) when
//! the tool is absent so the suite still runs in minimal CI environments.
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

fn tool_on_path(name: &str) -> bool {
    Command::new(name)
        .arg("-v")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

#[test]
fn happy_path_passes_low_gate() {
    if !tool_on_path("mutool") {
        eprintln!("skipping: mutool not on PATH");
        return;
    }
    let output = udoc_cmd()
        .arg("render-diff")
        .arg(test_pdf())
        .args(["--against", "mupdf"])
        .args(["--pages", "1"])
        .args(["--gate", "0.5"])
        .args(["--dpi", "150"])
        .output()
        .expect("failed to run udoc render-diff");
    assert!(
        output.status.success(),
        "exit {:?}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"gate\":\"pass\""), "stdout: {stdout}");
    assert!(stdout.contains("\"page\":1"));
    assert!(stdout.contains("\"ssim\":"));
    assert!(stdout.contains("\"psnr\":"));
}

#[test]
fn fail_below_gate_returns_exit_1() {
    if !tool_on_path("mutool") {
        eprintln!("skipping: mutool not on PATH");
        return;
    }
    let output = udoc_cmd()
        .arg("render-diff")
        .arg(test_pdf())
        .args(["--against", "mupdf"])
        .args(["--pages", "1"])
        .args(["--gate", "1.01"]) // unreachable
        .args(["--dpi", "150"])
        .output()
        .expect("failed to run udoc render-diff");
    assert_eq!(output.status.code(), Some(1), "should exit 1 on gate fail");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"gate\":\"fail\""), "stdout: {stdout}");
}

#[test]
fn missing_reference_tool_returns_exit_2() {
    // Simulate missing tool by setting PATH to an empty directory. The CLI
    // should detect the missing tool and exit 2 with a clear message.
    let tmp = std::env::temp_dir().join(format!("udoc-empty-path-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let output = udoc_cmd()
        .env("PATH", &tmp)
        .arg("render-diff")
        .arg(test_pdf())
        .args(["--against", "mupdf"])
        .args(["--pages", "1"])
        .args(["--gate", "0.5"])
        .args(["--dpi", "150"])
        .output()
        .expect("failed to run udoc render-diff");
    let _ = std::fs::remove_dir_all(&tmp);
    assert_eq!(
        output.status.code(),
        Some(2),
        "should exit 2 when reference tool is missing, got {:?}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("mutool") && stderr.contains("not found"),
        "stderr should mention missing mutool: {stderr}"
    );
}

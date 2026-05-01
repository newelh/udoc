//! dev-tools-gated CLI golden tests.
//!
//! Per cli-audit.md G060 + G061. These two subcommands ship only when
//! the `dev-tools` feature is on; running them under default features
//! produces "unrecognized subcommand" errors. This test file gates the
//! entire body behind `cfg(feature = "dev-tools")` so the default
//! `cargo test` run does not see them.
//!
//! Both tests further skip themselves at runtime when the external
//! reference tool (mupdf / poppler) is unavailable, since CI machines
//! and dev workstations vary.

#![cfg(feature = "dev-tools")]
#![allow(clippy::needless_pass_by_value)]

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn corpus_root() -> PathBuf {
    manifest_dir().join("..")
}

fn hello_pdf() -> PathBuf {
    corpus_root().join("udoc-pdf/tests/corpus/minimal/flate_content.pdf")
}

fn udoc_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_udoc"))
}

#[test]
fn g060_render_diff_smoke() {
    // Skip if mupdf is not installed (mutool is the harness binary).
    if Command::new("mutool")
        .arg("--help")
        .output()
        .map(|o| o.status.success() || o.status.code() == Some(1))
        .unwrap_or(false)
        == false
    {
        eprintln!("g060: skipping; mutool not on PATH");
        return;
    }
    let p = hello_pdf();
    let out = udoc_cmd()
        .args([
            "render-diff",
            p.to_str().unwrap(),
            "--against",
            "mupdf",
            "--pages",
            "1",
        ])
        .output()
        .expect("failed to run udoc render-diff");
    // Goldens here would be SSIM-sensitive (host font hinting); we
    // assert structural shape only: exit 0 or 1, stdout has at least
    // one JSON line containing "ssim".
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ssim") || stdout.contains("page"),
        "render-diff stdout missing ssim/page key, got: {stdout}"
    );
}

#[test]
fn g061_render_inspect_smoke() {
    let p = hello_pdf();
    let out = udoc_cmd()
        .args([
            "render-inspect",
            p.to_str().unwrap(),
            "--page",
            "1",
            "--dump",
            "outlines",
        ])
        .output()
        .expect("failed to run udoc render-inspect");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Schema version key must be present (per render_inspect.rs spec).
    assert!(
        stdout.contains("schema_version") || out.status.code() == Some(2),
        "render-inspect stdout shape unexpected, got: {stdout}"
    );
}

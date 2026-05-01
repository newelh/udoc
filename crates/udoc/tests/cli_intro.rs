//! Integration tests for the CLI introspection subcommands
//! (`udoc fonts`, `udoc images`, `udoc metadata`), the CLI
//! subcommand-tree restructure, and the shell-completion
//! generator ( /).
//!
//! These tests exercise the binary end-to-end through
//! `std::process::Command` so they cover clap parsing, dispatch, and
//! output formatting together.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn udoc_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_udoc"))
}

fn test_pdf() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../udoc-pdf/tests/corpus/minimal/table_layout.pdf")
}

fn test_image_pdf() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../udoc-pdf/tests/corpus/minimal/image_xobject.pdf")
}

fn test_info_pdf() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../udoc-pdf/tests/corpus/minimal/with_info.pdf")
}

fn temp_path(tag: &str) -> PathBuf {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("udoc-{}-{}-{}", tag, std::process::id(), id))
}

// ---------------------------------------------------------------------------
// bare-file zero-ceremony shortcut still works.
// ---------------------------------------------------------------------------

#[test]
fn bare_file_still_runs_extract() {
    // `udoc <file>` (no subcommand) should extract text just like it
    // did before the restructure. This is the  zero-ceremony
    // invariant.
    let output = udoc_cmd()
        .arg(test_pdf())
        .output()
        .expect("failed to run udoc");
    assert!(
        output.status.success(),
        "bare udoc <file> should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Alice"),
        "bare-file invocation should produce extracted text",
    );
}

#[test]
fn explicit_extract_matches_bare_file() {
    // `udoc extract <file>` is the explicit form; output must match
    // bare-file invocation byte-for-byte.
    let pdf = test_pdf();
    let bare = udoc_cmd().arg(&pdf).output().expect("bare udoc failed");
    let explicit = udoc_cmd()
        .arg("extract")
        .arg(&pdf)
        .output()
        .expect("udoc extract failed");
    assert!(bare.status.success());
    assert!(explicit.status.success());
    assert_eq!(
        bare.stdout, explicit.stdout,
        "explicit `extract` subcommand must produce identical output to bare-file shortcut",
    );
}

// ---------------------------------------------------------------------------
// `udoc fonts` subcommand.
// ---------------------------------------------------------------------------

#[test]
fn fonts_json_default() {
    let output = udoc_cmd()
        .arg("fonts")
        .arg(test_pdf())
        .output()
        .expect("failed to run udoc fonts");
    assert!(
        output.status.success(),
        "udoc fonts should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON: {e}\n{stdout}"));
    assert!(v["file"].is_string());
    assert!(v["pages"].as_u64().unwrap_or(0) > 0);
    let fonts = v["fonts"].as_array().expect("fonts is array");
    assert!(!fonts.is_empty(), "should list at least one font");
    // Each font entry must carry a FontResolution tag.
    let mut saw_known = false;
    for f in fonts {
        let status = f["resolution"]["status"].as_str().unwrap_or("");
        if matches!(status, "Exact" | "Substituted" | "SyntheticFallback") {
            saw_known = true;
        }
    }
    assert!(
        saw_known,
        "at least one font should have a known FontResolution tag",
    );
}

#[test]
fn fonts_text_format() {
    let output = udoc_cmd()
        .arg("fonts")
        .arg(test_pdf())
        .arg("--format")
        .arg("text")
        .output()
        .expect("failed to run udoc fonts --format text");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("File:"),
        "text output should begin with File: header, got: {stdout}",
    );
    assert!(
        stdout.contains("Resolution"),
        "text output should have a Resolution column header",
    );
}

// ---------------------------------------------------------------------------
// `udoc images` subcommand.
// ---------------------------------------------------------------------------

#[test]
fn images_json_lists_embedded_images() {
    let output = udoc_cmd()
        .arg("images")
        .arg(test_image_pdf())
        .output()
        .expect("failed to run udoc images");
    assert!(
        output.status.success(),
        "udoc images should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON: {e}\n{stdout}"));
    let images = v["images"].as_array().expect("images is array");
    assert!(
        !images.is_empty(),
        "image_xobject.pdf should expose at least one image",
    );
    // Each entry must carry the expected shape.
    for img in images {
        assert!(img["index"].as_u64().unwrap_or(0) > 0);
        assert!(img["filter"].is_string());
        assert!(img["width"].as_u64().is_some());
        assert!(img["height"].as_u64().is_some());
        assert!(img["bits_per_component"].as_u64().is_some());
        assert!(img["bytes"].as_u64().is_some());
    }
}

#[test]
fn images_extract_writes_files() {
    let tmp = temp_path("images-extract");
    let _ = std::fs::remove_dir_all(&tmp);

    let output = udoc_cmd()
        .arg("images")
        .arg(test_image_pdf())
        .arg("--extract")
        .arg(&tmp)
        .output()
        .expect("failed to run udoc images --extract");
    assert!(
        output.status.success(),
        "udoc images --extract should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    assert!(tmp.exists(), "extract dir {:?} should exist", tmp);
    let entries: Vec<_> = std::fs::read_dir(&tmp)
        .expect("should read extract dir")
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        !entries.is_empty(),
        "extract dir should contain dumped image files",
    );
    // Clean up.
    let _ = std::fs::remove_dir_all(&tmp);
}

// ---------------------------------------------------------------------------
// `udoc metadata` subcommand.
// ---------------------------------------------------------------------------

#[test]
fn metadata_json_default() {
    let output = udoc_cmd()
        .arg("metadata")
        .arg(test_info_pdf())
        .output()
        .expect("failed to run udoc metadata");
    assert!(
        output.status.success(),
        "udoc metadata should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON: {e}\n{stdout}"));
    assert!(v["file"].is_string(), "file should be a string");
    let md = &v["metadata"];
    assert!(md.is_object(), "metadata should be an object");
    // page_count is always populated post-extraction; it's the one
    // field we can rely on across any PDF.
    assert!(
        md["page_count"].as_u64().unwrap_or(0) >= 1,
        "metadata.page_count should be at least 1 on any valid PDF",
    );
}

#[test]
fn metadata_text_format() {
    let output = udoc_cmd()
        .arg("metadata")
        .arg(test_pdf())
        .arg("--format")
        .arg("text")
        .output()
        .expect("failed to run udoc metadata --format text");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("File:"),
        "text format should lead with File:, got: {stdout}",
    );
    assert!(
        stdout.contains("Pages:"),
        "text format should show Pages: line",
    );
}

// ---------------------------------------------------------------------------
// Shell completions (hidden subcommand).
// ---------------------------------------------------------------------------

#[test]
fn completions_bash_emits_script() {
    let output = udoc_cmd()
        .arg("completions")
        .arg("bash")
        .output()
        .expect("failed to run udoc completions bash");
    assert!(
        output.status.success(),
        "udoc completions bash should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Bash scripts emitted by clap_complete declare a shell function
    // named `_udoc` plus a `complete` binding. Checking for both avoids
    // coupling to the exact layout clap_complete uses.
    assert!(
        stdout.contains("_udoc"),
        "bash completion script should define a _udoc function, got: {stdout:.200}",
    );
    assert!(
        stdout.contains("complete"),
        "bash completion script should register a completion binding",
    );
}

#[test]
fn completions_zsh_emits_script() {
    let output = udoc_cmd()
        .arg("completions")
        .arg("zsh")
        .output()
        .expect("failed to run udoc completions zsh");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Zsh completions start with a `#compdef udoc` directive.
    assert!(
        stdout.contains("#compdef udoc"),
        "zsh completion script should start with #compdef udoc",
    );
}

// ---------------------------------------------------------------------------
// Help discoverability of the new subcommand tree.
// ---------------------------------------------------------------------------

#[test]
fn top_level_help_lists_new_subcommands() {
    let output = udoc_cmd()
        .arg("--help")
        .output()
        .expect("failed to run udoc --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for sub in &["extract", "render", "fonts", "images", "metadata"] {
        assert!(
            stdout.contains(sub),
            "top-level --help should mention `{sub}` subcommand, got:\n{stdout}",
        );
    }
    // `completions` is hidden; make sure it stays hidden.
    assert!(
        !stdout.contains("completions"),
        "completions subcommand should be hidden from top-level --help",
    );
}

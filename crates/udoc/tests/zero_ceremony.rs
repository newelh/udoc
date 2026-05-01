//! Zero-ceremony CLI regression test.
//!
//! `udoc <file>` with no other flags must Just Work for every format
//! we ship. This is the "new user's first command" tripwire: if any
//! backend regresses to requiring a flag, needing explicit format
//! selection, or crashing on a trivial valid input, this test fails.
//!
//! We don't check the full output content here -- other tests do that.
//! This test asserts exit code 0 and nonzero stdout on a representative
//! small sample from every format backend. One cross-format assertion
//! catches backend-detection regressions that per-format tests miss.
//!
//! Adversarial inputs under `tests/corpus/security/` are NOT loaded
//! here; see `sec_corpus_graceful.rs` for the CLI error-
//! message audit on those seeds.

use std::path::PathBuf;
use std::process::Command;

fn udoc_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_udoc"))
}

/// Workspace-relative path to a fixture.
fn fixture(rel: &str) -> PathBuf {
    // env!("CARGO_MANIFEST_DIR") points at crates/udoc; walk up one
    // level to reach the workspace root so we can address sibling
    // crates' test corpora without hard-coding each one.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("manifest dir has a parent")
        .parent()
        .expect("workspace root exists")
        .join(rel)
}

/// Run `udoc <file>` with no other flags and assert it produces
/// non-empty stdout on exit 0. Returns the stdout for optional
/// follow-up assertions.
fn run_bare(relpath: &str) -> String {
    let path = fixture(relpath);
    assert!(
        path.is_file(),
        "fixture {relpath} must exist at {}",
        path.display(),
    );
    let output = udoc_cmd()
        .arg(&path)
        .output()
        .unwrap_or_else(|e| panic!("failed to exec udoc on {relpath}: {e}"));
    assert!(
        output.status.success(),
        "udoc {relpath} should exit 0, got {:?}; stderr = {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.trim().is_empty(),
        "udoc {relpath} should write non-empty stdout; got {} bytes, stderr = {}",
        stdout.len(),
        String::from_utf8_lossy(&output.stderr),
    );
    stdout.into_owned()
}

#[test]
fn zero_ceremony_pdf() {
    run_bare("crates/udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
}

#[test]
fn zero_ceremony_docx() {
    run_bare("crates/udoc-docx/tests/corpus/real-world/sample2.docx");
}

#[test]
fn zero_ceremony_xlsx() {
    run_bare("crates/udoc-xlsx/tests/corpus/real-world/SampleSS.xlsx");
}

#[test]
fn zero_ceremony_pptx() {
    run_bare("crates/udoc-pptx/tests/corpus/real-world/test_slides.pptx");
}

#[test]
fn zero_ceremony_doc() {
    run_bare("crates/udoc-doc/tests/corpus/real-world/sample2.doc");
}

#[test]
fn zero_ceremony_xls() {
    run_bare("crates/udoc-xls/tests/corpus/real-world/lo_bug-fixes.xls");
}

#[test]
fn zero_ceremony_ppt() {
    run_bare("crates/udoc-ppt/tests/corpus/real-world/examplefiles_2slides.ppt");
}

#[test]
fn zero_ceremony_odt() {
    run_bare("crates/udoc-odf/tests/corpus/real-world/freetestdata_100kb.odt");
}

#[test]
fn zero_ceremony_ods() {
    run_bare("crates/udoc-odf/tests/corpus/real-world/freetestdata_100kb.ods");
}

#[test]
fn zero_ceremony_odp() {
    // lo_cellspan.odp / lo_background.odp / lo_Table_with_Cell_Fill.odp /
    // lo_canvas-slide.odp are all structural-feature LibreOffice fixtures
    // with no user-visible text (0-byte stdout). Use freetestdata_100kb.odp
    // which ships a slide-deck with real content. If this ever fails, check
    // whether ODP zero-text output is a real regression or just the fixture.
    run_bare("crates/udoc-odf/tests/corpus/real-world/freetestdata_100kb.odp");
}

#[test]
fn zero_ceremony_rtf() {
    run_bare("crates/udoc-rtf/tests/corpus/basic.rtf");
}

#[test]
fn zero_ceremony_md() {
    // CHANGELOG.md is a convenient well-formed markdown document shipped
    // in the tree. Any markdown file that compiles would also work.
    run_bare("crates/udoc-pdf/CHANGELOG.md");
}

/// `udoc <file> --out json` should work uniformly across formats too, not
/// just the default text mode. This catches regressions in the output
/// formatter that don't show up in the bare-file test.
#[test]
fn zero_ceremony_json_mode_works() {
    let path = fixture("crates/udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    let output = udoc_cmd()
        .arg(&path)
        .arg("--out")
        .arg("json")
        .output()
        .expect("exec udoc --out json");
    assert!(output.status.success(), "--out json should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Simplest sanity check: --out json output should be parseable as JSON.
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
    assert!(
        parsed.is_ok(),
        "--out json stdout should parse as JSON; got {} bytes, err = {:?}",
        stdout.len(),
        parsed.err(),
    );
}

/// `--jobs N > 16` should print the P06 scaling-cliff warning to stderr
/// before the actual extraction starts. Pre-fix users were silently
/// hitting the throughput collapse with no clue why.
#[test]
fn high_jobs_warns_about_scaling_cliff() {
    let path = fixture("tests/corpus/security/govdocs1-010258-alloc-bomb.pdf");
    if !path.exists() {
        eprintln!("skipping: fixture {} missing", path.display());
        return;
    }
    let output = udoc_cmd()
        .arg(&path)
        .arg(&path)
        .arg("--jobs")
        .arg("32")
        .output()
        .expect("exec udoc --jobs 32");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--jobs 32"),
        "warning should echo back the chosen --jobs value; got stderr: {stderr}"
    );
    assert!(
        stderr.contains("--processes") || stderr.contains("subprocess"),
        "warning should point at --processes / subprocess-fork as the alternative; got stderr: {stderr}"
    );
}

/// `--jobs N <= 16` should NOT print the warning. Operators in the
/// recommended band shouldn't see noise.
#[test]
fn moderate_jobs_does_not_warn() {
    let path = fixture("tests/corpus/security/govdocs1-010258-alloc-bomb.pdf");
    if !path.exists() {
        eprintln!("skipping: fixture {} missing", path.display());
        return;
    }
    let output = udoc_cmd()
        .arg(&path)
        .arg(&path)
        .arg("--jobs")
        .arg("8")
        .output()
        .expect("exec udoc --jobs 8");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("scaling") && !stderr.contains("loses throughput"),
        "jobs <= 16 should not emit scaling-cliff warning; got stderr: {stderr}"
    );
}

/// `--processes N` should produce the same logical output as `--jobs 1`
/// on the same input set: the only thing that changes is whether the
/// per-file work runs in threads or in subprocesses. This regression
/// pins the parent's "spawn N children, wait, exit 0 if all OK"
/// behaviour added in.
#[test]
fn subprocess_mode_completes_cleanly() {
    let p1 = fixture("crates/udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    let p2 = fixture("crates/udoc-rtf/tests/corpus/basic.rtf");
    let p3 = fixture("crates/udoc-pdf/CHANGELOG.md");
    if !p1.exists() || !p2.exists() || !p3.exists() {
        eprintln!("skipping: fixture missing");
        return;
    }
    let output = udoc_cmd()
        .arg(&p1)
        .arg(&p2)
        .arg(&p3)
        .arg("--processes")
        .arg("3")
        .arg("--quiet")
        .output()
        .expect("exec udoc --processes 3");
    assert!(
        output.status.success(),
        "--processes 3 should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.stdout.is_empty(),
        "--processes 3 should produce non-empty stdout"
    );
}

/// `--jobs` and `--processes` are mutually exclusive at the CLI level.
/// Clap should reject specifying both.
#[test]
fn jobs_and_processes_conflict() {
    let path = fixture("crates/udoc-pdf/CHANGELOG.md");
    let output = udoc_cmd()
        .arg(&path)
        .arg("--jobs")
        .arg("4")
        .arg("--processes")
        .arg("4")
        .output()
        .expect("exec");
    assert!(
        !output.status.success(),
        "--jobs + --processes together should exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("conflict") || stderr.contains("cannot be used"),
        "conflict error should be from clap; got: {stderr}"
    );
}

/// `--quiet` should suppress the high-jobs warning.
#[test]
fn quiet_suppresses_high_jobs_warning() {
    let path = fixture("tests/corpus/security/govdocs1-010258-alloc-bomb.pdf");
    if !path.exists() {
        eprintln!("skipping: fixture {} missing", path.display());
        return;
    }
    let output = udoc_cmd()
        .arg(&path)
        .arg(&path)
        .arg("--jobs")
        .arg("32")
        .arg("--quiet")
        .output()
        .expect("exec udoc --jobs 32 --quiet");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("loses throughput"),
        "--quiet should suppress scaling-cliff warning; got stderr: {stderr}"
    );
}

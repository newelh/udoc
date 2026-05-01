//! CLI golden snapshot suite.
//!
//! Per  §5.5 + /cli-audit.md G001-G080. The
//! regression gate for the locked CLI surface that landed in W2.
//!
//! Each test invokes `target/debug/udoc` (via `CARGO_BIN_EXE_udoc`)
//! as a subprocess, captures stdout / stderr / exit, applies the
//! determinism harness (path redaction, f64 rounding, optional JSON
//! key sort) and asserts against committed goldens under
//! `tests/cli_goldens/<id>.{stdout,stderr,exit}`.
//!
//! Refresh with `BLESS=1 cargo test --test cli_golden`. Without BLESS
//! the suite asserts.
//!
//! # Determinism handling (per cli-audit.md §4)
//!
//! 1. Absolute paths in stdout/stderr are redacted to `<INPUT>` (PDF
//!    fixture path) or `<TMPDIR>` (process-local temp dir) via regex.
//! 2. Float coordinates with >2 decimals are rounded to 2 (regex
//!    replace `(\d+)\.(\d{3,})` -> `\1.YY`).
//! 3. Per-test optional JSON normalisation: parse, sort keys, re-emit
//!    with stable f64 formatting.
//! 4. Stderr ordering is single-page-fixture-driven; multi-line stderr
//!    is line-sorted before comparison.
//! 5. `--errors json` `code` field assertions are exact; the
//!    `message`/`context` fields get path-redacted.
//!
//! # Fixtures
//!
//! Existing per-format corpus reused via path constants below; goldens
//! own one extra fixture (`malformed.pdf`, ~30 bytes) and one no-op
//! hook script (`echo-hook.sh`).

#![allow(clippy::needless_pass_by_value)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn goldens_dir() -> PathBuf {
    manifest_dir().join("tests/cli_goldens")
}

fn corpus_root() -> PathBuf {
    manifest_dir().join("..")
}

/// Reused fixtures from per-format test corpora. Keeps the goldens
/// crate small (1.4 MB across 12 formats vs. <2 KB committed here).
mod fx {
    use super::{corpus_root, manifest_dir, PathBuf};

    pub fn hello_pdf() -> PathBuf {
        corpus_root().join("udoc-pdf/tests/corpus/minimal/flate_content.pdf")
    }
    pub fn with_info_pdf() -> PathBuf {
        corpus_root().join("udoc-pdf/tests/corpus/minimal/with_info.pdf")
    }
    pub fn table_pdf() -> PathBuf {
        corpus_root().join("udoc-pdf/tests/corpus/minimal/table_layout.pdf")
    }
    pub fn image_pdf() -> PathBuf {
        corpus_root().join("udoc-pdf/tests/corpus/minimal/image_xobject.pdf")
    }
    pub fn multi_page_pdf() -> PathBuf {
        corpus_root().join("udoc-pdf/tests/corpus/realworld/multicolumn.pdf")
    }
    pub fn encrypted_pdf() -> PathBuf {
        corpus_root().join("udoc-pdf/tests/corpus/encrypted/rc4_128_user_password.pdf")
    }
    pub fn malformed_pdf() -> PathBuf {
        manifest_dir().join("tests/cli_goldens/inputs/pdfs/malformed.pdf")
    }
    pub fn echo_hook() -> PathBuf {
        manifest_dir().join("tests/cli_goldens/inputs/hooks/echo-hook.sh")
    }

    pub fn docx() -> PathBuf {
        corpus_root().join("udoc-docx/tests/corpus/real-world/pandoc_lists_compact.docx")
    }
    pub fn docx_image() -> PathBuf {
        corpus_root().join("udoc-docx/tests/corpus/real-world/python_docx_with_images.docx")
    }
    pub fn xlsx() -> PathBuf {
        corpus_root().join("udoc-xlsx/tests/corpus/real-world/InlineStrings.xlsx")
    }
    pub fn pptx() -> PathBuf {
        corpus_root().join("udoc-pptx/tests/corpus/real-world/minimal.pptx")
    }
    pub fn doc_file() -> PathBuf {
        corpus_root().join("udoc-doc/tests/corpus/real-world/footnote.doc")
    }
    pub fn xls() -> PathBuf {
        corpus_root().join("udoc-xls/tests/corpus/real-world/chinese_provinces.xls")
    }
    pub fn ppt() -> PathBuf {
        corpus_root().join("udoc-ppt/tests/corpus/real-world/with_textbox.ppt")
    }
    pub fn odt() -> PathBuf {
        corpus_root().join("udoc-odf/tests/corpus/real-world/synthetic_basic.odt")
    }
    pub fn ods() -> PathBuf {
        corpus_root().join("udoc-odf/tests/corpus/real-world/lo_border.ods")
    }
    pub fn odp() -> PathBuf {
        corpus_root().join("udoc-odf/tests/corpus/real-world/lo_canvas-slide.odp")
    }
    pub fn rtf() -> PathBuf {
        corpus_root().join("udoc-rtf/tests/corpus/basic.rtf")
    }
    pub fn md_file() -> PathBuf {
        corpus_root().join("udoc-markdown/tests/corpus/basic.md")
    }
}

// ---------------------------------------------------------------------------
// Subprocess helper
// ---------------------------------------------------------------------------

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_path(tag: &str) -> PathBuf {
    let id = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "udoc-cligolden-{}-{}-{}",
        tag,
        std::process::id(),
        id
    ))
}

fn udoc_cmd() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_udoc"));
    // Force a deterministic, non-tty output environment so tests don't
    // flip JSON / text default depending on host.
    cmd.env_remove("NO_COLOR");
    cmd.env_remove("TERM");
    cmd.env("UDOC_TEST_FORCE_TTY", "0");
    cmd
}

#[derive(Debug)]
struct Captured {
    stdout: String,
    stderr: String,
    exit: i32,
}

fn capture(args: &[&str]) -> Captured {
    let out = udoc_cmd().args(args).output().expect("failed to run udoc");
    capture_from(out)
}

fn capture_from(out: Output) -> Captured {
    Captured {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        exit: out.status.code().unwrap_or(-1),
    }
}

// ---------------------------------------------------------------------------
// Redaction helpers
// ---------------------------------------------------------------------------

/// Basic in-place find/replace for one substring -> placeholder. Used
/// to redact absolute fixture paths the test knows about up-front.
/// Substring forms: full path, parent dir, filename.
fn redact_path(text: &str, path: &Path, placeholder: &str) -> String {
    let mut out = text.to_string();
    let s = path.to_string_lossy().into_owned();
    out = out.replace(&s, placeholder);
    if let Some(parent) = path.parent() {
        let p = parent.to_string_lossy().into_owned();
        if !p.is_empty() {
            out = out.replace(&p, &format!("{placeholder}_DIR"));
        }
    }
    out
}

fn redact_tmp(text: &str) -> String {
    // Redact any /tmp/udoc-cligolden-* paths to <TMPDIR>/<TAG>.
    let tmp = std::env::temp_dir().to_string_lossy().into_owned();
    let mut out = text.to_string();
    while let Some(idx) = out.find(&tmp) {
        // Find end of the path: stop at whitespace, newline, ', "), ], or comma.
        let after = &out[idx..];
        let end_rel = after
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ')' | ']' | ','))
            .unwrap_or(after.len());
        let _ = &out[idx..idx + end_rel];
        out.replace_range(idx..idx + end_rel, "<TMPDIR>/<X>");
    }
    out
}

/// Round float literals with >=3 decimals down to 2 decimals so
/// platform-specific double rounding does not break goldens. Conservative
/// regex: `<digits>.<3+digits>` (no scientific notation, no negatives at
/// boundaries -- coordinate floats in udoc are positive and not exp-form).
fn round_floats(text: &str) -> String {
    // Hand-rolled to avoid pulling in regex dep. Walks bytes; when we see
    // a digit followed by '.' followed by 3+ digits, truncate to 2.
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_digit() {
            // Consume the integer prefix.
            let int_start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            // Optional fractional part with 3+ digits.
            if i + 3 < bytes.len() && bytes[i] == b'.' && bytes[i + 1].is_ascii_digit() {
                let frac_start = i + 1;
                let mut j = frac_start;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                let frac_len = j - frac_start;
                if frac_len >= 3 {
                    out.push_str(&text[int_start..i + 1]); // int + '.'
                    out.push_str(&text[frac_start..frac_start + 2]); // first 2
                    i = j;
                    continue;
                }
            }
            out.push_str(&text[int_start..i]);
        } else {
            out.push(c as char);
            i += 1;
        }
    }
    out
}

/// Sort the lines of `text` (used for stderr ordering determinism when
/// multiple warning lines may interleave).
fn sort_lines(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    lines.sort();
    let mut s = lines.join("\n");
    if text.ends_with('\n') {
        s.push('\n');
    }
    s
}

// ---------------------------------------------------------------------------
// assert_golden -- BLESS=1 aware
// ---------------------------------------------------------------------------

fn bless_enabled() -> bool {
    std::env::var("BLESS").is_ok_and(|v| !v.is_empty() && v != "0" && v != "false")
}

fn assert_golden(name: &str, suffix: &str, actual: &str) {
    let path = goldens_dir().join(format!("{name}.{suffix}"));
    if bless_enabled() {
        std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir -p goldens");
        std::fs::write(&path, actual).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("Golden file not found: {path:?}. Run with BLESS=1 to create."));
    if actual != expected {
        let mut diff = String::new();
        let exp_lines: Vec<&str> = expected.lines().collect();
        let act_lines: Vec<&str> = actual.lines().collect();
        let common = exp_lines.len().min(act_lines.len());
        for i in 0..common {
            if exp_lines[i] != act_lines[i] {
                diff.push_str(&format!("- {}\n+ {}\n", exp_lines[i], act_lines[i]));
            }
        }
        for line in exp_lines.iter().skip(common) {
            diff.push_str(&format!("- {line}\n"));
        }
        for line in act_lines.iter().skip(common) {
            diff.push_str(&format!("+ {line}\n"));
        }
        panic!(
            "Golden mismatch for {name}.{suffix}\n  expected: {path:?}\n  diff:\n{diff}\n  full actual:\n{actual}"
        );
    }
}

/// Per-test config for which streams we golden + how we redact.
#[derive(Default)]
struct GoldenSpec {
    /// Replace this fixture path with `<INPUT>` before comparison.
    input: Option<PathBuf>,
    /// Replace any /tmp/udoc-cligolden-* paths with `<TMPDIR>/<X>`.
    redact_tmp: bool,
    /// Round floats to 2 decimals.
    round_floats: bool,
    /// Sort stderr lines (for non-deterministic warning interleaving).
    sort_stderr: bool,
    /// If set, parse stdout as JSON, sort object keys, and re-emit pretty.
    canonicalize_json_stdout: bool,
    /// If set, drop stdout body entirely (just assert empty after
    /// redactions). For tests where stdout is binary or
    /// host-sensitive (PNGs, render output).
    skip_stdout: bool,
    /// If set, drop stderr body entirely (warnings vary by font set,
    /// host PDF parser tier, etc.). Use for tests where the assertion
    /// target is stdout + exit code only.
    skip_stderr: bool,
}

fn snapshot(name: &str, captured: &Captured, spec: GoldenSpec) {
    let mut stdout = captured.stdout.clone();
    let mut stderr = captured.stderr.clone();
    if let Some(p) = &spec.input {
        stdout = redact_path(&stdout, p, "<INPUT>");
        stderr = redact_path(&stderr, p, "<INPUT>");
    }
    if spec.redact_tmp {
        stdout = redact_tmp(&stdout);
        stderr = redact_tmp(&stderr);
    }
    if spec.round_floats {
        stdout = round_floats(&stdout);
        stderr = round_floats(&stderr);
    }
    if spec.canonicalize_json_stdout {
        stdout = canonicalize_json(&stdout);
    }
    if spec.sort_stderr {
        stderr = sort_lines(&stderr);
    }
    if !spec.skip_stdout {
        assert_golden(name, "stdout", &stdout);
    }
    if !spec.skip_stderr {
        assert_golden(name, "stderr", &stderr);
    }
    assert_golden(name, "exit", &format!("{}\n", captured.exit));
}

/// Parse-and-re-emit JSON with sorted keys, so map ordering does not
/// break goldens. Multi-line JSONL passes through line-by-line.
fn canonicalize_json(text: &str) -> String {
    if text.trim().is_empty() {
        return text.to_string();
    }
    // Try whole-text parse first (--out json output).
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
        let canon = canonicalize_value(v);
        return serde_json::to_string_pretty(&canon).unwrap_or_else(|_| text.to_string()) + "\n";
    }
    // Fall back to line-wise JSONL.
    let mut out = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            out.push('\n');
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(v) => {
                let canon = canonicalize_value(v);
                out.push_str(&serde_json::to_string(&canon).unwrap_or_else(|_| line.to_string()));
                out.push('\n');
            }
            Err(_) => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

fn canonicalize_value(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let mut sorted: std::collections::BTreeMap<String, serde_json::Value> =
                std::collections::BTreeMap::new();
            for (k, val) in map {
                sorted.insert(k, canonicalize_value(val));
            }
            let mut obj = serde_json::Map::new();
            for (k, val) in sorted {
                obj.insert(k, val);
            }
            serde_json::Value::Object(obj)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(canonicalize_value).collect())
        }
        other => other,
    }
}

// ===========================================================================
// G001 - G010: existing subcommands at canonical invocation
// ===========================================================================

#[test]
fn g001_extract_pdf_text() {
    let p = fx::hello_pdf();
    let cap = capture(&["extract", "--out", "text", "--quiet", p.to_str().unwrap()]);
    snapshot(
        "g001_extract_pdf_text",
        &cap,
        GoldenSpec {
            input: Some(p),
            ..Default::default()
        },
    );
}

#[test]
fn g002_render_pdf() {
    let p = fx::hello_pdf();
    let tmp = temp_path("render");
    let cap = capture(&["render", p.to_str().unwrap(), "-o", tmp.to_str().unwrap()]);
    // Render side-effect: stdout/stderr structure only; PNG bytes are
    // host-sensitive (font hinting), assert dir contents separately.
    assert_eq!(cap.exit, 0, "render exit: stderr={}", cap.stderr);
    let entries: Vec<String> = std::fs::read_dir(&tmp)
        .expect("read tmp")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        entries.iter().any(|n| n.starts_with("page-")),
        "expected page-N.png files, got {entries:?}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
    snapshot(
        "g002_render_pdf",
        &cap,
        GoldenSpec {
            input: Some(p),
            redact_tmp: true,
            skip_stderr: true,
            ..Default::default()
        },
    );
}

#[test]
fn g003_fonts_pdf() {
    let p = fx::hello_pdf();
    let cap = capture(&["fonts", p.to_str().unwrap()]);
    snapshot(
        "g003_fonts_pdf",
        &cap,
        GoldenSpec {
            input: Some(p),
            canonicalize_json_stdout: true,
            round_floats: true,
            skip_stderr: true,
            ..Default::default()
        },
    );
}

#[test]
fn g004_fonts_audit_pdf() {
    let p = fx::hello_pdf();
    let cap = capture(&["fonts", "--audit", p.to_str().unwrap()]);
    snapshot(
        "g004_fonts_audit_pdf",
        &cap,
        GoldenSpec {
            input: Some(p),
            canonicalize_json_stdout: true,
            round_floats: true,
            skip_stderr: true,
            ..Default::default()
        },
    );
}

#[test]
fn g005_images_pdf() {
    let p = fx::image_pdf();
    let cap = capture(&["images", p.to_str().unwrap()]);
    snapshot(
        "g005_images_pdf",
        &cap,
        GoldenSpec {
            input: Some(p),
            canonicalize_json_stdout: true,
            round_floats: true,
            skip_stderr: true,
            ..Default::default()
        },
    );
}

#[test]
fn g006_metadata_pdf() {
    let p = fx::with_info_pdf();
    let cap = capture(&["metadata", p.to_str().unwrap()]);
    snapshot(
        "g006_metadata_pdf",
        &cap,
        GoldenSpec {
            input: Some(p),
            canonicalize_json_stdout: true,
            skip_stderr: true,
            ..Default::default()
        },
    );
}

// Goldens for completions/help/features were captured under the default
// feature set. With `--features dev-tools` the completion/help text gains
// `render-diff` and `render-inspect`; with `--features cjk-fonts` the
// features report flips one row. Skip the byte-exact gate in those builds
// and assert structural shape only.
#[test]
fn g007_completions_bash() {
    let cap = capture(&["completions", "bash"]);
    if cfg!(feature = "dev-tools") {
        assert_eq!(cap.exit, 0);
        assert!(
            cap.stdout.contains("_udoc()"),
            "bash completion not emitted"
        );
        return;
    }
    snapshot("g007_completions_bash", &cap, GoldenSpec::default());
}

#[test]
fn g008_features() {
    let cap = capture(&["features"]);
    // The features report shifts one row per compile-time feature flag;
    // bless captures the default set and we only diff under that build.
    if cfg!(feature = "cjk-fonts") {
        assert_eq!(cap.exit, 0);
        assert!(cap.stdout.contains("compile-time features"));
        return;
    }
    snapshot("g008_features", &cap, GoldenSpec::default());
}

#[test]
fn g009_tables_xlsx() {
    let p = fx::xlsx();
    let cap = capture(&["tables", p.to_str().unwrap()]);
    snapshot(
        "g009_tables_xlsx",
        &cap,
        GoldenSpec {
            input: Some(p),
            skip_stderr: true,
            ..Default::default()
        },
    );
}

#[test]
fn g010_inspect_pdf() {
    let p = fx::hello_pdf();
    let cap = capture(&["inspect", p.to_str().unwrap()]);
    snapshot(
        "g010_inspect_pdf",
        &cap,
        GoldenSpec {
            input: Some(p),
            canonicalize_json_stdout: true,
            round_floats: true,
            skip_stderr: true,
            ..Default::default()
        },
    );
}

// ===========================================================================
// G011 - G013: inspect across diverse formats
// ===========================================================================

#[test]
fn g011_inspect_text_pdf() {
    let p = fx::multi_page_pdf();
    let cap = capture(&["inspect", p.to_str().unwrap()]);
    snapshot(
        "g011_inspect_text_pdf",
        &cap,
        GoldenSpec {
            input: Some(p),
            canonicalize_json_stdout: true,
            round_floats: true,
            skip_stderr: true,
            ..Default::default()
        },
    );
}

#[test]
fn g012_inspect_docx() {
    let p = fx::docx();
    let cap = capture(&["inspect", p.to_str().unwrap()]);
    snapshot(
        "g012_inspect_docx",
        &cap,
        GoldenSpec {
            input: Some(p),
            canonicalize_json_stdout: true,
            round_floats: true,
            skip_stderr: true,
            ..Default::default()
        },
    );
}

#[test]
fn g013_inspect_image_pdf() {
    // Image-only / no-text page used as the "scanned" proxy; the
    // realworld scanned corpus is not committed.
    let p = fx::image_pdf();
    let cap = capture(&["inspect", p.to_str().unwrap()]);
    snapshot(
        "g013_inspect_image_pdf",
        &cap,
        GoldenSpec {
            input: Some(p),
            canonicalize_json_stdout: true,
            round_floats: true,
            skip_stderr: true,
            ..Default::default()
        },
    );
}

// ===========================================================================
// G014 - G025: bare-file invocations on all 12 formats
// ===========================================================================

fn bare_file_test(name: &str, p: PathBuf) {
    let cap = capture(&["--out", "text", "--quiet", p.to_str().unwrap()]);
    snapshot(
        name,
        &cap,
        GoldenSpec {
            input: Some(p),
            ..Default::default()
        },
    );
}

#[test]
fn g014_bare_pdf() {
    bare_file_test("g014_bare_pdf", fx::hello_pdf());
}

#[test]
fn g015_bare_docx() {
    bare_file_test("g015_bare_docx", fx::docx());
}

#[test]
fn g016_bare_xlsx() {
    bare_file_test("g016_bare_xlsx", fx::xlsx());
}

#[test]
fn g017_bare_pptx() {
    bare_file_test("g017_bare_pptx", fx::pptx());
}

#[test]
fn g018_bare_doc() {
    bare_file_test("g018_bare_doc", fx::doc_file());
}

#[test]
fn g019_bare_xls() {
    bare_file_test("g019_bare_xls", fx::xls());
}

#[test]
fn g020_bare_ppt() {
    bare_file_test("g020_bare_ppt", fx::ppt());
}

#[test]
fn g021_bare_odt() {
    bare_file_test("g021_bare_odt", fx::odt());
}

#[test]
fn g022_bare_ods() {
    bare_file_test("g022_bare_ods", fx::ods());
}

#[test]
fn g023_bare_odp() {
    bare_file_test("g023_bare_odp", fx::odp());
}

#[test]
fn g024_bare_rtf() {
    bare_file_test("g024_bare_rtf", fx::rtf());
}

#[test]
fn g025_bare_md() {
    bare_file_test("g025_bare_md", fx::md_file());
}

// ===========================================================================
// G026 - G030: output modes on extract
// ===========================================================================

#[test]
fn g026_extract_out_text() {
    let p = fx::hello_pdf();
    let cap = capture(&["extract", "--out", "text", "--quiet", p.to_str().unwrap()]);
    snapshot(
        "g026_extract_out_text",
        &cap,
        GoldenSpec {
            input: Some(p),
            ..Default::default()
        },
    );
}

#[test]
fn g027_extract_out_json() {
    let p = fx::hello_pdf();
    let cap = capture(&["extract", "--out", "json", "--quiet", p.to_str().unwrap()]);
    snapshot(
        "g027_extract_out_json",
        &cap,
        GoldenSpec {
            input: Some(p),
            canonicalize_json_stdout: true,
            round_floats: true,
            ..Default::default()
        },
    );
}

#[test]
fn g028_extract_out_jsonl() {
    let p = fx::hello_pdf();
    let cap = capture(&["extract", "--out", "jsonl", "--quiet", p.to_str().unwrap()]);
    snapshot(
        "g028_extract_out_jsonl",
        &cap,
        GoldenSpec {
            input: Some(p),
            canonicalize_json_stdout: true,
            round_floats: true,
            ..Default::default()
        },
    );
}

#[test]
fn g029_extract_out_tsv() {
    let p = fx::table_pdf();
    let cap = capture(&["extract", "--out", "tsv", "--quiet", p.to_str().unwrap()]);
    snapshot(
        "g029_extract_out_tsv",
        &cap,
        GoldenSpec {
            input: Some(p),
            ..Default::default()
        },
    );
}

#[test]
fn g030_extract_out_markdown() {
    let p = fx::hello_pdf();
    let cap = capture(&[
        "extract",
        "--out",
        "markdown",
        "--quiet",
        p.to_str().unwrap(),
    ]);
    snapshot(
        "g030_extract_out_markdown",
        &cap,
        GoldenSpec {
            input: Some(p),
            round_floats: true,
            ..Default::default()
        },
    );
}

// ===========================================================================
// G031 - G034: --out chunks --chunk-by strategies
// ===========================================================================

fn chunks_test(name: &str, strategy: &str, extra: &[&str]) {
    let p = fx::table_pdf();
    let mut args: Vec<String> = vec![
        "extract".into(),
        "--out".into(),
        "chunks".into(),
        "--chunk-by".into(),
        strategy.into(),
        "--quiet".into(),
    ];
    for a in extra {
        args.push((*a).to_string());
    }
    args.push(p.to_str().unwrap().to_string());
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let cap = capture(&arg_refs);
    snapshot(
        name,
        &cap,
        GoldenSpec {
            input: Some(p),
            canonicalize_json_stdout: true,
            round_floats: true,
            ..Default::default()
        },
    );
}

#[test]
fn g031_chunks_by_page() {
    chunks_test("g031_chunks_by_page", "page", &[]);
}

#[test]
fn g032_chunks_by_heading() {
    chunks_test("g032_chunks_by_heading", "heading", &[]);
}

#[test]
fn g033_chunks_by_section() {
    chunks_test("g033_chunks_by_section", "section", &[]);
}

#[test]
fn g034_chunks_by_size() {
    chunks_test("g034_chunks_by_size", "size", &[]);
}

// ===========================================================================
// G035 - G037: filter combinations
// ===========================================================================

#[test]
fn g035_filter_pages() {
    let p = fx::multi_page_pdf();
    let cap = capture(&[
        "extract",
        "--pages",
        "1",
        "--out",
        "text",
        "--quiet",
        p.to_str().unwrap(),
    ]);
    snapshot(
        "g035_filter_pages",
        &cap,
        GoldenSpec {
            input: Some(p),
            ..Default::default()
        },
    );
}

#[test]
fn g036_filter_input_format() {
    let p = fx::hello_pdf();
    let cap = capture(&[
        "extract",
        "--input-format",
        "pdf",
        "--out",
        "text",
        "--quiet",
        p.to_str().unwrap(),
    ]);
    snapshot(
        "g036_filter_input_format",
        &cap,
        GoldenSpec {
            input: Some(p),
            ..Default::default()
        },
    );
}

#[test]
fn g037_filter_password() {
    let p = fx::encrypted_pdf();
    // RC4-128 with user password "test123"; per encrypted/generate.py:62.
    let cap = capture(&[
        "extract",
        "--password",
        "test123",
        "--out",
        "text",
        "--quiet",
        p.to_str().unwrap(),
    ]);
    snapshot(
        "g037_filter_password",
        &cap,
        GoldenSpec {
            input: Some(p),
            ..Default::default()
        },
    );
}

// ===========================================================================
// G038 - G042: hook flags (smoke; deterministic no-op echo hook)
// ===========================================================================

fn hook_test(name: &str, args_after_input: &[&str]) {
    let p = fx::hello_pdf();
    let h = fx::echo_hook();
    let mut args: Vec<String> = vec![
        "extract".into(),
        "--ocr".into(),
        h.to_string_lossy().into_owned(),
        "--out".into(),
        "text".into(),
        "--quiet".into(),
    ];
    for a in args_after_input {
        args.push((*a).to_string());
    }
    args.push(p.to_str().unwrap().to_string());
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let cap = capture(&arg_refs);
    snapshot(
        name,
        &cap,
        GoldenSpec {
            input: Some(p),
            redact_tmp: true,
            ..Default::default()
        },
    );
}

#[test]
fn g038_hook_ocr() {
    hook_test("g038_hook_ocr", &[]);
}

#[test]
fn g039_hook_post() {
    let p = fx::hello_pdf();
    let h = fx::echo_hook();
    let cap = capture(&[
        "extract",
        "--hook",
        h.to_str().unwrap(),
        "--out",
        "text",
        "--quiet",
        p.to_str().unwrap(),
    ]);
    snapshot(
        "g039_hook_post",
        &cap,
        GoldenSpec {
            input: Some(p),
            ..Default::default()
        },
    );
}

#[test]
fn g040_hook_ocr_all() {
    hook_test("g040_hook_ocr_all", &["--ocr-all"]);
}

#[test]
fn g041_hook_image_dir() {
    let p = fx::hello_pdf();
    let h = fx::echo_hook();
    let dir = temp_path("hookimg");
    std::fs::create_dir_all(&dir).expect("mkdir hookimg");
    let cap = capture(&[
        "extract",
        "--ocr",
        h.to_str().unwrap(),
        "--hook-image-dir",
        dir.to_str().unwrap(),
        "--out",
        "text",
        "--quiet",
        p.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_dir_all(&dir);
    snapshot(
        "g041_hook_image_dir",
        &cap,
        GoldenSpec {
            input: Some(p),
            redact_tmp: true,
            ..Default::default()
        },
    );
}

#[test]
fn g042_hook_timeout() {
    hook_test("g042_hook_timeout", &["--hook-timeout", "10"]);
}

// ===========================================================================
// G043 - G046: resource limits
// ===========================================================================

#[test]
fn g043_max_file_size() {
    let p = fx::hello_pdf();
    let cap = capture(&[
        "extract",
        "--max-file-size",
        "256mb",
        "--out",
        "text",
        "--quiet",
        p.to_str().unwrap(),
    ]);
    snapshot(
        "g043_max_file_size",
        &cap,
        GoldenSpec {
            input: Some(p),
            ..Default::default()
        },
    );
}

#[test]
fn g044_max_pages() {
    let p = fx::multi_page_pdf();
    let cap = capture(&[
        "extract",
        "--max-pages",
        "1",
        "--out",
        "text",
        "--quiet",
        p.to_str().unwrap(),
    ]);
    snapshot(
        "g044_max_pages",
        &cap,
        GoldenSpec {
            input: Some(p),
            ..Default::default()
        },
    );
}

#[test]
fn g045_no_tables() {
    let p = fx::table_pdf();
    let cap = capture(&[
        "extract",
        "--no-tables",
        "--out",
        "text",
        "--quiet",
        p.to_str().unwrap(),
    ]);
    snapshot(
        "g045_no_tables",
        &cap,
        GoldenSpec {
            input: Some(p),
            ..Default::default()
        },
    );
}

#[test]
fn g046_no_images() {
    let p = fx::image_pdf();
    let cap = capture(&[
        "extract",
        "--no-images",
        "--out",
        "text",
        "--quiet",
        p.to_str().unwrap(),
    ]);
    snapshot(
        "g046_no_images",
        &cap,
        GoldenSpec {
            input: Some(p),
            ..Default::default()
        },
    );
}

// ===========================================================================
// G047 - G050: output formatting
// ===========================================================================

#[test]
fn g047_no_presentation() {
    let p = fx::hello_pdf();
    let cap = capture(&[
        "extract",
        "--no-presentation",
        "--out",
        "json",
        "--quiet",
        p.to_str().unwrap(),
    ]);
    snapshot(
        "g047_no_presentation",
        &cap,
        GoldenSpec {
            input: Some(p),
            canonicalize_json_stdout: true,
            round_floats: true,
            ..Default::default()
        },
    );
}

#[test]
fn g048_raw_spans() {
    let p = fx::hello_pdf();
    let cap = capture(&[
        "extract",
        "--raw-spans",
        "--out",
        "json",
        "--quiet",
        p.to_str().unwrap(),
    ]);
    snapshot(
        "g048_raw_spans",
        &cap,
        GoldenSpec {
            input: Some(p),
            canonicalize_json_stdout: true,
            round_floats: true,
            ..Default::default()
        },
    );
}

#[test]
fn g049_pretty_json() {
    let p = fx::hello_pdf();
    let cap = capture(&[
        "extract",
        "--pretty",
        "--out",
        "json",
        "--quiet",
        p.to_str().unwrap(),
    ]);
    snapshot(
        "g049_pretty_json",
        &cap,
        GoldenSpec {
            input: Some(p),
            canonicalize_json_stdout: true,
            round_floats: true,
            ..Default::default()
        },
    );
}

#[test]
fn g050_output_file() {
    let p = fx::hello_pdf();
    let out = temp_path("outfile");
    let cap = capture(&[
        "extract",
        "--out",
        "text",
        "--quiet",
        "-o",
        out.to_str().unwrap(),
        p.to_str().unwrap(),
    ]);
    let body = std::fs::read_to_string(&out).unwrap_or_default();
    let _ = std::fs::remove_file(&out);
    assert!(
        body.contains("Hello"),
        "output file should contain text, got: {body:?}"
    );
    snapshot(
        "g050_output_file",
        &cap,
        GoldenSpec {
            input: Some(p),
            redact_tmp: true,
            ..Default::default()
        },
    );
}

// ===========================================================================
// G051 - G052: quiet / verbose
// ===========================================================================

#[test]
fn g051_quiet() {
    let p = fx::hello_pdf();
    let cap = capture(&["extract", "--quiet", "--out", "text", p.to_str().unwrap()]);
    snapshot(
        "g051_quiet",
        &cap,
        GoldenSpec {
            input: Some(p),
            ..Default::default()
        },
    );
}

#[test]
fn g052_default_warnings() {
    // No --verbose flag exists in the current CLI surface (per
    // cli-audit.md §2.2 it was proposed but not implemented in W2).
    // Fall back to default mode -- warnings appear on stderr by default.
    // We assert exit + stdout, drop stderr (font-set sensitive).
    let p = fx::hello_pdf();
    let cap = capture(&["extract", "--out", "text", p.to_str().unwrap()]);
    snapshot(
        "g052_default_warnings",
        &cap,
        GoldenSpec {
            input: Some(p),
            skip_stderr: true,
            ..Default::default()
        },
    );
}

// ===========================================================================
// G053 - G054: --errors json
// ===========================================================================

#[test]
fn g053_errors_json_valid() {
    let p = fx::hello_pdf();
    let cap = capture(&[
        "extract",
        "--errors",
        "json",
        "--out",
        "text",
        "--quiet",
        p.to_str().unwrap(),
    ]);
    snapshot(
        "g053_errors_json_valid",
        &cap,
        GoldenSpec {
            input: Some(p),
            ..Default::default()
        },
    );
}

#[test]
fn g054_errors_json_malformed() {
    let p = fx::malformed_pdf();
    let cap = capture(&["extract", "--errors", "json", p.to_str().unwrap()]);
    // Snapshot stderr through canonical-JSON pass so key order is stable.
    let mut stderr = cap.stderr.clone();
    stderr = redact_path(&stderr, &p, "<INPUT>");
    let stderr_canon = canonicalize_json(&stderr);
    // Verify the agent contract: code field is exactly E_PARSE_ERROR.
    assert!(
        stderr_canon.contains("\"E_PARSE_ERROR\""),
        "expected E_PARSE_ERROR code, got stderr:\n{stderr_canon}"
    );
    let cap2 = Captured {
        stdout: cap.stdout.clone(),
        stderr: stderr_canon,
        exit: cap.exit,
    };
    // Already redacted; tell snapshot to skip its own redaction.
    snapshot("g054_errors_json_malformed", &cap2, GoldenSpec::default());
}

// ===========================================================================
// G055 - G058: exit codes (0/1/2/3)
// ===========================================================================

#[test]
fn g055_exit_success() {
    let p = fx::hello_pdf();
    let cap = capture(&["extract", "--out", "text", "--quiet", p.to_str().unwrap()]);
    assert_eq!(cap.exit, 0);
    snapshot(
        "g055_exit_success",
        &cap,
        GoldenSpec {
            input: Some(p),
            ..Default::default()
        },
    );
}

#[test]
fn g056_exit_extraction_failure() {
    let p = fx::encrypted_pdf();
    let cap = capture(&["extract", "--out", "text", p.to_str().unwrap()]);
    assert_eq!(
        cap.exit, 1,
        "encrypted-no-password should exit 1, got {} stderr={}",
        cap.exit, cap.stderr
    );
    snapshot(
        "g056_exit_extraction_failure",
        &cap,
        GoldenSpec {
            input: Some(p),
            // stderr message includes path + variable detail; agent
            // contract is the exit code, so we drop stderr and rely
            // on the assertion above.
            skip_stderr: true,
            ..Default::default()
        },
    );
}

#[test]
fn g057_exit_usage_error() {
    let p = fx::hello_pdf();
    let cap = capture(&["extract", "--input-format", "mp4", p.to_str().unwrap()]);
    assert_eq!(cap.exit, 2, "invalid format should exit 2");
    snapshot(
        "g057_exit_usage_error",
        &cap,
        GoldenSpec {
            input: Some(p),
            skip_stderr: true,
            ..Default::default()
        },
    );
}

#[test]
fn g058_exit_file_not_found() {
    // Use a deterministic absent path; redact it so the golden does
    // not embed the test process pid.
    let p = std::env::temp_dir().join(format!("udoc-cligolden-missing-{}.pdf", std::process::id()));
    let cap = capture(&["extract", "--out", "text", p.to_str().unwrap()]);
    assert_eq!(cap.exit, 3, "missing file should exit 3");
    snapshot(
        "g058_exit_file_not_found",
        &cap,
        GoldenSpec {
            input: Some(p),
            redact_tmp: true,
            skip_stderr: true,
            ..Default::default()
        },
    );
}

// ===========================================================================
// G059: deprecated flag handling
// ===========================================================================

#[test]
fn g059_deprecated_format_flag() {
    let p = fx::hello_pdf();
    let cap = capture(&["extract", "--format", "pdf", p.to_str().unwrap()]);
    assert_eq!(cap.exit, 2, "--format should error rename");
    // stderr is clap's built-in error message; assert key substring.
    assert!(
        cap.stderr.contains("--format"),
        "stderr should mention the unknown flag, got: {}",
        cap.stderr
    );
    snapshot(
        "g059_deprecated_format_flag",
        &cap,
        GoldenSpec {
            input: Some(p),
            // clap formats its help output with the binary path
            // baked in; drop stderr from the golden body.
            skip_stderr: true,
            ..Default::default()
        },
    );
}

// ===========================================================================
// G060 - G061: dev-tools subcommands -- separate test binary (gated)
// ===========================================================================
// Lives in cli_golden_dev_tools.rs (created when feature is enabled).
// Skipped here.

// ===========================================================================
// G062 - G065: render subcommand variants
// ===========================================================================

fn render_test(name: &str, extra: &[&str], expected_pages: Option<usize>) {
    let p = fx::multi_page_pdf();
    let dir = temp_path(name);
    let mut args: Vec<String> = vec![
        "render".into(),
        p.to_str().unwrap().into(),
        "-o".into(),
        dir.to_str().unwrap().into(),
    ];
    for a in extra {
        args.push((*a).to_string());
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let cap = capture(&arg_refs);
    assert_eq!(cap.exit, 0, "render exit, stderr={}", cap.stderr);
    let entries: Vec<String> = std::fs::read_dir(&dir)
        .expect("read render dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".png"))
        .collect();
    if let Some(n) = expected_pages {
        assert_eq!(
            entries.len(),
            n,
            "render: expected {n} pngs, got {entries:?}"
        );
    } else {
        assert!(!entries.is_empty(), "render produced no PNGs");
    }
    // PNG magic check on first file
    let mut sorted_entries = entries.clone();
    sorted_entries.sort();
    let first = dir.join(&sorted_entries[0]);
    let bytes = std::fs::read(&first).expect("read png");
    assert_eq!(
        &bytes[..8],
        b"\x89PNG\r\n\x1a\n",
        "first render output not a PNG"
    );
    let _ = std::fs::remove_dir_all(&dir);
    snapshot(
        name,
        &cap,
        GoldenSpec {
            input: Some(p),
            redact_tmp: true,
            // PNG byte content is host-sensitive; we only golden the
            // CLI-visible streams.
            skip_stderr: true,
            ..Default::default()
        },
    );
}

#[test]
fn g062_render_default_dpi() {
    render_test("g062_render_default_dpi", &[], None);
}

#[test]
fn g063_render_dpi_150() {
    render_test("g063_render_dpi_150", &["--dpi", "150"], None);
}

#[test]
fn g064_render_dpi_300() {
    render_test("g064_render_dpi_300", &["--dpi", "300"], None);
}

#[test]
fn g065_render_pages_filter() {
    render_test("g065_render_pages_filter", &["--pages", "1-2"], Some(2));
}

// ===========================================================================
// G066 - G068: --help and --version
// ===========================================================================

#[test]
fn g066_top_level_help() {
    let cap = capture(&["--help"]);
    // The help text grows two extra subcommand rows under
    // `--features dev-tools`; the byte-exact gate only fires for the
    // default release surface.
    if cfg!(feature = "dev-tools") {
        assert_eq!(cap.exit, 0);
        assert!(
            cap.stdout.contains("Extract text") && cap.stdout.contains("extract"),
            "help missing canonical content"
        );
        return;
    }
    snapshot("g066_top_level_help", &cap, GoldenSpec::default());
}

#[test]
fn g067_version() {
    let cap = capture(&["--version"]);
    snapshot("g067_version", &cap, GoldenSpec::default());
}

#[test]
fn g068_extract_help() {
    let cap = capture(&["extract", "--help"]);
    snapshot("g068_extract_help", &cap, GoldenSpec::default());
}

// ===========================================================================
// G069 - G080: edge cases + extras
// ===========================================================================

#[test]
fn g069_inspect_full() {
    let p = fx::multi_page_pdf();
    let cap = capture(&["inspect", "--full", p.to_str().unwrap()]);
    snapshot(
        "g069_inspect_full",
        &cap,
        GoldenSpec {
            input: Some(p),
            canonicalize_json_stdout: true,
            round_floats: true,
            skip_stderr: true,
            ..Default::default()
        },
    );
}

#[test]
fn g070_tables_no_tables() {
    let p = fx::hello_pdf();
    let cap = capture(&["tables", p.to_str().unwrap()]);
    snapshot(
        "g070_tables_no_tables",
        &cap,
        GoldenSpec {
            input: Some(p),
            skip_stderr: true,
            ..Default::default()
        },
    );
}

#[test]
fn g071_images_docx() {
    let p = fx::docx_image();
    let cap = capture(&["images", p.to_str().unwrap()]);
    snapshot(
        "g071_images_docx",
        &cap,
        GoldenSpec {
            input: Some(p),
            canonicalize_json_stdout: true,
            round_floats: true,
            skip_stderr: true,
            ..Default::default()
        },
    );
}

#[test]
fn g072_metadata_docx() {
    let p = fx::docx();
    let cap = capture(&["metadata", p.to_str().unwrap()]);
    snapshot(
        "g072_metadata_docx",
        &cap,
        GoldenSpec {
            input: Some(p),
            canonicalize_json_stdout: true,
            skip_stderr: true,
            ..Default::default()
        },
    );
}

#[test]
fn g073_fonts_trace() {
    let p = fx::hello_pdf();
    let cap = capture(&["fonts", "--trace", p.to_str().unwrap()]);
    snapshot(
        "g073_fonts_trace",
        &cap,
        GoldenSpec {
            input: Some(p),
            canonicalize_json_stdout: true,
            round_floats: true,
            skip_stderr: true,
            ..Default::default()
        },
    );
}

#[test]
fn g074_images_extract_dir() {
    let p = fx::image_pdf();
    let dir = temp_path("imgex");
    std::fs::create_dir_all(&dir).expect("mkdir imgex");
    let cap = capture(&[
        "images",
        "--extract",
        dir.to_str().unwrap(),
        p.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_dir_all(&dir);
    snapshot(
        "g074_images_extract_dir",
        &cap,
        GoldenSpec {
            input: Some(p),
            redact_tmp: true,
            canonicalize_json_stdout: true,
            round_floats: true,
            skip_stderr: true,
            ..Default::default()
        },
    );
}

#[test]
fn g075_stdin_pipe() {
    use std::io::Write;
    let p = fx::hello_pdf();
    let bytes = std::fs::read(&p).expect("read pdf");
    let mut child = udoc_cmd()
        .arg("--out")
        .arg("text")
        .arg("--quiet")
        .arg("--input-format")
        .arg("pdf")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn udoc");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(&bytes)
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    let cap = capture_from(out);
    snapshot("g075_stdin_pipe", &cap, GoldenSpec::default());
}

#[test]
fn g076_combined_flags() {
    let p = fx::hello_pdf();
    let out = temp_path("combo");
    let cap = capture(&[
        "extract",
        "--out",
        "json",
        "--pretty",
        "-o",
        out.to_str().unwrap(),
        "--quiet",
        p.to_str().unwrap(),
    ]);
    let body = std::fs::read_to_string(&out).unwrap_or_default();
    let _ = std::fs::remove_file(&out);
    assert!(
        body.lines().count() > 1,
        "pretty JSON file should be multiline, got: {body}"
    );
    snapshot(
        "g076_combined_flags",
        &cap,
        GoldenSpec {
            input: Some(p),
            redact_tmp: true,
            ..Default::default()
        },
    );
}

#[test]
fn g077_render_encrypted_no_password() {
    let p = fx::encrypted_pdf();
    let dir = temp_path("encrendr");
    let cap = capture(&["render", p.to_str().unwrap(), "-o", dir.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_ne!(cap.exit, 0, "render encrypted with no password should fail");
    snapshot(
        "g077_render_encrypted_no_password",
        &cap,
        GoldenSpec {
            input: Some(p),
            redact_tmp: true,
            skip_stderr: true,
            ..Default::default()
        },
    );
}

#[test]
fn g078_inspect_encrypted() {
    let p = fx::encrypted_pdf();
    let cap = capture(&["inspect", p.to_str().unwrap()]);
    // inspect on encrypted PDF surfaces has_encryption=true; if the
    // inspect path errors out on encrypted docs we still want the
    // shape captured.
    snapshot(
        "g078_inspect_encrypted",
        &cap,
        GoldenSpec {
            input: Some(p),
            canonicalize_json_stdout: true,
            round_floats: true,
            skip_stderr: true,
            ..Default::default()
        },
    );
}

#[test]
fn g079_chunks_size_custom() {
    let p = fx::table_pdf();
    let cap = capture(&[
        "extract",
        "--out",
        "chunks",
        "--chunk-by",
        "size",
        "--chunk-size",
        "500",
        "--quiet",
        p.to_str().unwrap(),
    ]);
    snapshot(
        "g079_chunks_size_custom",
        &cap,
        GoldenSpec {
            input: Some(p),
            canonicalize_json_stdout: true,
            round_floats: true,
            ..Default::default()
        },
    );
}

#[test]
fn g080_completions_zsh() {
    let cap = capture(&["completions", "zsh"]);
    if cfg!(feature = "dev-tools") {
        assert_eq!(cap.exit, 0);
        assert!(
            cap.stdout.contains("#compdef udoc"),
            "zsh completion not emitted"
        );
        return;
    }
    snapshot("g080_completions_zsh", &cap, GoldenSpec::default());
}

// ===========================================================================
// Friction-deepdive: per-subcommand --help (additional 6 to round out
// the audit's "9 per-subcommand" target since we already covered the
// top-level help G066 + extract help G068).
// ===========================================================================

fn help_test(name: &str, sub: &str) {
    let cap = capture(&[sub, "--help"]);
    snapshot(name, &cap, GoldenSpec::default());
}

#[test]
fn g081_help_render() {
    help_test("g081_help_render", "render");
}

#[test]
fn g082_help_tables() {
    help_test("g082_help_tables", "tables");
}

#[test]
fn g083_help_images() {
    help_test("g083_help_images", "images");
}

#[test]
fn g084_help_metadata() {
    help_test("g084_help_metadata", "metadata");
}

#[test]
fn g085_help_fonts() {
    help_test("g085_help_fonts", "fonts");
}

#[test]
fn g086_help_inspect() {
    help_test("g086_help_inspect", "inspect");
}

#[test]
fn g087_help_features() {
    help_test("g087_help_features", "features");
}

// ===========================================================================
// G088: inspect perf fixture ( W0-100PAGE-FIXTURE)
// ===========================================================================
//
// Closes  AC #6 caveat: the largest local PDF was 30 pages, so the
// 500ms p95 budget on a 100-page PDF was deferred to . This test
// asserts the structural shape of `udoc inspect` against a synthetic
// 100-page fixture (sample_size=5 by construction, has_text on every
// sampled page).
//
// Wall-time budget (500ms p95) is a MANUAL perf check, not enforced
// here. CI runners vary too much. To verify locally:
//
//   time cargo test --test cli_golden g088_inspect_100page_fixture -- --nocapture
//
// or:
//
//   time target/debug/udoc inspect tests/corpus/inspect-perf/100page.pdf
//
// On a typical dev box the run completes in well under 100ms.
//
// Fixture is generated by `cargo test --test generate_inspect_fixture
// -- --ignored` and committed at `tests/corpus/inspect-perf/100page.pdf`.

#[test]
fn g088_inspect_100page_fixture() {
    let p = corpus_root().join("../tests/corpus/inspect-perf/100page.pdf");
    assert!(
        p.exists(),
        "100-page inspect fixture missing at {p:?}; regenerate with \
         `cargo test -p udoc-pdf --test generate_inspect_fixture -- --ignored`"
    );

    let cap = capture(&["inspect", p.to_str().unwrap()]);
    assert_eq!(cap.exit, 0, "inspect failed: stderr=\n{}", cap.stderr);

    let report: serde_json::Value = serde_json::from_str(&cap.stdout)
        .unwrap_or_else(|e| panic!("inspect stdout was not valid JSON: {e}\n{}", cap.stdout));

    assert_eq!(report["format"], "pdf");
    assert_eq!(report["page_count"], 100);
    assert_eq!(report["sampled"], true);
    assert_eq!(report["sample_size"], 5);
    assert_eq!(report["has_text"], true);
    assert_eq!(report["has_encryption"], false);
    // Sampling spread for n=100 is locked to [0, 49, 50, 51, 99]; if
    // sampled_indices() ever changes shape, this regression-fences it.
    assert_eq!(
        report["sample_pages"],
        serde_json::json!([0, 49, 50, 51, 99])
    );
}

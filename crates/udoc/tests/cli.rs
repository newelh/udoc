//! T-027: CLI integration tests.
//!
//! Runs the udoc binary via std::process::Command and verifies behavior.
//! Uses committed minimal corpus PDFs so all tests run on every checkout.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static CLI_TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

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

// ---------------------------------------------------------------------------
// Basic text extraction
// ---------------------------------------------------------------------------

#[test]
fn basic_text_extraction() {
    let output = udoc_cmd()
        .arg(test_pdf())
        .output()
        .expect("failed to run udoc");
    assert!(output.status.success(), "udoc should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Alice"), "should contain table data");
}

// ---------------------------------------------------------------------------
// JSON output
// ---------------------------------------------------------------------------

#[test]
fn json_output() {
    let output = udoc_cmd()
        .arg(test_pdf())
        .arg("--out")
        .arg("json")
        .output()
        .expect("failed to run udoc");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"version\":1"),
        "JSON output should contain version:1"
    );
}

// ---------------------------------------------------------------------------
// JSONL output
// ---------------------------------------------------------------------------

#[test]
fn jsonl_output() {
    let output = udoc_cmd()
        .arg(test_pdf())
        .arg("--out")
        .arg("jsonl")
        .output()
        .expect("failed to run udoc");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines.len() >= 2, "JSONL should have at least header+footer");

    // First line should be the header
    let header: serde_json::Value = serde_json::from_str(lines[0]).expect("valid JSON header");
    assert_eq!(header["udoc"], "header");
}

// ---------------------------------------------------------------------------
// Tables output (TSV)
// ---------------------------------------------------------------------------

#[test]
fn tables_output() {
    let output = udoc_cmd()
        .arg(test_pdf())
        .arg("--out")
        .arg("tsv")
        .output()
        .expect("failed to run udoc");
    assert!(output.status.success(), "tables mode should not error");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Name"),
        "table output should contain table header"
    );
}

// ---------------------------------------------------------------------------
// Page filtering
// ---------------------------------------------------------------------------

#[test]
fn page_filtering() {
    let output = udoc_cmd()
        .arg(test_pdf())
        .arg("--pages")
        .arg("1")
        .output()
        .expect("failed to run udoc");
    assert!(output.status.success(), "page filtering should work");
    assert!(!output.stdout.is_empty(), "page 1 should produce output");
}

// ---------------------------------------------------------------------------
// Quiet mode
// ---------------------------------------------------------------------------

#[test]
fn quiet_mode() {
    let output = udoc_cmd()
        .arg(test_pdf())
        .arg("-q")
        .output()
        .expect("failed to run udoc");
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("warning:"),
        "quiet mode should suppress warnings"
    );
}

// ---------------------------------------------------------------------------
// Exit codes
// ---------------------------------------------------------------------------

#[test]
fn exit_code_success() {
    let output = udoc_cmd()
        .arg(test_pdf())
        .output()
        .expect("failed to run udoc");
    assert_eq!(
        output.status.code(),
        Some(0),
        "successful extraction should exit 0"
    );
}

#[test]
fn exit_code_nonexistent_file() {
    let output = udoc_cmd()
        .arg("/nonexistent/file.pdf")
        .output()
        .expect("failed to run udoc");
    assert_ne!(
        output.status.code(),
        Some(0),
        "nonexistent file should exit non-zero"
    );
}

#[test]
fn exit_code_invalid_args() {
    // --out only takes a known value enum; bogus value is a usage error.
    let output = udoc_cmd()
        .arg("--out")
        .arg("not-a-mode")
        .arg("dummy.pdf")
        .output()
        .expect("failed to run udoc");
    assert_ne!(
        output.status.code(),
        Some(0),
        "invalid --out value should exit non-zero"
    );
}

// ---------------------------------------------------------------------------
// Help flag
// ---------------------------------------------------------------------------

#[test]
fn help_flag() {
    let output = udoc_cmd()
        .arg("--help")
        .output()
        .expect("failed to run udoc");
    assert!(output.status.success(), "--help should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Extract text"),
        "--help should describe the tool"
    );
}

// ---------------------------------------------------------------------------
// Version flag
// ---------------------------------------------------------------------------

#[test]
fn version_flag() {
    let output = udoc_cmd()
        .arg("--version")
        .output()
        .expect("failed to run udoc");
    assert!(output.status.success(), "--version should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("udoc"),
        "--version output should contain 'udoc'"
    );
}

// ---------------------------------------------------------------------------
// Stdin with bad data
// ---------------------------------------------------------------------------

#[test]
fn stdin_bad_format() {
    let mut child = udoc_cmd()
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn udoc");

    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin.write_all(b"not a pdf").expect("write");
    }

    let output = child.wait_with_output().expect("wait");
    assert_ne!(
        output.status.code(),
        Some(0),
        "bad stdin data should exit non-zero"
    );
}

// ---------------------------------------------------------------------------
// Format override
// ---------------------------------------------------------------------------

#[test]
fn format_override() {
    let output = udoc_cmd()
        .arg("--input-format")
        .arg("pdf")
        .arg(test_pdf())
        .output()
        .expect("failed to run udoc");
    assert!(
        output.status.success(),
        "--input-format pdf should work on a PDF file"
    );
}

// ---------------------------------------------------------------------------
// Pretty JSON
// ---------------------------------------------------------------------------

#[test]
fn pretty_json() {
    let output = udoc_cmd()
        .arg(test_pdf())
        .arg("--out")
        .arg("json")
        .arg("--pretty")
        .output()
        .expect("failed to run udoc");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.lines().count() > 1,
        "pretty JSON should be multi-line"
    );
}

// ---------------------------------------------------------------------------
// No presentation
// ---------------------------------------------------------------------------

#[test]
fn no_presentation() {
    let output = udoc_cmd()
        .arg(test_pdf())
        .arg("--out")
        .arg("json")
        .arg("--no-presentation")
        .output()
        .expect("failed to run udoc");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("\"presentation\""),
        "--no-presentation should omit presentation key"
    );
}

// ---------------------------------------------------------------------------
// Image extraction
// ---------------------------------------------------------------------------

#[test]
fn images_output_mode() {
    // dropped the --images extract flag in favor of the
    // `udoc images <file> --extract <dir>` subcommand. This test now drives
    // the subcommand instead.
    let id = CLI_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("udoc-test-images-{}-{}", std::process::id(), id));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create temp dir");
    let output = udoc_cmd()
        .arg("images")
        .arg(test_image_pdf())
        .arg("--extract")
        .arg(&tmp)
        .output()
        .expect("failed to run udoc images");
    assert!(
        output.status.success(),
        "udoc images --extract should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify image files were actually created.
    assert!(
        tmp.exists(),
        "image directory should have been created at {:?}",
        tmp
    );
    let entries: Vec<_> = std::fs::read_dir(&tmp)
        .expect("should read image dir")
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        !entries.is_empty(),
        "image directory should contain extracted files"
    );

    // Clean up
    let _ = std::fs::remove_dir_all(&tmp);
}

// ---------------------------------------------------------------------------
// Hook flag
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn hook_flag() {
    use std::io::Write;

    let id = CLI_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp =
        std::env::temp_dir().join(format!("udoc-test-hook-cli-{}-{}", std::process::id(), id));
    std::fs::create_dir_all(&tmp).expect("create temp dir");

    // Create a minimal hook that adds a label to metadata
    let script_path = tmp.join("test-hook.sh");
    let mut f = std::fs::File::create(&script_path).expect("create script");
    writeln!(
        f,
        r#"#!/bin/bash
echo '{{"protocol":"udoc-hook-v1","capabilities":["annotate"],"needs":["text"],"provides":["labels"]}}'
while IFS= read -r line; do
    echo '{{"labels":{{"cli_hook_ran":"true"}}}}'
done
"#
    )
    .expect("write script");
    drop(f);

    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&script_path)
        .status()
        .expect("chmod");

    let output = udoc_cmd()
        .arg(test_pdf())
        .arg("--out")
        .arg("json")
        .arg("--hook")
        .arg(&script_path)
        .output()
        .expect("failed to run udoc with --hook");

    assert!(
        output.status.success(),
        "udoc --hook should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // The hook adds a label, which appears in metadata.properties
    assert!(
        stdout.contains("cli_hook_ran"),
        "--hook should have run the hook and added labels to output"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

// ---------------------------------------------------------------------------
// Batch mode (multiple files)
// ---------------------------------------------------------------------------

#[test]
fn batch_mode_multiple_files() {
    let pdf = test_pdf();
    let output = udoc_cmd()
        .arg(&pdf)
        .arg(&pdf)
        .output()
        .expect("failed to run udoc");
    assert!(output.status.success(), "batch mode should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Two copies of the same file should produce the text twice.
    let alice_count = stdout.matches("Alice").count();
    assert!(
        alice_count >= 2,
        "batch mode should extract both files, got {alice_count} occurrences of 'Alice'"
    );
}

#[test]
fn batch_mode_one_bad_file_continues() {
    let pdf = test_pdf();
    let output = udoc_cmd()
        .arg("/nonexistent/file.pdf")
        .arg(&pdf)
        .output()
        .expect("failed to run udoc");
    // Should exit non-zero because one file failed.
    assert_ne!(
        output.status.code(),
        Some(0),
        "batch with one failure should exit non-zero"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The good file should still have been extracted.
    assert!(
        stdout.contains("Alice"),
        "good file should still be extracted despite earlier failure"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nonexistent"),
        "stderr should mention the failed file"
    );
}

#[test]
fn batch_mode_json_output() {
    let pdf = test_pdf();
    let output = udoc_cmd()
        .arg(&pdf)
        .arg(&pdf)
        .arg("--out")
        .arg("json")
        .output()
        .expect("failed to run udoc");
    assert!(output.status.success(), "batch JSON should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Each file produces a JSON document with "version":1.
    let version_count = stdout.matches("\"version\":1").count();
    assert!(
        version_count >= 2,
        "batch JSON should produce output for each file, got {version_count} version markers"
    );
}

// ---------------------------------------------------------------------------
// Parallel batch mode (--jobs)
// ---------------------------------------------------------------------------

#[test]
fn parallel_batch_matches_sequential() {
    let pdf = test_pdf();

    // Run sequential (default --jobs 1)
    let seq_output = udoc_cmd()
        .arg(&pdf)
        .arg(&pdf)
        .arg(&pdf)
        .arg(&pdf)
        .output()
        .expect("failed to run udoc sequential");
    assert!(
        seq_output.status.success(),
        "sequential batch should exit 0"
    );

    // Run parallel with --jobs 2
    let par_output = udoc_cmd()
        .arg(&pdf)
        .arg(&pdf)
        .arg(&pdf)
        .arg(&pdf)
        .arg("--jobs")
        .arg("2")
        .output()
        .expect("failed to run udoc parallel");
    assert!(par_output.status.success(), "parallel batch should exit 0");

    // Output should be byte-identical.
    assert_eq!(
        seq_output.stdout, par_output.stdout,
        "parallel output should match sequential output"
    );
}

#[test]
fn parallel_batch_json_matches_sequential() {
    let pdf = test_pdf();

    let seq_output = udoc_cmd()
        .arg(&pdf)
        .arg(&pdf)
        .arg(&pdf)
        .arg(&pdf)
        .arg("--out")
        .arg("json")
        .output()
        .expect("failed to run udoc sequential");
    assert!(seq_output.status.success());

    let par_output = udoc_cmd()
        .arg(&pdf)
        .arg(&pdf)
        .arg(&pdf)
        .arg(&pdf)
        .arg("--out")
        .arg("json")
        .arg("--jobs")
        .arg("4")
        .output()
        .expect("failed to run udoc parallel");
    assert!(par_output.status.success());

    assert_eq!(
        seq_output.stdout, par_output.stdout,
        "parallel JSON output should match sequential"
    );
}

#[test]
fn parallel_batch_one_bad_file_continues() {
    let pdf = test_pdf();
    let output = udoc_cmd()
        .arg("/nonexistent/file.pdf")
        .arg(&pdf)
        .arg(&pdf)
        .arg("--jobs")
        .arg("2")
        .output()
        .expect("failed to run udoc");
    // Should exit non-zero because one file failed.
    assert_ne!(
        output.status.code(),
        Some(0),
        "parallel batch with one failure should exit non-zero"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The good files should still have been extracted.
    let alice_count = stdout.matches("Alice").count();
    assert!(
        alice_count >= 2,
        "good files should still be extracted despite one failure, got {alice_count} occurrences"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nonexistent"),
        "stderr should mention the failed file"
    );
}

#[test]
fn parallel_batch_single_file_no_threads() {
    // --jobs 4 with only one file should still work (falls back to sequential).
    let pdf = test_pdf();
    let output = udoc_cmd()
        .arg(&pdf)
        .arg("--jobs")
        .arg("4")
        .output()
        .expect("failed to run udoc");
    assert!(
        output.status.success(),
        "--jobs with single file should exit 0"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Alice"),
        "single file with --jobs should still produce output"
    );
}

// ---------------------------------------------------------------------------
// --no-tables and --no-images flags
// ---------------------------------------------------------------------------

#[test]
fn no_tables_flag() {
    let output = udoc_cmd()
        .arg(test_pdf())
        .arg("--no-tables")
        .output()
        .expect("failed to run udoc");
    assert!(
        output.status.success(),
        "--no-tables should not error, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Should still produce text output (just without table detection).
    assert!(
        !output.stdout.is_empty(),
        "--no-tables should still produce output"
    );
}

#[test]
fn no_images_flag() {
    let output = udoc_cmd()
        .arg(test_image_pdf())
        .arg("--no-images")
        .output()
        .expect("failed to run udoc");
    assert!(
        output.status.success(),
        "--no-images should not error, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ---------------------------------------------------------------------------
// --max-file-size flag
// ---------------------------------------------------------------------------

#[test]
fn max_file_size_flag_rejects() {
    let output = udoc_cmd()
        .arg(test_pdf())
        .arg("--max-file-size")
        .arg("10b")
        .output()
        .expect("failed to run udoc");
    assert_ne!(
        output.status.code(),
        Some(0),
        "--max-file-size 10b should reject any real PDF"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("exceeds"),
        "error should mention size limit, stderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// audit-fonts subcommand (M-32c)
// ---------------------------------------------------------------------------

#[test]
fn audit_fonts_json_default() {
    // table_layout.pdf references /Helvetica (a standard-14 font) without
    // embedding it, so the font loader routes it to the bundled standard-14
    // equivalent and reports FontResolution::Substituted.
    let output = udoc_cmd()
        .arg("fonts")
        .arg("--audit")
        .arg(test_pdf())
        .output()
        .expect("failed to run udoc audit-fonts");
    assert!(
        output.status.success(),
        "audit-fonts should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON: {e}\n{stdout}"));

    // Top-level schema
    assert!(v["file"].is_string(), "file should be a string");
    assert!(v["pages"].as_u64().is_some(), "pages should be an integer");
    assert!(v["fonts"].is_array(), "fonts should be an array");
    assert!(v["summary"].is_object(), "summary should be an object");

    // Summary sanity checks
    let total_fonts = v["summary"]["total_fonts"]
        .as_u64()
        .expect("total_fonts u64");
    assert!(total_fonts > 0, "should find at least one font");
    let spans_total = v["summary"]["spans_total"]
        .as_u64()
        .expect("spans_total u64");
    assert!(spans_total > 0, "should have at least one span");
    assert!(
        v["summary"]["fallback_ratio"].as_f64().is_some(),
        "fallback_ratio should be a float",
    );

    // Each font entry should have the expected shape with a valid
    // resolution status.
    let fonts = v["fonts"].as_array().expect("fonts array");
    assert!(!fonts.is_empty());
    let mut saw_known_status = false;
    for font in fonts {
        assert!(font["referenced_name"].is_string());
        assert!(font["spans"].as_u64().unwrap_or(0) > 0);
        assert!(font["pages"].is_array());
        assert!(font["sample_chars"].is_string());
        let status = font["resolution"]["status"]
            .as_str()
            .expect("resolution.status");
        if matches!(status, "Exact" | "Substituted" | "SyntheticFallback") {
            saw_known_status = true;
        }
    }
    assert!(
        saw_known_status,
        "at least one font should have a known resolution status"
    );
}

#[test]
fn audit_fonts_text_format() {
    let output = udoc_cmd()
        .arg("fonts")
        .arg("--audit")
        .arg(test_pdf())
        .arg("--format")
        .arg("text")
        .output()
        .expect("failed to run udoc audit-fonts --format text");
    assert!(output.status.success(), "text-format audit should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("File:"),
        "text output should have File: header"
    );
    assert!(
        stdout.contains("Fonts:"),
        "text output should have a Fonts summary line"
    );
    assert!(
        stdout.contains("Resolution"),
        "text output should have a column header"
    );
}

#[test]
fn audit_fonts_writes_to_output_file() {
    let id = CLI_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!(
        "udoc-audit-fonts-{}-{}.json",
        std::process::id(),
        id
    ));
    let _ = std::fs::remove_file(&tmp);

    let output = udoc_cmd()
        .arg("fonts")
        .arg("--audit")
        .arg(test_pdf())
        .arg("--output")
        .arg(&tmp)
        .output()
        .expect("failed to run udoc audit-fonts -o");
    assert!(
        output.status.success(),
        "audit-fonts -o should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "with -o, stdout should be empty");
    let body = std::fs::read_to_string(&tmp).expect("output file should exist");
    let v: serde_json::Value =
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("invalid JSON: {e}"));
    assert!(v["summary"]["total_fonts"].as_u64().unwrap_or(0) > 0);
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn audit_fonts_reports_missing_glyphs_in_json() {
    // ArabicCIDTrueType.pdf embeds CID TrueType fonts whose ToUnicode
    // maps some chars to Arabic codepoints. `udoc audit-fonts` must
    // surface the `missing_glyphs` section of the JSON report so callers
    // can see where rendering would have produced .notdef boxes. See
    // issue #166.
    //
    // Since bundled Noto Sans Arabic (route_by_unicode
    // dispatches U+0600..06FF, U+0750..077F, U+FB50..FDFF, U+FE70..FEFF
    // to the Tier 2 Arabic face), this particular doc no longer exhausts
    // the fallback chain on Arabic glyphs. The test now asserts the JSON
    // schema is well-formed regardless of whether the missing_glyphs
    // array is populated. Schema-only assertions still cover #166's
    // promise: the section exists and entries (when present) have the
    // documented shape. A separate, non-Arabic regression doc would be
    // needed to re-pin "must be non-empty" -- punted to a follow-up.
    let pdf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../udoc-pdf/tests/corpus/minimal/ArabicCIDTrueType.pdf");
    let output = udoc_cmd()
        .arg("fonts")
        .arg("--audit")
        .arg(&pdf)
        .output()
        .expect("failed to run udoc audit-fonts on ArabicCIDTrueType.pdf");
    assert!(
        output.status.success(),
        "audit-fonts should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON: {e}\n{stdout}"));

    // Schema: missing_glyphs array + summary counters must exist.
    let missing = v["missing_glyphs"]
        .as_array()
        .expect("missing_glyphs should be an array");

    // The two summary counters must be consistent with the array shape.
    let pairs = v["summary"]["missing_glyph_pairs"].as_u64().unwrap_or(0);
    let total = v["summary"]["missing_glyph_total"].as_u64().unwrap_or(0);
    assert_eq!(
        pairs as usize,
        missing.len(),
        "summary.missing_glyph_pairs ({pairs}) must equal missing_glyphs.len() ({})",
        missing.len(),
    );
    assert!(
        (missing.is_empty() && total == 0) || (!missing.is_empty() && total > 0),
        "summary.missing_glyph_total ({total}) must be positive iff missing_glyphs non-empty",
    );

    // Each entry must carry the expected shape (vacuously true on empty).
    for m in missing {
        assert!(m["font"].is_string(), "missing_glyphs[].font string");
        assert!(
            m["codepoint"].as_u64().is_some(),
            "missing_glyphs[].codepoint u64",
        );
        let hex = m["codepoint_hex"]
            .as_str()
            .expect("missing_glyphs[].codepoint_hex string");
        assert!(
            hex.starts_with("U+"),
            "codepoint_hex must be prefixed 'U+', got: {hex}",
        );
        assert!(
            m["glyph_id"].as_u64().is_some(),
            "missing_glyphs[].glyph_id u64",
        );
        assert!(
            m["count"].as_u64().unwrap_or(0) > 0,
            "missing_glyphs[].count must be positive",
        );
    }

    // Sorted by count descending: the first entry has count >= the last.
    if missing.len() > 1 {
        let first_count = missing[0]["count"].as_u64().unwrap_or(0);
        let last_count = missing[missing.len() - 1]["count"].as_u64().unwrap_or(0);
        assert!(
            first_count >= last_count,
            "missing_glyphs should be sorted by count desc ({first_count} vs {last_count})",
        );
    }
}

#[test]
fn audit_fonts_nonexistent_file_exits_nonzero() {
    let output = udoc_cmd()
        .arg("fonts")
        .arg("--audit")
        .arg("/nonexistent/audit.pdf")
        .output()
        .expect("failed to run udoc audit-fonts");
    assert_ne!(
        output.status.code(),
        Some(0),
        "audit-fonts on missing file should exit non-zero"
    );
}

#[test]
fn audit_fonts_bad_format_exits_nonzero() {
    let output = udoc_cmd()
        .arg("fonts")
        .arg("--audit")
        .arg(test_pdf())
        .arg("--format")
        .arg("xml")
        .output()
        .expect("failed to run udoc audit-fonts");
    assert_ne!(
        output.status.code(),
        Some(0),
        "audit-fonts with unknown format should exit non-zero"
    );
}

// ---------------------------------------------------------------------------
// JSON pages field populated
// ---------------------------------------------------------------------------

#[test]
fn json_has_page_dimensions() {
    let output = udoc_cmd()
        .arg(test_pdf())
        .arg("--out")
        .arg("json")
        .output()
        .expect("failed to run udoc");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let pages = doc["presentation"]["pages"]
        .as_array()
        .expect("pages should be an array");
    assert!(!pages.is_empty(), "pages array should not be empty");
    let page0 = &pages[0];
    assert!(
        page0["width"].as_f64().unwrap_or(0.0) > 0.0,
        "page width should be positive"
    );
    assert!(
        page0["height"].as_f64().unwrap_or(0.0) > 0.0,
        "page height should be positive"
    );
}

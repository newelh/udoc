//! Golden-file tests for facade output formatters.
//!
//! Each test extracts a PDF via `udoc::extract()`, serializes through a
//! facade output formatter, and compares against a `.expected` file.
//! Run with `BLESS=1` to create or update golden files.

use std::path::PathBuf;

use udoc_core::test_harness::unified_diff;

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

fn corpus_pdf(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../udoc-pdf/tests/corpus/minimal")
        .join(name)
}

fn assert_golden(golden_name: &str, actual: &str) {
    let golden = golden_path(golden_name);

    let is_bless = std::env::var("BLESS")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));

    let expected = match std::fs::read_to_string(&golden) {
        Ok(s) => s,
        Err(_) if is_bless => {
            std::fs::write(&golden, actual).unwrap_or_else(|e| {
                panic!("failed to create golden file {}: {e}", golden.display())
            });
            eprintln!("Created golden file: {}", golden.display());
            return;
        }
        Err(e) => {
            panic!(
                "failed to read golden file {}: {e}\n\
                 Hint: run with BLESS=1 to create it.",
                golden.display()
            )
        }
    };

    // Normalize: trim trailing whitespace per line, trim trailing newlines
    let normalize = |s: &str| -> String {
        s.lines()
            .map(|l| l.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_string()
    };

    let actual_norm = normalize(actual);
    let expected_norm = normalize(&expected);

    if actual_norm != expected_norm {
        if is_bless {
            std::fs::write(&golden, actual).unwrap_or_else(|e| {
                panic!("failed to bless golden file {}: {e}", golden.display())
            });
            eprintln!("Blessed golden file: {}", golden.display());
            return;
        }

        let actual_lines: Vec<&str> = actual_norm.lines().collect();
        let expected_lines: Vec<&str> = expected_norm.lines().collect();
        let diff = unified_diff(&expected_lines, &actual_lines);

        panic!(
            "Golden file mismatch for {golden_name}\n\
             Golden file: {}\n\
             Differences:\n{diff}\n\
             --- EXPECTED ---\n{expected_norm}\n\
             --- ACTUAL ---\n{actual_norm}\n\
             ---\n\
             To update: run with BLESS=1 or copy actual output to {}",
            golden.display(),
            golden.display(),
        );
    }
}

#[test]
fn golden_json() {
    let pdf = corpus_pdf("table_layout.pdf");
    let doc = udoc::extract(&pdf).unwrap_or_else(|e| panic!("extract failed: {e}"));

    let mut buf = Vec::new();
    udoc::output::json::write_json(&doc, &mut buf, false, false, false).expect("write_json failed");
    let actual = String::from_utf8(buf).expect("non-UTF-8 JSON output");

    assert_golden("table_layout.json.expected", &actual);
}

#[test]
fn golden_jsonl() {
    let pdf = corpus_pdf("table_layout.pdf");
    let doc = udoc::extract(&pdf).unwrap_or_else(|e| panic!("extract failed: {e}"));

    let page_assignments = doc.presentation.as_ref().map(|p| &p.page_assignments);
    let mut buf = Vec::new();
    udoc::output::jsonl::write_jsonl(&doc, "PDF", &mut buf, page_assignments, 0)
        .expect("write_jsonl failed");
    let actual = String::from_utf8(buf).expect("non-UTF-8 JSONL output");

    assert_golden("table_layout.jsonl.expected", &actual);
}

#[test]
fn golden_text() {
    let pdf = corpus_pdf("table_layout.pdf");
    let doc = udoc::extract(&pdf).unwrap_or_else(|e| panic!("extract failed: {e}"));

    let mut buf = Vec::new();
    udoc::output::text::write_text(&doc, &mut buf).expect("write_text failed");
    let actual = String::from_utf8(buf).expect("non-UTF-8 text output");

    assert_golden("table_layout.text.expected", &actual);
}

// -- tables output for table_layout.pdf --

#[test]
fn golden_tables() {
    let pdf = corpus_pdf("table_layout.pdf");
    let config = udoc::Config::new().layers(udoc::LayerConfig::content_only());
    let doc = udoc::extract_with(&pdf, config).unwrap_or_else(|e| panic!("extract failed: {e}"));

    let page_assignments = doc.presentation.as_ref().map(|p| &p.page_assignments);
    let mut buf = Vec::new();
    udoc::output::tables::write_tables(&doc, &mut buf, page_assignments)
        .expect("write_tables failed");
    let actual = String::from_utf8(buf).expect("non-UTF-8 tables output");

    assert_golden("table_layout.tables.expected", &actual);
}

// -- multipage.pdf: text, json, jsonl --

#[test]
fn golden_multipage_text() {
    let pdf = corpus_pdf("multipage.pdf");
    let config = udoc::Config::new().layers(udoc::LayerConfig::content_only());
    let doc = udoc::extract_with(&pdf, config).unwrap_or_else(|e| panic!("extract failed: {e}"));

    let mut buf = Vec::new();
    udoc::output::text::write_text(&doc, &mut buf).expect("write_text failed");
    let actual = String::from_utf8(buf).expect("non-UTF-8 text output");

    assert_golden("multipage.text.expected", &actual);
}

#[test]
fn golden_multipage_json() {
    let pdf = corpus_pdf("multipage.pdf");
    let config = udoc::Config::new().layers(udoc::LayerConfig::content_only());
    let doc = udoc::extract_with(&pdf, config).unwrap_or_else(|e| panic!("extract failed: {e}"));

    let mut buf = Vec::new();
    udoc::output::json::write_json(&doc, &mut buf, false, false, false).expect("write_json failed");
    let actual = String::from_utf8(buf).expect("non-UTF-8 JSON output");

    assert_golden("multipage.json.expected", &actual);
}

#[test]
fn golden_multipage_jsonl() {
    let pdf = corpus_pdf("multipage.pdf");
    let config = udoc::Config::new().layers(udoc::LayerConfig::content_only());
    let doc = udoc::extract_with(&pdf, config).unwrap_or_else(|e| panic!("extract failed: {e}"));

    let page_assignments = doc.presentation.as_ref().map(|p| &p.page_assignments);
    let mut buf = Vec::new();
    udoc::output::jsonl::write_jsonl(&doc, "PDF", &mut buf, page_assignments, 0)
        .expect("write_jsonl failed");
    let actual = String::from_utf8(buf).expect("non-UTF-8 JSONL output");

    assert_golden("multipage.jsonl.expected", &actual);
}

// -- invisible_text.pdf: verify invisible text is excluded --

#[test]
fn golden_invisible_text() {
    let pdf = corpus_pdf("invisible_text.pdf");
    let config = udoc::Config::new().layers(udoc::LayerConfig::content_only());
    let doc = udoc::extract_with(&pdf, config).unwrap_or_else(|e| panic!("extract failed: {e}"));

    let mut buf = Vec::new();
    udoc::output::text::write_text(&doc, &mut buf).expect("write_text failed");
    let actual = String::from_utf8(buf).expect("non-UTF-8 text output");

    assert_golden("invisible_text.text.expected", &actual);
}

//! Real-world corpus integration tests for XLSX extraction.
//!
//! Runs udoc-xlsx against the real-world test corpus collected by
//! `tools/xlsx-corpus-builder/collect_bulk.py` and compares extracted
//! text against openpyxl ground truth from `ground_truth.py`.
//!
//! Run with: cargo test -p udoc-xlsx --test realworld_corpus_test -- --nocapture

use std::fs;
use std::path::{Path, PathBuf};

use udoc_core::backend::{FormatBackend, PageExtractor};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Normalize text for comparison: trim trailing whitespace per line,
/// trim trailing empty lines, normalize line endings.
fn normalize(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n");
    let mut lines: Vec<&str> = normalized.split('\n').map(|l| l.trim_end()).collect();
    while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    lines.join("\n")
}

/// Walk a directory tree for .xlsx files.
fn find_xlsx_files(dir: &PathBuf) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if !dir.exists() {
        return files;
    }
    for entry in walkdir(dir) {
        if entry.extension().map(|e| e == "xlsx").unwrap_or(false) {
            files.push(entry);
        }
    }
    files.sort();
    files
}

/// Simple recursive directory walker (no external dep).
fn walkdir(dir: &PathBuf) -> Vec<PathBuf> {
    let mut result = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                result.extend(walkdir(&path));
            } else {
                result.push(path);
            }
        }
    }
    result
}

/// Find the ground truth directory for a given corpus file.
/// Maps: tests/corpus-xlsx/downloaded/{source}/{file}.xlsx
///   ->  tests/ground-truth-xlsx/openpyxl/{source}/{file}_manifest.txt
fn find_gt_dir(xlsx_path: &Path, corpus_dir: &Path, gt_base: &Path) -> Option<PathBuf> {
    let rel = xlsx_path.strip_prefix(corpus_dir).ok()?;
    let parent = rel.parent()?;
    Some(gt_base.join(parent))
}

#[test]
fn realworld_corpus_text_extraction() {
    let root = workspace_root();
    let corpus_dir = root.join("tests/corpus-xlsx/downloaded");
    let gt_base = root.join("tests/ground-truth-xlsx/openpyxl");

    if !corpus_dir.exists() {
        eprintln!(
            "Real-world corpus not found at {}.\n\
             Run: cd tools/xlsx-corpus-builder && uv run python collect_bulk.py clone-repos",
            corpus_dir.display()
        );
        return;
    }

    if !gt_base.exists() {
        eprintln!(
            "Ground truth not found at {}.\n\
             Run: cd tools/xlsx-corpus-builder && uv run python ground_truth.py generate",
            gt_base.display()
        );
        return;
    }

    let xlsx_files = find_xlsx_files(&corpus_dir);
    if xlsx_files.is_empty() {
        eprintln!("No XLSX files found in {}", corpus_dir.display());
        return;
    }

    let total = xlsx_files.len();
    let mut passed = 0;
    let mut failed = 0;
    let mut errors = 0;
    let mut no_gt = 0;
    let mut failures: Vec<(String, String)> = Vec::new();

    for (i, xlsx_path) in xlsx_files.iter().enumerate() {
        let stem = xlsx_path.file_stem().unwrap().to_string_lossy();
        let gt_dir = match find_gt_dir(xlsx_path, &corpus_dir, &gt_base) {
            Some(d) => d,
            None => {
                no_gt += 1;
                continue;
            }
        };

        let manifest_path = gt_dir.join(format!("{stem}_manifest.txt"));
        if !manifest_path.exists() {
            no_gt += 1;
            continue;
        }

        // Open the XLSX
        let mut doc = match udoc_xlsx::XlsxDocument::open(xlsx_path) {
            Ok(d) => d,
            Err(e) => {
                errors += 1;
                failures.push((stem.to_string(), format!("OPEN ERROR: {e}")));
                continue;
            }
        };

        let page_count = FormatBackend::page_count(&doc);

        let manifest = fs::read_to_string(&manifest_path).unwrap();
        let expected_sheets = manifest
            .lines()
            .next()
            .unwrap_or("0")
            .parse::<usize>()
            .unwrap_or(0);

        if page_count != expected_sheets {
            failed += 1;
            failures.push((
                stem.to_string(),
                format!("PAGE COUNT: expected {expected_sheets}, got {page_count}"),
            ));
            continue;
        }

        let mut file_ok = true;
        for sheet_idx in 0..page_count {
            let gt_file = gt_dir.join(format!("{stem}_sheet{sheet_idx}.txt"));
            if !gt_file.exists() {
                // Missing per-sheet GT, skip
                no_gt += 1;
                file_ok = false;
                break;
            }

            let expected = fs::read_to_string(&gt_file).unwrap();
            let expected_norm = normalize(&expected);

            let actual = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut page =
                    FormatBackend::page(&mut doc, sheet_idx).expect("failed to open page");
                page.text().expect("failed to extract text")
            })) {
                Ok(t) => t,
                Err(_) => {
                    errors += 1;
                    failures.push((stem.to_string(), format!("PANIC on sheet {sheet_idx}")));
                    file_ok = false;
                    break;
                }
            };
            let actual_norm = normalize(&actual);

            if actual_norm != expected_norm {
                failed += 1;
                let exp_lines: Vec<&str> = expected_norm.lines().collect();
                let act_lines: Vec<&str> = actual_norm.lines().collect();
                let mut diff_msg = format!("MISMATCH sheet {sheet_idx}:");

                for (line_no, (e, a)) in exp_lines.iter().zip(act_lines.iter()).enumerate() {
                    if e != a {
                        diff_msg.push_str(&format!(
                            "\n  line {}: expected {:?}\n  line {}:      got {:?}",
                            line_no, e, line_no, a
                        ));
                        break;
                    }
                }
                if exp_lines.len() != act_lines.len() {
                    diff_msg.push_str(&format!(
                        "\n  line count: expected {}, got {}",
                        exp_lines.len(),
                        act_lines.len()
                    ));
                }

                failures.push((stem.to_string(), diff_msg));
                file_ok = false;
                break;
            }
        }

        if file_ok {
            passed += 1;
        }

        if (i + 1) % 100 == 0 {
            eprintln!(
                "  Progress: {}/{} files ({} passed, {} failed, {} errors, {} no GT)",
                i + 1,
                total,
                passed,
                failed,
                errors,
                no_gt
            );
        }
    }

    let tested = passed + failed + errors;
    eprintln!("\n=== XLSX Real-World Corpus Test Results ===");
    eprintln!("Total files: {total}");
    eprintln!("Tested:  {tested}");
    eprintln!("Passed:  {passed}");
    eprintln!("Failed:  {failed}");
    eprintln!("Errors:  {errors}");
    eprintln!("No GT:   {no_gt}");

    if !failures.is_empty() {
        let show = failures.len().min(50);
        eprintln!("\nFirst {show} failures:");
        for (name, msg) in &failures[..show] {
            eprintln!("  {name}: {msg}");
        }
    }

    if tested > 0 {
        let pass_rate = (passed as f64 / tested as f64) * 100.0;
        eprintln!("\nPass rate: {pass_rate:.1}% ({passed}/{tested})");

        // Real-world files are messier, start with a lower bar
        // and raise it as we fix issues
        assert!(
            pass_rate >= 50.0,
            "Pass rate {pass_rate:.1}% is below 50% threshold ({passed}/{tested})"
        );
    }
}

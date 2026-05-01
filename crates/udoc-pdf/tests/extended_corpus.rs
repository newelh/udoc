//! Extended corpus tests: run against large external PDF test suites.
//!
//! These tests are gated behind the UDOC_EXTENDED_CORPUS environment variable
//! because the corpus must be downloaded separately (see download-extended-corpus.sh).
//!
//! Run with: UDOC_EXTENDED_CORPUS=1 cargo test extended

use std::path::Path;
use std::sync::Arc;
use udoc_pdf::parse::DocumentParser;
use udoc_pdf::{CollectingDiagnostics, Config, Document};

const EXTENDED_DIR: &str = "tests/corpus/extended";

fn is_extended_corpus_available() -> bool {
    std::env::var("UDOC_EXTENDED_CORPUS").is_ok() && Path::new(EXTENDED_DIR).exists()
}

fn find_pdfs(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut pdfs = Vec::new();
    if !dir.exists() {
        return pdfs;
    }
    fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().and_then(|e| e.to_str()) == Some("pdf") {
                    out.push(path);
                }
            }
        }
    }
    walk(dir, &mut pdfs);
    pdfs.sort();
    pdfs
}

/// Structure test: parse header, xref, and trailer for all extended corpus PDFs.
/// We don't assert specific behavior since these are third-party files, but we
/// do assert no panics and count success/failure rates.
#[test]
fn extended_corpus_structure_test() {
    if !is_extended_corpus_available() {
        eprintln!(
            "Skipping extended corpus tests (set UDOC_EXTENDED_CORPUS=1 and run download script)"
        );
        return;
    }

    let pdfs = find_pdfs(Path::new(EXTENDED_DIR));
    if pdfs.is_empty() {
        eprintln!("No PDFs found in {EXTENDED_DIR}");
        return;
    }

    let mut success = 0usize;
    let mut failure = 0usize;
    let mut warnings_total = 0usize;

    for path in &pdfs {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(_) => {
                failure += 1;
                continue;
            }
        };

        let diag = Arc::new(CollectingDiagnostics::new());
        match DocumentParser::with_diagnostics(&data, diag.clone()).parse() {
            Ok(doc) => {
                success += 1;
                warnings_total += diag.warnings().len();
                // Basic sanity: xref shouldn't be empty
                if doc.xref.is_empty() {
                    eprintln!("WARNING: {} parsed but has empty xref", path.display());
                }
            }
            Err(_) => {
                failure += 1;
            }
        }
    }

    eprintln!(
        "Extended corpus: {}/{} parsed successfully ({} warnings total, {} failures)",
        success,
        pdfs.len(),
        warnings_total,
        failure
    );

    // We don't assert a specific success rate since external PDFs may be
    // intentionally malformed (test inputs for other parsers).
    assert!(
        success > 0,
        "extended corpus present but zero PDFs parsed successfully"
    );
}

// ---------------------------------------------------------------------------
// V-001/V-002: Full-pipeline validation with failure categorization
// CR-001/CR-002/CR-003: Corpus reclassification with per-category metrics
// ---------------------------------------------------------------------------

/// Failure category for extended corpus classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureKind {
    /// PDF could not be opened at all (structure/xref/trailer error)
    Parse,
    /// PDF opened but page() or text() failed
    TextExtraction,
    /// File could not be read from disk
    Io,
}

/// CR-001: Source-based classification for corpus PDFs.
///
/// Classification is by source directory, not by PDF quality:
/// - **PdfjsSuite**: PDFs from the pdf.js test suite. Mostly real-world
///   documents, but also contains some intentionally broken test fixtures.
/// - **PdfiumSuite**: PDFs from the pdfium test suite. Mostly intentionally
///   malformed test inputs, but also contains some real-world regression PDFs.
///
/// Per-file classification would require manual tagging of 2400+ PDFs, so
/// source-based grouping is the pragmatic choice. The thresholds account for
/// the mixed nature of each suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CorpusCategory {
    /// pdf.js test suite (mostly real-world documents, some test fixtures)
    PdfjsSuite,
    /// pdfium test suite (mostly malformed test inputs, some real-world regressions)
    PdfiumSuite,
}

impl std::fmt::Display for CorpusCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CorpusCategory::PdfjsSuite => write!(f, "pdf.js suite"),
            CorpusCategory::PdfiumSuite => write!(f, "pdfium suite"),
        }
    }
}

/// Classify a PDF based on its source directory.
///
/// This is a coarse classification by directory name, not by PDF quality.
/// The pdf.js suite contains mostly real-world PDFs but also some test
/// fixtures. The pdfium suite contains mostly malformed test inputs but
/// also some real-world regression PDFs. Per-file classification would
/// require manual tagging of 2400+ PDFs.
fn classify_pdf(path: &Path) -> CorpusCategory {
    let path_str = path.to_string_lossy();
    if path_str.contains("pdfjs") {
        CorpusCategory::PdfjsSuite
    } else {
        // pdfium or anything else defaults to pdfium suite
        CorpusCategory::PdfiumSuite
    }
}

/// Per-category success/failure tracking for CR-002.
#[derive(Default)]
struct CategoryStats {
    success_with_text: usize,
    success_empty: usize,
    failures: usize,
}

impl CategoryStats {
    fn total(&self) -> usize {
        self.success_with_text + self.success_empty + self.failures
    }

    fn success_total(&self) -> usize {
        self.success_with_text + self.success_empty
    }

    fn success_rate(&self) -> f64 {
        let total = self.total();
        if total == 0 {
            return 0.0;
        }
        self.success_total() as f64 / total as f64
    }
}

struct CorpusResult {
    path: String,
    kind: FailureKind,
    category: CorpusCategory,
    error: String,
}

/// Full-pipeline test: run Document::from_bytes -> page -> text on all extended
/// corpus PDFs. Categorizes failures and reports statistics.
///
/// This is V-001 (run content interpreter on all PDFs) and V-002 (categorize
/// failures) from .
#[test]
fn extended_corpus_full_pipeline() {
    if !is_extended_corpus_available() {
        eprintln!(
            "Skipping extended corpus full pipeline (set UDOC_EXTENDED_CORPUS=1 and run download script)"
        );
        return;
    }

    let pdfs = find_pdfs(Path::new(EXTENDED_DIR));
    if pdfs.is_empty() {
        eprintln!("No PDFs found in {EXTENDED_DIR}");
        return;
    }

    let mut failures: Vec<CorpusResult> = Vec::new();
    let mut warnings_total = 0usize;
    let mut pages_total = 0usize;

    // CR-001/CR-002: Per-category tracking
    let mut pdfjs_stats = CategoryStats::default();
    let mut pdfium_stats = CategoryStats::default();

    for path in &pdfs {
        let category = classify_pdf(path);
        let stats = match category {
            CorpusCategory::PdfjsSuite => &mut pdfjs_stats,
            CorpusCategory::PdfiumSuite => &mut pdfium_stats,
        };

        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                stats.failures += 1;
                failures.push(CorpusResult {
                    path: path.display().to_string(),
                    kind: FailureKind::Io,
                    category,
                    error: e.to_string(),
                });
                continue;
            }
        };

        let diag = Arc::new(CollectingDiagnostics::new());
        let config = Config::default().with_diagnostics(diag.clone());

        let mut doc = match Document::from_bytes_with_config(data, config) {
            Ok(d) => d,
            Err(e) => {
                stats.failures += 1;
                failures.push(CorpusResult {
                    path: path.display().to_string(),
                    kind: FailureKind::Parse,
                    category,
                    error: e.to_string(),
                });
                continue;
            }
        };

        let page_count = doc.page_count();
        let mut got_text = false;
        let mut page_failed = false;

        for i in 0..page_count {
            pages_total += 1;
            let mut page = match doc.page(i) {
                Ok(p) => p,
                Err(e) => {
                    stats.failures += 1;
                    failures.push(CorpusResult {
                        path: path.display().to_string(),
                        kind: FailureKind::TextExtraction,
                        category,
                        error: format!("page {i}: {e}"),
                    });
                    page_failed = true;
                    break;
                }
            };
            match page.text() {
                Ok(text) => {
                    if !text.trim().is_empty() {
                        got_text = true;
                    }
                }
                Err(e) => {
                    stats.failures += 1;
                    failures.push(CorpusResult {
                        path: path.display().to_string(),
                        kind: FailureKind::TextExtraction,
                        category,
                        error: format!("page {i} text: {e}"),
                    });
                    page_failed = true;
                    break;
                }
            }
        }

        if !page_failed {
            if got_text {
                stats.success_with_text += 1;
            } else {
                stats.success_empty += 1;
            }
        }

        warnings_total += diag.warnings().len();
    }

    // Categorize and report
    let parse_errors: Vec<_> = failures
        .iter()
        .filter(|f| f.kind == FailureKind::Parse)
        .collect();
    let text_errors: Vec<_> = failures
        .iter()
        .filter(|f| f.kind == FailureKind::TextExtraction)
        .collect();
    let io_errors: Vec<_> = failures
        .iter()
        .filter(|f| f.kind == FailureKind::Io)
        .collect();

    let total = pdfs.len();
    let success_total = pdfjs_stats.success_total() + pdfium_stats.success_total();

    eprintln!("=== Extended Corpus Full Pipeline Results ===");
    eprintln!("Total PDFs: {total}");
    eprintln!(
        "Success: {success_total} ({:.1}%) [{} with text, {} empty]",
        success_total as f64 / total as f64 * 100.0,
        pdfjs_stats.success_with_text + pdfium_stats.success_with_text,
        pdfjs_stats.success_empty + pdfium_stats.success_empty,
    );
    eprintln!("Pages processed: {pages_total}");
    eprintln!("Warnings total: {warnings_total}");
    eprintln!("---");
    eprintln!("Parse errors: {}", parse_errors.len());
    eprintln!("Text extraction errors: {}", text_errors.len());
    eprintln!("I/O errors: {}", io_errors.len());

    // CR-002: Per-category reporting
    eprintln!("---");
    eprintln!(
        "pdf.js suite: {}/{} ({:.1}%) [{} with text, {} empty, {} failed]",
        pdfjs_stats.success_total(),
        pdfjs_stats.total(),
        pdfjs_stats.success_rate() * 100.0,
        pdfjs_stats.success_with_text,
        pdfjs_stats.success_empty,
        pdfjs_stats.failures,
    );
    eprintln!(
        "pdfium suite: {}/{} ({:.1}%) [{} with text, {} empty, {} failed]",
        pdfium_stats.success_total(),
        pdfium_stats.total(),
        pdfium_stats.success_rate() * 100.0,
        pdfium_stats.success_with_text,
        pdfium_stats.success_empty,
        pdfium_stats.failures,
    );

    // Per-category failure details
    let pdfjs_failures: Vec<_> = failures
        .iter()
        .filter(|f| f.category == CorpusCategory::PdfjsSuite)
        .collect();
    let pdfium_failures: Vec<_> = failures
        .iter()
        .filter(|f| f.category == CorpusCategory::PdfiumSuite)
        .collect();

    if !pdfjs_failures.is_empty() {
        eprintln!("---");
        eprintln!("pdf.js suite failures ({}):", pdfjs_failures.len());
        for f in &pdfjs_failures {
            let short_path = f.path.rsplit('/').next().unwrap_or(&f.path);
            let short_error = f.error.lines().next().unwrap_or(&f.error);
            eprintln!("  {:?} {} -- {}", f.kind, short_path, short_error);
        }
    }

    // Aggregate error patterns
    let mut error_patterns: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for f in &failures {
        // Extract the root cause (first line or first 80 chars)
        let key = f
            .error
            .lines()
            .next()
            .unwrap_or(&f.error)
            .chars()
            .take(80)
            .collect::<String>();
        *error_patterns.entry(key).or_insert(0) += 1;
    }

    if !error_patterns.is_empty() {
        eprintln!("---");
        eprintln!("Error patterns (by frequency):");
        let mut patterns: Vec<_> = error_patterns.into_iter().collect();
        patterns.sort_by_key(|p| std::cmp::Reverse(p.1));
        for (pattern, count) in patterns.iter().take(20) {
            eprintln!("  [{count:>4}x] {pattern}");
        }
    }

    // List individual failures if few enough (pdfium only, pdfjs already shown above)
    if pdfium_failures.len() <= 50 {
        eprintln!("---");
        eprintln!("pdfium suite failures ({}):", pdfium_failures.len());
        for f in &pdfium_failures {
            let short_path = f.path.rsplit('/').next().unwrap_or(&f.path);
            let short_error = f.error.lines().next().unwrap_or(&f.error);
            eprintln!("  {:?} {} -- {}", f.kind, short_path, short_error);
        }
    }

    // CR-003: Per-category regression thresholds (ratcheted to measured values).
    // pdf.js suite: 94.5% measured (remaining failures are V=5/R=6 encryption,
    // unknown passwords, and non-encryption parsing edge cases)
    // pdfium suite: 81.6% measured (many are intentionally malformed)
    assert!(
        success_total > 0,
        "extended corpus present but zero PDFs processed successfully"
    );

    if pdfjs_stats.total() > 0 {
        let pdfjs_rate = pdfjs_stats.success_rate();
        assert!(
            pdfjs_rate >= 0.945,
            "pdf.js suite success rate {:.1}% is below 94.5% threshold",
            pdfjs_rate * 100.0
        );
    }

    if pdfium_stats.total() > 0 {
        let pdfium_rate = pdfium_stats.success_rate();
        assert!(
            pdfium_rate >= 0.815,
            "pdfium suite success rate {:.1}% is below 81.5% threshold",
            pdfium_rate * 100.0
        );
    }

    // Combined threshold (keep the old 85% floor as a backstop)
    let combined_rate = success_total as f64 / total as f64;
    assert!(
        combined_rate > 0.85,
        "combined corpus success rate {:.1}% is below 85% threshold",
        combined_rate * 100.0
    );
}

// ---------------------------------------------------------------------------
// T3-012: Type3-inside-Type3 cycle detection tests
// ---------------------------------------------------------------------------

/// Verify that a Type3 PDF with a cycle (Font A's CharProc uses Font B,
/// whose CharProc uses Font A) does not hang or panic. The security limits
/// (depth cap, visited set, invocation counter) should prevent infinite recursion.
#[test]
fn extended_type3_cycle_no_hang() {
    let path =
        Path::new(EXTENDED_DIR).join("pdfjs-repo/test/pdfs/ContentStreamCycleType3insideType3.pdf");
    if !path.exists() {
        if !is_extended_corpus_available() {
            eprintln!("Skipping T3-012 cycle test (extended corpus not available)");
            return;
        }
        eprintln!(
            "Skipping T3-012 cycle test (file not found: {})",
            path.display()
        );
        return;
    }

    let data = std::fs::read(&path).expect("read cycle test PDF");
    let config = Config::default();
    let mut doc = Document::from_bytes_with_config(data, config).expect("open cycle test PDF");

    // Extract text from all pages. Must complete without hanging.
    for i in 0..doc.page_count() {
        let mut page = doc.page(i).expect("get page");
        // We don't care about the content, only that it completes.
        let _text = page.text().expect("extract text from cycle PDF");
    }
}

/// Verify that a Type3 PDF with nested Type3 fonts (no cycle) works correctly.
#[test]
fn extended_type3_no_cycle_works() {
    let path = Path::new(EXTENDED_DIR)
        .join("pdfjs-repo/test/pdfs/ContentStreamNoCycleType3insideType3.pdf");
    if !path.exists() {
        if !is_extended_corpus_available() {
            eprintln!("Skipping T3-012 no-cycle test (extended corpus not available)");
            return;
        }
        eprintln!(
            "Skipping T3-012 no-cycle test (file not found: {})",
            path.display()
        );
        return;
    }

    let data = std::fs::read(&path).expect("read no-cycle test PDF");
    let config = Config::default();
    let mut doc = Document::from_bytes_with_config(data, config).expect("open no-cycle test PDF");

    for i in 0..doc.page_count() {
        let mut page = doc.page(i).expect("get page");
        let _text = page.text().expect("extract text from no-cycle PDF");
    }
}

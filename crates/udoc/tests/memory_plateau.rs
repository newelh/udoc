//! T60-MEMBATCH: memory plateau integration test.
//!
//! Loops `extract()` on the same PDF 100 times and asserts peak RSS
//! doesn't grow more than 1.5x the first iteration after a short warmup.
//! Linux-only (reads `/proc/self/status`); a no-op on other platforms.

#![cfg(target_os = "linux")]

use std::path::PathBuf;

fn test_pdf() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../udoc-pdf/tests/corpus/minimal/table_layout.pdf")
}

fn read_rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.split_whitespace().next().and_then(|s| s.parse().ok());
        }
    }
    None
}

#[test]
fn extract_loop_does_not_leak() {
    const ITERS: usize = 100;
    const WARMUP: usize = 5;

    let pdf = test_pdf();
    assert!(pdf.exists(), "test fixture missing: {}", pdf.display());

    let mut peaks = Vec::with_capacity(ITERS);
    for i in 0..ITERS {
        let _doc = udoc::extract(&pdf).expect("extract should succeed");
        if let Some(rss) = read_rss_kb() {
            peaks.push(rss);
            if i < WARMUP || i % 20 == 0 || i == ITERS - 1 {
                eprintln!("iter {i}: rss={rss} KB");
            }
        }
    }
    if peaks.len() < WARMUP + 10 {
        // Couldn't read /proc (container? sandbox?) -- skip without failing.
        eprintln!("skipping RSS assertion: only {} samples", peaks.len());
        return;
    }

    let warm_peak: u64 = *peaks[WARMUP..].iter().max().unwrap_or(&0);
    let first_post_warmup = peaks[WARMUP];
    let growth_ratio = warm_peak as f64 / first_post_warmup.max(1) as f64;
    eprintln!(
        "first-post-warmup: {first_post_warmup} KB, peak: {warm_peak} KB, ratio: {growth_ratio:.2}"
    );
    assert!(
        growth_ratio <= 1.5,
        "RSS grew {growth_ratio:.2}x over {} iters (first={first_post_warmup} KB, peak={warm_peak} KB); \
         expected <= 1.5x for a stable plateau",
        ITERS - WARMUP
    );
}

#[test]
fn reset_document_caches_in_loop() {
    // Same shape but exercises the explicit reset path. Should be at
    // least as good as the plain loop (never worse). Uses the public API.
    const ITERS: usize = 50;
    const WARMUP: usize = 5;

    let pdf = test_pdf();
    assert!(pdf.exists(), "test fixture missing");

    let mut peaks = Vec::with_capacity(ITERS);
    for i in 0..ITERS {
        let mut ext = udoc::Extractor::open(&pdf).expect("open");
        let _ = ext.text().expect("text");
        ext.reset_document_caches();
        drop(ext);
        if let Some(rss) = read_rss_kb() {
            peaks.push(rss);
            if i < WARMUP || i == ITERS - 1 {
                eprintln!("iter {i}: rss={rss} KB (reset called)");
            }
        }
    }
    if peaks.len() < WARMUP + 5 {
        return;
    }

    let warm_peak: u64 = *peaks[WARMUP..].iter().max().unwrap_or(&0);
    let first_post_warmup = peaks[WARMUP];
    let growth_ratio = warm_peak as f64 / first_post_warmup.max(1) as f64;
    eprintln!(
        "reset-loop first: {first_post_warmup} KB, peak: {warm_peak} KB, ratio: {growth_ratio:.2}"
    );
    assert!(
        growth_ratio <= 1.5,
        "RSS ratio {growth_ratio:.2} exceeded 1.5 with reset_document_caches"
    );
}

//! Streaming API RSS verification.
//!
//! `Extractor::page_text(i)` is supposed to be a streaming API: users
//! who iterate pages should see bounded memory growth, not "open the
//! whole document into RAM." This test asserts the contract by walking
//! every page of a multi-page fixture and measuring peak RSS via
//! `/proc/self/status`.
//!
//! Why it matters pre-alpha: users feeding udoc 1000-page reports
//! expect the crate to not balloon beyond the doc's serialized size.
//! A regression that starts materialising all pages at open time would
//! show up here as a peak well above the fixture size.
//!
//! The test is Linux-only because we read `/proc/self/status`; other
//! platforms get a compile-time skip, not a spurious pass.
#![cfg(target_os = "linux")]

use std::path::PathBuf;

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(rel)
}

/// Peak RSS of the current process, read from /proc/self/status.
/// Returns kilobytes. Panics if the value can't be parsed -- we run
/// on Linux only (cfg gated) so the proc file is expected to exist.
fn peak_rss_kb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmPeak:") {
            // Line format: "VmPeak:\t    12345 kB"
            let num: u64 = rest
                .split_whitespace()
                .next()
                .expect("VmPeak value")
                .parse()
                .expect("VmPeak is a number");
            return num;
        }
    }
    panic!("VmPeak missing from /proc/self/status");
}

/// Current VmRSS (resident set size, not the peak). Used before and
/// after a bounded operation so the delta represents that operation's
/// incremental allocation.
fn current_rss_kb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest
                .split_whitespace()
                .next()
                .expect("VmRSS value")
                .parse()
                .expect("VmRSS is a number");
        }
    }
    panic!("VmRSS missing from /proc/self/status");
}

/// Walk every page of a 12-page arxiv PDF via `page_text(i)` and
/// assert the net RSS delta is bounded. We use the ratio rather than
/// an absolute byte count to tolerate baseline noise from cargo-test
/// process setup (compiler caches, allocator warm-up, etc).
///
/// The fixture is 168 KB on disk. A true streaming implementation
/// keeps RSS close to (binary footprint) + (one page's working set).
/// A naive implementation that materialised all 12 pages upfront
/// would show ~12x the delta of a single page-read.
#[test]
fn page_by_page_extraction_has_bounded_rss_growth() {
    use udoc::Extractor;

    let pdf = fixture("crates/udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    assert!(pdf.is_file(), "fixture at {} must exist", pdf.display());

    // Warm up once: open + drop so allocator pages and any lazy statics
    // are warm before we sample the baseline. This avoids the first
    // extractor eating an extra ~2-5 MB of allocator-arena growth
    // that isn't related to the API contract.
    {
        let mut ext = Extractor::open(&pdf).expect("open warmup");
        let _ = ext.page_text(0);
    }

    let rss_before = current_rss_kb();

    let mut ext = Extractor::open(&pdf).expect("open");
    let page_count = ext.page_count();
    assert!(
        page_count >= 10,
        "fixture has {page_count} pages, expected >=10"
    );

    let mut total_chars = 0usize;
    for i in 0..page_count {
        let text = ext.page_text(i).expect("page text");
        total_chars += text.chars().count();
    }
    assert!(total_chars > 100, "got only {total_chars} chars total");

    let peak_during = peak_rss_kb();
    let rss_after_loop = current_rss_kb();
    drop(ext);
    let rss_after_drop = current_rss_kb();

    let peak_mb = peak_during / 1024;
    let delta_kb = rss_after_loop.saturating_sub(rss_before);
    let delta_mb = delta_kb / 1024;

    // Print so CI logs capture the real numbers; the baseline is
    // currently ~200 MB peak / ~30 MB delta on a 168 KB arxiv fixture,
    // dominated by process-global state (font bundles, auto-hinter
    // scratch, Tier1 CFF/TTF data). See /progress.md -- this
    // is a Lane-P target; the test exists to catch a *regression* vs
    // whatever the current baseline is, not to enforce our aspirational
    // <50 MB peak. Tighten the bound as Lane-P reduces the baseline.
    eprintln!(
        "streaming_rss: before={rss_before}kB after_loop={rss_after_loop}kB \
         after_drop={rss_after_drop}kB peak={peak_during}kB  ({peak_mb} MB peak, {delta_mb} MB delta)"
    );

    // Regression bound: peak RSS should stay well under 500 MB. At
    // current baseline (~200 MB) this leaves ~2.5x headroom. If a
    // change doubles the extraction-path footprint, it trips this
    // assertion and requires explicit justification or a tighter fix.
    assert!(
        peak_mb < 500,
        "peak RSS {peak_mb} MB exceeds 500 MB regression bound for 12-page arxiv extract"
    );

    // Per-loop growth bound: anything over 150 MB of incremental
    // delta over the 12-page iteration suggests page-state isn't
    // being released and the streaming contract is broken.
    assert!(
        delta_mb < 150,
        "rss grew by {delta_mb} MB over the extraction loop; streaming contract violated. \
         before={rss_before}kB after={rss_after_loop}kB peak={peak_during}kB after_drop={rss_after_drop}kB",
    );
}

/// Regression test for the contract where opening an Extractor does
/// NOT read or decode every page upfront. The cost of `open_with` +
/// `page_count` should be bounded by document-header work, not proportional
/// to page_count.
#[test]
fn open_without_iteration_is_cheap() {
    use udoc::Extractor;

    let pdf = fixture("crates/udoc-pdf/tests/corpus/realworld/cjk_chinese.pdf");
    assert!(pdf.is_file(), "fixture must exist at {}", pdf.display());

    // Warm allocator.
    {
        let _ = Extractor::open(&pdf);
    }

    let rss_before = current_rss_kb();

    let ext = Extractor::open(&pdf).expect("open");
    let page_count = ext.page_count();
    drop(ext);

    let rss_after = current_rss_kb();
    let delta_kb = rss_after.saturating_sub(rss_before);
    let delta_mb = delta_kb / 1024;

    assert!(
        page_count >= 10,
        "fixture is {page_count} pages, expected a multi-page PDF"
    );
    // Opening + page_count alone should cost much less than fully
    // reading the doc. 50 MB is generous headroom.
    assert!(
        delta_mb < 50,
        "open+page_count on {page_count}-page PDF grew RSS by {delta_mb} MB; \
         suggests eager page materialisation"
    );
}

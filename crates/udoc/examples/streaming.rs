//! Iterate a multi-page PDF page-by-page via [`Extractor`] with bounded RSS.
//!
//! Demonstrates the streaming API: open once, walk pages, drop the extractor.
//! Working set should grow with the document's *active page*, not its total
//! page count -- this is the contract enforced by `tests/streaming_rss.rs`.
//!
//! Run with:
//!
//! ```text
//! cargo run -p udoc --example streaming
//! ```
//!
//! Override the fixture path with the first argument:
//!
//! ```text
//! cargo run -p udoc --example streaming -- path/to/multipage.pdf
//! ```
//!
//! On Linux we sample `/proc/self/status` before and after the iteration loop
//! and assert the delta is bounded; on other platforms we just assert the
//! pages were extracted.

use std::path::PathBuf;

use udoc::Extractor;

/// Fallback regression bound for incremental RSS growth across the iteration
/// loop. Matches the headroom used by `tests/streaming_rss.rs` (150 MB) so the
/// example trips at the same place as the integration test.
const MAX_DELTA_MB: u64 = 150;

fn default_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/udoc -> crates")
        .parent()
        .expect("crates -> repo root")
        .join("crates/udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf")
}

/// Read VmRSS in kB from /proc/self/status. Returns None on non-Linux or on
/// any parse failure -- the example degrades to a "did we extract text" check.
fn current_rss_kb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                return rest.split_whitespace().next()?.parse().ok();
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(default_fixture);

    println!("streaming pages of {}", path.display());

    // Warm-up open: pulls in lazy statics (font bundles, regex caches) so
    // the measured baseline reflects steady-state, not first-touch growth.
    {
        let mut warm = Extractor::open(&path)?;
        let _ = warm.page_text(0);
    }

    let rss_before = current_rss_kb();

    // Real iteration. Open once, walk every page, drop. Each `page_text(i)`
    // is supposed to release per-page state before returning -- iterating
    // does not materialize every page into RAM.
    let mut ext = Extractor::open(&path)?;
    let page_count = ext.page_count();
    println!("page_count = {page_count}");

    let mut total_chars = 0usize;
    for i in 0..page_count {
        let text = ext.page_text(i)?;
        total_chars += text.chars().count();
        if i < 3 {
            // First few pages: dump a one-line preview so the user sees
            // something happening. Truncated to keep stdout tidy.
            let preview: String = text.chars().take(80).collect();
            println!("page {i:>3}: {} chars -- {preview}", text.chars().count());
        }
    }
    drop(ext);

    let rss_after = current_rss_kb();

    println!();
    println!("total chars extracted: {total_chars}");
    if let (Some(before), Some(after)) = (rss_before, rss_after) {
        let delta_kb = after.saturating_sub(before);
        let delta_mb = delta_kb / 1024;
        println!(
            "RSS delta over iteration: {delta_kb} kB ({delta_mb} MB), \
             before={before}kB after={after}kB"
        );

        // Bounded-RSS assertion: growth across the page loop should be well
        // under MAX_DELTA_MB. A regression that materializes all pages
        // upfront would scale with page_count and trip this gate.
        assert!(
            delta_mb < MAX_DELTA_MB,
            "RSS grew by {delta_mb} MB over {page_count}-page iteration \
             (limit {MAX_DELTA_MB} MB); streaming contract may be broken"
        );
    } else {
        println!("RSS sampling unavailable on this platform; skipping bounded-growth check");
    }

    // Always-on assertions, platform-agnostic.
    assert!(
        page_count > 0,
        "expected multi-page fixture, got {page_count}"
    );
    assert!(
        total_chars > 100,
        "expected nontrivial text across {page_count} pages, got {total_chars} chars"
    );

    println!("ok: extracted {total_chars} chars from {page_count} pages with bounded RSS");

    Ok(())
}

#[cfg(test)]
mod tests {
    /// Drive `main()` from a test so `cargo test --examples` exercises the
    /// streaming pipeline + bounded-RSS assertion, not just a compile check.
    #[test]
    fn example_runs() {
        super::main().expect("streaming example should succeed");
    }
}

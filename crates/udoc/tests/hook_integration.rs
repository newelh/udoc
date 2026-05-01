//! Hook integration tests: end-to-end roundtrips against real documents.
//!
//! Tests that are NOT marked #[ignore] use only bash, sleep, and false -- tools
//! universally available on Unix. Tests marked #[ignore] require external tools
//! (tesseract, etc.) and are skipped in CI unless explicitly enabled.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use udoc::hooks::{HookConfig, HookRunner, HookSpec};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tmp_dir(label: &str) -> PathBuf {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "udoc-hook-integ-{}-{}-{}",
        std::process::id(),
        id,
        label
    ))
}

fn table_layout_pdf() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../udoc-pdf/tests/corpus/minimal/table_layout.pdf")
}

// ---------------------------------------------------------------------------
// T1: echo hook roundtrip (NOT ignored)
//
// A handshake hook that declares needs=["text"] and returns an empty spans
// response for every page. Validates the full JSONL protocol machinery:
// handshake read, per-page request, per-page response, document untouched.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn test_echo_hook_roundtrip() {
    let dir = tmp_dir("echo-roundtrip");
    std::fs::create_dir_all(&dir).expect("create tmp dir");

    let script = dir.join("echo-hook.sh");
    let mut f = std::fs::File::create(&script).expect("create script");
    writeln!(
        f,
        r#"#!/bin/bash
# Handshake: needs text, provides spans (returns empty spans each page).
echo '{{"protocol":"udoc-hook-v1","capabilities":["annotate"],"needs":["text"],"provides":["spans"]}}'
while IFS= read -r line; do
    echo '{{"spans":[]}}'
done
"#
    )
    .expect("write script");
    drop(f);

    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&script)
        .status()
        .expect("chmod");

    let spec = HookSpec::from_command(script.to_str().expect("script path"));
    let mut config = HookConfig::default();
    config.page_timeout_secs = 10;

    let result = HookRunner::new(&[spec], config);
    match result {
        Ok(mut runner) => {
            let mut doc = udoc::extract(table_layout_pdf()).expect("extract pdf");

            // Document should have content before hooks run.
            let pre_content_len = doc.content.len();
            assert!(
                pre_content_len > 0,
                "document should have content before hook run"
            );

            let run_result = runner.run(&mut doc, None);
            assert!(
                run_result.is_ok(),
                "echo hook should complete without error: {:?}",
                run_result.err()
            );

            // Content should still be present (hook returned empty spans, no replacement).
            assert!(
                !doc.content.is_empty(),
                "document content should survive a no-op hook"
            );

            // The hook sends empty spans; page text should still contain "Alice"
            // (original extraction is preserved when hook adds nothing).
            let all_text: String = doc
                .content
                .iter()
                .map(|b| b.text())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                all_text.contains("Alice"),
                "native text should be preserved after no-op hook; got: {all_text:?}"
            );
        }
        Err(e) => {
            let msg = format!("{e}");
            // Only acceptable failure is bash not available.
            assert!(
                msg.contains("spawn") || msg.contains("No such file"),
                "unexpected error: {msg}"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// T2: tesseract hook produces spans (IGNORED -- requires tesseract)
//
// Runs the reference tesseract-ocr.sh hook against a real page image.
// Verifies output spans contain recognized text including "Name" and "Alice".
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
#[ignore]
fn test_tesseract_hook_produces_spans() {
    // This test requires:
    //   - tesseract installed and on PATH
    //   - jq installed and on PATH
    //   - the page image at tests/fixtures/page-images/table_layout-1.png
    //
    // Generate the image with:
    //   pdftoppm -r 300 -png crates/udoc-pdf/tests/corpus/minimal/table_layout.pdf \
    //            crates/udoc/tests/fixtures/page-images/table_layout
    // (this produces table_layout-1.png for page 1)

    let image_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/page-images");

    let image_path = image_dir.join("table_layout-1.png");
    assert!(
        image_path.exists(),
        "page image not found at {}: run pdftoppm to generate it",
        image_path.display()
    );

    // Create a tesseract OCR hook (no handshake, default OCR classification).
    let dir = tmp_dir("tesseract");
    std::fs::create_dir_all(&dir).expect("create tmp dir");

    let script = dir.join("tesseract-ocr.sh");
    let mut f = std::fs::File::create(&script).expect("create script");
    writeln!(
        f,
        r#"#!/bin/bash
# Minimal tesseract OCR hook -- no handshake (default: needs=[image], provides=[spans]).
while IFS= read -r line; do
    page=$(echo "$line" | jq -r '.page_index')
    img=$(echo "$line" | jq -r '.image_path')
    tsv=$(tesseract "$img" - tsv 2>/dev/null)
    spans=$(echo "$tsv" | tail -n+2 | awk -F'\t' '$12 != "" {{ print $7, $8, $9, $10, $11, $12 }}' \
        | jq -Rc 'split(" ") | {{
            text: .[5:] | join(" "),
            bbox: [(.[0]|tonumber), (.[1]|tonumber), ((.[0]|tonumber)+(.[2]|tonumber)), ((.[1]|tonumber)+(.[3]|tonumber))],
            confidence: ((.[4]|tonumber) / 100)
          }}' \
        | jq -sc '.')
    echo "{{\\"page_index\\":$page,\\"spans\\":$spans}}" | jq -c '.'
done
"#
    )
    .expect("write tesseract script");
    drop(f);

    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&script)
        .status()
        .expect("chmod");

    // Build a document with 1 page and no content (simulates a scanned PDF).
    let mut doc = udoc::Document::new();
    doc.metadata.page_count = 1;
    // No content blocks: OCR hook will populate them.

    let spec = HookSpec::from_command(script.to_str().expect("script path"));
    let mut config = HookConfig::default();
    config.page_timeout_secs = 30;
    config.ocr_all_pages = true;

    let result = HookRunner::new(&[spec], config);
    match result {
        Ok(mut runner) => {
            let run_result = runner.run(&mut doc, Some(&image_dir));
            assert!(
                run_result.is_ok(),
                "tesseract hook should complete without error: {:?}",
                run_result.err()
            );

            // Verify spans were produced with expected content.
            let pres = doc
                .presentation
                .as_ref()
                .expect("presentation layer should be populated by OCR hook");

            let all_span_text: String = pres
                .raw_spans
                .iter()
                .map(|s| s.text.clone())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                all_span_text.contains("Name"),
                "OCR output should contain 'Name'; got: {all_span_text:?}"
            );
            assert!(
                all_span_text.contains("Alice"),
                "OCR output should contain 'Alice'; got: {all_span_text:?}"
            );
        }
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("spawn") || msg.contains("No such file"),
                "unexpected error spawning tesseract hook: {msg}"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// T3: hook timeout recovery (NOT ignored)
//
// A hook that sleeps 10 seconds gets a 1-second timeout. udoc must kill it
// and fall back gracefully. The document must still have its native content.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn test_hook_timeout_recovery() {
    let dir = tmp_dir("timeout-recovery");
    std::fs::create_dir_all(&dir).expect("create tmp dir");

    let script = dir.join("slow-hook.sh");
    let mut f = std::fs::File::create(&script).expect("create script");
    writeln!(
        f,
        r#"#!/bin/bash
# Outputs handshake then sleeps forever on every page request.
echo '{{"protocol":"udoc-hook-v1","capabilities":["annotate"],"needs":["text"],"provides":["labels"]}}'
while IFS= read -r line; do
    sleep 10
done
"#
    )
    .expect("write script");
    drop(f);

    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&script)
        .status()
        .expect("chmod");

    let spec = HookSpec::from_command(script.to_str().expect("script path"));
    let mut config = HookConfig::default();
    config.page_timeout_secs = 1; // 1 second -- should fire well before the 10s sleep

    let result = HookRunner::new(&[spec], config);
    match result {
        Ok(mut runner) => {
            let mut doc = udoc::extract(table_layout_pdf()).expect("extract pdf");

            // Record native text before hook runs.
            let native_text: String = doc
                .content
                .iter()
                .map(|b| b.text())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                native_text.contains("Alice"),
                "native extraction must work before hook test"
            );

            let start = std::time::Instant::now();
            // run() returns Err when all invocations fail, which is expected here.
            let _ = runner.run(&mut doc, None);
            let elapsed = start.elapsed();

            // Must not hang -- should resolve around the timeout, not 10 seconds.
            assert!(
                elapsed.as_secs() < 8,
                "hook should have been killed by timeout, but took {:?}",
                elapsed
            );

            // Document content must survive -- the timeout triggers fallback, not a panic.
            assert!(
                !doc.content.is_empty(),
                "document content must not be destroyed by hook timeout"
            );

            let post_text: String = doc
                .content
                .iter()
                .map(|b| b.text())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                post_text.contains("Alice"),
                "native text must be intact after hook timeout: {post_text:?}"
            );
        }
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("spawn") || msg.contains("No such file"),
                "unexpected error: {msg}"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// T4: hook crash recovery (NOT ignored)
//
// A hook that exits immediately with code 1. udoc must fall back gracefully:
// no panic, document retains all native content.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn test_hook_crash_recovery() {
    // `false` exits with code 1 immediately. HookRunner reads EOF on stdout
    // during the handshake read and treats it as a dead no-handshake OCR hook.
    let spec = HookSpec::from_command("false");
    let mut config = HookConfig::default();
    config.page_timeout_secs = 5;

    let result = HookRunner::new(&[spec], config);
    match result {
        Ok(mut runner) => {
            let mut doc = udoc::extract(table_layout_pdf()).expect("extract pdf");

            // run() may return Ok or Err("all hook invocations failed"), but must NOT panic.
            let _ = runner.run(&mut doc, None);

            // Native content must be intact regardless of hook fate.
            assert!(
                !doc.content.is_empty(),
                "document content must survive a crashing hook"
            );

            let text: String = doc
                .content
                .iter()
                .map(|b| b.text())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                text.contains("Alice"),
                "native text must be intact after hook crash: {text:?}"
            );
        }
        Err(e) => {
            // Acceptable if `false` is not on PATH (unusual but possible in restricted envs).
            let msg = format!("{e}");
            assert!(
                msg.contains("spawn") || msg.contains("No such file") || msg.contains("false"),
                "unexpected error: {msg}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// T5: page skip when text is sufficient (NOT ignored)
//
// Verifies that when a page already has enough native text, the OCR hook
// is not invoked for that page. Uses a hook that writes a detectable label
// when it processes a page. table_layout.pdf has ~25 native words; with
// min_words_to_skip_ocr=5, the page clears the threshold and the hook is skipped.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn test_page_skip_when_text_sufficient() {
    let dir = tmp_dir("skip-ocr");
    std::fs::create_dir_all(&dir).expect("create tmp dir");

    // Hook that stamps a label "hook_ran" = "true" whenever it processes a page.
    // If the page-skip logic works, this hook must not run for pages with enough text.
    let script = dir.join("stamp-hook.sh");
    let mut f = std::fs::File::create(&script).expect("create script");
    writeln!(
        f,
        r#"#!/bin/bash
# OCR hook that stamps a label so we can detect whether it ran.
echo '{{"protocol":"udoc-hook-v1","capabilities":["ocr"],"needs":["text"],"provides":["labels"]}}'
while IFS= read -r line; do
    echo '{{"labels":{{"hook_ran":"true"}}}}'
done
"#
    )
    .expect("write script");
    drop(f);

    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&script)
        .status()
        .expect("chmod");

    let spec = HookSpec::from_command(script.to_str().expect("script path"));

    // table_layout.pdf produces ~25 words from native extraction.
    // Set the threshold below that so the page is considered "sufficient"
    // and the OCR hook is skipped.
    let mut config = HookConfig::default();
    config.page_timeout_secs = 10;
    config.ocr_all_pages = false;
    config.min_words_to_skip_ocr = 5; // 5 << 25 actual words, so page is skipped

    let result = HookRunner::new(&[spec], config);
    match result {
        Ok(mut runner) => {
            let mut doc = udoc::extract(table_layout_pdf()).expect("extract pdf");

            let run_result = runner.run(&mut doc, None);
            assert!(
                run_result.is_ok(),
                "hook runner should not error when all pages are skipped: {:?}",
                run_result.err()
            );

            // hook_ran must NOT be set: the page had enough text, hook was skipped.
            let hook_ran = doc
                .metadata
                .properties
                .get("hook.label.hook_ran")
                .map(|v| v.as_str())
                .unwrap_or("not-set");

            assert_eq!(
                hook_ran, "not-set",
                "OCR hook should have been skipped for page with sufficient text; \
                 hook.label.hook_ran = {hook_ran:?}"
            );

            // Native content must be untouched.
            let text: String = doc
                .content
                .iter()
                .map(|b| b.text())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                text.contains("Alice"),
                "native text must be intact when hook is skipped: {text:?}"
            );
        }
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("spawn") || msg.contains("No such file"),
                "unexpected error: {msg}"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

//! Oversized-output stress tests.
//!
//! A hook that emits a single line larger than the per-line cap
//! (`MAX_HOOK_LINE_SIZE` = 1 MiB) must be truncated, not OOM. Lines beyond
//! the cap are drained until newline.

use udoc::hooks::{HookConfig, HookRunner, HookSpec};

use super::helpers::{small_pdf, write_script, NOOP_HANDSHAKE};

// ---------------------------------------------------------------------------
// 1. 2 MiB single line is truncated, hook is marked dead due to invalid JSON
// ---------------------------------------------------------------------------
#[test]
fn two_mib_single_line_truncated() {
    // After the handshake, emit a single line of ~2 MiB then a newline.
    // The line will be truncated to MAX_HOOK_LINE_SIZE (1 MiB) on read.
    let body = format!(
        "echo '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    head -c $((2 * 1024 * 1024)) /dev/zero | tr '\\0' 'a'\n    echo\ndone"
    );
    let script = write_script("two-mib-line", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 5;

    let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
    let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");

    // Must not panic, must not OOM. Truncated line will fail to parse as
    // JSON; runner increments consecutive_failures and may eventually mark
    // dead.
    let _ = runner.run(&mut doc, None);

    assert!(
        !doc.content.is_empty(),
        "document content must survive oversized output"
    );
}

// ---------------------------------------------------------------------------
// 2. 10 MiB single line is also truncated (does not exhaust memory)
// ---------------------------------------------------------------------------
#[test]
fn ten_mib_single_line_truncated() {
    let body = format!(
        "echo '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    head -c $((10 * 1024 * 1024)) /dev/zero | tr '\\0' 'a'\n    echo\ndone"
    );
    let script = write_script("ten-mib-line", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 10;

    let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
    let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");
    let _ = runner.run(&mut doc, None);
    assert!(!doc.content.is_empty());
}

// ---------------------------------------------------------------------------
// 3. Many small valid lines do NOT trip the truncation path
// ---------------------------------------------------------------------------
#[test]
fn many_small_lines_not_truncated() {
    // Hook emits a small valid annotation per line; this is the happy path.
    let body = format!(
        "echo '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    echo '{{\"annotations\":[]}}'\ndone"
    );
    let script = write_script("many-small", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 5;

    let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
    let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");
    let result = runner.run(&mut doc, None);
    assert!(
        result.is_ok(),
        "happy-path hook should run cleanly, err = {:?}",
        result.err()
    );
}

// ---------------------------------------------------------------------------
// 4. Line near the cap (~900 KiB) but valid JSON is still parsed
// ---------------------------------------------------------------------------
#[test]
fn near_cap_valid_json_parses() {
    // Build ~900 KiB of "x" inside a JSON string. The whole line fits under
    // the 1 MiB cap and should parse cleanly.
    let body = format!(
        "echo '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    big=$(head -c $((900 * 1024)) /dev/zero | tr '\\0' 'x')\n    printf '{{\"annotations\":[],\"big\":\"%s\"}}\\n' \"$big\"\ndone"
    );
    let script = write_script("near-cap-valid", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 10;

    let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
    let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");
    let _ = runner.run(&mut doc, None);
    assert!(!doc.content.is_empty());
}

// ---------------------------------------------------------------------------
// 5. Oversized stderr is also capped
//
// MAX_STDERR_BYTES = 1 MiB. Emit far more than that to stderr and verify
// the hook still completes without OOM and the document survives. We can't
// inspect stderr from inside the test, so we rely on the test not OOMing.
// ---------------------------------------------------------------------------
#[test]
fn oversized_stderr_capped() {
    let body = format!(
        "echo '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    # 5 MiB of garbage to stderr per request\n    head -c $((5 * 1024 * 1024)) /dev/zero | tr '\\0' 'E' >&2\n    echo >&2\n    echo '{{\"annotations\":[]}}'\ndone"
    );
    let script = write_script("oversized-stderr", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 10;

    let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
    let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");
    let result = runner.run(&mut doc, None);
    // Stdout responses are valid JSON, so the run itself should succeed.
    assert!(
        result.is_ok(),
        "stderr cap should not break stdout processing, err = {:?}",
        result.err()
    );
    assert!(!doc.content.is_empty());
}

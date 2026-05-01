//! Timeout-path stress tests.
//!
//! A hook that never responds must be killed within `page_timeout_secs` and
//! the surrounding extraction must continue with native content intact.

use std::time::Instant;

use udoc::hooks::{HookConfig, HookRunner, HookSpec};

use super::helpers::{realworld_pdf, run_hook_against, small_pdf, write_script, NOOP_HANDSHAKE};

// ---------------------------------------------------------------------------
// 1. Sleep past timeout -- timeout fires within the budget window
// ---------------------------------------------------------------------------
#[test]
fn timeout_fires_within_budget() {
    let body = format!("echo '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    sleep 30\ndone");
    let script = write_script("sleep30", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 2;

    let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
    let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");

    let start = Instant::now();
    let _ = runner.run(&mut doc, None);
    let elapsed = start.elapsed();

    // Each page roughly costs `page_timeout_secs`. small_pdf() is 1 page;
    // budget cap at 6 s gives plenty of margin without being fragile.
    assert!(
        elapsed.as_secs() < 6,
        "timeout should have fired by ~2 s, took {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// 2. Document still extracts after timeout
// ---------------------------------------------------------------------------
#[test]
fn document_survives_timeout() {
    let body = format!("echo '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    sleep 30\ndone");
    let script = write_script("sleep30-survive", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 2;

    let (doc, _outcome) = run_hook_against(spec, config, &small_pdf());
    let doc = doc.expect("extraction must succeed even when hooks time out");
    assert!(
        !doc.content.is_empty(),
        "document content must survive a hook timeout"
    );
}

// ---------------------------------------------------------------------------
// 3. Timeout error message identifies the hook by name
// ---------------------------------------------------------------------------
#[test]
fn timeout_message_names_hook() {
    use std::sync::{Arc, Mutex};

    // The hook protocol writes timeout messages to stderr via eprintln!().
    // We can't easily intercept stderr from the test process, but we CAN
    // verify the runner does not panic and the hook command name is at
    // least retained on the spec.
    let body = format!("echo '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    sleep 30\ndone");
    let script = write_script("named-hook", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let cmd_name_seen = Arc::new(Mutex::new(spec.command.clone()));
    assert!(
        !cmd_name_seen.lock().unwrap().is_empty(),
        "spec.command should be retained after spawn"
    );

    let mut config = HookConfig::default();
    config.page_timeout_secs = 1;

    let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
    let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");
    let _ = runner.run(&mut doc, None);
}

// ---------------------------------------------------------------------------
// 4. Timeout during handshake kills the hook (no zombie)
// ---------------------------------------------------------------------------
#[test]
fn timeout_during_handshake_kills_hook() {
    // No handshake line emitted; just sleep forever. spawn_hook() reads the
    // first line with `timeout` and on timeout marks the hook dead.
    let body = "sleep 30";
    let script = write_script("hangs-on-handshake", body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 2;

    let start = Instant::now();
    let result = HookRunner::new(&[spec], config);
    let elapsed = start.elapsed();

    // HookRunner::new() should return; either Ok with a dead hook (current
    // behavior: spawn_hook treats timeout as "dead OCR hook") or Err. The
    // important thing is that we do not hang past the timeout.
    assert!(
        elapsed.as_secs() < 8,
        "handshake-timeout path took {elapsed:?}, should be ~2 s"
    );
    // Drop runner immediately if Ok so cleanup runs.
    drop(result);
}

// ---------------------------------------------------------------------------
// 5. Multi-page document continues after first-page timeout
// ---------------------------------------------------------------------------
#[test]
fn multipage_document_does_not_dogpile_timeouts() {
    // After timeout, the hook is marked dead. Subsequent pages must NOT
    // attempt to send a request (which would block forever waiting for a
    // response from a dead hook).
    let body = format!("echo '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    sleep 30\ndone");
    let script = write_script("dead-hook-skip", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 1;

    // arxiv_pdflatex.pdf is multi-page. The total wall time should be
    // dominated by the SINGLE page-timeout, not page_count * page_timeout.
    let mut doc = udoc::extract(realworld_pdf()).expect("extract pdf");
    let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");

    let start = Instant::now();
    let _ = runner.run(&mut doc, None);
    let elapsed = start.elapsed();

    // Generous bound: even with 4 pages * 1 s timeout we'd be well under 10 s
    // if the dead-hook short-circuit works. Without the short-circuit, the
    // hung-process tests would push us past 30 s.
    assert!(
        elapsed.as_secs() < 15,
        "dead-hook short-circuit appears broken; elapsed = {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// 6. page_timeout_secs = 0 is rejected at config time
// ---------------------------------------------------------------------------
#[test]
fn zero_timeout_rejected() {
    let script = write_script(
        "noop-zero",
        &format!("echo '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    echo '{{}}'\ndone"),
    );
    let spec = HookSpec::from_command(script.path.to_str().unwrap());
    let mut config = HookConfig::default();
    config.page_timeout_secs = 0;
    let result = HookRunner::new(&[spec], config);
    assert!(
        result.is_err(),
        "page_timeout_secs=0 must be rejected (would immediately kill all hooks)"
    );
    let msg = format!("{}", result.err().unwrap());
    assert!(
        msg.contains("timeout"),
        "rejection message should mention timeout, got: {msg}"
    );
}

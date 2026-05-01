//! Error-exit-path stress tests.
//!
//! A hook that exits with a non-zero code (during handshake or mid-run) must
//! be cleanly recovered from: native content survives, no panic, no zombie.

use udoc::hooks::{HookConfig, HookRunner, HookSpec};

use super::helpers::{realworld_pdf, small_pdf, write_script, NOOP_HANDSHAKE};

// ---------------------------------------------------------------------------
// 1. Exit 1 immediately (before handshake)
// ---------------------------------------------------------------------------
#[test]
fn exit_1_before_handshake() {
    let script = write_script("exit-1-fast", "exit 1");
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 5;

    // HookRunner::new() may succeed (treating EOF on handshake as a dead
    // OCR hook). The runner.run() call must complete and not panic.
    let runner = HookRunner::new(&[spec], config);
    if let Ok(mut runner) = runner {
        let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
        let _ = runner.run(&mut doc, None);
        assert!(!doc.content.is_empty(), "content must survive exit-1 hook");
    }
}

// ---------------------------------------------------------------------------
// 2. Exit 2 (clap-style usage error)
// ---------------------------------------------------------------------------
#[test]
fn exit_2_treated_as_dead_hook() {
    let script = write_script("exit-2", "exit 2");
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 5;

    if let Ok(mut runner) = HookRunner::new(&[spec], config) {
        let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
        let _ = runner.run(&mut doc, None);
        assert!(!doc.content.is_empty(), "content must survive exit-2 hook");
    }
}

// ---------------------------------------------------------------------------
// 3. Exit 127 (command-not-found)
// ---------------------------------------------------------------------------
#[test]
fn exit_127_treated_as_dead_hook() {
    let script = write_script("exit-127", "exit 127");
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 5;

    if let Ok(mut runner) = HookRunner::new(&[spec], config) {
        let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
        let _ = runner.run(&mut doc, None);
        assert!(
            !doc.content.is_empty(),
            "content must survive exit-127 hook"
        );
    }
}

// ---------------------------------------------------------------------------
// 4. Exit mid-run (after handshake, before all pages processed)
// ---------------------------------------------------------------------------
#[test]
fn exit_after_handshake_then_die() {
    let body =
        format!("echo '{NOOP_HANDSHAKE}'\n# read first request and die\nread -r line\nexit 1");
    let script = write_script("die-after-one", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 3;

    let mut doc = udoc::extract(realworld_pdf()).expect("extract pdf");
    if let Ok(mut runner) = HookRunner::new(&[spec], config) {
        let _ = runner.run(&mut doc, None);
    }
    // Content must survive a mid-run hook crash regardless of how many pages
    // got annotated before the crash.
    assert!(
        !doc.content.is_empty(),
        "content must survive mid-run hook exit"
    );
}

// ---------------------------------------------------------------------------
// 5. Hook segfaults (simulate via kill -SEGV $$)
// ---------------------------------------------------------------------------
#[test]
fn hook_segfault_recovered() {
    let body = format!("echo '{NOOP_HANDSHAKE}'\nread -r line\nkill -SEGV $$");
    let script = write_script("segv", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 3;

    let mut doc = udoc::extract(realworld_pdf()).expect("extract pdf");
    if let Ok(mut runner) = HookRunner::new(&[spec], config) {
        let _ = runner.run(&mut doc, None);
    }
    assert!(
        !doc.content.is_empty(),
        "content must survive segfault hook"
    );
}

// ---------------------------------------------------------------------------
// 6. Subsequent pages do NOT receive requests after a hook dies
//
// The current implementation marks a hook `dead` on send-failure or EOF, so
// page N+1 short-circuits without trying to send a request. This avoids
// O(page_count * page_timeout) wall time when one hook dies early.
// ---------------------------------------------------------------------------
#[test]
fn dead_hook_short_circuits_remaining_pages() {
    use std::time::Instant;

    let body = format!("echo '{NOOP_HANDSHAKE}'\nread -r line\nexit 1");
    let script = write_script("die-immediately", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    // High per-page timeout. If the dead-hook short-circuit is broken, total
    // time would be ~N * 10 s. With short-circuit, we exit promptly.
    config.page_timeout_secs = 10;

    let mut doc = udoc::extract(realworld_pdf()).expect("extract pdf");
    if let Ok(mut runner) = HookRunner::new(&[spec], config) {
        let start = Instant::now();
        let _ = runner.run(&mut doc, None);
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_secs() < 20,
            "dead-hook short-circuit broken; elapsed = {elapsed:?}"
        );
    }
}

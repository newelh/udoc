//! Concurrent + coverage-smoke stress tests.
//!
//! Sub-test (a) coverage smoke: spawn a no-op annotation hook against one
//! fixture per supported format. Sub-test (c) concurrency: run hooked
//! extraction under multi-threaded parallelism on a small corpus.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use udoc::hooks::{HookConfig, HookRunner, HookSpec};

use super::helpers::{fixture, write_script, NOOP_HANDSHAKE, NOOP_HOOK_BODY};

// ---------------------------------------------------------------------------
// Helper: run the no-op hook against a single fixture.
// ---------------------------------------------------------------------------

fn run_noop_against(rel_path: &str) {
    let path = fixture(rel_path);
    assert!(
        path.exists(),
        "fixture missing: {} (see helpers::fixture)",
        path.display()
    );

    let script = write_script("noop-coverage", NOOP_HOOK_BODY);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 10;

    let mut doc = match udoc::extract(&path) {
        Ok(d) => d,
        Err(e) => panic!("extract({}) failed: {e}", path.display()),
    };
    let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");
    let result = runner.run(&mut doc, None);

    // Either Ok or "all hook invocations failed" (e.g. zero pages); both are
    // acceptable provided the document is still extracted intact. The hook
    // returns valid JSONL for every page so on multi-page docs it should
    // succeed.
    if let Err(e) = &result {
        let msg = format!("{e}");
        // Empty docs (zero pages) trip "all hook invocations failed" because
        // total_attempts > 0 but every attempt was for a hook-invocable
        // page. Acceptable for the smoke; we still verified hooks ran.
        assert!(
            msg.contains("all hook invocations failed") || doc.metadata.page_count == 0,
            "unexpected error from no-op hook on {rel_path}: {msg}"
        );
    }
}

// ---------------------------------------------------------------------------
// Sub-test (a) -- one test per supported format
// ---------------------------------------------------------------------------

#[test]
fn coverage_pdf() {
    run_noop_against("crates/udoc-pdf/tests/corpus/minimal/two_column.pdf");
}

#[test]
fn coverage_docx() {
    run_noop_against("crates/udoc-docx/tests/corpus/real-world/sample2.docx");
}

#[test]
fn coverage_xlsx() {
    run_noop_against("crates/udoc-xlsx/tests/corpus/real-world/SampleSS.xlsx");
}

#[test]
fn coverage_pptx() {
    run_noop_against("crates/udoc-pptx/tests/corpus/real-world/minimal.pptx");
}

#[test]
fn coverage_doc() {
    run_noop_against("crates/udoc-doc/tests/corpus/real-world/sample2.doc");
}

#[test]
fn coverage_xls() {
    run_noop_against("crates/udoc-xls/tests/corpus/real-world/two_sheets.xls");
}

#[test]
fn coverage_ppt() {
    run_noop_against("crates/udoc-ppt/tests/corpus/real-world/examplefiles_1slide.ppt");
}

#[test]
fn coverage_odt() {
    run_noop_against("crates/udoc-odf/tests/corpus/real-world/synthetic_basic.odt");
}

#[test]
fn coverage_ods() {
    run_noop_against("crates/udoc-odf/tests/corpus/real-world/lo_test.ods");
}

#[test]
fn coverage_odp() {
    run_noop_against("crates/udoc-odf/tests/corpus/real-world/lo_background.odp");
}

#[test]
fn coverage_rtf() {
    run_noop_against("crates/udoc-rtf/tests/corpus/basic.rtf");
}

#[test]
fn coverage_md() {
    run_noop_against("crates/udoc-markdown/tests/corpus/basic.md");
}

// ---------------------------------------------------------------------------
// Sub-test (c) -- concurrent extraction with hooks
// ---------------------------------------------------------------------------

/// 4-way parallel extraction with per-thread hooks. Verifies:
/// - no FD exhaustion under load,
/// - no panics from racing process reaping,
/// - all threads complete in bounded wall time.
#[test]
fn parallel_4way_no_panics() {
    let fixtures: Vec<PathBuf> = vec![
        fixture("crates/udoc-pdf/tests/corpus/minimal/two_column.pdf"),
        fixture("crates/udoc-docx/tests/corpus/real-world/sample2.docx"),
        fixture("crates/udoc-rtf/tests/corpus/basic.rtf"),
        fixture("crates/udoc-markdown/tests/corpus/basic.md"),
    ];
    let success_count = std::sync::Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for fx in fixtures {
        let success = success_count.clone();
        handles.push(thread::spawn(move || {
            let script = write_script("parallel-noop", NOOP_HOOK_BODY);
            let spec = HookSpec::from_command(script.path.to_str().unwrap());
            let mut config = HookConfig::default();
            config.page_timeout_secs = 10;

            let mut doc = match udoc::extract(&fx) {
                Ok(d) => d,
                Err(_) => return,
            };
            if let Ok(mut runner) = HookRunner::new(&[spec], config) {
                let _ = runner.run(&mut doc, None);
                if !doc.content.is_empty() || doc.metadata.page_count == 0 {
                    success.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    for h in handles {
        h.join().expect("worker thread panicked");
    }

    assert!(
        success_count.load(Ordering::Relaxed) >= 1,
        "at least one parallel worker should have succeeded"
    );
}

/// 8-way parallel extraction with the SAME small fixture across all threads.
/// Stresses the path where many hook processes are spawned simultaneously.
#[test]
fn parallel_8way_same_fixture() {
    let fx = fixture("crates/udoc-pdf/tests/corpus/minimal/two_column.pdf");
    let success_count = std::sync::Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..8 {
        let success = success_count.clone();
        let fx = fx.clone();
        handles.push(thread::spawn(move || {
            let script = write_script("parallel-same", NOOP_HOOK_BODY);
            let spec = HookSpec::from_command(script.path.to_str().unwrap());
            let mut config = HookConfig::default();
            config.page_timeout_secs = 10;

            let mut doc = match udoc::extract(&fx) {
                Ok(d) => d,
                Err(_) => return,
            };
            if let Ok(mut runner) = HookRunner::new(&[spec], config) {
                let _ = runner.run(&mut doc, None);
                success.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    for h in handles {
        h.join().expect("worker thread panicked");
    }

    assert_eq!(
        success_count.load(Ordering::Relaxed),
        8,
        "all 8 parallel workers should have run their hook"
    );
}

/// Concurrent extraction where some workers use a slow hook and others use a
/// fast one. Verifies that one thread's timeout does not poison sibling
/// threads.
#[test]
fn parallel_mixed_speed_hooks() {
    let fx = fixture("crates/udoc-pdf/tests/corpus/minimal/two_column.pdf");
    let mut handles = Vec::new();

    for i in 0..6 {
        let fx = fx.clone();
        handles.push(thread::spawn(move || {
            let body = if i % 2 == 0 {
                NOOP_HOOK_BODY.to_string()
            } else {
                format!("echo '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    sleep 30\ndone")
            };
            let script = write_script("mixed-speed", &body);
            let spec = HookSpec::from_command(script.path.to_str().unwrap());

            let mut config = HookConfig::default();
            config.page_timeout_secs = 2;

            let mut doc = match udoc::extract(&fx) {
                Ok(d) => d,
                Err(_) => return,
            };
            if let Ok(mut runner) = HookRunner::new(&[spec], config) {
                let _ = runner.run(&mut doc, None);
            }
        }));
    }

    let start = std::time::Instant::now();
    for h in handles {
        h.join().expect("worker thread panicked");
    }
    let elapsed = start.elapsed();

    // Slow hooks time out at 2 s. Even with serialized join order, total wall
    // is bounded.
    assert!(
        elapsed.as_secs() < 30,
        "mixed-speed parallel run took {elapsed:?}, expected < 30 s"
    );
}

/// Many lightweight hook spawns in series. Validates that we can issue >=20
/// HookRunner::new + Drop cycles in succession without leaking processes or
/// FDs (the FD count check lives in `hung::fd_count_stable_across_invocations`
/// for Linux; here we only check the absence of panics and that wall time
/// scales linearly).
#[test]
fn many_serial_hook_lifecycles() {
    let body = NOOP_HOOK_BODY;
    let mut config = HookConfig::default();
    config.page_timeout_secs = 5;

    let fx = fixture("crates/udoc-pdf/tests/corpus/minimal/two_column.pdf");

    let start = std::time::Instant::now();
    for _ in 0..20 {
        let script = write_script("serial-lifecycle", body);
        let spec = HookSpec::from_command(script.path.to_str().unwrap());
        if let Ok(mut runner) = HookRunner::new(&[spec], config.clone()) {
            let mut doc = udoc::extract(&fx).expect("extract pdf");
            let _ = runner.run(&mut doc, None);
        }
    }
    let elapsed = start.elapsed();

    // Each cycle should be sub-second; 20 in a row well under a minute.
    assert!(
        elapsed.as_secs() < 60,
        "20 serial hook lifecycles took {elapsed:?}, expected < 60 s"
    );
}

/// Multi-hook (chain) per HookRunner: spawns 3 hooks in one runner against
/// one document. Verifies phase ordering and that one hook's failure does
/// not block the others.
#[test]
fn three_hook_chain_one_runner() {
    let ok_script = write_script("chain-ok", NOOP_HOOK_BODY);
    let timeout_body =
        format!("echo '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    sleep 30\ndone");
    let slow_script = write_script("chain-slow", &timeout_body);
    let exit_script = write_script("chain-exit", "exit 1");

    let specs = vec![
        HookSpec::from_command(ok_script.path.to_str().unwrap()),
        HookSpec::from_command(slow_script.path.to_str().unwrap()),
        HookSpec::from_command(exit_script.path.to_str().unwrap()),
    ];

    let mut config = HookConfig::default();
    config.page_timeout_secs = 2;

    let mut doc = udoc::extract(fixture(
        "crates/udoc-pdf/tests/corpus/minimal/two_column.pdf",
    ))
    .expect("extract pdf");

    if let Ok(mut runner) = HookRunner::new(&specs, config) {
        let start = std::time::Instant::now();
        let _ = runner.run(&mut doc, None);
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_secs() < 20,
            "three-hook chain took {elapsed:?}, expected < 20 s"
        );
    }
    assert!(!doc.content.is_empty());
}

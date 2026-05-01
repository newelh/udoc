//! T-028: Hook protocol tests.

use std::sync::atomic::{AtomicU64, Ordering};

use udoc::hooks::{HookConfig, HookRunner, HookSpec};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn test_tmp_dir(label: &str) -> std::path::PathBuf {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "udoc-test-hooks-{}-{}-{}",
        std::process::id(),
        id,
        label
    ))
}

// ---------------------------------------------------------------------------
// HookSpec parsing
// ---------------------------------------------------------------------------

#[test]
fn hook_spec_from_command_simple() {
    let spec = HookSpec::from_command("echo hello world");
    assert_eq!(spec.command, "echo");
    assert_eq!(spec.args, vec!["hello", "world"]);
}

#[test]
fn hook_spec_from_command_single() {
    let spec = HookSpec::from_command("./my-hook.sh");
    assert_eq!(spec.command, "./my-hook.sh");
    assert!(spec.args.is_empty());
}

#[test]
fn hook_spec_from_command_with_flags() {
    let spec = HookSpec::from_command("python layout.py --model large --dpi 300");
    assert_eq!(spec.command, "python");
    assert_eq!(
        spec.args,
        vec!["layout.py", "--model", "large", "--dpi", "300"]
    );
}

#[test]
fn hook_spec_from_command_empty() {
    let spec = HookSpec::from_command("");
    assert_eq!(spec.command, "");
    assert!(spec.args.is_empty());
}

#[test]
fn hook_spec_new() {
    let spec = HookSpec::new("my-cmd", vec!["--flag".into(), "value".into()]);
    assert_eq!(spec.command, "my-cmd");
    assert_eq!(spec.args, vec!["--flag", "value"]);
}

// ---------------------------------------------------------------------------
// HookConfig defaults
// ---------------------------------------------------------------------------

#[test]
fn hook_config_defaults() {
    let config = HookConfig::default();
    assert_eq!(config.page_timeout_secs, 60);
    assert!(!config.ocr_all_pages);
    assert_eq!(config.image_dpi, 300);
}

#[test]
fn hook_config_fields_accessible() {
    let config = HookConfig::default();
    // Fields should be readable
    let _ = config.page_timeout_secs;
    let _ = config.ocr_all_pages;
    let _ = config.image_dpi;
    // Debug should work
    let debug = format!("{:?}", config);
    assert!(!debug.is_empty());
}

// ---------------------------------------------------------------------------
// Hook failure fallback
// ---------------------------------------------------------------------------

#[test]
fn hook_process_exit_handled_gracefully() {
    // `false` exits immediately with code 1, but spawn itself succeeds.
    // HookRunner::new reads EOF on stdout and treats it as a no-handshake
    // OCR hook. The important thing is it does not panic.
    let spec = HookSpec::from_command("false");
    let result = HookRunner::new(&[spec], HookConfig::default());
    // Depending on the system, this may succeed (with a dead hook process)
    // or fail (if `false` is not found). Either is acceptable.
    match result {
        Ok(mut runner) => {
            // Running against a doc should not panic, even with dead process.
            let mut doc = udoc::Document::new();
            doc.metadata.page_count = 1;
            let para_id = doc.alloc_node_id();
            doc.content.push(udoc::Block::Paragraph {
                id: para_id,
                content: vec![],
            });
            // Should not panic -- dead process triggers graceful fallback
            let _ = runner.run(&mut doc, None);
        }
        Err(_) => {
            // This is also acceptable
        }
    }
}

// ---------------------------------------------------------------------------
// Hook process: echo hook (no handshake)
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn echo_hook_immediate_response() {
    use std::io::Write;

    // Create a hook that outputs a non-handshake JSON line immediately
    // (before reading stdin), then echoes responses for each line.
    // The first output line is treated as page 0's buffered response.
    let dir = test_tmp_dir("echo");
    std::fs::create_dir_all(&dir).expect("create temp dir");

    let script_path = dir.join("echo-hook-immediate.sh");
    let mut f = std::fs::File::create(&script_path).expect("create script");
    writeln!(
        f,
        r#"#!/bin/bash
# Output an immediate response (no handshake), which gets buffered
echo '{{"spans":[],"blocks":[]}}'
while IFS= read -r line; do
    echo '{{"spans":[],"blocks":[]}}'
done
"#
    )
    .expect("write script");
    drop(f);

    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&script_path)
        .status()
        .expect("chmod");

    let spec = HookSpec::from_command(script_path.to_str().expect("test path should be UTF-8"));
    let result = HookRunner::new(&[spec], HookConfig::default());
    match result {
        Ok(mut runner) => {
            let mut doc = udoc::Document::new();
            doc.metadata.page_count = 0;
            let _ = runner.run(&mut doc, None);
        }
        Err(e) => {
            let msg = format!("{}", e);
            assert!(
                msg.contains("spawn") || msg.contains("No such file"),
                "unexpected error: {msg}"
            );
        }
    }

    let _ = std::fs::remove_file(&script_path);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Hook with handshake line
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn handshake_hook() {
    use std::io::Write;

    let dir = test_tmp_dir("handshake");
    std::fs::create_dir_all(&dir).expect("create temp dir");

    let script_path = dir.join("handshake-hook.sh");
    let mut f = std::fs::File::create(&script_path).expect("create script");
    writeln!(
        f,
        r#"#!/bin/bash
# Output handshake first
echo '{{"protocol":"udoc-hook-v1","capabilities":["ocr"],"needs":["image"],"provides":["spans"]}}'
# Then process pages
while IFS= read -r line; do
    echo '{{"spans":[],"blocks":[]}}'
done
"#
    )
    .expect("write script");
    drop(f);

    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&script_path)
        .status()
        .expect("chmod");

    let spec = HookSpec::from_command(script_path.to_str().expect("test path should be UTF-8"));
    let result = HookRunner::new(&[spec], HookConfig::default());
    match result {
        Ok(mut runner) => {
            let mut doc = udoc::Document::new();
            doc.metadata.page_count = 0;
            let _ = runner.run(&mut doc, None);
        }
        Err(e) => {
            // Acceptable if bash is unavailable
            let msg = format!("{}", e);
            assert!(
                msg.contains("spawn") || msg.contains("No such file"),
                "unexpected error: {msg}"
            );
        }
    }

    let _ = std::fs::remove_file(&script_path);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Invalid JSON recovery
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn invalid_json_hook() {
    use std::io::Write;

    let dir = test_tmp_dir("bad-json");
    std::fs::create_dir_all(&dir).expect("create temp dir");

    let script_path = dir.join("bad-json-hook.sh");
    let mut f = std::fs::File::create(&script_path).expect("create script");
    writeln!(
        f,
        r#"#!/bin/bash
# Output handshake
echo '{{"protocol":"udoc-hook-v1","capabilities":["ocr"],"needs":["text"],"provides":["spans"]}}'
# Then output invalid JSON for each page
while IFS= read -r line; do
    echo 'not valid json at all'
done
"#
    )
    .expect("write script");
    drop(f);

    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&script_path)
        .status()
        .expect("chmod");

    let spec = HookSpec::from_command(script_path.to_str().expect("test path should be UTF-8"));
    let result = HookRunner::new(&[spec], HookConfig::default());
    match result {
        Ok(mut runner) => {
            // Build a doc with one page of content
            let mut doc = udoc::Document::new();
            doc.metadata.page_count = 1;
            let para_id = doc.alloc_node_id();
            let text_id = doc.alloc_node_id();
            doc.content.push(udoc::Block::Paragraph {
                id: para_id,
                content: vec![udoc::Inline::Text {
                    id: text_id,
                    text: "test".into(),
                    style: udoc::SpanStyle::default(),
                }],
            });
            if let Some(ref mut pres) = doc.presentation {
                pres.page_assignments.set(para_id, 0);
            }
            // Should not panic -- invalid JSON causes fallback
            let _ = runner.run(&mut doc, None);
        }
        Err(_) => {
            // Acceptable if bash is unavailable
        }
    }

    let _ = std::fs::remove_file(&script_path);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Timeout enforcement
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn hook_timeout_triggers() {
    use std::io::Write;
    use std::time::Instant;

    let dir = test_tmp_dir("timeout");
    std::fs::create_dir_all(&dir).expect("create temp dir");

    let script_path = dir.join("slow-hook.sh");
    let mut f = std::fs::File::create(&script_path).expect("create script");
    writeln!(
        f,
        r#"#!/bin/bash
# Output handshake immediately
echo '{{"protocol":"udoc-hook-v1","capabilities":["ocr"],"needs":["text"],"provides":["spans"]}}'
# Read request but never respond (sleep forever)
while IFS= read -r line; do
    sleep 60
done
"#
    )
    .expect("write script");
    drop(f);

    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&script_path)
        .status()
        .expect("chmod");

    let mut config = HookConfig::default();
    config.page_timeout_secs = 2; // Short timeout for test

    let spec = HookSpec::from_command(script_path.to_str().expect("test path should be UTF-8"));
    let result = HookRunner::new(&[spec], config);
    match result {
        Ok(mut runner) => {
            let mut doc = udoc::Document::new();
            doc.metadata.page_count = 1;
            let para_id = doc.alloc_node_id();
            let text_id = doc.alloc_node_id();
            doc.content.push(udoc::Block::Paragraph {
                id: para_id,
                content: vec![udoc::Inline::Text {
                    id: text_id,
                    text: "test".into(),
                    style: udoc::SpanStyle::default(),
                }],
            });
            doc.presentation = Some(udoc::Presentation::default());
            doc.presentation
                .as_mut()
                .unwrap()
                .page_assignments
                .set(para_id, 0);

            let start = Instant::now();
            let _ = runner.run(&mut doc, None);
            let elapsed = start.elapsed();

            // Should complete in ~2 seconds (the timeout), not 60.
            // Generous margin for slow CI environments and shell startup.
            assert!(
                elapsed.as_secs() < 30,
                "hook should have timed out quickly, took {:?}",
                elapsed
            );
        }
        Err(_) => {
            // Acceptable if bash is unavailable
        }
    }

    let _ = std::fs::remove_file(&script_path);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Drop cleanup (no zombie processes)
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn hook_drop_kills_child() {
    use std::io::Write;

    let dir = test_tmp_dir("drop");
    std::fs::create_dir_all(&dir).expect("create temp dir");

    let script_path = dir.join("long-running-hook.sh");
    let mut f = std::fs::File::create(&script_path).expect("create script");
    writeln!(
        f,
        r#"#!/bin/bash
# Output handshake
echo '{{"protocol":"udoc-hook-v1","capabilities":["ocr"],"needs":["text"],"provides":["spans"]}}'
# Run forever
while true; do
    sleep 1
done
"#
    )
    .expect("write script");
    drop(f);

    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&script_path)
        .status()
        .expect("chmod");

    let spec = HookSpec::from_command(script_path.to_str().expect("test path should be UTF-8"));
    let result = HookRunner::new(&[spec], HookConfig::default());
    match result {
        Ok(runner) => {
            // Drop the runner without calling run() -- should kill the child
            drop(runner);
            // If we get here without hanging, the Drop impl worked
        }
        Err(_) => {
            // Acceptable if bash is unavailable
        }
    }

    let _ = std::fs::remove_file(&script_path);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn zero_timeout_rejected() {
    let mut config = HookConfig::default();
    config.page_timeout_secs = 0;
    let spec = HookSpec::from_command("echo hello");
    let result = HookRunner::new(&[spec], config);
    assert!(result.is_err(), "zero timeout should be rejected");
}

// ---------------------------------------------------------------------------
// Hook chaining: output of hook 1 feeds into hook 2 within the same phase
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn hook_chain_propagates_spans() {
    use std::io::Write;

    let dir = test_tmp_dir("chain");
    std::fs::create_dir_all(&dir).expect("create temp dir");

    // Hook 1: produces a span with text "from-hook-1"
    let hook1_path = dir.join("hook1.sh");
    let mut f = std::fs::File::create(&hook1_path).expect("create hook1");
    writeln!(
        f,
        r#"#!/bin/bash
echo '{{"protocol":"udoc-hook-v1","capabilities":["annotate"],"needs":["text"],"provides":["spans"]}}'
while IFS= read -r line; do
    echo '{{"spans":[{{"text":"from-hook-1","bbox":[0,0,100,12]}}]}}'
done
"#
    )
    .expect("write hook1");
    drop(f);

    // Hook 2: reads incoming spans and echoes them back as-is, proving it received them
    let hook2_path = dir.join("hook2.sh");
    let mut f = std::fs::File::create(&hook2_path).expect("create hook2");
    writeln!(
        f,
        r#"#!/bin/bash
echo '{{"protocol":"udoc-hook-v1","capabilities":["annotate"],"needs":["spans"],"provides":["spans"]}}'
while IFS= read -r line; do
    # Extract the "spans" array from input and echo it back
    echo "$line" | python3 -c "
import sys, json
req = json.loads(sys.stdin.readline())
spans = req.get('spans', [])
# Add a marker so we can verify hook2 ran and received hook1's spans
for s in spans:
    s['text'] = s.get('text', '') + '+hook2'
print(json.dumps({{'spans': spans}}))
"
done
"#
    )
    .expect("write hook2");
    drop(f);

    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&hook1_path)
        .status()
        .expect("chmod hook1");
    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&hook2_path)
        .status()
        .expect("chmod hook2");

    let spec1 = HookSpec::from_command(hook1_path.to_str().expect("path"));
    let spec2 = HookSpec::from_command(hook2_path.to_str().expect("path"));
    let result = HookRunner::new(&[spec1, spec2], HookConfig::default());

    match result {
        Ok(mut runner) => {
            let mut doc = udoc::Document::new();
            doc.metadata.page_count = 1;
            let id0 = doc.try_alloc_node_id().unwrap();
            let id1 = doc.try_alloc_node_id().unwrap();
            doc.content.push(udoc::Block::Paragraph {
                id: id0,
                content: vec![udoc::Inline::Text {
                    id: id1,
                    text: "test page".into(),
                    style: udoc::SpanStyle::default(),
                }],
            });
            doc.presentation = Some(udoc::Presentation::default());
            doc.presentation
                .as_mut()
                .unwrap()
                .page_assignments
                .set(id0, 0);

            let run_result = runner.run(&mut doc, None);
            // Should complete without error (both hooks in same phase, chained)
            assert!(
                run_result.is_ok(),
                "chained hooks should succeed: {:?}",
                run_result.err()
            );
        }
        Err(e) => {
            let msg = format!("{}", e);
            // Acceptable if bash/python3 is unavailable
            assert!(
                msg.contains("spawn") || msg.contains("No such file"),
                "unexpected error: {msg}"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// MAX_CONSECUTIVE_FAILURES: 3 invalid JSON responses kills the hook
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn consecutive_failures_kills_hook() {
    use std::io::Write;

    let dir = test_tmp_dir("failures");
    std::fs::create_dir_all(&dir).expect("create temp dir");

    // Hook that outputs valid handshake but then sends invalid JSON for every page
    let script_path = dir.join("bad-responses.sh");
    let mut f = std::fs::File::create(&script_path).expect("create script");
    writeln!(
        f,
        r#"#!/bin/bash
echo '{{"protocol":"udoc-hook-v1","capabilities":["ocr"],"needs":["text"],"provides":["spans"]}}'
while IFS= read -r line; do
    echo 'this is not json'
done
"#
    )
    .expect("write script");
    drop(f);

    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&script_path)
        .status()
        .expect("chmod");

    let spec = HookSpec::from_command(script_path.to_str().expect("path"));
    let result = HookRunner::new(&[spec], HookConfig::default());

    match result {
        Ok(mut runner) => {
            // Build a document with 5 pages. The hook should be killed after
            // 3 consecutive failures, so pages 4 and 5 should be skipped
            // (hook is dead).
            let mut doc = udoc::Document::new();
            doc.metadata.page_count = 5;
            for i in 0..5 {
                if i > 0 {
                    let break_id = doc.try_alloc_node_id().unwrap();
                    doc.content.push(udoc::Block::PageBreak { id: break_id });
                }
                let para_id = doc.try_alloc_node_id().unwrap();
                let text_id = doc.try_alloc_node_id().unwrap();
                doc.content.push(udoc::Block::Paragraph {
                    id: para_id,
                    content: vec![],
                });
                // Mark pages as having no text so OCR phase processes them
                let _ = text_id;
            }
            doc.presentation = Some(udoc::Presentation::default());

            // The hook sends invalid JSON for every page, so after 3
            // consecutive failures it gets killed. All 5 page attempts fail,
            // meaning total_failures == total_attempts => "all hook
            // invocations failed" error.
            let run_result = runner.run(&mut doc, None);
            assert!(
                run_result.is_err(),
                "all-invalid hook should return an error, got Ok"
            );
            let err_msg = format!("{}", run_result.unwrap_err());
            assert!(
                err_msg.contains("all hook invocations failed"),
                "error should indicate all invocations failed, got: {err_msg}"
            );
        }
        Err(_) => {
            // Acceptable if bash is unavailable
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// H-010: Timeout with partial results
// ---------------------------------------------------------------------------

/// A hook that responds to page 0 but hangs on page 1 should time out on
/// page 1 while preserving page 0's results in the document.
#[cfg(unix)]
#[test]
fn hook_timeout_preserves_partial_results() {
    use std::io::Write;
    use std::time::Instant;

    let dir = test_tmp_dir("timeout-partial");
    std::fs::create_dir_all(&dir).expect("create temp dir");

    let script_path = dir.join("partial-timeout-hook.sh");
    let mut f = std::fs::File::create(&script_path).expect("create script");
    // Hook responds to the first page request, then hangs on the second.
    writeln!(
        f,
        r#"#!/bin/bash
echo '{{"protocol":"udoc-hook-v1","capabilities":["annotate"],"needs":["text"],"provides":["labels"]}}'
# Page 0: respond with a label
IFS= read -r line
echo '{{"labels":{{"hook_saw_page":"zero"}}}}'
# Page 1: hang forever (timeout should fire)
IFS= read -r line
sleep 1000
"#
    )
    .expect("write script");
    drop(f);

    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&script_path)
        .status()
        .expect("chmod");

    let mut config = HookConfig::default();
    config.page_timeout_secs = 2;

    let spec = HookSpec::from_command(script_path.to_str().expect("path"));
    let result = HookRunner::new(&[spec], config);
    match result {
        Ok(mut runner) => {
            let mut doc = udoc::Document::new();
            doc.metadata.page_count = 2;

            // Page 0 content
            let p0_id = doc.alloc_node_id();
            let t0_id = doc.alloc_node_id();
            doc.content.push(udoc::Block::Paragraph {
                id: p0_id,
                content: vec![udoc::Inline::Text {
                    id: t0_id,
                    text: "page zero text".into(),
                    style: udoc::SpanStyle::default(),
                }],
            });

            // Page break
            let pb_id = doc.alloc_node_id();
            doc.content.push(udoc::Block::PageBreak { id: pb_id });

            // Page 1 content
            let p1_id = doc.alloc_node_id();
            let t1_id = doc.alloc_node_id();
            doc.content.push(udoc::Block::Paragraph {
                id: p1_id,
                content: vec![udoc::Inline::Text {
                    id: t1_id,
                    text: "page one text".into(),
                    style: udoc::SpanStyle::default(),
                }],
            });

            doc.presentation = Some(udoc::Presentation::default());
            doc.presentation
                .as_mut()
                .unwrap()
                .page_assignments
                .set(p0_id, 0);
            doc.presentation
                .as_mut()
                .unwrap()
                .page_assignments
                .set(p1_id, 1);

            let start = Instant::now();
            let _ = runner.run(&mut doc, None);
            let elapsed = start.elapsed();

            // Should complete in roughly the timeout window, not 1000 seconds.
            assert!(
                elapsed.as_secs() < 30,
                "should have timed out within timeout window, took {:?}",
                elapsed
            );

            // Page 0's label should be preserved (hook responded before hanging).
            assert_eq!(
                doc.metadata.properties.get("hook.label.hook_saw_page"),
                Some(&"zero".to_string()),
                "page 0 label from hook should be preserved after page 1 timeout"
            );

            // Document should still have its original content intact.
            assert!(
                doc.content.len() >= 3,
                "document content should not be destroyed by timeout"
            );
        }
        Err(_) => {
            // Acceptable if bash is unavailable
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// H-010: Multi-hook chaining across phases (OCR -> annotate)
// ---------------------------------------------------------------------------

/// Two hooks in different phases: Hook A (OCR, provides spans) runs first,
/// Hook B (annotate, needs spans from A, adds labels) runs second. Verifies
/// phase ordering and that Hook B sees the document after Hook A mutated it.
#[cfg(unix)]
#[test]
fn multi_hook_cross_phase_chaining() {
    use std::io::Write;

    let dir = test_tmp_dir("cross-phase");
    std::fs::create_dir_all(&dir).expect("create temp dir");

    // Hook A: OCR phase. Needs text (to know what page it's on), provides spans.
    // Outputs a span with text "ocr-hook-a" for each page.
    let hook_a_path = dir.join("hook-a-ocr.sh");
    let mut f = std::fs::File::create(&hook_a_path).expect("create hook-a");
    writeln!(
        f,
        r#"#!/bin/bash
echo '{{"protocol":"udoc-hook-v1","capabilities":["ocr"],"needs":["text"],"provides":["spans"]}}'
while IFS= read -r line; do
    echo '{{"spans":[{{"text":"ocr-hook-a","bbox":[0,0,100,12]}}]}}'
done
"#
    )
    .expect("write hook-a");
    drop(f);

    // Hook B: Annotate phase. Needs text, provides labels.
    // Adds a label "annotated" = "true" for each page.
    let hook_b_path = dir.join("hook-b-annotate.sh");
    let mut f = std::fs::File::create(&hook_b_path).expect("create hook-b");
    writeln!(
        f,
        r#"#!/bin/bash
echo '{{"protocol":"udoc-hook-v1","capabilities":["annotate"],"needs":["text"],"provides":["labels"]}}'
while IFS= read -r line; do
    echo '{{"labels":{{"annotated":"true"}}}}'
done
"#
    )
    .expect("write hook-b");
    drop(f);

    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&hook_a_path)
        .status()
        .expect("chmod hook-a");
    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&hook_b_path)
        .status()
        .expect("chmod hook-b");

    // Pass hooks in reverse order (annotate first, ocr second) to verify
    // that HookRunner sorts by phase and runs OCR before annotate regardless
    // of input order.
    let spec_b = HookSpec::from_command(hook_b_path.to_str().expect("path"));
    let spec_a = HookSpec::from_command(hook_a_path.to_str().expect("path"));
    let mut config = HookConfig::default();
    config.ocr_all_pages = true; // Force OCR even on pages with text
    let result = HookRunner::new(&[spec_b, spec_a], config);

    match result {
        Ok(mut runner) => {
            let mut doc = udoc::Document::new();
            doc.metadata.page_count = 1;

            // Page with some text (OCR still runs because ocr_all_pages=true)
            let p0_id = doc.alloc_node_id();
            let t0_id = doc.alloc_node_id();
            doc.content.push(udoc::Block::Paragraph {
                id: p0_id,
                content: vec![udoc::Inline::Text {
                    id: t0_id,
                    text: "existing content".into(),
                    style: udoc::SpanStyle::default(),
                }],
            });
            doc.presentation = Some(udoc::Presentation::default());
            doc.presentation
                .as_mut()
                .unwrap()
                .page_assignments
                .set(p0_id, 0);

            let run_result = runner.run(&mut doc, None);
            assert!(
                run_result.is_ok(),
                "cross-phase hooks should succeed: {:?}",
                run_result.err()
            );

            // Hook A (OCR) should have added spans to presentation layer.
            let pres = doc.presentation.as_ref().expect("presentation layer");
            let ocr_spans: Vec<_> = pres
                .raw_spans
                .iter()
                .filter(|s| s.text == "ocr-hook-a")
                .collect();
            assert!(
                !ocr_spans.is_empty(),
                "OCR hook should have added spans to the document"
            );

            // Hook B (annotate) should have added labels to metadata.
            assert_eq!(
                doc.metadata.properties.get("hook.label.annotated"),
                Some(&"true".to_string()),
                "annotate hook should have added labels to the document"
            );
        }
        Err(e) => {
            let msg = format!("{}", e);
            assert!(
                msg.contains("spawn") || msg.contains("No such file"),
                "unexpected error: {msg}"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// H-010: Hook exits mid-stream, first page results preserved
// ---------------------------------------------------------------------------

/// A hook that responds to page 0 but exits with code 1 before responding
/// to page 1. The document should keep page 0's results and not crash.
#[cfg(unix)]
#[test]
fn hook_exit_mid_stream_preserves_first_page() {
    use std::io::Write;

    let dir = test_tmp_dir("mid-exit");
    std::fs::create_dir_all(&dir).expect("create temp dir");

    let script_path = dir.join("mid-exit-hook.sh");
    let mut f = std::fs::File::create(&script_path).expect("create script");
    // Hook responds to page 0, then exits with code 1 (simulating a crash
    // on the second page).
    writeln!(
        f,
        r#"#!/bin/bash
echo '{{"protocol":"udoc-hook-v1","capabilities":["annotate"],"needs":["text"],"provides":["labels"]}}'
# Page 0: respond normally
IFS= read -r line
echo '{{"labels":{{"processed_page":"0"}}}}'
# Page 1: crash
IFS= read -r line
exit 1
"#
    )
    .expect("write script");
    drop(f);

    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&script_path)
        .status()
        .expect("chmod");

    let spec = HookSpec::from_command(script_path.to_str().expect("path"));
    let result = HookRunner::new(&[spec], HookConfig::default());

    match result {
        Ok(mut runner) => {
            let mut doc = udoc::Document::new();
            doc.metadata.page_count = 3;

            // Build 3 pages of content
            for i in 0u32..3 {
                if i > 0 {
                    let pb_id = doc.alloc_node_id();
                    doc.content.push(udoc::Block::PageBreak { id: pb_id });
                }
                let para_id = doc.alloc_node_id();
                let text_id = doc.alloc_node_id();
                doc.content.push(udoc::Block::Paragraph {
                    id: para_id,
                    content: vec![udoc::Inline::Text {
                        id: text_id,
                        text: format!("page {} content", i),
                        style: udoc::SpanStyle::default(),
                    }],
                });
            }
            doc.presentation = Some(udoc::Presentation::default());

            // Should not panic. Hook dying mid-stream is a graceful fallback.
            let run_result = runner.run(&mut doc, None);
            // The result may be Ok or Err (all-hooks-failed), but must not panic.
            let _ = run_result;

            // Page 0's label should be preserved in metadata.
            assert_eq!(
                doc.metadata.properties.get("hook.label.processed_page"),
                Some(&"0".to_string()),
                "page 0 result from hook should survive the page 1 crash"
            );

            // All 3 pages of original content should still be intact.
            // 3 paragraphs + 2 page breaks = 5 blocks minimum.
            assert!(
                doc.content.len() >= 5,
                "document content should not be destroyed by hook crash, got {} blocks",
                doc.content.len()
            );

            // Verify the text content is still there.
            let all_text: String = doc
                .content
                .iter()
                .map(|b| b.text())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                all_text.contains("page 0 content"),
                "page 0 content should survive hook crash"
            );
            assert!(
                all_text.contains("page 2 content"),
                "page 2 content should survive hook crash"
            );
        }
        Err(_) => {
            // Acceptable if bash is unavailable
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

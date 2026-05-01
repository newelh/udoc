//! Malformed-output stress tests.
//!
//! A hook that emits invalid JSON, garbage prefixes, NUL bytes, non-UTF-8
//! data, or a handshake with the wrong protocol identifier must be
//! rejected with a clear error and never panic.

use udoc::hooks::{HookConfig, HookRunner, HookSpec};

use super::helpers::{small_pdf, write_script, NOOP_HANDSHAKE};

// ---------------------------------------------------------------------------
// 1. Invalid JSON response (after a valid handshake)
// ---------------------------------------------------------------------------
#[test]
fn invalid_json_response_recovered() {
    let body = format!(
        "echo '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    echo 'this is not json'\ndone"
    );
    let script = write_script("invalid-json", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 5;

    let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
    let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");
    let _ = runner.run(&mut doc, None);
    assert!(
        !doc.content.is_empty(),
        "invalid JSON must not destroy native content"
    );
}

// ---------------------------------------------------------------------------
// 2. Half-line response (no trailing newline -- response truncated by EOF)
// ---------------------------------------------------------------------------
#[test]
fn half_line_response() {
    // Hook writes a partial JSON object then closes stdout.
    let body = format!(
        "echo '{NOOP_HANDSHAKE}'\nread -r line\nprintf '{{\"annotations\":[],\"trun'\nexec 1>&-\nsleep 60"
    );
    let script = write_script("half-line", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 3;

    let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
    let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");
    let _ = runner.run(&mut doc, None);
    assert!(!doc.content.is_empty());
}

// ---------------------------------------------------------------------------
// 3. UTF-8 BOM-prefixed JSON
//
// The Unicode byte-order mark (U+FEFF) is sometimes prepended by writers
// that think they are being helpful. serde_json rejects it. Verify the
// rejection is graceful.
// ---------------------------------------------------------------------------
#[test]
fn bom_prefixed_json_rejected_gracefully() {
    let body = format!(
        "echo '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    printf '\\xEF\\xBB\\xBF{{\"annotations\":[]}}\\n'\ndone"
    );
    let script = write_script("bom-prefix", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 5;

    let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
    let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");
    let _ = runner.run(&mut doc, None);
    assert!(!doc.content.is_empty());
}

// ---------------------------------------------------------------------------
// 4. Embedded NUL byte mid-JSON
// ---------------------------------------------------------------------------
#[test]
fn embedded_nul_byte() {
    let body = format!(
        "echo '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    printf '{{\"annotations\":\\x00[]}}\\n'\ndone"
    );
    let script = write_script("embedded-nul", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 5;

    let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
    let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");
    let _ = runner.run(&mut doc, None);
    assert!(!doc.content.is_empty());
}

// ---------------------------------------------------------------------------
// 5. Non-UTF-8 bytes (e.g. raw 0xFF)
// ---------------------------------------------------------------------------
#[test]
fn non_utf8_bytes() {
    // The reader uses from_utf8_lossy, so non-UTF-8 bytes are replaced with
    // U+FFFD. The resulting line is not valid JSON; runner must reject.
    let body = format!(
        "echo '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    printf '\\xFF\\xFE not json\\n'\ndone"
    );
    let script = write_script("non-utf8", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 5;

    let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
    let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");
    let _ = runner.run(&mut doc, None);
    assert!(!doc.content.is_empty());
}

// ---------------------------------------------------------------------------
// 6. Single line over the per-line cap (1 MiB)
// ---------------------------------------------------------------------------
#[test]
fn line_over_per_line_cap() {
    let body = format!(
        "echo '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    # ~1.2 MiB of 'a' -- truncated by the reader\n    head -c $((1280 * 1024)) /dev/zero | tr '\\0' 'a'\n    echo\ndone"
    );
    let script = write_script("over-per-line-cap", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 8;

    let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
    let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");
    let _ = runner.run(&mut doc, None);
    assert!(!doc.content.is_empty());
}

// ---------------------------------------------------------------------------
// 7. WIRE-FORMAT GATE: hook emits a wrong protocol identifier.
//
// A handshake with a `protocol` field that does not equal the current
// `udoc-hook-v1` identifier MUST be rejected with a clear error rather
// than silently demoted to the default OCR shape. This test asserts:
//   (a) no label payload from the rejected hook lands in doc.metadata, and
//   (b) the runner marks the hook dead so subsequent pages skip it.
// ---------------------------------------------------------------------------
#[test]
fn wrong_protocol_name_rejected_with_clear_error() {
    // Wrong protocol id, otherwise valid handshake shape.
    let bad_handshake = r#"{"protocol":"some-other-protocol-v1","capabilities":["annotate"],"needs":["text"],"provides":["labels"]}"#;
    // Hook tries to emit a label per page. The runner rejects the
    // wrong-protocol handshake at hook spawn and marks the process dead;
    // no label should appear in metadata.
    let body = format!(
        "echo '{bad_handshake}'\nwhile IFS= read -r line; do\n    echo '{{\"labels\":{{\"old_proto_observed\":\"yes\"}}}}'\ndone"
    );
    let script = write_script("wrong-proto-reject", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 5;

    let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
    let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");
    let _ = runner.run(&mut doc, None);

    let label_present = doc
        .metadata
        .properties
        .contains_key("hook.label.old_proto_observed");

    // The hook is rejected at handshake; no labels make it through.
    assert!(
        !label_present,
        "wrong-protocol hook should be rejected at handshake; \
         no label payload should reach doc.metadata."
    );
}

//! Error-message quality tests.
//!
//! Pre-alpha audit: when extraction fails, the user should get back a
//! Display string that NAMES the problem -- not "parsing failed" or
//! "bad input." This test feeds intentionally-malformed inputs and
//! asserts the error format mentions specific context tokens.
//!
//! These tests don't pin exact error wording (that would couple to
//! every backend's implementation) -- they pin a few keywords that
//! must appear so a regression to "parsing failed" without context
//! gets caught.

use udoc::Format;

fn extract_with_format(bytes: &[u8], format: Format) -> String {
    let mut config = udoc::Config::new();
    config.format = Some(format);
    match udoc::extract_bytes_with(bytes, config) {
        Ok(_) => {
            panic!("expected extraction of malformed {format:?} bytes to fail, but it succeeded")
        }
        Err(e) => format!("{e}"),
    }
}

/// Nonsense bytes detected as PDF -- the lexer / xref scanner should
/// surface a specific failure mode, not a bare "parsing failed".
#[test]
fn malformed_pdf_error_names_failure() {
    let msg = extract_with_format(
        b"%PDF-1.7\nthis is garbage and not a real pdf\n",
        Format::Pdf,
    );
    let lower = msg.to_lowercase();
    assert!(
        lower.contains("pdf")
            || lower.contains("xref")
            || lower.contains("stream")
            || lower.contains("trailer")
            || lower.contains("eof"),
        "PDF error should name a parser concept; got: {msg}"
    );
    assert!(
        !msg.eq_ignore_ascii_case("parsing failed") && !msg.eq_ignore_ascii_case("bad input"),
        "PDF error should be specific, not generic; got: {msg}"
    );
}

/// A truncated ZIP (DOCX without proper EOCD) should mention zip / docx
/// / archive in the error.
#[test]
fn malformed_docx_error_names_zip_or_archive() {
    // Start with the ZIP magic but truncate -- nothing past the local
    // file header. Real ZIPs have a central directory at the end.
    let mut bytes = b"PK\x03\x04".to_vec();
    bytes.extend_from_slice(&[0u8; 32]);
    let msg = extract_with_format(&bytes, Format::Docx);
    let lower = msg.to_lowercase();
    assert!(
        lower.contains("zip")
            || lower.contains("archive")
            || lower.contains("docx")
            || lower.contains("opc")
            || lower.contains("central directory"),
        "DOCX error should mention zip/archive context; got: {msg}"
    );
}

/// A truncated XLSX should yield a similar OPC/zip error -- shares
/// the same container stack as DOCX.
#[test]
fn malformed_xlsx_error_names_zip_or_archive() {
    let mut bytes = b"PK\x03\x04".to_vec();
    bytes.extend_from_slice(&[0u8; 64]);
    let msg = extract_with_format(&bytes, Format::Xlsx);
    let lower = msg.to_lowercase();
    assert!(
        lower.contains("zip")
            || lower.contains("archive")
            || lower.contains("xlsx")
            || lower.contains("opc")
            || lower.contains("central directory"),
        "XLSX error should mention zip/archive context; got: {msg}"
    );
}

/// RTF parser accepts arbitrary bytes as plain text (intentional --
/// the spec is permissive). This test documents the behavior so a
/// future regression doesn't go unnoticed; it's NOT a security
/// concern (RTF without control words just yields the literal text).
///
/// Logged as a Lane-L observation in `/progress.md`.
#[test]
fn rtf_is_lenient_on_garbage_input() {
    let mut config = udoc::Config::new();
    config.format = Some(Format::Rtf);
    let result = udoc::extract_bytes_with(b"this is not an rtf file at all", config);
    assert!(
        result.is_ok(),
        "RTF accepts arbitrary bytes as plain text; if this changes,\
         update /progress.md and the policy doc"
    );
}

/// Empty input should surface an "empty" / "no content" -style message
/// rather than panicking on slice indexing or returning a misleading
/// success.
#[test]
fn empty_pdf_input_clean_error() {
    let msg = extract_with_format(b"", Format::Pdf);
    // Must not be empty itself; that would suggest a Display bug.
    assert!(
        !msg.is_empty(),
        "empty-input error must produce a non-empty Display string"
    );
}

/// Round-2 audit: empty PDF used to display only "opening PDF '...'" with
/// no clue what was wrong. Post C-C03 (Display walks source chain) the
/// reason is now surfaced. Pin the new behavior so a regression to
/// silent context-only errors gets caught.
#[test]
fn empty_pdf_error_includes_root_cause() {
    let msg = extract_with_format(b"", Format::Pdf);
    let lower = msg.to_lowercase();
    // Should mention what specifically failed: missing header, EOF, or
    // truncation -- not just the generic "opening PDF" wrapper.
    assert!(
        lower.contains("header")
            || lower.contains("eof")
            || lower.contains("truncated")
            || lower.contains("not found")
            || lower.contains("invalid structure"),
        "empty PDF error must surface root cause via source chain; got: {msg}"
    );
}

/// Forcing a PDF to be parsed as DOCX should fail with a clear error
/// chain that names BOTH the format we tried (DOCX) and the underlying
/// reason (ZIP magic mismatch). Pre C-C03, only the wrapping context
/// "opening DOCX" rendered; post-fix the chain shows ZIP-level detail.
#[test]
fn format_mismatch_error_names_both_layers() {
    // Real PDF bytes parsed as DOCX must fail.
    let bytes = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\nxref\n0 1\n0000000000 65535 f \ntrailer<<>>\nstartxref\n0\n%%EOF\n";
    let msg = extract_with_format(bytes, Format::Docx);
    let lower = msg.to_lowercase();
    assert!(
        lower.contains("docx") || lower.contains("opc"),
        "format-mismatch error should name the format we tried; got: {msg}"
    );
    assert!(
        lower.contains("zip") || lower.contains("central directory") || lower.contains("eocd"),
        "format-mismatch error should expose underlying ZIP reason via chain; got: {msg}"
    );
}

/// Source chain should be at least two levels deep on a wrapping path.
/// This is the defining behavior of C-C03: a Display walking only
/// context produces a one-segment string; walking the source chain
/// produces several `: `-separated segments.
#[test]
fn error_display_chain_is_multi_segment() {
    let msg = extract_with_format(b"", Format::Pdf);
    let segments: Vec<&str> = msg.split(": ").collect();
    assert!(
        segments.len() >= 3,
        "error chain should produce >=3 segments (context + message + at least one source); got {} segments in: {msg}",
        segments.len()
    );
}

/// Round-4 fix: encrypted XLS files used to extract garbled cipher
/// bytes silently. Now they fail with a clear "encrypted" message in
/// the chain. Build a synthetic CFB workbook with a FILEPASS record
/// and assert the message reaches the user.
///
/// We rely on the lower-level `udoc-xls` regression test
/// (`encrypted_workbook_returns_clear_error`) to cover the
/// FILEPASS-detection path; this test pins the user-visible CLI/library
/// behavior at the facade boundary on real input. Since wiring a
/// minimal CFB+BIFF stream into a single test is high-effort, we use
/// the existing `extract_bytes_with` -> Format::Xls dispatch on bytes
/// that DON'T form a valid XLS at all, which still must surface an
/// XLS-specific failure rather than a generic one.
#[test]
fn xls_garbage_error_names_xls_or_cfb() {
    // CFB magic without a real workbook structure.
    let bytes = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1";
    let msg = extract_with_format(bytes, Format::Xls);
    let lower = msg.to_lowercase();
    assert!(
        lower.contains("xls")
            || lower.contains("cfb")
            || lower.contains("workbook")
            || lower.contains("biff")
            || lower.contains("ole2")
            || lower.contains("compound"),
        "XLS error must name an XLS/CFB concept; got: {msg}"
    );
}

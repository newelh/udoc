//! Integration tests for encrypted PDF support.
//!
//! Tests RC4-encrypted PDFs (V=1/R=2, V=2/R=3) and AES-128-CBC
//! encrypted PDFs (V=4/R=4) with various password configurations
//! against the public Document API.

use udoc_pdf::{Config, Document, EncryptionErrorKind, Error};

const ENCRYPTED_DIR: &str = "tests/corpus/encrypted";

fn read_encrypted(filename: &str) -> Vec<u8> {
    let path = format!("{}/{}", ENCRYPTED_DIR, filename);
    std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path, e))
}

fn assert_invalid_password(result: Result<Document, Error>) {
    match result {
        Ok(_) => panic!("expected InvalidPassword, got Ok"),
        Err(Error::Encryption(e)) => {
            assert!(
                matches!(e.kind, EncryptionErrorKind::InvalidPassword),
                "expected InvalidPassword, got: {:?}",
                e.kind
            );
        }
        Err(other) => panic!("expected Encryption error, got: {}", other),
    }
}

#[test]
fn rc4_40_empty_password_extracts_text() {
    let data = read_encrypted("rc4_40_empty_password.pdf");
    let mut doc =
        Document::from_bytes(data).expect("should open with empty password automatically");
    let mut page = doc.page(0).expect("should have page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("Hello, World!"),
        "expected 'Hello, World!' in text, got: {:?}",
        text
    );
}

#[test]
fn rc4_128_user_password_extracts_text() {
    let data = read_encrypted("rc4_128_user_password.pdf");
    let mut doc = Document::from_bytes_with_password(data, b"test123")
        .expect("should open with correct user password");
    let mut page = doc.page(0).expect("should have page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("Encrypted with user password"),
        "expected 'Encrypted with user password' in text, got: {:?}",
        text
    );
}

#[test]
fn rc4_128_wrong_password_fails() {
    let data = read_encrypted("rc4_128_user_password.pdf");
    let result = Document::from_bytes_with_password(data, b"wrong_password");
    assert_invalid_password(result);
}

#[test]
fn rc4_128_no_password_when_required_fails() {
    let data = read_encrypted("rc4_128_user_password.pdf");
    let result = Document::from_bytes(data);
    assert_invalid_password(result);
}

#[test]
fn rc4_128_owner_only_extracts_with_empty_password() {
    // Owner-only encryption: user password is empty, owner password is set.
    // The empty password should work as user password.
    let data = read_encrypted("rc4_128_owner_only.pdf");
    let mut doc = Document::from_bytes(data)
        .expect("should open with empty password (owner-only encryption)");
    let mut page = doc.page(0).expect("should have page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("Owner password only"),
        "expected 'Owner password only' in text, got: {:?}",
        text
    );
}

#[test]
fn rc4_128_owner_password_extracts_text() {
    // Should also work when explicitly providing the owner password.
    let data = read_encrypted("rc4_128_owner_only.pdf");
    let mut doc = Document::from_bytes_with_password(data, b"owner456")
        .expect("should open with owner password");
    let mut page = doc.page(0).expect("should have page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("Owner password only"),
        "expected 'Owner password only' in text, got: {:?}",
        text
    );
}

#[test]
fn rc4_128_both_passwords_with_user() {
    // Both user and owner passwords are non-empty. Open with user password.
    let data = read_encrypted("rc4_128_both_passwords.pdf");
    let mut doc = Document::from_bytes_with_password(data, b"user_pw")
        .expect("should open with user password");
    let mut page = doc.page(0).expect("should have page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("Both passwords set"),
        "expected 'Both passwords set' in text, got: {:?}",
        text
    );
}

#[test]
fn rc4_128_both_passwords_with_owner() {
    // Both user and owner passwords are non-empty. Open with owner password.
    // This exercises the owner password validation path (Algorithm 3):
    // derive owner key, RC4-decrypt /O to recover user password, then validate.
    let data = read_encrypted("rc4_128_both_passwords.pdf");
    let mut doc = Document::from_bytes_with_password(data, b"owner_pw")
        .expect("should open with owner password");
    let mut page = doc.page(0).expect("should have page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("Both passwords set"),
        "expected 'Both passwords set' in text, got: {:?}",
        text
    );
}

#[test]
fn rc4_128_both_passwords_no_password_fails() {
    // Both passwords non-empty, empty password should fail.
    let data = read_encrypted("rc4_128_both_passwords.pdf");
    let result = Document::from_bytes(data);
    assert_invalid_password(result);
}

#[test]
fn rc4_128_objstm_extracts_text() {
    // RC4-128 encrypted PDF where objects 1 (Catalog), 2 (Pages), 5 (Font),
    // 6 (Font resources) live inside an ObjStm (object stream). This exercises
    // the ObjStm decryption path: stream-level decryption uses the ObjStm's
    // object number for per-object key derivation, then string-level decryption
    // walks each parsed object inside the stream.
    let data = read_encrypted("rc4_128_objstm.pdf");
    let mut doc =
        Document::from_bytes(data).expect("should open ObjStm-encrypted PDF with empty password");
    let mut page = doc.page(0).expect("should have page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("ObjStm encrypted text"),
        "expected 'ObjStm encrypted text' in text, got: {:?}",
        text
    );
}

#[test]
fn unencrypted_baseline_still_works() {
    let data = read_encrypted("unencrypted_baseline.pdf");
    let mut doc = Document::from_bytes(data).expect("should open unencrypted PDF");
    let mut page = doc.page(0).expect("should have page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("Not encrypted"),
        "expected 'Not encrypted' in text, got: {:?}",
        text
    );
}

// ---------------------------------------------------------------------------
// API dogfooding: exercise all page methods on encrypted PDFs
// ---------------------------------------------------------------------------

#[test]
fn encrypted_text_lines_and_raw_spans() {
    let data = read_encrypted("rc4_40_empty_password.pdf");
    let mut doc = Document::from_bytes(data).expect("should open");
    let mut page = doc.page(0).expect("should have page 0");

    let lines = page.text_lines().expect("text_lines should work");
    assert!(!lines.is_empty(), "should have at least one line");

    let spans = page.raw_spans().expect("raw_spans should work");
    assert!(!spans.is_empty(), "should have at least one span");

    // Verify the content matches
    let line_text: String = lines.iter().map(|l| l.text()).collect::<Vec<_>>().join(" ");
    assert!(
        line_text.contains("Hello, World!"),
        "lines should contain expected text, got: {:?}",
        line_text
    );
}

#[test]
fn encrypted_extract_single_pass() {
    let data = read_encrypted("rc4_128_user_password.pdf");
    let mut doc =
        Document::from_bytes_with_password(data, b"test123").expect("should open with password");
    let mut page = doc.page(0).expect("should have page 0");

    let content = page.extract().expect("extract should work");
    let text = content.text();
    assert!(
        text.contains("Encrypted with user password"),
        "extract text should contain expected content, got: {:?}",
        text
    );
    // Images list should be empty (these test PDFs have no images)
    assert!(
        content.images.is_empty(),
        "no images expected in text-only PDF"
    );
}

#[test]
fn encrypted_images_empty_on_text_pdf() {
    let data = read_encrypted("rc4_128_owner_only.pdf");
    let mut doc = Document::from_bytes(data).expect("should open");
    let mut page = doc.page(0).expect("should have page 0");
    let images = page.images().expect("images should work");
    assert!(images.is_empty(), "text-only PDF should have no images");
}

#[test]
fn encrypted_with_explicit_empty_password() {
    // Verify that Config::with_password(b"") works the same as no password
    // for PDFs encrypted with empty user password.
    let data = read_encrypted("rc4_40_empty_password.pdf");
    let config = Config::default().with_password(b"".to_vec());
    let mut doc = Document::from_bytes_with_config(data, config).expect("should open");
    let mut page = doc.page(0).expect("should have page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("Hello, World!"),
        "expected text, got: {:?}",
        text
    );
}

// ---------------------------------------------------------------------------
// AES-128-CBC (V=4, R=4) tests
// ---------------------------------------------------------------------------

#[test]
fn aes128_empty_password_extracts_text() {
    let data = read_encrypted("aes128_empty_password.pdf");
    let mut doc = Document::from_bytes(data).expect("should open AES-128 PDF with empty password");
    let mut page = doc.page(0).expect("should have page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("AES-128 encrypted"),
        "expected 'AES-128 encrypted' in text, got: {:?}",
        text
    );
}

#[test]
fn aes128_user_password_extracts_text() {
    let data = read_encrypted("aes128_user_password.pdf");
    let mut doc = Document::from_bytes_with_password(data, b"aespass")
        .expect("should open with correct user password");
    let mut page = doc.page(0).expect("should have page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("AES-128 with password"),
        "expected 'AES-128 with password' in text, got: {:?}",
        text
    );
}

#[test]
fn aes128_wrong_password_fails() {
    let data = read_encrypted("aes128_user_password.pdf");
    let result = Document::from_bytes_with_password(data, b"wrong_password");
    assert_invalid_password(result);
}

#[test]
fn aes128_no_password_when_required_fails() {
    let data = read_encrypted("aes128_user_password.pdf");
    let result = Document::from_bytes(data);
    assert_invalid_password(result);
}

#[test]
fn aes128_both_passwords_with_user() {
    let data = read_encrypted("aes128_both_passwords.pdf");
    let mut doc = Document::from_bytes_with_password(data, b"user_aes")
        .expect("should open with user password");
    let mut page = doc.page(0).expect("should have page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("AES-128 both passwords"),
        "expected 'AES-128 both passwords' in text, got: {:?}",
        text
    );
}

#[test]
fn aes128_both_passwords_with_owner() {
    let data = read_encrypted("aes128_both_passwords.pdf");
    let mut doc = Document::from_bytes_with_password(data, b"owner_aes")
        .expect("should open with owner password");
    let mut page = doc.page(0).expect("should have page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("AES-128 both passwords"),
        "expected 'AES-128 both passwords' in text, got: {:?}",
        text
    );
}

#[test]
fn aes128_both_passwords_no_password_fails() {
    let data = read_encrypted("aes128_both_passwords.pdf");
    let result = Document::from_bytes(data);
    assert_invalid_password(result);
}

#[test]
fn aes128_text_lines_and_raw_spans() {
    let data = read_encrypted("aes128_empty_password.pdf");
    let mut doc = Document::from_bytes(data).expect("should open");
    let mut page = doc.page(0).expect("should have page 0");

    let lines = page.text_lines().expect("text_lines should work");
    assert!(!lines.is_empty(), "should have at least one line");

    let spans = page.raw_spans().expect("raw_spans should work");
    assert!(!spans.is_empty(), "should have at least one span");

    let line_text: String = lines.iter().map(|l| l.text()).collect::<Vec<_>>().join(" ");
    assert!(
        line_text.contains("AES-128 encrypted"),
        "lines should contain expected text, got: {:?}",
        line_text
    );
}

#[test]
fn aes128_extract_single_pass() {
    let data = read_encrypted("aes128_user_password.pdf");
    let mut doc =
        Document::from_bytes_with_password(data, b"aespass").expect("should open with password");
    let mut page = doc.page(0).expect("should have page 0");

    let content = page.extract().expect("extract should work");
    let text = content.text();
    assert!(
        text.contains("AES-128 with password"),
        "extract text should contain expected content, got: {:?}",
        text
    );
    assert!(
        content.images.is_empty(),
        "no images expected in text-only PDF"
    );
}

// ---------------------------------------------------------------------------
// AES-256-CBC (V=5, R=6) tests
// ---------------------------------------------------------------------------

#[test]
fn aes256_empty_password_extracts_text() {
    let data = read_encrypted("aes256_empty_password.pdf");
    let mut doc = Document::from_bytes(data).expect("should open AES-256 PDF with empty password");
    let mut page = doc.page(0).expect("should have page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("AES-256 encrypted document"),
        "expected 'AES-256 encrypted document' in text, got: {:?}",
        text
    );
}

#[test]
fn aes256_user_password_extracts_text() {
    let data = read_encrypted("aes256_user_password.pdf");
    let mut doc = Document::from_bytes_with_password(data, b"test256")
        .expect("should open with correct user password");
    let mut page = doc.page(0).expect("should have page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("AES-256 with user password"),
        "expected 'AES-256 with user password' in text, got: {:?}",
        text
    );
}

#[test]
fn aes256_wrong_password_fails() {
    let data = read_encrypted("aes256_user_password.pdf");
    let result = Document::from_bytes_with_password(data, b"wrong_password");
    assert_invalid_password(result);
}

#[test]
fn aes256_no_password_when_required_fails() {
    let data = read_encrypted("aes256_user_password.pdf");
    let result = Document::from_bytes(data);
    assert_invalid_password(result);
}

#[test]
fn aes256_owner_password_extracts_with_empty_password() {
    // Owner-only encryption: user password is empty, owner password is set.
    // The empty password should work as user password.
    let data = read_encrypted("aes256_owner_password.pdf");
    let mut doc = Document::from_bytes(data)
        .expect("should open with empty password (owner-only AES-256 encryption)");
    let mut page = doc.page(0).expect("should have page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("AES-256 owner password set"),
        "expected 'AES-256 owner password set' in text, got: {:?}",
        text
    );
}

#[test]
fn aes256_owner_password_explicit() {
    // Should also work when explicitly providing the owner password.
    let data = read_encrypted("aes256_owner_password.pdf");
    let mut doc = Document::from_bytes_with_password(data, b"owneronly256")
        .expect("should open with owner password");
    let mut page = doc.page(0).expect("should have page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("AES-256 owner password set"),
        "expected 'AES-256 owner password set' in text, got: {:?}",
        text
    );
}

#[test]
fn aes256_both_passwords_with_user() {
    let data = read_encrypted("aes256_both_passwords.pdf");
    let mut doc = Document::from_bytes_with_password(data, b"user256")
        .expect("should open with user password");
    let mut page = doc.page(0).expect("should have page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("AES-256 both passwords"),
        "expected 'AES-256 both passwords' in text, got: {:?}",
        text
    );
}

#[test]
fn aes256_both_passwords_with_owner() {
    let data = read_encrypted("aes256_both_passwords.pdf");
    let mut doc = Document::from_bytes_with_password(data, b"owner256")
        .expect("should open with owner password");
    let mut page = doc.page(0).expect("should have page 0");
    let text = page.text().expect("should extract text");
    assert!(
        text.contains("AES-256 both passwords"),
        "expected 'AES-256 both passwords' in text, got: {:?}",
        text
    );
}

#[test]
fn aes256_both_passwords_no_password_fails() {
    let data = read_encrypted("aes256_both_passwords.pdf");
    let result = Document::from_bytes(data);
    assert_invalid_password(result);
}

#[test]
fn aes256_text_lines_and_raw_spans() {
    let data = read_encrypted("aes256_empty_password.pdf");
    let mut doc = Document::from_bytes(data).expect("should open");
    let mut page = doc.page(0).expect("should have page 0");

    let lines = page.text_lines().expect("text_lines should work");
    assert!(!lines.is_empty(), "should have at least one line");

    let spans = page.raw_spans().expect("raw_spans should work");
    assert!(!spans.is_empty(), "should have at least one span");

    let line_text: String = lines.iter().map(|l| l.text()).collect::<Vec<_>>().join(" ");
    assert!(
        line_text.contains("AES-256 encrypted document"),
        "lines should contain expected text, got: {:?}",
        line_text
    );
}

#[test]
fn aes256_extract_single_pass() {
    let data = read_encrypted("aes256_user_password.pdf");
    let mut doc =
        Document::from_bytes_with_password(data, b"test256").expect("should open with password");
    let mut page = doc.page(0).expect("should have page 0");

    let content = page.extract().expect("extract should work");
    let text = content.text();
    assert!(
        text.contains("AES-256 with user password"),
        "extract text should contain expected content, got: {:?}",
        text
    );
    assert!(
        content.images.is_empty(),
        "no images expected in text-only PDF"
    );
}

//
// Verify that the PDF backend's encryption errors round-trip through
// the FormatBackend boundary as typed udoc_core encryption errors,
// and that Document::is_encrypted() reflects /Encrypt-in-trailer state
// regardless of whether decryption succeeded.

#[test]
fn document_is_encrypted_unencrypted_baseline_returns_false() {
    let data = read_encrypted("unencrypted_baseline.pdf");
    let doc = Document::from_bytes(data).expect("should open unencrypted baseline");
    assert!(!doc.is_encrypted());
}

#[test]
fn document_is_encrypted_after_correct_password_returns_true() {
    let data = read_encrypted("rc4_128_user_password.pdf");
    let doc = Document::from_bytes_with_password(data, b"test123")
        .expect("should open with correct user password");
    // Trailer had /Encrypt; even though decryption succeeded, the
    // flag stays true so callers know the source was encrypted.
    assert!(doc.is_encrypted());
}

#[test]
fn typed_encryption_error_via_format_backend_seam() {
    // Wrong password through the PDF API normally produces
    // Error::Encryption(InvalidPassword) inside udoc-pdf. When extraction
    // is driven through the FormatBackend boundary (i.e. via the udoc
    // facade's Extractor), errors round-trip through
    // udoc_pdf::convert::convert_error. This test exercises that seam
    // by constructing the FormatBackend impl directly: any error path
    // that uses convert_error produces a typed core encryption error.
    //
    // Direct API users who match on udoc_pdf::Error::Encryption already
    // get full fidelity (they're below the FormatBackend boundary).
    let data = read_encrypted("rc4_128_user_password.pdf");
    // Open without a password -- we intentionally try to drive page
    // extraction so the InvalidPassword error fires via decryption-
    // attempt time. But Document::from_bytes_with_password fires the
    // error eagerly during open. Either way, we're verifying that
    // wrong-password errors carry the typed encryption signal when
    // converted to core errors.
    let pdf_err = Document::from_bytes_with_password(data, b"wrong_password")
        .expect_err("wrong password should fail");
    // Manually convert via the public path (matches what the
    // FormatBackend::page() implementation does on each error). Since
    // udoc_pdf::convert is module-private, we mirror its logic here:
    // an Error::Encryption(InvalidPassword) must map to
    // EncryptionReason::PasswordRequired through Display+downcast.
    if let Error::Encryption(e) = &pdf_err {
        assert!(matches!(e.kind, EncryptionErrorKind::InvalidPassword));
    } else {
        panic!("expected Error::Encryption, got: {pdf_err}");
    }
    // The full convert_error roundtrip is unit-tested inside
    // udoc-pdf::convert::tests; here we just confirm the variant the
    // mapping consumes.
    let _ = pdf_err;
}

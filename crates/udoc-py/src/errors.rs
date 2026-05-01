//! W1-EXCEPTIONS: Python exception hierarchy.
//!
//! Maps the udoc `Error` enum onto a Python class tree rooted at
//! `udoc.UdocError`. Registered first so every other module can
//! raise these without forward-reference concerns.
//!
//! the hierarchy is:
//!
//! ```text
//! UdocError (Exception)
//! ├── ExtractionError
//! ├── UnsupportedFormatError
//! ├── UnsupportedOperationError
//! ├── PasswordRequiredError
//! ├── WrongPasswordError
//! ├── LimitExceededError
//! ├── HookError
//! ├── IoError
//! ├── ParseError
//! ├── InvalidDocumentError
//! └── EncryptedDocumentError
//! ```
//!
//! W1-FOUNDATION lands the type definitions + module registration.
//! W1-METHODS-EXCEPTIONS adds [`udoc_error_to_py`], the single dispatch
//! function called from every `?` site that crosses the Python boundary.

use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;

use udoc_core::error::EncryptionReason;
use udoc_facade::Error;

create_exception!(
    udoc,
    UdocError,
    PyException,
    "Base class for all udoc errors."
);
create_exception!(
    udoc,
    ExtractionError,
    UdocError,
    "Extraction failed for unspecified reasons. Catch-all for backend errors."
);
create_exception!(
    udoc,
    UnsupportedFormatError,
    UdocError,
    "The document format is not recognized or no backend is registered."
);
create_exception!(
    udoc,
    UnsupportedOperationError,
    UdocError,
    "The backend does not support the requested operation (e.g. render on DOCX)."
);
create_exception!(
    udoc,
    PasswordRequiredError,
    UdocError,
    "The document is encrypted and no password was provided."
);
create_exception!(
    udoc,
    WrongPasswordError,
    UdocError,
    "The provided password did not unlock the document."
);
create_exception!(
    udoc,
    LimitExceededError,
    UdocError,
    "A configured resource limit was exceeded during extraction."
);
create_exception!(
    udoc,
    HookError,
    UdocError,
    "An external hook (OCR, layout, annotate) failed."
);
create_exception!(
    udoc,
    IoError,
    UdocError,
    "An underlying I/O operation failed."
);
create_exception!(
    udoc,
    ParseError,
    UdocError,
    "The document could not be parsed; the bytes are malformed."
);
create_exception!(
    udoc,
    InvalidDocumentError,
    UdocError,
    "The document parses but its structure is invalid (cycles, dangling refs, ...)."
);
create_exception!(
    udoc,
    EncryptedDocumentError,
    UdocError,
    "The document is encrypted and decryption failed or is unsupported."
);

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("UdocError", m.py().get_type::<UdocError>())?;
    m.add("ExtractionError", m.py().get_type::<ExtractionError>())?;
    m.add(
        "UnsupportedFormatError",
        m.py().get_type::<UnsupportedFormatError>(),
    )?;
    m.add(
        "UnsupportedOperationError",
        m.py().get_type::<UnsupportedOperationError>(),
    )?;
    m.add(
        "PasswordRequiredError",
        m.py().get_type::<PasswordRequiredError>(),
    )?;
    m.add(
        "WrongPasswordError",
        m.py().get_type::<WrongPasswordError>(),
    )?;
    m.add(
        "LimitExceededError",
        m.py().get_type::<LimitExceededError>(),
    )?;
    m.add("HookError", m.py().get_type::<HookError>())?;
    m.add("IoError", m.py().get_type::<IoError>())?;
    m.add("ParseError", m.py().get_type::<ParseError>())?;
    m.add(
        "InvalidDocumentError",
        m.py().get_type::<InvalidDocumentError>(),
    )?;
    m.add(
        "EncryptedDocumentError",
        m.py().get_type::<EncryptedDocumentError>(),
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Convert a Rust [`udoc_facade::Error`] into the right Python exception per
/// . Single dispatch call site for every `?` that crosses the Python
/// boundary.
///
/// Order of dispatch matters. Typed payloads land first via
/// [`Error::encryption_info`] / [`Error::resource_limit_info`]; those are the
/// strongly-typed signals the facade carries through `Error::source`. Next,
/// any `std::io::Error` reachable via the source chain routes to
/// [`IoError`] (every `From<io::Error>` + `Error::with_source(_, io_err)`
/// site lands here). After that we fall back to message-based heuristics on
/// the full Display chain, which covers PDF parse / structure errors that
/// reach the facade as `Error::with_source`, the facade's own
/// `Error::new("format X not yet supported")` / "unable to detect format" /
/// hook-failure messages, and so on. Anything that doesn't match a heuristic
/// falls through to [`ExtractionError`] as the catch-all.
///
/// The full Rust error chain (including context frames added by
/// [`udoc_facade::ResultExt::context`] and any wrapped `io::Error` /
/// backend error reached via `Error::source`) is preserved in
/// `str(exc)` because `Error::Display` already walks the source chain.
/// We do not set `__cause__` separately, the Display string is the
/// canonical surface ( message-preservation clause).
pub fn udoc_error_to_py(err: Error) -> PyErr {
    // Build the full message once (Display walks the source chain so this
    // already includes the io / parse / hook detail).
    let message = format!("{err}");

    // 1. Typed encryption payload (: PasswordRequired / WrongPassword
    //    / EncryptedDocument split on the `EncryptionReason`).
    if let Some(info) = err.encryption_info() {
        return match &info.reason {
            EncryptionReason::PasswordRequired => PasswordRequiredError::new_err(message),
            EncryptionReason::WrongPassword => WrongPasswordError::new_err(message),
            EncryptionReason::UnsupportedAlgorithm(_)
            | EncryptionReason::Malformed(_)
            | EncryptionReason::Other(_) => EncryptedDocumentError::new_err(message),
        };
    }

    // 2. Typed resource-limit payload.
    if err.resource_limit_info().is_some() {
        return LimitExceededError::new_err(message);
    }

    // 3. io::Error anywhere in the source chain.
    if has_io_source(&err) {
        return IoError::new_err(message);
    }

    // 4. Message-based heuristics for variants the facade doesn't carry as
    //    typed payloads. Lowercase once; substring-match cheap.
    let lower = message.to_lowercase();

    // Hook errors: the facade always prefixes hook failures with "hook"
    // ("hook command ...", "hook command failed", "all hook invocations
    // failed", ...). See crates/udoc/src/hooks/.
    if lower.contains("hook") {
        return HookError::new_err(message);
    }

    // Unsupported format: facade emits "format X not yet supported" and
    // "unable to detect format" / "unknown magic" from extractor.rs +
    // detect.rs. Order matters: check these before the generic
    // "unsupported" branch so they don't leak to UnsupportedOperation.
    if lower.contains("not yet supported")
        || lower.contains("unable to detect format")
        || lower.contains("unknown magic")
        || lower.contains("no backend")
        || (lower.contains("format") && lower.contains("not supported"))
    {
        return UnsupportedFormatError::new_err(message);
    }

    // Unsupported operation: render-on-DOCX style errors.
    if lower.contains("does not support")
        || lower.contains("not implemented")
        || lower.contains("unsupported operation")
    {
        return UnsupportedOperationError::new_err(message);
    }

    // Parse errors: lexer / object parser failures reach us with "expected
    // X, found Y" / "parse error" / "lex".
    if lower.contains("parse error")
        || lower.contains("expected ")
        || lower.contains(", found ")
        || lower.contains("lexer")
    {
        return ParseError::new_err(message);
    }

    // Structural problems: the document parses but the structure is
    // wrong (xref, trailer, cycles, dangling refs, page out of range,
    // bad page numbers in the page-range parser).
    if lower.contains("invalid")
        || lower.contains("malformed")
        || lower.contains("xref")
        || lower.contains("trailer")
        || lower.contains("cycle")
        || lower.contains("dangling")
        || lower.contains("structure")
        || lower.contains("out of range")
        || lower.contains("page number")
        || lower.contains("page range")
    {
        return InvalidDocumentError::new_err(message);
    }

    // 5. Catch-all.
    ExtractionError::new_err(message)
}

/// Walk the [`Error::source`] chain looking for a [`std::io::Error`].
fn has_io_source(err: &Error) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(err);
    while let Some(src) = current {
        if src.downcast_ref::<std::io::Error>().is_some() {
            return true;
        }
        current = src.source();
    }
    false
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, not(feature = "extension-module")))]
mod tests {
    use super::*;
    use pyo3::Python;
    use udoc_core::error::EncryptionReason;

    /// Helper: attach to the interpreter, dispatch the error, and assert
    /// it lands on the right Python exception class with the original
    /// message preserved.
    ///
    /// pyo3 0.28 renamed `with_gil` to `attach`. `Python::initialize()` is
    /// the supported entry point for unit tests that touch CPython without
    /// going through a `#[pymodule]`. Calling it more than once is a no-op,
    /// which keeps the per-test boilerplate honest.
    fn assert_maps_to(err: Error, expected_class: &str, message_fragment: &str) {
        Python::initialize();
        Python::attach(|py| {
            let py_err = udoc_error_to_py(err);
            let ty = py_err.get_type(py);
            let name = ty.name().expect("class name").to_string();
            assert_eq!(
                name, expected_class,
                "expected {expected_class}, got {name}; py_err={py_err:?}"
            );
            let display = py_err.to_string();
            assert!(
                display.contains(message_fragment),
                "expected {expected_class} display {display:?} to contain {message_fragment:?}"
            );
        });
    }

    // ----  mapping table: one test per row -----------------------

    #[test]
    fn io_error_maps_to_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file vanished");
        let err = Error::with_source("opening report.pdf", io_err);
        assert_maps_to(err, "IoError", "file vanished");
    }

    #[test]
    fn parse_error_maps_to_parse_error() {
        let err = Error::new("parse error: expected name, found integer at offset 42");
        assert_maps_to(err, "ParseError", "expected name");
    }

    #[test]
    fn invalid_structure_maps_to_invalid_document_error() {
        let err = Error::new("invalid xref structure: dangling indirect reference");
        assert_maps_to(err, "InvalidDocumentError", "dangling");
    }

    #[test]
    fn resource_limit_maps_to_limit_exceeded_error() {
        let err = Error::resource_limit_exceeded(1_000_000, 1024, "stream");
        assert_maps_to(err, "LimitExceededError", "1000000");
    }

    #[test]
    fn password_required_maps_to_password_required_error() {
        let err = Error::encryption_required(EncryptionReason::PasswordRequired);
        assert_maps_to(err, "PasswordRequiredError", "password required");
    }

    #[test]
    fn wrong_password_maps_to_wrong_password_error() {
        let err = Error::encryption_required(EncryptionReason::WrongPassword);
        assert_maps_to(err, "WrongPasswordError", "wrong password");
    }

    #[test]
    fn other_encryption_maps_to_encrypted_document_error() {
        let err = Error::encryption_required(EncryptionReason::UnsupportedAlgorithm(
            "V=5 R=6 (AES-256)".into(),
        ));
        assert_maps_to(err, "EncryptedDocumentError", "V=5");
    }

    #[test]
    fn unsupported_format_maps_to_unsupported_format_error() {
        let err = Error::new("format Pdf not yet supported");
        assert_maps_to(err, "UnsupportedFormatError", "not yet supported");
    }

    #[test]
    fn unsupported_operation_maps_to_unsupported_operation_error() {
        let err = Error::new("DOCX backend does not support page rendering");
        assert_maps_to(err, "UnsupportedOperationError", "does not support");
    }

    #[test]
    fn hook_error_maps_to_hook_error() {
        let err = Error::new("hook command 'ocr.py' failed: exit status 1");
        assert_maps_to(err, "HookError", "hook command");
    }

    #[test]
    fn unrecognised_error_maps_to_extraction_error() {
        // Generic backend error with no matching keyword; falls through to
        // the catch-all row 11.
        let err = Error::new("backend produced no output for this document");
        assert_maps_to(err, "ExtractionError", "backend produced no output");
    }

    // ---- Sanity checks (not part of the 11-row table) ------------------

    #[test]
    fn encryption_typed_payload_beats_message_heuristic() {
        // The message contains "invalid" which would otherwise route to
        // InvalidDocumentError. The typed payload must win.
        let err = Error::encryption_required(EncryptionReason::Malformed("invalid /U".into()))
            .with_context("opening report.pdf");
        assert_maps_to(err, "EncryptedDocumentError", "malformed");
    }

    #[test]
    fn io_source_on_chained_error_routes_to_io_error() {
        // Deeper source chain: facade wraps an io::Error which itself
        // originated from a missing file.
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err =
            Error::with_source("opening report.pdf", io_err).with_context("extracting document");
        assert_maps_to(err, "IoError", "denied");
    }
}

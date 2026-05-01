//! Error types for document extraction.
//!
//! Provides a context-chaining error type shared across all format backends.
//! Each backend may define additional error variants internally and convert
//! to the core Error at the FormatBackend trait boundary.

use std::fmt;

use crate::text::FallbackReason;

/// Core error type for document extraction operations.
///
/// Errors carry a message and an optional chain of context strings,
/// following the context-chaining pattern used throughout udoc.
///
/// # Adding context
///
/// Use the [`ResultExt`] trait to add context to any `Result`:
///
/// ```
/// use udoc_core::error::{Error, Result, ResultExt};
///
/// fn parse_header(data: &[u8]) -> Result<()> {
///     if data.is_empty() {
///         return Err(Error::new("empty input"));
///     }
///     Ok(())
/// }
///
/// fn process(data: &[u8]) -> Result<()> {
///     parse_header(data).context("processing document header")?;
///     Ok(())
/// }
/// ```
#[derive(Debug)]
pub struct Error {
    message: String,
    context: Vec<String>,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

/// Result type alias using the core Error.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Create a new error with a message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            context: Vec::new(),
            source: None,
        }
    }

    /// Create an error wrapping another error as its source.
    pub fn with_source(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            message: message.into(),
            context: Vec::new(),
            source: Some(Box::new(source)),
        }
    }

    /// Construct an [`Error`] signalling that strict-font mode rejected a
    /// non-Exact [`crate::text::FontResolution`].
    ///
    /// The typed [`FontFallbackRequired`] payload is attached as the
    /// underlying error source so callers can downcast via
    /// [`Error::font_fallback_info`] to inspect `requested` and `reason`.
    pub fn font_fallback_required(requested: impl Into<String>, reason: FallbackReason) -> Self {
        let payload = FontFallbackRequired {
            requested: requested.into(),
            reason,
        };
        Self {
            message: payload.to_string(),
            context: Vec::new(),
            source: Some(Box::new(payload)),
        }
    }

    /// Downcast the error's source to a [`FontFallbackRequired`] payload.
    ///
    /// Returns `Some` when the error was produced by
    /// [`Error::font_fallback_required`] (or any layer above it that left the
    /// source chain intact). Returns `None` otherwise.
    pub fn font_fallback_info(&self) -> Option<&FontFallbackRequired> {
        self.source.as_ref()?.downcast_ref::<FontFallbackRequired>()
    }

    /// Construct an [`Error`] signalling that a requested allocation exceeded
    /// a configured resource budget.
    ///
    /// Pair with [`crate::limits::safe_alloc_size`] on the call site that
    /// turns an attacker-controlled size field into a `Vec` allocation. The
    /// typed [`ResourceLimitExceeded`] payload is attached as the underlying
    /// source; callers can downcast via [`Error::resource_limit_info`] to
    /// inspect `requested`, `limit`, and `kind`.
    pub fn resource_limit_exceeded(requested: u64, limit: u64, kind: &'static str) -> Self {
        let payload = ResourceLimitExceeded {
            requested,
            limit,
            kind,
        };
        Self {
            message: payload.to_string(),
            context: Vec::new(),
            source: Some(Box::new(payload)),
        }
    }

    /// Downcast the error's source to a [`ResourceLimitExceeded`] payload.
    ///
    /// Returns `Some` when the error was produced by
    /// [`Error::resource_limit_exceeded`] or
    /// [`crate::limits::safe_alloc_size`]. Useful for callers that want to
    /// translate resource-limit errors into service-layer 413-style
    /// responses or to filter them out of diagnostics.
    pub fn resource_limit_info(&self) -> Option<&ResourceLimitExceeded> {
        self.source
            .as_ref()?
            .downcast_ref::<ResourceLimitExceeded>()
    }

    /// Construct an [`Error`] signalling that the source document is
    /// encrypted and the requested operation cannot proceed without a
    /// (correct) password, or that the encryption is otherwise unsupported.
    ///
    /// The typed [`EncryptionRequired`] payload is attached as the
    /// underlying error source so callers can downcast via
    /// [`Error::encryption_info`] to inspect the [`EncryptionReason`], or
    /// just call [`Error::is_encryption_error`] for a yes/no check.
    ///
    /// Used by the PDF backend on `/Encrypt`-bearing documents when no
    /// password / a wrong password / an unsupported encryption algorithm
    /// is encountered. Other backends can adopt the same pattern when
    /// they grow encryption support.
    pub fn encryption_required(reason: EncryptionReason) -> Self {
        let payload = EncryptionRequired { reason };
        Self {
            message: payload.to_string(),
            context: Vec::new(),
            source: Some(Box::new(payload)),
        }
    }

    /// Downcast the error's source to an [`EncryptionRequired`] payload.
    ///
    /// Returns `Some` when the error was produced by
    /// [`Error::encryption_required`] (or any layer above it that left the
    /// source chain intact). `None` otherwise. Use
    /// [`Error::is_encryption_error`] for the more common yes/no check.
    pub fn encryption_info(&self) -> Option<&EncryptionRequired> {
        self.source.as_ref()?.downcast_ref::<EncryptionRequired>()
    }

    /// Convenience predicate: `true` iff this error carries the typed
    /// [`EncryptionRequired`] payload (i.e. the document is encrypted and
    /// extraction can't proceed without password / supported algorithm).
    ///
    /// Equivalent to `self.encryption_info().is_some()`. Intended for
    /// CLI / agent code that just wants to surface "this PDF is
    /// encrypted" without case-matching on the reason.
    pub fn is_encryption_error(&self) -> bool {
        self.encryption_info().is_some()
    }

    /// Add context to this error (prepended to the context chain).
    pub fn with_context(mut self, ctx: impl Into<String>) -> Self {
        self.context.push(ctx.into());
        self
    }

    /// The error message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The context chain (most recent context last).
    pub fn context_chain(&self) -> &[String] {
        &self.context
    }
}

/// Typed payload carried by [`Error::font_fallback_required`].
///
/// Produced when a strict-font caller (e.g. the facade crate's
/// `Config::strict_fonts`) rejects a non-Exact
/// [`crate::text::FontResolution`] during extraction. Callers can recover
/// the raw `requested` font name and [`FallbackReason`] via
/// [`Error::font_fallback_info`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontFallbackRequired {
    /// Font name as referenced in the document.
    pub requested: String,
    /// Why the font loader would have substituted.
    pub reason: FallbackReason,
}

impl fmt::Display for FontFallbackRequired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "strict_fonts: font '{}' requires fallback ({})",
            self.requested,
            self.reason.as_str()
        )
    }
}

impl std::error::Error for FontFallbackRequired {}

/// Typed payload carried by [`Error::resource_limit_exceeded`] and
/// [`crate::limits::safe_alloc_size`].
///
/// Produced when a parser or decoder refuses to honour an allocation request
/// whose size exceeded the caller-provided ceiling. Normal parse errors are
/// still parse errors; this variant specifically signals "we rejected the
/// size ON PURPOSE because honouring it would be unsafe."
///
/// Common shapes:
/// - `kind = "stream"`: PDF `/Length` field vs `max_decompressed_size`.
/// - `kind = "image_buffer"`: `width * height * bpc / 8` vs a per-image cap.
/// - `kind = "raster"`: rasterizer backing buffer at a given page size + DPI.
/// - `kind = "jbig2_region"`: JBIG2 region dims from a segment header.
///
/// Callers can downcast via [`Error::resource_limit_info`] to read the raw
/// `requested` / `limit` / `kind` fields (for structured logging, service-layer
/// translation, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLimitExceeded {
    /// The size the parser / decoder asked for, in bytes.
    pub requested: u64,
    /// The configured ceiling that was enforced, in bytes.
    pub limit: u64,
    /// A short, stable kind tag naming the call site. Not user-facing;
    /// intended for structured log queries.
    pub kind: &'static str,
}

impl fmt::Display for ResourceLimitExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} allocation of {} bytes exceeds configured limit of {} bytes",
            self.kind, self.requested, self.limit,
        )
    }
}

impl std::error::Error for ResourceLimitExceeded {}

/// Typed payload carried by [`Error::encryption_required`].
///
/// Produced when a backend cannot extract content because the source
/// document is encrypted and either no password / a wrong password / an
/// unsupported encryption mechanism was encountered. Callers can recover
/// the [`EncryptionReason`] via [`Error::encryption_info`] for typed
/// dispatch, or just call [`Error::is_encryption_error`] for a yes/no
/// check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptionRequired {
    /// Why the backend rejected the document at this stage.
    pub reason: EncryptionReason,
}

/// Why the backend reported an encryption error.
///
/// Variants are intentionally coarse so they map cleanly onto
/// per-backend error variants without leaking format-specific details.
/// Use the contained `String` fields for additional context where
/// supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncryptionReason {
    /// The document is encrypted and no password was supplied.
    PasswordRequired,
    /// A password was supplied but it does not unlock the document.
    WrongPassword,
    /// The document declares encryption with an algorithm/version the
    /// backend does not implement (e.g. an unknown filter, a future R/V
    /// pair). The string carries a short backend-supplied detail
    /// suitable for logging (not stable; do not match on its text).
    UnsupportedAlgorithm(String),
    /// The document is encrypted but the encryption metadata is
    /// malformed in a way that prevents key derivation (missing /ID,
    /// invalid /U, /O length mismatch, etc.). String is a backend
    /// detail.
    Malformed(String),
    /// Catch-all for backends that have an encryption signal but no
    /// finer-grained classification yet. String is a backend detail.
    Other(String),
}

impl EncryptionReason {
    /// Stable string tag. Useful for structured logging.
    pub fn as_str(&self) -> &'static str {
        match self {
            EncryptionReason::PasswordRequired => "PasswordRequired",
            EncryptionReason::WrongPassword => "WrongPassword",
            EncryptionReason::UnsupportedAlgorithm(_) => "UnsupportedAlgorithm",
            EncryptionReason::Malformed(_) => "Malformed",
            EncryptionReason::Other(_) => "Other",
        }
    }
}

impl fmt::Display for EncryptionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncryptionReason::PasswordRequired => write!(f, "password required"),
            EncryptionReason::WrongPassword => write!(f, "wrong password"),
            EncryptionReason::UnsupportedAlgorithm(d) => write!(f, "unsupported algorithm ({d})"),
            EncryptionReason::Malformed(d) => write!(f, "malformed encryption metadata ({d})"),
            EncryptionReason::Other(d) => write!(f, "{d}"),
        }
    }
}

impl fmt::Display for EncryptionRequired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "document is encrypted: {}", self.reason)
    }
}

impl std::error::Error for EncryptionRequired {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Display context chain from outermost to innermost.
        for ctx in self.context.iter().rev() {
            write!(f, "{ctx}: ")?;
        }
        write!(f, "{}", self.message)?;

        // Walk the source chain so the actual root cause is visible to
        // the user. Without this, a wrapping error like
        // `Error::with_source("opening PDF '...'", io_err)` displayed only
        // "opening PDF '...'", hiding the underlying "missing header"
        // / "permission denied" / etc. ( error UX review).
        let mut current: Option<&dyn std::error::Error> = self.source.as_deref().map(|s| s as _);
        while let Some(src) = current {
            write!(f, ": {src}")?;
            current = src.source();
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|s| s.as_ref() as &(dyn std::error::Error + 'static))
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::with_source("I/O error", e)
    }
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::new(s)
    }
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error::new(s)
    }
}

/// Extension trait for adding context to Results.
pub trait ResultExt<T> {
    /// Add context to an error result.
    fn context(self, msg: impl Into<String>) -> Result<T>;
}

impl<T, E: Into<Error>> ResultExt<T> for std::result::Result<T, E> {
    fn context(self, msg: impl Into<String>) -> Result<T> {
        self.map_err(|e| e.into().with_context(msg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_new() {
        let e = Error::new("something broke");
        assert_eq!(e.message(), "something broke");
        assert!(e.context_chain().is_empty());
    }

    #[test]
    fn error_with_context() {
        let e = Error::new("bad token")
            .with_context("parsing object")
            .with_context("reading page");
        assert_eq!(e.message(), "bad token");
        assert_eq!(e.context_chain().len(), 2);
        assert_eq!(format!("{e}"), "reading page: parsing object: bad token");
    }

    #[test]
    fn result_ext_context() {
        let r: Result<()> = Err(Error::new("fail"));
        let r2 = r.context("doing something");
        let e = r2.unwrap_err();
        assert_eq!(format!("{e}"), "doing something: fail");
    }

    #[test]
    fn from_io_error() {
        use std::error::Error as StdError;
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let e: Error = io_err.into();
        assert_eq!(e.message(), "I/O error");
        assert!(StdError::source(&e).is_some());
    }

    /// Display walks the source chain so the root cause is visible
    /// to the user. Pre-fix, `Error::with_source("opening PDF '...'", io_err)`
    /// displayed only "opening PDF '...'", hiding the underlying io reason.
    #[test]
    fn display_walks_source_chain() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let e = Error::with_source("opening report.pdf", io_err).with_context("processing input");
        let s = format!("{e}");
        assert!(
            s.contains("processing input"),
            "should include outer context: {s}"
        );
        assert!(
            s.contains("opening report.pdf"),
            "should include error message: {s}"
        );
        assert!(
            s.contains("file missing"),
            "should walk source chain to root cause: {s}"
        );
    }

    #[test]
    fn from_string() {
        let e: Error = "oops".into();
        assert_eq!(e.message(), "oops");
    }

    #[test]
    fn font_fallback_required_carries_typed_payload() {
        let e = Error::font_fallback_required("Helvetica", FallbackReason::NameRouted);
        assert!(e.message().contains("Helvetica"));
        assert!(e.message().contains("NameRouted"));

        let info = e
            .font_fallback_info()
            .expect("typed payload should be recoverable");
        assert_eq!(info.requested, "Helvetica");
        assert_eq!(info.reason, FallbackReason::NameRouted);
    }

    #[test]
    fn font_fallback_payload_clones() {
        // FallbackReason derives Clone; make sure the typed payload does too.
        let e = Error::font_fallback_required(
            "MysteryFont",
            FallbackReason::EmbeddedCorrupt("oops".into()),
        );
        let info = e.font_fallback_info().unwrap();
        let cloned = info.clone();
        assert_eq!(cloned.requested, "MysteryFont");
        match cloned.reason {
            FallbackReason::EmbeddedCorrupt(detail) => assert_eq!(detail, "oops"),
            other => panic!("unexpected reason variant: {other:?}"),
        }
    }

    #[test]
    fn font_fallback_info_is_none_for_plain_errors() {
        let e = Error::new("unrelated");
        assert!(e.font_fallback_info().is_none());
    }

    #[test]
    fn font_fallback_context_chains_preserve_payload() {
        let e = Error::font_fallback_required("CMR10", FallbackReason::NameRouted)
            .with_context("extracting page 0");
        assert!(format!("{e}").starts_with("extracting page 0: "));
        assert!(e.font_fallback_info().is_some());
    }

    #[test]
    fn encryption_required_carries_typed_payload() {
        let e = Error::encryption_required(EncryptionReason::PasswordRequired);
        assert!(e.is_encryption_error());
        assert!(format!("{e}").contains("password required"));

        let info = e
            .encryption_info()
            .expect("typed payload should be recoverable");
        assert_eq!(info.reason, EncryptionReason::PasswordRequired);
    }

    #[test]
    fn encryption_required_wrong_password() {
        let e = Error::encryption_required(EncryptionReason::WrongPassword);
        assert!(e.is_encryption_error());
        match &e.encryption_info().unwrap().reason {
            EncryptionReason::WrongPassword => {}
            other => panic!("unexpected reason: {other:?}"),
        }
    }

    #[test]
    fn encryption_required_unsupported_algorithm_carries_detail() {
        let e = Error::encryption_required(EncryptionReason::UnsupportedAlgorithm(
            "V=5 R=6 (AES-256 with OE/UE)".into(),
        ));
        let info = e.encryption_info().unwrap();
        match &info.reason {
            EncryptionReason::UnsupportedAlgorithm(detail) => {
                assert!(detail.contains("V=5"));
            }
            other => panic!("unexpected reason: {other:?}"),
        }
        // as_str() returns the stable variant tag, NOT the detail.
        assert_eq!(info.reason.as_str(), "UnsupportedAlgorithm");
    }

    #[test]
    fn encryption_info_is_none_for_plain_errors() {
        let e = Error::new("unrelated");
        assert!(!e.is_encryption_error());
        assert!(e.encryption_info().is_none());
    }

    #[test]
    fn encryption_context_chains_preserve_payload() {
        let e = Error::encryption_required(EncryptionReason::PasswordRequired)
            .with_context("opening report.pdf");
        assert!(format!("{e}").starts_with("opening report.pdf: "));
        assert!(e.is_encryption_error());
    }
}

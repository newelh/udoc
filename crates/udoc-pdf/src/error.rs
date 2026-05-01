use std::fmt;

/// Result type alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Top-level error type for the PDF parser.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// I/O error during file access.
    Io(std::io::Error),
    /// Error during parsing (lexer or object parser).
    Parse(ParseError),
    /// Invalid document structure.
    InvalidStructure(StructureError),
    /// Resource limit exceeded (recursion depth, memory, etc.).
    ResourceLimit(ResourceLimitError),
    /// Encryption error (wrong password, unsupported algorithm, etc.).
    Encryption(EncryptionError),
}

/// A parse error with context about where and what went wrong.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ParseError {
    /// Byte offset in the source where the error occurred.
    pub offset: u64,
    /// What the parser expected.
    pub expected: String,
    /// What the parser found.
    pub found: String,
    /// Stack of context descriptions ("while parsing X").
    pub context: Vec<String>,
}

impl ParseError {
    /// Create a new parse error.
    pub fn new(offset: u64, expected: impl Into<String>, found: impl Into<String>) -> Self {
        Self {
            offset,
            expected: expected.into(),
            found: found.into(),
            context: Vec::new(),
        }
    }
}

/// An error in document structure.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct StructureError {
    /// Byte offset where the issue was detected, if available.
    pub offset: Option<u64>,
    /// Description of what's wrong.
    pub message: String,
    /// Additional context.
    pub context: Vec<String>,
}

impl StructureError {
    /// Create a new structure error with no offset.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            offset: None,
            message: message.into(),
            context: Vec::new(),
        }
    }

    /// Create a new structure error at a specific byte offset.
    pub fn at(offset: u64, message: impl Into<String>) -> Self {
        Self {
            offset: Some(offset),
            message: message.into(),
            context: Vec::new(),
        }
    }
}

/// An encryption-related error.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct EncryptionError {
    /// What kind of encryption error occurred.
    pub kind: EncryptionErrorKind,
    /// Additional context.
    pub context: Vec<String>,
}

/// Specific encryption error variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum EncryptionErrorKind {
    /// The /Filter is not /Standard (only standard handler is supported).
    UnsupportedFilter(String),
    /// The /V or /R version is not supported.
    UnsupportedVersion { v: i64, r: i64 },
    /// The supplied password is incorrect.
    InvalidPassword,
    /// Required encryption dictionary field is missing.
    MissingField(String),
    /// An encryption dictionary field is present but has an invalid value.
    InvalidField(String),
    /// Decryption produced invalid data (used by AES in future).
    #[allow(dead_code)] // reserved for AES decryption support
    DecryptionFailed(String),
}

/// A resource-limit error with context about where the limit was hit.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ResourceLimitError {
    /// Which limit was exceeded.
    pub limit: Limit,
    /// Additional context.
    pub context: Vec<String>,
}

/// Resource limit that was exceeded.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Limit {
    /// Recursion depth limit exceeded.
    RecursionDepth(usize),
    /// Maximum decompressed size exceeded.
    DecompressedSize(u64),
    /// Decompression ratio exceeded (potential bomb).
    DecompressionRatio { ratio: u64, limit: u64 },
    /// Object cache limit exceeded.
    CacheSize(usize),
    /// Collection (array/dictionary) element count exceeded.
    CollectionSize(usize),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Parse(e) => write!(f, "{e}"),
            Error::InvalidStructure(e) => write!(f, "{e}"),
            Error::ResourceLimit(e) => write!(f, "{e}"),
            Error::Encryption(e) => write!(f, "{e}"),
        }
    }
}

impl fmt::Display for ResourceLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.limit)?;
        for ctx in &self.context {
            write!(f, "\n  while {ctx}")?;
        }
        Ok(())
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "parse error at offset {}: expected {}, found {}",
            self.offset, self.expected, self.found
        )?;
        for ctx in &self.context {
            write!(f, "\n  while {ctx}")?;
        }
        Ok(())
    }
}

impl fmt::Display for StructureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.offset {
            Some(off) => write!(f, "invalid structure at offset {}: {}", off, self.message)?,
            None => write!(f, "invalid structure: {}", self.message)?,
        }
        for ctx in &self.context {
            write!(f, "\n  while {ctx}")?;
        }
        Ok(())
    }
}

impl fmt::Display for EncryptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "encryption error: {}", self.kind)?;
        for ctx in &self.context {
            write!(f, "\n  while {ctx}")?;
        }
        Ok(())
    }
}

impl fmt::Display for EncryptionErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncryptionErrorKind::UnsupportedFilter(name) => {
                write!(f, "unsupported encryption filter: {name}")
            }
            EncryptionErrorKind::UnsupportedVersion { v, r } => {
                write!(f, "unsupported encryption version V={v}, R={r}")
            }
            EncryptionErrorKind::InvalidPassword => write!(f, "invalid password"),
            EncryptionErrorKind::MissingField(field) => {
                write!(f, "missing required field: {field}")
            }
            EncryptionErrorKind::InvalidField(msg) => {
                write!(f, "invalid field value: {msg}")
            }
            EncryptionErrorKind::DecryptionFailed(msg) => {
                write!(f, "decryption failed: {msg}")
            }
        }
    }
}

impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Limit::RecursionDepth(d) => write!(f, "recursion depth limit ({d}) exceeded"),
            Limit::DecompressedSize(s) => write!(f, "decompressed size limit ({s} bytes) exceeded"),
            Limit::DecompressionRatio { ratio, limit } => {
                write!(f, "decompression ratio {ratio}:1 exceeds limit {limit}:1")
            }
            Limit::CacheSize(s) => write!(f, "cache size limit ({s}) exceeded"),
            Limit::CollectionSize(s) => {
                write!(f, "collection size limit ({s} elements) exceeded")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl Error {
    /// Create a parse error at the given offset.
    pub fn parse(offset: u64, expected: impl Into<String>, found: impl Into<String>) -> Self {
        Error::Parse(ParseError {
            offset,
            expected: expected.into(),
            found: found.into(),
            context: Vec::new(),
        })
    }

    /// Create a structure error with no offset.
    pub fn structure(message: impl Into<String>) -> Self {
        Error::InvalidStructure(StructureError {
            offset: None,
            message: message.into(),
            context: Vec::new(),
        })
    }

    /// Create a structure error at a specific byte offset.
    pub fn structure_at(offset: u64, message: impl Into<String>) -> Self {
        Error::InvalidStructure(StructureError {
            offset: Some(offset),
            message: message.into(),
            context: Vec::new(),
        })
    }

    /// Create a resource limit error.
    pub fn resource_limit(limit: Limit) -> Self {
        Error::ResourceLimit(ResourceLimitError {
            limit,
            context: Vec::new(),
        })
    }

    /// Create an encryption error.
    pub fn encryption(kind: EncryptionErrorKind) -> Self {
        Error::Encryption(EncryptionError {
            kind,
            context: Vec::new(),
        })
    }

    /// Add context to this error.
    ///
    /// Context is appended to all error variants except `Io` (which wraps
    /// a foreign error type that cannot carry additional context).
    pub fn context(mut self, ctx: impl Into<String>) -> Self {
        match &mut self {
            Error::Parse(e) => e.context.push(ctx.into()),
            Error::InvalidStructure(e) => e.context.push(ctx.into()),
            Error::ResourceLimit(e) => e.context.push(ctx.into()),
            Error::Encryption(e) => e.context.push(ctx.into()),
            Error::Io(_) => {}
        }
        self
    }
}

/// Extension trait for adding context to Results.
pub trait ResultExt<T> {
    /// Add context to the error case.
    fn context(self, ctx: impl Into<String>) -> Result<T>;
}

impl<T> ResultExt<T> for Result<T> {
    fn context(self, ctx: impl Into<String>) -> Result<T> {
        self.map_err(|e| e.context(ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err = Error::Io(io_err);
        let s = format!("{err}");
        assert!(s.contains("I/O error"), "got: {s}");
    }

    #[test]
    fn test_error_display_parse() {
        let err = Error::parse(42, "integer", "garbage");
        let s = format!("{err}");
        assert!(s.contains("offset 42"), "got: {s}");
        assert!(s.contains("integer"), "got: {s}");
    }

    #[test]
    fn test_error_display_structure() {
        let err = Error::structure("missing /Root");
        let s = format!("{err}");
        assert!(s.contains("missing /Root"), "got: {s}");
    }

    #[test]
    fn test_error_display_structure_at() {
        let err = Error::structure_at(100, "bad xref");
        let s = format!("{err}");
        assert!(s.contains("offset 100"), "got: {s}");
    }

    #[test]
    fn test_error_display_resource_limit() {
        let err = Error::resource_limit(Limit::RecursionDepth(50));
        let s = format!("{err}");
        assert!(s.contains("recursion depth"), "got: {s}");
    }

    #[test]
    fn test_limit_display_all_variants() {
        assert!(format!("{}", Limit::RecursionDepth(50)).contains("50"));
        assert!(format!("{}", Limit::DecompressedSize(1024)).contains("1024"));
        assert!(format!(
            "{}",
            Limit::DecompressionRatio {
                ratio: 150,
                limit: 100
            }
        )
        .contains("150"));
        assert!(format!("{}", Limit::CacheSize(100)).contains("100"));
        assert!(format!("{}", Limit::CollectionSize(999)).contains("999"));
    }

    #[test]
    fn test_error_display_encryption() {
        let err = Error::encryption(EncryptionErrorKind::InvalidPassword);
        let s = format!("{err}");
        assert!(s.contains("invalid password"), "got: {s}");
    }

    #[test]
    fn test_encryption_error_kind_display() {
        assert!(
            format!("{}", EncryptionErrorKind::UnsupportedFilter("Foo".into())).contains("Foo")
        );
        assert!(
            format!("{}", EncryptionErrorKind::UnsupportedVersion { v: 5, r: 6 }).contains("V=5")
        );
        assert!(format!("{}", EncryptionErrorKind::InvalidPassword).contains("invalid password"));
        assert!(format!("{}", EncryptionErrorKind::MissingField("/O".into())).contains("/O"));
        assert!(format!("{}", EncryptionErrorKind::InvalidField("bad".into())).contains("bad"));
        assert!(
            format!("{}", EncryptionErrorKind::DecryptionFailed("oops".into())).contains("oops")
        );
    }

    #[test]
    fn test_parse_error_with_context() {
        let err = Error::parse(10, "name", "integer")
            .context("parsing trailer")
            .context("loading document");
        let s = format!("{err}");
        assert!(s.contains("parsing trailer"), "got: {s}");
        assert!(s.contains("loading document"), "got: {s}");
    }

    #[test]
    fn test_structure_error_with_context() {
        let err = Error::structure("missing /Pages").context("reading page tree");
        let s = format!("{err}");
        assert!(s.contains("reading page tree"), "got: {s}");
    }

    #[test]
    fn test_encryption_error_with_context() {
        let err =
            Error::encryption(EncryptionErrorKind::InvalidPassword).context("validating password");
        let s = format!("{err}");
        assert!(s.contains("validating password"), "got: {s}");
    }

    #[test]
    fn test_resource_limit_error_with_context() {
        let err = Error::resource_limit(Limit::RecursionDepth(50)).context("resolving object 42");
        let s = format!("{err}");
        assert!(s.contains("recursion depth"), "got: {s}");
        assert!(s.contains("resolving object 42"), "got: {s}");
    }

    #[test]
    fn test_io_error_context_is_noop() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "x");
        let err = Error::Io(io_err).context("ignored");
        let s = format!("{err}");
        assert!(!s.contains("ignored"), "IO context should be ignored: {s}");
    }

    #[test]
    fn test_error_source_trait() {
        use std::error::Error as StdError;
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "x");
        let err = Error::Io(io_err);
        assert!(err.source().is_some());

        let err = Error::structure("x");
        assert!(err.source().is_none());
    }

    #[test]
    fn test_io_error_from() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "x");
        let err: Error = io_err.into();
        assert!(matches!(err, Error::Io(_)));
    }

    #[test]
    fn test_result_ext_context() {
        let result: Result<i32> = Err(Error::structure("fail"));
        let result = result.context("outer");
        let s = format!("{}", result.unwrap_err());
        assert!(s.contains("outer"), "got: {s}");
    }
}

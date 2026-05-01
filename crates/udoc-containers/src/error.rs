use std::fmt;

/// Result type alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Error type for container format parsers (ZIP, XML, CFB, OPC).
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// I/O error during file or stream access.
    Io {
        /// The underlying I/O error.
        source: std::io::Error,
        /// Stack of context descriptions.
        context: Vec<String>,
    },
    /// Error during ZIP parsing.
    Zip(ContainerError),
    /// Error during XML parsing.
    Xml(ContainerError),
    /// Error during CFB/OLE2 parsing.
    Cfb(ContainerError),
    /// Error in OPC package structure.
    Opc(ContainerError),
    /// Resource limit exceeded (decompression size, nesting depth, etc.).
    ResourceLimit(ResourceLimitError),
}

/// A container-format parse/structure error with optional offset and context chain.
#[derive(Debug, Clone)]
pub struct ContainerError {
    /// Byte offset where the error was detected, if available.
    pub offset: Option<u64>,
    /// Description of the error.
    pub message: String,
    /// Stack of context descriptions.
    pub context: Vec<String>,
}

impl ContainerError {
    /// Create a new error with a message and no offset.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            offset: None,
            message: message.into(),
            context: Vec::new(),
        }
    }

    /// Create a new error at a specific byte offset.
    pub fn at(offset: u64, message: impl Into<String>) -> Self {
        Self {
            offset: Some(offset),
            message: message.into(),
            context: Vec::new(),
        }
    }
}

/// A resource-limit error.
#[derive(Debug, Clone)]
pub struct ResourceLimitError {
    /// Description of which limit was exceeded.
    pub message: String,
    /// Stack of context descriptions.
    pub context: Vec<String>,
}

// -- Display impls --

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { source, context } => {
                write!(f, "I/O error: {source}")?;
                for ctx in context {
                    write!(f, "\n  while {ctx}")?;
                }
                Ok(())
            }
            Error::Zip(e) => write!(f, "ZIP error: {e}"),
            Error::Xml(e) => write!(f, "XML error: {e}"),
            Error::Cfb(e) => write!(f, "CFB error: {e}"),
            Error::Opc(e) => write!(f, "OPC error: {e}"),
            Error::ResourceLimit(e) => write!(f, "resource limit: {e}"),
        }
    }
}

impl fmt::Display for ContainerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.offset {
            Some(off) => write!(f, "at offset {off}: {}", self.message)?,
            None => write!(f, "{}", self.message)?,
        }
        for ctx in &self.context {
            write!(f, "\n  while {ctx}")?;
        }
        Ok(())
    }
}

impl fmt::Display for ResourceLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        for ctx in &self.context {
            write!(f, "\n  while {ctx}")?;
        }
        Ok(())
    }
}

// -- std::error::Error --

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

// -- From impls --

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io {
            source: e,
            context: Vec::new(),
        }
    }
}

impl From<Error> for udoc_core::error::Error {
    fn from(e: Error) -> Self {
        udoc_core::error::Error::with_source(format!("{e}"), e)
    }
}

impl Error {
    /// Create a ZIP parse error with no offset.
    pub fn zip(message: impl Into<String>) -> Self {
        Error::Zip(ContainerError::new(message))
    }

    /// Create a ZIP parse error at a specific byte offset.
    pub fn zip_at(offset: u64, message: impl Into<String>) -> Self {
        Error::Zip(ContainerError::at(offset, message))
    }

    /// Create an XML parse error with no offset.
    pub fn xml(message: impl Into<String>) -> Self {
        Error::Xml(ContainerError::new(message))
    }

    /// Create an XML parse error at a specific byte offset.
    pub fn xml_at(offset: u64, message: impl Into<String>) -> Self {
        Error::Xml(ContainerError::at(offset, message))
    }

    /// Create a CFB parse error with no offset.
    pub fn cfb(message: impl Into<String>) -> Self {
        Error::Cfb(ContainerError::new(message))
    }

    /// Create a CFB parse error at a specific byte offset.
    pub fn cfb_at(offset: u64, message: impl Into<String>) -> Self {
        Error::Cfb(ContainerError::at(offset, message))
    }

    /// Create an OPC structure error.
    pub fn opc(message: impl Into<String>) -> Self {
        Error::Opc(ContainerError::new(message))
    }

    /// Create a resource limit error.
    pub fn resource_limit(message: impl Into<String>) -> Self {
        Error::ResourceLimit(ResourceLimitError {
            message: message.into(),
            context: Vec::new(),
        })
    }

    /// Add context to this error.
    pub fn with_context(mut self, ctx: impl Into<String>) -> Self {
        match &mut self {
            Error::Zip(e) | Error::Xml(e) | Error::Cfb(e) | Error::Opc(e) => {
                e.context.push(ctx.into());
            }
            Error::ResourceLimit(e) => {
                e.context.push(ctx.into());
            }
            Error::Io { context, .. } => {
                context.push(ctx.into());
            }
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
        self.map_err(|e| e.with_context(ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_zip_error() {
        let err = Error::zip("missing end of central directory");
        let s = format!("{err}");
        assert!(s.contains("ZIP error"), "got: {s}");
        assert!(s.contains("missing end of central directory"), "got: {s}");
    }

    #[test]
    fn display_zip_error_at_offset() {
        let err = Error::zip_at(1024, "bad signature");
        let s = format!("{err}");
        assert!(s.contains("offset 1024"), "got: {s}");
    }

    #[test]
    fn display_xml_error() {
        let err = Error::xml("unexpected end of input");
        let s = format!("{err}");
        assert!(s.contains("XML error"), "got: {s}");
    }

    #[test]
    fn display_opc_error() {
        let err = Error::opc("missing [Content_Types].xml");
        let s = format!("{err}");
        assert!(s.contains("OPC error"), "got: {s}");
    }

    #[test]
    fn display_resource_limit() {
        let err = Error::resource_limit("decompressed size exceeds 250 MB");
        let s = format!("{err}");
        assert!(s.contains("resource limit"), "got: {s}");
        assert!(s.contains("250 MB"), "got: {s}");
    }

    #[test]
    fn context_chain() {
        let err = Error::zip("bad EOCD")
            .with_context("scanning central directory")
            .with_context("opening ZIP archive");
        let s = format!("{err}");
        assert!(s.contains("scanning central directory"), "got: {s}");
        assert!(s.contains("opening ZIP archive"), "got: {s}");
    }

    #[test]
    fn result_ext_context() {
        let result: Result<i32> = Err(Error::xml("fail"));
        let result = result.context("parsing document.xml");
        let s = format!("{}", result.unwrap_err());
        assert!(s.contains("parsing document.xml"), "got: {s}");
    }

    #[test]
    fn io_error_from() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let err: Error = io_err.into();
        assert!(matches!(err, Error::Io { .. }));
        let s = format!("{err}");
        assert!(s.contains("I/O error"), "got: {s}");
    }

    #[test]
    fn io_context_preserved() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "x");
        let err: Error = io_err.into();
        let err = err.with_context("reading file");
        let s = format!("{err}");
        assert!(
            s.contains("reading file"),
            "IO context should be preserved: {s}"
        );
    }

    #[test]
    fn into_core_error() {
        let err = Error::zip("bad archive");
        let core_err: udoc_core::error::Error = err.into();
        let s = format!("{core_err}");
        assert!(s.contains("ZIP error"), "got: {s}");
    }

    #[test]
    fn error_is_send_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<Error>();
        assert_sync::<Error>();
    }
}

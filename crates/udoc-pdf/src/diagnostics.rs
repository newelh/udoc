use std::fmt;
use std::sync::Mutex;

use crate::object::ObjRef;

/// Classification of warnings for filtering and programmatic handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WarningKind {
    /// Malformed cross-reference table or stream.
    MalformedXref,
    /// Malformed numeric or other token.
    MalformedToken,
    /// Malformed string literal or hex string.
    MalformedString,
    /// Garbage bytes encountered during parsing.
    GarbageBytes,
    /// Unrecognized keyword in token stream.
    UnknownKeyword,
    /// Unexpected token where a different one was expected.
    UnexpectedToken,
    /// Array or dictionary not properly terminated.
    UnterminatedCollection,

    /// Stream /Length is missing, indirect, or incorrect.
    StreamLengthMismatch,
    /// Stream data extends past end of file.
    StreamExtendsPastEof,
    /// Filter type not supported (e.g. image filters).
    UnsupportedFilter,
    /// Error during stream filter decoding.
    DecodeError,

    // -- Object resolution --
    /// Object header doesn't match xref entry.
    ObjectHeaderMismatch,
    /// Missing endobj after object body.
    MissingEndObj,

    // -- Document structure --
    /// Cycle detected in page tree.
    PageTreeCycle,
    /// Invalid page tree structure (non-reference /Kids entry, unknown /Type, etc.).
    InvalidPageTree,

    // -- Content interpretation --
    /// Font not found in page /Resources or failed to load.
    FontError,
    /// Informational message about font loading (font loaded, encoding applied, etc.).
    FontLoaded,
    /// CID font's /W advance width array disagrees with the embedded
    /// font's hmtx / charstring widths by more than 10% on at least 5
    /// glyphs. Usually a PDF-generator bug; the PDF spec says /W wins
    /// but the disagreement signals that downstream text extraction may
    /// produce wrong widths. See #188.
    FontMetricsDisagreement,
    /// A font reference was resolved to a fallback rather than the exact font
    /// requested by the document. Emitted at every substitution point so
    /// callers can audit which fonts affected which extracted text.
    ///
    /// The warning message carries the requested font, the resolution used,
    /// and the [`FallbackReason`](udoc_core::text::FallbackReason) display form.
    FallbackFontSubstitution,
    /// Graphics or text state issue (stack overflow/underflow, invalid matrix).
    InvalidState,
    /// Missing or invalid image metadata (dimensions, color space, etc.)
    InvalidImageMetadata,
    /// Feature present in the file but not yet implemented (e.g. color-space-dependent operators).
    UnsupportedFeature,
    /// An `sh` operator referenced a shading dictionary whose /ShadingType
    /// is not implemented by the renderer. Today Type 2 (axial) and
    /// Type 3 (radial) are fully rendered; Types 1 (function-based) and
    /// 4-7 (mesh / tensor / Coons) fall through to the base fill color
    /// with this warning. The message carries the raw /ShadingType
    /// integer.
    ///
    /// ISO 32000-2 §8.7.4.
    UnsupportedShadingType,
    /// A Pattern colorspace fill referenced a pattern resource whose
    /// /PatternType or /PaintType combination is not implemented by
    /// the renderer., only Type 1 /PaintType 1
    /// (coloured tiling) is fully supported; Type 1 /PaintType 2
    /// (uncoloured tiling) and Type 2 (shading patterns painted via
    /// the Pattern colorspace rather than `sh`) fall through to the
    /// base fill color with this warning.
    ///
    /// The message carries the raw /PatternType and /PaintType values.
    /// ISO 32000-2 §8.7.3.
    UnsupportedPatternType,

    // -- Security limits --
    /// A resource limit was reached (page count, nesting depth, etc.).
    ResourceLimit,

    // -- Reading order --
    /// Informational: which reading order tier was selected for a page.
    TierSelection,
    /// Reading order algorithm degraded (cycle detected, ambiguous coherence, etc.).
    ReadingOrder,

    /// Informational message about encryption handling.
    EncryptedDocument,
}

/// Severity level for diagnostic messages.
///
/// Two levels are sufficient for v1: informational messages (font loaded, encoding
/// selected) and warnings (missing font, decode failure). Errors are returned via
/// `Result`, not routed through diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WarningLevel {
    /// Informational message (e.g. "loaded font Helvetica (Type1, WinAnsi)").
    Info,
    /// Warning about a recoverable issue (e.g. "font /F1 not found in /Resources").
    Warning,
}

/// Structured context for a diagnostic message.
///
/// Fields are all optional because different layers know different things.
/// Parse-layer code knows byte offsets but not page numbers. The content
/// interpreter knows page numbers but not always byte offsets. Font loading
/// knows the object reference of the font being loaded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct WarningContext {
    /// Zero-based page index, if known.
    pub page_index: Option<usize>,
    /// Object reference related to this warning, if known.
    pub obj_ref: Option<ObjRef>,
}

/// A diagnostic message emitted during parsing or text extraction.
///
/// Offsets are always relative to the original source file (the bytes passed to
/// `Document::open` or `Document::from_bytes`), never offsets into decoded stream
/// data. When no meaningful offset exists (e.g. a font loading issue discovered
/// during content interpretation), offset is `None`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Warning {
    /// Byte offset in the source where the issue was found, or `None` if
    /// no meaningful file offset is available.
    pub offset: Option<u64>,
    /// Classification of the warning.
    pub kind: WarningKind,
    /// Severity level.
    pub level: WarningLevel,
    /// Structured context (page, object reference).
    pub context: WarningContext,
    /// Human-readable message.
    pub message: String,
}

impl Warning {
    /// Create a warning with default level (Warning) and no context.
    pub fn new(offset: Option<u64>, kind: WarningKind, message: impl Into<String>) -> Self {
        Self {
            offset,
            kind,
            level: WarningLevel::Warning,
            context: WarningContext::default(),
            message: message.into(),
        }
    }

    /// Create a warning with default level (Warning) and explicit context.
    pub fn with_context(
        offset: Option<u64>,
        kind: WarningKind,
        context: WarningContext,
        message: impl Into<String>,
    ) -> Self {
        Self {
            offset,
            kind,
            level: WarningLevel::Warning,
            context,
            message: message.into(),
        }
    }

    /// Create an info-level message with no context.
    pub fn info(kind: WarningKind, message: impl Into<String>) -> Self {
        Self {
            offset: None,
            kind,
            level: WarningLevel::Info,
            context: WarningContext::default(),
            message: message.into(),
        }
    }

    /// Create an info-level message with explicit context.
    pub fn info_with_context(
        kind: WarningKind,
        context: WarningContext,
        message: impl Into<String>,
    ) -> Self {
        Self {
            offset: None,
            kind,
            level: WarningLevel::Info,
            context,
            message: message.into(),
        }
    }
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let level = match self.level {
            WarningLevel::Info => "info",
            WarningLevel::Warning => "warning",
        };
        match self.offset {
            Some(off) => write!(f, "{} at offset {}: {}", level, off, self.message),
            None => write!(f, "{} (offset unknown): {}", level, self.message),
        }
    }
}

/// Pluggable sink for parser diagnostics.
///
/// Implementations must be thread-safe (`Send + Sync`). The diagnostics sink is
/// created by the user via `Config` and threaded through the resolver to all
/// internal layers. Parse-layer types receive `&dyn DiagnosticsSink` at construction.
/// Higher layers access it via `resolver.diagnostics()`.
pub trait DiagnosticsSink: Send + Sync {
    /// Report a warning about a recoverable issue.
    fn warning(&self, warn: Warning);

    /// Report an informational message.
    ///
    /// Default implementation delegates to `warning()` with `WarningLevel::Info`.
    /// Override if you want to filter info messages separately.
    fn info(&self, warn: Warning) {
        debug_assert!(warn.level == WarningLevel::Info);
        self.warning(warn);
    }
}

/// A diagnostics sink that discards all messages.
#[derive(Debug)]
pub struct NullDiagnostics;

impl DiagnosticsSink for NullDiagnostics {
    fn warning(&self, _warn: Warning) {}
}

/// A diagnostics sink that collects all warnings for later inspection.
///
/// Useful for testing and debugging.
#[derive(Debug)]
pub struct CollectingDiagnostics {
    warnings: Mutex<Vec<Warning>>,
}

impl CollectingDiagnostics {
    /// Create a new collecting diagnostics sink.
    pub fn new() -> Self {
        Self {
            warnings: Mutex::new(Vec::new()),
        }
    }

    /// Get all collected warnings.
    pub fn warnings(&self) -> Vec<Warning> {
        self.warnings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl Default for CollectingDiagnostics {
    fn default() -> Self {
        Self::new()
    }
}

impl DiagnosticsSink for CollectingDiagnostics {
    fn warning(&self, warn: Warning) {
        self.warnings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(warn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_warning_display_with_offset() {
        let w = Warning::new(Some(42), WarningKind::MalformedToken, "bad number");
        let s = format!("{w}");
        assert!(s.contains("warning"), "got: {s}");
        assert!(s.contains("offset 42"), "got: {s}");
        assert!(s.contains("bad number"), "got: {s}");
    }

    #[test]
    fn test_warning_display_without_offset() {
        let w = Warning::new(None, WarningKind::FontError, "font missing");
        let s = format!("{w}");
        assert!(s.contains("offset unknown"), "got: {s}");
    }

    #[test]
    fn test_warning_display_info_level() {
        let w = Warning::info(WarningKind::FontLoaded, "loaded Helvetica");
        let s = format!("{w}");
        assert!(s.contains("info"), "got: {s}");
        assert!(!s.contains("warning"), "info should not say 'warning': {s}");
    }

    #[test]
    fn test_collecting_diagnostics_default() {
        let d = CollectingDiagnostics::default();
        assert!(d.warnings().is_empty());
    }

    #[test]
    fn test_collecting_diagnostics_collects() {
        let d = CollectingDiagnostics::new();
        d.warning(Warning::new(None, WarningKind::GarbageBytes, "junk"));
        d.warning(Warning::new(Some(10), WarningKind::DecodeError, "err"));
        assert_eq!(d.warnings().len(), 2);
    }

    #[test]
    fn test_info_delegates_to_warning() {
        let d = CollectingDiagnostics::new();
        d.info(Warning::info(WarningKind::FontLoaded, "loaded font"));
        let warnings = d.warnings();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].level, WarningLevel::Info);
    }
}

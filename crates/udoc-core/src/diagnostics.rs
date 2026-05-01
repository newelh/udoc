//! Diagnostics types for document extraction.
//!
//! Provides a pluggable warning/info sink shared across all format backends.
//! The [`DiagnosticsSink`] trait receives structured [`Warning`] messages
//! during extraction. Implementations must be Send + Sync.

use std::fmt;
use std::sync::{Arc, Mutex};

/// Severity level for diagnostic messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WarningLevel {
    /// Informational message (not a problem, just noteworthy).
    Info,
    /// Warning: something is wrong but extraction can continue.
    Warning,
}

/// Context for a diagnostic message.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct WarningContext {
    /// Zero-based page index, if applicable.
    pub page_index: Option<usize>,
    /// Additional detail string.
    pub detail: Option<String>,
    /// Structured payload for [`WarningKind::MissingGlyph`] warnings. When
    /// the renderer's glyph-lookup path exhausts the fallback chain (the
    /// named font has no glyph for the codepoint AND Tier 1 name-routing
    /// AND the Unicode-range sniff all miss), it emits a warning carrying
    /// this record so audit tools can aggregate frequencies without
    /// parsing the human-readable message.
    pub missing_glyph: Option<MissingGlyphInfo>,
}

/// Structured payload for a missing-glyph diagnostic.
///
/// Emitted by the renderer (see `udoc-render`) when the current font has
/// no glyph for the requested codepoint and every fallback path (Tier 1
/// name-routing, Unicode-range sniff, CJK, generic serif/sans) also
/// missed. The final rendered output in that case would be a `.notdef` /
/// replacement box.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MissingGlyphInfo {
    /// Font name as referenced in the source document (e.g. the PDF font
    /// name). Matches `PositionedSpan::font_name` / `font_id` on the span
    /// that triggered the lookup.
    pub font: String,
    /// Unicode codepoint the renderer attempted to resolve. Stored as u32
    /// rather than `char` so invalid/unassigned codepoints (which can
    /// flow out of CID decoding or bogus ToUnicode maps) serialize cleanly.
    pub codepoint: u32,
    /// Glyph id within the named font when known, or `0` (the `.notdef`
    /// slot in both TrueType and CFF) when the lookup couldn't resolve a
    /// glyph id at all.
    pub glyph_id: u32,
}

/// Classification of warnings for filtering and programmatic handling.
///
/// This enum absorbs all PDF-specific [`WarningKind`] variants previously
/// only visible inside the PDF crate, plus a [`Custom`]
/// catch-all string for backend-specific kinds that have not yet been
/// promoted to typed variants. Marked `#[non_exhaustive]` so new variants
/// can be added without breaking downstream matchers (those should always
/// include a `_` arm).
///
/// **Comparison shortcuts:** `WarningKind` implements `PartialEq<&str>` and
/// `PartialEq<str>` so existing tests like `warning.kind == "FontError"`
/// keep working: the comparison matches the variant's canonical name (the
/// `Debug` spelling) for typed variants and the inner string for `Custom`.
///
/// **Construction shortcut:** [`Warning::new`] accepts `impl Into<WarningKind>`
/// and `From<&str>` / `From<String>` produce `WarningKind::Custom(_)` so
/// backends that haven't migrated to typed kinds keep compiling.
///
/// [`Custom`]: WarningKind::Custom
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WarningKind {
    // ---- PDF parsing ----
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

    // ---- PDF streams ----
    /// Stream /Length is missing, indirect, or incorrect.
    StreamLengthMismatch,
    /// Stream data extends past end of file.
    StreamExtendsPastEof,
    /// Filter type not supported (e.g. image filters).
    UnsupportedFilter,
    /// Error during stream filter decoding.
    DecodeError,

    // ---- PDF object resolution ----
    /// Object header doesn't match xref entry.
    ObjectHeaderMismatch,
    /// Missing endobj after object body.
    MissingEndObj,

    // ---- PDF document structure ----
    /// Cycle detected in page tree.
    PageTreeCycle,
    /// Invalid page tree structure (non-reference /Kids entry, unknown /Type, etc.).
    InvalidPageTree,

    // ---- PDF content interpretation ----
    /// Font not found in page /Resources or failed to load.
    FontError,
    /// Informational message about font loading.
    FontLoaded,
    /// CID font's /W advance widths disagree with the embedded font's
    /// hmtx / charstring widths by more than 10% on at least 5 glyphs.
    FontMetricsDisagreement,
    /// A font reference resolved to a fallback rather than the exact font
    /// requested by the document.
    FallbackFontSubstitution,
    /// Graphics or text state issue (stack overflow/underflow, invalid matrix).
    InvalidState,
    /// Missing or invalid image metadata.
    InvalidImageMetadata,
    /// Feature present in the file but not yet implemented.
    UnsupportedFeature,
    /// `sh` operator referenced an unsupported shading type.
    UnsupportedShadingType,
    /// Pattern colorspace fill referenced an unsupported pattern type.
    UnsupportedPatternType,

    /// Renderer's glyph fallback chain exhausted; final output is a
    /// `.notdef` / replacement box. Carries a [`MissingGlyphInfo`] payload
    /// in [`WarningContext::missing_glyph`].
    MissingGlyph,

    // ---- Reading order ----
    /// Informational: which reading order tier was selected for a page.
    TierSelection,
    /// Reading order algorithm degraded (cycle detected, ambiguous coherence, etc.).
    ReadingOrder,

    /// Informational message about encryption handling.
    EncryptedDocument,

    // ---- Limits (shared) ----
    /// A resource limit was reached (page count, nesting depth, etc.).
    ResourceLimit,
    /// The collecting diagnostics sink hit its warning cap and dropped
    /// further warnings (SEC #62 sentinel).
    WarningLimitReached,
    /// The facade-installed `Document::diagnostics()` collector hit its
    /// `Limits::max_warnings` cap; this variant carries the count of
    /// warnings dropped beyond the cap.
    ///
    /// Distinguished from [`WarningKind::WarningLimitReached`] so callers
    /// can tell "the per-document collector for `doc.diagnostics()` was
    /// truncated" from "the bare `CollectingDiagnostics` sink hit its
    /// own cap". The `WarningLimitReached` variant has been around since
    /// SEC #62 and downstream code may match it; this new variant
    /// is the contract for the four-state matrix and carries a numeric
    /// payload.
    WarningsTruncated {
        /// Number of warnings suppressed beyond the cap.
        suppressed: usize,
    },

    // ---- Backend-specific (escape hatch) ----
    /// Catch-all for backend-specific kinds not yet promoted to typed
    /// variants. Constructed automatically by `From<&str>` and
    /// `From<String>`. New backends should prefer adding a typed variant
    /// when the kind is part of a stable contract; `Custom` is fine for
    /// internal / one-off categories.
    Custom(String),
}

impl WarningKind {
    /// Canonical string spelling of this kind. For typed variants, returns
    /// the variant name (matches the `Debug` representation for unit
    /// variants); for `Custom`, returns the inner string slice.
    ///
    /// Used by `Display`, by the bridge from PDF to core, and by the
    /// `PartialEq<str>` / `PartialEq<&str>` impls that keep legacy
    /// `kind == "FooBar"` comparisons compiling.
    pub fn as_str(&self) -> &str {
        match self {
            WarningKind::MalformedXref => "MalformedXref",
            WarningKind::MalformedToken => "MalformedToken",
            WarningKind::MalformedString => "MalformedString",
            WarningKind::GarbageBytes => "GarbageBytes",
            WarningKind::UnknownKeyword => "UnknownKeyword",
            WarningKind::UnexpectedToken => "UnexpectedToken",
            WarningKind::UnterminatedCollection => "UnterminatedCollection",
            WarningKind::StreamLengthMismatch => "StreamLengthMismatch",
            WarningKind::StreamExtendsPastEof => "StreamExtendsPastEof",
            WarningKind::UnsupportedFilter => "UnsupportedFilter",
            WarningKind::DecodeError => "DecodeError",
            WarningKind::ObjectHeaderMismatch => "ObjectHeaderMismatch",
            WarningKind::MissingEndObj => "MissingEndObj",
            WarningKind::PageTreeCycle => "PageTreeCycle",
            WarningKind::InvalidPageTree => "InvalidPageTree",
            WarningKind::FontError => "FontError",
            WarningKind::FontLoaded => "FontLoaded",
            WarningKind::FontMetricsDisagreement => "FontMetricsDisagreement",
            WarningKind::FallbackFontSubstitution => "FallbackFontSubstitution",
            WarningKind::InvalidState => "InvalidState",
            WarningKind::InvalidImageMetadata => "InvalidImageMetadata",
            WarningKind::UnsupportedFeature => "UnsupportedFeature",
            WarningKind::UnsupportedShadingType => "UnsupportedShadingType",
            WarningKind::UnsupportedPatternType => "UnsupportedPatternType",
            WarningKind::MissingGlyph => "MissingGlyph",
            WarningKind::TierSelection => "TierSelection",
            WarningKind::ReadingOrder => "ReadingOrder",
            WarningKind::EncryptedDocument => "EncryptedDocument",
            WarningKind::ResourceLimit => "ResourceLimit",
            WarningKind::WarningLimitReached => "WarningLimitReached",
            WarningKind::WarningsTruncated { .. } => "WarningsTruncated",
            WarningKind::Custom(s) => s.as_str(),
        }
    }
}

impl fmt::Display for WarningKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for WarningKind {
    fn from(s: &str) -> Self {
        WarningKind::Custom(s.to_string())
    }
}

impl From<String> for WarningKind {
    fn from(s: String) -> Self {
        WarningKind::Custom(s)
    }
}

impl PartialEq<&str> for WarningKind {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<str> for WarningKind {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<WarningKind> for &str {
    fn eq(&self, other: &WarningKind) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<WarningKind> for str {
    fn eq(&self, other: &WarningKind) -> bool {
        self == other.as_str()
    }
}

/// Canonical kind strings used by warnings from this crate and downstream
/// crates. Retained for backward compatibility with code that compared
/// `kind` against these constants before [`WarningKind`] was promoted to
/// an enum. New code should prefer the typed variants
/// directly.
pub mod kind {
    /// Stable spelling of [`super::WarningKind::MissingGlyph`].
    pub const MISSING_GLYPH: &str = "MissingGlyph";
}

/// A diagnostic message emitted during document extraction.
///
/// The `kind` field is a [`WarningKind`] enum that carries PDF / renderer
/// kinds as typed variants and other backend-specific kinds as
/// [`WarningKind::Custom`]. The enum is `#[non_exhaustive]` so new typed
/// variants can be added without breaking existing matchers (use a `_`
/// arm in pattern matches).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Warning {
    /// Byte offset in the source data, if applicable.
    pub offset: Option<u64>,
    /// Warning severity.
    pub level: WarningLevel,
    /// Structured context (page index, etc.).
    pub context: WarningContext,
    /// Human-readable description.
    pub message: String,
    /// Machine-readable warning category. Format-specific.
    pub kind: WarningKind,
}

impl Warning {
    /// Create a warning with the given kind, message, and offset.
    pub fn new(kind: impl Into<WarningKind>, message: impl Into<String>) -> Self {
        Self {
            offset: None,
            level: WarningLevel::Warning,
            context: WarningContext::default(),
            message: message.into(),
            kind: kind.into(),
        }
    }

    /// Set the byte offset for this warning.
    pub fn at_offset(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Set the warning level.
    pub fn with_level(mut self, level: WarningLevel) -> Self {
        self.level = level;
        self
    }

    /// Set the warning context.
    pub fn with_context(mut self, context: WarningContext) -> Self {
        self.context = context;
        self
    }

    /// Build a [`WarningKind::MissingGlyph`] warning carrying a structured
    /// [`MissingGlyphInfo`] payload in [`WarningContext::missing_glyph`].
    ///
    /// Message is generated for human-readable logging; machine consumers
    /// should pattern-match on `kind == WarningKind::MissingGlyph` and read
    /// the structured payload rather than parse the message.
    pub fn missing_glyph(info: MissingGlyphInfo) -> Self {
        let message = format!(
            "font '{}' has no glyph for U+{:04X} (glyph_id {}); fallback chain exhausted",
            info.font, info.codepoint, info.glyph_id,
        );
        let context = WarningContext {
            missing_glyph: Some(info),
            ..WarningContext::default()
        };
        Self {
            offset: None,
            level: WarningLevel::Warning,
            context,
            message,
            kind: WarningKind::MissingGlyph,
        }
    }
}

/// Trait for receiving diagnostic messages during extraction.
///
/// Implementations must be Send + Sync because the sink is shared
/// via `Arc` across extraction operations.
pub trait DiagnosticsSink: Send + Sync {
    /// Receive a warning or info message.
    fn warning(&self, warning: Warning);

    /// Convenience method for info-level messages. Default delegates
    /// to [`warning`](DiagnosticsSink::warning).
    fn info(&self, warning: Warning) {
        self.warning(warning);
    }
}

/// Diagnostics sink that discards all messages.
pub struct NullDiagnostics;

impl DiagnosticsSink for NullDiagnostics {
    fn warning(&self, _warning: Warning) {}
}

/// Diagnostics sink that collects all messages into a Vec.
///
/// The sink enforces a hard cap on the collected warning count
/// (SEC #62  round-2 audit, CVSS 7.5): a malformed PDF / OOXML can
/// trigger thousands of `warning()` calls, each allocating a `String`
/// for the message and `WarningContext` payload. Without a cap, that's
/// an attacker-controlled allocation budget. The cap defaults to
/// [`crate::limits::DEFAULT_MAX_WARNINGS`] = 1000; when reached, a
/// single "warning-limit-reached" sentinel is appended and further
/// warnings are dropped.
pub struct CollectingDiagnostics {
    warnings: Mutex<Vec<Warning>>,
    /// Hard cap on Vec length (1000 by default). The +1 sentinel can
    /// push the actual length to `max_warnings + 1`.
    max_warnings: usize,
}

impl CollectingDiagnostics {
    /// Create a new empty collecting sink with the default warning cap
    /// ([`crate::limits::DEFAULT_MAX_WARNINGS`] = 1000).
    ///
    /// ```
    /// use udoc_core::diagnostics::CollectingDiagnostics;
    /// let sink = CollectingDiagnostics::new();
    /// assert_eq!(sink.warnings().len(), 0);
    /// ```
    pub fn new() -> Self {
        Self::with_max_warnings(crate::limits::DEFAULT_MAX_WARNINGS)
    }

    /// Create a sink with an explicit warning cap. Pass `usize::MAX`
    /// to disable the cap entirely (not recommended on untrusted input).
    ///
    /// ```
    /// use udoc_core::diagnostics::CollectingDiagnostics;
    /// let sink = CollectingDiagnostics::with_max_warnings(50);
    /// assert_eq!(sink.warnings().len(), 0);
    /// ```
    pub fn with_max_warnings(max_warnings: usize) -> Self {
        Self {
            warnings: Mutex::new(Vec::new()),
            max_warnings,
        }
    }

    /// Return all collected warnings.
    ///
    /// ```
    /// use udoc_core::diagnostics::{CollectingDiagnostics, DiagnosticsSink, Warning, WarningKind};
    /// let sink = CollectingDiagnostics::new();
    /// sink.warning(Warning::new(WarningKind::Custom("Demo".into()), "hello"));
    /// let collected = sink.warnings();
    /// assert_eq!(collected.len(), 1);
    /// assert_eq!(collected[0].message, "hello");
    /// ```
    pub fn warnings(&self) -> Vec<Warning> {
        self.warnings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Remove all collected warnings and return them.
    ///
    /// ```
    /// use udoc_core::diagnostics::{CollectingDiagnostics, DiagnosticsSink, Warning, WarningKind};
    /// let sink = CollectingDiagnostics::new();
    /// sink.warning(Warning::new(WarningKind::Custom("Demo".into()), "hello"));
    /// assert_eq!(sink.take_warnings().len(), 1);
    /// // Drained: the second take is empty.
    /// assert_eq!(sink.take_warnings().len(), 0);
    /// ```
    pub fn take_warnings(&self) -> Vec<Warning> {
        let mut w = self.warnings.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *w)
    }
}

impl Default for CollectingDiagnostics {
    fn default() -> Self {
        Self::new()
    }
}

impl DiagnosticsSink for CollectingDiagnostics {
    fn warning(&self, warning: Warning) {
        let mut w = self.warnings.lock().unwrap_or_else(|e| e.into_inner());
        if w.len() >= self.max_warnings {
            // Append a single sentinel the FIRST time we hit the cap so
            // downstream consumers can tell "everything fine, just 1000
            // real warnings" from "warnings were suppressed". After
            // that, drop further warnings on the floor (no allocation).
            if w.len() == self.max_warnings {
                w.push(Warning::new(
                    WarningKind::WarningLimitReached,
                    format!(
                        "warning limit ({}) reached; further warnings suppressed",
                        self.max_warnings
                    ),
                ));
            }
            return;
        }
        w.push(warning);
    }
}

/// Diagnostics sink that forwards every warning to two underlying sinks.
///
/// Used by the four-state behavior matrix in
/// to support the case where a caller wants both their own custom sink
/// AND `Document::diagnostics()` populated. The facade installs
/// `TeeDiagnostics::new(my_sink, internal_collector)` when the user has
/// both passed a custom sink via [`crate::error`]-style configuration
/// AND explicitly opted into `collect_diagnostics(true)`.
///
/// ```
/// use std::sync::Arc;
/// use udoc_core::diagnostics::{
///     CollectingDiagnostics, DiagnosticsSink, TeeDiagnostics, Warning,
/// };
///
/// let primary = Arc::new(CollectingDiagnostics::new());
/// let secondary = Arc::new(CollectingDiagnostics::new());
/// let tee = TeeDiagnostics::new(primary.clone(), secondary.clone());
/// tee.warning(Warning::new("Demo", "hello"));
/// assert_eq!(primary.warnings().len(), 1);
/// assert_eq!(secondary.warnings().len(), 1);
/// ```
pub struct TeeDiagnostics {
    primary: Arc<dyn DiagnosticsSink>,
    secondary: Arc<dyn DiagnosticsSink>,
}

impl TeeDiagnostics {
    /// Create a Tee sink that forwards each warning to both inner sinks.
    pub fn new(primary: Arc<dyn DiagnosticsSink>, secondary: Arc<dyn DiagnosticsSink>) -> Self {
        Self { primary, secondary }
    }
}

impl DiagnosticsSink for TeeDiagnostics {
    fn warning(&self, warning: Warning) {
        // Clone is unavoidable: each sink takes its own owned copy and
        // may stash it indefinitely. The cost is one Vec<u8> + a few
        // small fields per call -- negligible compared to the work the
        // backends did to produce the warning.
        self.primary.warning(warning.clone());
        self.secondary.warning(warning);
    }

    fn info(&self, warning: Warning) {
        self.primary.info(warning.clone());
        self.secondary.info(warning);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_diagnostics() {
        let sink = NullDiagnostics;
        sink.warning(Warning {
            offset: Some(42),
            level: WarningLevel::Warning,
            context: WarningContext::default(),
            message: "test".into(),
            kind: WarningKind::Custom("TestWarning".into()),
        });
        // just verifies it doesn't panic
    }

    #[test]
    fn collecting_diagnostics() {
        let sink = CollectingDiagnostics::new();
        sink.warning(Warning {
            offset: None,
            level: WarningLevel::Info,
            context: WarningContext {
                page_index: Some(0),
                detail: None,
                missing_glyph: None,
            },
            message: "info msg".into(),
            kind: WarningKind::Custom("InfoKind".into()),
        });
        sink.warning(Warning {
            offset: Some(100),
            level: WarningLevel::Warning,
            context: WarningContext::default(),
            message: "warn msg".into(),
            kind: WarningKind::Custom("WarnKind".into()),
        });
        let warnings = sink.warnings();
        assert_eq!(warnings.len(), 2);
        assert_eq!(warnings[0].kind, "InfoKind");
        assert_eq!(warnings[1].offset, Some(100));
    }

    #[test]
    fn take_warnings_drains() {
        let sink = CollectingDiagnostics::new();
        sink.warning(Warning::new("A", "first"));
        sink.warning(Warning::new("B", "second"));
        let taken = sink.take_warnings();
        assert_eq!(taken.len(), 2);
        // After take, the collector is empty.
        assert_eq!(sink.warnings().len(), 0);
        // New warnings accumulate from scratch.
        sink.warning(Warning::new("C", "third"));
        assert_eq!(sink.warnings().len(), 1);
    }

    #[test]
    fn warning_level_eq() {
        assert_eq!(WarningLevel::Info, WarningLevel::Info);
        assert_ne!(WarningLevel::Info, WarningLevel::Warning);
    }

    #[test]
    fn send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NullDiagnostics>();
        assert_send_sync::<CollectingDiagnostics>();
    }

    #[test]
    fn collecting_diagnostics_caps_at_max_warnings() {
        // SEC #62  round-2 audit: malformed input emitting unbounded
        // warnings used to pass through; now we cap at max_warnings + 1
        // (sentinel) and drop the rest on the floor.
        let sink = CollectingDiagnostics::with_max_warnings(5);
        for i in 0..100 {
            sink.warning(Warning::new("X", format!("warn {i}")));
        }
        let warnings = sink.warnings();
        assert_eq!(
            warnings.len(),
            6,
            "5 real warnings + 1 'limit reached' sentinel; got {}",
            warnings.len()
        );
        assert_eq!(warnings[5].kind, WarningKind::WarningLimitReached);
        assert!(warnings[5].message.contains("5"));
    }

    #[test]
    fn collecting_diagnostics_default_cap_is_1000() {
        let sink = CollectingDiagnostics::default();
        for i in 0..2000 {
            sink.warning(Warning::new("X", format!("warn {i}")));
        }
        // 1000 real + 1 sentinel = 1001
        assert_eq!(sink.warnings().len(), 1001);
    }

    #[test]
    fn missing_glyph_helper_populates_context() {
        let w = Warning::missing_glyph(MissingGlyphInfo {
            font: "Foo-Regular".into(),
            codepoint: 0x03A9, // U+03A9 GREEK CAPITAL LETTER OMEGA
            glyph_id: 0,
        });
        assert_eq!(w.kind, WarningKind::MissingGlyph);
        assert_eq!(w.kind, kind::MISSING_GLYPH); // legacy str-constant compat
        assert_eq!(w.level, WarningLevel::Warning);
        let info = w
            .context
            .missing_glyph
            .as_ref()
            .expect("missing_glyph payload");
        assert_eq!(info.font, "Foo-Regular");
        assert_eq!(info.codepoint, 0x03A9);
        assert_eq!(info.glyph_id, 0);
        assert!(
            w.message.contains("U+03A9"),
            "rendered message should include codepoint, got: {}",
            w.message
        );
    }

    #[test]
    fn warning_kind_str_compat() {
        // Legacy `kind == "Foo"` style comparisons must keep working for
        // both typed variants and Custom strings.
        let typed = WarningKind::StreamLengthMismatch;
        assert_eq!(typed, "StreamLengthMismatch");
        assert_eq!("StreamLengthMismatch", typed);
        assert_ne!(typed, "FontError");

        let custom = WarningKind::from("XlsxInvalidSstIndex");
        assert_eq!(custom, "XlsxInvalidSstIndex");
        assert_eq!(custom.as_str(), "XlsxInvalidSstIndex");

        // Display matches as_str.
        assert_eq!(format!("{typed}"), "StreamLengthMismatch");
        assert_eq!(format!("{custom}"), "XlsxInvalidSstIndex");
    }

    #[test]
    fn warning_new_accepts_typed_kind() {
        let w = Warning::new(WarningKind::FontError, "missing /F1");
        assert_eq!(w.kind, WarningKind::FontError);
        let w = Warning::new("StreamLengthMismatch", "bad length");
        // Constructed via &str -> Custom; matches typed variant by string.
        assert_eq!(w.kind, "StreamLengthMismatch");
        assert!(matches!(w.kind, WarningKind::Custom(_)));
    }

    #[test]
    fn warnings_truncated_variant_serializes_as_str() {
        let k = WarningKind::WarningsTruncated { suppressed: 17 };
        assert_eq!(k.as_str(), "WarningsTruncated");
        // PartialEq<&str> uses the canonical name.
        assert_eq!(k, "WarningsTruncated");
        // Display uses the canonical name (no payload formatting).
        assert_eq!(format!("{k}"), "WarningsTruncated");
    }

    #[test]
    fn warnings_truncated_variant_carries_suppressed_count() {
        let k = WarningKind::WarningsTruncated { suppressed: 1234 };
        match k {
            WarningKind::WarningsTruncated { suppressed } => assert_eq!(suppressed, 1234),
            other => panic!("expected WarningsTruncated, got {other:?}"),
        }
    }

    #[test]
    fn tee_forwards_to_both_sinks() {
        let primary = Arc::new(CollectingDiagnostics::new());
        let secondary = Arc::new(CollectingDiagnostics::new());
        let tee = TeeDiagnostics::new(primary.clone(), secondary.clone());
        tee.warning(Warning::new("A", "first"));
        tee.warning(Warning::new("B", "second"));
        assert_eq!(primary.warnings().len(), 2);
        assert_eq!(secondary.warnings().len(), 2);
        assert_eq!(primary.warnings()[0].message, "first");
        assert_eq!(secondary.warnings()[1].message, "second");
    }

    #[test]
    fn tee_info_path_forwards() {
        let primary = Arc::new(CollectingDiagnostics::new());
        let secondary = Arc::new(CollectingDiagnostics::new());
        let tee = TeeDiagnostics::new(primary.clone(), secondary.clone());
        let mut w = Warning::new("Info", "informational");
        w.level = WarningLevel::Info;
        tee.info(w);
        assert_eq!(primary.warnings().len(), 1);
        assert_eq!(secondary.warnings().len(), 1);
    }

    #[test]
    fn tee_with_null_secondary_is_loss_free() {
        // A common shape from the matrix: Tee(my_collector, NullDiagnostics)
        // when collect_diagnostics(false) but the user passed a sink. The
        // null side just discards.
        let primary = Arc::new(CollectingDiagnostics::new());
        let null = Arc::new(NullDiagnostics);
        let tee = TeeDiagnostics::new(primary.clone(), null);
        tee.warning(Warning::new("X", "msg"));
        assert_eq!(primary.warnings().len(), 1);
    }

    #[test]
    fn tee_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TeeDiagnostics>();
    }
}

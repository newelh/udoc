//! Streaming extractor API.
//!
//! The Extractor provides page-level streaming access to document content
//! without building the full Document model upfront. Use `into_document()`
//! to materialize the full model when needed.

use std::path::Path;

use udoc_core::document::DocumentMetadata;
use udoc_core::error::{Error, Result, ResultExt};

/// Wrap a backend open-from-path error with format name and path context.
fn wrap_open_err<E: std::error::Error + Send + Sync + 'static>(
    format: &str,
    path: &Path,
    e: E,
) -> Error {
    Error::with_source(format!("opening {format} '{}'", path.display()), e)
}

/// Wrap a backend open-from-bytes error with format name context.
fn wrap_bytes_err<E: std::error::Error + Send + Sync + 'static>(format: &str, e: E) -> Error {
    Error::with_source(format!("opening {format} from bytes"), e)
}

/// Read `(VmRSS_kb, VmHWM_kb)` from `/proc/self/status`.
///
/// Returns `(None, None)` on non-Linux or on parse failure. Used by the
/// [`Limits::memory_budget`](udoc_core::limits::Limits::memory_budget)
/// auto-reset path (T60-MEMBATCH). Matches the bench-compare RSS reader
/// to keep the two in sync.
fn read_rss_kb() -> (Option<u64>, Option<u64>) {
    #[cfg(target_os = "linux")]
    {
        let status = match std::fs::read_to_string("/proc/self/status") {
            Ok(s) => s,
            Err(_) => return (None, None),
        };
        let mut rss = None;
        let mut hwm = None;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                rss = rest.split_whitespace().next().and_then(|s| s.parse().ok());
            } else if let Some(rest) = line.strip_prefix("VmHWM:") {
                hwm = rest.split_whitespace().next().and_then(|s| s.parse().ok());
            }
        }
        (rss, hwm)
    }
    #[cfg(not(target_os = "linux"))]
    {
        (None, None)
    }
}

use crate::backend_trait::InternalBackend;
use crate::detect::Format;
use crate::doc_backend::DocInternalBackend;
use crate::docx_backend::DocxInternalBackend;
use crate::md_backend::MdInternalBackend;
use crate::odf_backend::{OdpInternalBackend, OdsInternalBackend, OdtInternalBackend};
use crate::pdf_backend::PdfInternalBackend;
use crate::ppt_backend::PptInternalBackend;
use crate::pptx_backend::PptxInternalBackend;
use crate::rtf_backend::RtfInternalBackend;
use crate::xls_backend::XlsInternalBackend;
use crate::xlsx_backend::XlsxInternalBackend;
use crate::Config;
use crate::Document;

/// Generate `dispatch_open` and `dispatch_bytes` functions from a single
/// format-to-backend mapping. PDF is excluded because it takes a separate
/// code path (password config, dedicated helper functions).
macro_rules! define_format_dispatch {
    ( $( $variant:ident => $backend:ident, $doc_ty:path, $label:expr; )* ) => {
        fn dispatch_open(
            format: Format,
            path: &std::path::Path,
            diag: std::sync::Arc<dyn udoc_core::diagnostics::DiagnosticsSink>,
        ) -> Result<Box<dyn InternalBackend>> {
            match format {
                $( Format::$variant => Ok(Box::new($backend::new(
                    <$doc_ty>::open_with_diag(path, diag)
                        .map_err(|e| wrap_open_err($label, path, e))?,
                ))), )*
                _ => Err(Error::new(format!("format {} not yet supported", format))),
            }
        }

        fn dispatch_bytes(
            format: Format,
            data: &[u8],
            diag: std::sync::Arc<dyn udoc_core::diagnostics::DiagnosticsSink>,
        ) -> Result<Box<dyn InternalBackend>> {
            match format {
                $( Format::$variant => Ok(Box::new($backend::new(
                    <$doc_ty>::from_bytes_with_diag(data, diag)
                        .map_err(|e| wrap_bytes_err($label, e))?,
                ))), )*
                _ => Err(Error::new(format!("format {} not yet supported", format))),
            }
        }
    };
}

define_format_dispatch! {
    Rtf  => RtfInternalBackend,  udoc_rtf::RtfDocument,      "RTF";
    Md   => MdInternalBackend,   udoc_markdown::MdDocument,   "Markdown";
    Pptx => PptxInternalBackend, udoc_pptx::PptxDocument,     "PPTX";
    Docx => DocxInternalBackend, udoc_docx::DocxDocument,     "DOCX";
    Xlsx => XlsxInternalBackend, udoc_xlsx::XlsxDocument,     "XLSX";
    Odt  => OdtInternalBackend,  udoc_odf::OdfDocument,       "ODT";
    Ods  => OdsInternalBackend,  udoc_odf::OdfDocument,       "ODS";
    Odp  => OdpInternalBackend,  udoc_odf::OdfDocument,       "ODP";
    Ppt  => PptInternalBackend,  udoc_ppt::PptDocument,       "PPT";
    Doc  => DocInternalBackend,  udoc_doc::DocDocument,        "DOC";
    Xls  => XlsInternalBackend,  udoc_xls::XlsDocument,        "XLS";
}

/// Backend-agnostic document extractor with page-level streaming access.
///
/// **Stability: experimental.** Per  §4.6, `Extractor` is not
/// part of the alpha-development frozen surface. Session usage data through
///  confirms that `extract()` / `extract_bytes()` are reached for ~99%
/// of the time; `Extractor` is rare. We reserve the right to demote or
/// rework `Extractor` between alpha tags. Prefer `udoc::extract*` for new
/// code unless you have a memory-bounded streaming requirement.
///
/// The Extractor holds an open document and provides methods to access
/// individual pages without building the full Document model. This is
/// useful for streaming scenarios or when you only need a subset of pages.
///
/// ```ignore
/// use udoc::Extractor;
///
/// // Drive the same API on the bundled fixture so the doctest runs end to end.
/// let bytes = include_bytes!("../../../tests/corpus/minimal/hello.pdf");
/// let mut ext = Extractor::from_bytes(bytes)?;
/// for i in 0..ext.page_count() {
///     let _ = ext.page_text(i)?;
/// }
/// // Or materialize the full document model:
/// let doc = ext.into_document()?;
/// assert_eq!(doc.metadata.page_count, 1);
/// # Ok::<(), udoc::Error>(())
/// ```
pub struct Extractor {
    inner: Box<dyn InternalBackend>,
    config: Config,
    /// When `Config::collect_diagnostics` is true (the default), the
    /// facade installs an internal [`CollectingDiagnostics`] sink and
    /// keeps a reference here so [`Extractor::diagnostics`] can read
    /// what's collected so far and [`Extractor::into_document`] can
    /// drain into [`Document::diagnostics`]. `None` when the user has
    /// installed a custom sink WITHOUT opting into the Tee mode (i.e.
    /// state 2 of the  four-state matrix).
    internal_collector: Option<std::sync::Arc<udoc_core::diagnostics::CollectingDiagnostics>>,
}

/// Resolve the four-state behavior matrix on a `Config` and
/// install the right sink. Returns `(installed_sink, internal_collector)`.
///
/// State table:
///
/// | State | `collect_diagnostics` | custom sink set | Installed | Collector |
/// |-------|----------------------|-----------------|-----------|-----------|
/// | 1     | true (default)       | no              | internal  | Some      |
/// | 2     | false (implicit)     | yes             | custom    | None      |
/// | 3     | false (explicit)     | no              | Null      | None      |
/// | 4     | true (forced)        | yes             | Tee(c, i) | Some      |
///
/// State 2 is set by `.diagnostics(custom)` (which implicitly disables
/// `collect_diagnostics`); state 4 is set by
/// `.diagnostics(custom).collect_diagnostics(true)` -- the explicit
/// re-arm AFTER setting the sink.
///
/// Disambiguation between states 1 and 4 uses
/// `Config::custom_diagnostics_set`, set internally by
/// `Config::diagnostics(...)`.
fn install_diagnostics(
    config: &mut Config,
) -> (
    std::sync::Arc<dyn udoc_core::diagnostics::DiagnosticsSink>,
    Option<std::sync::Arc<udoc_core::diagnostics::CollectingDiagnostics>>,
) {
    use udoc_core::diagnostics::{CollectingDiagnostics, TeeDiagnostics};

    let collect = config.collect_diagnostics;
    let custom_set = config.custom_diagnostics_set;

    if !collect {
        // States 2 and 3: do not install our own collector.
        let sink = config.diagnostics.clone();
        return (sink, None);
    }

    // Per-Document cap. None -> no cap (use usize::MAX).
    let cap = config.limits.max_warnings.unwrap_or(usize::MAX);
    let internal = std::sync::Arc::new(CollectingDiagnostics::with_max_warnings(cap));

    let installed: std::sync::Arc<dyn udoc_core::diagnostics::DiagnosticsSink> = if custom_set {
        // State 4: user opted into Tee.
        let internal_dyn: std::sync::Arc<dyn udoc_core::diagnostics::DiagnosticsSink> =
            internal.clone();
        std::sync::Arc::new(TeeDiagnostics::new(
            config.diagnostics.clone(),
            internal_dyn,
        ))
    } else {
        // State 1: only the internal collector.
        internal.clone() as std::sync::Arc<dyn udoc_core::diagnostics::DiagnosticsSink>
    };

    // Update the config so backends that read config.diagnostics during
    // open (e.g. open_pdf_path) see the installed sink.
    config.diagnostics = installed.clone();
    (installed, Some(internal))
}

/// Drain the internal collector's warnings, swapping the legacy
/// `WarningLimitReached` sentinel for the new typed
/// `WarningsTruncated { suppressed }` variant.
fn drain_internal_collector(
    collector: &Option<std::sync::Arc<udoc_core::diagnostics::CollectingDiagnostics>>,
    cap: Option<usize>,
) -> Vec<udoc_core::diagnostics::Warning> {
    use udoc_core::diagnostics::WarningKind;
    let Some(c) = collector else {
        return Vec::new();
    };
    let mut ws = c.take_warnings();
    if let Some(last) = ws.last_mut() {
        if matches!(last.kind, WarningKind::WarningLimitReached) {
            // CollectingDiagnostics caps Vec at cap+1 (cap real + 1
            // sentinel) and silently drops the rest. We can't recover
            // the exact suppressed count after the fact -- just record
            // "at least one was dropped" via a non-zero value when a
            // cap is configured. None-cap means we never installed the
            // sentinel, so this branch is unreachable in that case.
            let suppressed = match cap {
                Some(_) => 1, // exact count is lost; non-zero signals truncation
                None => 0,
            };
            last.kind = WarningKind::WarningsTruncated { suppressed };
            last.message = format!(
                "diagnostics collector cap reached; further warnings suppressed (cap={:?})",
                cap
            );
        }
    }
    ws
}

impl Extractor {
    /// Open a document from a file path with default config.
    ///
    /// ```no_run
    /// // ignore-runtime: requires a user-supplied file path that isn't bundled.
    /// let mut ext = udoc::Extractor::open("report.pdf")?;
    /// let _ = ext.page_count();
    /// # Ok::<(), udoc::Error>(())
    /// ```
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        Self::open_with(path, Config::default())
    }

    /// Open a document from a file path with custom config.
    ///
    /// ```no_run
    /// // ignore-runtime: requires a user-supplied file path that isn't bundled.
    /// use udoc::{Config, Extractor, Format};
    /// let cfg = Config::new().format(Format::Pdf);
    /// let mut ext = Extractor::open_with("report.pdf", cfg)?;
    /// let _ = ext.metadata();
    /// # Ok::<(), udoc::Error>(())
    /// ```
    pub fn open_with(path: impl AsRef<std::path::Path>, mut config: Config) -> Result<Self> {
        let path = path.as_ref();

        // Enforce file size limit from config (check before reading)
        if let Ok(meta) = std::fs::metadata(path) {
            let max_size = config.limits.max_file_size;
            if meta.len() > max_size {
                return Err(Error::new(format!(
                    "'{}' is {} bytes, exceeds max_file_size limit ({} bytes)",
                    path.display(),
                    meta.len(),
                    max_size
                )));
            }
        }

        // Resolve the four-state behavior matrix and install the right
        // diagnostics sink BEFORE detect_format/dispatch so the backends
        // see the same sink we'll drain into Document::diagnostics later.
        let (_installed, internal_collector) = install_diagnostics(&mut config);

        // Detect format. When detection requires reading the full file
        // (ZIP/OLE2 container inspection), the bytes are preserved so we
        // can pass them to the backend constructor instead of reading
        // from disk a second time (R-004).
        let (format, preread_data) = match config.format {
            Some(f) => (f, None),
            None => {
                let det = crate::detect::detect_format_path_reuse(path)
                    .context("detecting document format")?
                    .ok_or_else(|| {
                        Error::new(format!("unable to detect format for '{}'", path.display()))
                    })?;
                (det.format, det.data)
            }
        };

        let inner: Box<dyn InternalBackend> = if format == Format::Pdf {
            Box::new(PdfInternalBackend::new(crate::open_pdf_path(
                path, &config,
            )?))
        } else if let Some(data) = preread_data {
            // Reuse the bytes we already read during format detection.
            dispatch_bytes(format, &data, config.diagnostics.clone())?
        } else {
            dispatch_open(format, path, config.diagnostics.clone())?
        };

        Ok(Self {
            inner,
            config,
            internal_collector,
        })
    }

    /// Create an extractor from in-memory bytes with default config.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        Self::from_bytes_with(data, Config::default())
    }

    /// Create an extractor from in-memory bytes with custom config.
    pub fn from_bytes_with(data: &[u8], mut config: Config) -> Result<Self> {
        // Enforce file size limit from config
        let max_size = config.limits.max_file_size;
        if data.len() as u64 > max_size {
            return Err(Error::new(format!(
                "input size ({} bytes) exceeds max_file_size limit ({} bytes)",
                data.len(),
                max_size
            )));
        }

        // Resolve the four-state behavior matrix BEFORE format dispatch.
        let (_installed, internal_collector) = install_diagnostics(&mut config);

        let format = config
            .format
            .or_else(|| crate::detect::detect_format(data))
            .ok_or_else(|| Error::new("unable to detect format from bytes (unknown magic)"))?;

        let inner: Box<dyn InternalBackend> = if format == Format::Pdf {
            Box::new(PdfInternalBackend::new(crate::open_pdf_bytes(
                data, &config,
            )?))
        } else {
            dispatch_bytes(format, data, config.diagnostics.clone())?
        };

        Ok(Self {
            inner,
            config,
            internal_collector,
        })
    }

    /// Number of pages in the document, clamped to `config.limits.max_pages`.
    ///
    /// ```ignore
    /// let bytes = include_bytes!("../../../tests/corpus/minimal/hello.pdf");
    /// let ext = udoc::Extractor::from_bytes(bytes)?;
    /// assert_eq!(ext.page_count(), 1);
    /// # Ok::<(), udoc::Error>(())
    /// ```
    pub fn page_count(&self) -> usize {
        self.inner.page_count().min(self.config.limits.max_pages)
    }

    /// Document-level metadata.
    ///
    /// ```ignore
    /// let bytes = include_bytes!("../../../tests/corpus/minimal/hello.pdf");
    /// let ext = udoc::Extractor::from_bytes(bytes)?;
    /// let meta = ext.metadata();
    /// assert_eq!(meta.page_count, 1);
    /// # Ok::<(), udoc::Error>(())
    /// ```
    pub fn metadata(&self) -> DocumentMetadata {
        self.inner.metadata()
    }

    /// `true` iff the source document declared encryption.
    ///
    /// Returns `true` for PDFs whose trailer carries `/Encrypt`, even
    /// when extraction succeeded because the caller supplied a correct
    /// password. Returns `false` for formats with no encryption support
    /// (DOCX, XLSX, PPTX, ODF, RTF, Markdown) or for unencrypted PDFs.
    ///
    /// Used by `udoc inspect` and the Python `Document.is_encrypted`
    /// property to give downstream code a typed signal without
    /// substring-matching error messages. ( verify-report.md gap #7;
    ///.)
    ///
    /// ```ignore
    /// let bytes = include_bytes!("../../../tests/corpus/minimal/hello.pdf");
    /// let ext = udoc::Extractor::from_bytes(bytes)?;
    /// assert!(!ext.is_encrypted());
    /// # Ok::<(), udoc::Error>(())
    /// ```
    pub fn is_encrypted(&self) -> bool {
        self.inner.is_encrypted()
    }

    /// Return an error if the page index exceeds `max_pages`.
    fn check_max_pages(&self, index: usize) -> Result<()> {
        let limit = self.config.limits.max_pages;
        if index >= limit {
            return Err(Error::new(format!(
                "page index {index} exceeds max_pages limit ({limit})"
            )));
        }
        Ok(())
    }

    /// Extract full text from a single page.
    ///
    /// Returns an error if `index` is at or beyond `config.limits.max_pages`.
    pub fn page_text(&mut self, index: usize) -> Result<String> {
        self.check_max_pages(index)?;
        self.inner.page_text(index)
    }

    /// Extract text lines from a single page.
    ///
    /// Returns an error if `index` is at or beyond `config.limits.max_pages`.
    pub fn page_lines(&mut self, index: usize) -> Result<Vec<crate::page::TextLine>> {
        self.check_max_pages(index)?;
        self.inner.page_lines(index)
    }

    /// Extract raw text spans from a single page (no reading order).
    ///
    /// Returns an error if `index` is at or beyond `config.limits.max_pages`.
    pub fn page_spans(&mut self, index: usize) -> Result<Vec<crate::page::TextSpan>> {
        self.check_max_pages(index)?;
        self.inner.page_spans(index)
    }

    /// Extract tables from a single page.
    ///
    /// Returns an error if `index` is at or beyond `config.limits.max_pages`.
    pub fn page_tables(&mut self, index: usize) -> Result<Vec<crate::page::PageTable>> {
        self.check_max_pages(index)?;
        self.inner.page_tables(index)
    }

    /// Extract images from a single page.
    ///
    /// Returns an error if `index` is at or beyond `config.limits.max_pages`.
    pub fn page_images(&mut self, index: usize) -> Result<Vec<crate::page::PageImage>> {
        self.check_max_pages(index)?;
        self.inner.page_images(index)
    }

    /// Extract full text from all pages, concatenated with newlines.
    ///
    /// When [`Limits::memory_budget`](udoc_core::limits::Limits::memory_budget)
    /// is set and the process RSS exceeds the budget after the loop,
    /// document-scoped caches are released before the call returns so
    /// downstream operations (or the next Extractor) start cleaner.
    /// Peak memory during the loop is not affected by the budget.
    pub fn text(&mut self) -> Result<String> {
        let count = self.page_count();
        let mut parts = Vec::with_capacity(count);
        for i in 0..count {
            if let Some(ref range) = self.config.page_range {
                if !range.contains(i) {
                    continue;
                }
            }
            parts.push(self.page_text(i)?);
        }
        self.maybe_auto_reset();
        Ok(parts.join("\n"))
    }

    /// Auto-reset document caches if `Limits::memory_budget` is exceeded.
    ///
    /// Cheap when the budget is unset (the common case) -- a single
    /// `Option::is_some` check. When set, reads `/proc/self/status` once
    /// (a few microseconds on Linux) and fires `reset_document_caches`
    /// if over budget. Called at the end of batch-oriented methods.
    fn maybe_auto_reset(&mut self) {
        if let Some(budget) = self.config.limits.memory_budget {
            let (rss_kb, _) = read_rss_kb();
            if let Some(kb) = rss_kb {
                let rss_bytes = (kb as usize).saturating_mul(1024);
                if rss_bytes > budget {
                    self.inner.reset_document_caches();
                }
            }
        }
    }

    /// Diagnostics collected on the internal sink so far.
    ///
    /// Returns warnings populated by the facade-installed
    /// [`udoc_core::diagnostics::CollectingDiagnostics`] sink when the
    /// Config has `collect_diagnostics = true`. Empty when the user
    /// passed a custom sink without opting into the Tee mode (state 2)
    /// or explicitly opted out (state 3).
    ///
    /// Note: the underlying `CollectingDiagnostics::take_warnings`
    /// drains, so this method clones a snapshot via `warnings()`
    /// instead, leaving the collector intact for a subsequent
    /// `into_document` drain.
    pub fn diagnostics(&self) -> Vec<udoc_core::diagnostics::Warning> {
        match &self.internal_collector {
            Some(c) => c.warnings(),
            None => Vec::new(),
        }
    }

    /// Materialize the full Document model from this extractor.
    ///
    /// Drains the internal diagnostics collector (when present) into
    /// `Document::diagnostics()` and propagates the
    /// backend-level encryption flag to `Document::is_encrypted()`
    /// (W0-IS-ENCRYPTED).
    pub fn into_document(self) -> Result<Document> {
        // Capture before into_document consumes the backend.
        let is_encrypted = self.inner.is_encrypted();
        let mut doc = self.inner.into_document(&self.config)?;
        crate::convert::normalize_document_text(&mut doc);
        let cap = self.config.limits.max_warnings;
        let ws = drain_internal_collector(&self.internal_collector, cap);
        doc.set_diagnostics(ws);
        doc.set_is_encrypted(is_encrypted);
        Ok(doc)
    }

    /// Materialize the full Document model and return the detached AssetStore.
    ///
    /// The returned Document still has a (now-empty) `assets` field. The
    /// caller receives both pieces and can serialize them independently or
    /// recombine as needed.
    pub fn into_document_with_assets(self) -> Result<(Document, udoc_core::document::AssetStore)> {
        // Capture before into_document consumes the backend.
        let is_encrypted = self.inner.is_encrypted();
        let mut doc = self.inner.into_document(&self.config)?;
        crate::convert::normalize_document_text(&mut doc);
        let assets = std::mem::take(&mut doc.assets);
        let cap = self.config.limits.max_warnings;
        let ws = drain_internal_collector(&self.internal_collector, cap);
        doc.set_diagnostics(ws);
        doc.set_is_encrypted(is_encrypted);
        Ok((doc, assets))
    }

    /// The detected format of this document.
    pub fn format(&self) -> Format {
        self.inner.format()
    }

    /// Release document-scoped caches between documents.
    ///
    /// Pinned resources (Tier 1 fonts, compiled regexes, etc.) are retained;
    /// per-document object/stream/hint caches are dropped. Intended for
    /// long-running batch workers that extract from thousands of documents
    /// sequentially and want to cap process RSS between them. Peak memory
    /// within a single document is not affected.
    ///
    /// Currently a meaningful reset for the PDF backend only; other backends
    /// hold little document-scoped state and treat this as a no-op. The call
    /// is cheap and idempotent either way. See
    /// [`Limits::memory_budget`](udoc_core::limits::Limits::memory_budget) for
    /// an auto-reset variant that fires when process RSS exceeds a threshold.
    ///
    /// ```ignore
    /// // Demonstrate the batch loop on the bundled fixture so this runs
    /// // end to end. The reset call is cheap and idempotent on every
    /// // backend, so it's safe to call between documents in a loop.
    /// use udoc::Extractor;
    /// let bytes = include_bytes!("../../../tests/corpus/minimal/hello.pdf");
    /// let mut ext = Extractor::from_bytes(bytes)?;
    /// let _ = ext.text()?;
    /// ext.reset_document_caches();
    /// # Ok::<(), udoc::Error>(())
    /// ```
    pub fn reset_document_caches(&mut self) {
        self.inner.reset_document_caches();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::document::{Document, DocumentMetadata};
    use udoc_core::error::Error;
    use udoc_core::image::PageImage;
    use udoc_core::table::Table;
    use udoc_core::text::{TextLine, TextSpan};

    struct MockInternalBackend {
        pages: Vec<String>,
        fmt: Format,
    }

    impl InternalBackend for MockInternalBackend {
        fn page_count(&self) -> usize {
            self.pages.len()
        }

        fn page_text(&mut self, index: usize) -> Result<String> {
            self.pages
                .get(index)
                .cloned()
                .ok_or_else(|| Error::new(format!("page {index} out of range")))
        }

        fn page_lines(&mut self, index: usize) -> Result<Vec<TextLine>> {
            if index >= self.pages.len() {
                return Err(Error::new(format!("page {index} out of range")));
            }
            Ok(vec![])
        }

        fn page_spans(&mut self, index: usize) -> Result<Vec<TextSpan>> {
            if index >= self.pages.len() {
                return Err(Error::new(format!("page {index} out of range")));
            }
            Ok(vec![])
        }

        fn page_tables(&mut self, index: usize) -> Result<Vec<Table>> {
            if index >= self.pages.len() {
                return Err(Error::new(format!("page {index} out of range")));
            }
            Ok(vec![])
        }

        fn page_images(&mut self, index: usize) -> Result<Vec<PageImage>> {
            if index >= self.pages.len() {
                return Err(Error::new(format!("page {index} out of range")));
            }
            Ok(vec![])
        }

        fn metadata(&self) -> DocumentMetadata {
            DocumentMetadata::with_page_count(self.pages.len())
        }

        fn format(&self) -> Format {
            self.fmt
        }

        fn into_document(self: Box<Self>, _config: &Config) -> Result<Document> {
            let mut doc = Document::new();
            doc.metadata = DocumentMetadata::with_page_count(self.pages.len());
            Ok(doc)
        }
    }

    fn mock_extractor(pages: Vec<&str>, fmt: Format) -> Extractor {
        mock_extractor_with_config(pages, fmt, Config::default())
    }

    fn mock_extractor_with_config(pages: Vec<&str>, fmt: Format, config: Config) -> Extractor {
        let backend = MockInternalBackend {
            pages: pages.into_iter().map(String::from).collect(),
            fmt,
        };
        Extractor {
            inner: Box::new(backend),
            config,
            internal_collector: None,
        }
    }

    #[test]
    fn mock_backend_page_count() {
        let ext = mock_extractor(vec!["Page one", "Page two"], Format::Pdf);
        assert_eq!(ext.page_count(), 2);
    }

    #[test]
    fn mock_backend_page_text() {
        let mut ext = mock_extractor(vec!["Hello world"], Format::Pdf);
        assert_eq!(ext.page_text(0).unwrap(), "Hello world");
    }

    #[test]
    fn mock_backend_metadata() {
        let ext = mock_extractor(vec!["a", "b", "c"], Format::Pdf);
        assert_eq!(ext.metadata().page_count, 3);
    }

    #[test]
    fn mock_backend_format() {
        let ext = mock_extractor(vec![], Format::Md);
        assert_eq!(ext.format(), Format::Md);
    }

    #[test]
    fn mock_backend_into_document() {
        let ext = mock_extractor(vec!["page"], Format::Pdf);
        let doc = ext.into_document().unwrap();
        assert_eq!(doc.metadata.page_count, 1);
    }

    #[test]
    fn mock_backend_out_of_range() {
        let mut ext = mock_extractor(vec![], Format::Pdf);
        assert!(ext.page_text(0).is_err());
    }

    #[test]
    fn extractor_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Extractor>();
    }

    #[test]
    fn max_pages_clamps_page_count() {
        let mut config = Config::default();
        config.limits.max_pages = 2;
        let ext = mock_extractor_with_config(
            vec!["Page 1", "Page 2", "Page 3", "Page 4", "Page 5"],
            Format::Pdf,
            config,
        );
        assert_eq!(ext.page_count(), 2);
    }

    #[test]
    fn max_pages_allows_pages_within_limit() {
        let mut config = Config::default();
        config.limits.max_pages = 2;
        let mut ext = mock_extractor_with_config(
            vec!["Page 1", "Page 2", "Page 3", "Page 4", "Page 5"],
            Format::Pdf,
            config,
        );
        assert_eq!(ext.page_text(0).unwrap(), "Page 1");
        assert_eq!(ext.page_text(1).unwrap(), "Page 2");
    }

    #[test]
    fn max_pages_rejects_pages_beyond_limit() {
        let mut config = Config::default();
        config.limits.max_pages = 2;
        let mut ext = mock_extractor_with_config(
            vec!["Page 1", "Page 2", "Page 3", "Page 4", "Page 5"],
            Format::Pdf,
            config,
        );
        let err = ext.page_text(2).unwrap_err();
        assert!(
            err.to_string().contains("max_pages"),
            "error should mention max_pages, got: {err}"
        );
    }

    #[test]
    fn max_pages_caps_text_concatenation() {
        let mut config = Config::default();
        config.limits.max_pages = 2;
        let mut ext =
            mock_extractor_with_config(vec!["AAA", "BBB", "CCC", "DDD"], Format::Pdf, config);
        let text = ext.text().unwrap();
        assert_eq!(text, "AAA\nBBB");
    }

    #[test]
    fn max_pages_rejects_lines_beyond_limit() {
        let mut config = Config::default();
        config.limits.max_pages = 1;
        let mut ext = mock_extractor_with_config(vec!["Page 1", "Page 2"], Format::Pdf, config);
        assert!(ext.page_lines(0).is_ok());
        assert!(ext.page_lines(1).is_err());
    }

    #[test]
    fn max_pages_rejects_tables_beyond_limit() {
        let mut config = Config::default();
        config.limits.max_pages = 1;
        let mut ext = mock_extractor_with_config(vec!["Page 1", "Page 2"], Format::Pdf, config);
        assert!(ext.page_tables(0).is_ok());
        assert!(ext.page_tables(1).is_err());
    }

    #[test]
    fn max_pages_rejects_images_beyond_limit() {
        let mut config = Config::default();
        config.limits.max_pages = 1;
        let mut ext = mock_extractor_with_config(vec!["Page 1", "Page 2"], Format::Pdf, config);
        assert!(ext.page_images(0).is_ok());
        assert!(ext.page_images(1).is_err());
    }

    #[test]
    fn max_pages_rejects_spans_beyond_limit() {
        let mut config = Config::default();
        config.limits.max_pages = 1;
        let mut ext = mock_extractor_with_config(vec!["Page 1", "Page 2"], Format::Pdf, config);
        assert!(ext.page_spans(0).is_ok());
        assert!(ext.page_spans(1).is_err());
    }

    // T60-MEMBATCH: reset_document_caches exists and is callable on the mock.
    // Mock's InternalBackend default impl is a no-op, so this test just
    // exercises the dispatch path. End-to-end PDF test is below in
    // integration-style tests.
    #[test]
    fn reset_document_caches_callable() {
        let mut ext = mock_extractor(vec!["Page 1"], Format::Pdf);
        ext.reset_document_caches();
        // Idempotent: second call should also succeed.
        ext.reset_document_caches();
        // Extractor still usable after reset.
        assert_eq!(ext.page_text(0).unwrap(), "Page 1");
    }

    #[test]
    fn memory_budget_default_is_none() {
        let cfg = Config::default();
        assert!(cfg.limits.memory_budget.is_none());
    }

    #[test]
    fn memory_budget_builder_roundtrip() {
        use udoc_core::limits::Limits;
        let cfg =
            Config::new().limits(Limits::builder().memory_budget(Some(2_000_000_000)).build());
        assert_eq!(cfg.limits.memory_budget, Some(2_000_000_000));
        let cfg = cfg.limits(Limits::builder().memory_budget(None).build());
        assert!(cfg.limits.memory_budget.is_none());
    }

    #[test]
    fn text_with_memory_budget_does_not_panic() {
        // Budget set to 1 byte so it auto-triggers on any non-empty process,
        // but the mock backend's reset is a no-op so this only exercises
        // the dispatch path. Real memory effects are tested end-to-end.
        use udoc_core::limits::Limits;
        let cfg = Config::new().limits(Limits::builder().memory_budget(Some(1)).build());
        let mut ext = mock_extractor_with_config(vec!["hi", "there"], Format::Pdf, cfg);
        assert_eq!(ext.text().unwrap(), "hi\nthere");
    }
}

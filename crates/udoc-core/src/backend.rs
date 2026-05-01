//! Format backend trait for document extraction.
//!
//! The [`FormatBackend`] trait defines the contract between format-specific
//! backends (PDF, DOCX, XLSX, etc.) and the unified udoc API. Each backend
//! implements this trait to expose document content as format-agnostic types.
//!
//! # Design principles
//!
//! - **Page-oriented.** Even non-paginated formats (DOCX) expose content
//!   as logical pages or sections.
//! - **No I/O.** Backends operate on already-loaded data. The facade crate
//!   handles file I/O and format detection.
//! - **Core types only.** Methods return types from udoc-core, not
//!   format-specific types.
//! - **Mutable page access.** Pages take `&mut self` to support lazy
//!   computation (e.g., PDF defers text extraction until requested).

use crate::error::Result;
use crate::geometry::BoundingBox;
use crate::image::PageImage;
use crate::table::Table;
use crate::text::{TextLine, TextSpan};

/// Controls which document layers a backend extracts.
///
/// Marked `#[non_exhaustive]` so the struct can grow new feature flags
/// (e.g., "annotations", "hooks") across post-alpha releases without
/// breaking downstream code. Build with [`LayerConfig::default`] and
/// assign fields, or use the [`LayerConfig::content_only`] preset.
///
/// Lives in `udoc-core` (vs the facade) so the [`PageExtractor::bundle`]
/// trait method can name it as a parameter type without the core crate
/// depending on the facade. Re-exported from the facade as
/// `udoc::LayerConfig` for downstream callers.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct LayerConfig {
    /// Extract the presentation layer (geometry, fonts, colors).
    pub presentation: bool,
    /// Extract the relationships layer (footnotes, bookmarks).
    pub relationships: bool,
    /// Extract the interactions layer (forms, comments, tracked changes).
    pub interactions: bool,
    /// Detect and extract tables (PDF: ruled-line + text-alignment
    /// detection). Disabling this skips table detection, which is
    /// ~16% of PDF extraction time.
    pub tables: bool,
    /// Extract images from documents. Disabling this skips image
    /// decompression, which is ~19% of PDF extraction time.
    pub images: bool,
}

impl Default for LayerConfig {
    fn default() -> Self {
        Self {
            presentation: true,
            relationships: true,
            interactions: true,
            tables: true,
            images: true,
        }
    }
}

impl LayerConfig {
    /// Build a `LayerConfig` with only the content spine (Block/Inline
    /// tree) enabled -- presentation, relationships, and interactions
    /// overlays are all suppressed. Tables and images stay enabled
    /// (they are content-spine concerns, not overlays).
    ///
    /// Replaces the old `Config::content_only()` shortcut method.
    pub fn content_only() -> Self {
        Self {
            presentation: false,
            relationships: false,
            interactions: false,
            tables: true,
            images: true,
        }
    }
}

/// All layers extracted from a single page in one trait call.
///
/// Returned by [`PageExtractor::bundle`].
/// Contains text lines, tables, and images. NO `text` field on the
/// struct: the [`PageBundle::text`] method derives a string from
/// `lines` on demand. NO `spans` field initially -- defer until a
/// backend produces them cheaply AND a real caller asks.
///
/// The default trait impl composes `text_lines() + tables() + images()`
/// in three calls. Backends that compute all three in a single
/// content-stream pass (e.g. PDF) override [`PageExtractor::bundle`]
/// for efficiency.
#[derive(Debug, Default, Clone)]
pub struct PageBundle {
    /// Text lines in reading order.
    pub lines: Vec<TextLine>,
    /// Tables on this page.
    pub tables: Vec<Table>,
    /// Images on this page.
    pub images: Vec<PageImage>,
}

impl PageBundle {
    /// Render the bundle's text lines as one newline-joined string.
    /// Cheap derivation -- no allocation hot loop, just one `Vec<String>`
    /// of line strings followed by a join.
    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Trait implemented by each format backend.
///
/// Uses GATs (Generic Associated Types) for the page type to support
/// backends where pages borrow from the document (e.g., PDF's `Page<'a>`).
pub trait FormatBackend {
    /// The page type for this backend.
    type Page<'a>: PageExtractor
    where
        Self: 'a;

    /// Number of pages (or logical sections) in the document.
    fn page_count(&self) -> usize;

    /// Access a page by zero-based index.
    fn page(&mut self, index: usize) -> Result<Self::Page<'_>>;

    /// Document-level metadata.
    fn metadata(&self) -> DocumentMetadata;

    /// `true` iff the source document declared encryption.
    ///
    /// Default `false` for formats with no encryption support (DOCX,
    /// XLSX, PPTX, ODF, RTF, Markdown, ...). The PDF backend overrides
    /// this to return `true` when `/Encrypt` is in the trailer
    /// (regardless of whether decryption succeeded with the supplied
    /// password). DOC / XLS / PPT may grow encryption support later
    /// and override this then.
    ///
    /// Used by the facade to populate `udoc_core::document::Document::is_encrypted`
    /// at the conversion boundary, and by CLI / Python callers
    /// (`udoc inspect`, `Document.is_encrypted` Python property) to
    /// surface the typed signal without substring-matching error
    /// messages.
    fn is_encrypted(&self) -> bool {
        false
    }
}

/// Trait for extracting content from a single page.
///
/// Mirrors the tiered API: `text()` for full reconstruction,
/// `text_lines()` for positioned lines, `raw_spans()` for raw output.
pub trait PageExtractor {
    /// Full text with reading order reconstruction.
    fn text(&mut self) -> Result<String>;

    /// Text as positioned lines.
    fn text_lines(&mut self) -> Result<Vec<TextLine>>;

    /// Raw text spans in document order (no reordering).
    fn raw_spans(&mut self) -> Result<Vec<TextSpan>>;

    /// Tables on this page.
    fn tables(&mut self) -> Result<Vec<Table>>;

    /// Images on this page.
    fn images(&mut self) -> Result<Vec<PageImage>>;

    /// Bounding box of the page in page-native coordinates.
    ///
    /// For PDF: the crop box or media box (y-up, points). For other formats
    /// that have no page geometry, the default returns `None`.
    fn page_bbox(&mut self) -> Option<BoundingBox> {
        None
    }

    /// Page rotation in degrees (0, 90, 180, or 270).
    ///
    /// For PDF: the /Rotate entry from the page dictionary. For all other
    /// formats the default returns 0.
    fn rotation(&mut self) -> u16 {
        0
    }

    /// Extract the requested layers from this page in one trait call.
    ///
    /// Default impl composes [`PageExtractor::text_lines`] (always),
    /// [`PageExtractor::tables`] (gated on `layers.tables`), and
    /// [`PageExtractor::images`] (gated on `layers.images`). Text
    /// lines are the content spine and are always produced; the
    /// presentation/relationships/interactions overlay flags on
    /// `LayerConfig` are upper-layer concerns the extractor strips
    /// post hoc and have no effect on per-page bundle composition.
    ///
    /// Backends with a single-pass content extraction (e.g. PDF, where
    /// text + tables + images all share the content stream walk)
    /// override this for efficiency.
    /// resolves the F1 friction of callers composing 4 separate trait
    /// calls.
    fn bundle(&mut self, layers: &LayerConfig) -> Result<PageBundle> {
        let lines = self.text_lines()?;
        let tables = if layers.tables {
            self.tables()?
        } else {
            Vec::new()
        };
        let images = if layers.images {
            self.images()?
        } else {
            Vec::new()
        };
        Ok(PageBundle {
            lines,
            tables,
            images,
        })
    }
}

/// Validate a page index for single-page backends (DOCX, RTF, Markdown, etc.).
///
/// Returns `Ok(())` if `index == 0`, or an error with a message including the
/// format's unit name (e.g., "DOCX has 1 logical page").
pub fn validate_single_page(index: usize, unit: &str) -> Result<()> {
    if index != 0 {
        return Err(crate::error::Error::new(format!(
            "page {index} out of range ({unit} has 1 logical page)"
        )));
    }
    Ok(())
}

/// Generate the 3 boilerplate constructors that delegate to `from_bytes_with_diag`.
///
/// Backends must define `from_bytes_with_diag(&[u8], Arc<dyn DiagnosticsSink>) -> Result<Self>`
/// themselves. This macro generates `open`, `open_with_diag`, and `from_bytes`.
///
/// Usage inside an `impl MyDocument { ... }` block:
/// ```ignore
/// // ignore: this macro must be invoked inside an `impl Block` body that
/// // already defines `from_bytes_with_diag` and `Self`; not callable
/// // standalone in a doctest.
/// udoc_core::define_backend_constructors!(MAX_FILE_SIZE, "DOCX");
/// ```
#[macro_export]
macro_rules! define_backend_constructors {
    ($max_size:expr, $format_name:expr) => {
        /// Open a document from a file path.
        pub fn open(path: impl AsRef<std::path::Path>) -> $crate::error::Result<Self> {
            Self::open_with_diag(
                path,
                std::sync::Arc::new($crate::diagnostics::NullDiagnostics),
            )
        }

        /// Open a document from a file path with a diagnostics sink.
        pub fn open_with_diag(
            path: impl AsRef<std::path::Path>,
            diag: std::sync::Arc<dyn $crate::diagnostics::DiagnosticsSink>,
        ) -> $crate::error::Result<Self> {
            let data = $crate::io::read_file_checked(path.as_ref(), $max_size, $format_name)?;
            Self::from_bytes_with_diag(&data, diag)
        }

        /// Parse from in-memory bytes.
        pub fn from_bytes(data: &[u8]) -> $crate::error::Result<Self> {
            Self::from_bytes_with_diag(
                data,
                std::sync::Arc::new($crate::diagnostics::NullDiagnostics),
            )
        }
    };
}

/// Re-export from the document module. Single source of truth for metadata.
pub use crate::document::DocumentMetadata;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    // Mock backend to validate the trait compiles and works.
    struct MockBackend {
        pages: Vec<String>,
    }

    struct MockPage {
        content: String,
    }

    impl FormatBackend for MockBackend {
        type Page<'a> = MockPage;

        fn page_count(&self) -> usize {
            self.pages.len()
        }

        fn page(&mut self, index: usize) -> Result<MockPage> {
            self.pages
                .get(index)
                .map(|s| MockPage { content: s.clone() })
                .ok_or_else(|| Error::new(format!("page {index} out of range")))
        }

        fn metadata(&self) -> DocumentMetadata {
            let mut meta = DocumentMetadata::with_page_count(self.pages.len());
            meta.title = Some("Test Document".into());
            meta
        }
    }

    impl PageExtractor for MockPage {
        fn text(&mut self) -> Result<String> {
            Ok(self.content.clone())
        }

        fn text_lines(&mut self) -> Result<Vec<TextLine>> {
            Ok(vec![TextLine::new(
                vec![TextSpan::new(self.content.clone(), 0.0, 0.0, 100.0, 12.0)],
                0.0,
                false,
            )])
        }

        fn raw_spans(&mut self) -> Result<Vec<TextSpan>> {
            Ok(vec![TextSpan::new(
                self.content.clone(),
                0.0,
                0.0,
                100.0,
                12.0,
            )])
        }

        fn tables(&mut self) -> Result<Vec<Table>> {
            Ok(vec![])
        }

        fn images(&mut self) -> Result<Vec<PageImage>> {
            Ok(vec![])
        }
    }

    #[test]
    fn mock_backend_basic() {
        let mut backend = MockBackend {
            pages: vec!["Page one".into(), "Page two".into()],
        };
        assert_eq!(backend.page_count(), 2);
        assert_eq!(backend.metadata().title.as_deref(), Some("Test Document"));

        let mut page = backend.page(0).unwrap();
        assert_eq!(page.text().unwrap(), "Page one");
        assert_eq!(page.text_lines().unwrap().len(), 1);
        assert_eq!(page.raw_spans().unwrap().len(), 1);
        assert!(page.tables().unwrap().is_empty());
        assert!(page.images().unwrap().is_empty());
    }

    #[test]
    fn mock_backend_out_of_range() {
        let mut backend = MockBackend { pages: vec![] };
        assert!(backend.page(0).is_err());
    }

    #[test]
    fn metadata_default() {
        let meta = DocumentMetadata::default();
        assert!(meta.title.is_none());
        assert_eq!(meta.page_count, 0);
    }

    #[test]
    fn layer_config_default_all_on() {
        let lc = LayerConfig::default();
        assert!(lc.presentation);
        assert!(lc.relationships);
        assert!(lc.interactions);
        assert!(lc.tables);
        assert!(lc.images);
    }

    #[test]
    fn layer_config_content_only() {
        let lc = LayerConfig::content_only();
        assert!(!lc.presentation);
        assert!(!lc.relationships);
        assert!(!lc.interactions);
        assert!(lc.tables);
        assert!(lc.images);
    }

    #[test]
    fn page_bundle_text_derives_from_lines() {
        let bundle = PageBundle {
            lines: vec![
                TextLine::new(
                    vec![TextSpan::new("hello".into(), 0.0, 0.0, 50.0, 12.0)],
                    0.0,
                    false,
                ),
                TextLine::new(
                    vec![TextSpan::new("world".into(), 0.0, 14.0, 50.0, 12.0)],
                    14.0,
                    false,
                ),
            ],
            tables: vec![],
            images: vec![],
        };
        assert_eq!(bundle.text(), "hello\nworld");
    }

    #[test]
    fn page_bundle_text_handles_multi_span_lines() {
        let bundle = PageBundle {
            lines: vec![TextLine::new(
                vec![
                    TextSpan::new("foo".into(), 0.0, 0.0, 30.0, 12.0),
                    TextSpan::new(" bar".into(), 30.0, 0.0, 40.0, 12.0),
                ],
                0.0,
                false,
            )],
            tables: vec![],
            images: vec![],
        };
        assert_eq!(bundle.text(), "foo bar");
    }

    #[test]
    fn page_bundle_text_empty_when_no_lines() {
        let bundle = PageBundle::default();
        assert_eq!(bundle.text(), "");
    }

    #[test]
    fn bundle_default_impl_composes_lines_tables_images() {
        let mut backend = MockBackend {
            pages: vec!["Page A".into()],
        };
        let mut page = backend.page(0).unwrap();
        let bundle = page.bundle(&LayerConfig::default()).unwrap();
        assert_eq!(bundle.lines.len(), 1);
        assert_eq!(bundle.lines[0].spans[0].text, "Page A");
        // Mock backend has no tables/images.
        assert!(bundle.tables.is_empty());
        assert!(bundle.images.is_empty());
        assert_eq!(bundle.text(), "Page A");
    }

    #[test]
    fn bundle_skips_tables_when_layer_off() {
        let mut backend = MockBackend {
            pages: vec!["X".into()],
        };
        let mut page = backend.page(0).unwrap();
        let layers = LayerConfig {
            tables: false,
            ..LayerConfig::default()
        };
        let bundle = page.bundle(&layers).unwrap();
        assert!(bundle.tables.is_empty());
    }

    #[test]
    fn bundle_skips_images_when_layer_off() {
        let mut backend = MockBackend {
            pages: vec!["X".into()],
        };
        let mut page = backend.page(0).unwrap();
        let layers = LayerConfig {
            images: false,
            ..LayerConfig::default()
        };
        let bundle = page.bundle(&layers).unwrap();
        assert!(bundle.images.is_empty());
    }

    #[test]
    fn bundle_lines_always_extracted() {
        // Text lines are the content spine and ALWAYS extracted, even
        // when every overlay flag on LayerConfig is off.
        let mut backend = MockBackend {
            pages: vec!["spine".into()],
        };
        let mut page = backend.page(0).unwrap();
        let layers = LayerConfig::content_only();
        let bundle = page.bundle(&layers).unwrap();
        assert!(!bundle.lines.is_empty());
    }
}

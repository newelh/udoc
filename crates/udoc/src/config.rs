//! Configuration types: Config, LayerConfig, PageRange.

use std::fmt;
use std::sync::Arc;

use udoc_core::diagnostics::{DiagnosticsSink, NullDiagnostics};
use udoc_core::document::AssetConfig;
use udoc_core::error::{Error, Result};
use udoc_core::limits::Limits;

/// Re-export of [`udoc_core::backend::LayerConfig`].
///
/// Lives in `udoc-core` so the [`udoc_core::backend::PageExtractor::bundle`]
/// trait method can name it as a parameter type. Re-exported here so
/// downstream callers can use it as `udoc::LayerConfig`.
pub use udoc_core::backend::LayerConfig;

use crate::detect::Format;

/// Rendering profile: re-export of [`udoc_render::RenderingProfile`].
///
/// Controls trade-offs between viewer-grade pixel fidelity and OCR-grade
/// text legibility during page rendering. See
/// [`udoc_render::RenderingProfile`] for the full semantics. The default is
/// [`udoc_render::RenderingProfile::OcrFriendly`].
pub use udoc_render::RenderingProfile;

/// Top-level configuration for document extraction.
///
/// Use builder methods to customize. Defaults are sensible for most cases:
/// all layers enabled, no password, no page filtering.
///
/// Surface (10 builder methods): [`Config::new`], [`Config::format`],
/// [`Config::password`], [`Config::pages`], [`Config::diagnostics`],
/// [`Config::limits`], [`Config::assets`], [`Config::layers`],
/// [`Config::rendering_profile`], [`Config::collect_diagnostics`].
///
/// ```
/// use udoc::Config;
/// let config = Config::new()
///     .pages("1,3,5-10")
///     .expect("valid page range");
/// ```
#[non_exhaustive]
#[derive(Clone)]
pub struct Config {
    /// Force a specific format (bypass detection).
    pub format: Option<Format>,
    /// Document password for encrypted files.
    pub password: Option<String>,
    /// Diagnostics sink for warnings.
    pub diagnostics: Arc<dyn DiagnosticsSink>,
    /// Which layers to extract.
    pub layers: LayerConfig,
    /// Which asset types to extract (images, fonts, strict-font mode).
    pub assets: AssetConfig,
    /// Page range filter (None = all pages).
    pub page_range: Option<PageRange>,
    /// Safety limits for resource consumption (file size, page count,
    /// nesting depth, memory budget, warning cap, etc.).
    pub limits: Limits,

    /// Rendering profile: controls trade-offs between viewer-grade pixel
    /// fidelity and OCR-grade legibility during page rendering.
    ///
    /// Default is [`RenderingProfile::OcrFriendly`] -- this is the profile
    /// the 300 DPI OCR char-acc gate measures against, and it
    /// matches MuPDF LIGHT-mode aggregate SSIM better than the alternative.
    /// Callers who want FreeType NORMAL byte-exact per-glyph output (e.g.
    /// cursor-pair diagnostics) can opt into [`RenderingProfile::Visual`].
    ///
    /// Only observed by the renderer -- has no effect on non-rendering
    /// extraction paths. See [`RenderingProfile`] for the full trade-off.
    pub rendering_profile: RenderingProfile,

    /// When true, the facade installs an internal
    /// [`udoc_core::diagnostics::CollectingDiagnostics`] sink so
    /// `Document::diagnostics()` is populated for the four-state matrix
    /// (see [`Config::collect_diagnostics`] for the matrix).
    ///
    /// Default `true`. Setting a custom [`DiagnosticsSink`] via
    /// [`Config::diagnostics`] implicitly disables the auto-collect (the
    /// caller owns the stream); explicitly calling
    /// `.collect_diagnostics(true)` after a custom sink installs a Tee
    /// so both the caller's sink and `doc.diagnostics()` are populated.
    pub collect_diagnostics: bool,

    /// Tracks whether the user has explicitly called
    /// [`Config::diagnostics`]. Used by the facade to disambiguate state
    /// 1 (no custom sink) from state 4 (custom sink + opt-in tee) in
    /// the  four-state matrix. Not a public knob; the field is
    /// `pub(crate)` and not part of the documented API surface.
    #[doc(hidden)]
    pub(crate) custom_diagnostics_set: bool,
}

impl Config {
    /// Create a config with all defaults.
    ///
    /// ```
    /// use udoc::Config;
    /// let cfg = Config::new();
    /// assert!(cfg.format.is_none());
    /// assert!(cfg.password.is_none());
    /// ```
    pub fn new() -> Self {
        Self {
            format: None,
            password: None,
            diagnostics: Arc::new(NullDiagnostics),
            layers: LayerConfig::default(),
            assets: AssetConfig::default(),
            page_range: None,
            limits: Limits::default(),
            rendering_profile: RenderingProfile::default(),
            collect_diagnostics: true,
            custom_diagnostics_set: false,
        }
    }

    /// Force a specific format (bypass auto-detection).
    ///
    /// ```
    /// use udoc::{Config, Format};
    /// let cfg = Config::new().format(Format::Pdf);
    /// assert_eq!(cfg.format, Some(Format::Pdf));
    /// ```
    pub fn format(mut self, format: Format) -> Self {
        self.format = Some(format);
        self
    }

    /// Set the document password.
    ///
    /// ```
    /// use udoc::Config;
    /// let cfg = Config::new().password("hunter2");
    /// assert_eq!(cfg.password.as_deref(), Some("hunter2"));
    /// ```
    pub fn password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    /// Set the diagnostics sink.
    ///
    /// Set the diagnostics sink.
    ///
    /// this also implicitly sets
    /// [`Config::collect_diagnostics`] to `false`: the caller has
    /// declared they own the warning stream, so the facade does not
    /// double-collect into `Document::diagnostics()`. Reverse the
    /// implicit opt-out by calling `.collect_diagnostics(true)`
    /// AFTER `.diagnostics(...)` -- that installs a Tee so both the
    /// custom sink AND `doc.diagnostics()` are populated.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use udoc::{CollectingDiagnostics, Config};
    /// let sink = Arc::new(CollectingDiagnostics::new());
    /// let cfg = Config::new().diagnostics(sink.clone());
    /// assert_eq!(sink.warnings().len(), 0);
    /// // Implicit opt-out:
    /// assert!(!cfg.collect_diagnostics);
    /// // Force tee: both sink and doc.diagnostics() get populated.
    /// let cfg = cfg.collect_diagnostics(true);
    /// assert!(cfg.collect_diagnostics);
    /// drop(sink);
    /// ```
    pub fn diagnostics(mut self, sink: Arc<dyn DiagnosticsSink>) -> Self {
        self.diagnostics = sink;
        self.collect_diagnostics = false;
        self.custom_diagnostics_set = true;
        self
    }

    /// Set safety limits for resource consumption.
    ///
    /// ```
    /// use udoc::Config;
    /// use udoc_core::limits::{Limits, mb};
    ///
    /// let config = Config::new()
    ///     .limits(Limits::builder().max_file_size(mb(128)).build());
    /// ```
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Set page range filter. Input is a human-readable string like
    /// "1,3,5-10" (1-based page numbers).
    ///
    /// ```
    /// use udoc::Config;
    /// let cfg = Config::new().pages("1,3,5-10")?;
    /// assert!(cfg.page_range.is_some());
    /// # Ok::<(), udoc::Error>(())
    /// ```
    pub fn pages(mut self, spec: &str) -> Result<Self> {
        self.page_range = Some(PageRange::parse(spec)?);
        Ok(self)
    }

    /// Set asset extraction configuration.
    ///
    /// ```
    /// use udoc::{AssetConfig, Config};
    /// let cfg = Config::new().assets(AssetConfig::all());
    /// assert!(cfg.assets.fonts);
    /// assert!(cfg.assets.images);
    /// ```
    pub fn assets(mut self, config: AssetConfig) -> Self {
        self.assets = config;
        self
    }

    /// Set which document layers to extract.
    ///
    /// ```
    /// use udoc::{Config, LayerConfig};
    /// let cfg = Config::new().layers(LayerConfig::content_only());
    /// assert!(!cfg.layers.presentation);
    /// assert!(!cfg.layers.relationships);
    /// assert!(!cfg.layers.interactions);
    /// ```
    pub fn layers(mut self, layers: LayerConfig) -> Self {
        self.layers = layers;
        self
    }

    /// Set the [`RenderingProfile`] used by the page renderer.
    ///
    /// See the [`Config::rendering_profile`] field for the trade-off. The
    /// default is [`RenderingProfile::OcrFriendly`].
    ///
    /// ```
    /// use udoc::Config;
    /// use udoc::config::RenderingProfile;
    /// let cfg = Config::new().rendering_profile(RenderingProfile::Visual);
    /// assert_eq!(cfg.rendering_profile, RenderingProfile::Visual);
    /// ```
    pub fn rendering_profile(mut self, profile: RenderingProfile) -> Self {
        self.rendering_profile = profile;
        self
    }

    /// Control whether the facade installs an internal collecting sink
    /// for `Document::diagnostics()`.
    ///
    /// Default is `true`. The four-state matrix (T1a-DIAG-DEFAULT):
    ///
    /// 1. `Config::new()` (no custom sink) -> internal collector;
    ///    `doc.diagnostics()` is populated.
    /// 2. `Config::new().diagnostics(my_sink)` -> custom sink only; the
    ///    auto-collect is implicitly disabled (caller owns the stream).
    /// 3. `Config::new().diagnostics(NullDiagnostics)` -> explicit
    ///    opt-out; `doc.diagnostics()` is empty.
    /// 4. `Config::new().diagnostics(my_sink).collect_diagnostics(true)`
    ///    -> Tee: both `my_sink` and `doc.diagnostics()` populated.
    ///
    /// ```
    /// use udoc::Config;
    /// let cfg = Config::new();
    /// assert!(cfg.collect_diagnostics);
    /// let cfg = cfg.collect_diagnostics(false);
    /// assert!(!cfg.collect_diagnostics);
    /// ```
    pub fn collect_diagnostics(mut self, on: bool) -> Self {
        self.collect_diagnostics = on;
        self
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

// Custom Debug: mask password value.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("format", &self.format)
            .field(
                "password",
                if self.password.is_some() {
                    &"***"
                } else {
                    &"None"
                },
            )
            .field("layers", &self.layers)
            .field("assets", &self.assets)
            .field("page_range", &self.page_range)
            .field("limits", &self.limits)
            .field("rendering_profile", &self.rendering_profile)
            .field("collect_diagnostics", &self.collect_diagnostics)
            .finish()
    }
}

/// A set of page indices (0-based) parsed from a human-readable spec.
///
/// Parses specs like "1,3,5-10" where numbers are 1-based page numbers.
/// Internally stores as sorted 0-based indices.
#[derive(Debug, Clone)]
pub struct PageRange {
    indices: Vec<usize>,
}

impl PageRange {
    /// Parse a page range specification.
    ///
    /// Format: comma-separated items, where each item is either a single
    /// page number or a range "start-end" (inclusive, 1-based).
    ///
    /// Examples: "1", "1,3,5", "1-10", "1,3,5-10,15"
    ///
    /// Errors on: non-numeric input, zero page number, reversed range.
    pub fn parse(spec: &str) -> Result<Self> {
        if spec.trim().is_empty() {
            return Err(Error::new("page range cannot be empty"));
        }

        let mut indices = Vec::new();

        for part in spec.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }

            if let Some((start_str, end_str)) = part.split_once('-') {
                let start_trimmed = start_str.trim();
                let end_trimmed = end_str.trim();
                if start_trimmed.is_empty() || end_trimmed.is_empty() {
                    return Err(Error::new(format!(
                        "invalid page range: '{part}' (both start and end required)"
                    )));
                }
                let start: usize = start_trimmed
                    .parse()
                    .map_err(|_| Error::new(format!("invalid page number: '{start_trimmed}'")))?;
                let end: usize = end_trimmed
                    .parse()
                    .map_err(|_| Error::new(format!("invalid page number: '{end_trimmed}'")))?;

                if start == 0 {
                    return Err(Error::new("page numbers are 1-based, got 0"));
                }
                if end == 0 {
                    return Err(Error::new("page numbers are 1-based, got 0"));
                }
                if start > end {
                    return Err(Error::new(format!("reversed page range: {start}-{end}")));
                }

                // Cap range size to prevent OOM from absurdly large ranges.
                const MAX_RANGE_SIZE: usize = 1_000_000;
                if end - start + 1 > MAX_RANGE_SIZE {
                    return Err(Error::new(format!(
                        "page range {start}-{end} has {} pages, max is {MAX_RANGE_SIZE}",
                        end - start + 1
                    )));
                }

                for page in start..=end {
                    indices.push(page - 1); // Convert to 0-based
                }
            } else {
                let page: usize = part
                    .parse()
                    .map_err(|_| Error::new(format!("invalid page number: '{part}'")))?;
                if page == 0 {
                    return Err(Error::new("page numbers are 1-based, got 0"));
                }
                indices.push(page - 1); // Convert to 0-based
            }
        }

        indices.sort_unstable();
        indices.dedup();

        Ok(Self { indices })
    }

    /// Whether the given 0-based page index is in this range.
    pub fn contains(&self, index: usize) -> bool {
        self.indices.binary_search(&index).is_ok()
    }

    /// Iterate over all 0-based page indices in order.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.indices.iter().copied()
    }

    /// Number of pages in this range.
    pub fn len(&self) -> usize {
        self.indices.len()
    }

    /// Whether this range is empty.
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults() {
        let config = Config::new();
        assert!(config.format.is_none());
        assert!(config.password.is_none());
        assert!(config.page_range.is_none());
        assert!(config.layers.presentation);
        assert!(config.layers.relationships);
        assert!(config.layers.interactions);
    }

    #[test]
    fn config_builder() {
        let config = Config::new().format(Format::Pdf).password("secret");
        assert_eq!(config.format, Some(Format::Pdf));
        assert_eq!(config.password.as_deref(), Some("secret"));
    }

    #[test]
    fn config_layers_content_only() {
        let config = Config::new().layers(LayerConfig::content_only());
        assert!(!config.layers.presentation);
        assert!(!config.layers.relationships);
        assert!(!config.layers.interactions);
        assert!(config.layers.tables);
        assert!(config.layers.images);
    }

    #[test]
    fn config_pages() {
        let config = Config::new().pages("1,3,5-10").unwrap();
        let range = config.page_range.unwrap();
        assert!(range.contains(0)); // page 1
        assert!(!range.contains(1)); // page 2
        assert!(range.contains(2)); // page 3
        assert!(range.contains(4)); // page 5
        assert!(range.contains(9)); // page 10
        assert!(!range.contains(10)); // page 11
    }

    #[test]
    fn config_debug_masks_password() {
        let config = Config::new().password("hunter2");
        let debug = format!("{:?}", config);
        assert!(!debug.contains("hunter2"));
        assert!(debug.contains("***"));
    }

    #[test]
    fn config_debug_no_password() {
        let config = Config::new();
        let debug = format!("{:?}", config);
        assert!(debug.contains("None"));
    }

    #[test]
    fn page_range_single() {
        let r = PageRange::parse("5").unwrap();
        assert_eq!(r.len(), 1);
        assert!(r.contains(4)); // 0-based
        assert!(!r.contains(5));
    }

    #[test]
    fn page_range_list() {
        let r = PageRange::parse("1,3,5").unwrap();
        assert_eq!(r.len(), 3);
        assert!(r.contains(0));
        assert!(!r.contains(1));
        assert!(r.contains(2));
        assert!(r.contains(4));
    }

    #[test]
    fn page_range_range() {
        let r = PageRange::parse("3-7").unwrap();
        assert_eq!(r.len(), 5);
        assert!(!r.contains(1)); // page 2
        assert!(r.contains(2)); // page 3
        assert!(r.contains(6)); // page 7
        assert!(!r.contains(7)); // page 8
    }

    #[test]
    fn page_range_mixed() {
        let r = PageRange::parse("1,5-8,12").unwrap();
        assert_eq!(r.len(), 6);
        let pages: Vec<usize> = r.iter().collect();
        assert_eq!(pages, vec![0, 4, 5, 6, 7, 11]);
    }

    #[test]
    fn page_range_dedup() {
        let r = PageRange::parse("1,1,2,2-3").unwrap();
        assert_eq!(r.len(), 3); // 0, 1, 2 (deduped)
    }

    #[test]
    fn page_range_whitespace() {
        let r = PageRange::parse(" 1 , 3 , 5 - 7 ").unwrap();
        assert_eq!(r.len(), 5);
    }

    #[test]
    fn page_range_zero_error() {
        assert!(PageRange::parse("0").is_err());
        assert!(PageRange::parse("0-5").is_err());
        assert!(PageRange::parse("1-0").is_err());
    }

    #[test]
    fn page_range_reversed_error() {
        assert!(PageRange::parse("5-3").is_err());
    }

    #[test]
    fn page_range_non_numeric_error() {
        assert!(PageRange::parse("abc").is_err());
        assert!(PageRange::parse("1-abc").is_err());
        assert!(PageRange::parse("abc-5").is_err());
    }

    #[test]
    fn page_range_leading_trailing_dash() {
        // "-5" should fail with a clear error (empty start)
        let result = PageRange::parse("-5");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("both start and end required"),
            "error: {}",
            msg
        );

        // "5-" should fail with a clear error (empty end)
        let result = PageRange::parse("5-");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("both start and end required"),
            "error: {}",
            msg
        );
    }

    #[test]
    fn page_range_empty_error() {
        assert!(PageRange::parse("").is_err());
        assert!(PageRange::parse("  ").is_err());
    }

    #[test]
    fn page_range_too_large() {
        let result = PageRange::parse("1-2000000");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("max is 1000000"), "error: {}", msg);
    }

    #[test]
    fn layer_config_default() {
        let lc = LayerConfig::default();
        assert!(lc.presentation);
        assert!(lc.relationships);
        assert!(lc.interactions);
    }

    #[test]
    fn rendering_profile_default_is_ocr_friendly() {
        let config = Config::new();
        assert_eq!(config.rendering_profile, RenderingProfile::OcrFriendly);
    }

    #[test]
    fn rendering_profile_builder_toggles_field() {
        let config = Config::new().rendering_profile(RenderingProfile::Visual);
        assert_eq!(config.rendering_profile, RenderingProfile::Visual);
        let config = config.rendering_profile(RenderingProfile::OcrFriendly);
        assert_eq!(config.rendering_profile, RenderingProfile::OcrFriendly);
    }

    #[test]
    fn strict_fonts_default_off_via_assets() {
        let config = Config::new();
        assert!(!config.assets.strict_fonts);
    }

    #[test]
    fn strict_fonts_via_assets_builder() {
        let config = Config::new().assets(AssetConfig::default().strict_fonts(true));
        assert!(config.assets.strict_fonts);
    }

    // ---- coverage ----
    //
    // Each test pins one of the new field paths so a future cull can't
    // silently re-introduce the dropped methods or move state around
    // without breaking these.

    #[test]
    fn config_collect_diagnostics_default_true() {
        let cfg = Config::new();
        assert!(cfg.collect_diagnostics);
    }

    #[test]
    fn config_collect_diagnostics_builder_toggles() {
        let cfg = Config::new().collect_diagnostics(false);
        assert!(!cfg.collect_diagnostics);
        let cfg = cfg.collect_diagnostics(true);
        assert!(cfg.collect_diagnostics);
    }

    #[test]
    fn config_layers_builder_assigns() {
        // LayerConfig is #[non_exhaustive], so build via default + field
        // mutation rather than a struct literal.
        let mut lc = LayerConfig::default();
        lc.presentation = false;
        lc.interactions = false;
        lc.images = false;
        let cfg = Config::new().layers(lc);
        assert!(!cfg.layers.presentation);
        assert!(cfg.layers.relationships);
        assert!(!cfg.layers.interactions);
        assert!(cfg.layers.tables);
        assert!(!cfg.layers.images);
    }

    #[test]
    fn layer_config_content_only_constructor() {
        let lc = LayerConfig::content_only();
        assert!(!lc.presentation);
        assert!(!lc.relationships);
        assert!(!lc.interactions);
        // Tables and images are content-spine concerns; content_only
        // keeps them enabled so users get the document body but no
        // overlay metadata.
        assert!(lc.tables);
        assert!(lc.images);
    }

    #[test]
    fn config_assets_replaces_with_fonts_method() {
        // Old: Config::new().with_fonts() set assets.fonts = true.
        // New: callers chain through AssetConfig.
        let cfg = Config::new().assets(AssetConfig::default().fonts(true));
        assert!(cfg.assets.fonts);
        assert!(cfg.assets.images); // default still on
    }

    #[test]
    fn config_assets_default_has_no_strict_fonts() {
        let cfg = Config::new();
        assert!(!cfg.assets.strict_fonts);
        assert!(cfg.assets.images);
        assert!(!cfg.assets.fonts);
    }

    #[test]
    fn config_assets_strict_fonts_via_builder() {
        let cfg = Config::new().assets(AssetConfig::default().strict_fonts(true));
        assert!(cfg.assets.strict_fonts);
    }

    #[test]
    fn config_assets_full_chain() {
        // Cover the AssetConfig builder chain end to end.
        let cfg = Config::new().assets(
            AssetConfig::default()
                .fonts(true)
                .images(true)
                .strict_fonts(true),
        );
        assert!(cfg.assets.fonts);
        assert!(cfg.assets.images);
        assert!(cfg.assets.strict_fonts);
    }

    #[test]
    fn config_limits_memory_budget_default_is_none() {
        let cfg = Config::new();
        assert!(cfg.limits.memory_budget.is_none());
    }

    #[test]
    fn config_limits_memory_budget_via_builder() {
        let cfg =
            Config::new().limits(Limits::builder().memory_budget(Some(2_000_000_000)).build());
        assert_eq!(cfg.limits.memory_budget, Some(2_000_000_000));
    }

    #[test]
    fn config_limits_memory_budget_struct_literal_via_field() {
        let mut cfg = Config::new();
        cfg.limits.memory_budget = Some(1_000_000);
        assert_eq!(cfg.limits.memory_budget, Some(1_000_000));
        cfg.limits.memory_budget = None;
        assert!(cfg.limits.memory_budget.is_none());
    }

    #[test]
    fn config_limits_max_warnings_default_is_some_1000() {
        let cfg = Config::new();
        assert_eq!(cfg.limits.max_warnings, Some(1000));
    }

    #[test]
    fn config_limits_max_warnings_disable_via_none() {
        let cfg = Config::new().limits(Limits::builder().max_warnings(None).build());
        assert!(cfg.limits.max_warnings.is_none());
    }

    #[test]
    fn config_limits_max_warnings_custom_value() {
        let cfg = Config::new().limits(Limits::builder().max_warnings(Some(50)).build());
        assert_eq!(cfg.limits.max_warnings, Some(50));
    }

    #[test]
    fn config_assets_field_path_struct_literal_friendly() {
        // Verify that a caller can mutate AssetConfig fields directly,
        // matching the cli main.rs pattern (config.assets.fonts = true).
        let mut cfg = Config::new();
        cfg.assets.fonts = true;
        cfg.assets.strict_fonts = true;
        assert!(cfg.assets.fonts);
        assert!(cfg.assets.strict_fonts);
    }

    #[test]
    fn config_debug_does_not_leak_password() {
        // Regression: password masking still works after the field shape
        // change in.
        let cfg = Config::new().password("super-secret-2026");
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("super-secret-2026"));
        assert!(dbg.contains("***"));
    }

    #[test]
    fn config_debug_includes_collect_diagnostics_flag() {
        // The new collect_diagnostics flag must appear in Debug for
        // post-mortem on extraction failures where the flag matters.
        let cfg = Config::new();
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("collect_diagnostics"));
    }

    #[test]
    fn config_default_yields_same_as_new() {
        let new = Config::new();
        let def = Config::default();
        // Spot-check the non-Arc fields (Arc<dyn DiagnosticsSink> doesn't
        // PartialEq, so we sample observable fields instead).
        assert_eq!(new.format, def.format);
        assert_eq!(new.password, def.password);
        assert_eq!(new.collect_diagnostics, def.collect_diagnostics);
        assert_eq!(new.assets.fonts, def.assets.fonts);
        assert_eq!(new.assets.strict_fonts, def.assets.strict_fonts);
        assert_eq!(new.limits.memory_budget, def.limits.memory_budget);
        assert_eq!(new.limits.max_warnings, def.limits.max_warnings);
    }

    #[test]
    fn config_builder_methods_chain() {
        // Smoke test: every new builder method on the final 10-method
        // surface chains cleanly off Config::new() in one expression.
        let cfg = Config::new()
            .format(Format::Pdf)
            .password("p")
            .pages("1")
            .unwrap()
            .diagnostics(Arc::new(NullDiagnostics))
            .limits(Limits::default())
            .assets(AssetConfig::default())
            .layers(LayerConfig::default())
            .rendering_profile(RenderingProfile::default())
            .collect_diagnostics(true);
        assert_eq!(cfg.format, Some(Format::Pdf));
        assert!(cfg.collect_diagnostics);
    }
}

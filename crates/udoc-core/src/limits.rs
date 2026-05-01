//! Centralized safety limits for document extraction.
//!
//! Every backend uses safety limits to bound resource consumption on untrusted
//! input. This module provides a [`Limits`] struct with sensible defaults that
//! backends can reference. Users can override limits via [`Limits::builder()`].
//!
//! Format-specific limits (e.g., PDF lexer paren depth, BIFF8 record count)
//! stay in their respective crates. This module covers cross-format limits.
//!
//! # Bounding allocations
//!
//! Use [`safe_alloc_size`] at every call site that turns an attacker-controlled
//! size field into a `Vec` allocation (PDF `/Length`, image `width * height *
//! bpp / 8`, JBIG2 region dims, etc.). The helper checks the request against a
//! caller-chosen ceiling and returns a typed [`crate::error::ResourceLimitExceeded`]
//! instead of panicking the allocator.

/// Safety limits for document extraction.
///
/// All limits have sensible defaults. Use [`Limits::builder()`] to override.
///
/// ```
/// use udoc_core::limits::Limits;
///
/// let limits = Limits::builder()
///     .max_file_size(128 * 1024 * 1024)  // 128 MB
///     .max_pages(1000)
///     .build();
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Limits {
    /// Maximum file size in bytes (default: 256 MB).
    pub max_file_size: u64,

    /// Maximum number of pages/sheets/slides (default: 100,000).
    pub max_pages: usize,

    /// Maximum nesting depth for recursive structures (default: 256).
    pub max_nesting_depth: usize,

    /// Maximum number of table rows per table (default: 100,000).
    pub max_table_rows: usize,

    /// Maximum number of cells per table row (default: 10,000).
    pub max_cells_per_row: usize,

    /// Maximum text length per element in bytes (default: 10 MB).
    pub max_text_length: usize,

    /// Maximum number of styles/formats (default: 50,000).
    pub max_styles: usize,

    /// Maximum style inheritance chain depth (default: 10).
    pub max_style_depth: usize,

    /// Maximum number of images per document (default: 10,000).
    pub max_images: usize,

    /// Maximum decompressed size for compressed data (default: 250 MB).
    pub max_decompressed_size: u64,

    /// Maximum number of warnings collected before suppression (default
    /// `Some(1000)`). `None` disables the cap entirely (use only on
    /// trusted input). Per-Document, NOT per-page.
    ///
    /// When the cap is hit, the [`crate::diagnostics::CollectingDiagnostics`]
    /// sink installed by the facade replaces further warnings with a
    /// single synthetic [`crate::diagnostics::WarningKind::WarningsTruncated`]
    /// carrying the suppressed count. Custom sinks (those passed via
    /// [`crate::diagnostics::DiagnosticsSink`]) bypass this cap and own
    /// their own retention policy.
    pub max_warnings: Option<usize>,

    /// Soft memory budget in bytes for long-running batch workers
    /// (T60-MEMBATCH).
    ///
    /// When set, the facade releases document-scoped caches between
    /// documents if process RSS exceeds this budget. Peak memory *within*
    /// a single document is not affected -- this is a soft ceiling that
    /// kicks in between documents, before the next one starts. `None`
    /// (the default) means no budget.
    ///
    /// The RSS read is best-effort (`/proc/self/status` on Linux; a no-op
    /// fallback on other platforms). Callers who want deterministic
    /// per-doc resets should drive the extractor's reset method
    /// explicitly instead.
    ///
    /// Typical value: `Some(2_000_000_000)` (2 GB) for a 20K-doc CI worker.
    pub memory_budget: Option<usize>,
}

// ---- Default constants (single source of truth) ----

/// Default maximum file size: 256 MB.
pub const DEFAULT_MAX_FILE_SIZE: u64 = 256 * 1024 * 1024;

/// Default maximum pages/sheets/slides.
pub const DEFAULT_MAX_PAGES: usize = 100_000;

/// Default maximum nesting depth for recursive structures.
pub const DEFAULT_MAX_NESTING_DEPTH: usize = 256;

/// Default maximum table rows per table.
pub const DEFAULT_MAX_TABLE_ROWS: usize = 100_000;

/// Default maximum cells per table row.
pub const DEFAULT_MAX_CELLS_PER_ROW: usize = 10_000;

/// Default maximum text length per element: 10 MB.
pub const DEFAULT_MAX_TEXT_LENGTH: usize = 10 * 1024 * 1024;

/// Default maximum styles/formats.
pub const DEFAULT_MAX_STYLES: usize = 50_000;

/// Default maximum style inheritance chain depth.
pub const DEFAULT_MAX_STYLE_DEPTH: usize = 10;

/// Default maximum images per document.
pub const DEFAULT_MAX_IMAGES: usize = 10_000;

/// Default maximum decompressed size: 250 MB.
pub const DEFAULT_MAX_DECOMPRESSED_SIZE: u64 = 250 * 1024 * 1024;

/// Default maximum warnings before suppression.
pub const DEFAULT_MAX_WARNINGS: usize = 1_000;

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_size: DEFAULT_MAX_FILE_SIZE,
            max_pages: DEFAULT_MAX_PAGES,
            max_nesting_depth: DEFAULT_MAX_NESTING_DEPTH,
            max_table_rows: DEFAULT_MAX_TABLE_ROWS,
            max_cells_per_row: DEFAULT_MAX_CELLS_PER_ROW,
            max_text_length: DEFAULT_MAX_TEXT_LENGTH,
            max_styles: DEFAULT_MAX_STYLES,
            max_style_depth: DEFAULT_MAX_STYLE_DEPTH,
            max_images: DEFAULT_MAX_IMAGES,
            max_decompressed_size: DEFAULT_MAX_DECOMPRESSED_SIZE,
            max_warnings: Some(DEFAULT_MAX_WARNINGS),
            memory_budget: None,
        }
    }
}

impl Limits {
    /// Create limits with all defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a builder for customizing limits.
    pub fn builder() -> LimitsBuilder {
        LimitsBuilder(Self::default())
    }
}

/// Builder for [`Limits`].
pub struct LimitsBuilder(Limits);

impl LimitsBuilder {
    /// Set maximum file size in bytes.
    pub fn max_file_size(mut self, size: u64) -> Self {
        self.0.max_file_size = size;
        self
    }

    /// Set maximum pages/sheets/slides.
    pub fn max_pages(mut self, n: usize) -> Self {
        self.0.max_pages = n;
        self
    }

    /// Set maximum nesting depth.
    pub fn max_nesting_depth(mut self, n: usize) -> Self {
        self.0.max_nesting_depth = n;
        self
    }

    /// Set maximum table rows.
    pub fn max_table_rows(mut self, n: usize) -> Self {
        self.0.max_table_rows = n;
        self
    }

    /// Set maximum cells per row.
    pub fn max_cells_per_row(mut self, n: usize) -> Self {
        self.0.max_cells_per_row = n;
        self
    }

    /// Set maximum text length per element.
    pub fn max_text_length(mut self, n: usize) -> Self {
        self.0.max_text_length = n;
        self
    }

    /// Set maximum styles.
    pub fn max_styles(mut self, n: usize) -> Self {
        self.0.max_styles = n;
        self
    }

    /// Set maximum style chain depth.
    pub fn max_style_depth(mut self, n: usize) -> Self {
        self.0.max_style_depth = n;
        self
    }

    /// Set maximum images.
    pub fn max_images(mut self, n: usize) -> Self {
        self.0.max_images = n;
        self
    }

    /// Set maximum decompressed data size.
    pub fn max_decompressed_size(mut self, size: u64) -> Self {
        self.0.max_decompressed_size = size;
        self
    }

    /// Set maximum warnings before suppression. Pass `None` to disable
    /// the cap (use only on trusted input).
    pub fn max_warnings(mut self, n: Option<usize>) -> Self {
        self.0.max_warnings = n;
        self
    }

    /// Set the soft memory budget in bytes (T60-MEMBATCH).
    ///
    /// When set, the facade releases document-scoped caches between
    /// documents if process RSS exceeds this budget. `None` (default)
    /// disables the budget.
    pub fn memory_budget(mut self, budget: Option<usize>) -> Self {
        self.0.memory_budget = budget;
        self
    }

    /// Build the limits.
    pub fn build(self) -> Limits {
        self.0
    }
}

// ---------------------------------------------------------------------------
// Human-friendly size helpers
// ---------------------------------------------------------------------------

/// Kilobytes to bytes.
pub const fn kb(n: u64) -> u64 {
    n * 1024
}

/// Megabytes to bytes.
pub const fn mb(n: u64) -> u64 {
    n * 1024 * 1024
}

/// Gigabytes to bytes.
pub const fn gb(n: u64) -> u64 {
    n * 1024 * 1024 * 1024
}

/// Parse a human-friendly size string into bytes.
///
/// Accepts: `"256mb"`, `"1gb"`, `"10kb"`, `"4096"` (raw bytes).
/// Case-insensitive. No space between number and unit.
///
/// ```
/// use udoc_core::limits::parse_size;
/// assert_eq!(parse_size("256mb").unwrap(), 256 * 1024 * 1024);
/// assert_eq!(parse_size("1gb").unwrap(), 1024 * 1024 * 1024);
/// assert_eq!(parse_size("4096").unwrap(), 4096);
/// ```
pub fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty size string".into());
    }

    let lower = s.to_ascii_lowercase();

    // Find where the numeric part ends and the unit begins.
    let num_end = lower
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(lower.len());

    let num_str = &lower[..num_end];
    let unit = &lower[num_end..];

    let num: u64 = num_str
        .parse()
        .map_err(|_| format!("invalid number in size: {s:?}"))?;

    let multiplier = match unit {
        "" | "b" => 1,
        "kb" | "k" => 1024,
        "mb" | "m" => 1024 * 1024,
        "gb" | "g" => 1024 * 1024 * 1024,
        _ => return Err(format!("unknown size unit: {unit:?} (expected kb, mb, gb)")),
    };

    num.checked_mul(multiplier)
        .ok_or_else(|| format!("size overflow: {s:?}"))
}

// ---------------------------------------------------------------------------
// Allocation-size bounds (SEC-ALLOC-CLAMP, task #62)
// ---------------------------------------------------------------------------

/// Default ceiling for a single bounded allocation: 512 MB.
///
/// This is the fallback used when a call site doesn't have a more specific
/// budget from [`Limits`]. Chosen to accommodate large-page rasterization
/// (~5000x5000 RGBA = 100 MB, 5x headroom) while rejecting the petabyte-scale
/// allocations produced by corrupt / attacker-crafted size fields.
pub const DEFAULT_MAX_ALLOC_BYTES: u64 = 512 * 1024 * 1024;

/// Bound an attacker-controlled allocation request against a configured
/// ceiling, returning the safe `usize` on success.
///
/// Use at every site that computes a buffer size from input bytes and hands
/// it to `Vec::with_capacity` / `vec![0u8; n]` / similar. The function does
/// NOT allocate; the caller is responsible for the allocation once it has the
/// bounded return value.
///
/// - `requested` is the size the parser / decoder computed from input.
/// - `limit` is the caller-chosen ceiling (typically pulled from [`Limits`]).
/// - `kind` is a short, stable tag naming the call site (e.g. `"stream"`,
///   `"image_buffer"`, `"jbig2_region"`). Not user-facing; used in
///   structured log filtering.
///
/// Returns [`crate::error::ResourceLimitExceeded`] wrapped in [`crate::error::Error`]
/// when `requested > limit` OR when `requested` exceeds [`usize::MAX`] on the
/// target platform (e.g. a u64 value that overflows usize on 32-bit systems).
///
/// # Examples
///
/// ```
/// use udoc_core::limits::safe_alloc_size;
///
/// // Happy path: a reasonable stream allocation.
/// let size = safe_alloc_size(4096, 1 << 20, "stream").unwrap();
/// assert_eq!(size, 4096);
///
/// // Rejection path: a malformed /Length field asking for 254 PB.
/// let err = safe_alloc_size(
///     254_505_142_248_651_671,
///     512 * 1024 * 1024,
///     "stream",
/// )
/// .unwrap_err();
/// let info = err.resource_limit_info().unwrap();
/// assert_eq!(info.kind, "stream");
/// assert_eq!(info.requested, 254_505_142_248_651_671);
/// ```
pub fn safe_alloc_size(
    requested: u64,
    limit: u64,
    kind: &'static str,
) -> crate::error::Result<usize> {
    if requested > limit {
        return Err(crate::error::Error::resource_limit_exceeded(
            requested, limit, kind,
        ));
    }
    usize::try_from(requested).map_err(|_| {
        crate::error::Error::resource_limit_exceeded(requested, usize::MAX as u64, kind)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible() {
        let limits = Limits::default();
        assert_eq!(limits.max_file_size, 256 * 1024 * 1024);
        assert_eq!(limits.max_pages, 100_000);
        assert_eq!(limits.max_nesting_depth, 256);
        assert_eq!(limits.max_table_rows, 100_000);
        assert_eq!(limits.max_text_length, 10 * 1024 * 1024);
    }

    #[test]
    fn builder_overrides() {
        let limits = Limits::builder()
            .max_file_size(mb(128))
            .max_pages(500)
            .build();
        assert_eq!(limits.max_file_size, 128 * 1024 * 1024);
        assert_eq!(limits.max_pages, 500);
        assert_eq!(limits.max_nesting_depth, 256);
    }

    #[test]
    fn size_helpers() {
        assert_eq!(kb(1), 1024);
        assert_eq!(mb(1), 1024 * 1024);
        assert_eq!(gb(1), 1024 * 1024 * 1024);
        assert_eq!(mb(256), 256 * 1024 * 1024);
    }

    #[test]
    fn parse_size_bytes() {
        assert_eq!(parse_size("4096").unwrap(), 4096);
        assert_eq!(parse_size("0").unwrap(), 0);
    }

    #[test]
    fn parse_size_units() {
        assert_eq!(parse_size("10kb").unwrap(), 10 * 1024);
        assert_eq!(parse_size("256mb").unwrap(), 256 * 1024 * 1024);
        assert_eq!(parse_size("1gb").unwrap(), 1024 * 1024 * 1024);
    }

    #[test]
    fn parse_size_case_insensitive() {
        assert_eq!(parse_size("256MB").unwrap(), parse_size("256mb").unwrap());
        assert_eq!(parse_size("1GB").unwrap(), parse_size("1gb").unwrap());
        assert_eq!(parse_size("10Kb").unwrap(), parse_size("10kb").unwrap());
    }

    #[test]
    fn parse_size_short_units() {
        assert_eq!(parse_size("10k").unwrap(), 10 * 1024);
        assert_eq!(parse_size("256m").unwrap(), 256 * 1024 * 1024);
        assert_eq!(parse_size("1g").unwrap(), 1024 * 1024 * 1024);
    }

    #[test]
    fn parse_size_with_b_suffix() {
        assert_eq!(parse_size("4096b").unwrap(), 4096);
    }

    #[test]
    fn parse_size_errors() {
        assert!(parse_size("").is_err());
        assert!(parse_size("abc").is_err());
        assert!(parse_size("10tb").is_err()); // no terabyte support
    }

    #[test]
    fn safe_alloc_size_allows_under_limit() {
        let size = safe_alloc_size(4096, mb(1), "stream").expect("under limit");
        assert_eq!(size, 4096);
    }

    #[test]
    fn safe_alloc_size_allows_exactly_at_limit() {
        let size = safe_alloc_size(mb(1), mb(1), "raster").expect("at limit");
        assert_eq!(size, mb(1) as usize);
    }

    #[test]
    fn safe_alloc_size_zero_is_fine() {
        let size = safe_alloc_size(0, mb(1), "empty").expect("zero alloc");
        assert_eq!(size, 0);
    }

    #[test]
    fn safe_alloc_size_rejects_over_limit() {
        let err = safe_alloc_size(mb(2), mb(1), "stream").expect_err("over limit");
        let info = err.resource_limit_info().expect("typed payload");
        assert_eq!(info.requested, mb(2));
        assert_eq!(info.limit, mb(1));
        assert_eq!(info.kind, "stream");
    }

    #[test]
    fn safe_alloc_size_rejects_the_alloc_bomb_magnitude() {
        // Magnitude observed in task #62: govdocs1/010258.pdf tricks udoc
        // into requesting 254,505,142,248,651,671 bytes (~254 PB).
        let err = safe_alloc_size(254_505_142_248_651_671, DEFAULT_MAX_ALLOC_BYTES, "stream")
            .expect_err("petabyte ask must be refused");
        let info = err.resource_limit_info().expect("typed payload");
        assert_eq!(info.kind, "stream");
        assert!(info.requested > info.limit);
    }

    #[test]
    fn safe_alloc_size_error_message_is_human_readable() {
        let err = safe_alloc_size(u64::MAX, 1024, "image_buffer").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("image_buffer"), "error mentions kind: {msg}");
        assert!(msg.contains("1024"), "error mentions limit: {msg}");
    }

    #[test]
    fn safe_alloc_size_default_cap_is_sane() {
        // A 4K image at 4 bytes/pixel = 64 MB, well under the default cap.
        let frame = 4096u64 * 4096 * 4;
        safe_alloc_size(frame, DEFAULT_MAX_ALLOC_BYTES, "raster").expect("4K RGBA fits");

        // A 100k x 100k image at 4 bytes/pixel = 40 GB, well over the cap.
        let bomb = 100_000u64 * 100_000 * 4;
        safe_alloc_size(bomb, DEFAULT_MAX_ALLOC_BYTES, "raster").expect_err("100k-square refused");
    }
}

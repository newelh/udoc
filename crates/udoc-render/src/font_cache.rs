//! Font cache for the page renderer.
//!
//! Maps font names to parsed glyph outlines for rendering. Parses embedded
//! TrueType/CFF font data from the AssetStore eagerly at construction time.
//! Falls back to embedded Liberation Sans for fonts without embedded data.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use udoc_core::diagnostics::{DiagnosticsSink, MissingGlyphInfo, Warning};
use udoc_core::document::assets::{AssetStore, FontProgramType};
use udoc_font::cff::CffFont;
use udoc_font::hinting::{HintedGlyph, HintingState};
use udoc_font::ttf::{GlyphOutline, TrueTypeFont};
use udoc_font::type1::Type1Font;
use udoc_font::type3_outline;
use udoc_font::types::strip_subset_prefix;

/// Target face within the Tier 1 bundled fallback inventory.
///
/// Picked by [`FontCache::route_tier1`] from a font name (case-insensitive,
/// subset-prefix-stripped) or by [`FontCache::route_by_unicode`] from a
/// glyph's Unicode codepoint when the named font can't render it. Callers
/// pass the chosen target back into [`FontCache::tier1_outline`] /
/// [`FontCache::tier1_advance`] to read the glyph data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier1Target {
    /// Liberation Sans Regular (the existing `fallback_sans`).
    SansRegular,
    /// Liberation Sans Bold.
    SansBold,
    /// Liberation Sans Italic.
    SansItalic,
    /// Liberation Sans BoldItalic.
    SansBoldItalic,
    /// Liberation Serif Regular (the existing `fallback_serif`).
    SerifRegular,
    /// Liberation Serif Bold (T1-SERIF, #193).
    SerifBold,
    /// Liberation Serif Italic (T1-SERIF, #193).
    SerifItalic,
    /// Liberation Serif BoldItalic (T1-SERIF, #193).
    SerifBoldItalic,
    /// Liberation Mono Regular.
    Mono,
    /// Latin Modern Roman Regular. Routes from CMR* / LMRoman*.
    LmRoman,
    /// Latin Modern Roman Italic. Routes from CMTI* / CMSL* / LMRoman*-italic.
    LmRomanItalic,
    /// Latin Modern Math. Routes from CMMI*/CMSY*/CMEX*/MSAM*/MSBM*/LMMath*
    /// and as a per-glyph fallback for Unicode math-operator / Mathematical
    /// Alphanumeric Symbols codepoints.
    LmMath,
    /// Noto Sans Arabic Regular (Tier 2,). Routes from Arabic
    /// Unicode ranges (U+0600..06FF, U+0750..077F, U+FB50..FDFF, U+FE70..FEFF)
    /// when the source font lacks coverage. `None` when the `tier2-arabic`
    /// feature is disabled.
    ArabicRegular,
    /// Noto Sans Arabic Bold (Tier 2,). Routes from the same
    /// Arabic ranges as `ArabicRegular` when the source font name hints at
    /// bold weight. `None` when the `tier2-arabic` feature is disabled.
    ArabicBold,
}

// -----------------------------------------------------------------------------
// Font assets.
//
// The base Liberation Sans/Serif Regular pair is always linked -- they are the
// terminal fallback and the renderer must always have something to route to.
// Everything else is feature-gated so downstream consumers can opt
// out and shrink their binary. See the crate-level `Cargo.toml` for the
// feature matrix.
// -----------------------------------------------------------------------------

/// Soft cap on per-document cache sizes (T60-MEMBATCH).
///
/// `outline_cache`, `hinted_glyph_cache`, and `hinting_cache` each grow as
/// new `(font, char)` / `(font, gid, ppem)` / `(font, ppem)` tuples are
/// encountered during a document render. On pathological PDFs (e.g. full
/// Unicode range across many fonts) these can reach tens of thousands of
/// entries in a single document. We cap them and drop the whole cache on
/// overflow: hot entries repopulate in O(1) per lookup, the allocator
/// reclaims pages, and peak memory stays bounded.
///
/// 8192 keeps roughly one pass of body text + headings + math without
/// thrashing on a typical arxiv doc. Tune upward only if benchmark shows
/// glyph-hit rate degrading.
const CACHE_SOFT_CAP: usize = 8192;

/// Embedded Liberation Sans Regular for sans-serif fallback rendering.
/// Licensed under SIL Open Font License 1.1.
static LIBERATION_SANS: &[u8] = include_bytes!("../../udoc-font/assets/LiberationSans-Regular.ttf");

/// Embedded Liberation Serif Regular for serif fallback rendering.
/// Licensed under SIL Open Font License 1.1.
static LIBERATION_SERIF: &[u8] =
    include_bytes!("../../udoc-font/assets/LiberationSerif-Regular.ttf");

/// Embedded Liberation Sans Bold (Tier 1 bold weight fallback).
/// Licensed under SIL Open Font License 1.1.
#[cfg(feature = "tier1-sans-bold")]
static LIBERATION_SANS_BOLD: &[u8] =
    include_bytes!("../../udoc-font/assets/LiberationSans-Bold.ttf");

/// Embedded Liberation Sans Italic (Tier 1 italic fallback).
/// Licensed under SIL Open Font License 1.1.
#[cfg(feature = "tier1-sans-bold")]
static LIBERATION_SANS_ITALIC: &[u8] =
    include_bytes!("../../udoc-font/assets/LiberationSans-Italic.ttf");

/// Embedded Liberation Sans BoldItalic (Tier 1 bold+italic fallback).
/// Licensed under SIL Open Font License 1.1.
#[cfg(feature = "tier1-sans-bold")]
static LIBERATION_SANS_BOLD_ITALIC: &[u8] =
    include_bytes!("../../udoc-font/assets/LiberationSans-BoldItalic.ttf");

/// Embedded Liberation Serif Bold (Tier 1 serif bold fallback, #193).
/// Licensed under SIL Open Font License 1.1.
#[cfg(feature = "tier1-serif-bold")]
static LIBERATION_SERIF_BOLD: &[u8] =
    include_bytes!("../../udoc-font/assets/LiberationSerif-Bold.ttf");

/// Embedded Liberation Serif Italic (Tier 1 serif italic fallback, #193).
/// Licensed under SIL Open Font License 1.1.
#[cfg(feature = "tier1-serif-bold")]
static LIBERATION_SERIF_ITALIC: &[u8] =
    include_bytes!("../../udoc-font/assets/LiberationSerif-Italic.ttf");

/// Embedded Liberation Serif BoldItalic (Tier 1 serif bold+italic fallback, #193).
/// Licensed under SIL Open Font License 1.1.
#[cfg(feature = "tier1-serif-bold")]
static LIBERATION_SERIF_BOLD_ITALIC: &[u8] =
    include_bytes!("../../udoc-font/assets/LiberationSerif-BoldItalic.ttf");

/// Embedded Liberation Mono Regular (Tier 1 monospace fallback).
/// Licensed under SIL Open Font License 1.1.
#[cfg(feature = "tier1-fonts")]
static LIBERATION_MONO: &[u8] = include_bytes!("../../udoc-font/assets/LiberationMono-Regular.ttf");

/// Embedded Latin Modern Roman Regular (Tier 1 LaTeX CMR fallback).
/// Licensed under the GUST Font License (LPPL-based, DFSG-free).
#[cfg(feature = "tier1-fonts")]
static LATIN_MODERN_ROMAN: &[u8] =
    include_bytes!("../../udoc-font/assets/LatinModernRoman-Regular.otf");

/// Embedded Latin Modern Roman Italic (Tier 1 LaTeX CMMI/italic fallback).
/// Licensed under the GUST Font License (LPPL-based, DFSG-free).
#[cfg(feature = "tier1-fonts")]
static LATIN_MODERN_ROMAN_ITALIC: &[u8] =
    include_bytes!("../../udoc-font/assets/LatinModernRoman-Italic.otf");

/// Embedded Latin Modern Math subset (Tier 1 LaTeX CMSY/CMEX math symbol fallback).
/// Covers BMP math operators, Greek, mathematical alphanumeric symbols, arrows,
/// and supplemental math operators. Licensed under the GUST Font License.
#[cfg(feature = "tier1-fonts")]
static LATIN_MODERN_MATH: &[u8] =
    include_bytes!("../../udoc-font/assets/LatinModernMath-Subset.otf");

/// Embedded Noto Sans CJK SC subset for CJK character fallback.
/// Format: [cff_len:u32LE][cff_data][num_entries:u32LE][(codepoint:u32LE, gid:u16LE)...]
/// Licensed under SIL Open Font License 1.1.
#[cfg(feature = "cjk-fonts")]
static NOTO_SANS_CJK: &[u8] =
    include_bytes!("../../udoc-font/assets/NotoSansCJK-Subset.cff_bundle");

/// Embedded Noto Sans Arabic Regular (Tier 2 Arabic-script fallback,
///). Closes the Arabic-script genre on the IA corpus where
/// the source font's coverage is sparse or missing. Licensed under SIL
/// Open Font License 1.1 (see `LICENSES/NotoSansArabic-OFL.txt`).
#[cfg(feature = "tier2-arabic")]
static NOTO_SANS_ARABIC_REGULAR: &[u8] =
    include_bytes!("../../udoc-font/assets/NotoSansArabic-Regular.ttf");

/// Embedded Noto Sans Arabic Bold (Tier 2 Arabic-script bold fallback,
///). Licensed under SIL Open Font License 1.1.
#[cfg(feature = "tier2-arabic")]
static NOTO_SANS_ARABIC_BOLD: &[u8] =
    include_bytes!("../../udoc-font/assets/NotoSansArabic-Bold.ttf");

/// Tier 1 fallback font bundle: weights, monospace, and LaTeX math.
///
/// Each face is `Option` so the crate's feature flags (`tier1-fonts`,
/// `tier1-serif-bold`, `tier1-sans-bold`;) can opt out of
/// individual assets without breaking routing. When a routed face is
/// `None` the caller's fallback chain picks up `fallback_sans` or
/// `fallback_serif` (always available) and the render degrades to
/// a regular-weight substitute.
///
/// Parsed eagerly at construction so malformed assets fail loudly
/// rather than at render time. M-36 wires routing (font-name sniff
/// and Unicode range dispatch) over this bundle.
#[allow(dead_code)] // fields read in M-36 and in the Tier1 smoke tests
struct Tier1Bundle {
    /// Liberation Sans Bold. `None` when `tier1-sans-bold` feature is off.
    sans_bold: Option<TrueTypeFont>,
    /// Liberation Sans Italic. `None` when `tier1-sans-bold` feature is off.
    sans_italic: Option<TrueTypeFont>,
    /// Liberation Sans BoldItalic. `None` when `tier1-sans-bold` feature is off.
    sans_bold_italic: Option<TrueTypeFont>,
    /// Liberation Serif Bold (T1-SERIF, #193). `None` when `tier1-serif-bold` feature is off.
    serif_bold: Option<TrueTypeFont>,
    /// Liberation Serif Italic (T1-SERIF, #193). `None` when `tier1-serif-bold` feature is off.
    serif_italic: Option<TrueTypeFont>,
    /// Liberation Serif BoldItalic (T1-SERIF, #193). `None` when `tier1-serif-bold` feature is off.
    serif_bold_italic: Option<TrueTypeFont>,
    /// Liberation Mono Regular. `None` when `tier1-fonts` feature is off.
    mono: Option<TrueTypeFont>,
    /// Latin Modern Roman Regular (LaTeX CMR substitute). `None` when `tier1-fonts` feature is off.
    lm_roman: Option<CffFont>,
    /// Latin Modern Roman Italic (LaTeX CMMI / italic substitute). `None` when `tier1-fonts` feature is off.
    lm_roman_italic: Option<CffFont>,
    /// Latin Modern Math (LaTeX CMSY / CMEX / math symbol substitute). `None` when `tier1-fonts` feature is off.
    lm_math: Option<CffFont>,
    /// Noto Sans Arabic Regular (Tier 2 Arabic-script fallback,
    ///). `None` when the `tier2-arabic` feature is off.
    arabic_regular: Option<TrueTypeFont>,
    /// Noto Sans Arabic Bold (Tier 2 Arabic-script bold fallback,
    ///). `None` when the `tier2-arabic` feature is off.
    arabic_bold: Option<TrueTypeFont>,
}

impl Tier1Bundle {
    fn load() -> Self {
        #[cfg(feature = "tier1-sans-bold")]
        let sans_bold = Some(
            TrueTypeFont::from_bytes(LIBERATION_SANS_BOLD)
                .expect("embedded Liberation Sans Bold should parse"),
        );
        #[cfg(not(feature = "tier1-sans-bold"))]
        let sans_bold = None;

        #[cfg(feature = "tier1-sans-bold")]
        let sans_italic = Some(
            TrueTypeFont::from_bytes(LIBERATION_SANS_ITALIC)
                .expect("embedded Liberation Sans Italic should parse"),
        );
        #[cfg(not(feature = "tier1-sans-bold"))]
        let sans_italic = None;

        #[cfg(feature = "tier1-sans-bold")]
        let sans_bold_italic = Some(
            TrueTypeFont::from_bytes(LIBERATION_SANS_BOLD_ITALIC)
                .expect("embedded Liberation Sans BoldItalic should parse"),
        );
        #[cfg(not(feature = "tier1-sans-bold"))]
        let sans_bold_italic = None;

        #[cfg(feature = "tier1-serif-bold")]
        let serif_bold = Some(
            TrueTypeFont::from_bytes(LIBERATION_SERIF_BOLD)
                .expect("embedded Liberation Serif Bold should parse"),
        );
        #[cfg(not(feature = "tier1-serif-bold"))]
        let serif_bold = None;

        #[cfg(feature = "tier1-serif-bold")]
        let serif_italic = Some(
            TrueTypeFont::from_bytes(LIBERATION_SERIF_ITALIC)
                .expect("embedded Liberation Serif Italic should parse"),
        );
        #[cfg(not(feature = "tier1-serif-bold"))]
        let serif_italic = None;

        #[cfg(feature = "tier1-serif-bold")]
        let serif_bold_italic = Some(
            TrueTypeFont::from_bytes(LIBERATION_SERIF_BOLD_ITALIC)
                .expect("embedded Liberation Serif BoldItalic should parse"),
        );
        #[cfg(not(feature = "tier1-serif-bold"))]
        let serif_bold_italic = None;

        #[cfg(feature = "tier1-fonts")]
        let mono = Some(
            TrueTypeFont::from_bytes(LIBERATION_MONO)
                .expect("embedded Liberation Mono should parse"),
        );
        #[cfg(not(feature = "tier1-fonts"))]
        let mono = None;

        #[cfg(feature = "tier1-fonts")]
        let lm_roman = Some(
            parse_otf_cff(LATIN_MODERN_ROMAN).expect("embedded Latin Modern Roman should parse"),
        );
        #[cfg(not(feature = "tier1-fonts"))]
        let lm_roman = None;

        #[cfg(feature = "tier1-fonts")]
        let lm_roman_italic = Some(
            parse_otf_cff(LATIN_MODERN_ROMAN_ITALIC)
                .expect("embedded Latin Modern Roman Italic should parse"),
        );
        #[cfg(not(feature = "tier1-fonts"))]
        let lm_roman_italic = None;

        #[cfg(feature = "tier1-fonts")]
        let lm_math = Some(
            parse_otf_cff(LATIN_MODERN_MATH).expect("embedded Latin Modern Math should parse"),
        );
        #[cfg(not(feature = "tier1-fonts"))]
        let lm_math = None;

        #[cfg(feature = "tier2-arabic")]
        let arabic_regular = Some(
            TrueTypeFont::from_bytes(NOTO_SANS_ARABIC_REGULAR)
                .expect("embedded Noto Sans Arabic Regular should parse"),
        );
        #[cfg(not(feature = "tier2-arabic"))]
        let arabic_regular = None;

        #[cfg(feature = "tier2-arabic")]
        let arabic_bold = Some(
            TrueTypeFont::from_bytes(NOTO_SANS_ARABIC_BOLD)
                .expect("embedded Noto Sans Arabic Bold should parse"),
        );
        #[cfg(not(feature = "tier2-arabic"))]
        let arabic_bold = None;

        Self {
            sans_bold,
            sans_italic,
            sans_bold_italic,
            serif_bold,
            serif_italic,
            serif_bold_italic,
            mono,
            lm_roman,
            lm_roman_italic,
            lm_math,
            arabic_regular,
            arabic_bold,
        }
    }
}

/// Parse a CFF font program out of an OpenType container (magic `OTTO`).
///
/// Peel the `CFF ` table off an OpenType/CFF container and parse it.
///
/// The Tier 1 LM Roman and LM Math assets are distributed upstream as full
/// OTF files (sfnt table directory + `CFF ` table). The bare
/// `CffFont::from_bytes` parser expects raw CFF bytes (as a decompressed
/// PDF FontFile3 stream would arrive), so we pull the inner `CFF ` table
/// via the shared [`udoc_font::otf`] helper before handing bytes to CFF.
///
/// Returns `Err` if the data is not a valid OTF/CFF container or if the
/// embedded CFF table fails to parse. See issue #206.
#[cfg(feature = "tier1-fonts")]
fn parse_otf_cff(data: &[u8]) -> Result<CffFont, udoc_font::error::Error> {
    let cff = udoc_font::otf::extract_cff_table(data)?;
    CffFont::from_bytes(cff)
}

/// Process-global, immutable font assets shared across every [`FontCache`]
/// instance via an `Arc<FontBundle>` (loaded once via [`font_bundle`]).
///
/// These bytes never change between documents -- they're the same Liberation
/// Sans/Serif, CJK fallback, and Tier 1 bundle whether you're rendering a
/// 1-page receipt or doc #500 in a bench worker. Holding them in a process-
/// global Arc means:
///
/// - We parse them ONCE per process, not once per document. On a 500-doc
///   bench worker this avoids ~500 redundant ~5 MB parses (M-19, #63
///   JBIG2-SOFTMASK-HANG: the heap-fragmentation pressure that turned a
///   1.2 s render into a 27-min hang largely came from this churn).
/// - Many `FontCache` instances can share one bundle without copying the
///   underlying font program data; only the per-document maps grow.
///
/// The struct is intentionally `pub` (not `pub(crate)`) so embedded users
/// can hold a clone of the bundle across their own per-doc cache instances
/// when they want to amortise across many docs in one process.
pub struct FontBundle {
    /// Pre-parsed sans-serif fallback font (Liberation Sans).
    fallback_sans: TrueTypeFont,
    /// Pre-parsed serif fallback font (Liberation Serif).
    fallback_serif: TrueTypeFont,
    /// CJK fallback: CFF font for CJK characters (None if cjk-fonts feature off).
    fallback_cjk: Option<CffFont>,
    /// CJK fallback: Unicode codepoint -> GID mapping.
    cjk_cmap: HashMap<u32, u16>,
    /// Tier 1 bundle: bold/italic weights, monospace, and LaTeX math.
    tier1: Tier1Bundle,
}

impl FontBundle {
    /// Parse the embedded font assets fresh. Allocates several MB.
    /// Prefer [`font_bundle`] which calls this once per process and caches
    /// the result.
    fn load() -> Self {
        let fallback_sans = TrueTypeFont::from_bytes(LIBERATION_SANS)
            .expect("embedded Liberation Sans should parse");
        let fallback_serif = TrueTypeFont::from_bytes(LIBERATION_SERIF)
            .expect("embedded Liberation Serif should parse");
        let (fallback_cjk, cjk_cmap) = load_cjk_fallback();
        let tier1 = Tier1Bundle::load();
        Self {
            fallback_sans,
            fallback_serif,
            fallback_cjk,
            cjk_cmap,
            tier1,
        }
    }
}

/// Process-global cache for [`FontBundle`]. Loaded on first call; subsequent
/// calls return the same `Arc` clone (~few hundred ns).
pub fn font_bundle() -> Arc<FontBundle> {
    static BUNDLE: OnceLock<Arc<FontBundle>> = OnceLock::new();
    BUNDLE.get_or_init(|| Arc::new(FontBundle::load())).clone()
}

/// Parsed font cache, built from AssetStore fonts.
///
/// Parses all per-document font data eagerly at construction time (font
/// count is small, typically 3-10 per document). Falls back to Liberation
/// Sans (sans-serif) or Liberation Serif (serif) based on the requested
/// font name.
///
/// Process-global font assets (Liberation, CJK, Tier 1) live in a
/// shared `Arc<FontBundle>` and are loaded exactly once per process; only
/// per-document state is rebuilt when `FontCache::new` is called for a new
/// doc. This is the win that closes #63 (JBIG2-SOFTMASK-HANG) -- a bench
/// worker processing 500 docs no longer pays the ~5 MB bundle-parse cost
/// 500 times.
pub struct FontCache {
    /// Process-global font assets shared across every FontCache in this
    /// process. Loaded lazily once via [`font_bundle`].
    bundle: Arc<FontBundle>,
    /// Parsed TrueType fonts keyed by display name.
    ttf_fonts: HashMap<String, TrueTypeFont>,
    /// Parsed CFF fonts keyed by display name.
    cff_fonts: HashMap<String, CffFont>,
    /// Parsed Type1 fonts keyed by display name.
    type1_fonts: HashMap<String, Type1Font>,
    /// PDF encoding maps: font name -> (byte code -> glyph name).
    /// Used for by-code glyph lookup in subset fonts with custom encodings.
    encoding_maps: HashMap<String, HashMap<u8, String>>,
    /// Parsed `/W` tables for composite (Type0) fonts, keyed by font name.
    ///
    /// Value: `(default_width, per_cid_widths)`. Used by
    /// [`FontCache::advance_width_by_gid`] so CID TrueType subsets whose
    /// PDF-declared advances disagree with the embedded `hmtx` (MS Word
    /// export, issue #182) render with the correct per-glyph spacing.
    cid_widths: HashMap<String, (u32, HashMap<u32, u16>)>,
    /// Type3 glyph outlines keyed by (font_name, unicode_char).
    type3_glyphs: HashMap<String, HashMap<char, GlyphOutline>>,
    /// TrueType hinting state cache: (font_name, ppem) -> HintingState.
    hinting_cache: HashMap<(String, u16), HintingState>,
    /// Hinted glyph outline cache: (font_name, glyph_id, ppem) -> HintedGlyph.
    hinted_glyph_cache: HashMap<(String, u16, u16), HintedGlyph>,
    /// Glyph outline cache: avoids re-interpreting charstrings for previously
    /// seen glyphs. Keyed by (font_name, char) and persists across pages.
    outline_cache: HashMap<(String, char), Option<GlyphOutline>>,
    /// Auto-hinter global metrics cache: font_name -> GlobalMetrics.
    /// Computed lazily on first access by analyzing reference glyphs.
    auto_hint_metrics_cache: HashMap<String, Option<super::auto_hinter::metrics::GlobalMetrics>>,
    /// Optional diagnostics sink. When set, the cache emits a
    /// [`kind::MISSING_GLYPH`](udoc_core::diagnostics::kind::MISSING_GLYPH)
    /// warning the first time a `(font, codepoint)` lookup exhausts the
    /// fallback chain. Deduped via `missing_glyph_seen`.
    sink: Option<Arc<dyn DiagnosticsSink>>,
    /// `(font_name, codepoint)` pairs that have already triggered a
    /// missing-glyph warning. Prevents flooding the sink when a font is
    /// missing a glyph used on every line of a document.
    missing_glyph_seen: HashSet<(String, u32)>,
}

impl Default for FontCache {
    fn default() -> Self {
        Self::empty()
    }
}

impl FontCache {
    /// Create a font cache with only the fallback fonts (no embedded fonts).
    ///
    /// Reuses the process-global [`FontBundle`] (loaded once per process via
    /// [`font_bundle`]); subsequent calls in the same process don't re-parse
    /// the Liberation/CJK/Tier 1 byte arrays.
    pub fn empty() -> Self {
        Self::with_bundle(font_bundle())
    }

    /// Create an empty font cache backed by an explicit shared bundle.
    ///
    /// Most callers want [`FontCache::empty`] / [`FontCache::new`] which
    /// pull the bundle from a process-global `OnceLock`. This entry point
    /// is for tests and embedded users that want to manage the bundle
    /// lifetime themselves (e.g. bench-compare workers that hold one
    /// bundle across thousands of docs).
    pub fn with_bundle(bundle: Arc<FontBundle>) -> Self {
        Self {
            bundle,
            ttf_fonts: HashMap::new(),
            cff_fonts: HashMap::new(),
            type1_fonts: HashMap::new(),
            encoding_maps: HashMap::new(),
            cid_widths: HashMap::new(),
            type3_glyphs: HashMap::new(),
            hinting_cache: HashMap::new(),
            hinted_glyph_cache: HashMap::new(),
            outline_cache: HashMap::new(),
            auto_hint_metrics_cache: HashMap::new(),
            sink: None,
            missing_glyph_seen: HashSet::new(),
        }
    }

    /// Create a font cache from an AssetStore.
    ///
    /// Parses all font assets eagerly. Fonts that fail to parse are skipped
    /// (the fallback font will be used instead).
    ///
    /// Reuses the process-global [`FontBundle`] for fallback / Tier 1 / CJK
    /// fonts; only the AssetStore-provided embedded fonts are parsed fresh
    /// per call. Use [`FontCache::with_bundle`] + populate-yourself when
    /// you want to manage the bundle Arc explicitly.
    pub fn new(assets: &AssetStore) -> Self {
        Self::new_with_bundle(assets, font_bundle())
    }

    /// Variant of [`FontCache::new`] that lets the caller supply its own
    /// shared [`FontBundle`] (useful for tests and bench workers that
    /// hold one bundle across thousands of docs).
    pub fn new_with_bundle(assets: &AssetStore, bundle: Arc<FontBundle>) -> Self {
        let mut ttf_fonts = HashMap::new();
        let mut cff_fonts = HashMap::new();
        let mut type1_fonts = HashMap::new();
        let mut type3_glyphs: HashMap<String, HashMap<char, GlyphOutline>> = HashMap::new();
        let mut encoding_maps: HashMap<String, HashMap<u8, String>> = HashMap::new();
        let mut cid_widths: HashMap<String, (u32, HashMap<u32, u16>)> = HashMap::new();

        for font in assets.fonts() {
            // Store encoding map if present (for by-code glyph lookup).
            if let Some(ref enc) = font.encoding_map {
                encoding_maps.insert(font.name.clone(), enc.iter().cloned().collect());
            }
            // Store parsed /W table for composite (Type0) fonts. Widths are
            // kept in the PDF's glyph-space unit (1/1000 em) and converted
            // to the embedded font's UPM on lookup in advance_width_by_gid.
            if let Some((dw, entries)) = &font.cid_widths {
                let mut map: HashMap<u32, u16> = HashMap::with_capacity(entries.len());
                for (cid, w) in entries {
                    // Clamp to u16 (glyph-space widths almost never exceed
                    // 3000 but the PDF format allows negative/huge values).
                    let w_i = w.round().clamp(0.0, u16::MAX as f64) as u16;
                    map.insert(*cid, w_i);
                }
                cid_widths.insert(font.name.clone(), (*dw, map));
            }
            match font.program_type {
                FontProgramType::TrueType => {
                    if let Ok(ttf) = TrueTypeFont::from_bytes(&font.data) {
                        ttf_fonts.insert(font.name.clone(), ttf);
                    }
                }
                FontProgramType::Cff => {
                    if let Ok(cff) = CffFont::from_bytes(&font.data) {
                        cff_fonts.insert(font.name.clone(), cff);
                    }
                }
                FontProgramType::Type1 => {
                    if let Ok(t1) = Type1Font::from_bytes(&font.data) {
                        // Merge the Type1 font's built-in encoding into the
                        // encoding map. The builtin provides the authoritative
                        // byte-to-glyph-name mapping from the font program's
                        // /Encoding array. The PDF-level encoding_map (from
                        // encoding_glyph_names) may be incomplete because the
                        // AGL roundtrip can't resolve all glyph names (e.g.,
                        // math symbols like "similarequal", "negationslash").
                        if let Some(builtin) = t1.builtin_encoding() {
                            let map = encoding_maps.entry(font.name.clone()).or_default();
                            for (code, name) in builtin {
                                map.entry(*code).or_insert_with(|| name.clone());
                            }
                        }
                        type1_fonts.insert(font.name.clone(), t1);
                    }
                }
                FontProgramType::Type3 => {
                    // Asset name: "type3:{font_name}:U+{hex_codepoint}"
                    if let Some((font_name, ch)) = parse_type3_asset_name(&font.name) {
                        if let Some(outline) = type3_outline::deserialize_outline(&font.data) {
                            type3_glyphs
                                .entry(font_name)
                                .or_default()
                                .insert(ch, outline);
                        }
                    }
                }
            }
        }

        Self {
            bundle,
            ttf_fonts,
            cff_fonts,
            type1_fonts,
            encoding_maps,
            cid_widths,
            type3_glyphs,
            hinting_cache: HashMap::new(),
            hinted_glyph_cache: HashMap::new(),
            outline_cache: HashMap::new(),
            auto_hint_metrics_cache: HashMap::new(),
            sink: None,
            missing_glyph_seen: HashSet::new(),
        }
    }

    /// Attach a diagnostics sink so the cache can report glyph lookups
    /// that exhaust the fallback chain (see `kind::MISSING_GLYPH`).
    ///
    /// Without a sink the cache stays silent and only the final `None`
    /// return from `glyph_outline` signals the miss. With a
    /// sink, the first occurrence of each `(font, codepoint)` pair emits
    /// a structured [`Warning`] carrying a [`MissingGlyphInfo`] payload;
    /// subsequent lookups for the same pair are deduped silently.
    pub fn set_sink(&mut self, sink: Arc<dyn DiagnosticsSink>) {
        self.sink = Some(sink);
    }

    /// Probe the glyph-lookup path for `(font_name, ch)` and discard the
    /// result. Intended for audit tooling (`udoc audit-fonts`) that
    /// wants to drive missing-glyph diagnostics into a sink without
    /// rendering. The public-facing entry point; internal renderer code
    /// continues to call the private `glyph_outline` so it can consume
    /// the `GlyphOutline`.
    ///
    /// Returns whether the lookup succeeded, in case callers want to
    /// count hits/misses without round-tripping through the sink.
    pub fn probe_glyph(&mut self, font_name: &str, ch: char) -> bool {
        self.glyph_outline(font_name, ch).is_some()
    }

    /// Pick the right fallback font based on the requested font name.
    /// Serif names (Times, Roman, Georgia, Palatino, Garamond, etc.) get
    /// Liberation Serif; everything else gets Liberation Sans.
    ///
    /// Subset prefix (e.g. "ABCDEF+Times-Roman") is stripped before matching
    /// so this path agrees with `route_tier1`. Without stripping, a hostile
    /// prefix like "TIMES12+UnknownFont" would route to Serif via the raw
    /// substring match even though the real face is unknown. See issue #204.
    fn fallback_for(&self, font_name: &str) -> &TrueTypeFont {
        let stripped = strip_subset_prefix(font_name);
        let lower = stripped.to_ascii_lowercase();
        if lower.contains("times")
            || lower.contains("roman")
            || lower.contains("serif")
            || lower.contains("palatino")
            || lower.contains("garamond")
            || lower.contains("georgia")
            || lower.contains("cambria")
            || lower.contains("bookman")
            || lower.contains("century")
            || lower.contains("nimbus")
        {
            &self.bundle.fallback_serif
        } else {
            &self.bundle.fallback_sans
        }
    }

    /// Route a font name to a specific Tier 1 bundled face based on
    /// name-prefix heuristics. Returns `None` when no specific match fires
    /// and the caller should defer to `fallback_for` (generic serif/sans).
    ///
    /// Subset prefix (e.g. "ABCDEF+CMR10") is stripped before matching.
    /// Comparison is case-insensitive.
    fn route_tier1(&self, font_name: &str) -> Option<Tier1Target> {
        let stripped = strip_subset_prefix(font_name);
        let lower = stripped.to_ascii_lowercase();

        // LaTeX Computer Modern Math and symbol families.
        // CMMI = math italic, CMSY = math symbols, CMEX = math extension,
        // MSAM/MSBM = AMS math symbols. EUEX/EUFM/EUFB/EUSM/EUSB/EURM/EURB
        // = Euler families (Fraktur, Script, Roman) from the AMS eufrak /
        // eucal / eurm packages. CMBSY = CM Bold Symbol. MNSYMBOL/MNSYMBOLA/
        // STMARY/FOURIER-MATH = other math packages. All route to LM Math.
        if lower.starts_with("cmmi")
            || lower.starts_with("cmsy")
            || lower.starts_with("cmbsy")
            || lower.starts_with("cmex")
            || lower.starts_with("msam")
            || lower.starts_with("msbm")
            || lower.starts_with("lmmath")
            || lower.starts_with("latinmodernmath")
            || lower.contains("mathematical")
            || lower.starts_with("stix")
            || lower.starts_with("xits")
            || lower.starts_with("asanamath")
            || lower.contains("cambria math")
            || lower.starts_with("euex")
            || lower.starts_with("eufm")
            || lower.starts_with("eufb")
            || lower.starts_with("eusm")
            || lower.starts_with("eusb")
            || lower.starts_with("eurm")
            || lower.starts_with("eurb")
            || lower.starts_with("mnsymbol")
            || lower.starts_with("stmary")
            || lower.starts_with("wasy")
            || lower.starts_with("pzdr")
        {
            return Some(Tier1Target::LmMath);
        }

        // LaTeX Computer Modern Roman family. CMR/CMBX/CMTI/CMSL are
        // Regular/Bold/Italic/Slanted. LMRoman and LMSerif are the Unicode
        // ports. Detect italic/slanted via suffix.
        if lower.starts_with("cmti")
            || lower.starts_with("cmsl")
            || (lower.starts_with("lmroman") && lower.contains("italic"))
            || (lower.starts_with("lmserif") && lower.contains("italic"))
        {
            return Some(Tier1Target::LmRomanItalic);
        }
        if lower.starts_with("cmr")
            || lower.starts_with("cmbx")
            || lower.starts_with("cmb")
            || lower.starts_with("cmdunh")
            || lower.starts_with("cmfib")
            || lower.starts_with("cmu")
            || lower.starts_with("cmcsc")
            || lower.starts_with("lmroman")
            || lower.starts_with("lmserif")
            || lower.starts_with("latinmodernroman")
            || lower.starts_with("sfrm")
        {
            return Some(Tier1Target::LmRoman);
        }

        // LaTeX Computer Modern monospace and sans families.
        if lower.starts_with("cmtt")
            || lower.starts_with("lmmono")
            || lower.starts_with("lmtypewriter")
        {
            return Some(Tier1Target::Mono);
        }

        // Courier / other monospace. Includes TeX Gyre Cursor (Courier clone),
        // NimbusMonL / NimbusMono (URW++ Courier), Liberation Mono, FreeMono.
        if lower.starts_with("courier")
            || lower.starts_with("consolas")
            || lower.starts_with("monaco")
            || lower.starts_with("menlo")
            || lower.starts_with("texgyrecursor")
            || lower.starts_with("nimbusmonl")
            || lower.starts_with("nimbusmono")
            || lower.starts_with("liberationmono")
            || lower.starts_with("freemono")
            || lower.starts_with("sourcecodepro")
            || lower.starts_with("inconsolata")
        {
            // Courier-Bold etc. fall through to Mono Regular; Mono Bold is
            // not in Tier 1 yet (follow-up #193).
            return Some(Tier1Target::Mono);
        }

        // Helvetica/Arial family — detect weight suffix. Includes TeX Gyre
        // Heros (Helvetica clone), Nimbus Sans / Tahoma / Segoe / Calibri as
        // sans-serif aliases. LaTeX sfsl/sfbf are bold-italic / bold sans.
        let is_sans_family = lower.starts_with("helvetica")
            || lower.starts_with("arial")
            || lower.starts_with("verdana")
            || lower.starts_with("tahoma")
            || lower.starts_with("calibri")
            || lower.starts_with("segoeui")
            || lower.starts_with("lucida sans")
            || lower.starts_with("lucidasans")
            || lower.starts_with("trebuchet")
            || lower.starts_with("lmsans")
            || lower.starts_with("cmss")
            || lower.starts_with("cmssbx")
            || lower.starts_with("texgyreheros")
            || lower.starts_with("nimbussan")
            || lower.starts_with("nimbusl")
            || lower.starts_with("liberationsans");
        if is_sans_family {
            let is_bold = lower.contains("-bold") || lower.contains("bold");
            let is_italic = lower.contains("italic") || lower.contains("oblique");
            return Some(match (is_bold, is_italic) {
                (true, true) => Tier1Target::SansBoldItalic,
                (true, false) => Tier1Target::SansBold,
                (false, true) => Tier1Target::SansItalic,
                (false, false) => Tier1Target::SansRegular,
            });
        }

        // Times/Serif family — pick the matching Tier 1 weight (T1-SERIF #193).
        // `is_bold`/`is_italic` are purely name-based so a PostScript face
        // like "Times-BoldItalic" or "TimesNewRoman-Bold" routes to the
        // corresponding face. Upstream callers with an italic-angle signal
        // on the parsed face can still apply synthetic italic if the name
        // alone doesn't disambiguate.
        // Serif family detection. Includes TeX Gyre Termes (Times clone),
        // Pagella (Palatino clone), Schola (Century Schoolbook clone), and
        // Bonum (Bookman clone). Also MinionPro, Frutiger, GillSans-like
        // variants that should fall under serif fallback for weight-match.
        let is_serif_family = lower.starts_with("times")
            || lower.starts_with("nimbusromno9l")
            || lower.starts_with("nimbusroman")
            || lower.starts_with("nimbusromno")
            || lower.starts_with("timesnewroman")
            || lower.starts_with("garamond")
            || lower.starts_with("bookman")
            || lower.starts_with("palatino")
            || lower.starts_with("georgia")
            || lower.starts_with("century")
            || lower.starts_with("texgyretermes")
            || lower.starts_with("texgyrepagella")
            || lower.starts_with("texgyreschola")
            || lower.starts_with("texgyrebonum")
            || lower.starts_with("texgyretermesx")
            || lower.starts_with("liberationserif")
            || lower.starts_with("minionpro")
            || lower.starts_with("minion-")
            || lower.starts_with("minion_")
            || lower.starts_with("giovanni");
        if is_serif_family {
            let is_bold = lower.contains("bold");
            let is_italic = lower.contains("italic")
                || lower.contains("oblique")
                // Adobe short suffixes like "MinionPro-It" / "MinionPro-BoldIt"
                // use "-It"/"It$" for italic. Only match explicit suffix forms
                // to avoid false positives on "Initial" or similar spellings.
                || lower.ends_with("-it")
                || lower.ends_with("it")
                    && (lower.ends_with("boldit") || lower.ends_with("-it"));
            return Some(match (is_bold, is_italic) {
                (true, true) => Tier1Target::SerifBoldItalic,
                (true, false) => Tier1Target::SerifBold,
                (false, true) => Tier1Target::SerifItalic,
                (false, false) => Tier1Target::SerifRegular,
            });
        }

        // Arabic-script families. PDF authors that embed
        // an Arabic-only face usually name it explicitly. Pick weight from
        // the suffix; italic Noto Sans Arabic is not bundled (the face has
        // no italic counterpart upstream).
        let is_arabic_family = lower.starts_with("notosansarabic")
            || lower.starts_with("notonaskh")
            || lower.starts_with("notokufi")
            || lower.starts_with("arabictypesetting")
            || lower.starts_with("traditionalarabic")
            || lower.starts_with("simplifiedarabic")
            || lower.starts_with("scheherazade")
            || lower.starts_with("amiri")
            || lower.starts_with("lateef");
        if is_arabic_family {
            let is_bold = lower.contains("bold") || lower.contains("black");
            return Some(if is_bold {
                Tier1Target::ArabicBold
            } else {
                Tier1Target::ArabicRegular
            });
        }

        None
    }

    /// Returns the Tier 1 target best suited to render a specific Unicode
    /// character that the named font can't. Used as a secondary fallback
    /// after `route_tier1` when the routed-by-name face also lacks the
    /// glyph, or when a plain Liberation fallback wouldn't have it.
    ///
    /// Routes per-glyph based on Unicode block: math operator / symbol
    /// ranges go to LM Math regardless of the font name. CJK ranges are
    /// handled by the existing `fallback_cjk` path and not routed here.
    fn route_by_unicode(&self, ch: char) -> Option<Tier1Target> {
        let c = ch as u32;
        // Mathematical Operators, Supplemental Mathematical Operators,
        // Mathematical Alphanumeric Symbols, Miscellaneous Math A/B,
        // and Arrows route to LM Math.
        if (0x2200..=0x22FF).contains(&c)
            || (0x2A00..=0x2AFF).contains(&c)
            || (0x27C0..=0x27EF).contains(&c)
            || (0x2980..=0x29FF).contains(&c)
            || (0x1D400..=0x1D7FF).contains(&c)
            || (0x2190..=0x21FF).contains(&c)
        {
            return Some(Tier1Target::LmMath);
        }
        // Greek block (issue #205). Routes to Liberation Sans, which has
        // reasonable Greek coverage (alpha/beta/gamma/pi and the uppercase
        // counterparts). Documents whose name-routed face already matches
        // a known family will hit `route_tier1` first, so this only kicks
        // in when the source font name doesn't hint (e.g. "F1", "Embedded")
        // and the glyph is Greek. Keeps Greek-heavy documents from
        // defaulting to .notdef while staying neutral on font style.
        //
        // Routing dispatch only -- explicit so a future Tier 2 Greek face
        // (e.g. dedicated polytonic / academic Greek font) plugs in here
        // without changing the call sites in `lookup_outline` /
        // `advance_width_impl`.
        if (0x0370..=0x03FF).contains(&c) {
            return Some(Tier1Target::SansRegular);
        }
        // Arabic blocks: Arabic, Arabic Supplement,
        // Arabic Presentation Forms-A, Arabic Presentation Forms-B.
        // Routes to Noto Sans Arabic when the `tier2-arabic` feature is on.
        // Like the Greek block above, this is the routing layer; the actual
        // outline lookup in `tier1_outline` returns `None` when the feature
        // is disabled and the caller falls back to `fallback_for`. The
        // Liberation Sans/Serif fallbacks have *no* Arabic coverage, so
        // without this routing path Arabic glyphs render as `.notdef`
        // (the boxes that currently sink the IA-spanish-Arabic doc to
        // SSIM ~0.27 on the stratified-100 bench).
        if (0x0600..=0x06FF).contains(&c)
            || (0x0750..=0x077F).contains(&c)
            || (0xFB50..=0xFDFF).contains(&c)
            || (0xFE70..=0xFEFF).contains(&c)
        {
            return Some(Tier1Target::ArabicRegular);
        }
        None
    }

    /// Glyph outline lookup against a specific Tier 1 face.
    ///
    /// Returns `None` if the routed face is disabled via cargo feature
    /// (see [`Tier1Bundle`]). Callers fall through to `fallback_for()`
    /// on `None`, so a SerifBoldItalic lookup in a `--no-default-features`
    /// build still renders through `fallback_serif` (regular weight).
    fn tier1_outline(&self, target: Tier1Target, ch: char) -> Option<GlyphOutline> {
        match target {
            Tier1Target::SansRegular => {
                let gid = self.bundle.fallback_sans.glyph_id(ch)?;
                self.bundle.fallback_sans.glyph_outline(gid)
            }
            Tier1Target::SansBold => {
                let face = self.bundle.tier1.sans_bold.as_ref()?;
                let gid = face.glyph_id(ch)?;
                face.glyph_outline(gid)
            }
            Tier1Target::SansItalic => {
                let face = self.bundle.tier1.sans_italic.as_ref()?;
                let gid = face.glyph_id(ch)?;
                face.glyph_outline(gid)
            }
            Tier1Target::SansBoldItalic => {
                let face = self.bundle.tier1.sans_bold_italic.as_ref()?;
                let gid = face.glyph_id(ch)?;
                face.glyph_outline(gid)
            }
            Tier1Target::SerifRegular => {
                let gid = self.bundle.fallback_serif.glyph_id(ch)?;
                self.bundle.fallback_serif.glyph_outline(gid)
            }
            Tier1Target::SerifBold => {
                let face = self.bundle.tier1.serif_bold.as_ref()?;
                let gid = face.glyph_id(ch)?;
                face.glyph_outline(gid)
            }
            Tier1Target::SerifItalic => {
                let face = self.bundle.tier1.serif_italic.as_ref()?;
                let gid = face.glyph_id(ch)?;
                face.glyph_outline(gid)
            }
            Tier1Target::SerifBoldItalic => {
                let face = self.bundle.tier1.serif_bold_italic.as_ref()?;
                let gid = face.glyph_id(ch)?;
                face.glyph_outline(gid)
            }
            Tier1Target::Mono => {
                let face = self.bundle.tier1.mono.as_ref()?;
                let gid = face.glyph_id(ch)?;
                face.glyph_outline(gid)
            }
            Tier1Target::LmRoman => {
                let face = self.bundle.tier1.lm_roman.as_ref()?;
                let gid = face.glyph_id_for_char(ch)?;
                face.glyph_outline(gid)
            }
            Tier1Target::LmRomanItalic => {
                let face = self.bundle.tier1.lm_roman_italic.as_ref()?;
                let gid = face.glyph_id_for_char(ch)?;
                face.glyph_outline(gid)
            }
            Tier1Target::LmMath => {
                let face = self.bundle.tier1.lm_math.as_ref()?;
                let gid = face.glyph_id_for_char(ch)?;
                face.glyph_outline(gid)
            }
            Tier1Target::ArabicRegular => {
                let face = self.bundle.tier1.arabic_regular.as_ref()?;
                let gid = face.glyph_id(ch)?;
                face.glyph_outline(gid)
            }
            Tier1Target::ArabicBold => {
                let face = self.bundle.tier1.arabic_bold.as_ref()?;
                let gid = face.glyph_id(ch)?;
                face.glyph_outline(gid)
            }
        }
    }

    /// Advance width lookup against a specific Tier 1 face, in font units.
    ///
    /// Returns `None` if the routed face is disabled via cargo feature.
    /// See [`Self::tier1_outline`] for the degradation rules.
    fn tier1_advance(&self, target: Tier1Target, ch: char) -> Option<u16> {
        match target {
            Tier1Target::SansRegular => {
                let gid = self.bundle.fallback_sans.glyph_id(ch)?;
                Some(self.bundle.fallback_sans.advance_width(gid))
            }
            Tier1Target::SansBold => {
                let face = self.bundle.tier1.sans_bold.as_ref()?;
                let gid = face.glyph_id(ch)?;
                Some(face.advance_width(gid))
            }
            Tier1Target::SansItalic => {
                let face = self.bundle.tier1.sans_italic.as_ref()?;
                let gid = face.glyph_id(ch)?;
                Some(face.advance_width(gid))
            }
            Tier1Target::SansBoldItalic => {
                let face = self.bundle.tier1.sans_bold_italic.as_ref()?;
                let gid = face.glyph_id(ch)?;
                Some(face.advance_width(gid))
            }
            Tier1Target::SerifRegular => {
                let gid = self.bundle.fallback_serif.glyph_id(ch)?;
                Some(self.bundle.fallback_serif.advance_width(gid))
            }
            Tier1Target::SerifBold => {
                let face = self.bundle.tier1.serif_bold.as_ref()?;
                let gid = face.glyph_id(ch)?;
                Some(face.advance_width(gid))
            }
            Tier1Target::SerifItalic => {
                let face = self.bundle.tier1.serif_italic.as_ref()?;
                let gid = face.glyph_id(ch)?;
                Some(face.advance_width(gid))
            }
            Tier1Target::SerifBoldItalic => {
                let face = self.bundle.tier1.serif_bold_italic.as_ref()?;
                let gid = face.glyph_id(ch)?;
                Some(face.advance_width(gid))
            }
            Tier1Target::Mono => {
                let face = self.bundle.tier1.mono.as_ref()?;
                let gid = face.glyph_id(ch)?;
                Some(face.advance_width(gid))
            }
            Tier1Target::LmRoman => self
                .bundle
                .tier1
                .lm_roman
                .as_ref()
                .and_then(|f| f.advance_width_for_char(ch)),
            Tier1Target::LmRomanItalic => self
                .bundle
                .tier1
                .lm_roman_italic
                .as_ref()
                .and_then(|f| f.advance_width_for_char(ch)),
            Tier1Target::LmMath => self
                .bundle
                .tier1
                .lm_math
                .as_ref()
                .and_then(|f| f.advance_width_for_char(ch)),
            Tier1Target::ArabicRegular => {
                let face = self.bundle.tier1.arabic_regular.as_ref()?;
                let gid = face.glyph_id(ch)?;
                Some(face.advance_width(gid))
            }
            Tier1Target::ArabicBold => {
                let face = self.bundle.tier1.arabic_bold.as_ref()?;
                let gid = face.glyph_id(ch)?;
                Some(face.advance_width(gid))
            }
        }
    }

    /// Register a TrueType font by raw bytes (test-internals only).
    #[cfg(feature = "test-internals")]
    pub fn register_ttf(&mut self, name: &str, bytes: &[u8]) -> bool {
        match TrueTypeFont::from_bytes(bytes) {
            Ok(ttf) => {
                self.ttf_fonts.insert(name.to_string(), ttf);
                true
            }
            Err(_) => false,
        }
    }

    /// Register a CFF font by raw bytes (test-internals only).
    #[cfg(feature = "test-internals")]
    pub fn register_cff(&mut self, name: &str, bytes: &[u8]) -> bool {
        match CffFont::from_bytes(bytes) {
            Ok(cff) => {
                self.cff_fonts.insert(name.to_string(), cff);
                true
            }
            Err(_) => false,
        }
    }

    /// Register a Type1 font by raw bytes (test-internals only).
    #[cfg(feature = "test-internals")]
    pub fn register_type1(&mut self, name: &str, bytes: &[u8]) -> bool {
        match Type1Font::from_bytes(bytes) {
            Ok(t1) => {
                if let Some(builtin) = t1.builtin_encoding() {
                    let map = self.encoding_maps.entry(name.to_string()).or_default();
                    for (code, gn) in builtin {
                        map.entry(*code).or_insert_with(|| gn.clone());
                    }
                }
                self.type1_fonts.insert(name.to_string(), t1);
                true
            }
            Err(_) => false,
        }
    }

    /// Public access to glyph outline (test-internals only).
    #[cfg(feature = "test-internals")]
    pub fn outline(&mut self, font_name: &str, ch: char) -> Option<GlyphOutline> {
        self.glyph_outline(font_name, ch)
    }

    /// Public access to auto-hinter metrics (test-internals only).
    #[cfg(feature = "test-internals")]
    pub fn metrics(
        &mut self,
        font_name: &str,
    ) -> Option<&super::auto_hinter::metrics::GlobalMetrics> {
        self.auto_hint_metrics(font_name)
    }

    /// Look up a glyph outline for a character in the named font.
    /// Falls back to Liberation Sans if the font is not found or the
    /// character is missing.
    pub(crate) fn glyph_outline(&mut self, font_name: &str, ch: char) -> Option<GlyphOutline> {
        // Check outline cache first (avoids re-interpreting charstrings).
        let cache_key = (font_name.to_string(), ch);
        if let Some(cached) = self.outline_cache.get(&cache_key) {
            return cached.clone();
        }

        let result = self.lookup_outline(font_name, ch);
        if result.is_none() {
            // The named font plus every fallback layer (Tier 1 name-routing,
            // Unicode-range sniff, CJK bundle, generic serif/sans) missed.
            // The final rendered output for this glyph will be .notdef /
            // a replacement box, so surface the miss on the diagnostics
            // sink (#166). Dedup on (font_name, codepoint) so a glyph used
            // on every line doesn't flood the sink.
            self.report_missing_glyph(font_name, ch);
        }
        // Soft cap: flush the whole cache when it grows past CACHE_SOFT_CAP
        // so pathological documents (full Unicode spill, thousands of fonts)
        // can't cause per-doc heap blow-up. Hot entries repopulate on demand.
        if self.outline_cache.len() >= CACHE_SOFT_CAP {
            self.outline_cache = HashMap::new();
        }
        self.outline_cache.insert(cache_key, result.clone());
        result
    }

    /// Emit a [`kind::MISSING_GLYPH`] warning on the attached sink, if any,
    /// and record the `(font, codepoint)` pair in `missing_glyph_seen`.
    /// Looks up the named font's `glyph_id` when possible (a non-zero gid
    /// here means the cmap does map the char but no outline was produced;
    /// zero means the fallback also missed and we're landing on `.notdef`).
    ///
    /// [`kind::MISSING_GLYPH`]: udoc_core::diagnostics::kind::MISSING_GLYPH
    fn report_missing_glyph(&mut self, font_name: &str, ch: char) {
        let sink = match self.sink.as_ref() {
            Some(s) => Arc::clone(s),
            None => return,
        };
        let codepoint = ch as u32;
        let key = (font_name.to_string(), codepoint);
        if !self.missing_glyph_seen.insert(key) {
            return;
        }
        let glyph_id = self.resolve_gid(font_name, ch).unwrap_or(0) as u32;
        sink.warning(Warning::missing_glyph(MissingGlyphInfo {
            font: font_name.to_string(),
            codepoint,
            glyph_id,
        }));
    }

    /// Best-effort glyph-id lookup for the named font only (no fallback).
    /// Used by the missing-glyph reporter so the emitted diagnostic can
    /// distinguish "cmap had the codepoint, outline empty" (non-zero gid)
    /// from "cmap didn't even map the codepoint" (returns `None`).
    fn resolve_gid(&self, font_name: &str, ch: char) -> Option<u16> {
        if let Some(ttf) = self.ttf_fonts.get(font_name) {
            return ttf.glyph_id(ch);
        }
        if let Some(cff) = self.cff_fonts.get(font_name) {
            return cff.glyph_id_for_char(ch);
        }
        None
    }

    /// Internal outline lookup without caching.
    fn lookup_outline(&self, font_name: &str, ch: char) -> Option<GlyphOutline> {
        // Check Type3 font outlines first.
        if let Some(glyphs) = self.type3_glyphs.get(font_name) {
            if let Some(outline) = glyphs.get(&ch) {
                return Some(outline.clone());
            }
        }

        // Try the named font first.
        if let Some(ttf) = self.ttf_fonts.get(font_name) {
            if let Some(gid) = ttf.glyph_id(ch) {
                if let Some(outline) = ttf.glyph_outline(gid) {
                    return Some(outline);
                }
            }
        }
        if let Some(cff) = self.cff_fonts.get(font_name) {
            if let Some(gid) = cff.glyph_id_for_char(ch) {
                if let Some(outline) = cff.glyph_outline(gid) {
                    return Some(outline);
                }
            }
        }
        if let Some(t1) = self.type1_fonts.get(font_name) {
            if let Some(outline) = t1.glyph_outline(ch) {
                return Some(outline);
            }
        }

        // Tier 1 name-aware routing: CMR* -> LMR, CMMI* -> LM Math,
        // Helvetica-Bold -> Liberation Sans Bold, etc. Takes precedence
        // over the generic serif/sans bucket so the routed face is used
        // even when the coarse bucket would have picked the other one.
        if let Some(target) = self.route_tier1(font_name) {
            if let Some(outline) = self.tier1_outline(target, ch) {
                return Some(outline);
            }
        }

        // Unicode-range sniff: math operators and Mathematical Alphanumeric
        // Symbols route to LM Math per-glyph, regardless of the source
        // font's name (so a Times-Roman document rendering `∀` uses LM Math
        // for just that glyph).
        if let Some(target) = self.route_by_unicode(ch) {
            if let Some(outline) = self.tier1_outline(target, ch) {
                return Some(outline);
            }
        }

        // Fallback: Liberation Sans or Serif based on font name.
        let fb = self.fallback_for(font_name);
        if let Some(gid) = fb.glyph_id(ch) {
            return fb.glyph_outline(gid);
        }

        // CJK fallback: Noto Sans CJK for CJK characters.
        // Uses internal GID from the OTF cmap, bypassing CID-to-GID mapping.
        if let Some(ref cjk) = self.bundle.fallback_cjk {
            if let Some(&gid) = self.bundle.cjk_cmap.get(&(ch as u32)) {
                if let Some(outline) = cjk.glyph_outline_by_internal_gid(gid) {
                    return Some(outline);
                }
            }
        }

        None
    }

    /// Get the advance width for a character in font units.
    #[cfg(not(feature = "test-internals"))]
    pub(crate) fn advance_width(&self, font_name: &str, ch: char) -> u16 {
        self.advance_width_impl(font_name, ch)
    }

    /// Get the advance width for a character in font units (test-internals).
    #[cfg(feature = "test-internals")]
    pub fn advance_width(&self, font_name: &str, ch: char) -> u16 {
        self.advance_width_impl(font_name, ch)
    }

    /// Advance width for a CID/composite font indexed by raw glyph ID,
    /// returned in the font's own UPM units so callers can treat the
    /// result identically to [`FontCache::advance_width`].
    ///
    /// Prefers the PDF's parsed `/W` entry (authoritative per PDF spec 9.7.4)
    /// over the embedded font's `hmtx`. `/W` values are stored in 1/1000 em
    /// and scaled up to the embedded font's UPM here. Returns the default
    /// `/DW` width when the CID is absent from `/W`, or the embedded
    /// `hmtx` advance when no `/W` data is available for the font at all.
    /// `None` only when the named font is entirely unknown -- callers can
    /// fall back to [`FontCache::advance_width`].
    ///
    /// This is the correct entry point for renderers iterating CID glyphs
    /// via `char_gids` (Identity-H / Identity-V encodings). Using the
    /// char-indexed `advance_width` for CID text re-does the ToUnicode
    /// decode and can miss `/W` overrides when the embedded `hmtx`
    /// disagrees (MS Word export, issue #182).
    pub fn advance_width_by_gid(&self, font_name: &str, gid: u16) -> Option<u16> {
        // 1. Parsed /W table (authoritative for Type0 fonts). PDF /W values
        //    are in 1/1000 em; scale up to the embedded font's UPM so the
        //    result is comparable with hmtx-sourced widths.
        if let Some((dw, map)) = self.cid_widths.get(font_name) {
            let upm = self.units_per_em(font_name) as u32;
            let w_pdf = map.get(&(gid as u32)).map(|&w| w as u32).unwrap_or(*dw);
            let w_font = w_pdf.saturating_mul(upm) / 1000;
            return Some(w_font.min(u16::MAX as u32) as u16);
        }
        // 2. Embedded TrueType hmtx / CFF charstrings.
        if let Some(ttf) = self.ttf_fonts.get(font_name) {
            return Some(ttf.advance_width(gid));
        }
        if let Some(cff) = self.cff_fonts.get(font_name) {
            return cff.advance_width(gid);
        }
        None
    }

    fn advance_width_impl(&self, font_name: &str, ch: char) -> u16 {
        if let Some(ttf) = self.ttf_fonts.get(font_name) {
            if let Some(gid) = ttf.glyph_id(ch) {
                return ttf.advance_width(gid);
            }
        }
        if let Some(cff) = self.cff_fonts.get(font_name) {
            if let Some(w) = cff.advance_width_for_char(ch) {
                return w;
            }
        }
        if let Some(t1) = self.type1_fonts.get(font_name) {
            if let Some(w) = t1.advance_width(ch) {
                return w;
            }
        }
        // Tier 1 name-aware routing and Unicode-range sniffing, consistent
        // with the glyph-outline lookup above.
        if let Some(target) = self.route_tier1(font_name) {
            if let Some(w) = self.tier1_advance(target, ch) {
                return w;
            }
        }
        if let Some(target) = self.route_by_unicode(ch) {
            if let Some(w) = self.tier1_advance(target, ch) {
                return w;
            }
        }
        // Fallback advance width.
        let fb = self.fallback_for(font_name);
        if let Some(gid) = fb.glyph_id(ch) {
            return fb.advance_width(gid);
        }
        // Last resort: 60% of em.
        600
    }

    /// Get units per em for the named font (or fallback).
    pub fn units_per_em(&self, font_name: &str) -> u16 {
        if let Some(ttf) = self.ttf_fonts.get(font_name) {
            return ttf.units_per_em();
        }
        if self.cff_fonts.contains_key(font_name) {
            return 1000; // CFF convention (same as Type1)
        }
        if self.type1_fonts.contains_key(font_name) {
            return 1000; // Type1 convention
        }
        if self.type3_glyphs.contains_key(font_name) {
            return 1000; // Type3 outlines scaled to 1000 UPM in type3_outline.rs
        }
        self.fallback_for(font_name).units_per_em()
    }

    /// Look up a glyph outline by raw glyph ID.
    /// Used for CID/composite fonts where the char code IS the GID.
    pub(crate) fn glyph_outline_by_gid(&self, font_name: &str, gid: u16) -> Option<GlyphOutline> {
        if let Some(cff) = self.cff_fonts.get(font_name) {
            if let Some(outline) = cff.glyph_outline_by_gid(gid) {
                return Some(outline);
            }
        }
        if let Some(ttf) = self.ttf_fonts.get(font_name) {
            if let Some(outline) = ttf.glyph_outline(gid) {
                return Some(outline);
            }
        }
        None
    }

    /// Look up a glyph outline with fallback-font substitution.
    ///
    /// Unlike `glyph_outline_by_gid`, this falls through to the
    /// serif/sans Liberation fallback when the named font is absent (the
    /// common case for PDFs that reference standard 14 fonts without
    /// embedding them). Intended for debug tooling where "show something
    /// sensible" beats "refuse to render".
    pub fn glyph_outline_by_gid_with_fallback(
        &self,
        font_name: &str,
        gid: u16,
    ) -> Option<GlyphOutline> {
        if let Some(o) = self.glyph_outline_by_gid(font_name, gid) {
            return Some(o);
        }
        self.fallback_for(font_name).glyph_outline(gid)
    }

    /// Units-per-em for a font, accounting for fallback substitution.
    pub fn units_per_em_with_fallback(&self, font_name: &str) -> u16 {
        self.units_per_em(font_name)
    }

    /// Whether a font is backed by a real embedded program (as opposed to
    /// a fallback substitution). Inspect tooling uses this to surface the
    /// situation in JSON output.
    pub fn has_font(&self, font_name: &str) -> bool {
        self.ttf_fonts.contains_key(font_name)
            || self.cff_fonts.contains_key(font_name)
            || self.type1_fonts.contains_key(font_name)
    }

    /// Look up a glyph outline by original character code.
    /// Uses the PDF encoding map (byte -> glyph name) to find the correct
    /// charstring in subset CFF/Type1 fonts. Results are cached.
    pub(crate) fn glyph_outline_by_code(
        &mut self,
        font_name: &str,
        code: u8,
    ) -> Option<GlyphOutline> {
        // Use PUA char to cache by-code lookups without colliding with Unicode chars.
        let cache_char = char::from_u32(0xF0000 + code as u32).unwrap_or('\u{FFFD}');
        let cache_key = (font_name.to_string(), cache_char);
        if let Some(cached) = self.outline_cache.get(&cache_key) {
            return cached.clone();
        }

        let result = self.lookup_outline_by_code(font_name, code);
        if self.outline_cache.len() >= CACHE_SOFT_CAP {
            self.outline_cache = HashMap::new();
        }
        self.outline_cache.insert(cache_key, result.clone());
        result
    }

    fn lookup_outline_by_code(&self, font_name: &str, code: u8) -> Option<GlyphOutline> {
        let glyph_name = self.encoding_maps.get(font_name)?.get(&code)?;

        if let Some(cff) = self.cff_fonts.get(font_name) {
            if let Some(outline) = cff.glyph_outline_by_name(glyph_name) {
                return Some(outline);
            }
        }
        if let Some(t1) = self.type1_fonts.get(font_name) {
            if let Some(outline) = t1.glyph_outline_by_name(glyph_name) {
                return Some(outline);
            }
        }

        None
    }

    /// Attempt to hint a TrueType glyph at the given ppem.
    /// Returns hinted glyph data if the font has hinting instructions.
    /// Returns None for non-TrueType fonts or fonts without hinting.
    pub(crate) fn hint_glyph(
        &mut self,
        font_name: &str,
        glyph_id: u16,
        ppem: u16,
    ) -> Option<&HintedGlyph> {
        // Check glyph cache first.
        let glyph_key = (font_name.to_string(), glyph_id, ppem);
        if self.hinted_glyph_cache.contains_key(&glyph_key) {
            return self.hinted_glyph_cache.get(&glyph_key);
        }

        // Need a TrueType font with hinting.
        let ttf = self.ttf_fonts.get(font_name)?;
        if !ttf.has_hinting() {
            return None;
        }

        // Get raw glyph data (points + instructions).
        let raw = ttf.glyph_raw_data(glyph_id)?;
        if raw.instructions.is_empty() && raw.points.is_empty() {
            return None;
        }

        // Get or create HintingState for this (font, ppem).
        let hint_key = (font_name.to_string(), ppem);
        if !self.hinting_cache.contains_key(&hint_key) {
            let state = HintingState::new(
                ttf.fpgm_data(),
                ttf.prep_data(),
                &ttf.cvt_values(),
                ttf.hinting_limits(),
            )
            .ok()?;
            if self.hinting_cache.len() >= CACHE_SOFT_CAP {
                self.hinting_cache = HashMap::new();
            }
            self.hinting_cache.insert(hint_key.clone(), state);
        }

        let state = self.hinting_cache.get_mut(&hint_key)?;
        state.prepare_size(ppem, ttf.units_per_em()).ok()?;

        // Hint the glyph.
        let hinted = state.hint_glyph(&raw).ok()?;
        if self.hinted_glyph_cache.len() >= CACHE_SOFT_CAP {
            self.hinted_glyph_cache = HashMap::new();
        }
        self.hinted_glyph_cache.insert(glyph_key.clone(), hinted);
        self.hinted_glyph_cache.get(&glyph_key)
    }

    /// Get PS hint values (blue zones, standard stems) for Type1 or CFF fonts.
    pub(crate) fn ps_hint_values(
        &self,
        font_name: &str,
    ) -> Option<udoc_font::type1::Type1HintValues> {
        // Try Type1 first.
        if let Some(t1) = self.type1_fonts.get(font_name) {
            let hv = t1.hint_values();
            if !hv.blue_values.is_empty() || hv.std_hw > 0.0 || hv.std_vw > 0.0 {
                return Some(hv.clone());
            }
        }
        // Try CFF.
        if let Some(cff) = self.cff_fonts.get(font_name) {
            return cff.ps_hint_values();
        }
        None
    }

    /// Get auto-hinter global metrics for a font, computing lazily on first access.
    ///
    /// Analyzes reference glyphs to detect blue zones and standard stem widths.
    /// Returns None for fonts where analysis produces no useful metrics.
    pub(crate) fn auto_hint_metrics(
        &mut self,
        font_name: &str,
    ) -> Option<&super::auto_hinter::metrics::GlobalMetrics> {
        let key = font_name.to_string();
        if !self.auto_hint_metrics_cache.contains_key(&key) {
            let metrics = super::auto_hinter::metrics::compute_global_metrics(self, font_name);
            self.auto_hint_metrics_cache.insert(key.clone(), metrics);
        }
        self.auto_hint_metrics_cache
            .get(&key)
            .and_then(|o| o.as_ref())
    }

    /// Get the TrueType glyph ID for a Unicode character, if the font is TrueType.
    pub(crate) fn ttf_glyph_id(&self, font_name: &str, ch: char) -> Option<u16> {
        self.ttf_fonts.get(font_name)?.glyph_id(ch)
    }

    /// Whether the named TrueType font has hinting tables (fpgm/prep/cvt).
    /// Used by the render path as a proxy for "well-formed subset with
    /// trustworthy byte cmap": macOS Quartz preserves hinting when it
    /// subsets a font (10251), so byte-lookup returns the intended glyph.
    /// Ad-hoc subsets that drop hinting (13201) also tend to populate the
    /// built-in cmap(1,0) arbitrarily, making byte-lookup unsafe.
    pub(crate) fn ttf_has_hinting(&self, font_name: &str) -> bool {
        self.ttf_fonts
            .get(font_name)
            .is_some_and(|ttf| ttf.has_hinting())
    }

    /// Get the TrueType glyph ID for a raw byte code (simple TT fonts).
    ///
    /// PDF TrueType simple fonts without an explicit `/Encoding` entry use
    /// the font's built-in cmap (cmap(1,0) or cmap(3,0)) indexed by the
    /// content stream byte. This bypasses ToUnicode, which is crucial for
    /// subsets whose ToUnicode remaps ligature glyphs to ASCII-looking
    /// Unicode (e.g. the "ti" ligature glyph mapped to U+2019).
    pub(crate) fn ttf_glyph_id_by_byte(&self, font_name: &str, byte: u8) -> Option<u16> {
        self.ttf_fonts.get(font_name)?.glyph_id_by_byte(byte)
    }

    /// Resolve a Unicode character to a glyph ID, consulting the fallback
    /// TrueType font when the named font is absent. Intended for inspect
    /// tooling; the render pipeline has its own by-code resolution path.
    pub fn glyph_id_with_fallback(&self, font_name: &str, ch: char) -> Option<u16> {
        if let Some(ttf) = self.ttf_fonts.get(font_name) {
            if let Some(g) = ttf.glyph_id(ch) {
                return Some(g);
            }
        }
        if let Some(cff) = self.cff_fonts.get(font_name) {
            if let Some(g) = cff.glyph_id_for_char(ch) {
                return Some(g);
            }
        }
        self.fallback_for(font_name).glyph_id(ch)
    }

    /// Whether the cache has any embedded fonts (beyond the fallback).
    pub fn has_embedded_fonts(&self) -> bool {
        !self.ttf_fonts.is_empty() || !self.cff_fonts.is_empty() || !self.type1_fonts.is_empty()
    }

    /// Release all per-document glyph outline and hinting caches
    /// (T60-MEMBATCH).
    ///
    /// Pinned resources are retained: Tier 1 bundle, fallback sans/serif
    /// faces, CJK bundle, embedded font programs (`ttf_fonts`, `cff_fonts`,
    /// `type1_fonts`, `type3_glyphs`), encoding / CID width maps, and the
    /// missing-glyph dedup set. The transient hash maps built up as the
    /// renderer walks glyphs (outline_cache, hinted_glyph_cache,
    /// hinting_cache, auto_hint_metrics_cache) are dropped so the allocator
    /// can reclaim pages between documents.
    ///
    /// Safe to call between any two render operations. The next glyph
    /// lookup on the same (font, char) will be a miss and re-populate the
    /// cache at its normal per-glyph cost.
    pub fn reset_document_scoped(&mut self) {
        self.outline_cache = HashMap::new();
        self.hinting_cache = HashMap::new();
        self.hinted_glyph_cache = HashMap::new();
        self.auto_hint_metrics_cache = HashMap::new();
        // Keep `missing_glyph_seen` -- it's a dedup set for diagnostics,
        // not a perf cache, and clearing it would flood sinks on the
        // next render of a doc that uses the same missing-glyph font.
    }

    /// Number of entries currently retained in the per-document caches
    /// (T60-MEMBATCH). Exposed for tests + operator telemetry; returns
    /// `(outline, hinting_state, hinted_glyph)`. Counts are exact; size
    /// in bytes is workload-dependent and not reported here.
    pub fn cache_sizes(&self) -> (usize, usize, usize) {
        (
            self.outline_cache.len(),
            self.hinting_cache.len(),
            self.hinted_glyph_cache.len(),
        )
    }
}

/// Parse the CJK font bundle: [cff_len:u32LE][cff_data][num:u32LE][(cp:u32LE, gid:u16LE)...]
/// Load the CJK fallback bundle if the `cjk-fonts` feature is on.
///
/// Returns `(None, empty)` when the feature is disabled, in which case the
/// CJK fallback path in `glyph_outline` / `advance_width_impl` is a no-op
/// and CJK characters render as `.notdef` unless the document already
/// embeds its own CJK font.
#[cfg(feature = "cjk-fonts")]
fn load_cjk_fallback() -> (Option<CffFont>, HashMap<u32, u16>) {
    parse_cjk_bundle(NOTO_SANS_CJK)
}

#[cfg(not(feature = "cjk-fonts"))]
fn load_cjk_fallback() -> (Option<CffFont>, HashMap<u32, u16>) {
    (None, HashMap::new())
}

#[cfg(feature = "cjk-fonts")]
fn parse_cjk_bundle(data: &[u8]) -> (Option<CffFont>, HashMap<u32, u16>) {
    if data.len() < 8 {
        return (None, HashMap::new());
    }
    let cff_len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    if 4 + cff_len + 4 > data.len() {
        return (None, HashMap::new());
    }
    let cff_data = &data[4..4 + cff_len];
    let cmap_start = 4 + cff_len;
    let num_entries = u32::from_le_bytes([
        data[cmap_start],
        data[cmap_start + 1],
        data[cmap_start + 2],
        data[cmap_start + 3],
    ]) as usize;

    let mut cmap = HashMap::with_capacity(num_entries);
    let entries_start = cmap_start + 4;
    for i in 0..num_entries {
        let off = entries_start + i * 6;
        if off + 6 > data.len() {
            break;
        }
        let cp = u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
        let gid = u16::from_le_bytes([data[off + 4], data[off + 5]]);
        cmap.insert(cp, gid);
    }

    let cff = CffFont::from_bytes(cff_data).ok();
    (cff, cmap)
}

/// Parse a Type3 asset name "type3:{font_name}:U+{hex}" into (font_name, char).
fn parse_type3_asset_name(name: &str) -> Option<(String, char)> {
    let rest = name.strip_prefix("type3:")?;
    let colon_pos = rest.rfind(":U+")?;
    let font_name = &rest[..colon_pos];
    let hex_str = &rest[colon_pos + 3..];
    let codepoint = u32::from_str_radix(hex_str, 16).ok()?;
    let ch = char::from_u32(codepoint)?;
    Some((font_name.to_string(), ch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_fonts_load() {
        let cache = FontCache::empty();
        assert!(cache.bundle.fallback_sans.units_per_em() > 0);
        assert!(cache.bundle.fallback_serif.units_per_em() > 0);
    }

    #[test]
    fn font_bundle_is_shared_across_caches() {
        // The OnceLock guarantees a single FontBundle instance for the
        // process lifetime; two FontCaches share the same Arc.
        let a = FontCache::empty();
        let b = FontCache::empty();
        assert!(
            Arc::ptr_eq(&a.bundle, &b.bundle),
            "two FontCache::empty() calls should share the same FontBundle Arc",
        );
    }

    #[test]
    fn missing_glyph_emits_warning_once() {
        // Use a Private Use Area codepoint: Liberation Sans / Serif and
        // every Tier 1 face lack coverage for PUA, so the fallback chain
        // in `lookup_outline` exhausts and `glyph_outline` returns None.
        // With a sink attached, the first lookup should emit a structured
        // MissingGlyph warning; the second lookup for the same pair is
        // deduped and must not re-emit.
        use udoc_core::diagnostics::{kind, CollectingDiagnostics};

        let pua: char = '\u{E000}';
        let sink = Arc::new(CollectingDiagnostics::new());
        let mut cache = FontCache::empty();
        cache.set_sink(sink.clone());

        // Precondition: confirm we picked a char the fallback chain can't
        // render. A real-world regression that adds PUA glyphs to
        // Liberation would make this assertion fire rather than masking a
        // broken emission path.
        assert!(
            cache.glyph_outline("MysteryFont", pua).is_none(),
            "PUA codepoint must exhaust the fallback chain for this test",
        );

        let warnings = sink.warnings();
        assert_eq!(warnings.len(), 1, "expected exactly one warning emission");
        let w = &warnings[0];
        assert_eq!(w.kind, kind::MISSING_GLYPH);
        let info = w
            .context
            .missing_glyph
            .as_ref()
            .expect("missing_glyph payload must be populated");
        assert_eq!(info.font, "MysteryFont");
        assert_eq!(info.codepoint, pua as u32);

        // Second lookup for the same (font, codepoint) pair must be deduped.
        assert!(cache.glyph_outline("MysteryFont", pua).is_none());
        assert_eq!(
            sink.warnings().len(),
            1,
            "repeat lookups must not re-emit warnings",
        );
    }

    #[test]
    fn fallback_renders_ascii() {
        let mut cache = FontCache::empty();
        let outline = cache.glyph_outline("nonexistent", 'A');
        assert!(
            outline.is_some(),
            "Liberation Sans should have glyph for 'A'"
        );
    }

    #[test]
    fn fallback_advance_width() {
        let cache = FontCache::empty();
        let width = cache.advance_width("nonexistent", 'A');
        assert!(width > 0, "advance width should be positive");
    }

    #[test]
    fn advance_width_by_gid_prefers_cid_widths() {
        use udoc_core::document::assets::{FontAsset, FontProgramType};
        // Build an AssetStore with one TT asset (Liberation Sans bytes) and
        // override GID 5 via /W to an implausible value (50 1/1000em). The
        // CID-widths lookup should win over the embedded hmtx; unrelated
        // GIDs should fall back to the /DW default.
        let mut assets = AssetStore::new();
        assets.add_font(
            FontAsset::new(
                "MyCidFont".to_string(),
                LIBERATION_SANS.to_vec(),
                FontProgramType::TrueType,
            )
            .with_cid_widths(Some((500, vec![(5, 50.0)]))),
        );
        let cache = FontCache::new(&assets);
        let upm = cache.units_per_em("MyCidFont") as u32;
        // /W entry: 50 * upm / 1000 (scaled into font UPM).
        let expected_override = (50u32 * upm / 1000) as u16;
        let expected_dw = (500u32 * upm / 1000) as u16;
        assert_eq!(
            cache.advance_width_by_gid("MyCidFont", 5),
            Some(expected_override),
            "GID 5 should use /W override"
        );
        assert_eq!(
            cache.advance_width_by_gid("MyCidFont", 999),
            Some(expected_dw),
            "unmapped GID should use /DW default"
        );
    }

    #[test]
    fn advance_width_by_gid_falls_through_without_cid_widths() {
        use udoc_core::document::assets::{FontAsset, FontProgramType};
        // No /W data -> fall through to embedded hmtx. Liberation Sans'
        // cmap resolves 'A' to a real glyph with non-zero hmtx width.
        let mut assets = AssetStore::new();
        assets.add_font(FontAsset::new(
            "PlainTtf".to_string(),
            LIBERATION_SANS.to_vec(),
            FontProgramType::TrueType,
        ));
        let cache = FontCache::new(&assets);
        let ttf = cache.ttf_fonts.get("PlainTtf").unwrap();
        let gid_a = ttf.glyph_id('A').unwrap();
        let by_gid = cache.advance_width_by_gid("PlainTtf", gid_a);
        assert!(by_gid.is_some(), "embedded hmtx should satisfy lookup");
        assert!(by_gid.unwrap() > 0, "'A' must have non-zero advance");
    }

    #[test]
    fn advance_width_by_gid_unknown_font_is_none() {
        let cache = FontCache::empty();
        assert_eq!(cache.advance_width_by_gid("ghost", 1), None);
    }

    #[test]
    fn serif_fallback_for_times() {
        let mut cache = FontCache::empty();
        // Times-Roman should use serif fallback, not sans.
        let outline_serif = cache.glyph_outline("Times-Roman", 'A');
        let outline_sans = cache.glyph_outline("Helvetica", 'A');
        // Both should produce outlines, but from different fonts.
        assert!(outline_serif.is_some());
        assert!(outline_sans.is_some());
    }

    #[test]
    fn empty_cache_no_embedded() {
        let cache = FontCache::empty();
        assert!(!cache.has_embedded_fonts());
    }

    #[test]
    fn units_per_em_fallback() {
        let cache = FontCache::empty();
        let upm = cache.units_per_em("anything");
        assert!(upm > 0);
    }

    #[test]
    fn ttf_has_hinting_missing_font() {
        // Unregistered fonts return false (they'd fall back to Liberation
        // Sans via glyph_outline, but ttf_has_hinting is strictly about
        // the named font being present with hinting tables).
        let cache = FontCache::empty();
        assert!(!cache.ttf_has_hinting("unregistered"));
    }

    // --- Tier 1 font bundle smoke tests (M-35). -----------------------
    //
    // These verify that each embedded Tier 1 asset is present, non-empty,
    // and parseable by the font engine. Routing lives in M-36; these
    // tests are strictly about bundled-asset integrity so CI catches a
    // missing or corrupted font before render regressions do.
    //
    // Each block is gated by its corresponding cargo feature:
    // when a feature is off the asset byte array is absent and the test
    // is skipped at compile time.

    #[cfg(feature = "tier1-sans-bold")]
    #[test]
    fn tier1_sans_bold_assets_non_empty() {
        assert!(
            !LIBERATION_SANS_BOLD.is_empty(),
            "LiberationSans-Bold asset missing"
        );
        assert!(
            !LIBERATION_SANS_ITALIC.is_empty(),
            "LiberationSans-Italic asset missing"
        );
        assert!(
            !LIBERATION_SANS_BOLD_ITALIC.is_empty(),
            "LiberationSans-BoldItalic asset missing"
        );
    }

    #[cfg(feature = "tier1-serif-bold")]
    #[test]
    fn tier1_serif_bold_assets_non_empty() {
        assert!(
            !LIBERATION_SERIF_BOLD.is_empty(),
            "LiberationSerif-Bold asset missing"
        );
        assert!(
            !LIBERATION_SERIF_ITALIC.is_empty(),
            "LiberationSerif-Italic asset missing"
        );
        assert!(
            !LIBERATION_SERIF_BOLD_ITALIC.is_empty(),
            "LiberationSerif-BoldItalic asset missing"
        );
    }

    #[cfg(feature = "tier1-fonts")]
    #[test]
    fn tier1_base_assets_non_empty() {
        assert!(
            !LIBERATION_MONO.is_empty(),
            "LiberationMono-Regular asset missing"
        );
        assert!(
            !LATIN_MODERN_ROMAN.is_empty(),
            "LatinModernRoman-Regular asset missing"
        );
        assert!(
            !LATIN_MODERN_ROMAN_ITALIC.is_empty(),
            "LatinModernRoman-Italic asset missing"
        );
        assert!(
            !LATIN_MODERN_MATH.is_empty(),
            "LatinModernMath-Subset asset missing"
        );
    }

    #[cfg(feature = "tier1-serif-bold")]
    #[test]
    fn tier1_liberation_serif_bold_parses() {
        let ttf = TrueTypeFont::from_bytes(LIBERATION_SERIF_BOLD)
            .expect("LiberationSerif-Bold should parse as TrueType");
        assert!(ttf.units_per_em() > 0);
    }

    #[cfg(feature = "tier1-serif-bold")]
    #[test]
    fn tier1_liberation_serif_italic_parses() {
        let ttf = TrueTypeFont::from_bytes(LIBERATION_SERIF_ITALIC)
            .expect("LiberationSerif-Italic should parse as TrueType");
        assert!(ttf.units_per_em() > 0);
    }

    #[cfg(feature = "tier1-serif-bold")]
    #[test]
    fn tier1_liberation_serif_bold_italic_parses() {
        let ttf = TrueTypeFont::from_bytes(LIBERATION_SERIF_BOLD_ITALIC)
            .expect("LiberationSerif-BoldItalic should parse as TrueType");
        assert!(ttf.units_per_em() > 0);
    }

    #[cfg(feature = "tier1-sans-bold")]
    #[test]
    fn tier1_liberation_bold_parses() {
        let ttf = TrueTypeFont::from_bytes(LIBERATION_SANS_BOLD)
            .expect("LiberationSans-Bold should parse as TrueType");
        assert!(ttf.units_per_em() > 0);
    }

    #[cfg(feature = "tier1-sans-bold")]
    #[test]
    fn tier1_liberation_italic_parses() {
        let ttf = TrueTypeFont::from_bytes(LIBERATION_SANS_ITALIC)
            .expect("LiberationSans-Italic should parse as TrueType");
        assert!(ttf.units_per_em() > 0);
    }

    #[cfg(feature = "tier1-sans-bold")]
    #[test]
    fn tier1_liberation_bold_italic_parses() {
        let ttf = TrueTypeFont::from_bytes(LIBERATION_SANS_BOLD_ITALIC)
            .expect("LiberationSans-BoldItalic should parse as TrueType");
        assert!(ttf.units_per_em() > 0);
    }

    #[cfg(feature = "tier1-fonts")]
    #[test]
    fn tier1_liberation_mono_parses() {
        let ttf = TrueTypeFont::from_bytes(LIBERATION_MONO)
            .expect("LiberationMono-Regular should parse as TrueType");
        assert!(ttf.units_per_em() > 0);
    }

    #[cfg(feature = "tier1-fonts")]
    #[test]
    fn tier1_lm_roman_parses() {
        let cff = parse_otf_cff(LATIN_MODERN_ROMAN)
            .expect("LatinModernRoman-Regular should parse as OpenType/CFF");
        // LM Roman 10 has 822 glyphs in the full face.
        assert!(cff.num_glyphs() > 0);
    }

    #[cfg(feature = "tier1-fonts")]
    #[test]
    fn tier1_lm_roman_italic_parses() {
        let cff = parse_otf_cff(LATIN_MODERN_ROMAN_ITALIC)
            .expect("LatinModernRoman-Italic should parse as OpenType/CFF");
        assert!(cff.num_glyphs() > 0);
    }

    #[cfg(feature = "tier1-fonts")]
    #[test]
    fn tier1_lm_math_parses() {
        let cff = parse_otf_cff(LATIN_MODERN_MATH)
            .expect("LatinModernMath-Subset should parse as OpenType/CFF");
        // Subset target was ~1469 glyphs covering common math ranges.
        assert!(
            cff.num_glyphs() > 500,
            "LM Math subset should carry the math glyphs it was subsetted for"
        );
    }

    #[cfg(all(
        feature = "tier1-fonts",
        feature = "tier1-sans-bold",
        feature = "tier1-serif-bold"
    ))]
    #[test]
    fn tier1_bundle_constructs() {
        // Double-cover: the bundle is loaded as part of FontCache::empty,
        // but we also exercise the explicit Tier1Bundle::load path so a
        // regression in one constructor does not mask the other.
        let bundle = Tier1Bundle::load();
        assert!(bundle.sans_bold.as_ref().unwrap().units_per_em() > 0);
        assert!(bundle.sans_italic.as_ref().unwrap().units_per_em() > 0);
        assert!(bundle.sans_bold_italic.as_ref().unwrap().units_per_em() > 0);
        assert!(bundle.serif_bold.as_ref().unwrap().units_per_em() > 0);
        assert!(bundle.serif_italic.as_ref().unwrap().units_per_em() > 0);
        assert!(bundle.serif_bold_italic.as_ref().unwrap().units_per_em() > 0);
        assert!(bundle.mono.as_ref().unwrap().units_per_em() > 0);
        assert!(bundle.lm_roman.as_ref().unwrap().num_glyphs() > 0);
        assert!(bundle.lm_roman_italic.as_ref().unwrap().num_glyphs() > 0);
        assert!(bundle.lm_math.as_ref().unwrap().num_glyphs() > 0);
        #[cfg(feature = "tier2-arabic")]
        {
            assert!(bundle.arabic_regular.as_ref().unwrap().units_per_em() > 0);
            assert!(bundle.arabic_bold.as_ref().unwrap().units_per_em() > 0);
        }
    }

    #[cfg(all(
        feature = "tier1-fonts",
        feature = "tier1-sans-bold",
        feature = "tier1-serif-bold"
    ))]
    #[test]
    fn tier1_size_budget() {
        // Guardrail: Tier 1+2 additions must stay within 4.0 MB so the
        // published crate does not balloon unchecked. Post-T1-SERIF the
        // Tier 1 total is ~3.0 MB; with  W1S-NOTOA Tier 2 Arabic
        // (Regular + Bold) added ~485 KB. If this test fails, a new asset
        // was added or an existing one grew; bump the budget explicitly
        // rather than silently ( spirit: keep asset cost visible).
        const BUDGET_BYTES: usize = 4_000_000;
        let mut total = LIBERATION_SANS_BOLD.len()
            + LIBERATION_SANS_ITALIC.len()
            + LIBERATION_SANS_BOLD_ITALIC.len()
            + LIBERATION_SERIF_BOLD.len()
            + LIBERATION_SERIF_ITALIC.len()
            + LIBERATION_SERIF_BOLD_ITALIC.len()
            + LIBERATION_MONO.len()
            + LATIN_MODERN_ROMAN.len()
            + LATIN_MODERN_ROMAN_ITALIC.len()
            + LATIN_MODERN_MATH.len();
        #[cfg(feature = "tier2-arabic")]
        {
            total += NOTO_SANS_ARABIC_REGULAR.len() + NOTO_SANS_ARABIC_BOLD.len();
        }
        assert!(
            total <= BUDGET_BYTES,
            "Tier 1+2 font bundle is {total} bytes, exceeds {BUDGET_BYTES}-byte budget"
        );
    }

    // -------------------------------------------------------------------
    // M-36 — font-name routing tests
    // -------------------------------------------------------------------

    fn cache() -> FontCache {
        FontCache::empty()
    }

    #[test]
    fn route_cmr_to_lm_roman() {
        let fc = cache();
        assert_eq!(fc.route_tier1("CMR10"), Some(Tier1Target::LmRoman));
        assert_eq!(fc.route_tier1("CMBX12"), Some(Tier1Target::LmRoman));
        assert_eq!(
            fc.route_tier1("LMRoman10-Regular"),
            Some(Tier1Target::LmRoman)
        );
    }

    #[test]
    fn route_cmti_cmsl_to_lm_roman_italic() {
        let fc = cache();
        assert_eq!(fc.route_tier1("CMTI10"), Some(Tier1Target::LmRomanItalic));
        assert_eq!(fc.route_tier1("CMSL10"), Some(Tier1Target::LmRomanItalic));
        assert_eq!(
            fc.route_tier1("LMRoman10-Italic"),
            Some(Tier1Target::LmRomanItalic)
        );
    }

    #[test]
    fn route_cmmi_cmsy_cmex_to_lm_math() {
        let fc = cache();
        assert_eq!(fc.route_tier1("CMMI10"), Some(Tier1Target::LmMath));
        assert_eq!(fc.route_tier1("CMSY10"), Some(Tier1Target::LmMath));
        assert_eq!(fc.route_tier1("CMEX10"), Some(Tier1Target::LmMath));
        assert_eq!(fc.route_tier1("MSAM10"), Some(Tier1Target::LmMath));
        assert_eq!(fc.route_tier1("MSBM10"), Some(Tier1Target::LmMath));
        assert_eq!(
            fc.route_tier1("LMMathematical10"),
            Some(Tier1Target::LmMath)
        );
    }

    #[test]
    fn route_stix_xits_to_lm_math() {
        let fc = cache();
        assert_eq!(
            fc.route_tier1("STIXTwoMath-Regular"),
            Some(Tier1Target::LmMath)
        );
        assert_eq!(
            fc.route_tier1("XITSMath-Regular"),
            Some(Tier1Target::LmMath)
        );
        assert_eq!(
            fc.route_tier1("AsanaMath-Regular"),
            Some(Tier1Target::LmMath)
        );
    }

    #[test]
    fn route_helvetica_bold_to_sans_bold() {
        let fc = cache();
        assert_eq!(
            fc.route_tier1("Helvetica-Bold"),
            Some(Tier1Target::SansBold)
        );
        assert_eq!(fc.route_tier1("Arial-Bold"), Some(Tier1Target::SansBold));
    }

    #[test]
    fn route_helvetica_italic_and_oblique() {
        let fc = cache();
        assert_eq!(
            fc.route_tier1("Helvetica-Italic"),
            Some(Tier1Target::SansItalic)
        );
        assert_eq!(
            fc.route_tier1("Helvetica-Oblique"),
            Some(Tier1Target::SansItalic)
        );
        assert_eq!(
            fc.route_tier1("Helvetica-BoldItalic"),
            Some(Tier1Target::SansBoldItalic)
        );
        assert_eq!(
            fc.route_tier1("Helvetica-BoldOblique"),
            Some(Tier1Target::SansBoldItalic)
        );
    }

    #[test]
    fn route_helvetica_plain_to_sans_regular() {
        let fc = cache();
        assert_eq!(fc.route_tier1("Helvetica"), Some(Tier1Target::SansRegular));
        assert_eq!(fc.route_tier1("Arial"), Some(Tier1Target::SansRegular));
    }

    #[test]
    fn route_times_family_to_serif_weights() {
        let fc = cache();
        // (#193): Times-Bold, Times-Italic, Times-BoldItalic now
        // route to their matching Liberation Serif weight instead of falling
        // through to SerifRegular with synthetic stem-widening.
        assert_eq!(
            fc.route_tier1("Times-Roman"),
            Some(Tier1Target::SerifRegular)
        );
        assert_eq!(fc.route_tier1("Times-Bold"), Some(Tier1Target::SerifBold));
        assert_eq!(
            fc.route_tier1("Times-Italic"),
            Some(Tier1Target::SerifItalic)
        );
        assert_eq!(
            fc.route_tier1("Times-BoldItalic"),
            Some(Tier1Target::SerifBoldItalic)
        );
        // TimesNewRomanPS-BoldMT (the PostScript name MS Word emits) must
        // also land on the Bold face.
        assert_eq!(
            fc.route_tier1("TimesNewRomanPS-BoldMT"),
            Some(Tier1Target::SerifBold)
        );
        assert_eq!(
            fc.route_tier1("TimesNewRoman-BoldItalic"),
            Some(Tier1Target::SerifBoldItalic)
        );
        // Oblique (used by some serif distributions) is treated as italic.
        assert_eq!(
            fc.route_tier1("Times-BoldOblique"),
            Some(Tier1Target::SerifBoldItalic)
        );
        assert_eq!(
            fc.route_tier1("NimbusRomNo9L-Regular"),
            Some(Tier1Target::SerifRegular)
        );
        assert_eq!(
            fc.route_tier1("NimbusRomNo9L-Medi"),
            Some(Tier1Target::SerifRegular)
        );
        // Subset prefix stripping + Times Bold combo.
        assert_eq!(
            fc.route_tier1("ABCDEF+Times-BoldItalic"),
            Some(Tier1Target::SerifBoldItalic)
        );
    }

    #[test]
    fn route_mono_families() {
        let fc = cache();
        assert_eq!(fc.route_tier1("Courier"), Some(Tier1Target::Mono));
        assert_eq!(fc.route_tier1("Courier-Bold"), Some(Tier1Target::Mono));
        assert_eq!(fc.route_tier1("Consolas"), Some(Tier1Target::Mono));
        assert_eq!(fc.route_tier1("LMMono10-Regular"), Some(Tier1Target::Mono));
        assert_eq!(fc.route_tier1("CMTT10"), Some(Tier1Target::Mono));
        // extend mono coverage to TeX Gyre, Nimbus, Liberation.
        assert_eq!(
            fc.route_tier1("TeXGyreCursor-Regular"),
            Some(Tier1Target::Mono)
        );
        assert_eq!(fc.route_tier1("NimbusMonL-Regu"), Some(Tier1Target::Mono));
        assert_eq!(fc.route_tier1("LiberationMono"), Some(Tier1Target::Mono));
        assert_eq!(fc.route_tier1("SourceCodePro"), Some(Tier1Target::Mono));
        assert_eq!(fc.route_tier1("Inconsolata"), Some(Tier1Target::Mono));
    }

    #[test]
    fn route_latex_math_variants() {
        // additional LaTeX math family prefixes (Euler,
        // MnSymbol, St Mary, wasy, cmbsy) should route to LM Math.
        let fc = cache();
        assert_eq!(fc.route_tier1("EUEX10"), Some(Tier1Target::LmMath));
        assert_eq!(fc.route_tier1("EUFM7"), Some(Tier1Target::LmMath));
        assert_eq!(fc.route_tier1("EUFB10"), Some(Tier1Target::LmMath));
        assert_eq!(fc.route_tier1("EUSM10"), Some(Tier1Target::LmMath));
        assert_eq!(fc.route_tier1("EURM10"), Some(Tier1Target::LmMath));
        assert_eq!(fc.route_tier1("MnSymbol10"), Some(Tier1Target::LmMath));
        assert_eq!(fc.route_tier1("MnSymbolA10"), Some(Tier1Target::LmMath));
        assert_eq!(fc.route_tier1("stmaryrd10"), Some(Tier1Target::LmMath));
        assert_eq!(fc.route_tier1("wasy10"), Some(Tier1Target::LmMath));
        assert_eq!(fc.route_tier1("CMBSY10"), Some(Tier1Target::LmMath));
    }

    #[test]
    fn route_texgyre_and_sans_aliases() {
        // TeX Gyre families (Heros sans, Termes serif),
        // extended sans/serif aliases (Calibri/Tahoma/Segoe, Minion/Giovanni).
        let fc = cache();
        assert_eq!(
            fc.route_tier1("TeXGyreHeros-Regular"),
            Some(Tier1Target::SansRegular)
        );
        assert_eq!(
            fc.route_tier1("TeXGyreHeros-Bold"),
            Some(Tier1Target::SansBold)
        );
        assert_eq!(
            fc.route_tier1("TeXGyreTermes-Regular"),
            Some(Tier1Target::SerifRegular)
        );
        assert_eq!(
            fc.route_tier1("TeXGyreTermesX-Regular"),
            Some(Tier1Target::SerifRegular)
        );
        assert_eq!(
            fc.route_tier1("TeXGyrePagella-Regular"),
            Some(Tier1Target::SerifRegular)
        );
        assert_eq!(fc.route_tier1("Calibri"), Some(Tier1Target::SansRegular));
        assert_eq!(fc.route_tier1("Calibri-Bold"), Some(Tier1Target::SansBold));
        assert_eq!(
            fc.route_tier1("MinionPro-Regular"),
            Some(Tier1Target::SerifRegular)
        );
        assert_eq!(
            fc.route_tier1("MinionPro-BoldIt"),
            Some(Tier1Target::SerifBoldItalic)
        );
    }

    #[test]
    fn route_cmr_bold_and_caps_variants() {
        // CM bold serif variants (CMBX extended, CMDUNH,
        // CMFIB) and small caps.
        let fc = cache();
        assert_eq!(fc.route_tier1("CMBX10"), Some(Tier1Target::LmRoman));
        assert_eq!(fc.route_tier1("CMDUNH10"), Some(Tier1Target::LmRoman));
        assert_eq!(fc.route_tier1("CMCSC10"), Some(Tier1Target::LmRoman));
        assert_eq!(fc.route_tier1("SFRM1000"), Some(Tier1Target::LmRoman));
    }

    #[test]
    fn route_strips_subset_prefix() {
        let fc = cache();
        // ABCDEF+CMR10 should route exactly like CMR10.
        assert_eq!(fc.route_tier1("ABCDEF+CMR10"), Some(Tier1Target::LmRoman));
        assert_eq!(fc.route_tier1("ZZZZZZ+CMMI10"), Some(Tier1Target::LmMath));
        assert_eq!(
            fc.route_tier1("ABCDEF+Helvetica-Bold"),
            Some(Tier1Target::SansBold)
        );
    }

    #[test]
    fn route_is_case_insensitive() {
        let fc = cache();
        assert_eq!(fc.route_tier1("cmr10"), Some(Tier1Target::LmRoman));
        assert_eq!(
            fc.route_tier1("HELVETICA-bold"),
            Some(Tier1Target::SansBold)
        );
    }

    #[test]
    fn route_unknown_names_return_none() {
        let fc = cache();
        assert_eq!(fc.route_tier1("MysteryFont"), None);
        assert_eq!(fc.route_tier1("F1"), None);
        assert_eq!(fc.route_tier1("Embedded"), None);
    }

    #[test]
    fn fallback_for_strips_subset_prefix() {
        // Issue #204: `fallback_for` should agree with `route_tier1` on
        // subset-prefixed names. Compare pointer identity of the returned
        // TrueTypeFont against the bare name result.
        let fc = cache();
        let stripped_face = fc.fallback_for("Times-Bold") as *const _;
        let prefixed_face = fc.fallback_for("AAAAAA+Times-Bold") as *const _;
        assert_eq!(
            stripped_face, prefixed_face,
            "subset prefix should not change serif-vs-sans routing"
        );

        // Hostile-prefix case: a prefix that is a valid 6-uppercase subset
        // tag but embeds a serif keyword would, before the fix, trick
        // `fallback_for` into routing "TIMESX+Helvetica-Bold" to serif via
        // the raw substring match on "times". After stripping, both the
        // hostile and the bare Helvetica-Bold land on the sans face.
        let bare = fc.fallback_for("Helvetica-Bold") as *const _;
        let hostile = fc.fallback_for("TIMESX+Helvetica-Bold") as *const _;
        assert_eq!(
            bare, hostile,
            "hostile subset prefix must not leak serif keywords into routing"
        );
    }

    #[test]
    fn unicode_sniff_math_operators() {
        let fc = cache();
        // ∀ U+2200 (math operator) -> LM Math.
        assert_eq!(fc.route_by_unicode('∀'), Some(Tier1Target::LmMath));
        // ∑ U+2211 (n-ary summation) -> LM Math.
        assert_eq!(fc.route_by_unicode('∑'), Some(Tier1Target::LmMath));
        // → U+2192 (arrow) -> LM Math.
        assert_eq!(fc.route_by_unicode('→'), Some(Tier1Target::LmMath));
    }

    #[test]
    fn unicode_sniff_mathematical_alphanumerics() {
        let fc = cache();
        // 𝐴 U+1D434 (MATHEMATICAL ITALIC CAPITAL A) -> LM Math.
        assert_eq!(fc.route_by_unicode('\u{1D434}'), Some(Tier1Target::LmMath));
        // 𝟏 U+1D7CF (MATHEMATICAL BOLD DIGIT ONE) -> LM Math.
        assert_eq!(fc.route_by_unicode('\u{1D7CF}'), Some(Tier1Target::LmMath));
    }

    #[test]
    fn unicode_sniff_letters_return_none() {
        let fc = cache();
        // ASCII letters don't need math routing.
        assert_eq!(fc.route_by_unicode('A'), None);
        assert_eq!(fc.route_by_unicode('z'), None);
        // CJK handled by fallback_cjk, not routed here.
        assert_eq!(fc.route_by_unicode('中'), None);
    }

    #[test]
    fn unicode_sniff_greek_routes_to_sans() {
        // Issue #205: Greek block (0x0370..=0x03FF) falls through every
        // other routing layer when the source font name doesn't hint
        // (e.g. "F1"/"Embedded"). Pin the Greek -> Liberation Sans edge
        // so it doesn't regress to .notdef.
        let fc = cache();
        // α U+03B1 (lowercase alpha).
        assert_eq!(
            fc.route_by_unicode('\u{03B1}'),
            Some(Tier1Target::SansRegular)
        );
        // β U+03B2 (lowercase beta).
        assert_eq!(
            fc.route_by_unicode('\u{03B2}'),
            Some(Tier1Target::SansRegular)
        );
        // Γ U+0393 (uppercase gamma).
        assert_eq!(
            fc.route_by_unicode('\u{0393}'),
            Some(Tier1Target::SansRegular)
        );
        // π U+03C0 (lowercase pi).
        assert_eq!(
            fc.route_by_unicode('\u{03C0}'),
            Some(Tier1Target::SansRegular)
        );
        // Block edges.
        assert_eq!(
            fc.route_by_unicode('\u{0370}'),
            Some(Tier1Target::SansRegular)
        );
        assert_eq!(
            fc.route_by_unicode('\u{03FF}'),
            Some(Tier1Target::SansRegular)
        );
        // Just outside the block stays unrouted.
        assert_eq!(fc.route_by_unicode('\u{036F}'), None);
        assert_eq!(fc.route_by_unicode('\u{0400}'), None);
    }

    #[cfg(feature = "tier2-arabic")]
    #[test]
    fn unicode_sniff_arabic_routes_to_arabic_regular() {
        // Arabic and the three presentation-form blocks
        // route to Noto Sans Arabic Regular when the source font name
        // doesn't hint at weight. Pin so the IA-spanish-Arabic doc keeps
        // rendering glyphs (Liberation has zero Arabic coverage).
        let fc = cache();
        // Arabic block edges + a few common letters.
        // ا U+0627 (alef), ب U+0628 (beh), م U+0645 (meem), ي U+064A (yeh).
        for c in [0x0600u32, 0x0627, 0x0628, 0x0645, 0x064A, 0x06FF] {
            assert_eq!(
                fc.route_by_unicode(char::from_u32(c).unwrap()),
                Some(Tier1Target::ArabicRegular),
                "Arabic block U+{c:04X} should route to ArabicRegular"
            );
        }
        // Arabic Supplement.
        assert_eq!(
            fc.route_by_unicode('\u{0750}'),
            Some(Tier1Target::ArabicRegular)
        );
        assert_eq!(
            fc.route_by_unicode('\u{077F}'),
            Some(Tier1Target::ArabicRegular)
        );
        // Arabic Presentation Forms-A (contextual / ligature forms).
        assert_eq!(
            fc.route_by_unicode('\u{FB50}'),
            Some(Tier1Target::ArabicRegular)
        );
        assert_eq!(
            fc.route_by_unicode('\u{FDFF}'),
            Some(Tier1Target::ArabicRegular)
        );
        // Arabic Presentation Forms-B.
        assert_eq!(
            fc.route_by_unicode('\u{FE70}'),
            Some(Tier1Target::ArabicRegular)
        );
        assert_eq!(
            fc.route_by_unicode('\u{FEFF}'),
            Some(Tier1Target::ArabicRegular)
        );
        // Just outside each block stays unrouted.
        assert_eq!(fc.route_by_unicode('\u{05FF}'), None);
        assert_eq!(fc.route_by_unicode('\u{0700}'), None);
        assert_eq!(fc.route_by_unicode('\u{074F}'), None);
        assert_eq!(fc.route_by_unicode('\u{0780}'), None);
        assert_eq!(fc.route_by_unicode('\u{FB4F}'), None);
        assert_eq!(fc.route_by_unicode('\u{FE00}'), None);
        assert_eq!(fc.route_by_unicode('\u{FE6F}'), None);
    }

    #[cfg(feature = "tier2-arabic")]
    #[test]
    fn name_route_arabic_picks_weight() {
        // explicit Arabic font names route to the matching
        // Arabic weight. Bold suffix picks ArabicBold; bare names pick
        // ArabicRegular. Black is treated as bold (Noto-Naskh has Black
        // weight upstream; we route it to Bold here).
        let fc = cache();
        assert_eq!(
            fc.route_tier1("NotoSansArabic-Regular"),
            Some(Tier1Target::ArabicRegular)
        );
        assert_eq!(
            fc.route_tier1("NotoSansArabic-Bold"),
            Some(Tier1Target::ArabicBold)
        );
        assert_eq!(
            fc.route_tier1("NotoNaskhArabic"),
            Some(Tier1Target::ArabicRegular)
        );
        assert_eq!(
            fc.route_tier1("NotoNaskhArabic-Bold"),
            Some(Tier1Target::ArabicBold)
        );
        assert_eq!(
            fc.route_tier1("Amiri-Regular"),
            Some(Tier1Target::ArabicRegular)
        );
        assert_eq!(
            fc.route_tier1("Scheherazade-Bold"),
            Some(Tier1Target::ArabicBold)
        );
    }

    #[cfg(feature = "tier2-arabic")]
    #[test]
    fn tier1_outline_arabic_resolves() {
        // The whole point of bundling Noto Sans Arabic: a PDF that asks
        // for an Arabic glyph through an unrouted font name (e.g. "F1")
        // must produce a real outline, not a `.notdef` box. ا (U+0627
        // alef) is the most common Arabic letter; if any glyph resolves
        // through the new path it is this one.
        let mut fc = cache();
        let outline = fc.glyph_outline("F1", '\u{0627}');
        assert!(
            outline.is_some(),
            "Arabic alef U+0627 must resolve through Noto Sans Arabic via \
             route_by_unicode when the source font name doesn't hint"
        );

        // Same letter through an explicit Arabic-named face also resolves
        // (via name routing, not Unicode sniff). Both paths landing means
        // a regression in either is caught here.
        let via_name = fc.glyph_outline("NotoSansArabic-Regular", '\u{0627}');
        assert!(
            via_name.is_some(),
            "name-routed NotoSansArabic-Regular should also render U+0627"
        );

        // Bold-routed face resolves via the bold weight.
        let via_bold = fc.glyph_outline("NotoSansArabic-Bold", '\u{0627}');
        assert!(
            via_bold.is_some(),
            "name-routed NotoSansArabic-Bold should render U+0627"
        );
    }

    #[test]
    fn name_route_still_wins_over_greek_unicode_sniff() {
        // Routing fires in two distinct places in the renderer. At the
        // name level, `route_tier1` runs first: a serif-named face
        // resolves to SerifRegular regardless of the per-glyph Unicode
        // codepoint. `route_by_unicode` only kicks in when the name
        // miss falls through. Pin that so Greek letters in a Times
        // body paragraph don't get yanked into Liberation Sans.
        let fc = cache();
        // Times-Roman -> SerifRegular via name routing, independent of
        // whether the glyph is Greek.
        assert_eq!(
            fc.route_tier1("Times-Roman"),
            Some(Tier1Target::SerifRegular)
        );
    }

    #[test]
    fn tier1_outline_lm_roman_has_ascii() {
        let mut fc = cache();
        // CMR10 -> LM Roman: basic ASCII should resolve via routing.
        let outline = fc.glyph_outline("CMR10", 'A');
        assert!(
            outline.is_some(),
            "expected LM Roman to render 'A' for CMR10"
        );
    }

    #[test]
    fn tier1_outline_lm_math_has_sigma() {
        let mut fc = cache();
        // CMSY10 -> LM Math: ∑ (U+2211) should resolve.
        let outline = fc.glyph_outline("CMSY10", '∑');
        assert!(
            outline.is_some(),
            "expected LM Math to render U+2211 for CMSY10"
        );
    }

    #[test]
    fn tier1_outline_unicode_sniff_on_unrouted_font() {
        let mut fc = cache();
        // The font name doesn't route by name (MysteryFont falls through
        // `route_tier1`), but U+2211 '∑' is in the Mathematical Operators
        // block so `route_by_unicode` picks LM Math. The outline must
        // resolve via the Unicode-range path rather than the generic
        // serif/sans fallback (which doesn't carry math operator glyphs).
        let outline = fc.glyph_outline("MysteryFont", '∑');
        assert!(
            outline.is_some(),
            "Unicode-range sniff should route U+2211 through LM Math \
             even when the font name is unrouted"
        );

        // Same glyph through a routed math font name reaches LM Math via
        // `route_tier1` instead. Both routing layers must land on a
        // non-empty outline for the same char — if one regresses, the
        // other catches it.
        let via_name = fc.glyph_outline("CMSY10", '∑');
        assert!(
            via_name.is_some(),
            "CMSY10 should route U+2211 through LM Math via name"
        );
    }

    // ---------------- T60-MEMBATCH: bounded caches + reset ----------------

    /// `reset_document_scoped` drops the per-document glyph caches and
    /// keeps Tier 1 usable. Pre-reset: populate cache by resolving a few
    /// glyphs; post-reset: cache sizes are zero and the same glyph still
    /// resolves from the Tier 1 bundle (pinned, not cleared).
    #[test]
    fn reset_document_scoped_drops_caches_and_keeps_tier1() {
        let mut fc = cache();
        // Populate via a Tier-1-routed font.
        for ch in ["A", "B", "C", "a", "b", "c"]
            .iter()
            .flat_map(|s| s.chars())
        {
            let _ = fc.glyph_outline("CMR10", ch);
        }
        let (outline_before, _, _) = fc.cache_sizes();
        assert!(
            outline_before >= 6,
            "expected at least 6 outline-cache entries after populating, got {outline_before}"
        );

        fc.reset_document_scoped();
        let (outline_after, hinting_after, hinted_after) = fc.cache_sizes();
        assert_eq!(outline_after, 0, "outline_cache should be drained");
        assert_eq!(hinting_after, 0, "hinting_cache should be drained");
        assert_eq!(hinted_after, 0, "hinted_glyph_cache should be drained");

        // Tier 1 survives: same font + char still resolves via the pinned
        // bundle even though the outline cache was flushed.
        assert!(
            fc.glyph_outline("CMR10", 'A').is_some(),
            "Tier 1 bundle must remain available after reset"
        );
    }

    /// The soft cap bounds the outline cache. Pushing past CACHE_SOFT_CAP
    /// worth of distinct keys must not produce a cache larger than the cap.
    /// Uses by-code lookups (PUA chars) to manufacture unique keys cheaply.
    #[test]
    fn outline_cache_respects_soft_cap() {
        let mut fc = cache();
        // Register a dummy encoding entry so `glyph_outline_by_code`
        // actually hits the cache insert path. We don't care if the glyph
        // itself resolves; the soft-cap guard fires on miss inserts too.
        // Use a font we know the fallback path will see to trigger the
        // write path: prefer the existing Tier 1 routing.
        for code in 0u16..=((CACHE_SOFT_CAP as u16 / 2) + 64) {
            // Use glyph_outline with a synthetic char derived from `code`
            // so each iteration gets a fresh (font, char) cache key.
            let ch = char::from_u32(0x1000 + code as u32).unwrap_or('\u{FFFD}');
            let _ = fc.glyph_outline("CMR10", ch);
            let (len, _, _) = fc.cache_sizes();
            assert!(
                len <= CACHE_SOFT_CAP,
                "outline cache grew past soft cap ({} > {}) on iteration {}",
                len,
                CACHE_SOFT_CAP,
                code
            );
        }
    }

    /// `cache_sizes` returns monotonic counts matching inserts (modulo the
    /// soft-cap flush). Smoke test for the tuple shape + ordering.
    #[test]
    fn cache_sizes_tuple_shape() {
        let mut fc = cache();
        let (o0, h0, g0) = fc.cache_sizes();
        assert_eq!((o0, h0, g0), (0, 0, 0));
        for ch in "hello world".chars() {
            let _ = fc.glyph_outline("Times-Roman", ch);
        }
        let (o1, _h1, _g1) = fc.cache_sizes();
        assert!(o1 > 0, "outline cache should have grown");
    }
}

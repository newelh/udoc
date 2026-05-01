#![deny(unsafe_code)]
#![warn(missing_docs)]

//! PDF page renderer for the hook pipeline.
//!
//! Renders PDF pages to PNG images using embedded font outlines.
//! Text is rasterized at specified DPI using the document's own fonts
//! (extracted from FontFile2/FontFile3 streams), with Liberation Sans
//! as a fallback for non-embedded fonts.
//!
//! # Example
//!
//! ```
//! // Pick the rendering profile that matches your downstream consumer.
//! use udoc_render::RenderingProfile;
//! let profile = RenderingProfile::default();
//! assert_eq!(profile, RenderingProfile::OcrFriendly);
//! ```

/// Page-overlay annotation primitives. Doc-hidden because the
/// downstream-user surface for annotations is the `udoc::render`
/// facade.
#[doc(hidden)]
pub mod annotation;
#[cfg(feature = "test-internals")]
pub mod auto_hinter;
#[cfg(not(feature = "test-internals"))]
pub(crate) mod auto_hinter;
pub(crate) mod clip;
pub(crate) mod compositor;
/// Embedded-font cache. Doc-hidden -- internal to the renderer pipeline.
#[doc(hidden)]
pub mod font_cache;
/// Renderer inspection helpers used by `glyph-diff` and the
/// `cursor-pair-diagnose` tools. Doc-hidden -- not part of the
/// downstream-user surface.
#[doc(hidden)]
pub mod inspect;
pub(crate) mod path_raster;
pub(crate) mod pattern;
/// PNG encoding helpers. Doc-hidden -- internal to the renderer pipeline.
#[doc(hidden)]
pub mod png;
pub(crate) mod ps_hints;
#[cfg(feature = "test-internals")]
pub mod rasterizer;
#[cfg(not(feature = "test-internals"))]
pub(crate) mod rasterizer;
pub(crate) mod shading;

use std::collections::HashMap;

pub use self::annotation::render_annotations;

use self::compositor::{blend_pixel, composite_glyph, composite_glyph_at, set_pixel};
use self::font_cache::FontCache;
use udoc_core::document::presentation::{
    FillRule, ImagePlacement, PageDef, PageShape, PathShapeKind, PositionedSpan,
};
use udoc_core::document::Document;

/// Rendering profile: controls trade-offs between viewer-grade pixel fidelity
/// and OCR-grade text legibility.
///
/// Some hinter passes (notably the FreeType `aflatin.c:1197-1296` x-height
/// scale alignment, ported in M-39) match FreeType NORMAL mode per-glyph but
/// shift baselines by a fraction of a pixel, which can cause Tesseract in
/// single-block mode (`--psm 6`) to fragment lines on certain documents even
/// though aggregate SSIM stays within noise. MuPDF uses FreeType LIGHT mode
/// (no X-axis auto-hint, no x-height scale bump) so LIGHT-style output is
/// also the closest match to MuPDF reference renders on those docs.
///
/// Default is [`RenderingProfile::OcrFriendly`] -- this is the profile the
/// 300 DPI OCR char-acc gate measures against, and it also minimises
/// the aggregate-SSIM delta against MuPDF. Callers who want FT-NORMAL
/// byte-exact per-glyph output (e.g. tighter FT cursor-pair diagnostics)
/// can opt into [`RenderingProfile::Visual`] explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderingProfile {
    /// OCR-friendly: disable hinter passes that are FT-NORMAL-correct but
    /// perturb baselines enough to degrade tesseract psm-6 row segmentation.
    /// Specifically disables the M-39 x-height scale alignment (#175).
    /// Best match for MuPDF LIGHT-mode aggregate SSIM and for downstream
    /// OCR consumers.
    #[default]
    OcrFriendly,
    /// Visual / FT-NORMAL: enable all per-glyph fidelity passes including
    /// M-39 x-height scale alignment. Matches FreeType NORMAL byte-exact
    /// for cursor-pair probes but slightly perturbs baselines; documented
    /// to cost ~0.01 OCR char-acc on some arxiv documents at 300 DPI.
    Visual,
}

thread_local! {
    static RENDER_PROFILE: std::cell::Cell<RenderingProfile> =
        const { std::cell::Cell::new(RenderingProfile::OcrFriendly) };
}

/// Return the rendering profile in effect for the current call stack.
///
/// Used by the auto-hinter (x-height scale alignment gate) to switch
/// between FT-NORMAL-faithful and MuPDF-LIGHT-faithful output. Default
/// is [`RenderingProfile::OcrFriendly`].
pub(crate) fn current_rendering_profile() -> RenderingProfile {
    RENDER_PROFILE.with(|p| p.get())
}

/// RAII guard that sets the rendering profile for the current thread and
/// restores the previous value on drop. Used by [`render_page_rgb`] to
/// scope a per-call profile without breaking the existing function
/// signature.
struct RenderProfileGuard {
    previous: RenderingProfile,
}

impl RenderProfileGuard {
    fn new(profile: RenderingProfile) -> Self {
        let previous = RENDER_PROFILE.with(|p| p.replace(profile));
        Self { previous }
    }
}

impl Drop for RenderProfileGuard {
    fn drop(&mut self) {
        let prev = self.previous;
        RENDER_PROFILE.with(|p| p.set(prev));
    }
}
use udoc_core::error::{Error as CoreError, Result};
use udoc_core::geometry::BoundingBox;
use udoc_font::ttf::StemHints;

/// Default render DPI. 300 produces clean text at the cost of ~24 MB per
/// US Letter page (2550x3300 pixels). Use 150 for faster/smaller output
/// when quality is less critical (e.g., thumbnail generation).
pub const DEFAULT_DPI: u32 = 300;

/// Maximum render dimensions to prevent OOM. 8500x11000 at 3 bytes = ~280 MB.
/// Supports US Letter at 1000 DPI.
const MAX_DIMENSION: u32 = 8500;

/// Render a single page of a Document to PNG bytes.
///
/// Uses the document's positioned spans (text at known bbox positions) and
/// rasterizes each character using font outlines from the font cache.
/// The coordinate transform converts PDF y-up to image y-down and scales
/// by the DPI factor.
///
/// Returns an error if the page index is out of range or the document has
/// no page geometry (flow-based formats like DOCX/RTF).
/// Cache key for rasterized glyph bitmaps.
/// Keyed by (font_name_ptr, character, ppem_quarter, fract_x_bin) to avoid
/// re-rasterizing repeated characters. ppem_quarter is the actual rasterized
/// font size in quarter-pixel units (e.g. 45.5px -> 182). This prevents
/// different sub-pixel font sizes (11pt vs 11.1pt at 300dpi) from colliding
/// to a single template -- important on arxiv PDFs that mix body text with
/// slightly-resized headings or mathematical-notation runs. fract_x_bin
/// is the cursor's fractional X quantized into `SUBPIX_BINS` slots so glyphs
/// landing at different sub-pixel offsets get distinct AA-pattern variants.
type GlyphBitmapCache = HashMap<(usize, char, u16, u8), rasterizer::GlyphBitmap>;

/// Quantize font size in pixels to quarter-pixel units, capped to u16 range.
/// 0.25-pixel granularity is fine enough to capture meaningful glyph-shape
/// variation between sub-pixel sizes (~0.5% per quarter at typical 40-50ppem)
/// while keeping the cache key compact and yielding good hit rates.
#[inline]
fn font_size_key(font_size_px: f64) -> u16 {
    let q = (font_size_px * 4.0).round();
    q.clamp(0.0, u16::MAX as f64) as u16
}

/// Number of sub-pixel positioning bins along the X axis. With N bins,
/// glyphs are rasterized at fract_x in {0.5/N, 1.5/N, ..., (N-0.5)/N},
/// so each cursor sub-pixel offset maps to the AA pattern that best
/// represents it instead of being snapped to a single template per glyph.
/// 24 bins is the SSIM plateau on the 100-doc bench (sprint 48 round 4B)
/// paired with the quarter-pixel cache key.  removed the coverage-gamma
/// edge-darkening pass (replaced by the discovered-stem scanner at the
/// Type1 hint layer), which left the sub-pixel bin density dominant.
/// 32 doesn't help further and the cache miss rate starts costing serial
/// throughput.
const SUBPIX_BINS: u8 = 24;

/// Quantize fractional cursor X to a sub-pixel bin index.
#[inline]
fn fract_x_bin(cursor_x: f64) -> u8 {
    let f = cursor_x - cursor_x.floor();
    let bin = (f * SUBPIX_BINS as f64).floor() as i32;
    bin.clamp(0, SUBPIX_BINS as i32 - 1) as u8
}

/// Convert a sub-pixel bin index back to the fractional X offset used at
/// rasterization. Centers each bin at its midpoint so the bin best
/// represents cursor positions inside it.
#[inline]
fn fract_x_for_bin(bin: u8) -> f64 {
    (bin as f64 + 0.5) / SUBPIX_BINS as f64
}

/// Render a single page of a [`Document`] to a PNG byte buffer at the given
/// DPI, using the default rendering profile.
///
/// Convenience wrapper around [`render_page_with_profile`] for the common
/// case. `font_cache` is threaded through to share parsed font programs
/// across pages. Returns the encoded PNG bytes on success.
pub fn render_page(
    doc: &Document,
    page_index: usize,
    dpi: u32,
    font_cache: &mut FontCache,
) -> Result<Vec<u8>> {
    render_page_with_profile(
        doc,
        page_index,
        dpi,
        font_cache,
        RenderingProfile::default(),
    )
}

/// Render a page to PNG bytes with an explicit [`RenderingProfile`].
///
/// See [`RenderingProfile`] for the trade-off. The default profile (used by
/// [`render_page`]) is [`RenderingProfile::OcrFriendly`].
pub fn render_page_with_profile(
    doc: &Document,
    page_index: usize,
    dpi: u32,
    font_cache: &mut FontCache,
    profile: RenderingProfile,
) -> Result<Vec<u8>> {
    let (pixels, out_w, out_h) =
        render_page_rgb_with_profile(doc, page_index, dpi, font_cache, profile)?;
    Ok(png::encode_rgb_png(&pixels, out_w, out_h))
}

/// Render a page to a raw RGB8 pixel buffer plus dimensions.
///
/// Same behaviour as [`render_page`] but skips PNG encoding so callers that
/// need the pixel data directly (SSIM comparison, raw-bitmap inspection)
/// don't pay the round-trip through `encode_rgb_png` + a PNG decoder.
/// Pixel layout: `[R, G, B, R, G, B, ...]` row-major, top-left origin.
pub fn render_page_rgb(
    doc: &Document,
    page_index: usize,
    dpi: u32,
    font_cache: &mut FontCache,
) -> Result<(Vec<u8>, u32, u32)> {
    render_page_rgb_with_profile(
        doc,
        page_index,
        dpi,
        font_cache,
        RenderingProfile::default(),
    )
}

/// Render a page to a raw RGB8 pixel buffer with an explicit
/// [`RenderingProfile`]. See [`render_page_with_profile`] for the trade-off.
pub fn render_page_rgb_with_profile(
    doc: &Document,
    page_index: usize,
    dpi: u32,
    font_cache: &mut FontCache,
    profile: RenderingProfile,
) -> Result<(Vec<u8>, u32, u32)> {
    let _guard = RenderProfileGuard::new(profile);
    let pres = doc
        .presentation
        .as_ref()
        .ok_or_else(|| CoreError::new("document has no presentation data for rendering"))?;

    let page_def = pres
        .pages
        .get(page_index)
        .ok_or_else(|| CoreError::new(format!("page index {page_index} out of range")))?;

    let scale = dpi as f64 / 72.0;

    // Render into unrotated dimensions. /Rotate is a display-time transform
    // applied to the final pixel buffer, not a content-space transform.
    let render_w = (page_def.width * scale).round() as u32;
    let render_h = (page_def.height * scale).round() as u32;
    let (width_px, height_px) = (render_w, render_h);

    if width_px == 0 || height_px == 0 {
        return Err(CoreError::new("page has zero dimensions"));
    }
    if width_px > MAX_DIMENSION || height_px > MAX_DIMENSION {
        return Err(CoreError::new(format!(
            "page dimensions {width_px}x{height_px} exceed limit {MAX_DIMENSION}"
        )));
    }

    // LCD subpixel rendering is disabled by default (#183). It writes per-channel
    // alpha (R/G/B) into the framebuffer, which produces chromatic fringing on
    // every glyph edge -- red/orange on the left, blue on the right. That
    // fringing costs ~0.01-0.02 SSIM uniformly vs luminance-only references like
    // mupdf and pdftoppm, both of which emit grayscale coverage. The 5-tap LCD
    // filter smooths transitions but does NOT eliminate the chromatic residue,
    // because the channel-alpha offsets from a 3x-resolution coverage map are
    // inherently colored unless the output is composited to a physical RGB-stripe
    // LCD panel (which renders to PNG do not target). The subpixel code paths
    // are preserved for opt-in experimentation but are off for all default renders.
    let use_subpixel = false;

    // Allocate RGB pixel buffer (white background).
    let buf_size = width_px as usize * height_px as usize * 3;
    let mut pixels = vec![255u8; buf_size];

    // Filter spans for this page, then coalesce adjacent spans that form
    // parts of the same word. TJ arrays with kern adjustments create separate
    // spans per string element; coalescing eliminates the visible gaps.
    let filtered: Vec<&PositionedSpan> = pres
        .raw_spans
        .iter()
        .filter(|s| s.page_index == page_index)
        .collect();
    let coalesced = coalesce_spans(filtered);

    let page_height = effective_page_height(page_def);
    let page_origin_x = page_def.origin_x;
    let mut glyph_cache: GlyphBitmapCache = HashMap::new();

    // Build a unified render queue sorted by content stream z-order.
    // This ensures images, shapes, and text are painted in the same order
    // as the PDF content stream, preserving correct layering.
    enum RenderItem<'a> {
        Image(&'a ImagePlacement),
        Shape(&'a PageShape),
        PaintPath(&'a udoc_core::document::presentation::PaintPath),
        Shading(&'a udoc_core::document::presentation::PaintShading),
        Pattern(&'a udoc_core::document::presentation::PaintPattern),
        Text(&'a PositionedSpan),
    }
    let mut render_queue: Vec<(u32, RenderItem<'_>)> = Vec::new();

    for p in pres
        .image_placements
        .iter()
        .filter(|p| p.page_index == page_index)
    {
        render_queue.push((p.z_index, RenderItem::Image(p)));
    }
    for s in pres.shapes.iter().filter(|s| s.page_index == page_index) {
        render_queue.push((s.z_index, RenderItem::Shape(s)));
    }
    // UDOC_SKIP_PATHS=1 disables the paint-path rasterizer for A/B
    // spot-checks. Not a user-facing knob.
    let skip_paint_paths = std::env::var("UDOC_SKIP_PATHS").is_ok_and(|v| v == "1");
    if !skip_paint_paths {
        for pp in pres
            .paint_paths
            .iter()
            .filter(|pp| pp.page_index == page_index)
        {
            render_queue.push((pp.z_index, RenderItem::PaintPath(pp)));
        }
    }
    for sh in pres.shadings.iter().filter(|s| s.page_index == page_index) {
        render_queue.push((sh.z_index, RenderItem::Shading(sh)));
    }
    for tp in pres.patterns.iter().filter(|p| p.page_index == page_index) {
        render_queue.push((tp.z_index, RenderItem::Pattern(tp)));
    }
    for span in &coalesced {
        render_queue.push((span.z_index, RenderItem::Text(span)));
    }

    // Sort by z_index. Stable sort preserves relative order for items
    // with the same z_index (e.g., z_index=0 for non-PDF backends).
    render_queue.sort_by_key(|(z, _)| *z);

    for (_, item) in &render_queue {
        match item {
            RenderItem::Image(placement) => {
                render_image(
                    &mut pixels,
                    width_px,
                    height_px,
                    placement,
                    page_origin_x,
                    page_height,
                    scale,
                    doc,
                );
            }
            RenderItem::Shape(shape) => {
                render_shape(
                    &mut pixels,
                    width_px,
                    height_px,
                    shape,
                    page_origin_x,
                    page_height,
                    scale,
                );
            }
            RenderItem::PaintPath(pp) => {
                path_raster::rasterize_paint_path(
                    &mut pixels,
                    width_px,
                    height_px,
                    pp,
                    page_origin_x,
                    page_height,
                    scale,
                );
            }
            RenderItem::Shading(sh) => {
                shading::rasterize_paint_shading(
                    &mut pixels,
                    width_px,
                    height_px,
                    sh,
                    page_origin_x,
                    page_height,
                    scale,
                );
            }
            RenderItem::Pattern(tp) => {
                pattern::rasterize_paint_pattern(
                    &mut pixels,
                    width_px,
                    height_px,
                    tp,
                    page_origin_x,
                    page_height,
                    scale,
                );
            }
            RenderItem::Text(span) => {
                render_span(
                    &mut pixels,
                    width_px,
                    height_px,
                    span,
                    page_origin_x,
                    page_height,
                    scale,
                    font_cache,
                    &mut glyph_cache,
                    use_subpixel,
                );
            }
        }
    }

    // Apply page rotation to the final pixel buffer.
    let (pixels, out_w, out_h) =
        if page_def.rotation == 90 || page_def.rotation == 180 || page_def.rotation == 270 {
            rotate_pixel_buffer(&pixels, width_px, height_px, page_def.rotation as u32)
        } else {
            (pixels, width_px, height_px)
        };

    Ok((pixels, out_w, out_h))
}

/// Coalesce adjacent spans that are part of the same word.
///
/// TJ arrays with kern adjustments create separate spans per string element,
/// producing visible gaps mid-word (e.g., "Esti" + "mation"). This function
/// merges consecutive spans when they share the same font/size and are close
/// enough that no word boundary exists between them.
///
/// The merged span's char_advances are concatenated, and the normalization
/// in render_span distributes the inter-span gap proportionally across all
/// characters, eliminating the visible gap.
fn coalesce_spans(mut spans: Vec<&PositionedSpan>) -> Vec<PositionedSpan> {
    if spans.is_empty() {
        return Vec::new();
    }

    // Sort by baseline (y_min descending, since PDF y is up) then by x position.
    spans.sort_by(|a, b| {
        let y_cmp = b
            .bbox
            .y_min
            .partial_cmp(&a.bbox.y_min)
            .unwrap_or(std::cmp::Ordering::Equal);
        if y_cmp != std::cmp::Ordering::Equal {
            return y_cmp;
        }
        a.bbox
            .x_min
            .partial_cmp(&b.bbox.x_min)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut result: Vec<PositionedSpan> = Vec::with_capacity(spans.len());

    for &span in &spans {
        let should_merge = result.last().is_some_and(|prev: &PositionedSpan| {
            // Compare font identity, preferring font_id (unique per PDF subset).
            // Two spans that share a stripped display name but reference
            // different subsets must not coalesce: their glyph programs
            // differ, and merging breaks per-subset byte-to-glyph lookups.
            let same_font = match (prev.font_id.as_deref(), span.font_id.as_deref()) {
                (Some(a), Some(b)) => a == b,
                (None, None) => prev.font_name == span.font_name,
                _ => false,
            };
            let prev_size = prev.font_size.unwrap_or(12.0);
            let span_size = span.font_size.unwrap_or(12.0);
            let same_size = (prev_size - span_size).abs() < 0.5;

            // Same baseline: y_min within 30% of font size.
            let baseline_diff = (prev.bbox.y_min - span.bbox.y_min).abs();
            let same_baseline = baseline_diff < prev_size * 0.3;

            // Gap between previous span's right edge and this span's left edge.
            let gap = span.bbox.x_min - prev.bbox.x_max;
            // Estimate character width for overlap tolerance.
            let est_char_width = prev_size * 0.6;
            // Merge when gap is small:
            // - Positive gap < 15% of font size (no word boundary)
            // - Negative gap (overlap) up to half a char width (TJ kerning)
            let small_gap = if gap < 0.0 {
                gap.abs() < est_char_width * 0.5
            } else {
                gap < prev_size * 0.15
            };

            // Don't coalesce rotated spans with horizontal ones.
            let same_rotation = (prev.rotation - span.rotation).abs() < 1.0;

            same_font && same_size && same_baseline && small_gap && same_rotation
        });

        if should_merge {
            let prev = result.last_mut().unwrap();
            // Extend text.
            prev.text.push_str(&span.text);
            // Extend bbox to cover both spans.
            prev.bbox = BoundingBox::new(
                prev.bbox.x_min,
                prev.bbox.y_min.min(span.bbox.y_min),
                span.bbox.x_max,
                prev.bbox.y_max.max(span.bbox.y_max),
            );
            // Concatenate char_advances if both have them.
            match (&mut prev.char_advances, &span.char_advances) {
                (Some(ref mut prev_ca), Some(span_ca)) => {
                    prev_ca.extend_from_slice(span_ca);
                }
                _ => {
                    // If either is missing, drop to proportional fallback.
                    prev.char_advances = None;
                }
            }
            // Concatenate char_codes if both have them.
            match (&mut prev.char_codes, &span.char_codes) {
                (Some(ref mut prev_cc), Some(span_cc)) => {
                    prev_cc.extend_from_slice(span_cc);
                }
                _ => {
                    prev.char_codes = None;
                }
            }
            // Concatenate char_gids if both have them.
            match (&mut prev.char_gids, &span.char_gids) {
                (Some(ref mut prev_cg), Some(span_cg)) => {
                    prev_cg.extend_from_slice(span_cg);
                }
                _ => {
                    prev.char_gids = None;
                }
            }
        } else {
            result.push(span.clone());
        }
    }

    result
}

/// Render a single positioned span onto the pixel buffer.
///
/// `page_origin_x` and `page_height` together define the user-space visible
/// region: pixel x=0 corresponds to user-space x = page_origin_x; pixel y=0
/// corresponds to user-space y = page_height (top of visible region).
#[allow(clippy::too_many_arguments)]
fn render_span(
    pixels: &mut [u8],
    img_width: u32,
    img_height: u32,
    span: &PositionedSpan,
    page_origin_x: f64,
    page_height: f64,
    scale: f64,
    font_cache: &mut FontCache,
    glyph_cache: &mut GlyphBitmapCache,
    use_subpixel: bool,
) {
    let font_size_pt = span.font_size.unwrap_or(12.0);
    if font_size_pt <= 0.0 {
        return;
    }

    let font_size_px = font_size_pt * scale;

    // Prefer the per-subset font_id when present (PDF backend supplies the
    // raw BaseFont name, unique per subset). Falls back to font_name so that
    // non-PDF backends and un-subsetted fonts continue to work.
    let font_name = span
        .font_id
        .as_deref()
        .or(span.font_name.as_deref())
        .unwrap_or("default");

    // Foreground color from span (default: black).
    let fg_color = match &span.color {
        Some(c) => [c.r, c.g, c.b],
        None => [0, 0, 0],
    };

    // Starting position in pixels. For horizontal text, baseline is at y_min
    // (PDF y_min = bottom of text = lower in page). For 90-degree CCW rotated
    // text, the text advance direction is upward in page space (bottom-to-top),
    // so cursor_y starts at y_min (bottom of span in page, = higher y in image)
    // and decreases (moves up in image).
    let is_rotated_90_pos = (span.rotation - 90.0).abs() < 5.0;
    let start_x_px = if is_rotated_90_pos {
        // For 90-degree rotation, the glyph ascender extends LEFT from cursor_x.
        // Use x_max as the baseline position so the glyph body fits in the bbox.
        (span.bbox.x_max - page_origin_x) * scale
    } else {
        (span.bbox.x_min - page_origin_x) * scale
    };
    let baseline_y_px = if is_rotated_90_pos {
        // Start at bottom of span (y_min in PDF = higher pixel y in image).
        // Text flows upward (decreasing pixel y) with each character.
        (page_height - span.bbox.y_min) * scale
    } else {
        (page_height - span.bbox.y_min) * scale
    };

    let units_per_em = font_cache.units_per_em(font_name) as f64;
    if units_per_em <= 0.0 {
        return;
    }
    let glyph_scale = font_size_px / units_per_em;

    let char_count = span.text.chars().count();
    // For vertical text, use bbox height as the advance extent.
    let is_rotated_90 = (span.rotation - 90.0).abs() < 5.0;
    let is_rotated_270 = (span.rotation - 270.0).abs() < 5.0 || (span.rotation + 90.0).abs() < 5.0;
    let is_vertical = is_rotated_90 || is_rotated_270;
    let span_width_px = if is_vertical {
        (span.bbox.y_max - span.bbox.y_min) * scale
    } else {
        (span.bbox.x_max - span.bbox.x_min) * scale
    };

    // Advance width strategy:
    // char_advances are in text space. Normalize to span bbox pixel width,
    // which distributes the ink extent (including the last glyph's right
    // sidebearing) across all characters proportionally. This produces
    // better visual positioning than raw advance conversion because the
    // bbox captures the actual rendered extent.
    //
    // Ligature path: when char_codes.len() < char_count, char_advances has
    // one entry per code (not per char). We still normalize to bbox width
    // and the ligature loop below iterates by codes.
    let codes_len = span.char_codes.as_ref().map(|c| c.len()).unwrap_or(0);
    let is_ligature = codes_len > 0 && codes_len < char_count;
    let advances: Vec<f64> = if let Some(ref ca) = span.char_advances {
        let ca_sum: f64 = ca.iter().sum();
        let advance_count = if is_ligature { codes_len } else { char_count };
        if ca.len() == advance_count && ca_sum > 0.0 && has_proportional_variation(ca) {
            let normalize = span_width_px / ca_sum;
            ca.iter().map(|a| a * normalize).collect()
        } else {
            proportional_advances(span, char_count, span_width_px, font_cache, glyph_scale)
        }
    } else {
        proportional_advances(span, char_count, span_width_px, font_cache, glyph_scale)
    };

    let mut cursor_x = start_x_px;
    let mut cursor_y = baseline_y_px;

    // FreeType-style advance-cursor compensation. When X-axis auto-hinting
    // snaps a glyph's left/right edges to pixel boundaries, the hinter
    // emits per-glyph `lsb_delta_fu` / `rsb_delta_fu` that capture how
    // much the snap moved those edges. Without compensation the cursor
    // drifts sub-pixel across a line because raw advance widths don't
    // know about the snap. We carry the previous glyph's rsb shift
    // forward and apply it (minus the incoming glyph's lsb shift) to
    // each composite position. When X-axis hinting is off (the default
    // UDOC_XAXIS_AUTOHINT=0 / 'stems' / 'nostems' gate path for glyphs
    // without matching criteria) every bitmap carries zero shifts and
    // this machinery is an identity no-op.
    let mut prev_rsb_shift: f64 = 0.0;

    // Cache blue zone hints per span (same font for all chars in span).
    let hint_values = font_cache.ps_hint_values(font_name);

    // Use by-code glyph lookup when char_codes match char count (no ligatures).
    let use_char_codes = span
        .char_codes
        .as_ref()
        .is_some_and(|codes| codes.len() == char_count);
    // Use GID-based lookup for CID/composite fonts.
    let use_char_gids = span
        .char_gids
        .as_ref()
        .is_some_and(|gids| gids.len() == char_count);

    let ppem = font_size_px.round() as u16;
    // Cache key uses quarter-pixel font size so glyph shape differences
    // between near-equal point sizes don't get masked by ppem rounding.
    let cache_size_key = font_size_key(font_size_px);

    // Build the (code, char) iteration sequence. For ligature spans
    // (e.g. one byte 0x1B -> "ff"), char_codes.len() < text.chars().count().
    // We iterate by codes (one glyph per byte) so the ligature glyph is
    // drawn once instead of repeating the unligated chars individually.
    // The associated `ch` is the first char of the ligature expansion --
    // used only for the glyph bitmap cache key.
    let ligature_codes: Option<Vec<(u8, char)>> = span.char_codes.as_ref().and_then(|codes| {
        if codes.len() >= char_count {
            return None;
        }
        // Pair each code with its starting char in the expanded text.
        // Without a per-code char count we approximate by giving each
        // code one char up to the last code, which absorbs the rest.
        let mut chars = span.text.chars();
        let mut result = Vec::with_capacity(codes.len());
        let last = codes.len().saturating_sub(1);
        for (i, &code) in codes.iter().enumerate() {
            let ch = if i < last {
                chars.next().unwrap_or('\u{FFFD}')
            } else {
                // Last code consumes any remaining chars. Use the first
                // for the cache key; subsequent chars from the ligature
                // expansion are skipped during iteration.
                chars.next().unwrap_or('\u{FFFD}')
            };
            result.push((code, ch));
        }
        Some(result)
    });

    // Same construction for CID-driven ligatures (composite TT fonts). When
    // a single 2-byte CID expands to multiple Unicode chars via ToUnicode
    // (e.g. macOS Quartz's "ti" ligature), iterate by gid instead of by
    // char so the right glyph gets drawn once, not the disjoint chars
    // individually via Unicode fallback.
    let ligature_gids: Option<Vec<(u16, char)>> = if ligature_codes.is_some() {
        None
    } else {
        span.char_gids.as_ref().and_then(|gids| {
            if gids.len() >= char_count {
                return None;
            }
            let mut chars = span.text.chars();
            let mut result = Vec::with_capacity(gids.len());
            for &gid in gids.iter() {
                let ch = chars.next().unwrap_or('\u{FFFD}');
                result.push((gid, ch));
            }
            Some(result)
        })
    };

    if let Some(seq) = &ligature_codes {
        for (i, &(code, ch)) in seq.iter().enumerate() {
            let char_advance = advances.get(i).copied().unwrap_or(0.0);
            if ch != ' ' {
                let outline_data = font_cache
                    .glyph_outline_by_code(font_name, code)
                    .or_else(|| font_cache.glyph_outline(font_name, ch));
                if let Some(outline_data) = outline_data {
                    render_outline_at_cursor(
                        pixels,
                        img_width,
                        img_height,
                        &outline_data,
                        font_name,
                        char::from_u32(0xF0000 + code as u32).unwrap_or('\u{FFFD}'),
                        ppem,
                        cache_size_key,
                        cursor_x,
                        cursor_y,
                        glyph_scale,
                        font_cache,
                        glyph_cache,
                        fg_color,
                        hint_values.as_ref(),
                        is_vertical,
                        is_rotated_90,
                        is_rotated_270,
                        span.is_superscript,
                        span.is_subscript,
                        use_subpixel,
                    );
                }
            }
            if is_rotated_90 {
                cursor_y -= char_advance;
            } else if is_rotated_270 {
                cursor_y += char_advance;
            } else {
                cursor_x += char_advance;
            }
        }
        return;
    }

    if let Some(seq) = &ligature_gids {
        for (i, &(gid, ch)) in seq.iter().enumerate() {
            let char_advance = advances.get(i).copied().unwrap_or(0.0);
            if ch != ' ' {
                // Try TrueType hinting by GID. This is the same fast path the
                // char iteration below uses, but keyed on the CID/gid directly
                // so ligature glyphs are drawn once (e.g. a "ti" gid from
                // macOS Quartz that ToUnicode expanded to "t" + "i").
                let hinted_bitmap = (|| {
                    let hinted = font_cache.hint_glyph(font_name, gid, ppem)?;
                    let contours: Vec<Vec<(f64, f64, bool)>> =
                        hinted_to_contours(&hinted.points, &hinted.contour_ends);
                    if contours.is_empty() {
                        return None;
                    }
                    let hx = cursor_x.round();
                    let hy = cursor_y.round();
                    if use_subpixel {
                        rasterizer::rasterize_outline_subpixel(&contours, 1.0, hx, hy, None, None)
                    } else {
                        rasterizer::rasterize_outline(&contours, 1.0, hx, hy, None, None)
                    }
                })();

                if let Some(bitmap) = hinted_bitmap {
                    composite_glyph(pixels, img_width, img_height, &bitmap, fg_color);
                } else if let Some(outline_data) = font_cache
                    .glyph_outline_by_gid(font_name, gid)
                    .or_else(|| font_cache.glyph_outline(font_name, ch))
                {
                    render_outline_at_cursor(
                        pixels,
                        img_width,
                        img_height,
                        &outline_data,
                        font_name,
                        char::from_u32(0xF0000 + gid as u32).unwrap_or('\u{FFFD}'),
                        ppem,
                        cache_size_key,
                        cursor_x,
                        cursor_y,
                        glyph_scale,
                        font_cache,
                        glyph_cache,
                        fg_color,
                        hint_values.as_ref(),
                        is_vertical,
                        is_rotated_90,
                        is_rotated_270,
                        span.is_superscript,
                        span.is_subscript,
                        use_subpixel,
                    );
                }
            }
            if is_rotated_90 {
                cursor_y -= char_advance;
            } else if is_rotated_270 {
                cursor_y += char_advance;
            } else {
                cursor_x += char_advance;
            }
        }
        return;
    }

    for (i, ch) in span.text.chars().enumerate() {
        let char_advance = advances.get(i).copied().unwrap_or(0.0);

        if ch != ' ' {
            // Try TrueType hinting first (produces pixel-coordinate outlines).
            // For simple TT fonts we prefer byte-indexed lookup because the
            // ToUnicode map (what produced `ch`) can diverge from the glyph
            // the byte actually selects -- e.g. a subset's ligature glyph
            // re-labeled via ToUnicode as U+2019.
            let gid_for_hinting: Option<u16> = if use_char_gids {
                span.char_gids.as_ref().map(|g| g[i])
            } else {
                // Prefer Unicode cmap lookup. Fall back to byte-indexed
                // lookup only when Unicode fails -- this covers simple TT
                // fonts whose ToUnicode maps bytes to unreachable Unicode
                // codepoints. Doing byte lookup unconditionally regressed
                // some macOS Quartz PDFs where the byte cmap is "creative"
                // (see 2510.03359).
                let from_char = font_cache.ttf_glyph_id(font_name, ch);
                if from_char.is_some() {
                    from_char
                } else if use_char_codes {
                    let code = span.char_codes.as_ref().unwrap()[i];
                    font_cache.ttf_glyph_id_by_byte(font_name, code)
                } else {
                    None
                }
            };

            let bin = fract_x_bin(cursor_x);
            let hinted_bitmap = gid_for_hinting.and_then(|gid| {
                let hinted = font_cache.hint_glyph(font_name, gid, ppem)?;
                // Hinted points are in pixel coords. Build contours and rasterize
                // with scale=1.0 (already scaled).
                let contours: Vec<Vec<(f64, f64, bool)>> =
                    hinted_to_contours(&hinted.points, &hinted.contour_ends);
                if contours.is_empty() {
                    return None;
                }
                // Rasterize at the bin's sub-pixel X offset so the AA pattern
                // reflects where the glyph actually lands. MuPDF uses LIGHT
                // hinting which preserves sub-pixel X positioning; rounding
                // cursor_x to the nearest integer (the old behaviour) loses
                // that resolution and pulls every glyph to a pixel boundary,
                // which diverges from MuPDF even when the hinted outline is
                // correct. Y stays at 0.0 (we composite with cursor_y.round)
                // because TT hinting pins Y to the grid.
                let hx = fract_x_for_bin(bin);
                if use_subpixel {
                    rasterizer::rasterize_outline_subpixel(&contours, 1.0, hx, 0.0, None, None)
                } else {
                    rasterizer::rasterize_outline(&contours, 1.0, hx, 0.0, None, None)
                }
            });

            if let Some(bitmap) = hinted_bitmap {
                // Apply FT-style advance compensation: subtract this glyph's
                // lsb shift, add the previous glyph's rsb shift (both 0 on
                // the TT-hint path, so identity by default).
                let composite_x =
                    rasterizer::adjust_cursor_for_shift(cursor_x, prev_rsb_shift, bitmap.lsb_shift);
                composite_glyph_at(
                    pixels,
                    img_width,
                    img_height,
                    &bitmap,
                    fg_color,
                    composite_x.floor() as i32,
                    cursor_y.round() as i32,
                );
                prev_rsb_shift = bitmap.rsb_shift;
                if is_vertical {
                    cursor_y += char_advance;
                } else {
                    cursor_x += char_advance;
                }
                continue;
            }

            // Unhinted fallback: original glyph lookup path.
            // 1. GID-based (CID/composite fonts with Identity CMap)
            // 2. By-code (subset fonts with custom encodings)
            // 3. Byte-cmap (simple TT subsets where ToUnicode maps a byte
            //    to a Unicode codepoint unreachable in the font's cmap,
            //    or unreachable via the named glyph -- e.g. macOS Quartz
            //    subsets where byte 0x27 -> U+0027 but the glyph is the
            //    "ti" ligature). We already computed gid_for_hinting via
            //    ttf_glyph_id_by_byte above; reuse it so the unhinted
            //    raster path draws the right glyph instead of falling
            //    through to Unicode and getting the literal apostrophe.
            //    Only used when the Unicode cmap lookup also fails --
            //    this preserves the existing behaviour for fonts with a
            //    working Unicode cmap.
            // 4. Unicode-based (last resort).
            let outline_data = if use_char_gids {
                let gid = span.char_gids.as_ref().unwrap()[i];
                font_cache
                    .glyph_outline_by_gid(font_name, gid)
                    .or_else(|| font_cache.glyph_outline(font_name, ch))
            } else if use_char_codes {
                let code = span.char_codes.as_ref().unwrap()[i];
                let primary = font_cache.glyph_outline_by_code(font_name, code);
                if primary.is_some() {
                    primary
                } else if let Some(gid) = gid_for_hinting.filter(|_| {
                    // M-15 ligature binding: for simple TT subsets whose
                    // ToUnicode maps a byte to a Unicode codepoint that
                    // doesn't resolve to the right glyph (e.g. macOS
                    // Quartz's byte 0x27 -> U+0027 while the glyph is
                    // actually "ti"), prefer the font's own byte cmap
                    // over the Unicode fallback. Gated on
                    //   1. Named font's Unicode cmap missing this char
                    //      (so the Unicode lookup would hit Liberation
                    //      substitution anyway), and
                    //   2. Font has hinting tables (proxy for
                    //      well-formed subset). Ad-hoc subsets that
                    //      drop hinting tend to populate cmap(1,0)
                    //      arbitrarily, so byte-lookup there
                    //      substitutes a random glyph from a different
                    //      subset (see 2508.13201 ArialMT).
                    font_cache.ttf_glyph_id(font_name, ch).is_none()
                        && font_cache.ttf_has_hinting(font_name)
                }) {
                    font_cache
                        .glyph_outline_by_gid(font_name, gid)
                        .or_else(|| font_cache.glyph_outline(font_name, ch))
                } else {
                    font_cache.glyph_outline(font_name, ch)
                }
            } else {
                font_cache.glyph_outline(font_name, ch)
            };
            if let Some(outline_data) = outline_data {
                // For rotated text, rotate the glyph outline points.
                let contours: Vec<Vec<(f64, f64, bool)>> = if is_vertical {
                    outline_data
                        .contours
                        .iter()
                        .map(|c| {
                            c.points
                                .iter()
                                .map(|p| {
                                    if is_rotated_90 {
                                        // 90 degrees CCW: (x,y) -> (-y, x)
                                        (-p.y, p.x, p.on_curve)
                                    } else {
                                        // 270 degrees (or -90): (x,y) -> (y, -x)
                                        (p.y, -p.x, p.on_curve)
                                    }
                                })
                                .collect()
                        })
                        .collect()
                } else {
                    outline_data
                        .contours
                        .iter()
                        .map(|c| c.points.iter().map(|p| (p.x, p.y, p.on_curve)).collect())
                        .collect()
                };

                // Cache key: font name pointer + character + ppem + sub-pixel bin.
                // The font name's heap address is stable for the lifetime of the cache.
                // The sub-pixel bin captures the cursor's fractional X so glyphs
                // landing at different sub-pixel offsets keep distinct AA patterns
                // instead of being snapped to one template.
                let bin = fract_x_bin(cursor_x);
                let cache_key = (font_name.as_ptr() as usize, ch, cache_size_key, bin);

                let cached = glyph_cache.get(&cache_key);
                if let Some(template) = cached {
                    // Apply FT-style advance compensation before compositing
                    // the cached bitmap. With X-axis auto-hinting OFF (the
                    // default), template.lsb_shift/rsb_shift are 0 and
                    // prev_rsb_shift stays 0 across the loop, so this
                    // simplifies to floor(cursor_x) -- unchanged from the
                    // pre-M-38 baseline. With X-axis ON, the compensation
                    // chain prevents the sub-pixel inter-glyph drift that
                    // the  Round-9 diagnosis identified as the dominant
                    // X-axis-re-enable regressor.
                    let composite_x = rasterizer::adjust_cursor_for_shift(
                        cursor_x,
                        prev_rsb_shift,
                        template.lsb_shift,
                    );
                    let prev_rsb_after = template.rsb_shift;
                    composite_glyph_at(
                        pixels,
                        img_width,
                        img_height,
                        template,
                        fg_color,
                        composite_x.floor() as i32,
                        cursor_y.round() as i32,
                    );
                    prev_rsb_shift = prev_rsb_after;
                } else {
                    // Auto-hint the outline using topology-based analysis.
                    // Skip for small glyphs (ppem < 10): blue zone fitting at
                    // small sizes distorts glyph shapes (tapers the base of
                    // numerals like "1" because baseline snap is too aggressive
                    // relative to the glyph size). This catches superscripts
                    // regardless of how they're implemented (text rise vs text
                    // matrix positioning).

                    // Apply PS vstem hints for X-axis FIRST on the original
                    // contours, then auto-hint Y-axis on the result. Order
                    // matters: auto-hinting shifts Y on bezier control points,
                    // which changes flattened curve X positions and corrupts
                    // the X-axis snap if done in the other order.
                    let x_hinted: Vec<Vec<(f64, f64, bool)>>;
                    let contours_base = if !outline_data.stem_hints.v_stems.is_empty() {
                        let x_only = StemHints {
                            h_stems: Vec::new(),
                            v_stems: outline_data.stem_hints.v_stems.clone(),
                        };
                        x_hinted =
                            ps_hints::ps_hint_glyph(&contours, Some(&x_only), None, glyph_scale);
                        &x_hinted
                    } else {
                        &contours
                    };

                    // Auto-hint. Y-axis is always active. X-axis is
                    // PERMANENTLY DISABLED by default /
                    // (#207 closed). The  FT byte-exact
                    // port of `af_latin_hint_edges` closed the per-glyph
                    // stem-fit divergence (Liberation Sans Regular
                    // convergence 23% -> 56% of tuples within 2 fu of
                    // FreeType), but MuPDF's rendering oracle uses
                    // FT-LIGHT hinting (which zeroes X-axis fitting
                    // entirely) so matching FT-NORMAL regresses aggregate
                    // 100-doc SSIM by ~0.021 at 150 DPI and ~0.010 at
                    // 300 DPI. We ship the FT-byte-exact path behind
                    // `UDOC_XAXIS_AUTOHINT=1` for cursor-pair
                    // correctness tooling but keep it off the default
                    // renderer path.
                    //
                    // Modes still supported for diagnostics:
                    //   "1" | "both": enable X-axis for all fonts
                    //   "nostems":    enable X-axis only for fonts
                    //                 without declared v_stems
                    //   "stems":      enable X-axis only for fonts with
                    //                 declared v_stems
                    //   unset / "0":  DEFAULT -- X-axis off (matches MuPDF LIGHT)
                    let has_x_hints = !outline_data.stem_hints.v_stems.is_empty();
                    let xaxis_mode = std::env::var("UDOC_XAXIS_AUTOHINT")
                        .ok()
                        .unwrap_or_default();
                    let enable_x_axis = match xaxis_mode.as_str() {
                        "1" | "both" => true,
                        "nostems" => !has_x_hints,
                        "stems" => has_x_hints,
                        // "", "0", or any other value: X-axis OFF (default).
                        _ => false,
                    };
                    let auto_hinted = if ppem < 10 || span.is_superscript || span.is_subscript {
                        None
                    } else {
                        let axes = if enable_x_axis {
                            auto_hinter::HintAxes::Both
                        } else {
                            auto_hinter::HintAxes::Y
                        };
                        font_cache.auto_hint_metrics(font_name).map(|m| {
                            auto_hinter::auto_hint_glyph_axes(contours_base, m, glyph_scale, axes)
                        })
                    };
                    let (lsb_shift_px, rsb_shift_px) = match &auto_hinted {
                        Some(h) => (h.lsb_delta_fu * glyph_scale, h.rsb_delta_fu * glyph_scale),
                        None => (0.0, 0.0),
                    };
                    let contours_fallback = contours_base.clone();
                    let contours_for_raster: &Vec<Vec<(f64, f64, bool)>> = match &auto_hinted {
                        Some(h) => &h.contours,
                        None => &contours_fallback,
                    };

                    // X-axis handled above (vstems) OR by auto-hinter,
                    // Y-axis by auto-hinter. Only pass full PS hints when
                    // neither applied.
                    let (hints, hv) = if auto_hinted.is_some() || has_x_hints {
                        (None, None)
                    } else {
                        let h = if outline_data.stem_hints.h_stems.is_empty()
                            && outline_data.stem_hints.v_stems.is_empty()
                        {
                            None
                        } else {
                            Some(&outline_data.stem_hints)
                        };
                        (h, hint_values.as_ref())
                    };

                    // Rasterize at the bin's sub-pixel X offset so the AA
                    // pattern reflects where the glyph actually lands. The
                    // composite below uses floor(cursor_x) to match.
                    let raster_x_offset = fract_x_for_bin(bin);
                    let rasterize_fn = if use_subpixel {
                        rasterizer::rasterize_outline_subpixel
                    } else {
                        rasterizer::rasterize_outline
                    };
                    if let Some(mut bitmap) = rasterize_fn(
                        contours_for_raster,
                        glyph_scale,
                        raster_x_offset,
                        0.0,
                        hints,
                        hv,
                    ) {
                        bitmap.lsb_shift = lsb_shift_px;
                        bitmap.rsb_shift = rsb_shift_px;
                        // Cache then composite with position offset (no clone).
                        glyph_cache.insert(cache_key, bitmap);
                        let cached_bitmap = glyph_cache.get(&cache_key).unwrap();
                        // FT-style advance compensation (M-38). See the
                        // cached path above for identity-when-X-off rationale.
                        let composite_x = rasterizer::adjust_cursor_for_shift(
                            cursor_x,
                            prev_rsb_shift,
                            cached_bitmap.lsb_shift,
                        );
                        let prev_rsb_after = cached_bitmap.rsb_shift;
                        composite_glyph_at(
                            pixels,
                            img_width,
                            img_height,
                            cached_bitmap,
                            fg_color,
                            composite_x.floor() as i32,
                            cursor_y.round() as i32,
                        );
                        prev_rsb_shift = prev_rsb_after;
                    }
                }
            } else {
                let rect_w = char_advance.max(1.0) as u32;
                let rect_h = font_size_px as u32;
                draw_rect(
                    pixels,
                    img_width,
                    img_height,
                    cursor_x as i32,
                    cursor_y as i32,
                    rect_w,
                    rect_h,
                    180,
                );
                // Missing outline: no hinter shifts to carry forward.
                prev_rsb_shift = 0.0;
            }
        } else {
            // Space character has no outline and no hinter shifts.
            prev_rsb_shift = 0.0;
        }

        if is_rotated_90 {
            cursor_y -= char_advance; // 90-degree CCW: text flows upward
        } else if is_rotated_270 {
            cursor_y += char_advance; // 270-degree: text flows downward
        } else {
            cursor_x += char_advance;
        }
    }
}

/// Rasterize and composite a single glyph outline at the cursor position.
/// Used by the ligature-render path; mirrors the inline body of `render_span`
/// but factored so both call sites share the same hint/cache logic.
#[allow(clippy::too_many_arguments)]
fn render_outline_at_cursor(
    pixels: &mut [u8],
    img_width: u32,
    img_height: u32,
    outline_data: &udoc_font::ttf::GlyphOutline,
    font_name: &str,
    cache_ch: char,
    ppem: u16,
    cache_size_key: u16,
    cursor_x: f64,
    cursor_y: f64,
    glyph_scale: f64,
    font_cache: &mut font_cache::FontCache,
    glyph_cache: &mut GlyphBitmapCache,
    fg_color: [u8; 3],
    hint_values: Option<&udoc_font::type1::Type1HintValues>,
    is_vertical: bool,
    is_rotated_90: bool,
    is_rotated_270: bool,
    is_superscript: bool,
    is_subscript: bool,
    use_subpixel: bool,
) {
    let _ = (is_rotated_270, is_vertical); // ligature path: horizontal text only
    let contours: Vec<Vec<(f64, f64, bool)>> = if is_rotated_90 {
        outline_data
            .contours
            .iter()
            .map(|c| c.points.iter().map(|p| (-p.y, p.x, p.on_curve)).collect())
            .collect()
    } else {
        outline_data
            .contours
            .iter()
            .map(|c| c.points.iter().map(|p| (p.x, p.y, p.on_curve)).collect())
            .collect()
    };
    let bin = fract_x_bin(cursor_x);
    let cache_key = (font_name.as_ptr() as usize, cache_ch, cache_size_key, bin);
    if let Some(template) = glyph_cache.get(&cache_key) {
        composite_glyph_at(
            pixels,
            img_width,
            img_height,
            template,
            fg_color,
            cursor_x.floor() as i32,
            cursor_y.round() as i32,
        );
        return;
    }
    let x_hinted: Vec<Vec<(f64, f64, bool)>>;
    let contours_base = if !outline_data.stem_hints.v_stems.is_empty() {
        let x_only = StemHints {
            h_stems: Vec::new(),
            v_stems: outline_data.stem_hints.v_stems.clone(),
        };
        x_hinted = ps_hints::ps_hint_glyph(&contours, Some(&x_only), None, glyph_scale);
        &x_hinted
    } else {
        &contours
    };
    let auto_hinted = if ppem < 10 || is_superscript || is_subscript {
        None
    } else {
        font_cache
            .auto_hint_metrics(font_name)
            .map(|m| auto_hinter::auto_hint_glyph(contours_base, m, glyph_scale))
    };
    let contours_for_raster = auto_hinted.as_ref().unwrap_or(contours_base);
    let has_x_hints = !outline_data.stem_hints.v_stems.is_empty();
    let (hints, hv) = if auto_hinted.is_some() || has_x_hints {
        (None, None)
    } else {
        let h = if outline_data.stem_hints.h_stems.is_empty()
            && outline_data.stem_hints.v_stems.is_empty()
        {
            None
        } else {
            Some(&outline_data.stem_hints)
        };
        (h, hint_values)
    };
    let raster_x_offset = fract_x_for_bin(bin);
    let rasterize_fn = if use_subpixel {
        rasterizer::rasterize_outline_subpixel
    } else {
        rasterizer::rasterize_outline
    };
    if let Some(bitmap) = rasterize_fn(
        contours_for_raster,
        glyph_scale,
        raster_x_offset,
        0.0,
        hints,
        hv,
    ) {
        glyph_cache.insert(cache_key, bitmap);
        let cached_bitmap = glyph_cache.get(&cache_key).unwrap();
        composite_glyph_at(
            pixels,
            img_width,
            img_height,
            cached_bitmap,
            fg_color,
            cursor_x.floor() as i32,
            cursor_y.round() as i32,
        );
    }
}

/// Render an image onto the pixel buffer using nearest-neighbor scaling.
/// Only handles Raw (decoded) images. JPEG/JBIG2/CCITT are skipped.
#[allow(clippy::too_many_arguments)]
fn render_image(
    pixels: &mut [u8],
    img_width: u32,
    img_height: u32,
    placement: &ImagePlacement,
    page_origin_x: f64,
    page_height: f64,
    scale: f64,
    doc: &Document,
) {
    let images = doc.assets.images();
    let asset = match images.get(placement.asset_index) {
        Some(a) => a,
        None => return,
    };

    // Image mask: paint the fill color where mask bits are set.
    if placement.is_mask {
        render_image_mask(
            pixels,
            img_width,
            img_height,
            placement,
            page_origin_x,
            page_height,
            scale,
            &asset.data,
        );
        return;
    }

    // Decode image data to raw pixels based on filter type.
    let (pixel_data, src_w, src_h, bpp): (std::borrow::Cow<'_, [u8]>, usize, usize, usize) =
        match asset.filter {
            udoc_core::image::ImageFilter::Raw => {
                let w = placement.width as usize;
                let h = placement.height as usize;
                if w == 0 || h == 0 {
                    return;
                }
                let bpp: usize = match placement.color_space.as_deref() {
                    Some("DeviceGray") | Some("CalGray") => 1,
                    Some("DeviceCMYK") => 4,
                    _ => 3,
                };
                // Asset data is one byte per component per pixel, regardless
                // of the source PDF's bits_per_component. CCITT/JBIG2 decoders
                // unpack 1bpc bilevel data to one byte per pixel (0=black,
                // 255=white) before the renderer sees it.
                if asset.data.len() < w * h * bpp {
                    return;
                }
                (std::borrow::Cow::Borrowed(&asset.data), w, h, bpp)
            }
            udoc_core::image::ImageFilter::Jpeg => match decode_jpeg(&asset.data) {
                Some((data, w, h, bpp)) => (std::borrow::Cow::Owned(data), w, h, bpp),
                None => return,
            },
            udoc_core::image::ImageFilter::Jpeg2000 => {
                match decode_jpeg2000(&asset.data, placement.color_space.as_deref()) {
                    Some((data, w, h, bpp)) => (std::borrow::Cow::Owned(data), w, h, bpp),
                    None => return,
                }
            }
            // CCITT decoding requires /DecodeParms (K, Columns, BlackIs1) which
            // are not yet plumbed through the image pipeline. Without correct
            // params, decoding produces garbage. Disabled until params are available.
            // The ccitt module is ready for when we wire up DecodeParms.
            // JBIG2, CCITT, etc. not supported yet.
            _ => return,
        };

    // If the placement CTM carries a rotation or shear (nonzero b/c), the
    // AABB-based fast path below would stretch the source into the bbox,
    // losing the rotation baked into the content-stream CTM (ia-english
    // comic books pre-rotate image CTMs so the portrait-oriented content
    // lands correctly when the page's /Rotate is applied). Detect this
    // and take the inverse-affine path instead: for every pixel in the
    // AABB, invert the 2x2 linear part of the CTM (after y-flip + scale)
    // to sample the source pixel, letting the rotation survive.
    let ctm = placement.ctm;
    let has_rotation = ctm[1].abs() > 1e-6 || ctm[2].abs() > 1e-6;
    if has_rotation && bpp == 3 {
        render_image_affine(
            pixels,
            img_width,
            img_height,
            placement,
            page_origin_x,
            page_height,
            scale,
            &pixel_data,
            src_w,
            src_h,
            bpp,
        );
        return;
    }

    // Destination rectangle in pixel coordinates, kept as floats.
    // M-24: truncating to i32 pulled the edges to the nearest integer
    // below, causing a 1-2 row/column offset vs. MuPDF when the bbox
    // lands on fractional pixels. We iterate integer destination pixels
    // that overlap the float rect and attenuate the alpha by the fraction
    // of each pixel that's inside the rect (same algorithm as fill_rect_aa
    // for shapes). Round-to-nearest (M-20's Option A) regressed on some
    // docs because it discretized the edge without also blending; this
    // approach is symmetric and edge-independent.
    let dst_x_f0 = (placement.bbox.x_min - page_origin_x) * scale;
    let dst_x_f1 = (placement.bbox.x_max - page_origin_x) * scale;
    let dst_y_f0 = (page_height - placement.bbox.y_max) * scale;
    let dst_y_f1 = (page_height - placement.bbox.y_min) * scale;
    let dst_w_f = (dst_x_f1 - dst_x_f0).max(1e-9);
    let dst_h_f = (dst_y_f1 - dst_y_f0).max(1e-9);

    // Integer pixel range that may overlap the float rect.
    // floor(low) and ceil(high) ensure we cover any pixel with nonzero
    // coverage. Clip to framebuffer at iteration time.
    let px_lo = dst_x_f0.floor() as i32;
    let px_hi = dst_x_f1.ceil() as i32; // exclusive
    let py_lo = dst_y_f0.floor() as i32;
    let py_hi = dst_y_f1.ceil() as i32;
    if px_hi <= px_lo || py_hi <= py_lo {
        return;
    }

    // Per M-20: box-average when src is >= 2x dst (photos, figures).
    // Matches MuPDF/Poppler softened-edge behavior. Nearest-neighbor for
    // upscale or near-1x keeps small icons and stencils crisp.
    let scale_x = src_w as f64 / dst_w_f;
    let scale_y = src_h as f64 / dst_h_f;
    let use_box = scale_x >= 2.0 || scale_y >= 2.0;

    // Decode a single source pixel at (sx, sy) to RGB.
    let decode_rgb = |sx: usize, sy: usize| -> (u8, u8, u8) {
        let src_idx = (sy * src_w + sx) * bpp;
        match bpp {
            1 => {
                let v = pixel_data[src_idx];
                (v, v, v)
            }
            4 => {
                let c = pixel_data[src_idx] as f32 / 255.0;
                let m = pixel_data[src_idx + 1] as f32 / 255.0;
                let y = pixel_data[src_idx + 2] as f32 / 255.0;
                let k = pixel_data[src_idx + 3] as f32 / 255.0;
                (
                    (255.0 * (1.0 - c) * (1.0 - k)) as u8,
                    (255.0 * (1.0 - m) * (1.0 - k)) as u8,
                    (255.0 * (1.0 - y) * (1.0 - k)) as u8,
                )
            }
            _ => (
                pixel_data[src_idx],
                pixel_data[src_idx + 1],
                pixel_data[src_idx + 2],
            ),
        }
    };

    // Pixel-center inside test. Matches MuPDF/Poppler: a destination pixel
    // participates in the image if its geometric center (px+0.5, py+0.5)
    // falls inside the float destination rectangle. Fractional-edge
    // coverage (earlier iteration) over-dimmed the edge row because a
    // dark source pixel was alpha-blended against white -- MuPDF instead
    // paints the full source value. The difference between the two
    // conventions is the sub-pixel snap at the rect boundary, which is
    // what we want to match.
    for py in py_lo..py_hi {
        if py < 0 || py >= img_height as i32 {
            continue;
        }
        let py_center = py as f64 + 0.5;
        if py_center < dst_y_f0 || py_center > dst_y_f1 {
            continue;
        }
        // Map the destination pixel's full [py, py+1) range back to
        // source coords for box-averaging. Anchor on the pixel center
        // so the mapping is symmetric with MuPDF: the center is the
        // nominal sample location, and we average +/- half a dest
        // pixel's worth of source.
        let pyf0 = py as f64;
        let pyf1 = pyf0 + 1.0;
        let rel_y0 = ((pyf0 - dst_y_f0) * scale_y).max(0.0);
        let rel_y1 = ((pyf1 - dst_y_f0) * scale_y).min(src_h as f64);
        let sy_center = (((py_center - dst_y_f0) * scale_y) as usize).min(src_h.saturating_sub(1));

        for px in px_lo..px_hi {
            if px < 0 || px >= img_width as i32 {
                continue;
            }
            let px_center = px as f64 + 0.5;
            if px_center < dst_x_f0 || px_center > dst_x_f1 {
                continue;
            }
            let pxf0 = px as f64;
            let pxf1 = pxf0 + 1.0;
            let rel_x0 = ((pxf0 - dst_x_f0) * scale_x).max(0.0);
            let rel_x1 = ((pxf1 - dst_x_f0) * scale_x).min(src_w as f64);
            let sx_center =
                (((px_center - dst_x_f0) * scale_x) as usize).min(src_w.saturating_sub(1));

            let (r, g, b) = if use_box {
                // Uniform box average over the source rectangle under
                // the dest pixel. Clamped to src bounds so edge pixels
                // see a half-sized (or smaller) box at the image border
                // rather than reading past the end. Uniform (vs. area-
                // weighted) tracked MuPDF more closely on the arxiv-bio
                // fixture set at 150 DPI.
                let sx0 = (rel_x0 as usize).min(src_w - 1);
                let sy0 = (rel_y0 as usize).min(src_h - 1);
                let sx1 = (rel_x1.ceil() as usize).min(src_w).max(sx0 + 1);
                let sy1 = (rel_y1.ceil() as usize).min(src_h).max(sy0 + 1);
                let mut rs: u32 = 0;
                let mut gs: u32 = 0;
                let mut bs: u32 = 0;
                let mut n: u32 = 0;
                for yy in sy0..sy1 {
                    for xx in sx0..sx1 {
                        let (rr, gg, bb) = decode_rgb(xx, yy);
                        rs += rr as u32;
                        gs += gg as u32;
                        bs += bb as u32;
                        n += 1;
                    }
                }
                match rs.checked_div(n) {
                    Some(r) => {
                        let g = gs / n;
                        let b = bs / n;
                        (r as u8, g as u8, b as u8)
                    }
                    None => decode_rgb(sx_center, sy_center),
                }
            } else {
                decode_rgb(sx_center, sy_center)
            };

            let dst_idx = (py as usize * img_width as usize + px as usize) * 3;
            if dst_idx + 2 >= pixels.len() {
                continue;
            }

            // Apply soft mask alpha blending if present. Unlike the
            // previous coverage-based approach, we paint the source
            // color fully -- only the soft mask modulates opacity.
            let mask_alpha = if let Some(ref mask) = placement.soft_mask {
                let mw = placement.soft_mask_width as usize;
                let mh = placement.soft_mask_height as usize;
                if mw > 0 && mh > 0 {
                    let mx = (sx_center * mw / src_w).min(mw - 1);
                    let my = (sy_center * mh / src_h).min(mh - 1);
                    let mi = my * mw + mx;
                    if mi < mask.len() {
                        mask[mi]
                    } else {
                        255u8
                    }
                } else {
                    255u8
                }
            } else {
                255u8
            };
            if mask_alpha == 0 {
                continue;
            }
            if mask_alpha == 255 {
                pixels[dst_idx] = r;
                pixels[dst_idx + 1] = g;
                pixels[dst_idx + 2] = b;
            } else {
                let a = mask_alpha as u16;
                let inv = 255 - a;
                pixels[dst_idx] = ((pixels[dst_idx] as u16 * inv + r as u16 * a) / 255) as u8;
                pixels[dst_idx + 1] =
                    ((pixels[dst_idx + 1] as u16 * inv + g as u16 * a) / 255) as u8;
                pixels[dst_idx + 2] =
                    ((pixels[dst_idx + 2] as u16 * inv + b as u16 * a) / 255) as u8;
            }
        }
    }
}

/// Render a rotated / sheared image using the placement's full affine CTM.
///
/// The CTM `[a, b, c, d, e, f]` maps the source unit square `(0,0)-(1,1)` to
/// page user space (y-up, points). Composed with the y-flip and DPI scale,
/// every destination pixel `(px, py)` is mapped back to a source sample
/// `(sx, sy)` via the inverse 2x2 linear part plus translation. This
/// preserves rotations baked into the content stream (e.g. ia-english
/// comic books whose pages pre-rotate the scanned image 90 degrees so
/// the page /Rotate can cancel it at display time).
///
/// Assumes 3-byte-per-pixel source data; the mask / CMYK / Gray paths
/// remain on the AABB fast path for now.
#[allow(clippy::too_many_arguments)]
fn render_image_affine(
    pixels: &mut [u8],
    img_width: u32,
    img_height: u32,
    placement: &ImagePlacement,
    page_origin_x: f64,
    page_height: f64,
    scale: f64,
    pixel_data: &[u8],
    src_w: usize,
    src_h: usize,
    bpp: usize,
) {
    let [a, b, c, d, e, f] = placement.ctm;
    // Invert the 2x2 linear part [a c; b d].
    let det = a * d - b * c;
    if det.abs() < 1e-9 {
        return;
    }
    let inv_det = 1.0 / det;
    // Inverse: user_to_src (x, y) -> source (u, v) in unit square:
    //   u = inv_det * ( d * (x - e) - c * (y - f))
    //   v = inv_det * (-b * (x - e) + a * (y - f))
    let a_inv = d * inv_det;
    let c_inv = -c * inv_det;
    let b_inv = -b * inv_det;
    let d_inv = a * inv_det;

    // Scan the AABB in pixel coords. Destination pixel center (px+0.5,
    // py+0.5) -> user space (ux, uy) via inverse y-flip + scale + origin
    // shift. Then user -> unit square via inv(CTM_linear). Then unit ->
    // source pixel via (u * src_w, v * src_h).
    let dst_x_f0 = (placement.bbox.x_min - page_origin_x) * scale;
    let dst_x_f1 = (placement.bbox.x_max - page_origin_x) * scale;
    let dst_y_f0 = (page_height - placement.bbox.y_max) * scale;
    let dst_y_f1 = (page_height - placement.bbox.y_min) * scale;
    let px_lo = dst_x_f0.floor() as i32;
    let px_hi = dst_x_f1.ceil() as i32;
    let py_lo = dst_y_f0.floor() as i32;
    let py_hi = dst_y_f1.ceil() as i32;
    if px_hi <= px_lo || py_hi <= py_lo {
        return;
    }

    let src_w_f = src_w as f64;
    let src_h_f = src_h as f64;
    let inv_scale = 1.0 / scale;

    for py in py_lo..py_hi {
        if py < 0 || py >= img_height as i32 {
            continue;
        }
        let py_center = py as f64 + 0.5;
        let uy = page_height - py_center * inv_scale;
        for px in px_lo..px_hi {
            if px < 0 || px >= img_width as i32 {
                continue;
            }
            let px_center = px as f64 + 0.5;
            let ux = px_center * inv_scale + page_origin_x;
            // (ux, uy) in page user space. Map to source unit square.
            let dx = ux - e;
            let dy = uy - f;
            let u = a_inv * dx + c_inv * dy;
            let v = b_inv * dx + d_inv * dy;
            if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                continue;
            }
            // PDF image source has y-down convention in the source data
            // (row 0 is top of image), but the unit-square v axis runs
            // 0..1 in user-space y-up. Flip v.
            let v_flipped = 1.0 - v;
            let sx = (u * src_w_f) as usize;
            let sy = (v_flipped * src_h_f) as usize;
            let sx = sx.min(src_w - 1);
            let sy = sy.min(src_h - 1);
            let src_idx = (sy * src_w + sx) * bpp;
            if src_idx + 2 >= pixel_data.len() {
                continue;
            }
            let (r, g, b) = match bpp {
                1 => {
                    let v = pixel_data[src_idx];
                    (v, v, v)
                }
                4 => {
                    let c = pixel_data[src_idx] as f32 / 255.0;
                    let m = pixel_data[src_idx + 1] as f32 / 255.0;
                    let y = pixel_data[src_idx + 2] as f32 / 255.0;
                    let k = pixel_data[src_idx + 3] as f32 / 255.0;
                    (
                        (255.0 * (1.0 - c) * (1.0 - k)) as u8,
                        (255.0 * (1.0 - m) * (1.0 - k)) as u8,
                        (255.0 * (1.0 - y) * (1.0 - k)) as u8,
                    )
                }
                _ => (
                    pixel_data[src_idx],
                    pixel_data[src_idx + 1],
                    pixel_data[src_idx + 2],
                ),
            };

            let mask_alpha = if let Some(ref mask) = placement.soft_mask {
                let mw = placement.soft_mask_width as usize;
                let mh = placement.soft_mask_height as usize;
                if mw > 0 && mh > 0 {
                    let mx = (sx * mw / src_w).min(mw - 1);
                    let my = (sy * mh / src_h).min(mh - 1);
                    let mi = my * mw + mx;
                    if mi < mask.len() {
                        mask[mi]
                    } else {
                        255u8
                    }
                } else {
                    255u8
                }
            } else {
                255u8
            };
            if mask_alpha == 0 {
                continue;
            }

            let dst_idx = (py as usize * img_width as usize + px as usize) * 3;
            if dst_idx + 2 >= pixels.len() {
                continue;
            }
            if mask_alpha == 255 {
                pixels[dst_idx] = r;
                pixels[dst_idx + 1] = g;
                pixels[dst_idx + 2] = b;
            } else {
                let a = mask_alpha as u16;
                let inv = 255 - a;
                pixels[dst_idx] = ((pixels[dst_idx] as u16 * inv + r as u16 * a) / 255) as u8;
                pixels[dst_idx + 1] =
                    ((pixels[dst_idx + 1] as u16 * inv + g as u16 * a) / 255) as u8;
                pixels[dst_idx + 2] =
                    ((pixels[dst_idx + 2] as u16 * inv + b as u16 * a) / 255) as u8;
            }
        }
    }
}

/// Decode JPEG data to raw RGB pixels. Returns (data, width, height, bpp).
/// Render a stencil image mask. The mask data is 1-byte-per-pixel
/// grayscale where 0x00 = paint the mask color, 0xFF = transparent.
/// This is used for CCITT-encoded scanned documents where the page
/// content is a 1-bit image mask painted with the fill color.
#[allow(clippy::too_many_arguments)]
fn render_image_mask(
    pixels: &mut [u8],
    img_width: u32,
    img_height: u32,
    placement: &ImagePlacement,
    page_origin_x: f64,
    page_height: f64,
    scale: f64,
    mask_data: &[u8],
) {
    let src_w = placement.width as usize;
    let src_h = placement.height as usize;
    if src_w == 0 || src_h == 0 || mask_data.is_empty() {
        return;
    }

    let color = placement.mask_color;
    // M-24: match render_image's pixel-center-inside test. A dest pixel
    // participates in the stencil if its center lies inside the float
    // destination rect, and is painted with full mask color rather than
    // coverage-dimmed. This matches MuPDF/Poppler at fractional edges.
    let dst_x_f0 = (placement.bbox.x_min - page_origin_x) * scale;
    let dst_x_f1 = (placement.bbox.x_max - page_origin_x) * scale;
    let dst_y_f0 = (page_height - placement.bbox.y_max) * scale;
    let dst_y_f1 = (page_height - placement.bbox.y_min) * scale;
    let dst_w_f = (dst_x_f1 - dst_x_f0).max(1e-9);
    let dst_h_f = (dst_y_f1 - dst_y_f0).max(1e-9);
    let scale_x = src_w as f64 / dst_w_f;
    let scale_y = src_h as f64 / dst_h_f;

    let px_lo = dst_x_f0.floor() as i32;
    let px_hi = dst_x_f1.ceil() as i32;
    let py_lo = dst_y_f0.floor() as i32;
    let py_hi = dst_y_f1.ceil() as i32;
    if px_hi <= px_lo || py_hi <= py_lo {
        return;
    }

    for py in py_lo..py_hi {
        if py < 0 || py >= img_height as i32 {
            continue;
        }
        let py_center = py as f64 + 0.5;
        if py_center < dst_y_f0 || py_center > dst_y_f1 {
            continue;
        }
        let sy = (((py_center - dst_y_f0) * scale_y) as usize).min(src_h - 1);

        for px in px_lo..px_hi {
            if px < 0 || px >= img_width as i32 {
                continue;
            }
            let px_center = px as f64 + 0.5;
            if px_center < dst_x_f0 || px_center > dst_x_f1 {
                continue;
            }
            let sx = (((px_center - dst_x_f0) * scale_x) as usize).min(src_w - 1);

            let src_idx = sy * src_w + sx;
            if src_idx >= mask_data.len() {
                continue;
            }
            let mask_val = mask_data[src_idx];
            if mask_val > 128 {
                continue; // white = transparent
            }

            let idx = (py as usize * img_width as usize + px as usize) * 3;
            if idx + 2 >= pixels.len() {
                continue;
            }
            pixels[idx] = color[0];
            pixels[idx + 1] = color[1];
            pixels[idx + 2] = color[2];
        }
    }
}

fn decode_jpeg(data: &[u8]) -> Option<(Vec<u8>, usize, usize, usize)> {
    let mut decoder = jpeg_decoder::Decoder::new(data);
    let pixels = decoder.decode().ok()?;
    let info = decoder.info()?;
    let w = info.width as usize;
    let h = info.height as usize;
    if w == 0 || h == 0 {
        return None;
    }
    match info.pixel_format {
        jpeg_decoder::PixelFormat::L8 => Some((pixels, w, h, 1)),
        jpeg_decoder::PixelFormat::RGB24 => Some((pixels, w, h, 3)),
        jpeg_decoder::PixelFormat::L16 => {
            // 16-bit grayscale: downsample to 8-bit.
            let mut rgb = Vec::with_capacity(w * h);
            for chunk in pixels.chunks_exact(2) {
                rgb.push(chunk[0]); // Take high byte.
            }
            Some((rgb, w, h, 1))
        }
        jpeg_decoder::PixelFormat::CMYK32 => {
            // PDF-embedded CMYK JPEGs are Adobe-style: `chunk[i] = 0` means
            // maximum ink, `chunk[i] = 255` means no ink. jpeg_decoder
            // preserves that Adobe convention. The standard CMYK ink amount
            // is therefore `C_ink = 255 - chunk[0]`, etc.
            //
            // MuPDF (and Poppler without ICC) converts DeviceCMYK to DeviceRGB
            // using the classical subtractive formula from PDF 1.7 §10.3.2.3:
            //     R = 1 - min(1, C_ink + K_ink)
            //     G = 1 - min(1, M_ink + K_ink)
            //     B = 1 - min(1, Y_ink + K_ink)
            // Substituting C_ink = 255 - chunk[0] and K_ink = 255 - chunk[3]:
            //     R = max(0, chunk[0] + chunk[3] - 255)
            //
            // The older multiplicative form `R = c * k / 255` (equivalent to
            // `(1-C_ink)(1-K_ink)`) is gentler on mid-tones but keeps every
            // inked pixel well above zero. That left shadow regions on the
            // Alberts cover at ~(55, 50, 35) where MuPDF paints pure black;
            // swapping to the classical formula tightens dark tones and pulls
            // SSIM up (T2-ALBERTS-COLOR  artifact).
            let mut rgb = Vec::with_capacity(w * h * 3);
            for chunk in pixels.chunks_exact(4) {
                let c = chunk[0] as i32;
                let m = chunk[1] as i32;
                let y = chunk[2] as i32;
                let k = chunk[3] as i32;
                rgb.push((c + k - 255).clamp(0, 255) as u8);
                rgb.push((m + k - 255).clamp(0, 255) as u8);
                rgb.push((y + k - 255).clamp(0, 255) as u8);
            }
            Some((rgb, w, h, 3))
        }
    }
}

/// Decode JPEG 2000 data to raw pixels. Returns (data, width, height, bpp).
/// `pdf_color_space` is the PDF's /ColorSpace for this image (used as fallback
/// when the J2K codestream color space is ambiguous).
fn decode_jpeg2000(
    data: &[u8],
    pdf_color_space: Option<&str>,
) -> Option<(Vec<u8>, usize, usize, usize)> {
    use hayro_jpeg2000::{ColorSpace, DecodeSettings, Image};

    let image = Image::new(data, &DecodeSettings::default()).ok()?;
    let w = image.width() as usize;
    let h = image.height() as usize;
    if w == 0 || h == 0 {
        return None;
    }

    // Determine bpp from J2K color space, falling back to PDF hint.
    let bpp = match image.color_space() {
        ColorSpace::Gray => 1,
        ColorSpace::RGB => 3,
        ColorSpace::CMYK => 4,
        ColorSpace::Icc { num_channels, .. } | ColorSpace::Unknown { num_channels } => {
            // Use the J2K channel count, but cross-check with the PDF hint.
            let nc = *num_channels as usize;
            match (nc, pdf_color_space) {
                (1, _) => 1,
                (3, _) => 3,
                (4, _) => 4,
                (_, Some("DeviceGray") | Some("CalGray")) => 1,
                (_, Some("DeviceCMYK")) => 4,
                _ => nc.clamp(1, 4),
            }
        }
    };

    let pixels = image.decode().ok()?;
    let has_alpha = image.has_alpha();
    let total_channels = bpp + if has_alpha { 1 } else { 0 };

    // Strip alpha channel (we don't composite alpha yet).
    let pixel_data = if has_alpha {
        let mut stripped = Vec::with_capacity(w * h * bpp);
        for chunk in pixels.chunks_exact(total_channels) {
            stripped.extend_from_slice(&chunk[..bpp]);
        }
        stripped
    } else {
        pixels
    };

    // Verify data size matches expectations.
    if pixel_data.len() < w * h * bpp {
        return None;
    }

    Some((pixel_data, w, h, bpp))
}

/// Maximum area (in points^2) for a dark filled rectangle to be rendered.
/// Large dark filled rects are almost always clipping regions or figure
/// bounding boxes that should not be rendered as opaque fills. Colored
/// fills (backgrounds, highlights) are allowed at any size.
const MAX_DARK_FILLED_RECT_AREA: f64 = 5000.0;

/// Render a path shape (line or rectangle) onto the pixel buffer.
#[allow(clippy::too_many_arguments)]
fn render_shape(
    pixels: &mut [u8],
    img_width: u32,
    img_height: u32,
    shape: &PageShape,
    page_origin_x: f64,
    page_height: f64,
    scale: f64,
) {
    // Skip large filled rectangles that would cover page content.
    // These are typically clipping regions (dark/black) or background
    // clears (white) from the content stream. Colored fills like table
    // cells and page accents are kept.
    if shape.filled {
        if let PathShapeKind::Rect { width, height, .. } = &shape.kind {
            if width * height > MAX_DARK_FILLED_RECT_AREA {
                let is_dark = match &shape.fill_color {
                    Some(c) => c.r < 30 && c.g < 30 && c.b < 30,
                    None => true, // no color = default black
                };
                if is_dark {
                    return;
                }
            }
        }
    }

    // Clip-mask handling (#124). The pixel-writer helpers below
    // do not know about clipping, so we gate them via a save/restore
    // sandwich: snapshot the pixel region that could be touched by this
    // shape, run the normal render path, then undo every pixel outside
    // the effective clip mask. This keeps the hot-path writers unchanged
    // and localises clip logic to one place. Bounding-box cost is O(bbox
    // area) -- typically <100k pixels per clipped shape.
    let clip_mask = crate::clip::build_clip_mask(
        &shape.active_clips,
        img_width,
        img_height,
        page_origin_x,
        page_height,
        scale,
    );
    // Soft-mask handling ( extension, ISO 32000-2 §11.6.5). Same
    // sandwich pattern as the clip mask, but the restore lerps toward
    // the snapshot by the complement of the mask's per-pixel alpha
    // rather than a binary gate. Needs the same bbox + snapshot, so we
    // hoist the bbox/snapshot capture to cover both and share the
    // memcpy when both are active.
    let soft_mask = crate::clip::build_soft_mask(
        &shape.active_soft_masks,
        img_width,
        img_height,
        page_origin_x,
        page_height,
        scale,
    );
    let sandwich_bbox = if clip_mask.is_some() || soft_mask.is_some() {
        Some(shape_pixel_bbox(
            shape,
            page_origin_x,
            page_height,
            scale,
            img_width,
            img_height,
        ))
    } else {
        None
    };
    let sandwich_snapshot = sandwich_bbox.map(|b| snapshot_region(pixels, img_width, &b));
    let clip_sandwich = clip_mask
        .as_ref()
        .zip(sandwich_bbox.zip(sandwich_snapshot.as_ref()))
        .map(|(m, (b, s))| (m, b, s));
    let soft_mask_sandwich = soft_mask
        .as_ref()
        .zip(sandwich_bbox.zip(sandwich_snapshot.as_ref()))
        .map(|(m, (b, s))| (m, b, s));

    let stroke_rgb = match &shape.stroke_color {
        Some(c) => [c.r, c.g, c.b],
        None => [0, 0, 0],
    };
    let fill_rgb = match &shape.fill_color {
        Some(c) => [c.r, c.g, c.b],
        None => [0, 0, 0],
    };

    match &shape.kind {
        PathShapeKind::Line { x1, y1, x2, y2 } => {
            if !shape.stroked {
                return;
            }
            // Convert PDF coords (y-up) to image coords (y-down). Keep
            // fractional precision so the line drawer can do sub-pixel AA.
            let fx1 = (x1 - page_origin_x) * scale;
            let fy1 = (page_height - y1) * scale;
            let fx2 = (x2 - page_origin_x) * scale;
            let fy2 = (page_height - y2) * scale;
            let lw = (shape.line_width * scale).max(1.0);
            // Horizontal/vertical hairlines are a major SSIM cost on
            // table/figure-heavy pages. mupdf renders them with sub-pixel
            // Y/X AA: a 1px-wide stroke at fractional Y splits its coverage
            // across two scanlines. Truncating to int and painting a 1px
            // strip mismatches mupdf almost everywhere it draws a rule.
            if (fy1 - fy2).abs() < 0.001 {
                draw_horizontal_line_aa(
                    pixels, img_width, img_height, fx1, fx2, fy1, lw, stroke_rgb,
                );
            } else if (fx1 - fx2).abs() < 0.001 {
                draw_vertical_line_aa(pixels, img_width, img_height, fx1, fy1, fy2, lw, stroke_rgb);
            } else {
                // Diagonal: fall back to integer Bresenham for now.
                draw_line_bresenham(
                    pixels, img_width, img_height, fx1 as i32, fy1 as i32, fx2 as i32, fy2 as i32,
                    lw as i32, stroke_rgb,
                );
            }
        }
        PathShapeKind::Rect {
            x,
            y,
            width,
            height,
        } => {
            let fx = (x - page_origin_x) * scale;
            // PDF rect y is the bottom edge; flip to image top.
            let fy = (page_height - y - height) * scale;
            let fw = (width * scale).max(1.0);
            let fh = (height * scale).max(1.0);

            if shape.filled {
                // AA-aware fill: edge rows/cols get partial coverage based
                // on how much of the pixel the rect actually covers in
                // source coords. mupdf does the same -- a rect spanning
                // (3.7, 5.2) to (47.1, 99.8) softens the edges into the
                // adjacent pixels rather than truncating to integer.
                fill_rect_aa(
                    pixels,
                    img_width,
                    img_height,
                    fx,
                    fy,
                    fx + fw,
                    fy + fh,
                    fill_rgb,
                    shape.fill_alpha,
                );
            }
            if shape.stroked {
                let lw = (shape.line_width * scale).max(1.0);
                // Stroke each edge with the AA-aware horizontal/vertical
                // line drawer so rect borders match mupdf's rule rendering.
                draw_horizontal_line_aa(
                    pixels,
                    img_width,
                    img_height,
                    fx,
                    fx + fw,
                    fy,
                    lw,
                    stroke_rgb,
                );
                draw_horizontal_line_aa(
                    pixels,
                    img_width,
                    img_height,
                    fx,
                    fx + fw,
                    fy + fh,
                    lw,
                    stroke_rgb,
                );
                draw_vertical_line_aa(
                    pixels,
                    img_width,
                    img_height,
                    fx,
                    fy,
                    fy + fh,
                    lw,
                    stroke_rgb,
                );
                draw_vertical_line_aa(
                    pixels,
                    img_width,
                    img_height,
                    fx + fw,
                    fy,
                    fy + fh,
                    lw,
                    stroke_rgb,
                );
            }
        }
        PathShapeKind::Polygon {
            subpaths,
            fill_rule,
        } if shape.filled => {
            scanline_fill_polygon(
                pixels,
                img_width,
                img_height,
                subpaths,
                *fill_rule,
                page_origin_x,
                page_height,
                scale,
                fill_rgb,
                shape.fill_alpha,
            );
        }
        // Stroked polygon edges are emitted as separate Line segments.
        _ => {}
    }

    // Apply soft-mask FIRST ( extension): lerp each pixel in
    // the bbox toward the pre-paint snapshot by the complement of the
    // mask alpha. Done before clip because a hard clip is a strict
    // gate -- any pixel outside the clip region must be fully
    // restored regardless of what the soft mask says. Running
    // soft-mask before clip keeps that invariant.
    if let Some((mask, bbox, snapshot)) = soft_mask_sandwich {
        crate::compositor::apply_soft_mask_restore(
            pixels, img_width, bbox.x0, bbox.y0, bbox.x1, bbox.y1, mask, snapshot,
        );
    }
    // Apply clip mask by restoring snapshot pixels that fell
    // outside the effective region.
    if let Some((mask, bbox, snapshot)) = clip_sandwich {
        apply_clip_restore(pixels, img_width, &bbox, mask, snapshot);
    }
}

/// Pixel-aligned bbox (inclusive-exclusive) of the region that
/// [`render_shape`] could touch. Used to size the clip-mask sandwich.
#[derive(Clone, Copy)]
struct PixelBbox {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

fn shape_pixel_bbox(
    shape: &PageShape,
    page_origin_x: f64,
    page_height: f64,
    scale: f64,
    img_width: u32,
    img_height: u32,
) -> PixelBbox {
    let lw = (shape.line_width * scale).max(1.0);
    let pad = (lw.ceil() as i32) + 1;
    let (fx0, fy0, fx1, fy1) = match &shape.kind {
        PathShapeKind::Line { x1, y1, x2, y2 } => {
            let a = (*x1 - page_origin_x) * scale;
            let b = (page_height - *y1) * scale;
            let c = (*x2 - page_origin_x) * scale;
            let d = (page_height - *y2) * scale;
            (a.min(c), b.min(d), a.max(c), b.max(d))
        }
        PathShapeKind::Rect {
            x,
            y,
            width,
            height,
        } => {
            let fx = (*x - page_origin_x) * scale;
            let fy = (page_height - *y - *height) * scale;
            let fw = *width * scale;
            let fh = *height * scale;
            (fx, fy, fx + fw, fy + fh)
        }
        PathShapeKind::Polygon { subpaths, .. } => {
            let mut xmin = f64::INFINITY;
            let mut ymin = f64::INFINITY;
            let mut xmax = f64::NEG_INFINITY;
            let mut ymax = f64::NEG_INFINITY;
            for sub in subpaths {
                for (x, y) in sub {
                    let px = (*x - page_origin_x) * scale;
                    let py = (page_height - *y) * scale;
                    xmin = xmin.min(px);
                    ymin = ymin.min(py);
                    xmax = xmax.max(px);
                    ymax = ymax.max(py);
                }
            }
            (xmin, ymin, xmax, ymax)
        }
        _ => {
            // Non-exhaustive catch-all: use full image bounds.
            (0.0, 0.0, img_width as f64, img_height as f64)
        }
    };
    let x0 = ((fx0.floor() as i32) - pad).max(0);
    let y0 = ((fy0.floor() as i32) - pad).max(0);
    let x1 = ((fx1.ceil() as i32) + pad).min(img_width as i32);
    let y1 = ((fy1.ceil() as i32) + pad).min(img_height as i32);
    PixelBbox { x0, y0, x1, y1 }
}

fn snapshot_region(pixels: &[u8], img_width: u32, bbox: &PixelBbox) -> Vec<u8> {
    let w = (bbox.x1 - bbox.x0).max(0) as usize;
    let h = (bbox.y1 - bbox.y0).max(0) as usize;
    let stride = img_width as usize * 3;
    let mut out = Vec::with_capacity(w * h * 3);
    // Defensive clamp: a pathologically large line_width or an overflowing
    // img_width cast can push `bbox.x1`/`bbox.y1` past the buffer even though
    // `shape_pixel_bbox` clamps via `.min(img_width as i32)`. Snapshot is
    // best-effort (it drives clip-restore), so a partial copy is acceptable
    // when the bbox disagrees with the real buffer dimensions.
    for row in 0..h {
        let src = (bbox.y0 as usize + row) * stride + bbox.x0 as usize * 3;
        let end = src + w * 3;
        if end > pixels.len() {
            // Short row: take what's there, pad with zeros. Clip-restore
            // only reads positions inside the mask so trailing zeros are
            // harmless unless the mask extends beyond the buffer, which
            // would be a different bug.
            if src < pixels.len() {
                out.extend_from_slice(&pixels[src..pixels.len()]);
            }
            out.resize(out.len() + end.saturating_sub(pixels.len().max(src)), 0);
            continue;
        }
        out.extend_from_slice(&pixels[src..end]);
    }
    out
}

fn apply_clip_restore(
    pixels: &mut [u8],
    img_width: u32,
    bbox: &PixelBbox,
    mask: &crate::clip::ClipMask,
    snapshot: &[u8],
) {
    let w = (bbox.x1 - bbox.x0).max(0) as usize;
    let h = (bbox.y1 - bbox.y0).max(0) as usize;
    let stride = img_width as usize * 3;
    for row in 0..h {
        let y = bbox.y0 + row as i32;
        for col in 0..w {
            let x = bbox.x0 + col as i32;
            if !mask.allows(x, y) {
                let dst = y as usize * stride + x as usize * 3;
                let src = row * w * 3 + col * 3;
                if dst + 2 < pixels.len() && src + 2 < snapshot.len() {
                    pixels[dst] = snapshot[src];
                    pixels[dst + 1] = snapshot[src + 1];
                    pixels[dst + 2] = snapshot[src + 2];
                }
            }
        }
    }
}

/// Draw a line with configurable width.
/// Scanline polygon fill: renders filled arbitrary polygons using the
/// sorted-edge scanline algorithm with configurable fill rule.
#[allow(clippy::too_many_arguments)]
fn scanline_fill_polygon(
    pixels: &mut [u8],
    img_width: u32,
    img_height: u32,
    subpaths: &[Vec<(f64, f64)>],
    fill_rule: FillRule,
    page_origin_x: f64,
    page_height: f64,
    scale: f64,
    color: [u8; 3],
    alpha: u8,
) {
    // Edge: a line segment from (x_start, y_start) to (x_end, y_end) in pixel coords.
    // direction: +1 for downward (y increases), -1 for upward.
    struct Edge {
        y_min: i32,
        y_max: i32,
        x_at_y_min: f64,
        dx_per_dy: f64,
        direction: i32, // +1 or -1 for winding rule
    }

    let mut edges: Vec<Edge> = Vec::new();
    let h = img_height as i32;
    let w = img_width as i32;

    // Build edge table from all subpath vertices.
    for subpath in subpaths {
        if subpath.len() < 2 {
            continue;
        }
        let n = subpath.len();
        for i in 0..n {
            let (x0_page, y0_page) = subpath[i];
            let (x1_page, y1_page) = subpath[(i + 1) % n];

            // Convert from page coords to pixel coords.
            let px0 = (x0_page - page_origin_x) * scale;
            let py0 = (page_height - y0_page) * scale;
            let px1 = (x1_page - page_origin_x) * scale;
            let py1 = (page_height - y1_page) * scale;

            // Skip horizontal edges.
            let iy0 = py0 as i32;
            let iy1 = py1 as i32;
            if iy0 == iy1 {
                continue;
            }

            let (y_min, y_max, _x_start, direction) = if py0 < py1 {
                (iy0, iy1, px0, 1)
            } else {
                (iy1, iy0, px1, -1)
            };

            // Clip to image bounds.
            if y_max < 0 || y_min >= h {
                continue;
            }

            let dy = py1 - py0;
            let dx_per_dy = if dy.abs() > 1e-10 {
                (px1 - px0) / dy
            } else {
                0.0
            };

            // Adjust x_start if we need to start from a different y.
            let x_at_y_min = if py0 < py1 {
                px0 + dx_per_dy * (y_min as f64 - py0)
            } else {
                px1 + dx_per_dy * (y_min as f64 - py1)
            };

            edges.push(Edge {
                y_min,
                y_max,
                x_at_y_min,
                dx_per_dy,
                direction,
            });
        }
    }

    if edges.is_empty() {
        return;
    }

    // Sort edges by y_min.
    edges.sort_by_key(|a| a.y_min);

    // Find the global y range.
    let y_start = edges[0].y_min.max(0);
    let y_end = edges.iter().map(|e| e.y_max).max().unwrap_or(0).min(h);

    // Scanline sweep.
    let mut active_edges: Vec<(f64, i32)> = Vec::new(); // (x_intersection, direction)

    for y in y_start..y_end {
        active_edges.clear();

        // Collect x-intersections for all edges active at this scanline.
        for edge in &edges {
            if edge.y_min > y {
                break; // edges are sorted by y_min
            }
            if edge.y_max <= y {
                continue;
            }
            // Edge is active at this scanline.
            let x = edge.x_at_y_min + edge.dx_per_dy * (y - edge.y_min) as f64;
            active_edges.push((x, edge.direction));
        }

        if active_edges.is_empty() {
            continue;
        }

        // Sort by x.
        active_edges.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        // Fill spans with sub-pixel left/right edge coverage. fill_aa_span
        // applies fractional alpha to the boundary pixels of each span,
        // smoothing the polygon's silhouette horizontally. Y-axis is still
        // 1-bit per scanline; full 2D coverage would require per-row
        // supersampling. The X-only AA is the cheap half of the integral
        // that captures most of the visible aliasing on diagonal edges.
        match fill_rule {
            FillRule::EvenOdd => {
                let mut i = 0;
                while i + 1 < active_edges.len() {
                    fill_aa_span(
                        pixels,
                        img_width,
                        img_height,
                        y,
                        active_edges[i].0,
                        active_edges[i + 1].0,
                        w,
                        color,
                        alpha,
                    );
                    i += 2;
                }
            }
            FillRule::NonZeroWinding => {
                let mut winding = 0i32;
                for pair in active_edges.windows(2) {
                    winding += pair[0].1;
                    if winding != 0 {
                        fill_aa_span(
                            pixels, img_width, img_height, y, pair[0].0, pair[1].0, w, color, alpha,
                        );
                    }
                }
            }
        }
    }
}

/// Draw a horizontal line with sub-pixel Y antialiasing.
///
/// Models the line as a rectangle from (x_left, y_center - lw/2) to
/// (x_right, y_center + lw/2) and analytically computes per-pixel-row
/// coverage. This matches mupdf's rendering of rules and table borders:
/// a 1px line at fractional Y splits its alpha across the two adjacent
/// scanlines (e.g. y=672.6 -> 40% on row 672, 100% nothing, ... no, more
/// precisely: the line covers row y if its [y, y+1] span overlaps the
/// stroke's [y_top, y_bot] band).
///
/// X coverage is also AA'd at the left/right edges so non-integer line
/// endpoints (common when shapes are scaled from PDF user space) don't
/// produce hard 1-pixel offsets.
#[allow(clippy::too_many_arguments)]
fn draw_horizontal_line_aa(
    pixels: &mut [u8],
    img_width: u32,
    img_height: u32,
    fx1: f64,
    fx2: f64,
    fy_center: f64,
    line_width: f64,
    color: [u8; 3],
) {
    let (x_left, x_right) = if fx1 < fx2 { (fx1, fx2) } else { (fx2, fx1) };
    if x_right <= x_left {
        return;
    }

    let half_lw = (line_width * 0.5).max(0.5);
    let y_top = fy_center - half_lw;
    let y_bot = fy_center + half_lw;

    let row_min = y_top.floor() as i32;
    let row_max = y_bot.ceil() as i32;

    for row in row_min..row_max {
        if row < 0 || row >= img_height as i32 {
            continue;
        }
        let row_top = row as f64;
        let row_bot = row_top + 1.0;
        // Vertical coverage: how much of [row_top, row_bot] is inside
        // [y_top, y_bot]. Always in [0, 1].
        let v_cov = (row_bot.min(y_bot) - row_top.max(y_top)).clamp(0.0, 1.0);
        if v_cov <= 0.0 {
            continue;
        }
        let alpha_base = (v_cov * 255.0).round().clamp(0.0, 255.0) as u8;
        if alpha_base == 0 {
            continue;
        }
        // Now horizontal AA across the row.
        fill_aa_span(
            pixels,
            img_width,
            img_height,
            row,
            x_left,
            x_right,
            img_width as i32,
            color,
            alpha_base,
        );
    }
}

/// Draw a vertical line with sub-pixel X antialiasing.
///
/// The line is a rectangle [x_left, x_right] x [y_top, y_bot]. Per-column X
/// coverage is computed once per column (it's row-independent for a vertical
/// stroke). The middle rows are full-coverage; the top/bottom edge rows get
/// partial Y coverage. Final pixel alpha is `x_cov * y_cov * 255`.
///
/// The hot path uses precomputed Q15 weights and a row-major inner loop
/// keyed on column to maximize sequential pixel writes.
#[allow(clippy::too_many_arguments)]
fn draw_vertical_line_aa(
    pixels: &mut [u8],
    img_width: u32,
    img_height: u32,
    fx_center: f64,
    fy1: f64,
    fy2: f64,
    line_width: f64,
    color: [u8; 3],
) {
    let (y_top, y_bot) = if fy1 < fy2 { (fy1, fy2) } else { (fy2, fy1) };
    if y_bot <= y_top {
        return;
    }

    let half_lw = (line_width * 0.5).max(0.5);
    let x_left = fx_center - half_lw;
    let x_right = fx_center + half_lw;

    let row_min = (y_top.floor() as i32).max(0);
    let row_max = (y_bot.ceil() as i32).min(img_height as i32);
    if row_max <= row_min {
        return;
    }

    // Precompute per-row Y coverage as u8 alpha. Only the first and last
    // rows can have v_cov < 1.0; interior rows are always fully covered.
    let first_row = row_min as f64;
    let last_row = (row_max - 1) as f64;
    let first_v = (first_row + 1.0 - first_row.max(y_top)).clamp(0.0, 1.0);
    let last_v = (last_row + 1.0).min(y_bot) - last_row.max(y_top);
    let last_v = last_v.clamp(0.0, 1.0);
    let first_alpha = (first_v * 255.0).round() as u32;
    let last_alpha = (last_v * 255.0).round() as u32;

    let col_min = x_left.floor() as i32;
    let col_max = x_right.ceil() as i32;

    for col in col_min..col_max {
        if col < 0 || col >= img_width as i32 {
            continue;
        }
        let col_left = col as f64;
        let col_right = col_left + 1.0;
        let h_cov = (col_right.min(x_right) - col_left.max(x_left)).clamp(0.0, 1.0);
        if h_cov <= 0.0 {
            continue;
        }
        let h_cov_u = (h_cov * 255.0).round() as u32;

        // First (top) edge row.
        if row_max - row_min == 1 {
            // Span fits in a single row: alpha = h * (y_bot - y_top).
            let single = (y_bot - y_top).clamp(0.0, 1.0);
            let a = ((h_cov_u * (single * 255.0).round() as u32 + 127) / 255).min(255) as u8;
            if a > 0 {
                blend_pixel(pixels, img_width, img_height, col, row_min, color, a);
            }
            continue;
        }

        let a_first = ((h_cov_u * first_alpha + 127) / 255).min(255) as u8;
        if a_first > 0 {
            blend_pixel(pixels, img_width, img_height, col, row_min, color, a_first);
        }

        // Interior rows at full Y coverage.
        let a_interior = ((h_cov_u * 255 + 127) / 255).min(255) as u8;
        if a_interior > 0 {
            for row in (row_min + 1)..(row_max - 1) {
                blend_pixel(pixels, img_width, img_height, col, row, color, a_interior);
            }
        }

        // Last (bottom) edge row.
        let a_last = ((h_cov_u * last_alpha + 127) / 255).min(255) as u8;
        if a_last > 0 {
            blend_pixel(
                pixels,
                img_width,
                img_height,
                col,
                row_max - 1,
                color,
                a_last,
            );
        }
    }
}

///
/// For horizontal/vertical lines, uses fast axis-aligned rectangle fill.
/// For diagonal lines, falls back to Bresenham with perpendicular thickness.
#[allow(clippy::too_many_arguments)]
fn draw_line_bresenham(
    pixels: &mut [u8],
    img_width: u32,
    img_height: u32,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    line_width: i32,
    color: [u8; 3],
) {
    // Half-widths above and below center. For even widths, bias downward.
    // lw=1: above=0, below=0 (1px). lw=2: above=0, below=1 (2px).
    // lw=3: above=1, below=1 (3px). lw=4: above=1, below=2 (4px).
    let above = (line_width - 1) / 2;
    let below = line_width / 2;

    // Fast path: horizontal line.
    if y0 == y1 {
        let (lx, rx) = if x0 < x1 { (x0, x1) } else { (x1, x0) };
        for py in (y0 - above)..=(y0 + below) {
            if py < 0 || py >= img_height as i32 {
                continue;
            }
            for px in lx..=rx {
                set_pixel(pixels, img_width, img_height, px, py, color);
            }
        }
        return;
    }

    // Fast path: vertical line.
    if x0 == x1 {
        let (ty, by) = if y0 < y1 { (y0, y1) } else { (y1, y0) };
        for px in (x0 - above)..=(x0 + below) {
            if px < 0 || px >= img_width as i32 {
                continue;
            }
            for py in ty..=by {
                set_pixel(pixels, img_width, img_height, px, py, color);
            }
        }
        return;
    }

    // General case: Bresenham with perpendicular thickness.
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx: i32 = if x0 < x1 { 1 } else { -1 };
    let sy: i32 = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let mut x = x0;
    let mut y = y0;

    // For diagonal lines, expand perpendicular to the line direction.
    // Mostly-horizontal lines expand in y, mostly-vertical in x.
    let mostly_horizontal = dx > -dy;

    loop {
        if mostly_horizontal {
            for oy in -above..=below {
                set_pixel(pixels, img_width, img_height, x, y + oy, color);
            }
        } else {
            for ox in -above..=below {
                set_pixel(pixels, img_width, img_height, x + ox, y, color);
            }
        }

        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            if x == x1 {
                break;
            }
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            if y == y1 {
                break;
            }
            err += dx;
            y += sy;
        }
    }
}

/// Fill a horizontal span [x_left, x_right] in source coords with sub-pixel
/// X coverage at the left/right edges. Interior pixels get full `alpha_base`,
/// edge pixels get `alpha_base * fract_coverage`. Used by the polygon AA edge
/// path and the horizontal-line AA path.
#[allow(clippy::too_many_arguments)]
fn fill_aa_span(
    pixels: &mut [u8],
    img_width: u32,
    img_height: u32,
    y: i32,
    x_left: f64,
    x_right: f64,
    w: i32,
    color: [u8; 3],
    alpha_base: u8,
) {
    if x_right <= x_left {
        return;
    }
    let x_start_f = x_left.floor();
    let x_end_f = x_right.floor();
    let x_start = x_start_f as i32;
    let x_end = x_end_f as i32;

    if x_start == x_end {
        // Span contained within one pixel: alpha = base * span_width.
        let cov = (x_right - x_left).clamp(0.0, 1.0);
        if x_start >= 0 && x_start < w {
            let a = ((cov * alpha_base as f64).round() as u32).min(255) as u8;
            blend_pixel(pixels, img_width, img_height, x_start, y, color, a);
        }
        return;
    }

    // Left edge pixel: coverage = 1 - fract(x_left).
    let left_cov = 1.0 - (x_left - x_start_f);
    if x_start >= 0 && x_start < w {
        let a = ((left_cov * alpha_base as f64).round() as u32).min(255) as u8;
        blend_pixel(pixels, img_width, img_height, x_start, y, color, a);
    }

    // Interior pixels.
    let interior_start = (x_start + 1).max(0);
    let interior_end = x_end.min(w);
    for x in interior_start..interior_end {
        blend_pixel(pixels, img_width, img_height, x, y, color, alpha_base);
    }

    // Right edge pixel: coverage = fract(x_right).
    let right_cov = x_right - x_end_f;
    if right_cov > 0.0 && x_end >= 0 && x_end < w {
        let a = ((right_cov * alpha_base as f64).round() as u32).min(255) as u8;
        blend_pixel(pixels, img_width, img_height, x_end, y, color, a);
    }
}

/// Fill an axis-aligned rectangle with sub-pixel edge coverage. The rectangle
/// is in source pixel coords [x_left, x_right] x [y_top, y_bot]. Interior
/// pixels (whose box lies fully inside the rect) get full coverage; edge
/// pixels along all four sides get fractional coverage proportional to the
/// area of intersection. `base_alpha` modulates the final pixel alpha.
///
/// This matches mupdf's `fz_fill_rect` behaviour and is essential for
/// table cells / highlights whose bbox doesn't land on integer pixels --
/// truncating produces visible 1-pixel offsets between adjacent cells.
#[allow(clippy::too_many_arguments)]
fn fill_rect_aa(
    pixels: &mut [u8],
    img_width: u32,
    img_height: u32,
    x_left: f64,
    y_top: f64,
    x_right: f64,
    y_bot: f64,
    color: [u8; 3],
    base_alpha: u8,
) {
    if x_right <= x_left || y_bot <= y_top || base_alpha == 0 {
        return;
    }
    let row_min = (y_top.floor() as i32).max(0);
    let row_max = (y_bot.ceil() as i32).min(img_height as i32);
    if row_max <= row_min {
        return;
    }

    for row in row_min..row_max {
        let row_top = row as f64;
        let row_bot = row_top + 1.0;
        let v_cov = (row_bot.min(y_bot) - row_top.max(y_top)).clamp(0.0, 1.0);
        if v_cov <= 0.0 {
            continue;
        }
        let row_alpha = ((v_cov * base_alpha as f64).round() as u32).min(255) as u8;
        if row_alpha == 0 {
            continue;
        }
        fill_aa_span(
            pixels,
            img_width,
            img_height,
            row,
            x_left,
            x_right,
            img_width as i32,
            color,
            row_alpha,
        );
    }
}

/// Draw a filled rectangle. Replaced at the rect-shape call site by
/// `fill_rect_aa` for sub-pixel edge coverage; kept for any future
/// pixel-aligned solid-fill path.
#[allow(dead_code, clippy::too_many_arguments)]
fn draw_filled_rect(
    pixels: &mut [u8],
    img_width: u32,
    img_height: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    color: [u8; 3],
) {
    for dy in 0..h {
        let py = y + dy as i32;
        if py < 0 || py >= img_height as i32 {
            continue;
        }
        for dx in 0..w {
            let px = x + dx as i32;
            if px < 0 || px >= img_width as i32 {
                continue;
            }
            let idx = (py as usize * img_width as usize + px as usize) * 3;
            if idx + 2 < pixels.len() {
                pixels[idx] = color[0];
                pixels[idx + 1] = color[1];
                pixels[idx + 2] = color[2];
            }
        }
    }
}

/// Draw a stroked (outlined) rectangle. Replaced at call sites by direct
/// `draw_horizontal_line_aa` / `draw_vertical_line_aa` calls so each edge
/// gets sub-pixel AA. Kept as a private utility for any future paths that
/// want pixel-aligned rectangle strokes (currently unused, hence the
/// allow-dead lint).
#[allow(dead_code, clippy::too_many_arguments)]
fn draw_stroked_rect(
    pixels: &mut [u8],
    img_width: u32,
    img_height: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    line_width: i32,
    color: [u8; 3],
) {
    let x2 = x + w as i32;
    let y2 = y + h as i32;
    // Top edge
    draw_line_bresenham(
        pixels, img_width, img_height, x, y, x2, y, line_width, color,
    );
    // Bottom edge
    draw_line_bresenham(
        pixels, img_width, img_height, x, y2, x2, y2, line_width, color,
    );
    // Left edge
    draw_line_bresenham(
        pixels, img_width, img_height, x, y, x, y2, line_width, color,
    );
    // Right edge
    draw_line_bresenham(
        pixels, img_width, img_height, x2, y, x2, y2, line_width, color,
    );
}

/// Check whether char_advances have meaningful per-character variation.
///
/// When the PDF font returns a flat default width (e.g., 600) for every character,
/// char_advances will all be identical, providing zero useful proportional info.
/// In that case, font program advance widths are a better source of proportional
/// spacing. Returns true if the advances vary enough to be useful.
fn has_proportional_variation(advances: &[f64]) -> bool {
    if advances.len() < 2 {
        return true; // Single char, nothing to compare.
    }
    let first = advances[0];
    // If any advance differs by more than 1% from the first, it's proportional.
    advances.iter().any(|&a| (a - first).abs() > first * 0.01)
}

/// Compute proportional advance widths using the font's actual metrics.
///
/// Uses font_cache advance widths (embedded font or Liberation Sans fallback).
/// Does NOT inflate advances to fill the span bbox -- that would stretch characters
/// apart when the bbox was computed from wrong (flat default) PDF widths.
/// Only scales DOWN if advances exceed the bbox (prevents overflow).
fn proportional_advances(
    span: &PositionedSpan,
    char_count: usize,
    span_width_px: f64,
    font_cache: &FontCache,
    glyph_scale: f64,
) -> Vec<f64> {
    if char_count == 0 {
        return Vec::new();
    }
    let font_name = span.font_name.as_deref().unwrap_or("default");

    // CID/composite fonts (Identity-H): char_gids carries the raw CID per
    // char, which is the authoritative index into the parsed /W table.
    // Prefer GID-indexed advances so MS Word CID TrueType subsets pick up
    // the PDF's /W overrides instead of falling through to the embedded
    // hmtx or a fallback font's Unicode cmap (issue #182).
    let gid_advances: Option<Vec<f64>> = span.char_gids.as_ref().and_then(|gids| {
        if gids.len() != char_count {
            return None;
        }
        let mut out = Vec::with_capacity(gids.len());
        for &gid in gids {
            let w = font_cache.advance_width_by_gid(font_name, gid)?;
            out.push(w as f64 * glyph_scale);
        }
        Some(out)
    });

    // Collect raw advance widths from the font (embedded or fallback).
    let raw_advances: Vec<f64> = gid_advances.unwrap_or_else(|| {
        span.text
            .chars()
            .map(|ch| font_cache.advance_width(font_name, ch) as f64 * glyph_scale)
            .collect()
    });

    let sum: f64 = raw_advances.iter().sum();
    if sum <= 0.0 {
        // All zero-width: fall back to uniform.
        let uniform = span_width_px / char_count as f64;
        return vec![uniform; char_count];
    }

    // Scale proportionally: each character gets a share of bbox width
    // proportional to its font-metric advance width. This preserves the
    // font's natural character proportions (wide M, narrow i) while
    // filling the full span width (which includes word/char spacing).
    let scale_factor = span_width_px / sum;
    raw_advances.iter().map(|a| a * scale_factor).collect()
}

/// Convert hinted glyph points (flat array + contour_ends) to the contour
/// format used by the rasterizer: `Vec<Vec<(f64, f64, bool)>>`.
fn hinted_to_contours(
    points: &[udoc_font::ttf::OutlinePoint],
    contour_ends: &[usize],
) -> Vec<Vec<(f64, f64, bool)>> {
    // Hinted points come from the TT VM in font-space y-up convention
    // (positive y = above baseline). The rasterizer's flatten_contour
    // negates y to convert to image-space y-down, so we leave the y
    // sign alone here. Negating twice flips glyphs upside down.
    let mut contours = Vec::with_capacity(contour_ends.len());
    let mut start = 0;
    for &end in contour_ends {
        if end >= points.len() {
            break;
        }
        let mut contour = Vec::with_capacity(end - start + 1);
        for p in points.iter().take(end + 1).skip(start) {
            contour.push((p.x, p.y, p.on_curve));
        }
        if !contour.is_empty() {
            contours.push(contour);
        }
        start = end + 1;
    }
    contours
}

/// Draw a filled rectangle on the pixel buffer.
#[allow(clippy::too_many_arguments)]
fn draw_rect(
    pixels: &mut [u8],
    img_width: u32,
    img_height: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    gray: u8,
) {
    for dy in 0..h {
        let py = y + dy as i32;
        if py < 0 || py >= img_height as i32 {
            continue;
        }
        for dx in 0..w {
            let px = x + dx as i32;
            if px < 0 || px >= img_width as i32 {
                continue;
            }
            let idx = (py as usize * img_width as usize + px as usize) * 3;
            if idx + 2 < pixels.len() {
                pixels[idx] = gray;
                pixels[idx + 1] = gray;
                pixels[idx + 2] = gray;
            }
        }
    }
}

/// Rotate an RGB pixel buffer clockwise by 90, 180, or 270 degrees.
/// Returns the rotated buffer and new (width, height).
fn rotate_pixel_buffer(pixels: &[u8], w: u32, h: u32, rotation: u32) -> (Vec<u8>, u32, u32) {
    let (w, h) = (w as usize, h as usize);
    match rotation {
        90 => {
            // CW 90: (x, y) -> (h-1-y, x). Output is h wide, w tall.
            let mut out = vec![255u8; pixels.len()];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * 3;
                    let dst = (x * h + (h - 1 - y)) * 3;
                    out[dst..dst + 3].copy_from_slice(&pixels[src..src + 3]);
                }
            }
            (out, h as u32, w as u32)
        }
        180 => {
            // 180: (x, y) -> (w-1-x, h-1-y). Same dimensions.
            let mut out = vec![255u8; pixels.len()];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * 3;
                    let dst = ((h - 1 - y) * w + (w - 1 - x)) * 3;
                    out[dst..dst + 3].copy_from_slice(&pixels[src..src + 3]);
                }
            }
            (out, w as u32, h as u32)
        }
        270 => {
            // CW 270 (= CCW 90): (x, y) -> (y, w-1-x). Output is h wide, w tall.
            let mut out = vec![255u8; pixels.len()];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * 3;
                    let dst = ((w - 1 - x) * h + y) * 3;
                    out[dst..dst + 3].copy_from_slice(&pixels[src..src + 3]);
                }
            }
            (out, h as u32, w as u32)
        }
        _ => (pixels.to_vec(), w as u32, h as u32),
    }
}

/// Compute effective pixel dimensions accounting for page rotation.
#[cfg(test)]
fn effective_dimensions(page_def: &PageDef, scale: f64) -> (u32, u32) {
    let (w, h) = if page_def.rotation == 90 || page_def.rotation == 270 {
        (page_def.height, page_def.width)
    } else {
        (page_def.width, page_def.height)
    };
    ((w * scale).ceil() as u32, (h * scale).ceil() as u32)
}

/// Get the "top" Y coordinate of the visible region in user space (y-up).
/// Used for the y-flip formula `(page_top_y - y) * scale`. For pages whose
/// /CropBox or /MediaBox does not start at y=0 (e.g. two-page spreads with
/// a non-zero CropBox origin), this is `origin_y + height`; the formula
/// then correctly maps the top edge of the visible region to pixel row 0.
///
/// Always uses the unrotated height because content coordinates are in the
/// unrotated space. /Rotate is applied as a pixel buffer rotation after
/// rendering.
fn effective_page_height(page_def: &PageDef) -> f64 {
    page_def.height + page_def.origin_y
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::document::presentation::Presentation;
    use udoc_core::geometry::BoundingBox;

    fn make_test_doc(spans: Vec<PositionedSpan>, page_width: f64, page_height: f64) -> Document {
        let mut doc = Document::new();
        let mut pres = Presentation::default();
        pres.pages.push(PageDef::new(0, page_width, page_height, 0));
        pres.raw_spans = spans;
        doc.presentation = Some(pres);
        doc
    }

    #[test]
    fn hinted_to_contours_preserves_y_sign() {
        // Hinted points come from the TT VM in font-space y-up convention.
        // The rasterizer's flatten_contour negates y to image-space y-down,
        // so this helper must NOT also negate. A redundant negation flipped
        // glyphs upside down (Round 2C).
        use udoc_font::ttf::OutlinePoint;
        let points = vec![
            OutlinePoint {
                x: 0.0,
                y: 700.0,
                on_curve: true,
            },
            OutlinePoint {
                x: 500.0,
                y: 0.0,
                on_curve: true,
            },
            OutlinePoint {
                x: -250.0,
                y: -100.0,
                on_curve: false,
            },
        ];
        let contours = hinted_to_contours(&points, &[2]);
        assert_eq!(contours.len(), 1);
        let c = &contours[0];
        assert_eq!(c.len(), 3);
        assert_eq!(c[0], (0.0, 700.0, true));
        assert_eq!(c[1], (500.0, 0.0, true));
        assert_eq!(c[2], (-250.0, -100.0, false));
    }

    #[test]
    fn render_empty_page() {
        let doc = make_test_doc(vec![], 612.0, 792.0);
        let mut cache = FontCache::empty();
        let png = render_page(&doc, 0, 72, &mut cache).expect("should render");
        // Check it's a valid PNG.
        assert_eq!(&png[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
        // At 72 DPI, US Letter is 612x792 pixels.
        assert_eq!(
            u32::from_be_bytes([png[16], png[17], png[18], png[19]]),
            612
        );
    }

    #[test]
    fn render_page_out_of_range() {
        let doc = make_test_doc(vec![], 612.0, 792.0);
        let mut cache = FontCache::empty();
        assert!(render_page(&doc, 5, 72, &mut cache).is_err());
    }

    #[test]
    fn render_no_presentation() {
        let doc = Document::new();
        let mut cache = FontCache::empty();
        assert!(render_page(&doc, 0, 72, &mut cache).is_err());
    }

    #[test]
    fn rendering_profile_default_is_ocr_friendly() {
        assert_eq!(RenderingProfile::default(), RenderingProfile::OcrFriendly);
    }

    #[test]
    fn rendering_profile_guard_restores_previous() {
        // Before entering: default thread-local is OcrFriendly.
        assert_eq!(current_rendering_profile(), RenderingProfile::OcrFriendly);
        {
            let _guard = RenderProfileGuard::new(RenderingProfile::Visual);
            assert_eq!(current_rendering_profile(), RenderingProfile::Visual);
        }
        // After drop: restored to previous.
        assert_eq!(current_rendering_profile(), RenderingProfile::OcrFriendly);
    }

    #[test]
    fn rendering_profile_guard_nested() {
        let _outer = RenderProfileGuard::new(RenderingProfile::Visual);
        assert_eq!(current_rendering_profile(), RenderingProfile::Visual);
        {
            let _inner = RenderProfileGuard::new(RenderingProfile::OcrFriendly);
            assert_eq!(current_rendering_profile(), RenderingProfile::OcrFriendly);
        }
        // Inner drop restores to Visual (outer).
        assert_eq!(current_rendering_profile(), RenderingProfile::Visual);
    }

    #[test]
    fn render_page_with_profile_both_succeed() {
        let doc = make_test_doc(vec![], 612.0, 792.0);
        let mut cache = FontCache::empty();
        let ocr = render_page_with_profile(&doc, 0, 72, &mut cache, RenderingProfile::OcrFriendly)
            .expect("ocr render");
        let vis = render_page_with_profile(&doc, 0, 72, &mut cache, RenderingProfile::Visual)
            .expect("visual render");
        // Both produce PNGs of the same dimensions on an empty page.
        assert_eq!(&ocr[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
        assert_eq!(&vis[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
    }

    #[test]
    fn render_with_span() {
        let span = PositionedSpan::new(
            "Hello".to_string(),
            BoundingBox::new(72.0, 700.0, 200.0, 720.0),
            0,
        );
        let doc = make_test_doc(vec![span], 612.0, 792.0);
        let mut cache = FontCache::empty();
        let png = render_page(&doc, 0, 150, &mut cache).expect("should render");
        assert_eq!(&png[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
        // At 150 DPI: 612 * 150/72 = 1275 pixels wide.
        assert_eq!(
            u32::from_be_bytes([png[16], png[17], png[18], png[19]]),
            1275
        );
    }

    #[test]
    fn effective_dimensions_no_rotation() {
        let page = PageDef::new(0, 612.0, 792.0, 0);
        let (w, h) = effective_dimensions(&page, 1.0);
        assert_eq!(w, 612);
        assert_eq!(h, 792);
    }

    #[test]
    fn effective_dimensions_rotation_90() {
        let page = PageDef::new(0, 612.0, 792.0, 90);
        let (w, h) = effective_dimensions(&page, 1.0);
        assert_eq!(w, 792); // width and height swapped
        assert_eq!(h, 612);
    }
}

//! Annotation rendering wiring.
//!
//! Annotations (ISO 32000-2 §12.5) are composited onto the page by the
//! facade crate's `convert.rs`, which:
//!
//! 1. Enumerates annotations via `udoc_pdf::Page::annotations`.
//! 2. Interprets their /AP/N appearance streams with the §12.5.5
//!    `Matrix * RectFit` composite pre-applied, yielding page-space
//!    `PagePath` + `TextSpan` records.
//! 3. Emits those records as `PaintPath` and `PositionedSpan`
//!    entries on the presentation overlay, with a z-index band above
//!    the document content.
//! 4. Synthesises simple geometry for subtypes without /AP (Highlight,
//!    Underline, StrikeOut, Squiggly, Ink, Link border).
//!
//! By the time [`crate::render_page`] reads the presentation overlay, the
//! annotations are indistinguishable from native content-stream paths
//! and spans. This module provides the explicit entry point the task
//! charter asks for and documents the wiring, but the rasterization
//! itself is delegated to the existing
//! `crate::path_raster::rasterize_paint_path` and span rendering
//! helpers, there is no annotation-specific rasterization code.

use udoc_core::document::presentation::PaintPath;

use crate::path_raster::rasterize_paint_path;

/// Rasterize the annotation-derived `PaintPath` records for a single
/// page. Delegates to the path rasterizer; annotations share the same
/// compositor as native content-stream paths.
///
/// Returned no-op when `ap_paths` is empty. Callers that go through
/// [`crate::render_page`] do not need to call this directly; it exists
/// so external consumers (tests, tools) can render annotation ink in
/// isolation.
#[allow(clippy::too_many_arguments)]
pub fn render_annotations(
    pixels: &mut [u8],
    img_width: u32,
    img_height: u32,
    page_origin_x: f64,
    page_height: f64,
    scale: f64,
    ap_paths: &[PaintPath],
) {
    for path in ap_paths {
        rasterize_paint_path(
            pixels,
            img_width,
            img_height,
            path,
            page_origin_x,
            page_height,
            scale,
        );
    }
}

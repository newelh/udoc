//! Soft-mask rendering tests ( extension, ISO 32000-2 §11.6.5,
//! #T3-SOFTMASK).
//!
//! Tests the luminosity/alpha soft-mask compositor end-to-end by
//! injecting `SoftMaskLayer`s directly onto a `PageShape`'s
//! `active_soft_masks` field before handing the Document to the
//! renderer. This sidesteps the interpreter plumbing (which doesn't
//! yet lift ExtGState /SMask into the presentation layer, disjoint
//! from this sprint's renderer-side task) and lets us exercise the
//! composite path directly.
//!
//! Invariants asserted:
//!
//! - A soft-mask with uniform alpha = 0 erases a painted shape back to
//!   the page's pre-paint background.
//! - A soft-mask with uniform alpha = 255 is a no-op.
//! - A soft-mask with a midpoint alpha lerps each channel linearly
//!   between background and paint color.
//! - Luminosity and alpha subtypes agree when the mask bytes encode
//!   the same coverage (the subtype tag is metadata for the source
//!   conversion; the runtime composite path sees a baked byte buffer).
//! - Masks outside a shape's bbox don't leak into neighbouring
//!   content.

use udoc_core::document::presentation::{
    Color, PageDef, PageShape, PathShapeKind, Presentation, SoftMaskLayer, SoftMaskSubtype,
};
use udoc_core::document::Document;
use udoc_core::geometry::BoundingBox;

use udoc::render::font_cache::FontCache;
use udoc::render::render_page_rgb;

fn make_rect_shape(x: f64, y: f64, w: f64, h: f64, color: Color) -> PageShape {
    let mut shape = PageShape::new(
        0,
        PathShapeKind::Rect {
            x,
            y,
            width: w,
            height: h,
        },
        false,
        true,
        0.0,
    );
    shape.fill_color = Some(color);
    shape
}

/// Build a single-page Document of the given size whose only content is
/// the given list of shapes (with any soft masks pre-attached).
fn document_from_shapes(page_w: f64, page_h: f64, shapes: Vec<PageShape>) -> Document {
    let mut pres = Presentation::default();
    pres.pages.push(PageDef::new(0, page_w, page_h, 0));
    pres.shapes = shapes;
    let mut doc = Document::default();
    doc.presentation = Some(pres);
    doc
}

fn render_rgb(doc: &Document, dpi: u32) -> (Vec<u8>, u32, u32) {
    let mut cache = FontCache::new(&doc.assets);
    render_page_rgb(doc, 0, dpi, &mut cache).expect("render should succeed")
}

#[inline]
fn pixel_at(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
    let idx = (y as usize * w as usize + x as usize) * 3;
    [buf[idx], buf[idx + 1], buf[idx + 2]]
}

fn mask_rect(
    subtype: SoftMaskSubtype,
    bbox: BoundingBox,
    alpha_bytes: Vec<u8>,
    width: u32,
    height: u32,
) -> SoftMaskLayer {
    SoftMaskLayer {
        subtype,
        mask: alpha_bytes,
        width,
        height,
        bbox,
        backdrop_alpha: 255,
    }
}

#[test]
fn soft_mask_alpha_zero_fully_erases_painted_shape() {
    // 100x100 page, fill a red rect over the full page, attach a
    // 1x1 alpha=0 soft mask covering the whole page. The mask should
    // restore the white page background everywhere -> no red pixels.
    let mut shape = make_rect_shape(0.0, 0.0, 100.0, 100.0, Color::rgb(255, 0, 0));
    shape.active_soft_masks = vec![mask_rect(
        SoftMaskSubtype::Alpha,
        BoundingBox::new(0.0, 0.0, 100.0, 100.0),
        vec![0],
        1,
        1,
    )];
    let doc = document_from_shapes(100.0, 100.0, vec![shape]);
    let (pixels, w, _h) = render_rgb(&doc, 72);
    // Sample four representative positions; all should be white.
    assert_eq!(pixel_at(&pixels, w, 10, 10), [255, 255, 255]);
    assert_eq!(pixel_at(&pixels, w, 50, 50), [255, 255, 255]);
    assert_eq!(pixel_at(&pixels, w, 90, 90), [255, 255, 255]);
    assert_eq!(pixel_at(&pixels, w, 5, 95), [255, 255, 255]);
}

#[test]
fn soft_mask_alpha_full_is_no_op() {
    // Same as above but alpha = 255 everywhere: the rect should paint
    // as if no mask were present.
    let mut shape = make_rect_shape(0.0, 0.0, 100.0, 100.0, Color::rgb(0, 0, 255));
    shape.active_soft_masks = vec![mask_rect(
        SoftMaskSubtype::Alpha,
        BoundingBox::new(0.0, 0.0, 100.0, 100.0),
        vec![255],
        1,
        1,
    )];
    let doc = document_from_shapes(100.0, 100.0, vec![shape]);
    let (pixels, w, _h) = render_rgb(&doc, 72);
    // Interior of the rect must be blue (rasterizer may antialias
    // edges, so probe well away from them).
    assert_eq!(pixel_at(&pixels, w, 50, 50), [0, 0, 255]);
    assert_eq!(pixel_at(&pixels, w, 25, 75), [0, 0, 255]);
}

#[test]
fn soft_mask_alpha_midpoint_lerps_between_bg_and_paint() {
    // Green rect on white bg with uniform alpha = 128 mask. Expected:
    // each channel lerps ~halfway between (255, 255, 255) and (0, 255, 0).
    let mut shape = make_rect_shape(0.0, 0.0, 100.0, 100.0, Color::rgb(0, 255, 0));
    shape.active_soft_masks = vec![mask_rect(
        SoftMaskSubtype::Alpha,
        BoundingBox::new(0.0, 0.0, 100.0, 100.0),
        vec![128],
        1,
        1,
    )];
    let doc = document_from_shapes(100.0, 100.0, vec![shape]);
    let (pixels, w, _h) = render_rgb(&doc, 72);
    let [r, g, b] = pixel_at(&pixels, w, 50, 50);
    // Tolerance: integer rounding + the rasterizer's own alpha path
    // can drift by 1 either way on each channel.
    assert!((125..=130).contains(&r), "r drift: {r}");
    assert_eq!(g, 255);
    assert!((125..=130).contains(&b), "b drift: {b}");
}

#[test]
fn soft_mask_half_coverage_splits_rect_along_mask() {
    // A 200x100 page with a full-page red rect. The soft mask is a
    // 2x1 alpha bitmap (0, 255) covering the whole page: left half
    // alpha = 0 (fully masked out -> white), right half alpha = 255
    // (fully opaque -> red). Matches what MuPDF renders for the
    // committed synthetic `softmask_alpha.pdf` golden.
    let mut shape = make_rect_shape(0.0, 0.0, 200.0, 100.0, Color::rgb(255, 0, 0));
    shape.active_soft_masks = vec![mask_rect(
        SoftMaskSubtype::Alpha,
        BoundingBox::new(0.0, 0.0, 200.0, 100.0),
        vec![0, 255],
        2,
        1,
    )];
    let doc = document_from_shapes(200.0, 100.0, vec![shape]);
    let (pixels, w, _h) = render_rgb(&doc, 72);
    // Left half -> white; right half -> red. Probe well inside each
    // half to dodge edge antialiasing.
    assert_eq!(pixel_at(&pixels, w, 25, 50), [255, 255, 255]);
    assert_eq!(pixel_at(&pixels, w, 175, 50), [255, 0, 0]);
}

#[test]
fn soft_mask_luminosity_subtype_uses_baked_bytes_same_as_alpha() {
    // Subtype is metadata: the runtime composite path sees a baked
    // alpha byte buffer regardless. A luminosity mask with alpha
    // bytes [0, 255] should produce the same split as the alpha mask.
    let mut shape = make_rect_shape(0.0, 0.0, 200.0, 100.0, Color::rgb(0, 128, 0));
    shape.active_soft_masks = vec![mask_rect(
        SoftMaskSubtype::Luminosity,
        BoundingBox::new(0.0, 0.0, 200.0, 100.0),
        vec![0, 255],
        2,
        1,
    )];
    let doc = document_from_shapes(200.0, 100.0, vec![shape]);
    let (pixels, w, _h) = render_rgb(&doc, 72);
    assert_eq!(pixel_at(&pixels, w, 25, 50), [255, 255, 255]);
    assert_eq!(pixel_at(&pixels, w, 175, 50), [0, 128, 0]);
}

#[test]
fn soft_mask_bbox_limits_effect_to_its_own_region() {
    // Two horizontally adjacent red rects at y=0..100. Only the first
    // (x=0..100) gets a uniform-alpha=0 mask; the second (x=100..200)
    // has no mask and must remain red.
    let mut left = make_rect_shape(0.0, 0.0, 100.0, 100.0, Color::rgb(255, 0, 0));
    left.active_soft_masks = vec![mask_rect(
        SoftMaskSubtype::Alpha,
        BoundingBox::new(0.0, 0.0, 100.0, 100.0),
        vec![0],
        1,
        1,
    )];
    let right = make_rect_shape(100.0, 0.0, 100.0, 100.0, Color::rgb(255, 0, 0));
    let doc = document_from_shapes(200.0, 100.0, vec![left, right]);
    let (pixels, w, _h) = render_rgb(&doc, 72);
    // Left rect masked out -> white.
    assert_eq!(pixel_at(&pixels, w, 25, 50), [255, 255, 255]);
    // Right rect unmasked -> red.
    assert_eq!(pixel_at(&pixels, w, 175, 50), [255, 0, 0]);
}

#[test]
fn nested_soft_masks_intersect_by_min_combine() {
    // Two overlapping masks on the same shape. Each has uniform alpha
    // = 128; their min-combine is 128, so the paint is lerped to the
    // bg half-way.
    let mut shape = make_rect_shape(0.0, 0.0, 100.0, 100.0, Color::rgb(0, 255, 0));
    shape.active_soft_masks = vec![
        mask_rect(
            SoftMaskSubtype::Alpha,
            BoundingBox::new(0.0, 0.0, 100.0, 100.0),
            vec![128],
            1,
            1,
        ),
        mask_rect(
            SoftMaskSubtype::Alpha,
            BoundingBox::new(0.0, 0.0, 100.0, 100.0),
            vec![200],
            1,
            1,
        ),
    ];
    let doc = document_from_shapes(100.0, 100.0, vec![shape]);
    let (pixels, w, _h) = render_rgb(&doc, 72);
    // With the more restrictive 128-alpha mask winning, the center
    // pixel should be (127, 255, 127) give-or-take rounding.
    let [r, g, b] = pixel_at(&pixels, w, 50, 50);
    assert!((125..=130).contains(&r), "r: {r}");
    assert_eq!(g, 255);
    assert!((125..=130).contains(&b), "b: {b}");
}

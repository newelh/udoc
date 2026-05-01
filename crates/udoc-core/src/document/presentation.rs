//! Presentation overlay types: geometry, fonts, colors, page layout.
//!
//! This layer provides per-node spatial and visual data, keyed by NodeId
//! via Overlay (dense) or SparseOverlay (sparse). It is optional: absent
//! means the backend did not produce presentation data.

use crate::geometry::BoundingBox;

use super::overlay::{Overlay, SparseOverlay};

/// Presentation overlay: geometry, fonts, colors, page layout.
///
/// Dense vs sparse overlay choice per field:
/// - **Dense (Overlay):** page_assignments, geometry. Most nodes in paginated
///   formats (PDF, PPTX) have page assignments and bounding boxes.
/// - **Sparse (SparseOverlay):** text_styling, block_layout, column_specs,
///   layout_info. In flow-based formats (DOCX, ODT) only a fraction of nodes
///   have non-default styling or layout properties.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Presentation {
    /// Page definitions (dimensions, rotation).
    pub pages: Vec<PageDef>,
    /// Which page each node appears on. Dense: most nodes have a page.
    pub page_assignments: Overlay<usize>,
    /// Bounding boxes for nodes. Dense: most nodes in PDF/PPTX have geometry.
    pub geometry: Overlay<BoundingBox>,
    /// Extended text styling (font, size, color). Sparse: only non-default
    /// styled spans in DOCX/ODT, all spans in PDF.
    pub text_styling: SparseOverlay<ExtendedTextStyle>,
    /// Block-level layout (alignment, indent, spacing). Sparse: most blocks
    /// use default layout.
    pub block_layout: SparseOverlay<BlockLayout>,
    /// Table column specifications. Sparse: only table nodes have column specs.
    pub column_specs: SparseOverlay<Vec<ColSpec>>,
    /// Layout semantics for container nodes (flex, grid, etc.). Sparse: only
    /// container nodes in slide/web formats.
    pub layout_info: SparseOverlay<LayoutInfo>,
    /// Raw positioned text spans (escape hatch for PDF consumers).
    pub raw_spans: Vec<PositionedSpan>,
    /// Path shapes (lines, rectangles) from the page content stream.
    /// Used by the renderer to draw ruled lines, table borders, form
    /// outlines, and other non-text visual elements.
    pub shapes: Vec<PageShape>,
    /// Image placements for the renderer. Each entry locates an image
    /// asset on a specific page with its destination bounding box.
    pub image_placements: Vec<ImagePlacement>,
    /// Canonical paint-time path records for arbitrary fill + stroke
    /// rasterization ( /  / ).
    ///
    /// Distinct from `shapes` which is a flattened line/rect/polygon
    /// representation used by the table detector. `paint_paths` carries
    /// the full moveto/lineto/curveto/closepath IR plus CTM snapshot
    /// and stroke style, so the renderer can expand strokes to outlines,
    /// honour fill rules, and dash exact curves.
    pub paint_paths: Vec<PaintPath>,
    /// Shading-pattern records emitted by PDF `sh` operators
    /// (ISO 32000-2 §8.7.4,).
    ///
    /// Each entry carries the shading geometry (axial or radial), a
    /// pre-sampled color LUT, a CTM snapshot, alpha, and a z-index
    /// shared with `paint_paths` / `shapes` for back-to-front
    /// compositing. Unsupported shading types are recorded as
    /// `PaintShadingKind::Unsupported` so the renderer can skip
    /// gracefully (the PDF interpreter emits the warning once).
    pub shadings: Vec<PaintShading>,
    /// Tiling pattern records emitted by PDF Pattern colorspace fills
    /// (ISO 32000-2 §8.7.3,).
    ///
    /// Each entry describes one Type 1 coloured tiling pattern that is
    /// painted on a page: the tile cell's /BBox, spacing (/XStep,
    /// /YStep), pattern /Matrix, the nested /Resources dict, and the
    /// raw content-stream bytes that draw one tile. Rendering (tiling
    /// the cell across the fill region) is handled by
    /// in Wave 3.
    ///
    /// Type 2 (uncoloured tiling) and Type 2 shading-patterns fall
    /// through to the base fill color; the PDF interpreter
    /// emits `WarningKind::UnsupportedPatternType` once per occurrence.
    pub patterns: Vec<PaintPattern>,
}

/// Placement of an image asset on a page.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct ImagePlacement {
    pub page_index: usize,
    /// Destination bounding box in page coordinates (points). Axis-aligned
    /// bounding box of the placement's four corners. For rotated/sheared
    /// placements this is wider/taller than the painted region; see
    /// [`ImagePlacement::ctm`] for the full affine transform.
    pub bbox: BoundingBox,
    /// Index into `Document.assets.images()`.
    pub asset_index: usize,
    /// Pixel dimensions of the source image.
    pub width: u32,
    pub height: u32,
    /// Color space name (e.g., "DeviceRGB", "DeviceGray", "DeviceCMYK").
    pub color_space: Option<String>,
    /// Content stream render order for z-ordering.
    pub z_index: u32,
    /// True if this is a stencil image mask. The image data defines where
    /// to paint `mask_color`. Set bits are painted, clear bits are transparent.
    pub is_mask: bool,
    /// Fill color for image masks (RGB, 0-255).
    pub mask_color: [u8; 3],
    /// Soft mask alpha data (0=transparent, 255=opaque). When Some,
    /// the image should be alpha-blended onto the background.
    pub soft_mask: Option<Vec<u8>>,
    /// Soft mask width in pixels.
    pub soft_mask_width: u32,
    /// Soft mask height in pixels.
    pub soft_mask_height: u32,
    /// Full affine CTM for the placement, mapping the source unit square
    /// `(0,0)-(1,1)` to user-space (y-up, points). For pure scale + translate
    /// placements this simplifies to `[w, 0, 0, h, x_min, y_min]`. When the
    /// CTM has non-zero `b`/`c` (rotation/shear) the renderer should use
    /// this transform to paint the image with correct orientation; `bbox`
    /// alone loses that information.
    pub ctm: [f64; 6],
}

impl ImagePlacement {
    /// Create a new image placement with a pure scale+translate CTM
    /// synthesized from `bbox`. For placements that may carry a rotation or
    /// shear (e.g. `/Rotate 90` pages whose content stream pre-rotates the
    /// image), prefer [`ImagePlacement::with_ctm`] so the renderer can paint
    /// with the correct orientation.
    pub fn new(
        page_index: usize,
        bbox: BoundingBox,
        asset_index: usize,
        width: u32,
        height: u32,
        color_space: Option<String>,
    ) -> Self {
        let w = bbox.x_max - bbox.x_min;
        let h = bbox.y_max - bbox.y_min;
        Self {
            page_index,
            bbox,
            asset_index,
            width,
            height,
            color_space,
            z_index: 0,
            is_mask: false,
            mask_color: [0, 0, 0],
            soft_mask: None,
            soft_mask_width: 0,
            soft_mask_height: 0,
            ctm: [w, 0.0, 0.0, h, bbox.x_min, bbox.y_min],
        }
    }

    /// Create a new image placement with an explicit affine CTM. The CTM
    /// maps the source image's unit square to user space. `bbox` must be
    /// the axis-aligned bounding box of the four transformed corners.
    pub fn with_ctm(
        page_index: usize,
        bbox: BoundingBox,
        asset_index: usize,
        width: u32,
        height: u32,
        color_space: Option<String>,
        ctm: [f64; 6],
    ) -> Self {
        Self {
            page_index,
            bbox,
            asset_index,
            width,
            height,
            color_space,
            z_index: 0,
            is_mask: false,
            mask_color: [0, 0, 0],
            soft_mask: None,
            soft_mask_width: 0,
            soft_mask_height: 0,
            ctm,
        }
    }
}

impl Presentation {
    /// Returns true if the presentation overlay contains no data.
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
            && self.page_assignments.is_empty()
            && self.geometry.is_empty()
            && self.text_styling.is_empty()
            && self.block_layout.is_empty()
            && self.column_specs.is_empty()
            && self.layout_info.is_empty()
            && self.raw_spans.is_empty()
            && self.shapes.is_empty()
            && self.image_placements.is_empty()
            && self.paint_paths.is_empty()
            && self.shadings.is_empty()
            && self.patterns.is_empty()
    }
}

/// A rendered path shape on a page (line, rectangle, or filled area).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct PageShape {
    pub page_index: usize,
    pub kind: PathShapeKind,
    /// Whether the path is stroked (outlined).
    pub stroked: bool,
    /// Whether the path is filled.
    pub filled: bool,
    /// Stroke line width in points.
    pub line_width: f64,
    /// Stroke color (default: black).
    pub stroke_color: Option<Color>,
    /// Fill color (default: black).
    pub fill_color: Option<Color>,
    /// Content stream render order for z-ordering.
    pub z_index: u32,
    /// Fill opacity (0-255). 255 = fully opaque.
    pub fill_alpha: u8,
    /// Stroke opacity (0-255). 255 = fully opaque.
    pub stroke_alpha: u8,
    /// Active clipping regions at the moment this shape was painted
    /// (ISO 32000-2 §8.5.4). Empty = no clipping.
    #[cfg_attr(feature = "serde", serde(default))]
    pub active_clips: Vec<ClipRegion>,
    /// Active soft-mask layers at the moment this shape was painted
    /// ( extension, ISO 32000-2 §11.6.5). Empty = no soft-mask.
    /// Stacked soft masks (nested ExtGState /SMask) intersect by
    /// min-combine of per-pixel alphas.
    #[cfg_attr(feature = "serde", serde(default))]
    pub active_soft_masks: Vec<SoftMaskLayer>,
}

impl PageShape {
    /// Create a new path shape.
    pub fn new(
        page_index: usize,
        kind: PathShapeKind,
        stroked: bool,
        filled: bool,
        line_width: f64,
    ) -> Self {
        Self {
            page_index,
            kind,
            stroked,
            filled,
            line_width,
            stroke_color: None,
            fill_color: None,
            z_index: 0,
            fill_alpha: 255,
            stroke_alpha: 255,
            active_clips: Vec::new(),
            active_soft_masks: Vec::new(),
        }
    }
}

/// A single segment inside a [`PaintPath`]. Mirrors
/// `udoc_pdf::content::path::PathSegmentKind` in the core presentation
/// layer so the renderer does not need a PDF dependency.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PaintSegment {
    /// Begin a new subpath at `(x, y)`.
    MoveTo { x: f64, y: f64 },
    /// Straight line from the current point to `(x, y)`.
    LineTo { x: f64, y: f64 },
    /// Cubic bezier from the current point through (c1, c2) to (end).
    CurveTo {
        c1x: f64,
        c1y: f64,
        c2x: f64,
        c2y: f64,
        ex: f64,
        ey: f64,
    },
    /// Close the current subpath with a straight line to the last
    /// [`PaintSegment::MoveTo`].
    ClosePath,
}

/// Line cap style (ISO 32000-2 §8.4.3.3, operator `J`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PaintLineCap {
    Butt,
    Round,
    ProjectingSquare,
}

/// Line join style (ISO 32000-2 §8.4.3.4, operator `j`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PaintLineJoin {
    Miter,
    Round,
    Bevel,
}

/// Stroke parameters captured at paint time.
///
/// `line_width` is in user space (pre-CTM). The rasterizer scales it by
/// the CTM when expanding the stroke into an outline polygon.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PaintStroke {
    pub line_width: f32,
    pub line_cap: PaintLineCap,
    pub line_join: PaintLineJoin,
    pub miter_limit: f32,
    pub dash_pattern: Vec<f32>,
    pub dash_phase: f32,
    pub color: Color,
    /// Stroke opacity (0-255). 255 = fully opaque.
    pub alpha: u8,
}

/// A paint-time path record ( / ).
///
/// Captures a full moveto/lineto/curveto/closepath sequence in user
/// space plus the CTM snapshot required to map into device pixels,
/// along with fill rule (if filled) and stroke style (if stroked).
/// Emitted by the PDF interpreter once per paint operator and
/// consumed by the renderer to rasterize arbitrary geometry.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct PaintPath {
    pub page_index: usize,
    pub segments: Vec<PaintSegment>,
    /// `Some(rule)` if the path was painted with a fill op, `None` for
    /// stroke-only / clip-only paints.
    pub fill: Option<FillRule>,
    /// Fill color; only meaningful when `fill` is `Some`.
    pub fill_color: Option<Color>,
    /// Fill opacity (0-255). 255 = fully opaque.
    pub fill_alpha: u8,
    /// `Some(style)` if the path was painted with a stroke op.
    pub stroke: Option<PaintStroke>,
    /// CTM snapshot at the exact moment the paint operator executed.
    /// Stored as `[a, b, c, d, e, f]` in PDF row-vector convention:
    /// `x' = x*a + y*c + e`, `y' = x*b + y*d + f`.
    pub ctm: [f64; 6],
    /// Paint-order index within the page (0-based).
    pub z_index: u32,
}

impl PaintPath {
    /// Create a new paint-time path record.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        page_index: usize,
        segments: Vec<PaintSegment>,
        fill: Option<FillRule>,
        fill_color: Option<Color>,
        fill_alpha: u8,
        stroke: Option<PaintStroke>,
        ctm: [f64; 6],
        z_index: u32,
    ) -> Self {
        Self {
            page_index,
            segments,
            fill,
            fill_color,
            fill_alpha,
            stroke,
            ctm,
            z_index,
        }
    }
}

/// Fill rule for polygon rendering (PDF spec 8.5.3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum FillRule {
    /// Non-zero winding number rule (f, F, B, b operators).
    #[default]
    NonZeroWinding,
    /// Even-odd rule (f*, B*, b* operators).
    EvenOdd,
}

/// A shading-pattern record in the presentation overlay.
///
/// Produced by the PDF interpreter's 'sh' operator, consumed by the
/// page renderer's shading rasterizer. Mirrors
/// `udoc_pdf::PageShadingKind` with no PDF dependency.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum PaintShadingKind {
    /// Axial (linear) gradient from `p0` to `p1` in shading user space.
    /// The enclosing `PaintShading` carries the CTM snapshot that maps
    /// user space into page space for rasterization.
    Axial {
        /// Gradient axis start.
        p0x: f64,
        p0y: f64,
        /// Gradient axis end.
        p1x: f64,
        p1y: f64,
        /// 256-entry pre-sampled sRGB LUT over `t = [0, 1]`.
        samples: Vec<[u8; 3]>,
        /// Extend the start color past `t < 0`.
        extend_start: bool,
        /// Extend the end color past `t > 1`.
        extend_end: bool,
    },
    /// Radial (circle-to-circle) gradient.
    Radial {
        /// Start circle centre.
        c0x: f64,
        c0y: f64,
        /// Start circle radius.
        r0: f64,
        /// End circle centre.
        c1x: f64,
        c1y: f64,
        /// End circle radius.
        r1: f64,
        /// 256-entry pre-sampled sRGB LUT over `t = [0, 1]`.
        samples: Vec<[u8; 3]>,
        /// Extend the start color past `t < 0`.
        extend_start: bool,
        /// Extend the end color past `t > 1`.
        extend_end: bool,
    },
    /// Unsupported shading type (1, 4-7). Renderer skips; the PDF
    /// interpreter already emitted the diagnostics warning.
    Unsupported {
        /// The raw /ShadingType value from the source PDF.
        shading_type: u32,
    },
}

/// A shading-pattern placement for the renderer.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct PaintShading {
    /// Page index this shading is painted on.
    pub page_index: usize,
    /// Shading geometry + color LUT.
    pub kind: PaintShadingKind,
    /// CTM snapshot at the 'sh' op, `[a, b, c, d, e, f]`.
    pub ctm: [f64; 6],
    /// Fill-side opacity (0..=255).
    pub alpha: u8,
    /// Paint-order index within the page (z-order).
    pub z_index: u32,
}

impl PaintShading {
    /// Create a new paint-time shading record.
    pub fn new(
        page_index: usize,
        kind: PaintShadingKind,
        ctm: [f64; 6],
        alpha: u8,
        z_index: u32,
    ) -> Self {
        Self {
            page_index,
            kind,
            ctm,
            alpha,
            z_index,
        }
    }
}

/// A Type 1 coloured tiling pattern captured from the page's
/// `/Resources /Pattern` dict (ISO 32000-2 §8.7.3,
/// ).
///
/// The pattern itself owns the tile's content stream (drawing ops for
/// one cell), plus the geometry needed to tile the cell across a fill
/// region: `bbox` (tile extent in pattern space), `xstep` / `ystep`
/// (spacing between tile origins), `matrix` (pattern-space -> page-space
/// transform), and a flat list of "nested resource names" that the
/// renderer will need to resolve from the pattern's own /Resources.
///
/// `ctm_at_paint` is the device-space transform captured at the moment
/// the pattern was painted (or, when emitted at page-resource-scan
/// time, the identity matrix as a placeholder; in
/// Wave 3 replaces this with the actual fill-op CTM).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct PaintPattern {
    /// Page index this pattern was painted on.
    pub page_index: usize,
    /// Resource name the pattern was registered under in the page's
    /// `/Resources /Pattern` dict (e.g., "P1", "Pat0"). Callers match
    /// on this when wiring up `scn`/`SCN` calls.
    pub resource_name: String,
    /// Tile cell bounding box in pattern coordinates (points).
    /// `[llx, lly, urx, ury]` per `/BBox`.
    pub bbox: [f64; 4],
    /// Horizontal spacing between tile origins in pattern coordinates.
    pub xstep: f64,
    /// Vertical spacing between tile origins in pattern coordinates.
    pub ystep: f64,
    /// Pattern-to-userspace transform `[a, b, c, d, e, f]` (defaults to
    /// the identity when `/Matrix` is absent).
    pub matrix: [f64; 6],
    /// Raw content-stream bytes that draw one tile (already filter-
    /// decoded). The renderer interprets these as a mini PDF content
    /// stream using `resource_refs` for name lookups.
    #[cfg_attr(feature = "serde", serde(default))]
    pub content_stream: Vec<u8>,
    /// CTM snapshot captured at the paint op. When the pattern was
    /// emitted at page-resource-scan time rather than from an `scn`
    /// op, this is the identity matrix; Wave 3's
    /// replaces it with the actual paint-time CTM.
    pub ctm_at_paint: [f64; 6],
    /// Fill opacity at paint time (0..=255). 255 when emitted from
    /// resource enumeration (no gstate context).
    pub alpha: u8,
    /// Paint-order index within the page (0-based). Shared with
    /// `paint_paths` / `shapes` / `shadings` for back-to-front
    /// compositing.
    pub z_index: u32,
    /// Closed user-space subpaths defining the fill region this
    /// pattern paints into. Each inner `Vec<(f64, f64)>` is a closed
    /// polygon (last vertex implicitly connects to the first). Empty
    /// when the pattern was emitted from resource enumeration rather
    /// than a paint op.
    #[cfg_attr(feature = "serde", serde(default))]
    pub fill_subpaths: Vec<Vec<(f64, f64)>>,
    /// Fill rule for `fill_subpaths` (NonZero or EvenOdd).
    #[cfg_attr(feature = "serde", serde(default))]
    pub fill_rule: FillRule,
    /// Fallback solid fill color sampled from the tile content stream
    /// (first `rg`/`g`/`k`/`scn` op). Used when tile rasterization
    /// fails or for pattern kinds we don't fully implement.
    #[cfg_attr(feature = "serde", serde(default))]
    pub fallback_color: Option<Color>,
}

impl PaintPattern {
    /// Create a new Type 1 coloured tiling pattern record. Fill region
    /// fields default to empty; use
    /// [`with_fill_region`](Self::with_fill_region) to attach them at
    /// paint time.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        page_index: usize,
        resource_name: String,
        bbox: [f64; 4],
        xstep: f64,
        ystep: f64,
        matrix: [f64; 6],
        content_stream: Vec<u8>,
        ctm_at_paint: [f64; 6],
        alpha: u8,
        z_index: u32,
    ) -> Self {
        Self {
            page_index,
            resource_name,
            bbox,
            xstep,
            ystep,
            matrix,
            content_stream,
            ctm_at_paint,
            alpha,
            z_index,
            fill_subpaths: Vec::new(),
            fill_rule: FillRule::NonZeroWinding,
            fallback_color: None,
        }
    }

    /// Attach a fill region (closed subpaths + fill rule) plus an
    /// optional fallback solid color sampled from the tile stream.
    /// Returns the modified record for a fluent API.
    pub fn with_fill_region(
        mut self,
        fill_subpaths: Vec<Vec<(f64, f64)>>,
        fill_rule: FillRule,
        fallback_color: Option<Color>,
    ) -> Self {
        self.fill_subpaths = fill_subpaths;
        self.fill_rule = fill_rule;
        self.fallback_color = fallback_color;
        self
    }
}

/// A clipping region attached to a shape or span.
///
/// Captured by the PDF interpreter at a W/W* operator in device
/// coordinates (after CTM multiplication), then rewritten to page
/// coordinates at the facade boundary so the renderer can transform
/// them back with the same `page_height` + `scale` math as for
/// visible shapes. Curves are pre-flattened to line segments.
///
/// Subpaths are implicitly closed (start vertex implied at the end).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ClipRegion {
    /// Closed subpaths in page coordinates (pre-y-flip, same convention
    /// as `ImagePlacement::bbox`).
    pub subpaths: Vec<Vec<(f64, f64)>>,
    /// Fill rule from the clip operator (`W` -> `NonZero`,
    /// `W*` -> `EvenOdd`).
    pub fill_rule: ClipRegionFillRule,
}

/// Fill rule for a `ClipRegion`. Mirror of `FillRule` to keep the
/// clipping types self-documenting at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ClipRegionFillRule {
    /// Non-zero winding rule (`W`).
    NonZero,
    /// Even-odd rule (`W*`).
    EvenOdd,
}

/// A soft-mask layer attached to a shape or span ( extension,
/// ISO 32000-2 §11.6.5).
///
/// Soft masks come from an ExtGState `/SMask` dict whose `/G` entry is a
/// transparency group form XObject. The form XObject is evaluated to a
/// grayscale bitmap; `subtype` selects whether that bitmap is interpreted
/// as a luminosity channel (Y = 0.2126 R + 0.7152 G + 0.0722 B, Rec.709)
/// or as an alpha channel directly.
///
/// The mask covers the `bbox` region in page coordinates (y-up, pre-flip).
/// Outside the bbox, `backdrop_alpha` is used as the implicit mask value.
/// `backdrop_alpha` is derived from the /BC (backdrop color) array on the
/// SMask dict: in luminosity mode, /BC's luminance is the mask value
/// outside the form; in alpha mode, /BC is ignored (the form's own alpha
/// is the only signal).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SoftMaskLayer {
    /// Luminosity vs Alpha subtype.
    pub subtype: SoftMaskSubtype,
    /// Mask bitmap: one byte per pixel (0 = fully transparent,
    /// 255 = fully opaque). Interpretation depends on `subtype`.
    pub mask: Vec<u8>,
    /// Mask bitmap width (pixels).
    pub width: u32,
    /// Mask bitmap height (pixels).
    pub height: u32,
    /// Bounding box of the mask in page coordinates (y-up).
    pub bbox: BoundingBox,
    /// Backdrop alpha outside the form XObject's coverage (0..=255).
    /// Defaults to 255 (fully opaque, no masking outside bbox) if /BC
    /// is absent or fails to decode.
    pub backdrop_alpha: u8,
}

/// Subtype of a soft mask per ISO 32000-2 §11.6.5.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SoftMaskSubtype {
    /// `/S /Luminosity`: the form's rendered color is converted to a
    /// single luminosity value per pixel (Y = 0.2126 R + 0.7152 G +
    /// 0.0722 B, Rec.709 weights) and that luminance is the mask alpha.
    Luminosity,
    /// `/S /Alpha`: the form's rendered alpha channel is the mask alpha
    /// directly; RGB channels are discarded.
    Alpha,
}

/// The geometric shape type.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum PathShapeKind {
    /// A line segment from (x1, y1) to (x2, y2) in page coordinates.
    Line { x1: f64, y1: f64, x2: f64, y2: f64 },
    /// A rectangle at (x, y) with width and height in page coordinates.
    Rect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    /// An arbitrary closed polygon. Each subpath is a list of (x, y)
    /// vertices in page coordinates, implicitly closed.
    Polygon {
        subpaths: Vec<Vec<(f64, f64)>>,
        fill_rule: FillRule,
    },
}

/// Page definition (dimensions, rotation, and optional origin offset).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct PageDef {
    pub index: usize,
    pub width: f64,
    pub height: f64,
    /// Page rotation in degrees. Valid values are 0, 90, 180, 270.
    /// PDF pages may report other values; backends normalize to the
    /// nearest valid rotation.
    pub rotation: u16,
    /// X offset of the displayed region in content-space coordinates.
    /// For PDF, this is the x_min of CropBox (or MediaBox if no CropBox).
    /// Content-space coordinates must be translated by (-origin_x, -origin_y)
    /// before rendering so that the visible region starts at pixel (0,0).
    pub origin_x: f64,
    /// Y offset of the displayed region in content-space coordinates.
    /// For PDF, this is the y_min of CropBox (or MediaBox).
    pub origin_y: f64,
}

impl PageDef {
    /// Create a new page definition with zero origin offset.
    pub fn new(index: usize, width: f64, height: f64, rotation: u16) -> Self {
        Self {
            index,
            width,
            height,
            rotation,
            origin_x: 0.0,
            origin_y: 0.0,
        }
    }

    /// Create a new page definition with an explicit origin offset.
    /// Used for PDFs whose /CropBox or /MediaBox does not start at (0, 0).
    pub fn with_origin(
        index: usize,
        width: f64,
        height: f64,
        rotation: u16,
        origin_x: f64,
        origin_y: f64,
    ) -> Self {
        Self {
            index,
            width,
            height,
            rotation,
            origin_x,
            origin_y,
        }
    }
}

/// Extended text styling that lives in the presentation overlay.
/// Font name, size, and colors are purely visual and do not carry
/// semantic weight.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct ExtendedTextStyle {
    pub font_name: Option<String>,
    pub font_size: Option<f64>,
    pub color: Option<Color>,
    pub background_color: Option<Color>,
    /// Additional spacing between characters, in points.
    /// For PDF sources, this is the raw Tc (character spacing) value from the
    /// graphics state. Per the PDF spec (Section 9.3.3), Tc is specified in
    /// unscaled text space units, which default to points in standard user space.
    pub letter_spacing: Option<f64>,
}

impl ExtendedTextStyle {
    /// Create a new empty style. Use the builder methods to set fields.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn font_name(mut self, name: Option<String>) -> Self {
        self.font_name = name;
        self
    }

    pub fn font_size(mut self, size: Option<f64>) -> Self {
        self.font_size = size;
        self
    }

    pub fn color(mut self, color: Option<Color>) -> Self {
        self.color = color;
        self
    }

    pub fn background_color(mut self, color: Option<Color>) -> Self {
        self.background_color = color;
        self
    }

    pub fn letter_spacing(mut self, spacing: Option<f64>) -> Self {
        self.letter_spacing = spacing;
        self
    }

    /// Returns true if all fields are None (no styling to store).
    pub fn is_empty(&self) -> bool {
        self.font_name.is_none()
            && self.font_size.is_none()
            && self.color.is_none()
            && self.background_color.is_none()
            && self.letter_spacing.is_none()
    }
}

/// Block-level layout properties.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct BlockLayout {
    pub alignment: Option<Alignment>,
    pub indent_left: Option<f64>,
    pub indent_right: Option<f64>,
    pub space_before: Option<f64>,
    pub space_after: Option<f64>,
    pub background_color: Option<Color>,
}

impl BlockLayout {
    /// Create a new empty layout. Use the builder methods to set fields.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn alignment(mut self, alignment: Option<Alignment>) -> Self {
        self.alignment = alignment;
        self
    }

    pub fn indent_left(mut self, indent: Option<f64>) -> Self {
        self.indent_left = indent;
        self
    }

    pub fn indent_right(mut self, indent: Option<f64>) -> Self {
        self.indent_right = indent;
        self
    }

    pub fn space_before(mut self, space: Option<f64>) -> Self {
        self.space_before = space;
        self
    }

    pub fn space_after(mut self, space: Option<f64>) -> Self {
        self.space_after = space;
        self
    }

    pub fn background_color(mut self, color: Option<Color>) -> Self {
        self.background_color = color;
        self
    }

    /// Returns true if all fields are None (no layout to store).
    pub fn is_empty(&self) -> bool {
        self.alignment.is_none()
            && self.indent_left.is_none()
            && self.indent_right.is_none()
            && self.space_before.is_none()
            && self.space_after.is_none()
            && self.background_color.is_none()
    }
}

/// Table column specification.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct ColSpec {
    pub width: Option<f64>,
    pub alignment: Option<Alignment>,
}

impl ColSpec {
    /// Create a ColSpec with the given width (in points). Alignment defaults to None.
    pub fn with_width(width: f64) -> Self {
        Self {
            width: Some(width),
            alignment: None,
        }
    }

    /// Create an empty ColSpec (no width, no alignment).
    pub fn empty() -> Self {
        Self {
            width: None,
            alignment: None,
        }
    }
}

/// Layout info for container nodes.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct LayoutInfo {
    pub mode: LayoutMode,
    pub direction: Option<FlowDirection>,
    pub gap: Option<f64>,
    pub padding: Option<Padding>,
    /// Whether the container wraps its children. Defaults to `false`.
    /// Uses `serde(default)` so older serialized documents without this
    /// field deserialize correctly.
    #[cfg_attr(feature = "serde", serde(default))]
    pub wrap: bool,
}

/// Layout mode for containers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
#[non_exhaustive]
pub enum LayoutMode {
    Flow,
    FlexRow,
    FlexColumn,
    Grid,
    Absolute,
}

/// Flow direction for layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
#[non_exhaustive]
pub enum FlowDirection {
    LeftToRight,
    RightToLeft,
    TopToBottom,
}

/// Padding values (in points).
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Padding {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

impl Padding {
    /// Create a new Padding with explicit values.
    pub fn new(top: f64, right: f64, bottom: f64, left: f64) -> Self {
        Self {
            top,
            right,
            bottom,
            left,
        }
    }
}

/// A positioned text span from a paginated format. Preserves the raw
/// character-level position data for consumers who need it.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct PositionedSpan {
    pub text: String,
    pub bbox: BoundingBox,
    pub page_index: usize,
    pub font_name: Option<String>,
    pub font_size: Option<f64>,
    pub is_bold: bool,
    pub is_italic: bool,
    /// Fill color as RGB, if not black/default.
    pub color: Option<Color>,
    /// Character spacing in text space units, if non-zero.
    pub letter_spacing: Option<f64>,
    /// Whether text rise indicates superscript positioning.
    /// Inferred from positive text rise (PDF Ts > 0) or explicit markup
    /// (DOCX w:vertAlign="superscript", RTF \super). Mutually exclusive
    /// with `is_subscript`; if both are somehow set, superscript wins.
    pub is_superscript: bool,
    /// Whether text rise indicates subscript positioning.
    /// Inferred from negative text rise (PDF Ts < 0) or explicit markup
    /// (DOCX w:vertAlign="subscript", RTF \sub). Mutually exclusive with
    /// `is_superscript`.
    pub is_subscript: bool,
    /// Per-character advance widths in text space, if available.
    /// Each entry corresponds to one character in `text` (by chars()).
    /// Multiply by `advance_scale` to get user-space (page coordinate) widths.
    pub char_advances: Option<Vec<f64>>,
    /// Text-space to user-space horizontal scaling factor.
    /// Multiply char_advances by this to get page-coordinate advances,
    /// then by DPI scale to get pixel advances.
    #[cfg_attr(feature = "serde", serde(default = "default_advance_scale"))]
    pub advance_scale: f64,
    /// Original character codes from the PDF content stream (one byte per
    /// character for simple fonts). Used by the renderer for by-code glyph
    /// lookup in subset fonts with custom encodings.
    pub char_codes: Option<Vec<u8>>,
    /// Glyph IDs for composite (CID) fonts. Each entry is the raw 2-byte
    /// character code from the PDF content stream, which equals the GID for
    /// Identity-H/V encodings. Used by the renderer for direct GID-based
    /// glyph outline lookup.
    pub char_gids: Option<Vec<u16>>,
    /// Per-glyph bounding boxes in page coordinates.
    ///
    /// The length corresponds to GLYPHS rendered by the original content-stream
    /// operator, not characters: a single ligature glyph mapped to "fi" via
    /// ToUnicode produces one bbox for both chars. When `char_advances` is
    /// Some, its length equals this vector's length (glyph count matches
    /// char count). Otherwise use this vector's length as the glyph count
    /// and do not try to map by character index.
    ///
    /// See [`PositionedSpan::glyph_bbox_for_char_index`] for a safe lookup
    /// helper.
    #[cfg_attr(feature = "serde", serde(default))]
    pub glyph_bboxes: Option<Vec<BoundingBox>>,
    /// Rotation angle in degrees (0 = horizontal, 90 = vertical upward).
    /// Non-zero for rotated text like arXiv sidebars.
    pub rotation: f64,
    /// Content stream render order for interleaving images/shapes/text
    /// in the correct z-order during rendering. 0 = no ordering info.
    pub z_index: u32,
    /// Unique per-subset font identifier (e.g., subset-prefixed PDF font name).
    ///
    /// PDF documents frequently embed several subsets of the same display
    /// font (same `font_name` after subset-prefix stripping). Each subset
    /// has its own glyph program and encoding; keying the renderer's font
    /// cache by `font_name` alone collapses them into one entry, causing
    /// bytes from one subset to render via another subset's glyph program.
    ///
    /// When present, the renderer uses this as the font cache key. When
    /// absent (non-PDF backends, or PDFs without subset prefixes), the
    /// renderer falls back to `font_name`.
    #[cfg_attr(feature = "serde", serde(default))]
    pub font_id: Option<String>,
    /// How the font backing this span was resolved.
    ///
    /// `FontResolution::Exact` (the default) means the document's font was
    /// loaded as referenced. Other variants indicate a fallback, which may
    /// silently alter the extracted text or geometry. Consumers that care
    /// about fidelity should filter with `font_resolution.is_fallback()`.
    #[cfg_attr(feature = "serde", serde(default))]
    pub font_resolution: crate::text::FontResolution,
    /// Active clipping regions at the moment this span was painted
    /// (ISO 32000-2 §8.5.4). Empty = no clipping.
    #[cfg_attr(feature = "serde", serde(default))]
    pub active_clips: Vec<ClipRegion>,
    /// Active soft-mask layers at the moment this span was painted
    /// ( extension, ISO 32000-2 §11.6.5). Empty = no soft-mask.
    #[cfg_attr(feature = "serde", serde(default))]
    pub active_soft_masks: Vec<SoftMaskLayer>,
}

#[cfg(feature = "serde")]
fn default_advance_scale() -> f64 {
    1.0
}

impl PositionedSpan {
    /// Create a new PositionedSpan with required fields. Optional fields
    /// default to None/false.
    pub fn new(text: String, bbox: BoundingBox, page_index: usize) -> Self {
        Self {
            text,
            bbox,
            page_index,
            font_name: None,
            font_size: None,
            is_bold: false,
            is_italic: false,
            color: None,
            letter_spacing: None,
            is_superscript: false,
            is_subscript: false,
            char_advances: None,
            advance_scale: 1.0,
            char_codes: None,
            char_gids: None,
            glyph_bboxes: None,
            rotation: 0.0,
            z_index: 0,
            font_id: None,
            font_resolution: crate::text::FontResolution::Exact,
            active_clips: Vec::new(),
            active_soft_masks: Vec::new(),
        }
    }

    /// Look up the bounding box for the glyph covering `char_index`
    /// (a 0-based index into `text.chars()`).
    ///
    /// Returns None when glyph bboxes are not populated, when the index
    /// is out of range, or when there's a glyph/char count mismatch
    /// (e.g. a ligature glyph expanded to multiple chars via ToUnicode).
    pub fn glyph_bbox_for_char_index(&self, char_index: usize) -> Option<BoundingBox> {
        let bboxes = self.glyph_bboxes.as_ref()?;
        let char_count = self.text.chars().count();
        if bboxes.len() != char_count {
            return None;
        }
        bboxes.get(char_index).copied()
    }
}

/// Text alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
#[non_exhaustive]
pub enum Alignment {
    Left,
    Center,
    Right,
    Justify,
}

impl Alignment {
    /// Parse an alignment string from any supported format.
    ///
    /// Handles DOCX ("left", "start", "right", "end", "center", "both",
    /// "justify"), PPTX ("l", "ctr", "r", "just"), ODF ("start", "left",
    /// "center", "end", "right", "justify"), and XLSX ("left", "center",
    /// "right", "justify").
    ///
    /// Note: "start"/"end" are mapped to Left/Right assuming LTR text
    /// direction. RTL documents would need the opposite mapping, which
    /// is not yet implemented.
    pub fn from_format_str(s: &str) -> Option<Self> {
        match s {
            // TODO(RTL): "start"/"end" assume LTR. When RTL text direction
            // is supported, these should check the paragraph's bidi context.
            "left" | "start" | "l" => Some(Alignment::Left),
            "center" | "ctr" => Some(Alignment::Center),
            "right" | "end" | "r" => Some(Alignment::Right),
            "justify" | "just" | "both" => Some(Alignment::Justify),
            _ => None,
        }
    }
}

/// RGB color.
///
/// Note: alpha channel is not yet supported. PDF transparency (SMask),
/// DOCX theme tint/shade, and PPTX fill alpha will need an `a: u8` field
/// in a future version.
///
/// Theme-based colors (DOCX w:themeColor, XLSX theme index, PPTX schemeClr)
/// are only resolved when the backend has access to the theme data. PPTX
/// resolves scheme colors from theme1.xml with hardcoded fallbacks for
/// standard names (dk1, lt1, tx1, bg1). XLSX and DOCX do not yet resolve
/// theme colors and emit a diagnostic warning when they are encountered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    /// Create a color from RGB components.
    pub fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parse a 6-digit bare hex string without prefix (e.g. "FF0000") into
    /// a Color. Returns None if the string is not exactly 6 ASCII hex chars.
    ///
    /// Use this for OOXML `val="FF0000"` attributes and PDF hex colors.
    /// For CSS-style "#FF0000", use [`Color::from_css_hex`].
    /// For ARGB "FFFF0000", use [`Color::from_argb_hex`].
    pub fn from_hex(hex: &str) -> Option<Self> {
        let bytes = hex.as_bytes();
        if bytes.len() != 6 || !hex.is_ascii() {
            return None;
        }
        // Safe to use str slicing: is_ascii() guarantees single-byte chars.
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        Some(Self { r, g, b })
    }

    /// Parse a CSS-style hex color (e.g. "#FF0000") into a Color.
    /// The `#` prefix is required. Leading/trailing whitespace is trimmed.
    ///
    /// Use this for ODF `fo:color="#FF0000"` attributes.
    pub fn from_css_hex(value: &str) -> Option<Self> {
        let hex = value.trim().strip_prefix('#')?;
        Self::from_hex(hex)
    }

    /// Parse an 8-digit ARGB hex string (e.g. "FFFF0000"), skipping the
    /// alpha channel (first two hex digits).
    ///
    /// Use this for XLSX `rgb="FFFF0000"` attributes where the first two
    /// hex digits encode alpha.
    pub fn from_argb_hex(s: &str) -> Option<Self> {
        if s.len() != 8 || !s.is_ascii() {
            return None;
        }
        let r = u8::from_str_radix(&s[2..4], 16).ok()?;
        let g = u8::from_str_radix(&s[4..6], 16).ok()?;
        let b = u8::from_str_radix(&s[6..8], 16).ok()?;
        Some(Self { r, g, b })
    }

    /// Convert to a raw `[r, g, b]` array.
    pub fn to_array(self) -> [u8; 3] {
        [self.r, self.g, self.b]
    }
}

impl From<[u8; 3]> for Color {
    fn from([r, g, b]: [u8; 3]) -> Self {
        Self { r, g, b }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::NodeId;

    #[test]
    fn presentation_default() {
        let p = Presentation::default();
        assert!(p.pages.is_empty());
        assert!(p.page_assignments.is_empty());
        assert!(p.geometry.is_empty());
        assert!(p.text_styling.is_empty());
        assert!(p.raw_spans.is_empty());
    }

    #[test]
    fn color_rgb() {
        let c = Color::rgb(255, 128, 0);
        assert_eq!(c.r, 255);
        assert_eq!(c.g, 128);
        assert_eq!(c.b, 0);
    }

    #[test]
    fn extended_text_style_default() {
        let s = ExtendedTextStyle::default();
        assert!(s.font_name.is_none());
        assert!(s.font_size.is_none());
        assert!(s.color.is_none());
    }

    #[test]
    fn block_layout_default() {
        let l = BlockLayout::default();
        assert!(l.alignment.is_none());
        assert!(l.indent_left.is_none());
    }

    #[test]
    fn presentation_with_pages() {
        let mut p = Presentation::default();
        p.pages.push(PageDef::new(0, 612.0, 792.0, 0));
        let id = NodeId::new(5);
        p.page_assignments.set(id, 0);
        p.geometry.set(id, BoundingBox::new(0.0, 0.0, 100.0, 50.0));

        assert_eq!(p.pages.len(), 1);
        assert_eq!(p.page_assignments.get(id), Some(&0));
        assert!(p.geometry.contains(id));
    }

    #[test]
    fn layout_mode_eq() {
        assert_eq!(LayoutMode::Flow, LayoutMode::Flow);
        assert_ne!(LayoutMode::Flow, LayoutMode::Grid);
    }

    #[test]
    fn alignment_eq() {
        assert_eq!(Alignment::Left, Alignment::Left);
        assert_ne!(Alignment::Left, Alignment::Right);
    }

    #[test]
    fn alignment_from_format_str() {
        // DOCX / ODF canonical
        assert_eq!(Alignment::from_format_str("left"), Some(Alignment::Left));
        assert_eq!(
            Alignment::from_format_str("center"),
            Some(Alignment::Center)
        );
        assert_eq!(Alignment::from_format_str("right"), Some(Alignment::Right));
        assert_eq!(
            Alignment::from_format_str("justify"),
            Some(Alignment::Justify)
        );
        // DOCX aliases
        assert_eq!(Alignment::from_format_str("start"), Some(Alignment::Left));
        assert_eq!(Alignment::from_format_str("end"), Some(Alignment::Right));
        assert_eq!(Alignment::from_format_str("both"), Some(Alignment::Justify));
        // PPTX short forms
        assert_eq!(Alignment::from_format_str("l"), Some(Alignment::Left));
        assert_eq!(Alignment::from_format_str("ctr"), Some(Alignment::Center));
        assert_eq!(Alignment::from_format_str("r"), Some(Alignment::Right));
        assert_eq!(Alignment::from_format_str("just"), Some(Alignment::Justify));
        // Unknown
        assert_eq!(Alignment::from_format_str("middle"), None);
        assert_eq!(Alignment::from_format_str(""), None);
    }

    #[test]
    fn color_from_hex() {
        assert_eq!(Color::from_hex("FF0000"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(Color::from_hex("00ff00"), Some(Color::rgb(0, 255, 0)));
        assert_eq!(Color::from_hex("0000FF"), Some(Color::rgb(0, 0, 255)));
        assert_eq!(Color::from_hex("FF00"), None);
        assert_eq!(Color::from_hex("GGGGGG"), None);
        assert_eq!(Color::from_hex(""), None);
    }

    #[test]
    fn color_from_css_hex() {
        assert_eq!(Color::from_css_hex("#FF0000"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(Color::from_css_hex("FF0000"), None);
        assert_eq!(Color::from_css_hex("#GG0000"), None);
    }

    #[test]
    fn color_from_argb_hex() {
        assert_eq!(
            Color::from_argb_hex("FFFF0000"),
            Some(Color::rgb(255, 0, 0))
        );
        assert_eq!(
            Color::from_argb_hex("00FF0000"),
            Some(Color::rgb(255, 0, 0))
        );
        assert_eq!(Color::from_argb_hex("short"), None);
    }

    #[test]
    fn color_to_array() {
        assert_eq!(Color::rgb(1, 2, 3).to_array(), [1, 2, 3]);
    }

    #[test]
    fn padding_values() {
        let p = Padding::new(10.0, 20.0, 10.0, 20.0);
        assert!((p.top - 10.0).abs() < f64::EPSILON);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn layout_info_wrap_defaults_false() {
        // LayoutInfo.wrap is a bool, not Option. It must default to false
        // when omitted from JSON for forward compatibility.
        let json = r#"{"mode":"flow"}"#;
        let info: LayoutInfo = serde_json::from_str(json).expect("should deserialize");
        assert!(!info.wrap);
    }

    #[test]
    fn presentation_is_empty_default() {
        let p = Presentation::default();
        assert!(p.is_empty());
    }

    #[test]
    fn presentation_is_empty_with_pages() {
        let mut p = Presentation::default();
        assert!(p.is_empty());
        p.pages.push(PageDef::new(0, 612.0, 792.0, 0));
        assert!(!p.is_empty());
    }

    #[test]
    fn presentation_is_empty_with_text_styling() {
        let mut p = Presentation::default();
        let id = NodeId::new(1);
        p.text_styling.set(
            id,
            ExtendedTextStyle::new().color(Some(Color::rgb(255, 0, 0))),
        );
        assert!(!p.is_empty());
    }

    #[test]
    fn presentation_is_empty_with_block_layout() {
        let mut p = Presentation::default();
        let id = NodeId::new(1);
        p.block_layout
            .set(id, BlockLayout::new().alignment(Some(Alignment::Center)));
        assert!(!p.is_empty());
    }

    #[test]
    fn presentation_is_empty_with_raw_spans() {
        let mut p = Presentation::default();
        p.raw_spans.push(PositionedSpan::new(
            "test".into(),
            BoundingBox::new(0.0, 0.0, 10.0, 10.0),
            0,
        ));
        assert!(!p.is_empty());
    }

    #[test]
    fn no_formatting_produces_empty_styles() {
        // When a backend produces no styling data, ExtendedTextStyle and
        // BlockLayout report is_empty() = true, so they should not be stored.
        let style = ExtendedTextStyle::new();
        assert!(style.is_empty());
        let layout = BlockLayout::new();
        assert!(layout.is_empty());
    }

    #[test]
    fn partial_formatting_is_not_empty() {
        let style = ExtendedTextStyle::new().font_size(Some(12.0));
        assert!(!style.is_empty());
        let layout = BlockLayout::new().space_before(Some(6.0));
        assert!(!layout.is_empty());
    }
}

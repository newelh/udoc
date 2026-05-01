//! Path intermediate representation (IR) for PDF page rendering.
//!
//! This module defines the types emitted by the content interpreter when
//! the renderer asks for path data (fills, strokes, clip paths, and
//! annotation appearance drawing).
//!
//! It is distinct from `crate::table::PathSegment`, which is the
//! simplified, already-CTM-flattened, line/rect-only representation used
//! by the table detector. The renderer needs canonical cubic Beziers,
//! explicit fill rules, stroke styles captured at paint time, and the
//! CTM snapshot to draw into device space itself.
//!
//! Designed per  .

/// A 2D point in user space (PDF page coordinates, pre-CTM).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    /// X coordinate.
    pub x: f64,
    /// Y coordinate.
    pub y: f64,
}

impl Point {
    /// Create a new point.
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// A 3x3 affine transform in the PDF row-vector convention.
///
/// Represents the same 6-element matrix as the interpreter's internal
/// `Matrix` type, but lives on the public IR surface so renderer-side
/// crates don't need to import interpreter-private types.
///
/// ```text
/// | a  b  0 |
/// | c  d  0 |
/// | e  f  1 |
/// ```
///
/// Point transform: `[x', y', 1] = [x, y, 1] * M`, i.e.
/// `x' = x*a + y*c + e`, `y' = x*b + y*d + f`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix3 {
    /// Row-vector element (1,1).
    pub a: f64,
    /// Row-vector element (1,2).
    pub b: f64,
    /// Row-vector element (2,1).
    pub c: f64,
    /// Row-vector element (2,2).
    pub d: f64,
    /// Row-vector element (3,1) (translation x).
    pub e: f64,
    /// Row-vector element (3,2) (translation y).
    pub f: f64,
}

impl Matrix3 {
    /// Identity transform.
    pub fn identity() -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }

    /// Transform a point under this matrix.
    pub fn transform_point(&self, x: f64, y: f64) -> (f64, f64) {
        (
            x * self.a + y * self.c + self.e,
            x * self.b + y * self.d + self.f,
        )
    }
}

impl Default for Matrix3 {
    fn default() -> Self {
        Self::identity()
    }
}

/// Path fill rule as defined by ISO 32000-2 §8.5.3.
///
/// Used when a path is painted with an `f`/`f*`/`B`/`B*`/`b`/`b*`
/// operator. Starred operators select `EvenOdd`; unstarred select
/// `NonZero`. No implicit default; every filled `PagePath` carries an
/// explicit rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRule {
    /// Non-zero winding number rule (PDF default for unstarred paint ops).
    NonZero,
    /// Even-odd rule (PDF default for starred paint ops: `f*`, `B*`, `b*`).
    EvenOdd,
}

/// Line cap style (ISO 32000-2 §8.4.3.3, operator `J`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineCap {
    /// Butt cap (0). The stroke is squared off at the endpoint.
    Butt,
    /// Round cap (1). A semicircular arc with diameter equal to the line
    /// width is drawn around the endpoint.
    Round,
    /// Projecting square cap (2). The stroke continues beyond the
    /// endpoint for half the line width and is squared off.
    ProjectingSquare,
}

/// Line join style (ISO 32000-2 §8.4.3.4, operator `j`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineJoin {
    /// Miter join (0). Outer edges are extended to meet at a point,
    /// subject to the miter limit.
    Miter,
    /// Round join (1). A circular arc is drawn around the join point.
    Round,
    /// Bevel join (2). The outer corner is squared off.
    Bevel,
}

/// Paint color (opaque RGB plus alpha for now).
///
/// The PDF interpreter currently materializes all colors into sRGB. Device
/// colorspaces (CMYK, Gray, Lab) are pre-converted at the operator level.
/// A richer colorspace-aware variant can be added later without breaking
/// consumers that only read the RGB path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// sRGB color with 8-bit channels plus alpha in [0, 255].
    Rgb {
        /// Red channel (0..=255).
        r: u8,
        /// Green channel (0..=255).
        g: u8,
        /// Blue channel (0..=255).
        b: u8,
        /// Alpha channel (0..=255). 255 = fully opaque.
        a: u8,
    },
}

impl Color {
    /// Opaque black.
    pub const BLACK: Color = Color::Rgb {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
}

/// Stroke style snapshot captured at path paint time.
///
/// Populated from the current `GraphicsState` at the moment the paint
/// operator executes (not at path construction time). This matches the
/// PDF spec: stroke parameters that change between `m`/`l`/`c`/... and
/// the painting operator still affect the drawn stroke.
///
/// `line_width` is in user space (pre-CTM). The renderer scales it by
/// the CTM snapshot carried on the enclosing `PagePath`.
#[derive(Debug, Clone, PartialEq)]
pub struct StrokeStyle {
    /// Stroke width in user space (pre-CTM). PDF `w` operator.
    pub line_width: f32,
    /// Line cap style. PDF `J` operator.
    pub line_cap: LineCap,
    /// Line join style. PDF `j` operator.
    pub line_join: LineJoin,
    /// Miter limit. PDF `M` operator. Applies only when `line_join = Miter`.
    pub miter_limit: f32,
    /// Dash pattern array in user space. Empty = solid line. PDF `d` operator.
    pub dash_pattern: Vec<f32>,
    /// Dash phase (offset into the dash pattern). PDF `d` operator.
    pub dash_phase: f32,
    /// Stroke color (CS/SCN/RG/G/K, mapped to sRGB by the interpreter).
    pub color: Color,
}

impl Default for StrokeStyle {
    /// Defaults per ISO 32000-2 §8.4.1 and the PDF `q`/`Q` initial graphics
    /// state.
    fn default() -> Self {
        Self {
            line_width: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 10.0,
            dash_pattern: Vec::new(),
            dash_phase: 0.0,
            color: Color::BLACK,
        }
    }
}

/// A single path-construction segment in canonical IR form.
///
/// The content interpreter expands the full PDF path-construction vocabulary
/// (`m`, `l`, `c`, `v`, `y`, `re`, `h`) into this canonical four-variant form
/// at parse time, so the rasterizer only sees move/line/cubic-curve/close.
///
/// - `v` (final control point = end point) expands to `CurveTo { c1 = current
///   point, c2 = end, end }`.
/// - `y` (initial control point = start point) expands to
///   `CurveTo { c1 = x1y1_operand, c2 = end_operand, end = end_operand }`.
/// - `re x y w h` expands to `MoveTo { (x, y) } + LineTo + LineTo + LineTo +
///   ClosePath` (four line-tos plus an implicit close).
///
/// Segments live in user space. The enclosing `PagePath` carries the CTM
/// snapshot needed to map to device space.
#[derive(Debug, Clone, PartialEq)]
pub enum PathSegmentKind {
    /// Begin a new subpath at `p`. PDF `m` operator.
    MoveTo {
        /// Target point.
        p: Point,
    },
    /// Draw a straight line from the current point to `p`. PDF `l` operator
    /// (also expansion target of `re`).
    LineTo {
        /// Target point.
        p: Point,
    },
    /// Draw a cubic Bezier from the current point through `c1` and `c2` to
    /// `end`. PDF `c` operator; `v`/`y` normalize into this variant.
    CurveTo {
        /// First control point.
        c1: Point,
        /// Second control point.
        c2: Point,
        /// End point.
        end: Point,
    },
    /// Close the current subpath with a straight line to the last `MoveTo`.
    /// PDF `h` operator.
    ClosePath,
}

/// A complete path as emitted by the interpreter at a single paint
/// operator (`B`, `b`, `B*`, `b*`, `f`, `f*`, `S`, `s`, `n`).
///
/// The full drawing state required to rasterize the path is captured
/// here:
///
/// - `segments`: canonical moveto/lineto/curveto/closepath sequence in
///   user space.
/// - `fill`: `Some(rule)` if the paint op fills; `None` for stroke-only
///   (`S`, `s`) and clip-only (`n`).
/// - `stroke`: `Some(style)` if the paint op strokes; `None` for
///   fill-only (`f`, `f*`) and clip-only (`n`).
/// - `ctm_at_paint`: the CTM at the exact moment the paint operator
///   executed. Path segments use user-space coordinates; the rasterizer
///   multiplies by this matrix to map into device space. Capturing at
///   paint time (not construction time) matches mupdf/pdfium and is the
///   only correct behaviour when `cm` appears between path construction
///   and the paint op (e.g. `q 0 0 m 10 10 l 1 0 0 1 50 50 cm f Q`).
/// - `z`: monotonically increasing paint order index within the page.
///   Used to stabilize back-to-front composition.
#[derive(Debug, Clone, PartialEq)]
pub struct PagePath {
    /// Canonical path segments in user space.
    pub segments: Vec<PathSegmentKind>,
    /// Fill rule if filled, `None` for stroke-only or clip-only paths.
    pub fill: Option<FillRule>,
    /// Fill color snapshot. Only meaningful when `fill` is `Some`. Carries
    /// the sRGB color and alpha captured from the graphics state at paint
    /// time.
    pub fill_color: Option<Color>,
    /// Stroke style if stroked, `None` for fill-only or clip-only paths.
    pub stroke: Option<StrokeStyle>,
    /// CTM snapshot at the paint operator.
    pub ctm_at_paint: Matrix3,
    /// Paint-order index within the page (0-based).
    pub z: usize,
}

/// A sampled color LUT for a shading /Function (256 entries, sRGB).
///
/// PDF shading functions can be Type 0 (sampled), Type 2 (exponential
/// interpolation), Type 3 (stitching), or Type 4 (PostScript). Rather
/// than re-evaluate them per pixel, we sample once at dict-parse time
/// into a fixed 256-entry LUT keyed by the normalized parameter `t`.
/// Axial / radial rasterization indexes this table directly -- no
/// per-pixel function evaluation, no allocations during scan.
///
/// Each entry is an opaque sRGB triple. Alpha is carried on the
/// enclosing [`PageShading`] via the `/ca` opacity captured from the
/// graphics state at paint time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadingLut {
    /// 256 sRGB entries over `t = [0, 1]`.
    pub samples: Vec<[u8; 3]>,
}

impl ShadingLut {
    /// Sample color at parameter `t` (nearest-neighbour lookup).
    ///
    /// `t` is clamped to `[0, 1]`. The LUT is expected to have 256
    /// entries; shorter tables still work via saturation at the last
    /// entry but the parser is the one that guarantees density.
    pub fn sample(&self, t: f64) -> [u8; 3] {
        if self.samples.is_empty() {
            return [0, 0, 0];
        }
        let t = t.clamp(0.0, 1.0);
        let n = self.samples.len();
        let idx = (t * (n - 1) as f64).round() as usize;
        self.samples[idx.min(n - 1)]
    }
}

/// A PDF shading pattern captured at an 'sh' operator (ISO 32000-2
/// §8.7.4,).
///
/// Only types 2 (axial) and 3 (radial) are fully implemented. Types 1
/// and 4-7 are diagnosed via `WarningKind::UnsupportedShadingType`
/// and fall through to the base fill color; the interpreter still
/// emits a `PageShadingKind::Unsupported` record so the renderer can
/// decide what to do (today: skip).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum PageShadingKind {
    /// Axial (linear) gradient between two user-space points.
    Axial {
        /// Start point of the gradient axis.
        p0: Point,
        /// End point of the gradient axis.
        p1: Point,
        /// Pre-sampled color LUT over `t = [0, 1]`.
        lut: ShadingLut,
        /// Whether to extend the start color past `t < 0`.
        extend_start: bool,
        /// Whether to extend the end color past `t > 1`.
        extend_end: bool,
    },
    /// Radial (circle-to-circle) gradient.
    Radial {
        /// Start circle centre.
        c0: Point,
        /// Start circle radius.
        r0: f64,
        /// End circle centre.
        c1: Point,
        /// End circle radius.
        r1: f64,
        /// Pre-sampled color LUT over `t = [0, 1]`.
        lut: ShadingLut,
        /// Whether to extend the start circle's color past `t < 0`.
        extend_start: bool,
        /// Whether to extend the end circle's color past `t > 1`.
        extend_end: bool,
    },
    /// Any shading type we don't handle (types 1, 4, 5, 6, 7). The
    /// renderer skips these; the warning is the user-facing signal.
    Unsupported {
        /// The PDF /ShadingType value that wasn't handled.
        shading_type: u32,
    },
}

/// A shading-pattern record emitted by the 'sh' operator.
///
/// `ctm_at_paint` maps the shading's user-space coordinates (points,
/// radii) into device space for the renderer. `z` is the paint-order
/// index within the page, shared with [`PagePath`].
#[derive(Debug, Clone, PartialEq)]
pub struct PageShading {
    /// The shading geometry + sampled color table.
    pub kind: PageShadingKind,
    /// CTM snapshot at the 'sh' op.
    pub ctm_at_paint: Matrix3,
    /// Fill-side opacity (0..=255) from the graphics state at paint time.
    /// Shading ops use non-stroking color state per ISO 32000-2 §8.7.4.
    pub alpha: u8,
    /// Paint-order index within the page (0-based), shared with paths.
    pub z: usize,
}

/// A Type 1 coloured tiling pattern record emitted by the interpreter
/// when a path paint op fires while the current non-stroking colorspace
/// is `/Pattern` (ISO 32000-2 §8.7.3,).
///
/// Carries the tile geometry from the pattern resource dict plus the
/// paint-time context (CTM, alpha, fill-region subpaths, z-order). The
/// fill region is captured as the user-space subpaths of the path that
/// was being painted; the renderer tiles the pattern cell inside this
/// region.
#[derive(Debug, Clone, PartialEq)]
pub struct PageTilingPattern {
    /// Resource name under which the pattern was registered
    /// (`/Resources /Pattern /P1` -> `"P1"`).
    pub resource_name: String,
    /// Tile cell bounding box in pattern coordinates, `[llx, lly, urx, ury]`.
    pub bbox: [f64; 4],
    /// Horizontal tile spacing (`/XStep`).
    pub xstep: f64,
    /// Vertical tile spacing (`/YStep`).
    pub ystep: f64,
    /// Pattern-to-user-space transform `[a, b, c, d, e, f]`.
    pub matrix: [f64; 6],
    /// Filter-decoded tile content stream bytes. The renderer may
    /// re-interpret this for fancy tiles; v1 the renderer
    /// samples the tile's average color via [`tile_fallback_color`].
    pub content_stream: Vec<u8>,
    /// User-space subpaths of the fill region (closed polygons).
    /// Captured from the path under construction at the paint op.
    pub fill_subpaths: Vec<Vec<Point>>,
    /// Fill rule applied to `fill_subpaths`.
    pub fill_rule: FillRule,
    /// CTM snapshot at the moment the paint op fired.
    pub ctm_at_paint: Matrix3,
    /// Fill opacity (0..=255) from `/ca` in the ExtGState at paint time.
    pub alpha: u8,
    /// Fallback solid fill color in case the renderer cannot rasterize
    /// the tile. Sampled from the tile's content stream.
    pub fallback_color: Color,
    /// Paint-order index within the page (0-based), shared with
    /// [`PagePath`] and [`PageShading`].
    pub z: usize,
}

/// Sample a fallback solid color from a tile content stream.
///
/// Walks the content stream for the last non-stroking color op
/// (`rg`, `g`, `k`, `scn`, `sc`) and returns the resulting sRGB triple.
/// Defaults to mid-gray when no color op is found: that's better than
/// black for anti-aliased blends and usually invisible on white pages.
pub fn tile_fallback_color(stream: &[u8]) -> Color {
    // Very-tolerant tokenizer: splits on whitespace, tracks the last
    // run of up-to-4 numeric tokens, and looks for color op words.
    let mut nums: Vec<f64> = Vec::new();
    let mut last: Color = Color::Rgb {
        r: 128,
        g: 128,
        b: 128,
        a: 255,
    };
    for tok in split_tokens(stream) {
        if let Some(n) = parse_number(tok) {
            nums.push(n);
            if nums.len() > 4 {
                nums.remove(0);
            }
            continue;
        }
        match tok {
            b"rg" if nums.len() >= 3 => {
                let r = clamp_u8(nums[nums.len() - 3]);
                let g = clamp_u8(nums[nums.len() - 2]);
                let b = clamp_u8(nums[nums.len() - 1]);
                last = Color::Rgb { r, g, b, a: 255 };
                nums.clear();
            }
            b"g" if !nums.is_empty() => {
                let v = clamp_u8(*nums.last().unwrap());
                last = Color::Rgb {
                    r: v,
                    g: v,
                    b: v,
                    a: 255,
                };
                nums.clear();
            }
            b"k" if nums.len() >= 4 => {
                // Fast CMYK -> sRGB (same math as interpreter).
                let c = nums[nums.len() - 4].clamp(0.0, 1.0);
                let m = nums[nums.len() - 3].clamp(0.0, 1.0);
                let y = nums[nums.len() - 2].clamp(0.0, 1.0);
                let kk = nums[nums.len() - 1].clamp(0.0, 1.0);
                let r = ((1.0 - c) * (1.0 - kk) * 255.0).round() as u8;
                let g = ((1.0 - m) * (1.0 - kk) * 255.0).round() as u8;
                let b = ((1.0 - y) * (1.0 - kk) * 255.0).round() as u8;
                last = Color::Rgb { r, g, b, a: 255 };
                nums.clear();
            }
            b"scn" | b"sc" => {
                // Take whichever component count matches: 1, 3, or 4.
                match nums.len() {
                    1 => {
                        let v = clamp_u8(nums[0]);
                        last = Color::Rgb {
                            r: v,
                            g: v,
                            b: v,
                            a: 255,
                        };
                    }
                    3 => {
                        last = Color::Rgb {
                            r: clamp_u8(nums[0]),
                            g: clamp_u8(nums[1]),
                            b: clamp_u8(nums[2]),
                            a: 255,
                        };
                    }
                    4 => {
                        let c = nums[0].clamp(0.0, 1.0);
                        let m = nums[1].clamp(0.0, 1.0);
                        let y = nums[2].clamp(0.0, 1.0);
                        let kk = nums[3].clamp(0.0, 1.0);
                        let r = ((1.0 - c) * (1.0 - kk) * 255.0).round() as u8;
                        let g = ((1.0 - m) * (1.0 - kk) * 255.0).round() as u8;
                        let b = ((1.0 - y) * (1.0 - kk) * 255.0).round() as u8;
                        last = Color::Rgb { r, g, b, a: 255 };
                    }
                    _ => {}
                }
                nums.clear();
            }
            _ => {
                // Non-numeric non-color op: reset number accumulator
                // only on "ops we know consume stack". Conservative:
                // keep numbers until we see something match.
            }
        }
    }
    last
}

fn split_tokens(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    data.split(|b| matches!(*b, b' ' | b'\t' | b'\r' | b'\n' | b'\x0c'))
        .filter(|s| !s.is_empty())
}

fn parse_number(tok: &[u8]) -> Option<f64> {
    let s = std::str::from_utf8(tok).ok()?;
    s.parse::<f64>().ok()
}

fn clamp_u8(v: f64) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shading_lut_sample_clamps_and_indexes() {
        let samples: Vec<[u8; 3]> = (0..256).map(|i| [i as u8, 0, 0]).collect();
        let lut = ShadingLut { samples };
        assert_eq!(lut.sample(0.0), [0, 0, 0]);
        assert_eq!(lut.sample(1.0), [255, 0, 0]);
        assert_eq!(lut.sample(0.5), [128, 0, 0]);
        assert_eq!(lut.sample(-0.1), [0, 0, 0]);
        assert_eq!(lut.sample(2.0), [255, 0, 0]);
    }

    #[test]
    fn matrix3_identity_transforms_to_self() {
        let m = Matrix3::identity();
        let (x, y) = m.transform_point(12.5, -7.0);
        assert!((x - 12.5).abs() < 1e-12);
        assert!((y + 7.0).abs() < 1e-12);
    }

    #[test]
    fn matrix3_translation_transforms_points() {
        let m = Matrix3 {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 100.0,
            f: 50.0,
        };
        let (x, y) = m.transform_point(0.0, 0.0);
        assert!((x - 100.0).abs() < 1e-12);
        assert!((y - 50.0).abs() < 1e-12);
    }

    #[test]
    fn tile_fallback_color_rg() {
        let c = tile_fallback_color(b"q 1 0 0 rg 0 0 10 10 re f Q");
        assert_eq!(
            c,
            Color::Rgb {
                r: 255,
                g: 0,
                b: 0,
                a: 255
            }
        );
    }

    #[test]
    fn tile_fallback_color_grayscale() {
        let c = tile_fallback_color(b"0.5 g 0 0 1 1 re f");
        match c {
            Color::Rgb { r, g, b, a } => {
                assert_eq!(r, 128);
                assert_eq!(g, 128);
                assert_eq!(b, 128);
                assert_eq!(a, 255);
            }
        }
    }

    #[test]
    fn tile_fallback_color_defaults_to_grey() {
        let c = tile_fallback_color(b" /x1 Do\n");
        match c {
            Color::Rgb { r, g, b, .. } => {
                assert_eq!(r, 128);
                assert_eq!(g, 128);
                assert_eq!(b, 128);
            }
        }
    }

    #[test]
    fn tile_fallback_color_scn_three() {
        let c = tile_fallback_color(b"0.2 0.4 0.6 scn 0 0 1 1 re f");
        match c {
            Color::Rgb { r, g, b, a } => {
                assert_eq!(r, 51);
                assert_eq!(g, 102);
                assert_eq!(b, 153);
                assert_eq!(a, 255);
            }
        }
    }

    #[test]
    fn stroke_style_defaults_per_pdf_spec() {
        let s = StrokeStyle::default();
        assert!((s.line_width - 1.0).abs() < 1e-6);
        assert_eq!(s.line_cap, LineCap::Butt);
        assert_eq!(s.line_join, LineJoin::Miter);
        assert!((s.miter_limit - 10.0).abs() < 1e-6);
        assert!(s.dash_pattern.is_empty());
        assert!((s.dash_phase - 0.0).abs() < 1e-6);
        assert_eq!(s.color, Color::BLACK);
    }
}

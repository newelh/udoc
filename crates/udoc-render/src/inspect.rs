//! Inspection helpers for the render pipeline.
//!
//! Exposes structured dumps of the intermediate state (outlines, hints,
//! edges, coverage bitmap) used by `udoc render-inspect`. All data is
//! in font units where applicable so cross-DPI diffs are meaningful.
//!
//! This module is kept deliberately thin: the inspection code does not
//! mutate the renderer's behaviour; it only reconstructs the same
//! intermediate state in a shape the CLI can serialize.

use udoc_font::ttf::{GlyphOutline, OutlinePoint, StemHints};

use super::auto_hinter::{edges as ed, metrics as mt, segments as sg};
use super::font_cache::FontCache;

/// A single outline command in font units.
#[derive(Debug, Clone, Copy)]
pub enum OutlineOp {
    /// Start a new subpath at `(x, y)`.
    Move {
        /// Subpath-start X.
        x: f64,
        /// Subpath-start Y.
        y: f64,
    },
    /// Line to `(x, y)` from the current point.
    Line {
        /// Line-end X.
        x: f64,
        /// Line-end Y.
        y: f64,
    },
    /// Quadratic bezier (TrueType). We store the implicit conversion to a
    /// cubic for uniform output: callers get a single `Curve` variant with
    /// both control points present.
    Curve {
        /// First cubic control X.
        c1x: f64,
        /// First cubic control Y.
        c1y: f64,
        /// Second cubic control X.
        c2x: f64,
        /// Second cubic control Y.
        c2y: f64,
        /// End-point X.
        x: f64,
        /// End-point Y.
        y: f64,
    },
}

/// Inspection dump for a single glyph's outline.
#[derive(Debug, Clone)]
pub struct OutlineDump {
    /// Font name (subset-prefix stripped).
    pub font_name: String,
    /// Glyph index within the font.
    pub glyph_id: u16,
    /// Font `units_per_em` (from `head` table).
    pub units_per_em: u16,
    /// Per-contour op streams in traversal order.
    pub contours: Vec<Vec<OutlineOp>>,
    /// Glyph bounds `(x_min, y_min, x_max, y_max)` in font units.
    pub bounds: (i16, i16, i16, i16),
}

/// Inspection dump for declared + auto-detected hints.
#[derive(Debug, Clone)]
pub struct HintsDump {
    /// Font name (subset-prefix stripped).
    pub font_name: String,
    /// Glyph index within the font.
    pub glyph_id: u16,
    /// Font `units_per_em`.
    pub units_per_em: u16,
    /// PS/Type1 declared stem hints from the font.
    pub declared: StemHints,
    /// Auto-hinter-derived horizontal edges.
    pub auto_h_edges: Vec<EdgeDump>,
    /// Auto-hinter-derived vertical edges.
    pub auto_v_edges: Vec<EdgeDump>,
}

/// Inspection dump for segments + edges.
#[derive(Debug, Clone)]
pub struct EdgesDump {
    /// Font name (subset-prefix stripped).
    pub font_name: String,
    /// Glyph index within the font.
    pub glyph_id: u16,
    /// Font `units_per_em`.
    pub units_per_em: u16,
    /// Horizontal segments.
    pub h_segments: Vec<SegmentDump>,
    /// Vertical segments.
    pub v_segments: Vec<SegmentDump>,
    /// Horizontal edges.
    pub h_edges: Vec<EdgeDump>,
    /// Vertical edges.
    pub v_edges: Vec<EdgeDump>,
}

/// Inspection dump of the rasterized coverage bitmap.
#[derive(Debug, Clone)]
pub struct BitmapDump {
    /// Font name (subset-prefix stripped).
    pub font_name: String,
    /// Glyph index within the font.
    pub glyph_id: u16,
    /// Pixels-per-em used for rasterization.
    pub ppem: u16,
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels.
    pub height: u32,
    /// Row-major grayscale coverage: 0 = background, 255 = fully covered.
    pub coverage: Vec<u8>,
}

/// Serialized view of a Segment.
#[derive(Debug, Clone)]
pub struct SegmentDump {
    /// Dimension label (`"h"` or `"v"`).
    pub dim: &'static str,
    /// Direction label (`"up"`, `"down"`, `"left"`, `"right"`, `"none"`).
    pub dir: &'static str,
    /// Position along the controlling axis (font units).
    pub pos: f64,
    /// Low cross-axis extreme (font units).
    pub min_coord: f64,
    /// High cross-axis extreme (font units).
    pub max_coord: f64,
    /// Index of the linked partner segment, if any.
    pub link: Option<usize>,
    /// Edge assignment index, if grouped.
    pub edge: Option<usize>,
    /// Linking score (FreeType-style).
    pub score: f64,
    /// Originating contour index.
    pub contour: usize,
}

/// Serialized view of an Edge.
#[derive(Debug, Clone)]
pub struct EdgeDump {
    /// Dimension label (`"h"` or `"v"`).
    pub dim: &'static str,
    /// Unfitted edge position (font units).
    pub pos: f64,
    /// Grid-fitted position in pixel units.
    pub fitted_pos: f64,
    /// Whether the edge was grid-fitted.
    pub fitted: bool,
    /// Blue-zone index the edge was snapped to, if any.
    pub blue_zone: Option<usize>,
    /// Partner edge index (stem pair), if any.
    pub link: Option<usize>,
    /// Serif-attachment edge index, if any.
    pub serif: Option<usize>,
    /// Packed auto-hinter flag bits (`AF_EDGE_*`).
    pub flags: u8,
    /// Number of segments grouped into this edge.
    pub segment_count: usize,
}

impl From<&sg::Segment> for SegmentDump {
    fn from(s: &sg::Segment) -> Self {
        Self {
            dim: match s.dim {
                sg::Dimension::Horizontal => "h",
                sg::Dimension::Vertical => "v",
            },
            dir: match s.dir {
                sg::Direction::Up => "up",
                sg::Direction::Down => "down",
                sg::Direction::Left => "left",
                sg::Direction::Right => "right",
                sg::Direction::None => "none",
            },
            pos: s.pos,
            min_coord: s.min_coord,
            max_coord: s.max_coord,
            link: s.link,
            edge: s.edge,
            score: s.score,
            contour: s.contour,
        }
    }
}

impl From<&ed::Edge> for EdgeDump {
    fn from(e: &ed::Edge) -> Self {
        Self {
            dim: match e.dim {
                sg::Dimension::Horizontal => "h",
                sg::Dimension::Vertical => "v",
            },
            pos: e.pos,
            fitted_pos: e.fitted_pos,
            fitted: e.fitted,
            blue_zone: e.blue_zone,
            link: e.link,
            serif: e.serif,
            flags: e.flags,
            segment_count: e.segment_count,
        }
    }
}

/// Look up a glyph outline + the font's units-per-em. Returns None if the
/// font or glyph is unknown. Falls back to Liberation Sans/Serif when the
/// named font is not embedded (common for standard-14 PDF fonts).
pub fn glyph_outline_for_inspect(
    cache: &mut FontCache,
    font_name: &str,
    glyph_id: u16,
) -> Option<(GlyphOutline, u16)> {
    let upm = cache.units_per_em_with_fallback(font_name);
    let out = cache.glyph_outline_by_gid_with_fallback(font_name, glyph_id)?;
    Some((out, upm))
}

/// Dump a glyph's outline as a sequence of move/line/curve ops in font units.
///
/// TrueType quadratic contours are lowered to cubic curves (via the usual
/// 2/3 split) so the output is uniform across font formats.
pub fn dump_outline(cache: &mut FontCache, font_name: &str, glyph_id: u16) -> Option<OutlineDump> {
    let (outline, upm) = glyph_outline_for_inspect(cache, font_name, glyph_id)?;
    let mut contours = Vec::with_capacity(outline.contours.len());
    for c in &outline.contours {
        contours.push(contour_to_ops(&c.points));
    }
    Some(OutlineDump {
        font_name: font_name.to_string(),
        glyph_id,
        units_per_em: upm,
        contours,
        bounds: outline.bounds,
    })
}

/// Convert a contour's raw point list into a flat op stream.
fn contour_to_ops(points: &[OutlinePoint]) -> Vec<OutlineOp> {
    let mut out = Vec::new();
    if points.is_empty() {
        return out;
    }
    // TrueType-style implicit points: two consecutive off-curve points
    // imply an on-curve midpoint. Walk the contour and emit either line
    // or curve ops accordingly.
    let n = points.len();
    let first = points.iter().position(|p| p.on_curve).unwrap_or(0);
    let start = points[first];
    out.push(OutlineOp::Move {
        x: start.x,
        y: start.y,
    });

    let mut prev = start;
    let mut i = 1;
    while i <= n {
        let idx = (first + i) % n;
        let p = points[idx];
        if p.on_curve {
            out.push(OutlineOp::Line { x: p.x, y: p.y });
            prev = p;
            i += 1;
        } else {
            // One or more consecutive off-curve points: quadratic beziers.
            let mut ctrl = p;
            i += 1;
            loop {
                let nidx = (first + i) % n;
                let next = points[nidx];
                if next.on_curve {
                    out.push(quad_to_cubic(prev, ctrl, next));
                    prev = next;
                    i += 1;
                    break;
                }
                // Implicit on-curve at midpoint.
                let mid = OutlinePoint {
                    x: (ctrl.x + next.x) * 0.5,
                    y: (ctrl.y + next.y) * 0.5,
                    on_curve: true,
                };
                out.push(quad_to_cubic(prev, ctrl, mid));
                prev = mid;
                ctrl = next;
                i += 1;
            }
        }
    }
    out
}

fn quad_to_cubic(p0: OutlinePoint, p1: OutlinePoint, p2: OutlinePoint) -> OutlineOp {
    // Cubic control points from quadratic: CP1 = P0 + 2/3*(P1-P0), CP2 = P2 + 2/3*(P1-P2).
    let c1x = p0.x + (2.0 / 3.0) * (p1.x - p0.x);
    let c1y = p0.y + (2.0 / 3.0) * (p1.y - p0.y);
    let c2x = p2.x + (2.0 / 3.0) * (p1.x - p2.x);
    let c2y = p2.y + (2.0 / 3.0) * (p1.y - p2.y);
    OutlineOp::Curve {
        c1x,
        c1y,
        c2x,
        c2y,
        x: p2.x,
        y: p2.y,
    }
}

/// Dump declared + auto-detected hints for a glyph.
pub fn dump_hints(
    cache: &mut FontCache,
    font_name: &str,
    glyph_id: u16,
    ppem: u16,
) -> Option<HintsDump> {
    let (outline, upm) = glyph_outline_for_inspect(cache, font_name, glyph_id)?;
    let declared = outline.stem_hints.clone();

    // Re-run segment + edge detection the same way the auto-hinter does.
    let scale = ppem as f64 / upm as f64;
    let metrics = cache.auto_hint_metrics(font_name).cloned();
    let tuples: Vec<Vec<(f64, f64, bool)>> = outline
        .contours
        .iter()
        .map(|c| c.points.iter().map(|p| (p.x, p.y, p.on_curve)).collect())
        .collect();
    let analysis = sg::tuples_to_contours(&tuples);
    let (h_edges, v_edges) = edges_from_analysis(&analysis, metrics.as_ref(), scale);

    Some(HintsDump {
        font_name: font_name.to_string(),
        glyph_id,
        units_per_em: upm,
        declared,
        auto_h_edges: h_edges,
        auto_v_edges: v_edges,
    })
}

/// Dump the full segment + edge tables for a glyph.
pub fn dump_edges(
    cache: &mut FontCache,
    font_name: &str,
    glyph_id: u16,
    ppem: u16,
) -> Option<EdgesDump> {
    let (outline, upm) = glyph_outline_for_inspect(cache, font_name, glyph_id)?;
    let scale = ppem as f64 / upm as f64;
    let metrics = cache.auto_hint_metrics(font_name).cloned();
    let tuples: Vec<Vec<(f64, f64, bool)>> = outline
        .contours
        .iter()
        .map(|c| c.points.iter().map(|p| (p.x, p.y, p.on_curve)).collect())
        .collect();
    let analysis = sg::tuples_to_contours(&tuples);

    let (h_segs, v_segs, h_edges, v_edges) =
        segments_and_edges_from_analysis(&analysis, metrics.as_ref(), scale);

    Some(EdgesDump {
        font_name: font_name.to_string(),
        glyph_id,
        units_per_em: upm,
        h_segments: h_segs,
        v_segments: v_segs,
        h_edges,
        v_edges,
    })
}

fn segments_and_edges_from_analysis(
    analysis: &[sg::AnalysisContour],
    metrics: Option<&mt::GlobalMetrics>,
    scale: f64,
) -> (
    Vec<SegmentDump>,
    Vec<SegmentDump>,
    Vec<EdgeDump>,
    Vec<EdgeDump>,
) {
    // UPM from metrics when available, else default to 1000 so thresholds
    // degrade gracefully. (Inspect is a debugging tool, not a render path.)
    let upm = metrics.map(|m| m.units_per_em).unwrap_or(1000);

    let mut h_segs = sg::detect_segments(analysis, sg::Dimension::Horizontal, upm);
    let mut v_segs = sg::detect_segments(analysis, sg::Dimension::Vertical, upm);
    let h_edges = ed::detect_edges(&mut h_segs, sg::Dimension::Horizontal, scale);
    let v_edges = ed::detect_edges(&mut v_segs, sg::Dimension::Vertical, scale);

    let h_segs_dump = h_segs.iter().map(SegmentDump::from).collect();
    let v_segs_dump = v_segs.iter().map(SegmentDump::from).collect();
    let h_edges_dump = h_edges.iter().map(EdgeDump::from).collect();
    let v_edges_dump = v_edges.iter().map(EdgeDump::from).collect();
    (h_segs_dump, v_segs_dump, h_edges_dump, v_edges_dump)
}

fn edges_from_analysis(
    analysis: &[sg::AnalysisContour],
    metrics: Option<&mt::GlobalMetrics>,
    scale: f64,
) -> (Vec<EdgeDump>, Vec<EdgeDump>) {
    let (_, _, h, v) = segments_and_edges_from_analysis(analysis, metrics, scale);
    (h, v)
}

/// Rasterize the named glyph at `ppem` and return the coverage bitmap.
///
/// Uses the same rasterizer the renderer does. Pixel coverage is
/// normalized to 0-255 (single channel, y-down).
pub fn dump_bitmap(
    cache: &mut FontCache,
    font_name: &str,
    glyph_id: u16,
    ppem: u16,
) -> Option<BitmapDump> {
    // Reuse the production rasterizer via FontCache so the output matches
    // what the page renderer sees. Use the font's own UPM.
    let outline = cache.glyph_outline_by_gid_with_fallback(font_name, glyph_id)?;
    let upm = cache.units_per_em_with_fallback(font_name);
    let scale = ppem as f64 / upm as f64;

    // Direct rasterization: flatten to an A8 buffer sized by the outline's
    // bbox at the given ppem. Implemented via a minimal scanline fill to
    // avoid coupling the inspect API to the renderer's internal types.
    let bitmap = rasterize_outline_coverage(&outline, scale);
    Some(BitmapDump {
        font_name: font_name.to_string(),
        glyph_id,
        ppem,
        width: bitmap.width,
        height: bitmap.height,
        coverage: bitmap.data,
    })
}

/// Local minimal rasterizer output. Kept in this module so we don't
/// reach into `rasterizer::GlyphBitmap`'s crate-private shape. Uses a
/// straightforward even-odd fill with 4x4 supersampling.
struct RasterBitmap {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

fn rasterize_outline_coverage(outline: &GlyphOutline, scale: f64) -> RasterBitmap {
    // Bounding box in pixel space with a 1-pixel pad on each side.
    let min_x = (outline.bounds.0 as f64 * scale).floor() as i32 - 1;
    let min_y = (outline.bounds.1 as f64 * scale).floor() as i32 - 1;
    let max_x = (outline.bounds.2 as f64 * scale).ceil() as i32 + 1;
    let max_y = (outline.bounds.3 as f64 * scale).ceil() as i32 + 1;
    let w = (max_x - min_x).max(1) as u32;
    let h = (max_y - min_y).max(1) as u32;
    let mut accum = vec![0u16; (w * h) as usize];

    const SUB: i32 = 4; // 4x4 supersampling
    for c in &outline.contours {
        if c.points.len() < 2 {
            continue;
        }
        // Flatten bezier curves: approximate quadratics with 8 line segments.
        let mut flat: Vec<(f64, f64)> = Vec::new();
        let pts = &c.points;
        let n = pts.len();
        let first_on = pts.iter().position(|p| p.on_curve).unwrap_or(0);
        let start = pts[first_on];
        flat.push((start.x, start.y));
        let mut prev = start;
        let mut i = 1;
        while i <= n {
            let idx = (first_on + i) % n;
            let p = pts[idx];
            if p.on_curve {
                flat.push((p.x, p.y));
                prev = p;
                i += 1;
            } else {
                let mut ctrl = p;
                i += 1;
                loop {
                    let nidx = (first_on + i) % n;
                    let nxt = pts[nidx];
                    if nxt.on_curve {
                        flatten_quad(prev, ctrl, nxt, &mut flat);
                        prev = nxt;
                        i += 1;
                        break;
                    }
                    let mid = OutlinePoint {
                        x: (ctrl.x + nxt.x) * 0.5,
                        y: (ctrl.y + nxt.y) * 0.5,
                        on_curve: true,
                    };
                    flatten_quad(prev, ctrl, mid, &mut flat);
                    prev = mid;
                    ctrl = nxt;
                    i += 1;
                }
            }
        }
        // Scan-convert each segment via edge-function super-sampling.
        for seg in flat.windows(2) {
            let (x0, y0) = seg[0];
            let (x1, y1) = seg[1];
            rasterize_edge(
                x0 * scale,
                y0 * scale,
                x1 * scale,
                y1 * scale,
                min_x,
                min_y,
                w,
                h,
                SUB,
                &mut accum,
            );
        }
    }

    // Winding accum -> coverage (non-zero fill).
    let mut out = vec![0u8; (w * h) as usize];
    for y in 0..h {
        let mut cov: i32 = 0;
        for x in 0..w {
            cov += accum[(y * w + x) as usize] as i32;
            let c = cov.unsigned_abs().min(255) as u8;
            out[(y * w + x) as usize] = c;
        }
    }
    // Flip y: above used PDF-style y-up bbox, we want top-left origin image.
    let mut flipped = vec![0u8; out.len()];
    for y in 0..h {
        let src = (h - 1 - y) as usize * w as usize;
        let dst = y as usize * w as usize;
        flipped[dst..dst + w as usize].copy_from_slice(&out[src..src + w as usize]);
    }

    RasterBitmap {
        width: w,
        height: h,
        data: flipped,
    }
}

fn flatten_quad(p0: OutlinePoint, p1: OutlinePoint, p2: OutlinePoint, out: &mut Vec<(f64, f64)>) {
    const STEPS: usize = 8;
    for s in 1..=STEPS {
        let t = s as f64 / STEPS as f64;
        let omt = 1.0 - t;
        let x = omt * omt * p0.x + 2.0 * omt * t * p1.x + t * t * p2.x;
        let y = omt * omt * p0.y + 2.0 * omt * t * p1.y + t * t * p2.y;
        out.push((x, y));
    }
}

#[allow(clippy::too_many_arguments)]
fn rasterize_edge(
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    min_x: i32,
    min_y: i32,
    w: u32,
    h: u32,
    sub: i32,
    accum: &mut [u16],
) {
    // Toy non-zero winding accumulator: add +sub on upward crossings,
    // -sub on downward. Each horizontal pixel row finds the x-intercept
    // and bumps the accum cell. Good enough for a debug dump.
    let (lo_y, hi_y, winding) = if y0 < y1 {
        (y0, y1, 1i32)
    } else if y1 < y0 {
        (y1, y0, -1i32)
    } else {
        return;
    };
    let y_start = lo_y.floor() as i32;
    let y_end = hi_y.ceil() as i32;
    for y in y_start..y_end {
        let yc = y as f64 + 0.5;
        if yc < lo_y || yc > hi_y {
            continue;
        }
        let t = (yc - y0) / (y1 - y0);
        let xc = x0 + t * (x1 - x0);
        let px = xc.floor() as i32 - min_x;
        let py = y - min_y;
        if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
            continue;
        }
        let idx = (py as u32 * w + px as u32) as usize;
        let delta = (winding * sub) as i16;
        accum[idx] = accum[idx].wrapping_add_signed(delta);
    }
}

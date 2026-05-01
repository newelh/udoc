//! Path construction and painting operators extracted from interpreter.rs.
//!
//! Contains PathOp enum and all path-related methods on ContentInterpreter:
//! m, l, re, h, c, v, y (construction), S/s/f/F/f*/B/B*/b/b* (painting),
//! n (no-op paint), and the finalize_path helper.

use crate::content::path::{
    FillRule as PageFillRule, PagePath, PathSegmentKind as PageSegment, Point as PagePoint,
};
use crate::diagnostics::{Warning, WarningKind};
use crate::table::{ClipPathIR, FillRule, PathSegment, PathSegmentKind};

use super::{ContentInterpreter, MAX_PATH_SEGMENTS, MAX_SUBPATH_OPS};

/// Flatten a slice of canonical [`PageSegment`]s into one or more
/// closed subpath vertex lists (user-space). Beziers are sampled at 8
/// points; for tiling-pattern fill regions, which are almost always
/// axis-aligned rectangles or near-rects, 8 samples is more than
/// sufficient for region-scan rasterization.
fn flatten_segments_to_subpaths(segments: &[PageSegment]) -> Vec<Vec<PagePoint>> {
    let mut out: Vec<Vec<PagePoint>> = Vec::new();
    let mut current: Vec<PagePoint> = Vec::new();
    let mut cur = PagePoint::new(0.0, 0.0);
    let mut start = PagePoint::new(0.0, 0.0);
    for seg in segments {
        match seg {
            PageSegment::MoveTo { p } => {
                if current.len() >= 3 {
                    out.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
                current.push(*p);
                cur = *p;
                start = *p;
            }
            PageSegment::LineTo { p } => {
                current.push(*p);
                cur = *p;
            }
            PageSegment::CurveTo { c1, c2, end } => {
                // Sample cubic at t = 1/8, 2/8, ..., 1.
                for i in 1..=8 {
                    let t = i as f64 / 8.0;
                    let mt = 1.0 - t;
                    let x = mt * mt * mt * cur.x
                        + 3.0 * mt * mt * t * c1.x
                        + 3.0 * mt * t * t * c2.x
                        + t * t * t * end.x;
                    let y = mt * mt * mt * cur.y
                        + 3.0 * mt * mt * t * c1.y
                        + 3.0 * mt * t * t * c2.y
                        + t * t * t * end.y;
                    current.push(PagePoint::new(x, y));
                }
                cur = *end;
            }
            PageSegment::ClosePath => {
                if let Some(&last) = current.last() {
                    if (last.x - start.x).abs() > 1e-9 || (last.y - start.y).abs() > 1e-9 {
                        current.push(start);
                    }
                }
                if current.len() >= 3 {
                    out.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
                cur = start;
            }
        }
    }
    if current.len() >= 3 {
        out.push(current);
    }
    out
}

// ---------------------------------------------------------------------------
// Path types (for table detection)
// ---------------------------------------------------------------------------

/// A single operation in a path under construction.
/// CurveTo stores all 6 control point coordinates; only x3/y3 are used today
/// (to track the current point) but the full data is retained for future use
/// (e.g., curve-based table border detection).
#[derive(Debug, Clone)]
pub(super) enum PathOp {
    MoveTo {
        x: f64,
        y: f64,
    },
    LineTo {
        x: f64,
        y: f64,
    },
    Rect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    CurveTo {
        #[allow(dead_code)] // control point 1; reserved for path rendering
        x1: f64,
        #[allow(dead_code)] // control point 1; reserved for path rendering
        y1: f64,
        #[allow(dead_code)] // control point 2; reserved for path rendering
        x2: f64,
        #[allow(dead_code)] // control point 2; reserved for path rendering
        y2: f64,
        x3: f64,
        y3: f64,
    },
    ClosePath,
}

// ---------------------------------------------------------------------------
// Path operator methods on ContentInterpreter
// ---------------------------------------------------------------------------

impl ContentInterpreter<'_, '_> {
    /// Push a PathOp to the current subpath, checking the size limit.
    pub(super) fn push_path_op(&mut self, op: PathOp) {
        if self.current_subpath.len() >= MAX_SUBPATH_OPS {
            if !self.path_limit_warned {
                self.path_limit_warned = true;
                self.warn(&format!(
                    "subpath op count exceeded limit ({}), dropping further ops",
                    MAX_SUBPATH_OPS
                ));
            }
            return;
        }
        self.current_subpath.push(op);
    }

    /// m: Begin a new subpath by moving to (x, y).
    pub(super) fn op_path_m(&mut self) {
        if !self.extract_paths {
            return;
        }
        if let Some(nums) = self.pop_numbers(2) {
            let (x, y) = (nums[0], nums[1]);
            self.push_path_op(PathOp::MoveTo { x, y });
            if self.extract_page_paths {
                self.current_page_segments.push(PageSegment::MoveTo {
                    p: PagePoint::new(x, y),
                });
                self.current_page_subpath_start = Some((x, y));
                self.current_page_point = Some((x, y));
            }
        }
    }

    /// l: Append a straight line segment from the current point to (x, y).
    pub(super) fn op_path_l(&mut self) {
        if !self.extract_paths {
            return;
        }
        if let Some(nums) = self.pop_numbers(2) {
            let (x, y) = (nums[0], nums[1]);
            self.push_path_op(PathOp::LineTo { x, y });
            if self.extract_page_paths {
                self.current_page_segments.push(PageSegment::LineTo {
                    p: PagePoint::new(x, y),
                });
                self.current_page_point = Some((x, y));
            }
        }
    }

    /// re: Append a rectangle to the current path as a complete subpath.
    pub(super) fn op_path_re(&mut self) {
        if !self.extract_paths {
            return;
        }
        if let Some(nums) = self.pop_numbers(4) {
            let (x, y, w, h) = (nums[0], nums[1], nums[2], nums[3]);
            self.push_path_op(PathOp::Rect {
                x,
                y,
                width: w,
                height: h,
            });
            if self.extract_page_paths {
                // Canonical PDF rect: M(x,y) -> L(x+w,y) -> L(x+w,y+h) ->
                // L(x,y+h) -> Close. After re, the current point
                // is (x, y) per ISO 32000-2 §8.5.2.1.
                self.current_page_segments.push(PageSegment::MoveTo {
                    p: PagePoint::new(x, y),
                });
                self.current_page_segments.push(PageSegment::LineTo {
                    p: PagePoint::new(x + w, y),
                });
                self.current_page_segments.push(PageSegment::LineTo {
                    p: PagePoint::new(x + w, y + h),
                });
                self.current_page_segments.push(PageSegment::LineTo {
                    p: PagePoint::new(x, y + h),
                });
                self.current_page_segments.push(PageSegment::ClosePath);
                self.current_page_subpath_start = Some((x, y));
                self.current_page_point = Some((x, y));
            }
        }
    }

    /// h: Close the current subpath by appending a straight line from the
    /// current point to the starting point of the subpath.
    pub(super) fn op_path_h(&mut self) {
        if !self.extract_paths {
            return;
        }
        self.push_path_op(PathOp::ClosePath);
        if self.extract_page_paths {
            self.current_page_segments.push(PageSegment::ClosePath);
            if let Some(start) = self.current_page_subpath_start {
                self.current_page_point = Some(start);
            }
        }
    }

    /// c: Append a cubic Bezier curve (6 operands: x1 y1 x2 y2 x3 y3).
    pub(super) fn op_path_c(&mut self) {
        if !self.extract_paths {
            return;
        }
        if let Some(nums) = self.pop_numbers(6) {
            let (x1, y1, x2, y2, x3, y3) = (nums[0], nums[1], nums[2], nums[3], nums[4], nums[5]);
            self.push_path_op(PathOp::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            });
            if self.extract_page_paths {
                self.current_page_segments.push(PageSegment::CurveTo {
                    c1: PagePoint::new(x1, y1),
                    c2: PagePoint::new(x2, y2),
                    end: PagePoint::new(x3, y3),
                });
                self.current_page_point = Some((x3, y3));
            }
        }
    }

    /// v: Append a cubic Bezier curve with the initial control point at the
    /// current point (4 operands: x2 y2 x3 y3). Canonicalized to CurveTo
    /// with c1 == current point.
    pub(super) fn op_path_v(&mut self) {
        if !self.extract_paths {
            return;
        }
        if let Some(nums) = self.pop_numbers(4) {
            let (cx, cy) = self.path_current_point();
            let (x2, y2, x3, y3) = (nums[0], nums[1], nums[2], nums[3]);
            self.push_path_op(PathOp::CurveTo {
                x1: cx,
                y1: cy,
                x2,
                y2,
                x3,
                y3,
            });
            if self.extract_page_paths {
                let (pcx, pcy) = self.current_page_point.unwrap_or((cx, cy));
                self.current_page_segments.push(PageSegment::CurveTo {
                    c1: PagePoint::new(pcx, pcy),
                    c2: PagePoint::new(x2, y2),
                    end: PagePoint::new(x3, y3),
                });
                self.current_page_point = Some((x3, y3));
            }
        }
    }

    /// y: Append a cubic Bezier curve with the final control point at (x3,y3)
    /// (4 operands: x1 y1 x3 y3). Canonicalized to CurveTo with c2 == end
    pub(super) fn op_path_y(&mut self) {
        if !self.extract_paths {
            return;
        }
        if let Some(nums) = self.pop_numbers(4) {
            let (x1, y1, x3, y3) = (nums[0], nums[1], nums[2], nums[3]);
            self.push_path_op(PathOp::CurveTo {
                x1,
                y1,
                x2: x3,
                y2: y3,
                x3,
                y3,
            });
            if self.extract_page_paths {
                self.current_page_segments.push(PageSegment::CurveTo {
                    c1: PagePoint::new(x1, y1),
                    c2: PagePoint::new(x3, y3),
                    end: PagePoint::new(x3, y3),
                });
                self.current_page_point = Some((x3, y3));
            }
        }
    }

    /// Get the current point of the path under construction.
    /// Returns (0, 0) if no point has been established yet.
    pub(super) fn path_current_point(&self) -> (f64, f64) {
        // Walk backwards to find the last point established.
        for op in self.current_subpath.iter().rev() {
            match op {
                PathOp::MoveTo { x, y } | PathOp::LineTo { x, y } => return (*x, *y),
                PathOp::CurveTo { x3, y3, .. } => return (*x3, *y3),
                PathOp::Rect { x, y, .. } => {
                    // After a rect, current point is the starting corner.
                    return (*x, *y);
                }
                PathOp::ClosePath => {
                    // ClosePath moves back to the most recent MoveTo.
                    // Keep searching backwards for it.
                    continue;
                }
            }
        }
        (0.0, 0.0)
    }

    // -----------------------------------------------------------------------
    // Clipping (W, W*)
    // -----------------------------------------------------------------------

    /// W / W* (ISO 32000-2 §8.5.4): append the current path to the
    /// graphics-state clipping region with the given fill rule.
    ///
    /// The current path is read from `self.current_subpath` (un-painted,
    /// un-cleared). We flatten to device-space subpaths using the same
    /// bezier-flattening tolerance as visible fills, then push a
    /// `ClipPathIR` onto `gs.clip_path_stack`. The existing q/Q mechanism
    /// (which clones the whole graphics state) saves/restores this list
    /// automatically.
    ///
    /// `pending_clip = true` is also set by the caller so the next paint op
    /// knows to suppress the visible fill of the clip outline itself.
    pub(super) fn capture_clip_region(&mut self, fill_rule: FillRule) {
        if !self.extract_paths {
            return;
        }
        let ctm = &self.gs.ctm;
        let mut subpaths: Vec<Vec<(f64, f64)>> = Vec::new();
        let mut current: Vec<(f64, f64)> = Vec::new();
        let mut cur_x = 0.0_f64;
        let mut cur_y = 0.0_f64;
        let mut move_x = 0.0_f64;
        let mut move_y = 0.0_f64;

        for op in &self.current_subpath {
            match op {
                PathOp::MoveTo { x, y } => {
                    if current.len() >= 2 {
                        subpaths.push(std::mem::take(&mut current));
                    } else {
                        current.clear();
                    }
                    let (dx, dy) = ctm.transform_point(*x, *y);
                    current.push((dx, dy));
                    cur_x = *x;
                    cur_y = *y;
                    move_x = *x;
                    move_y = *y;
                }
                PathOp::LineTo { x, y } => {
                    let (dx, dy) = ctm.transform_point(*x, *y);
                    current.push((dx, dy));
                    cur_x = *x;
                    cur_y = *y;
                }
                PathOp::Rect {
                    x,
                    y,
                    width,
                    height,
                } => {
                    if current.len() >= 2 {
                        subpaths.push(std::mem::take(&mut current));
                    } else {
                        current.clear();
                    }
                    let (p0x, p0y) = ctm.transform_point(*x, *y);
                    let (p1x, p1y) = ctm.transform_point(*x + *width, *y);
                    let (p2x, p2y) = ctm.transform_point(*x + *width, *y + *height);
                    let (p3x, p3y) = ctm.transform_point(*x, *y + *height);
                    subpaths.push(vec![(p0x, p0y), (p1x, p1y), (p2x, p2y), (p3x, p3y)]);
                    cur_x = *x;
                    cur_y = *y;
                    move_x = *x;
                    move_y = *y;
                }
                PathOp::CurveTo {
                    x1,
                    y1,
                    x2,
                    y2,
                    x3,
                    y3,
                } => {
                    let (sx, sy) = ctm.transform_point(cur_x, cur_y);
                    let (c1x, c1y) = ctm.transform_point(*x1, *y1);
                    let (c2x, c2y) = ctm.transform_point(*x2, *y2);
                    let (ex, ey) = ctm.transform_point(*x3, *y3);
                    if current.is_empty() {
                        current.push((sx, sy));
                    }
                    flatten_cubic(sx, sy, c1x, c1y, c2x, c2y, ex, ey, &mut current, 0);
                    cur_x = *x3;
                    cur_y = *y3;
                }
                PathOp::ClosePath => {
                    if let (Some(first), Some(last)) = (current.first(), current.last()) {
                        if (first.0 - last.0).abs() > 1e-6 || (first.1 - last.1).abs() > 1e-6 {
                            current.push(*first);
                        }
                    }
                    if current.len() >= 2 {
                        subpaths.push(std::mem::take(&mut current));
                    } else {
                        current.clear();
                    }
                    cur_x = move_x;
                    cur_y = move_y;
                }
            }
        }
        if current.len() >= 2 {
            subpaths.push(current);
        }
        if subpaths.is_empty() {
            return;
        }
        // Safety: cap the nested-clip count so an adversarial stream can't
        // drive unbounded memory growth via repeated W operators inside a
        // single q/Q frame.
        const MAX_CLIP_STACK: usize = 64;
        if self.gs.clip_path_stack.len() >= MAX_CLIP_STACK {
            return;
        }
        self.gs.clip_path_stack.push(ClipPathIR {
            subpaths,
            fill_rule,
        });
    }

    // -----------------------------------------------------------------------
    // Path painting operators
    // -----------------------------------------------------------------------

    /// Paint the current path: convert accumulated PathOps to PathSegments
    /// and append to self.paths.
    /// `close`: whether to close the path first (s, b, b*).
    /// `stroked`: whether the path is stroked (S, s, B, B*, b, b*).
    /// `filled`: whether the path is filled (f, F, f*, B, B*, b, b*).
    /// `fill_rule`: fill rule for filled paths (NonZeroWinding or EvenOdd).
    ///
    /// When `extract_page_paths` is enabled, also snapshots the CTM and
    /// stroke style at this moment and emits one
    /// [`PagePath`](crate::content::path::PagePath) per paint operator into
    /// the `page_paths` accumulator.
    pub(super) fn op_path_paint(
        &mut self,
        close: bool,
        stroked: bool,
        filled: bool,
        fill_rule: FillRule,
    ) {
        if !self.extract_paths {
            self.current_subpath.clear();
            self.current_page_segments.clear();
            self.current_page_subpath_start = None;
            self.current_page_point = None;
            self.pending_clip = false;
            return;
        }

        if close {
            self.push_path_op(PathOp::ClosePath);
            if self.extract_page_paths {
                self.current_page_segments.push(PageSegment::ClosePath);
                if let Some(start) = self.current_page_subpath_start {
                    self.current_page_point = Some(start);
                }
            }
        }

        let was_clip = self.pending_clip;
        // When W/W* was set before this paint op, the path defines a clipping
        // boundary. Suppress the fill to avoid covering content outside the
        // clip region (we don't implement actual clipping yet). Keep strokes
        // since they're typically visible decorative borders.
        let effective_filled = if was_clip {
            self.pending_clip = false;
            false
        } else {
            filled
        };

        // Snapshot CTM + stroke style BEFORE consuming the segment buffer so
        // the renderer sees exactly the state at the paint op.
        if self.extract_page_paths && !self.current_page_segments.is_empty() {
            let ctm_at_paint = self.gs.capture_ctm();
            let stroke_style = if stroked {
                Some(self.gs.capture_stroke_style())
            } else {
                None
            };
            let fill_rule_ir = if effective_filled {
                Some(match fill_rule {
                    FillRule::NonZeroWinding => PageFillRule::NonZero,
                    FillRule::EvenOdd => PageFillRule::EvenOdd,
                })
            } else {
                None
            };
            // A clip-only path (W/W* then paint, or a plain `n`) is not
            // captured as a visible PagePath; the renderer will wire clip
            // masks in. For now: only emit when fill or stroke
            // is visible.
            if fill_rule_ir.is_some() || stroke_style.is_some() {
                // If this is a Pattern-colorspace fill, emit a tiling
                // pattern record in addition to (or instead of) the
                // standard PagePath., .
                let segments = std::mem::take(&mut self.current_page_segments);
                let pattern_emitted = if effective_filled {
                    self.try_emit_tiling_pattern(&segments, fill_rule_ir.unwrap(), ctm_at_paint)
                } else {
                    false
                };
                let z = self.page_paths.len() + self.page_tiling_patterns.len();
                let fill_color = if fill_rule_ir.is_some() {
                    Some(self.gs.capture_fill_color())
                } else {
                    None
                };
                // When a pattern fill was emitted, still record the base
                // fill as a PagePath so renderers that don't honour
                // patterns (or the pattern type is unsupported) have a
                // graceful fallback. The pattern record rides on top.
                if !pattern_emitted || stroke_style.is_some() {
                    self.page_paths.push(PagePath {
                        segments,
                        fill: if pattern_emitted { None } else { fill_rule_ir },
                        fill_color: if pattern_emitted { None } else { fill_color },
                        stroke: stroke_style,
                        ctm_at_paint,
                        z,
                    });
                }
            } else {
                self.current_page_segments.clear();
            }
        } else {
            self.current_page_segments.clear();
        }
        self.current_page_subpath_start = None;
        self.current_page_point = None;

        let ops = std::mem::take(&mut self.current_subpath);
        self.finalize_path(ops, stroked, effective_filled, fill_rule);
    }

    /// n: End path without painting (no-op for rendering, used for clipping).
    pub(super) fn op_path_n(&mut self) {
        self.current_subpath.clear();
        self.current_page_segments.clear();
        self.current_page_subpath_start = None;
        self.current_page_point = None;
        self.pending_clip = false;
    }

    /// When the gstate has a pattern-fill bound, resolve the pattern
    /// and emit a [`PageTilingPattern`](crate::content::path::PageTilingPattern)
    /// record describing the fill region + tile geometry. Returns true
    /// if a pattern record was emitted.
    ///
    ///ISO 32000-2 §8.7.3, .
    fn try_emit_tiling_pattern(
        &mut self,
        segments: &[PageSegment],
        fill_rule: PageFillRule,
        ctm_at_paint: crate::content::path::Matrix3,
    ) -> bool {
        let Some(name) = self.gs.fill_pattern_name.clone() else {
            return false;
        };
        let Some(pattern_obj) = self.pattern_resources.get(&name).cloned() else {
            // Named a pattern we don't know about: warn once and skip.
            if self.pattern_warned.insert(name.clone()) {
                self.diagnostics.warning(Warning::with_context(
                    None,
                    WarningKind::InvalidState,
                    self.warning_context(),
                    format!(
                        "scn: Pattern /{name} not found in /Resources /Pattern, \
                         falling through to base fill"
                    ),
                ));
            }
            return false;
        };
        let outcome = crate::pattern::parse_tiling_pattern(
            &name,
            &pattern_obj,
            self.resolver,
            &*self.diagnostics,
        );
        let tp = match outcome {
            crate::pattern::ParseOutcome::ColouredTiling(tp) => tp,
            crate::pattern::ParseOutcome::Unsupported { .. }
            | crate::pattern::ParseOutcome::Invalid => {
                // Already diagnosed by parse_tiling_pattern. Fall
                // through to the base fill color by leaving this
                // paint op as a normal PagePath.
                return false;
            }
        };
        // Build fill-region subpaths in user space from the canonical
        // segments IR. Bezier curves are flattened to polylines with a
        // coarse 0.25-user-space-unit tolerance (the renderer re-fits
        // them through the CTM later).
        let fill_subpaths = flatten_segments_to_subpaths(segments);
        if fill_subpaths.is_empty() {
            return false;
        }
        let fallback_color = crate::content::path::tile_fallback_color(&tp.content_stream);
        let alpha = (self.gs.fill_alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        let z = self.page_paths.len() + self.page_tiling_patterns.len();
        self.page_tiling_patterns
            .push(crate::content::path::PageTilingPattern {
                resource_name: name,
                bbox: tp.bbox,
                xstep: tp.xstep,
                ystep: tp.ystep,
                matrix: tp.matrix,
                content_stream: tp.content_stream,
                fill_subpaths,
                fill_rule,
                ctm_at_paint,
                alpha,
                fallback_color,
                z,
            });
        true
    }

    /// Convert accumulated PathOps into PathSegments in device space.
    ///
    /// For filled paths with curves or complex geometry: emits a Polygon
    /// with all subpaths flattened to vertex lists for scanline fill rendering.
    /// For stroked paths: also emits individual Line segments for table detection.
    /// Single-rect paths emit Rect as before.
    fn finalize_path(
        &mut self,
        ops: Vec<PathOp>,
        stroked: bool,
        filled: bool,
        fill_rule: FillRule,
    ) {
        let z_index = self.next_render_order();
        let ctm = &self.gs.ctm;
        let ctm_scale = (ctm.a.powi(2) + ctm.b.powi(2)).sqrt();
        let line_width = self.gs.line_width * ctm_scale;
        let stroke_color = self.gs.stroke_color;
        let fill_color = self.gs.fill_color;
        let fill_alpha = (self.gs.fill_alpha * 255.0).round() as u8;
        let stroke_alpha = (self.gs.stroke_alpha * 255.0).round() as u8;
        // Snapshot the active clip stack at paint time (ISO §8.5.4).
        // Every PathSegment emitted below inherits the same clip set so the
        // renderer can intersect at composite time.
        let active_clips: Vec<ClipPathIR> = self.gs.clip_path_stack.clone();

        // Shortcut: if all ops are Rect, emit individual Rect segments.
        // This preserves backward compat with table detection and avoids
        // unnecessarily promoting simple rects to Polygon.
        let all_rects = ops.iter().all(|op| matches!(op, PathOp::Rect { .. }));
        if all_rects {
            for op in &ops {
                if self.paths.len() >= MAX_PATH_SEGMENTS {
                    if !self.path_limit_warned {
                        self.path_limit_warned = true;
                        self.warn(&format!(
                            "path segment count exceeded limit ({}), dropping further paths",
                            MAX_PATH_SEGMENTS
                        ));
                    }
                    return;
                }
                if let PathOp::Rect {
                    x,
                    y,
                    width,
                    height,
                } = op
                {
                    let (p0x, p0y) = ctm.transform_point(*x, *y);
                    let (p1x, p1y) = ctm.transform_point(*x + *width, *y);
                    let (p2x, p2y) = ctm.transform_point(*x, *y + *height);
                    let (p3x, p3y) = ctm.transform_point(*x + *width, *y + *height);
                    let rx = p0x.min(p1x).min(p2x).min(p3x);
                    let ry = p0y.min(p1y).min(p2y).min(p3y);
                    let rw = p0x.max(p1x).max(p2x).max(p3x) - rx;
                    let rh = p0y.max(p1y).max(p2y).max(p3y) - ry;
                    self.paths.push(PathSegment {
                        kind: PathSegmentKind::Rect {
                            x: rx,
                            y: ry,
                            width: rw,
                            height: rh,
                        },
                        line_width,
                        stroked,
                        filled,
                        stroke_color,
                        fill_color,
                        z_index,
                        fill_alpha,
                        stroke_alpha,
                        active_clips: active_clips.clone(),
                    });
                }
            }
            return;
        }

        // Build vertex-based subpaths for polygon fill.
        let mut subpaths: Vec<Vec<(f64, f64)>> = Vec::new();
        let mut current_verts: Vec<(f64, f64)> = Vec::new();
        let mut cur_x = 0.0_f64;
        let mut cur_y = 0.0_f64;
        let mut move_x = 0.0_f64;
        let mut move_y = 0.0_f64;

        for op in &ops {
            match op {
                PathOp::MoveTo { x, y } => {
                    // Start new subpath; save previous if non-empty.
                    if current_verts.len() >= 2 {
                        subpaths.push(std::mem::take(&mut current_verts));
                    } else {
                        current_verts.clear();
                    }
                    let (dx, dy) = ctm.transform_point(*x, *y);
                    current_verts.push((dx, dy));
                    cur_x = *x;
                    cur_y = *y;
                    move_x = *x;
                    move_y = *y;
                }
                PathOp::LineTo { x, y } => {
                    let (dx, dy) = ctm.transform_point(*x, *y);
                    current_verts.push((dx, dy));
                    cur_x = *x;
                    cur_y = *y;
                }
                PathOp::Rect {
                    x,
                    y,
                    width,
                    height,
                } => {
                    // Save previous subpath.
                    if current_verts.len() >= 2 {
                        subpaths.push(std::mem::take(&mut current_verts));
                    } else {
                        current_verts.clear();
                    }
                    // Emit rect as a 4-vertex closed subpath.
                    let (p0x, p0y) = ctm.transform_point(*x, *y);
                    let (p1x, p1y) = ctm.transform_point(*x + *width, *y);
                    let (p2x, p2y) = ctm.transform_point(*x + *width, *y + *height);
                    let (p3x, p3y) = ctm.transform_point(*x, *y + *height);
                    subpaths.push(vec![(p0x, p0y), (p1x, p1y), (p2x, p2y), (p3x, p3y)]);
                    cur_x = *x;
                    cur_y = *y;
                    move_x = *x;
                    move_y = *y;
                }
                PathOp::CurveTo {
                    x1,
                    y1,
                    x2,
                    y2,
                    x3,
                    y3,
                } => {
                    // Flatten cubic bezier to vertices.
                    let (sx, sy) = ctm.transform_point(cur_x, cur_y);
                    let (c1x, c1y) = ctm.transform_point(*x1, *y1);
                    let (c2x, c2y) = ctm.transform_point(*x2, *y2);
                    let (ex, ey) = ctm.transform_point(*x3, *y3);
                    if current_verts.is_empty() {
                        current_verts.push((sx, sy));
                    }
                    flatten_cubic(sx, sy, c1x, c1y, c2x, c2y, ex, ey, &mut current_verts, 0);
                    cur_x = *x3;
                    cur_y = *y3;
                }
                PathOp::ClosePath => {
                    // Close by adding the start point if not already there.
                    if let (Some(first), Some(last)) = (current_verts.first(), current_verts.last())
                    {
                        if (first.0 - last.0).abs() > 1e-6 || (first.1 - last.1).abs() > 1e-6 {
                            current_verts.push(*first);
                        }
                    }
                    if current_verts.len() >= 2 {
                        subpaths.push(std::mem::take(&mut current_verts));
                    } else {
                        current_verts.clear();
                    }
                    cur_x = move_x;
                    cur_y = move_y;
                }
            }
        }
        // Final subpath.
        if current_verts.len() >= 2 {
            subpaths.push(current_verts);
        }

        if self.paths.len() >= MAX_PATH_SEGMENTS {
            return;
        }

        // Emit filled polygon for rendering.
        if filled && !subpaths.is_empty() {
            self.paths.push(PathSegment {
                kind: PathSegmentKind::Polygon {
                    subpaths: subpaths.clone(),
                    fill_rule,
                },
                line_width,
                stroked: false,
                filled: true,
                stroke_color,
                fill_color,
                z_index,
                fill_alpha,
                stroke_alpha,
                active_clips: active_clips.clone(),
            });
        }

        // Also emit individual line segments for stroked paths (backward compat
        // with table detection which matches on Line/Rect).
        if stroked {
            for subpath in &subpaths {
                for w in subpath.windows(2) {
                    if self.paths.len() >= MAX_PATH_SEGMENTS {
                        break;
                    }
                    self.paths.push(PathSegment {
                        kind: PathSegmentKind::Line {
                            x1: w[0].0,
                            y1: w[0].1,
                            x2: w[1].0,
                            y2: w[1].1,
                        },
                        line_width,
                        stroked: true,
                        filled: false,
                        stroke_color,
                        fill_color,
                        z_index,
                        fill_alpha,
                        stroke_alpha,
                        active_clips: active_clips.clone(),
                    });
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Flatten a cubic bezier curve into line segments via recursive subdivision.
/// Appends points (excluding the start point) to `pts`.
#[allow(clippy::too_many_arguments)]
fn flatten_cubic(
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    x3: f64,
    y3: f64,
    pts: &mut Vec<(f64, f64)>,
    depth: u32,
) {
    const MAX_DEPTH: u32 = 8;
    const FLATNESS: f64 = 0.5;

    if depth >= MAX_DEPTH {
        pts.push((x3, y3));
        return;
    }

    let dx = x3 - x0;
    let dy = y3 - y0;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-10 {
        pts.push((x3, y3));
        return;
    }

    let d1 = ((x1 - x0) * dy - (y1 - y0) * dx).abs();
    let d2 = ((x2 - x0) * dy - (y2 - y0) * dx).abs();
    let max_d = d1.max(d2);

    if max_d * max_d <= FLATNESS * FLATNESS * len_sq {
        pts.push((x3, y3));
        return;
    }

    // Subdivide at t=0.5 using de Casteljau.
    let m01x = (x0 + x1) * 0.5;
    let m01y = (y0 + y1) * 0.5;
    let m12x = (x1 + x2) * 0.5;
    let m12y = (y1 + y2) * 0.5;
    let m23x = (x2 + x3) * 0.5;
    let m23y = (y2 + y3) * 0.5;
    let m012x = (m01x + m12x) * 0.5;
    let m012y = (m01y + m12y) * 0.5;
    let m123x = (m12x + m23x) * 0.5;
    let m123y = (m12y + m23y) * 0.5;
    let mx = (m012x + m123x) * 0.5;
    let my = (m012y + m123y) * 0.5;

    flatten_cubic(x0, y0, m01x, m01y, m012x, m012y, mx, my, pts, depth + 1);
    flatten_cubic(mx, my, m123x, m123y, m23x, m23y, x3, y3, pts, depth + 1);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::{ContentInterpreter, MAX_PATH_SEGMENTS, MAX_SUBPATH_OPS};
    use crate::diagnostics::{CollectingDiagnostics, NullDiagnostics};
    use crate::object::resolver::ObjectResolver;
    use crate::object::{PdfDictionary, PdfObject};
    use crate::table::PathSegmentKind;

    #[test]
    fn test_path_extraction_disabled_by_default() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        // Path ops should be ignored when extract_paths is false (default)
        let content = b"100 200 300 400 re S";
        interp.interpret(content).unwrap();
        let paths = interp.take_paths();
        assert!(
            paths.is_empty(),
            "paths should not be collected when extract_paths is false"
        );
    }

    #[test]
    fn test_path_rect_basic() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        interp.set_extract_paths(true);
        let content = b"100 200 300 50 re S";
        interp.interpret(content).unwrap();
        let paths = interp.take_paths();
        assert_eq!(paths.len(), 1);
        match &paths[0].kind {
            PathSegmentKind::Rect {
                x,
                y,
                width,
                height,
            } => {
                assert!((x - 100.0).abs() < 1e-10);
                assert!((y - 200.0).abs() < 1e-10);
                assert!((width - 300.0).abs() < 1e-10);
                assert!((height - 50.0).abs() < 1e-10);
            }
            _ => panic!("expected Rect, got Line"),
        }
        assert!(paths[0].stroked);
        assert!(!paths[0].filled);
    }

    #[test]
    fn test_path_line_basic() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        interp.set_extract_paths(true);
        let content = b"10 20 m 100 200 l S";
        interp.interpret(content).unwrap();
        let paths = interp.take_paths();
        assert_eq!(paths.len(), 1);
        match &paths[0].kind {
            PathSegmentKind::Line { x1, y1, x2, y2 } => {
                assert!((*x1 - 10.0).abs() < 1e-10);
                assert!((*y1 - 20.0).abs() < 1e-10);
                assert!((*x2 - 100.0).abs() < 1e-10);
                assert!((*y2 - 200.0).abs() < 1e-10);
            }
            _ => panic!("expected Line, got Rect"),
        }
        assert!(paths[0].stroked);
        assert!(!paths[0].filled);
    }

    #[test]
    fn test_path_fill_operator() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        interp.set_extract_paths(true);
        let content = b"50 60 200 100 re f";
        interp.interpret(content).unwrap();
        let paths = interp.take_paths();
        assert_eq!(paths.len(), 1);
        assert!(!paths[0].stroked);
        assert!(paths[0].filled);
    }

    #[test]
    fn test_path_fill_and_stroke() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        interp.set_extract_paths(true);
        let content = b"50 60 200 100 re B";
        interp.interpret(content).unwrap();
        let paths = interp.take_paths();
        assert_eq!(paths.len(), 1);
        assert!(paths[0].stroked);
        assert!(paths[0].filled);
    }

    #[test]
    fn test_path_n_clears_without_emitting() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        interp.set_extract_paths(true);
        // Build a path and then use 'n' to discard it
        let content = b"10 20 m 100 200 l n";
        interp.interpret(content).unwrap();
        let paths = interp.take_paths();
        assert!(
            paths.is_empty(),
            "n operator should discard paths without emitting"
        );
    }

    #[test]
    fn test_path_line_width() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        interp.set_extract_paths(true);
        // Set line width to 2.5, then stroke a line
        let content = b"2.5 w 10 20 m 100 200 l S";
        interp.interpret(content).unwrap();
        let paths = interp.take_paths();
        assert_eq!(paths.len(), 1);
        assert!((paths[0].line_width - 2.5).abs() < 1e-10);
    }

    #[test]
    fn test_path_line_width_saved_restored() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        interp.set_extract_paths(true);
        // q saves state, w changes width, Q restores, stroke uses original width
        let content = b"q 5.0 w Q 10 20 m 100 200 l S";
        interp.interpret(content).unwrap();
        let paths = interp.take_paths();
        assert_eq!(paths.len(), 1);
        // Default line_width is 1.0, should be restored after Q
        assert!((paths[0].line_width - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_path_with_ctm_transform() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        interp.set_extract_paths(true);
        // Apply translation (100, 50) via cm, then draw a line from (0,0) to (10,10)
        let content = b"1 0 0 1 100 50 cm 0 0 m 10 10 l S";
        interp.interpret(content).unwrap();
        let paths = interp.take_paths();
        assert_eq!(paths.len(), 1);
        match &paths[0].kind {
            PathSegmentKind::Line { x1, y1, x2, y2 } => {
                assert!((*x1 - 100.0).abs() < 1e-10, "x1 should be 100, got {x1}");
                assert!((*y1 - 50.0).abs() < 1e-10, "y1 should be 50, got {y1}");
                assert!((*x2 - 110.0).abs() < 1e-10, "x2 should be 110, got {x2}");
                assert!((*y2 - 60.0).abs() < 1e-10, "y2 should be 60, got {y2}");
            }
            _ => panic!("expected Line"),
        }
    }

    #[test]
    fn test_path_close_stroke() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        interp.set_extract_paths(true);
        // Triangle: (0,0) -> (100,0) -> (50,100) -> close -> stroke
        // Should produce 3 line segments: the two explicit lines + close line
        let content = b"0 0 m 100 0 l 50 100 l s";
        interp.interpret(content).unwrap();
        let paths = interp.take_paths();
        // 2 LineTo segments + 1 ClosePath segment
        assert_eq!(paths.len(), 3, "triangle should produce 3 line segments");
        assert!(paths[0].stroked);
    }

    #[test]
    fn test_path_close_multi_subpath() {
        // Two subpaths in one paint: each ClosePath must close to its own MoveTo.
        // Subpath A: (0,0) -> (100,0) -> close (should close to 0,0)
        // Subpath B: (200,0) -> (300,0) -> close (should close to 200,0)
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        interp.set_extract_paths(true);
        let content = b"0 0 m 100 0 l h 200 0 m 300 0 l h S";
        interp.interpret(content).unwrap();
        let paths = interp.take_paths();
        // Each subpath: 1 LineTo + 1 ClosePath = 2 segments, total 4
        assert_eq!(
            paths.len(),
            4,
            "two subpaths with close should produce 4 lines"
        );
        // First close line: (100,0) -> (0,0)
        if let PathSegmentKind::Line { x1, y1, x2, y2 } = paths[1].kind {
            assert!(
                (x1 - 100.0).abs() < 0.01,
                "close A: x1 should be 100, got {x1}"
            );
            assert!(y1.abs() < 0.01, "close A: y1 should be 0, got {y1}");
            assert!(x2.abs() < 0.01, "close A: x2 should be 0, got {x2}");
            assert!(y2.abs() < 0.01, "close A: y2 should be 0, got {y2}");
        } else {
            panic!("expected Line for close segment A");
        }
        // Second close line: (300,0) -> (200,0)
        if let PathSegmentKind::Line { x1, y1, x2, y2 } = paths[3].kind {
            assert!(
                (x1 - 300.0).abs() < 0.01,
                "close B: x1 should be 300, got {x1}"
            );
            assert!(y1.abs() < 0.01, "close B: y1 should be 0, got {y1}");
            assert!(
                (x2 - 200.0).abs() < 0.01,
                "close B: x2 should be 200, got {x2}"
            );
            assert!(y2.abs() < 0.01, "close B: y2 should be 0, got {y2}");
        } else {
            panic!("expected Line for close segment B");
        }
    }

    #[test]
    fn test_path_multiple_rects() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        interp.set_extract_paths(true);
        // Two rects in one paint operation
        let content = b"10 20 100 50 re 200 300 150 75 re S";
        interp.interpret(content).unwrap();
        let paths = interp.take_paths();
        assert_eq!(paths.len(), 2, "two re ops should produce two PathSegments");
    }

    #[test]
    fn test_path_curve_skipped() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        interp.set_extract_paths(true);
        // A pure curve path: moveto then curveto. Flattened to line segments.
        let content = b"0 0 m 10 20 30 40 50 60 c S";
        interp.interpret(content).unwrap();
        let paths = interp.take_paths();
        // CurveTo is now flattened to line segments
        assert!(
            !paths.is_empty(),
            "curve paths should produce flattened line segments"
        );
    }

    #[test]
    fn test_path_segment_limit() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(CollectingDiagnostics::new());

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag.clone(), None);
        interp.set_extract_paths(true);

        // Build a content stream with more than MAX_PATH_SEGMENTS rects
        let count = MAX_PATH_SEGMENTS + 100;
        let mut content = Vec::with_capacity(count * 20);
        for i in 0..count {
            let s = format!("{} 0 10 10 re S ", i);
            content.extend_from_slice(s.as_bytes());
        }

        interp.interpret(&content).unwrap();
        let paths = interp.take_paths();
        assert_eq!(
            paths.len(),
            MAX_PATH_SEGMENTS,
            "should cap at MAX_PATH_SEGMENTS"
        );

        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("path segment count exceeded limit")),
            "should warn when path segment limit is exceeded"
        );
    }

    #[test]
    fn test_subpath_ops_limit() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(CollectingDiagnostics::new());

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag.clone(), None);
        interp.set_extract_paths(true);

        // Build one huge subpath with more than MAX_SUBPATH_OPS lineto ops
        // (no paint op until the end, so they all accumulate in current_subpath).
        let count = MAX_SUBPATH_OPS + 100;
        let mut content = Vec::with_capacity(count * 10 + 20);
        content.extend_from_slice(b"0 0 m ");
        for i in 1..=count {
            let s = format!("{} 0 l ", i);
            content.extend_from_slice(s.as_bytes());
        }
        content.extend_from_slice(b"S");

        interp.interpret(&content).unwrap();
        let paths = interp.take_paths();
        // Should be capped. Exact count depends on MAX_SUBPATH_OPS (MoveTo
        // uses 1 slot, then MAX_SUBPATH_OPS - 1 LineTos fit).
        assert!(
            paths.len() < count,
            "should cap subpath ops, got {} segments from {} ops",
            paths.len(),
            count
        );

        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("subpath op count exceeded limit")),
            "should warn when subpath op limit is exceeded"
        );
    }

    #[test]
    fn test_path_take_clears() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        interp.set_extract_paths(true);
        let content = b"10 20 100 50 re S";
        interp.interpret(content).unwrap();
        let paths1 = interp.take_paths();
        assert_eq!(paths1.len(), 1);
        // Second take should return empty
        let paths2 = interp.take_paths();
        assert!(paths2.is_empty(), "take_paths should leave vec empty");
    }

    #[test]
    fn test_path_v_operator() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        interp.set_extract_paths(true);
        // v operator: initial point replicated. Curve is flattened to line segments.
        // Follow with a lineto to verify the current point was tracked.
        let content = b"0 0 m 10 20 50 60 v 100 100 l S";
        interp.interpret(content).unwrap();
        let paths = interp.take_paths();
        // Curve segments + one explicit lineto from (50,60) to (100,100)
        assert!(paths.len() > 1, "should have curve segments + lineto");
        let last = &paths[paths.len() - 1];
        match &last.kind {
            PathSegmentKind::Line { x1, y1, x2, y2 } => {
                assert!(
                    (*x1 - 50.0).abs() < 1e-10,
                    "x1 should be 50 (curve endpoint)"
                );
                assert!((*y1 - 60.0).abs() < 1e-10);
                assert!((*x2 - 100.0).abs() < 1e-10);
                assert!((*y2 - 100.0).abs() < 1e-10);
            }
            _ => panic!("expected Line"),
        }
    }

    #[test]
    fn test_path_y_operator() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        interp.set_extract_paths(true);
        // y operator: final point replicated. Curve is flattened, then lineto.
        let content = b"0 0 m 10 20 50 60 y 200 300 l S";
        interp.interpret(content).unwrap();
        let paths = interp.take_paths();
        assert!(paths.len() > 1, "should have curve segments + lineto");
        let last = &paths[paths.len() - 1];
        match &last.kind {
            PathSegmentKind::Line { x1, y1, x2, y2 } => {
                assert!(
                    (*x1 - 50.0).abs() < 1e-10,
                    "x1 should be 50 (curve endpoint)"
                );
                assert!((*y1 - 60.0).abs() < 1e-10);
                assert!((*x2 - 200.0).abs() < 1e-10);
                assert!((*y2 - 300.0).abs() < 1e-10);
            }
            _ => panic!("expected Line"),
        }
    }

    #[test]
    fn test_path_f_star_operator() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        interp.set_extract_paths(true);
        let content = b"10 20 100 50 re f*";
        interp.interpret(content).unwrap();
        let paths = interp.take_paths();
        assert_eq!(paths.len(), 1);
        assert!(!paths[0].stroked);
        assert!(paths[0].filled);
    }

    #[test]
    fn test_path_b_star_operator() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        interp.set_extract_paths(true);
        // b* = close, fill, stroke (even-odd)
        let content = b"0 0 m 100 0 l 50 50 l b*";
        interp.interpret(content).unwrap();
        let paths = interp.take_paths();
        // 1 filled Polygon + 3 stroked Lines (for table detection backward compat)
        assert_eq!(paths.len(), 4);
        // First should be the filled polygon.
        assert!(paths[0].filled);
        assert!(!paths[0].stroked);
        assert!(matches!(paths[0].kind, PathSegmentKind::Polygon { .. }));
        // Remaining should be stroked lines.
        for p in &paths[1..] {
            assert!(p.stroked);
            assert!(!p.filled);
            assert!(matches!(p.kind, PathSegmentKind::Line { .. }));
        }
    }

    #[test]
    fn test_path_line_width_ctm_scaled() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        interp.set_extract_paths(true);
        // Apply 2x scaling CTM, set line width to 1.0, draw a line.
        // Effective visual width should be 2.0 (1.0 * scale factor 2.0).
        let content = b"2 0 0 2 0 0 cm 1 w 10 20 m 100 200 l S";
        interp.interpret(content).unwrap();
        let paths = interp.take_paths();
        assert_eq!(paths.len(), 1);
        assert!(
            (paths[0].line_width - 2.0).abs() < 1e-10,
            "line_width should be scaled by CTM: expected 2.0, got {}",
            paths[0].line_width,
        );
    }

    #[test]
    fn test_path_negative_line_width_ignored() {
        let resources = PdfDictionary::new();
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        interp.set_extract_paths(true);
        // Negative line width should be ignored, keeping the default 1.0
        let content = b"-1 w 10 20 100 50 re S";
        interp.interpret(content).unwrap();
        let paths = interp.take_paths();
        assert_eq!(paths.len(), 1);
        assert!((paths[0].line_width - 1.0).abs() < 1e-10);
    }

    /// Regression test for hf-pdfa black-background bug. PDF/A files commonly
    /// define `/CSp /DeviceRGB` directly (not via indirect reference) in the
    /// page resources. Previously these inline names were dropped from the
    /// colorspace_resources map, so `cs name` set fill_cs_components=0 and
    /// subsequent `scn` calls were no-ops, leaving fill_color at the default
    /// black. Fix: pre-resolve inline colorspace component counts at init.
    #[test]
    fn test_inline_colorspace_resolves_components_for_scn() {
        let data = b"%PDF-1.4\n";
        let xref = crate::parse::XrefTable::new();
        let mut resolver = ObjectResolver::new(data.as_slice(), xref);
        let diag = Arc::new(NullDiagnostics);

        // /Resources/ColorSpace/CSp = /DeviceRGB (inline name, not a reference)
        let mut cs_dict = PdfDictionary::new();
        cs_dict.insert(b"CSp".to_vec(), PdfObject::Name(b"DeviceRGB".to_vec()));
        let mut resources = PdfDictionary::new();
        resources.insert(b"ColorSpace".to_vec(), PdfObject::Dictionary(cs_dict));

        let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
        interp.set_extract_paths(true);
        // /CSp cs 1 1 1 scn 10 20 100 50 re f
        // Expected: rect filled white (255,255,255). Bug previously left it black.
        let content = b"/CSp cs 1 1 1 scn 10 20 100 50 re f";
        interp.interpret(content).unwrap();
        let paths = interp.take_paths();
        assert_eq!(paths.len(), 1, "expected one filled rect");
        assert!(paths[0].filled);
        assert_eq!(
            paths[0].fill_color,
            [255, 255, 255],
            "expected white fill (1 1 1 scn under inline /CSp /DeviceRGB)"
        );
    }
}

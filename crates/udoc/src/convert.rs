//! Backend-to-document model conversion.
//!
//! This module converts from the page-level FormatBackend/PageExtractor
//! types to the unified Document model. It walks all pages, extracts
//! text, tables, and images, and builds the Block/Inline tree with
//! optional Presentation overlay.

use std::collections::{HashMap, HashSet};

use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_core::convert::{alloc_id, build_table_data, convert_table_rows, maybe_insert_page_break};
use udoc_core::diagnostics::Warning;
use udoc_core::document::*;
use udoc_core::error::{Error, Result};
use udoc_core::geometry::BoundingBox;

/// Strip soft hyphens (U+00AD) from extracted text.
///
/// Soft hyphens are layout hints (where a text engine *may* break the word
/// across lines) and are rarely what downstream text consumers want. Most
/// popular extractors (poppler, pdfplumber) strip them by default.
/// Preserves the raw text on spans so low-level consumers can still see
/// the original; only the Document model's Inline::Text and the
/// PositionedSpan facade text are normalized.
fn strip_soft_hyphens(s: &str) -> String {
    if s.contains('\u{00AD}') {
        s.replace('\u{00AD}', "")
    } else {
        s.to_string()
    }
}

/// Normalize extracted text for the Document model and presentation overlay.
///
/// Applies soft-hyphen stripping and Unicode NFC (canonical composition).
/// PDFs frequently emit decomposed Unicode (e.g., "e" + combining acute)
/// via ToUnicode CMaps, /ActualText, or raw byte sequences. Most popular
/// extractors (poppler, pdfplumber, Word-to-text) normalize to NFC so
/// downstream consumers doing search, indexing, or case-folding see
/// precomposed forms. Low-level TextSpan.text still preserves the raw
/// decomposed form for fidelity.
pub(crate) fn normalize_for_document(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    strip_soft_hyphens(s).nfc().collect()
}

/// Post-pass at the facade boundary: NFC-normalize every text-bearing
/// Document field so all 12 formats behave consistently.
///
/// The PDF converter used to inline-normalize at construction time; other
/// backends (DOCX, XLSX, PPTX, ODF, RTF, Markdown, DOC, XLS, PPT) built
/// their `Document` inside the backend crate and never saw the facade's
/// normalization helper. Running this walker once when the facade returns
/// a `Document` means every caller of
/// [`crate::extract`] / [`crate::extract_with`] / [`crate::extract_bytes`]
/// / [`crate::extract_bytes_with`] / [`crate::Extractor::into_document`]
/// sees NFC-composed, soft-hyphen-stripped text regardless of format.
///
/// NFC is idempotent, so running this on a PDF document that was already
/// normalized inline (historic behaviour, now removed) would still be
/// correct. Soft-hyphen stripping is likewise idempotent.
///
/// Visits every `Inline::Text`, `Inline::Code`, `Inline::Link` content,
/// `Block::CodeBlock.text`, `Block::Heading` alt text paths, and each
/// `PositionedSpan.text` on the presentation overlay.
pub(crate) fn normalize_document_text(doc: &mut Document) {
    doc.walk_mut(&mut |block| normalize_block_text(block));

    if let Some(ref mut presentation) = doc.presentation {
        for span in &mut presentation.raw_spans {
            if needs_normalization(&span.text) {
                span.text = normalize_for_document(&span.text);
            }
        }
    }
}

/// Quick test: does this string contain anything that NFC or soft-hyphen
/// stripping would change? Avoids re-collecting an all-ASCII string.
fn needs_normalization(s: &str) -> bool {
    s.contains('\u{00AD}') || !s.is_ascii()
}

/// Normalize text in a single block. `walk_mut` recurses into nested
/// structures (lists, tables, sections, shapes) for us; this function
/// handles only the fields that live directly on `block`.
/// Returns true when the PagePath IR segments describe a pure axis-aligned
/// rectangle (M -> L -> L -> L -> Close, with collinear edges) or a single
/// line segment (M -> L). These paths are rendered crisper by the legacy
/// [`PageShape`] line/rect helpers than by the PaintPath outline-expansion
/// pipeline, so we skip emitting them as PaintPaths to avoid double-drawing.
fn path_is_simple_rect_or_line(segments: &[udoc_pdf::PagePathSegmentKind]) -> bool {
    use udoc_pdf::PagePathSegmentKind as S;
    // Pure line: M -> L, optionally followed by a Close.
    if matches!(segments.first(), Some(S::MoveTo { .. }))
        && matches!(segments.get(1), Some(S::LineTo { .. }))
        && segments.len() <= 3
    {
        let tail_ok = segments.len() == 2 || matches!(segments.get(2), Some(S::ClosePath));
        if tail_ok {
            return true;
        }
    }
    // Axis-aligned rect: M, L, L, L, Close. Rectangles from `re` expand
    // to exactly this shape in the IR (see path_ops.rs op_path_re).
    if segments.len() == 5
        && matches!(segments[0], S::MoveTo { .. })
        && matches!(segments[1], S::LineTo { .. })
        && matches!(segments[2], S::LineTo { .. })
        && matches!(segments[3], S::LineTo { .. })
        && matches!(segments[4], S::ClosePath)
    {
        // Check axis-alignment: consecutive edges must share an X or Y.
        let p0 = match segments[0] {
            S::MoveTo { p } => (p.x, p.y),
            _ => return false,
        };
        let p1 = match segments[1] {
            S::LineTo { p } => (p.x, p.y),
            _ => return false,
        };
        let p2 = match segments[2] {
            S::LineTo { p } => (p.x, p.y),
            _ => return false,
        };
        let p3 = match segments[3] {
            S::LineTo { p } => (p.x, p.y),
            _ => return false,
        };
        let axis_aligned = |a: (f64, f64), b: (f64, f64)| -> bool {
            (a.0 - b.0).abs() < 1e-6 || (a.1 - b.1).abs() < 1e-6
        };
        if axis_aligned(p0, p1)
            && axis_aligned(p1, p2)
            && axis_aligned(p2, p3)
            && axis_aligned(p3, p0)
        {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Annotation rendering.
//
// Each annotation is turned into one or more `PaintPath` records in page
// user space so the existing renderer pipeline composites them under the
// same z-ordered queue as native content-stream paths. Text-markup subtypes
// (Highlight / Underline / StrikeOut / Squiggly / border-decorated Link)
// are synthesized from /QuadPoints + /Rect without reading /AP. Subtypes
// that ship with a pre-rendered appearance (Stamp / Watermark / FreeText /
// Ink / other) have their /AP/N interpreted as a fresh content stream and
// every emitted path's CTM composed with §12.5.5 (Matrix * RectFit).
//
// ISO 32000-2 §12.5.5 equation we implement:
//   1. Transform the /BBox corners by the form-space /Matrix.
//   2. Compute the transformed bbox (tx_min, ty_min, tx_max, ty_max).
//   3. Build a rect-fit matrix A mapping transformed bbox -> /Rect.
//   4. Composite transform = Matrix * A.
//   5. Each AP-emitted PagePath CTM becomes old_ctm * (Matrix * A).
// ---------------------------------------------------------------------------

/// Emit PaintPaths for a single annotation whose subtype does NOT rely
/// on a pre-rendered /AP/N stream (text-markup / Ink / Link border).
/// Stamp/Watermark/FreeText/OtherWithAppearance are handled separately
/// in the main convert loop via `interpret_annotation_appearance`.
fn emit_annotation_paths(
    page_idx: usize,
    ann: &udoc_pdf::PageAnnotation,
    base_z: u32,
    out_paths: &mut Vec<PaintPath>,
    _out_spans: &mut Vec<PositionedSpan>,
) {
    use udoc_pdf::PageAnnotationKind as K;

    match ann.kind {
        K::Highlight => emit_highlight_overlay(page_idx, ann, base_z, out_paths),
        K::Underline => emit_markup_line(page_idx, ann, base_z, out_paths, MarkupLine::Underline),
        K::StrikeOut => emit_markup_line(page_idx, ann, base_z, out_paths, MarkupLine::StrikeOut),
        K::Squiggly => emit_markup_line(page_idx, ann, base_z, out_paths, MarkupLine::Squiggly),
        K::Link => {
            // /Link annotations are navigation-only by default. MuPDF's
            // `mutool draw` (and Acrobat in viewer-mode) suppresses the
            // /Border rectangle unless the annotation carries an explicit
            // appearance stream. Honouring /Border + /C unconditionally
            // painted a coloured frame on every arxiv hyperlink
            // (`/C [0 1 1] /Border [0 0 1]`), producing bright cyan/green
            // strips in regions MuPDF rendered as whitespace and
            // dragging the arxiv-physics cluster from ~0.97 to ~0.88
            // SSIM ( item 2). The appearance-stream
            // branch in `convert_pages` still emits any visible border
            // through the /AP path (which matches MuPDF); this no-op
            // only governs the AP-less default.
        }
        K::Ink => emit_ink_paths(page_idx, ann, base_z, out_paths),
        K::Stamp | K::Watermark | K::FreeText | K::OtherWithAppearance => {
            // AP-less fallback: nothing to draw (mupdf drops too).
        }
    }
}

/// Yellow-ish multiply-blend overlay over each /QuadPoints quad (or the
/// /Rect when QuadPoints is absent).
fn emit_highlight_overlay(
    page_idx: usize,
    ann: &udoc_pdf::PageAnnotation,
    base_z: u32,
    out_paths: &mut Vec<PaintPath>,
) {
    // Default highlight colour is yellow (1, 1, 0). /C overrides.
    let color = ann
        .color
        .map(|c| Color::rgb(c[0], c[1], c[2]))
        .unwrap_or(Color::rgb(255, 255, 0));
    // Multiply blend is approximated via reduced alpha so yellow tints the
    // underlying ink instead of masking it. 80 / 255 ~= 0.31 is close to
    // what mupdf produces for the default highlight blend mode.
    const HIGHLIGHT_ALPHA: u8 = 80;

    let quads = collect_quads(ann);
    for (i, quad) in quads.iter().enumerate() {
        let segs = vec![
            PaintSegment::MoveTo {
                x: quad[0].0,
                y: quad[0].1,
            },
            PaintSegment::LineTo {
                x: quad[1].0,
                y: quad[1].1,
            },
            PaintSegment::LineTo {
                x: quad[2].0,
                y: quad[2].1,
            },
            PaintSegment::LineTo {
                x: quad[3].0,
                y: quad[3].1,
            },
            PaintSegment::ClosePath,
        ];
        out_paths.push(PaintPath::new(
            page_idx,
            segs,
            Some(FillRule::NonZeroWinding),
            Some(color),
            HIGHLIGHT_ALPHA,
            None,
            identity_ctm(),
            base_z.saturating_add(i as u32),
        ));
    }
}

/// Straight-line or wavy decoration at the bottom / middle of each quad.
#[derive(Debug, Clone, Copy)]
enum MarkupLine {
    Underline,
    StrikeOut,
    Squiggly,
}

fn emit_markup_line(
    page_idx: usize,
    ann: &udoc_pdf::PageAnnotation,
    base_z: u32,
    out_paths: &mut Vec<PaintPath>,
    kind: MarkupLine,
) {
    let color = ann
        .color
        .map(|c| Color::rgb(c[0], c[1], c[2]))
        .unwrap_or(Color::rgb(0, 0, 0));
    let quads = collect_quads(ann);
    for (i, quad) in quads.iter().enumerate() {
        // Quad corners (PDF §12.5.6.10): UL UR LL LR. Our collect_quads
        // returns them in bottom-left / bottom-right / top-right / top-left
        // order (closed polygon), so:
        //   quad[0] = BL, quad[1] = BR, quad[2] = TR, quad[3] = TL.
        let bl = quad[0];
        let br = quad[1];
        let tr = quad[2];
        let tl = quad[3];
        // Thickness scales with quad height. MuPDF uses ~1/16 of quad
        // height for StrikeOut and Underline at default. Use a minimum
        // of 0.75 pt so thin spans still register.
        let quad_h = (tl.1 - bl.1).abs().max((tr.1 - br.1).abs()).max(1.0);
        let line_w = (quad_h / 16.0).max(0.75) as f32;
        let (from, to) = match kind {
            MarkupLine::Underline => (bl, br),
            MarkupLine::StrikeOut => {
                let mx = ((bl.0 + tl.0) / 2.0, (bl.1 + tl.1) / 2.0);
                let my = ((br.0 + tr.0) / 2.0, (br.1 + tr.1) / 2.0);
                (mx, my)
            }
            MarkupLine::Squiggly => (bl, br),
        };

        let segs = if matches!(kind, MarkupLine::Squiggly) {
            squiggly_segments(from, to, quad_h * 0.18)
        } else {
            vec![
                PaintSegment::MoveTo {
                    x: from.0,
                    y: from.1,
                },
                PaintSegment::LineTo { x: to.0, y: to.1 },
            ]
        };
        let stroke = PaintStroke {
            line_width: line_w,
            line_cap: PaintLineCap::Butt,
            line_join: PaintLineJoin::Miter,
            miter_limit: 10.0,
            dash_pattern: Vec::new(),
            dash_phase: 0.0,
            color,
            alpha: 255,
        };
        out_paths.push(PaintPath::new(
            page_idx,
            segs,
            None,
            None,
            255,
            Some(stroke),
            identity_ctm(),
            base_z.saturating_add(i as u32),
        ));
    }
}

/// Build a wavy polyline from `from` to `to` with `amp` peak-to-trough
/// amplitude.
fn squiggly_segments(from: (f64, f64), to: (f64, f64), amp: f64) -> Vec<PaintSegment> {
    let dx = to.0 - from.0;
    let dy = to.1 - from.1;
    let len = (dx * dx + dy * dy).sqrt().max(1e-6);
    // ~4-pt wavelength feels right at common highlight sizes.
    let wavelen = 4.0_f64;
    let n = ((len / wavelen).ceil() as usize).max(4);
    // Perpendicular unit vector (+y points up in page space).
    let nx = -dy / len;
    let ny = dx / len;
    let mut segs = Vec::with_capacity(n + 1);
    segs.push(PaintSegment::MoveTo {
        x: from.0,
        y: from.1,
    });
    for i in 1..=n {
        let t = i as f64 / n as f64;
        let bx = from.0 + dx * t;
        let by = from.1 + dy * t;
        let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
        let off = amp * sign;
        segs.push(PaintSegment::LineTo {
            x: bx + nx * off,
            y: by + ny * off,
        });
    }
    segs
}

/// Stroke the /Rect outline with the /Border width.
///
/// Currently unused: item 2 we suppress AP-less
/// /Link borders entirely (matching MuPDF default render behaviour).
/// Kept as a re-enable lever for future "viewer chrome" output modes.
#[allow(dead_code)]
fn emit_border_stroke(
    page_idx: usize,
    ann: &udoc_pdf::PageAnnotation,
    base_z: u32,
    out_paths: &mut Vec<PaintPath>,
) {
    let color = ann
        .color
        .map(|c| Color::rgb(c[0], c[1], c[2]))
        .unwrap_or(Color::rgb(0, 0, 0));
    let r = ann.rect;
    let segs = vec![
        PaintSegment::MoveTo {
            x: r.x_min,
            y: r.y_min,
        },
        PaintSegment::LineTo {
            x: r.x_max,
            y: r.y_min,
        },
        PaintSegment::LineTo {
            x: r.x_max,
            y: r.y_max,
        },
        PaintSegment::LineTo {
            x: r.x_min,
            y: r.y_max,
        },
        PaintSegment::ClosePath,
    ];
    let stroke = PaintStroke {
        line_width: ann.border_width as f32,
        line_cap: PaintLineCap::Butt,
        line_join: PaintLineJoin::Miter,
        miter_limit: 10.0,
        dash_pattern: Vec::new(),
        dash_phase: 0.0,
        color,
        alpha: 255,
    };
    out_paths.push(PaintPath::new(
        page_idx,
        segs,
        None,
        None,
        255,
        Some(stroke),
        identity_ctm(),
        base_z,
    ));
}

/// Stroke each /InkList polyline.
fn emit_ink_paths(
    page_idx: usize,
    ann: &udoc_pdf::PageAnnotation,
    base_z: u32,
    out_paths: &mut Vec<PaintPath>,
) {
    let color = ann
        .color
        .map(|c| Color::rgb(c[0], c[1], c[2]))
        .unwrap_or(Color::rgb(0, 0, 0));
    let line_w = if ann.border_width > 0.0 {
        ann.border_width as f32
    } else {
        1.0
    };
    for (i, stroke_pts) in ann.ink_list.iter().enumerate() {
        if stroke_pts.len() < 2 {
            continue;
        }
        let mut segs = Vec::with_capacity(stroke_pts.len());
        let (sx, sy) = stroke_pts[0];
        segs.push(PaintSegment::MoveTo { x: sx, y: sy });
        for &(x, y) in &stroke_pts[1..] {
            segs.push(PaintSegment::LineTo { x, y });
        }
        let stroke = PaintStroke {
            line_width: line_w,
            line_cap: PaintLineCap::Round,
            line_join: PaintLineJoin::Round,
            miter_limit: 10.0,
            dash_pattern: Vec::new(),
            dash_phase: 0.0,
            color,
            alpha: 255,
        };
        out_paths.push(PaintPath::new(
            page_idx,
            segs,
            None,
            None,
            255,
            Some(stroke),
            identity_ctm(),
            base_z.saturating_add(i as u32),
        ));
    }
}

/// Convert a single PagePath from udoc-pdf into a PaintPath in page
/// user space. Extracted from the inline loop in the main convert path
/// so annotation appearance streams can reuse the same mapping.
fn pagepath_to_paintpath(page_idx: usize, pp: &udoc_pdf::PagePath, z_index: u32) -> PaintPath {
    let segments: Vec<PaintSegment> = pp
        .segments
        .iter()
        .map(|seg| match seg {
            udoc_pdf::PagePathSegmentKind::MoveTo { p } => PaintSegment::MoveTo { x: p.x, y: p.y },
            udoc_pdf::PagePathSegmentKind::LineTo { p } => PaintSegment::LineTo { x: p.x, y: p.y },
            udoc_pdf::PagePathSegmentKind::CurveTo { c1, c2, end } => PaintSegment::CurveTo {
                c1x: c1.x,
                c1y: c1.y,
                c2x: c2.x,
                c2y: c2.y,
                ex: end.x,
                ey: end.y,
            },
            udoc_pdf::PagePathSegmentKind::ClosePath => PaintSegment::ClosePath,
        })
        .collect();

    let fill = pp.fill.map(|f| match f {
        udoc_pdf::PathFillRule::NonZero => FillRule::NonZeroWinding,
        udoc_pdf::PathFillRule::EvenOdd => FillRule::EvenOdd,
    });

    let (fill_color, fill_alpha_u8) = match pp.fill_color {
        Some(udoc_pdf::PathColor::Rgb { r, g, b, a }) => (Some(Color::rgb(r, g, b)), a),
        None => (None, 255),
    };

    let stroke = pp.stroke.as_ref().map(|s| {
        let (color, alpha) = match s.color {
            udoc_pdf::PathColor::Rgb { r, g, b, a } => (Color::rgb(r, g, b), a),
        };
        PaintStroke {
            line_width: s.line_width,
            line_cap: match s.line_cap {
                udoc_pdf::LineCap::Butt => PaintLineCap::Butt,
                udoc_pdf::LineCap::Round => PaintLineCap::Round,
                udoc_pdf::LineCap::ProjectingSquare => PaintLineCap::ProjectingSquare,
            },
            line_join: match s.line_join {
                udoc_pdf::LineJoin::Miter => PaintLineJoin::Miter,
                udoc_pdf::LineJoin::Round => PaintLineJoin::Round,
                udoc_pdf::LineJoin::Bevel => PaintLineJoin::Bevel,
            },
            miter_limit: s.miter_limit,
            dash_pattern: s.dash_pattern.clone(),
            dash_phase: s.dash_phase,
            color,
            alpha,
        }
    });

    let m = &pp.ctm_at_paint;
    PaintPath::new(
        page_idx,
        segments,
        fill,
        fill_color,
        fill_alpha_u8,
        stroke,
        [m.a, m.b, m.c, m.d, m.e, m.f],
        z_index,
    )
}

/// Push the PagePaths produced by an interpreted /AP/N stream onto
/// `out_paths`. The paths are already composited into page user space
/// by `Page::interpret_annotation_appearance`.
fn emit_appearance_paintpaths(
    page_idx: usize,
    ap_paths: &[udoc_pdf::PagePath],
    base_z: u32,
    out_paths: &mut Vec<PaintPath>,
) {
    for (i, pp) in ap_paths.iter().enumerate() {
        // Re-z the paths onto the annotation's reserved z band so
        // they paint on top of the document content stream regardless
        // of the paint order they had inside the AP stream.
        let z = base_z.saturating_add(i as u32);
        out_paths.push(pagepath_to_paintpath(page_idx, pp, z));
    }
}

/// Convert annotation-appearance TextSpans into PositionedSpans on the
/// presentation overlay. Spans from `Page::interpret_annotation_appearance`
/// are already in page user space and carry core-typed fields (the
/// PdfPageExtractor runs the PDF->core conversion on its side).
///
/// Consumes `ap_spans` by value so owned fields (text, font_name,
/// char_advances, char_codes, char_gids, font_id, font_resolution,
/// glyph_bboxes) can be MOVED into PositionedSpan instead of cloned
/// ( follow-up). Annotation appearance is a per-call sink: the
/// caller drops `ap_spans` immediately after this returns.
fn emit_appearance_spans(
    page_idx: usize,
    ap_spans: Vec<udoc_core::text::TextSpan>,
    base_z: u32,
    out_spans: &mut Vec<PositionedSpan>,
) {
    out_spans.reserve(ap_spans.len());
    for span in ap_spans {
        // Build a bbox mirroring the main-stream conversion above. For
        // horizontal text the span occupies (x, y) to (x+width,
        // y+font_size); vertical text is rare inside AP streams so we
        // handle it the simple way.
        let bbox = if span.rotation.abs() > 45.0 && span.rotation.abs() < 135.0 {
            let text_height = if span.width > 0.0 {
                span.width
            } else {
                span.font_size * 0.6 * span.text.chars().count() as f64
            };
            BoundingBox::new(
                span.x - span.font_size,
                span.y,
                span.x,
                span.y + text_height,
            )
        } else {
            BoundingBox::new(span.x, span.y, span.x + span.width, span.y + span.font_size)
        };
        let mut ps = PositionedSpan::new(span.text, bbox, page_idx);
        ps.font_name = span.font_name;
        ps.font_size = Some(span.font_size);
        ps.is_bold = span.is_bold;
        ps.is_italic = span.is_italic;
        ps.color = span.color;
        ps.letter_spacing = span.letter_spacing;
        ps.is_superscript = span.is_superscript;
        ps.is_subscript = span.is_subscript;
        ps.char_advances = span.char_advances;
        ps.advance_scale = span.advance_scale;
        ps.char_codes = span.char_codes;
        ps.char_gids = span.char_gids;
        ps.rotation = span.rotation;
        ps.font_id = span.font_id;
        ps.font_resolution = span.font_resolution;
        ps.glyph_bboxes = span.glyph_bboxes;
        ps.z_index = base_z.saturating_add(span.z_index);
        out_spans.push(ps);
    }
}

// emit_appearance_paths used to be a placeholder that painted a colored
// border + centred text for Stamp/Watermark/FreeText annotations that
// did not carry an /AP/N stream. Real AP-stream interpretation is now
// wired in via `Page::interpret_annotation_appearance` so the fallback
// only fires for malformed annotations that declare a subtype needing
// an appearance but ship none; in that case we render nothing (matches
// mupdf behaviour for missing /AP).

/// Identity CTM for annotation-synthesised paths that live directly in
/// page user space.
fn identity_ctm() -> [f64; 6] {
    [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
}

/// Collect /QuadPoints as four-corner tuples in BL, BR, TR, TL order.
/// When /QuadPoints is absent or malformed, falls back to the annotation
/// /Rect.
fn collect_quads(ann: &udoc_pdf::PageAnnotation) -> Vec<[(f64, f64); 4]> {
    let mut out: Vec<[(f64, f64); 4]> = Vec::new();
    if ann.quad_points.len() >= 8 {
        for chunk in ann.quad_points.chunks_exact(8) {
            // PDF §12.5.6.10: points are (x1,y1)..(x4,y4) = UL UR LL LR.
            // MuPDF implementation and most viewers accept both that order
            // and the more common BL BR TR TL encoding. Detect empirically
            // by the Y-sort of the four points: lower Y pair = bottom edge.
            let p1 = (chunk[0], chunk[1]);
            let p2 = (chunk[2], chunk[3]);
            let p3 = (chunk[4], chunk[5]);
            let p4 = (chunk[6], chunk[7]);
            let mut pts = [p1, p2, p3, p4];
            pts.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            // pts[0..=1] lowest Y (bottom edge), pts[2..=3] highest Y.
            let (bl, br) = if pts[0].0 <= pts[1].0 {
                (pts[0], pts[1])
            } else {
                (pts[1], pts[0])
            };
            let (tl, tr) = if pts[2].0 <= pts[3].0 {
                (pts[2], pts[3])
            } else {
                (pts[3], pts[2])
            };
            out.push([bl, br, tr, tl]);
        }
    }
    if out.is_empty() {
        let r = ann.rect;
        out.push([
            (r.x_min, r.y_min),
            (r.x_max, r.y_min),
            (r.x_max, r.y_max),
            (r.x_min, r.y_max),
        ]);
    }
    out
}

fn normalize_block_text(block: &mut Block) {
    match block {
        Block::Heading { content, .. } | Block::Paragraph { content, .. } => {
            for inline in content.iter_mut() {
                normalize_inline_text(inline);
            }
        }
        Block::CodeBlock { text, .. } if needs_normalization(text) => {
            *text = normalize_for_document(text);
        }
        _ => {}
    }
}

fn normalize_inline_text(inline: &mut Inline) {
    match inline {
        Inline::Text { text, .. } | Inline::Code { text, .. } if needs_normalization(text) => {
            *text = normalize_for_document(text);
        }
        Inline::Link { content, .. } => {
            for child in content.iter_mut() {
                normalize_inline_text(child);
            }
        }
        _ => {}
    }
}

/// Walk a page's spans and return a [`Error::font_fallback_required`] the
/// first time a non-Exact [`FontResolution`] is observed. Returns `None`
/// when every span is Exact.
///
/// Invoked only by `pdf_to_document` when `Config::strict_fonts` is set;
/// the PDF backend is the only format that currently populates
/// `FontResolution` (see `Config::strict_fonts` doc for scope). The check
/// is deliberately early (before block/presentation conversion) so callers
/// fail fast with useful context instead of getting a half-materialised
/// Document.
fn strict_fonts_check(spans: &[udoc_core::text::TextSpan], page_idx: usize) -> Option<Error> {
    use udoc_core::text::FontResolution;
    for span in spans {
        match &span.font_resolution {
            FontResolution::Exact => continue,
            FontResolution::Substituted {
                requested, reason, ..
            }
            | FontResolution::SyntheticFallback {
                requested, reason, ..
            } => {
                return Some(
                    Error::font_fallback_required(requested.clone(), reason.clone())
                        .with_context(format!("extracting page {page_idx}")),
                );
            }
            // FontResolution is marked #[non_exhaustive]; any future
            // non-Exact variant should also trip strict mode. Construct
            // the same typed payload as the known-variant arms so callers
            // that downcast via `Error::font_fallback_info` keep working
            // across enum additions. See issue #203.
            _ => {
                use udoc_core::text::FallbackReason;
                let requested = span
                    .font_name
                    .clone()
                    .unwrap_or_else(|| "(unknown)".to_string());
                return Some(
                    Error::font_fallback_required(requested, FallbackReason::Unknown)
                        .with_context(format!("extracting page {page_idx}")),
                );
            }
        }
    }
    None
}

/// Check if all pages are filtered out by page range. If so, return an
/// empty Document with metadata only. Returns `None` if there are pages
/// in range (caller should proceed with conversion).
///
/// Works for both multi-page backends (where page_count > 1) and single-page
/// backends (where page_count == 1). The single-page case is just a special
/// case where page_count is always 1.
fn empty_doc_if_no_pages_in_range<B: FormatBackend>(
    backend: &B,
    config: &crate::Config,
) -> Option<Document> {
    if let Some(ref range) = config.page_range {
        let page_count = FormatBackend::page_count(backend);
        let any_in_range = (0..page_count).any(|i| range.contains(i));
        if !any_in_range {
            let mut doc = Document::new();
            doc.metadata = FormatBackend::metadata(backend);
            return Some(doc);
        }
    }
    None
}

/// Try to set a value in an overlay, emitting a diagnostic on failure.
/// Failures here indicate a bug (NodeId exceeds MAX_NODE_ID), so we
/// warn rather than silently dropping the data in release builds.
macro_rules! overlay_try_set {
    ($overlay:expr, $id:expr, $value:expr, $label:expr, $diagnostics:expr) => {
        if $overlay.try_set($id, $value).is_err() {
            $diagnostics.warning(Warning::new(
                "OverlaySetFailed",
                format!("overlay set failed for {} (NodeId {})", $label, $id),
            ));
        }
    };
}

/// Convert a PDF backend into the unified Document model.
///
/// Walks all pages via FormatBackend/PageExtractor, extracts text, tables,
/// and images, and builds the Block/Inline tree with Presentation overlay.
pub(crate) fn pdf_to_document(
    pdf: &mut udoc_pdf::Document,
    config: &crate::Config,
) -> Result<Document> {
    let raw_page_count = FormatBackend::page_count(pdf);
    let page_count = raw_page_count.min(config.limits.max_pages);
    let mut doc = Document::new();

    // Metadata: backend returns the same DocumentMetadata type, assign directly.
    doc.metadata = FormatBackend::metadata(pdf);

    let build_presentation = config.layers.presentation;
    let diagnostics = config.diagnostics.as_ref();

    let mut all_positioned_spans: Vec<PositionedSpan> = Vec::new();
    let mut all_shapes: Vec<PageShape> = Vec::new();
    let mut all_image_placements: Vec<ImagePlacement> = Vec::new();
    let mut all_paint_paths: Vec<PaintPath> = Vec::new();
    let mut all_shadings: Vec<PaintShading> = Vec::new();
    let mut all_patterns: Vec<PaintPattern> = Vec::new();
    let mut page_defs: Vec<PageDef> = Vec::new();
    let mut page_assignments: Overlay<usize> = Overlay::new();
    let mut geom: Overlay<BoundingBox> = Overlay::new();
    let mut text_styling: SparseOverlay<ExtendedTextStyle> = SparseOverlay::new();
    let mut seen_urls: HashSet<String> = HashSet::new();

    for page_idx in 0..page_count {
        // Check page range filter
        if let Some(ref range) = config.page_range {
            if !range.contains(page_idx) {
                continue;
            }
        }

        maybe_insert_page_break(&mut doc)?;

        // Single page open: extract everything in one content stream pass.
        let mut page = FormatBackend::page(pdf, page_idx)
            .map_err(|e| Error::with_source(format!("opening page {page_idx}"), e))?;

        // Get page bbox (needed for presentation layer and background image detection).
        // Use the PageExtractor trait method; fall back to US Letter (612x792) if the
        // backend has no page geometry.
        let page_bbox = PageExtractor::page_bbox(&mut page)
            .unwrap_or_else(|| BoundingBox::new(0.0, 0.0, 612.0, 792.0));
        if build_presentation {
            let rotation = PageExtractor::rotation(&mut page);
            page_defs.push(PageDef::with_origin(
                page_idx,
                page_bbox.width(),
                page_bbox.height(),
                rotation,
                page_bbox.x_min,
                page_bbox.y_min,
            ));
        }

        // Use extract_full only when we need tables/images. For text-only
        // extraction (the common case), use the lighter text_lines + raw_spans
        // path which skips image decompression and table detection (~35% faster).
        let need_tables = config.layers.tables;
        let need_images = config.layers.images;

        let (text_lines, raw_spans, tables, images) = if need_tables || need_images {
            let extracted = page.extract_full().map_err(|e| {
                Error::with_source(format!("extracting content from page {page_idx}"), e)
            })?;
            (
                extracted.text_lines,
                extracted.raw_spans,
                extracted.tables,
                extracted.images,
            )
        } else {
            // Text-only path: skip image decompression + table detection
            let tl = page.text_lines().map_err(|e| {
                Error::with_source(format!("extracting text from page {page_idx}"), e)
            })?;
            let rs = page.raw_spans().map_err(|e| {
                Error::with_source(format!("extracting spans from page {page_idx}"), e)
            })?;
            (tl, rs, Vec::new(), Vec::new())
        };

        // Strict-font dispatch: fail fast on the first non-Exact resolution
        // so callers get a clear error with the requested font + reason
        // rather than a silent fallback. Running before block conversion keeps
        // the error cheap and avoids building a half-materialised Document.
        if config.assets.strict_fonts {
            if let Some(err) = strict_fonts_check(&raw_spans, page_idx) {
                return Err(err);
            }
        }

        // Find the dominant font size (mode) for heading detection.
        let dominant_size = compute_dominant_font_size(&text_lines);

        // Collect all blocks with their y-position sort key, then sort by
        // y_top descending so blocks appear in top-to-bottom page order
        // (PDF coordinates have y increasing upward).
        let page_area = page_bbox.area();
        let mut page_blocks: Vec<(f64, Block)> =
            Vec::with_capacity(text_lines.len() + tables.len() + images.len());

        // Convert text lines to Block::Paragraph or Block::Heading.
        // Filter out invisible spans (PDF Tr=3, OCR overlays) from the
        // document model spine. Raw spans in the Presentation overlay
        // still include them for positional hook use.
        //
        // consume `text_lines` by value so each span's owned
        // `text` String moves into `Inline::Text` instead of cloning.
        // text_lines is not used after this loop. Per-span saves: 1 String
        // alloc + 1 Option<String> alloc for font_name on the presentation
        // overlay path.
        for line in text_lines {
            let mut visible_spans: Vec<udoc_core::text::TextSpan> =
                Vec::with_capacity(line.spans.len());
            visible_spans.extend(line.spans.into_iter().filter(|s| !s.is_invisible));
            if visible_spans.is_empty() {
                continue;
            }

            let avg_size: f64 =
                visible_spans.iter().map(|s| s.font_size).sum::<f64>() / visible_spans.len() as f64;

            let heading_level = infer_heading_level(avg_size, dominant_size);

            // y_top = max(y + font_size) across visible spans (top of highest glyph).
            let y_top = visible_spans
                .iter()
                .map(|s| s.y + s.font_size)
                .fold(f64::NEG_INFINITY, f64::max);

            // Build inline content from visible spans, MOVING owned fields
            // (text, font_name) out of each span instead of cloning.
            let inlines: Vec<Inline> = visible_spans
                .into_iter()
                .map(|span| {
                    let inline_id = alloc_id(&doc)?;
                    let mut style = SpanStyle::default();
                    style.bold = span.is_bold;
                    style.italic = span.is_italic;
                    style.superscript = span.is_superscript;
                    style.subscript = span.is_subscript;

                    if build_presentation {
                        let ext_style = ExtendedTextStyle::new()
                            .font_name(span.font_name.clone())
                            .font_size(Some(span.font_size))
                            .color(span.color)
                            .letter_spacing(span.letter_spacing);
                        overlay_try_set!(
                            text_styling,
                            inline_id,
                            ext_style,
                            "text_styling",
                            diagnostics
                        );
                        overlay_try_set!(
                            geom,
                            inline_id,
                            BoundingBox::new(
                                span.x,
                                span.y,
                                span.x + span.width,
                                span.y + span.font_size,
                            ),
                            "geometry",
                            diagnostics
                        );
                        overlay_try_set!(
                            page_assignments,
                            inline_id,
                            page_idx,
                            "page_assignment",
                            diagnostics
                        );
                    }

                    // NFC/soft-hyphen normalization is applied uniformly
                    // across all formats by `normalize_document_text`
                    // after the facade returns; see Extractor::into_document.
                    Ok(Inline::Text {
                        id: inline_id,
                        text: span.text,
                        style,
                    })
                })
                .collect::<Result<Vec<Inline>>>()?;

            let block_id = alloc_id(&doc)?;
            if build_presentation {
                overlay_try_set!(
                    page_assignments,
                    block_id,
                    page_idx,
                    "page_assignment",
                    diagnostics
                );
            }

            let block = if heading_level > 0 {
                Block::Heading {
                    id: block_id,
                    level: heading_level,
                    content: inlines,
                }
            } else {
                Block::Paragraph {
                    id: block_id,
                    content: inlines,
                }
            };
            page_blocks.push((y_top, block));
        }

        // Convert tables.
        for table in &tables {
            // Fallback to 0.0 (page bottom in PDF coords) for tables without
            // bbox. Least-bad default: better at the bottom than displacing
            // real content at the top.
            let y_top = table.bbox.map(|b| b.y_max).unwrap_or(0.0);

            let table_id = alloc_id(&doc)?;
            if build_presentation {
                overlay_try_set!(
                    page_assignments,
                    table_id,
                    page_idx,
                    "page_assignment",
                    diagnostics
                );
                if let Some(bbox) = table.bbox {
                    overlay_try_set!(geom, table_id, bbox, "geometry", diagnostics);
                }
            }

            let rows = convert_table_rows(&doc, table)?;

            let mut td = build_table_data(rows, table);
            td.may_continue_from_previous = table.may_continue_from_previous;
            td.may_continue_to_next = table.may_continue_to_next;

            page_blocks.push((
                y_top,
                Block::Table {
                    id: table_id,
                    table: td,
                },
            ));
        }

        // Convert images.
        for annotated in &images {
            let img = &annotated.image;
            let image_asset_index = doc.assets.images().len();
            let asset_ref = doc.assets.add_image(ImageData::from(img));

            // Full-page background images (bbox area > 80% of page area)
            // sort to end of page.
            let y_top = match img.bbox {
                Some(bbox) => {
                    if page_area > 0.0 && bbox.area() > page_area * 0.8 {
                        f64::NEG_INFINITY
                    } else {
                        bbox.y_max
                    }
                }
                // No bbox: sort to page bottom (same rationale as tables).
                None => 0.0,
            };

            let block_id = alloc_id(&doc)?;
            if build_presentation {
                overlay_try_set!(
                    page_assignments,
                    block_id,
                    page_idx,
                    "page_assignment",
                    diagnostics
                );
                if let Some(bbox) = img.bbox {
                    overlay_try_set!(geom, block_id, bbox, "geometry", diagnostics);
                    let mut ip = if let Some(ctm) = img.ctm {
                        ImagePlacement::with_ctm(
                            page_idx,
                            bbox,
                            image_asset_index,
                            img.width,
                            img.height,
                            img.color_space.clone(),
                            ctm,
                        )
                    } else {
                        ImagePlacement::new(
                            page_idx,
                            bbox,
                            image_asset_index,
                            img.width,
                            img.height,
                            img.color_space.clone(),
                        )
                    };
                    ip.z_index = img.z_index;
                    ip.is_mask = img.is_mask;
                    ip.mask_color = img.mask_color;
                    ip.soft_mask = img.soft_mask.clone();
                    ip.soft_mask_width = img.soft_mask_width;
                    ip.soft_mask_height = img.soft_mask_height;
                    all_image_placements.push(ip);
                }
            }

            page_blocks.push((
                y_top,
                Block::Image {
                    id: block_id,
                    image_ref: asset_ref,
                    alt_text: annotated.alt_text.clone(),
                },
            ));
        }

        // Stable sort by y_top descending (top of page first in PDF coords).
        page_blocks.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        for (_y_top, block) in page_blocks {
            doc.content.push(block);
        }

        // Collect raw spans for presentation layer. Consume `raw_spans`
        // by value so we can MOVE owned fields (text, font_name, char_*,
        // glyph_bboxes, font_id, font_resolution) into PositionedSpan
        // instead of cloning them. Each span has 8 owned fields -> per-span
        // savings are 8 allocations dropped ( follow-up). raw_spans
        // is not used past this loop, so consuming it is safe.
        if build_presentation {
            for span in raw_spans {
                // For rotated text (e.g., 90 degrees), the "width" is in the
                // vertical direction. Compute bbox accordingly.
                let bbox = if span.rotation.abs() > 45.0 && span.rotation.abs() < 135.0 {
                    // ~90 degrees CCW: text flows vertically upward from (x, y).
                    // The glyph ascender extends LEFT from the text cursor,
                    // so the bbox spans from (x - font_size, y) to (x, y + height).
                    //
                    // span.width is the natural advance length (Euclidean
                    // magnitude of the cursor displacement), which equals the
                    // visual height for vertical text. Falling back on
                    // `font_size * 0.6 * char_count` was a 0.6-em-per-char
                    // heuristic that systematically over-sized vertical bboxes
                    // and made the renderer stretch glyphs apart -- M-25
                    // diagnosed this as the dominant OCR-spacing bug on the
                    // alphabetical-100. Use the real width when present and
                    // only fall back to the heuristic when the interpreter
                    // couldn't compute a displacement (e.g. degenerate matrix).
                    let text_height = if span.width > 0.0 {
                        span.width
                    } else {
                        span.font_size * 0.6 * span.text.chars().count() as f64
                    };
                    BoundingBox::new(
                        span.x - span.font_size,
                        span.y,
                        span.x,
                        span.y + text_height,
                    )
                } else {
                    BoundingBox::new(span.x, span.y, span.x + span.width, span.y + span.font_size)
                };
                // `normalize_document_text` will NFC/soft-hyphen-normalize
                // this span at the facade boundary alongside every other
                // backend's presentation spans.
                let mut ps = PositionedSpan::new(span.text, bbox, page_idx);
                ps.font_name = span.font_name;
                ps.font_size = Some(span.font_size);
                ps.is_bold = span.is_bold;
                ps.is_italic = span.is_italic;
                ps.color = span.color;
                ps.letter_spacing = span.letter_spacing;
                ps.is_superscript = span.is_superscript;
                ps.is_subscript = span.is_subscript;
                ps.char_advances = span.char_advances;
                ps.advance_scale = span.advance_scale;
                ps.char_codes = span.char_codes;
                ps.char_gids = span.char_gids;
                ps.rotation = span.rotation;
                ps.z_index = span.z_index;
                ps.font_id = span.font_id;
                ps.font_resolution = span.font_resolution;
                ps.glyph_bboxes = span.glyph_bboxes;
                //  text-span clip plumbing is deferred: the facade's
                // core TextSpan type does not carry clip data yet and the
                // core-to-PositionedSpan bridge strips format-specific
                // clipping fields. Shapes carry `active_clips`, which
                // covers the common W/W* use case (column clips, watermark
                // clips, image-mask clips). Text-under-clip support lands
                // with the next round of TextSpan plumbing, tracked in the
                //  attack list.
                all_positioned_spans.push(ps);
            }

            // Extract path shapes (lines, rectangles) for rendering.
            //
            // We only populate the legacy `shapes` buffer with Line and Rect
            // kinds. Polygon shapes (filled curves + complex outlines) are
            // now rasterized by the newer PaintPath pipeline below, which
            // handles arbitrary cubic curves, winding rules, and real stroke
            // outline expansion. Emitting a Polygon here would double-draw
            // on top of the PaintPath rasterizer. Line + Rect remain because
            // the table-border AA helpers in the renderer produce crisper
            // rules than the generic polygon fill at sub-pixel widths.
            if let Ok(paths) = page.path_segments() {
                for seg in &paths {
                    let kind = match &seg.kind {
                        udoc_pdf::PathSegmentKind::Line { x1, y1, x2, y2 } => PathShapeKind::Line {
                            x1: *x1,
                            y1: *y1,
                            x2: *x2,
                            y2: *y2,
                        },
                        udoc_pdf::PathSegmentKind::Rect {
                            x,
                            y,
                            width,
                            height,
                        } => PathShapeKind::Rect {
                            x: *x,
                            y: *y,
                            width: *width,
                            height: *height,
                        },
                        // Polygon-shaped paths are now owned by the PaintPath
                        // pipeline. Skip here to avoid double-painting.
                        udoc_pdf::PathSegmentKind::Polygon { .. } => continue,
                        _ => continue,
                    };
                    let mut shape =
                        PageShape::new(page_idx, kind, seg.stroked, seg.filled, seg.line_width);
                    shape.stroke_color = Some(Color::rgb(
                        seg.stroke_color[0],
                        seg.stroke_color[1],
                        seg.stroke_color[2],
                    ));
                    shape.fill_color = Some(Color::rgb(
                        seg.fill_color[0],
                        seg.fill_color[1],
                        seg.fill_color[2],
                    ));
                    shape.z_index = seg.z_index;
                    shape.fill_alpha = seg.fill_alpha;
                    shape.stroke_alpha = seg.stroke_alpha;
                    // Wire active clip regions for .
                    shape.active_clips = seg
                        .active_clips
                        .iter()
                        .map(|c| ClipRegion {
                            subpaths: c.subpaths.clone(),
                            fill_rule: match c.fill_rule {
                                udoc_pdf::FillRule::NonZeroWinding => ClipRegionFillRule::NonZero,
                                udoc_pdf::FillRule::EvenOdd => ClipRegionFillRule::EvenOdd,
                            },
                        })
                        .collect();
                    all_shapes.push(shape);
                }
            }

            // Extract canonical paint-time paths (PagePath IR) for the
            // renderer. Carries CTM + stroke style + fill rule per paint
            // op. Consumed by `udoc-render`.
            //
            // Filter out paths that the legacy `shapes` pipeline already
            // renders crisply: pure axis-aligned rectangles and single line
            // segments. For those the AA-aware rect/line helpers in the
            // renderer match mupdf's sub-pixel Y AA better than a full
            // outline expansion of a 1-px stroke. Everything with a curve,
            // a non-rectangular polygon, or multi-segment stroked geometry
            // routes through the new PaintPath rasterizer.
            if let Ok((paint_paths, page_shadings, page_patterns)) =
                page.paths_shadings_and_patterns()
            {
                for tp in &page_patterns {
                    let m = &tp.ctm_at_paint;
                    let ctm = [m.a, m.b, m.c, m.d, m.e, m.f];
                    let fill_rule = match tp.fill_rule {
                        udoc_pdf::PathFillRule::NonZero => FillRule::NonZeroWinding,
                        udoc_pdf::PathFillRule::EvenOdd => FillRule::EvenOdd,
                    };
                    let fill_subpaths: Vec<Vec<(f64, f64)>> = tp
                        .fill_subpaths
                        .iter()
                        .map(|sub| sub.iter().map(|p| (p.x, p.y)).collect())
                        .collect();
                    let fallback_color = match tp.fallback_color {
                        udoc_pdf::PathColor::Rgb { r, g, b, .. } => Some(Color::rgb(r, g, b)),
                    };
                    let pattern = PaintPattern::new(
                        page_idx,
                        tp.resource_name.clone(),
                        tp.bbox,
                        tp.xstep,
                        tp.ystep,
                        tp.matrix,
                        tp.content_stream.clone(),
                        ctm,
                        tp.alpha,
                        tp.z as u32,
                    )
                    .with_fill_region(fill_subpaths, fill_rule, fallback_color);
                    all_patterns.push(pattern);
                }
                for sh in &page_shadings {
                    let m = &sh.ctm_at_paint;
                    let ctm = [m.a, m.b, m.c, m.d, m.e, m.f];
                    let kind = match sh.kind.clone() {
                        udoc_pdf::PageShadingKind::Axial {
                            p0,
                            p1,
                            lut,
                            extend_start,
                            extend_end,
                        } => PaintShadingKind::Axial {
                            p0x: p0.x,
                            p0y: p0.y,
                            p1x: p1.x,
                            p1y: p1.y,
                            samples: lut.samples,
                            extend_start,
                            extend_end,
                        },
                        udoc_pdf::PageShadingKind::Radial {
                            c0,
                            r0,
                            c1,
                            r1,
                            lut,
                            extend_start,
                            extend_end,
                        } => PaintShadingKind::Radial {
                            c0x: c0.x,
                            c0y: c0.y,
                            r0,
                            c1x: c1.x,
                            c1y: c1.y,
                            r1,
                            samples: lut.samples,
                            extend_start,
                            extend_end,
                        },
                        udoc_pdf::PageShadingKind::Unsupported { shading_type } => {
                            PaintShadingKind::Unsupported { shading_type }
                        }
                        _ => PaintShadingKind::Unsupported { shading_type: 0 },
                    };
                    all_shadings.push(PaintShading::new(
                        page_idx,
                        kind,
                        ctm,
                        sh.alpha,
                        sh.z as u32,
                    ));
                }
                for pp in &paint_paths {
                    if path_is_simple_rect_or_line(&pp.segments) {
                        // Already covered by the shape pipeline above.
                        continue;
                    }
                    let segments: Vec<PaintSegment> = pp
                        .segments
                        .iter()
                        .map(|seg| match seg {
                            udoc_pdf::PagePathSegmentKind::MoveTo { p } => {
                                PaintSegment::MoveTo { x: p.x, y: p.y }
                            }
                            udoc_pdf::PagePathSegmentKind::LineTo { p } => {
                                PaintSegment::LineTo { x: p.x, y: p.y }
                            }
                            udoc_pdf::PagePathSegmentKind::CurveTo { c1, c2, end } => {
                                PaintSegment::CurveTo {
                                    c1x: c1.x,
                                    c1y: c1.y,
                                    c2x: c2.x,
                                    c2y: c2.y,
                                    ex: end.x,
                                    ey: end.y,
                                }
                            }
                            udoc_pdf::PagePathSegmentKind::ClosePath => PaintSegment::ClosePath,
                        })
                        .collect();

                    let fill = pp.fill.map(|f| match f {
                        udoc_pdf::PathFillRule::NonZero => FillRule::NonZeroWinding,
                        udoc_pdf::PathFillRule::EvenOdd => FillRule::EvenOdd,
                    });

                    let (fill_color, fill_alpha_u8) = match pp.fill_color {
                        Some(udoc_pdf::PathColor::Rgb { r, g, b, a }) => {
                            (Some(Color::rgb(r, g, b)), a)
                        }
                        None => (None, 255),
                    };

                    let stroke = pp.stroke.as_ref().map(|s| {
                        let (color, alpha) = match s.color {
                            udoc_pdf::PathColor::Rgb { r, g, b, a } => (Color::rgb(r, g, b), a),
                        };
                        PaintStroke {
                            line_width: s.line_width,
                            line_cap: match s.line_cap {
                                udoc_pdf::LineCap::Butt => PaintLineCap::Butt,
                                udoc_pdf::LineCap::Round => PaintLineCap::Round,
                                udoc_pdf::LineCap::ProjectingSquare => {
                                    PaintLineCap::ProjectingSquare
                                }
                            },
                            line_join: match s.line_join {
                                udoc_pdf::LineJoin::Miter => PaintLineJoin::Miter,
                                udoc_pdf::LineJoin::Round => PaintLineJoin::Round,
                                udoc_pdf::LineJoin::Bevel => PaintLineJoin::Bevel,
                            },
                            miter_limit: s.miter_limit,
                            dash_pattern: s.dash_pattern.clone(),
                            dash_phase: s.dash_phase,
                            color,
                            alpha,
                        }
                    });

                    let m = &pp.ctm_at_paint;
                    all_paint_paths.push(PaintPath::new(
                        page_idx,
                        segments,
                        fill,
                        fill_color,
                        fill_alpha_u8,
                        stroke,
                        [m.a, m.b, m.c, m.d, m.e, m.f],
                        pp.z as u32,
                    ));
                }
            }
        }

        // Extract renderable annotations.
        // Each emitted PaintPath is in page user space so the existing
        // renderer pipeline composites them as-is.
        let annotations = page.annotations();
        if !annotations.is_empty() {
            // Pick a z_index that puts annotations above all existing
            // content. The content interpreter uses small z values per
            // paint op; annotations logically layer on top per PDF spec
            // (§12.5 drawn after the page content).
            let base_z: u32 = u32::MAX / 2;
            for (i, ann) in annotations.iter().enumerate() {
                if ann.is_hidden() {
                    continue;
                }
                let z_start = base_z.saturating_add(i as u32 * 16);
                // For subtypes that carry a pre-rendered /AP/N appearance
                // (Stamp / Watermark / FreeText / OtherWithAppearance)
                // we ask the PDF crate to interpret the AP content
                // stream with the §12.5.5 composite applied, and thread
                // the resulting PagePaths + TextSpans through the same
                // PaintPath / PositionedSpan pipeline the main content
                // stream uses.
                let has_ap = ann.ap_stream.is_some();
                if has_ap
                    && matches!(
                        ann.kind,
                        udoc_pdf::PageAnnotationKind::Stamp
                            | udoc_pdf::PageAnnotationKind::Watermark
                            | udoc_pdf::PageAnnotationKind::FreeText
                            | udoc_pdf::PageAnnotationKind::Link
                            | udoc_pdf::PageAnnotationKind::OtherWithAppearance
                    )
                {
                    let (ap_paths, ap_spans) = page.interpret_annotation_appearance(ann);
                    emit_appearance_paintpaths(page_idx, &ap_paths, z_start, &mut all_paint_paths);
                    emit_appearance_spans(page_idx, ap_spans, z_start, &mut all_positioned_spans);
                    // Suppress the standalone /Border stroke for /Link
                    // annotations even when AP-equipped: the appearance
                    // stream already encodes any visible border, and
                    // double-drawing produced colour fringing on
                    // arxiv-physics docs ( item 2).
                } else {
                    emit_annotation_paths(
                        page_idx,
                        ann,
                        z_start,
                        &mut all_paint_paths,
                        &mut all_positioned_spans,
                    );
                }
            }
        }

        // Extract hyperlinks from /Link annotations on this page.
        // Store in the relationships overlay only -- annotations are geometric
        // overlay data, not document content, so no content spine nodes are created.
        let page_links = page.links();
        if !page_links.is_empty() {
            let rels = doc.relationships.get_or_insert_with(Relationships::default);
            for link in page_links {
                if !link.url.is_empty()
                    && seen_urls.insert(link.url.clone())
                    && !rels.add_hyperlink(link.url.clone())
                {
                    break;
                }
            }
        }
    }

    // Extract bookmarks from /Outlines.
    let bookmark_entries = pdf.bookmarks();
    if !bookmark_entries.is_empty() {
        let rels = doc.relationships.get_or_insert_with(Relationships::default);
        const MAX_FLATTEN_DEPTH: usize = 64;
        fn flatten_bookmarks(
            entries: &[udoc_pdf::BookmarkEntry],
            rels: &mut Relationships,
            depth: usize,
        ) {
            if depth >= MAX_FLATTEN_DEPTH {
                return;
            }
            for entry in entries {
                if !entry.title.is_empty() {
                    let result = rels.add_bookmark(entry.title.clone(), BookmarkTarget::Positional);
                    if result == BookmarkAddResult::LimitReached {
                        return;
                    }
                }
                flatten_bookmarks(&entry.children, rels, depth + 1);
            }
        }
        flatten_bookmarks(&bookmark_entries, rels, 0);
    }

    // Build presentation layer if requested.
    if build_presentation {
        let mut pres = Presentation::default();
        pres.pages = page_defs;
        pres.page_assignments = page_assignments;
        pres.geometry = geom;
        pres.text_styling = text_styling;
        pres.raw_spans = all_positioned_spans;
        pres.shapes = all_shapes;
        pres.image_placements = all_image_placements;
        pres.paint_paths = all_paint_paths;
        pres.shadings = all_shadings;
        pres.patterns = all_patterns;
        doc.presentation = Some(pres);
    }

    // Collect embedded font programs for rendering (if fonts requested).
    if config.assets.fonts {
        // Iterate in sorted name order so the AssetStore is populated
        // deterministically. font_programs() returns a HashMap whose
        // iteration order varies run-to-run with the random hasher seed,
        // which would leak into FontCache lookups (when two fonts share
        // a display name, find() returns the first-pushed entry).
        let mut font_programs: Vec<_> = pdf.font_programs().into_iter().collect();
        font_programs.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (name, (data, program, enc_map, cid_widths)) in font_programs {
            let program_type = match program {
                udoc_font::types::FontProgram::TrueType => {
                    udoc_core::document::assets::FontProgramType::TrueType
                }
                udoc_font::types::FontProgram::Cff => {
                    udoc_core::document::assets::FontProgramType::Cff
                }
                udoc_font::types::FontProgram::Type1 => {
                    udoc_core::document::assets::FontProgramType::Type1
                }
                udoc_font::types::FontProgram::None => continue,
            };
            doc.assets.add_font(
                udoc_core::document::assets::FontAsset::with_encoding(
                    name,
                    data,
                    program_type,
                    enc_map,
                )
                .with_cid_widths(cid_widths),
            );
        }
    }

    // Extract Type3 font outlines for rendering.
    if config.assets.fonts {
        let type3_outlines = pdf.type3_font_outlines();
        for (font_name, unicode_char, outline_data) in type3_outlines {
            let asset_name = format!("type3:{}:U+{:04X}", font_name, unicode_char as u32);
            doc.assets
                .add_font(udoc_core::document::assets::FontAsset::new(
                    asset_name,
                    outline_data,
                    udoc_core::document::assets::FontProgramType::Type3,
                ));
        }
    }

    // Strip non-content overlays when disabled. Relationships (hyperlinks,
    // bookmarks) are always extracted above for simplicity; strip post-hoc.
    if !config.layers.relationships {
        doc.relationships = None;
    }
    if !config.layers.interactions {
        doc.interactions = None;
    }

    Ok(doc)
}

/// Generate a thin converter wrapper that handles page range filtering and
/// max_pages enforcement, then delegates to the backend crate's conversion
/// function ( pattern).
macro_rules! define_backend_converter {
    ($fn_name:ident, $backend_ty:ty, $convert_fn:path) => {
        pub(crate) fn $fn_name(
            backend: &mut $backend_ty,
            config: &crate::Config,
        ) -> Result<Document> {
            if let Some(doc) = empty_doc_if_no_pages_in_range(backend, config) {
                return Ok(doc);
            }
            let mut doc = $convert_fn(
                backend,
                config.diagnostics.as_ref(),
                config.limits.max_pages,
            )?;
            // Hyperlinks are now registered inline during conversion (#142),
            // so the facade no longer walks the finished tree. See
            // `udoc_core::convert::register_hyperlink`.
            //
            // Strip overlay data when content_only is requested.
            // Backends build overlays inline during conversion, so we
            // remove them post-hoc rather than threading flags through
            // every backend signature.
            if !config.layers.presentation {
                doc.presentation = None;
            }
            if !config.layers.relationships {
                doc.relationships = None;
            }
            if !config.layers.interactions {
                doc.interactions = None;
            }
            Ok(doc)
        }
    };
}

define_backend_converter!(
    rtf_to_document,
    udoc_rtf::RtfDocument,
    udoc_rtf::rtf_to_document
);
define_backend_converter!(
    ppt_to_document,
    udoc_ppt::PptDocument,
    udoc_ppt::ppt_to_document
);
define_backend_converter!(
    doc_to_document,
    udoc_doc::DocDocument,
    udoc_doc::doc_to_document
);
define_backend_converter!(
    pptx_to_document,
    udoc_pptx::PptxDocument,
    udoc_pptx::pptx_to_document
);
define_backend_converter!(
    docx_to_document,
    udoc_docx::DocxDocument,
    udoc_docx::docx_to_document
);

/// Convert a Markdown backend into the unified Document model.
///
/// Walks the parsed AST directly instead of going through PageExtractor's
/// text_lines() flattening, which preserves heading levels, code block
/// language tags, list structure, and blockquote nesting.
pub(crate) fn md_to_document(
    md: &mut udoc_markdown::MdDocument,
    config: &crate::Config,
) -> Result<Document> {
    use udoc_markdown::MdBlock;

    if let Some(doc) = empty_doc_if_no_pages_in_range(md, config) {
        return Ok(doc);
    }

    let mut doc = Document::new();
    doc.metadata = FormatBackend::metadata(md);

    for (kind, msg) in md.warnings() {
        config
            .diagnostics
            .warning(Warning::new(kind.as_str(), msg.as_str()));
    }

    // Dedup set for hyperlink URLs collected during conversion (#142).
    let mut hyperlink_seen: HashSet<String> = HashSet::new();

    fn convert_md_blocks(
        doc: &Document,
        blocks: &[MdBlock],
        out: &mut Vec<Block>,
        seen_urls: &mut HashSet<String>,
    ) -> Result<()> {
        for block in blocks {
            match block {
                MdBlock::Heading { level, content } => {
                    let inlines = convert_md_inlines(doc, content, seen_urls)?;
                    if !inlines.is_empty() {
                        let id = alloc_id(doc)?;
                        out.push(Block::Heading {
                            id,
                            level: *level,
                            content: inlines,
                        });
                    }
                }
                MdBlock::Paragraph { content } => {
                    let inlines = convert_md_inlines(doc, content, seen_urls)?;
                    if !inlines.is_empty() {
                        let id = alloc_id(doc)?;
                        out.push(Block::Paragraph {
                            id,
                            content: inlines,
                        });
                    }
                }
                MdBlock::CodeBlock { text, language } => {
                    let id = alloc_id(doc)?;
                    out.push(Block::CodeBlock {
                        id,
                        text: text.clone(),
                        language: language.clone(),
                    });
                }
                MdBlock::ThematicBreak => {
                    let id = alloc_id(doc)?;
                    out.push(Block::ThematicBreak { id });
                }
                MdBlock::List {
                    items,
                    ordered,
                    start,
                } => {
                    let id = alloc_id(doc)?;
                    let kind = if *ordered {
                        ListKind::Ordered
                    } else {
                        ListKind::Unordered
                    };
                    let mut list_items = Vec::with_capacity(items.len());
                    for item in items {
                        let item_id = alloc_id(doc)?;
                        let mut item_blocks = Vec::with_capacity(item.content.len());
                        convert_md_blocks(doc, &item.content, &mut item_blocks, seen_urls)?;
                        list_items.push(ListItem::new(item_id, item_blocks));
                    }
                    out.push(Block::List {
                        id,
                        items: list_items,
                        kind,
                        start: *start,
                    });
                }
                MdBlock::Table {
                    header,
                    rows,
                    col_count,
                } => {
                    let table_id = alloc_id(doc)?;
                    // +1 for the header row
                    let mut doc_rows = Vec::with_capacity(rows.len() + 1);

                    // Header row.
                    let row_id = alloc_id(doc)?;
                    let cells = md_table_row_to_cells(doc, header, *col_count, seen_urls)?;
                    let mut tr = TableRow::new(row_id, cells);
                    tr.is_header = true;
                    doc_rows.push(tr);

                    // Data rows.
                    for row in rows {
                        let row_id = alloc_id(doc)?;
                        let cells = md_table_row_to_cells(doc, row, *col_count, seen_urls)?;
                        doc_rows.push(TableRow::new(row_id, cells));
                    }

                    let mut td = TableData::new(doc_rows);
                    td.num_columns = *col_count;
                    td.header_row_count = 1;
                    out.push(Block::Table {
                        id: table_id,
                        table: td,
                    });
                }
                MdBlock::Blockquote { children } => {
                    let id = alloc_id(doc)?;
                    let mut child_blocks = Vec::new();
                    convert_md_blocks(doc, children, &mut child_blocks, seen_urls)?;
                    out.push(Block::Section {
                        id,
                        role: Some(SectionRole::Blockquote),
                        children: child_blocks,
                    });
                }
                MdBlock::Image { alt, .. } => {
                    // Block-level images in Markdown are URL references, not
                    // embedded data. Emit the alt text as a paragraph.
                    if !alt.is_empty() {
                        let block_id = alloc_id(doc)?;
                        let text_id = alloc_id(doc)?;
                        out.push(Block::Paragraph {
                            id: block_id,
                            content: vec![Inline::Text {
                                id: text_id,
                                text: alt.clone(),
                                style: SpanStyle::default(),
                            }],
                        });
                    }
                }
            }
        }
        Ok(())
    }

    let mut blocks = Vec::new();
    convert_md_blocks(&doc, md.blocks(), &mut blocks, &mut hyperlink_seen)?;
    doc.content = blocks;

    // Wire deduplicated URLs collected during conversion (#142). Single pass:
    // no tree walk needed.
    if !hyperlink_seen.is_empty() {
        let rels = doc.relationships.get_or_insert_with(Relationships::default);
        for url in hyperlink_seen {
            if !rels.add_hyperlink(url) {
                break;
            }
        }
    }

    Ok(doc)
}

/// Convert MdInline elements to document model Inline elements.
///
/// Hyperlink URLs are collected into `seen_urls` during conversion so the
/// facade does not need a post-hoc tree walk (#142). The caller wires the
/// deduped set into `Relationships::hyperlinks` once conversion completes.
fn convert_md_inlines(
    doc: &Document,
    inlines: &[udoc_markdown::MdInline],
    seen_urls: &mut HashSet<String>,
) -> Result<Vec<Inline>> {
    use udoc_markdown::MdInline;

    let mut result = Vec::with_capacity(inlines.len());
    for inline in inlines {
        match inline {
            MdInline::Text {
                text,
                bold,
                italic,
                strikethrough,
            } => {
                if !text.is_empty() {
                    let id = alloc_id(doc)?;
                    let mut style = SpanStyle::default();
                    style.bold = *bold;
                    style.italic = *italic;
                    style.strikethrough = *strikethrough;
                    result.push(Inline::Text {
                        id,
                        text: text.clone(),
                        style,
                    });
                }
            }
            MdInline::Code { text } => {
                if !text.is_empty() {
                    let id = alloc_id(doc)?;
                    result.push(Inline::Code {
                        id,
                        text: text.clone(),
                    });
                }
            }
            MdInline::Link { url, content } => {
                let id = alloc_id(doc)?;
                let children = convert_md_inlines(doc, content, seen_urls)?;
                if !url.is_empty() {
                    seen_urls.insert(url.clone());
                }
                result.push(Inline::Link {
                    id,
                    url: url.clone(),
                    content: children,
                });
            }
            MdInline::Image { alt, .. } => {
                if !alt.is_empty() {
                    let id = alloc_id(doc)?;
                    result.push(Inline::Text {
                        id,
                        text: alt.clone(),
                        style: SpanStyle::default(),
                    });
                }
            }
            MdInline::SoftBreak => {
                let id = alloc_id(doc)?;
                result.push(Inline::SoftBreak { id });
            }
            MdInline::LineBreak => {
                let id = alloc_id(doc)?;
                result.push(Inline::LineBreak { id });
            }
        }
    }
    Ok(result)
}

/// Convert a Markdown table row (Vec of cell inlines) into document model
/// TableCells, normalizing to `col_count` columns.
fn md_table_row_to_cells(
    doc: &Document,
    cells: &[Vec<udoc_markdown::MdInline>],
    col_count: usize,
    seen_urls: &mut HashSet<String>,
) -> Result<Vec<TableCell>> {
    let mut doc_cells = Vec::with_capacity(col_count);
    for cell in cells.iter().take(col_count) {
        let cell_id = alloc_id(doc)?;
        let para_id = alloc_id(doc)?;
        let inlines = convert_md_inlines(doc, cell, seen_urls)?;
        let content = vec![Block::Paragraph {
            id: para_id,
            content: inlines,
        }];
        doc_cells.push(TableCell::new(cell_id, content));
    }
    // Pad with empty cells if row is short.
    while doc_cells.len() < col_count {
        let cell_id = alloc_id(doc)?;
        let para_id = alloc_id(doc)?;
        doc_cells.push(TableCell::new(
            cell_id,
            vec![Block::Paragraph {
                id: para_id,
                content: vec![],
            }],
        ));
    }
    Ok(doc_cells)
}

/// Determine heading level from font size relative to the dominant size.
/// Returns 0 for body text, 1-3 for headings.
fn infer_heading_level(avg_size: f64, dominant_size: f64) -> u8 {
    if dominant_size <= 0.0 {
        return 0;
    }
    if avg_size > dominant_size * 1.8 {
        1
    } else if avg_size > dominant_size * 1.5 {
        2
    } else if avg_size > dominant_size * 1.2 {
        3
    } else {
        0
    }
}

/// Find the most common (mode) font size across all spans in the text lines.
/// Returns 12.0 as a fallback if no spans are present.
fn compute_dominant_font_size(text_lines: &[udoc_core::text::TextLine]) -> f64 {
    let mut size_counts: HashMap<i32, usize> = HashMap::with_capacity(32);
    for line in text_lines {
        for span in &line.spans {
            // Skip invisible spans (PDF Tr=3, OCR overlays) so they
            // don't skew heading inference on scanned/overlaid PDFs.
            if span.is_invisible {
                continue;
            }
            // Skip zero/near-zero font sizes from malformed PDFs so they
            // don't win the mode contest on sparse pages.
            if span.font_size < 0.5 {
                continue;
            }
            // Quantize to tenths of a point to group similar sizes
            *size_counts
                .entry((span.font_size * 10.0).round() as i32)
                .or_default() += 1;
        }
    }
    if size_counts.is_empty() {
        return 12.0;
    }
    let dominant_key = size_counts
        .into_iter()
        .max_by_key(|&(_, count)| count)
        .map(|(key, _)| key)
        .unwrap_or(120);
    dominant_key as f64 / 10.0
}

define_backend_converter!(
    xlsx_to_document,
    udoc_xlsx::XlsxDocument,
    udoc_xlsx::xlsx_to_document
);
define_backend_converter!(
    xls_to_document,
    udoc_xls::XlsDocument,
    udoc_xls::xls_to_document
);
define_backend_converter!(
    odt_to_document,
    udoc_odf::OdfDocument,
    udoc_odf::odf_to_document
);
define_backend_converter!(
    ods_to_document,
    udoc_odf::OdfDocument,
    udoc_odf::odf_to_document
);
define_backend_converter!(
    odp_to_document,
    udoc_odf::OdfDocument,
    udoc_odf::odf_to_document
);

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::text::{TextLine, TextSpan};

    #[test]
    fn strip_soft_hyphens_removes_u00ad() {
        assert_eq!(strip_soft_hyphens("hello"), "hello");
        assert_eq!(strip_soft_hyphens("hel\u{00AD}lo"), "hello");
        assert_eq!(strip_soft_hyphens("\u{00AD}\u{00AD}"), "");
        // Non-ASCII content unaffected
        assert_eq!(strip_soft_hyphens("a\u{00AD}b\u{00AD}c"), "abc");
        // Other format chars (zero-width joiner, zero-width space) preserved
        assert_eq!(strip_soft_hyphens("a\u{200B}b"), "a\u{200B}b");
    }

    #[test]
    fn nfc_normalizes_combining_acute() {
        // "cafe" + U+0301 COMBINING ACUTE ACCENT -> precomposed "café"
        assert_eq!(normalize_for_document("cafe\u{0301}"), "café");
    }

    #[test]
    fn nfc_normalizes_hangul_jamo() {
        // U+1100 HANGUL CHOSEONG KIYEOK + U+1161 HANGUL JUNGSEONG A
        // -> U+AC00 HANGUL SYLLABLE GA (precomposed)
        assert_eq!(normalize_for_document("\u{1100}\u{1161}"), "\u{AC00}");
    }

    #[test]
    fn nfc_preserves_ascii() {
        assert_eq!(normalize_for_document("hello"), "hello");
    }

    #[test]
    fn nfc_preserves_already_composed() {
        // Already-precomposed "café" (single U+00E9) round-trips unchanged.
        assert_eq!(normalize_for_document("café"), "café");
    }

    #[test]
    fn soft_hyphen_and_nfc_compose_in_order() {
        // Soft hyphen is stripped, then the remaining "e" + combining acute
        // is composed into U+00E9.
        assert_eq!(normalize_for_document("ca\u{00AD}fe\u{0301}"), "café");
    }

    #[test]
    fn needs_normalization_detects_only_interesting_strings() {
        // ASCII: cheap early-out (neither soft-hyphen nor non-ASCII).
        assert!(!needs_normalization("hello"));
        assert!(!needs_normalization(""));
        // Soft hyphen triggers, even if surrounding text is ASCII.
        assert!(needs_normalization("hel\u{00AD}lo"));
        // Any non-ASCII byte triggers (covers combining marks and pre-composed).
        assert!(needs_normalization("café"));
        assert!(needs_normalization("cafe\u{0301}"));
    }

    #[test]
    fn normalize_document_text_rewrites_blocks_and_spans() {
        use udoc_core::document::{Block, Inline, NodeId, PositionedSpan, Presentation, SpanStyle};
        use udoc_core::geometry::BoundingBox;

        let mut doc = Document::new();

        // Paragraph with a decomposed + soft-hyphen Text inline.
        let para_id = NodeId::new(1);
        let text_id = NodeId::new(2);
        let code_id = NodeId::new(3);
        doc.content.push(Block::Paragraph {
            id: para_id,
            content: vec![
                Inline::Text {
                    id: text_id,
                    text: "ca\u{00AD}fe\u{0301}".into(),
                    style: SpanStyle::default(),
                },
                Inline::Code {
                    id: code_id,
                    text: "e\u{0301}".into(),
                },
            ],
        });

        // Heading containing a Link whose child needs normalization.
        let heading_id = NodeId::new(4);
        let link_id = NodeId::new(5);
        let link_child_id = NodeId::new(6);
        doc.content.push(Block::Heading {
            id: heading_id,
            level: 1,
            content: vec![Inline::Link {
                id: link_id,
                url: "https://example.com".into(),
                content: vec![Inline::Text {
                    id: link_child_id,
                    text: "fe\u{0301}".into(),
                    style: SpanStyle::default(),
                }],
            }],
        });

        // Code block with combining marks.
        let code_block_id = NodeId::new(7);
        doc.content.push(Block::CodeBlock {
            id: code_block_id,
            text: "a\u{0301}b".into(),
            language: None,
        });

        // Presentation overlay span with a decomposed + soft-hyphen string.
        let mut presentation = Presentation::default();
        presentation.raw_spans.push(PositionedSpan::new(
            "ca\u{00AD}fe\u{0301}".into(),
            BoundingBox::new(0.0, 0.0, 10.0, 10.0),
            0,
        ));
        doc.presentation = Some(presentation);

        normalize_document_text(&mut doc);

        // Paragraph inlines composed and soft-hyphen stripped.
        let Block::Paragraph { content, .. } = &doc.content[0] else {
            panic!("paragraph expected");
        };
        match &content[0] {
            Inline::Text { text, .. } => assert_eq!(text, "café"),
            other => panic!("expected Inline::Text, got {other:?}"),
        }
        match &content[1] {
            Inline::Code { text, .. } => assert_eq!(text, "é"),
            other => panic!("expected Inline::Code, got {other:?}"),
        }

        // Link child also normalized.
        let Block::Heading { content, .. } = &doc.content[1] else {
            panic!("heading expected");
        };
        let Inline::Link {
            content: link_content,
            ..
        } = &content[0]
        else {
            panic!("link expected");
        };
        match &link_content[0] {
            Inline::Text { text, .. } => assert_eq!(text, "fé"),
            other => panic!("expected Inline::Text in link, got {other:?}"),
        }

        // CodeBlock text normalized.
        let Block::CodeBlock { text, .. } = &doc.content[2] else {
            panic!("code block expected");
        };
        assert_eq!(text, "áb");

        // Presentation span normalized.
        let span = &doc.presentation.as_ref().unwrap().raw_spans[0];
        assert_eq!(span.text, "café");
    }

    #[test]
    fn dominant_font_size_empty() {
        assert!((compute_dominant_font_size(&[]) - 12.0).abs() < f64::EPSILON);
    }

    #[test]
    fn dominant_font_size_single() {
        let lines = vec![TextLine::new(
            vec![TextSpan::new("hello".into(), 0.0, 0.0, 50.0, 10.0)],
            0.0,
            false,
        )];
        assert!((compute_dominant_font_size(&lines) - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn dominant_font_size_mode() {
        // 3 spans at 12pt, 1 span at 18pt -> mode is 12pt
        let lines = vec![TextLine::new(
            vec![
                TextSpan::new("a".into(), 0.0, 0.0, 10.0, 12.0),
                TextSpan::new("b".into(), 10.0, 0.0, 10.0, 12.0),
                TextSpan::new("c".into(), 20.0, 0.0, 10.0, 12.0),
                TextSpan::new("BIG".into(), 30.0, 0.0, 20.0, 18.0),
            ],
            0.0,
            false,
        )];
        assert!((compute_dominant_font_size(&lines) - 12.0).abs() < f64::EPSILON);
    }

    #[test]
    fn heading_level_inference() {
        let dominant = 12.0;
        // 2x dominant -> level 1
        assert_eq!(infer_heading_level(24.0, dominant), 1);
        // 1.6x -> level 2
        assert_eq!(infer_heading_level(19.2, dominant), 2);
        // 1.3x -> level 3
        assert_eq!(infer_heading_level(15.6, dominant), 3);
        // same size -> body text
        assert_eq!(infer_heading_level(12.0, dominant), 0);
        // slightly larger but under threshold -> body
        assert_eq!(infer_heading_level(14.0, dominant), 0);
    }

    #[test]
    fn text_lines_to_paragraphs() {
        // Equal font sizes => all paragraphs, no headings.
        let lines = vec![
            TextLine::new(
                vec![TextSpan::new("Line one".into(), 0.0, 100.0, 80.0, 12.0)],
                100.0,
                false,
            ),
            TextLine::new(
                vec![TextSpan::new("Line two".into(), 0.0, 88.0, 80.0, 12.0)],
                88.0,
                false,
            ),
        ];

        let dominant = compute_dominant_font_size(&lines);
        assert!((dominant - 12.0).abs() < f64::EPSILON);

        for line in &lines {
            let avg_size: f64 =
                line.spans.iter().map(|s| s.font_size).sum::<f64>() / line.spans.len() as f64;
            assert_eq!(infer_heading_level(avg_size, dominant), 0);
        }
    }

    #[test]
    fn table_conversion_structure() {
        // Build a document model table from a core Table and verify structure.
        let core_table = udoc_core::table::Table::new(
            vec![udoc_core::table::TableRow::with_header(
                vec![
                    udoc_core::table::TableCell::new("Header A".into(), None),
                    udoc_core::table::TableCell::new("Header B".into(), None),
                ],
                true,
            )],
            None,
        );

        let doc = Document::new();
        let table_id = doc.alloc_node_id();
        let rows: Vec<TableRow> = core_table
            .rows
            .iter()
            .map(|row| {
                let row_id = doc.alloc_node_id();
                let cells: Vec<TableCell> = row
                    .cells
                    .iter()
                    .map(|cell| {
                        let cell_id = doc.alloc_node_id();
                        let para_id = doc.alloc_node_id();
                        let text_id = doc.alloc_node_id();
                        let content = vec![Block::Paragraph {
                            id: para_id,
                            content: vec![Inline::Text {
                                id: text_id,
                                text: cell.text.clone(),
                                style: SpanStyle::default(),
                            }],
                        }];
                        TableCell::new(cell_id, content)
                    })
                    .collect();
                let mut tr = TableRow::new(row_id, cells);
                tr.is_header = row.is_header;
                tr
            })
            .collect();

        let td = build_table_data(rows, &core_table);
        let block = Block::Table {
            id: table_id,
            table: td,
        };

        let Block::Table { table, .. } = &block else {
            unreachable!("expected Table block, got {:?}", block);
        };
        assert_eq!(table.rows.len(), 1);
        assert!(table.rows[0].is_header);
        assert_eq!(table.rows[0].cells.len(), 2);
        assert_eq!(table.rows[0].cells[0].content.len(), 1);
        assert_eq!(block.text(), "Header A\tHeader B");
    }

    // -----------------------------------------------------------------------
    // Zero-font-size guard in compute_dominant_font_size
    // -----------------------------------------------------------------------

    #[test]
    fn dominant_font_size_skips_zero_size_spans() {
        // 3 zero-size spans + 2 at 12pt: mode should be 12pt, not 0pt.
        let lines = vec![TextLine::new(
            vec![
                TextSpan::new("z".into(), 0.0, 0.0, 0.0, 0.0),
                TextSpan::new("z".into(), 0.0, 0.0, 0.0, 0.0),
                TextSpan::new("z".into(), 0.0, 0.0, 0.0, 0.0),
                TextSpan::new("a".into(), 0.0, 0.0, 10.0, 12.0),
                TextSpan::new("b".into(), 10.0, 0.0, 10.0, 12.0),
            ],
            0.0,
            false,
        )];
        assert!((compute_dominant_font_size(&lines) - 12.0).abs() < f64::EPSILON);
    }

    #[test]
    fn dominant_font_size_all_zero_fallback() {
        // All zero-size spans: fallback should be 12.0.
        let lines = vec![TextLine::new(
            vec![
                TextSpan::new("z".into(), 0.0, 0.0, 0.0, 0.0),
                TextSpan::new("z".into(), 0.0, 0.0, 0.0, 0.0),
            ],
            0.0,
            false,
        )];
        assert!((compute_dominant_font_size(&lines) - 12.0).abs() < f64::EPSILON);
    }

    // -----------------------------------------------------------------------
    // Strict-fonts dispatch check (M-32b)
    // -----------------------------------------------------------------------

    #[test]
    fn strict_fonts_check_accepts_all_exact_spans() {
        let spans = vec![
            TextSpan::new("hello".into(), 0.0, 0.0, 20.0, 12.0),
            TextSpan::new("world".into(), 20.0, 0.0, 20.0, 12.0),
        ];
        assert!(strict_fonts_check(&spans, 0).is_none());
    }

    #[test]
    fn strict_fonts_check_flags_substituted_span() {
        use udoc_core::text::{FallbackReason, FontResolution};
        let mut span = TextSpan::new("hi".into(), 0.0, 0.0, 10.0, 12.0);
        span.font_resolution = FontResolution::Substituted {
            requested: "CMR10".into(),
            resolved: "Latin Modern Roman".into(),
            reason: FallbackReason::NameRouted,
        };
        let err = strict_fonts_check(&[span], 7).expect("substituted span should trip strict mode");
        let info = err.font_fallback_info().expect("typed payload");
        assert_eq!(info.requested, "CMR10");
        assert_eq!(info.reason, FallbackReason::NameRouted);
        assert!(format!("{err}").contains("extracting page 7"));
    }

    #[test]
    fn strict_fonts_check_flags_synthetic_fallback_span() {
        use udoc_core::text::{FallbackReason, FontResolution};
        let mut span = TextSpan::new("hi".into(), 0.0, 0.0, 10.0, 12.0);
        span.font_resolution = FontResolution::SyntheticFallback {
            requested: "MysteryFont".into(),
            generic_family: "serif".into(),
            reason: FallbackReason::NotEmbedded,
        };
        let err = strict_fonts_check(&[span], 0).expect("synthetic span should trip strict mode");
        let info = err.font_fallback_info().unwrap();
        assert_eq!(info.requested, "MysteryFont");
        assert_eq!(info.reason, FallbackReason::NotEmbedded);
    }

    #[test]
    fn strict_fonts_check_unknown_variant_carries_payload() {
        // Construct an Error::font_fallback_required with FallbackReason::Unknown
        // and confirm the typed payload survives the downcast. The forward-compat
        // catch-all in strict_fonts_check uses this variant when a future
        // FontResolution variant is observed (issue #203). Exercising the path
        // by mutating the enum directly would require a dummy non_exhaustive
        // variant; instead assert the shape the catch-all produces so the
        // invariant is locked even without a live trigger.
        use udoc_core::text::FallbackReason;
        let err = Error::font_fallback_required("MysteryFace", FallbackReason::Unknown)
            .with_context("extracting page 4");
        let info = err
            .font_fallback_info()
            .expect("catch-all path must carry typed FontFallbackRequired payload");
        assert_eq!(info.requested, "MysteryFace");
        assert_eq!(info.reason, FallbackReason::Unknown);
        let rendered = format!("{err}");
        assert!(
            rendered.contains("MysteryFace"),
            "error message must include font name, got: {rendered}"
        );
    }

    #[test]
    fn strict_fonts_check_returns_on_first_offender() {
        // Ensure we bail on the first non-Exact span so the error surfaces
        // the *first* bad font rather than the last in the page.
        use udoc_core::text::{FallbackReason, FontResolution};
        let mut first = TextSpan::new("a".into(), 0.0, 0.0, 10.0, 12.0);
        first.font_resolution = FontResolution::Substituted {
            requested: "FirstFont".into(),
            resolved: "Fallback".into(),
            reason: FallbackReason::NameRouted,
        };
        let mut second = TextSpan::new("b".into(), 10.0, 0.0, 10.0, 12.0);
        second.font_resolution = FontResolution::SyntheticFallback {
            requested: "SecondFont".into(),
            generic_family: "serif".into(),
            reason: FallbackReason::NotEmbedded,
        };
        let err = strict_fonts_check(&[first, second], 0).unwrap();
        let info = err.font_fallback_info().unwrap();
        assert_eq!(info.requested, "FirstFont");
    }
}

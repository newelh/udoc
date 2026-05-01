//! CTM-snapshot and canonical path IR tests.
//!
//! These tests drive the  /  behavioural spec: the content
//! interpreter must expose a `Vec<PagePath>` where each entry carries a
//! CTM snapshot taken at the paint operator (not at path construction
//! time), an explicit `FillRule` (no implicit default), and a
//! `StrokeStyle` sourced from the graphics state at the moment the
//! paint operator fires.
//!
//! Tests construct minimal content streams and drive the interpreter
//! directly; full PDF byte-string harness is not needed here because
//! the IR is observable at the interpreter-accumulator level.

use std::sync::Arc;

use crate::content::path::{Color, FillRule as PageFillRule, LineCap, LineJoin, PathSegmentKind};
use crate::diagnostics::NullDiagnostics;
use crate::object::resolver::ObjectResolver;
use crate::object::PdfDictionary;

use super::ContentInterpreter;

/// Drive the interpreter with `content`, returning its page-path accumulator.
fn interpret_paths(content: &[u8]) -> Vec<crate::content::path::PagePath> {
    let data = b"%PDF-1.4\n".to_vec();
    let xref = crate::parse::XrefTable::new();
    let mut resolver = ObjectResolver::new(data.as_slice(), xref);
    let resources = PdfDictionary::new();
    let diag = Arc::new(NullDiagnostics);
    let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
    interp.set_extract_page_paths(true);
    interp.interpret(content).expect("interpret ok");
    interp.take_page_paths()
}

/// Test 1: Path constructed at identity CTM and filled. Expect the
/// snapshot to be the identity.
#[test]
fn ctm_snapshot_is_identity_when_no_cm() {
    let paths = interpret_paths(b"0 0 m 100 0 l 100 100 l 0 100 l h f");
    assert_eq!(paths.len(), 1, "one paint operator -> one PagePath");
    let p = &paths[0];
    let m = p.ctm_at_paint;
    assert!((m.a - 1.0).abs() < 1e-9, "a");
    assert!(m.b.abs() < 1e-9, "b");
    assert!(m.c.abs() < 1e-9, "c");
    assert!((m.d - 1.0).abs() < 1e-9, "d");
    assert!(m.e.abs() < 1e-9, "e");
    assert!(m.f.abs() < 1e-9, "f");
    assert_eq!(p.fill, Some(PageFillRule::NonZero));
    assert!(p.stroke.is_none());
}

/// Test 2: Path constructed, then q + rotate-90 (via cm), then fill.
/// CTM snapshot should carry the rotation (approx sin/cos 90deg: a=0, b=1,
/// c=-1, d=0).
#[test]
fn ctm_snapshot_captures_rotation_applied_after_path_construction() {
    let content = b"0 0 m 10 0 l q 0 1 -1 0 0 0 cm f Q";
    let paths = interpret_paths(content);
    assert_eq!(paths.len(), 1, "one fill -> one PagePath");
    let m = paths[0].ctm_at_paint;
    assert!(m.a.abs() < 1e-9, "rotation: a should be 0, got {}", m.a);
    assert!(
        (m.b - 1.0).abs() < 1e-9,
        "rotation: b should be 1, got {}",
        m.b
    );
    assert!(
        (m.c + 1.0).abs() < 1e-9,
        "rotation: c should be -1, got {}",
        m.c
    );
    assert!(m.d.abs() < 1e-9, "rotation: d should be 0, got {}", m.d);
}

/// Test 3: q, construct path, translate via cm, extend path, then fill,
/// then Q. CTM snapshot must reflect the post-translate CTM (not the
/// pre-translate one), because the snapshot is taken at the paint op.
#[test]
fn ctm_snapshot_taken_at_paint_not_at_construction() {
    let content = b"q 0 0 m 1 0 0 1 50 80 cm 10 10 l f Q";
    let paths = interpret_paths(content);
    assert_eq!(paths.len(), 1);
    let m = paths[0].ctm_at_paint;
    assert!((m.e - 50.0).abs() < 1e-9, "e should be 50, got {}", m.e);
    assert!((m.f - 80.0).abs() < 1e-9, "f should be 80, got {}", m.f);
}

/// Test 4: q saves state, path is constructed, Q restores state without
/// a paint op. No PagePath should be emitted; the interpreter must not
/// crash.
#[test]
fn q_before_paint_discards_path_gracefully() {
    let content = b"q 0 0 m 100 0 l 100 100 l Q";
    let paths = interpret_paths(content);
    assert!(
        paths.is_empty(),
        "no paint op -> no PagePath, got {}",
        paths.len()
    );
}

/// Test 5: Two paints on distinct paths -> two PagePaths. z increments
/// monotonically.
#[test]
fn two_paint_ops_emit_two_paths_with_increasing_z() {
    let content = b"0 0 m 10 0 l S 1 0 0 1 20 0 cm 0 0 m 10 0 l S";
    let paths = interpret_paths(content);
    assert_eq!(paths.len(), 2, "two strokes -> two PagePaths");
    assert!(
        paths[0].z < paths[1].z,
        "z should increase with paint order"
    );
    assert!(paths[0].ctm_at_paint.e.abs() < 1e-9);
    assert!((paths[1].ctm_at_paint.e - 20.0).abs() < 1e-9);
}

/// Test 6: N(PagePath) == N(paint op), not N(construction).
#[test]
fn n_page_paths_equals_n_paint_ops_not_n_constructions() {
    let content = b"0 0 m 10 10 l S 0 0 m 10 10 l S";
    let paths = interpret_paths(content);
    assert_eq!(paths.len(), 2, "two paint ops -> two PagePaths");
    for p in &paths {
        assert_eq!(p.segments.len(), 2);
        assert!(matches!(p.segments[0], PathSegmentKind::MoveTo { .. }));
        assert!(matches!(p.segments[1], PathSegmentKind::LineTo { .. }));
    }
}

/// Test 7: `B` operator = fill (non-zero) then stroke. Both populated.
/// StrokeStyle carries line_width from GS.
#[test]
fn big_b_emits_both_fill_nonzero_and_stroke() {
    let content = b"2.5 w 0 0 m 50 0 l 25 50 l h B";
    let paths = interpret_paths(content);
    assert_eq!(paths.len(), 1);
    let p = &paths[0];
    assert_eq!(p.fill, Some(PageFillRule::NonZero), "B fills with NonZero");
    let stroke = p.stroke.as_ref().expect("B also strokes");
    assert!((stroke.line_width - 2.5).abs() < 1e-6);
    assert_eq!(stroke.line_cap, LineCap::Butt);
    assert_eq!(stroke.line_join, LineJoin::Miter);
    assert_eq!(stroke.color, Color::BLACK);
}

/// Test 8: `b*` = close + fill-EvenOdd + stroke. Closing emits ClosePath.
#[test]
fn small_b_star_closes_and_fills_evenodd() {
    let content = b"0 0 m 100 0 l 50 50 l b*";
    let paths = interpret_paths(content);
    assert_eq!(paths.len(), 1);
    let p = &paths[0];
    assert_eq!(p.fill, Some(PageFillRule::EvenOdd));
    assert!(p.stroke.is_some());
    assert!(matches!(
        p.segments.last(),
        Some(PathSegmentKind::ClosePath)
    ));
}

/// Test 9: `n` (no-op end path) -> no PagePath emitted.
#[test]
fn n_operator_does_not_emit_page_path() {
    let content = b"0 0 m 100 0 l 100 100 l n";
    let paths = interpret_paths(content);
    assert!(paths.is_empty());
}

/// Test 10: `re x y w h` then `f` expands to canonical M + 3L + Close.
#[test]
fn re_then_fill_expands_to_four_line_tos_plus_close() {
    let content = b"10 20 40 30 re f";
    let paths = interpret_paths(content);
    assert_eq!(paths.len(), 1);
    let p = &paths[0];
    assert_eq!(p.fill, Some(PageFillRule::NonZero));
    assert!(p.stroke.is_none());
    assert_eq!(
        p.segments.len(),
        5,
        "re -> M + 3L + Z, got {:?}",
        p.segments
    );
    match &p.segments[0] {
        PathSegmentKind::MoveTo { p } => {
            assert!((p.x - 10.0).abs() < 1e-9);
            assert!((p.y - 20.0).abs() < 1e-9);
        }
        other => panic!("expected MoveTo, got {:?}", other),
    }
    match &p.segments[1] {
        PathSegmentKind::LineTo { p } => {
            assert!((p.x - 50.0).abs() < 1e-9);
            assert!((p.y - 20.0).abs() < 1e-9);
        }
        other => panic!("expected LineTo, got {:?}", other),
    }
    match &p.segments[2] {
        PathSegmentKind::LineTo { p } => {
            assert!((p.x - 50.0).abs() < 1e-9);
            assert!((p.y - 50.0).abs() < 1e-9);
        }
        other => panic!("expected LineTo, got {:?}", other),
    }
    match &p.segments[3] {
        PathSegmentKind::LineTo { p } => {
            assert!((p.x - 10.0).abs() < 1e-9);
            assert!((p.y - 50.0).abs() < 1e-9);
        }
        other => panic!("expected LineTo, got {:?}", other),
    }
    assert!(matches!(p.segments[4], PathSegmentKind::ClosePath));
}

/// Test 11: `v` -> CurveTo with c1 == current point; `y` -> c2 == end.
#[test]
fn v_and_y_expand_to_canonical_curve_to() {
    let paths = interpret_paths(b"0 0 m 10 20 30 40 v S");
    assert_eq!(paths.len(), 1);
    let p = &paths[0];
    assert_eq!(p.segments.len(), 2);
    match &p.segments[1] {
        PathSegmentKind::CurveTo { c1, c2, end } => {
            assert!(c1.x.abs() < 1e-9 && c1.y.abs() < 1e-9, "v c1: {:?}", c1);
            assert!((c2.x - 10.0).abs() < 1e-9 && (c2.y - 20.0).abs() < 1e-9);
            assert!((end.x - 30.0).abs() < 1e-9 && (end.y - 40.0).abs() < 1e-9);
        }
        other => panic!("expected CurveTo, got {:?}", other),
    }

    let paths_y = interpret_paths(b"0 0 m 10 20 30 40 y S");
    assert_eq!(paths_y.len(), 1);
    match &paths_y[0].segments[1] {
        PathSegmentKind::CurveTo { c1, c2, end } => {
            assert!((c1.x - 10.0).abs() < 1e-9 && (c1.y - 20.0).abs() < 1e-9);
            assert!((c2.x - 30.0).abs() < 1e-9 && (c2.y - 40.0).abs() < 1e-9);
            assert!((end.x - 30.0).abs() < 1e-9 && (end.y - 40.0).abs() < 1e-9);
        }
        other => panic!("expected CurveTo, got {:?}", other),
    }
}

/// Test 12: stroke operator `S` emits stroke, no fill; color flows from RG.
#[test]
fn stroke_only_emits_no_fill() {
    let content = b"1 0 0 RG 0 0 m 100 0 l S";
    let paths = interpret_paths(content);
    assert_eq!(paths.len(), 1);
    let p = &paths[0];
    assert!(p.fill.is_none());
    let stroke = p.stroke.as_ref().expect("S strokes");
    match stroke.color {
        Color::Rgb { r, g, b, a } => {
            assert_eq!(r, 255);
            assert_eq!(g, 0);
            assert_eq!(b, 0);
            assert_eq!(a, 255);
        }
    }
}

//! Thread-local scratch buffers for the auto-hinter hot path.
//!
//! Per-glyph fitting used to allocate a fresh `Vec<Segment>`, `Vec<Edge>`,
//! `Vec<AnalysisContour>` (with inner `Vec<AnalysisPoint>`), plus several
//! smaller working buffers on every call. On a rasterization pass that runs
//! the hinter for hundreds or thousands of glyphs per page, these short-lived
//! allocations dominate the hinter's allocator footprint.
//!
//! This module provides a single thread-local [`ScratchBuffers`] struct
//! that holds all of the reusable buffers. Callers acquire the struct via
//! [`with_scratch`], which guarantees exclusive per-thread access and clears
//! the buffers before handing them back. After the fitter runs, each Vec
//! keeps its capacity so the next glyph's calls reuse the same memory.
//!
//! The pool is per-thread (via `thread_local!`) rather than per-font-cache
//! because the auto-hinter is stateless from the caller's perspective; the
//! public `auto_hint_glyph` / `auto_hint_glyph_axes` API does not take a
//! context argument. Parallel renders naturally get one pool per worker
//! thread without any plumbing.
//!
//! Re-entrancy: `auto_hint_glyph` never calls itself directly or through any
//! of its helpers, so the exclusive borrow is safe. A re-entrant call (for
//! instance a diagnostic tool that runs the hinter from within a custom
//! `DiagnosticsSink` callback) would panic on the `RefCell` borrow, which
//! is the correct loud failure mode.

use super::edges::Edge;
use super::segments::{AnalysisContour, Segment};
use crate::ps_hints::HintAxis;

/// Bundle of reusable per-glyph scratch buffers.
///
/// Every field is cleared before handing to the hinter; capacity is
/// preserved across calls so the per-glyph hot path allocates nothing
/// after a brief warm-up on the first few glyphs.
#[derive(Default)]
pub(crate) struct ScratchBuffers {
    /// Analysis contours with point data. Inner `Vec<AnalysisPoint>` on each
    /// contour is recycled via `clear()`; capacity persists. We keep the
    /// outer Vec at whatever size we reached and walk `[..live_contours]`
    /// for the current call.
    pub analysis_contours: Vec<AnalysisContour>,
    /// Horizontal-dimension segments for the current glyph.
    pub h_segments: Vec<Segment>,
    /// Vertical-dimension segments for the current glyph.
    pub v_segments: Vec<Segment>,
    /// Horizontal-dimension edges.
    pub h_edges: Vec<Edge>,
    /// Vertical-dimension edges.
    pub v_edges: Vec<Edge>,
    /// Scratch for `detect_edges`: sorted segment indices within one
    /// dimension. Cleared and reused between the h and v calls.
    pub dim_indices: Vec<usize>,
    /// Scratch for `link_edges`: per-target-edge vote count (one entry per
    /// candidate edge).
    pub link_counts: Vec<usize>,
    /// Scratch for `link_edges`: per-target-edge serif vote count.
    pub serif_counts: Vec<usize>,
    /// Pre-computed `(ref_fit_px, shoot_fit_px)` per blue zone, used by
    /// `fit_blue_zone_edges`.
    pub zone_fits: Vec<(f64, f64)>,
    /// `(original_pos, fitted_pos)` pairs for already-fitted edges used by
    /// `fit_remaining_edges` to interpolate unfitted ones.
    pub fitted_pairs: Vec<(f64, f64)>,
    /// Position-sorted edge indices used by `fit_stem_edges_anchor`.
    pub order: Vec<usize>,
    /// Y-axis `HintAxis` used to map original coords to grid-snapped ones.
    /// Reused across calls; `clear()` preserves the inner Vec's capacity.
    pub y_axis: HintAxis,
    /// X-axis `HintAxis` (populated only when `HintAxes::Both`).
    pub x_axis: HintAxis,
}

impl ScratchBuffers {
    /// Clear all buffers before a new glyph call.
    ///
    /// Capacity on every Vec is preserved. Inner `Vec<AnalysisPoint>` on
    /// each `analysis_contours` entry is cleared individually to preserve
    /// its capacity.
    pub fn reset(&mut self) {
        for c in &mut self.analysis_contours {
            c.points.clear();
        }
        // Do not truncate analysis_contours; leaving over-long entries with
        // their cleared point vecs lets the next glyph reuse them.
        self.h_segments.clear();
        self.v_segments.clear();
        self.h_edges.clear();
        self.v_edges.clear();
        self.dim_indices.clear();
        self.link_counts.clear();
        self.serif_counts.clear();
        self.zone_fits.clear();
        self.fitted_pairs.clear();
        self.order.clear();
        self.y_axis.clear();
        self.x_axis.clear();
    }

    /// Take (or push) an `AnalysisContour` slot at `idx`, resetting its
    /// point buffer if it already exists. Used by `tuples_to_contours_into`
    /// to fill the outer Vec in place.
    pub fn ensure_contour_slot(&mut self, idx: usize) -> &mut AnalysisContour {
        while self.analysis_contours.len() <= idx {
            self.analysis_contours
                .push(AnalysisContour { points: Vec::new() });
        }
        &mut self.analysis_contours[idx]
    }
}

thread_local! {
    static SCRATCH: std::cell::RefCell<ScratchBuffers> =
        std::cell::RefCell::new(ScratchBuffers::default());
}

/// Run `f` with exclusive access to this thread's scratch buffers.
///
/// Buffers are cleared (capacities preserved) before `f` runs. The
/// `RefCell` borrow is exclusive for the duration; a re-entrant call from
/// inside `f` will panic.
pub(crate) fn with_scratch<R, F>(f: F) -> R
where
    F: FnOnce(&mut ScratchBuffers) -> R,
{
    SCRATCH.with(|cell| {
        let mut b = cell.borrow_mut();
        b.reset();
        f(&mut b)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto_hinter::segments::AnalysisPoint;

    #[test]
    fn reset_preserves_capacity() {
        let mut s = ScratchBuffers::default();
        s.h_segments.reserve(32);
        s.h_edges.reserve(16);
        s.dim_indices.reserve(8);
        let h_seg_cap = s.h_segments.capacity();
        let h_edge_cap = s.h_edges.capacity();
        let dim_cap = s.dim_indices.capacity();
        s.reset();
        assert_eq!(s.h_segments.capacity(), h_seg_cap);
        assert_eq!(s.h_edges.capacity(), h_edge_cap);
        assert_eq!(s.dim_indices.capacity(), dim_cap);
        assert!(s.h_segments.is_empty());
        assert!(s.h_edges.is_empty());
    }

    #[test]
    fn ensure_contour_slot_grows_pool() {
        let mut s = ScratchBuffers::default();
        {
            let c = s.ensure_contour_slot(2);
            c.points.push(AnalysisPoint {
                x: 1.0,
                y: 2.0,
                out_dir: crate::auto_hinter::segments::Direction::Up,
                on_curve: true,
            });
        }
        assert_eq!(s.analysis_contours.len(), 3);
        assert_eq!(s.analysis_contours[2].points.len(), 1);
        s.reset();
        // Capacity preserved; slot still present but points cleared.
        assert_eq!(s.analysis_contours.len(), 3);
        assert!(s.analysis_contours[2].points.is_empty());
    }

    #[test]
    fn with_scratch_clears_between_calls() {
        with_scratch(|s| {
            s.h_segments.push(Segment {
                dir: crate::auto_hinter::segments::Direction::Up,
                dim: crate::auto_hinter::segments::Dimension::Vertical,
                contour: 0,
                pos: 0.0,
                min_coord: 0.0,
                max_coord: 1.0,
                link: None,
                score: 0.0,
                edge: None,
                is_round: false,
                serif: None,
            });
        });
        with_scratch(|s| {
            assert!(s.h_segments.is_empty(), "buffers cleared on re-entry");
        });
    }
}

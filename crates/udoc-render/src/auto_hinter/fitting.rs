//! Edge fitting: grid-fit edges to pixel boundaries.
//!
//! Implements FreeType-style edge fitting in priority order:
//! 1. Blue zone edges (snap to alignment zones)
//! 2. Stem edges (round width, snap to grid)
//! 3. Serif edges (shift with base, don't snap independently)
//! 4. Remaining edges (interpolate between fitted neighbors)

use super::edges::{Edge, EDGE_SERIF};
use super::metrics::{BlueZone, GlobalMetrics};
use super::segments::Dimension;

/// Fit edges to the pixel grid for a specific render size.
///
/// Modifies `Edge::fitted_pos` and `Edge::fitted` in place.
#[allow(dead_code)] // kept for test harness and future direct callers
pub fn fit_edges(edges: &mut [Edge], metrics: &GlobalMetrics, scale: f64) {
    fit_edges_with(edges, metrics, scale, AnchorMode::Cascade)
}

/// Anchor selection for stem-edge fitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorMode {
    /// Use cascade (blue-zone-anchored) fitting. Correct for Y-axis.
    Cascade,
    /// Use single-anchor fitting (FreeType X-axis model).
    Anchor,
}

/// Fit edges, selecting the anchor model explicitly.
pub fn fit_edges_with(edges: &mut [Edge], metrics: &GlobalMetrics, scale: f64, mode: AnchorMode) {
    let mut zone_fits = Vec::new();
    let mut order = Vec::new();
    let mut fitted_pairs = Vec::new();
    fit_edges_with_scratch(
        edges,
        metrics,
        scale,
        mode,
        &mut zone_fits,
        &mut order,
        &mut fitted_pairs,
    );
}

/// Like [`fit_edges_with`] but reuses caller-supplied scratch buffers so
/// the per-glyph hot path does no fresh allocation.
///
/// `zone_fits`, `order`, and `fitted_pairs` are cleared on entry and left
/// populated on exit; capacity persists.
pub(crate) fn fit_edges_with_scratch(
    edges: &mut [Edge],
    metrics: &GlobalMetrics,
    scale: f64,
    mode: AnchorMode,
    zone_fits: &mut Vec<(f64, f64)>,
    order: &mut Vec<usize>,
    fitted_pairs: &mut Vec<(f64, f64)>,
) {
    if edges.is_empty() || scale <= 0.0 {
        return;
    }

    // Step 1: Fit blue zone edges.
    fit_blue_zone_edges(
        edges,
        &metrics.blue_zones,
        metrics.units_per_em,
        scale,
        zone_fits,
    );

    // Step 2: Fit stem edges. Cascade (Y-axis) vs anchor (X-axis) model.
    match mode {
        AnchorMode::Anchor => fit_stem_edges_anchor(edges, metrics, scale, order),
        AnchorMode::Cascade => fit_stem_edges_cascade(edges, metrics, scale),
    }

    // Step 3: Fit serif edges.
    fit_serif_edges(edges);

    // Step 4: Fit remaining unfitted edges by interpolation.
    fit_remaining_edges(edges, fitted_pairs);
}

/// Step 1: Snap edges that touch blue zones to grid-fitted zone positions.
///
/// Blue zones control Y-axis alignment (baselines, x-height, ascender).
/// Only `Dimension::Horizontal` edges (horizontal strokes that carry
/// y-coordinates) can snap to blue zones. V-dim edges (X-axis stems)
/// must stay out of the blue-zone pull -- mirrors FT's
/// `af_latin_hint_edges` which gates blue snapping on the vertical
/// dimension only (aflatin.c:3029).
///
/// Matches FreeType's `af_latin_metrics_scale_dim` x-height alignment
/// (`aflatin.c` lines 1197-1296): the effective y-scale is bumped so the
/// x-height overshoot lands on a pixel boundary. Without this, Liberation
/// Sans 'o' at ppem=12 renders 1 px shorter than FT's output (#175).
fn fit_blue_zone_edges(
    edges: &mut [Edge],
    blue_zones: &[BlueZone],
    units_per_em: u16,
    scale: f64,
    zone_fits: &mut Vec<(f64, f64)>,
) {
    // Blue fuzz: how close an edge must be to a blue zone to snap to it.
    let blue_fuzz = 10.0; // font units

    // Compute FT's x-height alignment bump. The adjusted scale is used for
    // all blue-zone fit positions (both ref and shoot). Stem edges that
    // cascade from fitted blue edges inherit the shifted positions.
    let fit_scale = x_height_adjusted_scale(blue_zones, units_per_em, scale);

    // Pre-compute the fit (ref.fit, shoot.fit) for each blue zone, in pixels
    // via the adjusted scale. FreeType's discretization (`aflatin.c:1368`):
    //   |dist_px| > 3/4: zone inactive -- ref.fit = ref.cur, shoot.fit = shoot.cur
    //   |dist_px| <= 3/4: ref.fit = PIX_ROUND(ref.cur), then
    //                      delta2 = 0    if |dist_26_6| < 32   (<0.5 px)
    //                      delta2 = 0.5  if |dist_26_6| < 48   (<0.75 px)
    //                      delta2 = 1.0  otherwise
    //                     shoot.fit = ref.fit +/- delta2 (sign = sign(dist)).
    zone_fits.clear();
    zone_fits.extend(blue_zones.iter().map(|zone| fit_blue_zone(zone, fit_scale)));

    #[allow(clippy::needless_range_loop)]
    for edge_idx in 0..edges.len() {
        if edges[edge_idx].fitted {
            continue;
        }
        // X-axis (Vertical-dim) edges must not snap to Y-axis blue zones.
        if edges[edge_idx].dim != Dimension::Horizontal {
            continue;
        }

        let edge_pos = edges[edge_idx].pos;
        let mut best_zone: Option<usize> = None;
        let mut snap_to_overshoot = false;
        let mut best_dist = f64::MAX;

        for (zone_idx, zone) in blue_zones.iter().enumerate() {
            // Check distance to reference edge.
            let dist_ref = (edge_pos - zone.reference).abs();
            // Check distance to overshoot edge.
            let dist_over = (edge_pos - zone.overshoot).abs();
            let (dist, this_is_overshoot) = if dist_ref <= dist_over {
                (dist_ref, false)
            } else {
                (dist_over, true)
            };

            if dist < blue_fuzz && dist < best_dist {
                best_dist = dist;
                best_zone = Some(zone_idx);
                snap_to_overshoot = this_is_overshoot;
            }
        }

        if let Some(zone_idx) = best_zone {
            let (ref_fit_px, shoot_fit_px) = zone_fits[zone_idx];
            let target_px = if snap_to_overshoot {
                shoot_fit_px
            } else {
                ref_fit_px
            };

            // Convert target pixel position back to font units using the
            // ORIGINAL (unadjusted) scale -- the rasterizer multiplies by
            // this scale to land at our desired pixel.
            edges[edge_idx].fitted_pos = target_px / scale;
            edges[edge_idx].fitted = true;
            edges[edge_idx].blue_zone = Some(zone_idx);
        }
    }
}

/// FreeType's x-height alignment: adjust y-scale so the x-height overshoot
/// aligns to the pixel grid. Mirrors `aflatin.c` lines 1197-1296.
///
/// Keeps the original scale when no x-height blue zone is marked, when the
/// shoot already lands on a pixel boundary, or when the bump would move the
/// em height by more than 2 pixels (FT's safety limit).
fn x_height_adjusted_scale(blue_zones: &[BlueZone], units_per_em: u16, scale: f64) -> f64 {
    // Gated on the active [`crate::RenderingProfile`] (T3-OCR).
    // M-39 is FT-NORMAL byte-exact but shifts baselines by a fraction of a
    // pixel, which degrades tesseract psm-6 row segmentation on some docs
    // (e.g. arxiv-bio/2508.07465.pdf drops from 0.99 to 0.25 char-acc).
    // MuPDF uses FT-LIGHT mode and doesn't apply this bump either; the
    // OcrFriendly profile (default) matches MuPDF behaviour while Visual
    // preserves the FT-NORMAL per-glyph fidelity for cursor-pair probes.
    if matches!(
        crate::current_rendering_profile(),
        crate::RenderingProfile::OcrFriendly
    ) {
        return scale;
    }
    let Some(xh) = blue_zones.iter().find(|z| z.is_x_height) else {
        return scale;
    };
    if xh.overshoot == 0.0 || scale <= 0.0 {
        return scale;
    }

    // FT's bias threshold is 40/64 = 0.625 px; pushes scaled_shoot values
    // above .375 up to the next pixel.
    let scaled_px = xh.overshoot * scale;
    let fitted_px = ((scaled_px * 64.0 + 40.0).floor() as i64 & !63) as f64 / 64.0;
    if (fitted_px - scaled_px).abs() < 1e-9 {
        return scale;
    }

    let new_scale = scale * fitted_px / scaled_px;

    // Safety: don't let the bump move the em-height by more than 2 px
    // (FT's 128/64 limit in `aflatin.c:1268`). Use the real UPM rather
    // than a hardcoded upper bound so CFF fonts (UPM=1000) and large-UPM
    // CJK fonts (UPM > 2048) get the same tolerance FT would apply.
    // Fall back to 1000 (the FT default) when a font reports UPM=0.
    let em = if units_per_em == 0 {
        1000.0_f64
    } else {
        f64::from(units_per_em)
    };
    let shift_px = em * (new_scale - scale);
    if shift_px.abs() < 2.0 {
        new_scale
    } else {
        scale
    }
}

/// Fit a single blue zone's (ref, shoot) positions in pixels using the
/// (possibly x-height-adjusted) scale. Returns `(ref_fit_px, shoot_fit_px)`.
///
/// Mirrors `aflatin.c` lines 1370-1423 exactly.
fn fit_blue_zone(zone: &BlueZone, scale: f64) -> (f64, f64) {
    let ref_px = zone.reference * scale;
    let shoot_px = zone.overshoot * scale;
    let dist_px = ref_px - shoot_px;

    // Zone inactive if |overshoot distance| > 3/4 pixel: leave both at
    // their scaled positions (no grid snap).
    if dist_px.abs() > 0.75 {
        return (ref_px, shoot_px);
    }

    // PIX_ROUND = round-to-nearest pixel.
    let ref_fit = ref_px.round();

    // Discretize scaled overshoot distance (FT's 32/48/64 thresholds
    // expressed in pixels: 0.5 / 0.75 / 1.0).
    let d_abs = dist_px.abs();
    let delta2 = if d_abs < 0.5 {
        0.0
    } else if d_abs < 0.75 {
        0.5
    } else {
        1.0
    };
    let signed_delta2 = if dist_px >= 0.0 { -delta2 } else { delta2 };
    let shoot_fit = ref_fit + signed_delta2;

    (ref_fit, shoot_fit)
}

/// Crossbar darkening: ppem gate for the +1 device-pixel bonus applied to
/// horizontal stems that the semantic classifier flagged as crossbars.
/// 150 DPI body text lands at ~12-16 ppem; 300 DPI body text is ~24+
/// ppem and the crossbar already resolves cleanly, so 300 DPI stays
/// untouched by design.
const CROSSBAR_PPEM_MIN: f64 = 6.0;
const CROSSBAR_PPEM_MAX: f64 = 16.0; // exclusive

/// Return the stem-width target in pixels, with a +1 device-pixel bump for
/// horizontal stems flagged as semantic crossbars when ppem is in
/// [CROSSBAR_PPEM_MIN, CROSSBAR_PPEM_MAX). Bumped target is
/// `ceil(stem_px) + 1` so a sub-pixel crossbar (rounded to 1 px) widens
/// to 2 px, making it survive Tesseract's binarization at 150 DPI.
fn compute_target_width_cascade(
    lo: &Edge,
    hi: &Edge,
    dominant_width: f64,
    scale: f64,
    ppem: f64,
) -> f64 {
    let base = compute_target_width(hi.pos, lo.pos, dominant_width, scale);

    let is_crossbar = lo.is_crossbar || hi.is_crossbar;
    let in_gate = (CROSSBAR_PPEM_MIN..CROSSBAR_PPEM_MAX).contains(&ppem);
    if !is_crossbar || !in_gate {
        return base;
    }

    let stem_px = (hi.pos - lo.pos).abs() * scale;
    let bumped = stem_px.ceil().max(1.0) + 1.0;
    // Never shrink below the unbumped rounded target. We only bump UP.
    bumped.max(base)
}

/// Step 2 (blue-zone cascade variant): Fit stem edges propagating from
/// blue-zone-anchored edges.
///
/// Used for the Y-axis (vertical dimension) where blue zones anchor the
/// baseline and x-height, and stems cascade outward from them.
fn fit_stem_edges_cascade(edges: &mut [Edge], metrics: &GlobalMetrics, scale: f64) {
    let n = edges.len();
    if n == 0 {
        return;
    }

    let ppem = scale * metrics.units_per_em as f64;

    let dominant = |e: &Edge| -> f64 {
        if e.dim == Dimension::Horizontal {
            metrics.dominant_h_width
        } else {
            metrics.dominant_v_width
        }
    };

    // Pass 1: Fit edges whose partner is already fitted (blue-zone cascade).
    // Repeat until no more edges get fitted (BFS-like propagation).
    let mut changed = true;
    while changed {
        changed = false;
        for edge_idx in 0..n {
            if edges[edge_idx].fitted {
                continue;
            }
            if edges[edge_idx].flags & EDGE_SERIF != 0 {
                continue;
            }
            let link_idx = match edges[edge_idx].link {
                Some(l) if l < n => l,
                _ => continue,
            };
            if !edges[link_idx].fitted {
                continue; // partner not yet fitted, skip for now
            }

            // Partner is fitted: derive our position from it.
            let (lo, hi) = if edges[edge_idx].pos < edges[link_idx].pos {
                (&edges[edge_idx], &edges[link_idx])
            } else {
                (&edges[link_idx], &edges[edge_idx])
            };
            let target = compute_target_width_cascade(lo, hi, dominant(lo), scale, ppem);
            let sign = if edges[edge_idx].pos > edges[link_idx].pos {
                1.0
            } else {
                -1.0
            };
            edges[edge_idx].fitted_pos = edges[link_idx].fitted_pos + sign * target / scale;
            edges[edge_idx].fitted = true;
            changed = true;
        }
    }

    // Pass 2: Fit remaining linked edges that have no blue-zone connection.
    // These get center-preserved width rounding.
    for edge_idx in 0..n {
        if edges[edge_idx].fitted {
            continue;
        }
        if edges[edge_idx].flags & EDGE_SERIF != 0 {
            continue;
        }
        let link_idx = match edges[edge_idx].link {
            Some(l) if l < n => l,
            _ => continue, // unlinked: handled by interpolation (step 4)
        };
        if edges[link_idx].fitted {
            // Partner got fitted in pass 1/2 since we last checked.
            let (lo, hi) = if edges[edge_idx].pos < edges[link_idx].pos {
                (&edges[edge_idx], &edges[link_idx])
            } else {
                (&edges[link_idx], &edges[edge_idx])
            };
            let target = compute_target_width_cascade(lo, hi, dominant(lo), scale, ppem);
            let sign = if edges[edge_idx].pos > edges[link_idx].pos {
                1.0
            } else {
                -1.0
            };
            edges[edge_idx].fitted_pos = edges[link_idx].fitted_pos + sign * target / scale;
            edges[edge_idx].fitted = true;
            continue;
        }

        // Neither fitted: keep center, round width only.
        let (lo_ref, hi_ref) = if edges[edge_idx].pos < edges[link_idx].pos {
            (&edges[edge_idx], &edges[link_idx])
        } else {
            (&edges[link_idx], &edges[edge_idx])
        };
        let target = compute_target_width_cascade(lo_ref, hi_ref, dominant(lo_ref), scale, ppem);
        let (lo, hi) = if edges[edge_idx].pos < edges[link_idx].pos {
            (edge_idx, link_idx)
        } else {
            (link_idx, edge_idx)
        };

        // Snap center to nearest half-pixel so edges land on pixel boundaries.
        // center 8.44 -> 8.5, with width 1px -> edges at 8.0 and 9.0 (clean).
        let center_px = (edges[lo].pos + edges[hi].pos) / 2.0 * scale;
        let center_px = (center_px * 2.0).round() / 2.0;
        let half_width = target / 2.0;

        let lo_px = edges[lo].pos * scale;
        let hi_px = edges[hi].pos * scale;
        edges[lo].fitted_pos = edges[lo].pos + (center_px - half_width - lo_px) / scale;
        edges[lo].fitted = true;
        edges[hi].fitted_pos = edges[hi].pos + (center_px + half_width - hi_px) / scale;
        edges[hi].fitted = true;
    }
}

/// Step 2 (anchor variant): Fit stem edges for the X-axis (no blue zones).
///
/// Byte-exact port of FreeType's `af_latin_hint_edges` stem loop
/// (aflatin.c:4346-4571) operating in 26.6 fixed-point pixel units so
/// the arithmetic matches FT to the last bit ( /).
///
/// Algorithm:
/// 1. Iterate edges in original-position order (FT's edge array is
///    already sorted by opos; we sort here to reproduce that).
/// 2. Skip serifs and unlinked edges (they go through the serif/interp
///    passes later).
/// 3. For the first linked pair encountered, use the ANCHOR branch
///    (aflatin.c:4378-4446): round center to the pixel grid, bias via
///    `u_off/d_off` for narrow stems.
/// 4. For subsequent linked pairs, use the STEM branch
///    (aflatin.c:4447-4570): preserve spacing relative to the anchor's
///    shift, then pick the best of two candidate fits for wider stems.
/// 5. When a pair's partner is already placed (via blue-zone snap in
///    horz path, or via a previous stem's alignment), run
///    `af_latin_align_linked_edge` to shift just this edge.
///
/// Round stems (`AF_EDGE_ROUND` -> our `is_round`) reuse the same
/// algorithm with a width adjustment in `af_latin_compute_stem_width`
/// (narrow rounds snap to 1 px). No separate "softer fit" path.
fn fit_stem_edges_anchor(
    edges: &mut [Edge],
    metrics: &GlobalMetrics,
    scale: f64,
    order: &mut Vec<usize>,
) {
    let n = edges.len();
    if n == 0 || scale <= 0.0 {
        return;
    }

    // FT works in 26.6 fixed-point pixel units; `scale * 64` converts a
    // font-unit position to that domain. Stash once so we can stay in it
    // throughout the loop.
    let fu_to_26_6 = scale * 64.0;

    let dominant_26_6 = {
        let dom_fu = if edges[0].dim == Dimension::Horizontal {
            metrics.dominant_h_width
        } else {
            metrics.dominant_v_width
        };
        ((dom_fu * fu_to_26_6).round()) as i64
    };

    // Sort edges by original position so we process left-to-right. FT's
    // edge array is built sorted on opos by `af_glyph_hints_build_edges`,
    // so this reproduces FT iteration order exactly.
    order.clear();
    order.extend(0..n);
    order.sort_by(|&a, &b| {
        edges[a]
            .pos
            .partial_cmp(&edges[b].pos)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Per-edge 26.6 opos snapshot (FT's `AF_Edge::opos`). We work with it
    // directly to keep the math identical to FT without repeated f64
    // multiplies inside the hot loop.
    let opos_26_6: Vec<i64> = edges
        .iter()
        .map(|e| (e.pos * fu_to_26_6).round() as i64)
        .collect();
    // Per-edge 26.6 fitted pos. Initialise to opos so unfitted edges still
    // carry a meaningful value for the LINK branch to read.
    let mut pos_26_6: Vec<i64> = opos_26_6.clone();

    let mut anchor: Option<usize> = None; // index of anchor edge in `edges`

    for &i in order.iter() {
        if edges[i].fitted {
            // Update our 26.6 snapshot from the existing fitted value so
            // downstream FT-logic sees the right base. (Blue-zone snap
            // above may have landed a horz edge here.)
            pos_26_6[i] = (edges[i].fitted_pos * fu_to_26_6).round() as i64;
            continue;
        }

        let link_idx = match edges[i].link {
            Some(l) if l < n => l,
            _ => continue, // unlinked: skipped, caught by serif/interp pass
        };

        if edges[i].flags & EDGE_SERIF != 0 {
            continue;
        }

        // When one member of the pair is already fitted, the other
        // follows via af_latin_align_linked_edge.
        if edges[link_idx].fitted {
            pos_26_6[link_idx] = (edges[link_idx].fitted_pos * fu_to_26_6).round() as i64;
            let dist_26_6 = opos_26_6[i] - opos_26_6[link_idx];
            let base_delta_26_6 = pos_26_6[link_idx] - opos_26_6[link_idx];
            let fitted_width = af_latin_compute_stem_width(
                dist_26_6,
                base_delta_26_6,
                edges[link_idx].is_round,
                edges[link_idx].flags & EDGE_SERIF != 0,
                edges[i].flags & EDGE_SERIF != 0,
                dominant_26_6,
            );
            pos_26_6[i] = pos_26_6[link_idx] + fitted_width;
            edges[i].fitted_pos = pos_26_6[i] as f64 / fu_to_26_6;
            edges[i].fitted = true;
            continue;
        }

        // Both unfitted: FT branches on anchor presence.
        // FT's `edge` / `edge2` pair is defined by iteration order, not
        // lo/hi. We therefore mirror FT: `edge` = the outer loop edge,
        // `edge2` = its link.
        let edge = i;
        let edge2 = link_idx;
        let org_len = opos_26_6[edge2] - opos_26_6[edge];
        let cur_len = af_latin_compute_stem_width(
            org_len,
            0,
            edges[edge].is_round,
            edges[edge].flags & EDGE_SERIF != 0,
            edges[edge2].flags & EDGE_SERIF != 0,
            dominant_26_6,
        );

        match anchor {
            None => {
                // ANCHOR branch (aflatin.c:4378-4446).
                if cur_len.abs() < 96 {
                    let org_center = opos_26_6[edge] + (org_len >> 1);
                    let mut cur_pos1 = ft_pix_round(org_center);
                    let (u_off, d_off) = if cur_len.abs() <= 64 {
                        (32_i64, 32_i64)
                    } else {
                        (38_i64, 26_i64)
                    };
                    let mut err1 = org_center - (cur_pos1 - u_off);
                    if err1 < 0 {
                        err1 = -err1;
                    }
                    let mut err2 = org_center - (cur_pos1 + d_off);
                    if err2 < 0 {
                        err2 = -err2;
                    }
                    if err1 < err2 {
                        cur_pos1 -= u_off;
                    } else {
                        cur_pos1 += d_off;
                    }
                    pos_26_6[edge] = cur_pos1 - cur_len / 2;
                    pos_26_6[edge2] = pos_26_6[edge] + cur_len;
                } else {
                    pos_26_6[edge] = ft_pix_round(opos_26_6[edge]);
                    let base_delta = pos_26_6[edge] - opos_26_6[edge];
                    let dist = opos_26_6[edge2] - opos_26_6[edge];
                    let fitted_width = af_latin_compute_stem_width(
                        dist,
                        base_delta,
                        edges[edge].is_round,
                        edges[edge].flags & EDGE_SERIF != 0,
                        edges[edge2].flags & EDGE_SERIF != 0,
                        dominant_26_6,
                    );
                    pos_26_6[edge2] = pos_26_6[edge] + fitted_width;
                }
                anchor = Some(edge);
            }
            Some(a) => {
                // STEM branch (aflatin.c:4447-4570). `a` is the anchor edge.
                let anchor_shift = pos_26_6[a] - opos_26_6[a];
                let org_pos = opos_26_6[edge] + anchor_shift;
                let org_center = org_pos + (org_len >> 1);

                if cur_len.abs() < 96 {
                    // Narrow stem: center-snap with asymmetric u_off/d_off.
                    let mut cur_pos1 = ft_pix_round(org_center);
                    let (u_off, d_off) = if cur_len.abs() <= 64 {
                        (32_i64, 32_i64)
                    } else {
                        (38_i64, 26_i64)
                    };
                    let mut delta1 = org_center - (cur_pos1 - u_off);
                    if delta1 < 0 {
                        delta1 = -delta1;
                    }
                    let mut delta2 = org_center - (cur_pos1 + d_off);
                    if delta2 < 0 {
                        delta2 = -delta2;
                    }
                    if delta1 < delta2 {
                        cur_pos1 -= u_off;
                    } else {
                        cur_pos1 += d_off;
                    }
                    pos_26_6[edge] = cur_pos1 - cur_len / 2;
                    pos_26_6[edge2] = cur_pos1 + cur_len / 2;
                } else {
                    // Wider stem: pick between rounding low edge vs rounding
                    // high edge, keeping center closest to org_center.
                    let cur_pos1 = ft_pix_round(org_pos);
                    let mut delta1 = cur_pos1 + (cur_len >> 1) - org_center;
                    if delta1 < 0 {
                        delta1 = -delta1;
                    }
                    let cur_pos2 = ft_pix_round(org_pos + org_len) - cur_len;
                    let mut delta2 = cur_pos2 + (cur_len >> 1) - org_center;
                    if delta2 < 0 {
                        delta2 = -delta2;
                    }
                    pos_26_6[edge] = if delta1 < delta2 { cur_pos1 } else { cur_pos2 };
                    pos_26_6[edge2] = pos_26_6[edge] + cur_len;
                }

                // BOUND check (aflatin.c:4550-4569): if the new position
                // would overlap the previous fitted edge in iteration
                // order, pull it back unless doing so would make the
                // partner collapse (16/64 = 1/4 px threshold).
                if let Some(order_pos) = order.iter().position(|&x| x == edge) {
                    if order_pos > 0 {
                        let prev = order[order_pos - 1];
                        if edges[prev].fitted
                            && pos_26_6[edge] < pos_26_6[prev]
                            && (pos_26_6[edge2] - pos_26_6[prev]).abs() > 16
                        {
                            pos_26_6[edge] = pos_26_6[prev];
                        }
                    }
                }
            }
        }

        edges[edge].fitted_pos = pos_26_6[edge] as f64 / fu_to_26_6;
        edges[edge].fitted = true;
        edges[edge2].fitted_pos = pos_26_6[edge2] as f64 / fu_to_26_6;
        edges[edge2].fitted = true;
    }
}

/// Round a 26.6 value to the nearest integer pixel (64 units). Matches
/// FT's `FT_PIX_ROUND` macro (`(x + 32) & ~63`).
#[inline]
fn ft_pix_round(x_26_6: i64) -> i64 {
    (x_26_6 + 32) & !63
}

/// Byte-exact port of FreeType's `af_latin_compute_stem_width`
/// (aflatin.c:3967-4158), smooth-hinting branch only (matches FT
/// `FT_LOAD_TARGET_NORMAL` default, i.e. what the auto-hinter's
/// `smooth` path produces -- which is what we want to reproduce).
///
/// All inputs in 26.6 fixed-point pixels. `width` is the signed
/// distance from the base edge to the linked edge. `base_delta` is the
/// fit shift already applied to the base edge (used for the length
/// compensation when the same-sign double-rounding could collide).
fn af_latin_compute_stem_width(
    width_26_6: i64,
    base_delta_26_6: i64,
    base_round: bool,
    base_serif: bool,
    stem_serif: bool,
    dominant_26_6: i64,
) -> i64 {
    // Smooth hinting: quantize the width lightly.
    let mut dist = width_26_6.abs();
    let sign = width_26_6 < 0;

    // Leave the widths of serifs alone when the stem is a serif attachment
    // and dist < 3px (aflatin.c:3997). We apply this to VERTICAL serifs in
    // FT; in our port the "vertical" flag is implicit (we only run the
    // anchor path for V-edges in the H-dim -> wait, no: V-dim edges are
    // X-axis stems, i.e. horizontal-direction strokes). FT's condition
    // keys on `vertical = (dim == AF_DIMENSION_VERT)` which is the Y-axis;
    // we only take this path for X-axis. Skip the serif-len bypass.
    let _ = stem_serif;
    let _ = base_serif;

    // AF_EDGE_ROUND adjustment (aflatin.c:4002-4006).
    if base_round {
        if dist < 80 {
            dist = 64;
        }
    } else if dist < 56 {
        dist = 56;
    }

    // Dominant-width snap (aflatin.c:4010-4080). Only apply when we have
    // a meaningful dominant width (0 means "no standard width detected").
    if dominant_26_6 > 0 {
        let mut delta = dist - dominant_26_6;
        if delta < 0 {
            delta = -delta;
        }

        if delta < 40 {
            dist = dominant_26_6;
            if dist < 48 {
                dist = 48;
            }
        } else if dist < 3 * 64 {
            // Smooth quantisation (aflatin.c:4032-4046).
            let delta = dist & 63;
            dist &= -64_i64;
            if delta < 10 {
                dist += delta;
            } else if delta < 32 {
                dist += 10;
            } else if delta < 54 {
                dist += 54;
            } else {
                dist += delta;
            }
        } else {
            // Wide stem: round with ppem-scaled compensation for
            // double-rounding (aflatin.c:4047-4079). We don't have the
            // caller's ppem here; FT's compensation is ppem-sensitive but
            // small (0 for ppem >= 30). For the X-axis path at body-text
            // sizes (ppem ~10-20) FT's bdelta term rarely exceeds a few
            // 1/64 px and the aggregate effect is negligible vs the
            // dominant-width snap above. We skip it; stem-diag data shows
            // the +22 wide-round already dominates for these cases.
            let _ = base_delta_26_6;
            dist = (dist + 32) & !63_i64;
        }
    }

    if sign {
        -dist
    } else {
        dist
    }
}

/// Round a stem width to integer pixels, with sub-pixel stems forced to 1px.
/// Used by the legacy strong-hint path (Y-axis cascade); X-axis anchor uses
/// the smooth variant below.
fn round_stem_width(width_px: f64) -> f64 {
    if width_px < 1.0 {
        1.0
    } else {
        width_px.round().max(1.0)
    }
}

/// Legacy asymmetric-rounding stem width used by the Y-axis cascade
/// (kept for golden compatibility and because Y-axis pixel snapping with
/// integer stem heights matches what the surrounding grid-fit assumes).
/// For the X-axis anchor path use [`af_latin_compute_stem_width`]
/// directly, which is byte-exact with FreeType.
fn compute_target_width(edge_pos: f64, link_pos: f64, dominant_width: f64, scale: f64) -> f64 {
    let stem_width = (edge_pos - link_pos).abs();
    let stem_width_px = stem_width * scale;
    let dominant_px = dominant_width * scale;

    if dominant_px > 0.0 && (stem_width_px - dominant_px).abs() < 0.5 {
        round_stem_width(dominant_px)
    } else if dominant_px > 0.0 && stem_width_px > dominant_px {
        (stem_width_px + 0.25).floor().max(1.0)
    } else if dominant_px > 0.0 {
        (stem_width_px + 0.75).floor().max(1.0)
    } else {
        round_stem_width(stem_width_px)
    }
}

/// Step 3: Fit serif edges by shifting with their base edge.
fn fit_serif_edges(edges: &mut [Edge]) {
    let n = edges.len();
    for edge_idx in 0..n {
        if edges[edge_idx].fitted {
            continue;
        }
        if let Some(base_idx) = edges[edge_idx].serif {
            if base_idx < n && edges[base_idx].fitted {
                let delta = edges[base_idx].fitted_pos - edges[base_idx].pos;
                edges[edge_idx].fitted_pos = edges[edge_idx].pos + delta;
                edges[edge_idx].fitted = true;
            }
        }
    }
}

/// Step 4: Fit remaining unfitted edges by interpolation between neighbors.
fn fit_remaining_edges(edges: &mut [Edge], fitted: &mut Vec<(f64, f64)>) {
    let n = edges.len();
    if n == 0 {
        return;
    }

    // Collect fitted edges sorted by original position.
    fitted.clear();
    for edge in edges.iter() {
        if edge.fitted {
            fitted.push((edge.pos, edge.fitted_pos));
        }
    }
    fitted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    if fitted.is_empty() {
        return;
    }

    #[allow(clippy::needless_range_loop)]
    for edge_idx in 0..n {
        if edges[edge_idx].fitted {
            continue;
        }

        let pos = edges[edge_idx].pos;

        // Find bracketing fitted edges.
        let idx = fitted.partition_point(|&(orig, _)| orig < pos);

        let fitted_pos = if idx == 0 {
            // Before all fitted: shift by first fitted's delta.
            let (orig, fit) = fitted[0];
            pos + (fit - orig)
        } else if idx >= fitted.len() {
            // After all fitted: shift by last fitted's delta.
            let (orig, fit) = fitted[fitted.len() - 1];
            pos + (fit - orig)
        } else {
            // Between two fitted: interpolate proportionally.
            let (lo_orig, lo_fit) = fitted[idx - 1];
            let (hi_orig, hi_fit) = fitted[idx];
            let span = hi_orig - lo_orig;
            if span.abs() < 0.001 {
                pos + (lo_fit - lo_orig)
            } else {
                let t = (pos - lo_orig) / span;
                lo_fit + t * (hi_fit - lo_fit)
            }
        };

        edges[edge_idx].fitted_pos = fitted_pos;
        edges[edge_idx].fitted = true;
    }
}

/// Diagnostic probe of the auto-hinter's X-axis output for a single glyph.
///
/// Returns the inputs and outputs of the X-axis stem fit in pixel units,
/// intended for side-by-side comparison against FreeType's `slot->lsb_delta`
/// / `slot->rsb_delta` / `slot->advance.x` via `tools/cursor-pair-diagnose`.
///
/// The `v_edges` field carries the pre-fit (`orig_pos_px`) and post-fit
/// (`fitted_pos_px`) positions of every X-axis (vertical-dimension) edge,
/// so the caller can distinguish:
///   - candidate (b) stem-fit divergence: edge-by-edge `fitted_pos - orig_pos`
///     disagreement with FT when run on the same outline;
///   - candidate (c) delta-emission timing: the bbox-derived
///     `lsb_delta_px`/`rsb_delta_px` vs edge-derived shifts.
///
/// Read-only. Gated on `cursor-diag` feature so it never links into
/// non-diagnostic builds.
#[cfg(feature = "cursor-diag")]
pub mod probe {
    use super::*;
    use crate::auto_hinter::{auto_hint_glyph_axes, segments, HintAxes};

    /// Per-edge pre/post-fit positions, in pixels.
    #[derive(Debug, Clone)]
    pub struct ProbeEdge {
        /// Edge position before fitting, in pixels.
        pub orig_pos_px: f64,
        /// Edge position after fitting, in pixels. Equal to `orig_pos_px` if
        /// `fitted == false`.
        pub fitted_pos_px: f64,
        /// Whether this edge was grid-fit.
        pub fitted: bool,
    }

    /// Structured output of `probe_glyph_cursor`.
    ///
    /// All pixel fields are in device pixels at the requested ppem (i.e.
    /// font-unit * ppem / units_per_em).
    #[derive(Debug, Clone)]
    pub struct ProbeResult {
        /// Advance width in pixels (font unit * scale). Not touched by the
        /// hinter; this is the raw hmtx/charstring value scaled.
        pub advance_px: f64,
        /// Font-unit left-side-bearing delta reported by the X-axis fit,
        /// scaled to pixels. Analogous to FT `slot->lsb_delta / 64.0`.
        pub lsb_delta_px: f64,
        /// Font-unit right-side-bearing delta scaled to pixels.
        pub rsb_delta_px: f64,
        /// Pre-fit outline min-X, in pixels.
        pub orig_xmin_px: f64,
        /// Pre-fit outline max-X, in pixels.
        pub orig_xmax_px: f64,
        /// Post-fit outline min-X, in pixels.
        pub hinted_xmin_px: f64,
        /// Post-fit outline max-X, in pixels.
        pub hinted_xmax_px: f64,
        /// Pre/post-fit positions of every X-axis edge.
        pub v_edges: Vec<ProbeEdge>,
    }

    /// Run the auto-hinter with both-axis fitting and capture diagnostic
    /// values. `contours` in font units, `metrics` the font's GlobalMetrics,
    /// `advance_fu` the hmtx/charstring advance width in font units,
    /// `scale = ppem / units_per_em`.
    ///
    /// The result mirrors what `auto_hint_glyph_axes(_, _, scale, Both)`
    /// computes, plus the per-edge pre/post-fit positions that the public
    /// API flattens away.
    pub fn probe_glyph_cursor(
        contours: &[Vec<(f64, f64, bool)>],
        metrics: &GlobalMetrics,
        advance_fu: f64,
        scale: f64,
    ) -> ProbeResult {
        // Re-run the full both-axis hint to pick up the bbox-derived deltas.
        let hinted = auto_hint_glyph_axes(contours, metrics, scale, HintAxes::Both);

        // Re-detect and re-fit X-axis edges so we can capture per-edge
        // orig/fitted positions. Mirrors the path inside
        // `auto_hint_glyph_axes` verbatim except we keep the edge vector.
        let analysis_contours = segments::tuples_to_contours(contours);
        let mut v_segments = segments::detect_segments(
            &analysis_contours,
            Dimension::Vertical,
            metrics.units_per_em,
        );
        let mut v_edges =
            crate::auto_hinter::edges::detect_edges(&mut v_segments, Dimension::Vertical, scale);
        let x_mode = match std::env::var("UDOC_XAXIS_FIT_MODE").ok().as_deref() {
            Some("cascade") => AnchorMode::Cascade,
            _ => AnchorMode::Anchor,
        };
        fit_edges_with(&mut v_edges, metrics, scale, x_mode);

        let probe_edges: Vec<ProbeEdge> = v_edges
            .iter()
            .map(|e| ProbeEdge {
                orig_pos_px: e.pos * scale,
                fitted_pos_px: e.fitted_pos * scale,
                fitted: e.fitted,
            })
            .collect();

        // Outline bbox pre- and post-fit, in pixels.
        let (mut orig_xmin, mut orig_xmax) = (f64::MAX, f64::MIN);
        for contour in contours {
            for &(x, _, _) in contour {
                if x < orig_xmin {
                    orig_xmin = x;
                }
                if x > orig_xmax {
                    orig_xmax = x;
                }
            }
        }
        let (mut hinted_xmin, mut hinted_xmax) = (f64::MAX, f64::MIN);
        for contour in &hinted.contours {
            for &(x, _, _) in contour {
                if x < hinted_xmin {
                    hinted_xmin = x;
                }
                if x > hinted_xmax {
                    hinted_xmax = x;
                }
            }
        }

        ProbeResult {
            advance_px: advance_fu * scale,
            lsb_delta_px: hinted.lsb_delta_fu * scale,
            rsb_delta_px: hinted.rsb_delta_fu * scale,
            orig_xmin_px: orig_xmin * scale,
            orig_xmax_px: orig_xmax * scale,
            hinted_xmin_px: hinted_xmin * scale,
            hinted_xmax_px: hinted_xmax * scale,
            v_edges: probe_edges,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto_hinter::edges;
    use crate::auto_hinter::metrics::BlueZone;
    use crate::auto_hinter::segments;

    #[test]
    fn stem_edges_snap_to_pixel_grid() {
        // Rectangle stem: left at x=50, right at x=150 (width=100 font units).
        // At scale=0.04 (40px font), width = 4px. Should round to 4px.
        let pts: Vec<(f64, f64, bool)> = vec![
            (50.0, 0.0, true),
            (50.0, 700.0, true),
            (150.0, 700.0, true),
            (150.0, 0.0, true),
        ];
        let contours = segments::tuples_to_contours(&[pts]);
        let mut segs = segments::detect_segments(&contours, segments::Dimension::Vertical, 1000);
        let scale = 0.04;
        let mut edge_list = edges::detect_edges(&mut segs, segments::Dimension::Vertical, scale);

        let metrics = GlobalMetrics {
            units_per_em: 1000,
            blue_zones: Vec::new(),
            dominant_h_width: 0.0,
            dominant_v_width: 100.0,
        };

        fit_edges(&mut edge_list, &metrics, scale);

        // All edges should be fitted.
        assert!(
            edge_list.iter().all(|e| e.fitted),
            "not all edges were fitted"
        );

        // Fitted positions should be on pixel boundaries (integer in pixel coords).
        for edge in &edge_list {
            let px = edge.fitted_pos * scale;
            let frac = px - px.round();
            assert!(
                frac.abs() < 0.01,
                "edge at {:.1} fu fitted to {:.2} px, not grid-aligned (frac={:.3})",
                edge.pos,
                px,
                frac
            );
        }
    }

    #[test]
    fn blue_zone_snapping() {
        // Edge near baseline (y=0) should snap to blue zone.
        let metrics = GlobalMetrics {
            units_per_em: 1000,
            blue_zones: vec![BlueZone {
                reference: 0.0,
                overshoot: -15.0,
                is_x_height: false,
            }],
            dominant_h_width: 80.0,
            dominant_v_width: 80.0,
        };

        let mut edge_list = vec![Edge {
            pos: 3.0, // slightly above baseline
            dim: Dimension::Horizontal,
            fitted_pos: 3.0,
            fitted: false,
            blue_zone: None,
            link: None,
            serif: None,
            flags: 0,
            segment_count: 1,
            is_round: false,
            is_crossbar: false,
        }];

        fit_edges(&mut edge_list, &metrics, 0.04);

        assert!(edge_list[0].fitted, "edge should be fitted");
        assert!(
            edge_list[0].blue_zone.is_some(),
            "edge should be assigned to blue zone"
        );
    }
}

//! Microbench for the auto-hinter's per-glyph fitting wall time.
//!
//! Loads Liberation Sans Regular, iterates over the first 100 covered ASCII
//! glyphs, and times `auto_hinter::auto_hint_glyph_axes(...)` against them
//! for a configurable number of iterations at 14px (150 DPI body text).
//!
//! Built only as a dev tool; not part of the shipped API. Compare timings
//! pre- and post-scratch-pool to quantify the speedup.
//!
//! Run with:
//!     cargo run --release -p udoc-render --example bench_hinter --features test-internals

#[cfg(not(feature = "test-internals"))]
fn main() {
    eprintln!(
        "bench_hinter needs --features test-internals so it can call the\n\
         internal hinter modules. Re-run with --features test-internals."
    );
    std::process::exit(2);
}

#[cfg(feature = "test-internals")]
fn main() {
    use std::time::Instant;
    use udoc_font::ttf::TrueTypeFont;
    use udoc_render::auto_hinter::{
        auto_hint_glyph_axes,
        metrics::{BlueZone, GlobalMetrics},
        HintAxes,
    };

    let font_bytes: &[u8] = include_bytes!("../../udoc-font/assets/LiberationSans-Regular.ttf");
    let font = TrueTypeFont::from_bytes(font_bytes).expect("parse Liberation Sans");
    let upm = font.units_per_em();

    // Static metrics approximating what the hinter would see at body text.
    // Exact values do not matter for timing; we just need a representative
    // shape of blue_zones so the fitter doesn't short-circuit.
    let metrics = GlobalMetrics {
        units_per_em: upm,
        blue_zones: vec![
            BlueZone {
                reference: 0.0,
                overshoot: -10.0,
                is_x_height: false,
            },
            BlueZone {
                reference: 520.0,
                overshoot: 535.0,
                is_x_height: true,
            },
            BlueZone {
                reference: 720.0,
                overshoot: 735.0,
                is_x_height: false,
            },
        ],
        dominant_h_width: 60.0,
        dominant_v_width: 80.0,
    };

    // Collect 100 ASCII glyphs from 0x20..0x7F, skipping missing ones.
    let mut glyphs: Vec<Vec<Vec<(f64, f64, bool)>>> = Vec::new();
    for ch in 0x20u32..0x7Fu32 {
        let Some(ch) = char::from_u32(ch) else {
            continue;
        };
        let Some(gid) = font.glyph_id(ch) else {
            continue;
        };
        if gid == 0 {
            continue;
        }
        let Some(outline) = font.glyph_outline(gid) else {
            continue;
        };
        let contours: Vec<Vec<(f64, f64, bool)>> = outline
            .contours
            .iter()
            .map(|c| c.points.iter().map(|p| (p.x, p.y, p.on_curve)).collect())
            .collect();
        if !contours.is_empty() {
            glyphs.push(contours);
        }
        if glyphs.len() >= 100 {
            break;
        }
    }
    eprintln!(
        "loaded {} glyphs from Liberation Sans Regular (UPM={})",
        glyphs.len(),
        upm
    );

    // 150 DPI body text = 14 pt -> ~14 px. scale = 14 / upm.
    let scale = 14.0f64 / upm as f64;

    // Warmup: prime allocations and CPU caches.
    for _ in 0..5 {
        for contours in &glyphs {
            let _ = auto_hint_glyph_axes(contours, &metrics, scale, HintAxes::Both);
        }
    }

    // Timed loop: take the minimum of several trials to filter out
    // scheduler noise / thermal throttling. Each trial runs ITERS outer
    // passes across all glyphs.
    const TRIALS: u32 = 15;
    const ITERS: u32 = 400;
    let mut best_ns = u128::MAX;
    let mut guard = 0u64;
    for _ in 0..TRIALS {
        let start = Instant::now();
        for _ in 0..ITERS {
            for contours in &glyphs {
                let h = auto_hint_glyph_axes(contours, &metrics, scale, HintAxes::Both);
                guard = guard.wrapping_add(h.contours.len() as u64);
            }
        }
        let elapsed_ns = start.elapsed().as_nanos();
        if elapsed_ns < best_ns {
            best_ns = elapsed_ns;
        }
    }
    std::hint::black_box(guard);

    let total_calls = ITERS as f64 * glyphs.len() as f64;
    let ns_per_call = best_ns as f64 / total_calls;
    println!(
        "hinter best-of-{} wall: {:.3} ms over {} calls ({} glyphs x {} iters)",
        TRIALS,
        best_ns as f64 / 1_000_000.0,
        total_calls as u64,
        glyphs.len(),
        ITERS
    );
    println!(
        "per-glyph: {:.0} ns ({:.3} us)  [min over {} trials]",
        ns_per_call,
        ns_per_call / 1000.0,
        TRIALS
    );
}

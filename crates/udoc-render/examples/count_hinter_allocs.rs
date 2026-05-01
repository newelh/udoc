//! Count global allocator calls inside the auto-hinter's hot path.
//!
//! Wraps `std::alloc::System` with a counting shim (atomic counters) so we
//! can assert that post-warmup glyph calls do not allocate. Run with:
//!
//!     cargo run --release -p udoc-render --example count_hinter_allocs --features test-internals
//!
//! The `alloc` / `dealloc` counters reset between the warmup and measurement
//! phases. After the pool is warm, the hinter should produce zero fresh
//! allocations per glyph aside from the unavoidable output `HintedOutline`
//! (one outer `Vec` plus one inner `Vec` per contour) the caller owns.

#[cfg(not(feature = "test-internals"))]
fn main() {
    eprintln!(
        "count_hinter_allocs needs --features test-internals so it can call\n\
         the internal hinter modules. Re-run with --features test-internals."
    );
    std::process::exit(2);
}

#[cfg(feature = "test-internals")]
use std::alloc::{GlobalAlloc, Layout, System};
#[cfg(feature = "test-internals")]
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(feature = "test-internals")]
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "test-internals")]
static DEALLOCS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "test-internals")]
static BYTES: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "test-internals")]
struct CountingAlloc;

#[cfg(feature = "test-internals")]
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        DEALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[cfg(feature = "test-internals")]
#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

#[cfg(feature = "test-internals")]
fn main() {
    use udoc_font::ttf::TrueTypeFont;
    use udoc_render::auto_hinter::{
        auto_hint_glyph_axes,
        metrics::{BlueZone, GlobalMetrics},
        HintAxes,
    };

    let font_bytes: &[u8] = include_bytes!("../../udoc-font/assets/LiberationSans-Regular.ttf");
    let font = TrueTypeFont::from_bytes(font_bytes).expect("parse Liberation Sans");
    let upm = font.units_per_em();

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

    // Collect 100 glyphs.
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
    }

    let scale = 14.0f64 / upm as f64;

    // Warmup: run each glyph a few times to grow all scratch buffers to
    // their steady-state capacity. The first call per glyph will allocate
    // the buffers inside `ScratchBuffers`; subsequent calls reuse them.
    for _ in 0..3 {
        for contours in &glyphs {
            let _ = auto_hint_glyph_axes(contours, &metrics, scale, HintAxes::Both);
        }
    }

    // Measure: reset the counters and run one more pass. Every call after
    // warmup should only allocate the caller-visible `HintedOutline`
    // output. Inside the hinter itself, the scratch pool should be cold.
    let start_allocs = ALLOCS.load(Ordering::Relaxed);
    let start_deallocs = DEALLOCS.load(Ordering::Relaxed);
    let start_bytes = BYTES.load(Ordering::Relaxed);

    let mut total_contours = 0usize;
    let mut guard = 0u64;
    for contours in &glyphs {
        let h = auto_hint_glyph_axes(contours, &metrics, scale, HintAxes::Both);
        total_contours += h.contours.len();
        for c in &h.contours {
            guard = guard.wrapping_add(c.len() as u64);
        }
    }

    let end_allocs = ALLOCS.load(Ordering::Relaxed);
    let end_deallocs = DEALLOCS.load(Ordering::Relaxed);
    let end_bytes = BYTES.load(Ordering::Relaxed);
    std::hint::black_box(guard);

    let allocs = end_allocs - start_allocs;
    let deallocs = end_deallocs - start_deallocs;
    let bytes = end_bytes - start_bytes;
    let calls = glyphs.len();

    println!(
        "post-warmup: {} hinter calls, {} contours returned",
        calls, total_contours
    );
    println!(
        "  allocs:   {} total, {:.2} per call",
        allocs,
        allocs as f64 / calls as f64
    );
    println!(
        "  deallocs: {} total, {:.2} per call",
        deallocs,
        deallocs as f64 / calls as f64
    );
    println!(
        "  bytes:    {} total, {:.1} per call",
        bytes,
        bytes as f64 / calls as f64
    );

    // The caller-visible output `HintedOutline.contours` is owned by the
    // caller, so each call MUST produce at least `1 + contours_per_glyph`
    // allocations (one outer Vec + one inner Vec per contour). Anything
    // beyond that is scratch leakage that the pool should have caught.
    let expected_output_allocs = calls + total_contours;
    println!(
        "  expected output allocs (1 outer + 1 per contour): {}",
        expected_output_allocs
    );
    if allocs <= expected_output_allocs {
        println!("OK: hinter internal path is allocation-free after warmup");
    } else {
        println!(
            "WARN: {} excess allocations beyond caller-visible output",
            allocs - expected_output_allocs
        );
    }
}

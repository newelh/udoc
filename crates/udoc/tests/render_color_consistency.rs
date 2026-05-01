//! Chromatic-fringing regression tests for the page compositor (#183).
//!
//! Every udoc-rendered page used to emit 50k-200k colored pixels on
//! nominally-mono-black content, because a subpixel-LCD rasterizer path was
//! writing 3-byte-per-pixel coverage into the final RGB buffer. Fixed
//! by disabling the LCD path by default (`use_subpixel = false`); locked in
//! here by rendering a hand-crafted mono-black page and asserting the
//! colored-pixel count stays below 1000.
//!
//! Bug symptom (in the sense of issue #183): "colored" = `max(R,G,B) - min(R,G,B) > 50`.
//! mupdf and pdftoppm emit 0 colored pixels on black-on-white text; udoc
//! must emit < 1000 to pass the  must-have gate.
//!
//! Two tests:
//!   - `mono_black_text_has_no_fringing`: all-black text on white. Expect 0
//!     (or very near 0) colored pixels. This is the acceptance gate.
//!   - `mono_red_text_keeps_deliberate_color`: control. Renders explicit-red
//!     text on white. Expects colored pixels WHERE THE TEXT IS (otherwise we
//!     would be silently discarding the span's color attribute) but zero
//!     colored pixels outside the text bbox.

use udoc::render::font_cache::FontCache;
use udoc::render::render_page_rgb;
use udoc_core::document::presentation::Color;
use udoc_core::document::presentation::{PageDef, PositionedSpan, Presentation};
use udoc_core::document::Document;
use udoc_core::geometry::BoundingBox;

const DPI: u32 = 150;
const PAGE_WIDTH_PT: f64 = 612.0;
const PAGE_HEIGHT_PT: f64 = 792.0;

/// Counts pixels where `max(R,G,B) - min(R,G,B) > threshold`, matching the
/// metric from issue #183.
fn count_colored(rgb: &[u8], threshold: u8) -> usize {
    rgb.chunks_exact(3)
        .filter(|p| {
            let (mn, mx) = p
                .iter()
                .fold((255u8, 0u8), |(mn, mx), &v| (mn.min(v), mx.max(v)));
            mx - mn > threshold
        })
        .count()
}

fn mono_black_doc() -> Document {
    let mut spans = Vec::new();
    // Sprinkle a few lines of body text and a title; more than one span
    // exercises the font cache across different sizes and cursor bins.
    let mut add = |text: &str, y_top: f64, size: f64| {
        let mut s = PositionedSpan::new(
            text.to_string(),
            BoundingBox::new(72.0, y_top - size, 72.0 + 0.5 * PAGE_WIDTH_PT, y_top),
            0,
        );
        s.font_size = Some(size);
        s.color = None; // explicit: default black.
        spans.push(s);
    };
    add("Chromatic fringing regression test", 740.0, 18.0);
    add("The quick brown fox jumps over the lazy dog.", 700.0, 12.0);
    add("Pack my box with five dozen liquor jugs.", 680.0, 12.0);
    add("Sphinx of black quartz, judge my vow.", 660.0, 12.0);
    add("How vexingly quick daft zebras jump!", 640.0, 12.0);
    add("0123456789 !@#$%^&*() {}[]|:;<>?,./", 620.0, 12.0);

    let mut doc = Document::new();
    let mut pres = Presentation::default();
    pres.pages
        .push(PageDef::new(0, PAGE_WIDTH_PT, PAGE_HEIGHT_PT, 0));
    pres.raw_spans = spans;
    doc.presentation = Some(pres);
    doc
}

fn mono_red_doc() -> Document {
    let mut s = PositionedSpan::new(
        "All red, all the time.".to_string(),
        BoundingBox::new(72.0, 688.0, 400.0, 700.0),
        0,
    );
    s.font_size = Some(12.0);
    s.color = Some(Color::rgb(200, 0, 0));

    let mut doc = Document::new();
    let mut pres = Presentation::default();
    pres.pages
        .push(PageDef::new(0, PAGE_WIDTH_PT, PAGE_HEIGHT_PT, 0));
    pres.raw_spans = vec![s];
    doc.presentation = Some(pres);
    doc
}

#[test]
fn mono_black_text_has_no_fringing() {
    let doc = mono_black_doc();
    let mut cache = FontCache::empty();
    let (rgb, w, h) = render_page_rgb(&doc, 0, DPI, &mut cache).expect("render");
    assert_eq!(rgb.len(), (w * h * 3) as usize);

    // Sprint-50 acceptance gate: colored-pixel count on a
    // mono-black test page must be < 1000 (down from 50k-200k pre-fix).
    // We use the same threshold-50 metric as issue #183.
    let colored = count_colored(&rgb, 50);
    eprintln!(
        "mono_black_text_has_no_fringing: {colored} colored pixels out of {} total",
        (w * h) as usize
    );
    assert!(
        colored < 1000,
        "mono-black page produced {colored} colored pixels (> 1000); \
         chromatic fringing regression (issue #183). \
         Image dims {w}x{h} = {} total pixels.",
        (w * h) as usize
    );

    // A grayscale-compositing renderer on this content (no images, no
    // shapes, no color text) should actually produce ZERO colored pixels.
    // The 1000-pixel slack is a guard-rail against antialiasing rounding
    // oddities near letter edges; in practice we expect 0.
    assert!(
        colored < 50,
        "mono-black page produced {colored} colored pixels, expected ~0. \
         This is not a strict fringing failure (< 1000 still passes the \
          gate) but suggests a rounding-based divergence worth \
         investigating."
    );
}

#[test]
fn mono_red_text_keeps_deliberate_color() {
    // Control test: explicit-red text (color=RGB(200,0,0)) must actually
    // produce red pixels in the output. If this fails we've gone too far
    // in the "suppress channel divergence" direction and accidentally
    // routed all text through a luminance-only path.
    let doc = mono_red_doc();
    let mut cache = FontCache::empty();
    let (rgb, w, h) = render_page_rgb(&doc, 0, DPI, &mut cache).expect("render");

    // Count pixels that look red-leaning: R significantly higher than G/B.
    let red_leaning = rgb
        .chunks_exact(3)
        .filter(|p| (p[0] as i32) > (p[1] as i32) + 30 && (p[0] as i32) > (p[2] as i32) + 30)
        .count();
    assert!(
        red_leaning > 100,
        "red-text page produced only {red_leaning} red-leaning pixels; \
         the deliberate-color path appears to have been broken along \
         with the fringing fix."
    );

    // Total pixels outside any text region must still be fully white.
    // Check the top 100 rows (y=0..100): the text is at y~700pt = 100..200px
    // from the top at 150 DPI, so rows 0-99 must be untouched white.
    for y in 0..100 {
        for x in 0..w as usize {
            let idx = (y * w as usize + x) * 3;
            assert_eq!(
                (rgb[idx], rgb[idx + 1], rgb[idx + 2]),
                (255, 255, 255),
                "pixel ({x},{y}) was touched by red-text render; \
                 expected pristine white background."
            );
        }
    }
    let _ = h;
}

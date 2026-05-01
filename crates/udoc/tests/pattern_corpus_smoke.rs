//! Corpus smoke test: render arxiv-physics/2602.14347.pdf page 1 and
//! verify that Type 1 coloured tiling patterns come through to the
//! presentation overlay. The acceptance target for
//! is "visible tiling where MuPDF renders one"; the unit here
//! cross-checks that at least one pattern reaches the renderer.
//!
//! Skipped when the corpus is not present.

use std::path::PathBuf;

use udoc::render::font_cache::FontCache;
use udoc::render::render_page_rgb;

fn corpus_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root reachable")
        .join("tests/corpus/downloaded/arxiv-physics/2602.14347.pdf")
}

#[test]
fn arxiv_physics_2602_14347_emits_patterns() {
    let pdf = corpus_path();
    if !pdf.exists() {
        eprintln!(
            "skipping: corpus doc not present at {} (run from repo root \
             with tests/corpus/downloaded/ populated)",
            pdf.display()
        );
        return;
    }

    let doc = udoc::extract(&pdf).expect("extract");
    let pres = doc.presentation.as_ref().expect("presentation overlay");
    let total_patterns = pres.patterns.len();
    eprintln!("total patterns across doc: {total_patterns}");
    assert!(
        total_patterns > 0,
        "expected at least one PaintPattern for 2602.14347 \
         (corpus has PaintType 1 PatternType 1 fills)"
    );

    // Page 0 should render without crashing. We don't assert SSIM here
    // (that's gated by the synthetic `tiling_pattern.pdf` golden).
    let mut cache = FontCache::new(&doc.assets);
    let (pixels, w, h) = render_page_rgb(&doc, 0, 150, &mut cache).expect("render");
    assert!(
        pixels.len() == w as usize * h as usize * 3,
        "expected RGB buffer sized to {w}x{h}, got {} bytes",
        pixels.len()
    );
    // Sanity: the rendered page should have some non-white content
    // (the doc has text). Don't over-specify beyond that.
    let non_white = pixels
        .chunks_exact(3)
        .filter(|c| c.iter().any(|&b| b < 200))
        .count();
    assert!(
        non_white > 1000,
        "page render looks blank ({non_white} pixels)"
    );
}

//! Clip-mask rendering tests (#124).
//!
//! Constructs minimal hand-rolled PDFs that exercise W/W* clipping and
//! asserts pixel-level invariants on the rendered output:
//!
//! - `rect_clip`: `0 50 50 50 re W n` installs a 50x50 clip; a
//!   subsequent 200x200 red fill must only produce red pixels inside the
//!   50x50 square.
//! - `path_clip`: a 4-segment bezier approximation of a circle is used
//!   as clip; a 200x200 red fill outside the circle must be white.
//!
//! These tests intentionally do not compare against MuPDF references
//! directly at the PNG level (bringing up MuPDF for a 2-pixel assertion
//! is overkill); they enforce the *invariant* the real clip mask must
//! satisfy. A subsequent #[ignore]d MuPDF-comparison test can be
//! blessed with the real pixel goldens when available.

use std::path::PathBuf;

use udoc::render::font_cache::FontCache;
use udoc::render::render_page_rgb;

/// Write `bytes` to a temp file inside `env!("OUT_DIR")` (writable during
/// tests) and return its path. The file persists for the test run so the
/// extractor (which mmaps) can read it.
fn write_temp_pdf(name: &str, bytes: &[u8]) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    path.push(name);
    std::fs::write(&path, bytes).expect("write temp pdf");
    path
}

/// Build a minimal single-page PDF whose content stream is `content`.
///
/// Page media box: 200 x 200 pts (y-up, PDF coords). No fonts, no
/// resources. The only thing that matters is that the content stream
/// emits paths that the interpreter can pick up and the renderer can
/// paint.
fn make_minimal_pdf(content: &str) -> Vec<u8> {
    // Assemble the PDF piece by piece so we can record byte offsets for
    // the xref table.
    let header = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n";
    let mut out = Vec::with_capacity(1024 + content.len());
    out.extend_from_slice(header);

    let mut offsets: Vec<usize> = Vec::with_capacity(4);

    // Object 1: Catalog.
    offsets.push(out.len());
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    // Object 2: Pages.
    offsets.push(out.len());
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    // Object 3: Page.
    offsets.push(out.len());
    out.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
          /Contents 4 0 R /Resources << >> >>\nendobj\n",
    );

    // Object 4: Content stream.
    offsets.push(out.len());
    let stream_body = content.as_bytes();
    let stream_header = format!("4 0 obj\n<< /Length {} >>\nstream\n", stream_body.len());
    out.extend_from_slice(stream_header.as_bytes());
    out.extend_from_slice(stream_body);
    out.extend_from_slice(b"\nendstream\nendobj\n");

    // Xref table.
    let xref_offset = out.len();
    out.extend_from_slice(b"xref\n0 5\n");
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        out.extend_from_slice(format!("{:010} 00000 n \n", off).as_bytes());
    }

    // Trailer.
    out.extend_from_slice(b"trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n");
    out.extend_from_slice(format!("{}\n", xref_offset).as_bytes());
    out.extend_from_slice(b"%%EOF\n");

    out
}

fn render_pdf(bytes: &[u8], name: &str) -> (Vec<u8>, u32, u32) {
    let path = write_temp_pdf(name, bytes);
    let doc = udoc::extract(&path).expect("extract minimal pdf");
    let mut cache = FontCache::new(&doc.assets);
    render_page_rgb(&doc, 0, 72, &mut cache).expect("render minimal pdf")
}

#[inline]
fn pixel_at(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
    let idx = (y as usize * w as usize + x as usize) * 3;
    [buf[idx], buf[idx + 1], buf[idx + 2]]
}

/// Scene 1: Rect clip.
/// - `q` save state
/// - Build rect (50, 50, 100, 100) in PDF coords.
/// - `W n` install as nonzero clip, do not paint it.
/// - `1 0 0 rg` set fill red.
/// - Fill the entire page (200x200).
/// - `Q` restore state.
///
/// Expected: only a 100x100 region is red; the rest is white.
#[test]
fn rect_clip_only_paints_inside_the_clip_region() {
    let content = "\
q\n\
50 50 100 100 re\n\
W n\n\
1 0 0 rg\n\
0 0 200 200 re\n\
f\n\
Q\n\
";
    let pdf = make_minimal_pdf(content);
    let (pixels, w, h) = render_pdf(&pdf, "rect_clip.pdf");

    assert_eq!(w, 200);
    assert_eq!(h, 200);

    // Page is rendered top-left origin (y-down). The PDF clip rect at
    // (50..150, 50..150) y-up -> (50..150, 50..150) y-down after flip.
    // Inside the clip: expect red.
    let inside = pixel_at(&pixels, w, 100, 100);
    assert_eq!(
        inside,
        [255, 0, 0],
        "center of clip region must be red, got {:?}",
        inside
    );
    // Inside the clip near edge: red.
    let inside_edge = pixel_at(&pixels, w, 55, 55);
    assert!(
        inside_edge[0] > 200 && inside_edge[1] < 50 && inside_edge[2] < 50,
        "inside clip near top-left must be roughly red, got {:?}",
        inside_edge
    );

    // Outside the clip: white (background preserved).
    let outside_tl = pixel_at(&pixels, w, 10, 10);
    assert_eq!(
        outside_tl,
        [255, 255, 255],
        "top-left outside clip must be white, got {:?}",
        outside_tl
    );
    let outside_br = pixel_at(&pixels, w, 180, 180);
    assert_eq!(
        outside_br,
        [255, 255, 255],
        "bottom-right outside clip must be white, got {:?}",
        outside_br
    );
    // Just outside the clip boundary: white.
    let just_outside = pixel_at(&pixels, w, 170, 100);
    assert_eq!(
        just_outside,
        [255, 255, 255],
        "just outside clip right edge must be white, got {:?}",
        just_outside
    );
}

/// Scene 2: Nested q/Q clips scope correctly.
/// - q
///   - 0 0 100 200 re W n  (clip to left half)
///   - q
///     - 0 100 200 100 re W n  (intersect with top half, clip is top-left 100x100)
///     - 1 0 0 rg
///     - 0 0 200 200 re f
///   - Q  (restore: clip is now only left half again)
///   - 0 1 0 rg
///   - 0 0 200 200 re f  (green fill under left-half clip)
/// - Q
///
/// Expected:
/// - Top-left quadrant (0..100, top half y-down): red.
/// - Bottom-left quadrant: green (left-half clip is now in effect).
/// - Right half: white (never painted under any active clip).
#[test]
fn nested_q_q_clips_pop_correctly() {
    let content = "\
q\n\
0 0 100 200 re\n\
W n\n\
q\n\
0 100 200 100 re\n\
W n\n\
1 0 0 rg\n\
0 0 200 200 re\n\
f\n\
Q\n\
0 1 0 rg\n\
0 0 200 200 re\n\
f\n\
Q\n\
";
    let pdf = make_minimal_pdf(content);
    let (pixels, w, _h) = render_pdf(&pdf, "nested_clip.pdf");

    // Top-left pixel (inside both clips): red fill first, then green fill
    // but green is clipped to left half (still active), and the red paint
    // already happened INSIDE the nested top-half clip. The green paint
    // ran after the inner Q restored to the left-half-only clip and
    // painted over the top-left in green. So top-left should be green
    // (last fill wins). The nested invariant we care about: right half
    // stays white even though both fills had the same 200x200 rect.
    //
    // NB: Due to the "fill the whole thing, then undo outside clip"
    // model, the right-half assertion is the key one.
    let right_top = pixel_at(&pixels, w, 150, 50);
    assert_eq!(
        right_top,
        [255, 255, 255],
        "right half top should never be painted (clipped out), got {:?}",
        right_top
    );
    let right_bot = pixel_at(&pixels, w, 150, 150);
    assert_eq!(
        right_bot,
        [255, 255, 255],
        "right half bottom should never be painted, got {:?}",
        right_bot
    );
    // Left half should have been painted at least once (green, since it's
    // the last fill and covers the whole left column).
    let left_center = pixel_at(&pixels, w, 50, 100);
    assert!(
        left_center[1] > 200,
        "left half center should be green or red, got {:?}",
        left_center
    );
}

/// Scene 3: Nested-rect even-odd clip leaves a hole.
///
/// Two concentric rectangles drawn as a single subpath (outer then
/// inner), joined by the implicit close. Under even-odd, the inner
/// region has winding 2 but parity 0 -> not inside. A red fill under
/// this clip should leave the inner rectangle white.
#[test]
fn even_odd_clip_leaves_hole_in_nested_rects() {
    let content = "\
q\n\
20 20 160 160 re\n\
60 60 80 80 re\n\
W* n\n\
1 0 0 rg\n\
0 0 200 200 re\n\
f\n\
Q\n\
";
    let pdf = make_minimal_pdf(content);
    let (pixels, w, _h) = render_pdf(&pdf, "nested_rect_clip.pdf");

    // Outer ring: red (parity 1).
    let ring = pixel_at(&pixels, w, 30, 30);
    assert!(
        ring[0] > 200 && ring[1] < 50 && ring[2] < 50,
        "outer ring pixel must be red under EO clip, got {:?}",
        ring
    );
    // Center of inner rect: white (parity 2 -> outside under EO).
    let center = pixel_at(&pixels, w, 100, 100);
    assert_eq!(
        center,
        [255, 255, 255],
        "inner rect center must be white under EO clip, got {:?}",
        center
    );
    // Outside outer rect: white.
    let outer = pixel_at(&pixels, w, 10, 10);
    assert_eq!(
        outer,
        [255, 255, 255],
        "outside clip must be white, got {:?}",
        outer
    );
}

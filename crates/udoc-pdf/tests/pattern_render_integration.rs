//! Integration tests for Type 1 coloured tiling pattern rendering
//! (ISO 32000-2 §8.7.3).
//!
//! These tests validate that the PDF content interpreter:
//!   1. Emits `PageTilingPattern` records for Pattern-colorspace fills
//!      where the pattern resource is a Type 1 coloured tiling pattern.
//!   2. Emits a `WarningKind::UnsupportedPatternType` diagnostic and
//!      falls through to the base fill for Type 1 uncoloured and Type 2
//!      shading patterns without crashing.
//!
//! Parser-level tests live in `pattern_parse_integration.rs`.

use std::sync::Arc;

use udoc_pdf::{CollectingDiagnostics, Config, Document, WarningKind};

/// Minimal 1-page PDF with a Type 1 coloured tiling pattern fill.
/// Mirrors `crates/udoc/tests/golden_pattern.rs::build_tiling_pattern_pdf`
/// so the assertions here don't depend on the facade.
fn build_type1_coloured_pdf() -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    let mut offsets: Vec<(u32, usize)> = Vec::new();
    buf.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");

    let push = |buf: &mut Vec<u8>, offsets: &mut Vec<(u32, usize)>, num: u32, body: &str| {
        offsets.push((num, buf.len()));
        buf.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        buf.extend_from_slice(body.as_bytes());
        buf.extend_from_slice(b"\nendobj\n");
    };

    push(
        &mut buf,
        &mut offsets,
        1,
        "<< /Type /Catalog /Pages 2 0 R >>",
    );
    push(
        &mut buf,
        &mut offsets,
        2,
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    );
    push(
        &mut buf,
        &mut offsets,
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 120] \
           /Contents 4 0 R \
           /Resources << \
             /ColorSpace << /Cs1 [/Pattern] >> \
             /Pattern << /P1 5 0 R >> \
           >> >>",
    );
    let content = "q\n/Cs1 cs\n/P1 scn\n50 20 100 80 re\nf\nQ\n";
    offsets.push((4, buf.len()));
    buf.extend_from_slice(format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes());
    buf.extend_from_slice(content.as_bytes());
    buf.extend_from_slice(b"\nendstream\nendobj\n");

    let tile = "1 0 0 rg\n0 0 10 10 re\nf\n";
    offsets.push((5, buf.len()));
    buf.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 \
             /BBox [0 0 10 10] /XStep 10 /YStep 10 \
             /Matrix [1 0 0 1 0 0] /Resources << >> /Length {} >>\nstream\n",
            tile.len()
        )
        .as_bytes(),
    );
    buf.extend_from_slice(tile.as_bytes());
    buf.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_off = buf.len();
    let n_objs = 6;
    buf.extend_from_slice(format!("xref\n0 {}\n", n_objs).as_bytes());
    buf.extend_from_slice(b"0000000000 65535 f \r\n");
    let mut sorted = offsets.clone();
    sorted.sort_by_key(|(n, _)| *n);
    for (_, off) in &sorted {
        buf.extend_from_slice(format!("{:010} 00000 n \r\n", off).as_bytes());
    }
    buf.extend_from_slice(format!("trailer\n<< /Size {n_objs} /Root 1 0 R >>\n").as_bytes());
    buf.extend_from_slice(format!("startxref\n{xref_off}\n%%EOF\n").as_bytes());
    buf
}

/// Same page + resources as `build_type1_coloured_pdf` but the pattern
/// resource is PaintType 2 (uncoloured). this must warn
/// and fall through; the interpreter records no pattern.
fn build_type1_uncoloured_pdf() -> Vec<u8> {
    let mut pdf = build_type1_coloured_pdf();
    // Flip PaintType 1 -> PaintType 2 inside object 5.
    let needle = b"/PaintType 1 ";
    let pos = find(&pdf, needle).expect("PaintType 1 marker present");
    pdf[pos + b"/PaintType ".len()] = b'2';
    pdf
}

/// Shading-pattern variant: object 5 is PatternType 2 with a minimal
/// /Shading dict. Interpreter must emit the unsupported warning.
fn build_type2_shading_pdf() -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    let mut offsets: Vec<(u32, usize)> = Vec::new();
    buf.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");

    let push = |buf: &mut Vec<u8>, offsets: &mut Vec<(u32, usize)>, num: u32, body: &str| {
        offsets.push((num, buf.len()));
        buf.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        buf.extend_from_slice(body.as_bytes());
        buf.extend_from_slice(b"\nendobj\n");
    };

    push(
        &mut buf,
        &mut offsets,
        1,
        "<< /Type /Catalog /Pages 2 0 R >>",
    );
    push(
        &mut buf,
        &mut offsets,
        2,
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    );
    push(
        &mut buf,
        &mut offsets,
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 120] \
           /Contents 4 0 R \
           /Resources << \
             /ColorSpace << /Cs1 [/Pattern] >> \
             /Pattern << /P1 5 0 R >> \
           >> >>",
    );
    let content = "q\n/Cs1 cs\n/P1 scn\n0 0 50 50 re\nf\nQ\n";
    offsets.push((4, buf.len()));
    buf.extend_from_slice(format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes());
    buf.extend_from_slice(content.as_bytes());
    buf.extend_from_slice(b"\nendstream\nendobj\n");

    // PatternType 2 (shading pattern) - not supported.
    push(
        &mut buf,
        &mut offsets,
        5,
        "<< /Type /Pattern /PatternType 2 \
           /Matrix [1 0 0 1 0 0] \
           /Shading << /ShadingType 2 /ColorSpace /DeviceRGB \
                       /Coords [0 0 100 0] \
                       /Function << /FunctionType 2 /Domain [0 1] /N 1 \
                                    /C0 [1 0 0] /C1 [0 0 1] >> >> >>",
    );

    let xref_off = buf.len();
    let n_objs = 6;
    buf.extend_from_slice(format!("xref\n0 {n_objs}\n").as_bytes());
    buf.extend_from_slice(b"0000000000 65535 f \r\n");
    let mut sorted = offsets.clone();
    sorted.sort_by_key(|(n, _)| *n);
    for (_, off) in &sorted {
        buf.extend_from_slice(format!("{:010} 00000 n \r\n", off).as_bytes());
    }
    buf.extend_from_slice(format!("trailer\n<< /Size {n_objs} /Root 1 0 R >>\n").as_bytes());
    buf.extend_from_slice(format!("startxref\n{xref_off}\n%%EOF\n").as_bytes());
    buf
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[test]
fn type1_coloured_emits_tiling_pattern_record() {
    let data = build_type1_coloured_pdf();
    let diag = Arc::new(CollectingDiagnostics::new());
    let cfg = Config::default().with_diagnostics(diag.clone());
    let mut doc = Document::from_bytes_with_config(data, cfg).expect("open");
    let mut page = doc.page(0).expect("page 0");
    let (_paths, _shadings, patterns) = page
        .paths_shadings_and_patterns()
        .expect("extract paint records");
    assert_eq!(patterns.len(), 1, "expected one tiling pattern record");
    let p = &patterns[0];
    assert_eq!(p.resource_name, "P1");
    assert_eq!(p.bbox, [0.0, 0.0, 10.0, 10.0]);
    assert_eq!(p.xstep, 10.0);
    assert_eq!(p.ystep, 10.0);
    assert!(!p.fill_subpaths.is_empty(), "fill region should be set");
    // No parse-level warnings should fire on the happy path.
    assert!(
        !diag
            .warnings()
            .iter()
            .any(|w| w.kind == WarningKind::UnsupportedPatternType),
        "unexpected UnsupportedPatternType warning"
    );
}

#[test]
fn type1_uncoloured_warns_and_does_not_emit_record() {
    let data = build_type1_uncoloured_pdf();
    let diag = Arc::new(CollectingDiagnostics::new());
    let cfg = Config::default().with_diagnostics(diag.clone());
    let mut doc = Document::from_bytes_with_config(data, cfg).expect("open");
    let mut page = doc.page(0).expect("page 0");
    let (paths, _shadings, patterns) = page
        .paths_shadings_and_patterns()
        .expect("extract paint records");
    assert!(
        patterns.is_empty(),
        "Type 1 uncoloured should not emit a tiling record"
    );
    // Fall-through to base fill: the rectangle still rasterizes as a
    // normal PagePath using whatever base color was in effect.
    assert!(
        !paths.is_empty(),
        "base fill fallback should produce at least one paint path"
    );
    let ws = diag.warnings();
    assert!(
        ws.iter()
            .any(|w| w.kind == WarningKind::UnsupportedPatternType),
        "expected UnsupportedPatternType warning, got {:?}",
        ws.iter().map(|w| w.kind).collect::<Vec<_>>()
    );
}

#[test]
fn type2_shading_pattern_warns_and_does_not_emit_record() {
    let data = build_type2_shading_pdf();
    let diag = Arc::new(CollectingDiagnostics::new());
    let cfg = Config::default().with_diagnostics(diag.clone());
    let mut doc = Document::from_bytes_with_config(data, cfg).expect("open");
    let mut page = doc.page(0).expect("page 0");
    let (_paths, _shadings, patterns) = page
        .paths_shadings_and_patterns()
        .expect("extract paint records");
    assert!(
        patterns.is_empty(),
        "Type 2 shading pattern should not emit a tiling record"
    );
    let ws = diag.warnings();
    assert!(
        ws.iter()
            .any(|w| w.kind == WarningKind::UnsupportedPatternType),
        "expected UnsupportedPatternType warning, got {:?}",
        ws.iter().map(|w| w.kind).collect::<Vec<_>>()
    );
}

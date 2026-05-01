//! Integration tests for M-34: CID font /W vs embedded hmtx disagreement
//! diagnostic (#188).
//!
//! Builds a synthetic PDF whose CIDFontType2 descendant embeds a tiny
//! TrueType with known hmtx widths, then drives the diagnostic two ways:
//!
//! 1. `/W` disagrees with hmtx on 10 glyphs by ~50%.
//!    Expected: a `FontMetricsDisagreement` warning that names the font
//!    and mentions the disagreeing-glyph count.
//! 2. `/W` matches hmtx exactly.
//!    Expected: no `FontMetricsDisagreement` warning.

mod common;

use common::PdfBuilder;
use std::sync::Arc;
use udoc_pdf::{CollectingDiagnostics, Config, Document, WarningKind};

/// Number of glyphs in the synthetic font. Chosen to be >= the
/// disagreement threshold with room to spare.
const NUM_GLYPHS: u16 = 20;

/// hmtx advance width assigned to every glyph (font units).
/// With unitsPerEm = 1000 the PDF glyph-space width is numerically equal.
const HMTX_WIDTH: u16 = 500;

/// Build a minimal TrueType font with `NUM_GLYPHS` glyphs, each carrying
/// `HMTX_WIDTH` as its advance. All glyphs are empty (no outlines) which
/// the parser accepts. unitsPerEm = 1000 so the hmtx values survive
/// `aw * 1000 / upem` normalization unchanged.
fn build_known_width_ttf() -> Vec<u8> {
    // Big-endian writers, kept local so this file stays self-contained.
    fn u16_be(buf: &mut Vec<u8>, v: u16) {
        buf.extend_from_slice(&v.to_be_bytes());
    }
    fn i16_be(buf: &mut Vec<u8>, v: i16) {
        buf.extend_from_slice(&v.to_be_bytes());
    }
    fn u32_be(buf: &mut Vec<u8>, v: u32) {
        buf.extend_from_slice(&v.to_be_bytes());
    }

    struct Tab {
        tag: [u8; 4],
        data: Vec<u8>,
    }
    let mut tables: Vec<Tab> = Vec::new();

    // head (54 bytes). Version 1.0, unitsPerEm = 1000, indexToLocFormat = 0 (short).
    let mut head = vec![0u8; 54];
    head[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    head[18..20].copy_from_slice(&1000u16.to_be_bytes());
    head[50..52].copy_from_slice(&0i16.to_be_bytes());
    tables.push(Tab {
        tag: *b"head",
        data: head,
    });

    // hhea (36 bytes). numberOfHMetrics = NUM_GLYPHS.
    let mut hhea = vec![0u8; 36];
    hhea[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    hhea[34..36].copy_from_slice(&NUM_GLYPHS.to_be_bytes());
    tables.push(Tab {
        tag: *b"hhea",
        data: hhea,
    });

    // maxp (6 bytes, version 0.5). numGlyphs = NUM_GLYPHS.
    let mut maxp = vec![0u8; 6];
    maxp[0..4].copy_from_slice(&0x0000_5000u32.to_be_bytes());
    maxp[4..6].copy_from_slice(&NUM_GLYPHS.to_be_bytes());
    tables.push(Tab {
        tag: *b"maxp",
        data: maxp,
    });

    // hmtx: NUM_GLYPHS long horizontal metrics, each (advanceWidth u16, lsb i16).
    let mut hmtx = Vec::with_capacity(NUM_GLYPHS as usize * 4);
    for _ in 0..NUM_GLYPHS {
        u16_be(&mut hmtx, HMTX_WIDTH);
        i16_be(&mut hmtx, 0);
    }
    tables.push(Tab {
        tag: *b"hmtx",
        data: hmtx,
    });

    // cmap: one format-4 subtable (the parser doesn't require cmap data for
    // advance_width lookups, but it does check the table is present).
    let mut cmap = Vec::new();
    u16_be(&mut cmap, 0); // version
    u16_be(&mut cmap, 1); // numTables
                          // encoding record: platform=3, encoding=1, offset=12
    u16_be(&mut cmap, 3);
    u16_be(&mut cmap, 1);
    u32_be(&mut cmap, 12);
    // format-4 minimal: one real segment + 0xFFFF sentinel.
    u16_be(&mut cmap, 4); // format
    u16_be(&mut cmap, 0); // length (unused by our parser)
    u16_be(&mut cmap, 0); // language
    u16_be(&mut cmap, 4); // segCountX2
    u16_be(&mut cmap, 0); // searchRange
    u16_be(&mut cmap, 0); // entrySelector
    u16_be(&mut cmap, 0); // rangeShift
    u16_be(&mut cmap, 65); // endCode[0] = 'A'
    u16_be(&mut cmap, 0xFFFF); // endCode[1] sentinel
    u16_be(&mut cmap, 0); // reservedPad
    u16_be(&mut cmap, 65); // startCode[0]
    u16_be(&mut cmap, 0xFFFF); // startCode[1]
    i16_be(&mut cmap, -64); // idDelta[0]
    i16_be(&mut cmap, 1); // idDelta[1]
    u16_be(&mut cmap, 0); // idRangeOffset[0]
    u16_be(&mut cmap, 0); // idRangeOffset[1]
    tables.push(Tab {
        tag: *b"cmap",
        data: cmap,
    });

    // loca (short format). NUM_GLYPHS + 1 entries, all zero (every glyph empty).
    let mut loca = Vec::with_capacity((NUM_GLYPHS as usize + 1) * 2);
    for _ in 0..=NUM_GLYPHS {
        u16_be(&mut loca, 0);
    }
    tables.push(Tab {
        tag: *b"loca",
        data: loca,
    });

    // glyf: empty (every glyph has zero length).
    tables.push(Tab {
        tag: *b"glyf",
        data: Vec::new(),
    });

    // Assemble the final font: 12-byte header, NxTab directory, then tables
    // padded to 4-byte alignment.
    let num_tables = tables.len() as u16;
    let dir_end = 12 + num_tables as usize * 16;

    let mut buf = Vec::new();
    u32_be(&mut buf, 0x0001_0000); // sfVersion
    u16_be(&mut buf, num_tables);
    u16_be(&mut buf, 0); // searchRange (unused)
    u16_be(&mut buf, 0); // entrySelector
    u16_be(&mut buf, 0); // rangeShift

    // Compute table offsets.
    let mut current = dir_end;
    let mut offsets = Vec::with_capacity(tables.len());
    for t in &tables {
        offsets.push(current);
        current += (t.data.len() + 3) & !3;
    }

    // Directory.
    for (i, t) in tables.iter().enumerate() {
        buf.extend_from_slice(&t.tag);
        u32_be(&mut buf, 0); // checksum (unused)
        u32_be(&mut buf, offsets[i] as u32);
        u32_be(&mut buf, t.data.len() as u32);
    }
    while buf.len() < dir_end {
        buf.push(0);
    }

    // Table data, 4-byte aligned.
    for t in &tables {
        buf.extend_from_slice(&t.data);
        while buf.len() % 4 != 0 {
            buf.push(0);
        }
    }

    buf
}

/// Build a CID-font PDF with the synthetic TTF embedded as /FontFile2.
///
/// `w_widths` is the /W array's width assigned to CIDs 0..NUM_GLYPHS.
/// CID 0 is .notdef (kept at HMTX_WIDTH so the diagnostic only trips
/// on the intentionally-wrong entries).
fn build_cid_ttf_pdf(w_widths: &[u16]) -> Vec<u8> {
    assert_eq!(w_widths.len(), NUM_GLYPHS as usize);

    let ttf_bytes = build_known_width_ttf();

    let mut b = PdfBuilder::new("1.4");
    let content = b"BT /F1 12 Tf 72 700 Td <0001> Tj ET";
    b.add_stream_object(5, "", content);

    // FontFile2 stream carrying the TTF program. /Length1 records the
    // decompressed size per PDF spec. We ship raw bytes (no /Filter).
    b.add_stream_object(8, &format!(" /Length1 {}", ttf_bytes.len()), &ttf_bytes);

    // FontDescriptor referencing FontFile2.
    b.add_object(
        9,
        b"<< /Type /FontDescriptor /FontName /CIDFont+TestTTF \
          /Flags 4 /FontBBox [0 0 500 700] /ItalicAngle 0 \
          /Ascent 700 /Descent 0 /CapHeight 700 /StemV 80 \
          /FontFile2 8 0 R >>",
    );

    // Build the /W array as `cid [w1 w2 ... wN]`.
    let mut w_array = String::from("[0 [");
    for (i, w) in w_widths.iter().enumerate() {
        if i > 0 {
            w_array.push(' ');
        }
        w_array.push_str(&w.to_string());
    }
    w_array.push_str("]]");

    // CIDFontType2 descendant.
    let descendant = format!(
        "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /CIDFont+TestTTF \
          /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
          /FontDescriptor 9 0 R /DW {} /W {} >>",
        HMTX_WIDTH, w_array
    );
    b.add_object(7, descendant.as_bytes());

    // Type0 parent.
    b.add_object(
        4,
        b"<< /Type /Font /Subtype /Type0 /BaseFont /CIDFont+TestTTF \
          /Encoding /Identity-H /DescendantFonts [7 0 R] >>",
    );
    b.add_object(6, b"<< /Font << /F1 4 0 R >> >>");
    b.add_object(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources 6 0 R >>",
    );
    b.add_object(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.add_object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    b.finish(1)
}

fn open_with_collector(pdf: Vec<u8>) -> (Document, Arc<CollectingDiagnostics>) {
    let diag = Arc::new(CollectingDiagnostics::new());
    let cfg = Config::default().with_diagnostics(diag.clone());
    let doc = Document::from_bytes_with_config(pdf, cfg).expect("should parse");
    (doc, diag)
}

#[test]
fn cid_w_disagreeing_with_hmtx_emits_warning() {
    // Disagree on 10 glyphs by ~50% (1.5x HMTX_WIDTH), keep the rest
    // matching. 10 > METRIC_DISAGREEMENT_MIN_COUNT (5) and 50% >
    // METRIC_DISAGREEMENT_DELTA (10%), so the diagnostic must fire.
    let mut w: Vec<u16> = vec![HMTX_WIDTH; NUM_GLYPHS as usize];
    for entry in w.iter_mut().take(10) {
        *entry = (HMTX_WIDTH as f32 * 1.5) as u16;
    }

    let pdf = build_cid_ttf_pdf(&w);
    let (mut doc, diag) = open_with_collector(pdf);

    // Force the font to load (Document::from_bytes doesn't load fonts eagerly).
    let mut page = doc.page(0).expect("page 0");
    let _ = page.raw_spans().expect("raw spans");

    let warnings = diag.warnings();
    let metric_warnings: Vec<_> = warnings
        .iter()
        .filter(|w| w.kind == WarningKind::FontMetricsDisagreement)
        .collect();

    assert_eq!(
        metric_warnings.len(),
        1,
        "expected exactly one FontMetricsDisagreement warning, got {}: {:?}",
        metric_warnings.len(),
        metric_warnings,
    );

    let msg = &metric_warnings[0].message;
    assert!(
        msg.contains("TestTTF"),
        "warning should name the font, got: {msg}"
    );
    assert!(
        msg.contains("10 / "),
        "warning should report 10 disagreeing glyphs, got: {msg}"
    );
    assert!(
        msg.contains("hmtx"),
        "warning should mention hmtx for TrueType, got: {msg}"
    );
}

#[test]
fn cid_w_matching_hmtx_emits_no_warning() {
    // All /W entries equal HMTX_WIDTH. No disagreement.
    let w: Vec<u16> = vec![HMTX_WIDTH; NUM_GLYPHS as usize];

    let pdf = build_cid_ttf_pdf(&w);
    let (mut doc, diag) = open_with_collector(pdf);

    let mut page = doc.page(0).expect("page 0");
    let _ = page.raw_spans().expect("raw spans");

    let warnings = diag.warnings();
    let metric_warnings: Vec<_> = warnings
        .iter()
        .filter(|w| w.kind == WarningKind::FontMetricsDisagreement)
        .collect();

    assert!(
        metric_warnings.is_empty(),
        "expected no FontMetricsDisagreement warning when /W matches hmtx, got {:?}",
        metric_warnings,
    );
}

#[test]
fn cid_w_disagreeing_below_count_threshold_emits_no_warning() {
    // Only 4 glyphs disagree (below the 5-glyph threshold). No warning.
    let mut w: Vec<u16> = vec![HMTX_WIDTH; NUM_GLYPHS as usize];
    for entry in w.iter_mut().take(4) {
        *entry = (HMTX_WIDTH as f32 * 1.5) as u16;
    }

    let pdf = build_cid_ttf_pdf(&w);
    let (mut doc, diag) = open_with_collector(pdf);

    let mut page = doc.page(0).expect("page 0");
    let _ = page.raw_spans().expect("raw spans");

    let warnings = diag.warnings();
    let metric_warnings: Vec<_> = warnings
        .iter()
        .filter(|w| w.kind == WarningKind::FontMetricsDisagreement)
        .collect();

    assert!(
        metric_warnings.is_empty(),
        "expected no warning with only 4 disagreeing glyphs, got {:?}",
        metric_warnings,
    );
}

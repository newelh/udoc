//! `udoc render-inspect` subcommand.
//!
//! Dumps intermediate renderer state (outlines, hints, edges, coverage
//! bitmap) as JSON or ASCII for a specific glyph on a specific page.
//! Used as a debugging aid so hinter/rasterizer iteration doesn't rely on
//! ad-hoc eprintln! lines.
//!
//! All JSON payloads include `"schema_version": 1`. Bump freely as a
//! debug-only API.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::str::FromStr;

use udoc::render::font_cache::FontCache;
use udoc::render::inspect::{
    dump_bitmap, dump_edges, dump_hints, dump_outline, BitmapDump, EdgeDump, EdgesDump, HintsDump,
    OutlineDump, OutlineOp, SegmentDump,
};
use udoc::render::png::encode_rgb_png;
use udoc::{extract_with, Config};

const SCHEMA_VERSION: u32 = 1;

/// What to dump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DumpKind {
    /// Glyph outlines (contour op streams).
    Outlines,
    /// Declared + auto-detected stem hints.
    Hints,
    /// Detected segments and edges.
    Edges,
    /// Rasterized coverage bitmap.
    Bitmap,
}

impl FromStr for DumpKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "outlines" | "outline" => Ok(DumpKind::Outlines),
            "hints" | "hint" => Ok(DumpKind::Hints),
            "edges" | "edge" => Ok(DumpKind::Edges),
            "bitmap" | "bmp" => Ok(DumpKind::Bitmap),
            other => Err(format!(
                "unknown dump '{other}', expected outlines|hints|edges|bitmap"
            )),
        }
    }
}

/// Output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Machine-readable JSON with `"schema_version": 1`.
    Json,
    /// Human-readable text dump (may include an ASCII-ramp bitmap).
    Text,
}

impl FromStr for Format {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "json" => Ok(Format::Json),
            "text" | "txt" => Ok(Format::Text),
            other => Err(format!("unknown format '{other}', expected json|text")),
        }
    }
}

/// Parsed CLI arguments.
#[derive(Debug, Clone)]
pub struct Args {
    /// PDF path to inspect.
    pub file: PathBuf,
    /// 1-based page number used to pick a default font/glyph.
    pub page: usize,
    /// Optional explicit glyph id (overrides the page-derived default).
    pub glyph: Option<u32>,
    /// Which intermediate state to dump.
    pub dump: DumpKind,
    /// Output format (`json` or `text`).
    pub format: Format,
    /// When true, emit single-line compact JSON.
    pub compact: bool,
    /// When true, bitmap dumps use an ASCII-ramp text rendering.
    pub ascii: bool,
    /// Pixels-per-em for the hint/edge/bitmap dumps.
    pub ppem: u16,
    /// Optional font name override (by default, the first font on `page`).
    pub font: Option<String>,
}

/// Run render-inspect. Returns a process exit code.
pub fn run(args: Args) -> u8 {
    match run_inner(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("udoc render-inspect: {e}");
            2
        }
    }
}

fn run_inner(args: Args) -> Result<(), String> {
    let mut config = Config::new();
    config.assets.fonts = true;
    let doc = extract_with(&args.file, config)
        .map_err(|e| format!("extracting '{}': {e}", args.file.display()))?;

    // Pick a font. If --font wasn't given, default to the first font name
    // that appears on the requested page (1-based).
    let font_name = pick_font_name(&doc, args.page, args.font.as_deref())?;
    let mut cache = FontCache::new(&doc.assets);

    let glyph_id = resolve_glyph_id(&mut cache, &font_name, &args, &doc)?;

    match args.dump {
        DumpKind::Outlines => {
            let d = dump_outline(&mut cache, &font_name, glyph_id)
                .ok_or_else(|| format!("no outline for '{font_name}' gid {glyph_id}"))?;
            if args.format == Format::Json {
                println!("{}", outline_json(&d, args.compact));
            } else {
                print!("{}", outline_text(&d));
            }
        }
        DumpKind::Hints => {
            let d = dump_hints(&mut cache, &font_name, glyph_id, args.ppem)
                .ok_or_else(|| format!("no hint data for '{font_name}' gid {glyph_id}"))?;
            if args.format == Format::Json {
                println!("{}", hints_json(&d, args.compact));
            } else {
                print!("{}", hints_text(&d));
            }
        }
        DumpKind::Edges => {
            let d = dump_edges(&mut cache, &font_name, glyph_id, args.ppem)
                .ok_or_else(|| format!("no edge data for '{font_name}' gid {glyph_id}"))?;
            if args.format == Format::Json {
                println!("{}", edges_json(&d, args.compact));
            } else {
                print!("{}", edges_text(&d));
            }
        }
        DumpKind::Bitmap => {
            let d = dump_bitmap(&mut cache, &font_name, glyph_id, args.ppem)
                .ok_or_else(|| format!("no bitmap for '{font_name}' gid {glyph_id}"))?;
            if args.ascii {
                // ASCII ramp to stdout.
                print!("{}", bitmap_ascii(&d));
            } else if args.format == Format::Json {
                // JSON descriptor with base64-free hex-encoded coverage on a
                // single line. Shape stays machine-parseable, and the field
                // name reminds callers the array is grayscale.
                println!("{}", bitmap_json(&d, args.compact));
            } else {
                // Binary PNG goes to stdout. Grayscale coverage mapped into RGB.
                let mut rgb = Vec::with_capacity(d.coverage.len() * 3);
                for &c in &d.coverage {
                    rgb.extend_from_slice(&[c, c, c]);
                }
                let png = encode_rgb_png(&rgb, d.width, d.height);
                use std::io::Write;
                std::io::stdout()
                    .write_all(&png)
                    .map_err(|e| format!("writing PNG to stdout: {e}"))?;
            }
        }
    }
    Ok(())
}

// --- Font / glyph resolution -----------------------------------------------

fn pick_font_name(
    doc: &udoc_core::document::Document,
    page: usize,
    explicit: Option<&str>,
) -> Result<String, String> {
    if let Some(name) = explicit {
        return Ok(name.to_string());
    }
    let pres = doc
        .presentation
        .as_ref()
        .ok_or("document has no presentation data")?;
    let page_idx = page.saturating_sub(1);
    let first = pres
        .raw_spans
        .iter()
        .find(|s| s.page_index == page_idx)
        .and_then(|s| s.font_name.clone());
    first.ok_or_else(|| {
        format!("page {page} has no text spans; specify --font NAME to force a font")
    })
}

fn resolve_glyph_id(
    cache: &mut FontCache,
    font_name: &str,
    args: &Args,
    doc: &udoc_core::document::Document,
) -> Result<u16, String> {
    if let Some(g) = args.glyph {
        return Ok(g.try_into().unwrap_or(u16::MAX));
    }
    // If no --glyph, try: first character of first span on the page, mapped
    // to a glyph id via the font's cmap.
    let pres = doc
        .presentation
        .as_ref()
        .ok_or("document has no presentation data")?;
    let page_idx = args.page.saturating_sub(1);
    let span_text = pres
        .raw_spans
        .iter()
        .find(|s| s.page_index == page_idx)
        .map(|s| s.text.clone())
        .unwrap_or_default();
    let ch = span_text
        .chars()
        .next()
        .ok_or("page has no text; pass --glyph GID")?;
    cache
        .glyph_id_with_fallback(font_name, ch)
        .ok_or_else(|| format!("font '{font_name}' has no glyph for '{ch}'; pass --glyph GID"))
}

// --- JSON serialization (no serde_json dep; hand-rolled to stay light) -----

fn outline_json(d: &OutlineDump, compact: bool) -> String {
    let mut s = String::new();
    let contours_json: Vec<String> = d
        .contours
        .iter()
        .map(|ops| {
            let items: Vec<String> = ops.iter().map(op_json).collect();
            format!("[{}]", items.join(","))
        })
        .collect();
    let font = escape_json(&d.font_name);
    let (b0, b1, b2, b3) = d.bounds;
    let core = format!(
        "\"schema_version\":{SCHEMA_VERSION},\"kind\":\"outline\",\"font\":\"{font}\",\"glyph_id\":{},\"units_per_em\":{},\"bounds\":[{b0},{b1},{b2},{b3}],\"contours\":[{}]",
        d.glyph_id,
        d.units_per_em,
        contours_json.join(",")
    );
    if compact {
        s.push('{');
        s.push_str(&core);
        s.push('}');
    } else {
        s.push_str("{\n  ");
        // Re-join with commas+newlines for readability.
        let parts: Vec<&str> = split_top_commas(&core);
        s.push_str(&parts.join(",\n  "));
        s.push_str("\n}");
    }
    s
}

fn op_json(op: &OutlineOp) -> String {
    match *op {
        OutlineOp::Move { x, y } => {
            format!("{{\"op\":\"move\",\"x\":{},\"y\":{}}}", fmt_f(x), fmt_f(y))
        }
        OutlineOp::Line { x, y } => {
            format!("{{\"op\":\"line\",\"x\":{},\"y\":{}}}", fmt_f(x), fmt_f(y))
        }
        OutlineOp::Curve {
            c1x,
            c1y,
            c2x,
            c2y,
            x,
            y,
        } => format!(
            "{{\"op\":\"curve\",\"c1x\":{},\"c1y\":{},\"c2x\":{},\"c2y\":{},\"x\":{},\"y\":{}}}",
            fmt_f(c1x),
            fmt_f(c1y),
            fmt_f(c2x),
            fmt_f(c2y),
            fmt_f(x),
            fmt_f(y)
        ),
    }
}

fn fmt_f(v: f64) -> String {
    // JSON has no IEEE infinities or NaN, and the segment table uses
    // f64::MAX as a sentinel. Render these as the literal string
    // "null" so downstream JSON parsers don't choke.
    if !v.is_finite() || v >= f64::MAX / 2.0 {
        return "null".into();
    }
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{:.4}", v)
    }
}

fn hints_json(d: &HintsDump, compact: bool) -> String {
    let font = escape_json(&d.font_name);
    let declared_h: Vec<String> = d
        .declared
        .h_stems
        .iter()
        .map(|(p, w)| format!("[{},{}]", fmt_f(*p), fmt_f(*w)))
        .collect();
    let declared_v: Vec<String> = d
        .declared
        .v_stems
        .iter()
        .map(|(p, w)| format!("[{},{}]", fmt_f(*p), fmt_f(*w)))
        .collect();
    let h_edges = edges_array_json(&d.auto_h_edges);
    let v_edges = edges_array_json(&d.auto_v_edges);
    let core = format!(
        "\"schema_version\":{SCHEMA_VERSION},\"kind\":\"hints\",\"font\":\"{font}\",\"glyph_id\":{},\"units_per_em\":{},\"declared\":{{\"hstem\":[{}],\"vstem\":[{}]}},\"auto_hinter\":{{\"h_edges\":{h_edges},\"v_edges\":{v_edges}}}",
        d.glyph_id,
        d.units_per_em,
        declared_h.join(","),
        declared_v.join(",")
    );
    wrap_json_object(&core, compact)
}

fn edges_json(d: &EdgesDump, compact: bool) -> String {
    let font = escape_json(&d.font_name);
    let core = format!(
        "\"schema_version\":{SCHEMA_VERSION},\"kind\":\"edges\",\"font\":\"{font}\",\"glyph_id\":{},\"units_per_em\":{},\"h_segments\":{},\"v_segments\":{},\"h_edges\":{},\"v_edges\":{}",
        d.glyph_id,
        d.units_per_em,
        segments_array_json(&d.h_segments),
        segments_array_json(&d.v_segments),
        edges_array_json(&d.h_edges),
        edges_array_json(&d.v_edges)
    );
    wrap_json_object(&core, compact)
}

fn segments_array_json(segs: &[SegmentDump]) -> String {
    let items: Vec<String> = segs
        .iter()
        .map(|s| {
            let link = s.link.map(|v| v.to_string()).unwrap_or("null".into());
            let edge = s.edge.map(|v| v.to_string()).unwrap_or("null".into());
            format!(
                "{{\"dim\":\"{}\",\"dir\":\"{}\",\"pos\":{},\"min_coord\":{},\"max_coord\":{},\"link_idx\":{link},\"edge_idx\":{edge},\"score\":{},\"contour\":{}}}",
                s.dim,
                s.dir,
                fmt_f(s.pos),
                fmt_f(s.min_coord),
                fmt_f(s.max_coord),
                fmt_f(s.score),
                s.contour
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}

fn edges_array_json(edges: &[EdgeDump]) -> String {
    let items: Vec<String> = edges
        .iter()
        .map(|e| {
            let blue = e.blue_zone.map(|v| v.to_string()).unwrap_or("null".into());
            let link = e.link.map(|v| v.to_string()).unwrap_or("null".into());
            let serif = e.serif.map(|v| v.to_string()).unwrap_or("null".into());
            format!(
                "{{\"dim\":\"{}\",\"pos\":{},\"fitted_pos\":{},\"fitted\":{},\"blue_zone\":{blue},\"link\":{link},\"serif\":{serif},\"flags\":{},\"segment_count\":{}}}",
                e.dim,
                fmt_f(e.pos),
                fmt_f(e.fitted_pos),
                e.fitted,
                e.flags,
                e.segment_count
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}

fn bitmap_json(d: &BitmapDump, compact: bool) -> String {
    // Emit the grayscale coverage as a flat array of u8. For pretty mode
    // we put it on one line to keep the output grep-able.
    let coverage: String = d
        .coverage
        .iter()
        .map(|b| b.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let font = escape_json(&d.font_name);
    let core = format!(
        "\"schema_version\":{SCHEMA_VERSION},\"kind\":\"bitmap\",\"font\":\"{font}\",\"glyph_id\":{},\"ppem\":{},\"width\":{},\"height\":{},\"coverage\":[{coverage}]",
        d.glyph_id, d.ppem, d.width, d.height
    );
    wrap_json_object(&core, compact)
}

fn wrap_json_object(core: &str, compact: bool) -> String {
    if compact {
        format!("{{{core}}}")
    } else {
        let parts: Vec<&str> = split_top_commas(core);
        format!("{{\n  {}\n}}", parts.join(",\n  "))
    }
}

/// Split a flat "k:v,k:v" payload at top-level commas (ignoring commas
/// inside `{...}` and `[...]`). Used by the hand-rolled JSON pretty printer.
fn split_top_commas(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut depth_obj: i32 = 0;
    let mut depth_arr: i32 = 0;
    let mut in_str = false;
    let mut escape = false;
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if escape {
            escape = false;
            continue;
        }
        if in_str {
            if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth_obj += 1,
            b'}' => depth_obj -= 1,
            b'[' => depth_arr += 1,
            b']' => depth_arr -= 1,
            b',' if depth_obj == 0 && depth_arr == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

// --- Text formatters --------------------------------------------------------

fn outline_text(d: &OutlineDump) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "outline font={} gid={} upm={} bounds=({},{},{},{})",
        d.font_name, d.glyph_id, d.units_per_em, d.bounds.0, d.bounds.1, d.bounds.2, d.bounds.3
    );
    for (i, contour) in d.contours.iter().enumerate() {
        let _ = writeln!(s, "contour {i}:");
        for op in contour {
            match *op {
                OutlineOp::Move { x, y } => {
                    let _ = writeln!(s, "  M {:.2} {:.2}", x, y);
                }
                OutlineOp::Line { x, y } => {
                    let _ = writeln!(s, "  L {:.2} {:.2}", x, y);
                }
                OutlineOp::Curve {
                    c1x,
                    c1y,
                    c2x,
                    c2y,
                    x,
                    y,
                } => {
                    let _ = writeln!(
                        s,
                        "  C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2}",
                        c1x, c1y, c2x, c2y, x, y
                    );
                }
            }
        }
    }
    s
}

fn hints_text(d: &HintsDump) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "hints font={} gid={} upm={}",
        d.font_name, d.glyph_id, d.units_per_em
    );
    let _ = writeln!(s, "declared hstem:");
    for (p, w) in &d.declared.h_stems {
        let _ = writeln!(s, "  {:.2} {:.2}", p, w);
    }
    let _ = writeln!(s, "declared vstem:");
    for (p, w) in &d.declared.v_stems {
        let _ = writeln!(s, "  {:.2} {:.2}", p, w);
    }
    let _ = writeln!(s, "auto h_edges:");
    for e in &d.auto_h_edges {
        edge_line(&mut s, e);
    }
    let _ = writeln!(s, "auto v_edges:");
    for e in &d.auto_v_edges {
        edge_line(&mut s, e);
    }
    s
}

fn edges_text(d: &EdgesDump) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "edges font={} gid={} upm={}",
        d.font_name, d.glyph_id, d.units_per_em
    );
    let _ = writeln!(s, "h_segments:");
    for seg in &d.h_segments {
        segment_line(&mut s, seg);
    }
    let _ = writeln!(s, "v_segments:");
    for seg in &d.v_segments {
        segment_line(&mut s, seg);
    }
    let _ = writeln!(s, "h_edges:");
    for e in &d.h_edges {
        edge_line(&mut s, e);
    }
    let _ = writeln!(s, "v_edges:");
    for e in &d.v_edges {
        edge_line(&mut s, e);
    }
    s
}

fn segment_line(s: &mut String, seg: &SegmentDump) {
    let _ = writeln!(
        s,
        "  {} {:>5} pos={:>8.2} [{:>8.2}..{:>8.2}] link={:?} edge={:?} score={:.2} contour={}",
        seg.dim,
        seg.dir,
        seg.pos,
        seg.min_coord,
        seg.max_coord,
        seg.link,
        seg.edge,
        seg.score,
        seg.contour
    );
}

fn edge_line(s: &mut String, e: &EdgeDump) {
    let _ = writeln!(
        s,
        "  {} pos={:>8.2} fitted={:>8.2} (={}) link={:?} serif={:?} blue={:?} flags={:#04x} n={}",
        e.dim,
        e.pos,
        e.fitted_pos,
        e.fitted,
        e.link,
        e.serif,
        e.blue_zone,
        e.flags,
        e.segment_count
    );
}

fn bitmap_ascii(d: &BitmapDump) -> String {
    const RAMP: &[u8] = b" .:-=+*#%@";
    let mut s = String::with_capacity(((d.width + 1) * d.height) as usize);
    for y in 0..d.height {
        for x in 0..d.width {
            let c = d.coverage[(y * d.width + x) as usize];
            let idx = ((c as u16 * (RAMP.len() as u16 - 1)) / 255) as usize;
            s.push(RAMP[idx] as char);
        }
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dump_kind() {
        assert_eq!(DumpKind::from_str("outlines").unwrap(), DumpKind::Outlines);
        assert_eq!(DumpKind::from_str("Hints").unwrap(), DumpKind::Hints);
        assert_eq!(DumpKind::from_str("EDGES").unwrap(), DumpKind::Edges);
        assert_eq!(DumpKind::from_str("bitmap").unwrap(), DumpKind::Bitmap);
        assert!(DumpKind::from_str("blueprint").is_err());
    }

    #[test]
    fn fmt_f_integer_stays_bare() {
        assert_eq!(fmt_f(0.0), "0");
        assert_eq!(fmt_f(100.0), "100");
        assert_eq!(fmt_f(-3.25), "-3.2500");
    }

    #[test]
    fn escape_json_basics() {
        assert_eq!(escape_json("hello"), "hello");
        assert_eq!(escape_json("a\"b"), "a\\\"b");
        assert_eq!(escape_json("x\ny"), "x\\ny");
    }

    #[test]
    fn split_top_commas_respects_nesting() {
        let s = "\"a\":1,\"b\":[1,2,3],\"c\":{\"d\":4}";
        let parts = split_top_commas(s);
        assert_eq!(parts, vec!["\"a\":1", "\"b\":[1,2,3]", "\"c\":{\"d\":4}"]);
    }

    #[test]
    fn split_top_commas_respects_strings() {
        let s = "\"a\":\"x,y\",\"b\":2";
        let parts = split_top_commas(s);
        assert_eq!(parts, vec!["\"a\":\"x,y\"", "\"b\":2"]);
    }
}

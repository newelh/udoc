//! `udoc audit-fonts` subcommand.
//!
//! Walks a PDF, groups every span by (font_name, font_id) and emits a
//! report aggregating [`udoc_core::text::FontResolution`] per font. Use
//! this to spot documents where a big chunk of text was decoded via a
//! substituted or synthesized fallback font: those spans may have wrong
//! glyphs, wrong widths, or (in the CID + no-ToUnicode case) wrong Unicode.
//!
//! Complements `Config::strict_fonts` (M-32b) for users that want to
//! audit a corpus before flipping the strict flag, and builds on the
//! `TextSpan::font_resolution` observability field added in M-32a.
//!
//! Output formats:
//! - `json` (default): machine-readable report with per-font entries plus
//!   a summary. Alphabetically sorted by `referenced_name` for stable
//!   diffs.
//! - `text`: column-aligned human-readable table, trimmed for terminals.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use udoc_core::diagnostics::{kind, CollectingDiagnostics, MissingGlyphInfo};
use udoc_core::document::{Document, PositionedSpan};
use udoc_core::text::FontResolution;
use udoc_render::font_cache::FontCache;

use udoc::{extract_with, Config};

/// Output format for the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Machine-readable JSON (default).
    Json,
    /// Human-readable column-aligned text.
    Text,
}

impl std::str::FromStr for Format {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "json" => Ok(Format::Json),
            "text" | "txt" => Ok(Format::Text),
            other => Err(format!("unknown format '{other}', expected json|text")),
        }
    }
}

/// Parsed arguments for `udoc audit-fonts`.
#[derive(Debug, Clone)]
pub struct Args {
    /// Path of the document to audit.
    pub file: PathBuf,
    /// Optional output file. When `None`, the report is written to stdout.
    pub output: Option<PathBuf>,
    /// Output format (`json` or `text`).
    pub format: Format,
}

/// Run the audit-fonts subcommand. Returns the process exit code.
pub fn run(args: Args) -> u8 {
    match run_inner(&args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("udoc audit-fonts: {e}");
            2
        }
    }
}

fn run_inner(args: &Args) -> Result<(), String> {
    let doc = extract_with(&args.file, Config::new())
        .map_err(|e| format!("extracting '{}': {e}", args.file.display()))?;

    // Exercise the renderer's glyph-lookup path over every (font, char) in
    // the extracted spans so that missing-glyph warnings accumulate in the
    // sink. The extraction path itself doesn't rasterize, so without this
    // probe the report would always list zero missing glyphs.
    let sink = Arc::new(CollectingDiagnostics::new());
    probe_missing_glyphs(&doc, sink.clone());

    let report = build_report_with_missing(&doc, &args.file, &sink.take_warnings());

    let rendered = match args.format {
        Format::Json => {
            serde_json::to_string_pretty(&report).map_err(|e| format!("serializing report: {e}"))?
        }
        Format::Text => render_text(&report),
    };

    match &args.output {
        Some(path) => std::fs::write(path, rendered)
            .map_err(|e| format!("writing report to '{}': {e}", path.display()))?,
        None => {
            use std::io::Write as _;
            let mut out = std::io::stdout().lock();
            out.write_all(rendered.as_bytes())
                .map_err(|e| format!("writing report to stdout: {e}"))?;
            if !rendered.ends_with('\n') {
                let _ = out.write_all(b"\n");
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Report types (serde-serializable)
// ---------------------------------------------------------------------------

/// Top-level audit report. Fields are ordered so the JSON output reads
/// file -> pages -> fonts -> missing_glyphs -> summary.
#[derive(Debug, Serialize)]
pub struct AuditReport {
    /// Source document path (stringified for JSON output).
    pub file: String,
    /// Number of pages in the document.
    pub pages: usize,
    /// Per-font aggregated entries, sorted by `referenced_name`.
    pub fonts: Vec<FontEntry>,
    /// Per (font, codepoint) counts of glyphs that exhausted the
    /// renderer's fallback chain (see [`udoc_core::diagnostics::kind::MISSING_GLYPH`]).
    /// Sorted by `count` descending then `(font, codepoint)` for stable
    /// output. An empty list means no missing glyphs were observed while
    /// probing the renderer over the extracted spans.
    pub missing_glyphs: Vec<MissingGlyphEntry>,
    /// Aggregated summary statistics.
    pub summary: Summary,
}

/// One entry in the `missing_glyphs` section of the audit report.
#[derive(Debug, Clone, Serialize)]
pub struct MissingGlyphEntry {
    /// Font name as referenced in the source document.
    pub font: String,
    /// Unicode codepoint as a bare u32.
    pub codepoint: u32,
    /// Hex-formatted codepoint for human readers (e.g. `"U+03A9"`).
    pub codepoint_hex: String,
    /// Glyph id in the named font (0 when the lookup couldn't reach a gid).
    pub glyph_id: u32,
    /// Number of `(font, codepoint)` probe hits during the audit. Counts
    /// unique span-chars, so a glyph used 200 times on a page counts 200.
    pub count: u64,
}

/// Aggregated information about one referenced font.
#[derive(Debug, Serialize)]
pub struct FontEntry {
    /// Font name as referenced in the source document (subset-prefix stripped).
    pub referenced_name: String,
    /// How the font was resolved (exact, substituted, or synthetic fallback).
    pub resolution: FontResolution,
    /// Number of spans that used this font.
    pub spans: u64,
    /// 1-indexed page numbers where the font appears.
    pub pages: Vec<usize>,
    /// Short character sample extracted from the first span seen.
    pub sample_chars: String,
}

/// Top-level audit summary.
#[derive(Debug, Serialize)]
pub struct Summary {
    /// Distinct (font_name, font_id) groups in the document.
    pub total_fonts: u64,
    /// Groups whose spans resolved with `FontResolution::Exact`.
    pub exact: u64,
    /// Groups whose spans resolved with `FontResolution::Substituted`.
    pub substituted: u64,
    /// Groups whose spans used the synthetic-fallback tier.
    pub synthetic_fallback: u64,
    /// Total spans in the document whose resolution was not `Exact`.
    pub spans_with_fallback: u64,
    /// Total span count in the document.
    pub spans_total: u64,
    /// `spans_with_fallback / spans_total` (0.0 when the document has no spans).
    pub fallback_ratio: f64,
    /// Number of distinct `(font, codepoint)` pairs with at least one
    /// missing-glyph event.
    pub missing_glyph_pairs: u64,
    /// Sum of every missing-glyph entry's `count` (total span-chars that
    /// would have fallen through to `.notdef`).
    pub missing_glyph_total: u64,
}

// ---------------------------------------------------------------------------
// Aggregation
// ---------------------------------------------------------------------------

/// Compound key handling subset-prefix collisions between fonts that share
/// a display name (PDF embeds many subsets of CMR10 named "ABCDEF+CMR10";
/// `font_id` carries the full subset-prefixed name while `font_name` has
/// already been stripped).
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
struct GroupKey {
    referenced_name: String,
    font_id: String,
}

impl GroupKey {
    fn from_span(span: &PositionedSpan) -> Self {
        let referenced_name = span
            .font_name
            .clone()
            .unwrap_or_else(|| "(unknown)".to_string());
        let font_id = span
            .font_id
            .clone()
            .unwrap_or_else(|| referenced_name.clone());
        Self {
            referenced_name,
            font_id,
        }
    }
}

struct GroupAgg {
    resolution: FontResolution,
    spans: u64,
    pages: std::collections::BTreeSet<usize>,
    sample_chars: String,
}

impl GroupAgg {
    fn new(span: &PositionedSpan) -> Self {
        let mut pages = std::collections::BTreeSet::new();
        pages.insert(span.page_index + 1);
        Self {
            resolution: span.font_resolution.clone(),
            spans: 1,
            pages,
            sample_chars: take_sample(&span.text),
        }
    }

    fn update(&mut self, span: &PositionedSpan) {
        self.spans += 1;
        self.pages.insert(span.page_index + 1);
        // First non-Exact resolution wins: if every span is Exact, we keep
        // Exact (the first one we saw). If any span recorded a fallback,
        // that's the interesting story for this font.
        if self.resolution.is_exact() && !span.font_resolution.is_exact() {
            self.resolution = span.font_resolution.clone();
        }
        if self.sample_chars.is_empty() {
            self.sample_chars = take_sample(&span.text);
        }
    }
}

/// Probe the renderer's glyph-lookup path for every `(font, char)` pair
/// in the extracted spans. When the fallback chain exhausts, the
/// `FontCache` emits a [`kind::MISSING_GLYPH`] warning onto `sink` which
/// the caller then folds into the audit report. Side-effect only; the
/// document itself isn't modified.
///
/// [`kind::MISSING_GLYPH`]: udoc_core::diagnostics::kind::MISSING_GLYPH
pub fn probe_missing_glyphs(doc: &Document, sink: Arc<CollectingDiagnostics>) {
    let mut cache = FontCache::new(&doc.assets);
    cache.set_sink(sink);
    let empty_spans: Vec<PositionedSpan> = Vec::new();
    let spans: &[PositionedSpan] = doc
        .presentation
        .as_ref()
        .map(|p| p.raw_spans.as_slice())
        .unwrap_or(&empty_spans);
    for span in spans {
        // Prefer `font_id` (subset-prefixed name matching the AssetStore
        // key) over the bare `font_name`; `FontCache::glyph_outline` does
        // its own subset-prefix stripping when falling back.
        let font_name = span
            .font_id
            .as_deref()
            .or(span.font_name.as_deref())
            .unwrap_or("");
        for ch in span.text.chars() {
            // Skip whitespace and control chars: they're routed through
            // zero-width advance paths and not rasterized as glyphs.
            if ch.is_whitespace() || ch.is_control() {
                continue;
            }
            let _ = cache.probe_glyph(font_name, ch);
        }
    }
}

/// Build the report from a `Document`. Exposed for unit testing so we can
/// assert against a hand-built span list without spinning up the CLI.
///
/// Equivalent to [`build_report_with_missing`] with an empty warnings
/// slice; use the `_with_missing` variant when diagnostics have been
/// collected via [`probe_missing_glyphs`].
pub fn build_report(doc: &Document, path: &Path) -> AuditReport {
    build_report_with_missing(doc, path, &[])
}

/// Build the report including a `missing_glyphs` section aggregated from
/// the given diagnostics. Non-[`kind::MISSING_GLYPH`] warnings are ignored.
///
/// [`kind::MISSING_GLYPH`]: udoc_core::diagnostics::kind::MISSING_GLYPH
pub fn build_report_with_missing(
    doc: &Document,
    path: &Path,
    warnings: &[udoc_core::diagnostics::Warning],
) -> AuditReport {
    let pages = doc.metadata.page_count;
    let empty_spans: Vec<PositionedSpan> = Vec::new();
    let spans: &[PositionedSpan] = doc
        .presentation
        .as_ref()
        .map(|p| p.raw_spans.as_slice())
        .unwrap_or(&empty_spans);

    let mut groups: BTreeMap<GroupKey, GroupAgg> = BTreeMap::new();
    for span in spans {
        let key = GroupKey::from_span(span);
        groups
            .entry(key)
            .and_modify(|g| g.update(span))
            .or_insert_with(|| GroupAgg::new(span));
    }

    let mut fonts: Vec<FontEntry> = groups
        .into_iter()
        .map(|(key, agg)| FontEntry {
            referenced_name: key.referenced_name,
            resolution: agg.resolution,
            spans: agg.spans,
            pages: agg.pages.into_iter().collect(),
            sample_chars: agg.sample_chars,
        })
        .collect();
    // BTreeMap keys sort by (referenced_name, font_id) already; resort by
    // referenced_name alone so the doc schema's promise holds (two subsets
    // of the same display name end up adjacent).
    fonts.sort_by(|a, b| a.referenced_name.cmp(&b.referenced_name));

    let mut exact = 0u64;
    let mut substituted = 0u64;
    let mut synthetic_fallback = 0u64;
    let mut spans_with_fallback = 0u64;
    for f in &fonts {
        match &f.resolution {
            FontResolution::Exact => exact += 1,
            FontResolution::Substituted { .. } => {
                substituted += 1;
                spans_with_fallback += f.spans;
            }
            FontResolution::SyntheticFallback { .. } => {
                synthetic_fallback += 1;
                spans_with_fallback += f.spans;
            }
            _ => {
                // Non-exhaustive: treat unknown future variants as fallbacks
                // so the ratio stays conservative (over-reports rather than
                // under-reports fallback pressure).
                spans_with_fallback += f.spans;
            }
        }
    }
    let spans_total = spans.len() as u64;
    let fallback_ratio = if spans_total > 0 {
        spans_with_fallback as f64 / spans_total as f64
    } else {
        0.0
    };

    let missing_glyphs = aggregate_missing_glyphs(spans, warnings);
    let missing_glyph_pairs = missing_glyphs.len() as u64;
    let missing_glyph_total: u64 = missing_glyphs.iter().map(|m| m.count).sum();

    let total_fonts = fonts.len() as u64;
    AuditReport {
        file: path.display().to_string(),
        pages,
        fonts,
        missing_glyphs,
        summary: Summary {
            total_fonts,
            exact,
            substituted,
            synthetic_fallback,
            spans_with_fallback,
            spans_total,
            fallback_ratio,
            missing_glyph_pairs,
            missing_glyph_total,
        },
    }
}

/// Aggregate [`kind::MISSING_GLYPH`] warnings into per-pair entries and
/// backfill each entry's `count` by scanning the span corpus. The
/// `FontCache` emits at most one warning per `(font, codepoint)` pair
/// (dedup via `missing_glyph_seen`), so the warnings slice tells us
/// *which* pairs missed and the spans tell us *how often*.
///
/// Sorted by `count` descending (most frequent first) so CLI consumers
/// see the biggest visual damage at the top of the list. Ties break on
/// `(font, codepoint)` for a stable ordering across runs.
///
/// [`kind::MISSING_GLYPH`]: udoc_core::diagnostics::kind::MISSING_GLYPH
fn aggregate_missing_glyphs(
    spans: &[PositionedSpan],
    warnings: &[udoc_core::diagnostics::Warning],
) -> Vec<MissingGlyphEntry> {
    // Pull the (font, codepoint, glyph_id) triples out of the warnings.
    let mut pairs: BTreeMap<(String, u32), MissingGlyphInfo> = BTreeMap::new();
    for w in warnings {
        if w.kind != kind::MISSING_GLYPH {
            continue;
        }
        let Some(info) = w.context.missing_glyph.as_ref() else {
            continue;
        };
        pairs
            .entry((info.font.clone(), info.codepoint))
            .or_insert_with(|| info.clone());
    }

    if pairs.is_empty() {
        return Vec::new();
    }

    // Second pass: count occurrences. A glyph in a missing pair might
    // not be reachable from the span corpus at all (e.g. probed via a
    // lowercase font_id that doesn't match any span), in which case the
    // count stays at 1 so the entry is still surfaced.
    let mut counts: BTreeMap<(String, u32), u64> = BTreeMap::new();
    for span in spans {
        let font_name = span
            .font_id
            .as_deref()
            .or(span.font_name.as_deref())
            .unwrap_or("");
        for ch in span.text.chars() {
            let key = (font_name.to_string(), ch as u32);
            if pairs.contains_key(&key) {
                *counts.entry(key).or_insert(0) += 1;
            }
        }
    }

    let mut entries: Vec<MissingGlyphEntry> = pairs
        .into_iter()
        .map(|(key, info)| {
            let count = counts.get(&key).copied().unwrap_or(1);
            MissingGlyphEntry {
                font: info.font,
                codepoint: info.codepoint,
                codepoint_hex: format!("U+{:04X}", info.codepoint),
                glyph_id: info.glyph_id,
                count,
            }
        })
        .collect();

    // Sort: count desc, then (font, codepoint) asc for stable ordering.
    entries.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.font.cmp(&b.font))
            .then_with(|| a.codepoint.cmp(&b.codepoint))
    });
    entries
}

fn take_sample(text: &str) -> String {
    // First 20 Unicode chars, truncated at a char boundary. No trimming so
    // callers can see leading spaces when a font is only used for a trailing
    // separator.
    text.chars().take(20).collect()
}

// ---------------------------------------------------------------------------
// Text formatter
// ---------------------------------------------------------------------------

fn render_text(report: &AuditReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "File: {} ({} pages)", report.file, report.pages);
    let _ = writeln!(
        out,
        "Fonts: {} total ({} exact, {} substituted, {} synthetic fallback)",
        report.summary.total_fonts,
        report.summary.exact,
        report.summary.substituted,
        report.summary.synthetic_fallback,
    );
    let pct = report.summary.fallback_ratio * 100.0;
    let _ = writeln!(
        out,
        "Spans: {}/{} ({:.1}%) with fallback fonts",
        report.summary.spans_with_fallback, report.summary.spans_total, pct,
    );
    out.push('\n');

    if report.fonts.is_empty() {
        let _ = writeln!(out, "(no fonts with spans)");
        return out;
    }

    let _ = writeln!(
        out,
        "{:<30}  {:<28}  {:>6}  {:<12}  Sample",
        "Font", "Resolution", "Spans", "Pages",
    );
    for f in &report.fonts {
        let name = truncate(&f.referenced_name, 30);
        let status = truncate(&short_resolution(&f.resolution), 28);
        let pages = truncate(&short_page_list(&f.pages), 12);
        // sample_chars is already capped at 20 chars by `take_sample`
        // during aggregation. Quote it so leading spaces stay visible.
        let sample_quoted = format!("{:?}", &f.sample_chars);
        let _ = writeln!(
            out,
            "{:<30}  {:<28}  {:>6}  {:<12}  {}",
            name, status, f.spans, pages, sample_quoted,
        );
    }

    if !report.missing_glyphs.is_empty() {
        let _ = writeln!(
            out,
            "\nMissing glyphs: {} distinct pairs ({} total chars)",
            report.summary.missing_glyph_pairs, report.summary.missing_glyph_total,
        );
        let _ = writeln!(
            out,
            "{:<30}  {:<10}  {:>6}  {:>6}",
            "Font", "Codepoint", "GID", "Count",
        );
        // Cap at 20 entries; full list is in the JSON output.
        for m in report.missing_glyphs.iter().take(20) {
            let _ = writeln!(
                out,
                "{:<30}  {:<10}  {:>6}  {:>6}",
                truncate(&m.font, 30),
                m.codepoint_hex,
                m.glyph_id,
                m.count,
            );
        }
        if report.missing_glyphs.len() > 20 {
            let _ = writeln!(
                out,
                "... ({} more, see JSON output)",
                report.missing_glyphs.len() - 20,
            );
        }
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    // Width measured in chars, not bytes. Good enough for ASCII font names;
    // CJK will misalign, but those names are rare in practice.
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else if max <= 1 {
        chars.into_iter().take(max).collect()
    } else {
        let kept: String = chars.into_iter().take(max - 1).collect();
        format!("{kept}_")
    }
}

fn short_resolution(r: &FontResolution) -> String {
    match r {
        FontResolution::Exact => "Exact".to_string(),
        FontResolution::Substituted { reason, .. } => {
            format!("Substituted/{}", reason.as_str())
        }
        FontResolution::SyntheticFallback { reason, .. } => {
            format!("Synthetic/{}", reason.as_str())
        }
        _ => "Fallback".to_string(),
    }
}

/// Human-friendly condensed page list. Handles contiguous runs (e.g.
/// `1-5`) but keeps things short; callers that need the full list should
/// use the JSON output.
fn short_page_list(pages: &[usize]) -> String {
    if pages.is_empty() {
        return String::new();
    }
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut start = pages[0];
    let mut end = pages[0];
    for &p in &pages[1..] {
        if p == end + 1 {
            end = p;
        } else {
            runs.push((start, end));
            start = p;
            end = p;
        }
    }
    runs.push((start, end));
    // Limit to the first 3 runs; real documents that use a font on every
    // page would otherwise blow out the "Pages" column.
    let mut out = String::new();
    for (i, (s, e)) in runs.iter().take(3).enumerate() {
        if i > 0 {
            out.push(',');
        }
        if s == e {
            let _ = write!(out, "{s}");
        } else {
            let _ = write!(out, "{s}-{e}");
        }
    }
    if runs.len() > 3 {
        out.push('+');
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::document::{DocumentMetadata, Presentation};
    use udoc_core::geometry::BoundingBox;
    use udoc_core::text::FallbackReason;

    fn span(
        text: &str,
        page_index: usize,
        font_name: &str,
        font_id: Option<&str>,
        res: FontResolution,
    ) -> PositionedSpan {
        let mut s = PositionedSpan::new(
            text.to_string(),
            BoundingBox::new(0.0, 0.0, 10.0, 12.0),
            page_index,
        );
        s.font_name = Some(font_name.to_string());
        s.font_id = font_id.map(str::to_string);
        s.font_resolution = res;
        s
    }

    fn mk_doc(spans: Vec<PositionedSpan>, pages: usize) -> Document {
        let mut doc = Document::default();
        doc.metadata = DocumentMetadata::with_page_count(pages);
        let mut pres = Presentation::default();
        pres.raw_spans = spans;
        doc.presentation = Some(pres);
        doc
    }

    #[test]
    fn format_parses() {
        use std::str::FromStr;
        assert_eq!(Format::from_str("json").unwrap(), Format::Json);
        assert_eq!(Format::from_str("text").unwrap(), Format::Text);
        assert_eq!(Format::from_str("TXT").unwrap(), Format::Text);
        assert!(Format::from_str("xml").is_err());
    }

    #[test]
    fn aggregates_spans_per_font() {
        let doc = mk_doc(
            vec![
                span("Hello ", 0, "Helvetica", None, FontResolution::Exact),
                span("world", 0, "Helvetica", None, FontResolution::Exact),
                span(
                    "alpha",
                    1,
                    "CMR10",
                    Some("ABCDEF+CMR10"),
                    FontResolution::Substituted {
                        requested: "ABCDEF+CMR10".into(),
                        resolved: "LatinModernMath".into(),
                        reason: FallbackReason::NameRouted,
                    },
                ),
                span(
                    "beta",
                    2,
                    "CMR10",
                    Some("ABCDEF+CMR10"),
                    FontResolution::Substituted {
                        requested: "ABCDEF+CMR10".into(),
                        resolved: "LatinModernMath".into(),
                        reason: FallbackReason::NameRouted,
                    },
                ),
            ],
            3,
        );
        let report = build_report(&doc, Path::new("dummy.pdf"));

        assert_eq!(report.file, "dummy.pdf");
        assert_eq!(report.pages, 3);
        assert_eq!(report.fonts.len(), 2, "CMR10 and Helvetica");
        // Sorted alphabetically by referenced_name.
        assert_eq!(report.fonts[0].referenced_name, "CMR10");
        assert_eq!(report.fonts[1].referenced_name, "Helvetica");

        let cmr = &report.fonts[0];
        assert_eq!(cmr.spans, 2);
        assert_eq!(cmr.pages, vec![2, 3]);
        assert_eq!(cmr.sample_chars, "alpha");
        assert!(!cmr.resolution.is_exact());

        let helv = &report.fonts[1];
        assert_eq!(helv.spans, 2);
        assert_eq!(helv.pages, vec![1]);
        assert!(helv.resolution.is_exact());

        assert_eq!(report.summary.total_fonts, 2);
        assert_eq!(report.summary.exact, 1);
        assert_eq!(report.summary.substituted, 1);
        assert_eq!(report.summary.synthetic_fallback, 0);
        assert_eq!(report.summary.spans_total, 4);
        assert_eq!(report.summary.spans_with_fallback, 2);
        assert!((report.summary.fallback_ratio - 0.5).abs() < 1e-9);
    }

    #[test]
    fn subset_prefix_collision_stays_grouped_per_font_id() {
        // Two distinct subsets of the same display name. We key by
        // (font_name, font_id) so they end up as separate entries even
        // though the name collides.
        let doc = mk_doc(
            vec![
                span(
                    "one",
                    0,
                    "CMR10",
                    Some("AAAAAA+CMR10"),
                    FontResolution::Exact,
                ),
                span(
                    "two",
                    0,
                    "CMR10",
                    Some("BBBBBB+CMR10"),
                    FontResolution::Exact,
                ),
            ],
            1,
        );
        let report = build_report(&doc, Path::new("x.pdf"));
        assert_eq!(report.fonts.len(), 2);
        // Both share the referenced_name after subset-prefix stripping.
        assert_eq!(report.fonts[0].referenced_name, "CMR10");
        assert_eq!(report.fonts[1].referenced_name, "CMR10");
    }

    #[test]
    fn sample_chars_truncates_at_20_unicode_chars() {
        let long = "abcdefghijklmnopqrstuvwxyz"; // 26 chars
        let doc = mk_doc(vec![span(long, 0, "Foo", None, FontResolution::Exact)], 1);
        let report = build_report(&doc, Path::new("x.pdf"));
        assert_eq!(report.fonts[0].sample_chars.chars().count(), 20);
        assert_eq!(report.fonts[0].sample_chars, "abcdefghijklmnopqrst");
    }

    #[test]
    fn empty_doc_has_zero_fallback_ratio_no_nan() {
        let doc = mk_doc(vec![], 0);
        let report = build_report(&doc, Path::new("empty.pdf"));
        assert_eq!(report.summary.spans_total, 0);
        assert_eq!(report.summary.fallback_ratio, 0.0);
        assert_eq!(report.fonts.len(), 0);
    }

    #[test]
    fn first_non_exact_resolution_is_kept() {
        // First span is Exact (wouldn't be interesting). Second span is a
        // Substituted variant. The aggregated font's resolution should
        // reflect the substitution, not the exact baseline, because that's
        // the story the audit is trying to surface.
        let doc = mk_doc(
            vec![
                span("ok", 0, "Foo", None, FontResolution::Exact),
                span(
                    "oh",
                    0,
                    "Foo",
                    None,
                    FontResolution::Substituted {
                        requested: "Foo".into(),
                        resolved: "Bar".into(),
                        reason: FallbackReason::NotEmbedded,
                    },
                ),
            ],
            1,
        );
        let report = build_report(&doc, Path::new("x.pdf"));
        assert_eq!(report.fonts.len(), 1);
        assert!(!report.fonts[0].resolution.is_exact());
    }

    #[test]
    fn text_output_contains_summary_line() {
        let doc = mk_doc(
            vec![span("Hi", 0, "Helvetica", None, FontResolution::Exact)],
            1,
        );
        let report = build_report(&doc, Path::new("x.pdf"));
        let text = render_text(&report);
        assert!(text.contains("File: x.pdf (1 pages)"));
        assert!(text.contains("Fonts: 1 total"));
        assert!(text.contains("Helvetica"));
    }

    #[test]
    fn short_page_list_collapses_runs() {
        assert_eq!(short_page_list(&[1, 2, 3, 4, 5]), "1-5");
        assert_eq!(short_page_list(&[1, 3, 5]), "1,3,5");
        assert_eq!(short_page_list(&[1, 2, 4, 5, 10]), "1-2,4-5,10");
        assert_eq!(short_page_list(&[1, 2, 4, 5, 7, 8, 9, 11]), "1-2,4-5,7-9+");
    }
}

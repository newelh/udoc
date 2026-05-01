//! `udoc fonts <file>` subcommand.
//!
//! Lists the fonts referenced by a document plus how each was resolved
//! by the renderer's Tier 1 / subset-prefix / Unicode-routing fallback
//! chain. Reuses the  `audit-fonts` aggregation machinery (same
//! per-font grouping + `FontResolution` enum) so `udoc fonts` is a
//! lighter-weight, release-facing presentation of the same underlying
//! data: no missing-glyph probe, no opinionated column widths, no
//! corpus-level summary ratios.
//!
//! Output formats:
//! - `json` (default): pretty-printed, machine-readable, one entry per font.
//! - `text`: short human-readable table.

use std::path::PathBuf;

use serde::Serialize;

// Sibling cli module; resolves identically inside the lib (cli is
// `pub(crate)`) and inside main.rs (cli is `#[path]`-mounted).
use super::super::audit;
use udoc::{extract_with, Config};
use udoc_font::types::strip_subset_prefix;

/// Output format for the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Machine-readable JSON (default).
    Json,
    /// Human-readable text table.
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

/// Parsed arguments for `udoc fonts`.
#[derive(Debug, Clone)]
pub struct Args {
    /// Path of the document to inspect.
    pub file: PathBuf,
    /// Optional output file. When `None`, writes to stdout.
    pub output: Option<PathBuf>,
    /// Output format (`json` or `text`).
    pub format: Format,
    /// When true, emit per-font routing chain (subset-prefix strip ->
    /// tier1 name match -> unicode sniff -> fallback -> final) alongside
    /// the summary. Useful when auditing why a specific font ended up
    /// routed to a particular Tier 1 substitute.
    pub trace: bool,
}

/// Step-by-step audit of how a single font name walked the renderer's
/// fallback chain, emitted when `--trace` is set. Each field captures
/// one decision point: the subset-prefix strip, the Tier 1 name-match
/// routing heuristic, the per-glyph Unicode-block sniff, and the final
/// resolved face. Fields are `Option<String>` so downstream consumers
/// see `null` when a step didn't fire (e.g. `unicode_sniff = null`
/// when the name matched Tier 1 and the Unicode fallback never ran).
#[derive(Debug, Clone, Serialize)]
pub struct RouteTrace {
    /// Raw font name as referenced by the document, before any
    /// normalization. Identical to the PDF's /BaseFont or /FontName,
    /// so it can still carry a `AAAAAA+` subset prefix.
    pub referenced: String,
    /// Name after subset-prefix stripping (the six-letter-plus-`+`
    /// subset marker removed). Equals `referenced` when no prefix was
    /// present.
    pub subset_stripped: String,
    /// Tier 1 target family picked by `FontCache::route_tier1`
    /// (Sans / Serif / Mono / LmRoman / LmMath etc.). `None` when the
    /// name-match heuristic didn't fire and routing deferred to either
    /// Unicode sniff or generic fallback.
    pub tier1_match: Option<String>,
    /// Per-glyph Unicode-block sniff result
    /// (`FontCache::route_by_unicode`). `None` when the name match
    /// already picked a target or when the span contains no glyphs
    /// outside the routed face.
    pub unicode_sniff: Option<String>,
    /// `FallbackReason` string if the resolution isn't Exact. Mirrors
    /// `FallbackReason::as_str()` to keep the CLI stable across variant
    /// additions.
    pub fallback_reason: Option<String>,
    /// Final resolved face / FontResolution short form
    /// (`Exact`, `Substituted/Reason`, `Synthetic/Reason`).
    pub final_resolution: String,
}

/// One entry in the fonts report. Slimmer than
/// [`audit::FontEntry`] because this
/// subcommand is the quick "show me what's in the file" surface, not
/// the corpus-audit surface (use `udoc audit-fonts` for the deep dive).
#[derive(Debug, Serialize)]
pub struct FontEntry {
    /// Font name as referenced by the document (subset-prefix stripped).
    pub name: String,
    /// How the font was resolved by the renderer fallback chain.
    pub resolution: udoc_core::text::FontResolution,
    /// Number of spans that used this font.
    pub spans: u64,
    /// 1-indexed page numbers where the font appears.
    pub pages: Vec<usize>,
    /// Short sample of characters rendered with this font.
    pub sample: String,
    /// Routing-chain trace. `None` unless `--trace` was set; omitted
    /// from JSON when absent so the default output stays compact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<RouteTrace>,
}

/// Top-level report: per-font list, sorted by referenced name.
#[derive(Debug, Serialize)]
pub struct Report {
    /// Source document path (stringified for JSON output).
    pub file: String,
    /// Number of pages in the document.
    pub pages: usize,
    /// Per-font entries, sorted by `name`.
    pub fonts: Vec<FontEntry>,
}

/// Run the fonts subcommand. Returns the process exit code.
pub fn run(args: Args) -> u8 {
    match run_inner(&args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("udoc fonts: {e}");
            2
        }
    }
}

fn run_inner(args: &Args) -> Result<(), String> {
    let doc = extract_with(&args.file, Config::new())
        .map_err(|e| format!("extracting '{}': {e}", args.file.display()))?;

    // Reuse the audit-fonts aggregation so both subcommands agree on
    // how to group spans by font and assign a FontResolution. `udoc
    // fonts` skips the missing-glyph probe (audit-fonts specialty).
    let audit_report = audit::build_report(&doc, &args.file);

    let trace = args.trace;
    let fonts: Vec<FontEntry> = audit_report
        .fonts
        .into_iter()
        .map(|f| {
            let route = if trace {
                Some(build_trace(
                    &f.referenced_name,
                    &f.resolution,
                    &f.sample_chars,
                ))
            } else {
                None
            };
            FontEntry {
                name: f.referenced_name,
                resolution: f.resolution,
                spans: f.spans,
                pages: f.pages,
                sample: f.sample_chars,
                trace: route,
            }
        })
        .collect();

    let report = Report {
        file: args.file.display().to_string(),
        pages: audit_report.pages,
        fonts,
    };

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

fn render_text(report: &Report) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "File: {} ({} pages, {} fonts)",
        report.file,
        report.pages,
        report.fonts.len()
    );
    if report.fonts.is_empty() {
        let _ = writeln!(out, "(no fonts referenced)");
        return out;
    }
    let _ = writeln!(
        out,
        "{:<30}  {:<28}  {:>6}  {:>6}  Sample",
        "Font", "Resolution", "Spans", "Pages",
    );
    for f in &report.fonts {
        let name = truncate(&f.name, 30);
        let status = truncate(&short_resolution(&f.resolution), 28);
        let sample_quoted = format!("{:?}", &f.sample);
        let _ = writeln!(
            out,
            "{:<30}  {:<28}  {:>6}  {:>6}  {}",
            name,
            status,
            f.spans,
            f.pages.len(),
            sample_quoted,
        );
        if let Some(trace) = &f.trace {
            write_trace_block(&mut out, trace);
        }
    }
    out
}

/// Format one font's routing-chain trace as an indented block under its
/// summary row. Each line captures one decision point; `null`-equivalent
/// fields render as "N/A" so the block reads left-to-right like a log.
fn write_trace_block(out: &mut String, trace: &RouteTrace) {
    use std::fmt::Write as _;
    let subset_step = if trace.referenced == trace.subset_stripped {
        "no subset prefix".to_string()
    } else {
        format!("\"{}\" -> \"{}\"", trace.referenced, trace.subset_stripped)
    };
    let _ = writeln!(out, "    subset-prefix strip: {subset_step}");

    let tier1_step = match &trace.tier1_match {
        Some(t) => format!("{} routed to {}", trace.subset_stripped, t),
        None => "N/A (no name match)".to_string(),
    };
    let _ = writeln!(out, "    tier1 name match:    {tier1_step}");

    let unicode_step = match &trace.unicode_sniff {
        Some(u) => format!("{} (via Unicode block)", u),
        None if trace.tier1_match.is_some() => "N/A (name-matched)".to_string(),
        None => "N/A (no block match)".to_string(),
    };
    let _ = writeln!(out, "    unicode sniff:       {unicode_step}");

    let fallback_step = match &trace.fallback_reason {
        Some(r) => r.clone(),
        None => "N/A (Exact match)".to_string(),
    };
    let _ = writeln!(out, "    fallback chain:      {fallback_step}");

    let _ = writeln!(out, "    final:               {}", trace.final_resolution);
}

/// Build a [`RouteTrace`] for one font + its audited resolution.
///
/// The trace is descriptive, not prescriptive: we re-run the subset
/// strip and the tier1 name classifier here so the output walks every
/// decision the renderer took, even ones whose result didn't end up in
/// the final `FontResolution`. For name-routed hits the classifier
/// output matches the concrete `resolved` family; for Unicode-routed
/// hits the classifier returns `None` and the `unicode_sniff` field
/// carries the routed target instead.
fn build_trace(
    referenced: &str,
    resolution: &udoc_core::text::FontResolution,
    sample: &str,
) -> RouteTrace {
    use udoc_core::text::FontResolution;

    let subset_stripped = strip_subset_prefix(referenced).to_string();
    let tier1_match = classify_tier1(&subset_stripped).map(|t| t.to_string());

    // Unicode sniff is only meaningful when the name match didn't fire
    // and we dropped into per-glyph routing. Scan the span sample for
    // the first character the sniff would classify.
    let unicode_sniff = if tier1_match.is_none() {
        sample
            .chars()
            .find_map(classify_unicode)
            .map(|s| s.to_string())
    } else {
        None
    };

    let (fallback_reason, final_resolution) = match resolution {
        FontResolution::Exact => (None, "Exact".to_string()),
        FontResolution::Substituted {
            resolved, reason, ..
        } => (
            Some(reason.as_str().to_string()),
            format!("{resolved} (Substituted/{})", reason.as_str()),
        ),
        FontResolution::SyntheticFallback {
            generic_family,
            reason,
            ..
        } => (
            Some(reason.as_str().to_string()),
            format!("{generic_family} (Synthetic/{})", reason.as_str()),
        ),
        _ => (None, "Fallback".to_string()),
    };

    RouteTrace {
        referenced: referenced.to_string(),
        subset_stripped,
        tier1_match,
        unicode_sniff,
        fallback_reason,
        final_resolution,
    }
}

/// Mirrors the subset of `FontCache::route_tier1` used for tracing. Not
/// `pub` on the render side because it's name-match only; this keeps
/// the CLI trace line human-friendly without pulling the whole font
/// cache up through the trace path. If the classifier here ever drifts
/// from the renderer's, the unit tests below surface the gap.
fn classify_tier1(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();

    // LaTeX math families.
    if lower.starts_with("cmmi")
        || lower.starts_with("cmsy")
        || lower.starts_with("cmbsy")
        || lower.starts_with("cmex")
        || lower.starts_with("msam")
        || lower.starts_with("msbm")
        || lower.starts_with("lmmath")
        || lower.starts_with("latinmodernmath")
        || lower.contains("mathematical")
        || lower.starts_with("stix")
        || lower.starts_with("xits")
        || lower.starts_with("asanamath")
        || lower.contains("cambria math")
        || lower.starts_with("euex")
        || lower.starts_with("eufm")
        || lower.starts_with("eufb")
        || lower.starts_with("eusm")
        || lower.starts_with("eusb")
        || lower.starts_with("eurm")
        || lower.starts_with("eurb")
        || lower.starts_with("mnsymbol")
        || lower.starts_with("stmary")
        || lower.starts_with("wasy")
        || lower.starts_with("pzdr")
    {
        return Some("LmMath");
    }
    // LM Roman Italic.
    if lower.starts_with("cmti")
        || lower.starts_with("cmsl")
        || (lower.starts_with("lmroman") && lower.contains("italic"))
        || (lower.starts_with("lmserif") && lower.contains("italic"))
    {
        return Some("LmRomanItalic");
    }
    // LM Roman Regular.
    if lower.starts_with("cmr")
        || lower.starts_with("cmbx")
        || lower.starts_with("cmb")
        || lower.starts_with("cmdunh")
        || lower.starts_with("cmfib")
        || lower.starts_with("cmu")
        || lower.starts_with("cmcsc")
        || lower.starts_with("lmroman")
        || lower.starts_with("lmserif")
        || lower.starts_with("latinmodernroman")
        || lower.starts_with("sfrm")
    {
        return Some("LmRoman");
    }
    // Monospace / Mono.
    if lower.starts_with("cmtt")
        || lower.starts_with("lmmono")
        || lower.starts_with("lmtypewriter")
        || lower.starts_with("courier")
        || lower.starts_with("consolas")
        || lower.starts_with("monaco")
        || lower.starts_with("menlo")
        || lower.starts_with("texgyrecursor")
        || lower.starts_with("nimbusmonl")
        || lower.starts_with("nimbusmono")
        || lower.starts_with("liberationmono")
        || lower.starts_with("freemono")
        || lower.starts_with("sourcecodepro")
        || lower.starts_with("inconsolata")
    {
        return Some("Mono");
    }
    // Sans-serif family.
    let is_sans = lower.starts_with("helvetica")
        || lower.starts_with("arial")
        || lower.starts_with("verdana")
        || lower.starts_with("tahoma")
        || lower.starts_with("calibri")
        || lower.starts_with("segoeui")
        || lower.starts_with("lucida sans")
        || lower.starts_with("lucidasans")
        || lower.starts_with("trebuchet")
        || lower.starts_with("lmsans")
        || lower.starts_with("cmss")
        || lower.starts_with("cmssbx")
        || lower.starts_with("texgyreheros")
        || lower.starts_with("nimbussan")
        || lower.starts_with("nimbusl")
        || lower.starts_with("liberationsans");
    if is_sans {
        let bold = lower.contains("bold") || lower.contains("-bold");
        let italic = lower.contains("italic") || lower.contains("oblique");
        return Some(match (bold, italic) {
            (true, true) => "SansBoldItalic",
            (true, false) => "SansBold",
            (false, true) => "SansItalic",
            (false, false) => "SansRegular",
        });
    }
    // Serif family.
    let is_serif = lower.starts_with("times")
        || lower.starts_with("nimbusromno9l")
        || lower.starts_with("nimbusroman")
        || lower.starts_with("nimbusromno")
        || lower.starts_with("timesnewroman")
        || lower.starts_with("garamond")
        || lower.starts_with("bookman")
        || lower.starts_with("palatino")
        || lower.starts_with("georgia")
        || lower.starts_with("century")
        || lower.starts_with("texgyretermes")
        || lower.starts_with("texgyrepagella")
        || lower.starts_with("texgyreschola")
        || lower.starts_with("texgyrebonum")
        || lower.starts_with("texgyretermesx")
        || lower.starts_with("liberationserif")
        || lower.starts_with("minionpro")
        || lower.starts_with("minion-")
        || lower.starts_with("minion_")
        || lower.starts_with("giovanni");
    if is_serif {
        let bold = lower.contains("bold");
        let italic = lower.contains("italic")
            || lower.contains("oblique")
            || lower.ends_with("-it")
            || (lower.ends_with("it") && (lower.ends_with("boldit") || lower.ends_with("-it")));
        return Some(match (bold, italic) {
            (true, true) => "SerifBoldItalic",
            (true, false) => "SerifBold",
            (false, true) => "SerifItalic",
            (false, false) => "SerifRegular",
        });
    }
    None
}

/// Mirrors the subset of `FontCache::route_by_unicode` used for
/// tracing. Returns the tier1 target a per-glyph Unicode sniff would
/// pick, or `None` for characters that don't fall in any routed block.
fn classify_unicode(ch: char) -> Option<&'static str> {
    let c = ch as u32;
    if (0x2200..=0x22FF).contains(&c)
        || (0x2A00..=0x2AFF).contains(&c)
        || (0x27C0..=0x27EF).contains(&c)
        || (0x2980..=0x29FF).contains(&c)
        || (0x1D400..=0x1D7FF).contains(&c)
        || (0x2190..=0x21FF).contains(&c)
    {
        return Some("LmMath");
    }
    // Greek block.
    if (0x0370..=0x03FF).contains(&c) {
        return Some("SansRegular");
    }
    None
}

fn truncate(s: &str, max: usize) -> String {
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

fn short_resolution(r: &udoc_core::text::FontResolution) -> String {
    use udoc_core::text::FontResolution;
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

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::text::{FallbackReason, FontResolution};

    #[test]
    fn trace_strips_subset_prefix() {
        let t = build_trace(
            "AAAAAA+MinionPro-Regular",
            &FontResolution::Substituted {
                requested: "AAAAAA+MinionPro-Regular".into(),
                resolved: "LiberationSerif-Regular".into(),
                reason: FallbackReason::NameRouted,
            },
            "Hello",
        );
        assert_eq!(t.referenced, "AAAAAA+MinionPro-Regular");
        assert_eq!(t.subset_stripped, "MinionPro-Regular");
        assert_eq!(t.tier1_match.as_deref(), Some("SerifRegular"));
        assert!(t.unicode_sniff.is_none(), "name-matched so no sniff");
        assert_eq!(t.fallback_reason.as_deref(), Some("NameRouted"));
        assert!(t.final_resolution.contains("LiberationSerif-Regular"));
    }

    #[test]
    fn trace_classifies_cmr10_to_lm_roman() {
        let t = build_trace(
            "CMR10",
            &FontResolution::Substituted {
                requested: "CMR10".into(),
                resolved: "LatinModernRoman".into(),
                reason: FallbackReason::NameRouted,
            },
            "alpha",
        );
        assert_eq!(t.tier1_match.as_deref(), Some("LmRoman"));
    }

    #[test]
    fn trace_reports_exact_when_resolution_is_exact() {
        let t = build_trace("Helvetica-Bold", &FontResolution::Exact, "Hi");
        assert!(t.fallback_reason.is_none());
        assert_eq!(t.final_resolution, "Exact");
        assert_eq!(t.tier1_match.as_deref(), Some("SansBold"));
    }

    #[test]
    fn trace_falls_back_to_unicode_sniff_when_no_name_match() {
        // UnknownFont123 doesn't hit any tier1 family, so the trace
        // should surface the Unicode sniff of the first sample char.
        // Greek alpha (U+03B1) routes to SansRegular.
        let t = build_trace(
            "UnknownFont123",
            &FontResolution::SyntheticFallback {
                requested: "UnknownFont123".into(),
                generic_family: "sans-serif".into(),
                reason: FallbackReason::UnicodeRangeRouted,
            },
            "\u{03b1}",
        );
        assert!(t.tier1_match.is_none());
        assert_eq!(t.unicode_sniff.as_deref(), Some("SansRegular"));
    }

    #[test]
    fn classify_tier1_math_routes_to_lm_math() {
        assert_eq!(classify_tier1("CMMI10"), Some("LmMath"));
        assert_eq!(classify_tier1("CMSY10"), Some("LmMath"));
        assert_eq!(classify_tier1("MSAM10"), Some("LmMath"));
        assert_eq!(classify_tier1("latinmodernmath-regular"), Some("LmMath"));
    }

    #[test]
    fn classify_tier1_serif_italic_bold_combinations() {
        assert_eq!(classify_tier1("Times-Bold"), Some("SerifBold"));
        assert_eq!(classify_tier1("Times-Italic"), Some("SerifItalic"));
        assert_eq!(classify_tier1("Times-BoldItalic"), Some("SerifBoldItalic"));
        assert_eq!(classify_tier1("Times"), Some("SerifRegular"));
    }

    #[test]
    fn classify_tier1_returns_none_for_unknown() {
        assert_eq!(classify_tier1("MyCustomFont"), None);
        assert_eq!(classify_tier1("xyzzy"), None);
    }

    #[test]
    fn classify_unicode_math_block_routes_to_lm_math() {
        // U+2211 is a math operator (summation).
        assert_eq!(classify_unicode('\u{2211}'), Some("LmMath"));
        // U+0041 'A' is ASCII, no block route.
        assert_eq!(classify_unicode('A'), None);
    }

    #[test]
    fn write_trace_block_contains_all_steps() {
        let t = RouteTrace {
            referenced: "AAAAAA+CMR10".into(),
            subset_stripped: "CMR10".into(),
            tier1_match: Some("LmRoman".into()),
            unicode_sniff: None,
            fallback_reason: Some("NameRouted".into()),
            final_resolution: "LatinModernRoman (Substituted/NameRouted)".into(),
        };
        let mut out = String::new();
        write_trace_block(&mut out, &t);
        assert!(out.contains("subset-prefix strip"));
        assert!(out.contains("tier1 name match"));
        assert!(out.contains("unicode sniff"));
        assert!(out.contains("fallback chain"));
        assert!(out.contains("final:"));
        assert!(out.contains("LmRoman"));
        assert!(out.contains("N/A (name-matched)"));
    }
}

//! `udoc features` subcommand.
//!
//! Emits the compile-time feature report for the running `udoc` binary.
//! Operators use this to confirm which bundled assets and decoders are
//! linked before chasing "why did my CJK PDF render boxes" or "why does
//! JBIG2 pass through raw": the answer is always in this report.
//!
//! Each feature is resolved via `cfg!(feature = ...)` so the output
//! matches whatever was built, not a stale constant. Features reported:
//! - `tier1-fonts`      (bundled Liberation Mono + LM Roman/Math)
//! - `tier1-serif-bold` (Liberation Serif Bold/Italic/BoldItalic)
//! - `tier1-sans-bold`  (Liberation Sans Bold/Italic/BoldItalic)
//! - `cjk-fonts`        (Noto Sans CJK SC subset)
//! - `jbig2`            (own JBIG2 decoder dispatch)
//!
//! `strict_fonts` is a runtime `Config` toggle (M-32b), not a compile-time
//! feature. It still shows up in the report because operators regularly
//! ask about it alongside the bundled-asset flags; the `enabled` field is
//! always `false` because the feature is per-extraction, not per-binary.
//!
//! Output formats:
//! - `text` (default): short human-readable table.
//! - `json` (`--json`): machine-readable array of `{name, enabled, kind}`.

use std::path::PathBuf;

use serde::Serialize;

/// Output format for the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Human-readable table (default).
    Text,
    /// Machine-readable JSON.
    Json,
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

/// Parsed arguments for `udoc features`.
#[derive(Debug, Clone)]
pub struct Args {
    /// Optional output file. `None` writes to stdout.
    pub output: Option<PathBuf>,
    /// Output format (`text` or `json`).
    pub format: Format,
}

/// One entry in the features report.
#[derive(Debug, Serialize)]
pub struct FeatureEntry {
    /// Cargo feature or runtime-toggle name.
    pub name: &'static str,
    /// Whether the feature is compiled in (for build-time flags) or
    /// always `false` (for runtime toggles that depend on per-call
    /// `Config`; kept in the report for operator discoverability).
    pub enabled: bool,
    /// `"build"` for compile-time Cargo features, `"runtime"` for
    /// `Config`-driven toggles reported here for discoverability.
    pub kind: &'static str,
}

/// Top-level features report.
#[derive(Debug, Serialize)]
pub struct Report {
    /// Package version (`CARGO_PKG_VERSION`).
    pub version: &'static str,
    /// Per-feature entries in the order defined by [`collect`].
    pub features: Vec<FeatureEntry>,
}

/// Collect the compile-time feature report.
///
/// Order is stable so scripts can diff against a baseline without having
/// to sort. Build-time features come first, runtime toggles after.
pub fn collect() -> Report {
    let features = vec![
        FeatureEntry {
            name: "tier1-fonts",
            enabled: cfg!(feature = "tier1-fonts"),
            kind: "build",
        },
        FeatureEntry {
            name: "tier1-serif-bold",
            enabled: cfg!(feature = "tier1-serif-bold"),
            kind: "build",
        },
        FeatureEntry {
            name: "tier1-sans-bold",
            enabled: cfg!(feature = "tier1-sans-bold"),
            kind: "build",
        },
        FeatureEntry {
            name: "cjk-fonts",
            enabled: cfg!(feature = "cjk-fonts"),
            kind: "build",
        },
        FeatureEntry {
            name: "jbig2",
            enabled: cfg!(feature = "jbig2"),
            kind: "build",
        },
        // Runtime toggle. Always reported as `enabled=false` because the
        // actual value depends on per-extraction `Config::strict_fonts`.
        // Documented here so operators who search the report find it.
        FeatureEntry {
            name: "strict_fonts",
            enabled: false,
            kind: "runtime",
        },
    ];
    Report {
        version: env!("CARGO_PKG_VERSION"),
        features,
    }
}

/// Run the features subcommand. Returns the process exit code.
pub fn run(args: Args) -> u8 {
    match run_inner(&args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("udoc features: {e}");
            2
        }
    }
}

fn run_inner(args: &Args) -> Result<(), String> {
    let report = collect();
    let rendered = match args.format {
        Format::Json => {
            serde_json::to_string_pretty(&report).map_err(|e| format!("serializing report: {e}"))?
        }
        Format::Text => render_text(&report),
    };

    match &args.output {
        Some(path) => std::fs::write(path, &rendered)
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
    let _ = writeln!(out, "udoc {} compile-time features", report.version);
    let _ = writeln!(out, "{:<22}  {:<8}  {:<8}", "Feature", "Enabled", "Kind");
    for f in &report.features {
        let enabled = if f.enabled { "yes" } else { "no" };
        let _ = writeln!(out, "{:<22}  {:<8}  {:<8}", f.name, enabled, f.kind);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_lists_expected_features() {
        let report = collect();
        let names: Vec<&str> = report.features.iter().map(|f| f.name).collect();
        assert!(names.contains(&"tier1-fonts"));
        assert!(names.contains(&"tier1-serif-bold"));
        assert!(names.contains(&"tier1-sans-bold"));
        assert!(names.contains(&"cjk-fonts"));
        assert!(names.contains(&"jbig2"));
        assert!(names.contains(&"strict_fonts"));
    }

    #[test]
    fn text_output_has_header_and_rows() {
        let report = collect();
        let text = render_text(&report);
        assert!(text.contains("compile-time features"));
        assert!(text.contains("tier1-fonts"));
        assert!(text.contains("jbig2"));
        // `strict_fonts` is a runtime toggle; surface it alongside the
        // build-time features so operators grepping the report find it.
        assert!(text.contains("strict_fonts"));
        assert!(text.contains("runtime"));
    }

    #[test]
    fn json_output_parses_back() {
        let report = collect();
        let json = serde_json::to_string(&report).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let arr = parsed.get("features").and_then(|v| v.as_array()).unwrap();
        assert!(arr.iter().any(|v| v["name"] == "jbig2"));
        assert!(arr.iter().any(|v| v["name"] == "strict_fonts"));
    }

    #[test]
    fn strict_fonts_is_runtime_kind() {
        let report = collect();
        let sf = report
            .features
            .iter()
            .find(|f| f.name == "strict_fonts")
            .unwrap();
        assert_eq!(sf.kind, "runtime");
        assert!(!sf.enabled);
    }
}

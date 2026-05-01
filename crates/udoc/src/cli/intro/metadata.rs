//! `udoc metadata <file>` subcommand.
//!
//! Emits structured metadata (title, author, producer, creation/
//! modification dates, page count, extended properties) extracted from
//! a document. Reuses the same extraction pipeline as `udoc extract`,
//! then narrows the output to [`DocumentMetadata`] so callers can pipe
//! the result into jq without wading through the full document tree.
//!
//! Output formats:
//! - `json` (default): pretty-printed metadata JSON.
//! - `text`: `Key: value` lines, one per populated field.

use std::path::PathBuf;

use serde::Serialize;

use udoc::{extract_with, Config, DocumentMetadata};

/// Output format for the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Machine-readable JSON (default).
    Json,
    /// Human-readable `Key: value` text lines.
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

/// Parsed arguments for `udoc metadata`.
#[derive(Debug, Clone)]
pub struct Args {
    /// Path of the document to read.
    pub file: PathBuf,
    /// Optional output file. When `None`, writes to stdout.
    pub output: Option<PathBuf>,
    /// Output format (`json` or `text`).
    pub format: Format,
}

/// Top-level JSON payload. Keeps the shape explicit (rather than
/// serialising `Document::metadata` directly) so a `file` field can
/// front the report for users batching across many docs.
#[derive(Debug, Serialize)]
struct Report<'a> {
    file: String,
    /// Dublin Core / PDF Info dictionary plus extended properties.
    metadata: &'a DocumentMetadata,
}

/// Run the metadata subcommand. Returns the process exit code.
pub fn run(args: Args) -> u8 {
    match run_inner(&args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("udoc metadata: {e}");
            2
        }
    }
}

fn run_inner(args: &Args) -> Result<(), String> {
    let doc = extract_with(&args.file, Config::new())
        .map_err(|e| format!("extracting '{}': {e}", args.file.display()))?;

    let report = Report {
        file: args.file.display().to_string(),
        metadata: &doc.metadata,
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

fn render_text(report: &Report<'_>) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "File: {}", report.file);
    let m = report.metadata;
    if let Some(t) = &m.title {
        let _ = writeln!(out, "Title: {t}");
    }
    if let Some(a) = &m.author {
        let _ = writeln!(out, "Author: {a}");
    }
    if let Some(s) = &m.subject {
        let _ = writeln!(out, "Subject: {s}");
    }
    if let Some(c) = &m.creator {
        let _ = writeln!(out, "Creator: {c}");
    }
    if let Some(p) = &m.producer {
        let _ = writeln!(out, "Producer: {p}");
    }
    if let Some(d) = &m.creation_date {
        let _ = writeln!(out, "Created: {d}");
    }
    if let Some(d) = &m.modification_date {
        let _ = writeln!(out, "Modified: {d}");
    }
    let _ = writeln!(out, "Pages: {}", m.page_count);
    if !m.properties.is_empty() {
        let _ = writeln!(out, "Properties:");
        // Sort for deterministic output; HashMap iteration order is random.
        let mut keys: Vec<&String> = m.properties.keys().collect();
        keys.sort();
        for k in keys {
            let _ = writeln!(out, "  {k}: {}", m.properties[k]);
        }
    }
    out
}

//! `udoc inspect <file>` subcommand.
//!
//! Quick "tell me about this document" probe. Emits JSON with the
//! signals an agent needs to decide which extraction shape to drive
//! next: format, page count, has-text, likely-scanned, has-tables,
//! has-encryption, font-resolution counts, file size, sampling
//! confidence.
//!
//! Per  §5.3 + Domain Expert spec + friction-deepdive A3
//! (extraction_format_hint + recommended_out_modes added so an agent
//! that hits an "unknown format" friction self-recovers from this
//! one call).
//!
//! Sampling: the expensive signals (text presence, table presence,
//! image presence, font resolution) are computed on five spread-out
//! pages -- `[0, mid-1, mid, mid+1, last]`. The spread guards against
//! the "scanned report with text title page" pathology that a naive
//! `[0..5]` would miss. `--full` forces a complete walk and updates
//! `confidence` to `"complete"`.
//!
//! Performance budget (per AC #6): <500ms wall on a 100-page PDF in
//! sampled mode; <1s for non-PDF formats.

use std::path::PathBuf;

use serde::Serialize;

use udoc::{Extractor, Format};
use udoc_core::text::FontResolution;

/// Parsed arguments for `udoc inspect`.
#[derive(Debug, Clone)]
pub struct Args {
    /// Document path.
    pub file: PathBuf,
    /// Force a complete scan (every page) instead of sampled mode.
    pub full: bool,
}

/// JSON shape emitted on stdout.
#[derive(Debug, Serialize)]
pub struct Report {
    /// Detected format ("pdf", "docx", ...). Lowercase.
    pub format: String,
    /// Total page count from the format's metadata.
    pub page_count: usize,
    /// Whether expensive signals came from a sample or a full scan.
    pub sampled: bool,
    /// Number of pages sampled. 0 when the document is empty.
    pub sample_size: usize,
    /// Page indices that were actually probed.
    pub sample_pages: Vec<usize>,
    /// True iff at least one sampled page yielded non-whitespace text.
    pub has_text: bool,
    /// True iff `has_text=false` and at least one sampled page has an
    /// image (the canonical scanned-report signature).
    pub likely_scanned: bool,
    /// True iff at least one sampled page surfaces a table.
    pub has_tables: bool,
    /// True iff the source file declares encryption (PDF only).
    pub has_encryption: bool,
    /// Font resolution counts. PDF-only; zero for other formats.
    pub font_resolution: FontResolutionCounts,
    /// Source file size in bytes.
    pub file_size_bytes: u64,
    /// "sampled" or "complete".
    pub confidence: &'static str,
    /// Format hint string for `--input-format` (per friction-deepdive
    /// A3 -- helps agents who hit "unknown format 'text'" recover by
    /// learning what input-format value to pass).
    pub extraction_format_hint: String,
    /// Recommended `--out` modes for this format. Plain hint; the CLI
    /// accepts more than what's listed here (the list is the typical
    /// agent menu, not an exhaustive enumeration).
    pub recommended_out_modes: Vec<&'static str>,
}

/// Per-font resolution counts from a sampled run.
#[derive(Debug, Default, Serialize)]
pub struct FontResolutionCounts {
    pub resolved: u64,
    pub substituted: u64,
    pub missing: u64,
}

/// Run the inspect subcommand. Returns the process exit code.
pub fn run(args: Args) -> u8 {
    match run_inner(&args) {
        Ok(report) => match serde_json::to_string_pretty(&report) {
            Ok(json) => {
                println!("{json}");
                0
            }
            Err(e) => {
                eprintln!("udoc inspect: serializing report: {e}");
                1
            }
        },
        Err(e) => {
            eprintln!("udoc inspect: {e}");
            1
        }
    }
}

fn run_inner(args: &Args) -> Result<Report, String> {
    let file_size_bytes = std::fs::metadata(&args.file)
        .map_err(|e| format!("stat '{}': {e}", args.file.display()))?
        .len();

    // Detect format from path (cheap signal).
    let format = udoc::detect::detect_format_path(&args.file)
        .map_err(|e| format!("detecting format: {e}"))?
        .ok_or_else(|| format!("unable to detect format for '{}'", args.file.display()))?;

    // Open lightweight extractor. This forwards to the format backend's
    // open path which performs the cheap header parse for paginated
    // formats and the ZIP+manifest walk for OOXML.
    //
    // Encryption (PDF) surfaces here as an Err with the encryption error
    // kind; we capture that as a non-fatal signal (has_encryption=true)
    // and short-circuit the rest of the report.
    let mut ext = match Extractor::open(&args.file) {
        Ok(e) => e,
        Err(e) => {
            // typed encryption check via the core
            // Error accessor instead of a substring match on the
            // displayed message. The PDF backend converts its
            // EncryptionError into Error::encryption_required(reason)
            // at the FormatBackend boundary (see
            // udoc_pdf::convert::convert_error), so this works without
            // udoc inspect knowing PDF specifics.
            if e.is_encryption_error() {
                return Ok(Report {
                    format: format_name(format).to_string(),
                    page_count: 0,
                    sampled: !args.full,
                    sample_size: 0,
                    sample_pages: Vec::new(),
                    has_text: false,
                    likely_scanned: false,
                    has_tables: false,
                    has_encryption: true,
                    font_resolution: FontResolutionCounts::default(),
                    file_size_bytes,
                    confidence: if args.full { "complete" } else { "sampled" },
                    extraction_format_hint: format_name(format).to_string(),
                    recommended_out_modes: recommended_modes(format),
                });
            }
            return Err(format!("opening '{}': {e}", args.file.display()));
        }
    };

    // W0-IS-ENCRYPTED: success path also surfaces the encryption flag,
    // covering the case where the user supplied a correct password and
    // extraction succeeded against an encrypted source.
    let has_encryption = ext.is_encrypted();

    let page_count = ext.page_count();
    let sample_pages = if args.full {
        (0..page_count).collect::<Vec<_>>()
    } else {
        sampled_indices(page_count)
    };

    // Walk sampled pages and accumulate signals.
    let mut has_text = false;
    let mut has_images = false;
    let mut has_tables = false;
    let mut font_counts = FontResolutionCounts::default();

    for &idx in &sample_pages {
        if let Ok(text) = ext.page_text(idx) {
            if text.chars().any(|c| !c.is_whitespace()) {
                has_text = true;
            }
        }
        if let Ok(images) = ext.page_images(idx) {
            if !images.is_empty() {
                has_images = true;
            }
        }
        if let Ok(tables) = ext.page_tables(idx) {
            if !tables.is_empty() {
                has_tables = true;
            }
        }
        if let Ok(spans) = ext.page_spans(idx) {
            for span in &spans {
                match &span.font_resolution {
                    FontResolution::Exact => font_counts.resolved += 1,
                    FontResolution::Substituted { .. } => font_counts.substituted += 1,
                    FontResolution::SyntheticFallback { .. } => font_counts.missing += 1,
                    // FontResolution is non_exhaustive; treat any future
                    // variant as "missing" until the report is updated.
                    _ => font_counts.missing += 1,
                }
            }
        }
    }

    let likely_scanned = !has_text && has_images;

    Ok(Report {
        format: format_name(format).to_string(),
        page_count,
        sampled: !args.full,
        sample_size: sample_pages.len(),
        sample_pages,
        has_text,
        likely_scanned,
        has_tables,
        has_encryption,
        font_resolution: font_counts,
        file_size_bytes,
        confidence: if args.full { "complete" } else { "sampled" },
        extraction_format_hint: format_name(format).to_string(),
        recommended_out_modes: recommended_modes(format),
    })
}

/// Spread-out sample of up to 5 page indices: `[0, mid-1, mid, mid+1, last]`,
/// deduplicated and sorted. For documents with fewer than 5 pages, returns
/// every page. Empty docs return an empty vec.
///
/// Per  §5.3 + Domain Expert spec: NOT `[0..5]`, which would
/// miss the scanned-report-with-text-title-page pathology.
pub(crate) fn sampled_indices(page_count: usize) -> Vec<usize> {
    if page_count == 0 {
        return Vec::new();
    }
    if page_count <= 5 {
        return (0..page_count).collect();
    }
    let mid = page_count / 2;
    let mut idx = vec![0, mid.saturating_sub(1), mid, mid + 1, page_count - 1];
    idx.sort_unstable();
    idx.dedup();
    idx
}

fn format_name(f: Format) -> &'static str {
    match f {
        Format::Pdf => "pdf",
        Format::Docx => "docx",
        Format::Xlsx => "xlsx",
        Format::Pptx => "pptx",
        Format::Doc => "doc",
        Format::Xls => "xls",
        Format::Ppt => "ppt",
        Format::Odt => "odt",
        Format::Ods => "ods",
        Format::Odp => "odp",
        Format::Rtf => "rtf",
        Format::Md => "md",
        // Format is non_exhaustive; fall back to lowercase Debug
        // representation for any future variant.
        _ => "unknown",
    }
}

fn recommended_modes(f: Format) -> Vec<&'static str> {
    match f {
        // Document-shaped formats: full text + structured exports +
        // markdown/chunk emission make sense.
        Format::Pdf | Format::Docx | Format::Doc | Format::Odt | Format::Rtf | Format::Md => {
            vec!["text", "json", "markdown", "chunks"]
        }
        // Presentation-shaped formats: same menu as document; chunks
        // tend to be slide-bounded.
        Format::Pptx | Format::Ppt | Format::Odp => {
            vec!["text", "json", "markdown", "chunks"]
        }
        // Spreadsheet-shaped formats: TSV is the right primary;
        // text/json keep their slot.
        Format::Xlsx | Format::Xls | Format::Ods => vec!["tsv", "json", "text"],
        _ => vec!["text", "json"],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampled_indices_handles_empty() {
        assert!(sampled_indices(0).is_empty());
    }

    #[test]
    fn sampled_indices_returns_all_for_small_docs() {
        assert_eq!(sampled_indices(1), vec![0]);
        assert_eq!(sampled_indices(3), vec![0, 1, 2]);
        assert_eq!(sampled_indices(5), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn sampled_indices_spreads_for_large_docs() {
        // 100-page doc: [0, 49, 50, 51, 99]. Critical -- NOT [0..5].
        let idx = sampled_indices(100);
        assert_eq!(idx, vec![0, 49, 50, 51, 99]);
    }

    #[test]
    fn sampled_indices_handles_six_pages() {
        // mid=3, indices {0, 2, 3, 4, 5} -> sorted [0,2,3,4,5].
        let idx = sampled_indices(6);
        assert_eq!(idx, vec![0, 2, 3, 4, 5]);
    }

    #[test]
    fn sampled_indices_dedups() {
        // page_count=2 returns all; verify no duplicates anywhere.
        for n in 1..=200 {
            let idx = sampled_indices(n);
            let mut sorted = idx.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(idx, sorted, "indices not unique for n={n}");
            for &i in &idx {
                assert!(i < n, "index {i} out of range for n={n}");
            }
        }
    }

    #[test]
    fn recommended_modes_pdf_includes_chunks() {
        let modes = recommended_modes(Format::Pdf);
        assert!(modes.contains(&"chunks"));
        assert!(modes.contains(&"markdown"));
    }

    #[test]
    fn recommended_modes_xlsx_leads_with_tsv() {
        let modes = recommended_modes(Format::Xlsx);
        assert_eq!(modes.first(), Some(&"tsv"));
    }
}

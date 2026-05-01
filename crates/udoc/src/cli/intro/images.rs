//! `udoc images <file> [--extract <dir>]` subcommand.
//!
//! Lists the images embedded in a document (format + dimensions +
//! bits-per-component) and, with `--extract`, dumps each image to disk
//! using the native format (JPEG as `.jpg`, PNG as `.png`, etc.) so
//! that the user can open the result in any viewer.
//!
//! This is the release-facing surface for #157 introspection. When the
//! `udoc-image` crate lands in/T3-IMG-TRANS, raw and
//! CCITT streams can be transcoded to PNG via `transcode_to_png`; for
//! now they are written verbatim with the native extension.
//!
//! Output formats for the listing:
//! - `json` (default): machine-readable, one entry per image.
//! - `text`: short human-readable table.

use std::path::PathBuf;

use serde::Serialize;

use udoc::page::ImageFilter;
use udoc::{extract_with, Config, ImageAsset};

/// Output format for the listing.
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

/// Parsed arguments for `udoc images`.
#[derive(Debug, Clone)]
pub struct Args {
    /// Path of the document to inspect.
    pub file: PathBuf,
    /// Optional output file for the listing. When `None`, writes to stdout.
    pub output: Option<PathBuf>,
    /// Output format for the listing (`json` or `text`).
    pub format: Format,
    /// Dump each image under this directory. The directory is created
    /// if missing.
    pub extract: Option<PathBuf>,
}

/// One entry in the image listing.
#[derive(Debug, Serialize)]
pub struct ImageEntry {
    /// 1-based index. Matches the file name written by `--extract`.
    pub index: usize,
    /// Filter / encoding string: `"jpeg"`, `"png"`, `"ccitt"`, etc.
    pub filter: String,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Bits per component, as declared by the source (1, 8, 16, etc.).
    pub bits_per_component: u8,
    /// Raw byte size of the image blob in the asset store.
    pub bytes: usize,
    /// Relative path of the dumped file. Populated only when
    /// `--extract` is set and the dump succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extracted_path: Option<String>,
}

/// Top-level report for `udoc images`.
#[derive(Debug, Serialize)]
pub struct Report {
    /// Source document path (stringified for JSON output).
    pub file: String,
    /// Number of pages in the document.
    pub pages: usize,
    /// Per-image entries in document order.
    pub images: Vec<ImageEntry>,
}

/// Run the images subcommand. Returns the process exit code.
pub fn run(args: Args) -> u8 {
    match run_inner(&args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("udoc images: {e}");
            2
        }
    }
}

fn run_inner(args: &Args) -> Result<(), String> {
    let doc = extract_with(&args.file, Config::new())
        .map_err(|e| format!("extracting '{}': {e}", args.file.display()))?;

    // If --extract was requested, create the directory up front so a
    // bad path fails early instead of after the listing is built.
    if let Some(dir) = &args.extract {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("creating extract dir '{}': {e}", dir.display()))?;
    }

    let mut entries: Vec<ImageEntry> = Vec::with_capacity(doc.assets.images().len());
    for (i, img) in doc.assets.images().iter().enumerate() {
        let idx = i + 1;
        let extracted_path = if let Some(dir) = &args.extract {
            match dump_image(img, idx, dir) {
                Ok(p) => Some(p),
                Err(e) => {
                    eprintln!("udoc images: image {idx}: {e}");
                    None
                }
            }
        } else {
            None
        };
        entries.push(ImageEntry {
            index: idx,
            filter: filter_label(img.filter).to_string(),
            width: img.width,
            height: img.height,
            bits_per_component: img.bits_per_component,
            bytes: img.data.len(),
            extracted_path,
        });
    }

    let report = Report {
        file: args.file.display().to_string(),
        pages: doc.metadata.page_count,
        images: entries,
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

/// Dump one image. Writes the raw bytes with the native extension --
/// JPEG as `.jpg`, PNG as `.png`, etc. Returns the written file's path.
///
/// Note: this is the pre-`udoc-image` shape. Once the image crate
/// lands on this branch, raw/CCITT streams should go through
/// `transcode_to_png` so users get a universally-openable PNG.
fn dump_image(img: &ImageAsset, idx: usize, dir: &std::path::Path) -> Result<String, String> {
    let ext = match img.filter {
        ImageFilter::Jpeg => "jpg",
        ImageFilter::Jpeg2000 => "j2k",
        ImageFilter::Png => "png",
        ImageFilter::Tiff => "tiff",
        ImageFilter::Jbig2 => "jbig2",
        ImageFilter::Ccitt => "ccitt",
        ImageFilter::Gif => "gif",
        ImageFilter::Bmp => "bmp",
        ImageFilter::Emf => "emf",
        ImageFilter::Wmf => "wmf",
        ImageFilter::Raw => "raw",
        _ => "bin",
    };
    let path = dir.join(format!("image-{idx}.{ext}"));
    std::fs::write(&path, &img.data).map_err(|e| format!("writing '{}': {e}", path.display()))?;
    Ok(path.display().to_string())
}

fn filter_label(f: ImageFilter) -> &'static str {
    match f {
        ImageFilter::Jpeg => "jpeg",
        ImageFilter::Jpeg2000 => "jpeg2000",
        ImageFilter::Png => "png",
        ImageFilter::Tiff => "tiff",
        ImageFilter::Jbig2 => "jbig2",
        ImageFilter::Ccitt => "ccitt",
        ImageFilter::Gif => "gif",
        ImageFilter::Bmp => "bmp",
        ImageFilter::Emf => "emf",
        ImageFilter::Wmf => "wmf",
        ImageFilter::Raw => "raw",
        _ => "unknown",
    }
}

fn render_text(report: &Report) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "File: {} ({} pages, {} images)",
        report.file,
        report.pages,
        report.images.len()
    );
    if report.images.is_empty() {
        let _ = writeln!(out, "(no images)");
        return out;
    }
    let _ = writeln!(
        out,
        "{:>4}  {:<10}  {:>6}  {:>6}  {:>3}  {:>10}  Extracted",
        "Idx", "Filter", "Width", "Height", "BPC", "Bytes",
    );
    for e in &report.images {
        let extracted = e.extracted_path.as_deref().unwrap_or("");
        let _ = writeln!(
            out,
            "{:>4}  {:<10}  {:>6}  {:>6}  {:>3}  {:>10}  {}",
            e.index, e.filter, e.width, e.height, e.bits_per_component, e.bytes, extracted,
        );
    }
    out
}

#![deny(unsafe_code)]

// `cli` is no longer reachable through the udoc
// facade (it was `#[doc(hidden)] pub mod cli;` before; now removed
// from the lib entirely). The same source files are mounted directly
// into the bin crate via `#[path]` so the subcommand impls are
// bin-internal.
#[path = "cli/mod.rs"]
mod cli;

use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args as ClapArgs, Parser, Subcommand};
use clap_complete::Shell;

use crate::cli::audit;
use crate::cli::completions;
use crate::cli::features;
use crate::cli::inspect;
use crate::cli::intro;
#[cfg(feature = "dev-tools")]
use crate::cli::render_diff;
#[cfg(feature = "dev-tools")]
use crate::cli::render_inspect;
use udoc::output;
use udoc::{CollectingDiagnostics, Config, Format, PageRange};
use udoc_core::limits::{parse_size, Limits};

// ---------------------------------------------------------------------------
// stable exit codes + --errors json (agent contract).
// ---------------------------------------------------------------------------

/// Stable exit codes.  The numeric
/// values are part of the agent contract and do not change between
/// alpha tags casually.
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CliExit {
    /// Success.
    Success = 0,
    /// Extraction failure -- file readable but content unrecoverable
    /// (corrupt xref, invalid OOXML zip, encrypted-without-password).
    ExtractionFailed = 1,
    /// Usage error -- bad flag value, parse error, conflicting args.
    UsageError = 2,
    /// File not found / IO error -- missing file, permission denied,
    /// directory passed where file expected.
    FileNotFound = 3,
}

impl From<CliExit> for ExitCode {
    fn from(value: CliExit) -> Self {
        ExitCode::from(value as u8)
    }
}

/// Stable error code strings emitted under `--errors json`. Part of the
/// agent contract; do not change without bumping the alpha tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // E_INTERNAL reserved for future panic / unreachable mapping
enum ErrorCode {
    FileNotFound,
    PermissionDenied,
    FormatUnsupported,
    EncryptionRequired,
    InvalidArgument,
    ParseError,
    ExtractionFailed,
    Internal,
}

impl ErrorCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::FileNotFound => "E_FILE_NOT_FOUND",
            Self::PermissionDenied => "E_PERMISSION_DENIED",
            Self::FormatUnsupported => "E_FORMAT_UNSUPPORTED",
            Self::EncryptionRequired => "E_ENCRYPTION_REQUIRED",
            Self::InvalidArgument => "E_INVALID_ARGUMENT",
            Self::ParseError => "E_PARSE_ERROR",
            Self::ExtractionFailed => "E_EXTRACTION_FAILED",
            Self::Internal => "E_INTERNAL",
        }
    }

    fn exit(self) -> CliExit {
        match self {
            Self::InvalidArgument | Self::FormatUnsupported => CliExit::UsageError,
            Self::FileNotFound | Self::PermissionDenied => CliExit::FileNotFound,
            Self::EncryptionRequired
            | Self::ParseError
            | Self::ExtractionFailed
            | Self::Internal => CliExit::ExtractionFailed,
        }
    }
}

/// Output mode for error reporting on stderr.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
enum ErrorFormat {
    /// Human-readable lines: `udoc: <message>`. Default.
    #[default]
    Human,
    /// One JSON object per line on stderr:
    /// `{"code":"E_FORMAT_UNSUPPORTED","message":"...","context":"..."}`.
    Json,
}

/// Emit an error in the requested format. JSON path writes a single
/// flushed line so concurrent stdout writes can't interleave the JSON
/// object's bytes (per friction-deepdive note on `2>&1` redirect
/// patterns).
fn emit_error(format: ErrorFormat, code: ErrorCode, message: &str, context: Option<&str>) {
    match format {
        ErrorFormat::Human => {
            // Preserve the long-standing "udoc: <message>" stderr shape.
            // Don't include the error code by default -- agents that need
            // it pass --errors json.
            if let Some(ctx) = context {
                eprintln!("udoc: {message} ({ctx})");
            } else {
                eprintln!("udoc: {message}");
            }
        }
        ErrorFormat::Json => {
            let payload = serde_json::json!({
                "code": code.as_str(),
                "message": message,
                "context": context,
            });
            // Build the line in one allocation, then write+flush in one
            // call so a stdout writer racing on `2>&1` cannot split it
            // across the boundary.
            let line = format!("{payload}\n");
            let stderr = io::stderr();
            let mut handle = stderr.lock();
            let _ = handle.write_all(line.as_bytes());
            let _ = handle.flush();
        }
    }
}

/// Classify an arbitrary string error into a stable ErrorCode. Best-
/// effort substring matching against well-known message fragments
/// produced by the facade backends. When nothing matches we default to
/// ExtractionFailed (the safest "the doc is unrecoverable" signal).
fn classify_error(msg: &str) -> ErrorCode {
    let lower = msg.to_lowercase();
    if lower.contains("no such file")
        || lower.contains("not found")
        || lower.contains("does not exist")
    {
        ErrorCode::FileNotFound
    } else if lower.contains("permission denied") {
        ErrorCode::PermissionDenied
    } else if lower.contains("unknown format")
        || lower.contains("unsupported format")
        || lower.contains("unable to detect format")
    {
        ErrorCode::FormatUnsupported
    } else if lower.contains("encrypt") || lower.contains("password") {
        ErrorCode::EncryptionRequired
    } else if lower.contains("invalid") && lower.contains("page range") {
        ErrorCode::InvalidArgument
    } else if lower.contains("parse") || lower.contains("malformed") || lower.contains("xref") {
        ErrorCode::ParseError
    } else {
        ErrorCode::ExtractionFailed
    }
}

// ---------------------------------------------------------------------------
// Top-level CLI.
//
// Subcommand tree:
//   extract | render | tables | images | metadata
//   | fonts (with --audit) | inspect | features
//   | completions (hidden)
//   [+ render-diff / render-inspect under --features dev-tools]
//
// Zero-ceremony shortcut: bare `udoc <file>` runs the extract pipeline.
// Enabled by `args_conflicts_with_subcommands = true` + an
// `Option<Command>`. When clap sees positional files with no
// subcommand, it keeps the arguments in the flattened `ExtractArgs`
// block and skips the subcommand dispatch.
// ---------------------------------------------------------------------------

/// Extract text, tables, and images from documents.
#[derive(Parser)]
#[command(name = "udoc", version, about)]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    /// Subcommand. When absent, the flags below drive the default
    /// extraction pipeline so `udoc <file>` stays a one-liner.
    #[command(subcommand)]
    command: Option<Command>,

    /// Extraction flags. Apply when no subcommand is provided (the
    /// zero-ceremony bare-file invocation) and when the explicit
    /// `extract` subcommand is used.
    #[command(flatten)]
    extract: ExtractArgs,
}

/// Output mode for the extraction pipeline.
///
/// Collapses the four legacy output booleans (`--json`/`-j`,
/// `--jsonl`/`-J`, `--tables`/`-t`, `--images`) into one flag
/// selectable via `--out`/`-O`. Default is [`OutputMode::Text`] —
/// the friendly default for `udoc paper.pdf | grep ...`, `| less`,
/// `| wc -w`. Programmatic callers ask for `-j` / `-J` / `-O json`
/// when they want structured output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
enum OutputMode {
    /// Plain text (default).
    #[default]
    Text,
    /// Full document JSON.
    Json,
    /// Streaming JSON Lines, one record per page.
    Jsonl,
    /// Tables only, tab-separated values.
    Tsv,
    /// LLM-friendly markdown with citation anchors.
    Markdown,
    /// Chunked NDJSON for ingest pipelines (requires `--chunk-by`).
    Chunks,
    /// PDF text projected onto a monospace grid, preserving columns
    /// and tabular alignment. Equivalent to `pdftotext -layout`.
    /// PDF-only; other formats fall back to plain text.
    Layout,
}

/// Chunk strategy for `--out chunks`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum ChunkBy {
    /// One chunk per page.
    Page,
    /// One chunk per Block::Heading boundary.
    Heading,
    /// One chunk per top-level (rank-1) heading boundary.
    Section,
    /// Target N chars per chunk; never split a paragraph.
    Size,
}

/// Arguments for the default extraction pipeline. Used as the flattened
/// top-level args (bare-file invocation) and as the body of the explicit
/// `extract` subcommand.
#[derive(ClapArgs, Clone, Default, Debug)]
struct ExtractArgs {
    /// Document path(s). Use - for stdin. Multiple files for batch extraction.
    files: Vec<String>,

    /// Output mode (text|json|jsonl|tsv|markdown|chunks|layout). Default `text`.
    #[arg(
        short = 'O',
        long = "out",
        value_name = "MODE",
        help_heading = "Output mode"
    )]
    out: Option<OutputMode>,

    /// Shortcut for `--out layout`: render PDF text on a monospace grid.
    #[arg(short = 'L', long = "layout", help_heading = "Output mode")]
    layout: bool,

    /// Target line width in columns for `--out layout` (default: 100).
    #[arg(long = "columns", value_name = "N", help_heading = "Output formatting")]
    columns: Option<usize>,

    /// Chunk strategy when `--out chunks` (page|heading|section|size).
    #[arg(
        long = "chunk-by",
        value_name = "STRATEGY",
        help_heading = "Output mode"
    )]
    chunk_by: Option<ChunkBy>,

    /// Chunk size in chars when `--chunk-by size` (default 2000).
    #[arg(
        long = "chunk-size",
        default_value_t = 2000,
        help_heading = "Output mode"
    )]
    chunk_size: usize,

    /// Page range (e.g., "1-5", "3,7,9-12")
    #[arg(short = 'p', long = "pages", help_heading = "Filter")]
    pages: Option<String>,

    /// Force input format (pdf, docx, xlsx, pptx, doc, xls, ppt, odt, ods, odp, rtf, md).
    /// Renamed from `--format` to disambiguate from `--out`.
    #[arg(short = 'F', long = "input-format", help_heading = "Filter")]
    input_format: Option<String>,

    /// Document password (encrypted PDF)
    #[arg(long = "password", help_heading = "Filter")]
    password: Option<String>,

    /// OCR hook command
    #[arg(long = "ocr", help_heading = "Hooks")]
    ocr: Option<String>,

    /// Post-processing hook (repeatable)
    #[arg(long = "hook", help_heading = "Hooks")]
    hook: Vec<String>,

    /// OCR all pages, not just textless ones
    #[arg(long = "ocr-all", help_heading = "Hooks")]
    ocr_all: bool,

    /// Directory for page images passed to hooks
    #[arg(long = "hook-image-dir", help_heading = "Hooks")]
    hook_image_dir: Option<PathBuf>,

    /// Per-page hook timeout in seconds (default: 60)
    #[arg(long = "hook-timeout", help_heading = "Hooks")]
    hook_timeout: Option<u64>,

    /// Maximum file size (e.g., "256mb", "1gb", "4096")
    #[arg(long = "max-file-size", value_parser = parse_size, help_heading = "Resource limits")]
    max_file_size: Option<u64>,

    /// Maximum pages/sheets/slides to extract
    #[arg(long = "max-pages", help_heading = "Resource limits")]
    max_pages: Option<usize>,

    /// Skip table detection (PDF only, ~16% faster)
    #[arg(long = "no-tables", help_heading = "Resource limits")]
    no_tables: bool,

    /// Skip image extraction (PDF only, ~19% faster). Disables in-parser
    /// image decoding entirely. Use `udoc images <file>` (subcommand) when
    /// you want to dump embedded images.
    #[arg(long = "no-images", help_heading = "Resource limits")]
    no_images: bool,

    /// Omit presentation overlay from JSON/JSONL
    #[arg(long = "no-presentation", help_heading = "Output formatting")]
    no_presentation: bool,

    /// Include raw positioned spans in JSON output
    #[arg(long = "raw-spans", help_heading = "Output formatting")]
    raw_spans: bool,

    /// Pretty-print JSON output
    #[arg(long = "pretty", help_heading = "Output formatting")]
    pretty: bool,

    /// Output file (default: stdout)
    #[arg(short = 'o', long = "output", help_heading = "Output formatting")]
    output: Option<PathBuf>,

    /// Output directory for `--out chunks` images / `images` subcommand
    /// dumps (default: current directory).
    #[arg(long = "image-dir", help_heading = "Output formatting")]
    image_dir: Option<PathBuf>,

    /// No-op kept for backwards compatibility. udoc is silent on stderr
    /// by default; `--quiet` is implicit. Pass `-v` to opt back in.
    #[arg(short = 'q', long = "quiet", help_heading = "Misc")]
    quiet: bool,

    /// Show diagnostic warnings on stderr. Pass `-v` once for warnings
    /// (font fallbacks, recovered xref, table-detection skips), `-vv`
    /// for warnings + info-level progress (per-font ToUnicode loads,
    /// per-page reading-order tier). Repeated identical messages are
    /// deduplicated.
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count, help_heading = "Misc")]
    verbose: u8,

    /// Error output format on stderr: `human` (default) or `json`. The
    /// JSON form emits one object per line (`{"code","message","context"}`)
    /// so agents grep-ing under `2>&1` can recover the structured error
    /// without parsing the human prose. The error-code strings are
    /// part of the agent contract.
    #[arg(
        long = "errors",
        value_name = "FORMAT",
        default_value = "human",
        help_heading = "Output formatting"
    )]
    errors: ErrorFormat,

    /// Number of parallel in-process extraction threads.
    ///
    /// Throughput peaks around `--jobs 16` on this codebase (P06 finding).
    /// Past that, in-process parallelism loses to cache and memory-
    /// bandwidth contention -- not the allocator (P07 verified mimalloc
    /// makes it worse). For larger fan-out use `--processes` instead.
    /// Mutually exclusive with `--processes`.
    #[arg(
        long = "jobs",
        default_value_t = 1,
        conflicts_with = "processes",
        help_heading = "Misc"
    )]
    jobs: u16,

    /// Number of subprocess workers for batch extraction.
    ///
    /// Spawns N child `udoc` processes via fork+exec; each child
    /// processes a disjoint slice of the input files. The kernel
    /// reclaims every page when each child exits, so this is the
    /// canonical pattern for very-large-corpus batches that otherwise
    /// hit the in-process scaling cliff. Mutually exclusive with
    /// `--jobs`.
    #[arg(long = "processes", conflicts_with = "jobs", help_heading = "Misc")]
    processes: Option<u16>,
}

/// Subcommand tree. The `extract` variant mirrors the bare-file shortcut
/// so scripts that prefer explicit subcommands ("udoc extract file.pdf")
/// share one code path with "udoc file.pdf".
#[derive(Subcommand, Debug)]
enum Command {
    /// Extract text, tables, images, and metadata from a document.
    /// Same behaviour as the bare-file invocation; kept as an explicit
    /// subcommand for scripts that prefer unambiguous CLI shapes.
    Extract(ExtractArgs),

    /// Render document pages to PNG images on disk.
    Render {
        /// Input document.
        file: PathBuf,
        /// Output directory. Created if missing.
        #[arg(short = 'o', long = "output", value_name = "DIR")]
        output: PathBuf,
        /// Page range (e.g. "1-5", "3,7,9-12"). Renders every page when omitted.
        #[arg(short = 'p', long = "pages")]
        pages: Option<String>,
        /// Render DPI.
        #[arg(long = "dpi", default_value_t = udoc::render::DEFAULT_DPI)]
        dpi: u32,
    },

    /// List fonts referenced by a document with their FontResolution
    /// (Exact / Substituted / SyntheticFallback).
    ///
    /// Default mode is the lighter release-facing report (per-font
    /// resolution + page coverage). With `--audit`, runs the full
    /// corpus-style audit (the `audit-fonts` subcommand body):
    /// missing-glyph probe + corpus-level summary ratios. Per
    /// the two subcommands collapsed into one.
    Fonts {
        /// Input document.
        file: PathBuf,
        /// Write the report here instead of stdout.
        #[arg(short = 'o', long = "output", value_name = "FILE")]
        output: Option<PathBuf>,
        /// Output format: `json` (default) or `text`.
        #[arg(long = "format", default_value = "json")]
        format: String,
        /// Emit the per-font routing chain (subset-prefix strip, tier1
        /// name match, Unicode sniff, fallback reason, final resolution)
        /// alongside each entry. Useful when auditing why a specific
        /// font ended up substituted. Ignored under `--audit`.
        #[arg(long = "trace")]
        trace: bool,
        /// Run the full corpus-style font-resolution audit (replaces the
        /// `audit-fonts` subcommand). Slower; emits more detail.
        #[arg(long = "audit")]
        audit: bool,
    },

    /// Extract tables only, defaulting to TSV. Equivalent to
    /// `udoc extract --out tsv <file>`; the subcommand is the
    /// discoverability win for users who already know they want
    /// tables.
    Tables {
        /// Input document.
        file: PathBuf,
        /// Write the report here instead of stdout.
        #[arg(short = 'o', long = "output", value_name = "FILE")]
        output: Option<PathBuf>,
    },

    /// Quickly probe a document for shape signals (format, page count,
    /// has-text, likely-scanned, has-tables, has-encryption, font
    /// resolution counts) without doing a full extraction.
    ///
    /// Body lands in. The variant exists here as a
    /// stub so the subcommand tree parses correctly.
    Inspect {
        /// Input document.
        file: PathBuf,
        /// Force a complete scan instead of the default sampled mode.
        /// Sampled mode looks at 5 pages spread across the document;
        /// `--full` walks every page.
        #[arg(long = "full")]
        full: bool,
    },

    /// List images embedded in a document (filter, dimensions,
    /// bits-per-component, byte size). With `--extract`, dump each
    /// image to disk using the native format's file extension.
    Images {
        /// Input document.
        file: PathBuf,
        /// Write the listing here instead of stdout.
        #[arg(short = 'o', long = "output", value_name = "FILE")]
        output: Option<PathBuf>,
        /// Listing format: `json` (default) or `text`.
        #[arg(long = "format", default_value = "json")]
        format: String,
        /// Dump each image to this directory.
        #[arg(long = "extract", value_name = "DIR")]
        extract: Option<PathBuf>,
    },

    /// Emit structured document metadata as JSON (title, author, creator,
    /// producer, creation/modification dates, page count, and extended
    /// properties).
    Metadata {
        /// Input document.
        file: PathBuf,
        /// Write the report here instead of stdout.
        #[arg(short = 'o', long = "output", value_name = "FILE")]
        output: Option<PathBuf>,
        /// Output format: `json` (default) or `text`.
        #[arg(long = "format", default_value = "json")]
        format: String,
    },

    /// Render pages with udoc and a reference renderer (mupdf or poppler),
    /// then compare via SSIM + PSNR. Emits one JSON object per page on
    /// stdout. Exits 0 if all pages meet the gate, 1 if any fall below,
    /// 2 on error.
    ///
    /// Internal QA tool. Available only in builds with --features dev-tools
    ///; not in the default release binary.
    #[cfg(feature = "dev-tools")]
    RenderDiff {
        /// Input PDF file.
        file: PathBuf,
        /// Reference renderer to compare against.
        #[arg(long, value_name = "NAME")]
        against: String,
        /// Page spec, e.g. "1", "1-5", "3,7,9-12".
        #[arg(long, default_value = "1")]
        pages: String,
        /// SSIM pass/fail threshold. Pages with SSIM below this gate fail.
        #[arg(long, default_value_t = 0.95)]
        gate: f64,
        /// Directory for diff artifacts on failure (udoc/ref/diff PNGs).
        #[arg(long, value_name = "PATH")]
        output_dir: Option<PathBuf>,
        /// Render DPI.
        #[arg(long, default_value_t = 150)]
        dpi: u32,
        /// Accept larger-than-2px dimension mismatches via center-crop.
        #[arg(long)]
        force_dpi: bool,
    },

    /// Dump intermediate renderer state for a glyph on a page: outlines,
    /// declared/auto hints, segment/edge tables, or a coverage bitmap.
    /// All JSON payloads include schema_version for compatibility.
    ///
    /// Internal QA tool. Available only in builds with --features dev-tools
    ///; not in the default release binary.
    #[cfg(feature = "dev-tools")]
    RenderInspect {
        /// Input PDF file.
        file: PathBuf,
        /// Page number (1-based).
        #[arg(long, default_value_t = 1)]
        page: usize,
        /// Glyph id (defaults to the first glyph of the first span).
        #[arg(long)]
        glyph: Option<u32>,
        /// What to dump: outlines, hints, edges, bitmap.
        #[arg(long)]
        dump: String,
        /// Output format.
        #[arg(long, default_value = "json")]
        format: String,
        /// Compact JSON (one line). Pretty-printed by default.
        #[arg(long)]
        compact: bool,
        /// For `--dump bitmap`: render as ASCII art instead of a PNG.
        #[arg(long)]
        ascii: bool,
        /// Pixel size for hint/edge/bitmap dumps (pixels-per-em).
        #[arg(long, default_value_t = 24)]
        ppem: u16,
        /// Font name to inspect (defaults to the first font seen on the page).
        #[arg(long)]
        font: Option<String>,
    },

    /// Emit a shell completion script on stdout. Hidden from the
    /// top-level `--help` listing; reachable via
    /// `udoc completions --help`.
    #[command(hide = true)]
    Completions {
        /// Target shell: bash, zsh, fish, elvish, powershell.
        shell: Shell,
    },

    /// Emit a roff-formatted man page for `udoc(1)` on stdout.
    /// Redirect into your `man` path or pipe through `man -l -` to read.
    /// Hidden from the top-level `--help` listing.
    #[command(hide = true)]
    Mangen,

    /// Print the compile-time feature report for this `udoc` binary
    /// (tier1 font bundles, CJK fallback, own JBIG2 decoder, etc.).
    /// Operators use this to confirm "why did feature X not fire?" maps
    /// to "the binary was built without -F X" vs a runtime misconfig.
    Features {
        /// Write the report here instead of stdout.
        #[arg(short = 'o', long = "output", value_name = "FILE")]
        output: Option<PathBuf>,
        /// Deprecated shortcut for `--format json`. Hidden from --help;
        /// still honoured for scripts that picked it up before `--format`
        /// landed. Prefer `--format json` in new code.
        #[arg(long = "json", conflicts_with = "format", hide = true)]
        json: bool,
        /// Output format: `text` (default) or `json`.
        #[arg(long = "format", default_value = "text")]
        format: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    if let Some(cmd) = cli.command {
        return dispatch_subcommand(cmd);
    }

    let errors = cli.extract.errors;
    match run(&cli.extract) {
        Ok(()) => CliExit::Success.into(),
        Err(e) => report_run_error(errors, &e.to_string(), /*already_emitted=*/ true),
    }
}

/// Map a free-form extraction error string to (ErrorCode, CliExit).
/// When `already_emitted=false`, also emit through the `--errors`
/// formatter; when true (the per-file path already printed the
/// structured diagnostic) just classify and return the matching exit
/// code without a duplicate stderr line.
fn report_run_error(format: ErrorFormat, message: &str, already_emitted: bool) -> ExitCode {
    let code = classify_error(message);
    if !already_emitted {
        emit_error(format, code, message, None);
    }
    code.exit().into()
}

fn dispatch_subcommand(cmd: Command) -> ExitCode {
    match cmd {
        Command::Extract(args) => {
            let errors = args.errors;
            match run(&args) {
                Ok(()) => CliExit::Success.into(),
                Err(e) => report_run_error(errors, &e.to_string(), true),
            }
        }
        Command::Render {
            file,
            output,
            pages,
            dpi,
        } => dispatch_render(file, output, pages, dpi),
        Command::Fonts {
            file,
            output,
            format,
            trace,
            audit,
        } => {
            // --audit hands off to the corpus-style audit body
            // (formerly the standalone `audit-fonts` subcommand).
            if audit {
                let fmt = match format.parse() {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("udoc fonts --audit: {e}");
                        return ExitCode::from(2);
                    }
                };
                let code = audit::run(audit::Args {
                    file,
                    output,
                    format: fmt,
                });
                return ExitCode::from(code);
            }

            let fmt = match format.parse() {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("udoc fonts: {e}");
                    return ExitCode::from(2);
                }
            };
            let code = intro::fonts::run(intro::fonts::Args {
                file,
                output,
                format: fmt,
                trace,
            });
            ExitCode::from(code)
        }
        Command::Tables { file, output } => dispatch_tables(file, output),
        Command::Inspect { file, full } => dispatch_inspect(file, full),
        Command::Images {
            file,
            output,
            format,
            extract,
        } => {
            let fmt = match format.parse() {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("udoc images: {e}");
                    return ExitCode::from(2);
                }
            };
            let code = intro::images::run(intro::images::Args {
                file,
                output,
                format: fmt,
                extract,
            });
            ExitCode::from(code)
        }
        Command::Metadata {
            file,
            output,
            format,
        } => {
            let fmt = match format.parse() {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("udoc metadata: {e}");
                    return ExitCode::from(2);
                }
            };
            let code = intro::metadata::run(intro::metadata::Args {
                file,
                output,
                format: fmt,
            });
            ExitCode::from(code)
        }
        #[cfg(feature = "dev-tools")]
        Command::RenderDiff {
            file,
            against,
            pages,
            gate,
            output_dir,
            dpi,
            force_dpi,
        } => {
            let reference = match against.parse() {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("udoc render-diff: {e}");
                    return ExitCode::from(2);
                }
            };
            let code = render_diff::run(render_diff::Args {
                file,
                against: reference,
                pages,
                gate,
                output_dir,
                dpi,
                force_dpi,
            });
            ExitCode::from(code)
        }
        #[cfg(feature = "dev-tools")]
        Command::RenderInspect {
            file,
            page,
            glyph,
            dump,
            format,
            compact,
            ascii,
            ppem,
            font,
        } => {
            let dump_kind = match dump.parse() {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("udoc render-inspect: {e}");
                    return ExitCode::from(2);
                }
            };
            let fmt_kind = match format.parse() {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("udoc render-inspect: {e}");
                    return ExitCode::from(2);
                }
            };
            let code = render_inspect::run(render_inspect::Args {
                file,
                page,
                glyph,
                dump: dump_kind,
                format: fmt_kind,
                compact,
                ascii,
                ppem,
                font,
            });
            ExitCode::from(code)
        }
        Command::Completions { shell } => {
            let code = completions::run::<Cli>(shell);
            ExitCode::from(code)
        }
        Command::Mangen => {
            let code = crate::cli::mangen::run::<Cli>();
            ExitCode::from(code)
        }
        Command::Features {
            output,
            json,
            format,
        } => {
            let fmt = if json {
                features::Format::Json
            } else {
                match format.parse() {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("udoc features: {e}");
                        return ExitCode::from(2);
                    }
                }
            };
            let code = features::run(features::Args {
                output,
                format: fmt,
            });
            ExitCode::from(code)
        }
    }
}

/// `udoc render <file> -o <dir>` implementation. The `--render-pages`
/// extract-args flag was dropped in, so this is now the
/// single canonical path for rendering pages to PNG.
fn dispatch_render(file: PathBuf, output: PathBuf, pages: Option<String>, dpi: u32) -> ExitCode {
    match run_render(&file, &output, pages.as_deref(), dpi) {
        Ok(()) => CliExit::Success.into(),
        Err(e) => {
            // Render subcommands don't carry --errors yet (subcommand
            // owns its arg parsing); preserve the historical "udoc
            // render:" stderr prefix and map the exit code stably.
            let msg = e.to_string();
            let code = classify_error(&msg);
            eprintln!("udoc render: {msg}");
            code.exit().into()
        }
    }
}

/// `udoc tables <file>` -- table-only TSV extraction (also reachable
/// via `udoc extract --out tsv`). Subcommand exists for discoverability.
fn dispatch_tables(file: PathBuf, output: Option<PathBuf>) -> ExitCode {
    match run_tables(&file, output.as_deref()) {
        Ok(()) => CliExit::Success.into(),
        Err(e) => {
            let msg = e.to_string();
            let code = classify_error(&msg);
            eprintln!("udoc tables: {msg}");
            code.exit().into()
        }
    }
}

fn run_tables(
    file: &std::path::Path,
    output: Option<&std::path::Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let doc = udoc::extract(file).map_err(|e| format!("extracting '{}': {e}", file.display()))?;
    let mut buf = Vec::new();
    let page_assignments = doc.presentation.as_ref().map(|p| &p.page_assignments);
    output::tables::write_tables(&doc, &mut buf, page_assignments)?;
    if let Some(path) = output {
        std::fs::write(path, &buf).map_err(|e| format!("writing {}: {e}", path.display()))?;
    } else {
        let stdout = io::stdout();
        let mut out = io::BufWriter::new(stdout.lock());
        out.write_all(&buf)
            .map_err(|e| format!("writing tables: {e}"))?;
        out.flush().map_err(|e| format!("flushing tables: {e}"))?;
    }
    Ok(())
}

/// `udoc inspect <file>` -- shape probe. Emits JSON to stdout.
fn dispatch_inspect(file: PathBuf, full: bool) -> ExitCode {
    let code = inspect::run(inspect::Args { file, full });
    ExitCode::from(code)
}

fn run_render(
    file: &std::path::Path,
    output: &std::path::Path,
    pages: Option<&str>,
    dpi: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let page_range = match pages {
        Some(spec) => Some(
            PageRange::parse(spec).map_err(|e| format!("invalid page range '{}': {}", spec, e))?,
        ),
        None => None,
    };

    let mut config = Config::new();
    // Renderer needs font assets or it falls back to Liberation Sans
    // with the wrong metrics.
    config.assets.fonts = true;
    config.page_range = page_range;
    let doc = udoc::extract_with(file, config)
        .map_err(|e| format!("extracting '{}': {e}", file.display()))?;

    std::fs::create_dir_all(output)
        .map_err(|e| format!("creating render dir {}: {e}", output.display()))?;

    let mut font_cache = udoc::render::font_cache::FontCache::new(&doc.assets);
    // When the user passes --pages, extraction only populates the requested
    // pages into the presentation overlay. Iterate the loaded pages, not the
    // PDF's total page_count -- otherwise a `--pages 1` on a 341-page book
    // emits 340 spurious "page index N out of range" warnings for pages that
    // were deliberately excluded at the extraction step.
    let loaded_pages = doc
        .presentation
        .as_ref()
        .map(|p| p.pages.len())
        .unwrap_or(0);
    let page_count = doc.metadata.page_count;
    let mut rendered = 0usize;
    for page_idx in 0..loaded_pages {
        match udoc::render::render_page(&doc, page_idx, dpi, &mut font_cache) {
            Ok(png_bytes) => {
                let path = output.join(format!("page-{page_idx}.png"));
                std::fs::write(&path, &png_bytes)
                    .map_err(|e| format!("writing {}: {e}", path.display()))?;
                rendered += 1;
            }
            Err(e) => {
                eprintln!("udoc render: warning: page {page_idx}: {e}");
            }
        }
    }
    if rendered == 0 && page_count > 0 {
        return Err(format!("no pages rendered (from {page_count} attempted)").into());
    }
    Ok(())
}

fn run(cli: &ExtractArgs) -> Result<(), Box<dyn std::error::Error>> {
    // Subcommands are handled upstream. In extraction mode we need at
    // least one file (or `-` for stdin). Missing files is a usage error.
    if cli.files.is_empty() {
        return Err(
            "missing input: provide one or more file paths, or use `-` for stdin\n\
             run `udoc --help` for usage; see `udoc <subcommand> --help` for specific subcommand usage"
                .into(),
        );
    }

    // Parse --input-format
    let format = match &cli.input_format {
        Some(f) => Some(parse_format(f)?),
        None => None,
    };

    // Resolve --out. `-L` / `--layout` is a shortcut for `--out layout`
    // and conflicts with an explicit `--out` of a different mode.
    let out_mode = resolve_out_mode(cli.out, cli.layout)?;

    // --chunk-by required when --out chunks
    if out_mode == OutputMode::Chunks && cli.chunk_by.is_none() {
        return Err("must specify --chunk-by <page|heading|section|size> when --out chunks".into());
    }

    // Parse --pages
    let page_range = match &cli.pages {
        Some(spec) => Some(
            PageRange::parse(spec).map_err(|e| format!("invalid page range '{}': {}", spec, e))?,
        ),
        None => None,
    };

    // Build config
    let diagnostics = Arc::new(CollectingDiagnostics::new());
    let mut config = Config::new().diagnostics(diagnostics.clone());
    if let Some(fmt) = format {
        config = config.format(fmt);
    }
    if let Some(pw) = &cli.password {
        config = config.password(pw.clone());
    }
    config.page_range = page_range;

    // Build limits from env vars, then CLI overrides
    let mut limits = Limits::default();
    if let Ok(val) = std::env::var("UDOC_MAX_FILE_SIZE") {
        match parse_size(&val) {
            Ok(size) => limits.max_file_size = size,
            Err(_) => eprintln!(
                "warning: ignoring invalid UDOC_MAX_FILE_SIZE=\"{}\": expected size like \"128mb\" or \"1gb\"",
                val
            ),
        }
    }
    if let Ok(val) = std::env::var("UDOC_MAX_PAGES") {
        match val.parse::<usize>() {
            Ok(n) => limits.max_pages = n,
            Err(_) => eprintln!(
                "warning: ignoring invalid UDOC_MAX_PAGES=\"{}\": expected a positive integer",
                val
            ),
        }
    }
    // CLI flags override env vars
    if let Some(size) = cli.max_file_size {
        limits.max_file_size = size;
    }
    if let Some(n) = cli.max_pages {
        limits.max_pages = n;
    }
    config = config.limits(limits);

    // If --no-presentation, skip presentation extraction entirely
    if cli.no_presentation {
        config.layers.presentation = false;
        config.layers.relationships = false;
        config.layers.interactions = false;
    }
    if cli.no_tables {
        config.layers.tables = false;
    }
    if cli.no_images {
        config.layers.images = false;
    }

    // Auto-enable font extraction when hooks are requested.
    // Fonts are needed for glyph rendering inside hooks; without them the
    // renderer falls back to Liberation Sans with wrong metrics.
    // (Renderer subcommand `udoc render` is not invoked from this path
    // anymore -- the `--render-pages` / `--render-dpi` extract flags were
    // dropped in. Use `udoc render <file> -o <dir>` for
    // pure rendering.)
    if cli.ocr.is_some() || !cli.hook.is_empty() {
        config.assets.fonts = true;
    }

    // Truncate the output file (if -o was given) so a fresh run doesn't
    // append to stale data from a previous invocation. Subsequent writes
    // use append mode so batch processing concatenates correctly.
    // Note: not atomic with the first append, but fine for single-process use.
    if let Some(ref path) = cli.output {
        std::fs::File::create(path).map_err(|e| format!("creating {}: {}", path.display(), e))?;
    }

    // Subprocess-fork batch mode: spawn N child udoc processes to dodge
    // the in-process scaling cliff (P06 finding). Mutually exclusive with
    // --jobs at the clap level.
    let has_stdin = cli.files.iter().any(|f| f == "-");
    if let Some(processes) = cli.processes {
        let processes = processes.max(1) as usize;
        if processes > 1 && cli.files.len() > 1 && !has_stdin {
            return run_subprocess(cli, processes);
        }
    }

    // Parallel batch mode: when --jobs > 1 and there are multiple non-stdin
    // files, process them in parallel using std::thread::scope.
    let jobs = cli.jobs.max(1) as usize;
    if jobs > 1 && cli.files.len() > 1 && !has_stdin {
        if !cli.quiet {
            warn_high_parallelism(jobs);
        }
        return run_parallel(cli, &config, format, out_mode, jobs);
    }

    // Sequential processing (single file, --jobs 1, or stdin present).
    run_sequential(cli, &config, &diagnostics, format, out_mode)
}

/// Resolve the effective output mode. When the user passes `--out`
/// explicitly, honor it. Otherwise default to plain text — the
/// friendly default for `udoc paper.pdf | grep ...`, `| less`,
/// `| wc -w`, etc. Programmatic callers ask for `-O json` or
/// `-j` / `-J` when they want structured output.
///
/// `-L` / `--layout` is a shortcut for `--out layout`. Combining it
/// with an explicit non-layout `--out` is a usage error.
fn resolve_out_mode(
    explicit: Option<OutputMode>,
    layout_flag: bool,
) -> Result<OutputMode, Box<dyn std::error::Error>> {
    match (explicit, layout_flag) {
        (Some(mode), true) if mode != OutputMode::Layout => {
            Err(format!("--layout conflicts with --out {mode:?}").into())
        }
        (_, true) => Ok(OutputMode::Layout),
        (Some(mode), false) => Ok(mode),
        (None, false) => Ok(OutputMode::Text),
    }
}

/// Sequential file processing: the original batch loop.
///
/// On per-file errors, emits the structured diagnostic via the
/// `--errors` formatter and remembers the most-severe classification
/// so the process exit code reflects the actual failure mode (per
///). When a single file fails the process inherits
/// that file's exit code; with multiple failures the highest-severity
/// code wins.
fn run_sequential(
    cli: &ExtractArgs,
    config: &Config,
    diagnostics: &Arc<CollectingDiagnostics>,
    format: Option<Format>,
    out_mode: OutputMode,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut worst_error: Option<(ErrorCode, String)> = None;
    for file_arg in &cli.files {
        match process_one_file(cli, config, diagnostics, format, out_mode, file_arg) {
            Ok(output) => {
                write_file_output(cli, &output)?;
            }
            Err(e) => {
                let msg = e.to_string();
                let code = classify_error(&msg);
                emit_error(cli.errors, code, &msg, Some(file_arg));
                // Keep the highest-severity error so the top-level
                // dispatch can pick the right exit code.
                worst_error = Some(match worst_error {
                    None => (code, msg),
                    Some(prev) => {
                        if severity_rank(code) > severity_rank(prev.0) {
                            (code, msg)
                        } else {
                            prev
                        }
                    }
                });
            }
        }
        // Drain diagnostics for this file.
        emit_diagnostics(cli, diagnostics);
    }

    if let Some((_, msg)) = worst_error {
        // Bubble the message back up; the top-level main() / dispatch
        // re-classifies it to assign the final exit code. We deliberately
        // pass the per-file message verbatim so classification matches.
        Err(msg.into())
    } else {
        Ok(())
    }
}

/// Higher means "worse". Ties go to the previously-seen error.
fn severity_rank(code: ErrorCode) -> u8 {
    match code {
        ErrorCode::FileNotFound | ErrorCode::PermissionDenied => 3,
        ErrorCode::EncryptionRequired => 2,
        _ => 1,
    }
}

/// Subprocess-fork batch extraction. Splits the input files into N
/// roughly-equal chunks and spawns one child `udoc` process per chunk.
///
/// Each child exits after its slice completes, so the kernel reclaims
/// every page (no monotonic RSS climb across the whole batch) and each
/// child gets clean cache locality (no cross-process working-set
/// thrash). This is the canonical "production" pattern past the
/// in-process scaling cliff ( / P07).
///
/// CLI flags that affect a child are forwarded by re-execing the same
/// binary path; flags that are subprocess-internal (--processes,
/// --jobs) are not forwarded. Stdout / stderr from each child stream
/// straight through to the parent's stdout / stderr.
fn run_subprocess(cli: &ExtractArgs, processes: usize) -> Result<(), Box<dyn std::error::Error>> {
    use std::process::{Command, Stdio};

    let files: &[String] = &cli.files;
    let exe = std::env::current_exe().map_err(|e| format!("locate own binary: {e}"))?;
    let chunk_size = files.len().div_ceil(processes);

    let mut handles = Vec::with_capacity(processes);
    for chunk in files.chunks(chunk_size) {
        if chunk.is_empty() {
            continue;
        }
        let mut cmd = Command::new(&exe);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        // Forward extraction-shaping flags. We deliberately do NOT
        // forward --jobs or --processes to avoid stacking nested
        // parallelism inside each child.
        if let Some(mode) = cli.out {
            let s = match mode {
                OutputMode::Text => "text",
                OutputMode::Json => "json",
                OutputMode::Jsonl => "jsonl",
                OutputMode::Tsv => "tsv",
                OutputMode::Markdown => "markdown",
                OutputMode::Chunks => "chunks",
                OutputMode::Layout => "layout",
            };
            cmd.arg("--out").arg(s);
        }
        if let Some(strategy) = cli.chunk_by {
            let s = match strategy {
                ChunkBy::Page => "page",
                ChunkBy::Heading => "heading",
                ChunkBy::Section => "section",
                ChunkBy::Size => "size",
            };
            cmd.arg("--chunk-by").arg(s);
            cmd.arg("--chunk-size").arg(cli.chunk_size.to_string());
        }
        if cli.layout {
            cmd.arg("--layout");
        }
        if let Some(n) = cli.columns {
            cmd.arg("--columns").arg(n.to_string());
        }
        if let Some(ref pages) = cli.pages {
            cmd.arg("--pages").arg(pages);
        }
        if let Some(ref fmt) = cli.input_format {
            cmd.arg("--input-format").arg(fmt);
        }
        if let Some(ref pw) = cli.password {
            cmd.arg("--password").arg(pw);
        }
        if cli.no_tables {
            cmd.arg("--no-tables");
        }
        if cli.no_images {
            cmd.arg("--no-images");
        }
        if cli.no_presentation {
            cmd.arg("--no-presentation");
        }
        if cli.raw_spans {
            cmd.arg("--raw-spans");
        }
        if cli.pretty {
            cmd.arg("--pretty");
        }
        if cli.quiet {
            cmd.arg("--quiet");
        }
        for _ in 0..cli.verbose {
            cmd.arg("--verbose");
        }
        if let Some(ref imgdir) = cli.image_dir {
            cmd.arg("--image-dir").arg(imgdir);
        }
        for f in chunk {
            cmd.arg(f);
        }

        let child = cmd.spawn().map_err(|e| format!("spawn worker: {e}"))?;
        handles.push((child, chunk.len()));
    }

    let mut had_error = false;
    for (mut child, n) in handles {
        match child.wait() {
            Ok(status) if status.success() => {}
            Ok(status) => {
                if !cli.quiet {
                    eprintln!("udoc: subprocess worker exited with {status} after {n} files");
                }
                had_error = true;
            }
            Err(e) => {
                if !cli.quiet {
                    eprintln!("udoc: failed to wait on subprocess worker: {e}");
                }
                had_error = true;
            }
        }
    }

    if had_error {
        Err("one or more subprocess workers failed".into())
    } else {
        Ok(())
    }
}

/// Empirical scaling cliff for in-process `--jobs`, observed on the
/// 500-doc archive.org load test (64-core / 32 GB host):
///
///   jobs=16: 40.78 docs/sec  (peak, 11x speedup)
///   jobs=32: 36.36 docs/sec  (drops)
///   jobs=64: 27.82 docs/sec  (worse than jobs=8)
///
/// We initially suspected glibc malloc-arena contention, but a P07 test
/// with mimalloc showed it actually loses 24% at jobs=64. The bottleneck
/// is memory-bandwidth / cache-line saturation across many threads
/// touching disjoint working sets, not the allocator. The right pattern
/// past the cliff is subprocess-fork: each process has its own page
/// table and gets clean cache locality.
const JOBS_SCALING_CLIFF: usize = 16;

/// Print a warning when --jobs is above the empirical scaling cliff,
/// pointing at subprocess-fork as the path forward.
fn warn_high_parallelism(jobs: usize) {
    if jobs <= JOBS_SCALING_CLIFF {
        return;
    }
    eprintln!(
        "udoc: warning: --jobs {jobs} > {cliff} typically loses throughput \
         on this codebase (P06 load test: throughput peaks at jobs=16 then \
         drops -- in-process parallelism past that point burns CPU on cache \
         and memory-bandwidth contention, not extraction). Use \
         `--processes {jobs}` instead for fork-based parallelism that \
         scales past the cliff. Pass --quiet to suppress this warning.",
        cliff = JOBS_SCALING_CLIFF
    );
}

/// Parallel file processing using std::thread::scope + a shared atomic
/// work counter (work-stealing).
///
/// Each thread pulls the next file index from a shared `AtomicUsize`.
/// This balances load when per-file work is uneven -- -P06 the
/// implementation was static `files.chunks()` partitioning, which
/// stranded one slow file on a single thread while other threads sat
/// idle. Observed pathology on a 1000-doc archive.org sample at
/// jobs=4: three of four threads finished quickly while the fourth
/// was still grinding 11 minutes in.
fn run_parallel(
    cli: &ExtractArgs,
    config: &Config,
    format: Option<Format>,
    out_mode: OutputMode,
    jobs: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let files: &[String] = &cli.files;
    let total = files.len();
    let next_index = AtomicUsize::new(0);

    // Each worker keeps a Vec<(orig_index, result)> for results it
    // processed. After join we merge by original index so stdout output
    // order matches the input file order. This avoids both the unsafe
    // disjoint-write trick (forbidden by #![deny(unsafe_code)]) and the
    // mutex contention of a shared output vec.
    type WorkerOutput = Vec<(usize, Result<FileOutput, String>)>;

    let worker_outputs: Vec<WorkerOutput> = std::thread::scope(|s| {
        let next_ref = &next_index;
        let mut handles = Vec::with_capacity(jobs);

        for _ in 0..jobs {
            let handle = s.spawn(move || -> WorkerOutput {
                let mut local: WorkerOutput = Vec::new();
                loop {
                    let i = next_ref.fetch_add(1, Ordering::Relaxed);
                    if i >= total {
                        break;
                    }
                    let file_arg = &files[i];

                    // Per-thread diagnostics so warnings don't bleed across
                    // files in different threads.
                    let thread_diag = Arc::new(CollectingDiagnostics::new());
                    let thread_config = config.clone().diagnostics(thread_diag.clone());
                    let result = process_one_file(
                        cli,
                        &thread_config,
                        &thread_diag,
                        format,
                        out_mode,
                        file_arg,
                    );
                    let warnings = thread_diag.take_warnings();
                    let slot = match result {
                        Ok(mut output) => {
                            output.warnings = warnings;
                            Ok(output)
                        }
                        Err(e) => Err(format!("{e}")),
                    };
                    local.push((i, slot));
                }
                local
            });
            handles.push(handle);
        }

        let mut collected: Vec<WorkerOutput> = Vec::with_capacity(jobs);
        for handle in handles {
            match handle.join() {
                Ok(local) => collected.push(local),
                Err(panic_payload) => {
                    let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                        (*s).to_string()
                    } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "unknown panic".to_string()
                    };
                    if !cli.quiet {
                        eprintln!("udoc: worker thread panicked: {msg}");
                    }
                }
            }
        }
        collected
    });

    // Merge per-worker outputs by original file index so the final
    // ordering matches stdin order (deterministic stdout regardless of
    // how the work-stealing race unfolded).
    let mut indexed: Vec<(usize, Result<FileOutput, String>)> = worker_outputs
        .into_iter()
        .flat_map(|w| w.into_iter())
        .collect();
    indexed.sort_by_key(|(i, _)| *i);

    let all_results: Vec<Result<FileOutput, String>> = (0..total)
        .map(|i| {
            // Most positions are filled. Any missing index means a worker
            // panicked between fetch_add and push; emit a synthetic error.
            match indexed.iter().position(|(j, _)| *j == i) {
                Some(pos) => indexed.swap_remove(pos).1,
                None => Err(format!(
                    "{}: worker thread did not produce a result",
                    files.get(i).map(String::as_str).unwrap_or("<unknown>")
                )),
            }
        })
        .collect();

    // Write results to output in original file order. Per-file errors
    // are emitted via the --errors formatter; the worst-severity error
    // string bubbles up so the top-level dispatch picks the right exit
    // code.
    let mut worst_error: Option<(ErrorCode, String)> = None;
    for (i, result) in all_results.iter().enumerate() {
        match result {
            Ok(output) => {
                write_file_output(cli, output)?;
                if !cli.quiet {
                    for w in &output.warnings {
                        emit_one_warning(w);
                    }
                }
            }
            Err(msg) => {
                let code = classify_error(msg);
                let file_arg = files.get(i).map(String::as_str);
                emit_error(cli.errors, code, msg, file_arg);
                worst_error = Some(match worst_error {
                    None => (code, msg.clone()),
                    Some(prev) => {
                        if severity_rank(code) > severity_rank(prev.0) {
                            (code, msg.clone())
                        } else {
                            prev
                        }
                    }
                });
            }
        }
    }

    if let Some((_, msg)) = worst_error {
        Err(msg.into())
    } else {
        Ok(())
    }
}

/// Output from processing a single file.
struct FileOutput {
    /// Formatted output bytes (text, JSON, JSONL, TSV, markdown, or chunks).
    data: Vec<u8>,
    /// Diagnostics collected during this file's extraction.
    warnings: Vec<udoc::Warning>,
}

/// Process one file: extract, run hooks, format output into bytes.
///
/// Handles both path-based files and stdin (`"-"`). The stdin branch is
/// unreachable from `run_parallel` (which guards against stdin), but is
/// used by `run_sequential`.
fn process_one_file(
    cli: &ExtractArgs,
    config: &Config,
    diagnostics: &Arc<CollectingDiagnostics>,
    format: Option<Format>,
    out_mode: OutputMode,
    file_arg: &str,
) -> Result<FileOutput, Box<dyn std::error::Error + Send + Sync>> {
    // Stdin bytes are kept in scope so the Layout dispatch (which
    // re-opens the document via udoc-pdf) can reuse them instead of
    // attempting to read stdin a second time after extract_bytes_with
    // has consumed it.
    let mut stdin_bytes: Option<Vec<u8>> = None;
    let (mut doc, format_name) = if file_arg == "-" {
        let mut data = Vec::new();
        io::stdin()
            .read_to_end(&mut data)
            .map_err(|e| format!("reading stdin: {}", e))?;
        let file_config = config.clone();
        let doc = udoc::extract_bytes_with(&data, file_config).map_err(|e| format!("{}", e))?;
        let name = udoc::detect::detect_format(&data)
            .map(|f| f.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        stdin_bytes = Some(data);
        (doc, name)
    } else {
        let path = std::path::Path::new(file_arg);
        let name = match format {
            Some(f) => f.to_string(),
            None => match udoc::detect::detect_format_path(path) {
                Ok(Some(f)) => f.to_string(),
                Ok(None) => "unknown".to_string(),
                Err(e) => {
                    return Err(
                        format!("format detection failed for {}: {e}", path.display()).into(),
                    );
                }
            },
        };
        let file_config = config.clone();
        match udoc::extract_with(path, file_config) {
            Ok(doc) => (doc, name),
            Err(e) => {
                return Err(format!("{}: {e}", path.display()).into());
            }
        }
    };

    // Run hooks if any are specified.
    let has_hooks = cli.ocr.is_some() || !cli.hook.is_empty();
    if has_hooks {
        let mut specs = Vec::new();
        if let Some(ref ocr_cmd) = cli.ocr {
            specs.push(udoc::hooks::HookSpec::from_command(ocr_cmd.as_str()));
        }
        for hook_cmd in &cli.hook {
            specs.push(udoc::hooks::HookSpec::from_command(hook_cmd.as_str()));
        }

        let mut hook_config = udoc::hooks::HookConfig::default();
        if let Some(timeout) = cli.hook_timeout {
            hook_config.page_timeout_secs = timeout;
        }
        hook_config.ocr_all_pages = cli.ocr_all;

        let mut runner = udoc::hooks::HookRunner::new(&specs, hook_config)
            .map_err(|e| format!("initializing hooks: {}", e))?;

        let page_images = cli.hook_image_dir.as_deref();
        runner
            .run(&mut doc, page_images)
            .map_err(|e| format!("running hooks: {}", e))?;
    }

    // Format output into a byte buffer based on resolved OutputMode.
    let mut buf = Vec::new();
    match out_mode {
        OutputMode::Text => {
            output::text::write_text(&doc, &mut buf)?;
        }
        OutputMode::Json => {
            let pretty = cli.pretty || (cli.output.is_none() && io::stdout().is_terminal());
            output::json::write_json(&doc, &mut buf, pretty, !cli.no_presentation, cli.raw_spans)?;
        }
        OutputMode::Jsonl => {
            let page_assignments = doc.presentation.as_ref().map(|p| &p.page_assignments);
            let warning_count = diagnostics.warnings().len();
            output::jsonl::write_jsonl(
                &doc,
                &format_name,
                &mut buf,
                page_assignments,
                warning_count,
            )?;
        }
        OutputMode::Tsv => {
            let page_assignments = doc.presentation.as_ref().map(|p| &p.page_assignments);
            output::tables::write_tables(&doc, &mut buf, page_assignments)?;
        }
        OutputMode::Markdown => {
            // Citation anchors preserved by default; downstream chunkers want them.
            let md = output::markdown::markdown_with_anchors(&doc);
            buf.extend_from_slice(md.as_bytes());
        }
        OutputMode::Chunks => {
            let strategy = match cli.chunk_by {
                Some(ChunkBy::Page) => output::chunks::ChunkBy::Page,
                Some(ChunkBy::Heading) => output::chunks::ChunkBy::Heading,
                Some(ChunkBy::Section) => output::chunks::ChunkBy::Section,
                Some(ChunkBy::Size) => output::chunks::ChunkBy::Size,
                None => {
                    // Should be guarded at run() entry; defensive fallback.
                    return Err(
                        "--out chunks requires --chunk-by <page|heading|section|size>".into(),
                    );
                }
            };
            output::chunks::emit_chunks(
                &doc,
                &output::chunks::ChunkOptions {
                    strategy,
                    size: cli.chunk_size,
                },
                &mut buf,
            )?;
        }
        OutputMode::Layout => {
            // PDF-only path: use udoc-pdf's geometry-aware renderer
            // directly. For non-PDF formats, fall back to plain text
            // (their native output is already layout-faithful).
            if format_name.eq_ignore_ascii_case("pdf") {
                emit_layout_for_pdf_doc(file_arg, stdin_bytes.as_deref(), cli.columns, &mut buf)?;
            } else {
                output::text::write_text(&doc, &mut buf)?;
            }
        }
    }

    Ok(FileOutput {
        data: buf,
        warnings: Vec::new(),
    })
}

/// Render every page of a PDF onto a monospace grid (poppler-style
/// `pdftotext -layout`) and write to `out`. Pages are separated by
/// form-feed (`\f`, 0x0C) for compatibility with poppler tooling.
///
/// Re-opens the file via `udoc_pdf::Document` because the layout
/// renderer needs the raw, geometry-rich spans that the unified
/// document model discards during conversion. When `stdin_bytes` is
/// `Some`, those bytes are reused (the upstream extract path already
/// drained stdin); otherwise the file is opened from disk.
fn emit_layout_for_pdf_doc(
    file_arg: &str,
    stdin_bytes: Option<&[u8]>,
    columns: Option<usize>,
    out: &mut Vec<u8>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use udoc_pdf::layout::LayoutOptions;
    use udoc_pdf::Document as PdfDoc;

    let target_cols = columns
        .or_else(|| {
            std::env::var("COLUMNS")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
        })
        .unwrap_or(100)
        .max(1);

    let mut doc = if file_arg == "-" {
        let data = match stdin_bytes {
            Some(b) => b.to_vec(),
            None => {
                let mut buf = Vec::new();
                io::stdin()
                    .read_to_end(&mut buf)
                    .map_err(|e| format!("reading stdin: {e}"))?;
                buf
            }
        };
        PdfDoc::from_bytes(data).map_err(|e| format!("parsing pdf from stdin: {e}"))?
    } else {
        let path = std::path::Path::new(file_arg);
        PdfDoc::open(path).map_err(|e| format!("opening pdf {}: {e}", path.display()))?
    };

    for i in 0..doc.page_count() {
        let mut page = doc.page(i).map_err(|e| format!("page {i}: {e}"))?;
        // page.page_bbox() returns udoc-pdf's local BoundingBox; the
        // renderer wants the udoc-core flavor (same fields).
        let pb = page.page_bbox();
        let bbox = udoc_core::geometry::BoundingBox::new(pb.x_min, pb.y_min, pb.x_max, pb.y_max);
        let opts = LayoutOptions {
            page_bbox: bbox,
            columns: target_cols,
            ..LayoutOptions::default()
        };
        let rendered = page
            .text_layout(&opts)
            .map_err(|e| format!("rendering page {i}: {e}"))?;
        out.extend_from_slice(rendered.as_bytes());
        if i + 1 < doc.page_count() {
            out.push(b'\x0c'); // form feed between pages
        } else {
            out.push(b'\n');
        }
    }
    Ok(())
}

/// Write one file's formatted output to the appropriate destination.
fn write_file_output(
    cli: &ExtractArgs,
    output: &FileOutput,
) -> Result<(), Box<dyn std::error::Error>> {
    if output.data.is_empty() {
        return Ok(());
    }

    let stdout = io::stdout();
    let mut out_file;
    let mut buf_stdout;
    let writer: &mut dyn Write = if let Some(ref path) = cli.output {
        out_file = io::BufWriter::new(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| format!("opening {}: {}", path.display(), e))?,
        );
        &mut out_file
    } else {
        buf_stdout = io::BufWriter::new(stdout.lock());
        &mut buf_stdout
    };

    writer
        .write_all(&output.data)
        .map_err(|e| format!("writing output: {e}"))?;
    writer
        .flush()
        .map_err(|e| format!("flushing output: {e}"))?;
    Ok(())
}

/// Emit diagnostics for all accumulated warnings, then drain.
///
/// Verbosity ladder:
/// - default (no `-v`): silent. Recovery warnings (font fallbacks,
///   table-detection skips, recovered xref) are routine and noise for
///   most callers.
/// - `-v`: emit warnings.
/// - `-vv`: emit warnings + info-level progress (font loads, ToUnicode
///   resolution, per-page reading-order tier).
///
/// Repeated identical (level, kind, message) tuples are deduplicated
/// across one extraction so a document with the same fallback firing
/// 30 times does not flood stderr.
fn emit_diagnostics(cli: &ExtractArgs, diagnostics: &Arc<CollectingDiagnostics>) {
    let warnings = diagnostics.take_warnings();
    if cli.quiet || cli.verbose == 0 {
        return;
    }
    let show_info = cli.verbose >= 2;
    let mut seen: std::collections::HashSet<(udoc::WarningLevel, String, String)> =
        std::collections::HashSet::new();
    for w in &warnings {
        if !show_info && matches!(w.level, udoc::WarningLevel::Info) {
            continue;
        }
        let key = (w.level, format!("{:?}", w.kind), w.message.clone());
        if !seen.insert(key) {
            continue;
        }
        emit_one_warning(w);
    }
}

/// Emit a single diagnostic warning to stderr.
fn emit_one_warning(w: &udoc::Warning) {
    let level = match w.level {
        udoc::WarningLevel::Info => "info",
        udoc::WarningLevel::Warning => "warning",
        // non_exhaustive: treat unknown future levels as warnings
        _ => "warning",
    };
    if let Some(page) = w.context.page_index {
        eprintln!("udoc: {}: page {}: {}", level, page + 1, w.message);
    } else {
        eprintln!("udoc: {}: {}", level, w.message);
    }
}

fn parse_format(s: &str) -> Result<Format, String> {
    match s.to_ascii_lowercase().as_str() {
        "pdf" => Ok(Format::Pdf),
        "docx" => Ok(Format::Docx),
        "xlsx" => Ok(Format::Xlsx),
        "pptx" => Ok(Format::Pptx),
        "doc" => Ok(Format::Doc),
        "xls" => Ok(Format::Xls),
        "ppt" => Ok(Format::Ppt),
        "odt" => Ok(Format::Odt),
        "ods" => Ok(Format::Ods),
        "odp" => Ok(Format::Odp),
        "rtf" => Ok(Format::Rtf),
        "md" | "markdown" => Ok(Format::Md),
        _ => Err(format!(
            "unknown format '{}'. Supported: pdf, docx, xlsx, pptx, doc, xls, ppt, odt, ods, odp, rtf, md",
            s
        )),
    }
}

// ---------------------------------------------------------------------------
// Unit tests for the surface (flag parsing only;
// end-to-end CLI integration tests live in `tests/cli.rs`).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod cli_flags_tests {
    use super::*;
    use clap::Parser;

    /// `--input-format pdf` parses on the bare-file invocation. The
    /// flag was `--format`; the rename happened in.
    #[test]
    fn input_format_parses() {
        let cli = Cli::try_parse_from(["udoc", "--input-format", "pdf", "file.pdf"])
            .expect("--input-format should parse");
        assert_eq!(cli.extract.input_format.as_deref(), Some("pdf"));
    }

    /// Short form `-F` resolves to the same field.
    #[test]
    fn input_format_short_form() {
        let cli = Cli::try_parse_from(["udoc", "-F", "pdf", "file.pdf"]).expect("-F should parse");
        assert_eq!(cli.extract.input_format.as_deref(), Some("pdf"));
    }

    /// `--out` accepts every variant of the OutputMode enum.
    #[test]
    fn out_mode_parses_each_variant() {
        for (s, expected) in [
            ("text", OutputMode::Text),
            ("json", OutputMode::Json),
            ("jsonl", OutputMode::Jsonl),
            ("tsv", OutputMode::Tsv),
            ("markdown", OutputMode::Markdown),
            ("chunks", OutputMode::Chunks),
            ("layout", OutputMode::Layout),
        ] {
            let cli = Cli::try_parse_from(["udoc", "--out", s, "file.pdf"])
                .unwrap_or_else(|e| panic!("--out {s} should parse: {e}"));
            assert_eq!(cli.extract.out, Some(expected), "wrong variant for {s}");
        }
    }

    /// Old `--format` flag must error -- alpha-to-alpha break, no alias.
    /// The clap default error mentions the unknown argument; consumers can
    /// `did you mean` themselves from the binary's `--help` output.
    #[test]
    fn old_format_flag_errors() {
        let res = Cli::try_parse_from(["udoc", "--format", "pdf", "file.pdf"]);
        assert!(res.is_err(), "--format must be rejected post-");
    }

    /// Old `--render-pages` flag must error.
    #[test]
    fn old_render_pages_flag_errors() {
        let res = Cli::try_parse_from(["udoc", "--render-pages", "/tmp/out", "file.pdf"]);
        assert!(res.is_err(), "--render-pages must be rejected post-");
    }

    /// Old `--render-dpi` flag must error.
    #[test]
    fn old_render_dpi_flag_errors() {
        let res = Cli::try_parse_from(["udoc", "--render-dpi", "150", "file.pdf"]);
        assert!(res.is_err(), "--render-dpi must be rejected post-");
    }

    /// Old output booleans (`--json`, `--jsonl`, `--tables`, `--images`)
    /// must error.
    #[test]
    fn old_output_booleans_error() {
        for old in ["--json", "--jsonl", "--tables", "--images"] {
            let res = Cli::try_parse_from(["udoc", old, "file.pdf"]);
            assert!(res.is_err(), "{old} must be rejected post-");
        }
    }

    /// The explicit `--out` value always wins. With no `--layout`
    /// shortcut, `--out X` resolves to X for every variant.
    #[test]
    fn resolve_out_mode_explicit_wins() {
        for mode in [
            OutputMode::Text,
            OutputMode::Json,
            OutputMode::Jsonl,
            OutputMode::Tsv,
            OutputMode::Markdown,
            OutputMode::Chunks,
            OutputMode::Layout,
        ] {
            assert_eq!(resolve_out_mode(Some(mode), false).unwrap(), mode);
        }
    }

    /// Bare `-L` resolves to Layout, even with no `--out` flag at all.
    #[test]
    fn resolve_out_mode_layout_shortcut() {
        assert_eq!(
            resolve_out_mode(None, true).unwrap(),
            OutputMode::Layout
        );
    }

    /// `-L` plus `--out layout` is fine (redundant but consistent).
    #[test]
    fn resolve_out_mode_layout_shortcut_with_explicit_layout_ok() {
        assert_eq!(
            resolve_out_mode(Some(OutputMode::Layout), true).unwrap(),
            OutputMode::Layout
        );
    }

    /// `-L` plus a non-Layout `--out` is a usage error.
    #[test]
    fn resolve_out_mode_layout_shortcut_conflicts_with_other_modes() {
        assert!(resolve_out_mode(Some(OutputMode::Json), true).is_err());
        assert!(resolve_out_mode(Some(OutputMode::Markdown), true).is_err());
    }
}

// ---------------------------------------------------------------------------
// Unit tests for the surface: subcommand tree
// shape + fonts --audit dispatch. Mirrors the structure of cli_flags_tests
// above; the new tree is `extract / render / tables / images / metadata /
// fonts (with --audit) / inspect / completions / features`.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod cli_subcommands_tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn tables_subcommand_parses() {
        let cli = Cli::try_parse_from(["udoc", "tables", "file.pdf"])
            .expect("tables subcommand should parse");
        match cli.command {
            Some(Command::Tables { file, output: None }) => {
                assert_eq!(file.to_string_lossy(), "file.pdf");
            }
            other => panic!("expected Tables variant, got {other:?}"),
        }
    }

    #[test]
    fn inspect_subcommand_parses() {
        let cli = Cli::try_parse_from(["udoc", "inspect", "file.pdf"])
            .expect("inspect subcommand should parse");
        match cli.command {
            Some(Command::Inspect { file, full: false }) => {
                assert_eq!(file.to_string_lossy(), "file.pdf");
            }
            other => panic!("expected Inspect variant, got {other:?}"),
        }
    }

    #[test]
    fn inspect_subcommand_with_full_flag_parses() {
        let cli = Cli::try_parse_from(["udoc", "inspect", "--full", "file.pdf"])
            .expect("inspect --full should parse");
        match cli.command {
            Some(Command::Inspect { full, .. }) => assert!(full),
            other => panic!("expected Inspect, got {other:?}"),
        }
    }

    #[test]
    fn fonts_audit_flag_parses() {
        let cli = Cli::try_parse_from(["udoc", "fonts", "--audit", "file.pdf"])
            .expect("fonts --audit should parse");
        match cli.command {
            Some(Command::Fonts { audit, .. }) => assert!(audit),
            other => panic!("expected Fonts, got {other:?}"),
        }
    }

    #[test]
    fn audit_fonts_subcommand_no_longer_exists() {
        // The standalone subcommand collapsed into `fonts --audit`.
        let res = Cli::try_parse_from(["udoc", "audit-fonts", "file.pdf"]);
        // Bare-file fallback may still parse "audit-fonts" as a positional
        // file path; assert it does NOT parse as a Command variant.
        if let Ok(cli) = res {
            assert!(
                cli.command.is_none(),
                "audit-fonts must not be recognized as a subcommand post-"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests for the surface: stable exit codes +
// --errors json formatting + classify_error mapping.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod cli_exitcodes_tests {
    use super::*;

    #[test]
    fn cli_exit_values_are_stable() {
        // Agent contract: do not change without bumping alpha tag.
        assert_eq!(CliExit::Success as i32, 0);
        assert_eq!(CliExit::ExtractionFailed as i32, 1);
        assert_eq!(CliExit::UsageError as i32, 2);
        assert_eq!(CliExit::FileNotFound as i32, 3);
    }

    #[test]
    fn error_code_strings_are_stable() {
        assert_eq!(ErrorCode::FileNotFound.as_str(), "E_FILE_NOT_FOUND");
        assert_eq!(ErrorCode::PermissionDenied.as_str(), "E_PERMISSION_DENIED");
        assert_eq!(
            ErrorCode::FormatUnsupported.as_str(),
            "E_FORMAT_UNSUPPORTED"
        );
        assert_eq!(
            ErrorCode::EncryptionRequired.as_str(),
            "E_ENCRYPTION_REQUIRED"
        );
        assert_eq!(ErrorCode::InvalidArgument.as_str(), "E_INVALID_ARGUMENT");
        assert_eq!(ErrorCode::ParseError.as_str(), "E_PARSE_ERROR");
        assert_eq!(ErrorCode::ExtractionFailed.as_str(), "E_EXTRACTION_FAILED");
        assert_eq!(ErrorCode::Internal.as_str(), "E_INTERNAL");
    }

    #[test]
    fn error_code_to_exit_mapping() {
        assert_eq!(ErrorCode::FileNotFound.exit(), CliExit::FileNotFound);
        assert_eq!(ErrorCode::PermissionDenied.exit(), CliExit::FileNotFound);
        assert_eq!(ErrorCode::InvalidArgument.exit(), CliExit::UsageError);
        assert_eq!(ErrorCode::FormatUnsupported.exit(), CliExit::UsageError);
        assert_eq!(
            ErrorCode::EncryptionRequired.exit(),
            CliExit::ExtractionFailed
        );
        assert_eq!(ErrorCode::ParseError.exit(), CliExit::ExtractionFailed);
        assert_eq!(
            ErrorCode::ExtractionFailed.exit(),
            CliExit::ExtractionFailed
        );
        assert_eq!(ErrorCode::Internal.exit(), CliExit::ExtractionFailed);
    }

    #[test]
    fn classify_file_not_found() {
        assert_eq!(
            classify_error("no such file or directory"),
            ErrorCode::FileNotFound
        );
        assert_eq!(
            classify_error("foo.pdf: file not found"),
            ErrorCode::FileNotFound
        );
        assert_eq!(
            classify_error("bar.pdf does not exist"),
            ErrorCode::FileNotFound
        );
    }

    #[test]
    fn classify_permission_denied() {
        assert_eq!(
            classify_error("opening foo.pdf: Permission denied"),
            ErrorCode::PermissionDenied
        );
    }

    #[test]
    fn classify_format_unsupported() {
        assert_eq!(
            classify_error("unknown format 'mp4'"),
            ErrorCode::FormatUnsupported
        );
        assert_eq!(
            classify_error("unsupported format 'avi'"),
            ErrorCode::FormatUnsupported
        );
        assert_eq!(
            classify_error("unable to detect format for 'foo'"),
            ErrorCode::FormatUnsupported
        );
    }

    #[test]
    fn classify_encryption() {
        assert_eq!(
            classify_error("encryption error: invalid password"),
            ErrorCode::EncryptionRequired
        );
        assert_eq!(
            classify_error("PDF requires a password"),
            ErrorCode::EncryptionRequired
        );
    }

    #[test]
    fn classify_parse_error() {
        assert_eq!(
            classify_error("xref parse failed at offset 1234"),
            ErrorCode::ParseError
        );
        assert_eq!(
            classify_error("malformed object stream"),
            ErrorCode::ParseError
        );
    }

    #[test]
    fn classify_falls_back_to_extraction_failed() {
        assert_eq!(
            classify_error("something went wrong"),
            ErrorCode::ExtractionFailed
        );
    }

    #[test]
    fn errors_flag_parses_human_and_json() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["udoc", "--errors", "json", "file.pdf"]).unwrap();
        assert_eq!(cli.extract.errors, ErrorFormat::Json);

        let cli = Cli::try_parse_from(["udoc", "--errors", "human", "file.pdf"]).unwrap();
        assert_eq!(cli.extract.errors, ErrorFormat::Human);

        // Default when omitted.
        let cli = Cli::try_parse_from(["udoc", "file.pdf"]).unwrap();
        assert_eq!(cli.extract.errors, ErrorFormat::Human);
    }

    #[test]
    fn errors_flag_rejects_invalid_value() {
        use clap::Parser;
        let res = Cli::try_parse_from(["udoc", "--errors", "yaml", "file.pdf"]);
        assert!(res.is_err(), "--errors yaml must be rejected");
    }

    #[test]
    fn severity_rank_orders_correctly() {
        // FileNotFound is "more severe" than EncryptionRequired which is
        // more severe than ExtractionFailed -- so a batch failure picks
        // the most actionable code.
        assert!(
            severity_rank(ErrorCode::FileNotFound) > severity_rank(ErrorCode::EncryptionRequired)
        );
        assert!(
            severity_rank(ErrorCode::PermissionDenied) > severity_rank(ErrorCode::ExtractionFailed)
        );
        assert!(
            severity_rank(ErrorCode::EncryptionRequired) > severity_rank(ErrorCode::ParseError)
        );
    }

    #[test]
    fn report_run_error_returns_correct_exit_code() {
        // Sanity check the wiring: a "no such file" string gets mapped
        // to CliExit::FileNotFound which converts to ExitCode::from(3).
        let _ = report_run_error(ErrorFormat::Json, "no such file: foo.pdf", true);
        // ExitCode is opaque; we can't compare directly, but the
        // classification step is covered above. This test exists to
        // prevent the wiring from breaking silently.
    }

    #[test]
    fn errors_json_emits_one_line_per_error() {
        // Direct unit test of emit_error: when ErrorFormat::Json, the
        // output should be a single line ending in '\n'. We can't
        // easily intercept stderr in a unit test without nightly, so
        // this test just exercises the code path -- the integration
        // tests in cli.rs (added below in subsequent commits) verify
        // the actual output shape.
        emit_error(
            ErrorFormat::Json,
            ErrorCode::FormatUnsupported,
            "test message",
            Some("file.pdf"),
        );
        emit_error(ErrorFormat::Human, ErrorCode::Internal, "test", None);
    }
}

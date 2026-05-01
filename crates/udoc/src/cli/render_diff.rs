//! `udoc render-diff` subcommand.
//!
//! Renders PDF pages with udoc and a reference renderer (mupdf or
//! poppler), then reports SSIM and PSNR per page. Intended to be the
//! inner-loop harness for renderer work: run on a target corpus, tune,
//! re-run, compare.
//!
//! Subprocess strategy: the reference renderer writes a PPM (netpbm P6)
//! file to a temp directory. PPM has a trivial header
//! (`P6\n<w> <h>\n<maxval>\n<raw-rgb-bytes>`) so we can parse it without a
//! PNG decoder dependency. udoc renders via [`udoc::render::render_page_rgb`]
//! to skip the PNG round-trip on the udoc side.
//!
//! Exit codes: 0 all pages pass, 1 any page below gate, 2 error
//! (missing external tool, IO, render failure).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::time::Instant;

use udoc_core::metrics::{mean_ssim, psnr};

use udoc::render::{font_cache::FontCache, png::encode_rgb_png, render_page_rgb};
use udoc::{extract_with, Config};

/// Reference renderer to compare against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reference {
    /// MuPDF via `mutool draw`.
    Mupdf,
    /// Poppler via `pdftoppm`.
    Poppler,
}

impl FromStr for Reference {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "mupdf" => Ok(Reference::Mupdf),
            "poppler" => Ok(Reference::Poppler),
            other => Err(format!(
                "unknown reference '{other}', expected 'mupdf' or 'poppler'"
            )),
        }
    }
}

/// Parsed arguments for the render-diff subcommand.
#[derive(Debug, Clone)]
pub struct Args {
    /// PDF path to render.
    pub file: PathBuf,
    /// Reference renderer to compare against.
    pub against: Reference,
    /// Page-range spec (`"1"`, `"1-5"`, `"3,7,9-12"`).
    pub pages: String,
    /// Per-page SSIM gate; pages below this are reported as `fail`.
    pub gate: f64,
    /// Optional directory to write `udoc/ref/diff` PNGs on failure.
    pub output_dir: Option<PathBuf>,
    /// DPI to render at on both sides.
    pub dpi: u32,
    /// Accept small dimension mismatches between udoc and the reference.
    pub force_dpi: bool,
}

/// Run the render-diff subcommand. Returns the process exit code.
pub fn run(args: Args) -> u8 {
    match run_inner(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("udoc render-diff: {e}");
            2
        }
    }
}

fn run_inner(args: &Args) -> Result<u8, String> {
    // Verify the reference tool is on PATH before doing any work.
    if !tool_available(args.against) {
        eprintln!(
            "udoc render-diff: '{}' not found on PATH (needed for --against {})",
            tool_name(args.against),
            reference_name(args.against),
        );
        return Ok(2);
    }

    let pages = parse_page_spec(&args.pages)?;
    if pages.is_empty() {
        return Err(format!("empty page spec '{}'", args.pages));
    }

    // Font assets are opt-in on the default extraction path; render-diff
    // must have them or the renderer falls back to Liberation Sans with
    // the wrong metrics and every SSIM result is misleading.
    let mut config = Config::new();
    config.assets.fonts = true;
    let doc = extract_with(&args.file, config)
        .map_err(|e| format!("extracting '{}': {e}", args.file.display()))?;
    let mut font_cache = FontCache::new(&doc.assets);

    if let Some(out_dir) = &args.output_dir {
        std::fs::create_dir_all(out_dir)
            .map_err(|e| format!("creating output dir '{}': {e}", out_dir.display()))?;
    }

    let tmp = tempdir_under(std::env::temp_dir(), "udoc-render-diff")?;

    let mut any_fail = false;
    for page in &pages {
        // Pages are 1-based in the CLI, 0-based internally.
        let idx = page.saturating_sub(1);
        let t_udoc = Instant::now();
        let (mut u_rgb, mut uw, mut uh) = render_page_rgb(&doc, idx, args.dpi, &mut font_cache)
            .map_err(|e| format!("udoc render page {page}: {e}"))?;
        let udoc_ms = t_udoc.elapsed().as_secs_f64() * 1000.0;

        let t_ref = Instant::now();
        let (mut r_rgb, mut rw, mut rh) =
            render_reference(&args.file, *page, args.dpi, args.against, tmp.path())?;
        let ref_ms = t_ref.elapsed().as_secs_f64() * 1000.0;

        // Dimension mismatch policy: ±2 px on each axis, center-crop to min.
        // Anything larger fails with a clear hint.
        if (uw, uh) != (rw, rh) {
            let dw = (uw as i64 - rw as i64).abs();
            let dh = (uh as i64 - rh as i64).abs();
            if args.force_dpi || (dw <= 2 && dh <= 2) {
                let (cw, ch) = (uw.min(rw), uh.min(rh));
                u_rgb = center_crop(&u_rgb, uw, uh, cw, ch);
                r_rgb = center_crop(&r_rgb, rw, rh, cw, ch);
                uw = cw;
                uh = ch;
                rw = cw;
                rh = ch;
            } else {
                return Err(format!(
                    "dimension mismatch on page {page}: udoc={uw}x{uh}, ref={rw}x{rh} (diff {dw}x{dh}); re-run with --force-dpi to accept the mismatch and center-crop"
                ));
            }
        }

        let ssim = mean_ssim(&u_rgb, &r_rgb, uw, uh);
        let p_db = psnr(&u_rgb, &r_rgb);
        let gate_pass = ssim >= args.gate;
        if !gate_pass {
            any_fail = true;
        }

        // JSON line per page.
        let psnr_s = if p_db.is_infinite() {
            "null".to_string()
        } else {
            format!("{:.3}", p_db)
        };
        println!(
            "{{\"page\":{page},\"ssim\":{:.4},\"psnr\":{psnr_s},\"gate\":\"{}\",\"udoc_ms\":{:.2},\"ref_ms\":{:.2}}}",
            ssim,
            if gate_pass { "pass" } else { "fail" },
            udoc_ms,
            ref_ms,
        );

        // Write diff artifacts on failure if an output dir was provided.
        if !gate_pass {
            if let Some(out_dir) = &args.output_dir {
                let doc_stem = args
                    .file
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("doc");
                let udoc_png = encode_rgb_png(&u_rgb, uw, uh);
                let ref_png = encode_rgb_png(&r_rgb, rw, rh);
                let diff_png = encode_rgb_png(&diff_buf(&u_rgb, &r_rgb), uw, uh);
                write_png(out_dir, &format!("{doc_stem}-p{page}.udoc.png"), &udoc_png)?;
                write_png(out_dir, &format!("{doc_stem}-p{page}.ref.png"), &ref_png)?;
                write_png(out_dir, &format!("{doc_stem}-p{page}.diff.png"), &diff_png)?;
            }
        }
    }

    Ok(if any_fail { 1 } else { 0 })
}

fn write_png(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    let path = dir.join(name);
    std::fs::write(&path, bytes).map_err(|e| format!("writing '{}': {e}", path.display()))
}

fn diff_buf(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.iter()
        .zip(b.iter())
        .map(|(&x, &y)| {
            let d = (x as i16 - y as i16).unsigned_abs() * 4;
            d.min(255) as u8
        })
        .collect()
}

fn center_crop(rgb: &[u8], w: u32, h: u32, cw: u32, ch: u32) -> Vec<u8> {
    let (w, h, cw, ch) = (w as usize, h as usize, cw as usize, ch as usize);
    let ox = (w.saturating_sub(cw)) / 2;
    let oy = (h.saturating_sub(ch)) / 2;
    let mut out = Vec::with_capacity(cw * ch * 3);
    for y in 0..ch {
        let src_row = (oy + y) * w + ox;
        let start = src_row * 3;
        let end = start + cw * 3;
        out.extend_from_slice(&rgb[start..end]);
    }
    out
}

/// Invoke the reference renderer; return RGB8 + dimensions.
fn render_reference(
    file: &Path,
    page: usize,
    dpi: u32,
    which: Reference,
    tmp: &Path,
) -> Result<(Vec<u8>, u32, u32), String> {
    let ppm_path = tmp.join(format!("ref-p{page}.ppm"));
    let status = match which {
        Reference::Mupdf => {
            // mutool draw -F ppm -r DPI -o out.ppm file.pdf N. Forcing `ppm`
            // gives us P6 (RGB); `pnm` would default to P5 grayscale for
            // purely-black-text pages, which our SSIM is fine with but other
            // tooling may not be.
            Command::new(tool_name(which))
                .arg("draw")
                .args(["-F", "ppm"])
                .args(["-r", &dpi.to_string()])
                .arg("-o")
                .arg(&ppm_path)
                .arg(file)
                .arg(page.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .map_err(|e| format!("spawn mutool: {e}"))?
        }
        Reference::Poppler => {
            // pdftoppm -r DPI -f N -l N -singlefile file.pdf out  -> writes out.ppm
            let out_prefix = ppm_path.with_extension("");
            let status = Command::new(tool_name(which))
                .args(["-r", &dpi.to_string()])
                .args(["-f", &page.to_string()])
                .args(["-l", &page.to_string()])
                .arg("-singlefile")
                .arg(file)
                .arg(&out_prefix)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .map_err(|e| format!("spawn pdftoppm: {e}"))?;
            // pdftoppm appends .ppm
            let ppm_actual = PathBuf::from(format!("{}.ppm", out_prefix.display()));
            if ppm_actual.exists() && ppm_actual != ppm_path {
                std::fs::rename(&ppm_actual, &ppm_path)
                    .map_err(|e| format!("renaming '{}': {e}", ppm_actual.display()))?;
            }
            status
        }
    };
    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        return Err(format!(
            "{} failed for page {page}: {}",
            tool_name(which),
            stderr.trim()
        ));
    }
    let bytes =
        std::fs::read(&ppm_path).map_err(|e| format!("reading '{}': {e}", ppm_path.display()))?;
    parse_ppm_p6(&bytes)
}

/// Parse a binary PNM file (P5 grayscale or P6 RGB) into RGB8 + dimensions.
///
/// Mupdf's `mutool draw -F pnm` emits P5 (grayscale) while `pdftoppm`
/// emits P6 (RGB) by default. We accept both so render-diff doesn't care
/// which reference renderer produced the file; callers always receive RGB.
fn parse_ppm_p6(data: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    if data.len() < 11 || data[0] != b'P' {
        return Err("not a PNM file".into());
    }
    let kind = data[1];
    if kind != b'5' && kind != b'6' {
        return Err(format!(
            "unsupported PNM magic 'P{}' (expected P5 grayscale or P6 RGB)",
            kind as char
        ));
    }
    let mut i = 2;
    let mut tokens: Vec<u32> = Vec::with_capacity(3);
    while tokens.len() < 3 && i < data.len() {
        // Skip whitespace and comments.
        while i < data.len() && (data[i].is_ascii_whitespace()) {
            i += 1;
        }
        if i < data.len() && data[i] == b'#' {
            while i < data.len() && data[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;
        while i < data.len() && !data[i].is_ascii_whitespace() {
            i += 1;
        }
        if start == i {
            break;
        }
        let tok =
            std::str::from_utf8(&data[start..i]).map_err(|e| format!("PNM header utf8: {e}"))?;
        tokens.push(
            tok.parse::<u32>()
                .map_err(|e| format!("PNM header parse: {e}"))?,
        );
    }
    if tokens.len() != 3 {
        return Err("PNM header missing width/height/maxval".into());
    }
    // One whitespace byte after maxval.
    if i < data.len() && data[i].is_ascii_whitespace() {
        i += 1;
    }
    let (w, h, maxval) = (tokens[0], tokens[1], tokens[2]);
    if maxval != 255 {
        return Err(format!("unsupported PNM maxval {maxval} (need 255)"));
    }
    let channels = if kind == b'6' { 3 } else { 1 };
    let expected = (w as usize) * (h as usize) * channels;
    let pixels = data.get(i..i + expected).ok_or_else(|| {
        format!(
            "truncated PNM: need {expected} bytes of pixel data, have {}",
            data.len().saturating_sub(i)
        )
    })?;
    if kind == b'6' {
        Ok((pixels.to_vec(), w, h))
    } else {
        // P5 grayscale -> RGB expansion (one byte per channel replicated 3x).
        let mut rgb = Vec::with_capacity(pixels.len() * 3);
        for &g in pixels {
            rgb.extend_from_slice(&[g, g, g]);
        }
        Ok((rgb, w, h))
    }
}

fn tool_available(which: Reference) -> bool {
    // `which` / `where` would be ideal but we avoid new deps; try a no-op
    // invocation and check the error kind.
    let name = tool_name(which);
    let mut cmd = Command::new(name);
    // Neutral-ish flag that exists in both tools.
    match which {
        Reference::Mupdf => {
            cmd.arg("-v");
        }
        Reference::Poppler => {
            cmd.arg("-v");
        }
    }
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    match cmd.status() {
        Ok(_) => true,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                // Tool exists but failed for some other reason; we still
                // treat it as available and let the real invocation report.
                return true;
            }
            false
        }
    }
}

fn tool_name(which: Reference) -> &'static str {
    match which {
        Reference::Mupdf => "mutool",
        Reference::Poppler => "pdftoppm",
    }
}

fn reference_name(which: Reference) -> &'static str {
    match which {
        Reference::Mupdf => "mupdf",
        Reference::Poppler => "poppler",
    }
}

/// Parse `"1-5"`, `"3,7,9-12"`, `"1"` into a `Vec<usize>` of 1-based page numbers.
pub fn parse_page_spec(spec: &str) -> Result<Vec<usize>, String> {
    let mut out: Vec<usize> = Vec::new();
    for part in spec.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if let Some((a, b)) = part.split_once('-') {
            let start: usize = a.parse().map_err(|e| format!("page '{a}': {e}"))?;
            let end: usize = b.parse().map_err(|e| format!("page '{b}': {e}"))?;
            if start == 0 || end == 0 || end < start {
                return Err(format!("invalid range '{part}'"));
            }
            for p in start..=end {
                out.push(p);
            }
        } else {
            let p: usize = part.parse().map_err(|e| format!("page '{part}': {e}"))?;
            if p == 0 {
                return Err(format!("page '{part}' must be >= 1"));
            }
            out.push(p);
        }
    }
    Ok(out)
}

/// Create a temp subdirectory with a unique suffix. No `tempfile` dep.
fn tempdir_under(base: PathBuf, prefix: &str) -> Result<TempDir, String> {
    // pid + nanos avoids collisions between parallel invocations.
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = base.join(format!("{prefix}-{pid}-{nanos}"));
    std::fs::create_dir_all(&path)
        .map_err(|e| format!("creating temp dir '{}': {e}", path.display()))?;
    Ok(TempDir { path })
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Best-effort cleanup. Ignore errors so we don't mask a panic.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_page() {
        assert_eq!(parse_page_spec("3").unwrap(), vec![3]);
    }

    #[test]
    fn parse_range() {
        assert_eq!(parse_page_spec("1-3").unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn parse_mixed() {
        assert_eq!(parse_page_spec("1,3-5,8").unwrap(), vec![1, 3, 4, 5, 8]);
    }

    #[test]
    fn parse_zero_rejected() {
        assert!(parse_page_spec("0").is_err());
    }

    #[test]
    fn parse_reversed_rejected() {
        assert!(parse_page_spec("5-3").is_err());
    }

    #[test]
    fn ppm_round_trip() {
        // 2x2 red/green/blue/white image.
        let pixels: Vec<u8> = vec![
            255, 0, 0, // R
            0, 255, 0, // G
            0, 0, 255, // B
            255, 255, 255, // W
        ];
        let mut ppm = Vec::new();
        ppm.extend_from_slice(b"P6\n2 2\n255\n");
        ppm.extend_from_slice(&pixels);
        let (rgb, w, h) = parse_ppm_p6(&ppm).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(rgb, pixels);
    }

    #[test]
    fn ppm_with_comments() {
        let pixels: Vec<u8> = vec![128, 128, 128];
        let mut ppm = Vec::new();
        ppm.extend_from_slice(b"P6\n# some comment\n1 1\n# another\n255\n");
        ppm.extend_from_slice(&pixels);
        let (rgb, w, h) = parse_ppm_p6(&ppm).unwrap();
        assert_eq!((w, h), (1, 1));
        assert_eq!(rgb, pixels);
    }

    #[test]
    fn center_crop_3x3_to_1x1() {
        let mut buf = Vec::with_capacity(3 * 3 * 3);
        for i in 0..9 {
            buf.extend_from_slice(&[i as u8, 0, 0]);
        }
        // Center pixel of a 3x3 is index 4.
        let cropped = center_crop(&buf, 3, 3, 1, 1);
        assert_eq!(cropped, vec![4, 0, 0]);
    }

    #[test]
    fn reference_parsing() {
        assert_eq!(Reference::from_str("mupdf").unwrap(), Reference::Mupdf);
        assert_eq!(Reference::from_str("Poppler").unwrap(), Reference::Poppler);
        assert!(Reference::from_str("pdfium").is_err());
    }
}

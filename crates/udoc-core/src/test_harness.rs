//! Shared golden-file test infrastructure for format backends.
//!
//! Gated behind the `test-internals` feature so it is only compiled in
//! dev/test builds.  Every backend crate can depend on
//! `udoc-core = { features = ["test-internals"] }` in `[dev-dependencies]`
//! and reuse the same assertion + diff helpers.
//!
//! # Usage
//!
//! ```ignore
//! // ignore: requires the `test-internals` feature and a backend-specific
//! // `actual_output` value; these are downstream test crate concerns.
//! use udoc_core::test_harness::assert_golden;
//!
//! let golden_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
//! assert_golden("my_test", &actual_output, &golden_dir);
//! ```
//!
//! Run with `BLESS=1` to create or update expected files.

use std::path::Path;

/// Compare `actual` against `{golden_dir}/{test_name}.expected`.
///
/// * If `BLESS=1` (or any truthy value) is set in the environment, the
///   expected file is created/overwritten with `actual`.
/// * Otherwise, the expected file is read and compared. On mismatch the
///   function panics with a unified diff.
pub fn assert_golden(test_name: &str, actual: &str, golden_dir: &Path) {
    let path = golden_dir.join(format!("{test_name}.expected"));

    if std::env::var("BLESS").is_ok_and(|v| !v.is_empty() && v != "0" && v != "false") {
        std::fs::create_dir_all(path.parent().expect("golden file path has no parent"))
            .expect("failed to create golden file directory");
        std::fs::write(&path, actual).expect("failed to write golden file");
        return;
    }

    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("Golden file not found: {path:?}. Run with BLESS=1 to create."));

    if actual != expected {
        let mut diff = String::new();
        diff.push_str(&format!("--- expected: {path:?}\n"));
        diff.push_str("+++ actual\n");
        for line in simple_diff(&expected, actual) {
            diff.push_str(&line);
            diff.push('\n');
        }
        panic!("Golden file mismatch for '{test_name}':\n{diff}");
    }
}

/// Produce a simple line-by-line diff with `+`/`-` markers.
///
/// Positional mismatches are shown as `-`/`+` pairs. Lines only present
/// in expected or actual are shown separately so length differences are
/// clear. Intentionally simple (no LCS, no context window) to keep the
/// dependency-free test harness small.
pub fn simple_diff(expected: &str, actual: &str) -> Vec<String> {
    let exp_lines: Vec<&str> = expected.lines().collect();
    let act_lines: Vec<&str> = actual.lines().collect();
    let mut result = Vec::new();
    let common = exp_lines.len().min(act_lines.len());
    for i in 0..common {
        if exp_lines[i] != act_lines[i] {
            result.push(format!("-{}", exp_lines[i]));
            result.push(format!("+{}", act_lines[i]));
        }
    }
    for line in exp_lines.iter().skip(common) {
        result.push(format!("-{line}"));
    }
    for line in act_lines.iter().skip(common) {
        result.push(format!("+{line}"));
    }
    result
}

/// Produce a unified-style diff between expected and actual line slices.
///
/// Uses an LCS (longest common subsequence) approach so insertions and
/// deletions show up correctly instead of only flagging same-index
/// mismatches. Changed hunks are emitted with up to 2 lines of context.
pub fn unified_diff(expected: &[&str], actual: &[&str]) -> String {
    let n = expected.len();
    let m = actual.len();

    // Build LCS length table
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in 1..=n {
        for j in 1..=m {
            dp[i][j] = if expected[i - 1] == actual[j - 1] {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
        }
    }

    // Backtrack to produce diff hunks
    let mut result = String::new();
    let (mut i, mut j) = (n, m);
    let mut hunks: Vec<(char, &str)> = Vec::new();
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && expected[i - 1] == actual[j - 1] {
            hunks.push((' ', expected[i - 1]));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            hunks.push(('+', actual[j - 1]));
            j -= 1;
        } else {
            hunks.push(('-', expected[i - 1]));
            i -= 1;
        }
    }
    hunks.reverse();

    // Emit only changed hunks with up to 2 lines of context
    let context = 2;
    let mut last_printed = 0;
    let changed: Vec<usize> = hunks
        .iter()
        .enumerate()
        .filter(|(_, (c, _))| *c != ' ')
        .map(|(idx, _)| idx)
        .collect();

    for &idx in &changed {
        let start = idx.saturating_sub(context).max(last_printed);
        if start > last_printed && last_printed > 0 {
            result.push_str("  ...\n");
        }
        let end = (idx + context).min(hunks.len() - 1);
        for (k, &(marker, line)) in hunks[start..=end].iter().enumerate() {
            result.push_str(&format!("  {marker} {line}\n"));
            last_printed = start + k + 1;
        }
    }

    result
}

// ---------------------------------------------------------------------------
// PNG pixel goldens (byte-exact and SSIM-threshold variants)
// ---------------------------------------------------------------------------

fn bless_enabled() -> bool {
    std::env::var("BLESS").is_ok_and(|v| !v.is_empty() && v != "0" && v != "false")
}

/// Compare `actual` PNG bytes against `{golden_dir}/{test_name}.expected.png`.
///
/// BLESS=1 writes the PNG; otherwise bytes are compared byte-exact. On
/// mismatch, both file sizes and FNV-1a 64-bit hex hashes are reported, and
/// `{test_name}.actual.png` is written beside the expected file for inspection.
pub fn assert_golden_png_bytes(test_name: &str, actual: &[u8], golden_dir: &Path) {
    let expected_path = golden_dir.join(format!("{test_name}.expected.png"));

    if bless_enabled() {
        std::fs::create_dir_all(golden_dir).expect("failed to create golden directory");
        std::fs::write(&expected_path, actual).expect("failed to write golden PNG");
        return;
    }

    let expected = std::fs::read(&expected_path).unwrap_or_else(|_| {
        panic!("Golden PNG not found: {expected_path:?}. Run with BLESS=1 to create.")
    });

    if actual != expected {
        let actual_path = golden_dir.join(format!("{test_name}.actual.png"));
        std::fs::create_dir_all(golden_dir).ok();
        std::fs::write(&actual_path, actual).ok();
        let ah = fnv1a64(actual);
        let eh = fnv1a64(&expected);
        panic!(
            "PNG golden mismatch for '{test_name}':\n  expected: {expected_path:?} ({} bytes, hash {eh:016x})\n  actual:   {actual_path:?} ({} bytes, hash {ah:016x})",
            expected.len(),
            actual.len()
        );
    }
}

/// Compare `actual` PNG bytes against `{golden_dir}/{test_name}.expected.png`
/// using mean SSIM with an 8x8 sliding window over Rec.709 luminance.
///
/// BLESS=1 writes the PNG; otherwise a mismatch below `threshold` panics
/// with the measured SSIM and writes `{name}.actual.png` plus
/// `{name}.diff.png` (abs-diff * 4 clamped to 0-255) alongside the expected.
pub fn assert_golden_png_ssim(test_name: &str, actual: &[u8], golden_dir: &Path, threshold: f64) {
    let expected_path = golden_dir.join(format!("{test_name}.expected.png"));

    if bless_enabled() {
        std::fs::create_dir_all(golden_dir).expect("failed to create golden directory");
        std::fs::write(&expected_path, actual).expect("failed to write golden PNG");
        return;
    }

    let expected = std::fs::read(&expected_path).unwrap_or_else(|_| {
        panic!("Golden PNG not found: {expected_path:?}. Run with BLESS=1 to create.")
    });

    let (a_rgb, aw, ah) = decode_png_rgb8(actual)
        .unwrap_or_else(|e| panic!("failed to decode actual PNG for '{test_name}': {e}"));
    let (e_rgb, ew, eh) = decode_png_rgb8(&expected)
        .unwrap_or_else(|e| panic!("failed to decode expected PNG for '{test_name}': {e}"));

    if (aw, ah) != (ew, eh) {
        let actual_path = golden_dir.join(format!("{test_name}.actual.png"));
        std::fs::write(&actual_path, actual).ok();
        panic!(
            "PNG dimension mismatch for '{test_name}': expected {ew}x{eh}, got {aw}x{ah}; wrote {actual_path:?}"
        );
    }

    let ssim = mean_ssim(&a_rgb, &e_rgb, aw, ah);
    if ssim < threshold {
        let actual_path = golden_dir.join(format!("{test_name}.actual.png"));
        let diff_path = golden_dir.join(format!("{test_name}.diff.png"));
        std::fs::write(&actual_path, actual).ok();
        let diff = diff_png(&a_rgb, &e_rgb, aw, ah);
        std::fs::write(&diff_path, diff).ok();
        panic!(
            "SSIM below threshold for '{test_name}': {ssim:.4} < {threshold:.4}; wrote {actual_path:?} and {diff_path:?}"
        );
    }
}

/// FNV-1a 64-bit hash for compact reporting of large byte blobs.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn decode_png_rgb8(data: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let decoder = png::Decoder::new(data);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let info = reader.info().clone();
    let mut buf = vec![0u8; reader.output_buffer_size()];
    reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let w = info.width;
    let h = info.height;
    let rgb = match info.color_type {
        png::ColorType::Rgb => buf,
        png::ColorType::Rgba => {
            let mut out = Vec::with_capacity((w * h * 3) as usize);
            for chunk in buf.chunks_exact(4) {
                out.extend_from_slice(&chunk[..3]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity((w * h * 3) as usize);
            for &v in &buf {
                out.extend_from_slice(&[v, v, v]);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity((w * h * 3) as usize);
            for chunk in buf.chunks_exact(2) {
                out.extend_from_slice(&[chunk[0], chunk[0], chunk[0]]);
            }
            out
        }
        png::ColorType::Indexed => return Err("indexed color PNGs not supported".into()),
    };
    Ok((rgb, w, h))
}

/// Compute mean SSIM over 8x8 windows on Rec.709 luminance.
///
/// Re-exported from [`crate::metrics::mean_ssim`] so the golden harness
/// keeps its one-stop import. Runtime callers (render-diff, render-inspect)
/// use `udoc_core::metrics::mean_ssim` directly.
pub use crate::metrics::mean_ssim;

fn diff_png(a: &[u8], b: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut diff = Vec::with_capacity(a.len());
    for (ax, bx) in a.iter().zip(b.iter()) {
        let d = (*ax as i16 - *bx as i16).unsigned_abs();
        diff.push((d.saturating_mul(4)).min(255) as u8);
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        writer.write_image_data(&diff).expect("png data");
    }
    out
}

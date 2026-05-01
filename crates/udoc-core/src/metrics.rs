//! Image similarity metrics (SSIM, PSNR) for visual regression testing.
//!
//! Used by the render-diff CLI to compare PDF renders against reference
//! renderers (mupdf, poppler) and by the golden-page test harness to detect
//! regressions. Inputs are flat `[R,G,B,R,G,B,...]` byte buffers; metrics
//! operate on Rec.709 luminance. Both algorithms are vendored with no
//! runtime dependencies so the core crate stays lightweight.

/// Compute mean SSIM over 8x8 windows on Rec.709 luminance.
///
/// Expects interleaved RGB8 input (3 bytes per pixel). C1 = (0.01 * 255)^2,
/// C2 = (0.03 * 255)^2. Per-window SSIM is
/// `(2*mu_x*mu_y + C1)(2*sigma_xy + C2) / ((mu_x^2 + mu_y^2 + C1)(sigma_x^2 + sigma_y^2 + C2))`.
///
/// Returns `0.0` if the input length doesn't match `width * height * 3`.
/// Returns `1.0` for identical images smaller than the 8x8 window.
pub fn mean_ssim(a: &[u8], b: &[u8], width: u32, height: u32) -> f64 {
    const W: usize = 8;
    let w = width as usize;
    let h = height as usize;
    if a.len() != w * h * 3 || b.len() != w * h * 3 {
        return 0.0;
    }
    if w < W || h < W {
        return if a == b { 1.0 } else { 0.0 };
    }

    let mut la = vec![0f64; w * h];
    let mut lb = vec![0f64; w * h];
    for i in 0..(w * h) {
        let ai = i * 3;
        la[i] = 0.2126 * a[ai] as f64 + 0.7152 * a[ai + 1] as f64 + 0.0722 * a[ai + 2] as f64;
        lb[i] = 0.2126 * b[ai] as f64 + 0.7152 * b[ai + 1] as f64 + 0.0722 * b[ai + 2] as f64;
    }

    let c1 = (0.01f64 * 255.0).powi(2);
    let c2 = (0.03f64 * 255.0).powi(2);
    let n = (W * W) as f64;

    let mut acc = 0f64;
    let mut count = 0u64;
    let mut y = 0;
    while y + W <= h {
        let mut x = 0;
        while x + W <= w {
            let mut sa = 0f64;
            let mut sb = 0f64;
            for j in 0..W {
                let row = (y + j) * w + x;
                for i in 0..W {
                    sa += la[row + i];
                    sb += lb[row + i];
                }
            }
            let mu_a = sa / n;
            let mu_b = sb / n;
            let mut var_a = 0f64;
            let mut var_b = 0f64;
            let mut cov = 0f64;
            for j in 0..W {
                let row = (y + j) * w + x;
                for i in 0..W {
                    let da = la[row + i] - mu_a;
                    let db = lb[row + i] - mu_b;
                    var_a += da * da;
                    var_b += db * db;
                    cov += da * db;
                }
            }
            var_a /= n;
            var_b /= n;
            cov /= n;
            let num = (2.0 * mu_a * mu_b + c1) * (2.0 * cov + c2);
            let den = (mu_a * mu_a + mu_b * mu_b + c1) * (var_a + var_b + c2);
            acc += num / den;
            count += 1;
            x += W;
        }
        y += W;
    }
    if count == 0 {
        1.0
    } else {
        acc / count as f64
    }
}

/// Compute PSNR in decibels between two RGB8 buffers.
///
/// PSNR = `10 * log10(255^2 / mse)` where MSE is the mean squared
/// per-channel pixel difference. Returns `f64::INFINITY` for identical
/// inputs (MSE == 0). Returns `0.0` if the buffers have mismatched lengths.
pub fn psnr(a: &[u8], b: &[u8]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut sum_sq: u64 = 0;
    for (&ax, &bx) in a.iter().zip(b.iter()) {
        let d = ax as i32 - bx as i32;
        sum_sq += (d * d) as u64;
    }
    let mse = sum_sq as f64 / a.len() as f64;
    if mse == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0f64 * 255.0 / mse).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssim_identical_images() {
        let pixels = vec![128u8; 16 * 16 * 3];
        let s = mean_ssim(&pixels, &pixels, 16, 16);
        assert!((s - 1.0).abs() < 1e-9, "expected 1.0, got {s}");
    }

    #[test]
    fn ssim_black_vs_white() {
        let black = vec![0u8; 16 * 16 * 3];
        let white = vec![255u8; 16 * 16 * 3];
        let s = mean_ssim(&black, &white, 16, 16);
        assert!((0.0..0.01).contains(&s), "expected near-zero, got {s}");
    }

    #[test]
    fn ssim_small_perturbation_stays_high() {
        let mut a = vec![200u8; 16 * 16 * 3];
        let mut b = a.clone();
        b[0] = 0;
        let s = mean_ssim(&a, &b, 16, 16);
        assert!((0.75..1.0).contains(&s), "expected (0.75, 1.0), got {s}");
        std::mem::swap(&mut a, &mut b);
        let s2 = mean_ssim(&a, &b, 16, 16);
        assert!((s - s2).abs() < 1e-9);
    }

    #[test]
    fn ssim_dimension_mismatch_returns_zero() {
        let a = vec![0u8; 10 * 10 * 3];
        let b = vec![0u8; 10 * 10 * 3];
        // Pass wrong width/height to simulate length mismatch.
        assert_eq!(mean_ssim(&a, &b, 5, 10), 0.0);
    }

    #[test]
    fn psnr_identical_images() {
        let pixels = vec![128u8; 64 * 64 * 3];
        assert!(psnr(&pixels, &pixels).is_infinite());
    }

    #[test]
    fn psnr_black_vs_white() {
        let black = vec![0u8; 64 * 64 * 3];
        let white = vec![255u8; 64 * 64 * 3];
        let db = psnr(&black, &white);
        // MSE = 255^2 = 65025, PSNR = 0 dB
        assert!(db.abs() < 1e-9, "expected 0 dB, got {db}");
    }

    #[test]
    fn psnr_small_error() {
        let a = vec![128u8; 64 * 64 * 3];
        let mut b = a.clone();
        for pix in b.iter_mut().take(64) {
            *pix = 130;
        }
        let db = psnr(&a, &b);
        // Small perturbation should give a very high PSNR (>= ~50 dB).
        assert!(db > 40.0, "expected >40 dB, got {db}");
    }

    #[test]
    fn psnr_length_mismatch_returns_zero() {
        let a = vec![0u8; 10];
        let b = vec![0u8; 12];
        assert_eq!(psnr(&a, &b), 0.0);
    }
}

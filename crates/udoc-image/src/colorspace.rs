//! Colorspace conversion helpers: CMYK, Gray, Lab to sRGB.
//!
//! These are naive, non-ICC conversions intended for viewer-grade rendering
//! and image extraction. Matches the behaviour that `mupdf` and `poppler` use
//! for DeviceCMYK / DeviceGray when no explicit ICC profile is attached.
//!
//! # Non-goals
//!
//! Full ICC profile application is out of scope (post-alpha). These helpers
//! do not consult `/ICCBased` colourspaces, do not perform gamut mapping, and
//! do not apply black-point compensation. For Lab, the reference illuminant
//! is pinned to D50 and adapted to D65 via Bradford before converting to
//! sRGB; that is standard practice for PDF Lab colourspaces but will not
//! match a colour-managed workflow exactly.
//!
//! # Output
//!
//! All helpers return 8-bit sRGB tuples or byte buffers. Values are clamped
//! to `[0, 255]`.

/// Colorspace descriptor for raw-to-PNG transcoding (#167).
///
/// Describes the colorspace of decoded raw pixel bytes going into the PNG
/// encoder. Callers that need ICC-tagged DeviceN should convert upstream and
/// pass [`Colorspace::Rgb`] or [`Colorspace::Gray`] here.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Colorspace {
    /// 3 samples per pixel: R, G, B.
    Rgb,
    /// 4 samples per pixel: C, M, Y, K.
    Cmyk,
    /// 1 sample per pixel (grayscale).
    Gray,
    /// 3 samples per pixel: L*, a*, b* (encoded per PDF DeviceLab).
    Lab,
    /// 1 sample per pixel indexing into the palette (RGB tuples).
    Indexed(Vec<(u8, u8, u8)>),
}

/// Byte-channel CMYK->RGB convenience wrapper over [`cmyk_to_rgb`].
#[inline]
pub fn cmyk_bytes_to_rgb(c: u8, m: u8, y: u8, k: u8) -> [u8; 3] {
    let (r, g, b) = cmyk_to_rgb(
        c as f32 / 255.0,
        m as f32 / 255.0,
        y as f32 / 255.0,
        k as f32 / 255.0,
    );
    [r, g, b]
}

/// Byte-channel Gray->RGB convenience wrapper over [`gray_to_rgb`].
#[inline]
pub fn gray_byte_to_rgb(gray: u8) -> [u8; 3] {
    let (r, g, b) = gray_to_rgb(gray);
    [r, g, b]
}

/// Byte-channel Lab->RGB convenience wrapper over [`lab_to_rgb`].
///
/// Input is PDF-style DeviceLab encoding where L is 0..255 mapping to
/// 0..100, and `a`/`b` are 0..255 mapping to -128..127.
#[inline]
pub fn lab_bytes_to_rgb(l: u8, a: u8, b: u8) -> [u8; 3] {
    let l_f = l as f32 / 255.0 * 100.0;
    let a_f = a as f32 - 128.0;
    let b_f = b as f32 - 128.0;
    let (r, g, blue) = lab_to_rgb(l_f, a_f, b_f);
    [r, g, blue]
}

/// Convert a CMYK tuple (each component in `[0.0, 1.0]`) to 8-bit sRGB using
/// the naive subtractive formula. Values outside `[0, 1]` are clamped.
///
/// ```
/// use udoc_image::cmyk_to_rgb;
/// assert_eq!(cmyk_to_rgb(0.0, 0.0, 0.0, 0.0), (255, 255, 255));
/// assert_eq!(cmyk_to_rgb(1.0, 0.0, 0.0, 0.0), (0, 255, 255));
/// ```
pub fn cmyk_to_rgb(c: f32, m: f32, y: f32, k: f32) -> (u8, u8, u8) {
    let c = c.clamp(0.0, 1.0);
    let m = m.clamp(0.0, 1.0);
    let y = y.clamp(0.0, 1.0);
    let k = k.clamp(0.0, 1.0);
    let r = 255.0 * (1.0 - c) * (1.0 - k);
    let g = 255.0 * (1.0 - m) * (1.0 - k);
    let b = 255.0 * (1.0 - y) * (1.0 - k);
    (
        r.round().clamp(0.0, 255.0) as u8,
        g.round().clamp(0.0, 255.0) as u8,
        b.round().clamp(0.0, 255.0) as u8,
    )
}

/// Convert an 8-bit grayscale value to 8-bit sRGB by replication.
///
/// ```
/// use udoc_image::gray_to_rgb;
/// assert_eq!(gray_to_rgb(128), (128, 128, 128));
/// ```
pub fn gray_to_rgb(gray: u8) -> (u8, u8, u8) {
    (gray, gray, gray)
}

/// Convert a CIE L*a*b* triple to 8-bit sRGB.
///
/// Input ranges follow PDF conventions: `L` in `[0, 100]`, `a` and `b` in a
/// nominal `[-128, 127]`. The reference white is D50 (PDF's default for
/// `/Lab`), adapted to D65 via Bradford before converting to sRGB. The
/// conversion is not ICC-accurate; tolerance of roughly +/-10 per channel
/// against reference implementations is expected for saturated colours.
pub fn lab_to_rgb(l: f32, a: f32, b: f32) -> (u8, u8, u8) {
    // Lab -> XYZ (D50)
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;

    // f^-1(t) = t^3 if t > 6/29, else 3 * (6/29)^2 * (t - 4/29).
    fn f_inv(t: f32) -> f32 {
        const DELTA: f32 = 6.0 / 29.0;
        if t > DELTA {
            t * t * t
        } else {
            3.0 * DELTA * DELTA * (t - 4.0 / 29.0)
        }
    }

    // D50 reference white.
    const XN: f32 = 0.96422;
    const YN: f32 = 1.00000;
    const ZN: f32 = 0.82521;
    let x_d50 = XN * f_inv(fx);
    let y_d50 = YN * f_inv(fy);
    let z_d50 = ZN * f_inv(fz);

    // Bradford-adapt D50 -> D65.
    let x = 0.9555766 * x_d50 + -0.0230393 * y_d50 + 0.0631636 * z_d50;
    let y = -0.0282895 * x_d50 + 1.0099416 * y_d50 + 0.0210077 * z_d50;
    let z = 0.0122982 * x_d50 + -0.0204830 * y_d50 + 1.3299098 * z_d50;

    // XYZ (D65) -> linear sRGB.
    let r_lin = 3.2404542 * x + -1.5371385 * y + -0.4985314 * z;
    let g_lin = -0.969_266 * x + 1.876_010_8 * y + 0.041_556 * z;
    let b_lin = 0.0556434 * x + -0.2040259 * y + 1.0572252 * z;

    // Linear sRGB -> gamma-encoded sRGB.
    fn encode(v: f32) -> u8 {
        let v = v.clamp(0.0, 1.0);
        let encoded = if v <= 0.0031308 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        };
        (encoded * 255.0).round().clamp(0.0, 255.0) as u8
    }
    (encode(r_lin), encode(g_lin), encode(b_lin))
}

/// Convert a CMYK image buffer (4 bytes per pixel) to sRGB (3 bytes per pixel).
///
/// Input components are u8 (0..=255) and interpreted as `fraction = byte/255`.
/// The output length is `width * height * 3`. If `data.len() < width * height * 4`
/// the returned buffer stops at the last full input pixel.
pub fn cmyk_image_to_rgb(data: &[u8], width: u32, height: u32) -> Vec<u8> {
    let pixel_count = (width as usize).saturating_mul(height as usize);
    let usable = pixel_count.min(data.len() / 4);
    let mut out = Vec::with_capacity(usable * 3);
    for chunk in data.chunks_exact(4).take(usable) {
        let (r, g, b) = cmyk_to_rgb(
            f32::from(chunk[0]) / 255.0,
            f32::from(chunk[1]) / 255.0,
            f32::from(chunk[2]) / 255.0,
            f32::from(chunk[3]) / 255.0,
        );
        out.push(r);
        out.push(g);
        out.push(b);
    }
    out
}

/// Convert a grayscale image buffer (1 byte per pixel) to sRGB (3 bytes per pixel).
///
/// Output length is `3 * data.len()`.
pub fn gray_image_to_rgb(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 3);
    for &g in data {
        out.push(g);
        out.push(g);
        out.push(g);
    }
    out
}

/// Convert a Lab image buffer (3 bytes per pixel, `L` in `[0,255]` mapped to
/// `[0,100]`, `a` and `b` as biased u8 where `128` = 0) to sRGB (3 bytes per
/// pixel).
///
/// The biasing convention is PDF's packed-Lab encoding for 8-bit samples:
/// `a_signed = a_byte - 128`, `b_signed = b_byte - 128`.
/// If `data.len() < width * height * 3` the returned buffer stops at the
/// last full input pixel.
pub fn lab_image_to_rgb(data: &[u8], width: u32, height: u32) -> Vec<u8> {
    let pixel_count = (width as usize).saturating_mul(height as usize);
    let usable = pixel_count.min(data.len() / 3);
    let mut out = Vec::with_capacity(usable * 3);
    for chunk in data.chunks_exact(3).take(usable) {
        let l = f32::from(chunk[0]) * (100.0 / 255.0);
        let a = f32::from(chunk[1]) - 128.0;
        let b = f32::from(chunk[2]) - 128.0;
        let (r, g, bl) = lab_to_rgb(l, a, b);
        out.push(r);
        out.push(g);
        out.push(bl);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: u8, b: u8, tol: u8) -> bool {
        a.abs_diff(b) <= tol
    }

    #[test]
    fn cmyk_cyan() {
        assert_eq!(cmyk_to_rgb(1.0, 0.0, 0.0, 0.0), (0, 255, 255));
    }

    #[test]
    fn cmyk_magenta() {
        assert_eq!(cmyk_to_rgb(0.0, 1.0, 0.0, 0.0), (255, 0, 255));
    }

    #[test]
    fn cmyk_yellow() {
        assert_eq!(cmyk_to_rgb(0.0, 0.0, 1.0, 0.0), (255, 255, 0));
    }

    #[test]
    fn cmyk_black() {
        assert_eq!(cmyk_to_rgb(0.0, 0.0, 0.0, 1.0), (0, 0, 0));
    }

    #[test]
    fn cmyk_white() {
        assert_eq!(cmyk_to_rgb(0.0, 0.0, 0.0, 0.0), (255, 255, 255));
    }

    #[test]
    fn cmyk_clamps_out_of_range() {
        // Negative inputs clamp to 0; >1 clamps to 1.
        assert_eq!(cmyk_to_rgb(-0.5, -0.5, -0.5, 0.0), (255, 255, 255));
        assert_eq!(cmyk_to_rgb(2.0, 0.0, 0.0, 0.0), (0, 255, 255));
    }

    #[test]
    fn gray_identity() {
        assert_eq!(gray_to_rgb(0), (0, 0, 0));
        assert_eq!(gray_to_rgb(128), (128, 128, 128));
        assert_eq!(gray_to_rgb(255), (255, 255, 255));
    }

    #[test]
    fn lab_black() {
        let (r, g, b) = lab_to_rgb(0.0, 0.0, 0.0);
        assert!(close(r, 0, 2), "r={}", r);
        assert!(close(g, 0, 2), "g={}", g);
        assert!(close(b, 0, 2), "b={}", b);
    }

    #[test]
    fn lab_white() {
        let (r, g, b) = lab_to_rgb(100.0, 0.0, 0.0);
        assert!(close(r, 255, 2), "r={}", r);
        assert!(close(g, 255, 2), "g={}", g);
        assert!(close(b, 255, 2), "b={}", b);
    }

    #[test]
    fn lab_vivid_red() {
        // L*=50, a*=80, b*=67 is the commonly-cited vivid-red Lab reference.
        // Pure D50 Lab -> Bradford-adapted sRGB math lands around (240, 0, 0);
        // ICC CMM implementations return a lower value (~220) because they
        // apply gamut compression. This helper is non-ICC, so the
        // mathematical answer is correct. Tolerance +/-10 per channel.
        let (r, g, b) = lab_to_rgb(50.0, 80.0, 67.0);
        assert!(close(r, 240, 10), "r={}", r);
        assert!(close(g, 0, 10), "g={}", g);
        assert!(close(b, 0, 10), "b={}", b);
    }

    #[test]
    fn cmyk_image_3x3_cyan() {
        // 3x3 pure-cyan CMYK image -> 3x3 cyan RGB.
        let mut data = Vec::with_capacity(9 * 4);
        for _ in 0..9 {
            data.extend_from_slice(&[255, 0, 0, 0]);
        }
        let rgb = cmyk_image_to_rgb(&data, 3, 3);
        assert_eq!(rgb.len(), 27);
        for px in rgb.chunks_exact(3) {
            assert_eq!(px, &[0, 255, 255]);
        }
    }

    #[test]
    fn gray_image_3x3() {
        let data = [0u8, 64, 128, 192, 255, 10, 20, 30, 40];
        let rgb = gray_image_to_rgb(&data);
        assert_eq!(rgb.len(), 27);
        for (i, px) in rgb.chunks_exact(3).enumerate() {
            assert_eq!(px, &[data[i], data[i], data[i]]);
        }
    }

    #[test]
    fn lab_image_3x3_white() {
        // 3x3 pixels of L=255 (=> L*=100), a=128 (=>0), b=128 (=>0) -> white.
        let mut data = Vec::with_capacity(9 * 3);
        for _ in 0..9 {
            data.extend_from_slice(&[255, 128, 128]);
        }
        let rgb = lab_image_to_rgb(&data, 3, 3);
        assert_eq!(rgb.len(), 27);
        for px in rgb.chunks_exact(3) {
            assert!(close(px[0], 255, 2));
            assert!(close(px[1], 255, 2));
            assert!(close(px[2], 255, 2));
        }
    }

    #[test]
    fn cmyk_image_truncated_input() {
        // 2 full pixels + 2 extra bytes. Extras should be ignored.
        let data = [255u8, 0, 0, 0, 0, 255, 0, 0, 1, 2];
        let rgb = cmyk_image_to_rgb(&data, 2, 1);
        assert_eq!(rgb.len(), 6);
        assert_eq!(&rgb[0..3], &[0, 255, 255]);
        assert_eq!(&rgb[3..6], &[255, 0, 255]);
    }
}

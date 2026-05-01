//! Shading-pattern dict parser (ISO 32000-2 §8.7.4) used by the `sh`
//! operator.
//!
//! Two shading types are fully materialized into renderable IR:
//!
//! * **Type 2 (axial):** `[/ShadingType 2 /Coords [x0 y0 x1 y1]
//!   /Function f]`. Gradient axis from `P0 -> P1` in shading user
//!   space; `t = 0` at `P0`, `t = 1` at `P1`.
//! * **Type 3 (radial):** `[/ShadingType 3 /Coords [x0 y0 r0 x1 y1 r1]
//!   /Function f]`. Circle-to-circle interpolation.
//!
//! Color functions are evaluated into a 256-entry LUT at parse time so
//! the renderer does zero function work per pixel. Supported function
//! types:
//!
//! * **Type 2 (exponential):** `C(t) = C0 + t^N * (C1 - C0)` directly.
//! * **Type 3 (stitching):** a sequence of type-2 subfunctions over
//!   `Bounds` and `Encode`. The 3-stop gradient case (red -> white ->
//!   blue) is stitching over two type-2 legs.
//!
//! Type 0 (sampled) and Type 4 (PostScript) functions are diagnosed as
//! `WarningKind::UnsupportedFeature` and the shading is recorded as
//! `PageShadingKind::Unsupported` so the renderer skips it instead of
//! crashing.
//!
//! Input-color-space conversion is best-effort: DeviceRGB / DeviceGray
//! map byte-exact; DeviceCMYK uses naive `1 - (C + K)` per channel;
//! anything else is emitted at sRGB 0.

use crate::content::path::{PageShadingKind, Point, ShadingLut};
use crate::diagnostics::{DiagnosticsSink, Warning, WarningKind};
use crate::object::resolver::ObjectResolver;
use crate::object::{PdfDictionary, PdfObject};

/// Number of entries in the sampled LUT. 256 is tight enough for
/// 8-bit output and large enough to avoid banding on smooth gradients.
const LUT_N: usize = 256;

/// Decode a shading-dict value (which may be an indirect reference or
/// an inline dict/stream) into a [`PageShadingKind`] by resolving the
/// dict and sampling its /Function into a 256-entry sRGB LUT.
///
/// Emits diagnostics via `sink`. Never panics on malformed input:
/// missing / garbled fields short-circuit to
/// [`PageShadingKind::Unsupported`] so the caller can fall through to
/// the base fill color.
pub(crate) fn parse_shading(
    value: &PdfObject,
    resolver: &mut ObjectResolver<'_>,
    sink: &dyn DiagnosticsSink,
) -> PageShadingKind {
    let owned_dict: Option<PdfDictionary> = match value {
        PdfObject::Reference(r) => match resolver.resolve(*r) {
            Ok(PdfObject::Dictionary(d)) => Some(d),
            Ok(PdfObject::Stream(s)) => Some(s.dict),
            _ => None,
        },
        _ => None,
    };
    let dict: Option<&PdfDictionary> = match value {
        PdfObject::Dictionary(d) => Some(d),
        PdfObject::Stream(s) => Some(&s.dict),
        PdfObject::Reference(_) => owned_dict.as_ref(),
        _ => None,
    };

    let Some(dict) = dict else {
        sink.warning(Warning::new(
            None,
            WarningKind::InvalidState,
            "sh: shading resource is not a dict/stream, skipping",
        ));
        return PageShadingKind::Unsupported { shading_type: 0 };
    };

    let stype = dict.get_i64(b"ShadingType").unwrap_or(-1);
    match stype {
        2 => parse_axial(dict, resolver, sink),
        3 => parse_radial(dict, resolver, sink),
        1 | 4 | 5 | 6 | 7 => {
            sink.warning(Warning::new(
                None,
                WarningKind::UnsupportedShadingType,
                format!("sh: unsupported /ShadingType {stype}, falling through to base fill"),
            ));
            PageShadingKind::Unsupported {
                shading_type: stype as u32,
            }
        }
        other => {
            sink.warning(Warning::new(
                None,
                WarningKind::InvalidState,
                format!("sh: invalid /ShadingType {other}"),
            ));
            PageShadingKind::Unsupported { shading_type: 0 }
        }
    }
}

fn parse_axial(
    dict: &PdfDictionary,
    resolver: &mut ObjectResolver<'_>,
    sink: &dyn DiagnosticsSink,
) -> PageShadingKind {
    let Some(coords) = parse_numbers(dict.get(b"Coords"), resolver) else {
        sink.warning(Warning::new(
            None,
            WarningKind::InvalidState,
            "sh: Type 2 shading missing /Coords array",
        ));
        return PageShadingKind::Unsupported { shading_type: 2 };
    };
    if coords.len() < 4 {
        sink.warning(Warning::new(
            None,
            WarningKind::InvalidState,
            format!(
                "sh: Type 2 /Coords has {} elements, expected 4 [x0 y0 x1 y1]",
                coords.len()
            ),
        ));
        return PageShadingKind::Unsupported { shading_type: 2 };
    }
    let domain = parse_numbers(dict.get(b"Domain"), resolver)
        .filter(|v| v.len() >= 2)
        .unwrap_or_else(|| vec![0.0, 1.0]);
    let (extend_start, extend_end) = parse_extend(dict.get(b"Extend"), resolver);

    let n_components = infer_n_components(dict);
    let lut = match build_lut(dict.get(b"Function"), resolver, sink, &domain, n_components) {
        Some(lut) => lut,
        None => return PageShadingKind::Unsupported { shading_type: 2 },
    };

    PageShadingKind::Axial {
        p0: Point::new(coords[0], coords[1]),
        p1: Point::new(coords[2], coords[3]),
        lut,
        extend_start,
        extend_end,
    }
}

fn parse_radial(
    dict: &PdfDictionary,
    resolver: &mut ObjectResolver<'_>,
    sink: &dyn DiagnosticsSink,
) -> PageShadingKind {
    let Some(coords) = parse_numbers(dict.get(b"Coords"), resolver) else {
        sink.warning(Warning::new(
            None,
            WarningKind::InvalidState,
            "sh: Type 3 shading missing /Coords array",
        ));
        return PageShadingKind::Unsupported { shading_type: 3 };
    };
    if coords.len() < 6 {
        sink.warning(Warning::new(
            None,
            WarningKind::InvalidState,
            format!(
                "sh: Type 3 /Coords has {} elements, expected 6 [x0 y0 r0 x1 y1 r1]",
                coords.len()
            ),
        ));
        return PageShadingKind::Unsupported { shading_type: 3 };
    }
    let domain = parse_numbers(dict.get(b"Domain"), resolver)
        .filter(|v| v.len() >= 2)
        .unwrap_or_else(|| vec![0.0, 1.0]);
    let (extend_start, extend_end) = parse_extend(dict.get(b"Extend"), resolver);

    let n_components = infer_n_components(dict);
    let lut = match build_lut(dict.get(b"Function"), resolver, sink, &domain, n_components) {
        Some(lut) => lut,
        None => return PageShadingKind::Unsupported { shading_type: 3 },
    };

    PageShadingKind::Radial {
        c0: Point::new(coords[0], coords[1]),
        r0: coords[2].max(0.0),
        c1: Point::new(coords[3], coords[4]),
        r1: coords[5].max(0.0),
        lut,
        extend_start,
        extend_end,
    }
}

/// Infer how many color components the shading's /Function returns.
/// Prefer `/ColorSpace` in the shading dict; fall back to 3 (DeviceRGB)
/// which is the common case.
fn infer_n_components(dict: &PdfDictionary) -> u8 {
    match dict.get(b"ColorSpace") {
        Some(PdfObject::Name(name)) => name_to_components(name),
        Some(PdfObject::Array(a)) => a
            .first()
            .and_then(|o| match o {
                PdfObject::Name(n) => Some(name_to_components(n)),
                _ => None,
            })
            .unwrap_or(3),
        _ => 3,
    }
}

fn name_to_components(name: &[u8]) -> u8 {
    match name {
        b"DeviceGray" | b"G" | b"CalGray" => 1,
        b"DeviceRGB" | b"RGB" | b"CalRGB" => 3,
        b"DeviceCMYK" | b"CMYK" => 4,
        b"Lab" => 3,
        _ => 3,
    }
}

/// Convert N color-function outputs to an opaque sRGB triple. Handles
/// the device colorspaces that cover >95% of real shadings.
fn components_to_rgb(c: &[f64]) -> [u8; 3] {
    let clamp01 = |v: f64| v.clamp(0.0, 1.0);
    match c.len() {
        0 => [0, 0, 0],
        1 => {
            let g = (clamp01(c[0]) * 255.0).round() as u8;
            [g, g, g]
        }
        3 => [
            (clamp01(c[0]) * 255.0).round() as u8,
            (clamp01(c[1]) * 255.0).round() as u8,
            (clamp01(c[2]) * 255.0).round() as u8,
        ],
        4 => {
            // Naive DeviceCMYK -> sRGB: r = (1 - C) * (1 - K) etc.
            // Good enough for viewer-grade; ICC comes later.
            let (cc, mm, yy, kk) = (clamp01(c[0]), clamp01(c[1]), clamp01(c[2]), clamp01(c[3]));
            let r = ((1.0 - cc) * (1.0 - kk) * 255.0).round() as u8;
            let g = ((1.0 - mm) * (1.0 - kk) * 255.0).round() as u8;
            let b = ((1.0 - yy) * (1.0 - kk) * 255.0).round() as u8;
            [r, g, b]
        }
        _ => [
            (clamp01(c[0]) * 255.0).round() as u8,
            (clamp01(c.get(1).copied().unwrap_or(0.0)) * 255.0).round() as u8,
            (clamp01(c.get(2).copied().unwrap_or(0.0)) * 255.0).round() as u8,
        ],
    }
}

fn parse_extend(value: Option<&PdfObject>, resolver: &mut ObjectResolver<'_>) -> (bool, bool) {
    let arr = match value {
        Some(PdfObject::Array(a)) => a.clone(),
        Some(PdfObject::Reference(r)) => match resolver.resolve(*r) {
            Ok(PdfObject::Array(a)) => a,
            _ => return (false, false),
        },
        _ => return (false, false),
    };
    let b0 = arr.first().and_then(|o| o.as_bool()).unwrap_or(false);
    let b1 = arr.get(1).and_then(|o| o.as_bool()).unwrap_or(false);
    (b0, b1)
}

fn parse_numbers(value: Option<&PdfObject>, resolver: &mut ObjectResolver<'_>) -> Option<Vec<f64>> {
    let arr = match value? {
        PdfObject::Array(a) => a.clone(),
        PdfObject::Reference(r) => match resolver.resolve(*r).ok()? {
            PdfObject::Array(a) => a,
            _ => return None,
        },
        _ => return None,
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in &arr {
        match item {
            PdfObject::Integer(i) => out.push(*i as f64),
            PdfObject::Real(v) => out.push(*v),
            PdfObject::Reference(r) => match resolver.resolve(*r).ok()? {
                PdfObject::Integer(i) => out.push(i as f64),
                PdfObject::Real(v) => out.push(v),
                _ => return None,
            },
            _ => return None,
        }
    }
    Some(out)
}

/// Sample a PDF /Function into a 256-entry sRGB LUT over
/// `[domain[0], domain[1]]`. Supports function types 2 and 3.
fn build_lut(
    func: Option<&PdfObject>,
    resolver: &mut ObjectResolver<'_>,
    sink: &dyn DiagnosticsSink,
    domain: &[f64],
    n_components: u8,
) -> Option<ShadingLut> {
    let (t0, t1) = (domain[0], domain[1]);

    let func_obj = func?;
    let owned: Option<PdfObject> = match func_obj {
        PdfObject::Reference(r) => Some(resolver.resolve(*r).ok()?),
        _ => None,
    };
    let fobj: &PdfObject = match func_obj {
        PdfObject::Reference(_) => owned.as_ref()?,
        other => other,
    };
    let dict: &PdfDictionary = match fobj {
        PdfObject::Dictionary(d) => d,
        PdfObject::Stream(s) => &s.dict,
        _ => return None,
    };

    let ftype = dict.get_i64(b"FunctionType").unwrap_or(-1);
    let mut samples = Vec::with_capacity(LUT_N);
    match ftype {
        2 => {
            let n = dict.get_f64(b"N").unwrap_or(1.0);
            let c0 = parse_numbers(dict.get(b"C0"), resolver).unwrap_or_else(|| vec![0.0]);
            let c1 = parse_numbers(dict.get(b"C1"), resolver).unwrap_or_else(|| vec![1.0]);
            for i in 0..LUT_N {
                let u = i as f64 / (LUT_N - 1) as f64;
                let t = t0 + u * (t1 - t0);
                let samp = sample_type2(t, n, &c0, &c1, n_components as usize);
                samples.push(components_to_rgb(&samp));
            }
        }
        3 => {
            // Stitching function. Parse sub-functions + bounds + encode.
            let subs: Vec<PdfObject> = match dict.get(b"Functions") {
                Some(PdfObject::Array(a)) => a.clone(),
                Some(PdfObject::Reference(r)) => match resolver.resolve(*r).ok()? {
                    PdfObject::Array(a) => a,
                    _ => {
                        sink.warning(Warning::new(
                            None,
                            WarningKind::InvalidState,
                            "sh: Type 3 stitching /Functions is not an array",
                        ));
                        return None;
                    }
                },
                _ => {
                    sink.warning(Warning::new(
                        None,
                        WarningKind::InvalidState,
                        "sh: Type 3 stitching missing /Functions",
                    ));
                    return None;
                }
            };
            let bounds = parse_numbers(dict.get(b"Bounds"), resolver).unwrap_or_default();
            let encode = parse_numbers(dict.get(b"Encode"), resolver).unwrap_or_default();
            let mut subtables: Vec<Type2Func> = Vec::with_capacity(subs.len());
            for sub in &subs {
                match parse_type2_fn(sub, resolver) {
                    Some(f) => subtables.push(f),
                    None => {
                        sink.warning(Warning::new(
                            None,
                            WarningKind::UnsupportedFeature,
                            "sh: Type 3 stitching sub-function is not Type 2, skipping shading",
                        ));
                        return None;
                    }
                }
            }
            if subtables.is_empty() {
                return None;
            }
            for i in 0..LUT_N {
                let u = i as f64 / (LUT_N - 1) as f64;
                let t = t0 + u * (t1 - t0);
                let samp = sample_stitching(
                    t,
                    t0,
                    t1,
                    &bounds,
                    &encode,
                    &subtables,
                    n_components as usize,
                );
                samples.push(components_to_rgb(&samp));
            }
        }
        0 | 4 => {
            sink.warning(Warning::new(
                None,
                WarningKind::UnsupportedFeature,
                format!("sh: /Function FunctionType {ftype} not supported, skipping shading"),
            ));
            return None;
        }
        _ => {
            sink.warning(Warning::new(
                None,
                WarningKind::InvalidState,
                format!("sh: invalid /FunctionType {ftype}"),
            ));
            return None;
        }
    }
    Some(ShadingLut { samples })
}

/// Parsed coefficients of a type-2 exponential /Function.
struct Type2Func {
    n: f64,
    c0: Vec<f64>,
    c1: Vec<f64>,
    domain: [f64; 2],
}

fn parse_type2_fn(obj: &PdfObject, resolver: &mut ObjectResolver<'_>) -> Option<Type2Func> {
    let owned: Option<PdfObject> = match obj {
        PdfObject::Reference(r) => Some(resolver.resolve(*r).ok()?),
        _ => None,
    };
    let dobj: &PdfObject = match obj {
        PdfObject::Reference(_) => owned.as_ref()?,
        other => other,
    };
    let dict = match dobj {
        PdfObject::Dictionary(d) => d,
        PdfObject::Stream(s) => &s.dict,
        _ => return None,
    };
    if dict.get_i64(b"FunctionType").unwrap_or(-1) != 2 {
        return None;
    }
    let n = dict.get_f64(b"N").unwrap_or(1.0);
    let c0 = parse_numbers(dict.get(b"C0"), resolver).unwrap_or_else(|| vec![0.0]);
    let c1 = parse_numbers(dict.get(b"C1"), resolver).unwrap_or_else(|| vec![1.0]);
    let dom = parse_numbers(dict.get(b"Domain"), resolver)
        .filter(|v| v.len() >= 2)
        .unwrap_or_else(|| vec![0.0, 1.0]);
    Some(Type2Func {
        n,
        c0,
        c1,
        domain: [dom[0], dom[1]],
    })
}

fn sample_type2(t: f64, n: f64, c0: &[f64], c1: &[f64], n_out: usize) -> Vec<f64> {
    let pow = if t < 0.0 { 0.0 } else { t.powf(n) };
    let mut out = Vec::with_capacity(n_out);
    for i in 0..n_out {
        let a = c0.get(i).copied().unwrap_or(0.0);
        let b = c1.get(i).copied().unwrap_or(1.0);
        out.push(a + pow * (b - a));
    }
    out
}

fn sample_stitching(
    t: f64,
    dom_lo: f64,
    dom_hi: f64,
    bounds: &[f64],
    encode: &[f64],
    subs: &[Type2Func],
    n_out: usize,
) -> Vec<f64> {
    let t_clamped = t.clamp(dom_lo, dom_hi);
    let mut idx = 0usize;
    let mut lo = dom_lo;
    let mut found = false;
    for (i, &b) in bounds.iter().enumerate() {
        if t_clamped < b {
            idx = i;
            found = true;
            break;
        }
        lo = b;
    }
    if !found {
        idx = bounds.len();
    }
    let hi = if idx < bounds.len() {
        bounds[idx]
    } else {
        dom_hi
    };
    let sub = match subs.get(idx) {
        Some(s) => s,
        None => return vec![0.0; n_out],
    };
    let e0 = encode.get(idx * 2).copied().unwrap_or(sub.domain[0]);
    let e1 = encode.get(idx * 2 + 1).copied().unwrap_or(sub.domain[1]);
    let t_remap = if (hi - lo).abs() < 1e-12 {
        e0
    } else {
        e0 + (t_clamped - lo) * (e1 - e0) / (hi - lo)
    };
    sample_type2(t_remap, sub.n, &sub.c0, &sub.c1, n_out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::CollectingDiagnostics;
    use crate::object::resolver::ObjectResolver;
    use crate::parse::XrefTable;

    fn mk_resolver() -> (&'static [u8], XrefTable) {
        (b"%PDF-1.4\n" as &[u8], XrefTable::new())
    }

    #[test]
    fn axial_red_to_blue_samples() {
        let mut sh = PdfDictionary::new();
        sh.insert(b"ShadingType".to_vec(), PdfObject::Integer(2));
        sh.insert(
            b"Coords".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Integer(0),
                PdfObject::Integer(0),
                PdfObject::Integer(100),
                PdfObject::Integer(0),
            ]),
        );
        sh.insert(
            b"ColorSpace".to_vec(),
            PdfObject::Name(b"DeviceRGB".to_vec()),
        );
        let mut f = PdfDictionary::new();
        f.insert(b"FunctionType".to_vec(), PdfObject::Integer(2));
        f.insert(b"N".to_vec(), PdfObject::Integer(1));
        f.insert(
            b"C0".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Integer(1),
                PdfObject::Integer(0),
                PdfObject::Integer(0),
            ]),
        );
        f.insert(
            b"C1".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Integer(0),
                PdfObject::Integer(0),
                PdfObject::Integer(1),
            ]),
        );
        sh.insert(b"Function".to_vec(), PdfObject::Dictionary(f));

        let (data, xref) = mk_resolver();
        let mut resolver = ObjectResolver::new(data, xref);
        let sink = CollectingDiagnostics::new();
        let kind = parse_shading(&PdfObject::Dictionary(sh), &mut resolver, &sink);
        match kind {
            PageShadingKind::Axial { p0, p1, lut, .. } => {
                assert_eq!(p0.x, 0.0);
                assert_eq!(p1.x, 100.0);
                assert_eq!(lut.sample(0.0), [255, 0, 0]);
                assert_eq!(lut.sample(1.0), [0, 0, 255]);
                let sm = lut.sample(0.5);
                assert!(sm[0] > 100 && sm[0] < 160, "mid r={}", sm[0]);
                assert!(sm[2] > 100 && sm[2] < 160, "mid b={}", sm[2]);
            }
            _ => panic!("expected Axial shading, got {:?}", kind),
        }
    }

    #[test]
    fn unsupported_type_1_logs_and_falls_through() {
        let mut sh = PdfDictionary::new();
        sh.insert(b"ShadingType".to_vec(), PdfObject::Integer(1));
        let (data, xref) = mk_resolver();
        let mut resolver = ObjectResolver::new(data, xref);
        let sink = CollectingDiagnostics::new();
        let kind = parse_shading(&PdfObject::Dictionary(sh), &mut resolver, &sink);
        match kind {
            PageShadingKind::Unsupported { shading_type } => assert_eq!(shading_type, 1),
            _ => panic!("expected Unsupported"),
        }
        let warnings = sink.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| matches!(w.kind, WarningKind::UnsupportedShadingType)),
            "should emit UnsupportedShadingType warning"
        );
    }

    #[test]
    fn radial_stitching_3stop_gradient() {
        let mut sh = PdfDictionary::new();
        sh.insert(b"ShadingType".to_vec(), PdfObject::Integer(3));
        sh.insert(
            b"Coords".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Integer(50),
                PdfObject::Integer(50),
                PdfObject::Integer(0),
                PdfObject::Integer(50),
                PdfObject::Integer(50),
                PdfObject::Integer(50),
            ]),
        );
        sh.insert(
            b"ColorSpace".to_vec(),
            PdfObject::Name(b"DeviceRGB".to_vec()),
        );
        let mk_sub = |c0: [i64; 3], c1: [i64; 3]| {
            let mut f = PdfDictionary::new();
            f.insert(b"FunctionType".to_vec(), PdfObject::Integer(2));
            f.insert(b"N".to_vec(), PdfObject::Integer(1));
            f.insert(
                b"Domain".to_vec(),
                PdfObject::Array(vec![PdfObject::Integer(0), PdfObject::Integer(1)]),
            );
            f.insert(
                b"C0".to_vec(),
                PdfObject::Array(c0.iter().map(|v| PdfObject::Integer(*v)).collect()),
            );
            f.insert(
                b"C1".to_vec(),
                PdfObject::Array(c1.iter().map(|v| PdfObject::Integer(*v)).collect()),
            );
            PdfObject::Dictionary(f)
        };
        let mut stitch = PdfDictionary::new();
        stitch.insert(b"FunctionType".to_vec(), PdfObject::Integer(3));
        stitch.insert(
            b"Domain".to_vec(),
            PdfObject::Array(vec![PdfObject::Integer(0), PdfObject::Integer(1)]),
        );
        stitch.insert(
            b"Functions".to_vec(),
            PdfObject::Array(vec![
                mk_sub([1, 0, 0], [1, 1, 1]),
                mk_sub([1, 1, 1], [0, 0, 1]),
            ]),
        );
        stitch.insert(
            b"Bounds".to_vec(),
            PdfObject::Array(vec![PdfObject::Real(0.5)]),
        );
        stitch.insert(
            b"Encode".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Integer(0),
                PdfObject::Integer(1),
                PdfObject::Integer(0),
                PdfObject::Integer(1),
            ]),
        );
        sh.insert(b"Function".to_vec(), PdfObject::Dictionary(stitch));

        let (data, xref) = mk_resolver();
        let mut resolver = ObjectResolver::new(data, xref);
        let sink = CollectingDiagnostics::new();
        let kind = parse_shading(&PdfObject::Dictionary(sh), &mut resolver, &sink);
        match kind {
            PageShadingKind::Radial { r0, r1, lut, .. } => {
                assert_eq!(r0, 0.0);
                assert_eq!(r1, 50.0);
                assert_eq!(lut.sample(0.0), [255, 0, 0]);
                let mid = lut.sample(0.5);
                assert!(
                    mid[0] > 240 && mid[1] > 240 && mid[2] > 240,
                    "mid={:?}",
                    mid
                );
                assert_eq!(lut.sample(1.0), [0, 0, 255]);
            }
            _ => panic!("expected Radial, got {:?}", kind),
        }
    }
}

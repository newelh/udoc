//! Colorspace classification for the content-stream interpreter.
//!
//! This module is intentionally narrow: it names the high-level shape
//! of a colorspace enough for the interpreter's `cs` / `CS` / `scn` /
//! `SCN` operators to decide *how many operands to pop* and *whether
//! the final operand is a Pattern resource name*. The existing
//! interpreter tracks only a `fill_cs_components: u8` and a raw RGB
//! color; everything else is baked into the `rg`/`RG`/`g`/`G`/`k`/`K`
//! ops. Pattern colorspace needs a richer tag because `scn` with a
//! Pattern CS takes a pattern name (`/P1`) as the final operand rather
//! than a numeric color tuple.
//!
//! we only materialize what is
//! needed to feed [`crate::pattern::parse_tiling_pattern`]. Full
//! colorspace modelling (ICCBased, CalRGB, Separation, DeviceN, Lab,
//! Indexed) is out of scope; those colorspaces still round-trip
//! through the legacy u8-component path.
//!
//! ISO 32000-2 §8.6.

use crate::object::resolver::ObjectResolver;
use crate::object::PdfObject;

/// The Pattern colorspace as it appears on a `cs`/`CS` stack.
///
/// PDF allows `/Pattern` directly (uncoloured / Type 2 shading form)
/// or `[/Pattern <base-cs>]` (uncoloured pattern whose cell needs a
/// base colorspace). We don't distinguish those at this layer; the
/// [`crate::pattern::parse_tiling_pattern`] downstream decides based
/// on `/PaintType` whether the pattern resource is renderable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternColorspace {
    /// True when the colorspace was `[/Pattern <base>]` (uncoloured)
    /// rather than a bare `/Pattern` (coloured / shading). The
    /// interpreter uses this to predict how many numeric operands
    /// precede the pattern-resource name on an `scn` call.
    pub has_base: bool,
}

/// Classify a resolved colorspace object as Pattern or not.
///
/// Returns:
/// * `Some(PatternColorspace { has_base: false })` for `/Pattern`
/// * `Some(PatternColorspace { has_base: true })` for `[/Pattern ...]`
/// * `None` for everything else (device/cal/ICC/indexed/etc.)
///
/// The caller owns numeric-component counting for non-Pattern cases
/// via the existing [`Colorspace::components`] path.
pub fn classify_pattern_colorspace(
    obj: &PdfObject,
    resolver: &mut ObjectResolver<'_>,
) -> Option<PatternColorspace> {
    // Resolve an indirect reference one step.
    let owned;
    let obj = match obj {
        PdfObject::Reference(r) => {
            owned = resolver.resolve(*r).ok()?;
            &owned
        }
        other => other,
    };
    match obj {
        PdfObject::Name(n) if n.as_slice() == b"Pattern" => {
            Some(PatternColorspace { has_base: false })
        }
        PdfObject::Array(a) => {
            let first = a.first()?.as_name()?;
            if first == b"Pattern" {
                Some(PatternColorspace {
                    has_base: a.len() >= 2,
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Convenience enum returned by [`Colorspace::classify`]. Carries
/// enough information for the interpreter's operand-popping logic
/// without leaking every PDF colorspace variant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Colorspace {
    /// DeviceGray, CalGray, or an Indexed/ICCBased with `/N 1`.
    DeviceGray,
    /// DeviceRGB, CalRGB, or an ICCBased with `/N 3`.
    DeviceRgb,
    /// DeviceCMYK or an ICCBased with `/N 4`.
    DeviceCmyk,
    /// `/Pattern` or `[/Pattern base.]`. See [`PatternColorspace`].
    Pattern(PatternColorspace),
    /// Colorspace resolves to a numeric component count but we don't
    /// name the specific family (Indexed, Separation, DeviceN, Lab).
    Other {
        /// Number of components each `scn` call expects before the
        /// pattern name (if any). Zero for Pattern-only CSes.
        components: u8,
    },
}

impl Colorspace {
    /// How many numeric operands `scn` / `SCN` consumes *before* the
    /// optional pattern-resource name. `1` for gray, `3` for RGB,
    /// `4` for CMYK, `0` for bare `/Pattern`, and the base CS's
    /// count for uncoloured patterns.
    pub fn components(&self) -> u8 {
        match self {
            Colorspace::DeviceGray => 1,
            Colorspace::DeviceRgb => 3,
            Colorspace::DeviceCmyk => 4,
            Colorspace::Pattern(PatternColorspace { has_base: false }) => 0,
            // Uncoloured patterns: callers infer the base component
            // count from the second array element. We don't try to
            // recurse here; the interpreter's existing
            // `resolve_color_space_components` does that.
            Colorspace::Pattern(PatternColorspace { has_base: true }) => 0,
            Colorspace::Other { components } => *components,
        }
    }

    /// Returns `Some(PatternColorspace)` if this is a Pattern CS.
    pub fn as_pattern(&self) -> Option<&PatternColorspace> {
        match self {
            Colorspace::Pattern(p) => Some(p),
            _ => None,
        }
    }

    /// Classify a resolved colorspace object into one of the five
    /// variants above. Returns `None` when `obj` isn't recognizable
    /// as a colorspace value (shouldn't happen in a well-formed PDF;
    /// the interpreter treats that as an unknown CS with zero
    /// components).
    pub fn classify(obj: &PdfObject, resolver: &mut ObjectResolver<'_>) -> Option<Self> {
        // Resolve an indirect reference one step.
        let owned;
        let obj = match obj {
            PdfObject::Reference(r) => {
                owned = resolver.resolve(*r).ok()?;
                &owned
            }
            other => other,
        };
        // Bare name form: /DeviceRGB, /DeviceGray, /DeviceCMYK, /Pattern.
        if let Some(n) = obj.as_name() {
            return Some(match n {
                b"DeviceGray" | b"G" => Colorspace::DeviceGray,
                b"DeviceRGB" | b"RGB" => Colorspace::DeviceRgb,
                b"DeviceCMYK" | b"CMYK" => Colorspace::DeviceCmyk,
                b"Pattern" => Colorspace::Pattern(PatternColorspace { has_base: false }),
                _ => Colorspace::Other { components: 0 },
            });
        }
        // Array form: [/Pattern ...], [/ICCBased stream], etc.
        if let Some(arr) = obj.as_array() {
            let first = arr.first().and_then(|o| o.as_name())?;
            return Some(match first {
                b"DeviceGray" | b"G" | b"CalGray" => Colorspace::DeviceGray,
                b"DeviceRGB" | b"RGB" | b"CalRGB" => Colorspace::DeviceRgb,
                b"DeviceCMYK" | b"CMYK" => Colorspace::DeviceCmyk,
                b"Pattern" => Colorspace::Pattern(PatternColorspace {
                    has_base: arr.len() >= 2,
                }),
                b"ICCBased" => {
                    let n = arr
                        .get(1)
                        .and_then(|o| o.as_reference())
                        .and_then(|r| resolver.resolve(r).ok())
                        .and_then(|resolved| match resolved {
                            PdfObject::Stream(s) => s.dict.get_i64(b"N").map(|n| n as u8),
                            _ => None,
                        })
                        .unwrap_or(0);
                    match n {
                        1 => Colorspace::DeviceGray,
                        3 => Colorspace::DeviceRgb,
                        4 => Colorspace::DeviceCmyk,
                        other => Colorspace::Other { components: other },
                    }
                }
                _ => Colorspace::Other { components: 0 },
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::PdfObject;
    use crate::parse::document_parser::XrefTable;

    fn name(b: &[u8]) -> PdfObject {
        PdfObject::Name(b.to_vec())
    }

    fn empty_resolver() -> ObjectResolver<'static> {
        ObjectResolver::new(&[], XrefTable::new())
    }

    #[test]
    fn classify_bare_pattern() {
        let mut r = empty_resolver();
        let out = Colorspace::classify(&name(b"Pattern"), &mut r);
        assert_eq!(
            out,
            Some(Colorspace::Pattern(PatternColorspace { has_base: false }))
        );
        assert_eq!(out.as_ref().unwrap().components(), 0);
    }

    #[test]
    fn classify_array_pattern_with_base() {
        let mut r = empty_resolver();
        let arr = PdfObject::Array(vec![name(b"Pattern"), name(b"DeviceRGB")]);
        let out = Colorspace::classify(&arr, &mut r);
        assert_eq!(
            out,
            Some(Colorspace::Pattern(PatternColorspace { has_base: true }))
        );
        assert!(out.as_ref().unwrap().as_pattern().unwrap().has_base);
    }

    #[test]
    fn classify_device_families() {
        let mut r = empty_resolver();
        assert_eq!(
            Colorspace::classify(&name(b"DeviceGray"), &mut r),
            Some(Colorspace::DeviceGray)
        );
        assert_eq!(
            Colorspace::classify(&name(b"DeviceRGB"), &mut r),
            Some(Colorspace::DeviceRgb)
        );
        assert_eq!(
            Colorspace::classify(&name(b"DeviceCMYK"), &mut r),
            Some(Colorspace::DeviceCmyk)
        );
        assert_eq!(Colorspace::DeviceGray.components(), 1);
        assert_eq!(Colorspace::DeviceRgb.components(), 3);
        assert_eq!(Colorspace::DeviceCmyk.components(), 4);
    }

    #[test]
    fn classify_non_pattern_returns_none_for_as_pattern() {
        let mut r = empty_resolver();
        let cs = Colorspace::classify(&name(b"DeviceRGB"), &mut r).unwrap();
        assert!(cs.as_pattern().is_none());
    }

    #[test]
    fn classify_pattern_helper_matches_full_classify() {
        let mut r = empty_resolver();
        let p = classify_pattern_colorspace(&name(b"Pattern"), &mut r);
        assert_eq!(p, Some(PatternColorspace { has_base: false }));
        let np = classify_pattern_colorspace(&name(b"DeviceRGB"), &mut r);
        assert_eq!(np, None);
    }
}

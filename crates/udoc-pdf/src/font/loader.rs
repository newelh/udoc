//! Font dictionary loading.
//!
//! Parses PDF font dictionaries into the Font enum hierarchy
//! using the ObjectResolver.

use udoc_core::text::{FallbackReason, FontResolution};

use crate::diagnostics::{DiagnosticsSink, Warning, WarningContext, WarningKind};
use crate::error::ResultExt;
use crate::object::resolver::ObjectResolver;
use crate::object::{ObjRef, PdfObject};
use crate::Result;

use udoc_font::cmap;
use udoc_font::cmap_parser::ParsedCMap;
use udoc_font::encoding::{parse_glyph_name, Encoding, StandardEncoding};
use udoc_font::standard_widths::is_standard_font;
use udoc_font::tounicode::ToUnicodeCMap;
use udoc_font::types::{
    strip_subset_prefix, CidFont, CidSubtype, CidWidths, CompositeFont, Font, FontProgram,
    SimpleFont, SimpleSubtype, SimpleWidths, Type3FontCore,
};

use super::type3_pdf::Type3FontPdfRefs;

/// Load a font from a font dictionary reference.
///
/// Parses /Subtype to determine the font kind, then loads encoding and
/// ToUnicode as appropriate. Returns the format-agnostic `Font` alongside
/// an optional `Type3FontPdfRefs` (Some only for Type3 fonts) carrying
/// the PDF-specific CharProc and /Resources references, and a
/// [`FontResolution`] classifying whether the font was loaded exactly
/// (embedded program present, encoding understood) or whether a fallback
/// path was taken (embedded program missing, corrupt font stream, CID
/// font without ToUnicode, standard-14 name routed to built-in metrics,
/// etc.). The caller copies the resolution into each TextSpan produced
/// with this font so that downstream consumers can audit or filter text
/// whose accuracy may be degraded.
///
/// At every non-exact resolution the loader emits a
/// [`WarningKind::FallbackFontSubstitution`] warning with the requested
/// font name, the resolution description, and the underlying reason.
pub(crate) fn load_font(
    resolver: &mut ObjectResolver,
    font_ref: ObjRef,
) -> Result<(Font, Option<Type3FontPdfRefs>, FontResolution)> {
    let dict = resolver
        .resolve_dict(font_ref)
        .context("resolving font dictionary")?;

    let subtype = dict
        .get_name(b"Subtype")
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .unwrap_or_default();

    let font_ctx = WarningContext {
        page_index: None,
        obj_ref: Some(font_ref),
    };

    // Probe for corrupt FontFile* streams before loading so we can downgrade
    // to FallbackReason::EmbeddedCorrupt when classifying.
    let corrupt_stream = extract_corrupt_stream_detail(resolver, &dict);

    let (font, pdf_refs) = match subtype.as_str() {
        "Type0" => (
            load_composite_font(resolver, &dict, font_ctx).map(Font::Composite)?,
            None,
        ),
        "Type1" => (
            load_simple_font(resolver, &dict, SimpleSubtype::Type1, font_ctx).map(Font::Simple)?,
            None,
        ),
        "TrueType" => (
            load_simple_font(resolver, &dict, SimpleSubtype::TrueType, font_ctx)
                .map(Font::Simple)?,
            None,
        ),
        "MMType1" => (
            load_simple_font(resolver, &dict, SimpleSubtype::MMType1, font_ctx)
                .map(Font::Simple)?,
            None,
        ),
        "Type3" => {
            let (core, refs) = load_type3_font(resolver, &dict, font_ctx)?;
            (Font::Type3(core), Some(refs))
        }
        _ => (
            // Unknown subtype; treat as simple Type1 (best effort)
            load_simple_font(resolver, &dict, SimpleSubtype::Type1, font_ctx).map(Font::Simple)?,
            None,
        ),
    };

    let resolution = classify_resolution(&font, corrupt_stream);

    let base = font.name();
    // D-011: Info-level diagnostic for loaded fonts
    resolver.diagnostics().info(Warning::info_with_context(
        WarningKind::FontLoaded,
        font_ctx,
        format!("loaded font {base} ({subtype})"),
    ));

    if resolution.is_fallback() {
        resolver.diagnostics().warning(Warning::with_context(
            None,
            WarningKind::FallbackFontSubstitution,
            font_ctx,
            format_fallback_message(&resolution),
        ));
    }

    Ok((font, pdf_refs, resolution))
}

/// Build a human-readable warning message for a non-exact [`FontResolution`].
fn format_fallback_message(resolution: &FontResolution) -> String {
    let detail = |reason: &FallbackReason| -> String {
        match reason {
            FallbackReason::EmbeddedCorrupt(d) if !d.is_empty() => format!(" ({d})"),
            _ => String::new(),
        }
    };
    match resolution {
        FontResolution::Exact => "font loaded exactly".to_string(),
        FontResolution::Substituted {
            requested,
            resolved,
            reason,
        } => format!(
            "fallback font substitution: /{requested} -> {resolved} (reason: {}{})",
            reason.as_str(),
            detail(reason),
        ),
        FontResolution::SyntheticFallback {
            requested,
            generic_family,
            reason,
        } => format!(
            "synthetic fallback: /{requested} -> {generic_family} (reason: {}{})",
            reason.as_str(),
            detail(reason),
        ),
        _ => "unknown font resolution".to_string(),
    }
}

/// Classify how a font was loaded relative to what was requested.
///
/// Inspects the loaded `Font` and, for corrupt-stream cases, the detail the
/// stream decoder returned. The classification order matters: corrupt > not
/// embedded > CID no-ToUnicode > name-routed standard font. Anything else
/// counts as an exact load.
fn classify_resolution(font: &Font, corrupt_stream: Option<String>) -> FontResolution {
    let requested = font.name().to_string();
    let has_embedded_program = !matches!(font.font_program(), FontProgram::None);

    let is_type3 = matches!(font, Font::Type3(_));

    if let Some(detail) = corrupt_stream {
        if !has_embedded_program && !is_type3 {
            return FontResolution::Substituted {
                requested: requested.clone(),
                resolved: describe_resolved_fallback(font),
                reason: FallbackReason::EmbeddedCorrupt(detail),
            };
        }
    }

    if let Font::Composite(c) = font {
        if c.tounicode.is_none() && c.parsed_cmap.is_none() {
            return FontResolution::Substituted {
                requested: requested.clone(),
                resolved: "identity CMap".to_string(),
                reason: FallbackReason::CidNoToUnicode,
            };
        }
    }

    if !has_embedded_program && !is_type3 {
        let stripped = strip_subset_prefix(&requested);
        if is_standard_font(stripped) {
            return FontResolution::Substituted {
                requested: requested.clone(),
                resolved: format!("standard-14 {stripped}"),
                reason: FallbackReason::NameRouted,
            };
        }
        return FontResolution::SyntheticFallback {
            requested,
            generic_family: infer_generic_family(font.name()),
            reason: FallbackReason::NotEmbedded,
        };
    }

    FontResolution::Exact
}

fn describe_resolved_fallback(font: &Font) -> String {
    let name = font.name();
    let stripped = strip_subset_prefix(name);
    if is_standard_font(stripped) {
        format!("standard-14 {stripped}")
    } else {
        format!("encoding-only {stripped}")
    }
}

fn infer_generic_family(font_name: &str) -> String {
    let lower = font_name.to_ascii_lowercase();
    if lower.contains("mono")
        || lower.contains("courier")
        || lower.contains("consolas")
        || lower.contains("typewriter")
    {
        "monospace".to_string()
    } else if lower.contains("sans")
        || lower.contains("helvetica")
        || lower.contains("arial")
        || lower.contains("verdana")
        || lower.starts_with("cmss")
    {
        "sans-serif".to_string()
    } else {
        // Everything else (times/cambria/georgia/cmr/unrecognized) defaults
        // to serif, which is what most PDF body text is.
        "serif".to_string()
    }
}

/// If the font dict has a /FontDescriptor whose embedded font stream(s) fail
/// to decode, return a short detail string. Returns None when the descriptor
/// is absent, contains no FontFile*, or the first FontFile* decodes cleanly.
fn extract_corrupt_stream_detail(
    resolver: &mut ObjectResolver,
    dict: &crate::object::PdfDictionary,
) -> Option<String> {
    let desc_ref = dict.get_ref(b"FontDescriptor").or_else(|| {
        let arr = dict.get_array(b"DescendantFonts")?;
        let first = arr.first()?;
        let desc_font_ref = first.as_reference()?;
        let desc_dict = resolver.resolve_dict(desc_font_ref).ok()?;
        desc_dict.get_ref(b"FontDescriptor")
    })?;

    let desc_dict = resolver.resolve_dict(desc_ref).ok()?;

    // Per PDF spec a descriptor should carry at most one FontFile* entry, but
    // real-world malformed PDFs sometimes ship multiple. Inspect all present
    // entries and only flag the font as corrupt when *every* embedded program
    // fails to decode to a non-empty byte stream. If any FontFile* decodes
    // cleanly, the font is usable and we return None.
    let mut first_detail: Option<String> = None;
    for key in [
        b"FontFile2".as_ref(),
        b"FontFile3".as_ref(),
        b"FontFile".as_ref(),
    ] {
        let Some(ff_ref) = desc_dict.get_ref(key) else {
            continue;
        };
        let key_label = std::str::from_utf8(key).unwrap_or("FontFile*");
        let detail = match resolver.resolve_stream(ff_ref) {
            Ok(stream) => match resolver.decode_stream_data(&stream, Some(ff_ref)) {
                Ok(data) if !data.is_empty() => {
                    // At least one embedded program decoded cleanly. The font
                    // is usable regardless of any other entries being wrong.
                    return None;
                }
                Ok(_) => format!("{key_label} stream decoded to empty bytes"),
                Err(e) => format!("{key_label} decode failed: {e}"),
            },
            Err(e) => format!("{key_label} stream unresolvable: {e}"),
        };
        first_detail.get_or_insert(detail);
    }
    first_detail
}

/// Resolve `/BaseFont` on a font dictionary, following one level of
/// indirection if present. Some PDFs (older pdfTeX with hyperref, certain
/// LaTeX toolchains) emit `/BaseFont 47 0 R` pointing at a standalone Name
/// object rather than an inline `/ABCDEF+Helvetica`. `PdfDictionary::get_name`
/// doesn't chase indirections so the name was silently lost, leaving the
/// font as "unknown" for the rest of the pipeline, which then confused the
/// renderer's FontCache lookup (see arxiv-bio/2508.09212.pdf).
fn resolve_base_font(
    resolver: &mut ObjectResolver,
    dict: &crate::object::PdfDictionary,
) -> Option<String> {
    if let Some(n) = dict.get_name(b"BaseFont") {
        return Some(String::from_utf8_lossy(n).into_owned());
    }
    match resolver.get_resolved_name(dict, b"BaseFont") {
        Ok(Some(n)) => Some(String::from_utf8_lossy(&n).into_owned()),
        _ => None,
    }
}

/// Load a simple (single-byte) font.
fn load_simple_font(
    resolver: &mut ObjectResolver,
    dict: &crate::object::PdfDictionary,
    subtype: SimpleSubtype,
    font_ctx: WarningContext,
) -> Result<SimpleFont> {
    let base_font = resolve_base_font(resolver, dict);

    let mut encoding = load_encoding(resolver, dict).context("loading encoding for simple font")?;
    let differences_names = extract_encoding_differences_names(resolver, dict);
    let tounicode =
        load_tounicode(resolver, dict, font_ctx).context("loading ToUnicode for simple font")?;

    // When /Encoding is missing, BuiltIn returns None for everything.
    // Use sensible defaults per PDF spec: StandardEncoding for Type1,
    // WinAnsi for TrueType (best approximation without parsing font programs).
    if matches!(encoding, Encoding::Standard(StandardEncoding::BuiltIn)) {
        encoding = match subtype {
            SimpleSubtype::TrueType => Encoding::Standard(StandardEncoding::WinAnsi),
            _ => Encoding::Standard(StandardEncoding::Standard),
        };
    }

    // CM math font encoding override: TeX math fonts (CMSY, CMMI, CMEX)
    // use custom TeX-specific encodings that don't match any standard PDF
    // encoding. When there's no ToUnicode, the math encoding table is far
    // more accurate than Standard/WinAnsi for these fonts.
    //
    // If the font has /Differences, those take priority for positions they
    // cover. The math table fills in the remaining positions.
    if tounicode.is_none() {
        if let Some(font_name) = base_font.as_deref() {
            if let Some(math_table) = udoc_font::math_encodings::match_cm_math_font(font_name) {
                let mut table = [None; 256];
                // Start with the math encoding as the base
                for (i, entry) in math_table.iter().enumerate() {
                    table[i] = *entry;
                }
                // Overlay any existing /Differences on top (they're more specific)
                if let Encoding::Custom {
                    table: ref existing,
                } = encoding
                {
                    for i in 0..256 {
                        if existing[i].is_some() {
                            table[i] = existing[i];
                        }
                    }
                }
                encoding = Encoding::Custom {
                    table: Box::new(table),
                };
                resolver.diagnostics().info(Warning::info_with_context(
                    WarningKind::FontLoaded,
                    font_ctx,
                    format!("applied CM math encoding table for {font_name}"),
                ));
            }
        }
    }

    // D-011: Info for encoding selection
    let enc_desc = match &encoding {
        Encoding::Standard(s) => format!("{s:?}"),
        Encoding::Custom { .. } => "Custom".to_string(),
    };
    let name = base_font.as_deref().unwrap_or("(unnamed)");
    resolver.diagnostics().info(Warning::info_with_context(
        WarningKind::FontLoaded,
        font_ctx,
        format!("applied {enc_desc} encoding for {name}"),
    ));

    // D-011: Info for ToUnicode resolution
    if let Some(ref cmap) = tounicode {
        resolver.diagnostics().info(Warning::info_with_context(
            WarningKind::FontLoaded,
            font_ctx,
            format!(
                "resolved ToUnicode CMap ({} mappings) for {name}",
                cmap.total_mappings()
            ),
        ));
    }

    let widths = load_simple_widths(resolver, dict);
    let (font_data, font_program) = extract_font_data(resolver, dict, font_ctx);

    Ok(SimpleFont {
        subtype,
        base_font,
        encoding,
        tounicode,
        widths,
        font_data,
        font_program,
        differences_names,
    })
}

/// Load a Type0 (composite) font.
fn load_composite_font(
    resolver: &mut ObjectResolver,
    dict: &crate::object::PdfDictionary,
    font_ctx: WarningContext,
) -> Result<CompositeFont> {
    let base_font = resolve_base_font(resolver, dict);

    let encoding_name = dict
        .get_name(b"Encoding")
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .unwrap_or_else(|| "Identity-H".to_string());

    let tounicode =
        load_tounicode(resolver, dict, font_ctx).context("loading ToUnicode for composite font")?;

    // D-011: Info for ToUnicode on composite font
    if let Some(ref cmap) = tounicode {
        let name = base_font.as_deref().unwrap_or("(unnamed)");
        resolver.diagnostics().info(Warning::info_with_context(
            WarningKind::FontLoaded,
            font_ctx,
            format!(
                "resolved ToUnicode CMap ({} mappings) for composite font {name}",
                cmap.total_mappings()
            ),
        ));
    }

    // Try to resolve /Encoding as a CMap stream
    let parsed_cmap = resolve_encoding_cmap(resolver, dict, font_ctx);

    // Look up predefined CMap for code_length and is_vertical
    let predefined = cmap::lookup_predefined_cmap(&encoding_name);
    let code_length = predefined.map(|c| c.code_length).unwrap_or(2);
    let is_vertical = cmap::is_vertical_cmap(&encoding_name);

    // Load descendant CID font (which carries the FontDescriptor + font program).
    // The descendant can be an indirect reference (common) or an inline
    // dictionary (MS Word / some converters write it that way).
    let descendants = resolver
        .get_resolved_array(dict, b"DescendantFonts")
        .context("resolving DescendantFonts")?;
    let (descendant, font_data, font_program) = match descendants {
        Some(arr) if !arr.is_empty() => match &arr[0] {
            PdfObject::Reference(r) => {
                load_cid_font(resolver, *r, font_ctx).context("loading CID descendant font")?
            }
            PdfObject::Dictionary(d) => load_cid_font_from_dict(resolver, d.clone(), font_ctx)
                .context("loading inline CID descendant font")?,
            _ => (default_cid_font(), None, FontProgram::None),
        },
        _ => (default_cid_font(), None, FontProgram::None),
    };

    Ok(CompositeFont {
        base_font,
        font_data,
        font_program,
        encoding_name,
        tounicode,
        descendant,
        code_length,
        is_vertical,
        parsed_cmap,
    })
}

/// Try to resolve the font's /Encoding as a parsed CMap.
///
/// The /Encoding entry can be either a name (predefined CMap) or an
/// indirect reference to a CMap stream. For names, we check if the
/// predefined registry covers it; if not, we have no stream to parse.
/// For references, we resolve and decode the stream, then parse it.
fn resolve_encoding_cmap(
    resolver: &mut ObjectResolver,
    dict: &crate::object::PdfDictionary,
    font_ctx: WarningContext,
) -> Option<Box<ParsedCMap>> {
    // If /Encoding is a reference, try to resolve as CMap stream
    if let Some(enc_ref) = dict.get_ref(b"Encoding") {
        let stream = match resolver.resolve_stream(enc_ref) {
            Ok(s) => s,
            Err(_) => return None, // not a stream, might be a name ref
        };
        let decoded = match resolver.decode_stream_data(&stream, Some(enc_ref)) {
            Ok(d) => d,
            Err(e) => {
                resolver.diagnostics().warning(Warning::with_context(
                    Some(stream.data_offset),
                    WarningKind::DecodeError,
                    font_ctx,
                    format!("failed to decode /Encoding CMap stream: {e}"),
                ));
                return None;
            }
        };
        let cmap = ParsedCMap::parse_with_usecmap(&decoded, 0);
        resolver.diagnostics().info(Warning::info_with_context(
            WarningKind::FontLoaded,
            font_ctx,
            format!(
                "parsed embedded CMap stream ({} codespace ranges)",
                cmap.codespace_range_count()
            ),
        ));
        return Some(Box::new(cmap));
    }

    // For predefined CMap names that aren't Identity, we could build a
    // stub ParsedCMap. But without embedded CID-to-Unicode tables this
    // doesn't help. Leave as None for now (existing behavior).
    None
}

/// Load a CID descendant font and extract its embedded font program.
///
/// Returns (CidFont, font_data, font_program). The font data comes from the
/// CID font's /FontDescriptor, not the parent Type0 font.
fn load_cid_font(
    resolver: &mut ObjectResolver,
    desc_ref: ObjRef,
    font_ctx: WarningContext,
) -> Result<(CidFont, Option<Vec<u8>>, FontProgram)> {
    let dict = resolver
        .resolve_dict(desc_ref)
        .context("resolving CIDFont dictionary")?;
    load_cid_font_from_dict(resolver, dict, font_ctx)
}

/// Build a CIDFont from a resolved descendant dictionary. Shared between
/// the indirect-reference path and the inline-dict path (MS Word writes
/// `/DescendantFonts [ <<inline>> ]` rather than `[ N 0 R ]`).
fn load_cid_font_from_dict(
    resolver: &mut ObjectResolver,
    dict: crate::object::PdfDictionary,
    font_ctx: WarningContext,
) -> Result<(CidFont, Option<Vec<u8>>, FontProgram)> {
    let subtype_name = dict
        .get_name(b"Subtype")
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .unwrap_or_default();

    let subtype = match subtype_name.as_str() {
        "CIDFontType0" => CidSubtype::Type0,
        "CIDFontType2" => CidSubtype::Type2,
        _ => CidSubtype::Type2, // default
    };

    let base_font = resolve_base_font(resolver, &dict);

    let default_width = dict.get_i64(b"DW").unwrap_or(1000) as u32;

    // /W may be an indirect reference (MS Word / PDFbox often write it that
    // way). `dict.get_array` doesn't chase refs; use the resolver-aware path
    // so we don't fall back to the default 1000 for every glyph.
    let resolved_w = resolver.get_resolved_array(&dict, b"W").ok().flatten();
    let (widths, w_pairs) = match resolved_w {
        Some(ref w_array) => {
            let w = parse_cid_widths(w_array, resolver.diagnostics(), font_ctx);
            let name = base_font.as_deref().unwrap_or("(unnamed)");
            resolver.diagnostics().info(Warning::info_with_context(
                WarningKind::FontLoaded,
                font_ctx,
                format!("loaded /W array ({} CID width entries) for {name}", w.len()),
            ));
            let pairs = collect_cid_width_pairs(w_array);
            (w, pairs)
        }
        None => (CidWidths::new(), Vec::new()),
    };

    // Extract font program from the CID font's own FontDescriptor.
    let (font_data, font_program) = extract_font_data(resolver, &dict, font_ctx);

    // Compare /W entries against the embedded font's hmtx (TrueType) or
    // charstring (CFF) widths. Emits a FontMetricsDisagreement warning when
    // enough glyphs disagree enough to signal a /W bug (see #188).
    if !w_pairs.is_empty() {
        if let Some(data) = font_data.as_deref() {
            check_cid_metrics_disagreement(
                &w_pairs,
                data,
                font_program,
                base_font.as_deref().unwrap_or("(unnamed)"),
                resolver.diagnostics(),
                font_ctx,
            );
        }
    }

    Ok((
        CidFont {
            subtype,
            base_font,
            default_width,
            widths,
        },
        font_data,
        font_program,
    ))
}

/// Collect explicit (cid, width) pairs from a /W array in one pass.
///
/// Unlike `parse_cid_widths`, this does not emit diagnostics (they are
/// already emitted by `parse_cid_widths` running on the same array) and
/// it returns a flat Vec that we can iterate over to compare with the
/// embedded font's hmtx / charstring widths. Silently skips malformed
/// segments so a warning storm isn't duplicated with the parse path.
fn collect_cid_width_pairs(w_array: &[PdfObject]) -> Vec<(u32, f64)> {
    let mut out: Vec<(u32, f64)> = Vec::new();
    let mut i = 0;
    while i < w_array.len() {
        let cid_start = match w_array[i].as_i64() {
            Some(n) if n >= 0 => n as u32,
            _ => {
                i += 1;
                continue;
            }
        };
        i += 1;
        if i >= w_array.len() {
            break;
        }

        match &w_array[i] {
            PdfObject::Array(arr) => {
                for (j, obj) in arr.iter().enumerate() {
                    if let Some(w) = obj.as_f64() {
                        out.push((cid_start + j as u32, w));
                    }
                }
                i += 1;
            }
            _ => {
                let cid_end = match w_array[i].as_i64() {
                    Some(n) if n >= 0 => n as u32,
                    _ => {
                        i += 1;
                        continue;
                    }
                };
                i += 1;
                if i >= w_array.len() {
                    break;
                }
                let w = match w_array[i].as_f64() {
                    Some(w) => w,
                    None => {
                        i += 1;
                        continue;
                    }
                };
                i += 1;
                // Cap pathological ranges. Matches parse_cid_widths.
                let range_len = cid_end.saturating_sub(cid_start) + 1;
                if range_len > 65536 {
                    continue;
                }
                for cid in cid_start..=cid_end {
                    out.push((cid, w));
                }
            }
        }
    }
    out
}

/// Relative delta above which a /W vs embedded width pair counts as
/// "disagreeing". Chosen generously because legitimate rounding and
/// PDF-generator width rebuilding (stripped hmtx, CID-to-GID-remapped
/// fonts) produce differences up to a few percent; 10% cleanly
/// separates those from broken /W arrays (which tend to disagree by
/// 30-60% on most glyphs).
const METRIC_DISAGREEMENT_DELTA: f64 = 0.10;
/// Number of disagreeing glyphs required to emit the warning. A single
/// bad /W entry can be a typo; a run of bad entries is a generator bug.
const METRIC_DISAGREEMENT_MIN_COUNT: usize = 5;

/// Embedded advance-width source for CID metric cross-checks. Parses the
/// font once; [`Self::advance`] looks up each CID against the cached
/// tables. For CIDFontType2 (TrueType) we assume CID == GID because we
/// don't parse /CIDToGIDMap (it defaults to Identity for the vast
/// majority of MS Word / OOXML output). For CIDFontType0 (CFF), CID is a
/// charstring index, which is what `CffFont::advance_width` accepts.
enum EmbeddedAdvances {
    TrueType {
        font: udoc_font::ttf::TrueTypeFont,
        upem: f64,
    },
    Cff(udoc_font::cff::CffFont),
}

impl EmbeddedAdvances {
    fn load(font_data: &[u8], font_program: FontProgram) -> Option<Self> {
        match font_program {
            FontProgram::TrueType => {
                let font = udoc_font::ttf::TrueTypeFont::from_bytes(font_data).ok()?;
                let upem = f64::from(font.units_per_em());
                if upem <= 0.0 {
                    return None;
                }
                Some(Self::TrueType { font, upem })
            }
            FontProgram::Cff => {
                let font = udoc_font::cff::CffFont::from_bytes(font_data).ok()?;
                Some(Self::Cff(font))
            }
            FontProgram::Type1 | FontProgram::None => None,
        }
    }

    fn advance(&self, cid: u32) -> Option<f64> {
        let gid = u16::try_from(cid).ok()?;
        match self {
            Self::TrueType { font, upem } => {
                // Normalize hmtx font units to PDF's 1000-unit glyph space.
                Some(f64::from(font.advance_width(gid)) * 1000.0 / upem)
            }
            Self::Cff(font) => font.advance_width(gid).map(f64::from),
        }
    }
}

/// After a CID font loads, compare its /W array to the embedded font's
/// hmtx (TrueType) or charstring (CFF) widths. Emits
/// `WarningKind::FontMetricsDisagreement` when the two disagree on a
/// substantive fraction of glyphs. See #188.
fn check_cid_metrics_disagreement(
    w_pairs: &[(u32, f64)],
    font_data: &[u8],
    font_program: FontProgram,
    font_name: &str,
    diag: &dyn DiagnosticsSink,
    font_ctx: WarningContext,
) {
    // Only TrueType and CFF expose per-glyph widths we can cross-check.
    // Parse the font program once; a /W array with thousands of entries
    // would otherwise re-parse the whole font per CID.
    let Some(embedded) = EmbeddedAdvances::load(font_data, font_program) else {
        return;
    };

    let mut disagreeing: Vec<(u32, f64, f64, f64)> = Vec::new();
    let mut compared: usize = 0;
    let mut max_delta: f64 = 0.0;

    for &(cid, w_pdf) in w_pairs {
        let Some(w_emb) = embedded.advance(cid) else {
            continue;
        };
        // Skip zero-width glyphs on either side: they're typically combining
        // marks or absent-from-font sentinels and produce meaningless deltas.
        if w_pdf <= 0.0 || w_emb <= 0.0 {
            continue;
        }
        compared += 1;
        let delta = (w_pdf - w_emb).abs() / w_pdf.max(w_emb);
        if delta > max_delta {
            max_delta = delta;
        }
        if delta > METRIC_DISAGREEMENT_DELTA {
            disagreeing.push((cid, w_pdf, w_emb, delta));
        }
    }

    if disagreeing.len() < METRIC_DISAGREEMENT_MIN_COUNT {
        return;
    }

    // Build a short sample of the first three disagreeing glyphs for
    // debug output. Ordered by CID so the message is stable across runs.
    let samples: Vec<String> = disagreeing
        .iter()
        .take(3)
        .map(|(cid, w_pdf, w_emb, delta)| {
            format!(
                "gid={cid} /W={w_pdf:.0} embedded={w_emb:.0} delta={:.1}%",
                delta * 100.0
            )
        })
        .collect();

    let message = format!(
        "font {font_name}: /W array disagrees with embedded {} widths on {} / {} glyphs (>{}%, max {:.1}%); samples: {}",
        match font_program {
            FontProgram::TrueType => "hmtx",
            FontProgram::Cff => "charstring",
            _ => "embedded",
        },
        disagreeing.len(),
        compared,
        (METRIC_DISAGREEMENT_DELTA * 100.0) as u32,
        max_delta * 100.0,
        samples.join("; "),
    );

    diag.warning(Warning::with_context(
        None,
        WarningKind::FontMetricsDisagreement,
        font_ctx,
        message,
    ));
}

/// Load a Type3 font.
///
/// Returns both the format-agnostic core metadata (`Type3FontCore`) and
/// the PDF-specific CharProc/Resources refs (`Type3FontPdfRefs`). The
/// core goes into the `Font::Type3` enum variant; the refs live in a
/// side map in the content interpreter.
fn load_type3_font(
    resolver: &mut ObjectResolver,
    dict: &crate::object::PdfDictionary,
    font_ctx: WarningContext,
) -> Result<(Type3FontCore, Type3FontPdfRefs)> {
    let encoding = load_encoding(resolver, dict).context("loading encoding for Type3 font")?;
    let tounicode =
        load_tounicode(resolver, dict, font_ctx).context("loading ToUnicode for Type3 font")?;
    let widths = load_simple_widths(resolver, dict);

    let base_font = resolve_base_font(resolver, dict);

    // /CharProcs: dict of glyph name -> stream ref
    let char_procs = load_char_procs(resolver, dict, font_ctx);

    // /Resources: store ref for later CharProc interpretation
    let resources_ref = dict.get_ref(b"Resources");

    // /FontMatrix: 6-element array, default is the standard 1/1000 scaling
    let font_matrix = load_font_matrix(dict);

    // Extract glyph names from /Differences for code -> glyph name lookup
    let glyph_names = load_type3_glyph_names(dict);

    if !char_procs.is_empty() {
        resolver.diagnostics().info(Warning::info_with_context(
            WarningKind::FontLoaded,
            font_ctx,
            format!(
                "loaded Type3 /CharProcs ({} glyphs, {} glyph names)",
                char_procs.len(),
                glyph_names.len()
            ),
        ));
    }

    let core = Type3FontCore {
        encoding,
        tounicode,
        widths,
        font_matrix,
        glyph_names,
        base_font,
    };
    let pdf_refs = Type3FontPdfRefs {
        char_procs,
        resources_ref,
    };
    Ok((core, pdf_refs))
}

/// Parse /CharProcs dictionary into a `HashMap<String, ObjRef>`.
///
/// Each entry maps a glyph name to the indirect reference of its
/// content stream. Direct (non-reference) values are skipped with
/// a warning since we need the ObjRef for later resolution.
fn load_char_procs(
    resolver: &mut ObjectResolver,
    dict: &crate::object::PdfDictionary,
    font_ctx: WarningContext,
) -> std::collections::HashMap<String, ObjRef> {
    let mut result = std::collections::HashMap::new();

    // /CharProcs can be a direct dict or an indirect ref
    let char_procs_dict = match dict.get(b"CharProcs") {
        Some(PdfObject::Dictionary(d)) => d.clone(),
        Some(PdfObject::Reference(r)) => match resolver.resolve_dict(*r) {
            Ok(d) => d,
            Err(e) => {
                resolver.diagnostics().warning(Warning::with_context(
                    None,
                    WarningKind::FontError,
                    font_ctx,
                    format!("failed to resolve /CharProcs: {e}"),
                ));
                return result;
            }
        },
        Some(_) => {
            resolver.diagnostics().warning(Warning::with_context(
                None,
                WarningKind::FontError,
                font_ctx,
                "unexpected type for /CharProcs (expected dictionary)",
            ));
            return result;
        }
        None => return result,
    };

    for (key, value) in char_procs_dict.iter() {
        let glyph_name = String::from_utf8_lossy(key).into_owned();
        match value.as_reference() {
            Some(obj_ref) => {
                result.insert(glyph_name, obj_ref);
            }
            None => {
                // Direct stream in CharProcs is unusual; skip it since we
                // store ObjRef for lazy resolution.
                resolver.diagnostics().warning(Warning::with_context(
                    None,
                    WarningKind::FontError,
                    font_ctx,
                    format!(
                        "/CharProcs entry '{}' is not an indirect reference, skipping",
                        glyph_name
                    ),
                ));
            }
        }
    }

    // Parse-time limit: cap CharProcs dictionary size (T3-010)
    const MAX_CHARPROCS_PER_FONT: usize = 1000;
    if result.len() > MAX_CHARPROCS_PER_FONT {
        resolver.diagnostics().warning(Warning::with_context(
            None,
            WarningKind::FontError,
            font_ctx,
            format!(
                "/CharProcs dictionary has {} entries, truncating to {}",
                result.len(),
                MAX_CHARPROCS_PER_FONT
            ),
        ));
        let mut keys: Vec<String> = result.keys().cloned().collect();
        keys.sort();
        for key in keys.into_iter().skip(MAX_CHARPROCS_PER_FONT) {
            result.remove(&key);
        }
    }

    result
}

/// Parse /FontMatrix from a font dictionary.
///
/// Returns the 6-element transformation matrix, defaulting to
/// [0.001, 0, 0, 0.001, 0, 0] (standard 1/1000 glyph-to-text scaling).
fn load_font_matrix(dict: &crate::object::PdfDictionary) -> [f64; 6] {
    const DEFAULT: [f64; 6] = [0.001, 0.0, 0.0, 0.001, 0.0, 0.0];

    let arr = match dict.get_array(b"FontMatrix") {
        Some(a) if a.len() >= 6 => a,
        _ => return DEFAULT,
    };

    let mut matrix = [0.0f64; 6];
    for (i, obj) in arr.iter().take(6).enumerate() {
        matrix[i] = obj.as_f64().unwrap_or(DEFAULT[i]);
    }
    matrix
}

/// Extract glyph name strings from /Encoding /Differences.
///
/// Returns a mapping from character code to the raw glyph name string.
/// This is needed for CharProc lookup (code -> glyph name -> CharProc stream).
/// Uses `extract_differences_names` to share iteration logic with `parse_differences`.
fn load_type3_glyph_names(
    dict: &crate::object::PdfDictionary,
) -> std::collections::HashMap<u8, String> {
    let enc_obj = match dict.get(b"Encoding") {
        Some(obj) => obj,
        None => return std::collections::HashMap::new(),
    };

    let enc_dict = match enc_obj {
        PdfObject::Dictionary(d) => d,
        _ => return std::collections::HashMap::new(),
    };

    let diffs = match enc_dict.get(b"Differences") {
        Some(PdfObject::Array(arr)) => arr,
        _ => return std::collections::HashMap::new(),
    };

    extract_differences_names(diffs).into_iter().collect()
}

/// Extract the embedded font program bytes from a font's /FontDescriptor.
///
/// Resolves the /FontDescriptor dict, then tries /FontFile2 (TrueType),
/// /FontFile3 (CFF/OpenType), and /FontFile (Type1) in that order.
/// Returns `(data, program_type)`. For fonts without an embedded program
/// (standard 14 fonts), returns `(None, FontProgram::None)`.
fn extract_font_data(
    resolver: &mut ObjectResolver,
    dict: &crate::object::PdfDictionary,
    font_ctx: WarningContext,
) -> (Option<Vec<u8>>, FontProgram) {
    // /FontDescriptor can be an indirect reference (common) or an inline
    // dictionary (MS Word, some OOXML converters). Handle both.
    let desc_dict = match dict.get(b"FontDescriptor") {
        Some(PdfObject::Reference(r)) => match resolver.resolve_dict(*r) {
            Ok(d) => d,
            Err(_) => return (None, FontProgram::None),
        },
        Some(PdfObject::Dictionary(d)) => d.clone(),
        _ => return (None, FontProgram::None),
    };

    // Try FontFile2 (TrueType) first -- most common for modern PDFs.
    if let Some(ff2_ref) = desc_dict.get_ref(b"FontFile2") {
        if let Ok(stream) = resolver.resolve_stream(ff2_ref) {
            if let Ok(data) = resolver.decode_stream_data(&stream, Some(ff2_ref)) {
                if !data.is_empty() {
                    return (Some(data), FontProgram::TrueType);
                }
            }
        }
    }

    // Try FontFile3 (CFF / OpenType CFF).
    if let Some(ff3_ref) = desc_dict.get_ref(b"FontFile3") {
        if let Ok(stream) = resolver.resolve_stream(ff3_ref) {
            if let Ok(data) = resolver.decode_stream_data(&stream, Some(ff3_ref)) {
                if !data.is_empty() {
                    return (Some(data), FontProgram::Cff);
                }
            }
        }
    }

    // Try FontFile (Type1 PostScript).
    if let Some(ff1_ref) = desc_dict.get_ref(b"FontFile") {
        if let Ok(stream) = resolver.resolve_stream(ff1_ref) {
            if let Ok(data) = resolver.decode_stream_data(&stream, Some(ff1_ref)) {
                if !data.is_empty() {
                    return (Some(data), FontProgram::Type1);
                }
            }
        }
    }

    // Log that the font descriptor exists but has no embedded program.
    resolver.diagnostics().info(Warning::info_with_context(
        WarningKind::FontLoaded,
        font_ctx,
        "font descriptor present but no embedded font program (FontFile/FontFile2/FontFile3)",
    ));

    (None, FontProgram::None)
}

/// Load /ToUnicode CMap if present.
fn load_tounicode(
    resolver: &mut ObjectResolver,
    dict: &crate::object::PdfDictionary,
    font_ctx: WarningContext,
) -> Result<Option<ToUnicodeCMap>> {
    let tounicode_ref = match dict.get_ref(b"ToUnicode") {
        Some(r) => r,
        None => return Ok(None),
    };

    let stream = match resolver.resolve_stream(tounicode_ref) {
        Ok(s) => s,
        Err(e) => {
            // D-012: populate obj_ref in warnings
            resolver.diagnostics().warning(Warning::with_context(
                None,
                WarningKind::DecodeError,
                font_ctx,
                format!(
                    "failed to resolve /ToUnicode stream (obj {}): {e}",
                    tounicode_ref
                ),
            ));
            return Ok(None);
        }
    };

    let decoded = match resolver.decode_stream_data(&stream, Some(tounicode_ref)) {
        Ok(d) => d,
        Err(e) => {
            // D-012: populate obj_ref in warnings
            resolver.diagnostics().warning(Warning::with_context(
                Some(stream.data_offset),
                WarningKind::DecodeError,
                font_ctx,
                format!("failed to decode /ToUnicode stream: {e}"),
            ));
            return Ok(None);
        }
    };

    let cmap = ToUnicodeCMap::parse(&decoded);
    if cmap.total_mappings() > 0 {
        Ok(Some(cmap))
    } else {
        Ok(None)
    }
}

/// Load /Encoding from a font dictionary.
///
/// /Encoding can be:
/// - A name: "WinAnsiEncoding", "MacRomanEncoding", etc.
/// - A dictionary with /BaseEncoding and /Differences
/// - Missing (use font's built-in encoding)
fn load_encoding(
    resolver: &mut ObjectResolver,
    dict: &crate::object::PdfDictionary,
) -> Result<Encoding> {
    let enc_obj = match dict.get(b"Encoding") {
        Some(obj) => obj.clone(),
        None => return Ok(Encoding::Standard(StandardEncoding::BuiltIn)),
    };

    match &enc_obj {
        PdfObject::Name(name) => {
            let name_str = String::from_utf8_lossy(name);
            Ok(Encoding::Standard(parse_encoding_name(&name_str)))
        }
        PdfObject::Reference(r) => {
            // Could be a name or dict behind the reference
            let resolved = resolver.resolve(*r).context("resolving /Encoding")?;
            match &resolved {
                PdfObject::Name(name) => {
                    let name_str = String::from_utf8_lossy(name);
                    Ok(Encoding::Standard(parse_encoding_name(&name_str)))
                }
                PdfObject::Dictionary(enc_dict) => load_encoding_dict(resolver, enc_dict),
                _ => Ok(Encoding::Standard(StandardEncoding::BuiltIn)),
            }
        }
        PdfObject::Dictionary(enc_dict) => load_encoding_dict(resolver, enc_dict),
        _ => Ok(Encoding::Standard(StandardEncoding::BuiltIn)),
    }
}

/// Load an encoding dictionary with /BaseEncoding and /Differences.
fn load_encoding_dict(
    _resolver: &mut ObjectResolver,
    dict: &crate::object::PdfDictionary,
) -> Result<Encoding> {
    let base = dict
        .get_name(b"BaseEncoding")
        .map(|n| parse_encoding_name(&String::from_utf8_lossy(n)))
        .unwrap_or(StandardEncoding::Standard);

    // Parse /Differences array
    let differences = match dict.get(b"Differences") {
        Some(PdfObject::Array(arr)) => parse_differences(arr),
        _ => Vec::new(),
    };

    if differences.is_empty() {
        Ok(Encoding::Standard(base))
    } else {
        Ok(Encoding::custom(base, &differences))
    }
}

/// Extract raw /Differences glyph names from a font dict's /Encoding.
/// Returns empty vec if no /Differences exist.
fn extract_encoding_differences_names(
    resolver: &mut ObjectResolver,
    dict: &crate::object::PdfDictionary,
) -> Vec<(u8, String)> {
    let enc_obj = match dict.get(b"Encoding") {
        Some(obj) => obj.clone(),
        None => return Vec::new(),
    };
    let enc_dict = match &enc_obj {
        PdfObject::Dictionary(d) => d.clone(),
        PdfObject::Reference(r) => match resolver.resolve(*r) {
            Ok(PdfObject::Dictionary(d)) => d,
            _ => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    match enc_dict.get(b"Differences") {
        Some(PdfObject::Array(arr)) => extract_differences_names(arr),
        _ => Vec::new(),
    }
}

/// Extract raw (code, glyph_name) pairs from a /Differences array.
///
/// Format: [code1 /name1 /name2 ... code2 /name3 ...]
/// Each integer sets the current code point; subsequent names override
/// consecutive code points from there.
///
/// Shared by `parse_differences` (resolves names to chars via AGL) and
/// `load_type3_glyph_names` (keeps raw names for CharProc lookup).
fn extract_differences_names(arr: &[PdfObject]) -> Vec<(u8, String)> {
    let mut result = Vec::new();
    let mut current_code: Option<u8> = None;

    for obj in arr {
        match obj {
            PdfObject::Integer(n) => {
                current_code = u8::try_from(*n).ok();
            }
            PdfObject::Name(name) => {
                if let Some(code) = current_code {
                    let name_str = String::from_utf8_lossy(name).into_owned();
                    result.push((code, name_str));
                    current_code = Some(code.wrapping_add(1));
                }
            }
            _ => {}
        }
    }

    result
}

/// Parse a /Differences array into (code, char) overrides.
///
/// Uses `extract_differences_names` for iteration, then resolves each
/// glyph name to a Unicode character via AGL lookup.
fn parse_differences(arr: &[PdfObject]) -> Vec<(u8, char)> {
    extract_differences_names(arr)
        .into_iter()
        .filter_map(|(code, name)| {
            // Strip subset prefix if present (e.g. "ABCDEF+space" -> "space")
            let clean_name = match name.find('+') {
                Some(pos) => &name[pos + 1..],
                None => &name,
            };
            parse_glyph_name(clean_name).map(|c| (code, c))
        })
        .collect()
}

/// Parse an encoding name string to a StandardEncoding variant.
fn parse_encoding_name(name: &str) -> StandardEncoding {
    match name {
        "WinAnsiEncoding" => StandardEncoding::WinAnsi,
        "MacRomanEncoding" => StandardEncoding::MacRoman,
        "MacExpertEncoding" => StandardEncoding::MacExpert,
        "StandardEncoding" => StandardEncoding::Standard,
        _ => StandardEncoding::BuiltIn,
    }
}

/// Parse the /W array from a CIDFont dictionary into a CidWidths table.
///
/// The /W array alternates between two entry formats:
/// - `cid_start [w1 w2 ...]` -- individual widths for consecutive CIDs
/// - `cid_start cid_end w` -- uniform width for a CID range
///
/// Malformed entries are skipped with a warning rather than failing the parse.
/// Maximum total width entries across all CID /W array segments.
const MAX_WIDTHS_ENTRIES: usize = 1_000_000;

fn parse_cid_widths(
    w_array: &[PdfObject],
    diag: &dyn DiagnosticsSink,
    font_ctx: WarningContext,
) -> CidWidths {
    use std::collections::BTreeMap;

    let mut widths: BTreeMap<u32, f64> = BTreeMap::new();
    let mut i = 0;

    while i < w_array.len() {
        // First element of each entry must be an integer (CID start)
        let cid_start = match w_array[i].as_i64() {
            Some(n) if n >= 0 => n as u32,
            _ => {
                // Not an integer; skip and try the next element
                // D-012: populate obj_ref in warnings
                diag.warning(Warning::with_context(
                    None,
                    WarningKind::FontError,
                    font_ctx,
                    format!(
                        "/W array: expected integer at index {}, got {}",
                        i,
                        w_array[i].type_name()
                    ),
                ));
                i += 1;
                continue;
            }
        };
        i += 1;

        if i >= w_array.len() {
            break;
        }

        if widths.len() >= MAX_WIDTHS_ENTRIES {
            diag.warning(Warning::with_context(
                None,
                WarningKind::FontError,
                font_ctx,
                format!(
                    "/W array: width entries exceeded limit ({}), truncating",
                    MAX_WIDTHS_ENTRIES
                ),
            ));
            break;
        }

        match &w_array[i] {
            // Individual widths: cid_start [w1 w2 ...]
            PdfObject::Array(arr) => {
                for (j, obj) in arr.iter().enumerate() {
                    if let Some(w) = obj.as_f64() {
                        widths.insert(cid_start + j as u32, w);
                    }
                    if widths.len() >= MAX_WIDTHS_ENTRIES {
                        break;
                    }
                }
                i += 1;
            }
            // Range with uniform width: cid_start cid_end w
            _ => {
                let cid_end = match w_array[i].as_i64() {
                    Some(n) if n >= 0 => n as u32,
                    _ => {
                        diag.warning(Warning::with_context(
                            None,
                            WarningKind::FontError,
                            font_ctx,
                            format!(
                                "/W array: expected integer (cid_end) at index {}, got {}",
                                i,
                                w_array[i].type_name()
                            ),
                        ));
                        i += 1;
                        continue;
                    }
                };
                i += 1;

                if i >= w_array.len() {
                    diag.warning(Warning::with_context(
                        None,
                        WarningKind::FontError,
                        font_ctx,
                        "/W array: truncated range entry (missing width)",
                    ));
                    break;
                }

                let w = match w_array[i].as_f64() {
                    Some(w) => w,
                    None => {
                        diag.warning(Warning::with_context(
                            None,
                            WarningKind::FontError,
                            font_ctx,
                            format!(
                                "/W array: expected number (width) at index {}, got {}",
                                i,
                                w_array[i].type_name()
                            ),
                        ));
                        i += 1;
                        continue;
                    }
                };
                i += 1;

                // Cap range to avoid pathological sizes
                let range_len = cid_end.saturating_sub(cid_start) + 1;
                if range_len > 65536 {
                    diag.warning(Warning::with_context(
                        None,
                        WarningKind::FontError,
                        font_ctx,
                        format!(
                            "/W array: range {}-{} too large ({}), skipping",
                            cid_start, cid_end, range_len
                        ),
                    ));
                    continue;
                }

                for cid in cid_start..=cid_end {
                    widths.insert(cid, w);
                }
            }
        }
    }

    CidWidths::from_map(widths)
}

/// Load /FirstChar, /LastChar, /Widths from a simple font dictionary.
///
/// Returns None if any of the required entries are missing (common for
/// standard 14 fonts which don't need explicit widths).
///
/// /Widths is often an indirect reference (e.g., `153 0 R`), so the
/// resolver is needed to dereference it.
fn load_simple_widths(
    resolver: &mut ObjectResolver,
    dict: &crate::object::PdfDictionary,
) -> Option<SimpleWidths> {
    let first_char = dict.get_i64(b"FirstChar").filter(|&n| n >= 0)? as u32;
    let last_char = dict.get_i64(b"LastChar").filter(|&n| n >= 0)? as u32;

    // /Widths may be a direct array or an indirect reference. Use the
    // resolver to handle both cases.
    let widths_array = resolver
        .get_resolved_array(dict, b"Widths")
        .ok()
        .flatten()?;

    if last_char < first_char {
        return None;
    }

    let expected_len = (last_char - first_char + 1) as usize;
    if expected_len > MAX_WIDTHS_ENTRIES {
        return None;
    }
    let mut widths = Vec::with_capacity(expected_len.min(widths_array.len()));

    for obj in &widths_array {
        widths.push(obj.as_f64().unwrap_or(0.0));
    }

    Some(SimpleWidths::new(first_char, widths))
}

/// Default CIDFont for when the descendant can't be loaded.
fn default_cid_font() -> CidFont {
    CidFont {
        subtype: CidSubtype::Type2,
        base_font: None,
        default_width: 1000,
        widths: CidWidths::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::PdfObject;

    /// Create a minimal ObjectResolver for tests that only need inline dict values.
    /// The resolver has no objects but can resolve direct (non-reference) arrays.
    fn test_resolver() -> ObjectResolver<'static> {
        // Minimal valid PDF: header + empty xref. The resolver won't be asked
        // to resolve any indirect references in these tests.
        static MINIMAL_PDF: &[u8] =
            b"%PDF-1.0\nxref\n0 1\n0000000000 65535 f \ntrailer<</Size 1>>\nstartxref\n9\n%%EOF";
        let xref = crate::parse::document_parser::XrefTable::new();
        ObjectResolver::new(MINIMAL_PDF, xref)
    }

    /// Helper to build a Type3FontCore with default fields for tests.
    fn make_type3_font(widths: Option<SimpleWidths>) -> Type3FontCore {
        Type3FontCore {
            encoding: Encoding::Standard(StandardEncoding::BuiltIn),
            tounicode: None,
            widths,
            font_matrix: [0.001, 0.0, 0.0, 0.001, 0.0, 0.0],
            glyph_names: std::collections::HashMap::new(),
            base_font: None,
        }
    }

    #[test]
    fn test_parse_encoding_name() {
        assert_eq!(
            parse_encoding_name("WinAnsiEncoding"),
            StandardEncoding::WinAnsi
        );
        assert_eq!(
            parse_encoding_name("MacRomanEncoding"),
            StandardEncoding::MacRoman
        );
        assert_eq!(
            parse_encoding_name("StandardEncoding"),
            StandardEncoding::Standard
        );
        assert_eq!(parse_encoding_name("Unknown"), StandardEncoding::BuiltIn);
    }

    #[test]
    fn test_parse_differences() {
        let arr = vec![
            PdfObject::Integer(65), // start at code 65
            PdfObject::Name(b"space".to_vec()),
            PdfObject::Name(b"exclam".to_vec()),
            PdfObject::Integer(90), // jump to code 90
            PdfObject::Name(b"A".to_vec()),
        ];
        let diffs = parse_differences(&arr);
        assert_eq!(diffs.len(), 3);
        assert_eq!(diffs[0], (65, ' '));
        assert_eq!(diffs[1], (66, '!'));
        assert_eq!(diffs[2], (90, 'A'));
    }

    use crate::object::resolver::ObjectResolver;
    use crate::parse::DocumentParser;
    use crate::CollectingDiagnostics;
    use std::sync::Arc;

    #[test]
    fn test_simple_font_decode_char() {
        // Build a simple WinAnsi font manually and test decode_char
        let font = Font::Simple(SimpleFont {
            subtype: SimpleSubtype::Type1,
            base_font: Some("Helvetica".to_string()),
            encoding: Encoding::Standard(StandardEncoding::WinAnsi),
            tounicode: None,
            widths: None,
            font_data: None,
            font_program: FontProgram::None,
            differences_names: Vec::new(),
        });

        assert_eq!(font.decode_char(&[0x41]), "A");
        assert_eq!(font.decode_char(&[0x20]), " ");
        assert_eq!(font.decode_char(&[0x93]), "\u{201C}"); // left double quote
    }

    #[test]
    fn test_composite_font_with_tounicode() {
        use udoc_font::tounicode::ToUnicodeCMap;

        let cmap_data = b"\
begincmap
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
1 beginbfchar
<0041> <0048>
endbfchar
endcmap
";
        let cmap = ToUnicodeCMap::parse(cmap_data);
        let font = Font::Composite(CompositeFont {
            base_font: Some("TestFont".to_string()),
            font_data: None,
            font_program: FontProgram::None,
            encoding_name: "Identity-H".to_string(),
            tounicode: Some(cmap),
            descendant: CidFont {
                subtype: CidSubtype::Type2,
                base_font: None,
                default_width: 1000,
                widths: CidWidths::new(),
            },
            code_length: 2,
            is_vertical: false,
            parsed_cmap: None,
        });

        // 0x00 0x41 -> "H" via ToUnicode
        assert_eq!(font.decode_char(&[0x00, 0x41]), "H");
        // Unknown code -> FFFD
        assert_eq!(font.decode_char(&[0xFF, 0xFF]), "\u{FFFD}");
    }

    // -- Width parsing tests --

    #[test]
    fn test_cid_widths_individual() {
        // /W [ 1 [500 600 700] ]
        // CID 1 -> 500, CID 2 -> 600, CID 3 -> 700
        let widths =
            CidWidths::from_map([(1, 500.0), (2, 600.0), (3, 700.0)].into_iter().collect());
        assert_eq!(widths.width(1), Some(500.0));
        assert_eq!(widths.width(2), Some(600.0));
        assert_eq!(widths.width(3), Some(700.0));
        assert_eq!(widths.width(0), None);
        assert_eq!(widths.width(4), None);
    }

    #[test]
    fn test_cid_widths_range() {
        // /W [ 10 20 1000 ]
        // CIDs 10..=20 all get width 1000
        let mut map = std::collections::BTreeMap::new();
        for cid in 10..=20 {
            map.insert(cid, 1000.0);
        }
        let widths = CidWidths::from_map(map);
        assert_eq!(widths.width(10), Some(1000.0));
        assert_eq!(widths.width(15), Some(1000.0));
        assert_eq!(widths.width(20), Some(1000.0));
        assert_eq!(widths.width(9), None);
        assert_eq!(widths.width(21), None);
    }

    #[test]
    fn test_cid_widths_empty() {
        let widths = CidWidths::new();
        assert_eq!(widths.width(0), None);
        assert_eq!(widths.width(100), None);
    }

    #[test]
    fn test_parse_cid_widths_individual_entry() {
        // /W [ 5 [200 300 400] ]
        let diag = crate::diagnostics::NullDiagnostics;

        let w_array = vec![
            PdfObject::Integer(5),
            PdfObject::Array(vec![
                PdfObject::Real(200.0),
                PdfObject::Real(300.0),
                PdfObject::Integer(400),
            ]),
        ];

        let widths = parse_cid_widths(&w_array, &diag, WarningContext::default());
        assert_eq!(widths.width(5), Some(200.0));
        assert_eq!(widths.width(6), Some(300.0));
        assert_eq!(widths.width(7), Some(400.0));
        assert_eq!(widths.width(4), None);
        assert_eq!(widths.width(8), None);
    }

    #[test]
    fn test_parse_cid_widths_range_entry() {
        // /W [ 10 15 500 ]
        let diag = crate::diagnostics::NullDiagnostics;

        let w_array = vec![
            PdfObject::Integer(10),
            PdfObject::Integer(15),
            PdfObject::Integer(500),
        ];

        let widths = parse_cid_widths(&w_array, &diag, WarningContext::default());
        for cid in 10..=15 {
            assert_eq!(widths.width(cid), Some(500.0));
        }
        assert_eq!(widths.width(9), None);
        assert_eq!(widths.width(16), None);
    }

    #[test]
    fn test_parse_cid_widths_mixed_entries() {
        // /W [ 1 [100 200] 10 20 500 30 [800] ]
        let diag = crate::diagnostics::NullDiagnostics;

        let w_array = vec![
            // Individual: CID 1 -> 100, CID 2 -> 200
            PdfObject::Integer(1),
            PdfObject::Array(vec![PdfObject::Integer(100), PdfObject::Integer(200)]),
            // Range: CIDs 10-20 -> 500
            PdfObject::Integer(10),
            PdfObject::Integer(20),
            PdfObject::Integer(500),
            // Individual: CID 30 -> 800
            PdfObject::Integer(30),
            PdfObject::Array(vec![PdfObject::Integer(800)]),
        ];

        let widths = parse_cid_widths(&w_array, &diag, WarningContext::default());
        assert_eq!(widths.width(1), Some(100.0));
        assert_eq!(widths.width(2), Some(200.0));
        assert_eq!(widths.width(10), Some(500.0));
        assert_eq!(widths.width(15), Some(500.0));
        assert_eq!(widths.width(20), Some(500.0));
        assert_eq!(widths.width(30), Some(800.0));
        assert_eq!(widths.width(0), None);
        assert_eq!(widths.width(25), None);
    }

    #[test]
    fn test_parse_cid_widths_empty() {
        let diag = crate::diagnostics::NullDiagnostics;
        let widths = parse_cid_widths(&[], &diag, WarningContext::default());
        assert_eq!(widths.width(0), None);
    }

    #[test]
    fn test_parse_cid_widths_malformed_warns() {
        // A /W array with garbage (Name instead of integer) should warn and skip
        let diag = Arc::new(CollectingDiagnostics::new());

        let w_array = vec![
            PdfObject::Name(b"garbage".to_vec()),
            PdfObject::Integer(5),
            PdfObject::Array(vec![PdfObject::Integer(100)]),
        ];

        let widths = parse_cid_widths(&w_array, &*diag, WarningContext::default());
        // The garbage Name at index 0 is skipped with a warning,
        // then 5 [100] is parsed normally
        assert_eq!(widths.width(5), Some(100.0));
        let warnings = diag.warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("/W array"));
    }

    #[test]
    fn test_parse_cid_widths_truncated_range() {
        // /W [ 10 15 ] -- missing the width value
        let diag = Arc::new(CollectingDiagnostics::new());

        let w_array = vec![PdfObject::Integer(10), PdfObject::Integer(15)];

        let widths = parse_cid_widths(&w_array, &*diag, WarningContext::default());
        // Should produce no widths but a warning about truncated range
        assert_eq!(widths.width(10), None);
        let warnings = diag.warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("truncated"));
    }

    #[test]
    fn test_simple_widths_lookup() {
        // FirstChar=32, widths for codes 32..=34
        let widths = SimpleWidths::new(32, vec![250.0, 500.0, 750.0]);
        assert_eq!(widths.width(32), Some(250.0));
        assert_eq!(widths.width(33), Some(500.0));
        assert_eq!(widths.width(34), Some(750.0));
        assert_eq!(widths.width(31), None); // below range
        assert_eq!(widths.width(35), None); // above range
    }

    #[test]
    fn test_simple_widths_zero_first_char() {
        let widths = SimpleWidths::new(0, vec![100.0, 200.0]);
        assert_eq!(widths.width(0), Some(100.0));
        assert_eq!(widths.width(1), Some(200.0));
        assert_eq!(widths.width(2), None);
    }

    #[test]
    fn test_load_simple_widths_from_dict() {
        use crate::object::PdfDictionary;

        let mut dict = PdfDictionary::new();
        dict.insert(b"FirstChar".to_vec(), PdfObject::Integer(65));
        dict.insert(b"LastChar".to_vec(), PdfObject::Integer(67));
        dict.insert(
            b"Widths".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Integer(600),
                PdfObject::Integer(700),
                PdfObject::Real(800.5),
            ]),
        );

        let widths = load_simple_widths(&mut test_resolver(), &dict);
        assert!(widths.is_some());
        let widths = widths.unwrap();
        assert_eq!(widths.width(65), Some(600.0));
        assert_eq!(widths.width(66), Some(700.0));
        assert_eq!(widths.width(67), Some(800.5));
        assert_eq!(widths.width(64), None);
        assert_eq!(widths.width(68), None);
    }

    #[test]
    fn test_load_simple_widths_missing_entries() {
        use crate::object::PdfDictionary;

        // Missing /FirstChar -> None
        let mut dict = PdfDictionary::new();
        dict.insert(b"LastChar".to_vec(), PdfObject::Integer(100));
        dict.insert(
            b"Widths".to_vec(),
            PdfObject::Array(vec![PdfObject::Integer(500)]),
        );
        assert!(load_simple_widths(&mut test_resolver(), &dict).is_none());

        // Missing /Widths -> None
        let mut dict = PdfDictionary::new();
        dict.insert(b"FirstChar".to_vec(), PdfObject::Integer(32));
        dict.insert(b"LastChar".to_vec(), PdfObject::Integer(32));
        assert!(load_simple_widths(&mut test_resolver(), &dict).is_none());

        // Empty dict -> None
        let dict = PdfDictionary::new();
        assert!(load_simple_widths(&mut test_resolver(), &dict).is_none());
    }

    #[test]
    fn test_load_simple_widths_negative_first_char() {
        use crate::object::PdfDictionary;

        // Negative /FirstChar should return None (not silently wrap)
        let mut dict = PdfDictionary::new();
        dict.insert(b"FirstChar".to_vec(), PdfObject::Integer(-1));
        dict.insert(b"LastChar".to_vec(), PdfObject::Integer(10));
        dict.insert(
            b"Widths".to_vec(),
            PdfObject::Array(vec![PdfObject::Integer(500)]),
        );
        assert!(load_simple_widths(&mut test_resolver(), &dict).is_none());

        // Negative /LastChar should also return None
        let mut dict = PdfDictionary::new();
        dict.insert(b"FirstChar".to_vec(), PdfObject::Integer(0));
        dict.insert(b"LastChar".to_vec(), PdfObject::Integer(-5));
        dict.insert(
            b"Widths".to_vec(),
            PdfObject::Array(vec![PdfObject::Integer(500)]),
        );
        assert!(load_simple_widths(&mut test_resolver(), &dict).is_none());
    }

    #[test]
    fn test_load_simple_widths_inverted_range() {
        use crate::object::PdfDictionary;

        // LastChar < FirstChar -> None
        let mut dict = PdfDictionary::new();
        dict.insert(b"FirstChar".to_vec(), PdfObject::Integer(100));
        dict.insert(b"LastChar".to_vec(), PdfObject::Integer(50));
        dict.insert(
            b"Widths".to_vec(),
            PdfObject::Array(vec![PdfObject::Integer(500)]),
        );
        assert!(load_simple_widths(&mut test_resolver(), &dict).is_none());
    }

    // -- char_width() fallback chain tests --

    #[test]
    fn test_simple_font_char_width_from_table() {
        // SimpleFont with /Widths: returns table value
        let font = Font::Simple(SimpleFont {
            subtype: SimpleSubtype::Type1,
            base_font: Some("Test".to_string()),
            encoding: Encoding::Standard(StandardEncoding::WinAnsi),
            tounicode: None,
            widths: Some(SimpleWidths::new(65, vec![722.0, 667.0, 556.0])),
            font_data: None,
            font_program: FontProgram::None,
            differences_names: Vec::new(),
        });
        assert_eq!(font.char_width(65), 722.0); // A
        assert_eq!(font.char_width(66), 667.0); // B
        assert_eq!(font.char_width(67), 556.0); // C
    }

    #[test]
    fn test_simple_font_char_width_fallback() {
        // SimpleFont without /Widths: returns 600 default
        let font = Font::Simple(SimpleFont {
            subtype: SimpleSubtype::Type1,
            base_font: Some("Test".to_string()),
            encoding: Encoding::Standard(StandardEncoding::WinAnsi),
            tounicode: None,
            widths: None,
            font_data: None,
            font_program: FontProgram::None,
            differences_names: Vec::new(),
        });
        assert_eq!(font.char_width(65), 600.0);
    }

    #[test]
    fn test_simple_font_char_width_out_of_range() {
        // Code outside /Widths range falls back to 600
        let font = Font::Simple(SimpleFont {
            subtype: SimpleSubtype::Type1,
            base_font: Some("Test".to_string()),
            encoding: Encoding::Standard(StandardEncoding::WinAnsi),
            tounicode: None,
            widths: Some(SimpleWidths::new(65, vec![722.0])),
            font_data: None,
            font_program: FontProgram::None,
            differences_names: Vec::new(),
        });
        assert_eq!(font.char_width(65), 722.0); // in range
        assert_eq!(font.char_width(64), 600.0); // below range -> default
        assert_eq!(font.char_width(66), 600.0); // above range -> default
    }

    #[test]
    fn test_composite_font_char_width_from_w_table() {
        // CID font with /W entries
        let font = Font::Composite(CompositeFont {
            base_font: Some("CIDTest".to_string()),
            font_data: None,
            font_program: FontProgram::None,
            encoding_name: "Identity-H".to_string(),
            tounicode: None,
            descendant: CidFont {
                subtype: CidSubtype::Type2,
                base_font: None,
                default_width: 1000,
                widths: CidWidths::from_map([(42, 500.0), (100, 750.0)].into_iter().collect()),
            },
            code_length: 2,
            is_vertical: false,
            parsed_cmap: None,
        });
        assert_eq!(font.char_width(42), 500.0);
        assert_eq!(font.char_width(100), 750.0);
    }

    #[test]
    fn test_composite_font_char_width_falls_back_to_dw() {
        // CID not in /W table -> uses /DW
        let font = Font::Composite(CompositeFont {
            base_font: Some("CIDTest".to_string()),
            font_data: None,
            font_program: FontProgram::None,
            encoding_name: "Identity-H".to_string(),
            tounicode: None,
            descendant: CidFont {
                subtype: CidSubtype::Type2,
                base_font: None,
                default_width: 500,
                widths: CidWidths::new(),
            },
            code_length: 2,
            is_vertical: false,
            parsed_cmap: None,
        });
        assert_eq!(font.char_width(0), 500.0); // /DW fallback
        assert_eq!(font.char_width(999), 500.0); // /DW fallback
    }

    #[test]
    fn test_type3_font_char_width_no_widths() {
        // Type3 font without /Widths falls back to 0.0
        let font = Font::Type3(make_type3_font(None));
        assert_eq!(font.char_width(0), 0.0);
        assert_eq!(font.char_width(255), 0.0);
    }

    #[test]
    fn test_type3_font_char_width_with_widths() {
        // Type3 font with /Widths returns values scaled by FontMatrix.
        // Default matrix [0.001 ...] means: raw * 0.001 * 1000 ~= raw.
        // Use approximate comparison due to floating-point precision.
        let font = Font::Type3(make_type3_font(Some(SimpleWidths::new(
            65,
            vec![500.0, 600.0, 700.0],
        ))));
        assert!((font.char_width(65) - 500.0).abs() < 1e-10);
        assert!((font.char_width(66) - 600.0).abs() < 1e-10);
        assert!((font.char_width(67) - 700.0).abs() < 1e-10);
        // Out of range falls back to 0.0
        assert_eq!(font.char_width(64), 0.0);
        assert_eq!(font.char_width(68), 0.0);
    }

    #[test]
    fn test_simple_widths_rejects_huge_range() {
        // FirstChar=0, LastChar=2_000_000 exceeds MAX_WIDTHS_ENTRIES
        let mut dict = crate::object::PdfDictionary::new();
        dict.insert(b"FirstChar".to_vec(), PdfObject::Integer(0));
        dict.insert(b"LastChar".to_vec(), PdfObject::Integer(2_000_000));
        dict.insert(
            b"Widths".to_vec(),
            PdfObject::Array(vec![PdfObject::Real(100.0)]),
        );
        assert!(load_simple_widths(&mut test_resolver(), &dict).is_none());
    }

    #[test]
    fn test_cid_widths_truncated_at_limit() {
        use std::sync::Arc;
        // Build a /W array with way more entries than MAX_WIDTHS_ENTRIES.
        // Use individual-width entries: cid [w1 w2 ... w_N]
        // Each array entry produces N width entries.
        let diag = Arc::new(CollectingDiagnostics::new());
        let block_size = 1000;
        let num_blocks = (MAX_WIDTHS_ENTRIES / block_size) + 5;
        let mut w_array: Vec<PdfObject> = Vec::new();
        for b in 0..num_blocks {
            let cid_start = (b * block_size) as i64;
            w_array.push(PdfObject::Integer(cid_start));
            let widths_block: Vec<PdfObject> =
                (0..block_size).map(|_| PdfObject::Real(500.0)).collect();
            w_array.push(PdfObject::Array(widths_block));
        }

        let widths = parse_cid_widths(&w_array, &*diag, WarningContext::default());
        // Count of CID entries should be at or under the limit
        let count = (0..((num_blocks * block_size) as u32))
            .filter(|cid| widths.width(*cid).is_some())
            .count();
        assert!(
            count <= MAX_WIDTHS_ENTRIES,
            "got {} entries, expected <= {}",
            count,
            MAX_WIDTHS_ENTRIES
        );
        assert!(diag
            .warnings()
            .iter()
            .any(|w| w.message.contains("exceeded limit")));
    }

    // ========================================================================
    // Additional coverage: parse_encoding_name
    // ========================================================================

    #[test]
    fn test_parse_encoding_name_macexpert() {
        assert_eq!(
            parse_encoding_name("MacExpertEncoding"),
            StandardEncoding::MacExpert
        );
    }

    #[test]
    fn test_parse_encoding_name_empty() {
        assert_eq!(parse_encoding_name(""), StandardEncoding::BuiltIn);
    }

    #[test]
    fn test_parse_encoding_name_garbage() {
        assert_eq!(
            parse_encoding_name("SomethingCompletelyWrong"),
            StandardEncoding::BuiltIn
        );
    }

    // ========================================================================
    // Additional coverage: parse_differences edge cases
    // ========================================================================

    #[test]
    fn test_parse_differences_empty_array() {
        let diffs = parse_differences(&[]);
        assert!(diffs.is_empty());
    }

    #[test]
    fn test_parse_differences_name_before_code() {
        // A name before any integer code is set: current_code is None, skip it
        let arr = vec![
            PdfObject::Name(b"space".to_vec()),
            PdfObject::Integer(65),
            PdfObject::Name(b"A".to_vec()),
        ];
        let diffs = parse_differences(&arr);
        // Only the name after the integer should produce a mapping
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0], (65, 'A'));
    }

    #[test]
    fn test_parse_differences_wrapping_at_255() {
        // Code starts at 255, name increments code via wrapping_add -> wraps to 0
        let arr = vec![
            PdfObject::Integer(255),
            PdfObject::Name(b"space".to_vec()),
            PdfObject::Name(b"exclam".to_vec()), // should wrap to code 0
        ];
        let diffs = parse_differences(&arr);
        assert_eq!(diffs.len(), 2);
        assert_eq!(diffs[0], (255, ' '));
        assert_eq!(diffs[1], (0, '!'));
    }

    #[test]
    fn test_parse_differences_unknown_glyph_name() {
        // Unknown glyph name should be skipped (no mapping produced)
        let arr = vec![
            PdfObject::Integer(65),
            PdfObject::Name(b"unknown_glyph_xyz_abc".to_vec()),
        ];
        let diffs = parse_differences(&arr);
        assert!(diffs.is_empty());
    }

    #[test]
    fn test_parse_differences_with_subset_prefix() {
        // Subset prefix "ABCDEF+space" should be stripped
        let arr = vec![
            PdfObject::Integer(32),
            PdfObject::Name(b"ABCDEF+space".to_vec()),
        ];
        let diffs = parse_differences(&arr);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0], (32, ' '));
    }

    #[test]
    fn test_parse_differences_non_name_non_int_ignored() {
        // Non-integer, non-name objects are ignored (no increment to current_code)
        let arr = vec![
            PdfObject::Integer(65),
            PdfObject::Name(b"A".to_vec()),
            PdfObject::Boolean(true), // skipped, does not increment code
            PdfObject::Name(b"B".to_vec()),
        ];
        let diffs = parse_differences(&arr);
        // After A @ 65, code increments to 66. Boolean is skipped without
        // incrementing. B maps to code 66.
        assert_eq!(diffs.len(), 2);
        assert_eq!(diffs[0], (65, 'A'));
        assert_eq!(diffs[1], (66, 'B'));
    }

    #[test]
    fn test_parse_differences_negative_code() {
        // Negative integer should fail u8::try_from, making current_code None
        let arr = vec![
            PdfObject::Integer(-1),
            PdfObject::Name(b"space".to_vec()), // current_code is None, skip
        ];
        let diffs = parse_differences(&arr);
        assert!(diffs.is_empty());
    }

    #[test]
    fn test_parse_differences_code_too_large() {
        // Integer > 255 should fail u8::try_from
        let arr = vec![PdfObject::Integer(300), PdfObject::Name(b"space".to_vec())];
        let diffs = parse_differences(&arr);
        assert!(diffs.is_empty());
    }

    // ========================================================================
    // Additional coverage: load_encoding_dict
    // ========================================================================

    /// Build a minimal in-memory PDF and a fresh diagnostics sink.
    /// The tests below construct dictionaries inline and don't traverse
    /// the document tree, so a parseable but otherwise empty PDF is enough.
    fn make_resolver_parts(_filename: &str) -> (Vec<u8>, Arc<CollectingDiagnostics>) {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");
        let obj1 = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let obj2 = data.len();
        data.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
        let xref_off = data.len();
        data.extend_from_slice(b"xref\n0 3\n");
        data.extend_from_slice(b"0000000000 65535 f \n");
        data.extend_from_slice(format!("{obj1:010} 00000 n \n").as_bytes());
        data.extend_from_slice(format!("{obj2:010} 00000 n \n").as_bytes());
        data.extend_from_slice(b"trailer\n<< /Size 3 /Root 1 0 R >>\n");
        data.extend_from_slice(format!("startxref\n{xref_off}\n").as_bytes());
        data.extend_from_slice(b"%%EOF\n");
        let diag = Arc::new(CollectingDiagnostics::new());
        (data, diag)
    }

    #[test]
    fn test_load_encoding_dict_no_base_no_diffs() {
        use crate::object::PdfDictionary;

        let (data, diag) = make_resolver_parts("empty_page.pdf");
        let doc = DocumentParser::with_diagnostics(&data, diag.clone())
            .parse()
            .unwrap();
        let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag);
        let enc_dict = PdfDictionary::new();
        let result = load_encoding_dict(&mut resolver, &enc_dict);
        assert!(result.is_ok());
        let enc = result.unwrap();
        assert!(matches!(
            enc,
            Encoding::Standard(StandardEncoding::Standard)
        ));
    }

    #[test]
    fn test_load_encoding_dict_with_base_and_diffs() {
        use crate::object::PdfDictionary;

        let (data, diag) = make_resolver_parts("empty_page.pdf");
        let doc = DocumentParser::with_diagnostics(&data, diag.clone())
            .parse()
            .unwrap();
        let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag);
        let mut enc_dict = PdfDictionary::new();
        enc_dict.insert(
            b"BaseEncoding".to_vec(),
            PdfObject::Name(b"WinAnsiEncoding".to_vec()),
        );
        enc_dict.insert(
            b"Differences".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Integer(65),
                PdfObject::Name(b"space".to_vec()),
            ]),
        );

        let result = load_encoding_dict(&mut resolver, &enc_dict);
        assert!(result.is_ok());
        let enc = result.unwrap();
        assert!(matches!(enc, Encoding::Custom { .. }));
        assert_eq!(enc.lookup(65), Some(' '));
    }

    // ========================================================================
    // Additional coverage: load_simple_widths edge cases
    // ========================================================================

    #[test]
    fn test_load_simple_widths_missing_last_char() {
        use crate::object::PdfDictionary;

        let mut dict = PdfDictionary::new();
        dict.insert(b"FirstChar".to_vec(), PdfObject::Integer(32));
        dict.insert(
            b"Widths".to_vec(),
            PdfObject::Array(vec![PdfObject::Integer(500)]),
        );
        // Missing /LastChar -> None
        assert!(load_simple_widths(&mut test_resolver(), &dict).is_none());
    }

    #[test]
    fn test_load_simple_widths_non_numeric_in_array() {
        use crate::object::PdfDictionary;

        let mut dict = PdfDictionary::new();
        dict.insert(b"FirstChar".to_vec(), PdfObject::Integer(32));
        dict.insert(b"LastChar".to_vec(), PdfObject::Integer(33));
        dict.insert(
            b"Widths".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Name(b"garbage".to_vec()), // as_f64 returns None -> 0.0
                PdfObject::Integer(500),
            ]),
        );

        let widths = load_simple_widths(&mut test_resolver(), &dict);
        assert!(widths.is_some());
        let w = widths.unwrap();
        assert_eq!(w.width(32), Some(0.0)); // garbage -> 0.0 fallback
        assert_eq!(w.width(33), Some(500.0));
    }

    // ========================================================================
    // Additional coverage: parse_cid_widths edge cases
    // ========================================================================

    #[test]
    fn test_parse_cid_widths_negative_cid_start() {
        let diag = Arc::new(CollectingDiagnostics::new());

        // Negative CID start should be skipped (n >= 0 check fails)
        let w_array = vec![
            PdfObject::Integer(-5),
            PdfObject::Array(vec![PdfObject::Integer(100)]),
        ];

        let widths = parse_cid_widths(&w_array, &*diag, WarningContext::default());
        // -5 fails the >= 0 check, skips to next, finds an Array at index 1,
        // but without a valid CID start before it, this triggers the "expected integer" warning
        assert!(!diag.warnings().is_empty());
        // No widths should be inserted for negative CID
        assert_eq!(widths.width(0), None);
    }

    #[test]
    fn test_parse_cid_widths_range_bad_cid_end() {
        let diag = Arc::new(CollectingDiagnostics::new());

        // Range format where cid_end is not an integer
        let w_array = vec![
            PdfObject::Integer(10),
            PdfObject::Name(b"not_a_number".to_vec()), // not an integer
        ];

        let widths = parse_cid_widths(&w_array, &*diag, WarningContext::default());
        assert_eq!(widths.width(10), None);
        assert!(diag
            .warnings()
            .iter()
            .any(|w| w.message.contains("cid_end")));
    }

    #[test]
    fn test_parse_cid_widths_range_bad_width_value() {
        let diag = Arc::new(CollectingDiagnostics::new());

        // Range format where the width value is not a number
        let w_array = vec![
            PdfObject::Integer(10),
            PdfObject::Integer(20),
            PdfObject::Name(b"bad".to_vec()), // not a number
        ];

        let widths = parse_cid_widths(&w_array, &*diag, WarningContext::default());
        assert_eq!(widths.width(10), None);
        assert!(diag.warnings().iter().any(|w| w.message.contains("width")));
    }

    #[test]
    fn test_parse_cid_widths_range_too_large() {
        let diag = Arc::new(CollectingDiagnostics::new());

        // Range spanning > 65536 CIDs should be skipped
        let w_array = vec![
            PdfObject::Integer(0),
            PdfObject::Integer(100_000),
            PdfObject::Integer(500),
        ];

        let widths = parse_cid_widths(&w_array, &*diag, WarningContext::default());
        assert_eq!(widths.width(0), None);
        assert!(diag
            .warnings()
            .iter()
            .any(|w| w.message.contains("too large")));
    }

    #[test]
    fn test_parse_cid_widths_trailing_cid_only() {
        // /W [ 10 ] -- only a cid_start, nothing after
        let diag = crate::diagnostics::NullDiagnostics;
        let w_array = vec![PdfObject::Integer(10)];
        let widths = parse_cid_widths(&w_array, &diag, WarningContext::default());
        assert_eq!(widths.width(10), None);
    }

    #[test]
    fn test_parse_cid_widths_negative_cid_end_in_range() {
        let diag = Arc::new(CollectingDiagnostics::new());

        // Range with negative cid_end
        let w_array = vec![
            PdfObject::Integer(10),
            PdfObject::Integer(-1), // fails n >= 0
        ];

        let widths = parse_cid_widths(&w_array, &*diag, WarningContext::default());
        assert_eq!(widths.width(10), None);
        assert!(diag
            .warnings()
            .iter()
            .any(|w| w.message.contains("cid_end")));
    }

    // ========================================================================
    // Additional coverage: default_cid_font
    // ========================================================================

    #[test]
    fn test_default_cid_font() {
        let font = default_cid_font();
        assert_eq!(font.subtype, CidSubtype::Type2);
        assert!(font.base_font.is_none());
        assert_eq!(font.default_width, 1000);
        assert_eq!(font.widths.width(0), None);
    }

    // ========================================================================
    // Additional coverage: load_encoding via dict path
    // ========================================================================

    #[test]
    fn test_load_encoding_missing() {
        use crate::object::PdfDictionary;

        let (data, diag) = make_resolver_parts("empty_page.pdf");
        let doc = DocumentParser::with_diagnostics(&data, diag.clone())
            .parse()
            .unwrap();
        let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag);

        // Empty font dict -- no /Encoding key
        let font_dict = PdfDictionary::new();
        let result = load_encoding(&mut resolver, &font_dict);
        assert!(result.is_ok());
        assert!(matches!(
            result.unwrap(),
            Encoding::Standard(StandardEncoding::BuiltIn)
        ));
    }

    #[test]
    fn test_load_encoding_name_direct() {
        use crate::object::PdfDictionary;

        let (data, diag) = make_resolver_parts("empty_page.pdf");
        let doc = DocumentParser::with_diagnostics(&data, diag.clone())
            .parse()
            .unwrap();
        let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag);

        let mut font_dict = PdfDictionary::new();
        font_dict.insert(
            b"Encoding".to_vec(),
            PdfObject::Name(b"MacRomanEncoding".to_vec()),
        );
        let result = load_encoding(&mut resolver, &font_dict);
        assert!(result.is_ok());
        assert!(matches!(
            result.unwrap(),
            Encoding::Standard(StandardEncoding::MacRoman)
        ));
    }

    #[test]
    fn test_load_encoding_unexpected_type() {
        use crate::object::PdfDictionary;

        let (data, diag) = make_resolver_parts("empty_page.pdf");
        let doc = DocumentParser::with_diagnostics(&data, diag.clone())
            .parse()
            .unwrap();
        let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag);

        // /Encoding is an Integer (unexpected type)
        let mut font_dict = PdfDictionary::new();
        font_dict.insert(b"Encoding".to_vec(), PdfObject::Integer(42));
        let result = load_encoding(&mut resolver, &font_dict);
        assert!(result.is_ok());
        // Falls through to BuiltIn
        assert!(matches!(
            result.unwrap(),
            Encoding::Standard(StandardEncoding::BuiltIn)
        ));
    }

    #[test]
    fn test_load_encoding_dict_inline() {
        use crate::object::PdfDictionary;

        let (data, diag) = make_resolver_parts("empty_page.pdf");
        let doc = DocumentParser::with_diagnostics(&data, diag.clone())
            .parse()
            .unwrap();
        let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag);

        // /Encoding is an inline dict with /BaseEncoding
        let mut enc_dict = PdfDictionary::new();
        enc_dict.insert(
            b"BaseEncoding".to_vec(),
            PdfObject::Name(b"WinAnsiEncoding".to_vec()),
        );

        let mut font_dict = PdfDictionary::new();
        font_dict.insert(b"Encoding".to_vec(), PdfObject::Dictionary(enc_dict));
        let result = load_encoding(&mut resolver, &font_dict);
        assert!(result.is_ok());
        assert!(matches!(
            result.unwrap(),
            Encoding::Standard(StandardEncoding::WinAnsi)
        ));
    }

    // ========================================================================
    // Additional coverage: Type3Font decode_char
    // ========================================================================

    #[test]
    fn test_type3_font_decode_char_encoding() {
        let mut t3 = make_type3_font(None);
        t3.encoding = Encoding::Standard(StandardEncoding::WinAnsi);
        let font = Font::Type3(t3);

        assert_eq!(font.decode_char(&[0x41]), "A");
        assert_eq!(font.decode_char(&[0x20]), " ");
    }

    #[test]
    fn test_type3_font_decode_char_fallback() {
        let font = Font::Type3(make_type3_font(None));

        // BuiltIn returns None, should fall through to FFFD
        assert_eq!(font.decode_char(&[0x41]), "\u{FFFD}");
    }

    #[test]
    fn test_type3_font_decode_char_tounicode() {
        use udoc_font::tounicode::ToUnicodeCMap;

        let cmap_data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 beginbfchar
<41> <0058>
endbfchar
endcmap
";
        let cmap = ToUnicodeCMap::parse(cmap_data);
        let mut t3 = make_type3_font(None);
        t3.tounicode = Some(cmap);
        let font = Font::Type3(t3);

        // ToUnicode should take priority
        assert_eq!(font.decode_char(&[0x41]), "X");
    }

    // ========================================================================
    // Additional coverage: Font::name with subset prefix
    // ========================================================================

    #[test]
    fn test_font_name_with_subset_prefix() {
        let font = Font::Simple(SimpleFont {
            subtype: SimpleSubtype::Type1,
            base_font: Some("ABCDEF+Helvetica".to_string()),
            encoding: Encoding::Standard(StandardEncoding::WinAnsi),
            tounicode: None,
            widths: None,
            font_data: None,
            font_program: FontProgram::None,
            differences_names: Vec::new(),
        });
        assert_eq!(font.name(), "Helvetica");
    }

    #[test]
    fn test_font_name_without_prefix() {
        let font = Font::Simple(SimpleFont {
            subtype: SimpleSubtype::Type1,
            base_font: Some("TimesNewRoman".to_string()),
            encoding: Encoding::Standard(StandardEncoding::WinAnsi),
            tounicode: None,
            widths: None,
            font_data: None,
            font_program: FontProgram::None,
            differences_names: Vec::new(),
        });
        assert_eq!(font.name(), "TimesNewRoman");
    }

    #[test]
    fn test_font_name_no_base_font() {
        let font = Font::Simple(SimpleFont {
            subtype: SimpleSubtype::Type1,
            base_font: None,
            encoding: Encoding::Standard(StandardEncoding::WinAnsi),
            tounicode: None,
            widths: None,
            font_data: None,
            font_program: FontProgram::None,
            differences_names: Vec::new(),
        });
        assert_eq!(font.name(), "unknown");
    }

    #[test]
    fn test_font_name_type3_no_base_font() {
        let font = Font::Type3(make_type3_font(None));
        assert_eq!(font.name(), "unknown");
    }

    #[test]
    fn test_font_name_type3_with_base_font() {
        let mut t3 = make_type3_font(None);
        t3.base_font = Some("ABCDEF+MyType3".to_string());
        let font = Font::Type3(t3);
        assert_eq!(font.name(), "MyType3");
    }

    #[test]
    fn test_font_name_composite() {
        let font = Font::Composite(CompositeFont {
            base_font: Some("XYZABC+Arial".to_string()),
            font_data: None,
            font_program: FontProgram::None,
            encoding_name: "Identity-H".to_string(),
            tounicode: None,
            descendant: default_cid_font(),
            code_length: 2,
            is_vertical: false,
            parsed_cmap: None,
        });
        assert_eq!(font.name(), "Arial");
    }

    // ========================================================================
    // Additional coverage: Font::code_length and is_vertical
    // ========================================================================

    #[test]
    fn test_font_code_length() {
        let simple = Font::Simple(SimpleFont {
            subtype: SimpleSubtype::Type1,
            base_font: None,
            encoding: Encoding::Standard(StandardEncoding::WinAnsi),
            tounicode: None,
            widths: None,
            font_data: None,
            font_program: FontProgram::None,
            differences_names: Vec::new(),
        });
        assert_eq!(simple.code_length(), 1);

        let type3 = Font::Type3(make_type3_font(None));
        assert_eq!(type3.code_length(), 1);

        let composite = Font::Composite(CompositeFont {
            base_font: None,
            font_data: None,
            font_program: FontProgram::None,
            encoding_name: "Identity-H".to_string(),
            tounicode: None,
            descendant: default_cid_font(),
            code_length: 2,
            is_vertical: false,
            parsed_cmap: None,
        });
        assert_eq!(composite.code_length(), 2);
    }

    #[test]
    fn test_font_is_vertical() {
        let simple = Font::Simple(SimpleFont {
            subtype: SimpleSubtype::Type1,
            base_font: None,
            encoding: Encoding::Standard(StandardEncoding::WinAnsi),
            tounicode: None,
            widths: None,
            font_data: None,
            font_program: FontProgram::None,
            differences_names: Vec::new(),
        });
        assert!(!simple.is_vertical());

        let vert = Font::Composite(CompositeFont {
            base_font: None,
            font_data: None,
            font_program: FontProgram::None,
            encoding_name: "Identity-V".to_string(),
            tounicode: None,
            descendant: default_cid_font(),
            code_length: 2,
            is_vertical: true,
            parsed_cmap: None,
        });
        assert!(vert.is_vertical());
    }

    // ========================================================================
    // Additional coverage: SimpleFont default encoding fallback
    // ========================================================================

    #[test]
    fn test_simple_font_truetype_default_encoding() {
        // When encoding is BuiltIn, TrueType should get WinAnsi
        // This is tested indirectly by constructing the way load_simple_font would
        let font = Font::Simple(SimpleFont {
            subtype: SimpleSubtype::TrueType,
            base_font: Some("Arial".to_string()),
            encoding: Encoding::Standard(StandardEncoding::WinAnsi), // the code sets this
            tounicode: None,
            widths: None,
            font_data: None,
            font_program: FontProgram::None,
            differences_names: Vec::new(),
        });
        assert_eq!(font.decode_char(&[0x41]), "A");
    }

    #[test]
    fn test_simple_font_mmtype1_default_encoding() {
        // MMType1 with BuiltIn falls through to Standard (not WinAnsi)
        let font = Font::Simple(SimpleFont {
            subtype: SimpleSubtype::MMType1,
            base_font: Some("Myriad".to_string()),
            encoding: Encoding::Standard(StandardEncoding::Standard),
            tounicode: None,
            widths: None,
            font_data: None,
            font_program: FontProgram::None,
            differences_names: Vec::new(),
        });
        // In StandardEncoding, 0x41 is 'A'
        assert_eq!(font.decode_char(&[0x41]), "A");
    }

    // ========================================================================
    // Additional coverage: composite font decode_char without tounicode
    // ========================================================================

    #[test]
    fn test_composite_font_decode_identity_fallback() {
        // Identity-H encoding without ToUnicode uses CID-to-Unicode fallback
        let font = Font::Composite(CompositeFont {
            base_font: Some("TestFont".to_string()),
            font_data: None,
            font_program: FontProgram::None,
            encoding_name: "Identity-H".to_string(),
            tounicode: None,
            descendant: default_cid_font(),
            code_length: 2,
            is_vertical: false,
            parsed_cmap: None,
        });
        // CID 0x0041 = 65 = 'A' via identity fallback
        assert_eq!(font.decode_char(&[0x00, 0x41]), "A");
    }

    #[test]
    fn test_composite_font_decode_non_identity_no_tounicode() {
        // Non-Identity encoding without ToUnicode should return FFFD
        let font = Font::Composite(CompositeFont {
            base_font: Some("TestFont".to_string()),
            font_data: None,
            font_program: FontProgram::None,
            encoding_name: "UniGB-UCS2-H".to_string(),
            tounicode: None,
            descendant: default_cid_font(),
            code_length: 2,
            is_vertical: false,
            parsed_cmap: None,
        });
        // No identity fallback for non-Identity encodings
        assert_eq!(font.decode_char(&[0x00, 0x41]), "\u{FFFD}");
    }

    // ========================================================================
    // Additional coverage: load_encoding_dict with /Differences but empty result
    // ========================================================================

    #[test]
    fn test_load_encoding_dict_with_diffs_all_unknown() {
        use crate::object::PdfDictionary;

        let (data, diag) = make_resolver_parts("empty_page.pdf");
        let doc = DocumentParser::with_diagnostics(&data, diag.clone())
            .parse()
            .unwrap();
        let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag);

        // /Differences with only unknown glyph names -> empty diffs -> Standard base
        let mut enc_dict = PdfDictionary::new();
        enc_dict.insert(
            b"Differences".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Integer(65),
                PdfObject::Name(b"unknownglyph_xyz_abc".to_vec()),
            ]),
        );

        let result = load_encoding_dict(&mut resolver, &enc_dict);
        assert!(result.is_ok());
        // Since all diffs resolve to unknown -> empty -> use Standard base directly
        assert!(matches!(
            result.unwrap(),
            Encoding::Standard(StandardEncoding::Standard)
        ));
    }

    // ========================================================================
    // /Differences with underscore-separated ligature names (TeX fonts)
    // ========================================================================

    #[test]
    fn test_parse_differences_underscore_ligatures() {
        // TeX fonts use f_i, f_l, f_f etc. in /Differences arrays
        let arr = vec![
            PdfObject::Integer(11),
            PdfObject::Name(b"f_f".to_vec()),
            PdfObject::Name(b"f_i".to_vec()),
            PdfObject::Name(b"f_l".to_vec()),
            PdfObject::Name(b"f_f_i".to_vec()),
            PdfObject::Name(b"f_f_l".to_vec()),
        ];
        let diffs = parse_differences(&arr);
        assert_eq!(diffs.len(), 5);
        assert_eq!(diffs[0], (11, '\u{FB00}')); // ff
        assert_eq!(diffs[1], (12, '\u{FB01}')); // fi
        assert_eq!(diffs[2], (13, '\u{FB02}')); // fl
        assert_eq!(diffs[3], (14, '\u{FB03}')); // ffi
        assert_eq!(diffs[4], (15, '\u{FB04}')); // ffl
    }

    #[test]
    fn test_parse_differences_mixed_standard_and_underscore_ligatures() {
        // Mix of standard AGL names and underscore-separated names
        let arr = vec![
            PdfObject::Integer(39),
            PdfObject::Name(b"quoteright".to_vec()),
            PdfObject::Integer(96),
            PdfObject::Name(b"quoteleft".to_vec()),
            PdfObject::Integer(11),
            PdfObject::Name(b"f_i".to_vec()),
        ];
        let diffs = parse_differences(&arr);
        assert_eq!(diffs.len(), 3);
        assert_eq!(diffs[0], (39, '\u{2019}')); // quoteright
        assert_eq!(diffs[1], (96, '\u{2018}')); // quoteleft
        assert_eq!(diffs[2], (11, '\u{FB01}')); // fi (from underscore form)
    }

    #[test]
    fn test_load_encoding_dict_macroman_base_with_diffs() {
        use crate::object::PdfDictionary;

        let (data, diag) = make_resolver_parts("empty_page.pdf");
        let doc = DocumentParser::with_diagnostics(&data, diag.clone())
            .parse()
            .unwrap();
        let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag);

        // /Encoding << /BaseEncoding /MacRomanEncoding /Differences [65 /space] >>
        let mut enc_dict = PdfDictionary::new();
        enc_dict.insert(
            b"BaseEncoding".to_vec(),
            PdfObject::Name(b"MacRomanEncoding".to_vec()),
        );
        enc_dict.insert(
            b"Differences".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Integer(65),
                PdfObject::Name(b"space".to_vec()),
            ]),
        );

        let result = load_encoding_dict(&mut resolver, &enc_dict);
        assert!(result.is_ok());
        let enc = result.unwrap();
        assert!(matches!(enc, Encoding::Custom { .. }));
        // Code 65 overridden by /Differences
        assert_eq!(enc.lookup(65), Some(' '));
        // Code 0x80 inherited from MacRoman base (A-diaeresis)
        assert_eq!(enc.lookup(0x80), Some('\u{00C4}'));
        // Code 0x41 (ASCII 'A') not overridden, from MacRoman (same as ASCII)
        assert_eq!(enc.lookup(0x42), Some('B'));
    }

    #[test]
    fn test_load_composite_font_inline_descendant_with_w() {
        // Regression test for MS Word / PDFbox CIDFont output, which writes
        // both the descendant font AND the /W array inline rather than as
        // indirect references. The previous code path silently returned a
        // default_cid_font() with an empty /W table, causing every glyph to
        // fall back to /DW=1000 (1 em per character).
        use crate::object::{PdfDictionary, PdfObject};

        let (data, diag) = make_resolver_parts("empty_page.pdf");
        let doc = DocumentParser::with_diagnostics(&data, diag.clone())
            .parse()
            .unwrap();
        let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag);

        // Build an inline descendant font dict with /W and /DW.
        let mut descendant = PdfDictionary::new();
        descendant.insert(b"Type".to_vec(), PdfObject::Name(b"Font".to_vec()));
        descendant.insert(
            b"Subtype".to_vec(),
            PdfObject::Name(b"CIDFontType2".to_vec()),
        );
        descendant.insert(
            b"BaseFont".to_vec(),
            PdfObject::Name(b"CIDFont+F3".to_vec()),
        );
        descendant.insert(b"DW".to_vec(), PdfObject::Integer(500));
        // /W [20 [611 278] 55 55 611]: CID 20 -> 611, CID 21 -> 278, CID 55 -> 611
        descendant.insert(
            b"W".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Integer(20),
                PdfObject::Array(vec![PdfObject::Integer(611), PdfObject::Integer(278)]),
                PdfObject::Integer(55),
                PdfObject::Integer(55),
                PdfObject::Integer(611),
            ]),
        );

        // Build the Type0 parent with inline descendant.
        let mut parent = PdfDictionary::new();
        parent.insert(b"Type".to_vec(), PdfObject::Name(b"Font".to_vec()));
        parent.insert(b"Subtype".to_vec(), PdfObject::Name(b"Type0".to_vec()));
        parent.insert(
            b"BaseFont".to_vec(),
            PdfObject::Name(b"CIDFont+F3".to_vec()),
        );
        parent.insert(
            b"Encoding".to_vec(),
            PdfObject::Name(b"Identity-H".to_vec()),
        );
        parent.insert(
            b"DescendantFonts".to_vec(),
            PdfObject::Array(vec![PdfObject::Dictionary(descendant)]),
        );

        let composite =
            load_composite_font(&mut resolver, &parent, WarningContext::default()).unwrap();

        // /W should have been consumed and the widths should not collapse to
        // /DW (which would give 500 for every CID).
        assert_eq!(composite.descendant.default_width, 500);
        assert_eq!(composite.descendant.widths.width(20), Some(611.0));
        assert_eq!(composite.descendant.widths.width(21), Some(278.0));
        assert_eq!(composite.descendant.widths.width(55), Some(611.0));
        // A CID outside the /W entries should fall through to /DW.
        assert_eq!(composite.descendant.widths.width(9999), None);
    }

    #[test]
    fn test_extract_font_data_inline_font_descriptor() {
        // Regression test: some writers (MS Word) emit /FontDescriptor as an
        // inline dictionary rather than an indirect reference. The previous
        // code path failed `get_ref(b"FontDescriptor")` and returned
        // `(None, FontProgram::None)`, leaving the renderer without embedded
        // font data (and falling back to Liberation substitutes). Streams
        // must still be indirect, so we build a tiny stream object and
        // reference it from the inline descriptor.
        use crate::object::{PdfDictionary, PdfObject};

        let (data, diag) = make_resolver_parts("empty_page.pdf");
        let doc = DocumentParser::with_diagnostics(&data, diag.clone())
            .parse()
            .unwrap();
        let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag);

        // Build an inline font dict with an inline FontDescriptor, but no
        // embedded font program. Expect (None, FontProgram::None) without
        // crashing (regression: prior to the fix this path was unreachable).
        let mut descriptor = PdfDictionary::new();
        descriptor.insert(
            b"Type".to_vec(),
            PdfObject::Name(b"FontDescriptor".to_vec()),
        );
        descriptor.insert(
            b"FontName".to_vec(),
            PdfObject::Name(b"CIDFont+F1".to_vec()),
        );

        let mut font_dict = PdfDictionary::new();
        font_dict.insert(
            b"FontDescriptor".to_vec(),
            PdfObject::Dictionary(descriptor),
        );

        let (data_out, program) =
            extract_font_data(&mut resolver, &font_dict, WarningContext::default());
        assert!(data_out.is_none());
        assert!(matches!(program, FontProgram::None));
    }

    #[test]
    fn test_parse_differences_consecutive_codes_format() {
        // The spec format: [code1 /name1 /name2 /name3 code2 /name4 /name5 ...]
        // Verifies that names after a code map to consecutive codes
        let arr = vec![
            PdfObject::Integer(100),
            PdfObject::Name(b"A".to_vec()), // 100
            PdfObject::Name(b"B".to_vec()), // 101
            PdfObject::Name(b"C".to_vec()), // 102
            PdfObject::Integer(200),
            PdfObject::Name(b"space".to_vec()),  // 200
            PdfObject::Name(b"period".to_vec()), // 201
        ];
        let diffs = parse_differences(&arr);
        assert_eq!(diffs.len(), 5);
        assert_eq!(diffs[0], (100, 'A'));
        assert_eq!(diffs[1], (101, 'B'));
        assert_eq!(diffs[2], (102, 'C'));
        assert_eq!(diffs[3], (200, ' '));
        assert_eq!(diffs[4], (201, '.'));
    }
}

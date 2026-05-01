//! M2.3 Font Spike -- prototype code and exploration tests.
//!
//! This is spike/research code, NOT production. All prototypes live here
//! as test-only code to inform the M3.1 Font Subsystem design.

use std::collections::HashMap;
use std::sync::Arc;
use udoc_pdf::object::resolver::ObjectResolver;
use udoc_pdf::object::{PdfDictionary, PdfObject};
use udoc_pdf::parse::DocumentParser;
use udoc_pdf::CollectingDiagnostics;

const CORPUS_DIR: &str = "tests/corpus/minimal";

fn read_corpus(filename: &str) -> Vec<u8> {
    let path = format!("{CORPUS_DIR}/{filename}");
    std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

// ---------------------------------------------------------------------------
// Font dictionary exploration helpers
// ---------------------------------------------------------------------------

/// Summary of a font found during exploration.
#[derive(Debug)]
struct FontInfo {
    pdf_file: String,
    page_index: usize,
    resource_name: String,
    font_type: String,
    subtype: String,
    base_font: String,
    encoding: String,
    has_tounicode: bool,
    has_descendant_fonts: bool,
    has_font_descriptor: bool,
}

/// Walk the page tree collecting page dictionaries.
///
/// PDF page trees are recursive: a /Pages node has /Kids which can be
/// either /Page leaves or nested /Pages nodes.
fn collect_pages(resolver: &mut ObjectResolver, pages_dict: &PdfDictionary) -> Vec<PdfDictionary> {
    let mut result = Vec::new();
    let kids = match resolver.get_resolved_array(pages_dict, b"Kids") {
        Ok(Some(kids)) => kids,
        _ => return result,
    };
    for kid in kids {
        let kid_dict = match resolver.resolve_as_dict(kid) {
            Ok(d) => d,
            Err(_) => continue,
        };
        match kid_dict.get_name(b"Type") {
            Some(b"Page") => result.push(kid_dict),
            Some(b"Pages") => {
                result.extend(collect_pages(resolver, &kid_dict));
            }
            _ => {
                // Treat unknown as page (lenient)
                result.push(kid_dict);
            }
        }
    }
    result
}

/// Extract font info from a single page's /Resources /Font dictionary.
fn extract_fonts_from_page(
    resolver: &mut ObjectResolver,
    page: &PdfDictionary,
    pdf_file: &str,
    page_index: usize,
) -> Vec<FontInfo> {
    let mut fonts = Vec::new();

    let resources = match resolver.get_resolved_dict(page, b"Resources") {
        Ok(Some(r)) => r,
        _ => return fonts,
    };
    let font_dict = match resolver.get_resolved_dict(&resources, b"Font") {
        Ok(Some(f)) => f,
        _ => return fonts,
    };

    for (key, value) in font_dict.iter() {
        let font = match value {
            PdfObject::Reference(r) => match resolver.resolve_dict(*r) {
                Ok(d) => d,
                Err(_) => continue,
            },
            PdfObject::Dictionary(d) => d.clone(),
            PdfObject::Stream(s) => s.dict.clone(),
            _ => continue,
        };

        let font_type = font
            .get_name(b"Type")
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .unwrap_or_default();
        let subtype = font
            .get_name(b"Subtype")
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .unwrap_or_default();
        let base_font = font
            .get_name(b"BaseFont")
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .unwrap_or_default();
        let encoding = match font.get(b"Encoding") {
            Some(PdfObject::Name(n)) => String::from_utf8_lossy(n).into_owned(),
            Some(PdfObject::Dictionary(_)) => "<<dict>>".to_string(),
            Some(PdfObject::Reference(_)) => "ref->...".to_string(),
            _ => "(none)".to_string(),
        };
        let has_tounicode = font.get(b"ToUnicode").is_some();
        let has_descendant_fonts = font.get(b"DescendantFonts").is_some();
        let has_font_descriptor = font.get(b"FontDescriptor").is_some();

        fonts.push(FontInfo {
            pdf_file: pdf_file.to_string(),
            page_index,
            resource_name: String::from_utf8_lossy(key).into_owned(),
            font_type,
            subtype,
            base_font,
            encoding,
            has_tounicode,
            has_descendant_fonts,
            has_font_descriptor,
        });
    }

    fonts
}

/// Open a corpus PDF, parse it, and return the resolver + page dicts.
///
/// Returns None if the file can't be parsed or has no page tree (logged to stderr).
/// The `data` must be passed in and must outlive the returned resolver.
fn open_corpus_pages<'a>(
    data: &'a [u8],
    filename: &str,
    diag: Arc<CollectingDiagnostics>,
) -> Option<(ObjectResolver<'a>, Vec<PdfDictionary>)> {
    let doc = match DocumentParser::with_diagnostics(data, diag.clone()).parse() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("  SKIP {filename}: parse error: {e}");
            return None;
        }
    };
    let mut resolver = ObjectResolver::from_document_with_diagnostics(data, doc, diag);

    let trailer = resolver.trailer()?.clone();
    let root_ref = trailer.get_ref(b"Root")?;
    let catalog = resolver.resolve_dict(root_ref).ok()?;
    let pages_ref = catalog.get_ref(b"Pages")?;
    let pages_dict = resolver.resolve_dict(pages_ref).ok()?;
    let pages = collect_pages(&mut resolver, &pages_dict);
    Some((resolver, pages))
}

/// Parse a PDF and extract all font info across all pages.
fn explore_fonts(filename: &str) -> Vec<FontInfo> {
    let data = read_corpus(filename);
    let diag = Arc::new(CollectingDiagnostics::new());
    let (mut resolver, pages) = match open_corpus_pages(&data, filename, diag) {
        Some(r) => r,
        None => return Vec::new(),
    };

    let mut all_fonts = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        all_fonts.extend(extract_fonts_from_page(&mut resolver, page, filename, i));
    }
    all_fonts
}

// ---------------------------------------------------------------------------
// Step 1: Font dictionary exploration test
// ---------------------------------------------------------------------------

#[test]
fn test_explore_fonts() {
    let corpus_files = [
        "ArabicCIDTrueType.pdf",
        "arial_unicode_ab_cidfont.pdf",
        "cid_cff.pdf",
        "simpletype3font.pdf",
        "text_clip_cff_cid.pdf",
        "TrueType_without_cmap.pdf",
        "xelatex.pdf",
        "xelatex-drawboard.pdf",
        "ostream1.pdf",
        "ostream2.pdf",
    ];

    let mut all_fonts = Vec::new();
    for file in &corpus_files {
        let fonts = explore_fonts(file);
        all_fonts.extend(fonts);
    }

    // Print summary table
    println!("\n=== Font Exploration Results ===\n");
    println!(
        "{:<28} {:<4} {:<8} {:<10} {:<14} {:<28} {:<14} {:<7} {:<7} {:<7}",
        "File", "Pg", "Res", "Type", "Subtype", "BaseFont", "Encoding", "ToUni", "Desc", "FDesc"
    );
    println!("{}", "-".repeat(140));
    for f in &all_fonts {
        println!(
            "{:<28} {:<4} {:<8} {:<10} {:<14} {:<28} {:<14} {:<7} {:<7} {:<7}",
            f.pdf_file,
            f.page_index,
            f.resource_name,
            f.font_type,
            f.subtype,
            if f.base_font.len() > 26 {
                let truncated: String = f.base_font.chars().take(23).collect();
                format!("{truncated}...")
            } else {
                f.base_font.clone()
            },
            f.encoding,
            f.has_tounicode,
            f.has_descendant_fonts,
            f.has_font_descriptor,
        );
    }

    // Summary stats
    let total = all_fonts.len();
    let type0_count = all_fonts.iter().filter(|f| f.subtype == "Type0").count();
    let type1_count = all_fonts.iter().filter(|f| f.subtype == "Type1").count();
    let truetype_count = all_fonts.iter().filter(|f| f.subtype == "TrueType").count();
    let type3_count = all_fonts.iter().filter(|f| f.subtype == "Type3").count();
    let cidfont_count = all_fonts
        .iter()
        .filter(|f| f.subtype == "CIDFontType0" || f.subtype == "CIDFontType2")
        .count();
    let tounicode_count = all_fonts.iter().filter(|f| f.has_tounicode).count();

    println!("\n=== Summary ===");
    println!("Total fonts: {total}");
    println!("  Type0 (composite): {type0_count}");
    println!("  Type1: {type1_count}");
    println!("  TrueType: {truetype_count}");
    println!("  Type3: {type3_count}");
    println!("  CIDFont (descendant): {cidfont_count}");
    println!("  With /ToUnicode: {tounicode_count}/{total}");

    // We should find at least some fonts in our corpus
    assert!(total > 0, "expected to find fonts in corpus");
}

// ---------------------------------------------------------------------------
// Step 2: ToUnicode CMap parser prototype (F-230b)
// ---------------------------------------------------------------------------

/// Minimal prototype ToUnicode CMap parser.
///
/// Production version will be more robust. This validates the approach.
struct ToUnicodeCMap {
    /// bfchar mappings: source code bytes -> unicode string
    bfchar: HashMap<Vec<u8>, String>,
    /// bfrange mappings: (start, end, base_unicode)
    bfrange: Vec<(Vec<u8>, Vec<u8>, String)>,
}

impl ToUnicodeCMap {
    fn new() -> Self {
        Self {
            bfchar: HashMap::new(),
            bfrange: Vec::new(),
        }
    }

    /// Parse a ToUnicode CMap from decoded stream bytes.
    fn parse(data: &[u8]) -> Self {
        let mut cmap = Self::new();
        let text = String::from_utf8_lossy(data);

        // Parse bfchar sections
        let mut rest = text.as_ref();
        while let Some(start) = rest.find("beginbfchar") {
            let after_begin = &rest[start + "beginbfchar".len()..];
            let end = match after_begin.find("endbfchar") {
                Some(e) => e,
                None => break,
            };
            let section = &after_begin[..end];
            Self::parse_bfchar_section(section, &mut cmap.bfchar);
            rest = &after_begin[end..];
        }

        // Parse bfrange sections
        rest = text.as_ref();
        while let Some(start) = rest.find("beginbfrange") {
            let after_begin = &rest[start + "beginbfrange".len()..];
            let end = match after_begin.find("endbfrange") {
                Some(e) => e,
                None => break,
            };
            let section = &after_begin[..end];
            Self::parse_bfrange_section(section, &mut cmap.bfrange);
            rest = &after_begin[end..];
        }

        cmap
    }

    fn parse_bfchar_section(section: &str, map: &mut HashMap<Vec<u8>, String>) {
        // Each line: <hex_src> <hex_dst>
        let mut chars = section.chars().peekable();
        while let Some(src) = Self::next_hex_token(&mut chars) {
            let dst = match Self::next_hex_token(&mut chars) {
                Some(h) => h,
                None => break,
            };
            let unicode = Self::hex_to_unicode_string(&dst);
            map.insert(src, unicode);
        }
    }

    fn parse_bfrange_section(section: &str, ranges: &mut Vec<(Vec<u8>, Vec<u8>, String)>) {
        let mut chars = section.chars().peekable();
        while let Some(start) = Self::next_hex_token(&mut chars) {
            let end = match Self::next_hex_token(&mut chars) {
                Some(h) => h,
                None => break,
            };
            let base = match Self::next_hex_token(&mut chars) {
                Some(h) => h,
                None => break,
            };
            let unicode = Self::hex_to_unicode_string(&base);
            ranges.push((start, end, unicode));
        }
    }

    /// Extract next <hex> token from stream, returning decoded bytes.
    fn next_hex_token(chars: &mut std::iter::Peekable<std::str::Chars>) -> Option<Vec<u8>> {
        // Skip to next '<'
        loop {
            match chars.peek() {
                Some('<') => {
                    chars.next();
                    break;
                }
                Some(_) => {
                    chars.next();
                }
                None => return None,
            }
        }
        // Read hex digits until '>'
        let mut hex = String::new();
        loop {
            match chars.next() {
                Some('>') => break,
                Some(c) if c.is_ascii_hexdigit() => hex.push(c),
                Some(_) => {} // skip whitespace inside hex
                None => break,
            }
        }
        // Decode hex pairs
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .filter_map(|i| {
                if i + 2 <= hex.len() {
                    u8::from_str_radix(&hex[i..i + 2], 16).ok()
                } else {
                    // Odd-length: treat trailing nibble as X0
                    u8::from_str_radix(&format!("{}0", &hex[i..i + 1]), 16).ok()
                }
            })
            .collect();
        Some(bytes)
    }

    /// Convert raw hex bytes to a Unicode string.
    /// ToUnicode values are big-endian UTF-16BE code points.
    ///
    /// NOTE: This prototype only handles BMP (2-byte) code points.
    /// Production parser must decode UTF-16 surrogate pairs for
    /// supplementary plane characters (U+10000+).
    fn hex_to_unicode_string(bytes: &[u8]) -> String {
        let mut result = String::new();
        let mut i = 0;
        while i + 1 < bytes.len() {
            let code_point = u16::from_be_bytes([bytes[i], bytes[i + 1]]);
            if let Some(c) = char::from_u32(code_point as u32) {
                result.push(c);
            }
            i += 2;
        }
        result
    }

    /// Look up a character code in the CMap.
    fn lookup(&self, code: &[u8]) -> Option<String> {
        // Check bfchar first (exact match)
        if let Some(s) = self.bfchar.get(code) {
            return Some(s.clone());
        }
        // Check bfrange
        for (start, end, base_unicode) in &self.bfrange {
            if code.len() == start.len() && code >= start.as_slice() && code <= end.as_slice() {
                // Offset from start
                let offset = Self::bytes_diff(start, code);
                if let Some(base_char) = base_unicode.chars().next() {
                    let target_code = base_char as u32 + offset;
                    if let Some(c) = char::from_u32(target_code) {
                        return Some(c.to_string());
                    }
                }
            }
        }
        None
    }

    /// Compute the integer difference between two byte sequences of equal length.
    fn bytes_diff(start: &[u8], code: &[u8]) -> u32 {
        let mut diff: u32 = 0;
        for (s, c) in start.iter().zip(code.iter()) {
            diff = diff
                .wrapping_mul(256)
                .wrapping_add((*c as u32).wrapping_sub(*s as u32));
        }
        diff
    }

    fn total_mappings(&self) -> usize {
        self.bfchar.len() + self.bfrange.len()
    }
}

#[test]
fn test_tounicode_cmap_parse_synthetic() {
    let cmap_data = b"\
/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CMapName /Test def
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
2 beginbfchar
<0041> <0041>
<0042> <0042>
endbfchar
1 beginbfrange
<0043> <0045> <0043>
endbfrange
endcmap
";
    let cmap = ToUnicodeCMap::parse(cmap_data);
    assert_eq!(cmap.bfchar.len(), 2);
    assert_eq!(cmap.bfrange.len(), 1);

    assert_eq!(cmap.lookup(&[0x00, 0x41]), Some("A".to_string()));
    assert_eq!(cmap.lookup(&[0x00, 0x42]), Some("B".to_string()));
    // Range: 0043-0045 maps to C, D, E
    assert_eq!(cmap.lookup(&[0x00, 0x43]), Some("C".to_string()));
    assert_eq!(cmap.lookup(&[0x00, 0x44]), Some("D".to_string()));
    assert_eq!(cmap.lookup(&[0x00, 0x45]), Some("E".to_string()));
    // Outside range
    assert_eq!(cmap.lookup(&[0x00, 0x46]), None);
}

#[test]
fn test_tounicode_from_corpus_pdf() {
    // Try to find and parse a real /ToUnicode stream from the corpus.
    // ArabicCIDTrueType.pdf is a CID font file that should have ToUnicode.
    let candidates = [
        "ArabicCIDTrueType.pdf",
        "arial_unicode_ab_cidfont.pdf",
        "cid_cff.pdf",
        "xelatex.pdf",
        "xelatex-drawboard.pdf",
    ];

    let mut found_any = false;

    for filename in &candidates {
        let data = read_corpus(filename);
        let diag = Arc::new(CollectingDiagnostics::new());
        let (mut resolver, pages) = match open_corpus_pages(&data, filename, diag) {
            Some(r) => r,
            None => continue,
        };

        for page in &pages {
            let resources = match resolver.get_resolved_dict(page, b"Resources") {
                Ok(Some(r)) => r,
                _ => continue,
            };
            let font_dict = match resolver.get_resolved_dict(&resources, b"Font") {
                Ok(Some(f)) => f,
                _ => continue,
            };

            for (key, value) in font_dict.iter() {
                let font = match value {
                    PdfObject::Reference(r) => match resolver.resolve_dict(*r) {
                        Ok(d) => d,
                        Err(_) => continue,
                    },
                    _ => continue,
                };

                // Check for /ToUnicode stream
                let tounicode_ref = match font.get_ref(b"ToUnicode") {
                    Some(r) => r,
                    None => continue,
                };

                let tounicode_stream = match resolver.resolve_stream(tounicode_ref) {
                    Ok(s) => s,
                    Err(_) => continue,
                };

                let decoded =
                    match resolver.decode_stream_data(&tounicode_stream, Some(tounicode_ref)) {
                        Ok(d) => d,
                        Err(_) => continue,
                    };

                let cmap = ToUnicodeCMap::parse(&decoded);
                let font_name = String::from_utf8_lossy(key);
                println!(
                    "  {filename} font /{font_name}: ToUnicode has {} bfchar + {} bfrange mappings",
                    cmap.bfchar.len(),
                    cmap.bfrange.len()
                );

                if cmap.total_mappings() > 0 {
                    found_any = true;
                }
            }
        }
    }

    assert!(
        found_any,
        "expected to find at least one ToUnicode CMap with mappings in corpus"
    );
}

// ---------------------------------------------------------------------------
// Step 3: Encoding prototypes (F-230c, F-230d)
// ---------------------------------------------------------------------------

/// Adobe Glyph List lookup (subset for prototype).
/// Production version will use the full AGL table (~4,300 entries).
fn agl_lookup(name: &str) -> Option<char> {
    match name {
        "space" => Some(' '),
        "exclam" => Some('!'),
        "quotedbl" => Some('"'),
        "numbersign" => Some('#'),
        "dollar" => Some('$'),
        "percent" => Some('%'),
        "ampersand" => Some('&'),
        "quotesingle" => Some('\''),
        "parenleft" => Some('('),
        "parenright" => Some(')'),
        "period" => Some('.'),
        "comma" => Some(','),
        "hyphen" => Some('-'),
        "zero" => Some('0'),
        "one" => Some('1'),
        "two" => Some('2'),
        "A" => Some('A'),
        "B" => Some('B'),
        "C" => Some('C'),
        "a" => Some('a'),
        "b" => Some('b'),
        "c" => Some('c'),
        _ => None,
    }
}

/// WinAnsiEncoding lookup (subset for prototype).
/// Production version will cover all 256 code points.
fn winansi_lookup(code: u8) -> Option<char> {
    match code {
        0x20 => Some(' '),
        0x21 => Some('!'),
        0x2C => Some(','),
        0x2E => Some('.'),
        0x30 => Some('0'),
        0x31 => Some('1'),
        0x41 => Some('A'),
        0x42 => Some('B'),
        0x61 => Some('a'),
        0x62 => Some('b'),
        // WinAnsi-specific: these differ from raw ISO-8859-1
        0x91 => Some('\u{2018}'), // left single quotation mark
        0x92 => Some('\u{2019}'), // right single quotation mark
        0x93 => Some('\u{201C}'), // left double quotation mark
        0x94 => Some('\u{201D}'), // right double quotation mark
        _ => None,
    }
}

/// Apply /Differences array to a base encoding table.
///
/// /Differences format: [code1 /name1 /name2 code2 /name3 ...]
/// Each integer sets the current code point, subsequent names override
/// consecutive code points from there.
fn apply_differences(
    base: &dyn Fn(u8) -> Option<char>,
    differences: &[(u8, Vec<String>)],
) -> HashMap<u8, char> {
    let mut table: HashMap<u8, char> = HashMap::new();
    // Pre-fill from base
    for code in 0..=255u8 {
        if let Some(c) = base(code) {
            table.insert(code, c);
        }
    }
    // Apply overrides
    for (start_code, names) in differences {
        let mut code = *start_code;
        for name in names {
            if let Some(c) = agl_lookup(name) {
                table.insert(code, c);
            }
            code = code.wrapping_add(1);
        }
    }
    table
}

#[test]
fn test_agl_lookup() {
    assert_eq!(agl_lookup("space"), Some(' '));
    assert_eq!(agl_lookup("A"), Some('A'));
    assert_eq!(agl_lookup("period"), Some('.'));
    assert_eq!(agl_lookup("nonexistent_glyph_xyz"), None);
}

#[test]
fn test_winansi_lookup() {
    assert_eq!(winansi_lookup(0x41), Some('A'));
    assert_eq!(winansi_lookup(0x20), Some(' '));
    // WinAnsi-specific smart quotes
    assert_eq!(winansi_lookup(0x93), Some('\u{201C}'));
    assert_eq!(winansi_lookup(0x94), Some('\u{201D}'));
}

#[test]
fn test_differences_override() {
    // Start with WinAnsi, override code 0x41 (normally 'A') with 'a' via /Differences
    let differences = vec![(0x41, vec!["a".to_string(), "b".to_string()])];
    let table = apply_differences(&winansi_lookup, &differences);

    // 0x41 should now be 'a' (was 'A')
    assert_eq!(table.get(&0x41), Some(&'a'));
    // 0x42 should now be 'b' (was 'B'), from consecutive override
    assert_eq!(table.get(&0x42), Some(&'b'));
    // 0x20 unchanged from base
    assert_eq!(table.get(&0x20), Some(&' '));
}

// ---------------------------------------------------------------------------
// Step 4: Fallback chain prototype (F-230e)
// ---------------------------------------------------------------------------

/// Resolve a character code to Unicode using the PDF font fallback chain.
///
/// Priority order:
/// 1. ToUnicode CMap (most authoritative)
/// 2. AGL glyph name lookup
/// 3. Encoding table (WinAnsi, MacRoman, etc.)
/// 4. U+FFFD replacement character
fn resolve_glyph(
    code: &[u8],
    tounicode: Option<&ToUnicodeCMap>,
    glyph_name: Option<&str>,
    encoding_fn: Option<&dyn Fn(u8) -> Option<char>>,
) -> char {
    // 1. ToUnicode
    if let Some(cmap) = tounicode {
        if let Some(s) = cmap.lookup(code) {
            if let Some(c) = s.chars().next() {
                return c;
            }
        }
    }

    // 2. AGL glyph name
    if let Some(name) = glyph_name {
        if let Some(c) = agl_lookup(name) {
            return c;
        }
    }

    // 3. Encoding table (use single-byte code)
    if let Some(enc) = encoding_fn {
        if code.len() == 1 {
            if let Some(c) = enc(code[0]) {
                return c;
            }
        }
    }

    // 4. Replacement character
    '\u{FFFD}'
}

#[test]
fn test_fallback_tounicode_wins() {
    let mut cmap = ToUnicodeCMap::new();
    cmap.bfchar.insert(vec![0x41], "X".to_string());

    // ToUnicode says 0x41 -> 'X', even though AGL says 'A' and WinAnsi says 'A'
    let result = resolve_glyph(&[0x41], Some(&cmap), Some("A"), Some(&winansi_lookup));
    assert_eq!(result, 'X');
}

#[test]
fn test_fallback_agl_when_no_tounicode() {
    let result = resolve_glyph(&[0x41], None, Some("A"), Some(&winansi_lookup));
    assert_eq!(result, 'A');
}

#[test]
fn test_fallback_encoding_when_no_agl() {
    let result = resolve_glyph(&[0x41], None, None, Some(&winansi_lookup));
    assert_eq!(result, 'A');
}

#[test]
fn test_fallback_replacement_char() {
    let result = resolve_glyph(&[0xFF], None, None, None);
    assert_eq!(result, '\u{FFFD}');
}

#[test]
fn test_fallback_agl_over_encoding() {
    // Glyph name "space" (AGL -> ' ') takes priority over encoding
    // even if encoding maps the code to something else
    let weird_encoding = |_code: u8| -> Option<char> { Some('?') };
    let result = resolve_glyph(&[0x20], None, Some("space"), Some(&weird_encoding));
    assert_eq!(result, ' ');
}

// ---------------------------------------------------------------------------
// Step 5: Font type hierarchy exploration
// ---------------------------------------------------------------------------

/// Explore descendant CID fonts from Type0 composite fonts.
#[test]
fn test_explore_cid_descendants() {
    let cid_files = [
        "ArabicCIDTrueType.pdf",
        "arial_unicode_ab_cidfont.pdf",
        "cid_cff.pdf",
        "text_clip_cff_cid.pdf",
    ];

    println!("\n=== CID Font Descendant Exploration ===\n");

    for filename in &cid_files {
        let data = read_corpus(filename);
        let diag = Arc::new(CollectingDiagnostics::new());
        let (mut resolver, pages) = match open_corpus_pages(&data, filename, diag) {
            Some(r) => r,
            None => continue,
        };

        for page in &pages {
            let resources = match resolver.get_resolved_dict(page, b"Resources") {
                Ok(Some(r)) => r,
                _ => continue,
            };
            let font_dict = match resolver.get_resolved_dict(&resources, b"Font") {
                Ok(Some(f)) => f,
                _ => continue,
            };

            for (key, value) in font_dict.iter() {
                let font_ref = match value.as_reference() {
                    Some(r) => r,
                    None => continue,
                };
                let font = match resolver.resolve_dict(font_ref) {
                    Ok(d) => d,
                    Err(_) => continue,
                };

                if font.get_name(b"Subtype") != Some(b"Type0") {
                    continue;
                }

                let font_name = String::from_utf8_lossy(key);
                let base_font = font
                    .get_name(b"BaseFont")
                    .map(|n| String::from_utf8_lossy(n).into_owned())
                    .unwrap_or_default();
                let encoding = font
                    .get_name(b"Encoding")
                    .map(|n| String::from_utf8_lossy(n).into_owned())
                    .unwrap_or("(none)".to_string());

                println!(
                    "  {filename} /{font_name}: Type0 BaseFont={base_font} Encoding={encoding}"
                );

                // Explore DescendantFonts
                let descendants = match resolver.get_resolved_array(&font, b"DescendantFonts") {
                    Ok(Some(a)) => a,
                    _ => {
                        println!("    (no DescendantFonts)");
                        continue;
                    }
                };

                for (di, desc_obj) in descendants.iter().enumerate() {
                    let desc_dict = match resolver.resolve_as_dict(desc_obj.clone()) {
                        Ok(d) => d,
                        Err(e) => {
                            println!("    descendant {di}: resolve error: {e}");
                            continue;
                        }
                    };

                    let desc_subtype = desc_dict
                        .get_name(b"Subtype")
                        .map(|n| String::from_utf8_lossy(n).into_owned())
                        .unwrap_or_default();
                    let desc_basefont = desc_dict
                        .get_name(b"BaseFont")
                        .map(|n| String::from_utf8_lossy(n).into_owned())
                        .unwrap_or_default();

                    // CIDSystemInfo
                    let cid_info = match resolver.get_resolved_dict(&desc_dict, b"CIDSystemInfo") {
                        Ok(Some(info)) => {
                            let registry = info
                                .get_str(b"Registry")
                                .map(|s| String::from_utf8_lossy(s.as_bytes()).into_owned())
                                .unwrap_or_default();
                            let ordering = info
                                .get_str(b"Ordering")
                                .map(|s| String::from_utf8_lossy(s.as_bytes()).into_owned())
                                .unwrap_or_default();
                            let supplement = info.get_i64(b"Supplement").unwrap_or(-1);
                            format!("{registry}-{ordering}-{supplement}")
                        }
                        _ => "(none)".to_string(),
                    };

                    let dw = desc_dict.get_i64(b"DW").unwrap_or(-1);
                    let has_w = desc_dict.get(b"W").is_some();
                    let has_descriptor = desc_dict.get(b"FontDescriptor").is_some();

                    println!("    descendant {di}: {desc_subtype} BaseFont={desc_basefont}");
                    println!(
                        "      CIDSystemInfo={cid_info} DW={dw} has_W={has_w} has_FontDescriptor={has_descriptor}"
                    );
                }
            }
        }
    }
}

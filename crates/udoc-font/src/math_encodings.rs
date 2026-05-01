//! TeX Computer Modern math font encoding tables.
//!
//! TeX-generated PDFs use CM math fonts (CMSY, CMMI, CMEX) with custom
//! TeX-specific encodings that differ from standard PDF encodings. These
//! fonts often lack ToUnicode maps, so we provide static lookup tables
//! for the 128-entry encoding vectors.
//!
//! Reference: Donald Knuth, "Computer Modern Typefaces" (Volume E of
//! Computers & Typesetting). TeX font metric (.tfm) files define the
//! encoding positions.

/// CMSY (Computer Modern Symbol) encoding table.
///
/// 128 entries mapping TeX symbol encoding positions to Unicode.
/// Positions 0x41-0x5A map to calligraphic/script uppercase letters.
pub static CMSY_TABLE: [Option<char>; 128] = [
    // 0x00-0x0F
    Some('\u{2212}'), // 0x00: minus sign
    Some('\u{00B7}'), // 0x01: middle dot (periodcentered)
    Some('\u{00D7}'), // 0x02: multiplication sign
    Some('\u{2217}'), // 0x03: asterisk operator
    Some('\u{00F7}'), // 0x04: division sign
    Some('\u{25C7}'), // 0x05: diamond
    Some('\u{00B1}'), // 0x06: plus-minus sign
    Some('\u{2213}'), // 0x07: minus-or-plus sign
    Some('\u{2295}'), // 0x08: circled plus
    Some('\u{2296}'), // 0x09: circled minus
    Some('\u{2297}'), // 0x0A: circled times
    Some('\u{2298}'), // 0x0B: circled division slash
    Some('\u{2299}'), // 0x0C: circled dot operator
    Some('\u{25CB}'), // 0x0D: white circle
    Some('\u{2218}'), // 0x0E: ring operator
    Some('\u{2219}'), // 0x0F: bullet operator
    // 0x10-0x1F
    Some('\u{224D}'), // 0x10: equivalent to (asymp)
    Some('\u{2261}'), // 0x11: identical to (equiv)
    Some('\u{2286}'), // 0x12: subset of or equal to
    Some('\u{2287}'), // 0x13: superset of or equal to
    Some('\u{2264}'), // 0x14: less-than or equal to
    Some('\u{2265}'), // 0x15: greater-than or equal to
    Some('\u{227C}'), // 0x16: precedes or equal to
    Some('\u{227D}'), // 0x17: succeeds or equal to
    Some('\u{223C}'), // 0x18: tilde operator (sim)
    Some('\u{2248}'), // 0x19: almost equal to (approx)
    Some('\u{2282}'), // 0x1A: subset of
    Some('\u{2283}'), // 0x1B: superset of
    Some('\u{226A}'), // 0x1C: much less-than
    Some('\u{226B}'), // 0x1D: much greater-than
    Some('\u{227A}'), // 0x1E: precedes
    Some('\u{227B}'), // 0x1F: succeeds
    // 0x20-0x2F: arrows
    Some('\u{2190}'), // 0x20: leftwards arrow
    Some('\u{2192}'), // 0x21: rightwards arrow
    Some('\u{2191}'), // 0x22: upwards arrow
    Some('\u{2193}'), // 0x23: downwards arrow
    Some('\u{2194}'), // 0x24: left right arrow
    Some('\u{2197}'), // 0x25: north east arrow
    Some('\u{2198}'), // 0x26: south east arrow
    Some('\u{2243}'), // 0x27: asymptotically equal to (simeq)
    Some('\u{21D0}'), // 0x28: leftwards double arrow
    Some('\u{21D2}'), // 0x29: rightwards double arrow
    Some('\u{21D1}'), // 0x2A: upwards double arrow
    Some('\u{21D3}'), // 0x2B: downwards double arrow
    Some('\u{21D4}'), // 0x2C: left right double arrow
    Some('\u{2196}'), // 0x2D: north west arrow
    Some('\u{2199}'), // 0x2E: south west arrow
    Some('\u{221D}'), // 0x2F: proportional to
    // 0x30-0x3F: misc symbols
    Some('\u{2032}'), // 0x30: prime
    Some('\u{221E}'), // 0x31: infinity
    Some('\u{2208}'), // 0x32: element of
    Some('\u{220B}'), // 0x33: contains as member
    Some('\u{25B3}'), // 0x34: white up-pointing triangle
    Some('\u{25BD}'), // 0x35: white down-pointing triangle
    Some('\u{0338}'), // 0x36: combining long solidus overlay (negation slash)
    Some('\u{21A6}'), // 0x37: rightwards arrow from bar (mapsto)
    Some('\u{2200}'), // 0x38: for all
    Some('\u{2203}'), // 0x39: there exists
    Some('\u{00AC}'), // 0x3A: not sign
    Some('\u{2205}'), // 0x3B: empty set
    Some('\u{211C}'), // 0x3C: black-letter capital R (Re)
    Some('\u{2111}'), // 0x3D: black-letter capital I (Im)
    Some('\u{22A4}'), // 0x3E: down tack (top)
    Some('\u{22A5}'), // 0x3F: up tack (perp/bot)
    // 0x40: alef symbol
    Some('\u{2135}'), // 0x40: alef symbol
    // 0x41-0x5A: calligraphic uppercase A-Z
    Some('A'), // 0x41
    Some('B'), // 0x42
    Some('C'), // 0x43
    Some('D'), // 0x44
    Some('E'), // 0x45
    Some('F'), // 0x46
    Some('G'), // 0x47
    Some('H'), // 0x48
    Some('I'), // 0x49
    Some('J'), // 0x4A
    Some('K'), // 0x4B
    Some('L'), // 0x4C
    Some('M'), // 0x4D
    Some('N'), // 0x4E
    Some('O'), // 0x4F
    Some('P'), // 0x50
    Some('Q'), // 0x51
    Some('R'), // 0x52
    Some('S'), // 0x53
    Some('T'), // 0x54
    Some('U'), // 0x55
    Some('V'), // 0x56
    Some('W'), // 0x57
    Some('X'), // 0x58
    Some('Y'), // 0x59
    Some('Z'), // 0x5A
    // 0x5B-0x5F: set operations and logic
    Some('\u{222A}'), // 0x5B: union
    Some('\u{2229}'), // 0x5C: intersection
    Some('\u{228E}'), // 0x5D: multiset union
    Some('\u{2227}'), // 0x5E: logical and
    Some('\u{2228}'), // 0x5F: logical or
    // 0x60-0x6F: misc
    Some('\u{22A2}'), // 0x60: right tack (vdash)
    Some('\u{22A3}'), // 0x61: left tack (dashv)
    Some('\u{230A}'), // 0x62: left floor
    Some('\u{230B}'), // 0x63: right floor
    Some('\u{2308}'), // 0x64: left ceiling
    Some('\u{2309}'), // 0x65: right ceiling
    Some('{'),        // 0x66: left curly bracket
    Some('}'),        // 0x67: right curly bracket
    Some('\u{27E8}'), // 0x68: mathematical left angle bracket
    Some('\u{27E9}'), // 0x69: mathematical right angle bracket
    Some('|'),        // 0x6A: vertical line
    Some('\u{2016}'), // 0x6B: double vertical line
    Some('\u{2195}'), // 0x6C: up down arrow
    Some('\u{21D5}'), // 0x6D: up down double arrow
    Some('\\'),       // 0x6E: reverse solidus (backslash)
    Some('\u{2240}'), // 0x6F: wreath product
    // 0x70-0x7F: operators and suits
    Some('\u{221A}'), // 0x70: square root
    Some('\u{2210}'), // 0x71: n-ary coproduct
    Some('\u{2207}'), // 0x72: nabla
    Some('\u{222B}'), // 0x73: integral
    Some('\u{2294}'), // 0x74: square cup
    Some('\u{2293}'), // 0x75: square cap
    Some('\u{2291}'), // 0x76: square image of or equal to
    Some('\u{2292}'), // 0x77: square original of or equal to
    Some('\u{00A7}'), // 0x78: section sign
    Some('\u{2020}'), // 0x79: dagger
    Some('\u{2021}'), // 0x7A: double dagger
    Some('\u{00B6}'), // 0x7B: pilcrow sign (paragraph)
    Some('\u{2663}'), // 0x7C: black club suit
    Some('\u{2662}'), // 0x7D: white diamond suit
    Some('\u{2661}'), // 0x7E: white heart suit
    Some('\u{2660}'), // 0x7F: black spade suit
];

/// CMMI (Computer Modern Math Italic) encoding table.
///
/// 128 entries mapping TeX math italic encoding positions to Unicode.
/// Contains Greek letters (0x00-0x27), digits (0x30-0x39), and
/// italic Latin letters (0x41-0x5A, 0x61-0x7A).
pub static CMMI_TABLE: [Option<char>; 128] = [
    // 0x00-0x0F: Greek capital and small letters
    Some('\u{0393}'), // 0x00: Greek capital Gamma
    Some('\u{0394}'), // 0x01: Greek capital Delta
    Some('\u{0398}'), // 0x02: Greek capital Theta
    Some('\u{039B}'), // 0x03: Greek capital Lambda
    Some('\u{039E}'), // 0x04: Greek capital Xi
    Some('\u{03A0}'), // 0x05: Greek capital Pi
    Some('\u{03A3}'), // 0x06: Greek capital Sigma
    Some('\u{03A5}'), // 0x07: Greek capital Upsilon
    Some('\u{03A6}'), // 0x08: Greek capital Phi
    Some('\u{03A8}'), // 0x09: Greek capital Psi
    Some('\u{03A9}'), // 0x0A: Greek capital Omega
    Some('\u{03B1}'), // 0x0B: Greek small alpha
    Some('\u{03B2}'), // 0x0C: Greek small beta
    Some('\u{03B3}'), // 0x0D: Greek small gamma
    Some('\u{03B4}'), // 0x0E: Greek small delta
    Some('\u{03B5}'), // 0x0F: Greek small epsilon
    // 0x10-0x1F: more Greek
    Some('\u{03B6}'), // 0x10: Greek small zeta
    Some('\u{03B7}'), // 0x11: Greek small eta
    Some('\u{03B8}'), // 0x12: Greek small theta
    Some('\u{03B9}'), // 0x13: Greek small iota
    Some('\u{03BA}'), // 0x14: Greek small kappa
    Some('\u{03BB}'), // 0x15: Greek small lambda
    Some('\u{03BC}'), // 0x16: Greek small mu
    Some('\u{03BD}'), // 0x17: Greek small nu
    Some('\u{03BE}'), // 0x18: Greek small xi
    Some('\u{03C0}'), // 0x19: Greek small pi
    Some('\u{03C1}'), // 0x1A: Greek small rho
    Some('\u{03C3}'), // 0x1B: Greek small sigma
    Some('\u{03C4}'), // 0x1C: Greek small tau
    Some('\u{03C5}'), // 0x1D: Greek small upsilon
    Some('\u{03C6}'), // 0x1E: Greek small phi
    Some('\u{03C7}'), // 0x1F: Greek small chi
    // 0x20-0x2F: psi, omega, variant Greek, harpoons
    Some('\u{03C8}'), // 0x20: Greek small psi
    Some('\u{03C9}'), // 0x21: Greek small omega
    Some('\u{025B}'), // 0x22: Latin small epsilon (varepsilon)
    Some('\u{03D1}'), // 0x23: Greek theta symbol (vartheta)
    Some('\u{03D6}'), // 0x24: Greek pi symbol (varpi)
    Some('\u{03F1}'), // 0x25: Greek rho symbol (varrho)
    Some('\u{03C2}'), // 0x26: Greek small final sigma (varsigma)
    Some('\u{03D5}'), // 0x27: Greek phi symbol (varphi)
    Some('\u{21BC}'), // 0x28: leftwards harpoon with barb upwards
    Some('\u{21BD}'), // 0x29: leftwards harpoon with barb downwards
    Some('\u{21C0}'), // 0x2A: rightwards harpoon with barb upwards
    Some('\u{21C1}'), // 0x2B: rightwards harpoon with barb downwards
    None,             // 0x2C: left hooktop (accent, not standalone)
    None,             // 0x2D: right hooktop (accent, not standalone)
    None,             // 0x2E: triangleright (accent variant)
    None,             // 0x2F: triangleleft (accent variant)
    // 0x30-0x3F: digits and punctuation
    Some('0'),        // 0x30
    Some('1'),        // 0x31
    Some('2'),        // 0x32
    Some('3'),        // 0x33
    Some('4'),        // 0x34
    Some('5'),        // 0x35
    Some('6'),        // 0x36
    Some('7'),        // 0x37
    Some('8'),        // 0x38
    Some('9'),        // 0x39
    Some('.'),        // 0x3A: period (as used in math mode)
    Some(','),        // 0x3B: comma
    Some('<'),        // 0x3C: less-than sign
    Some('/'),        // 0x3D: solidus (slash)
    Some('>'),        // 0x3E: greater-than sign
    Some('\u{22C6}'), // 0x3F: star operator
    // 0x40-0x5F: partial differential, italic uppercase, music accidentals
    Some('\u{2202}'), // 0x40: partial differential
    Some('A'),        // 0x41: italic uppercase A
    Some('B'),        // 0x42
    Some('C'),        // 0x43
    Some('D'),        // 0x44
    Some('E'),        // 0x45
    Some('F'),        // 0x46
    Some('G'),        // 0x47
    Some('H'),        // 0x48
    Some('I'),        // 0x49
    Some('J'),        // 0x4A
    Some('K'),        // 0x4B
    Some('L'),        // 0x4C
    Some('M'),        // 0x4D
    Some('N'),        // 0x4E
    Some('O'),        // 0x4F
    Some('P'),        // 0x50
    Some('Q'),        // 0x51
    Some('R'),        // 0x52
    Some('S'),        // 0x53
    Some('T'),        // 0x54
    Some('U'),        // 0x55
    Some('V'),        // 0x56
    Some('W'),        // 0x57
    Some('X'),        // 0x58
    Some('Y'),        // 0x59
    Some('Z'),        // 0x5A
    Some('\u{266D}'), // 0x5B: music flat sign
    Some('\u{266E}'), // 0x5C: music natural sign
    Some('\u{266F}'), // 0x5D: music sharp sign
    Some('\u{2323}'), // 0x5E: smile (smile arc)
    Some('\u{2322}'), // 0x5F: frown (frown arc)
    // 0x60-0x7F: ell, italic lowercase, dotless i/j, weierstrass p, vec
    Some('\u{2113}'), // 0x60: script small l (ell)
    Some('a'),        // 0x61: italic lowercase a
    Some('b'),        // 0x62
    Some('c'),        // 0x63
    Some('d'),        // 0x64
    Some('e'),        // 0x65
    Some('f'),        // 0x66
    Some('g'),        // 0x67
    Some('h'),        // 0x68
    Some('i'),        // 0x69
    Some('j'),        // 0x6A
    Some('k'),        // 0x6B
    Some('l'),        // 0x6C
    Some('m'),        // 0x6D
    Some('n'),        // 0x6E
    Some('o'),        // 0x6F
    Some('p'),        // 0x70
    Some('q'),        // 0x71
    Some('r'),        // 0x72
    Some('s'),        // 0x73
    Some('t'),        // 0x74
    Some('u'),        // 0x75
    Some('v'),        // 0x76
    Some('w'),        // 0x77
    Some('x'),        // 0x78
    Some('y'),        // 0x79
    Some('z'),        // 0x7A
    Some('\u{0131}'), // 0x7B: dotless i
    Some('\u{0237}'), // 0x7C: dotless j
    Some('\u{2118}'), // 0x7D: Weierstrass p (wp)
    Some('\u{2192}'), // 0x7E: vector arrow (fallback to rightwards arrow)
    None,             // 0x7F: tie accent (not standalone text)
];

/// CMEX (Computer Modern Math Extension) encoding table.
///
/// 128 entries for large delimiters, big operators, and radical pieces.
/// Most entries are size variants of delimiters. We map them to the base
/// Unicode character they represent (e.g. all sizes of "(" map to U+0028).
pub static CMEX_TABLE: [Option<char>; 128] = [
    // 0x00-0x0F: small delimiters (various sizes of parens, brackets)
    Some('('),        // 0x00: left paren (small)
    Some(')'),        // 0x01: right paren (small)
    Some('['),        // 0x02: left bracket (small)
    Some(']'),        // 0x03: right bracket (small)
    Some('\u{230A}'), // 0x04: left floor (small)
    Some('\u{230B}'), // 0x05: right floor (small)
    Some('\u{2308}'), // 0x06: left ceiling (small)
    Some('\u{2309}'), // 0x07: right ceiling (small)
    Some('{'),        // 0x08: left curly bracket (small)
    Some('}'),        // 0x09: right curly bracket (small)
    Some('\u{27E8}'), // 0x0A: left angle bracket (small)
    Some('\u{27E9}'), // 0x0B: right angle bracket (small)
    Some('|'),        // 0x0C: vertical bar (mid)
    Some('\u{2016}'), // 0x0D: double vertical bar
    Some('/'),        // 0x0E: solidus (small)
    Some('\\'),       // 0x0F: reverse solidus (small)
    // 0x10-0x1F: medium delimiters
    Some('('),        // 0x10: left paren (medium)
    Some(')'),        // 0x11: right paren (medium)
    Some('['),        // 0x12: left bracket (medium)
    Some(']'),        // 0x13: right bracket (medium)
    Some('\u{230A}'), // 0x14: left floor (medium)
    Some('\u{230B}'), // 0x15: right floor (medium)
    Some('\u{2308}'), // 0x16: left ceiling (medium)
    Some('\u{2309}'), // 0x17: right ceiling (medium)
    Some('{'),        // 0x18: left curly bracket (medium)
    Some('}'),        // 0x19: right curly bracket (medium)
    Some('\u{27E8}'), // 0x1A: left angle bracket (medium)
    Some('\u{27E9}'), // 0x1B: right angle bracket (medium)
    Some('|'),        // 0x1C: vertical bar (medium)
    Some('\u{2016}'), // 0x1D: double vertical bar (medium)
    Some('/'),        // 0x1E: solidus (medium)
    Some('\\'),       // 0x1F: reverse solidus (medium)
    // 0x20-0x2F: large delimiters
    Some('('),        // 0x20: left paren (large)
    Some(')'),        // 0x21: right paren (large)
    Some('['),        // 0x22: left bracket (large)
    Some(']'),        // 0x23: right bracket (large)
    Some('\u{230A}'), // 0x24: left floor (large)
    Some('\u{230B}'), // 0x25: right floor (large)
    Some('\u{2308}'), // 0x26: left ceiling (large)
    Some('\u{2309}'), // 0x27: right ceiling (large)
    Some('{'),        // 0x28: left curly bracket (large)
    Some('}'),        // 0x29: right curly bracket (large)
    Some('\u{27E8}'), // 0x2A: left angle bracket (large)
    Some('\u{27E9}'), // 0x2B: right angle bracket (large)
    Some('|'),        // 0x2C: vertical bar (large)
    Some('\u{2016}'), // 0x2D: double vertical bar (large)
    Some('/'),        // 0x2E: solidus (large)
    Some('\\'),       // 0x2F: reverse solidus (large)
    // 0x30-0x3F: extra-large delimiters
    Some('('),        // 0x30: left paren (extra-large)
    Some(')'),        // 0x31: right paren (extra-large)
    Some('['),        // 0x32: left bracket (extra-large)
    Some(']'),        // 0x33: right bracket (extra-large)
    Some('\u{230A}'), // 0x34: left floor (extra-large)
    Some('\u{230B}'), // 0x35: right floor (extra-large)
    Some('\u{2308}'), // 0x36: left ceiling (extra-large)
    Some('\u{2309}'), // 0x37: right ceiling (extra-large)
    Some('{'),        // 0x38: left curly bracket (extra-large)
    Some('}'),        // 0x39: right curly bracket (extra-large)
    Some('\u{27E8}'), // 0x3A: left angle bracket (extra-large)
    Some('\u{27E9}'), // 0x3B: right angle bracket (extra-large)
    Some('|'),        // 0x3C: vertical bar (extra-large)
    Some('\u{2016}'), // 0x3D: double vertical bar (extra-large)
    Some('/'),        // 0x3E: solidus (extra-large)
    Some('\\'),       // 0x3F: reverse solidus (extra-large)
    // 0x40-0x4F: extensible delimiter pieces (top, bot, mid, repeater)
    None, // 0x40: left paren top piece
    None, // 0x41: right paren top piece
    None, // 0x42: left bracket top piece
    None, // 0x43: right bracket top piece
    None, // 0x44: left bracket bottom piece
    None, // 0x45: right bracket bottom piece
    None, // 0x46: left brace top piece
    None, // 0x47: right brace top piece
    None, // 0x48: left brace bottom piece
    None, // 0x49: right brace bottom piece
    None, // 0x4A: left brace middle piece
    None, // 0x4B: right brace middle piece
    None, // 0x4C: vertical extension piece
    None, // 0x4D: double vertical extension piece
    None, // 0x4E: left paren bottom piece
    None, // 0x4F: right paren bottom piece
    // 0x50-0x5F: big operators (display and text style)
    Some('\u{2211}'), // 0x50: summation (displaystyle)
    Some('\u{220F}'), // 0x51: product (displaystyle)
    Some('\u{222B}'), // 0x52: integral (displaystyle)
    Some('\u{222A}'), // 0x53: union (large)
    Some('\u{2229}'), // 0x54: intersection (large)
    Some('\u{228E}'), // 0x55: multiset union (large)
    Some('\u{2227}'), // 0x56: logical and (large)
    Some('\u{2228}'), // 0x57: logical or (large)
    Some('\u{2211}'), // 0x58: summation (textstyle)
    Some('\u{220F}'), // 0x59: product (textstyle)
    Some('\u{222B}'), // 0x5A: integral (textstyle)
    Some('\u{222A}'), // 0x5B: union (small)
    Some('\u{2229}'), // 0x5C: intersection (small)
    Some('\u{228E}'), // 0x5D: multiset union (small)
    Some('\u{2227}'), // 0x5E: logical and (small)
    Some('\u{2228}'), // 0x5F: logical or (small)
    // 0x60-0x6F: more operators, delimiter pieces
    Some('\u{2210}'), // 0x60: coproduct (large)
    Some('\u{2210}'), // 0x61: coproduct (small)
    None,             // 0x62: hat accent (wide)
    None,             // 0x63: tilde accent (wide)
    None,             // 0x64: left bracket top extension
    None,             // 0x65: right bracket top extension
    None,             // 0x66: left bracket bottom extension
    None,             // 0x67: right bracket bottom extension
    None,             // 0x68: left paren extension
    None,             // 0x69: right paren extension
    None,             // 0x6A: left brace top hook
    None,             // 0x6B: right brace top hook
    None,             // 0x6C: left brace bottom hook
    None,             // 0x6D: right brace bottom hook
    None,             // 0x6E: left brace middle hook
    None,             // 0x6F: right brace middle hook
    // 0x70-0x7F: radical and arrow pieces
    Some('\u{221A}'), // 0x70: square root sign
    None,             // 0x71: radical vertical extension
    None,             // 0x72: radical bottom piece
    None,             // 0x73: radical large piece
    None,             // 0x74: arrow left piece
    None,             // 0x75: arrow right piece
    None,             // 0x76: double arrow left piece
    None,             // 0x77: double arrow right piece
    None,             // 0x78: arrow horizontal extension
    None,             // 0x79: double arrow horizontal extension
    None,             // 0x7A: arrow vertical piece
    None,             // 0x7B: double arrow vertical piece
    None,             // 0x7C: radical corner piece
    None,             // 0x7D: (unused)
    None,             // 0x7E: (unused)
    None,             // 0x7F: (unused)
];

/// Match a font's BaseFont name against Computer Modern math font families.
///
/// Strips the 6-letter subset prefix (e.g. "ABCDEF+CMSY10" -> "CMSY10")
/// then checks the prefix. Returns the appropriate encoding table if matched.
///
/// Covers all CM math font variants regardless of design size suffix
/// (CMSY5, CMSY7, CMSY10, CMMI6, CMMI8, CMMI12, CMEX10, etc.).
pub fn match_cm_math_font(base_font: &str) -> Option<&'static [Option<char>; 128]> {
    // Strip subset prefix: "ABCDEF+CMSY10" -> "CMSY10"
    let name = match base_font.find('+') {
        Some(pos) if pos == 6 && base_font[..6].chars().all(|c| c.is_ascii_uppercase()) => {
            &base_font[pos + 1..]
        }
        _ => base_font,
    };

    if name.starts_with("CMSY") {
        Some(&CMSY_TABLE)
    } else if name.starts_with("CMMI") {
        Some(&CMMI_TABLE)
    } else if name.starts_with("CMEX") {
        Some(&CMEX_TABLE)
    } else if name.starts_with("MSBM") {
        Some(&MSBM_TABLE)
    } else {
        None
    }
}

/// MSBM (AMS Blackboard Bold / Fraktur) encoding table.
///
/// 128 entries mapping AMS symbol positions to Unicode.
/// Positions 0x41-0x5A map to blackboard bold uppercase (U+1D538-U+1D551 or fallbacks).
/// Since many blackboard bold characters are outside the BMP, we use the common
/// Unicode block U+2100 range where available.
pub static MSBM_TABLE: [Option<char>; 128] = [
    // 0x00-0x0F
    None, // 0x00
    None, // 0x01
    None, // 0x02
    None, // 0x03
    None, // 0x04
    None, // 0x05
    None, // 0x06
    None, // 0x07
    None, // 0x08
    None, // 0x09
    None, // 0x0A
    None, // 0x0B
    None, // 0x0C
    None, // 0x0D
    None, // 0x0E
    None, // 0x0F
    // 0x10-0x1F
    None, // 0x10
    None, // 0x11
    None, // 0x12
    None, // 0x13
    None, // 0x14
    None, // 0x15
    None, // 0x16
    None, // 0x17
    None, // 0x18
    None, // 0x19
    None, // 0x1A
    None, // 0x1B
    None, // 0x1C
    None, // 0x1D
    None, // 0x1E
    None, // 0x1F
    // 0x20-0x2F
    None, // 0x20
    None, // 0x21
    None, // 0x22
    None, // 0x23
    None, // 0x24
    None, // 0x25
    None, // 0x26
    None, // 0x27
    None, // 0x28
    None, // 0x29
    None, // 0x2A
    None, // 0x2B
    None, // 0x2C
    None, // 0x2D
    None, // 0x2E
    None, // 0x2F
    // 0x30-0x3F
    None, // 0x30
    None, // 0x31
    None, // 0x32
    None, // 0x33
    None, // 0x34
    None, // 0x35
    None, // 0x36
    None, // 0x37
    None, // 0x38
    None, // 0x39
    None, // 0x3A
    None, // 0x3B
    None, // 0x3C
    None, // 0x3D
    None, // 0x3E
    None, // 0x3F
    // 0x40-0x5A: blackboard bold uppercase letters
    None,             // 0x40
    Some('A'),        // 0x41: blackboard bold A (use regular A as fallback)
    Some('B'),        // 0x42: blackboard bold B
    Some('\u{2102}'), // 0x43: blackboard bold C (double-struck C)
    Some('D'),        // 0x44: blackboard bold D
    Some('E'),        // 0x45: blackboard bold E
    Some('F'),        // 0x46: blackboard bold F
    Some('G'),        // 0x47: blackboard bold G
    Some('\u{210D}'), // 0x48: blackboard bold H (double-struck H)
    Some('I'),        // 0x49: blackboard bold I
    Some('J'),        // 0x4A: blackboard bold J
    Some('K'),        // 0x4B: blackboard bold K
    Some('L'),        // 0x4C: blackboard bold L
    Some('M'),        // 0x4D: blackboard bold M
    Some('\u{2115}'), // 0x4E: blackboard bold N (double-struck N)
    Some('O'),        // 0x4F: blackboard bold O
    Some('\u{2119}'), // 0x50: blackboard bold P (double-struck P)
    Some('\u{211A}'), // 0x51: blackboard bold Q (double-struck Q)
    Some('\u{211D}'), // 0x52: blackboard bold R (double-struck R)
    Some('S'),        // 0x53: blackboard bold S
    Some('T'),        // 0x54: blackboard bold T
    Some('U'),        // 0x55: blackboard bold U
    Some('V'),        // 0x56: blackboard bold V
    Some('W'),        // 0x57: blackboard bold W
    Some('X'),        // 0x58: blackboard bold X
    Some('Y'),        // 0x59: blackboard bold Y
    Some('\u{2124}'), // 0x5A: blackboard bold Z (double-struck Z)
    // 0x5B-0x5F
    None, // 0x5B
    None, // 0x5C
    None, // 0x5D
    None, // 0x5E
    None, // 0x5F
    // 0x60-0x7F
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None, // 0x60-0x67
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None, // 0x68-0x6F
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None, // 0x70-0x77
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None, // 0x78-0x7F
];

/// Supplementary TeX glyph name-to-Unicode mappings.
///
/// These names appear in /Differences arrays of CMSY/CMMI fonts but are
/// NOT in the Adobe Glyph List (AGL). The standard `parse_glyph_name`
/// function tries AGL first; this table is a secondary fallback for
/// TeX-specific names.
///
/// Sorted by name for binary search.
pub static TEX_GLYPH_TABLE: &[(&str, u32)] = &[
    ("arrownortheast", 0x2197),
    ("arrowsoutheast", 0x2198),
    ("circledivide", 0x2298),
    ("circledot", 0x2299),
    ("circleminus", 0x2296),
    ("coproduct", 0x2210),
    ("dotlessj", 0x0237),
    ("mapsto", 0x21A6),
    ("minusplus", 0x2213),
    ("negationslash", 0x0338),
    ("owner", 0x220B),
    ("turnstileleft", 0x22A2),
    ("turnstileright", 0x22A3),
    ("vector", 0x20D7),
    ("wreathproduct", 0x2240),
];

/// Look up a TeX-specific glyph name that is not in the AGL.
///
/// Returns the Unicode character for TeX glyph names commonly found
/// in CM math font /Differences arrays.
pub fn tex_glyph_lookup(name: &str) -> Option<char> {
    TEX_GLYPH_TABLE
        .binary_search_by_key(&name, |&(n, _)| n)
        .ok()
        .and_then(|idx| char::from_u32(TEX_GLYPH_TABLE[idx].1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_cmsy_plain() {
        assert!(match_cm_math_font("CMSY10").is_some());
        let table = match_cm_math_font("CMSY10").unwrap();
        assert_eq!(table[0x00], Some('\u{2212}')); // minus sign
    }

    #[test]
    fn match_cmsy_with_subset_prefix() {
        let table = match_cm_math_font("ABCDEF+CMSY10").unwrap();
        assert_eq!(table[0x06], Some('\u{00B1}')); // plus-minus
    }

    #[test]
    fn match_cmsy_different_sizes() {
        assert!(match_cm_math_font("CMSY5").is_some());
        assert!(match_cm_math_font("CMSY7").is_some());
        assert!(match_cm_math_font("CMSY12").is_some());
    }

    #[test]
    fn match_cmmi_plain() {
        let table = match_cm_math_font("CMMI10").unwrap();
        assert_eq!(table[0x00], Some('\u{0393}')); // Greek capital Gamma
    }

    #[test]
    fn match_cmmi_with_subset_prefix() {
        let table = match_cm_math_font("XYZABC+CMMI12").unwrap();
        assert_eq!(table[0x0B], Some('\u{03B1}')); // Greek small alpha
    }

    #[test]
    fn match_cmex_plain() {
        let table = match_cm_math_font("CMEX10").unwrap();
        assert_eq!(table[0x50], Some('\u{2211}')); // summation
    }

    #[test]
    fn match_cmex_with_subset_prefix() {
        let table = match_cm_math_font("QRSTUV+CMEX10").unwrap();
        assert_eq!(table[0x52], Some('\u{222B}')); // integral
    }

    #[test]
    fn no_match_for_non_cm_fonts() {
        assert!(match_cm_math_font("Helvetica").is_none());
        assert!(match_cm_math_font("TimesNewRoman").is_none());
        assert!(match_cm_math_font("CMR10").is_none()); // CM Roman, not math
        assert!(match_cm_math_font("ABCDEF+Arial").is_none());
    }

    #[test]
    fn no_match_for_invalid_subset_prefix() {
        // Lowercase prefix is not a valid subset prefix, so "+" is not stripped.
        // "abcdef+CMSY10" doesn't start with "CMSY", so no match.
        assert!(match_cm_math_font("abcdef+CMSY10").is_none());
        // Too short prefix: "ABC+CMSY10" doesn't strip, doesn't start with "CMSY".
        assert!(match_cm_math_font("ABC+CMSY10").is_none());
    }

    #[test]
    fn cmsy_calligraphic_letters() {
        // 0x41-0x5A should be A-Z
        assert_eq!(CMSY_TABLE[0x41], Some('A'));
        assert_eq!(CMSY_TABLE[0x4D], Some('M'));
        assert_eq!(CMSY_TABLE[0x5A], Some('Z'));
    }

    #[test]
    fn cmsy_arrows() {
        assert_eq!(CMSY_TABLE[0x20], Some('\u{2190}')); // leftwards arrow
        assert_eq!(CMSY_TABLE[0x21], Some('\u{2192}')); // rightwards arrow
        assert_eq!(CMSY_TABLE[0x29], Some('\u{21D2}')); // rightwards double arrow
    }

    #[test]
    fn cmmi_greek_letters() {
        assert_eq!(CMMI_TABLE[0x00], Some('\u{0393}')); // Gamma
        assert_eq!(CMMI_TABLE[0x0B], Some('\u{03B1}')); // alpha
        assert_eq!(CMMI_TABLE[0x19], Some('\u{03C0}')); // pi
        assert_eq!(CMMI_TABLE[0x20], Some('\u{03C8}')); // psi
        assert_eq!(CMMI_TABLE[0x21], Some('\u{03C9}')); // omega
    }

    #[test]
    fn cmmi_digits() {
        for (i, digit) in ('0'..='9').enumerate() {
            assert_eq!(CMMI_TABLE[0x30 + i], Some(digit));
        }
    }

    #[test]
    fn cmmi_italic_letters() {
        assert_eq!(CMMI_TABLE[0x41], Some('A'));
        assert_eq!(CMMI_TABLE[0x5A], Some('Z'));
        assert_eq!(CMMI_TABLE[0x61], Some('a'));
        assert_eq!(CMMI_TABLE[0x7A], Some('z'));
    }

    #[test]
    fn cmmi_special_chars() {
        assert_eq!(CMMI_TABLE[0x40], Some('\u{2202}')); // partial differential
        assert_eq!(CMMI_TABLE[0x60], Some('\u{2113}')); // script l (ell)
        assert_eq!(CMMI_TABLE[0x7B], Some('\u{0131}')); // dotless i
        assert_eq!(CMMI_TABLE[0x7C], Some('\u{0237}')); // dotless j
        assert_eq!(CMMI_TABLE[0x7D], Some('\u{2118}')); // Weierstrass p
    }

    #[test]
    fn cmex_big_operators() {
        assert_eq!(CMEX_TABLE[0x50], Some('\u{2211}')); // summation display
        assert_eq!(CMEX_TABLE[0x51], Some('\u{220F}')); // product display
        assert_eq!(CMEX_TABLE[0x52], Some('\u{222B}')); // integral display
        assert_eq!(CMEX_TABLE[0x58], Some('\u{2211}')); // summation text
        assert_eq!(CMEX_TABLE[0x59], Some('\u{220F}')); // product text
    }

    #[test]
    fn cmex_delimiters_map_to_base_chars() {
        // All sizes of parens should map to the same base char
        assert_eq!(CMEX_TABLE[0x00], Some('('));
        assert_eq!(CMEX_TABLE[0x10], Some('('));
        assert_eq!(CMEX_TABLE[0x20], Some('('));
        assert_eq!(CMEX_TABLE[0x30], Some('('));
    }

    #[test]
    fn cmex_extension_pieces_are_none() {
        // Extension pieces should not produce text output
        assert_eq!(CMEX_TABLE[0x40], None);
        assert_eq!(CMEX_TABLE[0x4C], None);
    }

    #[test]
    fn tex_glyph_lookup_known_names() {
        assert_eq!(tex_glyph_lookup("minusplus"), Some('\u{2213}'));
        assert_eq!(tex_glyph_lookup("circledot"), Some('\u{2299}'));
        assert_eq!(tex_glyph_lookup("mapsto"), Some('\u{21A6}'));
        assert_eq!(tex_glyph_lookup("wreathproduct"), Some('\u{2240}'));
        assert_eq!(tex_glyph_lookup("dotlessj"), Some('\u{0237}'));
    }

    #[test]
    fn tex_glyph_lookup_unknown_names() {
        assert_eq!(tex_glyph_lookup("Helvetica"), None);
        assert_eq!(tex_glyph_lookup("space"), None);
        assert_eq!(tex_glyph_lookup(""), None);
    }

    #[test]
    fn table_sizes() {
        assert_eq!(CMSY_TABLE.len(), 128);
        assert_eq!(CMMI_TABLE.len(), 128);
        assert_eq!(CMEX_TABLE.len(), 128);
    }
}

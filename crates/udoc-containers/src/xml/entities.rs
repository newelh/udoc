//! Entity decoding for XML text and attribute values.
//!
//! Handles the 5 predefined XML entities (`&amp;`, `&lt;`, `&gt;`, `&quot;`,
//! `&apos;`) plus decimal (`&#NNN;`) and hex (`&#xHHH;`) numeric character
//! references. Unknown named entities are passed through literally.

use std::borrow::Cow;

/// Maximum number of bytes to scan past `&` looking for `;`.
/// XML entity references are short (e.g. `&amp;`, `&#x20AC;`). Anything longer
/// than this is not a real entity -- treat the `&` as literal text.
const MAX_ENTITY_LEN: usize = 32;

/// Decode XML entities in `input`, returning a `Cow::Borrowed` when no
/// entities are present (zero-alloc fast path).
pub fn decode_entities(input: &str) -> Cow<'_, str> {
    // Fast path: no ampersand means no entities.
    if !input.contains('&') {
        return Cow::Borrowed(input);
    }

    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(amp_pos) = rest.find('&') {
        out.push_str(&rest[..amp_pos]);
        rest = &rest[amp_pos..];

        // Look for the closing semicolon within a bounded window. Scan
        // bytes directly to avoid slicing mid-character in multi-byte UTF-8.
        let semi_pos = rest.as_bytes()[1..]
            .iter()
            .take(MAX_ENTITY_LEN)
            .position(|&b| b == b';')
            .map(|p| p + 1);
        if let Some(semi_pos) = semi_pos {
            let entity = &rest[1..semi_pos]; // between & and ;
            if let Some(decoded) = resolve_entity(entity) {
                out.push_str(decoded);
                rest = &rest[semi_pos + 1..];
            } else if let Some(ch) = resolve_numeric(entity) {
                out.push(ch);
                rest = &rest[semi_pos + 1..];
            } else {
                // Unknown entity: pass through literally.
                out.push_str(&rest[..semi_pos + 1]);
                rest = &rest[semi_pos + 1..];
            }
        } else {
            // No closing semicolon nearby: bare '&'. Pass through and continue.
            out.push('&');
            rest = &rest[1..];
        }
    }

    out.push_str(rest);
    Cow::Owned(out)
}

/// Resolve one of the 5 predefined XML entities (without the `&` and `;`).
fn resolve_entity(name: &str) -> Option<&'static str> {
    match name {
        "amp" => Some("&"),
        "lt" => Some("<"),
        "gt" => Some(">"),
        "quot" => Some("\""),
        "apos" => Some("'"),
        _ => None,
    }
}

/// Resolve a numeric character reference (without the `&` and `;`).
/// Expects the content after `&` (e.g., `#65` or `#x41`).
///
/// Rejects XML-forbidden characters: U+0000 and the control ranges
/// U+0001..=U+0008, U+000B.=U+000C, U+000E.=U+001F (only TAB, LF, CR
/// are permitted in XML 1.0 content).
fn resolve_numeric(entity: &str) -> Option<char> {
    let bytes = entity.as_bytes();
    if bytes.first() != Some(&b'#') {
        return None;
    }
    let numeric_part = &entity[1..];
    let code_point = if numeric_part.starts_with('x') || numeric_part.starts_with('X') {
        u32::from_str_radix(&numeric_part[1..], 16).ok()?
    } else {
        numeric_part.parse::<u32>().ok()?
    };
    let ch = char::from_u32(code_point)?;
    // Reject XML 1.0 forbidden characters (s2.2 Char production).
    // Allowed: #x9 (TAB), #xA (LF), #xD (CR), #x20 and above.
    match code_point {
        0x00 => None,
        0x01..=0x08 | 0x0B..=0x0C | 0x0E..=0x1F => None,
        _ => Some(ch),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_entities_borrows() {
        let result = decode_entities("hello world");
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(result, "hello world");
    }

    #[test]
    fn predefined_entities() {
        assert_eq!(decode_entities("&amp;"), "&");
        assert_eq!(decode_entities("&lt;"), "<");
        assert_eq!(decode_entities("&gt;"), ">");
        assert_eq!(decode_entities("&quot;"), "\"");
        assert_eq!(decode_entities("&apos;"), "'");
    }

    #[test]
    fn mixed_entities_and_text() {
        assert_eq!(decode_entities("a &amp; b &lt; c"), "a & b < c");
    }

    #[test]
    fn decimal_numeric_ref() {
        assert_eq!(decode_entities("&#65;"), "A");
        assert_eq!(decode_entities("&#8364;"), "\u{20AC}"); // euro sign
    }

    #[test]
    fn hex_numeric_ref() {
        assert_eq!(decode_entities("&#x41;"), "A");
        assert_eq!(decode_entities("&#x20AC;"), "\u{20AC}");
    }

    #[test]
    fn unknown_entity_passed_through() {
        assert_eq!(decode_entities("&foo;"), "&foo;");
    }

    #[test]
    fn bare_ampersand_at_end() {
        // Bare & with no semicolon: pass through.
        assert_eq!(decode_entities("a & b"), "a & b");
    }

    #[test]
    fn multiple_entities() {
        assert_eq!(
            decode_entities("&lt;tag attr=&quot;val&quot;&gt;"),
            "<tag attr=\"val\">"
        );
    }

    #[test]
    fn empty_input() {
        let result = decode_entities("");
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(result, "");
    }

    #[test]
    fn null_char_ref_rejected() {
        // &#0; should be rejected (XML-forbidden character).
        assert_eq!(decode_entities("&#0;"), "&#0;");
        assert_eq!(decode_entities("&#x0;"), "&#x0;");
        assert_eq!(decode_entities("&#x00;"), "&#x00;");
    }

    #[test]
    fn forbidden_control_chars_rejected() {
        // U+0001..U+0008, U+000B.U+000C, U+000E.U+001F are all forbidden.
        assert_eq!(decode_entities("&#1;"), "&#1;");
        assert_eq!(decode_entities("&#8;"), "&#8;");
        assert_eq!(decode_entities("&#x0B;"), "&#x0B;");
        assert_eq!(decode_entities("&#x0C;"), "&#x0C;");
        assert_eq!(decode_entities("&#14;"), "&#14;");
        assert_eq!(decode_entities("&#31;"), "&#31;");
    }

    #[test]
    fn allowed_control_chars_pass_through() {
        // TAB (&#9;), LF (&#10;), CR (&#13;) are permitted in XML.
        assert_eq!(decode_entities("&#9;"), "\t");
        assert_eq!(decode_entities("&#10;"), "\n");
        assert_eq!(decode_entities("&#13;"), "\r");
    }

    #[test]
    fn ampersand_near_multibyte_char_boundary() {
        // Regression: decode_entities used to slice at a byte offset that
        // could land inside a multi-byte UTF-8 character (e.g. BOM \xEF\xBB\xBF).
        let input = "& \u{FEFF}";
        let result = decode_entities(input);
        assert_eq!(result, "& \u{FEFF}");

        // Ampersand followed by enough bytes to push the search window
        // past a 3-byte char boundary.
        let mut long = String::from("&");
        long.push_str(&"x".repeat(30));
        long.push('\u{20AC}'); // euro sign (3 bytes)
        long.push(';');
        // No panic, unknown entity passed through literally.
        let _ = decode_entities(&long);
    }
}

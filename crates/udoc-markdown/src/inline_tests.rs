use super::*;

fn text_of(inlines: &[MdInline]) -> String {
    inlines
        .iter()
        .map(|i| match i {
            MdInline::Text { text, .. } => text.clone(),
            MdInline::Code { text } => format!("`{text}`"),
            MdInline::Link { content, .. } => text_of(content),
            MdInline::Image { alt, .. } => alt.clone(),
            MdInline::SoftBreak => " ".to_string(),
            MdInline::LineBreak => "\n".to_string(),
        })
        .collect()
}

fn no_defs() -> HashMap<String, String> {
    HashMap::new()
}

#[test]
fn plain_text() {
    let result = parse_inlines("hello world", &no_defs());
    assert_eq!(result.len(), 1);
    assert_eq!(text_of(&result), "hello world");
}

#[test]
fn bold_text() {
    let result = parse_inlines("**bold**", &no_defs());
    assert_eq!(result.len(), 1);
    match &result[0] {
        MdInline::Text {
            text, bold, italic, ..
        } => {
            assert_eq!(text, "bold");
            assert!(bold);
            assert!(!italic);
        }
        _ => panic!("expected bold text"),
    }
}

#[test]
fn italic_text() {
    let result = parse_inlines("*italic*", &no_defs());
    assert_eq!(result.len(), 1);
    match &result[0] {
        MdInline::Text {
            text, bold, italic, ..
        } => {
            assert_eq!(text, "italic");
            assert!(!bold);
            assert!(italic);
        }
        _ => panic!("expected italic text"),
    }
}

#[test]
fn bold_italic_text() {
    let result = parse_inlines("***bold italic***", &no_defs());
    assert_eq!(result.len(), 1);
    match &result[0] {
        MdInline::Text {
            text, bold, italic, ..
        } => {
            assert_eq!(text, "bold italic");
            assert!(bold);
            assert!(italic);
        }
        _ => panic!("expected bold+italic text"),
    }
}

#[test]
fn underscore_bold() {
    let result = parse_inlines("__bold__", &no_defs());
    assert_eq!(result.len(), 1);
    match &result[0] {
        MdInline::Text { bold, .. } => assert!(bold),
        _ => panic!("expected bold text"),
    }
}

#[test]
fn underscore_italic() {
    let result = parse_inlines("_italic_", &no_defs());
    assert_eq!(result.len(), 1);
    match &result[0] {
        MdInline::Text { italic, .. } => assert!(italic),
        _ => panic!("expected italic text"),
    }
}

#[test]
fn inline_code() {
    let result = parse_inlines("`code`", &no_defs());
    assert_eq!(result.len(), 1);
    match &result[0] {
        MdInline::Code { text } => assert_eq!(text, "code"),
        _ => panic!("expected code"),
    }
}

#[test]
fn double_backtick_code() {
    let result = parse_inlines("`` code with ` backtick ``", &no_defs());
    assert_eq!(result.len(), 1);
    match &result[0] {
        MdInline::Code { text } => assert_eq!(text, "code with ` backtick"),
        _ => panic!("expected code"),
    }
}

#[test]
fn inline_link() {
    let result = parse_inlines("[click](https://example.com)", &no_defs());
    assert_eq!(result.len(), 1);
    match &result[0] {
        MdInline::Link { url, content } => {
            assert_eq!(url, "https://example.com");
            assert_eq!(text_of(content), "click");
        }
        _ => panic!("expected link"),
    }
}

#[test]
fn reference_link() {
    let mut defs = HashMap::new();
    defs.insert("example".to_string(), "https://example.com".to_string());
    let result = parse_inlines("[click][example]", &defs);
    assert_eq!(result.len(), 1);
    match &result[0] {
        MdInline::Link { url, .. } => assert_eq!(url, "https://example.com"),
        _ => panic!("expected link"),
    }
}

#[test]
fn shortcut_reference_link() {
    let mut defs = HashMap::new();
    defs.insert("example".to_string(), "https://example.com".to_string());
    let result = parse_inlines("[example]", &defs);
    assert_eq!(result.len(), 1);
    match &result[0] {
        MdInline::Link { url, .. } => assert_eq!(url, "https://example.com"),
        _ => panic!("expected link"),
    }
}

#[test]
fn inline_image() {
    let result = parse_inlines("![alt](img.png)", &no_defs());
    assert_eq!(result.len(), 1);
    match &result[0] {
        MdInline::Image { alt, url } => {
            assert_eq!(alt, "alt");
            assert_eq!(url, "img.png");
        }
        _ => panic!("expected image"),
    }
}

#[test]
fn autolink() {
    let result = parse_inlines("<https://example.com>", &no_defs());
    assert_eq!(result.len(), 1);
    match &result[0] {
        MdInline::Link { url, .. } => assert_eq!(url, "https://example.com"),
        _ => panic!("expected link"),
    }
}

#[test]
fn autolink_email() {
    let result = parse_inlines("<user@example.com>", &no_defs());
    assert_eq!(result.len(), 1);
    match &result[0] {
        MdInline::Link { url, .. } => assert_eq!(url, "mailto:user@example.com"),
        _ => panic!("expected email link"),
    }
}

#[test]
fn backslash_escape() {
    let result = parse_inlines("\\*not italic\\*", &no_defs());
    assert_eq!(text_of(&result), "*not italic*");
}

#[test]
fn strikethrough() {
    let result = parse_inlines("~~deleted~~", &no_defs());
    assert_eq!(result.len(), 1);
    match &result[0] {
        MdInline::Text {
            text,
            strikethrough,
            ..
        } => {
            assert_eq!(text, "deleted");
            assert!(strikethrough);
        }
        _ => panic!("expected strikethrough text"),
    }
}

#[test]
fn hard_line_break_backslash() {
    let result = parse_inlines("line one\\\nline two", &no_defs());
    assert!(result.iter().any(|i| matches!(i, MdInline::LineBreak)));
}

#[test]
fn hard_line_break_spaces() {
    let result = parse_inlines("line one  \nline two", &no_defs());
    assert!(result.iter().any(|i| matches!(i, MdInline::LineBreak)));
}

#[test]
fn soft_break() {
    let result = parse_inlines("line one\nline two", &no_defs());
    assert!(result.iter().any(|i| matches!(i, MdInline::SoftBreak)));
}

#[test]
fn empty_input() {
    let result = parse_inlines("", &no_defs());
    assert!(result.is_empty());
}

#[test]
fn mixed_formatting() {
    let result = parse_inlines("normal **bold** and *italic* text", &no_defs());
    assert!(result.len() >= 4, "got {} inlines", result.len());
    assert_eq!(text_of(&result), "normal bold and italic text");
}

#[test]
fn nested_bold_italic() {
    let result = parse_inlines("**bold *bold-italic* bold**", &no_defs());
    let full_text = text_of(&result);
    assert_eq!(full_text, "bold bold-italic bold");
}

#[test]
fn code_takes_precedence_over_emphasis() {
    let result = parse_inlines("`*not italic*`", &no_defs());
    assert_eq!(result.len(), 1);
    match &result[0] {
        MdInline::Code { text } => assert_eq!(text, "*not italic*"),
        _ => panic!("expected code"),
    }
}

#[test]
fn link_with_formatting() {
    let result = parse_inlines("[**bold link**](url)", &no_defs());
    assert_eq!(result.len(), 1);
    match &result[0] {
        MdInline::Link { url, content } => {
            assert_eq!(url, "url");
            assert!(content
                .iter()
                .any(|i| matches!(i, MdInline::Text { bold: true, .. })));
        }
        _ => panic!("expected link"),
    }
}

#[test]
fn unmatched_emphasis_markers() {
    let result = parse_inlines("**unclosed bold", &no_defs());
    // Should produce text, not panic.
    assert_eq!(text_of(&result), "**unclosed bold");
}

#[test]
fn unmatched_brackets() {
    let result = parse_inlines("[unclosed link", &no_defs());
    assert_eq!(text_of(&result), "[unclosed link");
}

#[test]
fn consecutive_code_spans() {
    let result = parse_inlines("`a` `b` `c`", &no_defs());
    let codes: Vec<_> = result
        .iter()
        .filter(|i| matches!(i, MdInline::Code { .. }))
        .collect();
    assert_eq!(codes.len(), 3);
}

#[test]
fn image_reference() {
    let mut defs = HashMap::new();
    defs.insert("logo".to_string(), "logo.png".to_string());
    let result = parse_inlines("![alt][logo]", &defs);
    assert_eq!(result.len(), 1);
    match &result[0] {
        MdInline::Image { alt, url } => {
            assert_eq!(alt, "alt");
            assert_eq!(url, "logo.png");
        }
        _ => panic!("expected image"),
    }
}

#[test]
fn underscore_intraword_not_emphasis() {
    // CommonMark spec: underscores inside words are not emphasis.
    let result = parse_inlines("foo_bar_baz", &no_defs());
    assert_eq!(text_of(&result), "foo_bar_baz");
    // No inline should be italic.
    assert!(
        !result
            .iter()
            .any(|i| matches!(i, MdInline::Text { italic: true, .. })),
        "intraword underscores should not produce emphasis"
    );
}

#[test]
fn underscore_emphasis_at_word_boundary() {
    // Underscores at word boundaries should still produce emphasis.
    let result = parse_inlines("_italic_ word", &no_defs());
    assert!(
        result
            .iter()
            .any(|i| matches!(i, MdInline::Text { italic: true, .. })),
        "underscore at word boundary should produce emphasis"
    );
}

#[test]
fn deeply_nested_emphasis_no_stack_overflow() {
    // 200 levels of nested emphasis, well above MAX_INLINE_DEPTH=128.
    let mut input = String::new();
    for _ in 0..200 {
        input.push('*');
    }
    input.push_str("deep");
    for _ in 0..200 {
        input.push('*');
    }
    // Should not panic or stack overflow.
    let result = parse_inlines(&input, &no_defs());
    let text = text_of(&result);
    assert!(text.contains("deep"), "got: {text}");
}

#[test]
fn deeply_nested_links_no_stack_overflow() {
    // Build nested links: [[[...text.](...)](...)](#)
    let mut input = String::new();
    for _ in 0..200 {
        input.push('[');
    }
    input.push_str("text");
    for _ in 0..200 {
        input.push_str("](url)");
    }
    // Should not panic or stack overflow.
    let result = parse_inlines(&input, &no_defs());
    let text = text_of(&result);
    assert!(!text.is_empty());
}

// -- Entity reference tests --

#[test]
fn entity_named_amp() {
    let result = parse_inlines("AT&amp;T", &no_defs());
    assert_eq!(text_of(&result), "AT&T");
}

#[test]
fn entity_named_common() {
    let result = parse_inlines("&lt;tag&gt; &amp; &quot;text&quot;", &no_defs());
    assert_eq!(text_of(&result), "<tag> & \"text\"");
}

#[test]
fn entity_numeric_decimal() {
    let result = parse_inlines("&#65;&#66;&#67;", &no_defs());
    assert_eq!(text_of(&result), "ABC");
}

#[test]
fn entity_numeric_hex() {
    let result = parse_inlines("&#x41;&#x42;&#x43;", &no_defs());
    assert_eq!(text_of(&result), "ABC");
}

#[test]
fn entity_zero_codepoint() {
    // U+0000 should become U+FFFD per spec.
    let result = parse_inlines("&#0;", &no_defs());
    assert_eq!(text_of(&result), "\u{FFFD}");
}

#[test]
fn entity_invalid_name() {
    // Unknown entity should be left as literal text.
    let result = parse_inlines("&notarealentity;", &no_defs());
    assert_eq!(text_of(&result), "&notarealentity;");
}

#[test]
fn entity_no_semicolon() {
    // Missing semicolon: not an entity.
    let result = parse_inlines("&amp text", &no_defs());
    assert_eq!(text_of(&result), "&amp text");
}

#[test]
fn entity_nbsp() {
    let result = parse_inlines("a&nbsp;b", &no_defs());
    assert_eq!(text_of(&result), "a\u{00A0}b");
}

#[test]
fn entity_inside_code_not_decoded() {
    // Entities inside code spans should NOT be decoded.
    let result = parse_inlines("`&amp;`", &no_defs());
    match &result[0] {
        MdInline::Code { text } => assert_eq!(text, "&amp;"),
        _ => panic!("expected code span"),
    }
}

// -- Emphasis algorithm tests --

#[test]
fn emphasis_partial_consumption() {
    // ***text* leaves ** as literal.
    let result = parse_inlines("***text*", &no_defs());
    let text = text_of(&result);
    assert!(text.contains("text"), "got: {text}");
    // Should have italic "text" and literal "**".
    assert!(
        result
            .iter()
            .any(|i| matches!(i, MdInline::Text { italic: true, .. })),
        "expected italic"
    );
}

#[test]
fn emphasis_strong_then_em() {
    // ***bold italic*** -> bold+italic
    let result = parse_inlines("***bold italic***", &no_defs());
    assert!(result.iter().any(|i| matches!(
        i,
        MdInline::Text {
            bold: true,
            italic: true,
            ..
        }
    )));
}

#[test]
fn emphasis_nested_strong_in_em() {
    // *foo **bar** baz* -> italic("foo " bold("bar") " baz")
    let result = parse_inlines("*foo **bar** baz*", &no_defs());
    let text = text_of(&result);
    assert_eq!(text, "foo bar baz");
    // bar should be bold+italic
    assert!(
        result.iter().any(|i| matches!(
            i,
            MdInline::Text {
                bold: true,
                italic: true,
                ..
            }
        )),
        "expected bold+italic: {result:?}"
    );
}

#[test]
fn emphasis_multiple_of_3_rule() {
    // When can_open && can_close with sum%3==0, skip match.
    // a]  *foo _bar* baz_ -- should NOT match *foo.* across underscores
    // The * delimiters should match, _ should be literal.
    let result = parse_inlines("*foo _bar* baz_", &no_defs());
    let text = text_of(&result);
    assert!(text.contains("foo"), "got: {text}");
}

#[test]
fn strikethrough_propagates_to_links() {
    let result = parse_inlines("~~[link text](url)~~", &no_defs());
    // The link content should have strikethrough applied.
    let has_link_with_st = result.iter().any(|i| {
        if let MdInline::Link { content, .. } = i {
            content.iter().any(|c| {
                matches!(
                    c,
                    MdInline::Text {
                        strikethrough: true,
                        ..
                    }
                )
            })
        } else {
            false
        }
    });
    assert!(
        has_link_with_st,
        "strikethrough should propagate into link content: {result:?}"
    );
}

// -- UTF-8 flanking regression tests --

#[test]
fn emphasis_after_multibyte_char() {
    // The 'e' in cafe is U+00E9 (2 bytes). Delimiter flanking must decode
    // the full character, not just the last byte.
    let result = parse_inlines("caf\u{00E9}*text*", &no_defs());
    assert!(
        result
            .iter()
            .any(|i| matches!(i, MdInline::Text { italic: true, .. })),
        "emphasis after multi-byte char should work: {result:?}"
    );
    assert_eq!(text_of(&result), "caf\u{00E9}text");
}

#[test]
fn emphasis_before_multibyte_char() {
    let result = parse_inlines("*text*\u{00E9}suite", &no_defs());
    assert!(
        result
            .iter()
            .any(|i| matches!(i, MdInline::Text { italic: true, .. })),
        "emphasis before multi-byte char should work: {result:?}"
    );
    assert_eq!(text_of(&result), "text\u{00E9}suite");
}

#[test]
fn underscore_intraword_with_multibyte() {
    // Underscores between multi-byte chars should not produce emphasis.
    let result = parse_inlines("caf\u{00E9}_lait_cr\u{00E8}me", &no_defs());
    assert!(
        !result
            .iter()
            .any(|i| matches!(i, MdInline::Text { italic: true, .. })),
        "intraword underscore with multi-byte chars should not be emphasis: {result:?}"
    );
}

// -- Backslash-escape ampersand test --

#[test]
fn backslash_escape_ampersand() {
    // CommonMark: \& should produce a literal &.
    let result = parse_inlines("\\&", &no_defs());
    assert_eq!(text_of(&result), "&");
}

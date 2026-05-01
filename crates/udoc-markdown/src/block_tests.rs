use super::*;

#[test]
fn parse_atx_headings() {
    let result = parse_blocks("# H1\n## H2\n### H3\n#### H4\n##### H5\n###### H6");
    assert_eq!(result.blocks.len(), 6);
    for (i, block) in result.blocks.iter().enumerate() {
        match block {
            MdBlock::Heading { level, .. } => assert_eq!(*level, (i + 1) as u8),
            _ => panic!("expected heading at index {i}"),
        }
    }
}

#[test]
fn parse_heading_content() {
    let result = parse_blocks("# Hello World");
    assert_eq!(result.blocks.len(), 1);
    match &result.blocks[0] {
        MdBlock::Heading { level, content } => {
            assert_eq!(*level, 1);
            assert_eq!(content.len(), 1);
            match &content[0] {
                MdInline::Text { text, .. } => assert_eq!(text, "Hello World"),
                _ => panic!("expected text inline"),
            }
        }
        _ => panic!("expected heading"),
    }
}

#[test]
fn parse_heading_closing_hashes() {
    let result = parse_blocks("## Heading ##");
    match &result.blocks[0] {
        MdBlock::Heading { level, content } => {
            assert_eq!(*level, 2);
            match &content[0] {
                MdInline::Text { text, .. } => assert_eq!(text, "Heading"),
                _ => panic!("expected text"),
            }
        }
        _ => panic!("expected heading"),
    }
}

#[test]
fn parse_paragraph() {
    let result = parse_blocks("Hello world.\nThis is a paragraph.");
    assert_eq!(result.blocks.len(), 1);
    assert!(matches!(&result.blocks[0], MdBlock::Paragraph { .. }));
}

#[test]
fn parse_thematic_break() {
    for input in &["---", "***", "___", "- - -", "* * *"] {
        let result = parse_blocks(input);
        assert_eq!(result.blocks.len(), 1, "input: {input}");
        assert!(
            matches!(&result.blocks[0], MdBlock::ThematicBreak),
            "input: {input}"
        );
    }
}

#[test]
fn parse_fenced_code_block() {
    let result = parse_blocks("```rust\nfn main() {}\n```");
    assert_eq!(result.blocks.len(), 1);
    match &result.blocks[0] {
        MdBlock::CodeBlock { text, language } => {
            assert_eq!(text, "fn main() {}");
            assert_eq!(language.as_deref(), Some("rust"));
        }
        _ => panic!("expected code block"),
    }
}

#[test]
fn parse_fenced_code_tilde() {
    let result = parse_blocks("~~~\ncode here\n~~~");
    assert_eq!(result.blocks.len(), 1);
    match &result.blocks[0] {
        MdBlock::CodeBlock { text, language } => {
            assert_eq!(text, "code here");
            assert!(language.is_none());
        }
        _ => panic!("expected code block"),
    }
}

#[test]
fn parse_indented_code_block() {
    let result = parse_blocks("    code line 1\n    code line 2");
    assert_eq!(result.blocks.len(), 1);
    match &result.blocks[0] {
        MdBlock::CodeBlock { text, language } => {
            assert_eq!(text, "code line 1\ncode line 2");
            assert!(language.is_none());
        }
        _ => panic!("expected code block"),
    }
}

#[test]
fn parse_unordered_list() {
    let result = parse_blocks("- item one\n- item two\n- item three");
    assert_eq!(result.blocks.len(), 1);
    match &result.blocks[0] {
        MdBlock::List {
            items,
            ordered,
            start,
        } => {
            assert!(!ordered);
            assert_eq!(*start, 1);
            assert_eq!(items.len(), 3);
        }
        _ => panic!("expected list"),
    }
}

#[test]
fn parse_ordered_list() {
    let result = parse_blocks("1. first\n2. second\n3. third");
    assert_eq!(result.blocks.len(), 1);
    match &result.blocks[0] {
        MdBlock::List {
            items,
            ordered,
            start,
        } => {
            assert!(ordered);
            assert_eq!(*start, 1);
            assert_eq!(items.len(), 3);
        }
        _ => panic!("expected list"),
    }
}

#[test]
fn parse_blockquote() {
    let result = parse_blocks("> Hello\n> World");
    assert_eq!(result.blocks.len(), 1);
    match &result.blocks[0] {
        MdBlock::Blockquote { children } => {
            assert_eq!(children.len(), 1);
            assert!(matches!(&children[0], MdBlock::Paragraph { .. }));
        }
        _ => panic!("expected blockquote"),
    }
}

#[test]
fn parse_nested_blockquote() {
    let result = parse_blocks("> > nested quote");
    assert_eq!(result.blocks.len(), 1);
    match &result.blocks[0] {
        MdBlock::Blockquote { children } => {
            assert_eq!(children.len(), 1);
            assert!(matches!(&children[0], MdBlock::Blockquote { .. }));
        }
        _ => panic!("expected blockquote"),
    }
}

#[test]
fn parse_table() {
    let input = "| A | B |\n| --- | --- |\n| 1 | 2 |";
    let result = parse_blocks(input);
    assert_eq!(result.blocks.len(), 1);
    match &result.blocks[0] {
        MdBlock::Table {
            header,
            rows,
            col_count,
        } => {
            assert_eq!(*col_count, 2);
            assert_eq!(header.len(), 2);
            assert_eq!(rows.len(), 1);
        }
        _ => panic!("expected table"),
    }
}

#[test]
fn parse_link_ref_def() {
    let result = parse_blocks("[foo]: https://example.com\n\n[foo]");
    assert_eq!(
        result.link_defs.get("foo").map(|s| s.as_str()),
        Some("https://example.com")
    );
}

#[test]
fn forward_reference_link_resolves() {
    // Link ref def appears AFTER the reference. Pre-scan should resolve it.
    let result = parse_blocks("A [click][ref1] here.\n\n[ref1]: https://example.com");
    assert_eq!(result.blocks.len(), 1);
    match &result.blocks[0] {
        MdBlock::Paragraph { content } => {
            let has_link = content
                .iter()
                .any(|i| matches!(i, MdInline::Link { url, .. } if url == "https://example.com"));
            assert!(has_link, "forward reference should resolve to a link");
        }
        _ => panic!("expected paragraph"),
    }
}

#[test]
fn parse_empty_input() {
    let result = parse_blocks("");
    assert!(result.blocks.is_empty());
}

#[test]
fn parse_only_whitespace() {
    let result = parse_blocks("   \n  \n\n");
    assert!(result.blocks.is_empty());
}

#[test]
fn parse_crlf_line_endings() {
    let result = parse_blocks("# Heading\r\n\r\nParagraph\r\n");
    assert_eq!(result.blocks.len(), 2);
    assert!(matches!(&result.blocks[0], MdBlock::Heading { .. }));
    assert!(matches!(&result.blocks[1], MdBlock::Paragraph { .. }));
}

#[test]
fn parse_unclosed_fenced_code() {
    let result = parse_blocks("```\ncode without closing");
    assert_eq!(result.blocks.len(), 1);
    assert!(matches!(&result.blocks[0], MdBlock::CodeBlock { .. }));
    assert!(
        result
            .warnings
            .iter()
            .any(|(kind, _)| kind == "UnclosedCodeFence"),
        "expected UnclosedCodeFence warning, got: {:?}",
        result.warnings
    );
}

#[test]
fn thematic_break_not_list() {
    // `---` should be thematic break, not a list with `-` marker.
    let result = parse_blocks("---");
    assert_eq!(result.blocks.len(), 1);
    assert!(matches!(&result.blocks[0], MdBlock::ThematicBreak));
}

#[test]
fn hash_without_space_is_not_heading() {
    let result = parse_blocks("#notaheading");
    assert_eq!(result.blocks.len(), 1);
    assert!(matches!(&result.blocks[0], MdBlock::Paragraph { .. }));
}

#[test]
fn empty_heading() {
    let result = parse_blocks("#");
    assert_eq!(result.blocks.len(), 1);
    match &result.blocks[0] {
        MdBlock::Heading { level, content } => {
            assert_eq!(*level, 1);
            assert!(
                content.is_empty()
                    || content.iter().all(|i| match i {
                        MdInline::Text { text, .. } => text.is_empty(),
                        _ => false,
                    })
            );
        }
        _ => panic!("expected heading"),
    }
}

#[test]
fn seven_hashes_not_heading() {
    let result = parse_blocks("####### not a heading");
    assert_eq!(result.blocks.len(), 1);
    assert!(matches!(&result.blocks[0], MdBlock::Paragraph { .. }));
}

#[test]
fn parse_image_block() {
    let result = parse_blocks("![alt text](https://example.com/image.png)");
    assert_eq!(result.blocks.len(), 1);
    match &result.blocks[0] {
        MdBlock::Image { alt, url } => {
            assert_eq!(alt, "alt text");
            assert_eq!(url, "https://example.com/image.png");
        }
        _ => panic!("expected image block, got: {:?}", result.blocks[0]),
    }
}

#[test]
fn parse_image_block_with_trailing_newline() {
    // Image followed by a soft break (from trailing newline in paragraph
    // continuation) should still be detected as a block-level image.
    let result = parse_blocks("![alt](img.png)\n");
    assert_eq!(result.blocks.len(), 1);
    assert!(
        matches!(&result.blocks[0], MdBlock::Image { .. }),
        "image with trailing newline should be block-level image, got: {:?}",
        result.blocks[0]
    );
}

#[test]
fn multiple_blocks_mixed() {
    let input = "# Title\n\nA paragraph.\n\n- list item\n\n> quote\n\n---\n\n```\ncode\n```";
    let result = parse_blocks(input);
    assert!(
        result.blocks.len() >= 5,
        "got {} blocks",
        result.blocks.len()
    );
}

#[test]
fn table_separator_detection() {
    assert!(table::is_separator("| --- | --- |"));
    assert!(table::is_separator("|---|---|"));
    assert!(table::is_separator("| :--- | ---: |"));
    assert!(!table::is_separator("| abc | def |"));
    assert!(!table::is_separator("not a separator"));
}

#[test]
fn list_with_star_marker() {
    let result = parse_blocks("* item one\n* item two");
    assert_eq!(result.blocks.len(), 1);
    match &result.blocks[0] {
        MdBlock::List { items, ordered, .. } => {
            assert!(!ordered);
            assert_eq!(items.len(), 2);
        }
        _ => panic!("expected list"),
    }
}

#[test]
fn list_with_plus_marker() {
    let result = parse_blocks("+ item one\n+ item two");
    assert_eq!(result.blocks.len(), 1);
    match &result.blocks[0] {
        MdBlock::List { items, ordered, .. } => {
            assert!(!ordered);
            assert_eq!(items.len(), 2);
        }
        _ => panic!("expected list"),
    }
}

#[test]
fn deeply_nested_blockquotes_hit_depth_limit() {
    // 300 levels of nesting, well above MAX_DEPTH=256.
    let mut input = String::new();
    for _ in 0..300 {
        input.push_str("> ");
    }
    input.push_str("deep text");
    let result = parse_blocks(&input);
    // Should not panic and should produce a warning about depth.
    assert!(
        result
            .warnings
            .iter()
            .any(|(kind, _)| kind == "MaxDepthExceeded"),
        "expected MaxDepthExceeded warning, got: {:?}",
        result.warnings
    );
    assert!(!result.blocks.is_empty());
}

#[test]
fn prose_with_pipes_not_table() {
    // Lines containing `|` but not starting/ending with `|` should be
    // parsed as paragraphs, not mistaken for table rows.
    let result = parse_blocks("The result is a | b or c.");
    assert_eq!(result.blocks.len(), 1);
    assert!(
        matches!(&result.blocks[0], MdBlock::Paragraph { .. }),
        "prose with pipes should be paragraph, got: {:?}",
        result.blocks[0]
    );
}

#[test]
fn link_ref_def_inside_blockquote_resolves() {
    // Link ref defs inside blockquotes should be found by the pre-scan
    // and available to references outside the blockquote.
    let input = "> [myref]: https://example.com\n\n[myref]";
    let result = parse_blocks(input);
    assert!(
        result.link_defs.contains_key("myref"),
        "link ref def inside blockquote should be found by pre-scan"
    );
}

#[test]
fn link_ref_def_inside_list_resolves() {
    // Link ref defs inside list items should be found by the pre-scan.
    let input = "- [listref]: https://example.com\n\n[listref]";
    let result = parse_blocks(input);
    assert!(
        result.link_defs.contains_key("listref"),
        "link ref def inside list item should be found by pre-scan"
    );
}

// -- Setext heading tests --

#[test]
fn setext_heading_h1() {
    let result = parse_blocks("Heading\n===");
    assert_eq!(result.blocks.len(), 1);
    match &result.blocks[0] {
        MdBlock::Heading { level, content } => {
            assert_eq!(*level, 1);
            assert_eq!(content.len(), 1);
            match &content[0] {
                MdInline::Text { text, .. } => assert_eq!(text, "Heading"),
                _ => panic!("expected text"),
            }
        }
        _ => panic!("expected heading, got: {:?}", result.blocks[0]),
    }
}

#[test]
fn setext_heading_h2() {
    let result = parse_blocks("Heading\n---");
    assert_eq!(result.blocks.len(), 1);
    match &result.blocks[0] {
        MdBlock::Heading { level, .. } => assert_eq!(*level, 2),
        _ => panic!("expected heading, got: {:?}", result.blocks[0]),
    }
}

#[test]
fn setext_heading_long_underline() {
    let result = parse_blocks("Heading\n======");
    assert_eq!(result.blocks.len(), 1);
    match &result.blocks[0] {
        MdBlock::Heading { level, .. } => assert_eq!(*level, 1),
        _ => panic!("expected heading"),
    }
}

#[test]
fn setext_heading_multiline_content() {
    let result = parse_blocks("Line one\nLine two\n===");
    assert_eq!(result.blocks.len(), 1);
    match &result.blocks[0] {
        MdBlock::Heading { level, .. } => assert_eq!(*level, 1),
        _ => panic!("expected heading, got: {:?}", result.blocks[0]),
    }
}

#[test]
fn standalone_dashes_still_thematic_break() {
    // `---` alone (not after paragraph text) should be a thematic break.
    let result = parse_blocks("---");
    assert_eq!(result.blocks.len(), 1);
    assert!(
        matches!(&result.blocks[0], MdBlock::ThematicBreak),
        "standalone --- should be thematic break, got: {:?}",
        result.blocks[0]
    );
}

#[test]
fn setext_h2_vs_thematic_break() {
    // `---` after paragraph text is setext H2, not thematic break.
    let result = parse_blocks("Heading\n---");
    assert_eq!(result.blocks.len(), 1);
    assert!(
        matches!(&result.blocks[0], MdBlock::Heading { level: 2, .. }),
        "--- after text should be setext heading, got: {:?}",
        result.blocks[0]
    );
}

#[test]
fn setext_heading_with_blank_line_before() {
    // Blank line before text + underline: text is a paragraph, underline is thematic break.
    let result = parse_blocks("\nHeading\n===");
    let headings: Vec<_> = result
        .blocks
        .iter()
        .filter(|b| matches!(b, MdBlock::Heading { .. }))
        .collect();
    assert_eq!(headings.len(), 1);
}

// -- Table cell mismatch warning tests --

#[test]
fn table_cell_mismatch_warns() {
    // Data row has more cells than header.
    let input = "| A | B |\n| --- | --- |\n| 1 | 2 | 3 |";
    let result = parse_blocks(input);
    assert!(
        result
            .warnings
            .iter()
            .any(|(kind, _)| kind == "TableCellMismatch"),
        "expected TableCellMismatch warning, got: {:?}",
        result.warnings
    );
}

// -- Malformed recovery tests: assert structured warnings --

#[test]
fn unclosed_fenced_code_emits_structured_warning() {
    let result = parse_blocks("```python\nprint('hello')\nno closing fence");
    assert_eq!(result.blocks.len(), 1);
    assert!(matches!(&result.blocks[0], MdBlock::CodeBlock { .. }));
    let fence_warnings: Vec<_> = result
        .warnings
        .iter()
        .filter(|(kind, _)| kind == "UnclosedCodeFence")
        .collect();
    assert_eq!(
        fence_warnings.len(),
        1,
        "expected exactly 1 UnclosedCodeFence warning, got: {:?}",
        result.warnings
    );
}

#[test]
fn max_depth_warning_includes_line_info() {
    // Verify the MaxDepthExceeded warning message includes the line number.
    let mut input = String::new();
    for _ in 0..300 {
        input.push_str("> ");
    }
    input.push_str("deep");
    let result = parse_blocks(&input);
    let depth_warnings: Vec<_> = result
        .warnings
        .iter()
        .filter(|(kind, _)| kind == "MaxDepthExceeded")
        .collect();
    assert!(
        !depth_warnings.is_empty(),
        "expected MaxDepthExceeded warning"
    );
    assert!(
        depth_warnings[0].1.contains("line"),
        "warning message should mention line number, got: {}",
        depth_warnings[0].1
    );
}

#[test]
fn unresolved_reference_link_emits_warning() {
    let result = parse_blocks("See [click here][nonexistent] for details.");
    assert_eq!(result.blocks.len(), 1);
    let ref_warnings: Vec<_> = result
        .warnings
        .iter()
        .filter(|(kind, _)| kind == "UnresolvedReferenceLink")
        .collect();
    assert_eq!(
        ref_warnings.len(),
        1,
        "expected exactly 1 UnresolvedReferenceLink warning, got: {:?}",
        result.warnings
    );
    assert!(
        ref_warnings[0].1.contains("nonexistent"),
        "warning message should mention the unresolved label, got: {}",
        ref_warnings[0].1
    );
}

#[test]
fn resolved_reference_link_no_warning() {
    let result = parse_blocks("[link]: https://example.com\n\nSee [click][link] for details.");
    let ref_warnings: Vec<_> = result
        .warnings
        .iter()
        .filter(|(kind, _)| kind == "UnresolvedReferenceLink")
        .collect();
    assert!(
        ref_warnings.is_empty(),
        "resolved reference link should not produce warning, got: {:?}",
        ref_warnings
    );
}

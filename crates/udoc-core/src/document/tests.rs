use std::collections::HashMap;
use std::sync::atomic::AtomicU64;

use super::*;
use crate::document::table::{CellValue, TableCell, TableData, TableRow};

// ---------------------------------------------------------------------------
// NodeId
// ---------------------------------------------------------------------------

#[test]
fn node_id_display() {
    // Display is now "node:{id}".
    assert_eq!(format!("{}", NodeId::new(42)), "node:42");
    assert_eq!(format!("{}", NodeId::new(0)), "node:0");
    assert_eq!(format!("{}", NodeId::new(1234)), "node:1234");
    // Round-trip via Debug stays untouched.
    assert_eq!(format!("{:?}", NodeId::new(42)), "NodeId(42)");
}

#[test]
fn node_id_ordering() {
    assert!(NodeId::new(1) < NodeId::new(2));
    assert!(NodeId::new(0) == NodeId::new(0));
    assert!(NodeId::new(10) > NodeId::new(5));
}

#[test]
fn node_id_hash() {
    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(NodeId::new(1));
    set.insert(NodeId::new(2));
    set.insert(NodeId::new(1)); // duplicate
    assert_eq!(set.len(), 2);
}

// ---------------------------------------------------------------------------
// Document construction and defaults
// ---------------------------------------------------------------------------

#[test]
fn document_new_empty() {
    let doc = Document::new();
    assert!(doc.content.is_empty());
    assert!(doc.presentation.is_none());
    assert!(doc.relationships.is_none());
    assert!(doc.interactions.is_none());
    assert!(doc.assets.is_empty());
    assert!(doc.metadata.title.is_none());
    assert_eq!(doc.metadata.page_count, 0);
}

#[test]
fn document_default() {
    let doc = Document::default();
    assert!(doc.content.is_empty());
}

// ---------------------------------------------------------------------------
// alloc_node_id
// ---------------------------------------------------------------------------

#[test]
fn alloc_node_id_sequential() {
    let doc = Document::new();
    assert_eq!(doc.alloc_node_id(), NodeId::new(0));
    assert_eq!(doc.alloc_node_id(), NodeId::new(1));
    assert_eq!(doc.alloc_node_id(), NodeId::new(2));
    assert_eq!(doc.alloc_node_id(), NodeId::new(3));
}

#[test]
fn try_alloc_node_id_returns_some() {
    let doc = Document::new();
    assert_eq!(doc.try_alloc_node_id(), Some(NodeId::new(0)));
    assert_eq!(doc.try_alloc_node_id(), Some(NodeId::new(1)));
}

#[test]
fn try_alloc_node_id_returns_none_at_limit() {
    let doc = Document {
        content: Vec::new(),
        presentation: None,
        relationships: None,
        metadata: DocumentMetadata::default(),
        interactions: None,
        assets: AssetStore::new(),
        diagnostics: Vec::new(),
        is_encrypted: false,
        next_node_id: AtomicU64::new(MAX_NODE_ID),
    };
    assert_eq!(doc.try_alloc_node_id(), None);
    // Regular alloc still works after failed try (counter rolled back)
    // Actually, the counter was at MAX and try_alloc incremented then rolled
    // back, so it's still at MAX. try again should also be None.
    assert_eq!(doc.try_alloc_node_id(), None);
}

#[test]
fn alloc_node_id_many() {
    let doc = Document::new();
    for i in 0..100 {
        assert_eq!(doc.alloc_node_id(), NodeId::new(i));
    }
}

// ---------------------------------------------------------------------------
// Document clone
// ---------------------------------------------------------------------------

#[test]
fn document_clone_continues_ids() {
    let doc = Document::new();
    let _ = doc.alloc_node_id(); // 0
    let _ = doc.alloc_node_id(); // 1

    let cloned = doc.clone();
    // Cloned doc should continue from where the original left off
    assert_eq!(cloned.alloc_node_id(), NodeId::new(2));
    // Original should also continue independently
    assert_eq!(doc.alloc_node_id(), NodeId::new(2));
}

#[test]
fn document_clone_content_matches() {
    let mut doc = Document::new();
    doc.content.push(Block::Paragraph {
        id: NodeId::new(0),
        content: vec![Inline::Text {
            id: NodeId::new(1),
            text: "hello".into(),
            style: SpanStyle::default(),
        }],
    });
    doc.metadata.title = Some("Test".into());
    doc.metadata.page_count = 3;

    let cloned = doc.clone();
    assert_eq!(cloned.content.len(), 1);
    assert_eq!(cloned.content[0].text(), "hello");
    assert_eq!(cloned.metadata.title.as_deref(), Some("Test"));
    assert_eq!(cloned.metadata.page_count, 3);
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

#[test]
fn document_metadata_fields() {
    let meta = DocumentMetadata {
        title: Some("Test".into()),
        author: Some("Author".into()),
        page_count: 5,
        properties: {
            let mut m = HashMap::new();
            m.insert("key".into(), "value".into());
            m
        },
        ..Default::default()
    };
    assert_eq!(meta.title.as_deref(), Some("Test"));
    assert_eq!(meta.page_count, 5);
    assert_eq!(
        meta.properties.get("key").map(|s| s.as_str()),
        Some("value")
    );
}

// ---------------------------------------------------------------------------
// walk() -- comprehensive tests
// ---------------------------------------------------------------------------

#[test]
fn walk_empty_doc() {
    let doc = Document::new();
    let mut count = 0;
    doc.walk(&mut |_| count += 1);
    assert_eq!(count, 0);
}

#[test]
fn walk_flat_paragraphs() {
    let mut doc = Document::new();
    doc.content.push(Block::Paragraph {
        id: NodeId::new(0),
        content: vec![],
    });
    doc.content.push(Block::Paragraph {
        id: NodeId::new(1),
        content: vec![],
    });

    let mut visited = Vec::new();
    doc.walk(&mut |b| visited.push(b.id()));
    assert_eq!(visited, vec![NodeId::new(0), NodeId::new(1)]);
}

#[test]
fn walk_section_children() {
    let doc = Document {
        content: vec![Block::Section {
            id: NodeId::new(0),
            role: None,
            children: vec![
                Block::Paragraph {
                    id: NodeId::new(1),
                    content: vec![],
                },
                Block::Paragraph {
                    id: NodeId::new(2),
                    content: vec![],
                },
            ],
        }],
        presentation: None,
        relationships: None,
        metadata: DocumentMetadata::default(),
        interactions: None,
        assets: AssetStore::new(),
        diagnostics: Vec::new(),
        is_encrypted: false,
        next_node_id: AtomicU64::new(3),
    };

    let mut visited = Vec::new();
    doc.walk(&mut |b| visited.push(b.id()));
    assert_eq!(
        visited,
        vec![NodeId::new(0), NodeId::new(1), NodeId::new(2)]
    );
}

#[test]
fn walk_nested_list() {
    // List with items containing paragraphs -- verify walk visits all inner blocks.
    let doc = Document {
        content: vec![Block::List {
            id: NodeId::new(0),
            items: vec![
                ListItem {
                    id: NodeId::new(1),
                    content: vec![
                        Block::Paragraph {
                            id: NodeId::new(2),
                            content: vec![],
                        },
                        Block::Paragraph {
                            id: NodeId::new(3),
                            content: vec![],
                        },
                    ],
                },
                ListItem {
                    id: NodeId::new(4),
                    content: vec![Block::Paragraph {
                        id: NodeId::new(5),
                        content: vec![],
                    }],
                },
            ],
            kind: ListKind::Unordered,
            start: 1,
        }],
        presentation: None,
        relationships: None,
        metadata: DocumentMetadata::default(),
        interactions: None,
        assets: AssetStore::new(),
        diagnostics: Vec::new(),
        is_encrypted: false,
        next_node_id: AtomicU64::new(6),
    };

    let mut visited = Vec::new();
    doc.walk(&mut |b| visited.push(b.id()));
    // List(0), Paragraph(2), Paragraph(3) [item 0], Paragraph(5) [item 1]
    assert_eq!(
        visited,
        vec![
            NodeId::new(0),
            NodeId::new(2),
            NodeId::new(3),
            NodeId::new(5)
        ]
    );
}

#[test]
fn walk_table_cells() {
    let doc = Document {
        content: vec![Block::Table {
            id: NodeId::new(0),
            table: TableData {
                rows: vec![TableRow {
                    id: NodeId::new(1),
                    cells: vec![
                        TableCell {
                            id: NodeId::new(2),
                            content: vec![Block::Paragraph {
                                id: NodeId::new(3),
                                content: vec![],
                            }],
                            col_span: 1,
                            row_span: 1,
                            value: None,
                        },
                        TableCell {
                            id: NodeId::new(4),
                            content: vec![Block::Paragraph {
                                id: NodeId::new(5),
                                content: vec![],
                            }],
                            col_span: 1,
                            row_span: 1,
                            value: None,
                        },
                    ],
                    is_header: false,
                }],
                num_columns: 2,
                header_row_count: 0,
                may_continue_from_previous: false,
                may_continue_to_next: false,
            },
        }],
        presentation: None,
        relationships: None,
        metadata: DocumentMetadata::default(),
        interactions: None,
        assets: AssetStore::new(),
        diagnostics: Vec::new(),
        is_encrypted: false,
        next_node_id: AtomicU64::new(6),
    };

    let mut visited = Vec::new();
    doc.walk(&mut |b| visited.push(b.id()));
    // Table(0), Paragraph(3) [cell 0], Paragraph(5) [cell 1]
    assert_eq!(
        visited,
        vec![NodeId::new(0), NodeId::new(3), NodeId::new(5)]
    );
}

#[test]
fn walk_shape() {
    let doc = Document {
        content: vec![Block::Shape {
            id: NodeId::new(0),
            kind: ShapeKind::Group,
            children: vec![Block::Paragraph {
                id: NodeId::new(1),
                content: vec![],
            }],
            alt_text: None,
        }],
        presentation: None,
        relationships: None,
        metadata: DocumentMetadata::default(),
        interactions: None,
        assets: AssetStore::new(),
        diagnostics: Vec::new(),
        is_encrypted: false,
        next_node_id: AtomicU64::new(2),
    };

    let mut visited = Vec::new();
    doc.walk(&mut |b| visited.push(b.id()));
    assert_eq!(visited, vec![NodeId::new(0), NodeId::new(1)]);
}

#[test]
fn walk_complex_tree() {
    // Heading + Section(Paragraph, List(Paragraph)) + Table(Paragraph)
    let doc = Document {
        content: vec![
            Block::Heading {
                id: NodeId::new(0),
                level: 1,
                content: vec![Inline::Text {
                    id: NodeId::new(1),
                    text: "Title".into(),
                    style: SpanStyle::default(),
                }],
            },
            Block::Section {
                id: NodeId::new(2),
                role: None,
                children: vec![
                    Block::Paragraph {
                        id: NodeId::new(3),
                        content: vec![],
                    },
                    Block::List {
                        id: NodeId::new(4),
                        items: vec![ListItem {
                            id: NodeId::new(5),
                            content: vec![Block::Paragraph {
                                id: NodeId::new(6),
                                content: vec![],
                            }],
                        }],
                        kind: ListKind::Unordered,
                        start: 1,
                    },
                ],
            },
            Block::Table {
                id: NodeId::new(7),
                table: TableData {
                    rows: vec![TableRow {
                        id: NodeId::new(8),
                        cells: vec![TableCell {
                            id: NodeId::new(9),
                            content: vec![Block::Paragraph {
                                id: NodeId::new(10),
                                content: vec![],
                            }],
                            col_span: 1,
                            row_span: 1,
                            value: None,
                        }],
                        is_header: false,
                    }],
                    num_columns: 1,
                    header_row_count: 0,
                    may_continue_from_previous: false,
                    may_continue_to_next: false,
                },
            },
        ],
        presentation: None,
        relationships: None,
        metadata: DocumentMetadata::default(),
        interactions: None,
        assets: AssetStore::new(),
        diagnostics: Vec::new(),
        is_encrypted: false,
        next_node_id: AtomicU64::new(11),
    };

    let mut visited = Vec::new();
    doc.walk(&mut |b| visited.push(b.id()));
    assert_eq!(
        visited,
        vec![
            NodeId::new(0),  // Heading
            NodeId::new(2),  // Section
            NodeId::new(3),  // Paragraph (in section)
            NodeId::new(4),  // List (in section)
            NodeId::new(6),  // Paragraph (in list item)
            NodeId::new(7),  // Table
            NodeId::new(10), // Paragraph (in table cell)
        ]
    );
}

#[test]
fn walk_mut_modifies() {
    let mut doc = Document::new();
    doc.content.push(Block::Paragraph {
        id: NodeId::new(0),
        content: vec![Inline::Text {
            id: NodeId::new(1),
            text: "hello".into(),
            style: SpanStyle::default(),
        }],
    });
    doc.content.push(Block::Section {
        id: NodeId::new(2),
        role: None,
        children: vec![Block::Paragraph {
            id: NodeId::new(3),
            content: vec![],
        }],
    });

    let mut count = 0;
    doc.walk_mut(&mut |_block| {
        count += 1;
    });
    // Paragraph(0), Section(2), Paragraph(3) = 3 blocks
    assert_eq!(count, 3);
}

// ---------------------------------------------------------------------------
// Section nesting -- walk visits all nested sections
// ---------------------------------------------------------------------------

#[test]
fn walk_nested_sections() {
    let doc = Document {
        content: vec![Block::Section {
            id: NodeId::new(0),
            role: Some(SectionRole::Article),
            children: vec![
                Block::Heading {
                    id: NodeId::new(1),
                    level: 1,
                    content: vec![],
                },
                Block::Paragraph {
                    id: NodeId::new(2),
                    content: vec![],
                },
                Block::Section {
                    id: NodeId::new(3),
                    role: None,
                    children: vec![
                        Block::Paragraph {
                            id: NodeId::new(4),
                            content: vec![],
                        },
                        Block::Paragraph {
                            id: NodeId::new(5),
                            content: vec![],
                        },
                    ],
                },
            ],
        }],
        presentation: None,
        relationships: None,
        metadata: DocumentMetadata::default(),
        interactions: None,
        assets: AssetStore::new(),
        diagnostics: Vec::new(),
        is_encrypted: false,
        next_node_id: AtomicU64::new(6),
    };

    let mut visited = Vec::new();
    doc.walk(&mut |b| visited.push(b.id()));
    assert_eq!(
        visited,
        vec![
            NodeId::new(0), // outer section
            NodeId::new(1), // heading
            NodeId::new(2), // paragraph
            NodeId::new(3), // inner section
            NodeId::new(4), // paragraph (inner)
            NodeId::new(5), // paragraph (inner)
        ]
    );
}

// ---------------------------------------------------------------------------
// Block::text() -- comprehensive text collection
// ---------------------------------------------------------------------------

#[test]
fn heading_text_levels() {
    for level in 1..=6u8 {
        let block = Block::Heading {
            id: NodeId::new(0),
            level,
            content: vec![Inline::Text {
                id: NodeId::new(1),
                text: format!("Level {}", level),
                style: SpanStyle::default(),
            }],
        };
        assert_eq!(block.text(), format!("Level {}", level));
        assert_eq!(block.id(), NodeId::new(0));
    }
}

#[test]
fn mixed_inline_styles_text() {
    let block = Block::Paragraph {
        id: NodeId::new(0),
        content: vec![
            Inline::Text {
                id: NodeId::new(1),
                text: "plain ".into(),
                style: SpanStyle::default(),
            },
            Inline::Text {
                id: NodeId::new(2),
                text: "bold ".into(),
                style: SpanStyle {
                    bold: true,
                    ..Default::default()
                },
            },
            Inline::Text {
                id: NodeId::new(3),
                text: "italic ".into(),
                style: SpanStyle {
                    italic: true,
                    ..Default::default()
                },
            },
            Inline::Text {
                id: NodeId::new(4),
                text: "both".into(),
                style: SpanStyle {
                    bold: true,
                    italic: true,
                    ..Default::default()
                },
            },
        ],
    };
    assert_eq!(block.text(), "plain bold italic both");
}

#[test]
fn table_2x2_text() {
    let block = Block::Table {
        id: NodeId::new(0),
        table: TableData {
            rows: vec![
                TableRow {
                    id: NodeId::new(1),
                    cells: vec![
                        TableCell {
                            id: NodeId::new(2),
                            content: vec![Block::Paragraph {
                                id: NodeId::new(3),
                                content: vec![Inline::Text {
                                    id: NodeId::new(4),
                                    text: "A".into(),
                                    style: SpanStyle::default(),
                                }],
                            }],
                            col_span: 1,
                            row_span: 1,
                            value: None,
                        },
                        TableCell {
                            id: NodeId::new(5),
                            content: vec![Block::Paragraph {
                                id: NodeId::new(6),
                                content: vec![Inline::Text {
                                    id: NodeId::new(7),
                                    text: "B".into(),
                                    style: SpanStyle::default(),
                                }],
                            }],
                            col_span: 1,
                            row_span: 1,
                            value: None,
                        },
                    ],
                    is_header: true,
                },
                TableRow {
                    id: NodeId::new(8),
                    cells: vec![
                        TableCell {
                            id: NodeId::new(9),
                            content: vec![Block::Paragraph {
                                id: NodeId::new(10),
                                content: vec![Inline::Text {
                                    id: NodeId::new(11),
                                    text: "1".into(),
                                    style: SpanStyle::default(),
                                }],
                            }],
                            col_span: 1,
                            row_span: 1,
                            value: None,
                        },
                        TableCell {
                            id: NodeId::new(12),
                            content: vec![Block::Paragraph {
                                id: NodeId::new(13),
                                content: vec![Inline::Text {
                                    id: NodeId::new(14),
                                    text: "2".into(),
                                    style: SpanStyle::default(),
                                }],
                            }],
                            col_span: 1,
                            row_span: 1,
                            value: None,
                        },
                    ],
                    is_header: false,
                },
            ],
            num_columns: 2,
            header_row_count: 1,
            may_continue_from_previous: false,
            may_continue_to_next: false,
        },
    };
    // Tab-separated columns, newline-separated rows
    assert_eq!(block.text(), "A\tB\n1\t2");
}

#[test]
fn rich_table_cell_text() {
    // TableCell with multiple paragraphs, verify text() joins them.
    let cell = TableCell {
        id: NodeId::new(0),
        content: vec![
            Block::Paragraph {
                id: NodeId::new(1),
                content: vec![Inline::Text {
                    id: NodeId::new(2),
                    text: "First paragraph".into(),
                    style: SpanStyle::default(),
                }],
            },
            Block::Paragraph {
                id: NodeId::new(3),
                content: vec![Inline::Text {
                    id: NodeId::new(4),
                    text: "Second paragraph".into(),
                    style: SpanStyle::default(),
                }],
            },
        ],
        col_span: 1,
        row_span: 1,
        value: None,
    };
    assert_eq!(cell.text(), "First paragraph\nSecond paragraph");
}

#[test]
fn section_text() {
    let block = Block::Section {
        id: NodeId::new(0),
        role: Some(SectionRole::Article),
        children: vec![
            Block::Heading {
                id: NodeId::new(1),
                level: 1,
                content: vec![Inline::Text {
                    id: NodeId::new(2),
                    text: "Title".into(),
                    style: SpanStyle::default(),
                }],
            },
            Block::Paragraph {
                id: NodeId::new(3),
                content: vec![Inline::Text {
                    id: NodeId::new(4),
                    text: "Body".into(),
                    style: SpanStyle::default(),
                }],
            },
        ],
    };
    assert_eq!(block.text(), "Title\nBody");
}

#[test]
fn leaf_block_text_is_empty() {
    assert_eq!(Block::PageBreak { id: NodeId::new(0) }.text(), "");
    assert_eq!(Block::ThematicBreak { id: NodeId::new(0) }.text(), "");
    assert_eq!(
        Block::Image {
            id: NodeId::new(0),
            image_ref: ImageRef::new(0),
            alt_text: Some("photo".into()),
        }
        .text(),
        ""
    );
}

// ---------------------------------------------------------------------------
// Block::children()
// ---------------------------------------------------------------------------

#[test]
fn block_children_section() {
    let section = Block::Section {
        id: NodeId::new(0),
        role: None,
        children: vec![Block::Paragraph {
            id: NodeId::new(1),
            content: vec![],
        }],
    };
    assert_eq!(section.children().len(), 1);
    assert_eq!(section.children()[0].id(), NodeId::new(1));
}

#[test]
fn block_children_leaf() {
    let paragraph = Block::Paragraph {
        id: NodeId::new(0),
        content: vec![],
    };
    assert!(paragraph.children().is_empty());

    let heading = Block::Heading {
        id: NodeId::new(0),
        level: 1,
        content: vec![],
    };
    assert!(heading.children().is_empty());

    let page_break = Block::PageBreak { id: NodeId::new(0) };
    assert!(page_break.children().is_empty());
}

#[test]
fn block_children_shape() {
    let shape = Block::Shape {
        id: NodeId::new(0),
        kind: ShapeKind::Group,
        children: vec![
            Block::Paragraph {
                id: NodeId::new(1),
                content: vec![],
            },
            Block::Paragraph {
                id: NodeId::new(2),
                content: vec![],
            },
        ],
        alt_text: None,
    };
    assert_eq!(shape.children().len(), 2);
}

// ---------------------------------------------------------------------------
// PDF round-trip: build a Document with heading, paragraph, table, image.
// Verify text() and walk().
// ---------------------------------------------------------------------------

#[test]
fn pdf_roundtrip_structure() {
    let doc = build_full_document();

    // text() on the full document via walk
    let mut all_text = String::new();
    doc.walk(&mut |b| {
        let t = b.text();
        if !t.is_empty() {
            if !all_text.is_empty() {
                all_text.push('\n');
            }
            all_text.push_str(&t);
        }
    });
    assert!(all_text.contains("Introduction"));
    assert!(all_text.contains("Body text"));
    assert!(all_text.contains("Header"));

    // walk visits all blocks
    let mut block_count = 0;
    doc.walk(&mut |_| block_count += 1);
    assert!(
        block_count >= 4,
        "expected at least 4 blocks, got {block_count}"
    );
}

fn build_full_document() -> Document {
    let doc = Document::new();

    let heading = Block::Heading {
        id: doc.alloc_node_id(),
        level: 1,
        content: vec![Inline::Text {
            id: doc.alloc_node_id(),
            text: "Introduction".into(),
            style: SpanStyle::default().with_bold(),
        }],
    };

    let paragraph = Block::Paragraph {
        id: doc.alloc_node_id(),
        content: vec![Inline::Text {
            id: doc.alloc_node_id(),
            text: "Body text".into(),
            style: SpanStyle::default(),
        }],
    };

    let table = Block::Table {
        id: doc.alloc_node_id(),
        table: TableData::new(vec![
            TableRow::new(
                doc.alloc_node_id(),
                vec![TableCell::new(
                    doc.alloc_node_id(),
                    vec![Block::Paragraph {
                        id: doc.alloc_node_id(),
                        content: vec![Inline::Text {
                            id: doc.alloc_node_id(),
                            text: "Header".into(),
                            style: SpanStyle::default(),
                        }],
                    }],
                )],
            )
            .with_header(),
            TableRow::new(
                doc.alloc_node_id(),
                vec![TableCell::new(
                    doc.alloc_node_id(),
                    vec![Block::Paragraph {
                        id: doc.alloc_node_id(),
                        content: vec![Inline::Text {
                            id: doc.alloc_node_id(),
                            text: "Data".into(),
                            style: SpanStyle::default(),
                        }],
                    }],
                )],
            ),
        ]),
    };

    let image = Block::Image {
        id: doc.alloc_node_id(),
        image_ref: ImageRef::new(0),
        alt_text: Some("Figure 1".into()),
    };

    // Reuse the same Document so next_node_id is correct.
    let mut doc = doc;
    doc.content = vec![heading, paragraph, table, image];
    doc.assets.add_image(ImageAsset::new(
        vec![0xFF, 0xD8, 0xFF],
        crate::image::ImageFilter::Jpeg,
        100,
        100,
        8,
    ));
    doc.metadata.page_count = 1;
    doc.metadata.title = Some("Test Document".into());
    doc
}

// ---------------------------------------------------------------------------
// Nested list text
// ---------------------------------------------------------------------------

#[test]
fn nested_list_text() {
    let block = Block::List {
        id: NodeId::new(0),
        items: vec![
            ListItem {
                id: NodeId::new(1),
                content: vec![
                    Block::Paragraph {
                        id: NodeId::new(2),
                        content: vec![Inline::Text {
                            id: NodeId::new(3),
                            text: "item one".into(),
                            style: SpanStyle::default(),
                        }],
                    },
                    Block::Paragraph {
                        id: NodeId::new(4),
                        content: vec![Inline::Text {
                            id: NodeId::new(5),
                            text: "continued".into(),
                            style: SpanStyle::default(),
                        }],
                    },
                ],
            },
            ListItem {
                id: NodeId::new(6),
                content: vec![Block::Paragraph {
                    id: NodeId::new(7),
                    content: vec![Inline::Text {
                        id: NodeId::new(8),
                        text: "item two".into(),
                        style: SpanStyle::default(),
                    }],
                }],
            },
        ],
        kind: ListKind::Ordered,
        start: 1,
    };
    assert_eq!(block.text(), "item one\ncontinued\nitem two");
}

// ---------------------------------------------------------------------------
// CellValue
// ---------------------------------------------------------------------------

#[test]
fn cell_value_equality() {
    assert_eq!(CellValue::Number(42.0), CellValue::Number(42.0));
    assert_ne!(CellValue::Number(1.0), CellValue::Number(2.0));
    assert_eq!(CellValue::Boolean(true), CellValue::Boolean(true));
    assert_eq!(CellValue::Text("x".into()), CellValue::Text("x".into()));
    assert_eq!(
        CellValue::Date("2026-01-01".into()),
        CellValue::Date("2026-01-01".into())
    );
    assert_eq!(
        CellValue::Error("#REF!".into()),
        CellValue::Error("#REF!".into())
    );
}

#[test]
fn cell_value_formula() {
    let f = CellValue::Formula {
        expression: "=SUM(A1:A10)".into(),
        result: Some(Box::new(CellValue::Number(100.0))),
    };
    match &f {
        CellValue::Formula { expression, result } => {
            assert_eq!(expression, "=SUM(A1:A10)");
            assert_eq!(result.as_deref(), Some(&CellValue::Number(100.0)));
        }
        _ => panic!("expected Formula"),
    }
}

// ---------------------------------------------------------------------------
// SpanStyle
// ---------------------------------------------------------------------------

#[test]
fn span_style_builders() {
    let s = SpanStyle::default().with_bold().with_italic();
    assert!(s.bold);
    assert!(s.italic);
    assert!(!s.underline);
    assert!(!s.is_plain());
}

#[test]
fn span_style_all_flags_checked() {
    let fields = [
        SpanStyle {
            bold: true,
            ..Default::default()
        },
        SpanStyle {
            italic: true,
            ..Default::default()
        },
        SpanStyle {
            underline: true,
            ..Default::default()
        },
        SpanStyle {
            strikethrough: true,
            ..Default::default()
        },
        SpanStyle {
            superscript: true,
            ..Default::default()
        },
        SpanStyle {
            subscript: true,
            ..Default::default()
        },
    ];
    for s in &fields {
        assert!(!s.is_plain());
    }
}

// ---------------------------------------------------------------------------
// TableData constructors
// ---------------------------------------------------------------------------

#[test]
fn table_data_new_computes_columns_and_headers() {
    let rows = vec![
        TableRow::new(
            NodeId::new(0),
            vec![
                TableCell::new(NodeId::new(1), vec![]),
                TableCell::new(NodeId::new(2), vec![]),
                TableCell::new(NodeId::new(3), vec![]),
            ],
        )
        .with_header(),
        TableRow::new(
            NodeId::new(4),
            vec![
                TableCell::new(NodeId::new(5), vec![]),
                TableCell::new(NodeId::new(6), vec![]),
                TableCell::new(NodeId::new(7), vec![]),
            ],
        ),
    ];
    let td = TableData::new(rows);
    assert_eq!(td.num_columns, 3);
    assert_eq!(td.header_row_count, 1);
    assert!(!td.may_continue_from_previous);
    assert!(!td.may_continue_to_next);
}

// ---------------------------------------------------------------------------
// Inline accessors
// ---------------------------------------------------------------------------

#[test]
fn inline_text_accessor_variants() {
    let id = NodeId::new(0);
    assert_eq!(
        Inline::Text {
            id,
            text: "hello".into(),
            style: SpanStyle::default(),
        }
        .text(),
        "hello"
    );
    assert_eq!(
        Inline::Code {
            id,
            text: "fn main()".into(),
        }
        .text(),
        "fn main()"
    );
    assert_eq!(
        Inline::Link {
            id,
            url: "https://example.com".into(),
            content: vec![],
        }
        .text(),
        ""
    );
    assert_eq!(Inline::SoftBreak { id }.text(), "");
    assert_eq!(Inline::LineBreak { id }.text(), "");
}

#[test]
fn try_alloc_at_boundary() {
    let doc = Document::new();
    doc.set_next_node_id_for_test(super::MAX_NODE_ID - 1);

    // One allocation should succeed.
    let result = doc.try_alloc_node_id();
    assert!(result.is_some(), "should succeed at MAX_NODE_ID - 1");
    assert_eq!(result.unwrap().value(), super::MAX_NODE_ID - 1);

    // Next should fail (at the limit).
    let result = doc.try_alloc_node_id();
    assert!(result.is_none(), "should fail at MAX_NODE_ID");

    // Repeated failure shouldn't change the counter.
    let result = doc.try_alloc_node_id();
    assert!(result.is_none(), "should still fail");
}

#[cfg(feature = "serde")]
#[test]
fn table_cell_flattened_synthetic_ids_safe_after_roundtrip() {
    use super::table::{TableCell, TableData, TableRow};

    let doc = Document::new();
    let cell_id = doc.alloc_node_id();
    let row_id = doc.alloc_node_id();
    let table_id = doc.alloc_node_id();

    let cell = TableCell {
        id: cell_id,
        content: vec![Block::Paragraph {
            id: NodeId::new(cell_id.value() + 1),
            content: vec![Inline::Text {
                id: NodeId::new(cell_id.value() + 2),
                text: "hello".into(),
                style: SpanStyle::default(),
            }],
        }],
        col_span: 1,
        row_span: 1,
        value: None,
    };
    let row = TableRow {
        id: row_id,
        cells: vec![cell],
        is_header: false,
    };
    let table_data = TableData::new(vec![row]);
    let table = Block::Table {
        id: table_id,
        table: table_data,
    };

    // Round-trip a full Document to trigger scan_blocks in deserialization.
    let mut doc_full = Document::new();
    doc_full.content.push(table);
    let doc_json = serde_json::to_string(&doc_full).expect("serialize doc");
    let doc2: Document = serde_json::from_str(&doc_json).expect("deserialize doc");

    // New allocations should not collide with any existing id.
    let new_id = doc2.alloc_node_id();
    assert!(
        new_id.value() > cell_id.value() + 2,
        "new id {} should be past synthetic ids (cell_id + 2 = {})",
        new_id.value(),
        cell_id.value() + 2
    );
}

#[cfg(feature = "serde")]
#[test]
fn deeply_nested_block_rejected_by_serde() {
    // serde_json has a 128-level recursion limit. Build a JSON string
    // with deeply nested sections to verify it triggers.
    let depth = 200;
    let mut json = String::new();
    for _ in 0..depth {
        json.push_str(r#"{"type":"section","id":0,"role":null,"children":["#);
    }
    json.push_str(r#"{"type":"page_break","id":0}"#);
    for _ in 0..depth {
        json.push_str("]}");
    }
    let result = serde_json::from_str::<Block>(&json);
    assert!(
        result.is_err(),
        "deeply nested JSON should be rejected by serde_json's recursion limit"
    );
}

#[cfg(feature = "serde")]
#[test]
fn synthetic_id_stays_in_range() {
    // Verify that the synthetic ID counter saturates at SYNTHETIC_ID_BASE
    // rather than underflowing into the real ID space.
    use super::table::SYNTHETIC_ID_BASE;

    // Deserialize a flattened cell to consume a couple synthetic IDs.
    let cell_json = r#"{"id": 5, "text": "hello"}"#;
    let cell: super::table::TableCell = serde_json::from_str(cell_json).expect("deserialize cell");
    // The wrapper paragraph + inline get synthetic IDs.
    assert_eq!(cell.content.len(), 1);
    let para_id = cell.content[0].id().value();
    assert!(
        para_id >= SYNTHETIC_ID_BASE,
        "synthetic ID {para_id} should be >= SYNTHETIC_ID_BASE ({SYNTHETIC_ID_BASE})"
    );
}

// ===========================================================================
// -- Document::*_for(node_id) accessors
// ===========================================================================

#[test]
fn presentation_for_returns_none_when_overlay_absent() {
    let doc = Document::new();
    let n = doc.alloc_node_id();
    assert!(doc.presentation.is_none());
    assert!(doc.presentation_for(n).is_none());
}

#[test]
fn presentation_for_returns_none_when_node_not_present() {
    use super::overlay::Overlay;
    use crate::geometry::BoundingBox;

    let mut doc = Document::new();
    let other = doc.alloc_node_id();
    let target = doc.alloc_node_id();
    let mut p = super::presentation::Presentation::default();
    let mut geom: Overlay<BoundingBox> = Overlay::new();
    geom.set(other, BoundingBox::new(0.0, 0.0, 100.0, 100.0));
    p.geometry = geom;
    doc.presentation = Some(p);
    // `target` is allocated but has no overlay data; result is None.
    assert!(doc.presentation_for(target).is_none());
    // `other` does have data; result is Some.
    assert!(doc.presentation_for(other).is_some());
}

#[test]
fn presentation_for_some_when_node_in_geometry() {
    use super::overlay::Overlay;
    use crate::geometry::BoundingBox;

    let mut doc = Document::new();
    let n = doc.alloc_node_id();
    let mut p = super::presentation::Presentation::default();
    let mut geom: Overlay<BoundingBox> = Overlay::new();
    geom.set(n, BoundingBox::new(10.0, 20.0, 100.0, 200.0));
    p.geometry = geom;
    doc.presentation = Some(p);
    let pres = doc.presentation_for(n).expect("Some(presentation)");
    assert_eq!(
        pres.geometry.get(n),
        Some(&BoundingBox::new(10.0, 20.0, 100.0, 200.0))
    );
}

#[test]
fn presentation_for_mut_returns_mut_borrow() {
    use super::overlay::Overlay;
    use crate::geometry::BoundingBox;

    let mut doc = Document::new();
    let n = doc.alloc_node_id();
    let mut p = super::presentation::Presentation::default();
    let mut geom: Overlay<BoundingBox> = Overlay::new();
    geom.set(n, BoundingBox::new(0.0, 0.0, 50.0, 50.0));
    p.geometry = geom;
    doc.presentation = Some(p);

    let pres = doc.presentation_for_mut(n).expect("Some");
    pres.geometry.set(n, BoundingBox::new(0.0, 0.0, 75.0, 75.0));
    assert_eq!(
        doc.presentation_for(n).unwrap().geometry.get(n),
        Some(&BoundingBox::new(0.0, 0.0, 75.0, 75.0))
    );
}

#[test]
fn relationships_for_none_when_overlay_absent() {
    let doc = Document::new();
    let n = doc.alloc_node_id();
    assert!(doc.relationships_for(n).is_none());
}

#[test]
fn relationships_for_none_when_node_not_anchored() {
    let mut doc = Document::new();
    let n = doc.alloc_node_id();
    doc.relationships = Some(super::relationships::Relationships::default());
    assert!(doc.relationships_for(n).is_none());
}

#[test]
fn relationships_for_some_when_node_in_captions() {
    let mut doc = Document::new();
    let img = doc.alloc_node_id();
    let cap = doc.alloc_node_id();
    let mut r = super::relationships::Relationships::default();
    r.set_caption(img, cap);
    doc.relationships = Some(r);
    let r = doc.relationships_for(img).expect("Some");
    assert_eq!(r.captions().get(img), Some(&cap));
}

#[test]
fn interactions_for_none_when_overlay_absent() {
    let doc = Document::new();
    let n = doc.alloc_node_id();
    assert!(doc.interactions_for(n).is_none());
}

#[test]
fn interactions_for_some_when_form_field_anchored() {
    let mut doc = Document::new();
    let n = doc.alloc_node_id();
    let i = super::interactions::Interactions {
        form_fields: vec![super::interactions::FormField {
            anchor: Some(n),
            name: "input1".into(),
            field_type: super::interactions::FormFieldType::Text,
            value: None,
            bbox: None,
            page_index: None,
        }],
        comments: vec![],
        tracked_changes: vec![],
    };
    doc.interactions = Some(i);
    assert!(doc.interactions_for(n).is_some());
    let other = doc.alloc_node_id();
    assert!(doc.interactions_for(other).is_none());
}

#[test]
fn interactions_for_mut_allows_mutation() {
    let mut doc = Document::new();
    let n = doc.alloc_node_id();
    let i = super::interactions::Interactions {
        form_fields: vec![super::interactions::FormField {
            anchor: Some(n),
            name: "input1".into(),
            field_type: super::interactions::FormFieldType::Text,
            value: None,
            bbox: None,
            page_index: None,
        }],
        comments: vec![],
        tracked_changes: vec![],
    };
    doc.interactions = Some(i);
    let inter = doc.interactions_for_mut(n).expect("mut borrow");
    inter.form_fields[0].value = Some("filled".into());
    assert_eq!(
        doc.interactions_for(n).unwrap().form_fields[0]
            .value
            .as_deref(),
        Some("filled")
    );
}

#[test]
fn presentation_for_node_in_text_styling() {
    use super::overlay::SparseOverlay;
    use super::presentation::ExtendedTextStyle;

    let mut doc = Document::new();
    let n = doc.alloc_node_id();
    let mut p = super::presentation::Presentation::default();
    let mut ts: SparseOverlay<ExtendedTextStyle> = SparseOverlay::new();
    ts.set(n, ExtendedTextStyle::default());
    p.text_styling = ts;
    doc.presentation = Some(p);
    assert!(doc.presentation_for(n).is_some());
}

#[test]
fn presentation_for_node_in_page_assignments() {
    use super::overlay::Overlay;

    let mut doc = Document::new();
    let n = doc.alloc_node_id();
    let mut p = super::presentation::Presentation::default();
    let mut pa: Overlay<usize> = Overlay::new();
    pa.set(n, 0);
    p.page_assignments = pa;
    doc.presentation = Some(p);
    assert!(doc.presentation_for(n).is_some());
}

#[test]
fn relationships_for_mut_allows_mutation() {
    let mut doc = Document::new();
    let img = doc.alloc_node_id();
    let cap = doc.alloc_node_id();
    let mut r = super::relationships::Relationships::default();
    r.set_caption(img, cap);
    doc.relationships = Some(r);
    let r = doc.relationships_for_mut(img).expect("mut");
    let _ = r.add_hyperlink("https://example.com".into());
    assert_eq!(
        doc.relationships.as_ref().unwrap().hyperlinks(),
        &["https://example.com".to_string()]
    );
}

#[test]
fn interactions_for_via_comment_anchor() {
    let mut doc = Document::new();
    let n = doc.alloc_node_id();
    let i = super::interactions::Interactions {
        form_fields: vec![],
        comments: vec![super::interactions::Comment {
            anchor: n,
            author: Some("alice".into()),
            date: None,
            text: "review this".into(),
            replies: vec![],
            bbox: None,
            page_index: None,
        }],
        tracked_changes: vec![],
    };
    doc.interactions = Some(i);
    assert!(doc.interactions_for(n).is_some());
}

#[test]
fn node_id_display_round_trip_format() {
    // Display contract: NodeId always renders as `node:{id}`. This is
    // load-bearing for the markdown emitter's citation anchors and for
    // log greppability.
    for v in [0u64, 1, 99, 1234, 999_999, 16_000_000 - 1] {
        assert_eq!(format!("{}", NodeId::new(v)), format!("node:{v}"));
    }
}

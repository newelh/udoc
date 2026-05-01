//! Tests against real-world-style Markdown files.
//!
//! These files are hand-crafted to mimic markdown styles found in GitHub
//! READMEs, blog posts, API docs, changelogs, and tutorials.

use udoc_core::backend::{FormatBackend, PageExtractor};

fn real_world_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/real-world")
}

/// Open a real-world Markdown file and extract text from page 0.
fn extract(name: &str) -> String {
    let path = real_world_dir().join(name);
    let mut doc = udoc_markdown::MdDocument::open(&path)
        .unwrap_or_else(|e| panic!("failed to open {name}: {e}"));
    assert_eq!(doc.page_count(), 1);
    let mut page = doc.page(0).expect("page 0");
    page.text().expect("text()")
}

/// Open a real-world Markdown file and return the document.
fn open(name: &str) -> udoc_markdown::MdDocument {
    let path = real_world_dir().join(name);
    udoc_markdown::MdDocument::open(&path).unwrap_or_else(|e| panic!("failed to open {name}: {e}"))
}

// ---- GitHub README style ----------------------------------------------------

#[test]
fn github_readme_headings() {
    let text = extract("github_readme.md");
    assert!(text.contains("awesome-project"), "got: {text}");
    assert!(text.contains("Installation"), "got: {text}");
    assert!(text.contains("Usage"), "got: {text}");
    assert!(text.contains("Contributing"), "got: {text}");
    assert!(text.contains("License"), "got: {text}");
}

#[test]
fn github_readme_code_blocks() {
    let text = extract("github_readme.md");
    assert!(text.contains("pip install"), "got: {text}");
    assert!(text.contains("from awesome"), "got: {text}");
}

#[test]
fn github_readme_tables() {
    let mut doc = open("github_readme.md");
    let mut page = doc.page(0).expect("page 0");
    let tables = page.tables().expect("tables()");
    assert!(!tables.is_empty(), "should have at least one table");
    let all_cell_text: String = tables
        .iter()
        .flat_map(|t| &t.rows)
        .flat_map(|r| &r.cells)
        .map(|c| c.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(all_cell_text.contains("Linux"), "got: {all_cell_text}");
}

// ---- Blog post style --------------------------------------------------------

#[test]
fn blog_post_content() {
    let text = extract("blog_post.md");
    assert!(text.contains("Rust Lifetimes"), "got: {text}");
    assert!(text.contains("borrow checker"), "got: {text}");
    assert!(text.contains("fn longest"), "got: {text}");
}

#[test]
fn blog_post_blockquote() {
    let text = extract("blog_post.md");
    assert!(
        text.contains("borrow checker is your friend"),
        "got: {text}"
    );
}

// ---- API documentation style ------------------------------------------------

#[test]
fn api_docs_structure() {
    let text = extract("api_docs.md");
    assert!(text.contains("API Reference"), "got: {text}");
    assert!(text.contains("Authentication"), "got: {text}");
    assert!(text.contains("GET /users"), "got: {text}");
    assert!(text.contains("POST /users"), "got: {text}");
}

#[test]
fn api_docs_tables() {
    let mut doc = open("api_docs.md");
    let mut page = doc.page(0).expect("page 0");
    let tables = page.tables().expect("tables()");
    assert!(
        tables.len() >= 2,
        "API docs should have at least 2 tables, got {}",
        tables.len()
    );
}

#[test]
fn api_docs_code_blocks() {
    let text = extract("api_docs.md");
    assert!(text.contains("Authorization: Bearer"), "got: {text}");
    assert!(text.contains("\"users\""), "got: {text}");
}

// ---- Changelog style --------------------------------------------------------

#[test]
fn changelog_versions() {
    let text = extract("changelog.md");
    assert!(text.contains("2.0.0"), "got: {text}");
    assert!(text.contains("1.5.0"), "got: {text}");
    assert!(text.contains("Breaking Changes"), "got: {text}");
}

#[test]
fn changelog_list_items() {
    let text = extract("changelog.md");
    assert!(text.contains("batch_process()"), "got: {text}");
    assert!(text.contains("Memory leak"), "got: {text}");
}

// ---- Tutorial style ---------------------------------------------------------

#[test]
fn tutorial_steps() {
    let text = extract("tutorial.md");
    assert!(text.contains("Getting Started"), "got: {text}");
    assert!(text.contains("Step 1"), "got: {text}");
    assert!(text.contains("Step 2"), "got: {text}");
    assert!(text.contains("Step 3"), "got: {text}");
}

#[test]
fn tutorial_code_blocks() {
    let text = extract("tutorial.md");
    assert!(text.contains("docker run"), "got: {text}");
    assert!(text.contains("Dockerfile"), "got: {text}");
}

#[test]
fn tutorial_blockquote() {
    let text = extract("tutorial.md");
    assert!(text.contains("Hello from Docker"), "got: {text}");
}

// ---- Rust README style (badges, feature flags, multiple tables) -------------

#[test]
fn rust_readme_headings() {
    let text = extract("rust_readme.md");
    assert!(text.contains("tokio-serde"), "got: {text}");
    assert!(text.contains("Overview"), "got: {text}");
    assert!(text.contains("Installation"), "got: {text}");
    assert!(text.contains("Quick Start"), "got: {text}");
    assert!(text.contains("Feature Flags"), "got: {text}");
}

#[test]
fn rust_readme_badge_images() {
    let text = extract("rust_readme.md");
    // Badge alt text should be extracted
    assert!(text.contains("Crates.io"), "got: {text}");
    assert!(text.contains("Documentation"), "got: {text}");
}

#[test]
fn rust_readme_code_blocks() {
    let text = extract("rust_readme.md");
    assert!(text.contains("tokio-serde = \"0.9\""), "got: {text}");
    assert!(text.contains("Framed::new"), "got: {text}");
}

#[test]
fn rust_readme_multiple_tables() {
    let mut doc = open("rust_readme.md");
    let mut page = doc.page(0).expect("page 0");
    let tables = page.tables().expect("tables()");
    assert!(
        tables.len() >= 2,
        "rust_readme should have at least 2 tables, got {}",
        tables.len()
    );
    // Check supported formats table
    let all_cell_text: String = tables
        .iter()
        .flat_map(|t| &t.rows)
        .flat_map(|r| &r.cells)
        .map(|c| c.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(all_cell_text.contains("JSON"), "got: {all_cell_text}");
    assert!(all_cell_text.contains("Bincode"), "got: {all_cell_text}");
}

#[test]
fn rust_readme_feature_list() {
    let text = extract("rust_readme.md");
    assert!(text.contains("json"), "got: {text}");
    assert!(text.contains("bincode"), "got: {text}");
    assert!(text.contains("messagepack"), "got: {text}");
}

// ---- Release notes style (thematic breaks, image refs, numbered lists) ------

#[test]
fn release_notes_versions() {
    let text = extract("release_notes.md");
    assert!(text.contains("v3.2.0"), "got: {text}");
    assert!(text.contains("v3.1.0"), "got: {text}");
    assert!(text.contains("v3.0.0"), "got: {text}");
}

#[test]
fn release_notes_content() {
    let text = extract("release_notes.md");
    assert!(text.contains("streaming support"), "got: {text}");
    assert!(text.contains("StreamReader"), "got: {text}");
    assert!(text.contains("legacy_parse()"), "got: {text}");
}

#[test]
fn release_notes_blockquote() {
    let text = extract("release_notes.md");
    assert!(text.contains("new Parser API"), "got: {text}");
}

#[test]
fn release_notes_tables() {
    let mut doc = open("release_notes.md");
    let mut page = doc.page(0).expect("page 0");
    let tables = page.tables().expect("tables()");
    assert!(
        !tables.is_empty(),
        "release notes should have at least one dependency table"
    );
}

#[test]
fn release_notes_code_block() {
    let text = extract("release_notes.md");
    assert!(text.contains("Parser::new"), "got: {text}");
}

#[test]
fn release_notes_image_alt() {
    let text = extract("release_notes.md");
    assert!(text.contains("Benchmark Chart"), "got: {text}");
}

// ---- Contributing guide style (numbered steps, blockquotes, code) -----------

#[test]
fn contributing_headings() {
    let text = extract("contributing.md");
    assert!(text.contains("Contributing to ripgrep"), "got: {text}");
    assert!(text.contains("Reporting Bugs"), "got: {text}");
    assert!(text.contains("Pull Requests"), "got: {text}");
    assert!(text.contains("Style Guide"), "got: {text}");
}

#[test]
fn contributing_code_blocks() {
    let text = extract("contributing.md");
    assert!(text.contains("cargo test --all"), "got: {text}");
    assert!(text.contains("cargo fmt --check"), "got: {text}");
}

#[test]
fn contributing_blockquotes() {
    let text = extract("contributing.md");
    assert!(text.contains("rg --debug"), "got: {text}");
}

#[test]
fn contributing_lists() {
    let text = extract("contributing.md");
    assert!(text.contains("operating system"), "got: {text}");
    assert!(text.contains("Steps to reproduce"), "got: {text}");
}

// ---- Architecture doc style (nested lists, multiple code langs, tables) -----

#[test]
fn architecture_headings() {
    let text = extract("architecture.md");
    assert!(text.contains("Architecture Overview"), "got: {text}");
    assert!(text.contains("Crate Structure"), "got: {text}");
    assert!(text.contains("Query Execution"), "got: {text}");
    assert!(text.contains("Type System"), "got: {text}");
}

#[test]
fn architecture_tables() {
    let mut doc = open("architecture.md");
    let mut page = doc.page(0).expect("page 0");
    let tables = page.tables().expect("tables()");
    assert!(
        tables.len() >= 2,
        "architecture doc should have at least 2 tables, got {}",
        tables.len()
    );
    let all_cell_text: String = tables
        .iter()
        .flat_map(|t| &t.rows)
        .flat_map(|r| &r.cells)
        .map(|c| c.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        all_cell_text.contains("max_connections"),
        "got: {all_cell_text}"
    );
    assert!(all_cell_text.contains("INTEGER"), "got: {all_cell_text}");
}

#[test]
fn architecture_code_blocks() {
    let text = extract("architecture.md");
    assert!(text.contains("query_as!"), "got: {text}");
    assert!(text.contains("fetch_all"), "got: {text}");
    assert!(text.contains("DatabaseError"), "got: {text}");
}

#[test]
fn architecture_nested_list() {
    let text = extract("architecture.md");
    assert!(text.contains("Connection trait"), "got: {text}");
    assert!(text.contains("Executor trait"), "got: {text}");
}

#[test]
fn architecture_blockquote() {
    let text = extract("architecture.md");
    assert!(text.contains("DatabaseError"), "got: {text}");
}

// ---- Security policy style (warnings, inline code, emphasis) ----------------

#[test]
fn security_policy_headings() {
    let text = extract("security_policy.md");
    assert!(text.contains("Security Policy"), "got: {text}");
    assert!(text.contains("Reporting a Vulnerability"), "got: {text}");
    assert!(text.contains("Disclosure Policy"), "got: {text}");
}

#[test]
fn security_policy_tables() {
    let mut doc = open("security_policy.md");
    let mut page = doc.page(0).expect("page 0");
    let tables = page.tables().expect("tables()");
    assert!(
        !tables.is_empty(),
        "security policy should have a versions table"
    );
    let all_cell_text: String = tables
        .iter()
        .flat_map(|t| &t.rows)
        .flat_map(|r| &r.cells)
        .map(|c| c.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(all_cell_text.contains("4.x"), "got: {all_cell_text}");
}

#[test]
fn security_policy_blockquotes() {
    let text = extract("security_policy.md");
    assert!(text.contains("Do not open a public issue"), "got: {text}");
}

#[test]
fn security_policy_code_blocks() {
    let text = extract("security_policy.md");
    assert!(text.contains("tls:"), "got: {text}");
    assert!(text.contains("parameterized query"), "got: {text}");
}

#[test]
fn security_policy_inline_code_and_emphasis() {
    let text = extract("security_policy.md");
    assert!(text.contains("AuthenticationFailed"), "got: {text}");
    assert!(text.contains("RateLimitExceeded"), "got: {text}");
    assert!(text.contains("TLS"), "got: {text}");
}

// ---- Cross-file structural checks -------------------------------------------

#[test]
fn all_files_produce_nonempty_headings() {
    let dir = real_world_dir();
    for name in &[
        "github_readme.md",
        "blog_post.md",
        "api_docs.md",
        "changelog.md",
        "tutorial.md",
        "rust_readme.md",
        "release_notes.md",
        "contributing.md",
        "architecture.md",
        "security_policy.md",
    ] {
        let path = dir.join(name);
        let mut doc = udoc_markdown::MdDocument::open(&path)
            .unwrap_or_else(|e| panic!("failed to open {name}: {e}"));
        let mut page = doc.page(0).expect("page 0");
        let lines = page.text_lines().expect("text_lines()");
        assert!(!lines.is_empty(), "{name} should have text lines");
    }
}

// ---- No panics across all files ---------------------------------------------

#[test]
fn all_real_world_files_no_panic() {
    let dir = real_world_dir();
    if !dir.exists() {
        return;
    }
    for entry in std::fs::read_dir(&dir).expect("read dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let mut doc = udoc_markdown::MdDocument::open(&path)
            .unwrap_or_else(|e| panic!("failed to open {name}: {e}"));
        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert!(!text.is_empty(), "{name} should produce non-empty text");
    }
}

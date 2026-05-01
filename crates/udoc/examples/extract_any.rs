//! Extract text from any supported document format.
//!
//! udoc auto-detects the format from magic bytes and file extension.
//! Works with PDF, DOCX, XLSX, PPTX, RTF, ODT, ODS, ODP, DOC, XLS, PPT, Markdown.
//!
//! Usage: cargo run -p udoc --example extract_any -- path/to/file.docx

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("Usage: extract_any <path>");
        eprintln!("  Supported: pdf, docx, xlsx, pptx, rtf, odt, ods, odp, doc, xls, ppt, md");
        std::process::exit(1);
    });

    // One-shot extraction: auto-detects format, returns a unified Document.
    let doc = udoc::extract(&path)?;

    // Metadata
    println!("Pages:   {}", doc.metadata.page_count);
    if let Some(title) = &doc.metadata.title {
        println!("Title:   {title}");
    }
    if let Some(author) = &doc.metadata.author {
        println!("Author:  {author}");
    }
    println!();

    // Content blocks: paragraphs, headings, tables, images, etc.
    for block in &doc.content {
        match block {
            udoc::Block::Heading { level, .. } => {
                println!("[h{}] {}", level, block.text());
            }
            udoc::Block::Table { .. } => {
                println!("[table] {}", block.text());
            }
            udoc::Block::PageBreak { .. } => {
                println!("--- page break ---");
            }
            _ => {
                let text = block.text();
                if !text.is_empty() {
                    println!("{text}");
                }
            }
        }
    }

    Ok(())
}

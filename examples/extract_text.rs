//! Extract plain text from a PDF file.
//!
//! Usage: cargo run --example extract_text -- path/to/file.pdf

use udoc_pdf::Document;

fn main() -> udoc_pdf::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("Usage: extract_text <path.pdf>");
        std::process::exit(1);
    });

    let mut doc = Document::open(&path)?;
    println!("Pages: {}", doc.page_count());

    for i in 0..doc.page_count() {
        let mut page = doc.page(i)?;
        let text = page.text()?;
        if !text.is_empty() {
            println!("--- Page {} ---", i + 1);
            println!("{text}");
        }
    }

    Ok(())
}

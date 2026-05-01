//! Extract structured text lines with position metadata.
//!
//! Usage: cargo run --example extract_lines -- path/to/file.pdf

use udoc_pdf::Document;

fn main() -> udoc_pdf::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("Usage: extract_lines <path.pdf>");
        std::process::exit(1);
    });

    let mut doc = Document::open(&path)?;

    for i in 0..doc.page_count() {
        let mut page = doc.page(i)?;
        let lines = page.text_lines()?;
        if lines.is_empty() {
            continue;
        }

        println!("--- Page {} ({} lines) ---", i + 1, lines.len());
        for line in &lines {
            let mode = if line.is_vertical { "V" } else { "H" };
            println!(
                "  [{mode} y={baseline:.0}] {text}",
                baseline = line.baseline,
                text = line.text()
            );
        }
    }

    Ok(())
}

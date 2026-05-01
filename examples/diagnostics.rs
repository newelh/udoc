//! Extract text with diagnostic warnings collected and displayed.
//!
//! Usage: cargo run --example diagnostics -- path/to/file.pdf

use std::sync::Arc;
use udoc_pdf::{CollectingDiagnostics, Config, Document, WarningLevel};

fn main() -> udoc_pdf::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("Usage: diagnostics <path.pdf>");
        std::process::exit(1);
    });

    let diag = Arc::new(CollectingDiagnostics::new());
    let config = Config::default().with_diagnostics(diag.clone());
    let mut doc = Document::open_with_config(&path, config)?;

    for i in 0..doc.page_count() {
        let mut page = doc.page(i)?;
        let text = page.text()?;
        if !text.is_empty() {
            println!("--- Page {} ---", i + 1);
            println!("{text}");
        }
    }

    let warnings = diag.warnings();
    if warnings.is_empty() {
        println!("\nNo diagnostics emitted.");
    } else {
        println!("\n--- Diagnostics ({} total) ---", warnings.len());
        for w in &warnings {
            let level = match w.level {
                WarningLevel::Info => "INFO",
                WarningLevel::Warning => "WARN",
                _ => "????", // forward-compatible: WarningLevel is #[non_exhaustive]
            };
            let page = w
                .context
                .page_index
                .map(|p| format!(" [page {}]", p + 1))
                .unwrap_or_default();
            let offset = w.offset.map(|o| format!(" @{o}")).unwrap_or_default();
            println!("  {level}{page}{offset}: {:?} - {}", w.kind, w.message);
        }
    }

    Ok(())
}

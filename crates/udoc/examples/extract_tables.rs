//! Extract tables from any document as TSV.
//!
//! Works with spreadsheets (XLSX, XLS, ODS), presentations (PPTX, PPT, ODP),
//! word processors (DOCX, DOC, ODT, RTF), and PDF.
//!
//! Usage: cargo run -p udoc --example extract_tables -- path/to/spreadsheet.xlsx

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("Usage: extract_tables <path>");
        std::process::exit(1);
    });

    let doc = udoc::extract(&path)?;

    let mut table_num = 0;
    for block in &doc.content {
        if let udoc::Block::Table { table, .. } = block {
            table_num += 1;
            println!("=== Table {table_num} ({} rows) ===", table.rows.len());
            for row in &table.rows {
                let cells: Vec<String> = row
                    .cells
                    .iter()
                    .map(|cell| cell.text().replace('\t', " "))
                    .collect();
                println!("{}", cells.join("\t"));
            }
            println!();
        }
    }

    if table_num == 0 {
        println!("No tables found in {path}");
    } else {
        println!("{table_num} table(s) extracted.");
    }

    Ok(())
}

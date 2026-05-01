//! File I/O helpers shared across format backends.
//!
//! Provides common operations like size-checked file reading that all
//! backends need but should not duplicate.

use std::path::Path;

use crate::error::{Error, Result};

/// Read a file into memory with a size limit check.
///
/// Checks the file's metadata size before reading. Returns an error if
/// the file exceeds `max_size` bytes or cannot be read.
///
/// `format_name` is included in error messages (e.g., "DOCX", "RTF").
pub fn read_file_checked(path: &Path, max_size: u64, format_name: &str) -> Result<Vec<u8>> {
    let meta = std::fs::metadata(path)
        .map_err(|e| Error::with_source(format!("reading {}", path.display()), e))?;
    if meta.len() > max_size {
        return Err(Error::new(format!(
            "{format_name} file too large ({} bytes, limit is {max_size} bytes)",
            meta.len(),
        )));
    }
    std::fs::read(path).map_err(|e| Error::with_source(format!("reading {}", path.display()), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_existing_file() {
        // Use Cargo.toml as a known-to-exist file in the crate root.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let data = read_file_checked(&path, 10 * 1024 * 1024, "TEST").unwrap();
        assert!(!data.is_empty());
    }

    #[test]
    fn reject_oversized_file() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        // Use a tiny limit so the real Cargo.toml exceeds it.
        let err = read_file_checked(&path, 1, "TEST").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("too large"),
            "error should mention size: {msg}"
        );
        assert!(msg.contains("TEST"), "error should mention format: {msg}");
    }

    #[test]
    fn reject_missing_file() {
        let err = read_file_checked(Path::new("/nonexistent/file.docx"), 1024, "DOCX").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("reading"), "error should have context: {msg}");
    }
}

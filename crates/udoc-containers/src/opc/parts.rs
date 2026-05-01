//! OPC part URI resolution.
//!
//! Handles relative URI resolution between parts within a package,
//! including `../` parent traversal and path normalization.

/// Resolve a target URI relative to a source part.
///
/// Given a source part like `/word/document.xml` and a relative target
/// like `styles.xml`, returns `/word/styles.xml`. Handles `../` traversal.
///
/// If the target is already absolute (starts with `/`), it is returned
/// normalized but unchanged.
pub fn resolve_uri(source_part: &str, target: &str) -> String {
    // If target is absolute, just normalize it
    if target.starts_with('/') {
        return normalize_path(target);
    }

    // Get the "directory" of the source part
    let base = match source_part.rfind('/') {
        Some(i) => &source_part[..=i],
        None => "/",
    };

    // Combine base + target
    let combined = format!("{base}{target}");
    normalize_path(&combined)
}

/// Normalize a path: resolve `.` and `..` segments, ensure leading `/`.
fn normalize_path(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();

    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            s => segments.push(s),
        }
    }

    format!("/{}", segments.join("/"))
}

/// Compute the `.rels` file path for a given part.
///
/// For `/word/document.xml`, returns `word/_rels/document.xml.rels`.
/// For `/_rels/.rels` (the package-level rels), this is not typically called.
pub fn rels_path_for(part_name: &str) -> String {
    let part = part_name.trim_start_matches('/');
    match part.rfind('/') {
        Some(i) => {
            let dir = &part[..i];
            let file = &part[i + 1..];
            format!("{dir}/_rels/{file}.rels")
        }
        None => {
            format!("_rels/{part}.rels")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_relative_same_dir() {
        let result = resolve_uri("/word/document.xml", "styles.xml");
        assert_eq!(result, "/word/styles.xml");
    }

    #[test]
    fn resolve_relative_parent_traversal() {
        let result = resolve_uri("/word/document.xml", "../docProps/core.xml");
        assert_eq!(result, "/docProps/core.xml");
    }

    #[test]
    fn resolve_relative_subdirectory() {
        let result = resolve_uri("/word/document.xml", "media/image1.png");
        assert_eq!(result, "/word/media/image1.png");
    }

    #[test]
    fn resolve_absolute_target() {
        let result = resolve_uri("/word/document.xml", "/xl/workbook.xml");
        assert_eq!(result, "/xl/workbook.xml");
    }

    #[test]
    fn normalize_double_dot() {
        assert_eq!(normalize_path("/a/b/../c"), "/a/c");
        assert_eq!(normalize_path("/a/b/c/../../d"), "/a/d");
    }

    #[test]
    fn normalize_dot_segment() {
        assert_eq!(normalize_path("/a/./b"), "/a/b");
    }

    #[test]
    fn normalize_trailing_slash() {
        assert_eq!(normalize_path("/a/b/"), "/a/b");
    }

    #[test]
    fn rels_path_for_word_document() {
        assert_eq!(
            rels_path_for("/word/document.xml"),
            "word/_rels/document.xml.rels"
        );
    }

    #[test]
    fn rels_path_for_root_file() {
        assert_eq!(
            rels_path_for("/[Content_Types].xml"),
            "_rels/[Content_Types].xml.rels"
        );
    }

    #[test]
    fn rels_path_for_nested() {
        assert_eq!(
            rels_path_for("/xl/worksheets/sheet1.xml"),
            "xl/worksheets/_rels/sheet1.xml.rels"
        );
    }
}

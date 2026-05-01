//! Namespace prefix resolution for XML elements and attributes.
//!
//! Maintains a stack of namespace scopes. Each `StartElement` pushes a new
//! scope containing any `xmlns` declarations found on that element. Each
//! `EndElement` pops the scope. Prefix resolution walks the stack top-down.

use std::sync::Arc;

/// Well-known namespace URI constants for OOXML and ODF formats.
pub mod ns {
    // OOXML namespaces
    /// WordprocessingML (w:)
    pub const WML: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    /// SpreadsheetML (x:)
    pub const SML: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    /// PresentationML (p:)
    pub const PML: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
    /// OPC relationships
    pub const RELATIONSHIPS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
    /// OPC content types
    pub const CONTENT_TYPES: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
    /// DrawingML (a:)
    pub const DRAWINGML: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";

    // OOXML Strict namespaces (Office 2013+)
    /// WordprocessingML Strict (w:)
    pub const WML_STRICT: &str = "http://purl.oclc.org/ooxml/wordprocessingml/main";
    /// SpreadsheetML Strict (x:)
    pub const SML_STRICT: &str = "http://purl.oclc.org/ooxml/spreadsheetml/main";
    /// PresentationML Strict (p:)
    pub const PML_STRICT: &str = "http://purl.oclc.org/ooxml/presentationml/main";
    /// DrawingML Strict (a:)
    pub const DRAWINGML_STRICT: &str = "http://purl.oclc.org/ooxml/drawingml/main";
    /// Office Document Relationships Strict
    pub const RELATIONSHIPS_STRICT: &str =
        "http://purl.oclc.org/ooxml/officeDocument/relationships";

    /// Markup Compatibility (mc:)
    pub const MARKUP_COMPATIBILITY: &str =
        "http://schemas.openxmlformats.org/markup-compatibility/2006";

    // Dublin Core / OPC metadata namespaces
    /// Dublin Core elements (dc:)
    pub const DC_ELEMENTS: &str = "http://purl.org/dc/elements/1.1/";
    /// Dublin Core terms (dcterms:)
    pub const DC_TERMS: &str = "http://purl.org/dc/terms/";
    /// OPC core properties (cp:)
    pub const CORE_PROPERTIES: &str =
        "http://schemas.openxmlformats.org/package/2006/metadata/core-properties";

    // ODF namespaces
    /// ODF text namespace (text:)
    pub const TEXT: &str = "urn:oasis:names:tc:opendocument:xmlns:text:1.0";
    /// ODF table namespace (table:)
    pub const TABLE: &str = "urn:oasis:names:tc:opendocument:xmlns:table:1.0";
    /// ODF office namespace (office:)
    pub const OFFICE: &str = "urn:oasis:names:tc:opendocument:xmlns:office:1.0";
    /// ODF style namespace (style:)
    pub const STYLE: &str = "urn:oasis:names:tc:opendocument:xmlns:style:1.0";
    /// XSL-FO namespace (fo:)
    pub const FO: &str = "urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0";
    /// ODF meta namespace (meta:)
    pub const META: &str = "urn:oasis:names:tc:opendocument:xmlns:meta:1.0";
    /// ODF manifest namespace (manifest:)
    pub const MANIFEST: &str = "urn:oasis:names:tc:opendocument:xmlns:manifest:1.0";
    /// ODF drawing namespace (draw:)
    pub const DRAW: &str = "urn:oasis:names:tc:opendocument:xmlns:drawing:1.0";
    /// ODF presentation namespace (presentation:)
    pub const PRESENTATION: &str = "urn:oasis:names:tc:opendocument:xmlns:presentation:1.0";
    /// ODF SVG-compatible namespace (svg:)
    pub const SVG: &str = "urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0";
}

/// A single namespace binding: prefix -> URI.
#[derive(Debug, Clone)]
struct Binding {
    prefix: String,
    uri: Arc<str>,
}

/// One scope of namespace declarations (corresponds to one element).
#[derive(Debug, Clone)]
struct Scope {
    bindings: Vec<Binding>,
}

/// Stack of namespace scopes for prefix resolution.
#[derive(Debug)]
pub struct NamespaceStack {
    scopes: Vec<Scope>,
}

impl NamespaceStack {
    /// Create an empty namespace stack with the `xml` prefix pre-bound.
    pub fn new() -> Self {
        // The `xml` prefix is always bound per the XML spec.
        let xml_binding = Binding {
            prefix: "xml".to_string(),
            uri: Arc::from("http://www.w3.org/XML/1998/namespace"),
        };
        Self {
            scopes: vec![Scope {
                bindings: vec![xml_binding],
            }],
        }
    }

    /// Push a new empty scope onto the stack. Call this at the start of
    /// processing a StartElement, before adding bindings.
    pub fn push_scope(&mut self) {
        self.scopes.push(Scope {
            bindings: Vec::new(),
        });
    }

    /// Pop the top scope. Call this when processing an EndElement.
    pub fn pop_scope(&mut self) {
        // Never pop the base scope (which holds the `xml` binding).
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }

    /// Add a namespace binding to the current (top) scope.
    /// `prefix` is empty for the default namespace (`xmlns="..."`).
    pub fn bind(&mut self, prefix: String, uri: String) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.bindings.push(Binding {
                prefix,
                uri: Arc::from(uri.as_str()),
            });
        }
    }

    /// Resolve a prefix to a namespace URI. Returns `None` if the prefix
    /// is not bound anywhere in the stack.
    ///
    /// An empty prefix resolves the default namespace.
    /// Returns a cloned `Arc<str>` -- cheap pointer increment, no string copy.
    pub fn resolve(&self, prefix: &str) -> Option<Arc<str>> {
        // Walk scopes top-down for most-recent binding.
        for scope in self.scopes.iter().rev() {
            for binding in scope.bindings.iter().rev() {
                if binding.prefix == prefix {
                    // An empty URI means the default namespace was undeclared.
                    if binding.uri.is_empty() {
                        return None;
                    }
                    return Some(Arc::clone(&binding.uri));
                }
            }
        }
        None
    }
}

impl Default for NamespaceStack {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_prefix_always_bound() {
        let stack = NamespaceStack::new();
        assert_eq!(
            stack.resolve("xml").as_deref(),
            Some("http://www.w3.org/XML/1998/namespace")
        );
    }

    #[test]
    fn unbound_prefix_returns_none() {
        let stack = NamespaceStack::new();
        assert_eq!(stack.resolve("w"), None);
    }

    #[test]
    fn bind_and_resolve() {
        let mut stack = NamespaceStack::new();
        stack.push_scope();
        stack.bind("w".to_string(), ns::WML.to_string());
        assert_eq!(stack.resolve("w").as_deref(), Some(ns::WML));
    }

    #[test]
    fn default_namespace() {
        let mut stack = NamespaceStack::new();
        stack.push_scope();
        stack.bind(String::new(), "http://example.com".to_string());
        assert_eq!(stack.resolve("").as_deref(), Some("http://example.com"));
    }

    #[test]
    fn pop_scope_removes_bindings() {
        let mut stack = NamespaceStack::new();
        stack.push_scope();
        stack.bind("w".to_string(), ns::WML.to_string());
        assert_eq!(stack.resolve("w").as_deref(), Some(ns::WML));
        stack.pop_scope();
        assert_eq!(stack.resolve("w"), None);
    }

    #[test]
    fn inner_scope_shadows_outer() {
        let mut stack = NamespaceStack::new();
        stack.push_scope();
        stack.bind("x".to_string(), "http://outer".to_string());
        stack.push_scope();
        stack.bind("x".to_string(), "http://inner".to_string());
        assert_eq!(stack.resolve("x").as_deref(), Some("http://inner"));
        stack.pop_scope();
        assert_eq!(stack.resolve("x").as_deref(), Some("http://outer"));
    }

    #[test]
    fn base_scope_never_popped() {
        let mut stack = NamespaceStack::new();
        stack.pop_scope(); // should be a no-op
        assert_eq!(
            stack.resolve("xml").as_deref(),
            Some("http://www.w3.org/XML/1998/namespace")
        );
    }

    #[test]
    fn well_known_constants() {
        // Verify they compile and contain expected substrings.
        assert!(ns::WML.contains("wordprocessingml"));
        assert!(ns::SML.contains("spreadsheetml"));
        assert!(ns::PML.contains("presentationml"));
        assert!(ns::RELATIONSHIPS.contains("relationships"));
        assert!(ns::CONTENT_TYPES.contains("content-types"));
        assert!(ns::DRAWINGML.contains("drawingml"));
        assert!(ns::WML_STRICT.contains("purl.oclc.org"));
        assert!(ns::SML_STRICT.contains("spreadsheetml"));
        assert!(ns::PML_STRICT.contains("presentationml"));
        assert!(ns::DRAWINGML_STRICT.contains("drawingml"));
        assert!(ns::RELATIONSHIPS_STRICT.contains("relationships"));
        assert!(ns::TEXT.contains("text"));
        assert!(ns::TABLE.contains("table"));
        assert!(ns::OFFICE.contains("office"));
        assert!(ns::STYLE.contains("style"));
        assert!(ns::FO.contains("fo-compatible"));
    }
}

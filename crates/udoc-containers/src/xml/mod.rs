//! XML pull-parser for OOXML/ODF subset.
//!
//! Namespace-aware, UTF-8 only. Handles elements, attributes, CDATA,
//! processing instructions (skipped), comments (skipped), and the 5
//! predefined XML entities plus numeric character references.
//!
//! # Usage
//!
//! ```
//! use udoc_containers::xml::{XmlReader, XmlEvent};
//!
//! let xml = b"<root><child>text</child></root>";
//! let mut reader = XmlReader::new(xml).unwrap();
//! loop {
//!     match reader.next_event().unwrap() {
//!         XmlEvent::Eof => break,
//!         event => { /* process event */ }
//!     }
//! }
//! ```

use std::borrow::Cow;
use std::sync::Arc;

pub mod entities;
pub mod namespace;
pub mod reader;

pub use namespace::ns;
pub use reader::XmlReader;

/// An XML attribute on a start element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute<'a> {
    /// Local part of the attribute name (after the prefix colon, if any).
    /// Borrows from the input (zero-alloc).
    pub local_name: Cow<'a, str>,
    /// Namespace prefix (empty string if unprefixed). Borrows from the input.
    pub prefix: Cow<'a, str>,
    /// Resolved namespace URI, or `None` if unprefixed (per XML namespace spec,
    /// unprefixed attributes do not inherit the default namespace).
    /// Shared via Arc -- cheap clone, no string copy per element.
    pub namespace_uri: Option<Arc<str>>,
    /// Decoded attribute value. Borrows from input when no entity decoding
    /// or whitespace normalization was needed; owned otherwise.
    pub value: Cow<'a, str>,
}

/// Events emitted by the XML pull-parser.
///
/// The lifetime `'a` ties content to the input data passed to
/// [`XmlReader::new`]. Element and attribute names borrow directly from the
/// input (zero-alloc). Namespace URIs are always owned (resolved from the
/// namespace stack). Attribute values and text content borrow when no entity
/// decoding or normalization was needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlEvent<'a> {
    /// An opening element tag. A matching `EndElement` will follow later.
    /// Self-closing tags (`<br/>`) emit a `StartElement` followed immediately
    /// by an `EndElement`.
    StartElement {
        /// Local name of the element (borrows from input).
        local_name: Cow<'a, str>,
        /// Namespace prefix (empty string if unprefixed, borrows from input).
        prefix: Cow<'a, str>,
        /// Resolved namespace URI (shared via Arc, cheap clone from namespace stack).
        namespace_uri: Option<Arc<str>>,
        /// Attributes on this element (namespace declarations are consumed
        /// and do not appear here).
        attributes: Vec<Attribute<'a>>,
    },

    /// A closing element tag.
    EndElement {
        /// Local name of the element (borrows from input).
        local_name: Cow<'a, str>,
        /// Namespace prefix (empty string if unprefixed, borrows from input).
        prefix: Cow<'a, str>,
        /// Resolved namespace URI (shared via Arc, cheap clone from namespace stack).
        namespace_uri: Option<Arc<str>>,
    },

    /// Entity-decoded text content between tags.
    /// Borrows from the input when no entity decoding was needed (zero-alloc
    /// fast path); owned when entities were decoded.
    Text(Cow<'a, str>),

    /// Raw CDATA section content (not entity-decoded).
    /// Always borrows from the input (zero-alloc).
    CData(Cow<'a, str>),

    /// End of input.
    Eof,
}

/// Find an attribute value by local name in a slice of attributes.
pub fn attr_value<'a>(attrs: &'a [Attribute<'_>], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|a| a.local_name == name)
        .map(|a| a.value.as_ref())
}

/// Parse an OOXML toggle attribute (absent = true, "0"/"false" = false).
///
/// OOXML defines toggle properties like `<b/>` (bold) where the element's
/// presence means true, `val="0"` or `val="false"` means false, and any
/// other value (including absent val) means true.
pub fn toggle_attr(val: Option<&str>) -> bool {
    !matches!(val, Some("0") | Some("false"))
}

/// Find a prefixed attribute value (e.g., `r:id`) in a slice of attributes.
///
/// OOXML uses `r:id` attributes to reference relationships. The prefix is
/// technically document-dependent but is universally `r` in practice.
pub fn prefixed_attr_value<'a>(
    attrs: &'a [Attribute<'_>],
    prefix: &str,
    local_name: &str,
) -> Option<&'a str> {
    attrs
        .iter()
        .find(|a| a.local_name.as_ref() == local_name && a.prefix.as_ref() == prefix)
        .map(|a| a.value.as_ref())
}

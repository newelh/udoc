//! XML pull-parser state machine.
//!
//! `XmlReader` scans through a UTF-8 byte slice, emitting `XmlEvent` values
//! for each start element, end element, text node, and CDATA section. Comments
//! and processing instructions (including `<?xml ... ?>`) are silently skipped.
//!
//! Self-closing tags (`<br/>`) emit a `StartElement` followed by an `EndElement`
//! on the next call. This keeps the event stream uniform for consumers.

use std::borrow::Cow;
use std::sync::Arc;

use memchr::memchr2;
use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use super::entities::decode_entities;
use super::namespace::NamespaceStack;
use super::{Attribute, XmlEvent};
use crate::error::{Error, Result, ResultExt};

/// Maximum element nesting depth to prevent stack exhaustion on malicious input.
const MAX_DEPTH: usize = udoc_core::MAX_NESTING_DEPTH;

/// Maximum number of attributes on a single element. OOXML elements rarely exceed
/// ~20 attributes; anything beyond 1024 is adversarial input.
const MAX_ATTRIBUTES: usize = 1024;

/// Maximum text node size in bytes. OOXML text runs are typically short;
/// anything beyond 16 MiB in a single text node is adversarial or corrupt.
const MAX_TEXT_SIZE: usize = 16 * 1024 * 1024;

/// Cumulative text-bytes budget for a single XML parse session
/// (SEC-ALLOC-CLAMP #62, OOXML-F3).
///
/// The per-node cap (`MAX_TEXT_SIZE = 16 MB`) alone doesn't prevent a
/// malformed XML with many 15-MB text nodes from exhausting memory. This
/// cumulative budget -- applied across every text node in one
/// `XmlReader` -- matches the OOXML-archive decompression budget so a
/// single part can't dwarf what its compressed container allowed in.
const MAX_TEXT_TOTAL: usize = 512 * 1024 * 1024;

/// Pull-parser for a UTF-8 XML document.
///
/// Construct with [`XmlReader::new`] and repeatedly call
/// [`next_event`](XmlReader::next_event) to get events. Returns
/// [`XmlEvent::Eof`] when the input is exhausted.
pub struct XmlReader<'a> {
    /// The full input as a UTF-8 string (validated or BOM-stripped upfront).
    input: &'a str,
    /// Current byte position in `input`.
    pos: usize,
    /// Namespace prefix stack.
    ns: NamespaceStack,
    /// Whether we have already returned Eof.
    eof: bool,
    /// Buffered EndElement to emit after a self-closing StartElement.
    pending_end: Option<XmlEvent<'a>>,
    /// Current element nesting depth (non-self-closing open elements).
    depth: usize,
    /// Running total of text bytes returned from `read_text`. Enforced
    /// against `MAX_TEXT_TOTAL` (SEC-ALLOC-CLAMP #62 OOXML-F3).
    text_total: usize,
    /// Tag name stack for matching start/end tags: (prefix, local_name).
    /// Both slices borrow directly from `input`, eliminating per-tag allocations.
    tag_stack: Vec<(&'a str, &'a str)>,
    /// Optional diagnostics sink for emitting warnings (e.g., tag mismatches).
    diag: Option<Arc<dyn DiagnosticsSink>>,
}

impl<'a> XmlReader<'a> {
    /// Create a new reader over raw bytes.
    ///
    /// Validates UTF-8 and strips a leading BOM if present.
    pub fn new(data: &'a [u8]) -> Result<Self> {
        Self::new_inner(data, None)
    }

    /// Create a new reader with a diagnostics sink for warnings.
    ///
    /// When provided, the reader emits warnings for issues like
    /// mismatched start/end tag names.
    pub fn with_diagnostics(data: &'a [u8], diag: Arc<dyn DiagnosticsSink>) -> Result<Self> {
        Self::new_inner(data, Some(diag))
    }

    /// Return the current byte offset into the input.
    pub fn current_offset(&self) -> usize {
        self.pos
    }

    fn new_inner(data: &'a [u8], diag: Option<Arc<dyn DiagnosticsSink>>) -> Result<Self> {
        let data = if data.starts_with(&[0xEF, 0xBB, 0xBF]) {
            &data[3..]
        } else {
            data
        };

        let input =
            std::str::from_utf8(data).map_err(|e| Error::xml(format!("invalid UTF-8: {e}")))?;

        Ok(Self {
            input,
            pos: 0,
            ns: NamespaceStack::new(),
            eof: false,
            pending_end: None,
            depth: 0,
            text_total: 0,
            tag_stack: Vec::new(),
            diag,
        })
    }

    /// Return the next XML event from the input.
    pub fn next_event(&mut self) -> Result<XmlEvent<'a>> {
        // If we have a buffered EndElement from a self-closing tag, emit it.
        if let Some(end_event) = self.pending_end.take() {
            // Pop the namespace scope for the self-closing element now.
            self.ns.pop_scope();
            return Ok(end_event);
        }

        if self.eof {
            return Ok(XmlEvent::Eof);
        }

        loop {
            if self.pos >= self.input.len() {
                self.eof = true;
                return Ok(XmlEvent::Eof);
            }

            if self.input.as_bytes()[self.pos] == b'<' {
                // Could be tag, comment, CDATA, PI, or declaration.
                let rest = &self.input[self.pos..];

                if rest.starts_with("<!--") {
                    self.skip_comment()?;
                    continue;
                }
                if rest.starts_with("<?") {
                    self.skip_pi()?;
                    continue;
                }
                if rest.starts_with("<!DOCTYPE") {
                    self.skip_doctype()?;
                    continue;
                }
                if rest.starts_with("<![CDATA[") {
                    return self.read_cdata();
                }
                if rest.starts_with("</") {
                    return self.read_end_tag();
                }
                return self.read_start_tag();
            }

            // Text content.
            return self.read_text();
        }
    }

    /// Convenience: return the next event that is not a whitespace-only Text.
    /// Useful for backends parsing structured XML where inter-element whitespace
    /// is irrelevant.
    pub fn next_element(&mut self) -> Result<XmlEvent<'a>> {
        loop {
            let event = self.next_event()?;
            match &event {
                XmlEvent::Text(s) if s.chars().all(char::is_whitespace) => continue,
                _ => return Ok(event),
            }
        }
    }

    // --- Internal scanning helpers ---

    /// Skip a `<!-- ... -->` comment.
    fn skip_comment(&mut self) -> Result<()> {
        let start = self.pos;
        self.pos += 4; // skip "<!--"
        let rest = &self.input[self.pos..];
        match rest.find("-->") {
            Some(end) => {
                self.pos += end + 3;
                Ok(())
            }
            None => Err(Error::xml_at(start as u64, "unclosed comment")),
        }
    }

    /// Skip a `<?target ... ?>` processing instruction (including `<?xml ...?>`).
    fn skip_pi(&mut self) -> Result<()> {
        let start = self.pos;
        self.pos += 2; // skip "<?"
        let rest = &self.input[self.pos..];
        match rest.find("?>") {
            Some(end) => {
                self.pos += end + 2;
                Ok(())
            }
            None => Err(Error::xml_at(
                start as u64,
                "unclosed processing instruction",
            )),
        }
    }

    /// Skip a `<!DOCTYPE ...>` declaration.
    ///
    /// DOCTYPE is not needed for OOXML/ODF parsing. We skip it rather than
    /// erroring to handle real-world files that include one (especially ODF).
    /// Handles nested `[...]` internal subset brackets up to
    /// [`MAX_DEPTH`]; adversarial input with pathologically deep bracket
    /// nesting is rejected rather than scanned to EOF (SEC-ALLOC-CLAMP
    /// #62 OOXML-F5).
    fn skip_doctype(&mut self) -> Result<()> {
        let start = self.pos;
        self.pos += 9; // skip "<!DOCTYPE"
        let mut bracket_depth: usize = 0;
        while self.pos < self.input.len() {
            let b = self.input.as_bytes()[self.pos];
            match b {
                b'[' => {
                    bracket_depth += 1;
                    if bracket_depth > MAX_DEPTH {
                        return Err(Error::xml_at(
                            start as u64,
                            format!(
                                "DOCTYPE internal-subset bracket nesting exceeds limit of {MAX_DEPTH}"
                            ),
                        ));
                    }
                    self.pos += 1;
                }
                b']' => {
                    bracket_depth = bracket_depth.saturating_sub(1);
                    self.pos += 1;
                }
                b'>' if bracket_depth == 0 => {
                    self.pos += 1;
                    return Ok(());
                }
                _ => self.pos += 1,
            }
        }
        Err(Error::xml_at(start as u64, "unclosed DOCTYPE declaration"))
    }

    /// Read a `<![CDATA[ ... ]]>` section.
    fn read_cdata(&mut self) -> Result<XmlEvent<'a>> {
        let start = self.pos;
        self.pos += 9; // skip "<![CDATA["
        let rest = &self.input[self.pos..];
        match rest.find("]]>") {
            Some(end) => {
                let content = &self.input[self.pos..self.pos + end];
                self.pos += end + 3;
                Ok(XmlEvent::CData(Cow::Borrowed(content)))
            }
            None => Err(Error::xml_at(start as u64, "unclosed CDATA section")),
        }
    }

    /// Read an end tag `</prefix:local>` or `</local>`.
    fn read_end_tag(&mut self) -> Result<XmlEvent<'a>> {
        let start = self.pos;
        self.pos += 2; // skip "</"

        let name_start = self.pos;
        self.skip_name();
        let full_name = &self.input[name_start..self.pos];

        self.skip_whitespace();

        if self.pos >= self.input.len() || self.input.as_bytes()[self.pos] != b'>' {
            return Err(Error::xml_at(
                start as u64,
                format!("expected '>' in end tag </{full_name}>"),
            ));
        }
        self.pos += 1; // skip '>'

        let (prefix, local_name) = split_qname(full_name);

        // Check tag name matches the corresponding start tag.
        if let Some((expected_prefix, expected_local)) = self.tag_stack.pop() {
            if expected_local != local_name || expected_prefix != prefix {
                if let Some(diag) = &self.diag {
                    let expected = if expected_prefix.is_empty() {
                        expected_local.to_string()
                    } else {
                        format!("{expected_prefix}:{expected_local}")
                    };
                    let actual = if prefix.is_empty() {
                        local_name.to_string()
                    } else {
                        format!("{prefix}:{local_name}")
                    };
                    diag.warning(
                        Warning::new(
                            "XmlTagMismatch",
                            format!("expected </{expected}>, got </{actual}>"),
                        )
                        .at_offset(start as u64),
                    );
                }
            }
        }

        // Resolve namespace before popping scope.
        let namespace_uri = self.ns.resolve(prefix);

        // Pop the namespace scope that was pushed for the matching start tag.
        self.ns.pop_scope();
        self.depth = self.depth.saturating_sub(1);

        Ok(XmlEvent::EndElement {
            local_name: Cow::Borrowed(local_name),
            prefix: Cow::Borrowed(prefix),
            namespace_uri,
        })
    }

    /// Read a start tag (possibly self-closing).
    fn read_start_tag(&mut self) -> Result<XmlEvent<'a>> {
        let start = self.pos;
        self.pos += 1; // skip '<'

        let name_start = self.pos;
        self.skip_name();
        let full_name = &self.input[name_start..self.pos];

        if full_name.is_empty() {
            return Err(Error::xml_at(start as u64, "empty element name"));
        }

        // Parse attributes. Names borrow from input (&'a str), values are
        // Cow (borrowed when no entity decoding/normalization needed).
        let mut raw_attrs: Vec<(&'a str, &'a str, Cow<'a, str>)> = Vec::new();
        let mut ns_decls: Vec<(String, String)> = Vec::new(); // (prefix, uri)

        loop {
            self.skip_whitespace();
            if self.pos >= self.input.len() {
                return Err(Error::xml_at(start as u64, "unclosed start tag"))
                    .context(format!("parsing <{full_name}>"));
            }

            let ch = self.input.as_bytes()[self.pos];
            if ch == b'/' {
                // Self-closing: />
                // Self-closing tags don't increment depth or push to tag_stack:
                // they can't nest (no child elements between open and close),
                // so depth tracking and end-tag matching are unnecessary.
                if self.pos + 1 < self.input.len() && self.input.as_bytes()[self.pos + 1] == b'>' {
                    self.pos += 2;

                    // Push scope and bind namespace declarations.
                    self.ns.push_scope();
                    for (pfx, uri) in &ns_decls {
                        self.ns.bind(pfx.clone(), uri.clone());
                    }

                    let (prefix, local_name) = split_qname(full_name);
                    let namespace_uri = self.ns.resolve(prefix);
                    let attributes = self.resolve_attributes(&raw_attrs);

                    // Buffer the EndElement. The scope will be popped when
                    // the pending EndElement is emitted on the next call.
                    self.pending_end = Some(XmlEvent::EndElement {
                        local_name: Cow::Borrowed(local_name),
                        prefix: Cow::Borrowed(prefix),
                        namespace_uri: namespace_uri.clone(),
                    });

                    return Ok(XmlEvent::StartElement {
                        local_name: Cow::Borrowed(local_name),
                        prefix: Cow::Borrowed(prefix),
                        namespace_uri,
                        attributes,
                    });
                }
                return Err(Error::xml_at(
                    self.pos as u64,
                    "expected '>' after '/' in tag",
                ));
            }
            if ch == b'>' {
                self.pos += 1;

                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err(Error::xml_at(
                        start as u64,
                        format!("element nesting depth exceeds limit of {MAX_DEPTH}"),
                    ));
                }

                // Push scope and bind namespace declarations.
                self.ns.push_scope();
                for (pfx, uri) in &ns_decls {
                    self.ns.bind(pfx.clone(), uri.clone());
                }

                let (prefix, local_name) = split_qname(full_name);
                let namespace_uri = self.ns.resolve(prefix);
                let attributes = self.resolve_attributes(&raw_attrs);

                // Track the tag name for end-tag matching. Both slices borrow
                // from self.input, so no allocation is needed.
                self.tag_stack.push((prefix, local_name));

                return Ok(XmlEvent::StartElement {
                    local_name: Cow::Borrowed(local_name),
                    prefix: Cow::Borrowed(prefix),
                    namespace_uri,
                    attributes,
                });
            }

            // Must be an attribute.
            if raw_attrs.len() + ns_decls.len() >= MAX_ATTRIBUTES {
                return Err(Error::xml_at(
                    start as u64,
                    format!("element <{full_name}> exceeds attribute limit of {MAX_ATTRIBUTES}"),
                ));
            }
            let (attr_name, attr_value) = self
                .read_attribute()
                .context(format!("parsing <{full_name}>"))?;

            // Check for namespace declaration. Namespace bindings need owned
            // strings (stored in the namespace stack), so we .into_owned() here.
            if attr_name == "xmlns" {
                ns_decls.push((String::new(), attr_value.into_owned()));
            } else if let Some(pfx) = attr_name.strip_prefix("xmlns:") {
                ns_decls.push((pfx.to_string(), attr_value.into_owned()));
            } else {
                let (pfx, local) = split_qname(attr_name);
                raw_attrs.push((pfx, local, attr_value));
            }
        }
    }

    /// Read a single attribute: `name="value"` or `name='value'`.
    /// Returns (full_name borrowed from input, decoded_value as Cow).
    fn read_attribute(&mut self) -> Result<(&'a str, Cow<'a, str>)> {
        let attr_name_start = self.pos;
        self.skip_name();
        let attr_name = &self.input[attr_name_start..self.pos];

        if attr_name.is_empty() {
            return Err(Error::xml_at(
                attr_name_start as u64,
                "expected attribute name",
            ));
        }

        self.skip_whitespace();

        // Expect '='
        if self.pos >= self.input.len() || self.input.as_bytes()[self.pos] != b'=' {
            return Err(Error::xml_at(
                self.pos as u64,
                format!("expected '=' after attribute name '{attr_name}'"),
            ));
        }
        self.pos += 1;
        self.skip_whitespace();

        // Expect quote
        if self.pos >= self.input.len() {
            return Err(Error::xml_at(self.pos as u64, "expected attribute value"));
        }
        let quote = self.input.as_bytes()[self.pos];
        if quote != b'"' && quote != b'\'' {
            return Err(Error::xml_at(
                self.pos as u64,
                format!("expected '\"' or '\\'', got '{}'", char::from(quote)),
            ));
        }
        self.pos += 1;

        // Read until matching quote. Per XML spec, `<` is not allowed in
        // attribute values (must be escaped as `&lt;`). Reject it to avoid
        // scanning past the tag boundary on malformed input.
        //
        // Use memchr2 for SIMD-accelerated search for the closing quote or `<`.
        let val_start = self.pos;
        let bytes = self.input.as_bytes();
        match memchr2(quote, b'<', &bytes[self.pos..]) {
            Some(offset) if bytes[self.pos + offset] == quote => {
                // Fast path: found closing quote with no `<` before it.
                self.pos += offset;
            }
            Some(offset) => {
                // `<` found before closing quote -- illegal in attribute values.
                self.pos += offset;
                return Err(Error::xml_at(
                    self.pos as u64,
                    format!("illegal '<' in attribute value for '{attr_name}'"),
                ));
            }
            None => {
                // Neither quote nor `<` found: unclosed attribute value.
                self.pos = bytes.len();
                return Err(Error::xml_at(val_start as u64, "unclosed attribute value"));
            }
        }
        if self.pos >= self.input.len() {
            return Err(Error::xml_at(val_start as u64, "unclosed attribute value"));
        }
        let raw_value = &self.input[val_start..self.pos];
        self.pos += 1; // skip closing quote

        let decoded = decode_entities(raw_value);
        // Normalize whitespace (XML 1.0 s3.3.3). Stays borrowed (zero-alloc)
        // when the value has no entities and no whitespace to normalize.
        let value = normalize_attr_value(decoded);
        Ok((attr_name, value))
    }

    /// Read text content (everything before the next '<').
    fn read_text(&mut self) -> Result<XmlEvent<'a>> {
        let text_start = self.pos;
        let remaining = &self.input.as_bytes()[self.pos..];
        // memchr uses SIMD when available: much faster than byte-by-byte loop.
        self.pos += memchr::memchr(b'<', remaining).unwrap_or(remaining.len());
        let len = self.pos - text_start;
        if len > MAX_TEXT_SIZE {
            return Err(Error::xml_at(
                text_start as u64,
                format!("text node size ({len} bytes) exceeds limit of {MAX_TEXT_SIZE}"),
            ));
        }
        // SEC-ALLOC-CLAMP #62 ( OOXML-F3): cumulative per-part
        // text cap. Without this a malformed part with many 15-MB text
        // nodes exhausts memory even though each individual node is
        // within MAX_TEXT_SIZE.
        self.text_total = self.text_total.saturating_add(len);
        if self.text_total > MAX_TEXT_TOTAL {
            return Err(Error::xml_at(
                text_start as u64,
                format!(
                    "cumulative text bytes ({}) in this XML part exceed limit of {} \
                     (triggered at node of {len} bytes)",
                    self.text_total, MAX_TEXT_TOTAL
                ),
            ));
        }
        let raw_text = &self.input[text_start..self.pos];
        let decoded = decode_entities(raw_text);
        Ok(XmlEvent::Text(decoded))
    }

    /// Advance past XML name characters (letters, digits, ':', '-', '_', '.').
    /// Intentionally ASCII-only: OOXML/ODF element and attribute names are
    /// always ASCII. Full XML spec allows Unicode in names, but that is out
    /// of scope for this subset parser.
    // ODF XML names are ASCII (text:p, table:table-cell, etc.). Tested with
    // 64+ ODF tests including fuzz target. Non-ASCII in XML names is technically
    // possible per XML spec but not used by ODF or OOXML in practice.
    fn skip_name(&mut self) {
        let bytes = self.input.as_bytes();
        while self.pos < bytes.len() && is_name_byte(bytes[self.pos]) {
            self.pos += 1;
        }
    }

    /// Advance past ASCII whitespace.
    fn skip_whitespace(&mut self) {
        let bytes = self.input.as_bytes();
        while self.pos < bytes.len() && is_xml_whitespace(bytes[self.pos]) {
            self.pos += 1;
        }
    }

    /// Resolve prefixed attributes against the namespace stack.
    /// Per XML namespace spec, unprefixed attributes do NOT get the default
    /// namespace.
    fn resolve_attributes(
        &self,
        raw_attrs: &[(&'a str, &'a str, Cow<'a, str>)],
    ) -> Vec<Attribute<'a>> {
        raw_attrs
            .iter()
            .map(|&(prefix, local_name, ref value)| {
                let namespace_uri = if prefix.is_empty() {
                    // Unprefixed attributes have no namespace.
                    None
                } else {
                    self.ns.resolve(prefix)
                };
                Attribute {
                    local_name: Cow::Borrowed(local_name),
                    prefix: Cow::Borrowed(prefix),
                    namespace_uri,
                    value: value.clone(),
                }
            })
            .collect()
    }
}

/// Split a qualified name `prefix:local` into `(prefix, local)`.
/// If there is no colon, returns `("", full_name)`.
#[inline]
fn split_qname(name: &str) -> (&str, &str) {
    match name.find(':') {
        Some(idx) => (&name[..idx], &name[idx + 1..]),
        None => ("", name),
    }
}

/// Fast byte classification for XML name characters.
/// Uses a lookup table instead of 5 comparisons per byte.
#[inline(always)]
fn is_name_byte(b: u8) -> bool {
    // XML name chars: [A-Za-z0-9:_.-]
    // Lookup table: 256 bools, indexed by byte value.
    const TABLE: [bool; 256] = {
        let mut t = [false; 256];
        let mut i = 0u16;
        while i < 256 {
            let b = i as u8;
            t[i as usize] =
                b.is_ascii_alphanumeric() || b == b':' || b == b'-' || b == b'_' || b == b'.';
            i += 1;
        }
        t
    };
    TABLE[b as usize]
}

/// Fast byte classification for XML whitespace.
#[inline(always)]
fn is_xml_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Normalize attribute value per XML 1.0 s3.3.3:
/// replace `\t`, `\n`, `\r` with space. Preserves the `Cow` variant when
/// no normalization is needed so the common no-entities, no-whitespace path
/// stays zero-alloc.
fn normalize_attr_value<'a>(value: Cow<'a, str>) -> Cow<'a, str> {
    if memchr::memchr3(0x09, 0x0A, 0x0D, value.as_bytes()).is_some() {
        Cow::Owned(
            value
                .chars()
                .map(|c| match c {
                    '\t' | '\n' | '\r' => ' ',
                    _ => c,
                })
                .collect(),
        )
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events<'a>(xml: &'a str) -> Result<Vec<XmlEvent<'a>>> {
        let mut reader = XmlReader::new(xml.as_bytes())?;
        let mut out = Vec::new();
        loop {
            let ev = reader.next_event()?;
            if matches!(ev, XmlEvent::Eof) {
                break;
            }
            out.push(ev);
        }
        Ok(out)
    }

    #[test]
    fn simple_element() {
        let evs = events("<hello>world</hello>").unwrap();
        assert_eq!(evs.len(), 3);
        match &evs[0] {
            XmlEvent::StartElement { local_name, .. } => assert_eq!(local_name, "hello"),
            other => panic!("expected StartElement, got {other:?}"),
        }
        match &evs[1] {
            XmlEvent::Text(s) => assert_eq!(s, "world"),
            other => panic!("expected Text, got {other:?}"),
        }
        match &evs[2] {
            XmlEvent::EndElement { local_name, .. } => assert_eq!(local_name, "hello"),
            other => panic!("expected EndElement, got {other:?}"),
        }
    }

    #[test]
    fn nested_elements() {
        let evs = events("<a><b>text</b></a>").unwrap();
        assert_eq!(evs.len(), 5);
        assert!(matches!(&evs[0], XmlEvent::StartElement { local_name, .. } if local_name == "a"));
        assert!(matches!(&evs[1], XmlEvent::StartElement { local_name, .. } if local_name == "b"));
        assert!(matches!(&evs[2], XmlEvent::Text(s) if s == "text"));
        assert!(matches!(&evs[3], XmlEvent::EndElement { local_name, .. } if local_name == "b"));
        assert!(matches!(&evs[4], XmlEvent::EndElement { local_name, .. } if local_name == "a"));
    }

    #[test]
    fn self_closing_emits_start_then_end() {
        let evs = events("<br/>").unwrap();
        assert_eq!(evs.len(), 2);
        match &evs[0] {
            XmlEvent::StartElement { local_name, .. } => assert_eq!(local_name, "br"),
            other => panic!("expected StartElement, got {other:?}"),
        }
        match &evs[1] {
            XmlEvent::EndElement { local_name, .. } => assert_eq!(local_name, "br"),
            other => panic!("expected EndElement, got {other:?}"),
        }
    }

    #[test]
    fn self_closing_with_space() {
        let evs = events("<br />").unwrap();
        assert_eq!(evs.len(), 2);
        assert!(matches!(&evs[0], XmlEvent::StartElement { local_name, .. } if local_name == "br"));
        assert!(matches!(&evs[1], XmlEvent::EndElement { local_name, .. } if local_name == "br"));
    }

    #[test]
    fn attributes_with_entity_refs() {
        let evs = events(r#"<a href="a&amp;b"/>"#).unwrap();
        match &evs[0] {
            XmlEvent::StartElement { attributes, .. } => {
                assert_eq!(attributes.len(), 1);
                assert_eq!(attributes[0].local_name, "href");
                assert_eq!(attributes[0].value, "a&b");
            }
            other => panic!("expected StartElement, got {other:?}"),
        }
    }

    #[test]
    fn single_quoted_attributes() {
        let evs = events("<a x='hello'/>").unwrap();
        match &evs[0] {
            XmlEvent::StartElement { attributes, .. } => {
                assert_eq!(attributes[0].value, "hello");
            }
            other => panic!("expected StartElement, got {other:?}"),
        }
    }

    #[test]
    fn cdata_section() {
        let evs = events("<r><![CDATA[<not>xml]]></r>").unwrap();
        assert_eq!(evs.len(), 3);
        match &evs[1] {
            XmlEvent::CData(s) => assert_eq!(s, "<not>xml"),
            other => panic!("expected CData, got {other:?}"),
        }
    }

    #[test]
    fn comments_skipped() {
        let evs = events("<!-- comment --><a/>").unwrap();
        assert_eq!(evs.len(), 2); // StartElement + EndElement
        assert!(matches!(&evs[0], XmlEvent::StartElement { local_name, .. } if local_name == "a"));
        assert!(matches!(&evs[1], XmlEvent::EndElement { local_name, .. } if local_name == "a"));
    }

    #[test]
    fn processing_instructions_skipped() {
        let evs = events("<?xml version=\"1.0\"?><?mso-application progid=\"Word.Document\"?><a/>")
            .unwrap();
        assert_eq!(evs.len(), 2);
        assert!(matches!(&evs[0], XmlEvent::StartElement { local_name, .. } if local_name == "a"));
    }

    #[test]
    fn utf8_bom_stripped() {
        let mut data = vec![0xEF, 0xBB, 0xBF]; // BOM
        data.extend_from_slice(b"<a/>");
        let mut reader = XmlReader::new(&data).unwrap();
        let ev = reader.next_event().unwrap();
        assert!(matches!(ev, XmlEvent::StartElement { .. }));
    }

    #[test]
    fn namespace_declaration_and_resolution() {
        let xml = r#"<w:document xmlns:w="http://example.com/w"><w:body/></w:document>"#;
        let evs = events(xml).unwrap();
        // <w:document>, <w:body/> -> start+end, </w:document>
        assert_eq!(evs.len(), 4);
        match &evs[0] {
            XmlEvent::StartElement {
                local_name,
                prefix,
                namespace_uri,
                ..
            } => {
                assert_eq!(local_name, "document");
                assert_eq!(prefix, "w");
                assert_eq!(namespace_uri.as_deref(), Some("http://example.com/w"));
            }
            other => panic!("expected StartElement, got {other:?}"),
        }
        // <w:body/> emits StartElement
        match &evs[1] {
            XmlEvent::StartElement {
                local_name,
                prefix,
                namespace_uri,
                ..
            } => {
                assert_eq!(local_name, "body");
                assert_eq!(prefix, "w");
                assert_eq!(namespace_uri.as_deref(), Some("http://example.com/w"));
            }
            other => panic!("expected StartElement (body), got {other:?}"),
        }
        // then EndElement for the self-closing body
        match &evs[2] {
            XmlEvent::EndElement {
                local_name,
                prefix,
                namespace_uri,
            } => {
                assert_eq!(local_name, "body");
                assert_eq!(prefix, "w");
                assert_eq!(namespace_uri.as_deref(), Some("http://example.com/w"));
            }
            other => panic!("expected EndElement (body), got {other:?}"),
        }
    }

    #[test]
    fn default_namespace() {
        let xml = r#"<root xmlns="http://default"><child/></root>"#;
        let evs = events(xml).unwrap();
        match &evs[0] {
            XmlEvent::StartElement { namespace_uri, .. } => {
                assert_eq!(namespace_uri.as_deref(), Some("http://default"));
            }
            other => panic!("expected StartElement, got {other:?}"),
        }
        // Unprefixed child also gets default namespace (self-closing -> StartElement).
        match &evs[1] {
            XmlEvent::StartElement { namespace_uri, .. } => {
                assert_eq!(namespace_uri.as_deref(), Some("http://default"));
            }
            other => panic!("expected StartElement (child), got {other:?}"),
        }
    }

    #[test]
    fn namespace_prefix_rebinding() {
        let xml = r#"<a xmlns:x="http://one"><b xmlns:x="http://two"><x:c/></b><x:d/></a>"#;
        let mut reader = XmlReader::new(xml.as_bytes()).unwrap();

        // <a ...>
        let ev = reader.next_event().unwrap();
        assert!(matches!(ev, XmlEvent::StartElement { .. }));

        // <b ...>
        let ev = reader.next_event().unwrap();
        assert!(matches!(ev, XmlEvent::StartElement { .. }));

        // <x:c/> StartElement -- should resolve to "http://two"
        let ev = reader.next_event().unwrap();
        match &ev {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                ..
            } => {
                assert_eq!(local_name, "c");
                assert_eq!(namespace_uri.as_deref(), Some("http://two"));
            }
            other => panic!("expected StartElement, got {other:?}"),
        }

        // <x:c/> EndElement
        let ev = reader.next_event().unwrap();
        assert!(matches!(ev, XmlEvent::EndElement { .. }));

        // </b>
        let ev = reader.next_event().unwrap();
        assert!(matches!(ev, XmlEvent::EndElement { .. }));

        // <x:d/> StartElement -- back to "http://one"
        let ev = reader.next_event().unwrap();
        match &ev {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                ..
            } => {
                assert_eq!(local_name, "d");
                assert_eq!(namespace_uri.as_deref(), Some("http://one"));
            }
            other => panic!("expected StartElement, got {other:?}"),
        }

        // <x:d/> EndElement
        let ev = reader.next_event().unwrap();
        assert!(matches!(ev, XmlEvent::EndElement { .. }));
    }

    #[test]
    fn multiple_namespace_prefixes_on_one_element() {
        let xml = r#"<root xmlns:a="http://a" xmlns:b="http://b"><a:x/><b:y/></root>"#;
        let evs = events(xml).unwrap();
        // root start, a:x start, a:x end, b:y start, b:y end, root end = 6
        assert_eq!(evs.len(), 6);
        match &evs[1] {
            XmlEvent::StartElement {
                prefix,
                namespace_uri,
                ..
            } => {
                assert_eq!(prefix, "a");
                assert_eq!(namespace_uri.as_deref(), Some("http://a"));
            }
            other => panic!("expected StartElement, got {other:?}"),
        }
        match &evs[3] {
            XmlEvent::StartElement {
                prefix,
                namespace_uri,
                ..
            } => {
                assert_eq!(prefix, "b");
                assert_eq!(namespace_uri.as_deref(), Some("http://b"));
            }
            other => panic!("expected StartElement, got {other:?}"),
        }
    }

    #[test]
    fn text_with_entity_references() {
        let evs = events("<r>a &amp; b &lt; &gt; &quot; &apos;</r>").unwrap();
        match &evs[1] {
            XmlEvent::Text(s) => assert_eq!(s, "a & b < > \" '"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn numeric_char_refs_in_text() {
        let evs = events("<r>&#65;&#x42;</r>").unwrap();
        match &evs[1] {
            XmlEvent::Text(s) => assert_eq!(s, "AB"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn empty_element_with_attributes() {
        let evs = events(r#"<img src="a.png" alt=""/>"#).unwrap();
        match &evs[0] {
            XmlEvent::StartElement { attributes, .. } => {
                assert_eq!(attributes.len(), 2);
                assert_eq!(attributes[0].local_name, "src");
                assert_eq!(attributes[0].value, "a.png");
                assert_eq!(attributes[1].local_name, "alt");
                assert_eq!(attributes[1].value, "");
            }
            other => panic!("expected StartElement, got {other:?}"),
        }
    }

    #[test]
    fn attribute_value_normalization() {
        // \t and \n should become spaces per XML 1.0 s3.3.3
        let evs = events("<a x=\"a\tb\nc\rd\"/>").unwrap();
        match &evs[0] {
            XmlEvent::StartElement { attributes, .. } => {
                assert_eq!(attributes[0].value, "a b c d");
            }
            other => panic!("expected StartElement, got {other:?}"),
        }
    }

    #[test]
    fn normalize_attr_value_preserves_borrowed() {
        let input = Cow::Borrowed("no whitespace to normalize");
        let result = normalize_attr_value(input);
        assert!(matches!(result, Cow::Borrowed(_)), "should stay borrowed");

        let input = Cow::Borrowed("has\ttab");
        let result = normalize_attr_value(input);
        assert!(matches!(result, Cow::Owned(_)), "should become owned");
        assert_eq!(result, "has tab");
    }

    #[test]
    fn deeply_nested_50_levels() {
        let open: String = (0..50).map(|i| format!("<l{i}>")).collect();
        let close: String = (0..50).rev().map(|i| format!("</l{i}>")).collect();
        let xml = format!("{open}deep{close}");
        let evs = events(&xml).unwrap();
        // 50 start + 1 text + 50 end = 101
        assert_eq!(evs.len(), 101);
    }

    #[test]
    fn malformed_unclosed_tag() {
        let result = events("<unclosed");
        assert!(result.is_err());
    }

    #[test]
    fn unescaped_ampersand_in_text() {
        // Bare & is technically invalid XML, but decode_entities handles it
        // gracefully by passing it through when no closing ; is found.
        let evs = events("<r>a & b</r>").unwrap();
        match &evs[1] {
            XmlEvent::Text(s) => assert_eq!(s, "a & b"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn next_element_skips_whitespace_text() {
        let xml = "<a>\n  <b/>\n</a>";
        let mut reader = XmlReader::new(xml.as_bytes()).unwrap();
        let ev = reader.next_element().unwrap();
        assert!(matches!(ev, XmlEvent::StartElement { .. })); // <a>
        let ev = reader.next_element().unwrap();
        assert!(matches!(&ev, XmlEvent::StartElement { local_name, .. } if local_name == "b"));
        let ev = reader.next_element().unwrap();
        assert!(matches!(&ev, XmlEvent::EndElement { local_name, .. } if local_name == "b"));
        let ev = reader.next_element().unwrap();
        assert!(matches!(&ev, XmlEvent::EndElement { local_name, .. } if local_name == "a"));
    }

    #[test]
    fn realistic_ooxml_fragment() {
        let xml = concat!(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
            r#"<w:body><w:p><w:r><w:t>Hello</w:t></w:r></w:p></w:body>"#,
            r#"</w:document>"#,
        );
        let mut reader = XmlReader::new(xml.as_bytes()).unwrap();

        // <w:document>
        let ev = reader.next_element().unwrap();
        match &ev {
            XmlEvent::StartElement {
                local_name,
                prefix,
                namespace_uri,
                ..
            } => {
                assert_eq!(local_name, "document");
                assert_eq!(prefix, "w");
                assert_eq!(
                    namespace_uri.as_deref(),
                    Some("http://schemas.openxmlformats.org/wordprocessingml/2006/main")
                );
            }
            other => panic!("expected StartElement, got {other:?}"),
        }

        // <w:body>
        let ev = reader.next_element().unwrap();
        assert!(
            matches!(&ev, XmlEvent::StartElement { local_name, prefix, .. }
            if local_name == "body" && prefix == "w")
        );

        // <w:p>
        let ev = reader.next_element().unwrap();
        assert!(matches!(&ev, XmlEvent::StartElement { local_name, .. } if local_name == "p"));

        // <w:r>
        let ev = reader.next_element().unwrap();
        assert!(matches!(&ev, XmlEvent::StartElement { local_name, .. } if local_name == "r"));

        // <w:t>
        let ev = reader.next_element().unwrap();
        assert!(matches!(&ev, XmlEvent::StartElement { local_name, .. } if local_name == "t"));

        // "Hello"
        let ev = reader.next_element().unwrap();
        assert!(matches!(&ev, XmlEvent::Text(s) if s == "Hello"));

        // </w:t>, </w:r>, </w:p>, </w:body>, </w:document>
        for expected in &["t", "r", "p", "body", "document"] {
            let ev = reader.next_element().unwrap();
            match &ev {
                XmlEvent::EndElement { local_name, .. } => {
                    assert_eq!(local_name, *expected);
                }
                other => panic!("expected EndElement for {expected}, got {other:?}"),
            }
        }

        assert!(matches!(reader.next_element().unwrap(), XmlEvent::Eof));
    }

    #[test]
    fn default_ns_does_not_apply_to_unprefixed_attributes() {
        let xml = r#"<root xmlns="http://default" attr="val"/>"#;
        let evs = events(xml).unwrap();
        match &evs[0] {
            XmlEvent::StartElement {
                namespace_uri,
                attributes,
                ..
            } => {
                // Element gets default namespace.
                assert_eq!(namespace_uri.as_deref(), Some("http://default"));
                // Unprefixed attribute does NOT.
                assert_eq!(attributes[0].namespace_uri, None);
            }
            other => panic!("expected StartElement, got {other:?}"),
        }
    }

    #[test]
    fn tag_mismatch_emits_warning() {
        use udoc_core::diagnostics::CollectingDiagnostics;

        let xml = b"<a></b>";
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut reader = XmlReader::with_diagnostics(xml, diag.clone()).unwrap();
        let _ = reader.next_event().unwrap(); // <a>
        let _ = reader.next_event().unwrap(); // </b> (mismatch)

        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "XmlTagMismatch"),
            "expected XmlTagMismatch warning, got: {warnings:?}"
        );
    }

    #[test]
    fn tag_mismatch_no_warning_without_diagnostics() {
        // Without a DiagnosticsSink, mismatch is silently accepted
        let evs = events("<a></b>").unwrap();
        assert_eq!(evs.len(), 2);
        assert!(matches!(&evs[0], XmlEvent::StartElement { local_name, .. } if local_name == "a"));
        assert!(matches!(&evs[1], XmlEvent::EndElement { local_name, .. } if local_name == "b"));
    }

    #[test]
    fn eof_is_idempotent() {
        let mut reader = XmlReader::new(b"<a/>").unwrap();
        let _ = reader.next_event().unwrap(); // StartElement
        let _ = reader.next_event().unwrap(); // EndElement
        assert!(matches!(reader.next_event().unwrap(), XmlEvent::Eof));
        assert!(matches!(reader.next_event().unwrap(), XmlEvent::Eof));
    }

    #[test]
    fn depth_limit_exceeded() {
        let open: String = (0..257).map(|i| format!("<l{i}>")).collect();
        let close: String = (0..257).rev().map(|i| format!("</l{i}>")).collect();
        let xml = format!("{open}{close}");
        let result = events(&xml);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("depth"), "got: {err}");
    }

    #[test]
    fn depth_at_limit_succeeds() {
        // Exactly 256 levels should be fine.
        let open: String = (0..256).map(|i| format!("<l{i}>")).collect();
        let close: String = (0..256).rev().map(|i| format!("</l{i}>")).collect();
        let xml = format!("{open}leaf{close}");
        let evs = events(&xml).unwrap();
        // 256 start + 1 text + 256 end = 513
        assert_eq!(evs.len(), 513);
    }

    #[test]
    fn angle_bracket_in_attribute_rejected() {
        let result = events(r#"<a x="val<ue"/>"#);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("<"), "got: {err}");
    }

    #[test]
    fn attribute_limit_exceeded() {
        // Build an element with more than MAX_ATTRIBUTES attributes.
        let mut xml = String::from("<a ");
        for i in 0..1025 {
            xml.push_str(&format!("x{}=\"v\" ", i));
        }
        xml.push_str("/>");
        let result = events(&xml);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("attribute limit"), "got: {err}");
    }

    #[test]
    fn text_node_size_limit() {
        // Build a text node just over MAX_TEXT_SIZE (16 MiB).
        let text = "x".repeat(16 * 1024 * 1024 + 1);
        let xml = format!("<r>{text}</r>");
        let result = events(&xml);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("text node size"), "got: {err}");
    }

    #[test]
    fn doctype_skipped() {
        let evs = events("<!DOCTYPE html><html/>").unwrap();
        assert_eq!(evs.len(), 2);
        assert!(
            matches!(&evs[0], XmlEvent::StartElement { local_name, .. } if local_name == "html")
        );
        assert!(matches!(&evs[1], XmlEvent::EndElement { local_name, .. } if local_name == "html"));
    }

    #[test]
    fn doctype_with_internal_subset_skipped() {
        let xml = r#"<!DOCTYPE doc [<!ENTITY foo "bar">]><doc/>"#;
        let evs = events(xml).unwrap();
        assert_eq!(evs.len(), 2);
        assert!(
            matches!(&evs[0], XmlEvent::StartElement { local_name, .. } if local_name == "doc")
        );
    }

    #[test]
    fn unclosed_doctype_is_error() {
        let result = events("<!DOCTYPE html");
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("DOCTYPE"), "got: {err}");
    }

    #[test]
    fn self_closing_with_namespace_scope_isolation() {
        // The namespace declared on a self-closing element should NOT leak
        // to siblings.
        let xml = r#"<root><a xmlns:z="http://z"/><b/></root>"#;
        let evs = events(xml).unwrap();
        // root start, a start, a end, b start, b end, root end = 6
        assert_eq!(evs.len(), 6);
        // <a> should have z namespace available
        match &evs[1] {
            XmlEvent::StartElement { local_name, .. } => assert_eq!(local_name, "a"),
            other => panic!("expected StartElement, got {other:?}"),
        }
        // <b> should NOT have z namespace
        match &evs[3] {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                ..
            } => {
                assert_eq!(local_name, "b");
                assert_eq!(*namespace_uri, None);
            }
            other => panic!("expected StartElement, got {other:?}"),
        }
    }

    #[test]
    fn current_offset_starts_at_zero() {
        let reader = XmlReader::new(b"<a/>").unwrap();
        assert_eq!(reader.current_offset(), 0);
    }

    #[test]
    fn current_offset_advances_after_events() {
        let xml = b"<a>text</a>";
        let mut reader = XmlReader::new(xml).unwrap();

        // After parsing <a>, offset should have moved past the tag.
        let ev = reader.next_event().unwrap();
        assert!(matches!(ev, XmlEvent::StartElement { .. }));
        let after_start = reader.current_offset();
        assert!(after_start > 0, "offset should advance past start tag");

        // After parsing "text", offset should advance further.
        let ev = reader.next_event().unwrap();
        assert!(matches!(ev, XmlEvent::Text(_)));
        let after_text = reader.current_offset();
        assert!(
            after_text > after_start,
            "offset should advance past text node"
        );

        // After parsing </a>, offset should be at the end of input.
        let ev = reader.next_event().unwrap();
        assert!(matches!(ev, XmlEvent::EndElement { .. }));
        assert_eq!(reader.current_offset(), xml.len());
    }
}

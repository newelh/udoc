//! ODF style resolver with parent-chain inheritance.
//!
//! Parses style:style elements from both styles.xml and content.xml
//! automatic-styles sections. Resolves properties through the
//! style:parent-style-name chain with a depth cap.

use std::collections::HashMap;

use udoc_containers::xml::namespace::ns;
use udoc_containers::xml::{attr_value, XmlEvent, XmlReader};
use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use crate::error::{Result, ResultExt};

/// Maximum depth for style:parent-style-name inheritance chains.
const MAX_BASED_ON_DEPTH: usize = 10;

/// Maximum number of style definitions (safety limit).
const MAX_STYLES: usize = 50_000;

/// A style definition from ODF.
#[derive(Debug, Clone)]
pub(crate) struct StyleDef {
    /// Style name (the style:name attribute).
    #[allow(dead_code)]
    pub name: String,
    /// Style family: "paragraph", "text", "table", etc.
    #[allow(dead_code)]
    pub family: String,
    /// Parent style name (style:parent-style-name).
    pub parent_style_name: Option<String>,
    /// Bold from fo:font-weight="bold".
    pub bold: Option<bool>,
    /// Italic from fo:font-style="italic".
    pub italic: Option<bool>,
    /// Font size from fo:font-size (e.g. "14pt").
    pub font_size: Option<f64>,
    /// Heading outline level from style:default-outline-level.
    pub outline_level: Option<u8>,
    /// Text color from fo:color="#RRGGBB".
    pub color: Option<[u8; 3]>,
    /// Background color from fo:background-color="#RRGGBB".
    pub background_color: Option<[u8; 3]>,
    /// Font name from style:font-name or fo:font-family.
    pub font_name: Option<String>,
    /// Underline from style:text-underline-style (any non-"none" value).
    pub underline: Option<bool>,
    /// Strikethrough from style:text-line-through-style (any non-"none" value).
    pub strikethrough: Option<bool>,
    /// Text alignment from fo:text-align on style:paragraph-properties.
    pub alignment: Option<String>,
    /// Space before from fo:margin-top on style:paragraph-properties (in points).
    pub space_before: Option<f64>,
    /// Space after from fo:margin-bottom on style:paragraph-properties (in points).
    pub space_after: Option<f64>,
    /// Left indent from fo:margin-left on style:paragraph-properties (in points).
    pub indent_left: Option<f64>,
    /// Right indent from fo:margin-right on style:paragraph-properties (in points).
    pub indent_right: Option<f64>,
}

/// Span-level boolean properties resolved from the style inheritance chain.
///
/// Each field is `None` when the property is not set anywhere in the chain,
/// or `Some(value)` when it was found (possibly via parent inheritance).
#[derive(Debug, Clone, Default)]
pub(crate) struct ResolvedSpanFlags {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strikethrough: Option<bool>,
}

/// All text-level properties resolved from the style inheritance chain in a
/// single walk: boolean flags + font name, font size, color, background color.
#[derive(Debug, Clone, Default)]
pub(crate) struct ResolvedTextProps {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strikethrough: Option<bool>,
    pub font_name: Option<String>,
    pub font_size: Option<f64>,
    pub color: Option<[u8; 3]>,
    pub background_color: Option<[u8; 3]>,
}

/// All block-layout properties resolved from the style inheritance chain in a
/// single walk: alignment, indentation, and spacing.
#[derive(Debug, Clone, Default)]
pub(crate) struct ResolvedBlockProps {
    pub alignment: Option<String>,
    pub indent_left: Option<f64>,
    pub indent_right: Option<f64>,
    pub space_before: Option<f64>,
    pub space_after: Option<f64>,
}

/// Map of style names to their definitions.
#[derive(Debug, Default)]
pub(crate) struct OdfStyleMap {
    styles: HashMap<String, StyleDef>,
}

impl OdfStyleMap {
    /// Look up a style by name.
    #[cfg(test)]
    pub fn get(&self, name: &str) -> Option<&StyleDef> {
        self.styles.get(name)
    }

    /// Resolve the heading level for a paragraph with the given style name.
    /// Returns 0 for body text, 1-6 for heading levels.
    pub fn resolve_heading_level(&self, style_name: &str) -> u8 {
        self.resolve_heading_inner(style_name, 0)
    }

    fn resolve_heading_inner(&self, style_name: &str, depth: usize) -> u8 {
        if depth >= MAX_BASED_ON_DEPTH {
            return 0;
        }
        let style = match self.styles.get(style_name) {
            Some(s) => s,
            None => return 0,
        };

        if let Some(level) = style.outline_level {
            return level.clamp(1, 6);
        }

        if let Some(ref parent) = style.parent_style_name {
            return self.resolve_heading_inner(parent, depth + 1);
        }

        0
    }

    /// Resolve whether a style (including inheritance) is bold.
    /// Superseded by `resolve_span_flags` for production use; kept for
    /// test assertions that verify individual property resolution.
    #[cfg(test)]
    pub fn resolve_bold(&self, style_name: &str) -> Option<bool> {
        self.resolve_bool_prop(style_name, |s| s.bold, 0)
    }

    /// Resolve whether a style (including inheritance) is italic.
    #[cfg(test)]
    pub fn resolve_italic(&self, style_name: &str) -> Option<bool> {
        self.resolve_bool_prop(style_name, |s| s.italic, 0)
    }

    /// Resolve whether a style (including inheritance) has underline.
    #[cfg(test)]
    pub fn resolve_underline(&self, style_name: &str) -> Option<bool> {
        self.resolve_bool_prop(style_name, |s| s.underline, 0)
    }

    /// Resolve whether a style (including inheritance) has strikethrough.
    #[cfg(test)]
    pub fn resolve_strikethrough(&self, style_name: &str) -> Option<bool> {
        self.resolve_bool_prop(style_name, |s| s.strikethrough, 0)
    }

    /// Resolve bold, italic, underline, and strikethrough in a single
    /// parent-chain walk. Lighter than `resolve_text_props` since it
    /// skips font name, font size, and color fields.
    pub fn resolve_span_flags(&self, style_name: &str) -> ResolvedSpanFlags {
        let mut flags = ResolvedSpanFlags::default();
        let mut current = style_name;

        for _ in 0..MAX_BASED_ON_DEPTH {
            let style = match self.styles.get(current) {
                Some(s) => s,
                None => break,
            };

            if flags.bold.is_none() {
                flags.bold = style.bold;
            }
            if flags.italic.is_none() {
                flags.italic = style.italic;
            }
            if flags.underline.is_none() {
                flags.underline = style.underline;
            }
            if flags.strikethrough.is_none() {
                flags.strikethrough = style.strikethrough;
            }

            if flags.bold.is_some()
                && flags.italic.is_some()
                && flags.underline.is_some()
                && flags.strikethrough.is_some()
            {
                break;
            }

            match style.parent_style_name {
                Some(ref parent) => current = parent,
                None => break,
            }
        }

        flags
    }

    /// Resolve all text-level properties in a single parent-chain walk.
    /// Replaces calling resolve_span_flags + resolve_font_name + resolve_font_size
    /// + resolve_color + resolve_background_color separately (5 walks -> 1).
    pub fn resolve_text_props(&self, style_name: &str) -> ResolvedTextProps {
        let mut props = ResolvedTextProps::default();
        let mut current = style_name;

        for _ in 0..MAX_BASED_ON_DEPTH {
            let style = match self.styles.get(current) {
                Some(s) => s,
                None => break,
            };

            if props.bold.is_none() {
                props.bold = style.bold;
            }
            if props.italic.is_none() {
                props.italic = style.italic;
            }
            if props.underline.is_none() {
                props.underline = style.underline;
            }
            if props.strikethrough.is_none() {
                props.strikethrough = style.strikethrough;
            }
            if props.font_name.is_none() {
                props.font_name = style.font_name.clone();
            }
            if props.font_size.is_none() {
                props.font_size = style.font_size;
            }
            if props.color.is_none() {
                props.color = style.color;
            }
            if props.background_color.is_none() {
                props.background_color = style.background_color;
            }

            // All properties resolved, no need to walk further.
            if props.bold.is_some()
                && props.italic.is_some()
                && props.underline.is_some()
                && props.strikethrough.is_some()
                && props.font_name.is_some()
                && props.font_size.is_some()
                && props.color.is_some()
                && props.background_color.is_some()
            {
                break;
            }

            match style.parent_style_name {
                Some(ref parent) => current = parent,
                None => break,
            }
        }

        props
    }

    /// Resolve all block-layout properties in a single parent-chain walk.
    /// Replaces calling resolve_alignment + resolve_indent_left + resolve_indent_right
    /// + resolve_space_before + resolve_space_after separately (5 walks -> 1).
    pub fn resolve_block_props(&self, style_name: &str) -> ResolvedBlockProps {
        let mut props = ResolvedBlockProps::default();
        let mut current = style_name;

        for _ in 0..MAX_BASED_ON_DEPTH {
            let style = match self.styles.get(current) {
                Some(s) => s,
                None => break,
            };

            if props.alignment.is_none() {
                props.alignment = style.alignment.clone();
            }
            if props.indent_left.is_none() {
                props.indent_left = style.indent_left;
            }
            if props.indent_right.is_none() {
                props.indent_right = style.indent_right;
            }
            if props.space_before.is_none() {
                props.space_before = style.space_before;
            }
            if props.space_after.is_none() {
                props.space_after = style.space_after;
            }

            // All properties resolved, no need to walk further.
            if props.alignment.is_some()
                && props.indent_left.is_some()
                && props.indent_right.is_some()
                && props.space_before.is_some()
                && props.space_after.is_some()
            {
                break;
            }

            match style.parent_style_name {
                Some(ref parent) => current = parent,
                None => break,
            }
        }

        props
    }

    /// Resolve text color (including inheritance).
    #[cfg(test)]
    pub fn resolve_color(&self, style_name: &str) -> Option<[u8; 3]> {
        self.resolve_option_prop(style_name, |s| s.color, 0)
    }

    /// Resolve font name (including inheritance).
    #[cfg(test)]
    pub fn resolve_font_name(&self, style_name: &str) -> Option<String> {
        self.resolve_clone_prop(style_name, |s| s.font_name.as_ref(), 0)
    }

    #[cfg(test)]
    fn resolve_bool_prop(
        &self,
        style_name: &str,
        getter: fn(&StyleDef) -> Option<bool>,
        depth: usize,
    ) -> Option<bool> {
        if depth >= MAX_BASED_ON_DEPTH {
            return None;
        }
        let style = self.styles.get(style_name)?;
        if let Some(val) = getter(style) {
            return Some(val);
        }
        if let Some(ref parent) = style.parent_style_name {
            return self.resolve_bool_prop(parent, getter, depth + 1);
        }
        None
    }

    /// Resolve a Copy property through the inheritance chain.
    #[cfg(test)]
    fn resolve_option_prop<T: Copy>(
        &self,
        style_name: &str,
        getter: fn(&StyleDef) -> Option<T>,
        depth: usize,
    ) -> Option<T> {
        if depth >= MAX_BASED_ON_DEPTH {
            return None;
        }
        let style = self.styles.get(style_name)?;
        if let Some(val) = getter(style) {
            return Some(val);
        }
        if let Some(ref parent) = style.parent_style_name {
            return self.resolve_option_prop(parent, getter, depth + 1);
        }
        None
    }

    /// Resolve a Clone property (String etc.) through the inheritance chain.
    #[cfg(test)]
    fn resolve_clone_prop<T: Clone>(
        &self,
        style_name: &str,
        getter: fn(&StyleDef) -> Option<&T>,
        depth: usize,
    ) -> Option<T> {
        if depth >= MAX_BASED_ON_DEPTH {
            return None;
        }
        let style = self.styles.get(style_name)?;
        if let Some(val) = getter(style) {
            return Some(val.clone());
        }
        if let Some(ref parent) = style.parent_style_name {
            return self.resolve_clone_prop(parent, getter, depth + 1);
        }
        None
    }

    /// Merge another style map into this one. Existing entries are NOT overwritten
    /// (automatic styles from content.xml take precedence over styles.xml defaults).
    pub fn merge_defaults(&mut self, other: OdfStyleMap) {
        for (name, def) in other.styles {
            self.styles.entry(name).or_insert(def);
        }
    }

    /// Number of styles loaded.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.styles.len()
    }

    /// Whether the style map is empty.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.styles.is_empty()
    }
}

/// Check if a namespace URI is the ODF style namespace.
fn is_style_ns(ns: Option<&str>) -> bool {
    matches!(ns, Some(ns::STYLE))
}

/// Check if a namespace URI is the FO namespace.
fn is_fo_ns(ns: Option<&str>) -> bool {
    matches!(ns, Some(ns::FO))
}

/// Parse style:style elements from an XML document (styles.xml or content.xml).
pub(crate) fn parse_styles(data: &[u8], diag: &dyn DiagnosticsSink) -> Result<OdfStyleMap> {
    let mut reader = XmlReader::new(data).context("initializing XML parser for styles")?;

    let mut styles = HashMap::new();

    loop {
        let event = reader.next_element().context("parsing styles")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } if is_style_ns(namespace_uri.as_deref()) && local_name.as_ref() == "style" => {
                if styles.len() >= MAX_STYLES {
                    diag.warning(Warning::new(
                        "OdfMaxStyles",
                        format!("style definition limit ({MAX_STYLES}) exceeded, truncating"),
                    ));
                    skip_element(&mut reader)?;
                    continue;
                }

                let style_name = attr_value(&attributes, "name").unwrap_or("").to_string();
                let family = attr_value(&attributes, "family")
                    .unwrap_or("paragraph")
                    .to_string();
                let parent = attr_value(&attributes, "parent-style-name").map(|s| s.to_string());
                let outline_level = attr_value(&attributes, "default-outline-level")
                    .and_then(|s| s.parse::<u8>().ok());

                if !style_name.is_empty() {
                    let style = parse_style_def(
                        &mut reader,
                        style_name.clone(),
                        family,
                        parent,
                        outline_level,
                    )?;
                    styles.insert(style_name, style);
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(OdfStyleMap { styles })
}

/// Parse a single style:style element body for text and paragraph properties.
fn parse_style_def(
    reader: &mut XmlReader<'_>,
    name: String,
    family: String,
    parent_style_name: Option<String>,
    outline_level: Option<u8>,
) -> Result<StyleDef> {
    let mut style = StyleDef {
        name,
        family,
        parent_style_name,
        bold: None,
        italic: None,
        font_size: None,
        outline_level,
        color: None,
        background_color: None,
        font_name: None,
        underline: None,
        strikethrough: None,
        alignment: None,
        space_before: None,
        space_after: None,
        indent_left: None,
        indent_right: None,
    };

    let mut depth: usize = 1;

    loop {
        let event = reader.next_element().context("parsing style:style")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                depth += 1;

                if is_style_ns(namespace_uri.as_deref()) {
                    match local_name.as_ref() {
                        "text-properties" => {
                            parse_text_properties(&attributes, &mut style);
                        }
                        "paragraph-properties" => {
                            parse_paragraph_properties(&attributes, &mut style);
                        }
                        _ => {}
                    }
                }
            }
            XmlEvent::EndElement { .. } => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(style)
}

/// Extract text properties from style:text-properties attributes.
fn parse_text_properties(attributes: &[udoc_containers::xml::Attribute<'_>], style: &mut StyleDef) {
    for attr in attributes {
        if is_fo_ns(attr.namespace_uri.as_deref()) {
            match attr.local_name.as_ref() {
                "font-weight" => {
                    style.bold = Some(attr.value.as_ref() == "bold");
                }
                "font-style" => {
                    style.italic = Some(attr.value.as_ref() == "italic");
                }
                "font-size" => {
                    style.font_size = parse_font_size(attr.value.as_ref());
                }
                "color" => {
                    style.color = parse_hex_color(attr.value.as_ref());
                }
                "background-color" => {
                    style.background_color = parse_hex_color(attr.value.as_ref());
                }
                "font-family" => {
                    let val = attr.value.as_ref().trim();
                    if !val.is_empty() {
                        // Strip quotes around font family names (single or double).
                        let unquoted = val
                            .strip_prefix('\'')
                            .and_then(|s| s.strip_suffix('\''))
                            .or_else(|| val.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
                            .unwrap_or(val);
                        if style.font_name.is_none() {
                            style.font_name = Some(unquoted.to_string());
                        }
                    }
                }
                _ => {}
            }
        } else if is_style_ns(attr.namespace_uri.as_deref()) {
            match attr.local_name.as_ref() {
                "font-name" => {
                    let val = attr.value.as_ref().trim();
                    if !val.is_empty() {
                        // style:font-name takes precedence over fo:font-family.
                        style.font_name = Some(val.to_string());
                    }
                }
                "text-underline-style" => {
                    style.underline = Some(attr.value.as_ref() != "none");
                }
                "text-line-through-style" => {
                    style.strikethrough = Some(attr.value.as_ref() != "none");
                }
                _ => {}
            }
        }
    }
}

/// Extract paragraph properties from style:paragraph-properties attributes.
fn parse_paragraph_properties(
    attributes: &[udoc_containers::xml::Attribute<'_>],
    style: &mut StyleDef,
) {
    for attr in attributes {
        if is_fo_ns(attr.namespace_uri.as_deref()) {
            match attr.local_name.as_ref() {
                "text-align" => {
                    let val = attr.value.as_ref().trim();
                    if !val.is_empty() {
                        style.alignment = Some(val.to_string());
                    }
                }
                "margin-top" => {
                    style.space_before = parse_length_to_pt(attr.value.as_ref());
                }
                "margin-bottom" => {
                    style.space_after = parse_length_to_pt(attr.value.as_ref());
                }
                "margin-left" => {
                    style.indent_left = parse_length_to_pt(attr.value.as_ref());
                }
                "margin-right" => {
                    style.indent_right = parse_length_to_pt(attr.value.as_ref());
                }
                _ => {}
            }
        }
    }
}

/// Parse a font-size value into points.
///
/// Supports "pt" suffix directly, then falls back to `parse_length_to_pt`
/// for cm/in/mm, and finally tries a bare number (treated as pt).
fn parse_font_size(value: &str) -> Option<f64> {
    let trimmed = value.trim();
    let pts = if let Some(numeric) = trimmed.strip_suffix("pt") {
        numeric.parse::<f64>().ok()
    } else if let Some(v) = parse_length_to_pt(trimmed) {
        Some(v)
    } else {
        // Bare number with no unit: treat as points (lenient recovery).
        // ODF spec requires a unit suffix; bare numbers indicate a non-conformant
        // producer. We accept them to avoid rejecting real-world documents.
        trimmed.parse::<f64>().ok()
    };
    // Negative font sizes are meaningless; treat as unparseable.
    pts.filter(|v| *v >= 0.0)
}

/// Parse a "#RRGGBB" hex color string into [r, g, b].
fn parse_hex_color(value: &str) -> Option<[u8; 3]> {
    udoc_core::document::Color::from_css_hex(value).map(|c| c.to_array())
}

/// Parse a length value with unit suffix to points.
/// Supports: pt, cm, in, mm. Returns None if unparseable.
fn parse_length_to_pt(value: &str) -> Option<f64> {
    let trimmed = value.trim();
    if let Some(num) = trimmed.strip_suffix("pt") {
        return num.parse::<f64>().ok();
    }
    if let Some(num) = trimmed.strip_suffix("cm") {
        return num.parse::<f64>().ok().map(|v| v * 28.3465);
    }
    if let Some(num) = trimmed.strip_suffix("in") {
        return num.parse::<f64>().ok().map(|v| v * 72.0);
    }
    if let Some(num) = trimmed.strip_suffix("mm") {
        return num.parse::<f64>().ok().map(|v| v * 2.83465);
    }
    None
}

/// Skip an element and all its children.
pub(crate) fn skip_element(reader: &mut XmlReader<'_>) -> Result<()> {
    let mut depth: usize = 1;
    loop {
        let event = reader.next_element().context("skipping element")?;
        match event {
            XmlEvent::StartElement { .. } => depth += 1,
            XmlEvent::EndElement { .. } => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Ok(());
                }
            }
            XmlEvent::Eof => return Ok(()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::diagnostics::NullDiagnostics;

    #[test]
    fn parse_basic_style() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:styles>
    <style:style style:name="Bold" style:family="text">
      <style:text-properties fo:font-weight="bold" fo:font-size="14pt"/>
    </style:style>
  </office:styles>
</office:document-styles>"#;

        let map = parse_styles(xml, &NullDiagnostics).unwrap();
        let style = map.get("Bold").unwrap();
        assert_eq!(style.bold, Some(true));
        assert_eq!(style.font_size, Some(14.0));
    }

    #[test]
    fn chain_resolution() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:styles>
    <style:style style:name="Parent" style:family="paragraph">
      <style:text-properties fo:font-weight="bold"/>
    </style:style>
    <style:style style:name="Child" style:family="paragraph" style:parent-style-name="Parent"/>
  </office:styles>
</office:document-styles>"#;

        let map = parse_styles(xml, &NullDiagnostics).unwrap();
        assert_eq!(map.resolve_bold("Child"), Some(true));
        assert_eq!(map.resolve_italic("Child"), None);
    }

    #[test]
    fn heading_level_from_outline() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0">
  <office:styles>
    <style:style style:name="Heading_20_1" style:family="paragraph"
                 style:default-outline-level="1"/>
    <style:style style:name="CustomH" style:family="paragraph"
                 style:parent-style-name="Heading_20_1"/>
  </office:styles>
</office:document-styles>"#;

        let map = parse_styles(xml, &NullDiagnostics).unwrap();
        assert_eq!(map.resolve_heading_level("Heading_20_1"), 1);
        assert_eq!(map.resolve_heading_level("CustomH"), 1);
        assert_eq!(map.resolve_heading_level("nonexistent"), 0);
    }

    #[test]
    fn depth_cap_prevents_infinite_loop() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0">
  <office:styles>
    <style:style style:name="A" style:family="paragraph" style:parent-style-name="B"/>
    <style:style style:name="B" style:family="paragraph" style:parent-style-name="A"/>
  </office:styles>
</office:document-styles>"#;

        let map = parse_styles(xml, &NullDiagnostics).unwrap();
        assert_eq!(map.resolve_heading_level("A"), 0);
        assert_eq!(map.resolve_bold("A"), None);
    }

    #[test]
    fn parse_italic_style() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:styles>
    <style:style style:name="ItalicStyle" style:family="text">
      <style:text-properties fo:font-style="italic"/>
    </style:style>
  </office:styles>
</office:document-styles>"#;

        let map = parse_styles(xml, &NullDiagnostics).unwrap();
        let style = map.get("ItalicStyle").unwrap();
        assert_eq!(style.italic, Some(true));
        assert_eq!(style.bold, None);
    }

    #[test]
    fn merge_defaults() {
        let xml1 = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:styles>
    <style:style style:name="S1" style:family="paragraph">
      <style:text-properties fo:font-weight="bold"/>
    </style:style>
  </office:styles>
</office:document-styles>"#;

        let xml2 = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:styles>
    <style:style style:name="S1" style:family="paragraph">
      <style:text-properties fo:font-style="italic"/>
    </style:style>
    <style:style style:name="S2" style:family="paragraph"/>
  </office:styles>
</office:document-styles>"#;

        let mut map1 = parse_styles(xml1, &NullDiagnostics).unwrap();
        let map2 = parse_styles(xml2, &NullDiagnostics).unwrap();

        map1.merge_defaults(map2);
        assert_eq!(map1.resolve_bold("S1"), Some(true));
        assert!(map1.get("S2").is_some());
    }

    #[test]
    fn max_styles_limit() {
        let count = MAX_STYLES + 100;
        let mut xml = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <office:document-styles\n\
                 xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\"\n\
                 xmlns:style=\"urn:oasis:names:tc:opendocument:xmlns:style:1.0\">\n\
               <office:styles>\n",
        );
        for i in 0..count {
            xml.push_str(&format!(
                "    <style:style style:name=\"S{i}\" style:family=\"paragraph\"/>\n"
            ));
        }
        xml.push_str("  </office:styles>\n</office:document-styles>");

        let map = parse_styles(xml.as_bytes(), &NullDiagnostics).unwrap();
        assert_eq!(map.len(), MAX_STYLES);
    }

    #[test]
    fn parse_text_color() {
        let xml = br##"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:styles>
    <style:style style:name="Red" style:family="text">
      <style:text-properties fo:color="#FF0000"/>
    </style:style>
  </office:styles>
</office:document-styles>"##;

        let map = parse_styles(xml, &NullDiagnostics).unwrap();
        let style = map.get("Red").unwrap();
        assert_eq!(style.color, Some([255, 0, 0]));
    }

    #[test]
    fn parse_background_color() {
        let xml = br##"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:styles>
    <style:style style:name="Highlight" style:family="text">
      <style:text-properties fo:background-color="#FFFF00"/>
    </style:style>
  </office:styles>
</office:document-styles>"##;

        let map = parse_styles(xml, &NullDiagnostics).unwrap();
        let style = map.get("Highlight").unwrap();
        assert_eq!(style.background_color, Some([255, 255, 0]));
    }

    #[test]
    fn parse_font_name_and_underline() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:styles>
    <style:style style:name="Fancy" style:family="text">
      <style:text-properties style:font-name="Arial"
                             style:text-underline-style="solid"
                             style:text-line-through-style="solid"/>
    </style:style>
  </office:styles>
</office:document-styles>"#;

        let map = parse_styles(xml, &NullDiagnostics).unwrap();
        let style = map.get("Fancy").unwrap();
        assert_eq!(style.font_name.as_deref(), Some("Arial"));
        assert_eq!(style.underline, Some(true));
        assert_eq!(style.strikethrough, Some(true));
    }

    #[test]
    fn parse_underline_none() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0">
  <office:styles>
    <style:style style:name="NoUnderline" style:family="text">
      <style:text-properties style:text-underline-style="none"/>
    </style:style>
  </office:styles>
</office:document-styles>"#;

        let map = parse_styles(xml, &NullDiagnostics).unwrap();
        let style = map.get("NoUnderline").unwrap();
        assert_eq!(style.underline, Some(false));
    }

    #[test]
    fn parse_paragraph_props() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:styles>
    <style:style style:name="Centered" style:family="paragraph">
      <style:paragraph-properties fo:text-align="center"
                                  fo:margin-top="12pt"
                                  fo:margin-bottom="6pt"
                                  fo:margin-left="1cm"
                                  fo:margin-right="0.5in"/>
    </style:style>
  </office:styles>
</office:document-styles>"#;

        let map = parse_styles(xml, &NullDiagnostics).unwrap();
        let style = map.get("Centered").unwrap();
        assert_eq!(style.alignment.as_deref(), Some("center"));
        assert!((style.space_before.unwrap() - 12.0).abs() < 0.01);
        assert!((style.space_after.unwrap() - 6.0).abs() < 0.01);
        assert!((style.indent_left.unwrap() - 28.3465).abs() < 0.1);
        assert!((style.indent_right.unwrap() - 36.0).abs() < 0.01);
    }

    #[test]
    fn resolve_color_through_inheritance() {
        let xml = br##"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:styles>
    <style:style style:name="Base" style:family="paragraph">
      <style:text-properties fo:color="#0000FF" style:font-name="Helvetica"/>
    </style:style>
    <style:style style:name="Derived" style:family="paragraph"
                 style:parent-style-name="Base"/>
  </office:styles>
</office:document-styles>"##;

        let map = parse_styles(xml, &NullDiagnostics).unwrap();
        assert_eq!(map.resolve_color("Derived"), Some([0, 0, 255]));
        assert_eq!(
            map.resolve_font_name("Derived"),
            Some("Helvetica".to_string())
        );
    }

    #[test]
    fn parse_hex_color_edge_cases() {
        assert_eq!(parse_hex_color("#FF0000"), Some([255, 0, 0]));
        assert_eq!(parse_hex_color("#000000"), Some([0, 0, 0]));
        assert_eq!(parse_hex_color("#ffffff"), Some([255, 255, 255]));
        assert_eq!(parse_hex_color("transparent"), None);
        assert_eq!(parse_hex_color("#FFF"), None);
        assert_eq!(parse_hex_color(""), None);
        assert_eq!(parse_hex_color("#GGHHII"), None);
        // Multi-byte UTF-8 that happens to be 6 bytes after '#' must not panic.
        assert_eq!(parse_hex_color("#\u{00e9}\u{00e9}\u{00e9}"), None);
        assert_eq!(parse_hex_color("#\u{0100}\u{0101}\u{0102}"), None);
    }

    #[test]
    fn parse_length_conversions() {
        assert!((parse_length_to_pt("12pt").unwrap() - 12.0).abs() < 0.01);
        assert!((parse_length_to_pt("1in").unwrap() - 72.0).abs() < 0.01);
        assert!((parse_length_to_pt("1cm").unwrap() - 28.3465).abs() < 0.1);
        assert!((parse_length_to_pt("10mm").unwrap() - 28.3465).abs() < 0.1);
        assert!(parse_length_to_pt("invalid").is_none());
        assert!(parse_length_to_pt("").is_none());
    }

    #[test]
    fn font_family_fallback() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:styles>
    <style:style style:name="FontFamily" style:family="text">
      <style:text-properties fo:font-family="'Times New Roman'"/>
    </style:style>
  </office:styles>
</office:document-styles>"#;

        let map = parse_styles(xml, &NullDiagnostics).unwrap();
        let style = map.get("FontFamily").unwrap();
        assert_eq!(style.font_name.as_deref(), Some("Times New Roman"));
    }

    #[test]
    fn resolve_span_flags_all_set_directly() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:styles>
    <style:style style:name="AllSet" style:family="text">
      <style:text-properties fo:font-weight="bold"
                             fo:font-style="italic"
                             style:text-underline-style="solid"
                             style:text-line-through-style="solid"/>
    </style:style>
  </office:styles>
</office:document-styles>"#;

        let map = parse_styles(xml, &NullDiagnostics).unwrap();
        let flags = map.resolve_span_flags("AllSet");
        assert_eq!(flags.bold, Some(true));
        assert_eq!(flags.italic, Some(true));
        assert_eq!(flags.underline, Some(true));
        assert_eq!(flags.strikethrough, Some(true));
    }

    #[test]
    fn resolve_span_flags_inherited() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:styles>
    <style:style style:name="Base" style:family="text">
      <style:text-properties fo:font-weight="bold"
                             style:text-underline-style="solid"/>
    </style:style>
    <style:style style:name="Child" style:family="text"
                 style:parent-style-name="Base">
      <style:text-properties fo:font-style="italic"
                             style:text-line-through-style="solid"/>
    </style:style>
  </office:styles>
</office:document-styles>"#;

        let map = parse_styles(xml, &NullDiagnostics).unwrap();
        let flags = map.resolve_span_flags("Child");
        // bold and underline inherited from Base.
        assert_eq!(flags.bold, Some(true));
        assert_eq!(flags.underline, Some(true));
        // italic and strikethrough set directly on Child.
        assert_eq!(flags.italic, Some(true));
        assert_eq!(flags.strikethrough, Some(true));

        // Verify consistency with individual resolvers.
        assert_eq!(flags.bold, map.resolve_bold("Child"));
        assert_eq!(flags.italic, map.resolve_italic("Child"));
        assert_eq!(flags.underline, map.resolve_underline("Child"));
        assert_eq!(flags.strikethrough, map.resolve_strikethrough("Child"));
    }

    #[test]
    fn resolve_span_flags_unknown_style() {
        let map = OdfStyleMap::default();
        let flags = map.resolve_span_flags("does_not_exist");
        assert_eq!(flags.bold, None);
        assert_eq!(flags.italic, None);
        assert_eq!(flags.underline, None);
        assert_eq!(flags.strikethrough, None);
    }

    #[test]
    fn font_family_double_quoted() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:styles>
    <style:style style:name="DQ" style:family="text">
      <style:text-properties fo:font-family="&quot;Times New Roman&quot;"/>
    </style:style>
    <style:style style:name="SQ" style:family="text">
      <style:text-properties fo:font-family="'Courier New'"/>
    </style:style>
    <style:style style:name="NQ" style:family="text">
      <style:text-properties fo:font-family="Arial"/>
    </style:style>
  </office:styles>
</office:document-styles>"#;

        let map = parse_styles(xml, &NullDiagnostics).unwrap();
        assert_eq!(
            map.get("DQ").unwrap().font_name.as_deref(),
            Some("Times New Roman"),
            "double-quoted font family should have quotes stripped"
        );
        assert_eq!(
            map.get("SQ").unwrap().font_name.as_deref(),
            Some("Courier New"),
            "single-quoted font family should have quotes stripped"
        );
        assert_eq!(
            map.get("NQ").unwrap().font_name.as_deref(),
            Some("Arial"),
            "unquoted font family should be preserved as-is"
        );
    }
}

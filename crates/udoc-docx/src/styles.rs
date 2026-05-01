//! DOCX styles.xml parser.
//!
//! Parses style definitions from word/styles.xml including paragraph styles,
//! character styles, and w:basedOn inheritance chains. Used for heading
//! detection and run property inheritance.

use std::collections::HashMap;
use std::sync::Arc;

use udoc_containers::xml::{attr_value, toggle_attr, XmlEvent, XmlReader};
use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use crate::error::{Result, ResultExt};
use crate::parser::is_wml;

/// Maximum depth for w:basedOn style inheritance chains (security limit).
const MAX_BASED_ON_DEPTH: usize = 10;

/// Maximum number of style definitions (safety limit).
const MAX_STYLES: usize = 50_000;

/// A style definition from styles.xml.
#[derive(Debug, Clone)]
pub struct StyleDef {
    /// Style ID (the w:styleId attribute).
    pub id: String,
    /// Style type: "paragraph", "character", "table", "numbering".
    pub style_type: String,
    /// Human-readable style name (w:name val).
    pub name: Option<String>,
    /// Parent style ID (w:basedOn val).
    pub based_on: Option<String>,
    /// Outline level from this style definition (w:outlineLvl val).
    pub outline_level: Option<u8>,
    /// Whether runs in this style are bold.
    pub bold: Option<bool>,
    /// Whether runs in this style are italic.
    pub italic: Option<bool>,
    /// Font name from this style.
    pub font_name: Option<String>,
    /// Font size in points from this style.
    pub font_size_pts: Option<f64>,
}

/// Map of style IDs to their definitions.
#[derive(Debug, Default)]
pub struct StyleMap {
    styles: HashMap<String, StyleDef>,
}

impl StyleMap {
    /// Look up a style by ID.
    pub fn get(&self, id: &str) -> Option<&StyleDef> {
        self.styles.get(id)
    }

    /// Resolve the heading level for a paragraph with the given style ID.
    /// Returns 0 for body text, 1-6 for heading levels.
    ///
    /// Strategy:
    /// 1. Check direct w:outlineLvl on the paragraph (already parsed).
    /// 2. Check w:outlineLvl in the style definition.
    /// 3. Chase w:basedOn chain looking for outlineLvl (capped at depth 10).
    /// 4. Fallback: match w:name against "heading N" (English canonical names).
    pub fn resolve_heading_level(&self, style_id: &str) -> u8 {
        self.resolve_heading_level_inner(style_id, 0)
    }

    fn resolve_heading_level_inner(&self, style_id: &str, depth: usize) -> u8 {
        if depth >= MAX_BASED_ON_DEPTH {
            return 0;
        }

        let style = match self.styles.get(style_id) {
            Some(s) => s,
            None => return 0,
        };

        // Check outlineLvl on this style.
        if let Some(level) = style.outline_level {
            // outlineLvl is 0-based: val=0 -> Heading 1, val=5 -> Heading 6.
            return (level + 1).min(6);
        }

        // Chase basedOn chain before falling back to name heuristic.
        // Inherited outlineLvl is authoritative per OOXML spec.
        if let Some(ref parent_id) = style.based_on {
            let inherited = self.resolve_heading_level_inner(parent_id, depth + 1);
            if inherited > 0 {
                return inherited;
            }
        }

        // Last-resort fallback: match style name against "heading N" pattern.
        if let Some(ref name) = style.name {
            let lower = name.to_ascii_lowercase();
            if let Some(rest) = lower.strip_prefix("heading") {
                let rest = rest.trim();
                if let Ok(n) = rest.parse::<u8>() {
                    if (1..=6).contains(&n) {
                        return n;
                    }
                }
            }
        }

        0
    }

    /// Resolve whether a style (including inheritance) is bold.
    pub fn resolve_bold(&self, style_id: &str) -> Option<bool> {
        self.resolve_bool_prop(style_id, |s| s.bold, 0)
    }

    /// Resolve whether a style (including inheritance) is italic.
    pub fn resolve_italic(&self, style_id: &str) -> Option<bool> {
        self.resolve_bool_prop(style_id, |s| s.italic, 0)
    }

    fn resolve_bool_prop(
        &self,
        style_id: &str,
        getter: fn(&StyleDef) -> Option<bool>,
        depth: usize,
    ) -> Option<bool> {
        if depth >= MAX_BASED_ON_DEPTH {
            return None;
        }
        let style = self.styles.get(style_id)?;
        if let Some(val) = getter(style) {
            return Some(val);
        }
        if let Some(ref parent) = style.based_on {
            return self.resolve_bool_prop(parent, getter, depth + 1);
        }
        None
    }
}

/// Parse styles.xml into a StyleMap.
pub(crate) fn parse_styles(data: &[u8], diag: &Arc<dyn DiagnosticsSink>) -> Result<StyleMap> {
    let mut reader = XmlReader::new(data).context("initializing XML parser for styles.xml")?;

    let mut styles = HashMap::new();

    loop {
        let event = reader.next_element().context("parsing styles.xml")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } if is_wml(namespace_uri.as_deref()) && local_name == "style" => {
                if styles.len() >= MAX_STYLES {
                    diag.warning(Warning::new(
                        "DocxMaxStyles",
                        format!("style definition limit ({MAX_STYLES}) exceeded, truncating"),
                    ));
                    crate::parser::skip_element(&mut reader)?;
                    continue;
                }

                let style_id = attr_value(&attributes, "styleId").unwrap_or("").to_string();
                let style_type = attr_value(&attributes, "type")
                    .unwrap_or("paragraph")
                    .to_string();

                if !style_id.is_empty() {
                    let style = parse_style_def(&mut reader, style_id.clone(), style_type, diag)?;
                    styles.insert(style_id, style);
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(StyleMap { styles })
}

/// Parse a single w:style element into a StyleDef.
fn parse_style_def(
    reader: &mut XmlReader<'_>,
    id: String,
    style_type: String,
    _diag: &Arc<dyn DiagnosticsSink>,
) -> Result<StyleDef> {
    let mut style = StyleDef {
        id,
        style_type,
        name: None,
        based_on: None,
        outline_level: None,
        bold: None,
        italic: None,
        font_name: None,
        font_size_pts: None,
    };

    let mut depth: usize = 1;
    let mut in_ppr = false;
    let mut in_rpr = false;

    loop {
        let event = reader.next_element().context("parsing w:style")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                depth += 1;
                if is_wml(namespace_uri.as_deref()) {
                    match local_name.as_ref() {
                        "name" => {
                            if let Some(val) = attr_value(&attributes, "val") {
                                style.name = Some(val.to_string());
                            }
                        }
                        "basedOn" => {
                            if let Some(val) = attr_value(&attributes, "val") {
                                style.based_on = Some(val.to_string());
                            }
                        }
                        "pPr" => in_ppr = true,
                        "rPr" => in_rpr = true,
                        "outlineLvl" if in_ppr => {
                            if let Some(val) = attr_value(&attributes, "val") {
                                if let Ok(level) = val.parse::<u8>() {
                                    style.outline_level = Some(level);
                                }
                            }
                        }
                        "b" if in_rpr => {
                            style.bold = Some(toggle_attr(attr_value(&attributes, "val")));
                        }
                        "i" if in_rpr => {
                            style.italic = Some(toggle_attr(attr_value(&attributes, "val")));
                        }
                        "rFonts" if in_rpr => {
                            let font = attr_value(&attributes, "ascii")
                                .or_else(|| attr_value(&attributes, "hAnsi"))
                                .or_else(|| attr_value(&attributes, "cs"))
                                .or_else(|| attr_value(&attributes, "eastAsia"));
                            if let Some(f) = font {
                                style.font_name = Some(f.to_string());
                            }
                        }
                        "sz" if in_rpr => {
                            if let Some(val) = attr_value(&attributes, "val") {
                                if let Ok(half_pts) = val.parse::<f64>() {
                                    style.font_size_pts = Some(half_pts / 2.0);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            XmlEvent::EndElement {
                local_name,
                namespace_uri,
                ..
            } => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
                if is_wml(namespace_uri.as_deref()) {
                    match local_name.as_ref() {
                        "pPr" => in_ppr = false,
                        "rPr" => in_rpr = false,
                        _ => {}
                    }
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::diagnostics::NullDiagnostics;

    fn null_diag() -> Arc<dyn DiagnosticsSink> {
        Arc::new(NullDiagnostics)
    }

    #[test]
    fn parse_heading_style() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="Heading1">
    <w:name w:val="heading 1"/>
    <w:pPr><w:outlineLvl w:val="0"/></w:pPr>
    <w:rPr><w:b/><w:sz w:val="32"/></w:rPr>
  </w:style>
</w:styles>"#;

        let map = parse_styles(xml, &null_diag()).unwrap();
        let style = map.get("Heading1").unwrap();
        assert_eq!(style.outline_level, Some(0));
        assert_eq!(style.bold, Some(true));
        assert_eq!(style.font_size_pts, Some(16.0));
        assert_eq!(map.resolve_heading_level("Heading1"), 1);
    }

    #[test]
    fn parse_based_on_chain() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="Heading1">
    <w:name w:val="heading 1"/>
    <w:pPr><w:outlineLvl w:val="0"/></w:pPr>
  </w:style>
  <w:style w:type="paragraph" w:styleId="CustomHeading">
    <w:name w:val="My Custom Heading"/>
    <w:basedOn w:val="Heading1"/>
  </w:style>
</w:styles>"#;

        let map = parse_styles(xml, &null_diag()).unwrap();
        // CustomHeading inherits outlineLvl from Heading1.
        assert_eq!(map.resolve_heading_level("CustomHeading"), 1);
    }

    #[test]
    fn heading_name_fallback() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="Heading3">
    <w:name w:val="heading 3"/>
  </w:style>
</w:styles>"#;

        let map = parse_styles(xml, &null_diag()).unwrap();
        // No outlineLvl, but name matches "heading 3".
        assert_eq!(map.resolve_heading_level("Heading3"), 3);
    }

    #[test]
    fn body_text_returns_zero() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="Normal">
    <w:name w:val="Normal"/>
  </w:style>
</w:styles>"#;

        let map = parse_styles(xml, &null_diag()).unwrap();
        assert_eq!(map.resolve_heading_level("Normal"), 0);
    }

    #[test]
    fn unknown_style_returns_zero() {
        let map = StyleMap::default();
        assert_eq!(map.resolve_heading_level("nonexistent"), 0);
    }

    #[test]
    fn based_on_outlinelvl_takes_precedence_over_name() {
        // A style named "heading 3" but inheriting outlineLvl=0 from parent
        // should resolve to the inherited outlineLvl (heading 1), not the name.
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="BaseHeading">
    <w:name w:val="Base Heading"/>
    <w:pPr><w:outlineLvl w:val="0"/></w:pPr>
  </w:style>
  <w:style w:type="paragraph" w:styleId="MisnamedH3">
    <w:name w:val="heading 3"/>
    <w:basedOn w:val="BaseHeading"/>
  </w:style>
</w:styles>"#;

        let map = parse_styles(xml, &null_diag()).unwrap();
        // Inherited outlineLvl=0 -> heading 1, not name-based "heading 3".
        assert_eq!(map.resolve_heading_level("MisnamedH3"), 1);
    }

    #[test]
    fn resolve_bold_inheritance() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="BoldParent">
    <w:name w:val="Bold Parent"/>
    <w:rPr><w:b/></w:rPr>
  </w:style>
  <w:style w:type="paragraph" w:styleId="Child">
    <w:name w:val="Child"/>
    <w:basedOn w:val="BoldParent"/>
  </w:style>
</w:styles>"#;

        let map = parse_styles(xml, &null_diag()).unwrap();
        assert_eq!(map.resolve_bold("BoldParent"), Some(true));
        assert_eq!(map.resolve_bold("Child"), Some(true));
        assert_eq!(map.resolve_italic("Child"), None);
    }
}

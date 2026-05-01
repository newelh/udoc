//! DOCX numbering.xml parser.
//!
//! Handles the three-level numbering indirection:
//! paragraph w:numPr/w:numId + w:ilvl -> numbering.xml w:num/w:abstractNumId
//! -> w:abstractNum/w:lvl. Determines list kind (ordered vs unordered) from
//! w:numFmt.

use std::collections::HashMap;
use std::sync::Arc;

use udoc_containers::xml::{attr_value, XmlEvent, XmlReader};
use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use crate::error::{Result, ResultExt};
use crate::parser::is_wml;

/// Maximum number of abstract numbering definitions (safety limit).
const MAX_ABSTRACT_NUMS: usize = 10_000;

/// Maximum number of concrete numbering definitions (safety limit).
const MAX_NUMS: usize = 10_000;

/// Maximum number of levels per abstract numbering (OOXML allows 9).
const MAX_LEVELS: usize = 9;

/// A level definition within an abstract numbering.
#[derive(Debug, Clone)]
pub(crate) struct LevelDef {
    /// Level index (0-8).
    pub ilvl: u8,
    /// Number format: "bullet", "decimal", "lowerRoman", etc.
    pub num_fmt: String,
    /// Level text pattern (e.g., "%1.", "%1.%2.").
    pub lvl_text: Option<String>,
    /// Start value for this level.
    pub start: u64,
}

/// An abstract numbering definition.
#[derive(Debug, Clone)]
struct AbstractNum {
    id: String,
    levels: Vec<LevelDef>,
}

/// A concrete numbering definition (w:num).
#[derive(Debug, Clone)]
struct NumDef {
    num_id: String,
    abstract_num_id: String,
    // Level overrides (w:lvlOverride) can change numFmt/start for a level.
    overrides: HashMap<u8, LevelOverride>,
}

/// A level override within a w:num.
#[derive(Debug, Clone)]
struct LevelOverride {
    start: Option<u64>,
    num_fmt: Option<String>,
}

/// Numbering definitions resolved from numbering.xml.
#[derive(Debug, Default)]
pub struct NumberingDefs {
    abstract_nums: HashMap<String, AbstractNum>,
    nums: HashMap<String, NumDef>,
}

/// The resolved list kind for a paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    Ordered,
    Unordered,
}

impl NumberingDefs {
    /// Resolve the list kind for a paragraph with the given numId and ilvl.
    /// Returns None if the numbering definition is not found.
    pub fn resolve_list_kind(&self, num_id: &str, ilvl: u8) -> Option<ListKind> {
        let num = self.nums.get(num_id)?;

        // Check for level override first.
        if let Some(ovr) = num.overrides.get(&ilvl) {
            if let Some(ref fmt) = ovr.num_fmt {
                return Some(num_fmt_to_kind(fmt));
            }
        }

        // Look up the abstract numbering.
        let abstract_num = self.abstract_nums.get(&num.abstract_num_id)?;
        let level = abstract_num.levels.iter().find(|l| l.ilvl == ilvl)?;
        Some(num_fmt_to_kind(&level.num_fmt))
    }

    /// Get the start value for a numbering level.
    pub fn resolve_start(&self, num_id: &str, ilvl: u8) -> u64 {
        if let Some(num) = self.nums.get(num_id) {
            if let Some(ovr) = num.overrides.get(&ilvl) {
                if let Some(start) = ovr.start {
                    return start;
                }
            }
            if let Some(abs) = self.abstract_nums.get(&num.abstract_num_id) {
                if let Some(level) = abs.levels.iter().find(|l| l.ilvl == ilvl) {
                    return level.start;
                }
            }
        }
        1
    }
}

/// Determine list kind from w:numFmt value.
fn num_fmt_to_kind(fmt: &str) -> ListKind {
    match fmt {
        "bullet" | "none" => ListKind::Unordered,
        _ => ListKind::Ordered,
    }
}

/// Parse numbering.xml into NumberingDefs.
pub(crate) fn parse_numbering(
    data: &[u8],
    diag: &Arc<dyn DiagnosticsSink>,
) -> Result<NumberingDefs> {
    let mut reader = XmlReader::new(data).context("initializing XML parser for numbering.xml")?;

    let mut abstract_nums: HashMap<String, AbstractNum> = HashMap::new();
    let mut nums: HashMap<String, NumDef> = HashMap::new();

    loop {
        let event = reader.next_element().context("parsing numbering.xml")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                if !is_wml(namespace_uri.as_deref()) {
                    continue;
                }

                match local_name.as_ref() {
                    "abstractNum" => {
                        if abstract_nums.len() >= MAX_ABSTRACT_NUMS {
                            diag.warning(Warning::new(
                                "DocxMaxAbstractNums",
                                format!(
                                    "abstract numbering limit ({}) exceeded",
                                    MAX_ABSTRACT_NUMS
                                ),
                            ));
                            crate::parser::skip_element(&mut reader)?;
                            continue;
                        }
                        if let Some(id) = attr_value(&attributes, "abstractNumId") {
                            let abs = parse_abstract_num(&mut reader, id.to_string(), diag)?;
                            abstract_nums.insert(abs.id.clone(), abs);
                        }
                    }
                    "num" => {
                        if nums.len() >= MAX_NUMS {
                            diag.warning(Warning::new(
                                "DocxMaxNums",
                                format!("numbering definition limit ({}) exceeded", MAX_NUMS),
                            ));
                            crate::parser::skip_element(&mut reader)?;
                            continue;
                        }
                        if let Some(num_id) = attr_value(&attributes, "numId") {
                            let num = parse_num(&mut reader, num_id.to_string(), diag)?;
                            nums.insert(num.num_id.clone(), num);
                        }
                    }
                    _ => {}
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(NumberingDefs {
        abstract_nums,
        nums,
    })
}

/// Parse a w:abstractNum element.
fn parse_abstract_num(
    reader: &mut XmlReader<'_>,
    id: String,
    _diag: &Arc<dyn DiagnosticsSink>,
) -> Result<AbstractNum> {
    let mut levels = Vec::new();
    let mut depth: usize = 1;

    loop {
        let event = reader.next_element().context("parsing w:abstractNum")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                depth += 1;
                if is_wml(namespace_uri.as_deref())
                    && local_name == "lvl"
                    && levels.len() < MAX_LEVELS
                {
                    if let Some(ilvl) = attr_value(&attributes, "ilvl") {
                        if let Ok(level_idx) = ilvl.parse::<u8>() {
                            let level = parse_level(reader, level_idx)?;
                            levels.push(level);
                            depth = depth.saturating_sub(1);
                        }
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

    Ok(AbstractNum { id, levels })
}

/// Parse a w:lvl element.
fn parse_level(reader: &mut XmlReader<'_>, ilvl: u8) -> Result<LevelDef> {
    let mut level = LevelDef {
        ilvl,
        num_fmt: "decimal".to_string(),
        lvl_text: None,
        start: 1,
    };

    let mut depth: usize = 1;

    loop {
        let event = reader.next_element().context("parsing w:lvl")?;

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
                        "start" => {
                            if let Some(val) = attr_value(&attributes, "val") {
                                if let Ok(n) = val.parse::<u64>() {
                                    level.start = n;
                                }
                            }
                        }
                        "numFmt" => {
                            if let Some(val) = attr_value(&attributes, "val") {
                                level.num_fmt = val.to_string();
                            }
                        }
                        "lvlText" => {
                            if let Some(val) = attr_value(&attributes, "val") {
                                level.lvl_text = Some(val.to_string());
                            }
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

    Ok(level)
}

/// Parse a w:num element.
fn parse_num(
    reader: &mut XmlReader<'_>,
    num_id: String,
    _diag: &Arc<dyn DiagnosticsSink>,
) -> Result<NumDef> {
    let mut abstract_num_id = String::new();
    let mut overrides = HashMap::new();
    let mut depth: usize = 1;

    loop {
        let event = reader.next_element().context("parsing w:num")?;

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
                        "abstractNumId" => {
                            if let Some(val) = attr_value(&attributes, "val") {
                                abstract_num_id = val.to_string();
                            }
                        }
                        "lvlOverride" => {
                            if let Some(ilvl_str) = attr_value(&attributes, "ilvl") {
                                if let Ok(ilvl) = ilvl_str.parse::<u8>() {
                                    let ovr = parse_level_override(reader)?;
                                    overrides.insert(ilvl, ovr);
                                    depth = depth.saturating_sub(1);
                                }
                            }
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

    Ok(NumDef {
        num_id,
        abstract_num_id,
        overrides,
    })
}

/// Parse a w:lvlOverride element.
fn parse_level_override(reader: &mut XmlReader<'_>) -> Result<LevelOverride> {
    let mut ovr = LevelOverride {
        start: None,
        num_fmt: None,
    };

    let mut depth: usize = 1;

    loop {
        let event = reader.next_element().context("parsing w:lvlOverride")?;

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
                        "startOverride" => {
                            if let Some(val) = attr_value(&attributes, "val") {
                                if let Ok(n) = val.parse::<u64>() {
                                    ovr.start = Some(n);
                                }
                            }
                        }
                        "numFmt" => {
                            if let Some(val) = attr_value(&attributes, "val") {
                                ovr.num_fmt = Some(val.to_string());
                            }
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

    Ok(ovr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::diagnostics::NullDiagnostics;

    fn null_diag() -> Arc<dyn DiagnosticsSink> {
        Arc::new(NullDiagnostics)
    }

    #[test]
    fn parse_bullet_list() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="bullet"/>
      <w:lvlText w:val=""/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

        let defs = parse_numbering(xml, &null_diag()).unwrap();
        assert_eq!(defs.resolve_list_kind("1", 0), Some(ListKind::Unordered));
    }

    #[test]
    fn parse_decimal_list() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="1">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="decimal"/>
      <w:lvlText w:val="%1."/>
    </w:lvl>
    <w:lvl w:ilvl="1">
      <w:start w:val="1"/>
      <w:numFmt w:val="lowerLetter"/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="2">
    <w:abstractNumId w:val="1"/>
  </w:num>
</w:numbering>"#;

        let defs = parse_numbering(xml, &null_diag()).unwrap();
        assert_eq!(defs.resolve_list_kind("2", 0), Some(ListKind::Ordered));
        assert_eq!(defs.resolve_list_kind("2", 1), Some(ListKind::Ordered));
        assert_eq!(defs.resolve_start("2", 0), 1);
    }

    #[test]
    fn unknown_num_id_returns_none() {
        let defs = NumberingDefs::default();
        assert_eq!(defs.resolve_list_kind("999", 0), None);
    }
}

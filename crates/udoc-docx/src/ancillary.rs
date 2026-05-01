//! DOCX ancillary content: headers, footers, footnotes, endnotes.
//!
//! Parses these parts via their relationship types from document.xml.rels.

use std::cell::Cell;
use std::sync::Arc;

use udoc_containers::opc::rel_types;
use udoc_containers::opc::OpcPackage;
use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use crate::parser::{parse_paragraphs_from_part, DocxContext, Endnote, Footnote, Paragraph};

/// Parse all header parts linked from the document.
pub(crate) fn parse_headers(
    pkg: &OpcPackage<'_>,
    doc_part: &str,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Vec<Vec<Paragraph>> {
    let rels = pkg.find_all_part_rels_by_type(doc_part, rel_types::HEADER);
    let mut headers = Vec::new();

    for rel in rels {
        let uri = pkg.resolve_uri(doc_part, &rel.target);
        match pkg.read_part(&uri) {
            Ok(data) => {
                let hyperlink_map = crate::parser::build_hyperlink_map(pkg, &uri);
                let ctx = DocxContext {
                    hyperlink_map: &hyperlink_map,
                    diag,
                    theme_color_warned: Cell::new(false),
                };
                match parse_paragraphs_from_part(&data, &ctx) {
                    Ok(paras) => headers.push(paras),
                    Err(e) => {
                        diag.warning(Warning::new(
                            "DocxHeaderParseError",
                            format!("error parsing header {}: {e}", rel.target),
                        ));
                    }
                }
            }
            Err(e) => {
                diag.warning(Warning::new(
                    "DocxMissingHeader",
                    format!("could not read header {}: {e}", rel.target),
                ));
            }
        }
    }

    headers
}

/// Parse all footer parts linked from the document.
pub(crate) fn parse_footers(
    pkg: &OpcPackage<'_>,
    doc_part: &str,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Vec<Vec<Paragraph>> {
    let rels = pkg.find_all_part_rels_by_type(doc_part, rel_types::FOOTER);
    let mut footers = Vec::new();

    for rel in rels {
        let uri = pkg.resolve_uri(doc_part, &rel.target);
        match pkg.read_part(&uri) {
            Ok(data) => {
                let hyperlink_map = crate::parser::build_hyperlink_map(pkg, &uri);
                let ctx = DocxContext {
                    hyperlink_map: &hyperlink_map,
                    diag,
                    theme_color_warned: Cell::new(false),
                };
                match parse_paragraphs_from_part(&data, &ctx) {
                    Ok(paras) => footers.push(paras),
                    Err(e) => {
                        diag.warning(Warning::new(
                            "DocxFooterParseError",
                            format!("error parsing footer {}: {e}", rel.target),
                        ));
                    }
                }
            }
            Err(e) => {
                diag.warning(Warning::new(
                    "DocxMissingFooter",
                    format!("could not read footer {}: {e}", rel.target),
                ));
            }
        }
    }

    footers
}

/// Parse footnotes.xml.
pub(crate) fn parse_footnotes(
    pkg: &OpcPackage<'_>,
    doc_part: &str,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Vec<Footnote> {
    let rel = match pkg.find_part_rel_by_type(doc_part, rel_types::FOOTNOTES) {
        Some(r) => r,
        None => return Vec::new(),
    };

    let uri = pkg.resolve_uri(doc_part, &rel.target);
    let data = match pkg.read_part(&uri) {
        Ok(d) => d,
        Err(e) => {
            diag.warning(Warning::new(
                "DocxMissingFootnotes",
                format!("could not read footnotes: {e}"),
            ));
            return Vec::new();
        }
    };

    let hyperlink_map = crate::parser::build_hyperlink_map(pkg, &uri);
    let ctx = DocxContext {
        hyperlink_map: &hyperlink_map,
        diag,
        theme_color_warned: Cell::new(false),
    };
    parse_note_part(&data, "footnote", &ctx)
        .into_iter()
        .map(|(id, paras)| Footnote {
            id,
            paragraphs: paras,
        })
        .collect()
}

/// Parse endnotes.xml.
pub(crate) fn parse_endnotes(
    pkg: &OpcPackage<'_>,
    doc_part: &str,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Vec<Endnote> {
    let rel = match pkg.find_part_rel_by_type(doc_part, rel_types::ENDNOTES) {
        Some(r) => r,
        None => return Vec::new(),
    };

    let uri = pkg.resolve_uri(doc_part, &rel.target);
    let data = match pkg.read_part(&uri) {
        Ok(d) => d,
        Err(e) => {
            diag.warning(Warning::new(
                "DocxMissingEndnotes",
                format!("could not read endnotes: {e}"),
            ));
            return Vec::new();
        }
    };

    let hyperlink_map = crate::parser::build_hyperlink_map(pkg, &uri);
    let ctx = DocxContext {
        hyperlink_map: &hyperlink_map,
        diag,
        theme_color_warned: Cell::new(false),
    };
    parse_note_part(&data, "endnote", &ctx)
        .into_iter()
        .map(|(id, paras)| Endnote {
            id,
            paragraphs: paras,
        })
        .collect()
}

/// Maximum number of notes we'll parse from a single part (security limit).
const MAX_NOTES: usize = 50_000;

/// Parse a footnotes/endnotes XML part.
/// Returns Vec<(id, paragraphs)>, skipping separator/continuation notes (id 0 and 1).
fn parse_note_part(
    data: &[u8],
    element_name: &str,
    ctx: &DocxContext<'_>,
) -> Vec<(String, Vec<Paragraph>)> {
    use udoc_containers::xml::ns;
    use udoc_containers::xml::{attr_value, XmlEvent, XmlReader};

    let mut reader = match XmlReader::new(data) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut notes = Vec::new();
    // Bookmarks in ancillary parts (footnotes, endnotes, headers, footers) are
    // collected but intentionally not wired to the document's relationships
    // overlay. These bookmarks only make sense within their local scope and
    // cannot be cross-referenced from the main document body.
    let mut ignored_bookmarks = Vec::new();

    loop {
        match reader.next_element() {
            Ok(XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            }) => {
                let is_wml = matches!(
                    namespace_uri.as_deref(),
                    Some(ns::WML) | Some(ns::WML_STRICT)
                );
                if is_wml && local_name == element_name {
                    let id = attr_value(&attributes, "id").unwrap_or("0").to_string();

                    // Skip separator (id=0) and continuation separator (id=1)
                    // per OOXML spec 17.11.3 and 17.11.20.
                    if id == "0" || id == "1" {
                        // Consume the separator note's subtree.
                        let _ = crate::parser::skip_element(&mut reader);
                        continue;
                    }

                    // Parse paragraphs inside this note until the end element.
                    let mut paras = Vec::new();
                    let mut depth: usize = 1;
                    loop {
                        match reader.next_element() {
                            Ok(XmlEvent::StartElement {
                                local_name: inner_name,
                                namespace_uri: inner_ns,
                                attributes: inner_attrs,
                                ..
                            }) => {
                                depth += 1;
                                let inner_wml = matches!(
                                    inner_ns.as_deref(),
                                    Some(ns::WML) | Some(ns::WML_STRICT)
                                );
                                if inner_wml && inner_name == "p" {
                                    if let Ok(para) = crate::parser::parse_paragraph(
                                        &mut reader,
                                        &inner_attrs,
                                        ctx,
                                        &mut ignored_bookmarks,
                                    ) {
                                        paras.push(para);
                                    }
                                    depth = depth.saturating_sub(1);
                                }
                            }
                            Ok(XmlEvent::EndElement { .. }) => {
                                depth = depth.saturating_sub(1);
                                if depth == 0 {
                                    break;
                                }
                            }
                            Ok(XmlEvent::Eof) => break,
                            Err(_) => break,
                            _ => {}
                        }
                    }

                    notes.push((id, paras));
                    if notes.len() >= MAX_NOTES {
                        ctx.diag.warning(Warning::new(
                            "DocxNoteLimitReached",
                            format!("stopped parsing {element_name}s at {MAX_NOTES} limit"),
                        ));
                        return notes;
                    }
                }
            }
            Ok(XmlEvent::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }

    notes
}

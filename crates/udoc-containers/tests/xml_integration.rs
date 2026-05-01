//! Integration tests for the XML pull-parser.
//!
//! These tests exercise realistic OOXML and ODF document fragments including
//! namespace declarations, prefix resolution, attributes, and text nodes.

use udoc_containers::xml::{ns, Attribute, XmlEvent, XmlReader};

// --------------------------------------------------------------------------
// Helpers
// --------------------------------------------------------------------------

/// Collect all non-whitespace events from a byte slice, excluding Eof.
fn collect_events(xml: &[u8]) -> Vec<XmlEvent<'_>> {
    let mut reader = XmlReader::new(xml).unwrap();
    let mut events = Vec::new();
    loop {
        let ev = reader.next_element().unwrap();
        if matches!(ev, XmlEvent::Eof) {
            break;
        }
        events.push(ev);
    }
    events
}

/// Assert that `event` is a StartElement with the given local name and namespace URI.
fn assert_start(event: &XmlEvent, expected_local: &str, expected_ns: Option<&str>) {
    match event {
        XmlEvent::StartElement {
            local_name,
            namespace_uri,
            ..
        } => {
            assert_eq!(
                local_name, expected_local,
                "local_name mismatch: expected {expected_local}, got {local_name}"
            );
            assert_eq!(
                namespace_uri.as_deref(),
                expected_ns,
                "namespace_uri mismatch for <{local_name}>: expected {expected_ns:?}"
            );
        }
        other => panic!("expected StartElement for <{expected_local}>, got: {other:?}"),
    }
}

/// Assert that `event` is an EndElement with the given local name.
fn assert_end(event: &XmlEvent, expected_local: &str) {
    match event {
        XmlEvent::EndElement { local_name, .. } => {
            assert_eq!(local_name, expected_local);
        }
        other => panic!("expected EndElement for </{expected_local}>, got: {other:?}"),
    }
}

/// Assert that `event` is a Text node with the given value.
fn assert_text(event: &XmlEvent, expected: &str) {
    match event {
        XmlEvent::Text(s) => assert_eq!(
            s, expected,
            "text mismatch: expected {expected:?}, got {s:?}"
        ),
        other => panic!("expected Text({expected:?}), got: {other:?}"),
    }
}

/// Find the first attribute with the given local name in a slice of Attributes.
fn find_attr<'a>(attrs: &'a [Attribute<'a>], local_name: &str) -> Option<&'a Attribute<'a>> {
    attrs.iter().find(|a| a.local_name == local_name)
}

// --------------------------------------------------------------------------
// OOXML w:document fragment
// --------------------------------------------------------------------------

/// A realistic OOXML `<w:document>` fragment with namespace declarations on the
/// root element and nested `<w:body>/<w:p>/<w:r>/<w:t>` structure.
///
/// Verified assertions:
///   - Every w: element resolves to the WML namespace URI.
///   - The `w:rsidR` attribute on `<w:p>` resolves to WML.
///   - The `w:space` attribute on `<w:t>` resolves to WML.
///   - The `w:val` attribute on `<w:pStyle>` resolves to WML.
///   - Text content "Hello, World!" is decoded without modification.
#[test]
fn ooxml_word_document_namespace_resolution() {
    let xml = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
        "<w:document",
        "  xmlns:wpc=\"http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas\"",
        "  xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"",
        "  xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"",
        "  xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\">",
        "  <w:body>",
        "    <w:p w:rsidR=\"00A1B2C3\">",
        "      <w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr>",
        "      <w:r><w:t w:space=\"preserve\">Hello, World!</w:t></w:r>",
        "    </w:p>",
        "    <w:p><w:r><w:t>Second paragraph.</w:t></w:r></w:p>",
        "  </w:body>",
        "</w:document>",
    );

    let mut reader = XmlReader::new(xml.as_bytes()).unwrap();

    // <w:document>
    let ev = reader.next_element().unwrap();
    assert_start(&ev, "document", Some(ns::WML));

    // <w:body>
    let ev = reader.next_element().unwrap();
    assert_start(&ev, "body", Some(ns::WML));

    // <w:p w:rsidR="00A1B2C3">
    let ev = reader.next_element().unwrap();
    match &ev {
        XmlEvent::StartElement {
            local_name,
            namespace_uri,
            attributes,
            ..
        } => {
            assert_eq!(local_name, "p");
            assert_eq!(namespace_uri.as_deref(), Some(ns::WML));

            let rsid = find_attr(attributes, "rsidR")
                .expect("w:rsidR attribute should be present on <w:p>");
            assert_eq!(rsid.prefix, "w");
            assert_eq!(rsid.namespace_uri.as_deref(), Some(ns::WML));
            assert_eq!(rsid.value, "00A1B2C3");
        }
        other => panic!("expected StartElement for <w:p>, got: {other:?}"),
    }

    // <w:pPr>
    assert_start(&reader.next_element().unwrap(), "pPr", Some(ns::WML));

    // <w:pStyle w:val="Heading1"/>
    let ev = reader.next_element().unwrap();
    match &ev {
        XmlEvent::StartElement {
            local_name,
            namespace_uri,
            attributes,
            ..
        } => {
            assert_eq!(local_name, "pStyle");
            assert_eq!(namespace_uri.as_deref(), Some(ns::WML));

            let val = find_attr(attributes, "val").expect("w:val attribute on <w:pStyle>");
            assert_eq!(val.prefix, "w");
            assert_eq!(val.namespace_uri.as_deref(), Some(ns::WML));
            assert_eq!(val.value, "Heading1");
        }
        other => panic!("expected StartElement for <w:pStyle/>, got: {other:?}"),
    }

    // </w:pStyle> (self-closing tag emits an EndElement)
    assert_end(&reader.next_element().unwrap(), "pStyle");

    // </w:pPr>
    assert_end(&reader.next_element().unwrap(), "pPr");

    // <w:r>
    assert_start(&reader.next_element().unwrap(), "r", Some(ns::WML));

    // <w:t w:space="preserve">
    let ev = reader.next_element().unwrap();
    match &ev {
        XmlEvent::StartElement {
            local_name,
            namespace_uri,
            attributes,
            ..
        } => {
            assert_eq!(local_name, "t");
            assert_eq!(namespace_uri.as_deref(), Some(ns::WML));

            let space = find_attr(attributes, "space").expect("w:space attribute on <w:t>");
            assert_eq!(space.prefix, "w");
            assert_eq!(space.namespace_uri.as_deref(), Some(ns::WML));
            assert_eq!(space.value, "preserve");
        }
        other => panic!("expected StartElement for <w:t>, got: {other:?}"),
    }

    // "Hello, World!"
    assert_text(&reader.next_element().unwrap(), "Hello, World!");

    // </w:t>, </w:r>, </w:p>
    assert_end(&reader.next_element().unwrap(), "t");
    assert_end(&reader.next_element().unwrap(), "r");
    assert_end(&reader.next_element().unwrap(), "p");

    // Second <w:p>
    assert_start(&reader.next_element().unwrap(), "p", Some(ns::WML));
    assert_start(&reader.next_element().unwrap(), "r", Some(ns::WML));
    assert_start(&reader.next_element().unwrap(), "t", Some(ns::WML));
    assert_text(&reader.next_element().unwrap(), "Second paragraph.");
    assert_end(&reader.next_element().unwrap(), "t");
    assert_end(&reader.next_element().unwrap(), "r");
    assert_end(&reader.next_element().unwrap(), "p");

    // </w:body>, </w:document>
    assert_end(&reader.next_element().unwrap(), "body");
    assert_end(&reader.next_element().unwrap(), "document");

    assert!(matches!(reader.next_element().unwrap(), XmlEvent::Eof));
}

/// Multiple namespace prefixes declared on the root (WML + R + DrawingML).
/// A prefixed attribute `r:id` on `<w:r>` must resolve to the R namespace.
/// A `<a:graphic>` element must resolve to the DrawingML namespace.
#[test]
fn ooxml_multiple_namespace_prefixes_on_root() {
    let xml = concat!(
        "<w:document",
        "  xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"",
        "  xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"",
        "  xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\">",
        "  <w:body>",
        "    <w:p><w:r r:id=\"rId1\"><w:t>Text</w:t></w:r></w:p>",
        "    <a:graphic><a:graphicData/></a:graphic>",
        "  </w:body>",
        "</w:document>",
    );

    let events = collect_events(xml.as_bytes());

    // <w:r> must be WML; its r:id attribute must resolve to the R namespace.
    let run_ev = events
        .iter()
        .find(|ev| {
            if let XmlEvent::StartElement {
                local_name,
                namespace_uri: Some(u),
                ..
            } = ev
            {
                local_name == "r" && u.as_ref() == ns::WML
            } else {
                false
            }
        })
        .expect("<w:r> element not found");

    match run_ev {
        XmlEvent::StartElement { attributes, .. } => {
            let r_id = find_attr(attributes, "id").expect("r:id attribute on <w:r>");
            assert_eq!(r_id.prefix, "r");
            assert_eq!(
                r_id.namespace_uri.as_deref(),
                Some("http://schemas.openxmlformats.org/officeDocument/2006/relationships")
            );
            assert_eq!(r_id.value, "rId1");
        }
        _ => unreachable!(),
    }

    // <a:graphic> must resolve to DrawingML.
    let graphic_ev = events
        .iter()
        .find(
            |ev| matches!(ev, XmlEvent::StartElement { local_name, .. } if local_name == "graphic"),
        )
        .expect("<a:graphic> not found");
    assert_start(graphic_ev, "graphic", Some(ns::DRAWINGML));

    // <a:graphicData/> also resolves to DrawingML.
    let gd_ev = events
        .iter()
        .find(|ev| {
            matches!(ev, XmlEvent::StartElement { local_name, .. } if local_name == "graphicData")
        })
        .expect("<a:graphicData/> not found");
    assert_start(gd_ev, "graphicData", Some(ns::DRAWINGML));
}

/// Text inside a Word run with XML entity references is decoded correctly.
#[test]
fn ooxml_entity_references_in_run_text() {
    let xml = concat!(
        "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">",
        "<w:body><w:p><w:r>",
        "<w:t>Price: &lt;10 &amp; &gt;5 &quot;sale&quot; it&apos;s</w:t>",
        "</w:r></w:p></w:body></w:document>",
    );

    let mut reader = XmlReader::new(xml.as_bytes()).unwrap();
    loop {
        match reader.next_element().unwrap() {
            XmlEvent::Text(s) => {
                assert_eq!(s, "Price: <10 & >5 \"sale\" it's");
                return;
            }
            XmlEvent::Eof => panic!("reached EOF before finding text node"),
            _ => {}
        }
    }
}

// --------------------------------------------------------------------------
// ODF fragment
// --------------------------------------------------------------------------

/// ODF office:document-content with text:p and table:table namespaces.
/// Every element and prefixed attribute must resolve to the correct ODF URI.
#[test]
fn odf_document_namespace_resolution() {
    let xml = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
        "<office:document-content",
        "  xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\"",
        "  xmlns:text=\"urn:oasis:names:tc:opendocument:xmlns:text:1.0\"",
        "  xmlns:table=\"urn:oasis:names:tc:opendocument:xmlns:table:1.0\"",
        "  xmlns:style=\"urn:oasis:names:tc:opendocument:xmlns:style:1.0\"",
        "  office:version=\"1.3\">",
        "  <office:body>",
        "    <office:text>",
        "      <text:p text:style-name=\"Text_20_Body\">Hello ODF!</text:p>",
        "      <table:table table:name=\"Table1\">",
        "        <table:table-row>",
        "          <table:table-cell><text:p>Cell</text:p></table:table-cell>",
        "        </table:table-row>",
        "      </table:table>",
        "    </office:text>",
        "  </office:body>",
        "</office:document-content>",
    );

    let mut reader = XmlReader::new(xml.as_bytes()).unwrap();

    // <office:document-content office:version="1.3">
    let ev = reader.next_element().unwrap();
    match &ev {
        XmlEvent::StartElement {
            local_name,
            namespace_uri,
            attributes,
            ..
        } => {
            assert_eq!(local_name, "document-content");
            assert_eq!(namespace_uri.as_deref(), Some(ns::OFFICE));

            let version = find_attr(attributes, "version")
                .expect("office:version attribute on <office:document-content>");
            assert_eq!(version.prefix, "office");
            assert_eq!(version.namespace_uri.as_deref(), Some(ns::OFFICE));
            assert_eq!(version.value, "1.3");
        }
        other => panic!("expected StartElement, got: {other:?}"),
    }

    // <office:body>
    assert_start(&reader.next_element().unwrap(), "body", Some(ns::OFFICE));

    // <office:text>
    assert_start(&reader.next_element().unwrap(), "text", Some(ns::OFFICE));

    // <text:p text:style-name="Text_20_Body">
    let ev = reader.next_element().unwrap();
    match &ev {
        XmlEvent::StartElement {
            local_name,
            namespace_uri,
            attributes,
            ..
        } => {
            assert_eq!(local_name, "p");
            assert_eq!(namespace_uri.as_deref(), Some(ns::TEXT));

            let style =
                find_attr(attributes, "style-name").expect("text:style-name attribute on <text:p>");
            assert_eq!(style.prefix, "text");
            assert_eq!(style.namespace_uri.as_deref(), Some(ns::TEXT));
            assert_eq!(style.value, "Text_20_Body");
        }
        other => panic!("expected StartElement for <text:p>, got: {other:?}"),
    }

    assert_text(&reader.next_element().unwrap(), "Hello ODF!");
    assert_end(&reader.next_element().unwrap(), "p");

    // <table:table table:name="Table1">
    let ev = reader.next_element().unwrap();
    match &ev {
        XmlEvent::StartElement {
            local_name,
            namespace_uri,
            attributes,
            ..
        } => {
            assert_eq!(local_name, "table");
            assert_eq!(namespace_uri.as_deref(), Some(ns::TABLE));

            let name =
                find_attr(attributes, "name").expect("table:name attribute on <table:table>");
            assert_eq!(name.prefix, "table");
            assert_eq!(name.namespace_uri.as_deref(), Some(ns::TABLE));
            assert_eq!(name.value, "Table1");
        }
        other => panic!("expected StartElement for <table:table>, got: {other:?}"),
    }

    // <table:table-row>
    assert_start(
        &reader.next_element().unwrap(),
        "table-row",
        Some(ns::TABLE),
    );

    // <table:table-cell>
    assert_start(
        &reader.next_element().unwrap(),
        "table-cell",
        Some(ns::TABLE),
    );

    // <text:p>Cell</text:p>
    assert_start(&reader.next_element().unwrap(), "p", Some(ns::TEXT));
    assert_text(&reader.next_element().unwrap(), "Cell");
    assert_end(&reader.next_element().unwrap(), "p");

    // </table:table-cell>, </table:table-row>, </table:table>
    assert_end(&reader.next_element().unwrap(), "table-cell");
    assert_end(&reader.next_element().unwrap(), "table-row");
    assert_end(&reader.next_element().unwrap(), "table");

    // </office:text>, </office:body>, </office:document-content>
    assert_end(&reader.next_element().unwrap(), "text");
    assert_end(&reader.next_element().unwrap(), "body");
    assert_end(&reader.next_element().unwrap(), "document-content");

    assert!(matches!(reader.next_element().unwrap(), XmlEvent::Eof));
}

/// ODF style definitions use style:, fo:, and office: prefixes simultaneously.
/// All attributes on <style:text-properties> must resolve to the fo: namespace.
#[test]
fn odf_style_fragment_multiple_prefixes() {
    let xml = concat!(
        "<office:automatic-styles",
        "  xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\"",
        "  xmlns:style=\"urn:oasis:names:tc:opendocument:xmlns:style:1.0\"",
        "  xmlns:fo=\"urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0\">",
        "  <style:style style:name=\"P1\" style:family=\"paragraph\">",
        "    <style:text-properties fo:font-size=\"12pt\" fo:color=\"#000000\"/>",
        "  </style:style>",
        "</office:automatic-styles>",
    );

    let events = collect_events(xml.as_bytes());

    // <office:automatic-styles>
    assert_start(&events[0], "automatic-styles", Some(ns::OFFICE));

    // <style:style style:name="P1" style:family="paragraph">
    match &events[1] {
        XmlEvent::StartElement {
            local_name,
            namespace_uri,
            attributes,
            ..
        } => {
            assert_eq!(local_name, "style");
            assert_eq!(namespace_uri.as_deref(), Some(ns::STYLE));

            let name = find_attr(attributes, "name").expect("style:name");
            assert_eq!(name.prefix, "style");
            assert_eq!(name.namespace_uri.as_deref(), Some(ns::STYLE));
            assert_eq!(name.value, "P1");

            let family = find_attr(attributes, "family").expect("style:family");
            assert_eq!(family.prefix, "style");
            assert_eq!(family.namespace_uri.as_deref(), Some(ns::STYLE));
            assert_eq!(family.value, "paragraph");
        }
        other => panic!("expected StartElement for <style:style>, got: {other:?}"),
    }

    // <style:text-properties fo:font-size="12pt" fo:color="#000000"/>
    match &events[2] {
        XmlEvent::StartElement {
            local_name,
            namespace_uri,
            attributes,
            ..
        } => {
            assert_eq!(local_name, "text-properties");
            assert_eq!(namespace_uri.as_deref(), Some(ns::STYLE));

            let font_size = find_attr(attributes, "font-size").expect("fo:font-size");
            assert_eq!(font_size.prefix, "fo");
            assert_eq!(font_size.namespace_uri.as_deref(), Some(ns::FO));
            assert_eq!(font_size.value, "12pt");

            let color = find_attr(attributes, "color").expect("fo:color");
            assert_eq!(color.prefix, "fo");
            assert_eq!(color.namespace_uri.as_deref(), Some(ns::FO));
            assert_eq!(color.value, "#000000");
        }
        other => panic!("expected StartElement for <style:text-properties/>, got: {other:?}"),
    }

    // </style:text-properties> (self-closing)
    assert_end(&events[3], "text-properties");

    // </style:style>
    assert_end(&events[4], "style");

    // </office:automatic-styles>
    assert_end(&events[5], "automatic-styles");
}

/// Namespace scope isolation: a prefix declared on one child element must not
/// be visible on its siblings. This ensures the namespace stack is popped
/// correctly when EndElement is processed.
#[test]
fn namespace_scope_isolation_across_siblings() {
    // ext: is declared only inside <w:r>; the sibling <w:bookmarkStart> must
    // NOT see it. Its w: attributes must resolve to WML.
    let xml = concat!(
        "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">",
        "  <w:body><w:p>",
        "    <w:r xmlns:ext=\"http://example.com/ext\"><w:t>Hello</w:t></w:r>",
        "    <w:bookmarkStart w:id=\"0\" w:name=\"b1\"/>",
        "  </w:p></w:body>",
        "</w:document>",
    );

    let events = collect_events(xml.as_bytes());

    // <w:r> resolves to WML.
    let run_ev = events
        .iter()
        .find(|ev| {
            if let XmlEvent::StartElement {
                local_name,
                namespace_uri: Some(u),
                ..
            } = ev
            {
                local_name == "r" && u.as_ref() == ns::WML
            } else {
                false
            }
        })
        .expect("<w:r> not found");
    assert_start(run_ev, "r", Some(ns::WML));

    // <w:bookmarkStart> must resolve to WML; attributes must resolve to WML.
    let bm_ev = events
        .iter()
        .find(|ev| {
            matches!(ev, XmlEvent::StartElement { local_name, .. } if local_name == "bookmarkStart")
        })
        .expect("<w:bookmarkStart/> not found");

    match bm_ev {
        XmlEvent::StartElement {
            namespace_uri,
            attributes,
            ..
        } => {
            assert_eq!(namespace_uri.as_deref(), Some(ns::WML));

            let id_attr = find_attr(attributes, "id").expect("w:id attribute on <w:bookmarkStart>");
            assert_eq!(id_attr.prefix, "w");
            assert_eq!(id_attr.namespace_uri.as_deref(), Some(ns::WML));

            let name_attr =
                find_attr(attributes, "name").expect("w:name attribute on <w:bookmarkStart>");
            assert_eq!(name_attr.prefix, "w");
            assert_eq!(name_attr.namespace_uri.as_deref(), Some(ns::WML));
        }
        _ => unreachable!(),
    }
}

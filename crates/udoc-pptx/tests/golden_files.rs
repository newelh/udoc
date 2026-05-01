//! Golden file tests for PPTX text extraction.
//!
//! Run with `BLESS=1 cargo test -p udoc-pptx --test golden_files` to update expected files.

use std::path::PathBuf;

use udoc_containers::test_util::{
    build_stored_zip, PPTX_CONTENT_TYPES_1SLIDE, PPTX_CONTENT_TYPES_2SLIDES,
    PPTX_CONTENT_TYPES_NOTES, PPTX_PACKAGE_RELS, PPTX_PRESENTATION_1SLIDE,
    PPTX_PRESENTATION_2SLIDES, PPTX_PRES_RELS_1SLIDE, PPTX_PRES_RELS_2SLIDES,
    PPTX_SLIDE_RELS_EMPTY, PPTX_SLIDE_RELS_WITH_NOTES,
};
use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_core::test_harness::assert_golden;
use udoc_pptx::PptxDocument;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

// ---------------------------------------------------------------------------
// PPTX builders
// ---------------------------------------------------------------------------

fn build_basic_pptx() -> Vec<u8> {
    let slide1 = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:cSld>
    <p:spTree>
      <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr/>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="TextBox 1"/>
          <p:cNvSpPr/>
          <p:nvPr/>
        </p:nvSpPr>
        <p:spPr>
          <a:xfrm><a:off x="457200" y="274638"/><a:ext cx="8229600" cy="1143000"/></a:xfrm>
        </p:spPr>
        <p:txBody>
          <a:bodyPr/>
          <a:p><a:r><a:t>Hello World</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", PPTX_CONTENT_TYPES_1SLIDE),
        ("_rels/.rels", PPTX_PACKAGE_RELS),
        ("ppt/presentation.xml", PPTX_PRESENTATION_1SLIDE),
        ("ppt/_rels/presentation.xml.rels", PPTX_PRES_RELS_1SLIDE),
        ("ppt/slides/slide1.xml", slide1),
        ("ppt/slides/_rels/slide1.xml.rels", PPTX_SLIDE_RELS_EMPTY),
    ])
}

fn build_multi_slide_pptx() -> Vec<u8> {
    let slide1 = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr/>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Title 1"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="title"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr>
          <a:xfrm><a:off x="457200" y="274638"/><a:ext cx="8229600" cy="1143000"/></a:xfrm>
        </p:spPr>
        <p:txBody>
          <a:bodyPr/>
          <a:p><a:r><a:t>First Slide Title</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="3" name="Body 1"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="body" idx="1"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr>
          <a:xfrm><a:off x="457200" y="1600200"/><a:ext cx="8229600" cy="4525963"/></a:xfrm>
        </p:spPr>
        <p:txBody>
          <a:bodyPr/>
          <a:p><a:r><a:t>Content on slide one</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

    let slide2 = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr/>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Title 2"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="title"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr>
          <a:xfrm><a:off x="457200" y="274638"/><a:ext cx="8229600" cy="1143000"/></a:xfrm>
        </p:spPr>
        <p:txBody>
          <a:bodyPr/>
          <a:p><a:r><a:t>Second Slide Title</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="3" name="Body 2"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="body" idx="1"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr>
          <a:xfrm><a:off x="457200" y="1600200"/><a:ext cx="8229600" cy="4525963"/></a:xfrm>
        </p:spPr>
        <p:txBody>
          <a:bodyPr/>
          <a:p><a:r><a:t>Content on slide two</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", PPTX_CONTENT_TYPES_2SLIDES),
        ("_rels/.rels", PPTX_PACKAGE_RELS),
        ("ppt/presentation.xml", PPTX_PRESENTATION_2SLIDES),
        ("ppt/_rels/presentation.xml.rels", PPTX_PRES_RELS_2SLIDES),
        ("ppt/slides/slide1.xml", slide1),
        ("ppt/slides/_rels/slide1.xml.rels", PPTX_SLIDE_RELS_EMPTY),
        ("ppt/slides/slide2.xml", slide2),
        ("ppt/slides/_rels/slide2.xml.rels", PPTX_SLIDE_RELS_EMPTY),
    ])
}

fn build_table_pptx() -> Vec<u8> {
    let slide1 = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr/>
      <p:graphicFrame>
        <p:nvGraphicFramePr>
          <p:cNvPr id="2" name="Table 1"/>
          <p:cNvGraphicFramePr/>
          <p:nvPr/>
        </p:nvGraphicFramePr>
        <p:xfrm><a:off x="457200" y="274638"/><a:ext cx="8229600" cy="2000000"/></p:xfrm>
        <a:graphic>
          <a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table">
            <a:tbl>
              <a:tblGrid>
                <a:gridCol w="4114800"/>
                <a:gridCol w="4114800"/>
              </a:tblGrid>
              <a:tr h="500000">
                <a:tc>
                  <a:txBody><a:bodyPr/><a:p><a:r><a:t>Header A</a:t></a:r></a:p></a:txBody>
                  <a:tcPr/>
                </a:tc>
                <a:tc>
                  <a:txBody><a:bodyPr/><a:p><a:r><a:t>Header B</a:t></a:r></a:p></a:txBody>
                  <a:tcPr/>
                </a:tc>
              </a:tr>
              <a:tr h="500000">
                <a:tc>
                  <a:txBody><a:bodyPr/><a:p><a:r><a:t>Cell 1</a:t></a:r></a:p></a:txBody>
                  <a:tcPr/>
                </a:tc>
                <a:tc>
                  <a:txBody><a:bodyPr/><a:p><a:r><a:t>Cell 2</a:t></a:r></a:p></a:txBody>
                  <a:tcPr/>
                </a:tc>
              </a:tr>
            </a:tbl>
          </a:graphicData>
        </a:graphic>
      </p:graphicFrame>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", PPTX_CONTENT_TYPES_1SLIDE),
        ("_rels/.rels", PPTX_PACKAGE_RELS),
        ("ppt/presentation.xml", PPTX_PRESENTATION_1SLIDE),
        ("ppt/_rels/presentation.xml.rels", PPTX_PRES_RELS_1SLIDE),
        ("ppt/slides/slide1.xml", slide1),
        ("ppt/slides/_rels/slide1.xml.rels", PPTX_SLIDE_RELS_EMPTY),
    ])
}

fn build_notes_pptx() -> Vec<u8> {
    let slide1 = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr/>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Title 1"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="title"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr>
          <a:xfrm><a:off x="457200" y="274638"/><a:ext cx="8229600" cy="1143000"/></a:xfrm>
        </p:spPr>
        <p:txBody>
          <a:bodyPr/>
          <a:p><a:r><a:t>Slide With Notes</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

    let notes1 = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:notes xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
         xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr/>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Slide Image"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="sldImg"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
      </p:sp>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="3" name="Notes Placeholder"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="body" idx="1"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:bodyPr/>
          <a:p><a:r><a:t>These are the speaker notes</a:t></a:r></a:p>
          <a:p><a:r><a:t>Second line of notes</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:notes>"#;

    build_stored_zip(&[
        ("[Content_Types].xml", PPTX_CONTENT_TYPES_NOTES),
        ("_rels/.rels", PPTX_PACKAGE_RELS),
        ("ppt/presentation.xml", PPTX_PRESENTATION_1SLIDE),
        ("ppt/_rels/presentation.xml.rels", PPTX_PRES_RELS_1SLIDE),
        ("ppt/slides/slide1.xml", slide1),
        (
            "ppt/slides/_rels/slide1.xml.rels",
            PPTX_SLIDE_RELS_WITH_NOTES,
        ),
        ("ppt/notesSlides/notesSlide1.xml", notes1),
    ])
}

// ---------------------------------------------------------------------------
// Golden tests -- all use from_bytes() to avoid file I/O race conditions
// ---------------------------------------------------------------------------

#[test]
fn golden_basic_text() {
    let data = build_basic_pptx();
    let mut doc = PptxDocument::from_bytes(&data).expect("open");
    assert_eq!(doc.page_count(), 1);
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text");
    assert_golden("basic_text", &text, &golden_dir());
}

#[test]
fn golden_multi_slide_text() {
    let data = build_multi_slide_pptx();
    let mut doc = PptxDocument::from_bytes(&data).expect("open");
    assert_eq!(doc.page_count(), 2);

    let mut page0 = doc.page(0).expect("page 0");
    let text0 = page0.text().expect("text");
    assert_golden("multi_slide_page0", &text0, &golden_dir());

    let mut page1 = doc.page(1).expect("page 1");
    let text1 = page1.text().expect("text");
    assert_golden("multi_slide_page1", &text1, &golden_dir());
}

#[test]
fn golden_table_text() {
    let data = build_table_pptx();
    let mut doc = PptxDocument::from_bytes(&data).expect("open");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text");
    assert_golden("table_text", &text, &golden_dir());

    // Also verify the structured table extraction
    let mut page = doc.page(0).expect("page 0");
    let tables = page.tables().expect("tables");
    assert_eq!(tables.len(), 1, "should have exactly 1 table");
    let table = &tables[0];
    assert_eq!(table.rows.len(), 2, "table should have 2 rows");
    assert_eq!(table.rows[0].cells.len(), 2, "row 0 should have 2 cells");
    assert_eq!(table.rows[0].cells[0].text, "Header A");
    assert_eq!(table.rows[0].cells[1].text, "Header B");
    assert_eq!(table.rows[1].cells[0].text, "Cell 1");
    assert_eq!(table.rows[1].cells[1].text, "Cell 2");
}

#[test]
fn golden_notes_text() {
    let data = build_notes_pptx();
    let mut doc = PptxDocument::from_bytes(&data).expect("open");

    // Slide text
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text");
    assert_golden("notes_slide_text", &text, &golden_dir());

    // Speaker notes
    let notes = doc.notes(0).expect("should have notes");
    assert_golden("notes_speaker", &notes, &golden_dir());
}

#[test]
fn golden_basic_metadata() {
    let data = build_basic_pptx();
    let doc = PptxDocument::from_bytes(&data).expect("open");
    let meta = doc.metadata();

    let output = format!("page_count: {}", meta.page_count);
    assert_golden("basic_metadata", &output, &golden_dir());
}

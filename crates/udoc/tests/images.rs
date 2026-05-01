//! Cross-format image integration tests.
//!
//! Verifies that image extraction works through the full udoc pipeline.
//! PPTX is the simplest format for synthetic image embedding (image data
//! is a separate ZIP entry referenced by slide rels).

use udoc_containers::test_util::{
    build_stored_zip, PPTX_PACKAGE_RELS, PPTX_PRESENTATION_1SLIDE, PPTX_PRES_RELS_1SLIDE,
};

/// Minimal valid 1x1 red PNG (67 bytes). Pre-computed to avoid pulling in
/// a PNG encoder dependency.
fn minimal_png() -> Vec<u8> {
    let mut png = Vec::new();
    // PNG signature
    png.extend_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);

    // IHDR chunk: 1x1, 8-bit RGB
    let ihdr_data: [u8; 13] = [
        0x00, 0x00, 0x00, 0x01, // width = 1
        0x00, 0x00, 0x00, 0x01, // height = 1
        0x08, // bit depth = 8
        0x02, // color type = RGB
        0x00, // compression
        0x00, // filter
        0x00, // interlace
    ];
    let ihdr_crc = crc32_png(b"IHDR", &ihdr_data);
    png.extend_from_slice(&(13u32).to_be_bytes()); // length
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&ihdr_data);
    png.extend_from_slice(&ihdr_crc.to_be_bytes());

    // IDAT chunk: deflate-compressed scanline (filter byte 0 + RGB red pixel)
    // Raw data: [0x00, 0xFF, 0x00, 0x00] (filter=none, R=255, G=0, B=0)
    // Zlib wrapper: 0x78 0x01 (low compression), then deflate, then adler32
    let raw_scanline = [0x00u8, 0xFF, 0x00, 0x00];
    let idat_payload = zlib_compress(&raw_scanline);
    let idat_crc = crc32_png(b"IDAT", &idat_payload);
    png.extend_from_slice(&(idat_payload.len() as u32).to_be_bytes());
    png.extend_from_slice(b"IDAT");
    png.extend_from_slice(&idat_payload);
    png.extend_from_slice(&idat_crc.to_be_bytes());

    // IEND chunk
    let iend_crc = crc32_png(b"IEND", &[]);
    png.extend_from_slice(&0u32.to_be_bytes()); // length = 0
    png.extend_from_slice(b"IEND");
    png.extend_from_slice(&iend_crc.to_be_bytes());

    png
}

/// CRC32 for PNG chunks (covers type + data).
fn crc32_png(chunk_type: &[u8], data: &[u8]) -> u32 {
    let mut combined = Vec::with_capacity(chunk_type.len() + data.len());
    combined.extend_from_slice(chunk_type);
    combined.extend_from_slice(data);
    crc32(&combined)
}

/// CRC32 (ISO 3309 / ITU-T V.42). Used by PNG for chunk checksums.
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// Minimal zlib compression (store, no actual deflate compression).
/// Wraps raw data in a valid zlib stream using non-compressed deflate blocks.
fn zlib_compress(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    // Zlib header: CMF=0x78 (deflate, window 32K), FLG=0x01 (no dict, check)
    out.push(0x78);
    out.push(0x01);

    // Deflate non-compressed block (BFINAL=1, BTYPE=00)
    out.push(0x01); // BFINAL=1, BTYPE=0 (no compression)
    let len = data.len() as u16;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&(!len).to_le_bytes()); // NLEN
    out.extend_from_slice(data);

    // Adler-32 checksum
    let adler = adler32(data);
    out.extend_from_slice(&adler.to_be_bytes());

    out
}

/// Adler-32 checksum.
fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

// ---------------------------------------------------------------------------
// PPTX: embedded image via pic:pic + slide rels
// ---------------------------------------------------------------------------

fn make_pptx_with_image(png_data: &[u8]) -> Vec<u8> {
    // Content types need to include PNG
    let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Default Extension="png" ContentType="image/png"/>
  <Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
  <Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
</Types>"#;

    let slide_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:cSld>
    <p:spTree>
      <p:pic>
        <p:nvPicPr>
          <p:cNvPr id="4" name="Picture 1"/>
          <p:cNvPicPr/>
          <p:nvPr/>
        </p:nvPicPr>
        <p:blipFill>
          <a:blip r:embed="rId2"/>
        </p:blipFill>
        <p:spPr>
          <a:xfrm>
            <a:off x="0" y="0"/>
            <a:ext cx="914400" cy="914400"/>
          </a:xfrm>
        </p:spPr>
      </p:pic>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

    let slide_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId2"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image"
        Target="../media/image1.png"/>
</Relationships>"#;

    // Build entries list, adding the PNG as a separate entry
    let entries: Vec<(&str, &[u8])> = vec![
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", PPTX_PACKAGE_RELS),
        ("ppt/presentation.xml", PPTX_PRESENTATION_1SLIDE),
        ("ppt/_rels/presentation.xml.rels", PPTX_PRES_RELS_1SLIDE),
        ("ppt/slides/slide1.xml", slide_xml),
        ("ppt/slides/_rels/slide1.xml.rels", slide_rels),
        ("ppt/media/image1.png", png_data),
    ];

    build_stored_zip(&entries)
}

/// Smoke test: verifies that PPTX extraction does not crash on slides
/// containing image shapes. The converter does not populate doc.images yet
/// (images are only accessible via the Extractor's page_images() API through
/// the PageExtractor trait), so we only assert the document was produced.
#[test]
fn pptx_image_extraction_smoke_test() {
    let png_data = minimal_png();
    let data = make_pptx_with_image(&png_data);
    let doc = udoc::extract_bytes(&data).expect("PPTX extract should succeed");

    assert!(
        doc.metadata.page_count > 0,
        "PPTX should have at least one slide"
    );
}

#[test]
fn pptx_image_via_extractor_page_images() {
    let png_data = minimal_png();
    let data = make_pptx_with_image(&png_data);

    let mut ext = udoc::Extractor::from_bytes(&data).expect("Extractor::from_bytes should succeed");
    assert!(ext.page_count() > 0, "should have at least one slide");

    let images = ext.page_images(0).expect("page_images(0) should succeed");
    assert!(
        !images.is_empty(),
        "page_images(0) should return at least one image for PPTX with embedded PNG"
    );

    let img = &images[0];
    assert!(!img.data.is_empty(), "page image data should not be empty");

    // Verify the image is detected as PNG.
    assert_eq!(
        img.filter,
        udoc_core::image::ImageFilter::Png,
        "embedded image should be detected as PNG"
    );

    // Verify dimensions are parsed from the PNG header (1x1 pixel).
    assert_eq!(img.width, 1, "PNG width should be 1");
    assert_eq!(img.height, 1, "PNG height should be 1");
}

// ---------------------------------------------------------------------------
// DOCX: embedded image via document.xml.rels image relationship
// ---------------------------------------------------------------------------

fn make_docx_with_image(png_data: &[u8]) -> Vec<u8> {
    // Content types including PNG default
    let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Default Extension="png" ContentType="image/png"/>
    <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

    let package_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
        Target="word/document.xml"/>
</Relationships>"#;

    // Document rels with an image relationship
    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId2"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image"
        Target="media/image1.png"/>
</Relationships>"#;

    let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Hello with image</w:t></w:r></w:p>
    </w:body>
</w:document>"#;

    let entries: Vec<(&str, &[u8])> = vec![
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", package_rels),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/document.xml", document_xml),
        ("word/media/image1.png", png_data),
    ];

    build_stored_zip(&entries)
}

#[test]
fn docx_image_extraction() {
    let png_data = minimal_png();
    let data = make_docx_with_image(&png_data);

    let mut ext = udoc::Extractor::from_bytes(&data).expect("Extractor::from_bytes should succeed");
    assert!(ext.page_count() > 0, "should have at least one page");

    let images = ext.page_images(0).expect("page_images(0) should succeed");
    assert!(
        !images.is_empty(),
        "page_images(0) should return at least one image for DOCX with embedded PNG"
    );

    let img = &images[0];
    assert!(!img.data.is_empty(), "page image data should not be empty");

    // Verify the image is detected as PNG.
    assert_eq!(
        img.filter,
        udoc_core::image::ImageFilter::Png,
        "embedded image should be detected as PNG"
    );

    // Verify dimensions are parsed from the PNG header (1x1 pixel).
    assert_eq!(img.width, 1, "PNG width should be 1");
    assert_eq!(img.height, 1, "PNG height should be 1");
}

// ---------------------------------------------------------------------------
// ODF (ODT): embedded image via draw:frame/draw:image + Pictures/ ZIP entry
// ---------------------------------------------------------------------------

fn make_odt_with_image(png_data: &[u8]) -> Vec<u8> {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
    xmlns:xlink="http://www.w3.org/1999/xlink">
  <office:body>
    <office:text>
      <text:p>Before image</text:p>
      <draw:frame draw:name="img1">
        <draw:image xlink:href="Pictures/image1.png" xlink:type="simple"/>
      </draw:frame>
      <text:p>After image</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

    let entries: Vec<(&str, &[u8])> = vec![
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.text" as &[u8],
        ),
        ("content.xml", content_xml),
        ("Pictures/image1.png", png_data),
    ];

    build_stored_zip(&entries)
}

#[test]
fn odf_image_extraction() {
    let png_data = minimal_png();
    let data = make_odt_with_image(&png_data);

    let mut ext = udoc::Extractor::from_bytes(&data).expect("Extractor::from_bytes should succeed");
    assert!(ext.page_count() > 0, "should have at least one page");

    let images = ext.page_images(0).expect("page_images(0) should succeed");
    assert!(
        !images.is_empty(),
        "page_images(0) should return at least one image for ODT with embedded PNG"
    );

    let img = &images[0];
    assert!(!img.data.is_empty(), "page image data should not be empty");

    // Verify the image is detected as PNG.
    assert_eq!(
        img.filter,
        udoc_core::image::ImageFilter::Png,
        "embedded image should be detected as PNG"
    );

    // Verify dimensions are parsed from the PNG header (1x1 pixel).
    assert_eq!(img.width, 1, "PNG width should be 1");
    assert_eq!(img.height, 1, "PNG height should be 1");
}

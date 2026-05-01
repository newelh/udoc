//! Conversion from PDF-specific types to udoc-core types.
//!
//! These functions live at the FormatBackend boundary: PDF internals use
//! crate-local types with full fidelity; the public backend trait returns
//! format-agnostic core types. Conversion happens once per API call, not
//! in hot loops.

use udoc_core::document::presentation::Color;
use udoc_core::geometry::BoundingBox as CoreBBox;
use udoc_core::image::{ImageFilter as CoreImageFilter, PageImage as CorePageImage};
use udoc_core::table::{Table as CoreTable, TableCell as CoreTableCell, TableRow as CoreTableRow};
use udoc_core::text::{TextLine as CoreTextLine, TextSpan as CoreTextSpan};

use crate::image::{ImageFilter as PdfImageFilter, PageImage as PdfPageImage};
use crate::table::{Table as PdfTable, TableCell as PdfTableCell, TableRow as PdfTableRow};
use crate::text::{TextLine as PdfTextLine, TextSpan as PdfTextSpan};

/// Convert a PDF BoundingBox to a core BoundingBox.
///
/// Both types have identical field layout so this is a direct field copy.
pub(crate) fn convert_bbox(bbox: &crate::geometry::BoundingBox) -> CoreBBox {
    CoreBBox::new(bbox.x_min, bbox.y_min, bbox.x_max, bbox.y_max)
}

/// Heuristic: infer bold from font name substrings.
///
/// Called per text span in the PDF -> core conversion, so an allocation
/// every call would dominate ( flamegraph: ~0.4% / 1395 samples
/// just in the lowercase copy). We do an in-place case-insensitive scan
/// instead.
fn infer_bold(font_name: &str) -> bool {
    contains_ascii_ci(font_name, "bold")
        || contains_ascii_ci(font_name, "-bd")
        || contains_ascii_ci(font_name, "black")
}

/// Heuristic: infer italic from font name substrings. See [`infer_bold`].
fn infer_italic(font_name: &str) -> bool {
    contains_ascii_ci(font_name, "italic")
        || contains_ascii_ci(font_name, "oblique")
        || contains_ascii_ci(font_name, "-it")
}

/// Case-insensitive ASCII substring search without allocating.
///
/// `needle` MUST be ASCII lowercase (no validation; this is a private
/// helper for fixed string literals like "bold").
fn contains_ascii_ci(haystack: &str, needle: &str) -> bool {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() {
        return true;
    }
    if h.len() < n.len() {
        return false;
    }
    'outer: for i in 0..=h.len() - n.len() {
        for j in 0..n.len() {
            if h[i + j].to_ascii_lowercase() != n[j] {
                continue 'outer;
            }
        }
        return true;
    }
    false
}

/// Convert a PDF TextSpan to a core TextSpan, consuming the source.
///
/// Moves heap-allocated fields (text, char_advances, char_codes, char_gids,
/// glyph_bboxes, font_resolution) out of `span` instead of cloning them.
/// per-span this drops 5 Vec clones + 1 String clone on the
/// extraction hot path.
///
/// Drops PDF-internal fields (mcid, space_width, has_font_metrics, is_annotation).
/// Infers is_bold/is_italic from font_name since PDF doesn't carry explicit bold/italic flags.
pub fn convert_text_span_owned(span: PdfTextSpan) -> CoreTextSpan {
    let is_bold = infer_bold(&span.font_name);
    let is_italic = infer_italic(&span.font_name);
    let mut core = CoreTextSpan::with_style(
        span.text,
        span.x,
        span.y,
        span.width,
        Some(span.font_name.to_string()),
        span.font_size,
        is_bold,
        is_italic,
        span.is_invisible,
        span.rotation,
    );
    core.color = span.color.map(Color::from);
    core.letter_spacing = span.letter_spacing;
    core.is_superscript = span.is_superscript;
    core.is_subscript = span.is_subscript;
    core.char_advances = span.char_advances;
    core.advance_scale = span.advance_scale;
    core.char_codes = span.char_codes;
    core.char_gids = span.char_gids;
    core.z_index = span.z_index;
    core.font_id = span.font_id.as_deref().map(str::to_string);
    core.font_resolution = span.font_resolution;
    core.glyph_bboxes = span.glyph_bboxes;
    core
}

/// Convert a Vec of PDF TextSpans to core TextSpans, consuming the source.
pub fn convert_text_spans_owned(spans: Vec<PdfTextSpan>) -> Vec<CoreTextSpan> {
    spans.into_iter().map(convert_text_span_owned).collect()
}

/// Convert a PDF TextLine to a core TextLine, consuming the source.
pub fn convert_text_line_owned(line: PdfTextLine) -> CoreTextLine {
    CoreTextLine::new(
        line.spans
            .into_iter()
            .map(convert_text_span_owned)
            .collect(),
        line.baseline,
        line.is_vertical,
    )
}

/// Convert a Vec of PDF TextLines to core TextLines, consuming the source.
pub fn convert_text_lines_owned(lines: Vec<PdfTextLine>) -> Vec<CoreTextLine> {
    lines.into_iter().map(convert_text_line_owned).collect()
}

/// Convert a PDF ImageFilter to a core ImageFilter.
///
/// PDF's TransportEncoded (inline images where decoding failed) maps to Raw
/// since the core API doesn't distinguish transport-encoded data.
fn convert_image_filter(filter: PdfImageFilter) -> CoreImageFilter {
    match filter {
        PdfImageFilter::Jpeg => CoreImageFilter::Jpeg,
        PdfImageFilter::Jpeg2000 => CoreImageFilter::Jpeg2000,
        PdfImageFilter::Jbig2 => CoreImageFilter::Jbig2,
        PdfImageFilter::Ccitt => CoreImageFilter::Ccitt,
        PdfImageFilter::Raw => CoreImageFilter::Raw,
        PdfImageFilter::TransportEncoded => CoreImageFilter::Raw,
    }
}

/// Convert a PDF PageImage to a core PageImage, consuming the source.
///
/// Moves the image data, color_space, and soft_mask buffers instead of
/// cloning them. Image data is the largest per-allocation in the extraction
/// hot path.
///
/// Constructs a BoundingBox from the PDF image's x/y/display_width/display_height.
/// Drops PDF-specific fields (color_space, inline, separate x/y/display dimensions).
pub fn convert_page_image_owned(img: PdfPageImage) -> CorePageImage {
    let bbox = CoreBBox::new(
        img.x,
        img.y,
        img.x + img.display_width,
        img.y + img.display_height,
    );
    let mut core = CorePageImage::new(
        img.data,
        convert_image_filter(img.filter),
        img.width,
        img.height,
        img.bits_per_component,
        Some(bbox),
    );
    core.color_space = Some(img.color_space);
    core.z_index = img.z_index;
    core.is_mask = img.is_mask;
    core.mask_color = img.mask_color;
    core.soft_mask = img.soft_mask;
    core.soft_mask_width = img.soft_mask_width;
    core.soft_mask_height = img.soft_mask_height;
    core.ctm = Some(img.ctm);
    core
}

/// Convert a Vec of PDF PageImages to core PageImages, consuming the source.
pub fn convert_page_images_owned(images: Vec<PdfPageImage>) -> Vec<CorePageImage> {
    images.into_iter().map(convert_page_image_owned).collect()
}

/// Convert a PDF TableCell to a core TableCell, consuming the source.
fn convert_table_cell_owned(cell: PdfTableCell) -> CoreTableCell {
    let bbox = convert_bbox(&cell.bbox);
    CoreTableCell::with_spans(cell.text, Some(bbox), cell.col_span, cell.row_span)
}

/// Convert a PDF TableRow to a core TableRow, consuming the source.
fn convert_table_row_owned(row: PdfTableRow) -> CoreTableRow {
    CoreTableRow::with_header(
        row.cells
            .into_iter()
            .map(convert_table_cell_owned)
            .collect(),
        row.is_header,
    )
}

/// Convert a PDF Table to a core Table, consuming the source.
///
/// Wraps bbox in Some (PDF tables always have geometry).
/// Drops detection_method and column_positions (PDF-specific)..
pub fn convert_table_owned(table: PdfTable) -> CoreTable {
    let bbox = convert_bbox(&table.bbox);
    let rows: Vec<CoreTableRow> = table
        .rows
        .into_iter()
        .map(convert_table_row_owned)
        .collect();
    CoreTable::with_continuation(
        rows,
        Some(bbox),
        table.may_continue_from_previous,
        table.may_continue_to_next,
    )
}

/// Convert a Vec of PDF Tables to core Tables, consuming the source.
pub fn convert_tables_owned(tables: Vec<PdfTable>) -> Vec<CoreTable> {
    tables.into_iter().map(convert_table_owned).collect()
}

/// Convert a PDF error to a core error.
///
/// Encryption errors are mapped to the typed
/// [`udoc_core::error::Error::encryption_required`] constructor with a
/// reason that mirrors the PDF [`crate::error::EncryptionErrorKind`]
/// variant. Downstream callers can recover the typed signal via
/// [`udoc_core::error::Error::is_encryption_error`] /
/// [`udoc_core::error::Error::encryption_info`] without substring-
/// matching the displayed message. All other variants stringify
/// (preserving the existing display, including any backend context
/// chain).
pub fn convert_error(err: crate::Error) -> udoc_core::error::Error {
    use crate::error::EncryptionErrorKind;
    use udoc_core::error::EncryptionReason;

    if let crate::Error::Encryption(enc) = &err {
        let reason = match &enc.kind {
            EncryptionErrorKind::InvalidPassword => {
                // We can't tell from this boundary whether the caller
                // supplied a (wrong) password or no password at all.
                // Default to PasswordRequired since that's the common
                // case for `extract(path)` without options. Callers
                // that have explicit "password was supplied" context
                // can post-process via `Error::encryption_info()` and
                // re-raise as WrongPassword if they care to distinguish.
                EncryptionReason::PasswordRequired
            }
            EncryptionErrorKind::UnsupportedFilter(name) => {
                EncryptionReason::UnsupportedAlgorithm(format!("filter={name}"))
            }
            EncryptionErrorKind::UnsupportedVersion { v, r } => {
                EncryptionReason::UnsupportedAlgorithm(format!("V={v} R={r}"))
            }
            EncryptionErrorKind::MissingField(field) => {
                EncryptionReason::Malformed(format!("missing field: {field}"))
            }
            EncryptionErrorKind::InvalidField(detail) => {
                EncryptionReason::Malformed(detail.clone())
            }
            EncryptionErrorKind::DecryptionFailed(detail) => {
                EncryptionReason::Other(detail.clone())
            }
        };
        // Preserve the backend's full display (with any context chain)
        // by attaching it to the core error's context. The typed
        // payload survives the with_context wrap.
        return udoc_core::error::Error::encryption_required(reason).with_context(format!("{err}"));
    }
    udoc_core::error::Error::new(format!("{err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_span_basic() {
        let pdf_span = PdfTextSpan::new(
            "hello".to_string(),
            10.0,
            20.0,
            50.0,
            "Helvetica-Bold",
            12.0,
        );
        let core = convert_text_span_owned(pdf_span);
        assert_eq!(core.text, "hello");
        assert_eq!(core.x, 10.0);
        assert_eq!(core.font_name.as_deref(), Some("Helvetica-Bold"));
        assert!(core.is_bold);
        assert!(!core.is_italic);
        assert!(!core.is_invisible);
    }

    #[test]
    fn convert_span_italic() {
        let pdf_span = PdfTextSpan::new(
            "hi".to_string(),
            0.0,
            0.0,
            10.0,
            "TimesNewRoman-Italic",
            12.0,
        );
        let core = convert_text_span_owned(pdf_span);
        assert!(core.is_italic);
        assert!(!core.is_bold);
    }

    #[test]
    fn convert_span_bold_italic() {
        let pdf_span = PdfTextSpan::new("x".to_string(), 0.0, 0.0, 5.0, "Arial-BoldItalic", 10.0);
        let core = convert_text_span_owned(pdf_span);
        assert!(core.is_bold);
        assert!(core.is_italic);
    }

    #[test]
    fn convert_line() {
        let pdf_line = PdfTextLine {
            spans: vec![PdfTextSpan::new(
                "test".to_string(),
                0.0,
                100.0,
                40.0,
                "Courier",
                10.0,
            )],
            baseline: 100.0,
            is_vertical: false,
        };
        let core = convert_text_line_owned(pdf_line);
        assert_eq!(core.spans.len(), 1);
        assert_eq!(core.baseline, 100.0);
        assert!(!core.is_vertical);
    }

    #[test]
    fn convert_image_jpeg() {
        let pdf_img = PdfPageImage {
            x: 50.0,
            y: 100.0,
            width: 200,
            height: 300,
            display_width: 150.0,
            display_height: 200.0,
            color_space: "DeviceRGB".into(),
            bits_per_component: 8,
            data: vec![0xFF, 0xD8],
            filter: PdfImageFilter::Jpeg,
            inline: false,
            mcid: None,
            z_index: 0,
            is_mask: false,
            mask_color: [0, 0, 0],
            soft_mask: None,
            soft_mask_width: 0,
            soft_mask_height: 0,
            ctm: [150.0, 0.0, 0.0, 200.0, 50.0, 100.0],
        };
        let core = convert_page_image_owned(pdf_img);
        assert_eq!(core.width, 200);
        assert_eq!(core.height, 300);
        assert_eq!(core.filter, CoreImageFilter::Jpeg);
        assert_eq!(core.bits_per_component, 8);
        let bbox = core.bbox.unwrap();
        assert_eq!(bbox.x_min, 50.0);
        assert_eq!(bbox.y_min, 100.0);
        assert_eq!(bbox.x_max, 200.0); // 50 + 150
        assert_eq!(bbox.y_max, 300.0); // 100 + 200
    }

    #[test]
    fn convert_image_transport_encoded_to_raw() {
        assert_eq!(
            convert_image_filter(PdfImageFilter::TransportEncoded),
            CoreImageFilter::Raw
        );
    }

    #[test]
    fn convert_table_basic() {
        use crate::geometry::BoundingBox;
        use crate::table::TableDetectionMethod;

        let pdf_table = PdfTable::new(
            BoundingBox::new(0.0, 0.0, 100.0, 50.0),
            vec![PdfTableRow::new(vec![PdfTableCell::new(
                "cell".into(),
                BoundingBox::new(0.0, 0.0, 50.0, 25.0),
            )])],
            TableDetectionMethod::RuledLine,
        );
        let core = convert_table_owned(pdf_table);
        assert!(core.bbox.is_some());
        assert_eq!(core.rows.len(), 1);
        assert_eq!(core.num_columns, 1);
        assert_eq!(core.rows[0].cells[0].text, "cell");
        assert!(core.rows[0].cells[0].bbox.is_some());
    }

    #[test]
    fn infer_bold_cases() {
        assert!(infer_bold("Helvetica-Bold"));
        assert!(infer_bold("ArialBold"));
        assert!(infer_bold("TimesNewRoman-BdIt"));
        assert!(infer_bold("NotoSans-Black"));
        assert!(!infer_bold("Helvetica"));
        assert!(!infer_bold("Courier"));
    }

    #[test]
    fn infer_italic_cases() {
        assert!(infer_italic("Helvetica-Oblique"));
        assert!(infer_italic("TimesNewRoman-Italic"));
        assert!(infer_italic("Arial-BoldItalic"));
        assert!(!infer_italic("Helvetica"));
        assert!(!infer_italic("Courier-Bold"));
    }

    //
    // Verify that convert_error preserves the typed encryption signal
    // across the FormatBackend boundary so downstream callers (CLI
    // inspect, Python doc.is_encrypted) can dispatch on
    // Error::is_encryption_error / Error::encryption_info instead of
    // substring-matching the displayed message.

    use crate::error::{EncryptionError, EncryptionErrorKind};
    use udoc_core::error::EncryptionReason;

    fn pdf_encryption_error(kind: EncryptionErrorKind) -> crate::Error {
        crate::Error::Encryption(EncryptionError {
            kind,
            context: Vec::new(),
        })
    }

    #[test]
    fn convert_error_invalid_password_maps_to_password_required() {
        let err = pdf_encryption_error(EncryptionErrorKind::InvalidPassword);
        let core = convert_error(err);
        assert!(core.is_encryption_error());
        let info = core.encryption_info().unwrap();
        assert!(matches!(info.reason, EncryptionReason::PasswordRequired));
    }

    #[test]
    fn convert_error_unsupported_filter_maps_to_unsupported_algorithm() {
        let err = pdf_encryption_error(EncryptionErrorKind::UnsupportedFilter("Foo".into()));
        let core = convert_error(err);
        assert!(core.is_encryption_error());
        let info = core.encryption_info().unwrap();
        match &info.reason {
            EncryptionReason::UnsupportedAlgorithm(detail) => assert!(detail.contains("Foo")),
            other => panic!("unexpected reason: {other:?}"),
        }
    }

    #[test]
    fn convert_error_unsupported_version_maps_to_unsupported_algorithm() {
        let err = pdf_encryption_error(EncryptionErrorKind::UnsupportedVersion { v: 5, r: 6 });
        let core = convert_error(err);
        assert!(core.is_encryption_error());
        let info = core.encryption_info().unwrap();
        match &info.reason {
            EncryptionReason::UnsupportedAlgorithm(detail) => {
                assert!(detail.contains("V=5"));
                assert!(detail.contains("R=6"));
            }
            other => panic!("unexpected reason: {other:?}"),
        }
    }

    #[test]
    fn convert_error_missing_field_maps_to_malformed() {
        let err = pdf_encryption_error(EncryptionErrorKind::MissingField("ID".into()));
        let core = convert_error(err);
        let info = core.encryption_info().unwrap();
        match &info.reason {
            EncryptionReason::Malformed(detail) => assert!(detail.contains("ID")),
            other => panic!("unexpected reason: {other:?}"),
        }
    }

    #[test]
    fn convert_error_non_encryption_returns_plain_error() {
        // Anything that isn't Error::Encryption stays a plain core
        // error -- is_encryption_error() must be false.
        let plain = crate::Error::structure("synthetic test error");
        let core = convert_error(plain);
        assert!(!core.is_encryption_error());
    }

    #[test]
    fn convert_error_preserves_displayed_message_in_context() {
        let err = pdf_encryption_error(EncryptionErrorKind::InvalidPassword);
        let core = convert_error(err);
        // The full backend display string should be in the context
        // chain so user-facing messages still surface "encryption
        // error: invalid password" verbatim.
        let s = format!("{core}");
        assert!(s.contains("invalid password"), "got: {s}");
    }
}

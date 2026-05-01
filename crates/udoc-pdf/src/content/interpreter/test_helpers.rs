//! Test helper functions for building minimal PDFs used by interpreter tests.
//!
//! These helpers construct in-memory PDF byte sequences with corresponding
//! xref tables, suitable for feeding into an ObjectResolver in unit tests.

use crate::object::{ObjRef, PdfDictionary, PdfObject};
use crate::parse::{XrefEntry, XrefTable};

// -----------------------------------------------------------------------
// Helper to create interpreter with minimal PDF + optional font
// -----------------------------------------------------------------------

/// Create a minimal PDF with a Helvetica font at obj 1.
/// Returns (pdf_data, xref_table).
pub(super) fn make_interp_with_font() -> (Vec<u8>, XrefTable) {
    let font_dict_bytes =
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>";
    let mut pdf_data = b"%PDF-1.4\n".to_vec();
    let offset = pdf_data.len() as u64;
    pdf_data.extend_from_slice(b"1 0 obj ");
    pdf_data.extend_from_slice(font_dict_bytes);
    pdf_data.extend_from_slice(b" endobj\n");

    let mut xref = XrefTable::new();
    xref.insert_if_absent(1, XrefEntry::Uncompressed { offset, gen: 0 });

    (pdf_data, xref)
}

pub(super) fn font_resources_dict() -> PdfDictionary {
    let mut font_dict = PdfDictionary::new();
    font_dict.insert(b"F1".to_vec(), PdfObject::Reference(ObjRef::new(1, 0)));
    let mut resources = PdfDictionary::new();
    resources.insert(b"Font".to_vec(), PdfObject::Dictionary(font_dict));
    resources
}

// -----------------------------------------------------------------------
// CharProc text extraction helpers
// -----------------------------------------------------------------------

/// Build a minimal PDF with a Type3 font that has a CharProc containing
/// text operators.
///
/// Object layout:
/// - Obj 1: Type3 font dict (with /CharProcs pointing to obj 2)
/// - Obj 2: CharProc stream containing the given content
/// - Obj 3: Inner font (Helvetica, used by the CharProc)
/// - Obj 4: Type3 /Resources dict with the inner font
///
/// The Type3 font's encoding maps code 0x80 to glyph name "customGlyph".
/// "customGlyph" is not in the AGL, so decode_char returns U+FFFD,
/// triggering the CharProc text extraction fallback.
pub(super) fn make_type3_charproc_pdf(charproc_content: &[u8]) -> (Vec<u8>, XrefTable) {
    let mut pdf_data = b"%PDF-1.4\n".to_vec();

    // Obj 3: inner font (Helvetica)
    let obj3_offset = pdf_data.len() as u64;
    let obj3_bytes = b"3 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >> endobj\n";
    pdf_data.extend_from_slice(obj3_bytes);

    // Obj 4: Type3 /Resources dict with inner font as /F1
    let obj4_offset = pdf_data.len() as u64;
    let obj4_bytes = b"4 0 obj << /Font << /F1 3 0 R >> >> endobj\n";
    pdf_data.extend_from_slice(obj4_bytes);

    // Obj 2: CharProc stream
    let obj2_offset = pdf_data.len() as u64;
    let stream_header = format!("2 0 obj << /Length {} >> stream\n", charproc_content.len());
    pdf_data.extend_from_slice(stream_header.as_bytes());
    pdf_data.extend_from_slice(charproc_content);
    pdf_data.extend_from_slice(b"\nendstream endobj\n");

    // Obj 1: Type3 font dict
    // Code 0x80 (128) maps to glyph "customGlyph" via /Differences.
    // StandardEncoding returns None for 0x80, and "customGlyph" is not in AGL,
    // so decode_char returns FFFD, triggering CharProc fallback.
    let obj1_offset = pdf_data.len() as u64;
    let obj1_bytes = b"1 0 obj << /Type /Font /Subtype /Type3 \
        /FontBBox [0 0 1000 1000] /FontMatrix [0.001 0 0 0.001 0 0] \
        /Encoding << /Type /Encoding /Differences [128 /customGlyph] >> \
        /CharProcs << /customGlyph 2 0 R >> \
        /Resources 4 0 R >> endobj\n";
    pdf_data.extend_from_slice(obj1_bytes);

    let mut xref = XrefTable::new();
    xref.insert_if_absent(
        1,
        XrefEntry::Uncompressed {
            offset: obj1_offset,
            gen: 0,
        },
    );
    xref.insert_if_absent(
        2,
        XrefEntry::Uncompressed {
            offset: obj2_offset,
            gen: 0,
        },
    );
    xref.insert_if_absent(
        3,
        XrefEntry::Uncompressed {
            offset: obj3_offset,
            gen: 0,
        },
    );
    xref.insert_if_absent(
        4,
        XrefEntry::Uncompressed {
            offset: obj4_offset,
            gen: 0,
        },
    );

    (pdf_data, xref)
}

/// Build a /Resources dict pointing to a Type3 font at obj 1.
pub(super) fn type3_font_resources_dict() -> PdfDictionary {
    let mut font_dict = PdfDictionary::new();
    font_dict.insert(b"F1".to_vec(), PdfObject::Reference(ObjRef::new(1, 0)));
    let mut resources = PdfDictionary::new();
    resources.insert(b"Font".to_vec(), PdfObject::Dictionary(font_dict));
    resources
}

// -----------------------------------------------------------------------
// Type3-inside-Type3 recursion test helpers (T3-012)
// -----------------------------------------------------------------------

/// Build a self-referencing Type3 PDF: the Type3 font's /Resources point
/// back to itself, and its CharProc shows the same character code (0x80)
/// that triggered the CharProc. Without cycle detection this would
/// infinite-loop.
///
/// Object layout:
/// - Obj 1: Type3 font dict (/Resources -> obj 4, /CharProcs -> obj 2)
/// - Obj 2: CharProc stream: "BT /F1 12 Tf <80> Tj ET"
/// - Obj 4: /Resources dict with /Font /F1 -> obj 1 (self-reference!)
pub(super) fn make_self_referencing_type3_pdf() -> (Vec<u8>, XrefTable) {
    let mut pdf_data = b"%PDF-1.4\n".to_vec();

    // Obj 4: /Resources dict pointing /F1 back to obj 1 (the Type3 font itself)
    let obj4_offset = pdf_data.len() as u64;
    let obj4_bytes = b"4 0 obj << /Font << /F1 1 0 R >> >> endobj\n";
    pdf_data.extend_from_slice(obj4_bytes);

    // Obj 2: CharProc stream that uses /F1 (which is the Type3 font itself)
    // and shows the same character code 0x80 that triggers CharProc lookup.
    let charproc_content = b"BT /F1 12 Tf <80> Tj ET";
    let obj2_offset = pdf_data.len() as u64;
    let stream_header = format!("2 0 obj << /Length {} >> stream\n", charproc_content.len());
    pdf_data.extend_from_slice(stream_header.as_bytes());
    pdf_data.extend_from_slice(charproc_content);
    pdf_data.extend_from_slice(b"\nendstream endobj\n");

    // Obj 1: Type3 font dict (self-referencing via /Resources -> obj 4 -> obj 1)
    let obj1_offset = pdf_data.len() as u64;
    let obj1_bytes = b"1 0 obj << /Type /Font /Subtype /Type3 \
        /FontBBox [0 0 1000 1000] /FontMatrix [0.001 0 0 0.001 0 0] \
        /Encoding << /Type /Encoding /Differences [128 /customGlyph] >> \
        /CharProcs << /customGlyph 2 0 R >> \
        /Resources 4 0 R >> endobj\n";
    pdf_data.extend_from_slice(obj1_bytes);

    let mut xref = XrefTable::new();
    xref.insert_if_absent(
        1,
        XrefEntry::Uncompressed {
            offset: obj1_offset,
            gen: 0,
        },
    );
    xref.insert_if_absent(
        2,
        XrefEntry::Uncompressed {
            offset: obj2_offset,
            gen: 0,
        },
    );
    xref.insert_if_absent(
        4,
        XrefEntry::Uncompressed {
            offset: obj4_offset,
            gen: 0,
        },
    );

    (pdf_data, xref)
}

/// Build a PDF with two Type3 fonts that reference each other's CharProcs,
/// creating mutual recursion: font A's CharProc uses font B, and font B's
/// CharProc uses font A.
///
/// Object layout:
/// - Obj 1: Type3 font A (/Resources -> obj 4, /CharProcs -> obj 2)
/// - Obj 2: Font A's CharProc stream: "BT /F1 12 Tf <80> Tj ET" (uses font B)
/// - Obj 4: Font A's /Resources dict (/F1 -> obj 5, i.e. font B)
/// - Obj 5: Type3 font B (/Resources -> obj 7, /CharProcs -> obj 6)
/// - Obj 6: Font B's CharProc stream: "BT /F1 12 Tf <80> Tj ET" (uses font A)
/// - Obj 7: Font B's /Resources dict (/F1 -> obj 1, i.e. font A)
pub(super) fn make_mutual_type3_pdf() -> (Vec<u8>, XrefTable) {
    let mut pdf_data = b"%PDF-1.4\n".to_vec();

    // Obj 7: Font B's /Resources dict, /F1 -> obj 1 (font A)
    let obj7_offset = pdf_data.len() as u64;
    pdf_data.extend_from_slice(b"7 0 obj << /Font << /F1 1 0 R >> >> endobj\n");

    // Obj 6: Font B's CharProc stream (uses /F1 = font A, shows 0x80)
    let charproc_b = b"BT /F1 12 Tf <80> Tj ET";
    let obj6_offset = pdf_data.len() as u64;
    let header6 = format!("6 0 obj << /Length {} >> stream\n", charproc_b.len());
    pdf_data.extend_from_slice(header6.as_bytes());
    pdf_data.extend_from_slice(charproc_b);
    pdf_data.extend_from_slice(b"\nendstream endobj\n");

    // Obj 5: Type3 font B
    let obj5_offset = pdf_data.len() as u64;
    pdf_data.extend_from_slice(
        b"5 0 obj << /Type /Font /Subtype /Type3 \
        /FontBBox [0 0 1000 1000] /FontMatrix [0.001 0 0 0.001 0 0] \
        /Encoding << /Type /Encoding /Differences [128 /glyphB] >> \
        /CharProcs << /glyphB 6 0 R >> \
        /Resources 7 0 R >> endobj\n",
    );

    // Obj 4: Font A's /Resources dict, /F1 -> obj 5 (font B)
    let obj4_offset = pdf_data.len() as u64;
    pdf_data.extend_from_slice(b"4 0 obj << /Font << /F1 5 0 R >> >> endobj\n");

    // Obj 2: Font A's CharProc stream (uses /F1 = font B, shows 0x80)
    let charproc_a = b"BT /F1 12 Tf <80> Tj ET";
    let obj2_offset = pdf_data.len() as u64;
    let header2 = format!("2 0 obj << /Length {} >> stream\n", charproc_a.len());
    pdf_data.extend_from_slice(header2.as_bytes());
    pdf_data.extend_from_slice(charproc_a);
    pdf_data.extend_from_slice(b"\nendstream endobj\n");

    // Obj 1: Type3 font A
    let obj1_offset = pdf_data.len() as u64;
    pdf_data.extend_from_slice(
        b"1 0 obj << /Type /Font /Subtype /Type3 \
        /FontBBox [0 0 1000 1000] /FontMatrix [0.001 0 0 0.001 0 0] \
        /Encoding << /Type /Encoding /Differences [128 /glyphA] >> \
        /CharProcs << /glyphA 2 0 R >> \
        /Resources 4 0 R >> endobj\n",
    );

    let mut xref = XrefTable::new();
    for (num, offset) in [
        (1, obj1_offset),
        (2, obj2_offset),
        (4, obj4_offset),
        (5, obj5_offset),
        (6, obj6_offset),
        (7, obj7_offset),
    ] {
        xref.insert_if_absent(num, XrefEntry::Uncompressed { offset, gen: 0 });
    }

    (pdf_data, xref)
}

/// Build a PDF with a chain of Type3 fonts that exceeds MAX_CHARPROC_DEPTH.
/// Font at obj N references font at obj N+3 in its CharProc /Resources.
/// Chain length is depth+1 to ensure the limit is hit.
///
/// Each font has:
/// - Font dict at obj (base + 0)
/// - CharProc stream at obj (base + 1)
/// - /Resources dict at obj (base + 2)
///
/// The last font in the chain has a CharProc with a simple text operator
/// (Helvetica) so the chain would produce text if depth limits did not
/// prevent it from getting that far.
pub(super) fn make_deep_type3_chain_pdf(chain_len: usize) -> (Vec<u8>, XrefTable) {
    let mut pdf_data = b"%PDF-1.4\n".to_vec();
    let mut xref = XrefTable::new();

    // Terminal font: a simple Helvetica so the deepest CharProc has something
    // real to use. Its obj number is (chain_len * 3 + 1).
    let terminal_obj = (chain_len * 3 + 1) as u32;
    let terminal_offset = pdf_data.len() as u64;
    let terminal_bytes = format!(
        "{} 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
         /Encoding /WinAnsiEncoding >> endobj\n",
        terminal_obj
    );
    pdf_data.extend_from_slice(terminal_bytes.as_bytes());
    xref.insert_if_absent(
        terminal_obj,
        XrefEntry::Uncompressed {
            offset: terminal_offset,
            gen: 0,
        },
    );

    // Build the chain from the last Type3 font backward to the first.
    // Font i (0-indexed) occupies objects: base = i*3 + 1
    //   font dict = base, charproc stream = base+1, resources = base+2
    // Its CharProc references the *next* font in the chain (or terminal Helvetica
    // for the last Type3 in the chain).
    for i in (0..chain_len).rev() {
        let base = (i * 3 + 1) as u32;
        let font_obj = base;
        let charproc_obj = base + 1;
        let resources_obj = base + 2;

        // What font does this CharProc's /Resources point to?
        let next_font_obj = if i + 1 < chain_len {
            ((i + 1) * 3 + 1) as u32 // next Type3 in chain
        } else {
            terminal_obj // Helvetica at the end
        };

        // Resources dict: /Font /F1 -> next font
        let res_offset = pdf_data.len() as u64;
        let res_bytes = format!(
            "{} 0 obj << /Font << /F1 {} 0 R >> >> endobj\n",
            resources_obj, next_font_obj
        );
        pdf_data.extend_from_slice(res_bytes.as_bytes());
        xref.insert_if_absent(
            resources_obj,
            XrefEntry::Uncompressed {
                offset: res_offset,
                gen: 0,
            },
        );

        // CharProc stream: use /F1 and show 0x80 (triggers next CharProc)
        // For the last Type3, its /F1 is Helvetica, so show "(Z)" normally
        let charproc_content: &[u8] = if i + 1 < chain_len {
            b"BT /F1 12 Tf <80> Tj ET"
        } else {
            b"BT /F1 12 Tf (Z) Tj ET"
        };
        let cp_offset = pdf_data.len() as u64;
        let cp_header = format!(
            "{} 0 obj << /Length {} >> stream\n",
            charproc_obj,
            charproc_content.len()
        );
        pdf_data.extend_from_slice(cp_header.as_bytes());
        pdf_data.extend_from_slice(charproc_content);
        pdf_data.extend_from_slice(b"\nendstream endobj\n");
        xref.insert_if_absent(
            charproc_obj,
            XrefEntry::Uncompressed {
                offset: cp_offset,
                gen: 0,
            },
        );

        // Font dict
        let glyph_name = format!("glyph{}", i);
        let font_offset = pdf_data.len() as u64;
        let font_bytes = format!(
            "{} 0 obj << /Type /Font /Subtype /Type3 \
             /FontBBox [0 0 1000 1000] /FontMatrix [0.001 0 0 0.001 0 0] \
             /Encoding << /Type /Encoding /Differences [128 /{}] >> \
             /CharProcs << /{} {} 0 R >> \
             /Resources {} 0 R >> endobj\n",
            font_obj, glyph_name, glyph_name, charproc_obj, resources_obj
        );
        pdf_data.extend_from_slice(font_bytes.as_bytes());
        xref.insert_if_absent(
            font_obj,
            XrefEntry::Uncompressed {
                offset: font_offset,
                gen: 0,
            },
        );
    }

    (pdf_data, xref)
}

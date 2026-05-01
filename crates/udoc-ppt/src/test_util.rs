//! Shared test helpers for building synthetic PPT binary structures.
//!
//! Available when the `test-internals` feature is enabled (dev-dependencies
//! enable it by default). Used by unit tests, golden file tests, malformed
//! recovery tests, and facade integration tests.

use crate::records::{rt, HEADER_SIZE};

/// Build a raw PPT record: 8-byte header + payload.
pub fn build_record(rec_ver: u8, rec_instance: u16, rec_type: u16, payload: &[u8]) -> Vec<u8> {
    let ver_inst: u16 = ((rec_instance & 0x0FFF) << 4) | (rec_ver as u16 & 0x0F);
    let rec_len = payload.len() as u32;
    let mut buf = Vec::with_capacity(HEADER_SIZE + payload.len());
    buf.extend_from_slice(&ver_inst.to_le_bytes());
    buf.extend_from_slice(&rec_type.to_le_bytes());
    buf.extend_from_slice(&rec_len.to_le_bytes());
    buf.extend_from_slice(payload);
    buf
}

/// Build an atom record (rec_ver = 0, rec_instance = 0).
pub fn build_atom(rec_type: u16, payload: &[u8]) -> Vec<u8> {
    build_record(0, 0, rec_type, payload)
}

/// Build a container record (rec_ver = 0xF) with given instance.
pub fn build_container_with_instance(
    rec_instance: u16,
    rec_type: u16,
    children_data: &[u8],
) -> Vec<u8> {
    build_record(0xF, rec_instance, rec_type, children_data)
}

/// Build a container record (rec_ver = 0xF, instance = 0).
pub fn build_container(rec_type: u16, children_data: &[u8]) -> Vec<u8> {
    build_container_with_instance(0, rec_type, children_data)
}

/// Build a SlidePersistAtom with the given persist ID.
/// slideIdentifier defaults to persist_id.
pub fn build_slide_persist_atom(persist_id: u32) -> Vec<u8> {
    build_slide_persist_atom_with_id(persist_id, persist_id)
}

/// Build a SlidePersistAtom with explicit persist ID and slideIdentifier.
/// The slideIdentifier (bytes 12-15) determines presentation order.
pub fn build_slide_persist_atom_with_id(persist_id: u32, slide_id: u32) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&persist_id.to_le_bytes()); // psrReference (bytes 0-3)
    data.extend_from_slice(&0u32.to_le_bytes()); // flags (bytes 4-7)
    data.extend_from_slice(&0u32.to_le_bytes()); // numberTexts (bytes 8-11)
    data.extend_from_slice(&slide_id.to_le_bytes()); // slideIdentifier (bytes 12-15)
    build_atom(rt::SLIDE_PERSIST_ATOM, &data)
}

/// Build a TextHeaderAtom with the given text type.
pub fn build_text_header_atom(text_type: u32) -> Vec<u8> {
    build_atom(rt::TEXT_HEADER_ATOM, &text_type.to_le_bytes())
}

/// Build a TextCharsAtom from a &str (encodes as UTF-16LE).
pub fn build_text_chars_atom(text: &str) -> Vec<u8> {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let mut bytes = Vec::with_capacity(utf16.len() * 2);
    for unit in &utf16 {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    build_atom(rt::TEXT_CHARS_ATOM, &bytes)
}

/// Build a TextBytesAtom from raw bytes.
pub fn build_text_bytes_atom(data: &[u8]) -> Vec<u8> {
    build_atom(rt::TEXT_BYTES_ATOM, data)
}

/// Build a DocumentContainer with slide and/or notes SLWT containers.
pub fn build_ppt_stream_with_slwts(slide_slwt: &[u8], notes_slwt: &[u8]) -> Vec<u8> {
    let mut doc_children = Vec::new();
    if !slide_slwt.is_empty() {
        let slwt_container = build_container_with_instance(0, rt::SLIDE_LIST_WITH_TEXT, slide_slwt);
        doc_children.extend_from_slice(&slwt_container);
    }
    if !notes_slwt.is_empty() {
        let notes_container =
            build_container_with_instance(2, rt::SLIDE_LIST_WITH_TEXT, notes_slwt);
        doc_children.extend_from_slice(&notes_container);
    }
    build_container(rt::DOCUMENT, &doc_children)
}

/// Build a UserEditAtom record.
pub fn build_user_edit_atom(offset_last_edit: u32, offset_persist_dir: u32) -> Vec<u8> {
    let mut atom_data = Vec::new();
    atom_data.extend_from_slice(&0u32.to_le_bytes()); // lastSlideIdRef
    atom_data.extend_from_slice(&0u16.to_le_bytes()); // minorVersion
    atom_data.extend_from_slice(&3u16.to_le_bytes()); // majorVersion
    atom_data.extend_from_slice(&offset_last_edit.to_le_bytes());
    atom_data.extend_from_slice(&offset_persist_dir.to_le_bytes());
    atom_data.resize(28, 0);

    let ver_inst: u16 = 0;
    let rec_len = atom_data.len() as u32;
    let mut buf = Vec::new();
    buf.extend_from_slice(&ver_inst.to_le_bytes());
    buf.extend_from_slice(&rt::USER_EDIT_ATOM.to_le_bytes());
    buf.extend_from_slice(&rec_len.to_le_bytes());
    buf.extend_from_slice(&atom_data);
    buf
}

/// Build a PersistDirectoryAtom record from `(persist_id_start, &[offset])` entries.
pub fn build_persist_directory_atom(entries: &[(u32, &[u32])]) -> Vec<u8> {
    let mut atom_data = Vec::new();
    for &(persist_id_start, offsets) in entries {
        let count = offsets.len() as u32;
        let header_word = (persist_id_start & 0x000F_FFFF) | (count << 20);
        atom_data.extend_from_slice(&header_word.to_le_bytes());
        for &off in offsets {
            atom_data.extend_from_slice(&off.to_le_bytes());
        }
    }

    let ver_inst: u16 = 0;
    let rec_len = atom_data.len() as u32;
    let mut buf = Vec::new();
    buf.extend_from_slice(&ver_inst.to_le_bytes());
    buf.extend_from_slice(&rt::PERSIST_DIRECTORY_ATOM.to_le_bytes());
    buf.extend_from_slice(&rec_len.to_le_bytes());
    buf.extend_from_slice(&atom_data);
    buf
}

/// Build the "Current User" stream with PPT97 NoCrypt headerToken
/// (real-world unencrypted fixture). 0xE391C05F per [MS-PPT] section 2.3.2.
pub fn build_current_user(offset_to_current_edit: u32) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&20u32.to_le_bytes()); // size
    buf.extend_from_slice(&0xE391_C05Fu32.to_le_bytes()); // headerToken: NoCrypt
    buf.extend_from_slice(&offset_to_current_edit.to_le_bytes());
    buf.resize(20, 0);
    buf
}

/// Build a complete PPT stream from a DocumentContainer, with persist
/// directory and UserEditAtom pointing to the DocumentContainer at
/// offset 0 via persist ID 0.
///
/// Returns `(ppt_stream, current_user_stream)`.
pub fn build_ppt_stream_with_persist(doc_container: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let doc_offset = 0u32;
    let persist_atom = build_persist_directory_atom(&[(0, &[doc_offset])]);
    let persist_offset = doc_container.len() as u32;
    let user_edit = build_user_edit_atom(0, persist_offset);
    let user_edit_offset = persist_offset + persist_atom.len() as u32;

    let mut ppt_stream = Vec::new();
    ppt_stream.extend_from_slice(doc_container);
    ppt_stream.extend_from_slice(&persist_atom);
    ppt_stream.extend_from_slice(&user_edit);

    let current_user = build_current_user(user_edit_offset);
    (ppt_stream, current_user)
}

/// Build a minimal valid PPT CFB file from slide and notes SLWT data.
///
/// Wraps the PPT binary structures in a CFB archive with "PowerPoint Document"
/// and "Current User" streams.
pub fn build_ppt_cfb(slide_slwt: &[u8], notes_slwt: &[u8]) -> Vec<u8> {
    let doc_container = build_ppt_stream_with_slwts(slide_slwt, notes_slwt);
    let (ppt_stream, current_user) = build_ppt_stream_with_persist(&doc_container);
    udoc_containers::test_util::build_cfb(&[
        ("PowerPoint Document", &ppt_stream),
        ("Current User", &current_user),
    ])
}

//! CurrentUser parsing, UserEditAtom chain traversal, and PersistDirectory
//! construction for PPT binary files.
//!
//! The PPT edit model works like this:
//! 1. The "Current User" CFB stream points to the latest UserEditAtom offset
//!    in the "PowerPoint Document" stream.
//! 2. Each UserEditAtom points to its PersistDirectoryAtom and to the
//!    previous UserEditAtom (forming a backward chain).
//! 3. PersistDirectoryAtom entries map persist IDs to stream offsets.
//!
//! We walk the chain newest-to-oldest and use first-seen-wins semantics,
//! so the latest edit's persist entries take precedence.

use std::collections::{HashMap, HashSet};

use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use crate::error::{Error, Result, ResultExt};
use crate::records::{self, rt, HEADER_SIZE};
use crate::MAX_EDITS;

/// `headerToken` value in CurrentUserAtom signaling an UNENCRYPTED PPT97+ file
/// (per [MS-PPT] section 2.3.2 -- "NoCrypt"). Real-world unencrypted PPTs
/// observed in our corpus all carry this value.
const PPT97_HEADER_TOKEN_NO_CRYPT: u32 = 0xE391_C05F;

/// `headerToken` value in CurrentUserAtom signaling an ENCRYPTED PPT97+ file
/// ([MS-PPT] "Crypt"). We have no decryption support, so this triggers an
/// explicit error rather than the previous silent garbled-text fallback
/// ( round-4).
const PPT97_HEADER_TOKEN_CRYPT: u32 = 0xF3D1_C4DF;

/// Magic value for PowerPoint 95 files (unsupported).
const PP95_MAGIC: u32 = 0xE391_C9BF;

/// Minimum size of the CurrentUser record data (after the record header).
const CURRENT_USER_MIN_SIZE: usize = 20;

/// Mapping from persist IDs to byte offsets in the PowerPoint Document stream.
#[derive(Debug)]
pub struct PersistDirectory {
    entries: HashMap<u32, u64>,
    /// Persist ID of the DocumentContainer (from the latest UserEditAtom).
    pub doc_persist_id: u32,
}

impl PersistDirectory {
    /// Look up the stream offset for a persist ID.
    pub fn get(&self, persist_id: u32) -> Option<u64> {
        self.entries.get(&persist_id).copied()
    }

    /// Number of entries in the directory.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the directory is empty.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Detect whether the Current User stream starts with an 8-byte record header.
///
/// Some PPT files wrap the CurrentUserAtom data in a record header
/// (recType = 0x0FF6), others store the data directly. We check for the
/// record header and skip it if present.
fn current_user_data_start(stream: &[u8]) -> usize {
    if stream.len() >= HEADER_SIZE {
        let rec_type = u16::from_le_bytes([stream[2], stream[3]]);
        if rec_type == rt::CURRENT_USER_ATOM {
            return HEADER_SIZE;
        }
    }
    0
}

/// Parse the "Current User" CFB stream and return the offset to the latest
/// UserEditAtom in the "PowerPoint Document" stream.
///
/// Handles both variants: raw CurrentUserAtom data and record-header-wrapped
/// data. The CurrentUserAtom layout (after any record header):
/// - Bytes 0-3: size (u32 LE), must be >= 20
/// - Bytes 4-7: headerToken (0xE391C05F NoCrypt / 0xF3D1C4DF Crypt for PPT97+,
///   0xE391C9BF for PP95)
/// - Bytes 8-11: offsetToCurrentEdit (u32 LE)
pub fn parse_current_user(stream: &[u8]) -> Result<u64> {
    let skip = current_user_data_start(stream);
    let data = stream.get(skip..).unwrap_or(&[]);

    if data.len() < 12 {
        return Err(Error::new(format!(
            "Current User stream too short: {} bytes (skip={skip}), need at least 12",
            data.len()
        )));
    }

    let size = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if (size as usize) < CURRENT_USER_MIN_SIZE {
        return Err(Error::new(format!(
            "Current User size field too small: {size}, minimum is {CURRENT_USER_MIN_SIZE}"
        )));
    }

    let header_token = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    if header_token == PP95_MAGIC {
        return Err(Error::new(
            "PowerPoint 95 format is not supported (magic 0xE391C9BF)",
        ));
    }
    if header_token == PPT97_HEADER_TOKEN_CRYPT {
        return Err(Error::new(
            "PPT file is encrypted (CurrentUserAtom headerToken = 0xF3D1C4DF Crypt); decryption is not supported",
        ));
    }
    if header_token != PPT97_HEADER_TOKEN_NO_CRYPT {
        return Err(Error::new(format!(
            "unrecognized Current User headerToken: 0x{header_token:08X}, expected 0x{PPT97_HEADER_TOKEN_NO_CRYPT:08X} (NoCrypt) or 0x{PPT97_HEADER_TOKEN_CRYPT:08X} (Crypt)"
        )));
    }

    let offset = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    Ok(offset as u64)
}

/// Internal representation of a UserEditAtom's key fields.
struct UserEdit {
    offset_last_edit: u32,
    offset_persist_directory: u32,
    /// Persist ID of the DocumentContainer (bytes 16-19).
    doc_persist_id: u32,
}

/// Parse a UserEditAtom at the given offset in the PPT stream.
///
/// The UserEditAtom record has an 8-byte record header (type 0x0FF5)
/// followed by the atom data:
/// - Bytes 0-3: lastSlideIdRef (u32 LE) -- skip
/// - Bytes 4-5: minorVersion (u16 LE) -- skip
/// - Bytes 6-7: majorVersion (u16 LE) -- skip
/// - Bytes 8-11: offsetLastEdit (u32 LE)
/// - Bytes 12-15: offsetPersistDirectory (u32 LE)
/// - Bytes 16-19: docPersistIdRef (u32 LE)
fn parse_user_edit_atom(ppt_stream: &[u8], offset: u64) -> Result<UserEdit> {
    let offset = offset as usize;

    let hdr =
        records::read_record_header(ppt_stream, offset).context("reading UserEditAtom header")?;

    if hdr.rec_type != rt::USER_EDIT_ATOM {
        return Err(Error::new(format!(
            "expected UserEditAtom (0x{:04X}) at offset {offset}, found 0x{:04X}",
            rt::USER_EDIT_ATOM,
            hdr.rec_type
        )));
    }

    let data_start = offset.checked_add(HEADER_SIZE).ok_or_else(|| {
        Error::new(format!(
            "UserEditAtom data start overflow at offset {offset}"
        ))
    })?;
    // Need 20 bytes to read through docPersistIdRef
    let data_end = data_start
        .checked_add(20)
        .ok_or_else(|| Error::new(format!("UserEditAtom data end overflow at offset {offset}")))?;

    let atom_data = ppt_stream.get(data_start..data_end).ok_or_else(|| {
        Error::new(format!(
            "truncated UserEditAtom at offset {offset}: need 20 bytes of data, have {}",
            ppt_stream.len().saturating_sub(data_start)
        ))
    })?;

    let offset_last_edit =
        u32::from_le_bytes([atom_data[8], atom_data[9], atom_data[10], atom_data[11]]);
    let offset_persist_dir =
        u32::from_le_bytes([atom_data[12], atom_data[13], atom_data[14], atom_data[15]]);
    let doc_persist_id =
        u32::from_le_bytes([atom_data[16], atom_data[17], atom_data[18], atom_data[19]]);

    Ok(UserEdit {
        offset_last_edit,
        offset_persist_directory: offset_persist_dir,
        doc_persist_id,
    })
}

/// Parse a PersistDirectoryAtom and insert entries into `map`.
///
/// Uses first-seen-wins: if a persist ID is already in the map, skip it.
/// This gives newest-edit-wins semantics when called newest-to-oldest.
fn parse_persist_directory_atom(
    ppt_stream: &[u8],
    offset: usize,
    map: &mut HashMap<u32, u64>,
    diag: &dyn DiagnosticsSink,
) -> Result<()> {
    let hdr = records::read_record_header(ppt_stream, offset)
        .context("reading PersistDirectoryAtom header")?;

    if hdr.rec_type != rt::PERSIST_DIRECTORY_ATOM {
        return Err(Error::new(format!(
            "expected PersistDirectoryAtom (0x{:04X}) at offset {offset}, found 0x{:04X}",
            rt::PERSIST_DIRECTORY_ATOM,
            hdr.rec_type
        )));
    }

    let data_start = offset.checked_add(HEADER_SIZE).ok_or_else(|| {
        Error::new(format!(
            "PersistDirectoryAtom data start overflow at offset {offset}"
        ))
    })?;
    let data_end = data_start
        .checked_add(hdr.rec_len as usize)
        .ok_or_else(|| {
            Error::new(format!(
                "PersistDirectoryAtom data end overflow at offset {offset}: rec_len={}",
                hdr.rec_len
            ))
        })?;
    let data_end = data_end.min(ppt_stream.len());

    let atom_data = ppt_stream.get(data_start..data_end).ok_or_else(|| {
        Error::new(format!(
            "PersistDirectoryAtom data region out of bounds at offset {offset}"
        ))
    })?;

    let mut pos = 0;
    while pos + 4 <= atom_data.len() {
        let header_word = u32::from_le_bytes([
            atom_data[pos],
            atom_data[pos + 1],
            atom_data[pos + 2],
            atom_data[pos + 3],
        ]);
        pos += 4;

        let persist_id_start = header_word & 0x000F_FFFF; // bits 0-19
        let count = (header_word >> 20) as usize; // bits 20-31

        let needed = count.checked_mul(4).ok_or_else(|| {
            Error::new(format!(
                "persist entry count overflow: count={count} at offset {}",
                offset + HEADER_SIZE + pos
            ))
        })?;

        if pos + needed > atom_data.len() {
            diag.warning(Warning::new(
                "TruncatedPersistEntry",
                format!(
                    "persist directory entry truncated at offset {}: need {} bytes for {count} offsets, have {}",
                    offset + HEADER_SIZE + pos,
                    needed,
                    atom_data.len() - pos
                ),
            ).at_offset((offset + HEADER_SIZE + pos) as u64));

            // Parse as many offsets as we can
            let available = (atom_data.len() - pos) / 4;
            for i in 0..available {
                let entry_pos = pos + i * 4;
                let stream_offset = u32::from_le_bytes([
                    atom_data[entry_pos],
                    atom_data[entry_pos + 1],
                    atom_data[entry_pos + 2],
                    atom_data[entry_pos + 3],
                ]);
                let pid = persist_id_start
                    .checked_add(i as u32)
                    .ok_or_else(|| Error::new("persist ID overflow"))?;

                if (stream_offset as usize) >= ppt_stream.len() {
                    diag.warning(Warning::new(
                        "InvalidPersistOffset",
                        format!(
                            "persist ID {pid} offset {stream_offset} exceeds stream length {}",
                            ppt_stream.len()
                        ),
                    ));
                }
                map.entry(pid).or_insert(stream_offset as u64);
            }
            break;
        }

        for i in 0..count {
            let entry_pos = pos + i * 4;
            let stream_offset = u32::from_le_bytes([
                atom_data[entry_pos],
                atom_data[entry_pos + 1],
                atom_data[entry_pos + 2],
                atom_data[entry_pos + 3],
            ]);
            let pid = persist_id_start
                .checked_add(i as u32)
                .ok_or_else(|| Error::new("persist ID overflow"))?;

            if (stream_offset as usize) >= ppt_stream.len() {
                diag.warning(Warning::new(
                    "InvalidPersistOffset",
                    format!(
                        "persist ID {pid} offset {stream_offset} exceeds stream length {}",
                        ppt_stream.len()
                    ),
                ));
            }
            map.entry(pid).or_insert(stream_offset as u64);
        }

        pos += needed;
    }

    Ok(())
}

/// Build the merged persist directory by walking the UserEditAtom chain.
///
/// Starts at `current_edit_offset` (from CurrentUser) and follows the
/// chain backward through offsetLastEdit until reaching 0.
///
/// Uses first-seen-wins semantics: since we walk newest to oldest, the
/// latest edit's entries take precedence for any given persist ID.
pub fn build_persist_directory(
    ppt_stream: &[u8],
    current_edit_offset: u64,
    diag: &dyn DiagnosticsSink,
) -> Result<PersistDirectory> {
    let mut entries = HashMap::new();
    let mut visited = HashSet::new();
    let mut edit_offset = current_edit_offset;
    let mut edit_count = 0usize;
    // doc_persist_id from the latest (first) UserEditAtom in the chain.
    let mut doc_persist_id: u32 = 0;

    loop {
        if edit_offset == 0 {
            break;
        }

        // Cycle detection
        if !visited.insert(edit_offset) {
            diag.warning(
                Warning::new(
                    "CircularEditChain",
                    format!("UserEditAtom chain cycle detected at offset {edit_offset}"),
                )
                .at_offset(edit_offset),
            );
            break;
        }

        // Chain depth cap
        edit_count += 1;
        if edit_count > MAX_EDITS {
            diag.warning(
                Warning::new(
                    "EditChainTooLong",
                    format!("UserEditAtom chain exceeded {MAX_EDITS} entries, stopping"),
                )
                .at_offset(edit_offset),
            );
            break;
        }

        let user_edit = parse_user_edit_atom(ppt_stream, edit_offset)
            .context(format!("parsing UserEditAtom at offset {edit_offset}"))?;

        // Capture docPersistIdRef from the latest (first) edit in the chain.
        if edit_count == 1 {
            doc_persist_id = user_edit.doc_persist_id;
        }

        parse_persist_directory_atom(
            ppt_stream,
            user_edit.offset_persist_directory as usize,
            &mut entries,
            diag,
        )
        .context(format!(
            "parsing PersistDirectoryAtom at offset {}",
            user_edit.offset_persist_directory
        ))?;

        edit_offset = user_edit.offset_last_edit as u64;
    }

    Ok(PersistDirectory {
        entries,
        doc_persist_id,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use udoc_core::diagnostics::{CollectingDiagnostics, DiagnosticsSink, NullDiagnostics};

    use super::*;
    use crate::test_util::*;

    fn null_diag() -> Arc<dyn DiagnosticsSink> {
        Arc::new(NullDiagnostics)
    }

    /// Build a PPT stream with one UserEditAtom and one PersistDirectory.
    fn build_single_edit_stream(persist_entries: &[(u32, &[u32])]) -> (Vec<u8>, u64) {
        let persist_atom = build_persist_directory_atom(persist_entries);
        let user_edit_offset = persist_atom.len() as u32;
        let user_edit = build_user_edit_atom(0, 0);

        let mut stream = Vec::new();
        stream.extend_from_slice(&persist_atom);
        stream.extend_from_slice(&user_edit);

        (stream, user_edit_offset as u64)
    }

    #[test]
    fn current_user_parses_ppt97() {
        let data = build_current_user(0x1234);
        let offset = parse_current_user(&data).unwrap();
        assert_eq!(offset, 0x1234);
    }

    #[test]
    fn current_user_with_record_header() {
        // Build a Current User stream wrapped in a record header (recType=0x0FF6).
        let raw_data = build_current_user(0x5678);
        let wrapped = build_atom(rt::CURRENT_USER_ATOM, &raw_data);

        let offset = parse_current_user(&wrapped).unwrap();
        assert_eq!(offset, 0x5678);
    }

    #[test]
    fn current_user_rejects_pp95() {
        let mut data = build_current_user(0);
        data[4..8].copy_from_slice(&PP95_MAGIC.to_le_bytes());
        let result = parse_current_user(&data);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("PowerPoint 95"));
    }

    /// Round-4 audit: encrypted PPT files carry headerToken=0xF3D1C4DF (Crypt)
    /// in CurrentUserAtom. Pre-fix the parser had the magic constants swapped
    /// and accepted Crypt as the expected value, then later parsing produced
    /// garbled output. Now we detect Crypt and return a clear error.
    #[test]
    fn current_user_rejects_encrypted_crypt_token() {
        let mut data = build_current_user(0);
        data[4..8].copy_from_slice(&PPT97_HEADER_TOKEN_CRYPT.to_le_bytes());
        let err = parse_current_user(&data).expect_err("expected encrypted error");
        let msg = format!("{err}");
        assert!(
            msg.to_lowercase().contains("encrypted"),
            "error should mention encryption: {msg}"
        );
    }

    #[test]
    fn single_edit_persist_directory() {
        let (stream, edit_offset) = build_single_edit_stream(&[(1, &[100, 200, 300])]);

        let dir = build_persist_directory(&stream, edit_offset, null_diag().as_ref()).unwrap();
        assert_eq!(dir.len(), 3);
        assert_eq!(dir.get(1), Some(100));
        assert_eq!(dir.get(2), Some(200));
        assert_eq!(dir.get(3), Some(300));
        assert_eq!(dir.get(0), None);
        assert_eq!(dir.get(4), None);
    }

    #[test]
    fn multi_edit_chain_newest_wins() {
        let persist_dir_1 = build_persist_directory_atom(&[(1, &[100, 200])]);
        let pd1_len = persist_dir_1.len();

        let user_edit_1 = build_user_edit_atom(0, 0);
        let ue1_offset = pd1_len;
        let ue1_end = ue1_offset + user_edit_1.len();

        let persist_dir_2 = build_persist_directory_atom(&[(1, &[500])]);
        let pd2_offset = ue1_end;
        let pd2_end = pd2_offset + persist_dir_2.len();

        let user_edit_2 = build_user_edit_atom(ue1_offset as u32, pd2_offset as u32);
        let ue2_offset = pd2_end;

        let mut stream = Vec::new();
        stream.extend_from_slice(&persist_dir_1);
        stream.extend_from_slice(&user_edit_1);
        stream.extend_from_slice(&persist_dir_2);
        stream.extend_from_slice(&user_edit_2);

        let dir =
            build_persist_directory(&stream, ue2_offset as u64, null_diag().as_ref()).unwrap();

        assert_eq!(dir.get(1), Some(500));
        assert_eq!(dir.get(2), Some(200));
        assert_eq!(dir.len(), 2);
    }

    #[test]
    fn circular_edit_chain_detected() {
        let persist_atom = build_persist_directory_atom(&[(1, &[0])]);
        let ue_offset = persist_atom.len() as u32;
        let user_edit = build_user_edit_atom(ue_offset, 0);

        let mut stream = Vec::new();
        stream.extend_from_slice(&persist_atom);
        stream.extend_from_slice(&user_edit);

        let diag = Arc::new(CollectingDiagnostics::new());
        let dir = build_persist_directory(&stream, ue_offset as u64, diag.as_ref()).unwrap();

        assert_eq!(dir.len(), 1);

        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "CircularEditChain"),
            "expected CircularEditChain warning, got: {warnings:?}"
        );
    }

    #[test]
    fn truncated_persist_directory_warns() {
        let mut atom_data = Vec::new();
        let header_word: u32 = (1 & 0x000F_FFFF) | (3 << 20);
        atom_data.extend_from_slice(&header_word.to_le_bytes());
        atom_data.extend_from_slice(&100u32.to_le_bytes());

        let ver_inst: u16 = 0;
        let rec_len = atom_data.len() as u32;
        let mut persist_record = Vec::new();
        persist_record.extend_from_slice(&ver_inst.to_le_bytes());
        persist_record.extend_from_slice(&rt::PERSIST_DIRECTORY_ATOM.to_le_bytes());
        persist_record.extend_from_slice(&rec_len.to_le_bytes());
        persist_record.extend_from_slice(&atom_data);

        let ue_offset = persist_record.len() as u32;
        let user_edit = build_user_edit_atom(0, 0);

        let mut stream = Vec::new();
        stream.extend_from_slice(&persist_record);
        stream.extend_from_slice(&user_edit);

        let diag = Arc::new(CollectingDiagnostics::new());
        let dir = build_persist_directory(&stream, ue_offset as u64, diag.as_ref()).unwrap();

        assert_eq!(dir.len(), 1);
        assert_eq!(dir.get(1), Some(100));

        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "TruncatedPersistEntry"),
            "expected TruncatedPersistEntry warning, got: {warnings:?}"
        );
    }

    #[test]
    fn current_user_too_short() {
        let data = vec![0u8; 8];
        let result = parse_current_user(&data);
        assert!(result.is_err());
    }

    #[test]
    fn empty_persist_directory() {
        let persist_atom = build_persist_directory_atom(&[]);
        let ue_offset = persist_atom.len() as u32;
        let user_edit = build_user_edit_atom(0, 0);

        let mut stream = Vec::new();
        stream.extend_from_slice(&persist_atom);
        stream.extend_from_slice(&user_edit);

        let dir = build_persist_directory(&stream, ue_offset as u64, null_diag().as_ref()).unwrap();
        assert!(dir.is_empty());
        assert_eq!(dir.len(), 0);
    }
}

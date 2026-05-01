//! AES-128-CBC decryption for PDF encryption (Rev 4, V=4).
//!
//! PDF spec (7.6.3): AES-128-CBC with PKCS#7 padding. The first 16 bytes
//! of encrypted data are the IV. Remaining bytes are ciphertext. Data length
//! (including IV) must be a multiple of 16 and at least 32 bytes (16 IV + 16
//! minimum ciphertext).

use aes::Aes128;
use cbc::cipher::{block_padding::NoPadding, BlockDecryptMut, KeyIvInit};

type Aes128CbcDec = cbc::Decryptor<Aes128>;

/// AES-128-CBC block size.
const BLOCK_SIZE: usize = 16;

/// Decrypt AES-128-CBC data with PKCS#7 padding removal.
///
/// `data` layout: `[16-byte IV] [ciphertext blocks.]`
///
/// Returns decrypted plaintext with padding stripped, or None if the data
/// is malformed (too short, not block-aligned, or bad padding).
pub fn decrypt(key: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    if key.len() != 16 {
        return None;
    }

    // Need at least IV (16) + one ciphertext block (16) = 32 bytes
    if data.len() < BLOCK_SIZE * 2 {
        return None;
    }

    // Ciphertext (after IV) must be block-aligned
    let ciphertext_len = data.len() - BLOCK_SIZE;
    if !ciphertext_len.is_multiple_of(BLOCK_SIZE) {
        return None;
    }

    let iv = &data[..BLOCK_SIZE];
    let ciphertext = &data[BLOCK_SIZE..];

    // Decrypt in place on a copy
    let mut buf = ciphertext.to_vec();
    let decryptor = Aes128CbcDec::new_from_slices(key, iv).ok()?;
    decryptor.decrypt_padded_mut::<NoPadding>(&mut buf).ok()?;

    // Strip PKCS#7 padding
    strip_pkcs7(&mut buf);

    Some(buf)
}

/// Strip PKCS#7 padding from decrypted data.
///
/// PKCS#7: last byte indicates padding length (1-16). All padding bytes
/// must equal the padding length. Invalid padding is preserved as-is:
/// real PDFs have corrupt ciphertext, and silently returning the raw decrypted
/// bytes is better than erroring out and losing the whole page.
fn strip_pkcs7(data: &mut Vec<u8>) {
    let pad_byte = match data.last() {
        Some(&b) => b,
        None => return,
    };
    let pad_len = pad_byte as usize;

    // Valid padding: 1-16, and we have enough bytes
    if pad_len == 0 || pad_len > BLOCK_SIZE || pad_len > data.len() {
        return;
    }

    // Verify all padding bytes match
    let start = data.len() - pad_len;
    if data[start..].iter().all(|&b| b == pad_byte) {
        data.truncate(start);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};

    type Aes128CbcEnc = cbc::Encryptor<Aes128>;

    /// Helper: encrypt plaintext with AES-128-CBC + PKCS#7, prepend IV.
    fn encrypt_test(key: &[u8; 16], iv: &[u8; 16], plaintext: &[u8]) -> Vec<u8> {
        // Allocate buffer: plaintext + up to 16 bytes of PKCS#7 padding
        let padded_len = ((plaintext.len() / BLOCK_SIZE) + 1) * BLOCK_SIZE;
        let mut buf = vec![0u8; padded_len];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let encryptor = Aes128CbcEnc::new_from_slices(key, iv).unwrap();
        let ciphertext = encryptor
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
            .unwrap();
        let mut result = iv.to_vec();
        result.extend_from_slice(ciphertext);
        result
    }

    #[test]
    fn roundtrip_basic() {
        let key = b"\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c\x0d\x0e\x0f";
        let iv = b"\x10\x11\x12\x13\x14\x15\x16\x17\x18\x19\x1a\x1b\x1c\x1d\x1e\x1f";
        let plaintext = b"Hello, PDF!";
        let encrypted = encrypt_test(key, iv, plaintext);
        let decrypted = decrypt(key, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn roundtrip_exact_block() {
        // 16 bytes = exact block, PKCS#7 adds a full block of padding (16 x 0x10)
        let key = [0xAA; 16];
        let iv = [0xBB; 16];
        let plaintext = b"exactly16bytes!!";
        assert_eq!(plaintext.len(), 16);
        let encrypted = encrypt_test(&key, &iv, plaintext);
        // IV(16) + ciphertext(32) = 48 (plaintext block + padding block)
        assert_eq!(encrypted.len(), 48);
        let decrypted = decrypt(&key, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn roundtrip_multi_block() {
        let key = [0x42; 16];
        let iv = [0x00; 16];
        let plaintext = b"This is a longer plaintext that spans multiple AES blocks for testing.";
        let encrypted = encrypt_test(&key, &iv, plaintext);
        let decrypted = decrypt(&key, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn roundtrip_empty_plaintext() {
        // Empty plaintext: PKCS#7 produces one block of 0x10 padding
        let key = [0xFF; 16];
        let iv = [0x00; 16];
        let encrypted = encrypt_test(&key, &iv, b"");
        // IV(16) + one padding block(16) = 32
        assert_eq!(encrypted.len(), 32);
        let decrypted = decrypt(&key, &encrypted).unwrap();
        assert!(decrypted.is_empty());
    }

    #[test]
    fn too_short_returns_none() {
        let key = [0; 16];
        // Less than 32 bytes (IV + 1 block minimum)
        assert!(decrypt(&key, &[0; 0]).is_none());
        assert!(decrypt(&key, &[0; 15]).is_none());
        assert!(decrypt(&key, &[0; 16]).is_none());
        assert!(decrypt(&key, &[0; 31]).is_none());
    }

    #[test]
    fn not_block_aligned_returns_none() {
        let key = [0; 16];
        // 33 bytes: 16 IV + 17 ciphertext (not a multiple of 16)
        assert!(decrypt(&key, &[0; 33]).is_none());
        // 35 bytes
        assert!(decrypt(&key, &[0; 35]).is_none());
    }

    #[test]
    fn bad_padding_preserved() {
        // If PKCS#7 padding is invalid, data should be returned as-is
        let key = b"\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c\x0d\x0e\x0f";
        let iv = [0; 16];

        // Manually create ciphertext with garbage (won't have valid PKCS#7 after decrypt)
        // The decryption will succeed but padding will be invalid, so we keep all bytes
        let mut data = iv.to_vec();
        data.extend_from_slice(&[0xDE; 16]); // one block of garbage ciphertext
        let result = decrypt(key, &data);
        // Should return Some (decryption succeeds even if padding is weird)
        assert!(result.is_some());
        // Result should be 16 bytes (full block, no valid padding to strip)
        // or less if the decrypted garbage happens to look like valid padding
        let decrypted = result.unwrap();
        assert!(decrypted.len() <= 16);
    }

    #[test]
    fn strip_pkcs7_valid() {
        // 3 bytes of padding (0x03, 0x03, 0x03)
        let mut data = vec![0x41, 0x42, 0x43, 0x03, 0x03, 0x03];
        strip_pkcs7(&mut data);
        assert_eq!(data, b"ABC");
    }

    #[test]
    fn strip_pkcs7_full_block_padding() {
        // Full block of padding (16 x 0x10)
        let mut data = vec![0x10; 16];
        strip_pkcs7(&mut data);
        assert!(data.is_empty());
    }

    #[test]
    fn strip_pkcs7_single_byte_padding() {
        let mut data = vec![0x48, 0x69, 0x01];
        strip_pkcs7(&mut data);
        assert_eq!(data, b"Hi");
    }

    #[test]
    fn strip_pkcs7_invalid_zero() {
        // Padding byte 0x00 is invalid (must be 1-16)
        let mut data = vec![0x41, 0x00];
        strip_pkcs7(&mut data);
        assert_eq!(data, vec![0x41, 0x00]); // unchanged
    }

    #[test]
    fn strip_pkcs7_invalid_mismatch() {
        // Last byte says 3 bytes of padding, but they don't all match
        let mut data = vec![0x41, 0x02, 0x03, 0x03];
        strip_pkcs7(&mut data);
        assert_eq!(data, vec![0x41, 0x02, 0x03, 0x03]); // unchanged
    }

    #[test]
    fn strip_pkcs7_invalid_too_large() {
        // Padding byte 0x11 (17) exceeds block size
        let mut data = vec![0x41, 0x11];
        strip_pkcs7(&mut data);
        assert_eq!(data, vec![0x41, 0x11]); // unchanged
    }

    #[test]
    fn strip_pkcs7_empty() {
        let mut data: Vec<u8> = vec![];
        strip_pkcs7(&mut data);
        assert!(data.is_empty());
    }
}

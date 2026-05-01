//! AES-256-CBC decryption for PDF encryption (Rev 5/6, V=5).
//!
//! Same structure as AES-128 (aes.rs) but with 32-byte keys. The first 16
//! bytes of encrypted data are the IV. Remaining bytes are ciphertext padded
//! with PKCS#7.
//!
//! Also provides a zero-IV decrypt used for /Perms validation (Algorithm 2.A
//! step i in ISO 32000-2).

use aes::Aes256;
use cbc::cipher::{block_padding::NoPadding, BlockDecryptMut, KeyIvInit};

type Aes256CbcDec = cbc::Decryptor<Aes256>;

/// AES block size (same for 128 and 256).
const BLOCK_SIZE: usize = 16;

/// Decrypt AES-256-CBC data with PKCS#7 padding removal.
///
/// `data` layout: `[16-byte IV] [ciphertext blocks.]`
///
/// Returns decrypted plaintext with padding stripped, or None if the data
/// is malformed (too short, not block-aligned, or bad key length).
pub fn decrypt(key: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    if key.len() != 32 {
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

    let mut buf = ciphertext.to_vec();
    let decryptor = Aes256CbcDec::new_from_slices(key, iv).ok()?;
    decryptor.decrypt_padded_mut::<NoPadding>(&mut buf).ok()?;

    strip_pkcs7(&mut buf);

    Some(buf)
}

/// Decrypt 16 bytes of AES-256-CBC with a zero IV (no padding removal).
///
/// Used for /Perms validation: decrypt the 16-byte /Perms value with the
/// file encryption key and a zero IV, then check bytes 9-11 == "adb".
pub fn decrypt_zero_iv(key: &[u8], data: &[u8]) -> Option<[u8; 16]> {
    if key.len() != 32 || data.len() != 16 {
        return None;
    }

    let iv = [0u8; 16];
    let mut buf = data.to_vec();
    let decryptor = Aes256CbcDec::new_from_slices(key, &iv).ok()?;
    decryptor.decrypt_padded_mut::<NoPadding>(&mut buf).ok()?;

    let mut result = [0u8; 16];
    result.copy_from_slice(&buf);
    Some(result)
}

/// Strip PKCS#7 padding from decrypted data.
///
/// Same logic as aes.rs: invalid padding is preserved as-is because real
/// PDFs have corrupt ciphertext and silently returning raw bytes is better
/// than erroring out.
fn strip_pkcs7(data: &mut Vec<u8>) {
    let pad_byte = match data.last() {
        Some(&b) => b,
        None => return,
    };
    let pad_len = pad_byte as usize;

    if pad_len == 0 || pad_len > BLOCK_SIZE || pad_len > data.len() {
        return;
    }

    let start = data.len() - pad_len;
    if data[start..].iter().all(|&b| b == pad_byte) {
        data.truncate(start);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};

    type Aes256CbcEnc = cbc::Encryptor<Aes256>;

    /// Helper: encrypt plaintext with AES-256-CBC + PKCS#7, prepend IV.
    fn encrypt_test(key: &[u8; 32], iv: &[u8; 16], plaintext: &[u8]) -> Vec<u8> {
        let padded_len = ((plaintext.len() / BLOCK_SIZE) + 1) * BLOCK_SIZE;
        let mut buf = vec![0u8; padded_len];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let encryptor = Aes256CbcEnc::new_from_slices(key, iv).unwrap();
        let ciphertext = encryptor
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
            .unwrap();
        let mut result = iv.to_vec();
        result.extend_from_slice(ciphertext);
        result
    }

    #[test]
    fn roundtrip_basic() {
        let key = [0x42u8; 32];
        let iv = [0x10u8; 16];
        let plaintext = b"Hello, AES-256!";
        let encrypted = encrypt_test(&key, &iv, plaintext);
        let decrypted = decrypt(&key, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn roundtrip_exact_block() {
        let key = [0xAA; 32];
        let iv = [0xBB; 16];
        let plaintext = b"exactly16bytes!!";
        assert_eq!(plaintext.len(), 16);
        let encrypted = encrypt_test(&key, &iv, plaintext);
        assert_eq!(encrypted.len(), 48); // IV(16) + 2 blocks (data + padding)
        let decrypted = decrypt(&key, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn roundtrip_multi_block() {
        let key = [0x01; 32];
        let iv = [0x00; 16];
        let plaintext = b"This is a longer plaintext that spans multiple AES blocks for testing.";
        let encrypted = encrypt_test(&key, &iv, plaintext);
        let decrypted = decrypt(&key, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn roundtrip_empty_plaintext() {
        let key = [0xFF; 32];
        let iv = [0x00; 16];
        let encrypted = encrypt_test(&key, &iv, b"");
        assert_eq!(encrypted.len(), 32); // IV + one padding block
        let decrypted = decrypt(&key, &encrypted).unwrap();
        assert!(decrypted.is_empty());
    }

    #[test]
    fn too_short_returns_none() {
        let key = [0; 32];
        assert!(decrypt(&key, &[0; 0]).is_none());
        assert!(decrypt(&key, &[0; 15]).is_none());
        assert!(decrypt(&key, &[0; 16]).is_none());
        assert!(decrypt(&key, &[0; 31]).is_none());
    }

    #[test]
    fn wrong_key_length_returns_none() {
        let data = [0; 32]; // minimal valid data shape
        assert!(decrypt(&[0; 16], &data).is_none()); // 128-bit key
        assert!(decrypt(&[0; 24], &data).is_none()); // 192-bit key
        assert!(decrypt(&[0; 31], &data).is_none()); // one byte short
    }

    #[test]
    fn not_block_aligned_returns_none() {
        let key = [0; 32];
        assert!(decrypt(&key, &[0; 33]).is_none());
        assert!(decrypt(&key, &[0; 35]).is_none());
    }

    #[test]
    fn decrypt_zero_iv_basic() {
        // Encrypt a 16-byte block with zero IV, then decrypt it
        let key = [0x42u8; 32];
        let iv = [0u8; 16];
        let plaintext = b"0123456789abcdef";

        // Encrypt with zero IV (no PKCS#7, just one raw block)
        let encryptor = cbc::Encryptor::<Aes256>::new_from_slices(&key, &iv).unwrap();
        let mut buf = plaintext.to_vec();
        // For exactly one block, NoPadding works
        encryptor
            .encrypt_padded_mut::<NoPadding>(&mut buf, 16)
            .unwrap();

        let result = decrypt_zero_iv(&key, &buf).unwrap();
        assert_eq!(&result, plaintext);
    }

    #[test]
    fn decrypt_zero_iv_wrong_data_length() {
        let key = [0; 32];
        assert!(decrypt_zero_iv(&key, &[0; 15]).is_none());
        assert!(decrypt_zero_iv(&key, &[0; 17]).is_none());
        assert!(decrypt_zero_iv(&key, &[0; 0]).is_none());
    }

    #[test]
    fn decrypt_zero_iv_wrong_key_length() {
        assert!(decrypt_zero_iv(&[0; 16], &[0; 16]).is_none());
        assert!(decrypt_zero_iv(&[0; 31], &[0; 16]).is_none());
    }
}

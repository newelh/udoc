//! RC4 stream cipher implementation for PDF decryption.
//!
//! PDF uses RC4 with variable-length keys (5-16 bytes). The RustCrypto rc4
//! crate requires compile-time key sizes, so we implement it directly.
//! RC4 is a simple stream cipher: KSA (key scheduling) + PRGA (keystream).

/// Decrypt (or encrypt) data using RC4.
///
/// RC4 is symmetric, so encrypt and decrypt are the same operation.
/// Returns data unchanged if key is empty (defense in depth).
pub fn decrypt(key: &[u8], data: &[u8]) -> Vec<u8> {
    if key.is_empty() || data.is_empty() {
        return data.to_vec();
    }

    // Key-scheduling algorithm (KSA)
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j: u8 = 0;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }

    // Pseudo-random generation algorithm (PRGA)
    let mut output = data.to_vec();
    let mut i: u8 = 0;
    j = 0;
    for byte in &mut output {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        let k = s[(s[i as usize].wrapping_add(s[j as usize])) as usize];
        *byte ^= k;
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rc4_roundtrip() {
        let key = b"secret";
        let plaintext = b"hello world";
        let ciphertext = decrypt(key, plaintext);
        assert_ne!(&ciphertext[..], plaintext);
        let decrypted = decrypt(key, &ciphertext);
        assert_eq!(&decrypted[..], plaintext);
    }

    #[test]
    fn test_rc4_empty_data() {
        let key = b"key";
        let result = decrypt(key, b"");
        assert!(result.is_empty());
    }

    #[test]
    fn test_rc4_empty_key_returns_data_unchanged() {
        let result = decrypt(b"", b"hello");
        assert_eq!(&result[..], b"hello");
    }

    #[test]
    fn test_rc4_known_vector() {
        // RC4 test vector: Key = "Key", Plaintext = "Plaintext"
        let key = b"Key";
        let plaintext = b"Plaintext";
        let ciphertext = decrypt(key, plaintext);
        assert_eq!(
            ciphertext,
            [0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]
        );
    }

    #[test]
    fn test_rc4_known_vector_wiki() {
        // Key = "Wiki", Plaintext = "pedia"
        let ciphertext = decrypt(b"Wiki", b"pedia");
        assert_eq!(ciphertext, [0x10, 0x21, 0xBF, 0x04, 0x20]);
    }

    #[test]
    fn test_rc4_known_vector_secret() {
        // Key = "Secret", Plaintext = "Attack at dawn"
        let ciphertext = decrypt(b"Secret", b"Attack at dawn");
        assert_eq!(
            ciphertext,
            [0x45, 0xA0, 0x1F, 0x64, 0x5F, 0xC3, 0x5B, 0x38, 0x35, 0x52, 0x54, 0x4B, 0x9B, 0xF5]
        );
    }
}

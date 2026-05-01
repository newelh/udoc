//! Key derivation and password validation for PDF encryption.
//!
//! Implements PDF spec Algorithms 1-5 for the Standard security handler (R2-4),
//! plus ISO 32000-2 Algorithms 2.A/2.B for R5/R6 (AES-256).

use super::EncryptionConfig;
use crate::object::ObjRef;
use md5::{Digest, Md5};
use sha2::Sha256;

/// Constant-time byte slice comparison to prevent timing side-channels
/// in password validation. Returns true iff slices are equal length and
/// contain identical bytes.
///
/// The early return on length mismatch leaks the length via timing, but this
/// is acceptable: the compared values (/U, /O) are always fixed 32-byte
/// strings defined by the PDF spec, so their lengths are not secret.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// The 32-byte padding string from PDF spec Table 3.19.
const PASSWORD_PADDING: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// Pad or truncate a password to exactly 32 bytes using the PDF padding string.
pub fn pad_password(password: &[u8]) -> [u8; 32] {
    let mut padded = [0u8; 32];
    let take = password.len().min(32);
    padded[..take].copy_from_slice(&password[..take]);
    if take < 32 {
        padded[take..].copy_from_slice(&PASSWORD_PADDING[..32 - take]);
    }
    padded
}

/// Derive the document encryption key (Algorithm 2).
///
/// For Rev 2: single MD5 pass, truncate to key_length/8 bytes.
/// For Rev 3: MD5 50 additional times, truncate to key_length/8 bytes.
pub fn derive_document_key(password: &[u8], config: &EncryptionConfig, file_id: &[u8]) -> Vec<u8> {
    debug_assert!(config.key_length > 0 && config.key_length.is_multiple_of(8));
    let padded = pad_password(password);
    let key_bytes = config.key_length / 8;

    let mut hasher = Md5::new();
    hasher.update(padded);
    hasher.update(&config.o);
    hasher.update((config.p as u32).to_le_bytes());
    hasher.update(file_id);

    // If EncryptMetadata is false (Rev 4 only, but handle it)
    if !config.encrypt_metadata {
        hasher.update([0xFF, 0xFF, 0xFF, 0xFF]);
    }

    let mut hash = hasher.finalize().to_vec();

    // Rev 3+: re-hash 50 times
    if config.r >= 3 {
        for _ in 0..50 {
            let mut h = Md5::new();
            h.update(&hash[..key_bytes]);
            hash = h.finalize().to_vec();
        }
    }

    hash.truncate(key_bytes);
    hash
}

/// Hash the document key and object reference into an MD5 hasher.
///
/// Shared prefix for both RC4 and AES per-object key derivation (Algorithm 1):
/// MD5 input = document_key + obj_num_3LE + gen_num_2LE.
fn hash_key_and_obj_ref(hasher: &mut Md5, document_key: &[u8], obj_ref: ObjRef) {
    hasher.update(document_key);

    // Object number as 3 little-endian bytes
    let obj_num = obj_ref.num;
    hasher.update([
        (obj_num & 0xFF) as u8,
        ((obj_num >> 8) & 0xFF) as u8,
        ((obj_num >> 16) & 0xFF) as u8,
    ]);

    // Generation number as 2 little-endian bytes
    let gen = obj_ref.gen;
    hasher.update([(gen & 0xFF) as u8, ((gen >> 8) & 0xFF) as u8]);
}

/// Derive the per-object encryption key (Algorithm 1).
///
/// MD5(document_key + obj_num_3LE + gen_num_2LE), truncated to min(key_len + 5, 16).
/// Returns `(effective_len, full_hash)` so callers can slice without heap allocation.
pub fn per_object_key(
    document_key: &[u8],
    obj_ref: ObjRef,
    key_length: usize,
) -> (usize, [u8; 16]) {
    debug_assert!(key_length > 0 && key_length.is_multiple_of(8));
    let key_bytes = key_length / 8;
    let n = (key_bytes + 5).min(16);

    let mut hasher = Md5::new();
    hash_key_and_obj_ref(&mut hasher, document_key, obj_ref);

    let hash = hasher.finalize();
    (n, hash.into())
}

/// Derive the per-object encryption key for AES (Algorithm 1 with "sAlT" suffix).
///
/// Same as `per_object_key` but appends the 4-byte "sAlT" marker (0x73414C54)
/// after the generation number bytes, per PDF spec 7.6.3. The effective key
/// length is always 16 bytes (full MD5 hash) for AES-128. No `key_length`
/// parameter because V=4 always uses 128-bit keys.
pub fn per_object_key_aes(document_key: &[u8], obj_ref: ObjRef) -> [u8; 16] {
    // V=4 always means 128-bit (16-byte) document key. parse_encrypt_dict enforces this.
    debug_assert!(
        document_key.len() == 16,
        "AES-128 requires 16-byte document key"
    );
    let mut hasher = Md5::new();
    hash_key_and_obj_ref(&mut hasher, document_key, obj_ref);

    // AES marker: "sAlT" (0x73, 0x41, 0x6C, 0x54)
    hasher.update([0x73, 0x41, 0x6C, 0x54]);

    hasher.finalize().into()
}

/// Validate a user password (Algorithm 4 for Rev 2, Algorithm 5 for Rev 3).
///
/// Returns the document key if the password is valid, None otherwise.
pub fn validate_user_password(
    password: &[u8],
    config: &EncryptionConfig,
    file_id: &[u8],
) -> Option<Vec<u8>> {
    let doc_key = derive_document_key(password, config, file_id);

    if config.r == 2 {
        // Algorithm 4: RC4-encrypt the padding with the doc key, compare to /U
        let encrypted = super::rc4::decrypt(&doc_key, &PASSWORD_PADDING);
        if constant_time_eq(&encrypted, &config.u) {
            return Some(doc_key);
        }
    } else {
        // Algorithm 5 (Rev 3): MD5(padding + file_id), then 20 rounds of RC4
        let mut hasher = Md5::new();
        hasher.update(PASSWORD_PADDING);
        hasher.update(file_id);
        let mut result = hasher.finalize().to_vec();

        // 20 rounds: key XOR i
        for i in 0u8..20 {
            let round_key: Vec<u8> = doc_key.iter().map(|b| b ^ i).collect();
            result = super::rc4::decrypt(&round_key, &result);
        }

        // Compare first 16 bytes of /U
        if config.u.len() >= 16 && constant_time_eq(&result, &config.u[..16]) {
            return Some(doc_key);
        }
    }

    None
}

/// Validate an owner password (Algorithm 3 variant).
///
/// Derives the owner key, RC4-decrypts /O to recover the user password,
/// then validates that recovered password via validate_user_password.
pub fn validate_owner_password(
    password: &[u8],
    config: &EncryptionConfig,
    file_id: &[u8],
) -> Option<Vec<u8>> {
    let key_bytes = config.key_length / 8;
    let padded = pad_password(password);

    // MD5 the padded owner password
    let mut hasher = Md5::new();
    hasher.update(padded);
    let mut hash = hasher.finalize().to_vec();

    // Rev 3+: re-hash 50 times
    if config.r >= 3 {
        for _ in 0..50 {
            let mut h = Md5::new();
            h.update(&hash);
            hash = h.finalize().to_vec();
        }
    }
    hash.truncate(key_bytes);

    // Decrypt /O to recover the user password
    let mut user_password = config.o.clone();

    if config.r == 2 {
        user_password = super::rc4::decrypt(&hash, &user_password);
    } else {
        // Rev 3: 20 rounds in reverse (19 down to 0)
        for i in (0u8..20).rev() {
            let round_key: Vec<u8> = hash.iter().map(|b| b ^ i).collect();
            user_password = super::rc4::decrypt(&round_key, &user_password);
        }
    }

    // Validate the recovered user password
    validate_user_password(&user_password, config, file_id)
}

// ---------------------------------------------------------------------------
// V=5 / R=5-6: AES-256 key derivation (ISO 32000-2)
// ---------------------------------------------------------------------------

/// Truncate password to 127 bytes per ISO 32000-2 (SASLprep is not implemented;
/// we just truncate, which handles the vast majority of real-world PDFs).
fn saslprep_truncate(password: &[u8]) -> &[u8] {
    let len = password.len().min(127);
    &password[..len]
}

/// Compute password hash for R=5 (simple SHA-256).
///
/// Algorithm 2.B (R=5): SHA-256(password + salt + extra)
/// where extra is /U[0..48] for owner password checks, empty for user.
pub(super) fn compute_hash_r5(password: &[u8], salt: &[u8], extra: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(password);
    hasher.update(salt);
    hasher.update(extra);
    let result = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

/// Compute password hash for R=6 (Algorithm 2.B from ISO 32000-2).
///
/// This is the iterative hash with SHA-256, SHA-384, and SHA-512 rounds.
/// The loop runs until the last byte of the last AES-CBC block satisfies
/// a modulo-3 condition AND at least 64 rounds have run.
fn compute_hash_r6(password: &[u8], salt: &[u8], extra: &[u8]) -> [u8; 32] {
    use sha2::{Sha384, Sha512};

    // Initial hash: SHA-256(password + salt + extra)
    let mut hasher = Sha256::new();
    hasher.update(password);
    hasher.update(salt);
    hasher.update(extra);
    let initial = hasher.finalize();
    let mut k = initial.to_vec();

    let mut round = 0u32;
    loop {
        // Build K1 = password + k + extra, repeated 64 times
        let block_len = password.len() + k.len() + extra.len();
        let mut k1 = Vec::with_capacity(block_len * 64);
        for _ in 0..64 {
            k1.extend_from_slice(password);
            k1.extend_from_slice(&k);
            k1.extend_from_slice(extra);
        }

        // AES-128-CBC encrypt K1 using first 16 bytes of K as key, next 16 as IV
        let aes_key = &k[..16];
        let aes_iv = &k[16..32];
        let encrypted = aes_cbc_encrypt_no_padding(aes_key, aes_iv, &k1);

        // Determine which hash to use based on sum of first 16 bytes mod 3
        let sum: u32 = encrypted[..16].iter().map(|&b| b as u32).sum();
        let hash_result = match sum % 3 {
            0 => {
                let mut h = Sha256::new();
                h.update(&encrypted);
                h.finalize().to_vec()
            }
            1 => {
                let mut h = Sha384::new();
                h.update(&encrypted);
                h.finalize().to_vec()
            }
            _ => {
                let mut h = Sha512::new();
                h.update(&encrypted);
                h.finalize().to_vec()
            }
        };

        k = hash_result;
        round += 1;

        // Termination: at least 64 rounds, then check last byte of encrypted data
        if round >= 64 {
            let last_byte = *encrypted.last().unwrap_or(&0);
            if last_byte <= (round - 32) as u8 {
                break;
            }
        }

        // SEC #62 ( round-2 audit, CVSS 5.9): hard ceiling on the loop.
        // The probabilistic termination above CAN run for thousands of
        // rounds on adversarial salt/password combinations -- the spec
        // doesn't bound it, but 1024 rounds is several orders of
        // magnitude above what real PDFs require (typical real-world
        // R=6 password is ~64-128 rounds). Above the ceiling we accept
        // the current `k` as the derived key; if the password was
        // wrong the downstream auth check fails normally, but we don't
        // burn unbounded CPU per opening attempt.
        const MAX_R6_ROUNDS: u32 = 1024;
        if round >= MAX_R6_ROUNDS {
            break;
        }
    }

    let mut out = [0u8; 32];
    out.copy_from_slice(&k[..32]);
    out
}

/// AES-128-CBC encryption without padding, used by Algorithm 2.B (R=6).
///
/// The input length must be a multiple of 16. This is guaranteed because
/// the K1 buffer is (password + k + extra) * 64, and the spec ensures
/// these sum to at least 32 * 64 = 2048 bytes (always block-aligned since
/// the hash output k is 32/48/64 bytes and password is 0-127).
fn aes_cbc_encrypt_no_padding(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    use cbc::cipher::{block_padding::NoPadding, BlockEncryptMut, KeyIvInit};
    type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;

    let mut buf = data.to_vec();
    let encryptor = match Aes128CbcEnc::new_from_slices(key, iv) {
        Ok(enc) => enc,
        Err(_) => return buf, // should not happen with valid key/iv sizes
    };
    // NoPadding encrypt on block-aligned data should not fail; ignore error
    let _ = encryptor.encrypt_padded_mut::<NoPadding>(&mut buf, data.len());
    buf
}

/// Validate user password for R=5/R=6 and recover the file encryption key.
///
/// /U layout: [0..32] hash, [32..40] validation salt, [40..48] key salt.
/// Validation: compute_hash(password, validation_salt) == /U[0..32].
/// Key recovery: compute_hash(password, key_salt), then AES-256-CBC
/// decrypt /UE with that hash as key and zero IV to get the 32-byte FEK.
pub fn validate_user_password_r56(password: &[u8], config: &EncryptionConfig) -> Option<Vec<u8>> {
    let pw = saslprep_truncate(password);

    if config.u.len() < 48 || config.ue.len() < 32 {
        return None;
    }

    let u_hash = &config.u[..32];
    let u_validation_salt = &config.u[32..40];
    let u_key_salt = &config.u[40..48];

    // Validate: hash(password, validation_salt) == /U[0..32]
    let computed = if config.r == 5 {
        compute_hash_r5(pw, u_validation_salt, &[])
    } else {
        compute_hash_r6(pw, u_validation_salt, &[])
    };

    if !constant_time_eq(&computed, u_hash) {
        return None;
    }

    // Derive the intermediate key: hash(password, key_salt)
    let intermediate = if config.r == 5 {
        compute_hash_r5(pw, u_key_salt, &[])
    } else {
        compute_hash_r6(pw, u_key_salt, &[])
    };

    // Decrypt /UE (32 bytes) with AES-256-CBC, zero IV to get FEK
    let fek = decrypt_aes256_cbc_32(&intermediate, &config.ue[..32])?;
    Some(fek.to_vec())
}

/// Validate owner password for R=5/R=6 and recover the file encryption key.
///
/// /O layout: [0..32] hash, [32..40] validation salt, [40..48] key salt.
/// Same as user but with /U[0..48] as the "extra" input to the hash,
/// and /OE as the encrypted FEK.
pub fn validate_owner_password_r56(password: &[u8], config: &EncryptionConfig) -> Option<Vec<u8>> {
    let pw = saslprep_truncate(password);

    if config.o.len() < 48 || config.oe.len() < 32 || config.u.len() < 48 {
        return None;
    }

    let o_hash = &config.o[..32];
    let o_validation_salt = &config.o[32..40];
    let o_key_salt = &config.o[40..48];
    let u_truncated = &config.u[..48];

    // Validate: hash(password, validation_salt, /U[0..48]) == /O[0..32]
    let computed = if config.r == 5 {
        compute_hash_r5(pw, o_validation_salt, u_truncated)
    } else {
        compute_hash_r6(pw, o_validation_salt, u_truncated)
    };

    if !constant_time_eq(&computed, o_hash) {
        return None;
    }

    // Derive intermediate key: hash(password, key_salt, /U[0..48])
    let intermediate = if config.r == 5 {
        compute_hash_r5(pw, o_key_salt, u_truncated)
    } else {
        compute_hash_r6(pw, o_key_salt, u_truncated)
    };

    // Decrypt /OE (32 bytes) with AES-256-CBC, zero IV to get FEK
    let fek = decrypt_aes256_cbc_32(&intermediate, &config.oe[..32])?;
    Some(fek.to_vec())
}

/// Validate the /Perms entry to confirm the file encryption key is correct.
///
/// Decrypt /Perms (16 bytes) with AES-256-ECB (CBC with zero IV, one block)
/// using the FEK. Bytes 9-11 of the result must be "adb". Returns true if
/// valid, false otherwise.
pub fn validate_perms(fek: &[u8], perms: &[u8]) -> bool {
    if fek.len() != 32 || perms.len() < 16 {
        return false;
    }

    match super::aes256::decrypt_zero_iv(fek, &perms[..16]) {
        Some(decrypted) => {
            // Bytes 9-11 must be "adb" (0x61, 0x64, 0x62)
            decrypted[9] == b'a' && decrypted[10] == b'd' && decrypted[11] == b'b'
        }
        None => false,
    }
}

/// Decrypt a 32-byte value using AES-256-CBC with zero IV.
/// Used for /UE and /OE decryption in V=5 key recovery.
fn decrypt_aes256_cbc_32(key: &[u8], data: &[u8]) -> Option<[u8; 32]> {
    use aes::Aes256;
    use cbc::cipher::{block_padding::NoPadding, BlockDecryptMut, KeyIvInit};
    type Aes256CbcDec = cbc::Decryptor<Aes256>;

    if key.len() != 32 || data.len() != 32 {
        return None;
    }

    let iv = [0u8; 16];
    let mut buf = data.to_vec();
    let decryptor = Aes256CbcDec::new_from_slices(key, &iv).ok()?;
    decryptor.decrypt_padded_mut::<NoPadding>(&mut buf).ok()?;

    let mut result = [0u8; 32];
    result.copy_from_slice(&buf);
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pad_password_empty() {
        let padded = pad_password(b"");
        assert_eq!(padded, PASSWORD_PADDING);
    }

    #[test]
    fn test_pad_password_short() {
        let padded = pad_password(b"test");
        assert_eq!(&padded[..4], b"test");
        assert_eq!(&padded[4..], &PASSWORD_PADDING[..28]);
    }

    #[test]
    fn test_pad_password_exact_32() {
        let pw = [b'A'; 32];
        let padded = pad_password(&pw);
        assert_eq!(padded, pw);
    }

    #[test]
    fn test_pad_password_longer_than_32() {
        let pw = [b'B'; 64];
        let padded = pad_password(&pw);
        assert_eq!(padded, [b'B'; 32]);
    }

    #[test]
    fn test_per_object_key_length() {
        let doc_key = vec![0u8; 5]; // 40-bit key
        let obj_ref = ObjRef { num: 1, gen: 0 };
        let (n, _key) = per_object_key(&doc_key, obj_ref, 40);
        // min(5 + 5, 16) = 10
        assert_eq!(n, 10);
    }

    #[test]
    fn test_per_object_key_max_16() {
        let doc_key = vec![0u8; 16]; // 128-bit key
        let obj_ref = ObjRef { num: 1, gen: 0 };
        let (n, _key) = per_object_key(&doc_key, obj_ref, 128);
        // min(16 + 5, 16) = 16
        assert_eq!(n, 16);
    }

    #[test]
    fn test_derive_document_key_deterministic() {
        let config = EncryptionConfig {
            v: 1,
            r: 2,
            key_length: 40,
            o: vec![0u8; 32],
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: true,
            stream_algorithm: crate::crypt::CryptAlgorithm::Rc4,
            string_algorithm: crate::crypt::CryptAlgorithm::Rc4,
            oe: Vec::new(),
            ue: Vec::new(),
            perms: Vec::new(),
        };
        let key1 = derive_document_key(b"test", &config, b"file-id");
        let key2 = derive_document_key(b"test", &config, b"file-id");
        assert_eq!(key1, key2);
        assert_eq!(key1.len(), 5); // 40/8
    }

    #[test]
    fn test_derive_document_key_rev3_longer() {
        let config = EncryptionConfig {
            v: 2,
            r: 3,
            key_length: 128,
            o: vec![0u8; 32],
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: true,
            stream_algorithm: crate::crypt::CryptAlgorithm::Rc4,
            string_algorithm: crate::crypt::CryptAlgorithm::Rc4,
            oe: Vec::new(),
            ue: Vec::new(),
            perms: Vec::new(),
        };
        let key = derive_document_key(b"test", &config, b"file-id");
        assert_eq!(key.len(), 16); // 128/8
    }

    #[test]
    fn test_derive_document_key_rev3_64bit() {
        // V=2/R=3 with 64-bit key (valid but uncommon)
        let config = EncryptionConfig {
            v: 2,
            r: 3,
            key_length: 64,
            o: vec![0u8; 32],
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: true,
            stream_algorithm: crate::crypt::CryptAlgorithm::Rc4,
            string_algorithm: crate::crypt::CryptAlgorithm::Rc4,
            oe: Vec::new(),
            ue: Vec::new(),
            perms: Vec::new(),
        };
        let key = derive_document_key(b"test", &config, b"file-id");
        assert_eq!(key.len(), 8); // 64/8
    }

    #[test]
    fn test_per_object_key_64bit() {
        // Per-object key with 64-bit document key
        let doc_key = vec![0xAB; 8]; // 64-bit key
        let obj_ref = ObjRef { num: 5, gen: 0 };
        let (n, _key) = per_object_key(&doc_key, obj_ref, 64);
        // min(8 + 5, 16) = 13
        assert_eq!(n, 13);
    }

    #[test]
    fn test_per_object_key_40bit() {
        // Per-object key with 40-bit document key
        let doc_key = vec![0xCD; 5]; // 40-bit key
        let obj_ref = ObjRef { num: 10, gen: 2 };
        let (n, key) = per_object_key(&doc_key, obj_ref, 40);
        // min(5 + 5, 16) = 10
        assert_eq!(n, 10);
        // Different obj_ref should produce different key
        let (n2, key2) = per_object_key(&doc_key, ObjRef { num: 11, gen: 2 }, 40);
        assert_ne!(key[..n], key2[..n2]);
    }

    #[test]
    fn test_constant_time_eq_equal() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(&[0u8; 32], &[0u8; 32]));
    }

    #[test]
    fn test_constant_time_eq_not_equal() {
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"hello", b"hell"));
        assert!(!constant_time_eq(b"", b"x"));
        // Single bit difference
        assert!(!constant_time_eq(&[0x00], &[0x01]));
    }

    #[test]
    fn test_derive_document_key_encrypt_metadata_false() {
        // When encrypt_metadata is false, 0xFFFFFFFF is appended to the MD5 input.
        // This should produce a different key than encrypt_metadata=true.
        let config_true = EncryptionConfig {
            v: 2,
            r: 3,
            key_length: 128,
            o: vec![0u8; 32],
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: true,
            stream_algorithm: crate::crypt::CryptAlgorithm::Rc4,
            string_algorithm: crate::crypt::CryptAlgorithm::Rc4,
            oe: Vec::new(),
            ue: Vec::new(),
            perms: Vec::new(),
        };
        let config_false = EncryptionConfig {
            v: 2,
            r: 3,
            key_length: 128,
            o: vec![0u8; 32],
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: false,
            stream_algorithm: crate::crypt::CryptAlgorithm::Rc4,
            string_algorithm: crate::crypt::CryptAlgorithm::Rc4,
            oe: Vec::new(),
            ue: Vec::new(),
            perms: Vec::new(),
        };
        let key_true = derive_document_key(b"test", &config_true, b"file-id");
        let key_false = derive_document_key(b"test", &config_false, b"file-id");
        assert_ne!(
            key_true, key_false,
            "encrypt_metadata flag should affect key derivation"
        );
        assert_eq!(key_false.len(), 16);
    }

    // -- R=5/R=6 tests --

    #[test]
    fn test_saslprep_truncate_short() {
        let pw = b"hello";
        assert_eq!(saslprep_truncate(pw), b"hello");
    }

    #[test]
    fn test_saslprep_truncate_at_127() {
        let pw = vec![b'A'; 200];
        assert_eq!(saslprep_truncate(&pw).len(), 127);
    }

    #[test]
    fn test_compute_hash_r5_deterministic() {
        let hash1 = compute_hash_r5(b"password", b"12345678", &[]);
        let hash2 = compute_hash_r5(b"password", b"12345678", &[]);
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 32);
    }

    #[test]
    fn test_compute_hash_r5_different_passwords() {
        let hash1 = compute_hash_r5(b"password1", b"12345678", &[]);
        let hash2 = compute_hash_r5(b"password2", b"12345678", &[]);
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_hash_r5_different_salts() {
        let hash1 = compute_hash_r5(b"password", b"12345678", &[]);
        let hash2 = compute_hash_r5(b"password", b"87654321", &[]);
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_hash_r5_with_extra() {
        let hash1 = compute_hash_r5(b"password", b"12345678", &[]);
        let hash2 = compute_hash_r5(b"password", b"12345678", b"extra");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_hash_r6_deterministic() {
        let hash1 = compute_hash_r6(b"password", b"12345678", &[]);
        let hash2 = compute_hash_r6(b"password", b"12345678", &[]);
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 32);
    }

    #[test]
    fn test_compute_hash_r6_different_from_r5() {
        let hash_r5 = compute_hash_r5(b"password", b"12345678", &[]);
        let hash_r6 = compute_hash_r6(b"password", b"12345678", &[]);
        assert_ne!(
            hash_r5, hash_r6,
            "R=5 and R=6 should produce different hashes"
        );
    }

    #[test]
    fn test_decrypt_aes256_cbc_32_wrong_key_length() {
        assert!(decrypt_aes256_cbc_32(&[0; 16], &[0; 32]).is_none());
        assert!(decrypt_aes256_cbc_32(&[0; 31], &[0; 32]).is_none());
    }

    #[test]
    fn test_decrypt_aes256_cbc_32_wrong_data_length() {
        assert!(decrypt_aes256_cbc_32(&[0; 32], &[0; 16]).is_none());
        assert!(decrypt_aes256_cbc_32(&[0; 32], &[0; 48]).is_none());
    }

    #[test]
    fn test_decrypt_aes256_cbc_32_roundtrip() {
        use aes::Aes256;
        use cbc::cipher::{block_padding::NoPadding, BlockEncryptMut, KeyIvInit};
        type Aes256CbcEnc = cbc::Encryptor<Aes256>;

        let key = [0x42u8; 32];
        let iv = [0u8; 16];
        let plaintext = [0xABu8; 32];

        let mut buf = plaintext.to_vec();
        let encryptor = Aes256CbcEnc::new_from_slices(&key, &iv).unwrap();
        encryptor
            .encrypt_padded_mut::<NoPadding>(&mut buf, 32)
            .unwrap();

        let result = decrypt_aes256_cbc_32(&key, &buf).unwrap();
        assert_eq!(result, plaintext);
    }

    #[test]
    fn test_validate_perms_wrong_key_length() {
        assert!(!validate_perms(&[0; 16], &[0; 16]));
    }

    #[test]
    fn test_validate_perms_wrong_data_length() {
        assert!(!validate_perms(&[0; 32], &[0; 15]));
    }
}

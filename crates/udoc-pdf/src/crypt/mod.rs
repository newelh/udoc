//! PDF encryption support.
//!
//! Handles /Encrypt dictionary parsing, key derivation, password validation,
//! and transparent decryption of strings and streams. Supports the Standard
//! security handler with RC4 (Rev 2-3, V 1-2), AES-128-CBC (Rev 4, V 4),
//! and AES-256-CBC (Rev 5-6, V 5).

mod aes;
mod aes256;
mod key;
mod rc4;

use crate::diagnostics::{DiagnosticsSink, Warning, WarningKind};
use crate::error::{EncryptionErrorKind, Error, Result};
use crate::object::{ObjRef, PdfDictionary, PdfObject, PdfString};
use std::sync::Arc;

/// Encryption algorithm used for a particular data type (streams or strings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CryptAlgorithm {
    /// No encryption (identity filter).
    None,
    /// RC4 encryption (V=1-2, or V=4 with /CFM = /V2).
    Rc4,
    /// AES-128-CBC encryption (V=4 with /CFM = /AESV2).
    Aes128,
    /// AES-256-CBC encryption (V=5 with /CFM = /AESV3).
    Aes256,
}

/// Parsed encryption configuration from the /Encrypt dictionary.
#[derive(Debug, Clone)]
pub(crate) struct EncryptionConfig {
    /// Encryption algorithm version (/V). 1 = RC4-40, 2 = RC4-variable, 4 = crypt filters, 5 = AES-256.
    pub v: i64,
    /// Standard security handler revision (/R). 2, 3, 4, 5, or 6.
    pub r: i64,
    /// Key length in bits (/Length). Default 128 for V=2/4, 40 for V=1, 256 for V=5.
    pub key_length: usize,
    /// Owner password hash (/O). 32 bytes for R<=4, 48 bytes for R=5/6.
    pub o: Vec<u8>,
    /// User password hash (/U). 32 bytes for R<=4, 48 bytes for R=5/6.
    pub u: Vec<u8>,
    /// Permission flags (/P), as a signed 32-bit integer.
    pub p: i32,
    /// Whether to encrypt metadata (/EncryptMetadata). Default true.
    pub encrypt_metadata: bool,
    /// Algorithm for stream decryption. V=1-2 always RC4. V=4 uses crypt filters.
    pub stream_algorithm: CryptAlgorithm,
    /// Algorithm for string decryption. V=1-2 always RC4. V=4 uses crypt filters.
    pub string_algorithm: CryptAlgorithm,
    /// Owner encrypted key (/OE), 32 bytes. V=5 only.
    pub oe: Vec<u8>,
    /// User encrypted key (/UE), 32 bytes. V=5 only.
    pub ue: Vec<u8>,
    /// Encrypted permissions (/Perms), 16 bytes. V=5 only.
    pub perms: Vec<u8>,
}

/// Handles decryption of PDF objects and streams.
pub(crate) struct CryptHandler {
    /// The document-level encryption key.
    document_key: Vec<u8>,
    /// Encryption config for key length info.
    config: EncryptionConfig,
    /// Object reference of the /Encrypt dictionary itself (never decrypt).
    encrypt_obj_ref: Option<ObjRef>,
    /// Diagnostics sink for decryption warnings (e.g. AES failures).
    diagnostics: Arc<dyn DiagnosticsSink>,
}

impl CryptHandler {
    /// Create a new CryptHandler from a validated config and derived document key.
    pub fn new(
        document_key: Vec<u8>,
        config: EncryptionConfig,
        encrypt_obj_ref: Option<ObjRef>,
        diagnostics: Arc<dyn DiagnosticsSink>,
    ) -> Self {
        Self {
            document_key,
            config,
            encrypt_obj_ref,
            diagnostics,
        }
    }

    /// Decrypt all strings within a PdfObject tree.
    ///
    /// Walks arrays and dicts recursively. Names and other non-string types
    /// are left untouched. Returns the object with decrypted strings.
    pub fn decrypt_object(&self, obj: PdfObject, obj_ref: ObjRef) -> PdfObject {
        // Skip decryption for the /Encrypt dictionary itself
        if self.encrypt_obj_ref == Some(obj_ref) {
            return obj;
        }

        match self.config.string_algorithm {
            CryptAlgorithm::None => obj,
            CryptAlgorithm::Rc4 => {
                let (n, obj_key) =
                    key::per_object_key(&self.document_key, obj_ref, self.config.key_length);
                decrypt_object_recursive(
                    obj,
                    &obj_key[..n],
                    StringDecryptMode::Rc4,
                    &self.diagnostics,
                )
            }
            CryptAlgorithm::Aes128 => {
                let obj_key = key::per_object_key_aes(&self.document_key, obj_ref);
                decrypt_object_recursive(
                    obj,
                    &obj_key,
                    StringDecryptMode::Aes128,
                    &self.diagnostics,
                )
            }
            CryptAlgorithm::Aes256 => {
                // V=5: no per-object key derivation, use document key directly
                decrypt_object_recursive(
                    obj,
                    &self.document_key,
                    StringDecryptMode::Aes256,
                    &self.diagnostics,
                )
            }
        }
    }

    /// Check whether a given object should be skipped for decryption.
    ///
    /// Returns true for the /Encrypt dictionary, XRef streams, and (when
    /// EncryptMetadata is false) Metadata streams.
    pub fn should_skip(&self, obj_ref: ObjRef, stream_type: Option<&[u8]>) -> bool {
        if self.encrypt_obj_ref == Some(obj_ref) || stream_type == Some(b"XRef") {
            return true;
        }
        // When EncryptMetadata is false, /Type /Metadata streams are not encrypted
        if !self.config.encrypt_metadata && stream_type == Some(b"Metadata") {
            return true;
        }
        false
    }

    /// Decrypt raw stream data before the filter chain is applied.
    ///
    /// Caller must check `should_skip()` first to avoid unnecessary work
    /// on streams that should not be decrypted.
    pub fn decrypt_stream_data(&self, data: &[u8], obj_ref: ObjRef) -> Vec<u8> {
        match self.config.stream_algorithm {
            // allocates even for identity. Cow<[u8]> return deferred to perf sprint.
            CryptAlgorithm::None => data.to_vec(),
            CryptAlgorithm::Rc4 => {
                let (n, obj_key) =
                    key::per_object_key(&self.document_key, obj_ref, self.config.key_length);
                rc4::decrypt(&obj_key[..n], data)
            }
            CryptAlgorithm::Aes128 => {
                let obj_key = key::per_object_key_aes(&self.document_key, obj_ref);
                aes::decrypt(&obj_key, data).unwrap_or_else(|| {
                    self.diagnostics.warning(Warning::new(
                        None,
                        WarningKind::EncryptedDocument,
                        format!(
                            "AES-128 stream decryption failed for {obj_ref}, returning raw bytes"
                        ),
                    ));
                    data.to_vec()
                })
            }
            CryptAlgorithm::Aes256 => {
                // V=5: no per-object key, use document key directly
                aes256::decrypt(&self.document_key, data).unwrap_or_else(|| {
                    self.diagnostics.warning(Warning::new(
                        None,
                        WarningKind::EncryptedDocument,
                        format!(
                            "AES-256 stream decryption failed for {obj_ref}, returning raw bytes"
                        ),
                    ));
                    data.to_vec()
                })
            }
        }
    }

    /// Try to initialize encryption from a trailer's /Encrypt dictionary.
    ///
    /// Attempts the empty password first. If that fails and a password is
    /// provided, tries it as both user and owner password.
    pub fn from_encrypt_dict(
        encrypt_dict: &PdfDictionary,
        file_id: &[u8],
        password: Option<&[u8]>,
        encrypt_obj_ref: Option<ObjRef>,
        diagnostics: &Arc<dyn DiagnosticsSink>,
    ) -> Result<Self> {
        let config = parse_encrypt_dict(encrypt_dict, diagnostics)?;

        diagnostics.warning(Warning::info(
            WarningKind::EncryptedDocument,
            format!(
                "encrypted PDF: V={}, R={}, key_length={}",
                config.v, config.r, config.key_length
            ),
        ));

        // V=5 (R=5/R=6): AES-256 key derivation, no file_id needed
        if config.v == 5 {
            return Self::try_passwords_r56(&config, password, encrypt_obj_ref, diagnostics);
        }

        // V=1-4: MD5-based key derivation
        // Try empty password first
        if let Some(doc_key) = key::validate_user_password(b"", &config, file_id) {
            diagnostics.warning(Warning::info(
                WarningKind::EncryptedDocument,
                "opened with empty password".to_string(),
            ));
            return Ok(Self::new(
                doc_key,
                config,
                encrypt_obj_ref,
                Arc::clone(diagnostics),
            ));
        }

        // Try supplied password as user password
        if let Some(pw) = password {
            if let Some(doc_key) = key::validate_user_password(pw, &config, file_id) {
                diagnostics.warning(Warning::info(
                    WarningKind::EncryptedDocument,
                    "opened with user password".to_string(),
                ));
                return Ok(Self::new(
                    doc_key,
                    config,
                    encrypt_obj_ref,
                    Arc::clone(diagnostics),
                ));
            }

            // Try as owner password
            if let Some(doc_key) = key::validate_owner_password(pw, &config, file_id) {
                diagnostics.warning(Warning::info(
                    WarningKind::EncryptedDocument,
                    "opened with owner password".to_string(),
                ));
                return Ok(Self::new(
                    doc_key,
                    config,
                    encrypt_obj_ref,
                    Arc::clone(diagnostics),
                ));
            }
        }

        Err(Error::encryption(EncryptionErrorKind::InvalidPassword))
    }

    /// Try passwords for V=5 (R=5/R=6) AES-256 encryption.
    ///
    /// V=5 key derivation uses SHA-256 and does not require file_id. The FEK
    /// (file encryption key) is stored encrypted in /UE and /OE, recovered
    /// after password validation via SHA-256 hash comparison.
    fn try_passwords_r56(
        config: &EncryptionConfig,
        password: Option<&[u8]>,
        encrypt_obj_ref: Option<ObjRef>,
        diagnostics: &Arc<dyn DiagnosticsSink>,
    ) -> Result<Self> {
        // Helper to validate /Perms and emit warning if invalid
        let validate_and_build =
            |fek: Vec<u8>,
             source: &str,
             config: EncryptionConfig,
             encrypt_obj_ref: Option<ObjRef>,
             diagnostics: &Arc<dyn DiagnosticsSink>| {
                // Validate /Perms (should-have, warn but don't fail)
                if !config.perms.is_empty() && !key::validate_perms(&fek, &config.perms) {
                    diagnostics.warning(Warning::new(
                        None,
                        WarningKind::EncryptedDocument,
                        "/Perms validation failed (bytes 9-11 != 'adb'), key may be incorrect"
                            .to_string(),
                    ));
                }

                diagnostics.warning(Warning::info(
                    WarningKind::EncryptedDocument,
                    format!("opened with {source}"),
                ));
                Ok(Self::new(
                    fek,
                    config,
                    encrypt_obj_ref,
                    Arc::clone(diagnostics),
                ))
            };

        // Try empty password as user
        if let Some(fek) = key::validate_user_password_r56(b"", config) {
            return validate_and_build(
                fek,
                "empty password",
                config.clone(),
                encrypt_obj_ref,
                diagnostics,
            );
        }

        // Try supplied password
        if let Some(pw) = password {
            if let Some(fek) = key::validate_user_password_r56(pw, config) {
                return validate_and_build(
                    fek,
                    "user password",
                    config.clone(),
                    encrypt_obj_ref,
                    diagnostics,
                );
            }

            if let Some(fek) = key::validate_owner_password_r56(pw, config) {
                return validate_and_build(
                    fek,
                    "owner password",
                    config.clone(),
                    encrypt_obj_ref,
                    diagnostics,
                );
            }
        }

        Err(Error::encryption(EncryptionErrorKind::InvalidPassword))
    }
}

/// Maximum nesting depth for recursive decryption. Matches the object parser's
/// depth limit. Beyond this, objects are returned as-is (strings left encrypted).
const MAX_DECRYPT_DEPTH: usize = 256;

/// How to decrypt strings within an object tree.
#[derive(Debug, Clone, Copy)]
enum StringDecryptMode {
    Rc4,
    Aes128,
    Aes256,
}

/// Recursively decrypt strings within a PdfObject tree.
fn decrypt_object_recursive(
    obj: PdfObject,
    obj_key: &[u8],
    mode: StringDecryptMode,
    diagnostics: &Arc<dyn DiagnosticsSink>,
) -> PdfObject {
    decrypt_object_inner(obj, obj_key, mode, diagnostics, 0)
}

fn decrypt_object_inner(
    obj: PdfObject,
    obj_key: &[u8],
    mode: StringDecryptMode,
    diagnostics: &Arc<dyn DiagnosticsSink>,
    depth: usize,
) -> PdfObject {
    if depth >= MAX_DECRYPT_DEPTH {
        return obj;
    }
    match obj {
        PdfObject::String(s) => {
            let decrypted =
                match mode {
                    StringDecryptMode::Rc4 => rc4::decrypt(obj_key, s.as_bytes()),
                    StringDecryptMode::Aes128 => aes::decrypt(obj_key, s.as_bytes())
                        .unwrap_or_else(|| {
                            diagnostics.warning(Warning::new(
                                None,
                                WarningKind::EncryptedDocument,
                                format!(
                                "AES-128 string decryption failed ({} bytes), preserving raw bytes",
                                s.as_bytes().len()
                            ),
                            ));
                            s.as_bytes().to_vec()
                        }),
                    StringDecryptMode::Aes256 => aes256::decrypt(obj_key, s.as_bytes())
                        .unwrap_or_else(|| {
                            diagnostics.warning(Warning::new(
                                None,
                                WarningKind::EncryptedDocument,
                                format!(
                                "AES-256 string decryption failed ({} bytes), preserving raw bytes",
                                s.as_bytes().len()
                            ),
                            ));
                            s.as_bytes().to_vec()
                        }),
                };
            PdfObject::String(PdfString::new(decrypted))
        }
        PdfObject::Array(items) => PdfObject::Array(
            items
                .into_iter()
                .map(|item| decrypt_object_inner(item, obj_key, mode, diagnostics, depth + 1))
                .collect(),
        ),
        PdfObject::Dictionary(dict) => {
            let mut new_dict = PdfDictionary::new();
            for (k, v) in dict {
                new_dict.insert(
                    k,
                    decrypt_object_inner(v, obj_key, mode, diagnostics, depth + 1),
                );
            }
            PdfObject::Dictionary(new_dict)
        }
        other => other,
    }
}

/// Parse an /Encrypt dictionary into an EncryptionConfig.
fn parse_encrypt_dict(
    dict: &PdfDictionary,
    diagnostics: &Arc<dyn DiagnosticsSink>,
) -> Result<EncryptionConfig> {
    // /Filter must be /Standard
    let filter = dict
        .get_name(b"Filter")
        .ok_or_else(|| Error::encryption(EncryptionErrorKind::MissingField("Filter".into())))?;
    if filter != b"Standard" {
        return Err(Error::encryption(EncryptionErrorKind::UnsupportedFilter(
            String::from_utf8_lossy(filter).into_owned(),
        )));
    }

    let v = dict.get_i64(b"V").unwrap_or(0);

    let r = dict
        .get_i64(b"R")
        .ok_or_else(|| Error::encryption(EncryptionErrorKind::MissingField("R".into())))?;

    // V and R validation. We validate V and R independently rather
    // than checking valid (V,R) pairs. The spec says V=4 requires R=4, V=2
    // requires R=3, etc., but real-world PDFs have mismatched combos like
    // V=1/R=3 or V=2/R=4. Rejecting these would break working documents.
    // V=1-2/R=2-3: RC4 only. V=4/R=4: crypt filter-based (RC4 or AES-128).
    // V=5/R=5-6: AES-256 (ISO 32000-2).
    if v != 1 && v != 2 && v != 4 && v != 5 {
        return Err(Error::encryption(EncryptionErrorKind::UnsupportedVersion {
            v,
            r,
        }));
    }
    if r != 2 && r != 3 && r != 4 && r != 5 && r != 6 {
        return Err(Error::encryption(EncryptionErrorKind::UnsupportedVersion {
            v,
            r,
        }));
    }

    // Key length: V=1 always 40-bit, V=2 variable, V=4 always 128-bit, V=5 always 256-bit.
    let key_length = if v == 1 {
        40
    } else if v == 4 {
        // V=4 always uses 128-bit keys (PDF spec Table 3.18)
        128
    } else if v == 5 {
        // V=5 always uses 256-bit keys (ISO 32000-2)
        256
    } else {
        let len = match dict.get_i64(b"Length") {
            Some(l) => l,
            None => {
                // PDF spec says default is 40, but V=2 without /Length almost always
                // means 128-bit in practice. Default to 128 to match real-world PDFs.
                diagnostics.warning(Warning::new(
                    None,
                    WarningKind::EncryptedDocument,
                    "/Encrypt has V=2 but no /Length, defaulting to 128".to_string(),
                ));
                128
            }
        };
        if !(40..=128).contains(&len) || len % 8 != 0 {
            return Err(Error::encryption(EncryptionErrorKind::InvalidField(
                "/Length must be 40-128 and a multiple of 8".into(),
            )));
        }
        len as usize
    };

    // Parse crypt filter algorithms. V=4/5 use crypt filters, V=1-2 always RC4.
    let (stream_algorithm, string_algorithm) = if v == 4 || v == 5 {
        parse_crypt_filters(dict, diagnostics)?
    } else {
        (CryptAlgorithm::Rc4, CryptAlgorithm::Rc4)
    };

    let o = dict
        .get_str(b"O")
        .ok_or_else(|| Error::encryption(EncryptionErrorKind::MissingField("O".into())))?
        .as_bytes()
        .to_vec();

    // V=5: /O is 48 bytes (32 hash + 8 validation salt + 8 key salt).
    // V=1-4: /O is 32 bytes.
    let min_o_len = if v == 5 { 48 } else { 32 };
    if o.len() < min_o_len {
        return Err(Error::encryption(EncryptionErrorKind::InvalidField(
            format!("/O must be at least {} bytes, got {}", min_o_len, o.len()),
        )));
    }

    let u = dict
        .get_str(b"U")
        .ok_or_else(|| Error::encryption(EncryptionErrorKind::MissingField("U".into())))?
        .as_bytes()
        .to_vec();

    // V=5: /U is 48 bytes. Rev 3 slices /U[..16], so at least 16 for R<=4.
    let min_u_len = if v == 5 { 48 } else { 16 };
    if u.len() < min_u_len {
        return Err(Error::encryption(EncryptionErrorKind::InvalidField(
            format!("/U must be at least {} bytes, got {}", min_u_len, u.len()),
        )));
    }

    let p_raw = dict
        .get_i64(b"P")
        .ok_or_else(|| Error::encryption(EncryptionErrorKind::MissingField("P".into())))?;
    // /P is a signed 32-bit integer per the PDF spec, but some writers (pypdf)
    // emit it as unsigned (e.g. 4294967292 instead of -4). The `as i32` cast
    // reinterprets the low 32 bits, which correctly recovers the signed value.
    #[allow(clippy::cast_possible_truncation)]
    let p = i32::try_from(p_raw).unwrap_or(p_raw as i32);

    let encrypt_metadata = dict.get_bool(b"EncryptMetadata").unwrap_or(true);

    // V=5 additional fields: /OE, /UE, /Perms
    let (oe, ue, perms) = if v == 5 {
        let oe_val = dict
            .get_str(b"OE")
            .ok_or_else(|| Error::encryption(EncryptionErrorKind::MissingField("OE".into())))?
            .as_bytes()
            .to_vec();
        if oe_val.len() < 32 {
            return Err(Error::encryption(EncryptionErrorKind::InvalidField(
                format!("/OE must be 32 bytes, got {}", oe_val.len()),
            )));
        }

        let ue_val = dict
            .get_str(b"UE")
            .ok_or_else(|| Error::encryption(EncryptionErrorKind::MissingField("UE".into())))?
            .as_bytes()
            .to_vec();
        if ue_val.len() < 32 {
            return Err(Error::encryption(EncryptionErrorKind::InvalidField(
                format!("/UE must be 32 bytes, got {}", ue_val.len()),
            )));
        }

        let perms_val = dict
            .get_str(b"Perms")
            .ok_or_else(|| Error::encryption(EncryptionErrorKind::MissingField("Perms".into())))?
            .as_bytes()
            .to_vec();
        if perms_val.len() < 16 {
            return Err(Error::encryption(EncryptionErrorKind::InvalidField(
                format!("/Perms must be 16 bytes, got {}", perms_val.len()),
            )));
        }

        (oe_val, ue_val, perms_val)
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };

    Ok(EncryptionConfig {
        v,
        r,
        key_length,
        o,
        u,
        p,
        encrypt_metadata,
        stream_algorithm,
        string_algorithm,
        oe,
        ue,
        perms,
    })
}

/// Parse crypt filter configuration from /CF, /StmF, /StrF entries.
///
/// /CF is a dictionary of named crypt filters. /StmF and /StrF name the
/// default filters for streams and strings respectively. Each filter has
/// /CFM specifying the algorithm: /V2 (RC4), /AESV2 (AES-128), /None (identity).
fn parse_crypt_filters(
    dict: &PdfDictionary,
    diagnostics: &Arc<dyn DiagnosticsSink>,
) -> Result<(CryptAlgorithm, CryptAlgorithm)> {
    // /StmF and /StrF name the crypt filter to use. Default is "Identity" (no encryption).
    let stm_filter_name = dict.get_name(b"StmF").unwrap_or(b"Identity");
    let str_filter_name = dict.get_name(b"StrF").unwrap_or(b"Identity");

    let stream_algo = resolve_filter_algorithm(dict, stm_filter_name, diagnostics);
    let string_algo = resolve_filter_algorithm(dict, str_filter_name, diagnostics);

    Ok((stream_algo, string_algo))
}

/// Look up a named crypt filter in the /CF dictionary and return its algorithm.
fn resolve_filter_algorithm(
    dict: &PdfDictionary,
    filter_name: &[u8],
    diagnostics: &Arc<dyn DiagnosticsSink>,
) -> CryptAlgorithm {
    // "Identity" is the built-in no-encryption filter
    if filter_name == b"Identity" {
        return CryptAlgorithm::None;
    }

    // Look up the filter in /CF
    let cf_dict = match dict.get_dict(b"CF") {
        Some(cf) => cf,
        None => {
            diagnostics.warning(Warning::new(
                None,
                WarningKind::EncryptedDocument,
                format!(
                    "/CF dictionary missing but filter '{}' referenced, defaulting to RC4",
                    String::from_utf8_lossy(filter_name)
                ),
            ));
            return CryptAlgorithm::Rc4;
        }
    };

    let filter_dict = match cf_dict.get_dict(filter_name) {
        Some(fd) => fd,
        None => {
            diagnostics.warning(Warning::new(
                None,
                WarningKind::EncryptedDocument,
                format!(
                    "crypt filter '{}' not found in /CF, defaulting to RC4",
                    String::from_utf8_lossy(filter_name)
                ),
            ));
            return CryptAlgorithm::Rc4;
        }
    };

    // /CFM specifies the algorithm
    match filter_dict.get_name(b"CFM") {
        Some(b"V2") => CryptAlgorithm::Rc4,
        Some(b"AESV2") => CryptAlgorithm::Aes128,
        Some(b"AESV3") => CryptAlgorithm::Aes256,
        Some(b"None") => CryptAlgorithm::None,
        Some(other) => {
            diagnostics.warning(Warning::new(
                None,
                WarningKind::EncryptedDocument,
                format!(
                    "unknown /CFM value '{}', defaulting to RC4",
                    String::from_utf8_lossy(other)
                ),
            ));
            CryptAlgorithm::Rc4
        }
        None => {
            // /CFM missing defaults to /None per spec (Table 3.23).
            diagnostics.warning(Warning::new(
                None,
                WarningKind::EncryptedDocument,
                "crypt filter missing /CFM, defaulting to None (identity)".to_string(),
            ));
            CryptAlgorithm::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::NullDiagnostics;

    // The external `aes` crate is shadowed by our local `mod aes` sub-module.
    // Import the cipher types via the absolute crate path.
    type ExtAes128 = ::aes::Aes128;
    type ExtAes256 = ::aes::Aes256;

    fn test_diagnostics() -> Arc<dyn DiagnosticsSink> {
        Arc::new(NullDiagnostics)
    }

    #[test]
    fn crypt_handler_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CryptHandler>();
    }

    #[test]
    fn test_parse_encrypt_dict_valid() {
        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(1));
        dict.insert(b"R".to_vec(), PdfObject::Integer(2));
        dict.insert(b"Length".to_vec(), PdfObject::Integer(40));
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(
            b"U".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));

        let config = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap();
        assert_eq!(config.v, 1);
        assert_eq!(config.r, 2);
        assert_eq!(config.key_length, 40);
        assert_eq!(config.p, -4);
        assert!(config.encrypt_metadata);
    }

    #[test]
    fn test_parse_encrypt_dict_unsupported_filter() {
        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"PublicKey".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(1));
        dict.insert(b"R".to_vec(), PdfObject::Integer(2));

        let err = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap_err();
        assert!(err.to_string().contains("unsupported encryption filter"));
    }

    #[test]
    fn test_parse_encrypt_dict_unsupported_version() {
        // V=3 and R=1 are not valid
        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(3));
        dict.insert(b"R".to_vec(), PdfObject::Integer(2));

        let err = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap_err();
        assert!(err.to_string().contains("unsupported encryption version"));

        // V=6 is not valid
        let mut dict2 = PdfDictionary::new();
        dict2.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict2.insert(b"V".to_vec(), PdfObject::Integer(6));
        dict2.insert(b"R".to_vec(), PdfObject::Integer(6));

        let err2 = parse_encrypt_dict(&dict2, &test_diagnostics()).unwrap_err();
        assert!(err2.to_string().contains("unsupported encryption version"));
    }

    #[test]
    fn test_parse_encrypt_dict_missing_r() {
        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(1));

        let err = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap_err();
        assert!(err.to_string().contains("missing required field: R"));
    }

    #[test]
    fn test_parse_encrypt_dict_v1_forces_40bit_key() {
        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(1));
        dict.insert(b"R".to_vec(), PdfObject::Integer(2));
        dict.insert(b"Length".to_vec(), PdfObject::Integer(128));
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(
            b"U".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));

        let config = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap();
        assert_eq!(config.key_length, 40, "V=1 must always use 40-bit key");
    }

    #[test]
    fn test_parse_encrypt_dict_v2_rejects_bad_length() {
        let make_dict = |length: i64| {
            let mut dict = PdfDictionary::new();
            dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
            dict.insert(b"V".to_vec(), PdfObject::Integer(2));
            dict.insert(b"R".to_vec(), PdfObject::Integer(3));
            dict.insert(b"Length".to_vec(), PdfObject::Integer(length));
            dict.insert(
                b"O".to_vec(),
                PdfObject::String(PdfString::new(vec![0u8; 32])),
            );
            dict.insert(
                b"U".to_vec(),
                PdfObject::String(PdfString::new(vec![0u8; 32])),
            );
            dict.insert(b"P".to_vec(), PdfObject::Integer(-4));
            dict
        };

        let diag = test_diagnostics();
        // /Length 0 -- would cause division by zero without validation
        assert!(parse_encrypt_dict(&make_dict(0), &diag).is_err());
        // /Length -8 -- negative
        assert!(parse_encrypt_dict(&make_dict(-8), &diag).is_err());
        // /Length 32 -- below minimum
        assert!(parse_encrypt_dict(&make_dict(32), &diag).is_err());
        // /Length 256 -- above maximum
        assert!(parse_encrypt_dict(&make_dict(256), &diag).is_err());
        // /Length 50 -- not a multiple of 8
        assert!(parse_encrypt_dict(&make_dict(50), &diag).is_err());
        // /Length 64 -- valid
        assert!(parse_encrypt_dict(&make_dict(64), &diag).is_ok());
        // /Length 40 -- valid minimum
        assert!(parse_encrypt_dict(&make_dict(40), &diag).is_ok());
        // /Length 128 -- valid maximum
        assert!(parse_encrypt_dict(&make_dict(128), &diag).is_ok());
    }

    // -- decrypt_object_recursive tests --

    #[test]
    fn test_decrypt_recursive_string() {
        let key = b"test_key";
        let plaintext = b"hello";
        let encrypted = rc4::decrypt(key, plaintext);

        let obj = PdfObject::String(PdfString::new(encrypted));
        let decrypted =
            decrypt_object_recursive(obj, key, StringDecryptMode::Rc4, &test_diagnostics());

        match decrypted {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), plaintext),
            other => panic!("expected String, got {:?}", other),
        }
    }

    #[test]
    fn test_decrypt_recursive_nested_array() {
        let key = b"test_key";
        let encrypted = rc4::decrypt(key, b"inner");

        let obj = PdfObject::Array(vec![
            PdfObject::Integer(42),
            PdfObject::String(PdfString::new(encrypted)),
            PdfObject::Name(b"Untouched".to_vec()),
        ]);
        let decrypted =
            decrypt_object_recursive(obj, key, StringDecryptMode::Rc4, &test_diagnostics());

        match decrypted {
            PdfObject::Array(items) => {
                assert_eq!(items.len(), 3);
                assert!(matches!(&items[0], PdfObject::Integer(42)));
                match &items[1] {
                    PdfObject::String(s) => assert_eq!(s.as_bytes(), b"inner"),
                    other => panic!("expected String, got {:?}", other),
                }
                assert!(matches!(&items[2], PdfObject::Name(n) if n == b"Untouched"));
            }
            other => panic!("expected Array, got {:?}", other),
        }
    }

    #[test]
    fn test_decrypt_recursive_nested_dict() {
        let key = b"test_key";
        let encrypted = rc4::decrypt(key, b"value");

        let mut inner_dict = PdfDictionary::new();
        inner_dict.insert(
            b"Key".to_vec(),
            PdfObject::String(PdfString::new(encrypted)),
        );

        let mut outer_dict = PdfDictionary::new();
        outer_dict.insert(b"Inner".to_vec(), PdfObject::Dictionary(inner_dict));
        outer_dict.insert(b"Num".to_vec(), PdfObject::Integer(7));

        let obj = PdfObject::Dictionary(outer_dict);
        let decrypted =
            decrypt_object_recursive(obj, key, StringDecryptMode::Rc4, &test_diagnostics());

        match decrypted {
            PdfObject::Dictionary(d) => {
                let inner = d.get_dict(b"Inner").expect("missing Inner");
                let s = inner.get_str(b"Key").expect("missing Key");
                assert_eq!(s.as_bytes(), b"value");
                assert_eq!(d.get_i64(b"Num"), Some(7));
            }
            other => panic!("expected Dictionary, got {:?}", other),
        }
    }

    #[test]
    fn test_decrypt_recursive_non_string_passthrough() {
        let key = b"test_key";
        // Non-string types should pass through unchanged
        let obj = PdfObject::Integer(42);
        let result =
            decrypt_object_recursive(obj, key, StringDecryptMode::Rc4, &test_diagnostics());
        assert!(matches!(result, PdfObject::Integer(42)));

        let obj = PdfObject::Name(b"Hello".to_vec());
        let result =
            decrypt_object_recursive(obj, key, StringDecryptMode::Rc4, &test_diagnostics());
        assert!(matches!(result, PdfObject::Name(n) if n == b"Hello"));

        let obj = PdfObject::Boolean(true);
        let result =
            decrypt_object_recursive(obj, key, StringDecryptMode::Rc4, &test_diagnostics());
        assert!(matches!(result, PdfObject::Boolean(true)));
    }

    #[test]
    fn test_decrypt_recursive_depth_limit() {
        // Build a deeply nested array structure at MAX_DECRYPT_DEPTH.
        // The string at the bottom should NOT be decrypted (depth exceeded).
        let key = b"test_key";
        let encrypted = rc4::decrypt(key, b"deep");

        let mut obj = PdfObject::String(PdfString::new(encrypted.clone()));
        for _ in 0..MAX_DECRYPT_DEPTH {
            obj = PdfObject::Array(vec![obj]);
        }

        let result =
            decrypt_object_recursive(obj, key, StringDecryptMode::Rc4, &test_diagnostics());

        // Unwrap all the nesting
        let mut current = &result;
        for _ in 0..MAX_DECRYPT_DEPTH {
            match current {
                PdfObject::Array(items) => current = &items[0],
                other => panic!("expected Array, got {:?}", other),
            }
        }
        // The innermost string should still be encrypted (depth limit hit)
        match current {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), &encrypted),
            other => panic!("expected String, got {:?}", other),
        }
    }

    #[test]
    fn test_decrypt_recursive_just_under_depth_limit() {
        // Build nesting at MAX_DECRYPT_DEPTH - 1, so the string IS decrypted.
        let key = b"test_key";
        let encrypted = rc4::decrypt(key, b"reachable");

        let mut obj = PdfObject::String(PdfString::new(encrypted));
        for _ in 0..MAX_DECRYPT_DEPTH - 1 {
            obj = PdfObject::Array(vec![obj]);
        }

        let result =
            decrypt_object_recursive(obj, key, StringDecryptMode::Rc4, &test_diagnostics());

        let mut current = &result;
        for _ in 0..MAX_DECRYPT_DEPTH - 1 {
            match current {
                PdfObject::Array(items) => current = &items[0],
                other => panic!("expected Array, got {:?}", other),
            }
        }
        match current {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), b"reachable"),
            other => panic!("expected String, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_encrypt_dict_encrypt_metadata_false() {
        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(2));
        dict.insert(b"R".to_vec(), PdfObject::Integer(3));
        dict.insert(b"Length".to_vec(), PdfObject::Integer(128));
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(
            b"U".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));
        dict.insert(b"EncryptMetadata".to_vec(), PdfObject::Boolean(false));

        let config = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap();
        assert!(!config.encrypt_metadata);
    }

    #[test]
    fn test_parse_encrypt_dict_p_unsigned_representation() {
        // pypdf writes /P as unsigned (4294967292 instead of -4).
        // The parser must reinterpret the low 32 bits to get -4.
        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(1));
        dict.insert(b"R".to_vec(), PdfObject::Integer(2));
        dict.insert(b"Length".to_vec(), PdfObject::Integer(40));
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(
            b"U".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        // 4294967292u32 == 0xFFFFFFFC == -4i32
        dict.insert(b"P".to_vec(), PdfObject::Integer(4294967292));

        let config = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap();
        assert_eq!(config.p, -4, "unsigned /P must be reinterpreted as signed");
    }

    #[test]
    fn test_parse_encrypt_dict_o_too_short() {
        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(1));
        dict.insert(b"R".to_vec(), PdfObject::Integer(2));
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 16])),
        );
        dict.insert(
            b"U".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));

        let err = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap_err();
        assert!(err.to_string().contains("/O must be at least 32 bytes"));
    }

    #[test]
    fn test_parse_encrypt_dict_u_too_short() {
        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(1));
        dict.insert(b"R".to_vec(), PdfObject::Integer(2));
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(
            b"U".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 8])),
        );
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));

        let err = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap_err();
        assert!(err.to_string().contains("/U must be at least 16 bytes"));
    }

    #[test]
    fn test_parse_encrypt_dict_invalid_v_r_combos() {
        let make_dict = |v: i64, r: i64| {
            let mut dict = PdfDictionary::new();
            dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
            dict.insert(b"V".to_vec(), PdfObject::Integer(v));
            dict.insert(b"R".to_vec(), PdfObject::Integer(r));
            dict.insert(b"Length".to_vec(), PdfObject::Integer(40));
            dict.insert(
                b"O".to_vec(),
                PdfObject::String(PdfString::new(vec![0u8; 32])),
            );
            dict.insert(
                b"U".to_vec(),
                PdfObject::String(PdfString::new(vec![0u8; 32])),
            );
            dict.insert(b"P".to_vec(), PdfObject::Integer(-4));
            dict
        };

        let diag = test_diagnostics();

        // Valid combos: V=1/R=2, V=1/R=3, V=2/R=2, V=2/R=3, V=4/R=4
        assert!(parse_encrypt_dict(&make_dict(1, 2), &diag).is_ok());
        assert!(parse_encrypt_dict(&make_dict(1, 3), &diag).is_ok());
        assert!(parse_encrypt_dict(&make_dict(2, 2), &diag).is_ok());
        assert!(parse_encrypt_dict(&make_dict(2, 3), &diag).is_ok());
        assert!(parse_encrypt_dict(&make_dict(4, 4), &diag).is_ok());

        // Invalid V values
        assert!(parse_encrypt_dict(&make_dict(0, 2), &diag).is_err());
        assert!(parse_encrypt_dict(&make_dict(3, 2), &diag).is_err());
        assert!(parse_encrypt_dict(&make_dict(6, 6), &diag).is_err());

        // Invalid R values with valid V
        assert!(parse_encrypt_dict(&make_dict(1, 1), &diag).is_err());
        assert!(parse_encrypt_dict(&make_dict(2, 0), &diag).is_err());

        // V=1-2 with R=4/5/6 is lenient (real-world PDFs have mismatched combos)
        assert!(parse_encrypt_dict(&make_dict(1, 4), &diag).is_ok());
        assert!(parse_encrypt_dict(&make_dict(2, 4), &diag).is_ok());
        assert!(parse_encrypt_dict(&make_dict(1, 5), &diag).is_ok());
        assert!(parse_encrypt_dict(&make_dict(1, 6), &diag).is_ok());
    }

    // -- V=4 crypt filter tests --

    /// Helper: build a V=4/R=4 encrypt dict with a /CF entry.
    fn make_v4_dict(cfm_stm: &[u8], cfm_str: &[u8]) -> PdfDictionary {
        // Build individual filter dicts
        let mut stm_filter = PdfDictionary::new();
        stm_filter.insert(b"CFM".to_vec(), PdfObject::Name(cfm_stm.to_vec()));

        let mut str_filter = PdfDictionary::new();
        str_filter.insert(b"CFM".to_vec(), PdfObject::Name(cfm_str.to_vec()));

        let mut cf = PdfDictionary::new();
        cf.insert(b"StdCF_Stm".to_vec(), PdfObject::Dictionary(stm_filter));
        cf.insert(b"StdCF_Str".to_vec(), PdfObject::Dictionary(str_filter));

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(4));
        dict.insert(b"R".to_vec(), PdfObject::Integer(4));
        dict.insert(b"CF".to_vec(), PdfObject::Dictionary(cf));
        dict.insert(b"StmF".to_vec(), PdfObject::Name(b"StdCF_Stm".to_vec()));
        dict.insert(b"StrF".to_vec(), PdfObject::Name(b"StdCF_Str".to_vec()));
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(
            b"U".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));
        dict
    }

    #[test]
    fn test_parse_v4_aes128_both() {
        let dict = make_v4_dict(b"AESV2", b"AESV2");
        let config = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap();
        assert_eq!(config.v, 4);
        assert_eq!(config.r, 4);
        assert_eq!(config.key_length, 128);
        assert_eq!(config.stream_algorithm, CryptAlgorithm::Aes128);
        assert_eq!(config.string_algorithm, CryptAlgorithm::Aes128);
    }

    #[test]
    fn test_parse_v4_rc4_both() {
        let dict = make_v4_dict(b"V2", b"V2");
        let config = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap();
        assert_eq!(config.stream_algorithm, CryptAlgorithm::Rc4);
        assert_eq!(config.string_algorithm, CryptAlgorithm::Rc4);
    }

    #[test]
    fn test_parse_v4_mixed_filters() {
        // Streams use RC4, strings use AES (unusual but spec-valid)
        let dict = make_v4_dict(b"V2", b"AESV2");
        let config = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap();
        assert_eq!(config.stream_algorithm, CryptAlgorithm::Rc4);
        assert_eq!(config.string_algorithm, CryptAlgorithm::Aes128);
    }

    #[test]
    fn test_parse_v4_identity_filter() {
        // /StmF = Identity means streams are not encrypted
        let mut dict = make_v4_dict(b"AESV2", b"AESV2");
        dict.insert(b"StmF".to_vec(), PdfObject::Name(b"Identity".to_vec()));
        let config = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap();
        assert_eq!(config.stream_algorithm, CryptAlgorithm::None);
        assert_eq!(config.string_algorithm, CryptAlgorithm::Aes128);
    }

    #[test]
    fn test_parse_v4_no_stmf_strf_defaults_identity() {
        // Missing /StmF and /StrF default to Identity (no encryption)
        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(4));
        dict.insert(b"R".to_vec(), PdfObject::Integer(4));
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(
            b"U".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));

        let config = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap();
        assert_eq!(config.stream_algorithm, CryptAlgorithm::None);
        assert_eq!(config.string_algorithm, CryptAlgorithm::None);
    }

    #[test]
    fn test_parse_v4_missing_cf_warns() {
        // /StmF references a filter but /CF is missing -- fallback to RC4 with warning
        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(4));
        dict.insert(b"R".to_vec(), PdfObject::Integer(4));
        dict.insert(b"StmF".to_vec(), PdfObject::Name(b"StdCF".to_vec()));
        dict.insert(b"StrF".to_vec(), PdfObject::Name(b"StdCF".to_vec()));
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(
            b"U".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));

        let config = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap();
        // Falls back to RC4 when /CF is missing
        assert_eq!(config.stream_algorithm, CryptAlgorithm::Rc4);
        assert_eq!(config.string_algorithm, CryptAlgorithm::Rc4);
    }

    #[test]
    fn test_parse_v4_uses_same_filter_name() {
        // Common pattern: both /StmF and /StrF use the same filter name "StdCF"
        let mut cf_entry = PdfDictionary::new();
        cf_entry.insert(b"CFM".to_vec(), PdfObject::Name(b"AESV2".to_vec()));

        let mut cf = PdfDictionary::new();
        cf.insert(b"StdCF".to_vec(), PdfObject::Dictionary(cf_entry));

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(4));
        dict.insert(b"R".to_vec(), PdfObject::Integer(4));
        dict.insert(b"CF".to_vec(), PdfObject::Dictionary(cf));
        dict.insert(b"StmF".to_vec(), PdfObject::Name(b"StdCF".to_vec()));
        dict.insert(b"StrF".to_vec(), PdfObject::Name(b"StdCF".to_vec()));
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(
            b"U".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));

        let config = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap();
        assert_eq!(config.stream_algorithm, CryptAlgorithm::Aes128);
        assert_eq!(config.string_algorithm, CryptAlgorithm::Aes128);
    }

    #[test]
    fn test_v1_v2_always_rc4() {
        // V=1-2 should always produce RC4 regardless of anything else
        let diag = test_diagnostics();

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(1));
        dict.insert(b"R".to_vec(), PdfObject::Integer(2));
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(
            b"U".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));

        let config = parse_encrypt_dict(&dict, &diag).unwrap();
        assert_eq!(config.stream_algorithm, CryptAlgorithm::Rc4);
        assert_eq!(config.string_algorithm, CryptAlgorithm::Rc4);
    }

    // -- V=5 (AES-256) parse tests --

    /// Helper: build a V=5/R=6 encrypt dict with AESV3 crypt filter.
    fn make_v5_dict() -> PdfDictionary {
        let mut aesv3_filter = PdfDictionary::new();
        aesv3_filter.insert(b"CFM".to_vec(), PdfObject::Name(b"AESV3".to_vec()));

        let mut cf = PdfDictionary::new();
        cf.insert(b"StdCF".to_vec(), PdfObject::Dictionary(aesv3_filter));

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(5));
        dict.insert(b"R".to_vec(), PdfObject::Integer(6));
        dict.insert(b"CF".to_vec(), PdfObject::Dictionary(cf));
        dict.insert(b"StmF".to_vec(), PdfObject::Name(b"StdCF".to_vec()));
        dict.insert(b"StrF".to_vec(), PdfObject::Name(b"StdCF".to_vec()));
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 48])),
        );
        dict.insert(
            b"U".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 48])),
        );
        dict.insert(
            b"OE".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(
            b"UE".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(
            b"Perms".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 16])),
        );
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));
        dict
    }

    #[test]
    fn test_parse_v5_aes256() {
        let dict = make_v5_dict();
        let config = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap();
        assert_eq!(config.v, 5);
        assert_eq!(config.r, 6);
        assert_eq!(config.key_length, 256);
        assert_eq!(config.stream_algorithm, CryptAlgorithm::Aes256);
        assert_eq!(config.string_algorithm, CryptAlgorithm::Aes256);
        assert_eq!(config.o.len(), 48);
        assert_eq!(config.u.len(), 48);
        assert_eq!(config.oe.len(), 32);
        assert_eq!(config.ue.len(), 32);
        assert_eq!(config.perms.len(), 16);
    }

    /// Helper: build a V=5/R=6 dict without a specific field.
    fn make_v5_dict_without(skip_field: &[u8]) -> PdfDictionary {
        let mut aesv3_filter = PdfDictionary::new();
        aesv3_filter.insert(b"CFM".to_vec(), PdfObject::Name(b"AESV3".to_vec()));
        let mut cf = PdfDictionary::new();
        cf.insert(b"StdCF".to_vec(), PdfObject::Dictionary(aesv3_filter));

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(5));
        dict.insert(b"R".to_vec(), PdfObject::Integer(6));
        dict.insert(b"CF".to_vec(), PdfObject::Dictionary(cf));
        dict.insert(b"StmF".to_vec(), PdfObject::Name(b"StdCF".to_vec()));
        dict.insert(b"StrF".to_vec(), PdfObject::Name(b"StdCF".to_vec()));
        if skip_field != b"O" {
            dict.insert(
                b"O".to_vec(),
                PdfObject::String(PdfString::new(vec![0u8; 48])),
            );
        }
        if skip_field != b"U" {
            dict.insert(
                b"U".to_vec(),
                PdfObject::String(PdfString::new(vec![0u8; 48])),
            );
        }
        if skip_field != b"OE" {
            dict.insert(
                b"OE".to_vec(),
                PdfObject::String(PdfString::new(vec![0u8; 32])),
            );
        }
        if skip_field != b"UE" {
            dict.insert(
                b"UE".to_vec(),
                PdfObject::String(PdfString::new(vec![0u8; 32])),
            );
        }
        if skip_field != b"Perms" {
            dict.insert(
                b"Perms".to_vec(),
                PdfObject::String(PdfString::new(vec![0u8; 16])),
            );
        }
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));
        dict
    }

    #[test]
    fn test_parse_v5_missing_oe() {
        let dict = make_v5_dict_without(b"OE");
        let err = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap_err();
        assert!(err.to_string().contains("missing required field: OE"));
    }

    #[test]
    fn test_parse_v5_missing_ue() {
        let dict = make_v5_dict_without(b"UE");
        let err = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap_err();
        assert!(err.to_string().contains("missing required field: UE"));
    }

    #[test]
    fn test_parse_v5_missing_perms() {
        let dict = make_v5_dict_without(b"Perms");
        let err = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap_err();
        assert!(err.to_string().contains("missing required field: Perms"));
    }

    #[test]
    fn test_parse_v5_o_too_short() {
        // Build a V=5 dict with only 32-byte /O (needs 48)
        let mut dict = make_v5_dict_without(b"O");
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        let err = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap_err();
        assert!(err.to_string().contains("/O must be at least 48 bytes"));
    }

    #[test]
    fn test_parse_v5_u_too_short() {
        let mut dict = make_v5_dict_without(b"U");
        dict.insert(
            b"U".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        let err = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap_err();
        assert!(err.to_string().contains("/U must be at least 48 bytes"));
    }

    #[test]
    fn test_parse_v5_r5() {
        // Build a V=5/R=5 dict
        let mut dict = make_v5_dict_without(b"_none_");
        // Override R to 5 (insert again; PdfDictionary keeps last insert for lookups)
        dict.insert(b"R".to_vec(), PdfObject::Integer(5));
        let config = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap();
        assert_eq!(config.v, 5);
        // The dict has two /R entries, get_i64 returns the last one
        assert_eq!(config.r, 5);
        assert_eq!(config.key_length, 256);
    }

    // =========================================================================
    // Coverage gap: AES-128 string decryption (L377-388)
    // =========================================================================

    #[test]
    fn test_decrypt_recursive_aes128_string() {
        // Encrypt a string with AES-128-CBC, then decrypt it through the
        // recursive object decryption path (StringDecryptMode::Aes128).
        use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};

        let key = [0x42u8; 16];
        let iv = [0x10u8; 16];
        let plaintext = b"Hello, AES!";

        // Build AES-128-CBC encrypted data: [IV][ciphertext]
        let padded_len = ((plaintext.len() / 16) + 1) * 16;
        let mut buf = vec![0u8; padded_len];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let encryptor = cbc::Encryptor::<ExtAes128>::new_from_slices(&key, &iv).unwrap();
        let ciphertext = encryptor
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
            .unwrap();
        let mut encrypted = iv.to_vec();
        encrypted.extend_from_slice(ciphertext);

        let obj = PdfObject::String(PdfString::new(encrypted));
        let decrypted =
            decrypt_object_recursive(obj, &key, StringDecryptMode::Aes128, &test_diagnostics());

        match decrypted {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), plaintext),
            other => panic!("expected String, got {:?}", other),
        }
    }

    #[test]
    fn test_decrypt_recursive_aes128_string_failure_preserves_raw() {
        // When AES-128 string decryption fails (data too short), the fallback
        // returns raw bytes and emits a warning.
        use crate::diagnostics::CollectingDiagnostics;

        let key = [0x42u8; 16];
        // Data shorter than 32 bytes (IV + 1 block minimum) causes AES failure
        let short_data = vec![0xDE; 10];

        let diag = Arc::new(CollectingDiagnostics::new());
        let obj = PdfObject::String(PdfString::new(short_data.clone()));
        let result = decrypt_object_recursive(
            obj,
            &key,
            StringDecryptMode::Aes128,
            &(diag.clone() as Arc<dyn DiagnosticsSink>),
        );

        match result {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), &short_data),
            other => panic!("expected String, got {:?}", other),
        }
        let warnings = diag.warnings();
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0]
                .message
                .contains("AES-128 string decryption failed"),
            "got: {}",
            warnings[0].message
        );
    }

    // =========================================================================
    // Coverage gap: AES-256 string decryption (L389-400)
    // =========================================================================

    #[test]
    fn test_decrypt_recursive_aes256_string() {
        use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};

        let key = [0x99u8; 32];
        let iv = [0xAA; 16];
        let plaintext = b"AES-256 test!";

        let padded_len = ((plaintext.len() / 16) + 1) * 16;
        let mut buf = vec![0u8; padded_len];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let encryptor = cbc::Encryptor::<ExtAes256>::new_from_slices(&key, &iv).unwrap();
        let ciphertext = encryptor
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
            .unwrap();
        let mut encrypted = iv.to_vec();
        encrypted.extend_from_slice(ciphertext);

        let obj = PdfObject::String(PdfString::new(encrypted));
        let decrypted =
            decrypt_object_recursive(obj, &key, StringDecryptMode::Aes256, &test_diagnostics());

        match decrypted {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), plaintext),
            other => panic!("expected String, got {:?}", other),
        }
    }

    #[test]
    fn test_decrypt_recursive_aes256_string_failure_preserves_raw() {
        use crate::diagnostics::CollectingDiagnostics;

        let key = [0x99u8; 32];
        let short_data = vec![0xBB; 10];

        let diag = Arc::new(CollectingDiagnostics::new());
        let obj = PdfObject::String(PdfString::new(short_data.clone()));
        let result = decrypt_object_recursive(
            obj,
            &key,
            StringDecryptMode::Aes256,
            &(diag.clone() as Arc<dyn DiagnosticsSink>),
        );

        match result {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), &short_data),
            other => panic!("expected String, got {:?}", other),
        }
        let warnings = diag.warnings();
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0]
                .message
                .contains("AES-256 string decryption failed"),
            "got: {}",
            warnings[0].message
        );
    }

    // =========================================================================
    // Coverage gap: Stream decryption failure fallbacks (L161-183)
    // =========================================================================

    #[test]
    fn test_decrypt_stream_aes128_failure_returns_raw() {
        use crate::diagnostics::CollectingDiagnostics;

        let diag = Arc::new(CollectingDiagnostics::new());
        let config = EncryptionConfig {
            v: 4,
            r: 4,
            key_length: 128,
            o: vec![0u8; 32],
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: true,
            stream_algorithm: CryptAlgorithm::Aes128,
            string_algorithm: CryptAlgorithm::Aes128,
            oe: Vec::new(),
            ue: Vec::new(),
            perms: Vec::new(),
        };
        let handler = CryptHandler::new(
            vec![0x42u8; 16],
            config,
            None,
            diag.clone() as Arc<dyn DiagnosticsSink>,
        );

        // Data too short for AES (less than 32 bytes) triggers the fallback
        let bad_data = vec![0xDE; 10];
        let obj_ref = ObjRef { num: 1, gen: 0 };
        let result = handler.decrypt_stream_data(&bad_data, obj_ref);

        // Fallback: raw bytes returned as-is
        assert_eq!(result, bad_data);
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("AES-128 stream decryption failed")),
            "expected AES-128 stream failure warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_decrypt_stream_aes256_failure_returns_raw() {
        use crate::diagnostics::CollectingDiagnostics;

        let diag = Arc::new(CollectingDiagnostics::new());
        let config = EncryptionConfig {
            v: 5,
            r: 6,
            key_length: 256,
            o: vec![0u8; 48],
            u: vec![0u8; 48],
            p: -4,
            encrypt_metadata: true,
            stream_algorithm: CryptAlgorithm::Aes256,
            string_algorithm: CryptAlgorithm::Aes256,
            oe: vec![0u8; 32],
            ue: vec![0u8; 32],
            perms: vec![0u8; 16],
        };
        let handler = CryptHandler::new(
            vec![0x99u8; 32],
            config,
            None,
            diag.clone() as Arc<dyn DiagnosticsSink>,
        );

        let bad_data = vec![0xDE; 10];
        let obj_ref = ObjRef { num: 2, gen: 0 };
        let result = handler.decrypt_stream_data(&bad_data, obj_ref);

        assert_eq!(result, bad_data);
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("AES-256 stream decryption failed")),
            "expected AES-256 stream failure warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    // =========================================================================
    // Coverage gap: CryptAlgorithm::None passthrough (L99, L153)
    // =========================================================================

    #[test]
    fn test_decrypt_object_none_algorithm_passthrough() {
        // CryptAlgorithm::None in string_algorithm returns object unchanged
        let diag = test_diagnostics();
        let config = EncryptionConfig {
            v: 4,
            r: 4,
            key_length: 128,
            o: vec![0u8; 32],
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: true,
            stream_algorithm: CryptAlgorithm::None,
            string_algorithm: CryptAlgorithm::None,
            oe: Vec::new(),
            ue: Vec::new(),
            perms: Vec::new(),
        };
        let handler = CryptHandler::new(vec![0u8; 16], config, None, diag);

        let original_bytes = b"not encrypted at all";
        let obj = PdfObject::String(PdfString::new(original_bytes.to_vec()));
        let obj_ref = ObjRef { num: 5, gen: 0 };
        let result = handler.decrypt_object(obj, obj_ref);

        match result {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), original_bytes),
            other => panic!("expected String, got {:?}", other),
        }
    }

    #[test]
    fn test_decrypt_stream_none_algorithm_passthrough() {
        // CryptAlgorithm::None in stream_algorithm returns data as-is (cloned)
        let diag = test_diagnostics();
        let config = EncryptionConfig {
            v: 4,
            r: 4,
            key_length: 128,
            o: vec![0u8; 32],
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: true,
            stream_algorithm: CryptAlgorithm::None,
            string_algorithm: CryptAlgorithm::None,
            oe: Vec::new(),
            ue: Vec::new(),
            perms: Vec::new(),
        };
        let handler = CryptHandler::new(vec![0u8; 16], config, None, diag);

        let data = b"stream data in the clear";
        let obj_ref = ObjRef { num: 5, gen: 0 };
        let result = handler.decrypt_stream_data(data, obj_ref);
        assert_eq!(result, data);
    }

    // =========================================================================
    // Coverage gap: Crypt filter edge cases (L647-683)
    // =========================================================================

    #[test]
    fn test_parse_v4_unknown_cf_filter_name() {
        // /CF exists but the filter name referenced by /StmF is not in it.
        // Should fall back to RC4 with a warning.
        use crate::diagnostics::CollectingDiagnostics;

        let mut filter = PdfDictionary::new();
        filter.insert(b"CFM".to_vec(), PdfObject::Name(b"AESV2".to_vec()));

        let mut cf = PdfDictionary::new();
        cf.insert(b"StdCF".to_vec(), PdfObject::Dictionary(filter));

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(4));
        dict.insert(b"R".to_vec(), PdfObject::Integer(4));
        dict.insert(b"CF".to_vec(), PdfObject::Dictionary(cf));
        // Reference a filter name that doesn't exist in /CF
        dict.insert(b"StmF".to_vec(), PdfObject::Name(b"NonExistent".to_vec()));
        dict.insert(b"StrF".to_vec(), PdfObject::Name(b"StdCF".to_vec()));
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(
            b"U".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));

        let diag = Arc::new(CollectingDiagnostics::new());
        let config =
            parse_encrypt_dict(&dict, &(diag.clone() as Arc<dyn DiagnosticsSink>)).unwrap();

        // Stream filter falls back to RC4
        assert_eq!(config.stream_algorithm, CryptAlgorithm::Rc4);
        // String filter found correctly
        assert_eq!(config.string_algorithm, CryptAlgorithm::Aes128);

        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("not found in /CF")),
            "expected 'not found in /CF' warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_parse_v4_unknown_cfm_value() {
        // /CF has the filter, but /CFM has an unknown value.
        use crate::diagnostics::CollectingDiagnostics;

        let mut filter = PdfDictionary::new();
        filter.insert(b"CFM".to_vec(), PdfObject::Name(b"WeirdAlgo".to_vec()));

        let mut cf = PdfDictionary::new();
        cf.insert(b"StdCF".to_vec(), PdfObject::Dictionary(filter));

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(4));
        dict.insert(b"R".to_vec(), PdfObject::Integer(4));
        dict.insert(b"CF".to_vec(), PdfObject::Dictionary(cf));
        dict.insert(b"StmF".to_vec(), PdfObject::Name(b"StdCF".to_vec()));
        dict.insert(b"StrF".to_vec(), PdfObject::Name(b"StdCF".to_vec()));
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(
            b"U".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));

        let diag = Arc::new(CollectingDiagnostics::new());
        let config =
            parse_encrypt_dict(&dict, &(diag.clone() as Arc<dyn DiagnosticsSink>)).unwrap();

        // Both fall back to RC4
        assert_eq!(config.stream_algorithm, CryptAlgorithm::Rc4);
        assert_eq!(config.string_algorithm, CryptAlgorithm::Rc4);

        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("unknown /CFM value")),
            "expected 'unknown /CFM value' warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_parse_v4_missing_cfm_in_filter() {
        // /CF has the filter dict, but it's missing /CFM entirely.
        // Should default to None (identity) per spec Table 3.23.
        use crate::diagnostics::CollectingDiagnostics;

        let filter = PdfDictionary::new(); // empty, no /CFM

        let mut cf = PdfDictionary::new();
        cf.insert(b"StdCF".to_vec(), PdfObject::Dictionary(filter));

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(4));
        dict.insert(b"R".to_vec(), PdfObject::Integer(4));
        dict.insert(b"CF".to_vec(), PdfObject::Dictionary(cf));
        dict.insert(b"StmF".to_vec(), PdfObject::Name(b"StdCF".to_vec()));
        dict.insert(b"StrF".to_vec(), PdfObject::Name(b"StdCF".to_vec()));
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(
            b"U".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));

        let diag = Arc::new(CollectingDiagnostics::new());
        let config =
            parse_encrypt_dict(&dict, &(diag.clone() as Arc<dyn DiagnosticsSink>)).unwrap();

        // Missing /CFM defaults to None (identity)
        assert_eq!(config.stream_algorithm, CryptAlgorithm::None);
        assert_eq!(config.string_algorithm, CryptAlgorithm::None);

        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.message.contains("missing /CFM")),
            "expected 'missing /CFM' warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_parse_v4_cfm_none_explicit() {
        // /CFM = /None (explicit identity filter)
        let mut filter = PdfDictionary::new();
        filter.insert(b"CFM".to_vec(), PdfObject::Name(b"None".to_vec()));

        let mut cf = PdfDictionary::new();
        cf.insert(b"StdCF".to_vec(), PdfObject::Dictionary(filter));

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(4));
        dict.insert(b"R".to_vec(), PdfObject::Integer(4));
        dict.insert(b"CF".to_vec(), PdfObject::Dictionary(cf));
        dict.insert(b"StmF".to_vec(), PdfObject::Name(b"StdCF".to_vec()));
        dict.insert(b"StrF".to_vec(), PdfObject::Name(b"StdCF".to_vec()));
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(
            b"U".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));

        let config = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap();
        assert_eq!(config.stream_algorithm, CryptAlgorithm::None);
        assert_eq!(config.string_algorithm, CryptAlgorithm::None);
    }

    // =========================================================================
    // Coverage gap: V=2 without /Length (L479-484)
    // =========================================================================

    #[test]
    fn test_parse_v2_no_length_defaults_128() {
        use crate::diagnostics::CollectingDiagnostics;

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(2));
        dict.insert(b"R".to_vec(), PdfObject::Integer(3));
        // No /Length entry
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(
            b"U".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));

        let diag = Arc::new(CollectingDiagnostics::new());
        let config =
            parse_encrypt_dict(&dict, &(diag.clone() as Arc<dyn DiagnosticsSink>)).unwrap();

        assert_eq!(
            config.key_length, 128,
            "V=2 without /Length should default to 128"
        );
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("V=2 but no /Length")),
            "expected V=2 no /Length warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    // =========================================================================
    // Coverage gap: R=5 /Perms validation failure (L282-287)
    // =========================================================================

    #[test]
    fn test_try_passwords_r56_perms_validation_failure() {
        // Build a V=5/R=5 config where the empty password works but /Perms
        // validation fails (bytes 9-11 are not "adb").
        use crate::diagnostics::CollectingDiagnostics;

        // To make the empty password work, we need /U[32..40] (validation salt)
        // such that SHA-256("" + salt) == /U[0..32].
        // We use R=5 (simple SHA-256) for simplicity.
        let password = b"";
        let validation_salt = [0x11u8; 8];
        let key_salt = [0x22u8; 8];

        // Compute the expected hash for validation
        let u_hash = key::compute_hash_r5(password, &validation_salt, &[]);

        // Build /U: [hash(32)][validation_salt(8)][key_salt(8)]
        let mut u = Vec::with_capacity(48);
        u.extend_from_slice(&u_hash);
        u.extend_from_slice(&validation_salt);
        u.extend_from_slice(&key_salt);

        // Compute intermediate key from key_salt
        let intermediate = key::compute_hash_r5(password, &key_salt, &[]);

        // Build /UE: encrypt a known FEK with AES-256-CBC, zero IV
        let fek = [0xABu8; 32];
        let ue = {
            use cbc::cipher::{block_padding::NoPadding, BlockEncryptMut, KeyIvInit};
            let iv = [0u8; 16];
            let mut buf = fek.to_vec();
            let enc = cbc::Encryptor::<ExtAes256>::new_from_slices(&intermediate, &iv).unwrap();
            enc.encrypt_padded_mut::<NoPadding>(&mut buf, 32).unwrap();
            buf
        };

        // Build /Perms: 16 bytes of garbage (will fail "adb" check)
        let perms = vec![0xFFu8; 16];

        // Build /O and /OE (won't be used since empty password matches /U)
        let o = vec![0u8; 48];
        let oe = vec![0u8; 32];

        let diag = Arc::new(CollectingDiagnostics::new());
        let config = EncryptionConfig {
            v: 5,
            r: 5,
            key_length: 256,
            o,
            u,
            p: -4,
            encrypt_metadata: true,
            stream_algorithm: CryptAlgorithm::Aes256,
            string_algorithm: CryptAlgorithm::Aes256,
            oe,
            ue,
            perms,
        };

        let result = CryptHandler::try_passwords_r56(
            &config,
            None,
            None,
            &(diag.clone() as Arc<dyn DiagnosticsSink>),
        );

        // Should succeed (password validation passes)
        assert!(
            result.is_ok(),
            "expected Ok, got Err({})",
            result
                .as_ref()
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default()
        );

        // But /Perms warning should have been emitted
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("/Perms validation failed")),
            "expected /Perms validation warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    // =========================================================================
    // Coverage gap: CryptHandler.decrypt_object with Aes128 and Aes256
    // (L110-127) - exercises the decrypt_object method dispatch
    // =========================================================================

    #[test]
    fn test_crypt_handler_decrypt_object_aes128() {
        use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};

        let doc_key = vec![0x42u8; 16]; // 128-bit document key
        let config = EncryptionConfig {
            v: 4,
            r: 4,
            key_length: 128,
            o: vec![0u8; 32],
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: true,
            stream_algorithm: CryptAlgorithm::Aes128,
            string_algorithm: CryptAlgorithm::Aes128,
            oe: Vec::new(),
            ue: Vec::new(),
            perms: Vec::new(),
        };
        let handler = CryptHandler::new(doc_key.clone(), config, None, test_diagnostics());

        let obj_ref = ObjRef { num: 7, gen: 0 };
        let plaintext = b"encrypted string";

        // Derive the per-object AES key the same way CryptHandler would
        let obj_key = key::per_object_key_aes(&doc_key, obj_ref);

        // Encrypt with AES-128-CBC
        let iv = [0x55u8; 16];
        let padded_len = ((plaintext.len() / 16) + 1) * 16;
        let mut buf = vec![0u8; padded_len];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let enc = cbc::Encryptor::<ExtAes128>::new_from_slices(&obj_key, &iv).unwrap();
        let ct = enc
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
            .unwrap();
        let mut encrypted = iv.to_vec();
        encrypted.extend_from_slice(ct);

        let obj = PdfObject::String(PdfString::new(encrypted));
        let result = handler.decrypt_object(obj, obj_ref);

        match result {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), plaintext),
            other => panic!("expected String, got {:?}", other),
        }
    }

    #[test]
    fn test_crypt_handler_decrypt_object_aes256() {
        use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};

        let doc_key = vec![0x99u8; 32]; // 256-bit document key
        let config = EncryptionConfig {
            v: 5,
            r: 6,
            key_length: 256,
            o: vec![0u8; 48],
            u: vec![0u8; 48],
            p: -4,
            encrypt_metadata: true,
            stream_algorithm: CryptAlgorithm::Aes256,
            string_algorithm: CryptAlgorithm::Aes256,
            oe: vec![0u8; 32],
            ue: vec![0u8; 32],
            perms: vec![0u8; 16],
        };
        let handler = CryptHandler::new(doc_key.clone(), config, None, test_diagnostics());

        let obj_ref = ObjRef { num: 3, gen: 0 };
        let plaintext = b"aes256 encrypted";

        // V=5 uses the document key directly (no per-object derivation)
        let iv = [0x77u8; 16];
        let padded_len = ((plaintext.len() / 16) + 1) * 16;
        let mut buf = vec![0u8; padded_len];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let enc = cbc::Encryptor::<ExtAes256>::new_from_slices(&doc_key, &iv).unwrap();
        let ct = enc
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
            .unwrap();
        let mut encrypted = iv.to_vec();
        encrypted.extend_from_slice(ct);

        let obj = PdfObject::String(PdfString::new(encrypted));
        let result = handler.decrypt_object(obj, obj_ref);

        match result {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), plaintext),
            other => panic!("expected String, got {:?}", other),
        }
    }

    #[test]
    fn test_crypt_handler_skip_encrypt_dict() {
        // decrypt_object should return the object unchanged for the /Encrypt dict itself
        let doc_key = vec![0x42u8; 16];
        let encrypt_ref = ObjRef { num: 10, gen: 0 };
        let config = EncryptionConfig {
            v: 4,
            r: 4,
            key_length: 128,
            o: vec![0u8; 32],
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: true,
            stream_algorithm: CryptAlgorithm::Aes128,
            string_algorithm: CryptAlgorithm::Aes128,
            oe: Vec::new(),
            ue: Vec::new(),
            perms: Vec::new(),
        };
        let handler = CryptHandler::new(doc_key, config, Some(encrypt_ref), test_diagnostics());

        let original = b"should not be decrypted";
        let obj = PdfObject::String(PdfString::new(original.to_vec()));
        let result = handler.decrypt_object(obj, encrypt_ref);

        match result {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), original),
            other => panic!("expected String, got {:?}", other),
        }
    }

    // =========================================================================
    // Coverage: should_skip edge cases
    // =========================================================================

    #[test]
    fn test_should_skip_metadata_when_encrypt_metadata_false() {
        let config = EncryptionConfig {
            v: 4,
            r: 4,
            key_length: 128,
            o: vec![0u8; 32],
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: false,
            stream_algorithm: CryptAlgorithm::Aes128,
            string_algorithm: CryptAlgorithm::Aes128,
            oe: Vec::new(),
            ue: Vec::new(),
            perms: Vec::new(),
        };
        let handler = CryptHandler::new(vec![0u8; 16], config, None, test_diagnostics());

        let obj_ref = ObjRef { num: 5, gen: 0 };
        assert!(handler.should_skip(obj_ref, Some(b"Metadata")));
        assert!(handler.should_skip(obj_ref, Some(b"XRef")));
        assert!(!handler.should_skip(obj_ref, Some(b"Page")));
        assert!(!handler.should_skip(obj_ref, None));
    }

    #[test]
    fn test_should_skip_xref_stream() {
        let config = EncryptionConfig {
            v: 2,
            r: 3,
            key_length: 128,
            o: vec![0u8; 32],
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: true,
            stream_algorithm: CryptAlgorithm::Rc4,
            string_algorithm: CryptAlgorithm::Rc4,
            oe: Vec::new(),
            ue: Vec::new(),
            perms: Vec::new(),
        };
        let handler = CryptHandler::new(vec![0u8; 16], config, None, test_diagnostics());

        let obj_ref = ObjRef { num: 1, gen: 0 };
        assert!(handler.should_skip(obj_ref, Some(b"XRef")));
        assert!(!handler.should_skip(obj_ref, Some(b"Page")));
    }

    // =========================================================================
    // Coverage: AES-128 stream decryption success path
    // =========================================================================

    #[test]
    fn test_decrypt_stream_aes128_success() {
        use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};

        let doc_key = vec![0x42u8; 16];
        let config = EncryptionConfig {
            v: 4,
            r: 4,
            key_length: 128,
            o: vec![0u8; 32],
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: true,
            stream_algorithm: CryptAlgorithm::Aes128,
            string_algorithm: CryptAlgorithm::Aes128,
            oe: Vec::new(),
            ue: Vec::new(),
            perms: Vec::new(),
        };
        let handler = CryptHandler::new(doc_key.clone(), config, None, test_diagnostics());

        let obj_ref = ObjRef { num: 3, gen: 0 };
        let plaintext = b"stream content here";

        // Derive the per-object AES key
        let obj_key = key::per_object_key_aes(&doc_key, obj_ref);

        // Encrypt
        let iv = [0x33u8; 16];
        let padded_len = ((plaintext.len() / 16) + 1) * 16;
        let mut buf = vec![0u8; padded_len];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let enc = cbc::Encryptor::<ExtAes128>::new_from_slices(&obj_key, &iv).unwrap();
        let ct = enc
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
            .unwrap();
        let mut encrypted = iv.to_vec();
        encrypted.extend_from_slice(ct);

        let result = handler.decrypt_stream_data(&encrypted, obj_ref);
        assert_eq!(result, plaintext);
    }

    #[test]
    fn test_decrypt_stream_aes256_success() {
        use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};

        let doc_key = vec![0x99u8; 32];
        let config = EncryptionConfig {
            v: 5,
            r: 6,
            key_length: 256,
            o: vec![0u8; 48],
            u: vec![0u8; 48],
            p: -4,
            encrypt_metadata: true,
            stream_algorithm: CryptAlgorithm::Aes256,
            string_algorithm: CryptAlgorithm::Aes256,
            oe: vec![0u8; 32],
            ue: vec![0u8; 32],
            perms: vec![0u8; 16],
        };
        let handler = CryptHandler::new(doc_key.clone(), config, None, test_diagnostics());

        let obj_ref = ObjRef { num: 4, gen: 0 };
        let plaintext = b"aes256 stream";

        // V=5: use document key directly
        let iv = [0x44u8; 16];
        let padded_len = ((plaintext.len() / 16) + 1) * 16;
        let mut buf = vec![0u8; padded_len];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let enc = cbc::Encryptor::<ExtAes256>::new_from_slices(&doc_key, &iv).unwrap();
        let ct = enc
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
            .unwrap();
        let mut encrypted = iv.to_vec();
        encrypted.extend_from_slice(ct);

        let result = handler.decrypt_stream_data(&encrypted, obj_ref);
        assert_eq!(result, plaintext);
    }

    // =========================================================================
    // Coverage: V=4 with /CFM = /AESV3 (resolves to AES-256)
    // =========================================================================

    #[test]
    fn test_parse_v4_cfm_aesv3() {
        // Unusual but valid: V=4 with /CFM = /AESV3 (AES-256 algorithm)
        let dict = make_v4_dict(b"AESV3", b"AESV3");
        let config = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap();
        assert_eq!(config.stream_algorithm, CryptAlgorithm::Aes256);
        assert_eq!(config.string_algorithm, CryptAlgorithm::Aes256);
    }

    // =========================================================================
    // Coverage: AES string decryption in nested structures
    // =========================================================================

    #[test]
    fn test_decrypt_recursive_aes128_in_dict() {
        use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};

        let key = [0x42u8; 16];
        let iv = [0x10u8; 16];
        let plaintext = b"nested aes";

        let padded_len = ((plaintext.len() / 16) + 1) * 16;
        let mut buf = vec![0u8; padded_len];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let enc = cbc::Encryptor::<ExtAes128>::new_from_slices(&key, &iv).unwrap();
        let ct = enc
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
            .unwrap();
        let mut encrypted = iv.to_vec();
        encrypted.extend_from_slice(ct);

        let mut inner_dict = PdfDictionary::new();
        inner_dict.insert(
            b"Value".to_vec(),
            PdfObject::String(PdfString::new(encrypted)),
        );

        let obj = PdfObject::Dictionary(inner_dict);
        let decrypted =
            decrypt_object_recursive(obj, &key, StringDecryptMode::Aes128, &test_diagnostics());

        match decrypted {
            PdfObject::Dictionary(d) => {
                let s = d.get_str(b"Value").expect("missing Value");
                assert_eq!(s.as_bytes(), plaintext);
            }
            other => panic!("expected Dictionary, got {:?}", other),
        }
    }

    // =========================================================================
    // Coverage gap: CryptHandler.decrypt_object / decrypt_stream_data with RC4
    // (L101-107, L155-157) - exercises the CryptHandler method dispatch for RC4
    // =========================================================================

    #[test]
    fn test_crypt_handler_decrypt_object_rc4() {
        // Exercise CryptHandler.decrypt_object with string_algorithm = Rc4
        let doc_key = vec![0x42u8; 5]; // 40-bit key
        let config = EncryptionConfig {
            v: 1,
            r: 2,
            key_length: 40,
            o: vec![0u8; 32],
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: true,
            stream_algorithm: CryptAlgorithm::Rc4,
            string_algorithm: CryptAlgorithm::Rc4,
            oe: Vec::new(),
            ue: Vec::new(),
            perms: Vec::new(),
        };
        let handler = CryptHandler::new(doc_key.clone(), config, None, test_diagnostics());

        let obj_ref = ObjRef { num: 1, gen: 0 };
        let plaintext = b"rc4 string";

        // Derive per-object key the same way CryptHandler will
        let (n, obj_key) = key::per_object_key(&doc_key, obj_ref, 40);
        // RC4 encrypt = RC4 decrypt (symmetric)
        let encrypted = rc4::decrypt(&obj_key[..n], plaintext);

        let obj = PdfObject::String(PdfString::new(encrypted));
        let result = handler.decrypt_object(obj, obj_ref);

        match result {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), plaintext),
            other => panic!("expected String, got {:?}", other),
        }
    }

    #[test]
    fn test_crypt_handler_decrypt_stream_rc4() {
        // Exercise CryptHandler.decrypt_stream_data with stream_algorithm = Rc4
        let doc_key = vec![0x42u8; 5];
        let config = EncryptionConfig {
            v: 1,
            r: 2,
            key_length: 40,
            o: vec![0u8; 32],
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: true,
            stream_algorithm: CryptAlgorithm::Rc4,
            string_algorithm: CryptAlgorithm::Rc4,
            oe: Vec::new(),
            ue: Vec::new(),
            perms: Vec::new(),
        };
        let handler = CryptHandler::new(doc_key.clone(), config, None, test_diagnostics());

        let obj_ref = ObjRef { num: 2, gen: 0 };
        let plaintext = b"rc4 stream data";

        let (n, obj_key) = key::per_object_key(&doc_key, obj_ref, 40);
        let encrypted = rc4::decrypt(&obj_key[..n], plaintext);

        let result = handler.decrypt_stream_data(&encrypted, obj_ref);
        assert_eq!(result, plaintext);
    }

    // =========================================================================
    // Coverage gap: V=5 field too-short validation (L549-574)
    // /OE too short, /UE too short, /Perms too short
    // =========================================================================

    #[test]
    fn test_parse_v5_oe_too_short() {
        let mut dict = make_v5_dict_without(b"OE");
        dict.insert(
            b"OE".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 16])), // needs 32
        );
        let err = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap_err();
        assert!(err.to_string().contains("/OE must be 32 bytes"));
    }

    #[test]
    fn test_parse_v5_ue_too_short() {
        let mut dict = make_v5_dict_without(b"UE");
        dict.insert(
            b"UE".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 16])), // needs 32
        );
        let err = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap_err();
        assert!(err.to_string().contains("/UE must be 32 bytes"));
    }

    #[test]
    fn test_parse_v5_perms_too_short() {
        let mut dict = make_v5_dict_without(b"Perms");
        dict.insert(
            b"Perms".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 8])), // needs 16
        );
        let err = parse_encrypt_dict(&dict, &test_diagnostics()).unwrap_err();
        assert!(err.to_string().contains("/Perms must be 16 bytes"));
    }

    // =========================================================================
    // Coverage gap: from_encrypt_dict integration (L192-260)
    // Tests the full password validation flow through from_encrypt_dict
    // =========================================================================

    #[test]
    fn test_from_encrypt_dict_empty_password_v1() {
        // Build a V=1/R=2 encrypt dict where the empty password works.
        // We need /U to match the expected hash for the empty password.
        use crate::diagnostics::CollectingDiagnostics;

        let file_id = b"0123456789abcdef"; // 16-byte file ID

        let temp_config = EncryptionConfig {
            v: 1,
            r: 2,
            key_length: 40,
            o: vec![0u8; 32],
            u: vec![0u8; 32], // placeholder
            p: -4,
            encrypt_metadata: true,
            stream_algorithm: CryptAlgorithm::Rc4,
            string_algorithm: CryptAlgorithm::Rc4,
            oe: Vec::new(),
            ue: Vec::new(),
            perms: Vec::new(),
        };

        // Derive document key with empty password
        let doc_key = key::derive_document_key(b"", &temp_config, file_id);
        let key_len = temp_config.key_length / 8;

        // For R=2, /U = RC4(doc_key[..key_len], PASSWORD_PADDING)
        // pad_password(b"") returns PASSWORD_PADDING
        let padding = key::pad_password(b"");
        let u_hash = rc4::decrypt(&doc_key[..key_len], &padding);

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(1));
        dict.insert(b"R".to_vec(), PdfObject::Integer(2));
        dict.insert(b"Length".to_vec(), PdfObject::Integer(40));
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(b"U".to_vec(), PdfObject::String(PdfString::new(u_hash)));
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));

        let diag = Arc::new(CollectingDiagnostics::new());
        let result = CryptHandler::from_encrypt_dict(
            &dict,
            file_id,
            None,
            None,
            &(diag.clone() as Arc<dyn DiagnosticsSink>),
        );

        assert!(
            result.is_ok(),
            "from_encrypt_dict should succeed with empty password: {:?}",
            result.err()
        );

        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("opened with empty password")),
            "expected 'opened with empty password' info, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_from_encrypt_dict_invalid_password() {
        // Build a V=1/R=2 dict where no password works
        use crate::diagnostics::CollectingDiagnostics;

        let file_id = b"0123456789abcdef";

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(1));
        dict.insert(b"R".to_vec(), PdfObject::Integer(2));
        dict.insert(b"Length".to_vec(), PdfObject::Integer(40));
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0xAA; 32])),
        );
        // /U with random bytes that won't match any password
        dict.insert(
            b"U".to_vec(),
            PdfObject::String(PdfString::new(vec![0xBB; 32])),
        );
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));

        let diag = Arc::new(CollectingDiagnostics::new());
        let result = CryptHandler::from_encrypt_dict(
            &dict,
            file_id,
            None,
            None,
            &(diag.clone() as Arc<dyn DiagnosticsSink>),
        );

        match result {
            Ok(_) => panic!("expected error for invalid password"),
            Err(err) => assert!(
                err.to_string().contains("invalid password"),
                "expected invalid password error, got: {}",
                err
            ),
        }
    }

    #[test]
    fn test_from_encrypt_dict_user_password_v1() {
        // Build a V=1/R=2 dict where a specific user password works
        use crate::diagnostics::CollectingDiagnostics;

        let file_id = b"0123456789abcdef";
        let password = b"secret";

        let temp_config = EncryptionConfig {
            v: 1,
            r: 2,
            key_length: 40,
            o: vec![0u8; 32],
            u: vec![0u8; 32],
            p: -4,
            encrypt_metadata: true,
            stream_algorithm: CryptAlgorithm::Rc4,
            string_algorithm: CryptAlgorithm::Rc4,
            oe: Vec::new(),
            ue: Vec::new(),
            perms: Vec::new(),
        };

        // Derive doc key with the user password
        let doc_key = key::derive_document_key(password, &temp_config, file_id);
        let key_len = temp_config.key_length / 8;
        let padding = key::pad_password(b"");
        let u_hash = rc4::decrypt(&doc_key[..key_len], &padding);

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(1));
        dict.insert(b"R".to_vec(), PdfObject::Integer(2));
        dict.insert(b"Length".to_vec(), PdfObject::Integer(40));
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0u8; 32])),
        );
        dict.insert(b"U".to_vec(), PdfObject::String(PdfString::new(u_hash)));
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));

        let diag = Arc::new(CollectingDiagnostics::new());
        let result = CryptHandler::from_encrypt_dict(
            &dict,
            file_id,
            Some(password),
            None,
            &(diag.clone() as Arc<dyn DiagnosticsSink>),
        );

        assert!(
            result.is_ok(),
            "should open with user password: {:?}",
            result.err()
        );

        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("opened with user password")),
            "expected 'opened with user password' info, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_from_encrypt_dict_supplied_password_fails_both() {
        // A supplied password that fails both user and owner validation
        use crate::diagnostics::CollectingDiagnostics;

        let file_id = b"0123456789abcdef";

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Standard".to_vec()));
        dict.insert(b"V".to_vec(), PdfObject::Integer(1));
        dict.insert(b"R".to_vec(), PdfObject::Integer(2));
        dict.insert(b"Length".to_vec(), PdfObject::Integer(40));
        dict.insert(
            b"O".to_vec(),
            PdfObject::String(PdfString::new(vec![0xCC; 32])),
        );
        dict.insert(
            b"U".to_vec(),
            PdfObject::String(PdfString::new(vec![0xDD; 32])),
        );
        dict.insert(b"P".to_vec(), PdfObject::Integer(-4));

        let diag = Arc::new(CollectingDiagnostics::new());
        let result = CryptHandler::from_encrypt_dict(
            &dict,
            file_id,
            Some(b"wrongpassword"),
            None,
            &(diag.clone() as Arc<dyn DiagnosticsSink>),
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_decrypt_recursive_aes256_in_array() {
        use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};

        let key = [0x99u8; 32];
        let iv = [0xBB; 16];
        let plaintext = b"array element";

        let padded_len = ((plaintext.len() / 16) + 1) * 16;
        let mut buf = vec![0u8; padded_len];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let enc = cbc::Encryptor::<ExtAes256>::new_from_slices(&key, &iv).unwrap();
        let ct = enc
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
            .unwrap();
        let mut encrypted = iv.to_vec();
        encrypted.extend_from_slice(ct);

        let obj = PdfObject::Array(vec![
            PdfObject::Integer(1),
            PdfObject::String(PdfString::new(encrypted)),
        ]);
        let decrypted =
            decrypt_object_recursive(obj, &key, StringDecryptMode::Aes256, &test_diagnostics());

        match decrypted {
            PdfObject::Array(items) => {
                assert_eq!(items.len(), 2);
                match &items[1] {
                    PdfObject::String(s) => assert_eq!(s.as_bytes(), plaintext),
                    other => panic!("expected String, got {:?}", other),
                }
            }
            other => panic!("expected Array, got {:?}", other),
        }
    }
}

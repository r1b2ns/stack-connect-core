//! `.scexport` account-export crypto, byte-for-byte compatible with the iOS
//! `AccountCrypto` implementation so export files are interchangeable between
//! the iOS app and the Flutter apps.
//!
//! # File format (`.scexport`)
//!
//! ```text
//! [4 bytes]  Magic: "SCEX"
//! [1 byte]   Version
//! [4 bytes]  PBKDF2 iterations (u32 big-endian) — v3 only
//! [16 bytes] Random salt
//! [12 bytes] AES-GCM nonce
//! [N bytes]  Ciphertext + GCM tag (16 bytes)
//! ```
//!
//! # Versions
//!
//! - **v1** (`0x01`, legacy, decrypt-only): key =
//!   `HKDF-SHA256(IKM = SHA256(appSalt || password), salt = randomSalt,
//!   info = "StackConnect.Export.v1", L = 32)`.
//! - **v2** (`0x02`, legacy, decrypt-only): key =
//!   `PBKDF2-HMAC-SHA256(password, salt = appSalt || randomSalt, 600_000, 32)`.
//!   The iteration count is *implied* by the constant — it is not stored.
//! - **v3** (`0x03`, current): same KDF as v2, but the iteration count is stored
//!   in the header (so changing the default later does not break older files).
//!
//! [`encrypt`] always writes v3 with [`PBKDF2_ITERATIONS`] iterations, a fresh
//! 16-byte random salt and a fresh 12-byte random nonce. [`decrypt`] accepts
//! v1, v2 and v3.

use std::collections::HashMap;

use aes_gcm::{
    aead::{rand_core::RngCore, Aead, KeyInit, OsRng, Payload},
    AeadCore, Aes256Gcm, Key, Nonce,
};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};

/// Fixed app-level salt (32 bytes), copied verbatim from the iOS `AccountCrypto`.
/// Mixed with the per-file random salt so decryption requires both the user
/// password AND knowledge of this constant. Not a secret (it ships in every
/// build) — it binds the format to StackConnect. MUST stay identical to iOS.
const APP_SALT: [u8; 32] = [
    0x4A, 0xC7, 0x1E, 0x93, 0xD2, 0x58, 0xBF, 0x06, 0x7D, 0xA4, 0x3B, 0xE1, 0x90, 0xF5, 0x2C, 0x68,
    0x15, 0x87, 0xDA, 0x4F, 0xC3, 0x72, 0xAE, 0x09, 0x5B, 0xE6, 0x31, 0x8C, 0xF0, 0x64, 0xA9, 0x2D,
];

/// Magic prefix identifying a StackConnect export file: ASCII `"SCEX"`.
const MAGIC: &[u8; 4] = b"SCEX";

/// Current (and only written) format version.
const VERSION_V3: u8 = 0x03;
/// Legacy PBKDF2 version with an implied iteration count.
const VERSION_V2: u8 = 0x02;
/// Legacy HKDF version.
const VERSION_V1: u8 = 0x01;

/// HKDF `info` string for v1 key derivation. MUST stay identical to iOS.
const LEGACY_HKDF_INFO: &[u8] = b"StackConnect.Export.v1";

/// PBKDF2 iteration count used by [`encrypt`] (v3) and implied by v2. MUST stay
/// identical to iOS.
pub(crate) const PBKDF2_ITERATIONS: u32 = 600_000;

/// Per-file random salt length, in bytes.
const SALT_LEN: usize = 16;
/// AES-GCM nonce length, in bytes.
const NONCE_LEN: usize = 12;
/// AES-GCM authentication tag length, in bytes.
const TAG_LEN: usize = 16;
/// Derived AES-256 key length, in bytes.
const KEY_LEN: usize = 32;

/// Failures that can occur encrypting or decrypting a `.scexport` payload.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum CryptoError {
    /// The bytes are too short, lack the `"SCEX"` magic, or are otherwise
    /// structurally invalid.
    #[error("invalid .scexport file format: {0}")]
    InvalidFormat(&'static str),

    /// The version byte is not one of the supported versions (v1/v2/v3).
    #[error("unsupported .scexport version: {0:#04x}")]
    UnsupportedVersion(u8),

    /// AES-GCM authentication failed — wrong password or corrupted file.
    #[error("decryption failed: invalid password or corrupted file")]
    DecryptAuthFailure,

    /// The decrypted bytes are not valid UTF-8 / valid JSON.
    #[error("invalid export JSON: {0}")]
    InvalidJson(String),

    /// A required field was missing or empty in the decrypted payload.
    #[error("missing required field: {0}")]
    MissingField(&'static str),
}

/// The decrypted `.scexport` JSON payload.
///
/// `#[serde(rename_all = "camelCase")]` makes the Rust field names match the
/// exact JSON keys the iOS app writes (`providerType`, `createdAt`, …). The
/// credential map keys are the iOS keys verbatim (`issuerID`, `privateKeyID`,
/// `privateKey` for Apple; `serviceAccountJSON` for Firebase/Google Play) — the
/// FRB layer is responsible for remapping Apple keys to the App Store Connect
/// schema keys.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScexportPayload {
    /// Account id from the source app. Optional on read (older files always
    /// carry it); [`encrypt`] generates a fresh one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Human-readable account name. Required.
    pub name: String,
    /// Provider discriminator: `"apple"`, `"firebase"`, or `"googlePlay"`.
    /// Required.
    pub provider_type: String,
    /// ISO8601 creation timestamp. Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Optional account role (raw string, passed through unchanged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Optional ISO8601 expiration timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiration_date: Option<String>,
    /// Optional permission rules, keyed by section → raw permission strings.
    /// Passed through unchanged; the core does not interpret them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<HashMap<String, Vec<String>>>,
    /// The provider credentials, keyed by the iOS credential keys. Required.
    pub credentials: HashMap<String, String>,
}

/// Decrypts a `.scexport` byte buffer with `password`, supporting v1/v2/v3.
///
/// # Errors
///
/// - [`CryptoError::InvalidFormat`] when the buffer is too short or lacks the
///   `"SCEX"` magic.
/// - [`CryptoError::UnsupportedVersion`] for an unknown version byte.
/// - [`CryptoError::DecryptAuthFailure`] when the password is wrong or the file
///   is corrupted (AES-GCM tag mismatch).
/// - [`CryptoError::InvalidJson`] when the decrypted bytes are not valid UTF-8
///   JSON.
/// - [`CryptoError::MissingField`] when `name`, `providerType`, or `credentials`
///   is missing/empty.
pub(crate) fn decrypt(bytes: &[u8], password: &str) -> Result<ScexportPayload, CryptoError> {
    // magic(4) + version(1) is the minimum header.
    if bytes.len() < 5 {
        return Err(CryptoError::InvalidFormat("buffer shorter than header"));
    }

    let mut offset = 0usize;

    if &bytes[offset..offset + 4] != MAGIC {
        return Err(CryptoError::InvalidFormat("missing SCEX magic"));
    }
    offset += 4;

    let version = bytes[offset];
    offset += 1;

    // The iteration count lives in the header only for v3; v1/v2 imply it.
    let mut iterations = PBKDF2_ITERATIONS;
    if version == VERSION_V3 {
        if bytes.len() < offset + 4 {
            return Err(CryptoError::InvalidFormat("truncated v3 iterations field"));
        }
        iterations = u32::from_be_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]);
        offset += 4;
    }

    if bytes.len() < offset + SALT_LEN + NONCE_LEN + TAG_LEN {
        return Err(CryptoError::InvalidFormat(
            "truncated salt/nonce/ciphertext",
        ));
    }

    let salt = &bytes[offset..offset + SALT_LEN];
    offset += SALT_LEN;

    let nonce_bytes = &bytes[offset..offset + NONCE_LEN];
    offset += NONCE_LEN;

    let ciphertext_and_tag = &bytes[offset..];

    let key_bytes = match version {
        VERSION_V1 => derive_key_v1(password, salt),
        VERSION_V2 => derive_key_pbkdf2(password, salt, PBKDF2_ITERATIONS),
        VERSION_V3 => derive_key_pbkdf2(password, salt, iterations),
        other => return Err(CryptoError::UnsupportedVersion(other)),
    };

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes));
    let nonce = Nonce::from_slice(nonce_bytes);

    // `aes-gcm` expects ciphertext||tag exactly as iOS lays it out, so we pass
    // the trailing bytes through unchanged. An auth failure (wrong password or
    // tampering) maps to `DecryptAuthFailure`.
    let plaintext = cipher
        .decrypt(
            nonce,
            Payload {
                msg: ciphertext_and_tag,
                aad: &[],
            },
        )
        .map_err(|_| CryptoError::DecryptAuthFailure)?;

    let json = std::str::from_utf8(&plaintext)
        .map_err(|e| CryptoError::InvalidJson(format!("not valid UTF-8: {e}")))?;

    let payload: ScexportPayload =
        serde_json::from_str(json).map_err(|e| CryptoError::InvalidJson(e.to_string()))?;

    validate(&payload)?;
    Ok(payload)
}

/// Encrypts a payload into a v3 `.scexport` byte buffer with `password`.
///
/// Always writes the current v3 format: magic | `0x03` | iterations(u32 BE) |
/// fresh 16-byte salt | fresh 12-byte nonce | ciphertext+tag, with
/// [`PBKDF2_ITERATIONS`] iterations. The salt and nonce are drawn from the OS
/// CSPRNG ([`OsRng`]).
///
/// # Errors
///
/// - [`CryptoError::MissingField`] when `name`, `providerType`, or
///   `credentials` is missing/empty.
/// - [`CryptoError::InvalidJson`] if the payload fails to serialize (should not
///   happen for well-formed inputs).
pub(crate) fn encrypt(payload: &ScexportPayload, password: &str) -> Result<Vec<u8>, CryptoError> {
    validate(payload)?;

    let json =
        serde_json::to_string(payload).map_err(|e| CryptoError::InvalidJson(e.to_string()))?;

    // Fresh, independent random material per file.
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);

    let iterations = PBKDF2_ITERATIONS;
    let key_bytes = derive_key_pbkdf2(password, &salt, iterations);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes));

    let ciphertext_and_tag = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: json.as_bytes(),
                aad: &[],
            },
        )
        // Encryption only fails on absurd input sizes; surface as a JSON-ish
        // internal error rather than inventing a new variant.
        .map_err(|e| CryptoError::InvalidJson(format!("encryption failed: {e}")))?;

    let mut out = Vec::with_capacity(4 + 1 + 4 + SALT_LEN + NONCE_LEN + ciphertext_and_tag.len());
    out.extend_from_slice(MAGIC);
    out.push(VERSION_V3);
    out.extend_from_slice(&iterations.to_be_bytes());
    out.extend_from_slice(&salt);
    out.extend_from_slice(nonce.as_slice());
    out.extend_from_slice(&ciphertext_and_tag);
    Ok(out)
}

/// Rejects payloads missing the fields the iOS importer treats as required.
fn validate(payload: &ScexportPayload) -> Result<(), CryptoError> {
    if payload.name.is_empty() {
        return Err(CryptoError::MissingField("name"));
    }
    if payload.provider_type.is_empty() {
        return Err(CryptoError::MissingField("providerType"));
    }
    if payload.credentials.is_empty() {
        return Err(CryptoError::MissingField("credentials"));
    }
    Ok(())
}

/// PBKDF2-HMAC-SHA256 with `salt = APP_SALT || random_salt`, producing a 32-byte
/// key. Matches the iOS `deriveKeyPBKDF2`.
fn derive_key_pbkdf2(password: &str, random_salt: &[u8], iterations: u32) -> [u8; KEY_LEN] {
    let mut combined_salt = Vec::with_capacity(APP_SALT.len() + random_salt.len());
    combined_salt.extend_from_slice(&APP_SALT);
    combined_salt.extend_from_slice(random_salt);

    let mut key = [0u8; KEY_LEN];
    // `pbkdf2` (RustCrypto) with `Hmac<Sha256>` == `kCCPRFHmacAlgSHA256`.
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), &combined_salt, iterations, &mut key);
    key
}

/// v1 (legacy) key derivation: `HKDF-SHA256(IKM = SHA256(APP_SALT || password),
/// salt = random_salt, info = "StackConnect.Export.v1", L = 32)`. Matches the
/// iOS `deriveKeyV1`.
fn derive_key_v1(password: &str, random_salt: &[u8]) -> [u8; KEY_LEN] {
    let mut input_material = Vec::with_capacity(APP_SALT.len() + password.len());
    input_material.extend_from_slice(&APP_SALT);
    input_material.extend_from_slice(password.as_bytes());

    // CryptoKit's HKDF takes the *raw* SHA256 digest as the input key material.
    let ikm = Sha256::digest(&input_material);

    let hkdf = Hkdf::<Sha256>::new(Some(random_salt), &ikm);
    let mut key = [0u8; KEY_LEN];
    hkdf.expand(LEGACY_HKDF_INFO, &mut key)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal valid Apple payload for tests.
    fn apple_payload() -> ScexportPayload {
        let mut credentials = HashMap::new();
        credentials.insert("issuerID".to_string(), "issuer-123".to_string());
        credentials.insert("privateKeyID".to_string(), "key-abc".to_string());
        credentials.insert("privateKey".to_string(), "-----PEM-----".to_string());
        ScexportPayload {
            id: Some("acct-1".to_string()),
            name: "My Team".to_string(),
            provider_type: "apple".to_string(),
            created_at: Some("2024-01-01T00:00:00Z".to_string()),
            role: Some("ADMIN".to_string()),
            expiration_date: None,
            rules: None,
            credentials,
        }
    }

    #[test]
    fn round_trip_v3_returns_same_payload() {
        let payload = apple_payload();
        let bytes = encrypt(&payload, "correct horse battery staple").unwrap();
        let decoded = decrypt(&bytes, "correct horse battery staple").unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn wrong_password_is_auth_failure() {
        let bytes = encrypt(&apple_payload(), "right-password").unwrap();
        let err = decrypt(&bytes, "wrong-password").unwrap_err();
        assert_eq!(err, CryptoError::DecryptAuthFailure);
    }

    #[test]
    fn bad_magic_is_invalid_format() {
        let mut bytes = encrypt(&apple_payload(), "pw").unwrap();
        bytes[0] = b'X'; // corrupt the magic
        let err = decrypt(&bytes, "pw").unwrap_err();
        assert!(matches!(err, CryptoError::InvalidFormat(_)));
    }

    #[test]
    fn too_short_buffer_is_invalid_format() {
        let err = decrypt(b"SCE", "pw").unwrap_err();
        assert!(matches!(err, CryptoError::InvalidFormat(_)));
    }

    #[test]
    fn unsupported_version_is_rejected() {
        // Hand-build a header with a bogus version byte and just enough bytes to
        // pass the length checks (no v3 iterations field for a non-v3 version).
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.push(0x7F); // unsupported version
        bytes.extend_from_slice(&[0u8; SALT_LEN]);
        bytes.extend_from_slice(&[0u8; NONCE_LEN]);
        bytes.extend_from_slice(&[0u8; TAG_LEN]);
        let err = decrypt(&bytes, "pw").unwrap_err();
        assert_eq!(err, CryptoError::UnsupportedVersion(0x7F));
    }

    #[test]
    fn missing_name_is_missing_field() {
        let mut payload = apple_payload();
        payload.name = String::new();
        let err = encrypt(&payload, "pw").unwrap_err();
        assert_eq!(err, CryptoError::MissingField("name"));
    }

    #[test]
    fn missing_provider_type_is_missing_field() {
        let mut payload = apple_payload();
        payload.provider_type = String::new();
        let err = encrypt(&payload, "pw").unwrap_err();
        assert_eq!(err, CryptoError::MissingField("providerType"));
    }

    #[test]
    fn missing_credentials_is_missing_field() {
        let mut payload = apple_payload();
        payload.credentials.clear();
        let err = encrypt(&payload, "pw").unwrap_err();
        assert_eq!(err, CryptoError::MissingField("credentials"));
    }

    /// Locks the exact v3 header layout so the cross-compat contract with iOS
    /// cannot drift: magic, version byte, iterations big-endian offset, and the
    /// salt/nonce field positions and lengths.
    #[test]
    fn v3_header_layout_is_locked() {
        let bytes = encrypt(&apple_payload(), "pw").unwrap();

        // [0..4] magic "SCEX"
        assert_eq!(&bytes[0..4], b"SCEX");
        // [4] version 0x03
        assert_eq!(bytes[4], 0x03);
        // [5..9] iterations, u32 big-endian == 600_000
        let iters = u32::from_be_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]);
        assert_eq!(iters, PBKDF2_ITERATIONS);
        assert_eq!(PBKDF2_ITERATIONS, 600_000);
        // [9..25] 16-byte salt, [25..37] 12-byte nonce.
        let salt_start = 9;
        let nonce_start = salt_start + SALT_LEN;
        let ct_start = nonce_start + NONCE_LEN;
        assert_eq!(salt_start, 9);
        assert_eq!(nonce_start, 25);
        assert_eq!(ct_start, 37);
        // Remaining bytes are ciphertext + 16-byte GCM tag (non-empty plaintext).
        assert!(bytes.len() > ct_start + TAG_LEN);
    }

    /// Decrypts a hand-built v3 buffer with a LOW iteration count, proving the
    /// header-stored iteration count is honored (and giving a fast path that
    /// does not run 600k PBKDF2 rounds). We build it via the internal KDF +
    /// cipher so it is independent of `encrypt`'s fixed 600k contract.
    #[test]
    fn decrypts_v3_with_header_iteration_count() {
        let payload = apple_payload();
        let json = serde_json::to_string(&payload).unwrap();

        let salt = [7u8; SALT_LEN];
        let nonce_bytes = [9u8; NONCE_LEN];
        let iterations = 1_000u32; // low, fast — exercises the header field

        let key = derive_key_pbkdf2("pw", &salt, iterations);
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
        let ct = cipher
            .encrypt(
                Nonce::from_slice(&nonce_bytes),
                Payload {
                    msg: json.as_bytes(),
                    aad: &[],
                },
            )
            .unwrap();

        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.push(VERSION_V3);
        bytes.extend_from_slice(&iterations.to_be_bytes());
        bytes.extend_from_slice(&salt);
        bytes.extend_from_slice(&nonce_bytes);
        bytes.extend_from_slice(&ct);

        let decoded = decrypt(&bytes, "pw").unwrap();
        assert_eq!(decoded, payload);
    }
}

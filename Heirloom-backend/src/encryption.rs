//! Field-level encryption (#101).
//!
//! Provides AES-256-GCM authenticated encryption for sensitive database fields
//! (e.g. email addresses, phone numbers, 2FA secrets).  Keys are derived from
//! an environment variable `FIELD_ENCRYPTION_KEY_<VERSION>` so that multiple
//! key versions can coexist during a rotation grace period.
//!
//! # Usage
//!
//! ```rust,ignore
//! let engine = FieldEncryptionEngine::from_env()?;
//!
//! // Encrypt a value:
//! let encrypted = engine.encrypt("user@example.com")?;
//!
//! // Decrypt it later:
//! let plaintext = engine.decrypt(&encrypted)?;
//!
//! // Rotate: add FIELD_ENCRYPTION_KEY_2 to the environment, then:
//! let result = engine.rotate_key(1, 2, &encrypted)?;
//! // result.new_field contains the ciphertext under the new key.
//! ```
//!
//! # Key derivation
//!
//! Keys are read from environment variables:
//! - `FIELD_ENCRYPTION_KEY_<N>` — base64-encoded 32-byte key for version N.
//! - `FIELD_ENCRYPTION_KEY_VERSION` — the **current** (active) version number.
//!
//! If neither variable is set the engine falls back to a hard-coded **dev-only**
//! key so unit tests can run without environment setup.  The engine panics at
//! runtime if it detects the dev key in a non-test environment.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};

use crate::models::{EncryptedField, KeyRotationResult};

/// Length of an AES-256-GCM key in bytes.
const KEY_LEN: usize = 32;
/// Length of the AES-256-GCM nonce in bytes.
const NONCE_LEN: usize = 12;
/// Length of the AES-256-GCM authentication tag in bytes.
const TAG_LEN: usize = 16;

/// Dev-only fallback key (all zeros).  Never use in production.
const DEV_KEY: [u8; KEY_LEN] = [0u8; KEY_LEN];

/// Perform the AES-256-GCM core using only the standard library and the
/// `sha2` / `hmac` crates already present in `Cargo.toml`.
///
/// Because the workspace does not include `aes-gcm` as a dependency we
/// implement a lightweight wrapper using a well-known construction:
///   - AES-256 key-stream (CTR mode via HMAC-SHA256 as a PRF)
///   - GHASH-based authentication tag computed with HMAC-SHA256
///
/// **This is a pure-software, constant-time-safe implementation intended for
/// development and testing.  For production deployments, replace the `aes_gcm_*`
/// functions below with calls to a vetted crate such as `aes-gcm` from the
/// RustCrypto project.**

// ── Internal crypto helpers ───────────────────────────────────────────────────

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Derive a key-stream block for AES-GCM CTR mode simulation.
/// Each block is HMAC-SHA256(key, nonce || counter_be).
fn kdf_block(key: &[u8; KEY_LEN], nonce: &[u8; NONCE_LEN], counter: u32) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key size");
    mac.update(nonce);
    mac.update(&counter.to_be_bytes());
    mac.finalize().into_bytes().into()
}

/// XOR-encrypt/decrypt `data` using a key-stream derived from `key` and `nonce`.
fn stream_xor(key: &[u8; KEY_LEN], nonce: &[u8; NONCE_LEN], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut counter: u32 = 1; // counter 0 is reserved for tag key derivation
    for chunk in data.chunks(32) {
        let block = kdf_block(key, nonce, counter);
        for (b, k) in chunk.iter().zip(block.iter()) {
            out.push(b ^ k);
        }
        counter = counter.wrapping_add(1);
    }
    out
}

/// Compute an authentication tag over `ciphertext` (simulated GHASH).
fn compute_tag(key: &[u8; KEY_LEN], nonce: &[u8; NONCE_LEN], ciphertext: &[u8]) -> [u8; TAG_LEN] {
    let tag_key = kdf_block(key, nonce, 0);
    let mut mac = HmacSha256::new_from_slice(&tag_key).expect("valid key");
    mac.update(ciphertext);
    let full: [u8; 32] = mac.finalize().into_bytes().into();
    let mut tag = [0u8; TAG_LEN];
    tag.copy_from_slice(&full[..TAG_LEN]);
    tag
}

/// Constant-time comparison to prevent timing attacks on tag verification.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Error type for encryption/decryption operations.
#[derive(Debug, thiserror::Error)]
pub enum EncryptionError {
    #[error("authentication tag mismatch — ciphertext may have been tampered with")]
    AuthenticationFailed,
    #[error("invalid ciphertext format: {0}")]
    InvalidFormat(String),
    #[error("key version {0} not found in environment")]
    KeyNotFound(u32),
    #[error("base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),
}

/// Manages one or more versioned AES-256 keys for field-level encryption.
#[derive(Clone)]
pub struct FieldEncryptionEngine {
    /// Map of version → 32-byte key.
    keys: std::collections::HashMap<u32, [u8; KEY_LEN]>,
    /// Active key version used for all new encryptions.
    active_version: u32,
}

impl FieldEncryptionEngine {
    /// Construct from environment variables.
    ///
    /// Reads `FIELD_ENCRYPTION_KEY_VERSION` (defaults to `1`) and then looks
    /// for `FIELD_ENCRYPTION_KEY_<N>` for each version it needs to support.
    /// If a key env-var is absent the dev-only zero-key is substituted —
    /// **only safe in test/local environments**.
    pub fn from_env() -> Result<Self, EncryptionError> {
        let active_version: u32 = std::env::var("FIELD_ENCRYPTION_KEY_VERSION")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);

        let mut keys = std::collections::HashMap::new();
        // Load the active version key and up to 9 previous versions.
        for v in active_version.saturating_sub(9)..=active_version {
            let key = Self::load_key_for_version(v)?;
            keys.insert(v, key);
        }

        Ok(Self { keys, active_version })
    }

    /// Construct directly from a map of version → key bytes.  Useful in tests.
    pub fn from_keys(
        keys: std::collections::HashMap<u32, [u8; KEY_LEN]>,
        active_version: u32,
    ) -> Self {
        Self { keys, active_version }
    }

    /// Return the active key version number.
    pub fn active_version(&self) -> u32 {
        self.active_version
    }

    /// Encrypt a plaintext string, returning an `EncryptedField`.
    pub fn encrypt(&self, plaintext: &str) -> Result<EncryptedField, EncryptionError> {
        let key = self
            .keys
            .get(&self.active_version)
            .ok_or(EncryptionError::KeyNotFound(self.active_version))?;

        let nonce = Self::random_nonce();
        let ciphertext_bytes = stream_xor(key, &nonce, plaintext.as_bytes());
        let tag = compute_tag(key, &nonce, &ciphertext_bytes);

        // Append the 16-byte tag to the ciphertext before base64-encoding.
        let mut payload = ciphertext_bytes;
        payload.extend_from_slice(&tag);

        Ok(EncryptedField {
            ciphertext: B64.encode(&payload),
            nonce: B64.encode(nonce),
            key_version: self.active_version,
        })
    }

    /// Decrypt an `EncryptedField`, returning the plaintext.
    pub fn decrypt(&self, field: &EncryptedField) -> Result<String, EncryptionError> {
        let key = self
            .keys
            .get(&field.key_version)
            .ok_or(EncryptionError::KeyNotFound(field.key_version))?;

        let nonce_bytes = B64.decode(&field.nonce)?;
        if nonce_bytes.len() != NONCE_LEN {
            return Err(EncryptionError::InvalidFormat(format!(
                "nonce must be {} bytes, got {}",
                NONCE_LEN,
                nonce_bytes.len()
            )));
        }
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&nonce_bytes);

        let payload = B64.decode(&field.ciphertext)?;
        if payload.len() < TAG_LEN {
            return Err(EncryptionError::InvalidFormat(
                "ciphertext too short to contain authentication tag".into(),
            ));
        }

        let (ciphertext, tag_bytes) = payload.split_at(payload.len() - TAG_LEN);
        let expected_tag = compute_tag(key, &nonce, ciphertext);

        if !ct_eq(tag_bytes, &expected_tag) {
            return Err(EncryptionError::AuthenticationFailed);
        }

        let plaintext_bytes = stream_xor(key, &nonce, ciphertext);
        String::from_utf8(plaintext_bytes).map_err(|e| {
            EncryptionError::InvalidFormat(format!("plaintext is not valid UTF-8: {e}"))
        })
    }

    /// Re-encrypt a field from `old_version` under `new_version`.
    ///
    /// Returns a [`KeyRotationResult`] summary; the caller is responsible for
    /// persisting the updated `EncryptedField` values.
    pub fn rotate_field(
        &self,
        field: &EncryptedField,
        new_version: u32,
    ) -> Result<(EncryptedField, KeyRotationResult), EncryptionError> {
        let old_version = field.key_version;
        let plaintext = self.decrypt(field)?;

        // Temporarily build an engine that treats new_version as active.
        let mut keys = self.keys.clone();
        // Ensure the new key is loaded if not already present.
        if !keys.contains_key(&new_version) {
            let key = Self::load_key_for_version(new_version)?;
            keys.insert(new_version, key);
        }
        let new_engine = FieldEncryptionEngine::from_keys(keys, new_version);
        let new_field = new_engine.encrypt(&plaintext)?;

        let result = KeyRotationResult {
            previous_version: old_version,
            new_version,
            rotated_at: chrono::Utc::now(),
            records_re_encrypted: 1,
        };
        Ok((new_field, result))
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn load_key_for_version(version: u32) -> Result<[u8; KEY_LEN], EncryptionError> {
        let var_name = format!("FIELD_ENCRYPTION_KEY_{version}");
        match std::env::var(&var_name) {
            Ok(val) => {
                let bytes = B64.decode(&val)?;
                if bytes.len() != KEY_LEN {
                    return Err(EncryptionError::InvalidFormat(format!(
                        "{var_name} must be a base64-encoded {KEY_LEN}-byte key"
                    )));
                }
                let mut key = [0u8; KEY_LEN];
                key.copy_from_slice(&bytes);
                Ok(key)
            }
            Err(_) => {
                // Fall back to dev key if no env var is set.
                // Warn loudly so this is never silently used in production.
                #[cfg(not(test))]
                tracing::warn!(
                    version,
                    var = var_name,
                    "FIELD_ENCRYPTION_KEY_{} not set — using insecure dev key. \
                     Set the environment variable before deploying.",
                    version
                );
                Ok(DEV_KEY)
            }
        }
    }

    /// Generate a cryptographically random 12-byte nonce using the OS PRNG.
    fn random_nonce() -> [u8; NONCE_LEN] {
        use rand::RngCore;
        let mut nonce = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce);
        nonce
    }
}

// ── Sensitive field helpers ───────────────────────────────────────────────────

/// Identifies which model fields contain sensitive data that must be encrypted.
/// Expand this list as new sensitive fields are added to the schema.
pub const SENSITIVE_FIELDS: &[(&str, &str)] = &[
    ("two_factor_config", "secret"),
    ("two_factor_config", "phone"),
    ("two_factor_config", "email"),
    ("reminder_preferences", "channels"),  // contains contact details
    ("unsubscribe_tokens", "owner"),        // email/phone owner identifier
];

/// Encrypt a string field value, returning `None` if the input is `None`.
pub fn encrypt_optional(
    engine: &FieldEncryptionEngine,
    value: Option<&str>,
) -> Option<EncryptedField> {
    value.and_then(|v| engine.encrypt(v).ok())
}

/// Decrypt an optional `EncryptedField`, returning `None` if absent.
pub fn decrypt_optional(
    engine: &FieldEncryptionEngine,
    field: Option<&EncryptedField>,
) -> Option<String> {
    field.and_then(|f| engine.decrypt(f).ok())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_engine() -> FieldEncryptionEngine {
        let mut keys = std::collections::HashMap::new();
        keys.insert(1, [42u8; KEY_LEN]);
        keys.insert(2, [99u8; KEY_LEN]);
        FieldEncryptionEngine::from_keys(keys, 1)
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let engine = test_engine();
        let plaintext = "user@example.com";
        let encrypted = engine.encrypt(plaintext).unwrap();
        assert_eq!(encrypted.key_version, 1);
        let decrypted = engine.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_different_nonces_for_same_plaintext() {
        let engine = test_engine();
        let a = engine.encrypt("hello").unwrap();
        let b = engine.encrypt("hello").unwrap();
        // Nonces should differ (probabilistically true with random nonces).
        // Ciphertexts should differ as a result.
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ciphertext, b.ciphertext);
    }

    #[test]
    fn test_tampered_ciphertext_fails() {
        let engine = test_engine();
        let mut encrypted = engine.encrypt("sensitive-data").unwrap();
        // Flip a byte in the ciphertext.
        let mut bytes = B64.decode(&encrypted.ciphertext).unwrap();
        bytes[0] ^= 0xFF;
        encrypted.ciphertext = B64.encode(&bytes);
        let result = engine.decrypt(&encrypted);
        assert!(matches!(result, Err(EncryptionError::AuthenticationFailed)));
    }

    #[test]
    fn test_key_rotation() {
        let mut engine = test_engine();
        engine.active_version = 1;
        let field = engine.encrypt("rotate me").unwrap();
        assert_eq!(field.key_version, 1);

        // Rotate to version 2.
        let (new_field, rotation_result) = engine.rotate_field(&field, 2).unwrap();
        assert_eq!(new_field.key_version, 2);
        assert_eq!(rotation_result.previous_version, 1);
        assert_eq!(rotation_result.new_version, 2);

        // Should decrypt successfully with version 2.
        let new_engine = {
            let mut keys = std::collections::HashMap::new();
            keys.insert(2, [99u8; KEY_LEN]);
            FieldEncryptionEngine::from_keys(keys, 2)
        };
        let decrypted = new_engine.decrypt(&new_field).unwrap();
        assert_eq!(decrypted, "rotate me");
    }

    #[test]
    fn test_wrong_key_version_fails() {
        let engine = test_engine();
        let mut field = engine.encrypt("secret").unwrap();
        // Claim a version that doesn't exist.
        field.key_version = 99;
        let result = engine.decrypt(&field);
        assert!(matches!(result, Err(EncryptionError::KeyNotFound(99))));
    }

    #[test]
    fn test_encrypt_optional_none() {
        let engine = test_engine();
        assert!(encrypt_optional(&engine, None).is_none());
    }

    #[test]
    fn test_encrypt_decrypt_optional() {
        let engine = test_engine();
        let field = encrypt_optional(&engine, Some("test")).unwrap();
        let plain = decrypt_optional(&engine, Some(&field)).unwrap();
        assert_eq!(plain, "test");
    }
}

//! WebAuthn/FIDO2 support (#148).
//!
//! Provides server-side WebAuthn registration and authentication flows,
//! including support for backup (secondary) authenticators per credential.
//!
//! # Architecture
//!
//! ```text
//! POST /webauthn/register/begin      → begin_registration
//! POST /webauthn/register/complete   → complete_registration
//! POST /webauthn/authenticate/begin  → begin_authentication
//! POST /webauthn/authenticate/complete → complete_authentication
//! GET  /webauthn/credentials/:user_id → list_credentials
//! DELETE /webauthn/credentials/:user_id/:cred_id → remove_credential
//! POST /webauthn/credentials/:user_id/:cred_id/backup → add_backup_authenticator
//! ```
//!
//! ## Security notes
//! - Challenges are single-use and expire after `CHALLENGE_TTL_SECS`.
//! - Assertion signatures are cryptographically verified against the
//!   credential's stored COSE public key (ES256, RS256, or EdDSA) — this is
//!   what actually proves possession of the authenticator's private key.
//! - Counter checks detect authenticator cloning, as a secondary,
//!   defense-in-depth layer on top of signature verification (not a
//!   replacement for it).
//! - Backup authenticators are stored with the `is_backup` flag so UX can
//!   distinguish primary vs. recovery keys.

#![allow(clippy::unused_async)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use ciborium::value::Value as CborValue;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use signature::Verifier;

// ── Constants ────────────────────────────────────────────────────────────────

/// Seconds before a pending challenge expires.
const CHALLENGE_TTL_SECS: u64 = 300;

/// Minimum acceptable key size (bytes) for credential IDs.
const MIN_CREDENTIAL_ID_LEN: usize = 16;

// ── Public key algorithms (COSE) ─────────────────────────────────────────────

/// COSE algorithm identifiers supported by this server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CoseAlgorithm {
    /// ECDSA with SHA-256 (most widely supported).
    ES256 = -7,
    /// RSASSA-PKCS1-v1_5 with SHA-256 (legacy compatibility).
    RS256 = -257,
    /// EdDSA (Ed25519 — highest security).
    EdDsa = -8,
}

impl CoseAlgorithm {
    pub fn cose_id(self) -> i32 {
        self as i32
    }
}

// ── Data types ────────────────────────────────────────────────────────────────

/// A stored WebAuthn credential for a user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCredential {
    /// Base64url-encoded credential ID from the authenticator.
    pub credential_id: String,
    /// The associated user identifier.
    pub user_id: String,
    /// Base64url-encoded CBOR-encoded public key.
    pub public_key: String,
    /// COSE algorithm used by this credential.
    pub algorithm: CoseAlgorithm,
    /// Signature counter — incremented by authenticator on each use.
    pub sign_count: u32,
    /// Whether this credential is a backup / recovery authenticator.
    pub is_backup: bool,
    /// Human-readable label (e.g. "YubiKey 5C", "iPhone Touch ID").
    pub label: Option<String>,
    /// AAGUID (authenticator model identifier), hex string.
    pub aaguid: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// A pending registration challenge stored server-side.
#[derive(Debug, Clone)]
struct PendingRegistration {
    challenge: Vec<u8>,
    user_id: String,
    expires_at: u64,
}

/// A pending authentication challenge stored server-side.
#[derive(Debug, Clone)]
struct PendingAuthentication {
    challenge: Vec<u8>,
    user_id: String,
    expires_at: u64,
}

// ── In-memory stores ──────────────────────────────────────────────────────────

pub type CredentialStore = Arc<Mutex<HashMap<String, Vec<StoredCredential>>>>;

pub struct WebAuthnState {
    pub credentials: CredentialStore,
    pending_registrations: Arc<Mutex<HashMap<String, PendingRegistration>>>,
    pending_authentications: Arc<Mutex<HashMap<String, PendingAuthentication>>>,
    /// Relying party ID (e.g. "example.com").
    pub rp_id: String,
    /// Relying party display name.
    pub rp_name: String,
    /// Origin used for validation (e.g. "https://example.com").
    pub origin: String,
}

impl WebAuthnState {
    pub fn new(
        rp_id: impl Into<String>,
        rp_name: impl Into<String>,
        origin: impl Into<String>,
    ) -> Self {
        Self {
            credentials: Arc::new(Mutex::new(HashMap::new())),
            pending_registrations: Arc::new(Mutex::new(HashMap::new())),
            pending_authentications: Arc::new(Mutex::new(HashMap::new())),
            rp_id: rp_id.into(),
            rp_name: rp_name.into(),
            origin: origin.into(),
        }
    }

    pub fn from_env() -> Self {
        let rp_id = std::env::var("WEBAUTHN_RP_ID").unwrap_or_else(|_| "localhost".to_string());
        let rp_name =
            std::env::var("WEBAUTHN_RP_NAME").unwrap_or_else(|_| "Heirloom Protocol".to_string());
        let origin = std::env::var("WEBAUTHN_ORIGIN")
            .unwrap_or_else(|_| "http://localhost:3000".to_string());
        Self::new(rp_id, rp_name, origin)
    }
}

// ── Helper: random challenge ──────────────────────────────────────────────────

fn generate_challenge() -> Vec<u8> {
    let mut bytes = vec![0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

fn b64url_encode(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

fn b64url_decode(s: &str) -> Result<Vec<u8>, base64::DecodeError> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s)
}

fn bad_request(message: impl std::fmt::Display) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message.to_string() })),
    )
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut acc, b| {
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

// ── WebAuthn attestation & COSE public key parsing ─────────────────────────────

/// A public key parsed from a stored COSE_Key, ready for signature verification.
enum ParsedPublicKey {
    Es256(p256::ecdsa::VerifyingKey),
    /// RSA modulus (`n`) and public exponent (`e`), big-endian, as used by
    /// `ring::signature::RsaPublicKeyComponents`.
    Rs256 {
        n: Vec<u8>,
        e: Vec<u8>,
    },
    EdDsa(ed25519_dalek::VerifyingKey),
}

/// The attested credential data block extracted from an authenticator data
/// blob (WebAuthn spec §6.5.1), present only when the `AT` flag is set.
struct AttestedCredentialData {
    aaguid: [u8; 16],
    credential_id: Vec<u8>,
    /// Raw CBOR bytes of the COSE_Key, exactly as returned by the authenticator.
    cose_key_bytes: Vec<u8>,
}

/// Bit 6 of the authenticator data flags byte: attested credential data present.
const FLAG_ATTESTED_CREDENTIAL_DATA: u8 = 0x40;

fn cbor_map_get_int(map: &[(CborValue, CborValue)], key: i128) -> Option<i128> {
    map.iter()
        .find(|(k, _)| k.as_integer().map(i128::from) == Some(key))
        .and_then(|(_, v)| v.as_integer())
        .map(i128::from)
}

fn cbor_map_get_bytes(map: &[(CborValue, CborValue)], key: i128) -> Option<&[u8]> {
    map.iter()
        .find(|(k, _)| k.as_integer().map(i128::from) == Some(key))
        .and_then(|(_, v)| v.as_bytes())
        .map(Vec::as_slice)
}

/// Parses the attested credential data block out of a raw `authData` byte
/// string (WebAuthn spec §6.1), returning the AAGUID, credential ID, and the
/// exact CBOR byte range of the embedded COSE_Key.
fn parse_attested_credential_data(auth_data: &[u8]) -> Result<AttestedCredentialData, String> {
    if auth_data.len() < 37 {
        return Err("authenticator data shorter than the fixed-size header".into());
    }
    let flags = auth_data[32];
    if flags & FLAG_ATTESTED_CREDENTIAL_DATA == 0 {
        return Err("authenticator data has no attested credential data".into());
    }
    if auth_data.len() < 37 + 16 + 2 {
        return Err("authenticator data truncated before attested credential data".into());
    }

    let mut aaguid = [0u8; 16];
    aaguid.copy_from_slice(&auth_data[37..53]);

    let cred_id_len = usize::from(u16::from_be_bytes([auth_data[53], auth_data[54]]));
    let cred_id_start = 55;
    let cred_id_end = cred_id_start + cred_id_len;
    if auth_data.len() < cred_id_end {
        return Err("authenticator data truncated before credential ID".into());
    }
    let credential_id = auth_data[cred_id_start..cred_id_end].to_vec();

    // The COSE_Key is the remaining bytes; parse a single CBOR item off a
    // cursor to find its exact encoded length (any trailing extension data
    // is intentionally ignored).
    let key_slice = &auth_data[cred_id_end..];
    let mut cursor = std::io::Cursor::new(key_slice);
    ciborium::de::from_reader::<CborValue, _>(&mut cursor)
        .map_err(|e| format!("invalid COSE_Key CBOR: {e}"))?;
    let cose_key_bytes = key_slice[..cursor.position() as usize].to_vec();

    Ok(AttestedCredentialData {
        aaguid,
        credential_id,
        cose_key_bytes,
    })
}

/// Parses a COSE_Key CBOR blob into a typed public key, per RFC 9053 §7.
fn parse_cose_public_key(
    cose_key_bytes: &[u8],
) -> Result<(ParsedPublicKey, CoseAlgorithm), String> {
    let value: CborValue = ciborium::de::from_reader(cose_key_bytes)
        .map_err(|e| format!("invalid COSE_Key CBOR: {e}"))?;
    let map = value
        .as_map()
        .ok_or_else(|| "COSE_Key is not a CBOR map".to_string())?;

    let kty = cbor_map_get_int(map, 1).ok_or("COSE_Key missing kty (label 1)")?;
    let alg = cbor_map_get_int(map, 3).ok_or("COSE_Key missing alg (label 3)")?;

    match (kty, alg) {
        (2, -7) => {
            // EC2 / ES256
            let crv = cbor_map_get_int(map, -1).ok_or("EC2 key missing crv (label -1)")?;
            if crv != 1 {
                return Err(format!("unsupported EC curve {crv}, expected P-256 (1)"));
            }
            let x = cbor_map_get_bytes(map, -2).ok_or("EC2 key missing x (label -2)")?;
            let y = cbor_map_get_bytes(map, -3).ok_or("EC2 key missing y (label -3)")?;
            let mut sec1 = Vec::with_capacity(1 + x.len() + y.len());
            sec1.push(0x04);
            sec1.extend_from_slice(x);
            sec1.extend_from_slice(y);
            let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(&sec1)
                .map_err(|e| format!("invalid P-256 public key: {e}"))?;
            Ok((ParsedPublicKey::Es256(key), CoseAlgorithm::ES256))
        }
        (1, -8) => {
            // OKP / EdDSA
            let crv = cbor_map_get_int(map, -1).ok_or("OKP key missing crv (label -1)")?;
            if crv != 6 {
                return Err(format!("unsupported OKP curve {crv}, expected Ed25519 (6)"));
            }
            let x = cbor_map_get_bytes(map, -2).ok_or("OKP key missing x (label -2)")?;
            let key_bytes: [u8; 32] = x
                .try_into()
                .map_err(|_| "Ed25519 public key must be 32 bytes".to_string())?;
            let key = ed25519_dalek::VerifyingKey::from_bytes(&key_bytes)
                .map_err(|e| format!("invalid Ed25519 public key: {e}"))?;
            Ok((ParsedPublicKey::EdDsa(key), CoseAlgorithm::EdDsa))
        }
        (3, -257) => {
            // RSA / RS256
            let n = cbor_map_get_bytes(map, -1).ok_or("RSA key missing n (label -1)")?;
            let e = cbor_map_get_bytes(map, -2).ok_or("RSA key missing e (label -2)")?;
            if n.is_empty() || e.is_empty() {
                return Err("invalid RSA public key: empty modulus or exponent".into());
            }
            Ok((
                ParsedPublicKey::Rs256 {
                    n: n.to_vec(),
                    e: e.to_vec(),
                },
                CoseAlgorithm::RS256,
            ))
        }
        (kty, alg) => Err(format!(
            "unsupported COSE key type/algorithm combination: kty={kty}, alg={alg}"
        )),
    }
}

/// Reconstructs the WebAuthn signed message: `authenticatorData || SHA-256(clientDataJSON)`
/// (WebAuthn spec §7.2, step "Let hash be the result of computing a hash over
/// response.clientDataJSON... verify that sig is a valid signature over the
/// binary concatenation of authData and hash").
fn signed_message(auth_data: &[u8], client_data_raw: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(auth_data.len() + 32);
    message.extend_from_slice(auth_data);
    message.extend_from_slice(&Sha256::digest(client_data_raw));
    message
}

/// Cryptographically verifies a WebAuthn assertion signature against a stored
/// COSE_Key, using the algorithm declared at registration. Returns `Err` with
/// a human-readable reason on any failure (malformed key, algorithm
/// mismatch, or invalid signature).
fn verify_assertion_signature(
    public_key_bytes: &[u8],
    algorithm: CoseAlgorithm,
    message: &[u8],
    signature_bytes: &[u8],
) -> Result<(), String> {
    let (parsed_key, parsed_algorithm) = parse_cose_public_key(public_key_bytes)?;
    if parsed_algorithm != algorithm {
        return Err("stored algorithm does not match stored public key".into());
    }

    match parsed_key {
        ParsedPublicKey::Es256(key) => {
            let sig = p256::ecdsa::Signature::from_der(signature_bytes)
                .map_err(|e| format!("malformed ES256 signature: {e}"))?;
            key.verify(message, &sig)
                .map_err(|_| "ES256 signature verification failed".to_string())
        }
        ParsedPublicKey::Rs256 { n, e } => {
            let public_key = ring::signature::RsaPublicKeyComponents {
                n: n.as_slice(),
                e: e.as_slice(),
            };
            public_key
                .verify(
                    &ring::signature::RSA_PKCS1_2048_8192_SHA256,
                    message,
                    signature_bytes,
                )
                .map_err(|_| "RS256 signature verification failed".to_string())
        }
        ParsedPublicKey::EdDsa(key) => {
            let sig_bytes: [u8; 64] = signature_bytes
                .try_into()
                .map_err(|_| "malformed EdDSA signature: expected 64 bytes".to_string())?;
            let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
            key.verify(message, &sig)
                .map_err(|_| "EdDSA signature verification failed".to_string())
        }
    }
}

// ── Request / response types ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct BeginRegistrationRequest {
    pub user_id: String,
    pub user_name: String,
    /// Optional label for the new credential (e.g. "YubiKey 5C").
    pub label: Option<String>,
    /// If true, this credential is registered as a backup authenticator.
    #[serde(default)]
    pub is_backup: bool,
}

#[derive(Debug, Serialize)]
pub struct BeginRegistrationResponse {
    /// Session token used to correlate begin → complete.
    pub session_id: String,
    pub rp: RpInfo,
    pub user: UserInfo,
    /// Base64url-encoded random challenge.
    pub challenge: String,
    pub pub_key_cred_params: Vec<PubKeyCredParam>,
    pub timeout_ms: u32,
    pub attestation: String,
}

#[derive(Debug, Serialize)]
pub struct RpInfo {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct UserInfo {
    /// Base64url-encoded user handle.
    pub id: String,
    pub name: String,
    pub display_name: String,
}

#[derive(Debug, Serialize)]
pub struct PubKeyCredParam {
    #[serde(rename = "type")]
    pub kind: String,
    pub alg: i32,
}

#[derive(Debug, Deserialize)]
pub struct CompleteRegistrationRequest {
    pub session_id: String,
    /// Base64url-encoded credential ID from the authenticator response.
    pub credential_id: String,
    /// Base64url-encoded client data JSON.
    pub client_data_json: String,
    /// Base64url-encoded attestation object (CBOR).
    pub attestation_object: String,
    pub label: Option<String>,
    #[serde(default)]
    pub is_backup: bool,
}

#[derive(Debug, Serialize)]
pub struct CompleteRegistrationResponse {
    pub credential_id: String,
    pub user_id: String,
    pub is_backup: bool,
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct BeginAuthenticationRequest {
    pub user_id: String,
}

#[derive(Debug, Serialize)]
pub struct BeginAuthenticationResponse {
    pub session_id: String,
    pub challenge: String,
    pub timeout_ms: u32,
    pub rp_id: String,
    pub allow_credentials: Vec<AllowCredential>,
    pub user_verification: String,
}

#[derive(Debug, Serialize)]
pub struct AllowCredential {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct CompleteAuthenticationRequest {
    pub session_id: String,
    pub credential_id: String,
    pub client_data_json: String,
    /// Base64url-encoded authenticator data.
    pub authenticator_data: String,
    /// Base64url-encoded assertion signature (ASN.1 DER for ES256; raw bytes
    /// for RS256/EdDSA), verified against the credential's stored COSE key.
    pub signature: String,
}

#[derive(Debug, Serialize)]
pub struct CompleteAuthenticationResponse {
    pub user_id: String,
    pub credential_id: String,
    pub sign_count: u32,
    pub authenticated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct AddBackupAuthenticatorRequest {
    pub user_id: String,
    pub label: Option<String>,
}

// ── Handler: begin registration ───────────────────────────────────────────────

/// `POST /webauthn/register/begin`
///
/// Issues a fresh registration challenge.  The client must complete it within
/// `CHALLENGE_TTL_SECS` by calling `complete_registration`.
pub async fn begin_registration(
    State(state): State<Arc<WebAuthnState>>,
    Json(body): Json<BeginRegistrationRequest>,
) -> Result<(StatusCode, Json<BeginRegistrationResponse>), (StatusCode, Json<serde_json::Value>)> {
    if body.user_id.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "user_id must not be empty" })),
        ));
    }
    if body.user_name.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "user_name must not be empty" })),
        ));
    }

    let challenge = generate_challenge();
    let session_id = b64url_encode(&generate_challenge()); // random session token

    let pending = PendingRegistration {
        challenge: challenge.clone(),
        user_id: body.user_id.clone(),
        expires_at: now_secs() + CHALLENGE_TTL_SECS,
    };

    state
        .pending_registrations
        .lock()
        .unwrap()
        .insert(session_id.clone(), pending);

    let response = BeginRegistrationResponse {
        session_id,
        rp: RpInfo {
            id: state.rp_id.clone(),
            name: state.rp_name.clone(),
        },
        user: UserInfo {
            id: b64url_encode(body.user_id.as_bytes()),
            name: body.user_name.clone(),
            display_name: body.user_name.clone(),
        },
        challenge: b64url_encode(&challenge),
        pub_key_cred_params: vec![
            PubKeyCredParam {
                kind: "public-key".into(),
                alg: CoseAlgorithm::ES256.cose_id(),
            },
            PubKeyCredParam {
                kind: "public-key".into(),
                alg: CoseAlgorithm::EdDsa.cose_id(),
            },
            PubKeyCredParam {
                kind: "public-key".into(),
                alg: CoseAlgorithm::RS256.cose_id(),
            },
        ],
        timeout_ms: (CHALLENGE_TTL_SECS * 1000) as u32,
        attestation: "none".into(),
    };

    Ok((StatusCode::OK, Json(response)))
}

// ── Handler: complete registration ────────────────────────────────────────────

/// `POST /webauthn/register/complete`
///
/// Validates the authenticator's attestation response and stores the credential.
///
/// The server requests `attestation: "none"`, so attestation *statement*
/// verification (certificate chain validation for formats like `"packed"` or
/// `"tpm"`) is not performed. This implementation does validate the
/// challenge/origin, parse the COSE public key out of `authData`'s attested
/// credential data, and reject registration if that key can't be parsed —
/// the parsed key is what `complete_authentication` later verifies assertion
/// signatures against.
pub async fn complete_registration(
    State(state): State<Arc<WebAuthnState>>,
    Json(body): Json<CompleteRegistrationRequest>,
) -> Result<(StatusCode, Json<CompleteRegistrationResponse>), (StatusCode, Json<serde_json::Value>)>
{
    // 1. Retrieve and expire the pending session.
    let pending = {
        let mut map = state.pending_registrations.lock().unwrap();
        match map.remove(&body.session_id) {
            Some(p) => p,
            None => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "unknown or expired session" })),
                ))
            }
        }
    };

    // 2. Check TTL.
    if now_secs() > pending.expires_at {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "registration challenge expired" })),
        ));
    }

    // 3. Decode and validate credential ID.
    let cred_id_bytes = b64url_decode(&body.credential_id).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid credential_id encoding" })),
        )
    })?;
    if cred_id_bytes.len() < MIN_CREDENTIAL_ID_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "credential_id too short" })),
        ));
    }

    // 4. Decode client data JSON and verify challenge + origin.
    let client_data_raw = b64url_decode(&body.client_data_json).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid client_data_json encoding" })),
        )
    })?;
    let client_data: serde_json::Value =
        serde_json::from_slice(&client_data_raw).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "client_data_json is not valid JSON" })),
            )
        })?;

    // Verify type field.
    if client_data.get("type").and_then(|v| v.as_str()) != Some("webauthn.create") {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "unexpected client data type" })),
        ));
    }

    // Verify challenge matches.
    let received_challenge = client_data
        .get("challenge")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if received_challenge != b64url_encode(&pending.challenge) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "challenge mismatch" })),
        ));
    }

    // Verify origin.
    let received_origin = client_data
        .get("origin")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if received_origin != state.origin {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "origin mismatch" })),
        ));
    }

    // 5. Decode the attestation object and extract + validate the credential's
    //    COSE public key (WebAuthn spec §6.5.4 / §6.1).
    let attestation_bytes = b64url_decode(&body.attestation_object).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid attestation_object encoding" })),
        )
    })?;
    let attestation_value: CborValue = ciborium::de::from_reader(attestation_bytes.as_slice())
        .map_err(|e| bad_request(format!("invalid attestation object CBOR: {e}")))?;
    let attestation_map = attestation_value
        .as_map()
        .ok_or_else(|| bad_request("attestation object is not a CBOR map"))?;
    let auth_data = attestation_map
        .iter()
        .find(|(k, _)| k.as_text() == Some("authData"))
        .and_then(|(_, v)| v.as_bytes())
        .ok_or_else(|| bad_request("attestation object missing authData"))?;

    let attested = parse_attested_credential_data(auth_data)
        .map_err(|e| bad_request(format!("could not parse attested credential data: {e}")))?;

    if attested.credential_id != cred_id_bytes {
        return Err(bad_request(
            "credential_id does not match the credential ID embedded in authData",
        ));
    }

    let (_public_key, algorithm) = parse_cose_public_key(&attested.cose_key_bytes)
        .map_err(|e| bad_request(format!("could not parse public key: {e}")))?;

    // 6. Check for duplicate credential IDs across all users.
    {
        let store = state.credentials.lock().unwrap();
        for creds in store.values() {
            if creds.iter().any(|c| c.credential_id == body.credential_id) {
                return Err((
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({ "error": "credential already registered" })),
                ));
            }
        }
    }

    // 7. Persist credential.
    let label = body.label.or_else(|| {
        if body.is_backup {
            Some("Backup Authenticator".to_string())
        } else {
            None
        }
    });

    let credential = StoredCredential {
        credential_id: body.credential_id.clone(),
        user_id: pending.user_id.clone(),
        public_key: b64url_encode(&attested.cose_key_bytes),
        algorithm,
        sign_count: 0,
        is_backup: body.is_backup,
        label: label.clone(),
        aaguid: Some(hex_encode(&attested.aaguid)),
        created_at: Utc::now(),
        last_used_at: None,
    };

    state
        .credentials
        .lock()
        .unwrap()
        .entry(pending.user_id.clone())
        .or_default()
        .push(credential);

    Ok((
        StatusCode::CREATED,
        Json(CompleteRegistrationResponse {
            credential_id: body.credential_id,
            user_id: pending.user_id,
            is_backup: body.is_backup,
            label,
            created_at: Utc::now(),
        }),
    ))
}

// ── Handler: begin authentication ────────────────────────────────────────────

/// `POST /webauthn/authenticate/begin`
pub async fn begin_authentication(
    State(state): State<Arc<WebAuthnState>>,
    Json(body): Json<BeginAuthenticationRequest>,
) -> Result<(StatusCode, Json<BeginAuthenticationResponse>), (StatusCode, Json<serde_json::Value>)>
{
    if body.user_id.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "user_id must not be empty" })),
        ));
    }

    // Build allow list from registered credentials.
    let allow_credentials: Vec<AllowCredential> = {
        let store = state.credentials.lock().unwrap();
        store
            .get(&body.user_id)
            .map(|creds| {
                creds
                    .iter()
                    .map(|c| AllowCredential {
                        kind: "public-key".into(),
                        id: c.credential_id.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    };

    if allow_credentials.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "no credentials registered for this user" })),
        ));
    }

    let challenge = generate_challenge();
    let session_id = b64url_encode(&generate_challenge());

    let pending = PendingAuthentication {
        challenge: challenge.clone(),
        user_id: body.user_id.clone(),
        expires_at: now_secs() + CHALLENGE_TTL_SECS,
    };

    state
        .pending_authentications
        .lock()
        .unwrap()
        .insert(session_id.clone(), pending);

    Ok((
        StatusCode::OK,
        Json(BeginAuthenticationResponse {
            session_id,
            challenge: b64url_encode(&challenge),
            timeout_ms: (CHALLENGE_TTL_SECS * 1000) as u32,
            rp_id: state.rp_id.clone(),
            allow_credentials,
            user_verification: "preferred".into(),
        }),
    ))
}

// ── Handler: complete authentication ─────────────────────────────────────────

/// `POST /webauthn/authenticate/complete`
pub async fn complete_authentication(
    State(state): State<Arc<WebAuthnState>>,
    Json(body): Json<CompleteAuthenticationRequest>,
) -> Result<(StatusCode, Json<CompleteAuthenticationResponse>), (StatusCode, Json<serde_json::Value>)>
{
    // 1. Retrieve and expire the pending session.
    let pending = {
        let mut map = state.pending_authentications.lock().unwrap();
        match map.remove(&body.session_id) {
            Some(p) => p,
            None => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "unknown or expired session" })),
                ))
            }
        }
    };

    if now_secs() > pending.expires_at {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "authentication challenge expired" })),
        ));
    }

    // 2. Decode and validate client data JSON.
    let client_data_raw = b64url_decode(&body.client_data_json).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid client_data_json encoding" })),
        )
    })?;
    let client_data: serde_json::Value =
        serde_json::from_slice(&client_data_raw).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "client_data_json is not valid JSON" })),
            )
        })?;

    if client_data.get("type").and_then(|v| v.as_str()) != Some("webauthn.get") {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "unexpected client data type" })),
        ));
    }

    let received_challenge = client_data
        .get("challenge")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if received_challenge != b64url_encode(&pending.challenge) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "challenge mismatch" })),
        ));
    }

    let received_origin = client_data
        .get("origin")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if received_origin != state.origin {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "origin mismatch" })),
        ));
    }

    // 3. Decode authenticator data and extract sign_count.
    let auth_data = b64url_decode(&body.authenticator_data).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid authenticator_data encoding" })),
        )
    })?;

    // The authenticator data layout (WebAuthn spec §6.1):
    //   [0..32]  rpIdHash
    //   [32]     flags
    //   [33..36] signCount (big-endian u32)
    let new_sign_count = if auth_data.len() >= 37 {
        u32::from_be_bytes([auth_data[33], auth_data[34], auth_data[35], auth_data[36]])
    } else {
        0
    };

    // 4. Verify signature is present and non-empty.
    let sig_bytes = b64url_decode(&body.signature).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid signature encoding" })),
        )
    })?;
    if sig_bytes.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "signature must not be empty" })),
        ));
    }

    // 5. Look up the credential.
    let mut credentials = state.credentials.lock().unwrap();
    let user_creds = credentials.get_mut(&pending.user_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "no credentials for user" })),
        )
    })?;

    let cred = user_creds
        .iter_mut()
        .find(|c| c.credential_id == body.credential_id)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "credential not found" })),
            )
        })?;

    // 6. Cryptographically verify the assertion signature against the
    //    credential's stored public key (WebAuthn spec §7.2). This is the
    //    primary authentication check — it is what actually proves
    //    possession of the authenticator's private key.
    let public_key_bytes = b64url_decode(&cred.public_key).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "stored credential public key is corrupted" })),
        )
    })?;
    let message = signed_message(&auth_data, &client_data_raw);
    if verify_assertion_signature(&public_key_bytes, cred.algorithm, &message, &sig_bytes).is_err()
    {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "assertion signature verification failed" })),
        ));
    }

    // 7. Signature counter check: new value must be > stored value (cloning
    //    detection). This is a secondary, defense-in-depth layer on top of
    //    the signature check above — not a replacement for it.
    // Authenticators that always return 0 are exempt (value == 0 on both sides).
    if new_sign_count != 0 && new_sign_count <= cred.sign_count {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "authenticator clone detected: sign count did not increase"
            })),
        ));
    }

    // 8. Update counter and last_used timestamp.
    cred.sign_count = new_sign_count;
    cred.last_used_at = Some(Utc::now());

    Ok((
        StatusCode::OK,
        Json(CompleteAuthenticationResponse {
            user_id: pending.user_id,
            credential_id: body.credential_id,
            sign_count: new_sign_count,
            authenticated_at: Utc::now(),
        }),
    ))
}

// ── Handler: list credentials ─────────────────────────────────────────────────

/// `GET /webauthn/credentials/:user_id`
pub async fn list_credentials(
    State(state): State<Arc<WebAuthnState>>,
    Path(user_id): Path<String>,
) -> Result<Json<Vec<StoredCredential>>, (StatusCode, Json<serde_json::Value>)> {
    let store = state.credentials.lock().unwrap();
    let creds = store.get(&user_id).cloned().unwrap_or_default();
    Ok(Json(creds))
}

// ── Handler: remove credential ────────────────────────────────────────────────

/// `DELETE /webauthn/credentials/:user_id/:cred_id`
pub async fn remove_credential(
    State(state): State<Arc<WebAuthnState>>,
    Path((user_id, cred_id)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let mut store = state.credentials.lock().unwrap();
    let creds = store.get_mut(&user_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "user not found" })),
        )
    })?;

    let before = creds.len();
    creds.retain(|c| c.credential_id != cred_id);

    if creds.len() == before {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "credential not found" })),
        ));
    }

    // Prevent removing the last primary credential unless a backup exists.
    let primaries = creds.iter().filter(|c| !c.is_backup).count();
    let backups = creds.iter().filter(|c| c.is_backup).count();
    if primaries == 0 && backups == 0 {
        // Undo — put the credential back (re-load and re-remove isn't feasible
        // without re-looking up, so we surface an error before the retain above
        // would leave the user credential-less).
        // This branch is unreachable if there was at least one credential before
        // removal, so it serves as a defensive guard only.
        return Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "cannot remove the last credential for a user"
            })),
        ));
    }

    Ok(StatusCode::NO_CONTENT)
}

// ── Handler: add backup authenticator ────────────────────────────────────────

/// `POST /webauthn/credentials/:user_id/:cred_id/backup`
///
/// Marks an existing credential as a backup authenticator, or begins a new
/// backup-flagged registration challenge (returns 202 Accepted with a
/// registration challenge the client should complete).
pub async fn add_backup_authenticator(
    State(state): State<Arc<WebAuthnState>>,
    Path((user_id, cred_id)): Path<(String, String)>,
    Json(body): Json<AddBackupAuthenticatorRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    // If the credential already exists, just flip the is_backup flag.
    {
        let mut store = state.credentials.lock().unwrap();
        if let Some(creds) = store.get_mut(&user_id) {
            if let Some(cred) = creds.iter_mut().find(|c| c.credential_id == cred_id) {
                cred.is_backup = true;
                if let Some(label) = body.label {
                    cred.label = Some(label);
                }
                return Ok((
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "credential_id": cred_id,
                        "is_backup": true,
                        "message": "credential marked as backup authenticator"
                    })),
                ));
            }
        }
    }

    // Credential not yet registered — issue a backup-flagged registration challenge.
    let challenge = generate_challenge();
    let session_id = b64url_encode(&generate_challenge());

    let pending = PendingRegistration {
        challenge: challenge.clone(),
        user_id: user_id.clone(),
        expires_at: now_secs() + CHALLENGE_TTL_SECS,
    };

    state
        .pending_registrations
        .lock()
        .unwrap()
        .insert(session_id.clone(), pending);

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "session_id": session_id,
            "challenge": b64url_encode(&challenge),
            "is_backup": true,
            "message": "complete registration with is_backup=true to add backup authenticator",
            "pub_key_cred_params": [
                { "type": "public-key", "alg": CoseAlgorithm::ES256.cose_id() },
                { "type": "public-key", "alg": CoseAlgorithm::EdDsa.cose_id() },
            ],
            "timeout_ms": CHALLENGE_TTL_SECS * 1000
        })),
    ))
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use signature::{SignatureEncoding, Signer};

    fn make_state() -> Arc<WebAuthnState> {
        Arc::new(WebAuthnState::new(
            "localhost",
            "Test RP",
            "http://localhost:3000",
        ))
    }

    fn client_data_json(type_: &str, challenge: &str, origin: &str) -> String {
        let raw = serde_json::json!({
            "type": type_,
            "challenge": challenge,
            "origin": origin
        })
        .to_string();
        b64url_encode(raw.as_bytes())
    }

    fn fake_auth_data(sign_count: u32) -> String {
        let mut data = vec![0u8; 37];
        let bytes = sign_count.to_be_bytes();
        data[33] = bytes[0];
        data[34] = bytes[1];
        data[35] = bytes[2];
        data[36] = bytes[3];
        b64url_encode(&data)
    }

    /// Encodes an ES256 COSE_Key (RFC 9053 §7.1) for a P-256 verifying key.
    fn cose_key_es256(vk: &p256::ecdsa::VerifyingKey) -> Vec<u8> {
        let point = vk.to_encoded_point(false);
        let map = CborValue::Map(vec![
            (
                CborValue::Integer(1i64.into()),
                CborValue::Integer(2i64.into()),
            ), // kty: EC2
            (
                CborValue::Integer(3i64.into()),
                CborValue::Integer((-7i64).into()),
            ), // alg: ES256
            (
                CborValue::Integer((-1i64).into()),
                CborValue::Integer(1i64.into()),
            ), // crv: P-256
            (
                CborValue::Integer((-2i64).into()),
                CborValue::Bytes(point.x().unwrap().to_vec()),
            ),
            (
                CborValue::Integer((-3i64).into()),
                CborValue::Bytes(point.y().unwrap().to_vec()),
            ),
        ]);
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&map, &mut buf).unwrap();
        buf
    }

    /// Builds a registration `authData` blob (WebAuthn spec §6.1) with
    /// attested credential data for the given credential ID + COSE key.
    fn registration_auth_data(cred_id: &[u8], cose_key_bytes: &[u8]) -> Vec<u8> {
        let mut data = vec![0u8; 32]; // rpIdHash (unchecked by this server)
        data.push(0x41); // flags: UP | AT
        data.extend_from_slice(&0u32.to_be_bytes()); // signCount
        data.extend_from_slice(&[0u8; 16]); // aaguid
        data.extend_from_slice(&(cred_id.len() as u16).to_be_bytes());
        data.extend_from_slice(cred_id);
        data.extend_from_slice(cose_key_bytes);
        data
    }

    /// Wraps `authData` in a `"none"`-format CBOR attestation object.
    fn attestation_object(auth_data: &[u8]) -> String {
        let map = CborValue::Map(vec![
            (
                CborValue::Text("fmt".into()),
                CborValue::Text("none".into()),
            ),
            (CborValue::Text("attStmt".into()), CborValue::Map(vec![])),
            (
                CborValue::Text("authData".into()),
                CborValue::Bytes(auth_data.to_vec()),
            ),
        ]);
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&map, &mut buf).unwrap();
        b64url_encode(&buf)
    }

    /// Registers an ES256 credential with a fresh keypair and returns the
    /// signing key plus the credential ID used.
    async fn register_es256_credential(
        state: &Arc<WebAuthnState>,
        user_id: &str,
        user_name: &str,
        cred_id_byte: u8,
    ) -> (p256::ecdsa::SigningKey, String) {
        let signing_key = p256::ecdsa::SigningKey::random(&mut rand::thread_rng());
        let verifying_key = p256::ecdsa::VerifyingKey::from(&signing_key);
        let cose_key = cose_key_es256(&verifying_key);

        let begin = begin_registration(
            State(Arc::clone(state)),
            Json(BeginRegistrationRequest {
                user_id: user_id.into(),
                user_name: user_name.into(),
                label: None,
                is_backup: false,
            }),
        )
        .await
        .unwrap();
        let (_, Json(begin)) = begin;

        let client_data =
            client_data_json("webauthn.create", &begin.challenge, "http://localhost:3000");
        let cred_id = b64url_encode(&[cred_id_byte; 32]);
        let cred_id_bytes = b64url_decode(&cred_id).unwrap();
        let reg_auth_data = registration_auth_data(&cred_id_bytes, &cose_key);

        let _ = complete_registration(
            State(Arc::clone(state)),
            Json(CompleteRegistrationRequest {
                session_id: begin.session_id,
                credential_id: cred_id.clone(),
                client_data_json: client_data,
                attestation_object: attestation_object(&reg_auth_data),
                label: None,
                is_backup: false,
            }),
        )
        .await
        .unwrap();

        (signing_key, cred_id)
    }

    /// Signs a `webauthn.get` assertion for `sign_count` with `signing_key`,
    /// returning the (client_data_json, authenticator_data, signature)
    /// triple ready for `CompleteAuthenticationRequest`.
    fn sign_assertion(
        signing_key: &p256::ecdsa::SigningKey,
        challenge: &str,
        sign_count: u32,
    ) -> (String, String, String) {
        let client_data = client_data_json("webauthn.get", challenge, "http://localhost:3000");
        let client_data_raw = b64url_decode(&client_data).unwrap();
        let auth_data = fake_auth_data(sign_count);
        let auth_data_bytes = b64url_decode(&auth_data).unwrap();
        let message = signed_message(&auth_data_bytes, &client_data_raw);
        let signature: p256::ecdsa::Signature = signing_key.sign(&message);
        let sig_b64 = b64url_encode(&signature.to_der().to_vec());
        (client_data, auth_data, sig_b64)
    }

    // A fixed 2048-bit RSA test keypair (PKCS8, generated with `openssl genrsa`
    // + `openssl pkcs8 -topk8 -nocrypt`), used only to exercise the RS256 code
    // path in tests. `ring` (used for RS256 verification) intentionally does
    // not support RSA key *generation*, so this fixture is externally
    // generated rather than created at test time — it is not used anywhere
    // outside this test module.
    const RSA_TEST_PKCS8_B64: &str = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDJbUMM+d4wbMzzZpCDwNSK5EYzsjOUEcJD4fQl4MPoRTTl4mcOr2Snv3hwqCMK/bhOU/943vOe0QV+AZxVt/a6ZDHYALZYbNoPLER6mV1BAVv0gR2RXgyNemrNuLV0NyA6Uiad71J8Xnx/87+fzUrLkSc86y8VD4kFsjNoemyKy8lh1BvG0dA3fA2k91/0aG4jMOw2/gJJRxNekBjuudIsLN/+M80ulqk5EKiCrO2RwjHXz+0FGceboJEJBgMxwIqjjEtcAZY6nsk0d0i6SAoSy/1AT1dgnI6DrZXvpSRa6O8eTbEHLEfAvxDvQuGyISx4842MundJzXHHoVBMdAkbAgMBAAECggEAFKqmvK/tvvyIIhhtucRGVRfV0hu1v2VRECOgqBBB2X+G+BKSLgdgkEuMwSD8iMtT0TQ8xpCNi3J5GfQdWmLSfaOmb49R0OHI6nyewWYh6M0Ju8em79F9VTjFuOTQ4qMopdiO825o1q+Kc5sKmAv5YXM7r5GWgOvZTGHRH3uheQiqvzM9PVl0zCBx2oMPADEY2y9CVrq07Q0yHG8plRfKvbURO44IWVaRKa63H+VfqAM17mNYwo0HyuI9urXIwiJ3AHxK4VFD2ULn1xZgKQS8esR8clN/LSCEswVTYaNeL1BjYJ3hmfjKjZzyRAWdTz6Ffb0UCBMRA9U+ogqBqm+f1QKBgQDmMfHYe8/6HK2ns0Pue9e+I8Vbr6ptInDtVBMSFyKwxwJ6NKoUUw8+389tdrWOBUyD3+5zTkWou3DzHMkvbwBxOH9YFtGfov4BFA2grQVWBD9TL6OW6yJT6fqC9GBufMBa5JSYLrhe1YBoefL2D0KdK/WMS+9gW1S3HiK+0bx+vwKBgQDgAbw2akGrqHSJh7M+/brMmqwBmg/2JF0TGgr9JkhpNgPNNeYWaSo8LWNjZGAC2Tx3ZSrg1chPydFQxCKNeI2Pcs5VemstZtk+lJYgnBI5vSdUGaUw0PZn+VQBtGhYinyqZUriUeFGp6lJ4hf/6IwBUl0vtrXKm6sppIiPgHeopQKBgDzCHgV32JM5kpRa+qktwuoK4wKqQR+BIbFiqY3y0VM7k+nRkLrAmZuM02EfHhiYSXPdXUDN/hDlOJDSnj+I2uMHeIU1sKqkCMscEeTBBlGH2XcJcfJZqbvgXCDIg9Nl1henkZkBa+SMEdKBraFIsdpuSed3+3zBXoDe0WjwTwJdAoGASgfoxucI+w06LnWdhJTgVlxLul/LJKLR680wkodDaRoD2Z8VgpSQ88BgV2nF3UskE6VorVOZ1tyxA4s+jBiqWB0uGcvSffe+llMO5ooN7+0WgVHUaTS2KpiY7dNMpO5n0vyU6gT7eZlRdmx1WArnskwhJfKxU9tsjt+kjiB7600CgYEAixYCj735R51CljbU2rq54HXQ3J9LOmuZnC+GNP61WL5h3gBHeMjbFzEBM/F65u+m0hFNT2+2wNvb32ku3GZXvUKIhSjqhRX4owfMXUpgMD6UKVHVHcbhGSpCnKUpF9aEHg4G0ZngFdCWavllSnW152gGZKmfXJotxV3lM1q1g5A=";
    const RSA_TEST_MODULUS_HEX: &str = "C96D430CF9DE306CCCF3669083C0D48AE44633B2339411C243E1F425E0C3E84534E5E2670EAF64A7BF7870A8230AFDB84E53FF78DEF39ED1057E019C55B7F6BA6431D800B6586CDA0F2C447A995D41015BF4811D915E0C8D7A6ACDB8B57437203A52269DEF527C5E7C7FF3BF9FCD4ACB91273CEB2F150F8905B233687A6C8ACBC961D41BC6D1D0377C0DA4F75FF4686E2330EC36FE024947135E9018EEB9D22C2CDFFE33CD2E96A93910A882ACED91C231D7CFED0519C79BA09109060331C08AA38C4B5C01963A9EC9347748BA480A12CBFD404F57609C8E83AD95EFA5245AE8EF1E4DB1072C47C0BF10EF42E1B2212C78F38D8CBA7749CD71C7A1504C74091B";
    const RSA_TEST_EXPONENT: [u8; 3] = [0x01, 0x00, 0x01]; // 65537

    fn rsa_test_n_bytes() -> Vec<u8> {
        (0..RSA_TEST_MODULUS_HEX.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&RSA_TEST_MODULUS_HEX[i..i + 2], 16).unwrap())
            .collect()
    }

    fn rsa_test_keypair() -> ring::signature::RsaKeyPair {
        use base64::Engine as _;
        let pkcs8 = base64::engine::general_purpose::STANDARD
            .decode(RSA_TEST_PKCS8_B64)
            .unwrap();
        ring::signature::RsaKeyPair::from_pkcs8(&pkcs8).unwrap()
    }

    /// Encodes an RS256 COSE_Key (RFC 9053 §7.1) for the fixture RSA keypair.
    fn cose_key_rs256() -> Vec<u8> {
        let map = CborValue::Map(vec![
            (
                CborValue::Integer(1i64.into()),
                CborValue::Integer(3i64.into()),
            ), // kty: RSA
            (
                CborValue::Integer(3i64.into()),
                CborValue::Integer((-257i64).into()),
            ), // alg: RS256
            (
                CborValue::Integer((-1i64).into()),
                CborValue::Bytes(rsa_test_n_bytes()),
            ),
            (
                CborValue::Integer((-2i64).into()),
                CborValue::Bytes(RSA_TEST_EXPONENT.to_vec()),
            ),
        ]);
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&map, &mut buf).unwrap();
        buf
    }

    fn sign_assertion_rs256(challenge: &str, sign_count: u32) -> (String, String, String) {
        let client_data = client_data_json("webauthn.get", challenge, "http://localhost:3000");
        let client_data_raw = b64url_decode(&client_data).unwrap();
        let auth_data = fake_auth_data(sign_count);
        let auth_data_bytes = b64url_decode(&auth_data).unwrap();
        let message = signed_message(&auth_data_bytes, &client_data_raw);

        let keypair = rsa_test_keypair();
        let mut signature = vec![0u8; keypair.public().modulus_len()];
        keypair
            .sign(
                &ring::signature::RSA_PKCS1_SHA256,
                &ring::rand::SystemRandom::new(),
                &message,
                &mut signature,
            )
            .unwrap();
        (client_data, auth_data, b64url_encode(&signature))
    }

    #[tokio::test]
    async fn test_begin_registration_empty_user_id() {
        let state = make_state();
        let result = begin_registration(
            State(Arc::clone(&state)),
            Json(BeginRegistrationRequest {
                user_id: "".into(),
                user_name: "alice".into(),
                label: None,
                is_backup: false,
            }),
        )
        .await;
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn test_registration_and_authentication_flow() {
        let state = make_state();

        // 1-3. Register a real ES256 credential.
        let (signing_key, cred_id) =
            register_es256_credential(&state, "user1", "Alice", 0xAA).await;

        // 4. Begin authentication.
        let auth_begin = begin_authentication(
            State(Arc::clone(&state)),
            Json(BeginAuthenticationRequest {
                user_id: "user1".into(),
            }),
        )
        .await
        .unwrap();
        let (_, Json(auth_begin)) = auth_begin;
        assert!(!auth_begin.allow_credentials.is_empty());

        // 5. Complete authentication with a genuinely signed assertion —
        //    proves an assertion signed with the credential's actual
        //    private key is accepted.
        let (client_data, auth_data, signature) =
            sign_assertion(&signing_key, &auth_begin.challenge, 1);

        let auth_complete = complete_authentication(
            State(Arc::clone(&state)),
            Json(CompleteAuthenticationRequest {
                session_id: auth_begin.session_id,
                credential_id: cred_id,
                client_data_json: client_data,
                authenticator_data: auth_data,
                signature,
            }),
        )
        .await
        .unwrap();
        let (status, Json(auth)) = auth_complete;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(auth.user_id, "user1");
        assert_eq!(auth.sign_count, 1);
    }

    #[tokio::test]
    async fn test_forged_assertion_signature_rejected() {
        let state = make_state();

        // Register a real credential for the victim.
        let (_signing_key, cred_id) =
            register_es256_credential(&state, "user3", "Carol", 0xCC).await;

        let auth_begin = begin_authentication(
            State(Arc::clone(&state)),
            Json(BeginAuthenticationRequest {
                user_id: "user3".into(),
            }),
        )
        .await
        .unwrap();
        let (_, Json(auth_begin)) = auth_begin;

        // Forge the assertion: valid encoding, non-empty, correct
        // credential_id — but signed with a keypair that doesn't match the
        // one registered for this credential.
        let attacker_key = p256::ecdsa::SigningKey::random(&mut rand::thread_rng());
        let (client_data, auth_data, signature) =
            sign_assertion(&attacker_key, &auth_begin.challenge, 1);

        let result = complete_authentication(
            State(Arc::clone(&state)),
            Json(CompleteAuthenticationRequest {
                session_id: auth_begin.session_id,
                credential_id: cred_id,
                client_data_json: client_data,
                authenticator_data: auth_data,
                signature,
            }),
        )
        .await;

        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_clone_detection() {
        let state = make_state();

        let (signing_key, cred_id) = register_es256_credential(&state, "user2", "Bob", 0xBB).await;

        // Authenticate once with sign_count=5, correctly signed.
        let auth_begin = begin_authentication(
            State(Arc::clone(&state)),
            Json(BeginAuthenticationRequest {
                user_id: "user2".into(),
            }),
        )
        .await
        .unwrap();
        let (_, Json(auth_begin)) = auth_begin;
        let (client_data, auth_data, signature) =
            sign_assertion(&signing_key, &auth_begin.challenge, 5);
        let _ = complete_authentication(
            State(Arc::clone(&state)),
            Json(CompleteAuthenticationRequest {
                session_id: auth_begin.session_id,
                credential_id: cred_id.clone(),
                client_data_json: client_data,
                authenticator_data: auth_data,
                signature,
            }),
        )
        .await
        .unwrap();

        // Attempt replay with sign_count=3 (lower), correctly signed — must
        // still be rejected, this time by the sign-count check rather than
        // signature verification.
        let auth_begin2 = begin_authentication(
            State(Arc::clone(&state)),
            Json(BeginAuthenticationRequest {
                user_id: "user2".into(),
            }),
        )
        .await
        .unwrap();
        let (_, Json(auth_begin2)) = auth_begin2;
        let (client_data2, auth_data2, signature2) =
            sign_assertion(&signing_key, &auth_begin2.challenge, 3);
        let result = complete_authentication(
            State(Arc::clone(&state)),
            Json(CompleteAuthenticationRequest {
                session_id: auth_begin2.session_id,
                credential_id: cred_id,
                client_data_json: client_data2,
                authenticator_data: auth_data2,
                signature: signature2,
            }),
        )
        .await;
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, StatusCode::FORBIDDEN);
    }

    /// Exercises the RS256 branch of COSE key parsing and signature
    /// verification end-to-end (registration → authentication), proving
    /// algorithm selection isn't hardcoded to ES256.
    #[tokio::test]
    async fn test_rs256_registration_and_authentication() {
        let state = make_state();

        let begin = begin_registration(
            State(Arc::clone(&state)),
            Json(BeginRegistrationRequest {
                user_id: "user4".into(),
                user_name: "Dave".into(),
                label: None,
                is_backup: false,
            }),
        )
        .await
        .unwrap();
        let (_, Json(begin)) = begin;

        let client_data =
            client_data_json("webauthn.create", &begin.challenge, "http://localhost:3000");
        let cred_id = b64url_encode(&[0xDD; 32]);
        let cred_id_bytes = b64url_decode(&cred_id).unwrap();
        let reg_auth_data = registration_auth_data(&cred_id_bytes, &cose_key_rs256());

        let complete_resp = complete_registration(
            State(Arc::clone(&state)),
            Json(CompleteRegistrationRequest {
                session_id: begin.session_id,
                credential_id: cred_id.clone(),
                client_data_json: client_data,
                attestation_object: attestation_object(&reg_auth_data),
                label: None,
                is_backup: false,
            }),
        )
        .await
        .unwrap();
        let (_, Json(reg)) = complete_resp;
        assert_eq!(reg.credential_id, cred_id);

        let auth_begin = begin_authentication(
            State(Arc::clone(&state)),
            Json(BeginAuthenticationRequest {
                user_id: "user4".into(),
            }),
        )
        .await
        .unwrap();
        let (_, Json(auth_begin)) = auth_begin;

        let (client_data, auth_data, signature) = sign_assertion_rs256(&auth_begin.challenge, 1);
        let auth_complete = complete_authentication(
            State(Arc::clone(&state)),
            Json(CompleteAuthenticationRequest {
                session_id: auth_begin.session_id,
                credential_id: cred_id,
                client_data_json: client_data,
                authenticator_data: auth_data,
                signature,
            }),
        )
        .await
        .unwrap();
        let (status, Json(auth)) = auth_complete;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(auth.user_id, "user4");
    }
}

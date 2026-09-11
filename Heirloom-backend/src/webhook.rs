//! Webhook system for event push notifications (#65).
//!
//! Clients register a URL (and optional secret) to receive HTTP POST payloads
//! whenever a vault event occurs.  The delivery engine retries failed
//! deliveries with exponential back-off.
//!
//! # Architecture
//!
//! ```text
//! POST /webhooks            → register_webhook  (stores WebhookRegistration)
//! GET  /webhooks            → list_webhooks
//! DELETE /webhooks/:id      → delete_webhook
//! Internal: deliver_event() → called by handlers when events are emitted
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Data types ────────────────────────────────────────────────────────────────

/// Events that can trigger webhook delivery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebhookEventType {
    VaultCreated,
    VaultCheckedIn,
    VaultReleased,
    VaultDeposit,
    VaultWithdrawal,
    BeneficiaryUpdated,
    VaultPaused,
    VaultResumed,
}

/// A persisted webhook subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookRegistration {
    pub id: String,
    /// The HTTPS (or HTTP for local dev) URL to POST events to.
    pub url: String,
    /// Optional vault filter; `None` = receive events for all vaults.
    pub vault_id: Option<String>,
    /// Event types to deliver; empty = all event types.
    pub event_types: Vec<WebhookEventType>,
    /// HMAC secret used to sign payloads (optional).
    pub secret: Option<String>,
    /// Algorithm used for HMAC signing. Defaults to SHA-256.
    #[serde(default)]
    pub algorithm: SignatureAlgorithm,
    pub created_at: DateTime<Utc>,
    pub active: bool,
}

/// Request body for `POST /webhooks`.
#[derive(Debug, Deserialize)]
pub struct RegisterWebhookRequest {
    pub url: String,
    pub vault_id: Option<String>,
    #[serde(default)]
    pub event_types: Vec<WebhookEventType>,
    pub secret: Option<String>,
    /// Signing algorithm to use when a `secret` is provided. Defaults to SHA-256.
    #[serde(default)]
    pub algorithm: SignatureAlgorithm,
}

/// Payload sent to the registered URL on each event.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookPayload {
    pub id: String,
    pub event_type: WebhookEventType,
    pub vault_id: String,
    pub timestamp: DateTime<Utc>,
    pub data: serde_json::Value,
}

// ── In-memory store ───────────────────────────────────────────────────────────

pub type WebhookStore = Arc<Mutex<HashMap<String, WebhookRegistration>>>;

pub fn create_webhook_store() -> WebhookStore {
    Arc::new(Mutex::new(HashMap::new()))
}

// ── Axum handler state ────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct WebhookState {
    pub store: WebhookStore,
    pub http_client: Client,
    /// Dead-letter queue that exhausted deliveries are routed into.
    pub dlq_state: Arc<crate::dlq::DlqState>,
    /// Health-aware routing weights, keyed by webhook URL.
    pub health_routing_state: Arc<crate::health_routing::HealthRoutingState>,
}

impl WebhookState {
    pub fn new() -> Self {
        Self::with_reliability_state(
            Arc::new(crate::dlq::DlqState::new()),
            Arc::new(crate::health_routing::HealthRoutingState::new()),
        )
    }

    pub fn with_reliability_state(
        dlq_state: Arc<crate::dlq::DlqState>,
        health_routing_state: Arc<crate::health_routing::HealthRoutingState>,
    ) -> Self {
        Self {
            store: create_webhook_store(),
            http_client: Client::new(),
            dlq_state,
            health_routing_state,
        }
    }
}

impl Default for WebhookState {
    fn default() -> Self {
        Self::new()
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// `POST /webhooks` — register a new webhook endpoint.
pub async fn register_webhook(
    State(state): State<Arc<WebhookState>>,
    Json(body): Json<RegisterWebhookRequest>,
) -> Result<(StatusCode, Json<WebhookRegistration>), (StatusCode, Json<serde_json::Value>)> {
    if body.url.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "url must not be empty" })),
        ));
    }

    let registration = WebhookRegistration {
        id: Uuid::new_v4().to_string(),
        url: body.url,
        vault_id: body.vault_id,
        event_types: body.event_types,
        secret: body.secret,
        algorithm: body.algorithm,
        created_at: Utc::now(),
        active: true,
    };

    let mut store = state.store.lock().unwrap();
    store.insert(registration.id.clone(), registration.clone());

    Ok((StatusCode::CREATED, Json(registration)))
}

/// `GET /webhooks` — list all registered webhooks.
pub async fn list_webhooks(
    State(state): State<Arc<WebhookState>>,
) -> Json<Vec<WebhookRegistration>> {
    let store = state.store.lock().unwrap();
    let webhooks: Vec<WebhookRegistration> = store.values().cloned().collect();
    Json(webhooks)
}

/// `DELETE /webhooks/:id` — deactivate a webhook.
pub async fn delete_webhook(
    State(state): State<Arc<WebhookState>>,
    Path(id): Path<String>,
) -> StatusCode {
    let mut store = state.store.lock().unwrap();
    if let Some(wh) = store.get_mut(&id) {
        wh.active = false;
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

// ── Event delivery ────────────────────────────────────────────────────────────

/// Deliver `payload` to all matching active webhook registrations.
///
/// Each delivery is attempted asynchronously.  A failed request is retried up
/// to `MAX_RETRIES` times with exponential back-off before being dropped.
///
/// This function is fire-and-forget: it spawns a Tokio task per matching
/// webhook so that the calling handler is never blocked.
pub fn deliver_event(
    state: Arc<WebhookState>,
    event_type: WebhookEventType,
    vault_id: String,
    data: serde_json::Value,
) {
    let payload = WebhookPayload {
        id: Uuid::new_v4().to_string(),
        event_type: event_type.clone(),
        vault_id: vault_id.clone(),
        timestamp: Utc::now(),
        data,
    };

    let registrations: Vec<WebhookRegistration> = {
        let store = state.store.lock().unwrap();
        store
            .values()
            .filter(|wh| {
                if !wh.active {
                    return false;
                }
                // Filter by vault_id if specified.
                if let Some(ref wh_vault) = wh.vault_id {
                    if *wh_vault != vault_id {
                        return false;
                    }
                }
                // Filter by event type if a non-empty list is configured.
                if !wh.event_types.is_empty() && !wh.event_types.contains(&event_type) {
                    return false;
                }
                true
            })
            .cloned()
            .collect()
    };

    for registration in registrations {
        // Health-aware routing (#4): skip endpoints that have been failing
        // consistently rather than continuing to hammer them.
        if !crate::health_routing::should_route(&state.health_routing_state, &registration.url) {
            tracing::warn!(
                webhook_id = %registration.id,
                url = %registration.url,
                "skipping delivery — endpoint is routed as unhealthy"
            );
            continue;
        }

        let client = state.http_client.clone();
        let payload_clone = payload.clone();
        let state_clone = Arc::clone(&state);

        tokio::spawn(async move {
            attempt_delivery(&client, &registration, &payload_clone, &state_clone).await;
        });
    }
}

// ── Internal delivery with retry ──────────────────────────────────────────────

const MAX_RETRIES: u32 = 4;
const BASE_DELAY_MS: u64 = 250;

async fn attempt_delivery(
    client: &Client,
    registration: &WebhookRegistration,
    payload: &WebhookPayload,
    state: &WebhookState,
) {
    let body = match serde_json::to_string(payload) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(webhook_id = %registration.id, "Failed to serialize webhook payload: {e}");
            return;
        }
    };

    for attempt in 0..=MAX_RETRIES {
        if attempt > 0 {
            let delay_ms = BASE_DELAY_MS * (1 << (attempt - 1)); // 250, 500, 1000, 2000 ms
            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
        }

        let mut req = client
            .post(&registration.url)
            .header("Content-Type", "application/json")
            .header("X-Heirloom-Event", format!("{:?}", payload.event_type))
            .header("X-Heirloom-Delivery", &payload.id)
            .body(body.clone());

        // Add HMAC-SHA256 signature + timestamp headers if a secret is configured.
        if let Some(ref secret) = registration.secret {
            let (signature, timestamp) =
                build_signature_headers(&body, secret, registration.algorithm);
            req = req
                .header("X-Heirloom-Signature", signature)
                .header("X-Heirloom-Timestamp", timestamp);
        }

        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::debug!(
                    webhook_id = %registration.id,
                    status = %resp.status(),
                    attempt,
                    "Webhook delivery succeeded"
                );
                crate::health_routing::record_outcome(
                    &state.health_routing_state,
                    &registration.url,
                    true,
                );
                return;
            }
            Ok(resp) => {
                tracing::warn!(
                    webhook_id = %registration.id,
                    status = %resp.status(),
                    attempt,
                    "Webhook delivery failed — will retry"
                );
                crate::health_routing::record_outcome(
                    &state.health_routing_state,
                    &registration.url,
                    false,
                );
            }
            Err(e) => {
                tracing::warn!(
                    webhook_id = %registration.id,
                    attempt,
                    "Webhook delivery error: {e} — will retry"
                );
                crate::health_routing::record_outcome(
                    &state.health_routing_state,
                    &registration.url,
                    false,
                );
            }
        }
    }

    tracing::error!(
        webhook_id = %registration.id,
        "Webhook delivery exhausted all retries — dropping"
    );

    // Dead-letter the payload (#2) instead of discarding it so it can be
    // inspected and replayed once the endpoint recovers.
    crate::dlq::route_to_dlq(
        &state.dlq_state,
        format!("webhook:{}", registration.id),
        Some(registration.url.clone()),
        serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
        "exhausted all delivery retries",
        MAX_RETRIES + 1,
    );
}

// ── Signature algorithms (#149) ───────────────────────────────────────────────

/// HMAC algorithm used for webhook payload signing and verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SignatureAlgorithm {
    /// HMAC-SHA256 (default, most widely supported).
    #[default]
    Sha256,
    /// HMAC-SHA1 (legacy compatibility — prefer SHA-256 for new integrations).
    Sha1,
    /// HMAC-SHA512 (highest security).
    Sha512,
}

impl SignatureAlgorithm {
    /// Returns the prefix used in the `X-Heirloom-Signature` header value,
    /// e.g. `"sha256"`.
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha1 => "sha1",
            Self::Sha512 => "sha512",
        }
    }
}

// ── Multi-algorithm signing ───────────────────────────────────────────────────

/// Compute an HMAC hex-digest over `body` using the requested `algorithm`.
///
/// Returns `"<algorithm>=<hex-digest>"`, matching the format expected in the
/// `X-Heirloom-Signature` header.
pub fn sign_payload_with_algorithm(
    body: &str,
    secret: &str,
    algorithm: SignatureAlgorithm,
) -> String {
    use hmac::{Hmac, Mac};
    use sha1::Sha1 as HmacSha1Inner;
    use sha2::{Sha256, Sha512};

    let hex_digest = match algorithm {
        SignatureAlgorithm::Sha256 => {
            type H = Hmac<Sha256>;
            let mut mac = H::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
            mac.update(body.as_bytes());
            mac.finalize()
                .into_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        }
        SignatureAlgorithm::Sha1 => {
            type H = Hmac<HmacSha1Inner>;
            let mut mac = H::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
            mac.update(body.as_bytes());
            mac.finalize()
                .into_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        }
        SignatureAlgorithm::Sha512 => {
            type H = Hmac<Sha512>;
            let mut mac = H::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
            mac.update(body.as_bytes());
            mac.finalize()
                .into_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        }
    };

    format!("{}={}", algorithm.prefix(), hex_digest)
}

// ── Timestamp validation (#149) ───────────────────────────────────────────────

/// Maximum age (seconds) of a webhook timestamp before it is rejected.
/// Protects against replay attacks.
pub const TIMESTAMP_TOLERANCE_SECS: i64 = 300;

/// Validates that `timestamp` is within [`TIMESTAMP_TOLERANCE_SECS`] of now.
///
/// Returns `Ok(())` when the timestamp is acceptable.
/// Returns `Err(reason)` when it is absent, unparseable, or too old/future.
pub fn validate_timestamp(timestamp: Option<&str>) -> Result<(), String> {
    let ts_str = timestamp.ok_or_else(|| "missing X-Heirloom-Timestamp header".to_string())?;

    let ts_secs: i64 = ts_str
        .parse()
        .map_err(|_| format!("unparseable timestamp: {ts_str}"))?;

    let now_secs = Utc::now().timestamp();
    let diff = (now_secs - ts_secs).abs();

    if diff > TIMESTAMP_TOLERANCE_SECS {
        return Err(format!(
            "timestamp out of tolerance: |now({now_secs}) - ts({ts_secs})| = {diff}s > {TIMESTAMP_TOLERANCE_SECS}s"
        ));
    }

    Ok(())
}

// ── Signature verification (#149) ────────────────────────────────────────────

/// The result of verifying an incoming webhook request.
#[derive(Debug, Serialize)]
pub struct VerificationResult {
    pub valid: bool,
    /// Algorithm detected from the `X-Heirloom-Signature` header prefix.
    pub algorithm: Option<String>,
    /// Reason for failure when `valid` is false.
    pub reason: Option<String>,
}

/// Verify an incoming webhook request's signature and timestamp.
///
/// # Parameters
/// - `body`: the raw request body bytes as a UTF-8 string.
/// - `secret`: the shared HMAC secret configured for this webhook.
/// - `signature_header`: value of the `X-Heirloom-Signature` header
///   (format: `"<algorithm>=<hex-digest>"`).
/// - `timestamp_header`: optional value of the `X-Heirloom-Timestamp` header
///   (Unix seconds as a decimal string).
///
/// The function performs constant-time comparison to prevent timing attacks.
pub fn verify_webhook_signature(
    body: &str,
    secret: &str,
    signature_header: Option<&str>,
    timestamp_header: Option<&str>,
) -> VerificationResult {
    // 1. Validate timestamp first (cheapest check).
    if let Err(reason) = validate_timestamp(timestamp_header) {
        return VerificationResult {
            valid: false,
            algorithm: None,
            reason: Some(reason),
        };
    }

    // 2. Parse the signature header.
    let sig_value = match signature_header {
        Some(v) => v,
        None => {
            return VerificationResult {
                valid: false,
                algorithm: None,
                reason: Some("missing X-Heirloom-Signature header".into()),
            }
        }
    };

    let (prefix, received_hex) = match sig_value.split_once('=') {
        Some(parts) => parts,
        None => {
            return VerificationResult {
                valid: false,
                algorithm: None,
                reason: Some("malformed signature header (expected '<alg>=<hex>')".into()),
            }
        }
    };

    let algorithm = match prefix {
        "sha256" => SignatureAlgorithm::Sha256,
        "sha1" => SignatureAlgorithm::Sha1,
        "sha512" => SignatureAlgorithm::Sha512,
        other => {
            return VerificationResult {
                valid: false,
                algorithm: Some(other.to_string()),
                reason: Some(format!("unsupported algorithm: {other}")),
            }
        }
    };

    // 3. Compute expected signature.
    let expected = sign_payload_with_algorithm(body, secret, algorithm);
    let expected_hex = expected.split_once('=').map(|(_, h)| h).unwrap_or("");

    // 4. Constant-time comparison.
    let valid = constant_time_eq(received_hex.as_bytes(), expected_hex.as_bytes());

    VerificationResult {
        valid,
        algorithm: Some(prefix.to_string()),
        reason: if valid {
            None
        } else {
            Some("signature mismatch".into())
        },
    }
}

/// Constant-time byte-slice comparison (prevents timing side-channels).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ── Verification request body ─────────────────────────────────────────────────

/// Request body for the `POST /webhooks/verify` endpoint.
#[derive(Debug, Deserialize)]
pub struct VerifyWebhookRequest {
    /// Raw payload body that was received.
    pub body: String,
    /// Shared secret to verify against.
    pub secret: String,
    /// Value of the `X-Heirloom-Signature` header.
    pub signature: String,
    /// Value of the `X-Heirloom-Timestamp` header (Unix seconds).
    pub timestamp: Option<String>,
}

/// `POST /webhooks/verify` — verify a received webhook signature.
///
/// Consumers can call this endpoint to check whether an incoming webhook
/// request is authentic before processing it.
pub async fn verify_webhook(
    Json(body): Json<VerifyWebhookRequest>,
) -> (StatusCode, Json<VerificationResult>) {
    let result = verify_webhook_signature(
        &body.body,
        &body.secret,
        Some(&body.signature),
        body.timestamp.as_deref(),
    );

    let status = if result.valid {
        StatusCode::OK
    } else {
        StatusCode::UNAUTHORIZED
    };

    (status, Json(result))
}

/// Build the `X-Heirloom-Signature` and `X-Heirloom-Timestamp` headers for a
/// webhook delivery.  Call this from `attempt_delivery` when a secret is set.
pub fn build_signature_headers(
    body: &str,
    secret: &str,
    algorithm: SignatureAlgorithm,
) -> (String, String) {
    let signature = sign_payload_with_algorithm(body, secret, algorithm);
    let timestamp = Utc::now().timestamp().to_string();
    (signature, timestamp)
}

// ── Tests (#149) ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ts_now() -> String {
        Utc::now().timestamp().to_string()
    }

    #[test]
    fn test_sign_and_verify_sha256() {
        let body = r#"{"event":"vault_created"}"#;
        let secret = "test-secret";
        let sig = sign_payload_with_algorithm(body, secret, SignatureAlgorithm::Sha256);
        let ts = ts_now();

        let result = verify_webhook_signature(body, secret, Some(&sig), Some(&ts));
        assert!(result.valid, "expected valid: {:?}", result.reason);
        assert_eq!(result.algorithm.as_deref(), Some("sha256"));
    }

    #[test]
    fn test_sign_and_verify_sha1() {
        let body = r#"{"event":"vault_checked_in"}"#;
        let secret = "another-secret";
        let sig = sign_payload_with_algorithm(body, secret, SignatureAlgorithm::Sha1);
        let ts = ts_now();

        let result = verify_webhook_signature(body, secret, Some(&sig), Some(&ts));
        assert!(result.valid, "expected valid: {:?}", result.reason);
        assert_eq!(result.algorithm.as_deref(), Some("sha1"));
    }

    #[test]
    fn test_sign_and_verify_sha512() {
        let body = r#"{"event":"vault_released"}"#;
        let secret = "s3cr3t";
        let sig = sign_payload_with_algorithm(body, secret, SignatureAlgorithm::Sha512);
        let ts = ts_now();

        let result = verify_webhook_signature(body, secret, Some(&sig), Some(&ts));
        assert!(result.valid, "expected valid: {:?}", result.reason);
        assert_eq!(result.algorithm.as_deref(), Some("sha512"));
    }

    #[test]
    fn test_wrong_secret_fails() {
        let body = r#"{"event":"vault_created"}"#;
        let sig = sign_payload_with_algorithm(body, "secret-a", SignatureAlgorithm::Sha256);
        let ts = ts_now();

        let result = verify_webhook_signature(body, "secret-b", Some(&sig), Some(&ts));
        assert!(!result.valid);
        assert_eq!(result.reason.as_deref(), Some("signature mismatch"));
    }

    #[test]
    fn test_tampered_body_fails() {
        let original = r#"{"amount":100}"#;
        let tampered = r#"{"amount":999}"#;
        let secret = "key";
        let sig = sign_payload_with_algorithm(original, secret, SignatureAlgorithm::Sha256);
        let ts = ts_now();

        let result = verify_webhook_signature(tampered, secret, Some(&sig), Some(&ts));
        assert!(!result.valid);
    }

    #[test]
    fn test_missing_timestamp_fails() {
        let body = "hello";
        let secret = "key";
        let sig = sign_payload_with_algorithm(body, secret, SignatureAlgorithm::Sha256);

        let result = verify_webhook_signature(body, secret, Some(&sig), None);
        assert!(!result.valid);
        assert!(result.reason.unwrap().contains("missing"));
    }

    #[test]
    fn test_stale_timestamp_fails() {
        let body = "hello";
        let secret = "key";
        let sig = sign_payload_with_algorithm(body, secret, SignatureAlgorithm::Sha256);
        // Timestamp from 10 minutes ago.
        let old_ts = (Utc::now().timestamp() - 600).to_string();

        let result = verify_webhook_signature(body, secret, Some(&sig), Some(&old_ts));
        assert!(!result.valid);
        assert!(result.reason.unwrap().contains("out of tolerance"));
    }

    #[test]
    fn test_missing_signature_fails() {
        let ts = ts_now();
        let result = verify_webhook_signature("body", "secret", None, Some(&ts));
        assert!(!result.valid);
        assert!(result
            .reason
            .unwrap()
            .contains("missing X-Heirloom-Signature"));
    }

    #[test]
    fn test_unsupported_algorithm_fails() {
        let ts = ts_now();
        let result = verify_webhook_signature("body", "secret", Some("md5=deadbeef"), Some(&ts));
        assert!(!result.valid);
        assert!(result.reason.unwrap().contains("unsupported algorithm"));
    }

    #[test]
    fn test_malformed_signature_header_fails() {
        let ts = ts_now();
        let result = verify_webhook_signature("body", "secret", Some("nodivider"), Some(&ts));
        assert!(!result.valid);
        assert!(result.reason.unwrap().contains("malformed"));
    }

    #[test]
    fn test_validate_timestamp_ok() {
        let ts = Utc::now().timestamp().to_string();
        assert!(validate_timestamp(Some(&ts)).is_ok());
    }

    #[test]
    fn test_validate_timestamp_future_within_tolerance() {
        // 1 minute in the future — still acceptable.
        let ts = (Utc::now().timestamp() + 60).to_string();
        assert!(validate_timestamp(Some(&ts)).is_ok());
    }

    #[test]
    fn test_validate_timestamp_too_old() {
        let ts = (Utc::now().timestamp() - 600).to_string();
        let err = validate_timestamp(Some(&ts)).unwrap_err();
        assert!(err.contains("out of tolerance"));
    }
}

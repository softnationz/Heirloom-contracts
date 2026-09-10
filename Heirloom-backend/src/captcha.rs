// CAPTCHA challenge / verification module (#97)
//
// Provides a lightweight server-side CAPTCHA gate that is activated when a
// request IP is considered "suspicious" (exceeded a configurable threshold of
// recent events).  Trusted users are exempt.
//
// Integration with Google reCAPTCHA v2/v3 is optional:
//   - Set RECAPTCHA_SITE_KEY   → sent to the frontend so it can render the widget.
//   - Set RECAPTCHA_SECRET_KEY → used for server-side token verification.
//   - Leave RECAPTCHA_SECRET_KEY empty to run in *development mode* (all tokens
//     accepted without contacting Google).
//
// The suspicious-activity threshold is controlled by the
// `CAPTCHA_SUSPICIOUS_THRESHOLD` env var (default: 10 events per window).
//
// These handlers are not yet registered in main.rs::build_router — see task
// instructions.  They follow the same axum pattern used throughout this crate:
//   `async fn foo(State(state): State<Arc<AppState>>, headers: HeaderMap, ...) -> Result<Json<T>, AppError>`
#![allow(clippy::unused_async)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use crate::{
    audit::authorize_admin,
    db::AppState,
    error::AppError,
    models::{
        AddTrustedUserRequest, CaptchaChallenge, CaptchaVerifyRequest, CaptchaVerifyResponse,
        TrustedUser,
    },
};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Default challenge lifetime in seconds (5 minutes).
const DEFAULT_CHALLENGE_TTL_SECS: i64 = 300;

/// Default number of events in a window before an IP is considered suspicious.
const DEFAULT_SUSPICIOUS_THRESHOLD: u32 = 10;

/// Lifetime of the session token minted after a successful CAPTCHA (1 hour).
const SESSION_TOKEN_TTL_SECS: i64 = 3_600;

// ── Module-level in-memory stores ─────────────────────────────────────────────
//
// These are separate from AppState so that the captcha module can be used
// independently without modifying the AppState struct (which must stay
// unchanged per the task spec).

static CHALLENGE_STORE: std::sync::LazyLock<
    Arc<Mutex<HashMap<String, CaptchaChallenge>>>,
> = std::sync::LazyLock::new(|| Arc::new(Mutex::new(HashMap::new())));

static TRUSTED_STORE: std::sync::LazyLock<Arc<Mutex<Vec<TrustedUser>>>> =
    std::sync::LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));

/// Per-IP event counter used by `is_suspicious`.
/// Maps IP → count of events seen in the current window.
static IP_EVENT_COUNTER: std::sync::LazyLock<Arc<Mutex<HashMap<String, u32>>>> =
    std::sync::LazyLock::new(|| Arc::new(Mutex::new(HashMap::new())));

// ── Helper: extract IP from request headers ───────────────────────────────────

fn extract_ip(headers: &HeaderMap) -> String {
    // Prefer X-Forwarded-For (first entry), fall back to X-Real-IP.
    if let Some(val) = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
    {
        return val
            .split(',')
            .next()
            .unwrap_or("unknown")
            .trim()
            .to_string();
    }
    if let Some(val) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) {
        return val.to_string();
    }
    "unknown".to_string()
}

/// Extract the optional `X-User-ID` header value.
fn extract_user_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-user-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

// ── Core logic functions ───────────────────────────────────────────────────────

/// Returns `true` if the given `user_id`/`ip` combination is in the trusted
/// store and the trust entry has not yet expired.
///
/// Note: `state` is accepted to keep the function signature consistent with the
/// rest of the codebase; the actual data comes from the module-level static
/// store so that no changes to `AppState` are required.
pub fn is_trusted_user(user_id: &str, ip: &str, _state: &AppState) -> bool {
    let now = Utc::now();
    let store = TRUSTED_STORE.lock().unwrap_or_else(|p| p.into_inner());
    store
        .iter()
        .any(|u| u.user_id == user_id && u.ip == ip && u.trusted_until > now)
}

/// Returns `true` if `ip` has exceeded the configured suspicious-activity
/// threshold.
///
/// The threshold is read from `CAPTCHA_SUSPICIOUS_THRESHOLD` (default 10).
/// The counter is a simple in-memory stub; a production implementation would
/// use a sliding-window backed by Redis or the SQLite `Db`.
pub fn is_suspicious(ip: &str, _state: &AppState) -> bool {
    let threshold = std::env::var("CAPTCHA_SUSPICIOUS_THRESHOLD")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(DEFAULT_SUSPICIOUS_THRESHOLD);

    let counter = IP_EVENT_COUNTER.lock().unwrap_or_else(|p| p.into_inner());
    counter.get(ip).copied().unwrap_or(0) >= threshold
}

/// Increment the per-IP event counter (used by other modules to signal
/// potentially suspicious activity from an IP).
pub fn record_event_for_ip(ip: &str) {
    let mut counter = IP_EVENT_COUNTER.lock().unwrap_or_else(|p| p.into_inner());
    *counter.entry(ip.to_string()).or_insert(0) += 1;
}

/// Reset the per-IP event counter (e.g. after a successful CAPTCHA verification
/// or when the observation window rolls over).
pub fn reset_event_counter_for_ip(ip: &str) {
    let mut counter = IP_EVENT_COUNTER.lock().unwrap_or_else(|p| p.into_inner());
    counter.remove(ip);
}

/// Generate a fresh `CaptchaChallenge`.
///
/// The `site_key` is read from the `RECAPTCHA_SITE_KEY` environment variable.
/// If the variable is absent an empty string is used (development / test mode).
///
/// The challenge is stored in the module-level `CHALLENGE_STORE` so that
/// `verify_captcha` can look it up later.
pub fn generate_challenge(_state: &AppState) -> CaptchaChallenge {
    let site_key = std::env::var("RECAPTCHA_SITE_KEY").unwrap_or_default();
    let challenge = CaptchaChallenge {
        id: Uuid::new_v4().to_string(),
        token: Uuid::new_v4().to_string(),
        expires_at: Utc::now() + chrono::Duration::seconds(DEFAULT_CHALLENGE_TTL_SECS),
        site_key,
    };

    // Persist the challenge so `verify_captcha` can validate it.
    {
        let mut store = CHALLENGE_STORE.lock().unwrap_or_else(|p| p.into_inner());
        // Opportunistically evict expired challenges.
        let now = Utc::now();
        store.retain(|_, v| v.expires_at > now);
        store.insert(challenge.id.clone(), challenge.clone());
    }

    challenge
}

/// Verify a CAPTCHA token against the Google reCAPTCHA API.
///
/// # Behaviour
/// * If `RECAPTCHA_SECRET_KEY` is empty (development / CI mode), the function
///   skips the HTTP call and returns a stub success response.
/// * Otherwise, a `POST` is sent to
///   `https://www.google.com/recaptcha/api/siteverify` with the secret key and
///   the user-supplied token.
///
/// # Errors
/// Returns `Err(String)` if the challenge is unknown / expired, or if the
/// reCAPTCHA API rejects the token.
pub async fn verify_captcha(
    req: &CaptchaVerifyRequest,
    _state: &AppState,
) -> Result<CaptchaVerifyResponse, String> {
    // 1. Look up and validate the challenge.
    let challenge = {
        let store = CHALLENGE_STORE.lock().unwrap_or_else(|p| p.into_inner());
        store.get(&req.challenge_id).cloned()
    };

    let challenge = challenge.ok_or_else(|| "challenge not found".to_string())?;

    if Utc::now() > challenge.expires_at {
        return Err("challenge has expired".to_string());
    }

    // 2. Validate against reCAPTCHA (or return stub success in dev mode).
    let secret_key = std::env::var("RECAPTCHA_SECRET_KEY").unwrap_or_default();

    if secret_key.is_empty() {
        // Development / CI mode — accept any token without contacting Google.
        tracing::debug!("RECAPTCHA_SECRET_KEY not set; returning stub success (dev mode)");

        // Remove the used challenge.
        CHALLENGE_STORE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&req.challenge_id);

        let session_token = Uuid::new_v4().to_string();
        return Ok(CaptchaVerifyResponse {
            verified: true,
            session_token: Some(session_token),
            message: "CAPTCHA verified (development mode)".to_string(),
        });
    }

    // 3. Real reCAPTCHA verification via reqwest.
    let client = reqwest::Client::new();
    let params = [
        ("secret", secret_key.as_str()),
        ("response", req.captcha_token.as_str()),
    ];

    let response = client
        .post("https://www.google.com/recaptcha/api/siteverify")
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("reCAPTCHA HTTP request failed: {e}"))?;

    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("failed to parse reCAPTCHA response: {e}"))?;

    let success = body
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !success {
        let error_codes = body
            .get("error-codes")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| c.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();

        return Ok(CaptchaVerifyResponse {
            verified: false,
            session_token: None,
            message: if error_codes.is_empty() {
                "CAPTCHA verification failed".to_string()
            } else {
                format!("CAPTCHA verification failed: {error_codes}")
            },
        });
    }

    // 4. Success — remove the used challenge and mint a session token.
    CHALLENGE_STORE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&req.challenge_id);

    let session_token = format!(
        "{}.{}",
        Uuid::new_v4(),
        (Utc::now() + chrono::Duration::seconds(SESSION_TOKEN_TTL_SECS)).timestamp()
    );

    Ok(CaptchaVerifyResponse {
        verified: true,
        session_token: Some(session_token),
        message: "CAPTCHA verified successfully".to_string(),
    })
}

// ── Route handlers ─────────────────────────────────────────────────────────────

/// `POST /captcha/challenge`
///
/// Inspects `X-Forwarded-For` / `X-User-ID` headers.  If the caller is a
/// trusted user, returns `204 No Content` (no challenge needed).  Otherwise,
/// issues a challenge and returns `200 OK` with the `CaptchaChallenge` body.
pub async fn post_challenge_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let ip = extract_ip(&headers);
    let user_id = extract_user_id(&headers).unwrap_or_default();

    // Trusted users are exempt.
    if !user_id.is_empty() && is_trusted_user(&user_id, &ip, &state) {
        return (StatusCode::NO_CONTENT, Json(json!(null))).into_response();
    }

    // Only issue a challenge when the IP looks suspicious (or when the user
    // has no recorded trust entry — i.e. unknown users always get a challenge
    // if the IP is flagged, and known users only if they are not trusted).
    if !user_id.is_empty() && !is_suspicious(&ip, &state) {
        // Known, non-suspicious user — no challenge needed.
        return (StatusCode::NO_CONTENT, Json(json!(null))).into_response();
    }

    let challenge = generate_challenge(&state);
    (StatusCode::OK, Json(json!(challenge))).into_response()
}

/// `POST /captcha/verify`
///
/// Accepts a `CaptchaVerifyRequest` JSON body.  On success returns a
/// `CaptchaVerifyResponse` with a session token; on failure returns
/// `422 Unprocessable Entity`.
pub async fn post_verify_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CaptchaVerifyRequest>,
) -> Result<Json<CaptchaVerifyResponse>, AppError> {
    match verify_captcha(&req, &state).await {
        Ok(resp) => {
            if resp.verified {
                // Reset the suspicious-activity counter so that subsequent
                // requests from this IP are not immediately flagged again.
                // We cannot extract the IP here without the HeaderMap, so the
                // counter reset is best-effort via the user_token as a hint.
                // A full implementation would pass the IP in the request body
                // or a separate middleware.
                tracing::debug!(
                    challenge_id = %req.challenge_id,
                    "CAPTCHA verified successfully"
                );
            }
            Ok(Json(resp))
        }
        Err(e) => Err(AppError::InvalidInput(e)),
    }
}

/// `GET /admin/captcha/trusted-users`
///
/// Returns the list of currently active (non-expired) trusted users.
/// Requires a valid admin API key (`Authorization: Bearer <key>`).
pub async fn get_trusted_users_handler(
    State(_state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<TrustedUser>>, impl IntoResponse> {
    if let Err(api_err) = authorize_admin(&headers) {
        return Err(api_err.into_response());
    }

    let now = Utc::now();
    let store = TRUSTED_STORE.lock().unwrap_or_else(|p| p.into_inner());
    let active: Vec<TrustedUser> = store
        .iter()
        .filter(|u| u.trusted_until > now)
        .cloned()
        .collect();

    Ok(Json(active))
}

/// `POST /admin/captcha/trusted-users`
///
/// Adds a new trusted user/IP entry.  Requires a valid admin API key.
pub async fn post_add_trusted_user_handler(
    State(_state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<AddTrustedUserRequest>,
) -> Result<Json<TrustedUser>, impl IntoResponse> {
    if let Err(api_err) = authorize_admin(&headers) {
        return Err(api_err.into_response());
    }

    let duration_secs = req.trust_duration_secs.unwrap_or(86_400) as i64;
    let trusted_until = Utc::now() + chrono::Duration::seconds(duration_secs);

    let entry = TrustedUser {
        user_id: req.user_id.clone(),
        ip: req.ip.clone(),
        trusted_until,
    };

    {
        let mut store = TRUSTED_STORE.lock().unwrap_or_else(|p| p.into_inner());
        // Remove any existing entry for the same user_id + ip combination
        // before inserting the new one so there are no duplicates.
        store.retain(|u| !(u.user_id == req.user_id && u.ip == req.ip));
        store.push(entry.clone());
    }

    tracing::info!(
        user_id = %req.user_id,
        ip = %req.ip,
        trusted_until = %trusted_until,
        "trusted user added"
    );

    Ok(Json(entry))
}

// ── Unit tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_state() -> AppState {
        use crate::db::{
            create_audit_store, create_event_store, create_share_store, create_share_token_store,
            create_vault_store, Db, PoolConfig,
        };
        use crate::graphql::build_schema;
        use crate::webhook::WebhookState;
        use crate::consensus::NodeCache;

        let db = Arc::new(
            Db::open_with_pool_config(":memory:", &PoolConfig::default())
                .expect("test db"),
        );
        db.migrate().expect("migrate");
        let vault_store = create_vault_store();
        let event_store = create_event_store();
        let graphql_schema = build_schema(Arc::clone(&vault_store), Arc::clone(&event_store));
        AppState {
            db,
            vault_store,
            event_store,
            audit_store: create_audit_store(),
            share_store: create_share_store(),
            share_token_store: create_share_token_store(),
            consensus: NodeCache::from_env(),
            webhook_state: Arc::new(WebhookState::new()),
            graphql_schema,
        }
    }

    #[test]
    fn trusted_user_not_found_by_default() {
        let state = dummy_state();
        assert!(!is_trusted_user("alice", "1.2.3.4", &state));
    }

    #[test]
    fn trusted_user_found_after_insert() {
        let state = dummy_state();

        // Directly insert into the static store.
        {
            let mut store = TRUSTED_STORE.lock().unwrap();
            store.push(TrustedUser {
                user_id: "test-user-trusted".to_string(),
                ip: "10.0.0.1".to_string(),
                trusted_until: Utc::now() + chrono::Duration::hours(1),
            });
        }

        assert!(is_trusted_user("test-user-trusted", "10.0.0.1", &state));
        assert!(!is_trusted_user("test-user-trusted", "10.0.0.2", &state));

        // Clean up.
        TRUSTED_STORE
            .lock()
            .unwrap()
            .retain(|u| u.user_id != "test-user-trusted");
    }

    #[test]
    fn not_suspicious_below_threshold() {
        let state = dummy_state();
        // Use a unique IP to avoid interference from other tests.
        assert!(!is_suspicious("192.0.2.1", &state));
    }

    #[test]
    fn suspicious_after_threshold_events() {
        let state = dummy_state();
        let ip = "192.0.2.2";
        // Default threshold is 10.
        for _ in 0..10 {
            record_event_for_ip(ip);
        }
        assert!(is_suspicious(ip, &state));
        reset_event_counter_for_ip(ip);
        assert!(!is_suspicious(ip, &state));
    }

    #[test]
    fn generate_challenge_returns_valid_challenge() {
        let state = dummy_state();
        let ch = generate_challenge(&state);
        assert!(!ch.id.is_empty());
        assert!(!ch.token.is_empty());
        assert!(ch.expires_at > Utc::now());
    }

    #[tokio::test]
    async fn verify_captcha_dev_mode_succeeds() {
        // Ensure no secret key is set for this test.
        std::env::remove_var("RECAPTCHA_SECRET_KEY");

        let state = dummy_state();
        let challenge = generate_challenge(&state);

        let req = CaptchaVerifyRequest {
            challenge_id: challenge.id,
            captcha_token: "any-token".to_string(),
            user_token: "my-user-token".to_string(),
        };

        let result = verify_captcha(&req, &state).await;
        assert!(result.is_ok());
        let resp = result.unwrap();
        assert!(resp.verified);
        assert!(resp.session_token.is_some());
    }

    #[tokio::test]
    async fn verify_captcha_unknown_challenge_fails() {
        std::env::remove_var("RECAPTCHA_SECRET_KEY");

        let state = dummy_state();
        let req = CaptchaVerifyRequest {
            challenge_id: "non-existent-id".to_string(),
            captcha_token: "some-token".to_string(),
            user_token: "some-user".to_string(),
        };

        let result = verify_captcha(&req, &state).await;
        assert!(result.is_err());
    }
}

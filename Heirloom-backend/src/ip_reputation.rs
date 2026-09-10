//! IP Reputation Checking — task #96
//!
//! Provides:
//! - In-memory stores for reputation scores and block rules (via `once_cell::sync::Lazy`)
//! - `check_ip_reputation` — optionally calls the AbuseIPDB v2 API
//! - `is_ip_blocked` — tests an IP against local block rules
//! - Six axum route handlers wired via `build_router` in `main.rs`

#![allow(clippy::unused_async)]

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::Utc;
use once_cell::sync::Lazy;
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    audit::authorize_admin,
    db::AppState,
    error::AppError,
    models::{
        IpBlockRequest, IpBlockRule, IpReputationCheckRequest, IpReputationConfig,
        IpReputationScore, RiskLevel,
    },
};

// ── Global in-memory stores ──────────────────────────────────────────────────

/// Keyed by IP address string.
pub type IpReputationStore = Arc<Mutex<HashMap<String, IpReputationScore>>>;

/// Ordered list of active block rules.
pub type IpBlockRuleStore = Arc<Mutex<Vec<IpBlockRule>>>;

/// Global reputation score cache.
pub static IP_REPUTATION_STORE: Lazy<IpReputationStore> =
    Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

/// Global block rules list.
pub static IP_BLOCK_RULES: Lazy<IpBlockRuleStore> =
    Lazy::new(|| Arc::new(Mutex::new(Vec::new())));

/// Global configuration for the IP reputation subsystem.
pub static IP_REPUTATION_CONFIG: Lazy<Arc<Mutex<IpReputationConfig>>> =
    Lazy::new(|| Arc::new(Mutex::new(IpReputationConfig::default())));

// ── Core logic ───────────────────────────────────────────────────────────────

/// Derive a `RiskLevel` from a numeric abuse confidence score (0–100).
fn score_to_risk(score: f64) -> RiskLevel {
    match score as u64 {
        0..=24 => RiskLevel::Low,
        25..=49 => RiskLevel::Medium,
        50..=74 => RiskLevel::High,
        _ => RiskLevel::Critical,
    }
}

/// Check the reputation of an IP address.
///
/// - If `ABUSEIPDB_API_KEY` is set in the environment, calls the AbuseIPDB v2
///   API and returns the real abuse confidence score.
/// - Otherwise returns a stub score of `0.0` (`RiskLevel::Low`) so the
///   system remains functional without third-party credentials.
pub fn check_ip_reputation(ip: &str, config: &IpReputationConfig) -> IpReputationScore {
    if !config.check_enabled {
        return stub_score(ip, "disabled");
    }

    let api_key = match std::env::var("ABUSEIPDB_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => return stub_score(ip, "stub"),
    };

    // Synchronous HTTP call via reqwest blocking client.
    let client = reqwest::blocking::Client::new();
    let url = "https://api.abuseipdb.com/api/v2/check";

    match client
        .get(url)
        .header("Key", &api_key)
        .header("Accept", "application/json")
        .query(&[("ipAddress", ip), ("maxAgeInDays", "90")])
        .send()
    {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                tracing::warn!(ip, %status, "AbuseIPDB returned non-2xx response");
                return stub_score(ip, "abuseipdb-error");
            }
            match resp.json::<serde_json::Value>() {
                Ok(body) => {
                    let data = body.get("data").cloned().unwrap_or(serde_json::Value::Null);
                    let confidence = data
                        .get("abuseConfidenceScore")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let risk = score_to_risk(confidence);
                    IpReputationScore {
                        ip: ip.to_string(),
                        score: confidence,
                        risk_level: risk,
                        is_blocked: false,
                        last_checked: Utc::now(),
                        source: "abuseipdb".to_string(),
                        details: data,
                    }
                }
                Err(e) => {
                    tracing::error!(ip, error = %e, "Failed to parse AbuseIPDB response");
                    stub_score(ip, "abuseipdb-parse-error")
                }
            }
        }
        Err(e) => {
            tracing::error!(ip, error = %e, "AbuseIPDB request failed");
            stub_score(ip, "abuseipdb-request-error")
        }
    }
}

/// Build a zero-risk stub score used when no API key is configured or the
/// subsystem is disabled.
fn stub_score(ip: &str, source: &str) -> IpReputationScore {
    IpReputationScore {
        ip: ip.to_string(),
        score: 0.0,
        risk_level: RiskLevel::Low,
        is_blocked: false,
        last_checked: Utc::now(),
        source: source.to_string(),
        details: serde_json::json!({ "note": "stub — no API key configured" }),
    }
}

/// Return `true` if `ip` matches any active (non-expired) block rule.
///
/// Matching strategy:
/// 1. **Exact match**: the rule's `ip_pattern` equals `ip`.
/// 2. **/24 prefix match**: if `ip_pattern` contains exactly two dots it is
///    treated as a `a.b.c` prefix and the rule matches any IP whose first three
///    octets are identical (e.g. pattern `192.168.1` matches `192.168.1.42`).
pub fn is_ip_blocked(ip: &str, rules: &[IpBlockRule]) -> bool {
    let now = Utc::now();
    for rule in rules {
        // Skip expired rules.
        if let Some(expires_at) = rule.expires_at {
            if expires_at <= now {
                continue;
            }
        }

        // Exact match.
        if rule.ip_pattern == ip {
            return true;
        }

        // /24 subnet prefix match (pattern has the form "a.b.c").
        let dot_count = rule.ip_pattern.chars().filter(|&c| c == '.').count();
        if dot_count == 2 {
            let prefix = format!("{}.", rule.ip_pattern);
            if ip.starts_with(&prefix) {
                return true;
            }
        }
    }
    false
}

// ── Query parameter structs ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct IpQuery {
    pub ip: String,
}

// ── Route handlers ───────────────────────────────────────────────────────────

/// GET /admin/ip-reputation?ip=x.x.x.x
///
/// Look up (or refresh) the reputation score for the given IP address.
/// The result is cached in `IP_REPUTATION_STORE`; if a cached entry
/// already exists it is refreshed on each admin request to stay current.
pub async fn get_ip_reputation_handler(
    State(_state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<IpQuery>,
) -> Result<Json<IpReputationScore>, AppError> {
    authorize_admin(&headers).map_err(|_| AppError::InvalidInput("unauthorized".into()))?;

    let ip = params.ip.trim().to_string();
    if ip.is_empty() {
        return Err(AppError::InvalidInput("ip query parameter is required".into()));
    }

    let config = IP_REPUTATION_CONFIG.lock().unwrap().clone();
    let mut score = check_ip_reputation(&ip, &config);

    // Annotate with current block status.
    {
        let rules = IP_BLOCK_RULES.lock().unwrap();
        score.is_blocked = is_ip_blocked(&ip, &rules);
    }

    // Cache the refreshed score.
    {
        let mut store = IP_REPUTATION_STORE.lock().unwrap();
        store.insert(ip.clone(), score.clone());
    }

    Ok(Json(score))
}

/// POST /admin/ip-reputation/block
///
/// Add a new block rule for an IP address or /24 subnet prefix.
pub async fn post_block_ip_handler(
    State(_state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<IpBlockRequest>,
) -> Result<(StatusCode, Json<IpBlockRule>), AppError> {
    authorize_admin(&headers).map_err(|_| AppError::InvalidInput("unauthorized".into()))?;

    let pattern = body.ip_pattern.trim().to_string();
    if pattern.is_empty() {
        return Err(AppError::InvalidInput("ip_pattern must not be empty".into()));
    }
    if body.reason.trim().is_empty() {
        return Err(AppError::InvalidInput("reason must not be empty".into()));
    }

    let now = Utc::now();
    let expires_at = body.expires_in_hours.map(|h| now + chrono::Duration::hours(h as i64));

    // Extract the caller identity from the Authorization header (best effort).
    let created_by = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| format!("key:{}", &t[..t.len().min(8)]))
        .unwrap_or_else(|| "admin".to_string());

    let rule = IpBlockRule {
        id: Uuid::new_v4().to_string(),
        ip_pattern: pattern,
        reason: body.reason,
        created_at: now,
        expires_at,
        created_by,
    };

    {
        let mut rules = IP_BLOCK_RULES.lock().unwrap();
        rules.push(rule.clone());
    }

    // Invalidate cached score for the exact IP so it reflects the new block.
    {
        let mut store = IP_REPUTATION_STORE.lock().unwrap();
        if let Some(entry) = store.get_mut(&rule.ip_pattern) {
            entry.is_blocked = true;
        }
    }

    Ok((StatusCode::CREATED, Json(rule)))
}

/// GET /admin/ip-reputation/rules
///
/// List all current block rules (including expired ones for audit purposes).
pub async fn get_block_rules_handler(
    State(_state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<IpBlockRule>>, AppError> {
    authorize_admin(&headers).map_err(|_| AppError::InvalidInput("unauthorized".into()))?;

    let rules = IP_BLOCK_RULES.lock().unwrap().clone();
    Ok(Json(rules))
}

/// DELETE /admin/ip-reputation/rules/:id
///
/// Remove a block rule by its UUID. Returns 404 if the rule does not exist.
pub async fn delete_block_rule_handler(
    State(_state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    authorize_admin(&headers).map_err(|_| AppError::InvalidInput("unauthorized".into()))?;

    let mut rules = IP_BLOCK_RULES.lock().unwrap();
    let original_len = rules.len();
    rules.retain(|r| r.id != id);

    if rules.len() == original_len {
        return Err(AppError::NotFound);
    }

    Ok(StatusCode::NO_CONTENT)
}

/// GET /admin/ip-reputation/config
///
/// Return the current IP reputation configuration.
pub async fn get_reputation_config_handler(
    State(_state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<IpReputationConfig>, AppError> {
    authorize_admin(&headers).map_err(|_| AppError::InvalidInput("unauthorized".into()))?;

    let config = IP_REPUTATION_CONFIG.lock().unwrap().clone();
    Ok(Json(config))
}

/// POST /ip-reputation/check
///
/// Public endpoint that lets callers check whether their own IP (or an IP they
/// provide in the request body) is flagged by the reputation system.
///
/// IP resolution order:
/// 1. `ip` field in the JSON body (if present and non-empty)
/// 2. `X-Forwarded-For` header (first address in the list)
/// 3. `X-Real-IP` header
/// 4. Falls back to `"unknown"` if none of the above are available.
pub async fn post_check_handler(
    State(_state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<IpReputationCheckRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Resolve the IP to check.
    let ip = resolve_ip(&headers, body.ip.as_deref());

    if ip == "unknown" {
        return Err(AppError::InvalidInput(
            "Could not determine IP address. Provide it in the request body or via X-Forwarded-For header.".into(),
        ));
    }

    // Check against local block rules first (fast path).
    let is_blocked = {
        let rules = IP_BLOCK_RULES.lock().unwrap();
        is_ip_blocked(&ip, &rules)
    };

    // Return a lightweight public response — don't leak the full score details.
    Ok(Json(serde_json::json!({
        "ip": ip,
        "is_blocked": is_blocked,
        "message": if is_blocked {
            "This IP address is currently blocked."
        } else {
            "This IP address is not blocked."
        },
        "checked_at": Utc::now().to_rfc3339(),
    })))
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Resolve the IP address to use for a reputation check.
///
/// Priority: explicit body value → X-Forwarded-For → X-Real-IP → "unknown".
fn resolve_ip(headers: &HeaderMap, body_ip: Option<&str>) -> String {
    if let Some(ip) = body_ip {
        let trimmed = ip.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    if let Some(forwarded) = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
    {
        let first = forwarded.split(',').next().unwrap_or("").trim();
        if !first.is_empty() {
            return first.to_string();
        }
    }

    if let Some(real_ip) = headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
    {
        let trimmed = real_ip.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    "unknown".to_string()
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::IpBlockRule;

    fn make_rule(pattern: &str, expires_at: Option<chrono::DateTime<Utc>>) -> IpBlockRule {
        IpBlockRule {
            id: Uuid::new_v4().to_string(),
            ip_pattern: pattern.to_string(),
            reason: "test".to_string(),
            created_at: Utc::now(),
            expires_at,
            created_by: "test".to_string(),
        }
    }

    #[test]
    fn exact_match_blocks() {
        let rules = vec![make_rule("1.2.3.4", None)];
        assert!(is_ip_blocked("1.2.3.4", &rules));
        assert!(!is_ip_blocked("1.2.3.5", &rules));
    }

    #[test]
    fn subnet_prefix_blocks() {
        let rules = vec![make_rule("10.0.0", None)];
        assert!(is_ip_blocked("10.0.0.1", &rules));
        assert!(is_ip_blocked("10.0.0.255", &rules));
        assert!(!is_ip_blocked("10.0.1.1", &rules));
    }

    #[test]
    fn expired_rule_does_not_block() {
        let past = Utc::now() - chrono::Duration::hours(1);
        let rules = vec![make_rule("5.5.5.5", Some(past))];
        assert!(!is_ip_blocked("5.5.5.5", &rules));
    }

    #[test]
    fn future_expiry_still_blocks() {
        let future = Utc::now() + chrono::Duration::hours(1);
        let rules = vec![make_rule("6.6.6.6", Some(future))];
        assert!(is_ip_blocked("6.6.6.6", &rules));
    }

    #[test]
    fn score_to_risk_buckets() {
        assert_eq!(score_to_risk(0.0), RiskLevel::Low);
        assert_eq!(score_to_risk(24.9), RiskLevel::Low);
        assert_eq!(score_to_risk(25.0), RiskLevel::Medium);
        assert_eq!(score_to_risk(49.9), RiskLevel::Medium);
        assert_eq!(score_to_risk(50.0), RiskLevel::High);
        assert_eq!(score_to_risk(74.9), RiskLevel::High);
        assert_eq!(score_to_risk(75.0), RiskLevel::Critical);
        assert_eq!(score_to_risk(100.0), RiskLevel::Critical);
    }

    #[test]
    fn stub_score_returned_when_disabled() {
        let config = IpReputationConfig {
            check_enabled: false,
            ..IpReputationConfig::default()
        };
        let score = check_ip_reputation("1.2.3.4", &config);
        assert_eq!(score.source, "disabled");
        assert_eq!(score.score, 0.0);
    }

    #[test]
    fn resolve_ip_from_body() {
        let headers = HeaderMap::new();
        assert_eq!(resolve_ip(&headers, Some("7.7.7.7")), "7.7.7.7");
    }

    #[test]
    fn resolve_ip_from_forwarded_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "8.8.8.8, 9.9.9.9".parse().unwrap(),
        );
        assert_eq!(resolve_ip(&headers, None), "8.8.8.8");
    }

    #[test]
    fn resolve_ip_unknown_fallback() {
        let headers = HeaderMap::new();
        assert_eq!(resolve_ip(&headers, None), "unknown");
    }
}

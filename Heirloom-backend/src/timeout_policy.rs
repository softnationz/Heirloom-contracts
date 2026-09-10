//! Per-endpoint timeout configuration (Issue: "Timeouts are hardcoded").
//!
//! Lets timeouts be tuned per endpoint via `POST /admin/timeout-policies`
//! instead of being hardcoded constants, supports a per-request override
//! header, and emits an alert (tracing warning + counter) whenever a
//! request is aborted for exceeding its timeout budget.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

/// The header a caller may set to request a tighter or looser timeout for
/// a single request, bounded by `TimeoutState::max_override_ms`.
pub const TIMEOUT_OVERRIDE_HEADER: &str = "x-timeout-override-ms";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeoutPolicy {
    pub id: String,
    pub endpoint_pattern: String,
    pub timeout_ms: u64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTimeoutPolicyRequest {
    pub endpoint_pattern: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Default)]
struct TimeoutViolations {
    total: AtomicU64,
}

/// Registry of per-endpoint timeout policies plus a global default.
pub struct TimeoutPolicyStore {
    default_timeout_ms: u64,
    max_override_ms: u64,
    policies: Mutex<HashMap<String, TimeoutPolicy>>,
    violations: TimeoutViolations,
}

impl TimeoutPolicyStore {
    pub fn new(default_timeout_ms: u64, max_override_ms: u64) -> Self {
        Self {
            default_timeout_ms,
            max_override_ms,
            policies: Mutex::new(HashMap::new()),
            violations: TimeoutViolations::default(),
        }
    }

    pub fn upsert(&self, policy: TimeoutPolicy) -> TimeoutPolicy {
        let mut guard = self.policies.lock().unwrap();
        guard.insert(policy.id.clone(), policy.clone());
        policy
    }

    pub fn get(&self, id: &str) -> Option<TimeoutPolicy> {
        self.policies.lock().unwrap().get(id).cloned()
    }

    pub fn list(&self) -> Vec<TimeoutPolicy> {
        self.policies.lock().unwrap().values().cloned().collect()
    }

    /// Resolves the configured timeout (ms) for `path`, preferring the most
    /// specific matching policy and falling back to the global default.
    pub fn resolve_timeout_ms(&self, path: &str) -> u64 {
        self.policies
            .lock()
            .unwrap()
            .values()
            .filter(|p| path.starts_with(p.endpoint_pattern.trim_end_matches('*')))
            .max_by_key(|p| p.endpoint_pattern.len())
            .map(|p| p.timeout_ms)
            .unwrap_or(self.default_timeout_ms)
    }

    /// Applies a per-request override header, clamped to `max_override_ms`.
    pub fn apply_override(&self, resolved_ms: u64, headers: &HeaderMap) -> u64 {
        headers
            .get(TIMEOUT_OVERRIDE_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(|override_ms| override_ms.min(self.max_override_ms))
            .unwrap_or(resolved_ms)
    }

    fn record_violation(&self, path: &str, timeout_ms: u64) {
        let total = self.violations.total.fetch_add(1, Ordering::SeqCst) + 1;
        tracing::warn!(
            path,
            timeout_ms,
            total_violations = total,
            "timeout policy violated: request aborted"
        );
    }

    pub fn violation_count(&self) -> u64 {
        self.violations.total.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
pub struct TimeoutState {
    pub store: Arc<TimeoutPolicyStore>,
}

impl TimeoutState {
    pub fn new() -> Self {
        Self {
            store: Arc::new(TimeoutPolicyStore::new(30_000, 120_000)),
        }
    }
}

impl Default for TimeoutState {
    fn default() -> Self {
        Self::new()
    }
}

async fn create_timeout_policy(
    State(state): State<TimeoutState>,
    Json(body): Json<CreateTimeoutPolicyRequest>,
) -> Result<(StatusCode, Json<TimeoutPolicy>), (StatusCode, String)> {
    if body.timeout_ms == 0 {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "timeout_ms must be > 0".into(),
        ));
    }

    let policy = TimeoutPolicy {
        id: uuid::Uuid::new_v4().to_string(),
        endpoint_pattern: body.endpoint_pattern,
        timeout_ms: body.timeout_ms,
        created_at: chrono::Utc::now(),
    };
    let saved = state.store.upsert(policy);
    Ok((StatusCode::CREATED, Json(saved)))
}

async fn list_timeout_policies(State(state): State<TimeoutState>) -> Json<Vec<TimeoutPolicy>> {
    Json(state.store.list())
}

async fn get_timeout_policy(
    State(state): State<TimeoutState>,
    Path(id): Path<String>,
) -> Result<Json<TimeoutPolicy>, StatusCode> {
    state.store.get(&id).map(Json).ok_or(StatusCode::NOT_FOUND)
}

async fn timeout_violations_handler(State(state): State<TimeoutState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "total_violations": state.store.violation_count() }))
}

/// Axum middleware enforcing the resolved (policy or override) timeout for
/// every request, emitting an alert on violation.
pub async fn timeout_middleware(
    State(state): State<TimeoutState>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    let resolved_ms = state.store.resolve_timeout_ms(&path);
    let effective_ms = state.store.apply_override(resolved_ms, request.headers());

    match tokio::time::timeout(Duration::from_millis(effective_ms), next.run(request)).await {
        Ok(response) => response,
        Err(_) => {
            state.store.record_violation(&path, effective_ms);
            (
                StatusCode::GATEWAY_TIMEOUT,
                Json(serde_json::json!({
                    "code": "timeout_exceeded",
                    "message": format!("request exceeded {effective_ms}ms timeout budget for {path}"),
                })),
            )
                .into_response()
        }
    }
}

/// Builds the `/admin/timeout-policies` router with its own state; merge it
/// into the main application router.
pub fn router(state: TimeoutState) -> Router {
    Router::new()
        .route(
            "/admin/timeout-policies",
            post(create_timeout_policy).get(list_timeout_policies),
        )
        .route("/admin/timeout-policies/:id", get(get_timeout_policy))
        .route(
            "/admin/timeout-policies/violations",
            get(timeout_violations_handler),
        )
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_timeout_falls_back_to_default() {
        let store = TimeoutPolicyStore::new(30_000, 120_000);
        assert_eq!(store.resolve_timeout_ms("/api/whatever"), 30_000);
    }

    #[test]
    fn resolve_timeout_prefers_most_specific_policy() {
        let store = TimeoutPolicyStore::new(30_000, 120_000);
        store.upsert(TimeoutPolicy {
            id: "a".into(),
            endpoint_pattern: "/api".into(),
            timeout_ms: 5_000,
            created_at: chrono::Utc::now(),
        });
        store.upsert(TimeoutPolicy {
            id: "b".into(),
            endpoint_pattern: "/api/vaults".into(),
            timeout_ms: 15_000,
            created_at: chrono::Utc::now(),
        });
        assert_eq!(store.resolve_timeout_ms("/api/vaults/1"), 15_000);
    }

    #[test]
    fn override_header_is_clamped_to_max() {
        let store = TimeoutPolicyStore::new(30_000, 60_000);
        let mut headers = HeaderMap::new();
        headers.insert(TIMEOUT_OVERRIDE_HEADER, "999999".parse().unwrap());
        assert_eq!(store.apply_override(30_000, &headers), 60_000);
    }
}

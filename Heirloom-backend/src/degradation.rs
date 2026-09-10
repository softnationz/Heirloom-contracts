//! Graceful degradation for missing or unhealthy features.
//!
//! Previously, a missing or failing feature (e.g. a downstream dependency
//! being unavailable) caused hard errors for the whole request. This module
//! lets capabilities be marked degraded or unavailable independently, so
//! clients can negotiate what's actually usable and fall back to reduced
//! functionality instead of failing outright.
//!
//! Capability statuses are persisted in the SQL database and shared across all
//! instances in a load-balanced deployment. When an operator marks a capability
//! degraded via `POST /admin/capabilities`, all instances immediately observe
//! the change on subsequent reads.
//!
//! # Concepts
//!
//! - [`DegradationLevel`] — `Full`, `Degraded`, or `Unavailable` for a given
//!   named capability
//! - [`DegradationState`] — registry of capability -> status backed by SQL,
//!   defaulting to `Full` for anything not explicitly registered
//! - Capability negotiation — a client posts the capabilities it wants to
//!   use; the server reports which are fully available, degraded (usable
//!   with reduced functionality), or unavailable (client should use a
//!   fallback or skip that feature)
//!
//! # API
//!
//! - `POST /admin/capabilities` — set a capability's degradation level
//! - `GET /admin/capabilities` — list all registered capability statuses
//! - `POST /capabilities/negotiate` — negotiate a set of requested capabilities
//! - `GET /capabilities/:name/fallback` — fallback response for a capability

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// How usable a capability currently is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DegradationLevel {
    /// Fully functional.
    Full,
    /// Usable, but with reduced functionality (e.g. cached/stale data,
    /// slower path, or a subset of normal behavior).
    Degraded,
    /// Not usable at all right now; callers should use a fallback or skip it.
    Unavailable,
}

/// Current status of a named capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityStatus {
    pub name: String,
    pub level: DegradationLevel,
    pub reason: Option<String>,
    /// Whether a fallback endpoint/response exists for this capability.
    pub fallback_available: bool,
    pub updated_at: DateTime<Utc>,
}

/// Request body for `POST /admin/capabilities`.
#[derive(Debug, Deserialize)]
pub struct SetCapabilityRequest {
    pub name: String,
    pub level: DegradationLevel,
    pub reason: Option<String>,
    #[serde(default)]
    pub fallback_available: bool,
}

/// Request body for `POST /capabilities/negotiate`.
#[derive(Debug, Deserialize)]
pub struct NegotiateRequest {
    pub requested: Vec<String>,
}

/// Per-capability negotiation outcome.
#[derive(Debug, Serialize)]
pub struct NegotiatedCapability {
    pub name: String,
    pub level: DegradationLevel,
    pub reason: Option<String>,
    pub use_fallback: bool,
}

/// Result of negotiating a set of requested capabilities.
#[derive(Debug, Serialize)]
pub struct NegotiationResult {
    pub capabilities: Vec<NegotiatedCapability>,
    /// True if every requested capability is at least `Degraded` (i.e. the
    /// client can proceed in some form without hard failure).
    pub can_proceed: bool,
}

/// Database-backed registry of capability statuses.
/// All instances in a load-balanced deployment share the same storage,
/// ensuring consistent degradation state across the fleet.
#[derive(Clone)]
pub struct DegradationState {
    db: Arc<crate::db::Db>,
}

impl DegradationState {
    pub fn new(db: Arc<crate::db::Db>) -> Self {
        Self { db }
    }

    /// Register or update a capability's degradation status.
    /// This change is immediately visible to all instances reading from the shared database.
    ///
    /// Setting a capability to [`DegradationLevel::Full`] deregisters it:
    /// `check` already defaults to `Full` for unregistered capabilities, and
    /// `list` reports only capabilities that are (or were) degraded.
    pub fn set_status(
        &self,
        name: &str,
        level: DegradationLevel,
        reason: Option<String>,
        fallback_available: bool,
    ) -> Result<CapabilityStatus, String> {
        let status = CapabilityStatus {
            name: name.to_string(),
            level,
            reason,
            fallback_available,
            updated_at: Utc::now(),
        };
        if level == DegradationLevel::Full {
            self.db
                .delete_capability_status(name)
                .map_err(|e| format!("failed to clear capability status: {}", e))?;
        } else {
            self.db
                .set_capability_status(&status)
                .map_err(|e| format!("failed to set capability status: {}", e))?;
        }
        Ok(status)
    }

    /// Look up a capability's status, defaulting to `Full` if unregistered.
    /// Reads from the shared database, ensuring all instances see the same state.
    pub fn check(&self, name: &str) -> Result<CapabilityStatus, String> {
        self.db
            .get_capability_status(name)
            .map_err(|e| format!("failed to get capability status: {}", e))
    }

    pub fn list(&self) -> Result<Vec<CapabilityStatus>, String> {
        self.db
            .list_capability_statuses()
            .map_err(|e| format!("failed to list capability statuses: {}", e))
    }

    /// Negotiate a set of requested capabilities against current status.
    pub fn negotiate(&self, requested: &[String]) -> Result<NegotiationResult, String> {
        let capabilities: Result<Vec<NegotiatedCapability>, String> = requested
            .iter()
            .map(|name| {
                let status = self.check(name)?;
                Ok(NegotiatedCapability {
                    name: status.name,
                    level: status.level,
                    reason: status.reason,
                    use_fallback: status.level != DegradationLevel::Full
                        && status.fallback_available,
                })
            })
            .collect();

        let capabilities = capabilities?;
        let can_proceed = capabilities
            .iter()
            .all(|c| c.level != DegradationLevel::Unavailable || c.use_fallback);

        Ok(NegotiationResult {
            capabilities,
            can_proceed,
        })
    }
}

// ── HTTP handlers ─────────────────────────────────────────────────────────────

/// `POST /admin/capabilities` — set a capability's degradation level.
pub async fn set_capability(
    State(state): State<Arc<DegradationState>>,
    Json(body): Json<SetCapabilityRequest>,
) -> Result<Json<CapabilityStatus>, (StatusCode, Json<serde_json::Value>)> {
    if body.name.trim().is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "name must not be empty" })),
        ));
    }
    state
        .set_status(&body.name, body.level, body.reason, body.fallback_available)
        .map(Json)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e })),
            )
        })
}

/// `GET /admin/capabilities` — list all registered capability statuses.
pub async fn list_capabilities(
    State(state): State<Arc<DegradationState>>,
) -> Result<Json<Vec<CapabilityStatus>>, (StatusCode, Json<serde_json::Value>)> {
    state.list().map(Json).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e })),
        )
    })
}

/// `POST /capabilities/negotiate` — negotiate a set of requested capabilities.
pub async fn negotiate_capabilities(
    State(state): State<Arc<DegradationState>>,
    Json(body): Json<NegotiateRequest>,
) -> Result<Json<NegotiationResult>, (StatusCode, Json<serde_json::Value>)> {
    state.negotiate(&body.requested).map(Json).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e })),
        )
    })
}

/// `GET /capabilities/:name/fallback` — reduced-functionality fallback
/// response for a capability that is degraded or unavailable.
pub async fn capability_fallback(
    State(state): State<Arc<DegradationState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let status = state
        .check(&name)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if status.level == DegradationLevel::Full {
        return Err(StatusCode::NOT_FOUND);
    }
    if !status.fallback_available {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    Ok(Json(serde_json::json!({
        "capability": status.name,
        "level": status.level,
        "reason": status.reason,
        "message": "serving reduced-functionality fallback response",
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_db() -> Arc<crate::db::Db> {
        let db = Arc::new(crate::db::Db::open(":memory:").expect("failed to open in-memory db"));
        db.migrate().expect("migration failed");
        db
    }

    #[test]
    fn unregistered_capability_defaults_to_full() {
        let db = create_test_db();
        let state = DegradationState::new(db);
        let status = state.check("payments").expect("check failed");
        assert_eq!(status.level, DegradationLevel::Full);
        assert_eq!(status.name, "payments");
    }

    #[test]
    fn set_and_get_capability_status() {
        let db = create_test_db();
        let state = DegradationState::new(db);

        let set_result = state
            .set_status(
                "search",
                DegradationLevel::Unavailable,
                Some("index rebuilding".to_string()),
                true,
            )
            .expect("set_status failed");
        assert_eq!(set_result.level, DegradationLevel::Unavailable);
        assert_eq!(set_result.reason, Some("index rebuilding".to_string()));

        let retrieved = state.check("search").expect("check failed");
        assert_eq!(retrieved.level, DegradationLevel::Unavailable);
        assert_eq!(retrieved.reason, Some("index rebuilding".to_string()));
        assert!(retrieved.fallback_available);
    }

    #[test]
    fn negotiate_allows_proceeding_with_fallback() {
        let db = create_test_db();
        let state = DegradationState::new(db);

        state
            .set_status(
                "search",
                DegradationLevel::Unavailable,
                Some("index rebuilding".to_string()),
                true,
            )
            .expect("set_status failed");

        let result = state
            .negotiate(&["search".to_string()])
            .expect("negotiate failed");
        assert!(result.can_proceed);
        assert!(result.capabilities[0].use_fallback);
    }

    #[test]
    fn negotiate_blocks_without_fallback() {
        let db = create_test_db();
        let state = DegradationState::new(db);

        state
            .set_status("search", DegradationLevel::Unavailable, None, false)
            .expect("set_status failed");

        let result = state
            .negotiate(&["search".to_string()])
            .expect("negotiate failed");
        assert!(!result.can_proceed);
    }

    #[test]
    fn degraded_capability_can_proceed() {
        let db = create_test_db();
        let state = DegradationState::new(db);

        state
            .set_status(
                "recommendations",
                DegradationLevel::Degraded,
                Some("stale cache".to_string()),
                false,
            )
            .expect("set_status failed");

        let result = state
            .negotiate(&["recommendations".to_string()])
            .expect("negotiate failed");
        assert!(result.can_proceed);
    }

    #[test]
    fn two_handles_share_same_store() {
        let db = create_test_db();
        let state1 = DegradationState::new(Arc::clone(&db));
        let state2 = DegradationState::new(Arc::clone(&db));

        // Set via state1
        state1
            .set_status(
                "payments",
                DegradationLevel::Degraded,
                Some("slow processing".to_string()),
                true,
            )
            .expect("set_status failed");

        // Observe change via state2
        let status = state2.check("payments").expect("check failed");
        assert_eq!(status.level, DegradationLevel::Degraded);
        assert_eq!(status.reason, Some("slow processing".to_string()));
    }

    #[test]
    fn degradation_state_persists_across_instances() {
        let db = create_test_db();

        // Instance 1: set a capability status
        {
            let state1 = DegradationState::new(Arc::clone(&db));
            state1
                .set_status(
                    "notifications",
                    DegradationLevel::Unavailable,
                    Some("service down".to_string()),
                    false,
                )
                .expect("set_status failed");
        }

        // Instance 2: read the same state after instance 1 is dropped
        {
            let state2 = DegradationState::new(Arc::clone(&db));
            let status = state2.check("notifications").expect("check failed");
            assert_eq!(status.level, DegradationLevel::Unavailable);
            assert_eq!(status.reason, Some("service down".to_string()));
        }
    }

    #[test]
    fn list_returns_all_registered_capabilities() {
        let db = create_test_db();
        let state = DegradationState::new(db);

        state
            .set_status("search", DegradationLevel::Degraded, None, false)
            .expect("set_status failed");
        state
            .set_status(
                "recommendations",
                DegradationLevel::Unavailable,
                None,
                false,
            )
            .expect("set_status failed");
        state
            .set_status("analytics", DegradationLevel::Full, None, false)
            .expect("set_status failed");

        let list = state.list().expect("list failed");
        // Setting `analytics` back to `Full` deregisters it (check() already
        // defaults to Full for unregistered capabilities), so only the two
        // genuinely degraded capabilities remain.
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|s| s.name == "search"));
        assert!(list.iter().any(|s| s.name == "recommendations"));
        // Full means unregistered, so it never appears in the list.
        assert!(!list.iter().any(|s| s.name == "analytics"));
    }
}

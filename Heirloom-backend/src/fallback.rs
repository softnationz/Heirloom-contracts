//! Fallback chain registry for graceful degradation (reliability roadmap #1).
//!
//! Several subsystems in this backend depend on a single downstream target
//! (a webhook URL, an RPC endpoint, a notification provider). If that single
//! target is unavailable the whole operation fails even though a secondary
//! target could have served the request. This module lets operators
//! register an ordered chain of fallback targets for a named resource and
//! cascade through them until one succeeds.
//!
//! # Architecture
//!
//! ```text
//! POST /admin/fallback-chains            → register_fallback_chain
//! GET  /admin/fallback-chains             → list_fallback_chains
//! GET  /admin/fallback-chains/:id         → get_fallback_chain
//! POST /admin/fallback-chains/:id/test    → test_fallback_chain
//! Internal: execute_with_fallback()       → used by callers that want
//!                                            cascading fallback behavior
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Data types ──────────────────────────────────────────────────────────────

/// A single target within a fallback chain, in priority order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackTarget {
    pub name: String,
    /// URL or logical identifier the target resolves to.
    pub endpoint: String,
    /// Lower numbers are attempted first.
    pub priority: u32,
}

/// A named, ordered chain of fallback targets for a single resource.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackChain {
    pub id: String,
    pub name: String,
    /// The resource this chain protects (e.g. "webhook:vault-created").
    pub resource: String,
    pub targets: Vec<FallbackTarget>,
    pub created_at: DateTime<Utc>,
    pub active: bool,
}

#[derive(Debug, Deserialize)]
pub struct RegisterFallbackChainRequest {
    pub name: String,
    pub resource: String,
    pub targets: Vec<FallbackTarget>,
}

/// Outcome of attempting a single target during a cascade.
#[derive(Debug, Clone, Serialize)]
pub struct FallbackAttempt {
    pub target: String,
    pub priority: u32,
    pub succeeded: bool,
    pub error: Option<String>,
}

/// Result of cascading through a chain, either live or simulated.
#[derive(Debug, Clone, Serialize)]
pub struct FallbackExecutionResult {
    pub chain_id: String,
    pub attempts: Vec<FallbackAttempt>,
    pub resolved_target: Option<String>,
    pub degraded: bool,
}

/// Request body for `POST /admin/fallback-chains/:id/test`.
///
/// `simulate_failures` lists target names that should be treated as failing
/// so operators can validate cascade behavior without needing every
/// downstream target to actually be down.
#[derive(Debug, Deserialize, Default)]
pub struct TestFallbackChainRequest {
    #[serde(default)]
    pub simulate_failures: Vec<String>,
}

// ── In-memory store ─────────────────────────────────────────────────────────

pub type FallbackStore = Arc<Mutex<HashMap<String, FallbackChain>>>;

pub fn create_fallback_store() -> FallbackStore {
    Arc::new(Mutex::new(HashMap::new()))
}

#[derive(Clone)]
pub struct FallbackState {
    pub store: FallbackStore,
}

impl FallbackState {
    pub fn new() -> Self {
        Self {
            store: create_fallback_store(),
        }
    }
}

impl Default for FallbackState {
    fn default() -> Self {
        Self::new()
    }
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// `POST /admin/fallback-chains` — register a new fallback chain.
pub async fn register_fallback_chain(
    State(state): State<Arc<FallbackState>>,
    Json(body): Json<RegisterFallbackChainRequest>,
) -> Result<(StatusCode, Json<FallbackChain>), (StatusCode, Json<serde_json::Value>)> {
    if body.targets.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "at least one fallback target is required" })),
        ));
    }

    let mut targets = body.targets;
    targets.sort_by_key(|t| t.priority);

    let chain = FallbackChain {
        id: Uuid::new_v4().to_string(),
        name: body.name,
        resource: body.resource,
        targets,
        created_at: Utc::now(),
        active: true,
    };

    let mut store = state.store.lock().unwrap();
    store.insert(chain.id.clone(), chain.clone());

    Ok((StatusCode::CREATED, Json(chain)))
}

/// `GET /admin/fallback-chains` — list all registered fallback chains.
pub async fn list_fallback_chains(
    State(state): State<Arc<FallbackState>>,
) -> Json<Vec<FallbackChain>> {
    let store = state.store.lock().unwrap();
    Json(store.values().cloned().collect())
}

/// `GET /admin/fallback-chains/:id` — fetch a single chain.
pub async fn get_fallback_chain(
    State(state): State<Arc<FallbackState>>,
    Path(id): Path<String>,
) -> Result<Json<FallbackChain>, StatusCode> {
    let store = state.store.lock().unwrap();
    store
        .get(&id)
        .cloned()
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

/// `POST /admin/fallback-chains/:id/test` — dry-run a cascade through the
/// chain, optionally simulating failures for named targets, without
/// performing any real network calls.
pub async fn test_fallback_chain(
    State(state): State<Arc<FallbackState>>,
    Path(id): Path<String>,
    Json(body): Json<TestFallbackChainRequest>,
) -> Result<Json<FallbackExecutionResult>, StatusCode> {
    let chain = {
        let store = state.store.lock().unwrap();
        store.get(&id).cloned().ok_or(StatusCode::NOT_FOUND)?
    };

    let result = cascade(&chain, |target| {
        if body.simulate_failures.contains(&target.name) {
            Err("simulated failure".to_string())
        } else {
            Ok(())
        }
    });

    Ok(Json(result))
}

// ── Cascade execution ───────────────────────────────────────────────────────

/// Walk `chain.targets` in priority order, invoking `attempt` for each until
/// one succeeds or the chain is exhausted. `attempt` returns `Ok(())` on
/// success or `Err(reason)` on failure.
///
/// This is the core cascading-fallback primitive: callers that talk to a
/// single downstream target (webhook delivery, RPC calls, notification
/// providers) can use it to transparently retry against the next configured
/// target when the primary is unavailable.
pub fn cascade<F>(chain: &FallbackChain, mut attempt: F) -> FallbackExecutionResult
where
    F: FnMut(&FallbackTarget) -> Result<(), String>,
{
    let mut attempts = Vec::with_capacity(chain.targets.len());
    let mut resolved_target = None;

    let mut sorted_targets = chain.targets.clone();
    sorted_targets.sort_by_key(|t| t.priority);

    for target in &sorted_targets {
        match attempt(target) {
            Ok(()) => {
                attempts.push(FallbackAttempt {
                    target: target.name.clone(),
                    priority: target.priority,
                    succeeded: true,
                    error: None,
                });
                resolved_target = Some(target.endpoint.clone());
                break;
            }
            Err(reason) => {
                attempts.push(FallbackAttempt {
                    target: target.name.clone(),
                    priority: target.priority,
                    succeeded: false,
                    error: Some(reason),
                });
            }
        }
    }

    // Degraded means we didn't resolve on the first (highest-priority) target.
    let degraded = resolved_target.is_some() && attempts.first().is_some_and(|a| !a.succeeded);

    FallbackExecutionResult {
        chain_id: chain.id.clone(),
        attempts,
        resolved_target,
        degraded,
    }
}

/// Find the active chain protecting `resource`, if any.
pub fn find_chain_for_resource(state: &FallbackState, resource: &str) -> Option<FallbackChain> {
    let store = state.store.lock().unwrap();
    store
        .values()
        .find(|c| c.active && c.resource == resource)
        .cloned()
}

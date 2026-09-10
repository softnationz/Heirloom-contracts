//! Health-aware routing for outbound webhook delivery (reliability roadmap #4).
//!
//! `webhook::deliver_event` currently sends to every matching registration
//! regardless of how often that endpoint has recently failed, so requests
//! keep going to instances that are known to be unhealthy. This module
//! tracks a rolling health score per webhook registration, applies a
//! slow-start ramp for newly (re)registered endpoints, and exposes the
//! resulting weights/metrics so delivery can skip or de-prioritize
//! unhealthy targets.
//!
//! # Architecture
//!
//! ```text
//! Internal: record_outcome()        → called after each delivery attempt
//! Internal: routing_weight()        → consulted before/while delivering
//! GET  /admin/routing/health        → list_health (per-endpoint status)
//! GET  /admin/routing/metrics       → routing_metrics (aggregate view)
//! POST /admin/routing/test          → test_routing_decision
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{extract::State, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Number of requests a newly registered endpoint spends ramping up from a
/// reduced weight to full weight.
const SLOW_START_REQUESTS: u32 = 10;

/// Consecutive failures after which an endpoint is treated as unhealthy and
/// routed around entirely (weight 0) until it recovers.
const UNHEALTHY_THRESHOLD: u32 = 5;

/// Exponential moving average smoothing factor applied to each new outcome.
const EWMA_ALPHA: f64 = 0.3;

// ── Data types ──────────────────────────────────────────────────────────────

/// Rolling health record for a single delivery target (identified by URL or
/// registration id).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointHealth {
    pub endpoint: String,
    /// Exponential moving average of success (1.0) / failure (0.0) outcomes.
    pub success_rate_ewma: f64,
    pub total_requests: u32,
    pub total_successes: u32,
    pub total_failures: u32,
    pub consecutive_failures: u32,
    /// Requests served so far while ramping up from slow-start.
    pub slow_start_requests_served: u32,
    /// Current effective weight in `[0.0, 1.0]`, combining health + slow-start.
    pub weight: f64,
    pub first_seen: DateTime<Utc>,
    pub last_updated: DateTime<Utc>,
}

impl EndpointHealth {
    fn new(endpoint: String) -> Self {
        let now = Utc::now();
        Self {
            endpoint,
            success_rate_ewma: 1.0,
            total_requests: 0,
            total_successes: 0,
            total_failures: 0,
            consecutive_failures: 0,
            slow_start_requests_served: 0,
            weight: slow_start_weight(0),
            first_seen: now,
            last_updated: now,
        }
    }

    fn is_healthy(&self) -> bool {
        self.consecutive_failures < UNHEALTHY_THRESHOLD
    }
}

/// Weight applied during the slow-start ramp: starts at 10% and linearly
/// climbs to 100% over `SLOW_START_REQUESTS` requests.
fn slow_start_weight(requests_served: u32) -> f64 {
    if requests_served >= SLOW_START_REQUESTS {
        1.0
    } else {
        0.1 + 0.9 * (requests_served as f64 / SLOW_START_REQUESTS as f64)
    }
}

#[derive(Debug, Serialize)]
pub struct RoutingMetrics {
    pub total_endpoints: usize,
    pub healthy_endpoints: usize,
    pub unhealthy_endpoints: usize,
    pub endpoints_in_slow_start: usize,
    pub average_success_rate: f64,
}

#[derive(Debug, Deserialize)]
pub struct TestRoutingRequest {
    pub endpoint: String,
}

#[derive(Debug, Serialize)]
pub struct TestRoutingResponse {
    pub endpoint: String,
    pub would_route: bool,
    pub weight: f64,
    pub reason: String,
}

// ── State ────────────────────────────────────────────────────────────────────

pub type HealthStore = Arc<Mutex<HashMap<String, EndpointHealth>>>;

pub fn create_health_store() -> HealthStore {
    Arc::new(Mutex::new(HashMap::new()))
}

#[derive(Clone)]
pub struct HealthRoutingState {
    pub store: HealthStore,
}

impl HealthRoutingState {
    pub fn new() -> Self {
        Self {
            store: create_health_store(),
        }
    }
}

impl Default for HealthRoutingState {
    fn default() -> Self {
        Self::new()
    }
}

// ── Core routing logic ──────────────────────────────────────────────────────

/// Record the outcome of a delivery attempt against `endpoint`, updating its
/// rolling health score, slow-start progress, and effective weight.
pub fn record_outcome(state: &HealthRoutingState, endpoint: &str, success: bool) {
    let mut store = state.store.lock().unwrap();
    let health = store
        .entry(endpoint.to_string())
        .or_insert_with(|| EndpointHealth::new(endpoint.to_string()));

    health.total_requests += 1;
    if success {
        health.total_successes += 1;
        health.consecutive_failures = 0;
    } else {
        health.total_failures += 1;
        health.consecutive_failures += 1;
    }

    let outcome_value = if success { 1.0 } else { 0.0 };
    health.success_rate_ewma =
        EWMA_ALPHA * outcome_value + (1.0 - EWMA_ALPHA) * health.success_rate_ewma;

    if health.slow_start_requests_served < SLOW_START_REQUESTS {
        health.slow_start_requests_served += 1;
    }

    let ramp_weight = slow_start_weight(health.slow_start_requests_served);
    let health_weight = if health.is_healthy() {
        health.success_rate_ewma.max(0.0)
    } else {
        0.0
    };
    health.weight = ramp_weight * health_weight;
    health.last_updated = Utc::now();
}

/// Current routing weight for `endpoint` in `[0.0, 1.0]`. Endpoints never
/// seen before default to the initial slow-start weight so brand-new
/// targets are exercised cautiously rather than being skipped outright.
pub fn routing_weight(state: &HealthRoutingState, endpoint: &str) -> f64 {
    let store = state.store.lock().unwrap();
    store
        .get(endpoint)
        .map(|h| h.weight)
        .unwrap_or_else(|| slow_start_weight(0))
}

/// Whether `endpoint` should currently receive traffic at all (weight > 0).
pub fn should_route(state: &HealthRoutingState, endpoint: &str) -> bool {
    routing_weight(state, endpoint) > 0.0
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// `GET /admin/routing/health` — per-endpoint health/weight snapshot.
pub async fn list_health(
    State(state): State<Arc<HealthRoutingState>>,
) -> Json<Vec<EndpointHealth>> {
    let store = state.store.lock().unwrap();
    let mut endpoints: Vec<EndpointHealth> = store.values().cloned().collect();
    endpoints.sort_by(|a, b| a.endpoint.cmp(&b.endpoint));
    Json(endpoints)
}

/// `GET /admin/routing/metrics` — aggregate routing health metrics.
pub async fn routing_metrics(State(state): State<Arc<HealthRoutingState>>) -> Json<RoutingMetrics> {
    let store = state.store.lock().unwrap();
    let total_endpoints = store.len();
    let healthy_endpoints = store.values().filter(|h| h.is_healthy()).count();
    let endpoints_in_slow_start = store
        .values()
        .filter(|h| h.slow_start_requests_served < SLOW_START_REQUESTS)
        .count();
    let average_success_rate = if total_endpoints == 0 {
        1.0
    } else {
        store.values().map(|h| h.success_rate_ewma).sum::<f64>() / total_endpoints as f64
    };

    Json(RoutingMetrics {
        total_endpoints,
        healthy_endpoints,
        unhealthy_endpoints: total_endpoints - healthy_endpoints,
        endpoints_in_slow_start,
        average_success_rate,
    })
}

/// `POST /admin/routing/test` — check whether a given endpoint would
/// currently receive traffic, and why, without performing any delivery.
pub async fn test_routing_decision(
    State(state): State<Arc<HealthRoutingState>>,
    Json(body): Json<TestRoutingRequest>,
) -> Json<TestRoutingResponse> {
    let store = state.store.lock().unwrap();
    let (weight, reason) = match store.get(&body.endpoint) {
        None => (
            slow_start_weight(0),
            "no history yet; entering slow-start at reduced weight".to_string(),
        ),
        Some(health) if !health.is_healthy() => (
            0.0,
            format!(
                "endpoint marked unhealthy after {} consecutive failures",
                health.consecutive_failures
            ),
        ),
        Some(health) if health.slow_start_requests_served < SLOW_START_REQUESTS => (
            health.weight,
            format!(
                "endpoint in slow-start ({}/{} requests served)",
                health.slow_start_requests_served, SLOW_START_REQUESTS
            ),
        ),
        Some(health) => (
            health.weight,
            format!(
                "endpoint healthy with {:.1}% rolling success rate",
                health.success_rate_ewma * 100.0
            ),
        ),
    };

    Json(TestRoutingResponse {
        endpoint: body.endpoint,
        would_route: weight > 0.0,
        weight,
        reason,
    })
}

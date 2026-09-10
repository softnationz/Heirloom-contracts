//! Canary deployment strategy.
//!
//! Deployments were previously all-or-nothing, so a bad release affected
//! 100% of traffic immediately. This module implements gradual, staged
//! traffic-split rollouts with metric-based progression and automated
//! rollback when error budgets are breached.
//!
//! # Architecture
//!
//! ```text
//! POST /deployments/canary               → start_canary_deployment
//! GET  /deployments/canary/:id           → get_canary_deployment
//! POST /deployments/canary/:id/evaluate  → evaluate_canary (reports metrics, may advance/rollback)
//! POST /deployments/canary/:id/rollback  → rollback_canary (manual/forced rollback)
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

/// One stage of a canary rollout: the percentage of traffic to route to the
/// new version, and how long to hold at that percentage before evaluating
/// whether to progress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanaryStage {
    pub traffic_percent: u8,
    pub min_duration_minutes: i64,
}

/// Thresholds beyond which a canary is considered unhealthy and rolled back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricThresholds {
    pub max_error_rate: f64,
    pub max_latency_p99_ms: f64,
}

impl Default for MetricThresholds {
    fn default() -> Self {
        Self {
            max_error_rate: 0.02,
            max_latency_p99_ms: 500.0,
        }
    }
}

/// A metrics sample reported for the canary's current traffic slice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanaryMetrics {
    pub error_rate: f64,
    pub latency_p99_ms: f64,
}

/// Status of a canary deployment.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CanaryStatus {
    InProgress,
    Completed,
    RolledBack,
}

/// One recorded step in the deployment's progression history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanaryEvent {
    pub timestamp: DateTime<Utc>,
    pub description: String,
}

/// A canary deployment tracked from start to completion or rollback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanaryDeployment {
    pub id: String,
    pub service: String,
    pub version: String,
    pub stages: Vec<CanaryStage>,
    pub current_stage: usize,
    pub thresholds: MetricThresholds,
    pub status: CanaryStatus,
    pub last_metrics: Option<CanaryMetrics>,
    pub history: Vec<CanaryEvent>,
    pub started_at: DateTime<Utc>,
    pub stage_started_at: DateTime<Utc>,
}

impl CanaryDeployment {
    /// The traffic percentage currently routed to the new version.
    pub fn current_traffic_percent(&self) -> u8 {
        self.stages
            .get(self.current_stage)
            .map(|s| s.traffic_percent)
            .unwrap_or(0)
    }

    fn push_event(&mut self, description: impl Into<String>) {
        self.history.push(CanaryEvent {
            timestamp: Utc::now(),
            description: description.into(),
        });
    }
}

/// Request body for `POST /deployments/canary`.
#[derive(Debug, Deserialize)]
pub struct StartCanaryRequest {
    pub service: String,
    pub version: String,
    /// Ordered rollout stages; defaults to a standard 5/25/50/100 ramp if
    /// omitted.
    pub stages: Option<Vec<CanaryStage>>,
    pub thresholds: Option<MetricThresholds>,
}

/// Request body for `POST /deployments/canary/:id/evaluate`.
#[derive(Debug, Deserialize)]
pub struct EvaluateCanaryRequest {
    pub metrics: CanaryMetrics,
}

/// Request body for `POST /deployments/canary/:id/rollback`.
#[derive(Debug, Deserialize)]
pub struct RollbackCanaryRequest {
    pub reason: String,
}

fn default_stages() -> Vec<CanaryStage> {
    vec![
        CanaryStage { traffic_percent: 5, min_duration_minutes: 10 },
        CanaryStage { traffic_percent: 25, min_duration_minutes: 15 },
        CanaryStage { traffic_percent: 50, min_duration_minutes: 15 },
        CanaryStage { traffic_percent: 100, min_duration_minutes: 0 },
    ]
}

pub type CanaryStore = Arc<Mutex<HashMap<String, CanaryDeployment>>>;

pub fn create_canary_store() -> CanaryStore {
    Arc::new(Mutex::new(HashMap::new()))
}

#[derive(Clone)]
pub struct CanaryState {
    pub store: CanaryStore,
}

impl CanaryState {
    pub fn new() -> Self {
        Self {
            store: create_canary_store(),
        }
    }
}

impl Default for CanaryState {
    fn default() -> Self {
        Self::new()
    }
}

/// `POST /deployments/canary` — start a new canary deployment at the first
/// (smallest traffic) stage.
pub async fn start_canary_deployment(
    State(state): State<Arc<CanaryState>>,
    Json(body): Json<StartCanaryRequest>,
) -> Result<(StatusCode, Json<CanaryDeployment>), (StatusCode, Json<serde_json::Value>)> {
    let stages = body.stages.unwrap_or_else(default_stages);
    if stages.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "at least one stage is required" })),
        ));
    }

    let now = Utc::now();
    let mut deployment = CanaryDeployment {
        id: Uuid::new_v4().to_string(),
        service: body.service,
        version: body.version,
        stages,
        current_stage: 0,
        thresholds: body.thresholds.unwrap_or_default(),
        status: CanaryStatus::InProgress,
        last_metrics: None,
        history: vec![],
        started_at: now,
        stage_started_at: now,
    };
    deployment.push_event(format!(
        "canary started at {}% traffic",
        deployment.current_traffic_percent()
    ));

    let mut store = state.store.lock().unwrap();
    store.insert(deployment.id.clone(), deployment.clone());

    Ok((StatusCode::CREATED, Json(deployment)))
}

/// `GET /deployments/canary/:id`
pub async fn get_canary_deployment(
    State(state): State<Arc<CanaryState>>,
    Path(id): Path<String>,
) -> Result<Json<CanaryDeployment>, StatusCode> {
    let store = state.store.lock().unwrap();
    store.get(&id).cloned().map(Json).ok_or(StatusCode::NOT_FOUND)
}

/// `POST /deployments/canary/:id/evaluate` — report the latest metrics for
/// the canary's current traffic slice. If metrics breach thresholds, the
/// deployment is automatically rolled back; otherwise, once the stage's
/// minimum duration has elapsed, it progresses to the next stage.
pub async fn evaluate_canary(
    State(state): State<Arc<CanaryState>>,
    Path(id): Path<String>,
    Json(body): Json<EvaluateCanaryRequest>,
) -> Result<Json<CanaryDeployment>, StatusCode> {
    let mut store = state.store.lock().unwrap();
    let deployment = store.get_mut(&id).ok_or(StatusCode::NOT_FOUND)?;

    if deployment.status != CanaryStatus::InProgress {
        return Ok(Json(deployment.clone()));
    }

    deployment.last_metrics = Some(body.metrics.clone());

    let breached = body.metrics.error_rate > deployment.thresholds.max_error_rate
        || body.metrics.latency_p99_ms > deployment.thresholds.max_latency_p99_ms;

    if breached {
        deployment.status = CanaryStatus::RolledBack;
        deployment.push_event(format!(
            "automated rollback: error_rate={:.4} latency_p99_ms={:.1} breached thresholds",
            body.metrics.error_rate, body.metrics.latency_p99_ms
        ));
        tracing::error!(
            deployment_id = %id,
            error_rate = body.metrics.error_rate,
            latency_p99_ms = body.metrics.latency_p99_ms,
            "canary rolled back automatically due to metric breach"
        );
        return Ok(Json(deployment.clone()));
    }

    let elapsed_minutes = (Utc::now() - deployment.stage_started_at).num_minutes();
    let stage = deployment.stages[deployment.current_stage].clone();

    if elapsed_minutes >= stage.min_duration_minutes {
        if deployment.current_stage + 1 < deployment.stages.len() {
            deployment.current_stage += 1;
            deployment.stage_started_at = Utc::now();
            deployment.push_event(format!(
                "progressed to {}% traffic",
                deployment.current_traffic_percent()
            ));
        } else {
            deployment.status = CanaryStatus::Completed;
            deployment.push_event("canary completed at 100% traffic".to_string());
        }
    }

    Ok(Json(deployment.clone()))
}

/// `POST /deployments/canary/:id/rollback` — force an immediate rollback,
/// e.g. triggered manually or by an external alert.
pub async fn rollback_canary(
    State(state): State<Arc<CanaryState>>,
    Path(id): Path<String>,
    Json(body): Json<RollbackCanaryRequest>,
) -> Result<Json<CanaryDeployment>, StatusCode> {
    let mut store = state.store.lock().unwrap();
    let deployment = store.get_mut(&id).ok_or(StatusCode::NOT_FOUND)?;

    deployment.status = CanaryStatus::RolledBack;
    deployment.push_event(format!("manual rollback: {}", body.reason));

    tracing::warn!(deployment_id = %id, reason = %body.reason, "canary rolled back manually");

    Ok(Json(deployment.clone()))
}

//! Dead-letter queue for failed asynchronous deliveries (reliability roadmap #2).
//!
//! Today a webhook delivery that exhausts its retries is simply logged and
//! dropped (see `webhook::attempt_delivery`). This module captures those
//! failures instead so operators can inspect what was lost and replay it
//! once the downstream target recovers.
//!
//! # Architecture
//!
//! ```text
//! Internal: route_to_dlq()        → called on exhausted delivery retries
//! GET  /admin/dlq                 → list_dlq_entries (inspection)
//! GET  /admin/dlq/:id             → get_dlq_entry
//! POST /admin/dlq/replay          → replay_dlq_entries
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Data types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DlqStatus {
    /// Awaiting operator action or automatic replay.
    Pending,
    /// Replay was attempted and the downstream target accepted it.
    Replayed,
    /// Replay was attempted and the downstream target rejected it again.
    ReplayFailed,
}

/// A single failed delivery captured in the dead-letter queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DlqEntry {
    pub id: String,
    /// Where the failure originated, e.g. "webhook:<registration_id>".
    pub source: String,
    /// The downstream URL/endpoint the payload was destined for, if replayable.
    pub target: Option<String>,
    pub payload: serde_json::Value,
    pub error: String,
    pub attempts: u32,
    pub status: DlqStatus,
    pub created_at: DateTime<Utc>,
    pub last_attempt_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct ListDlqQuery {
    pub status: Option<String>,
    pub source: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct ReplayDlqRequest {
    /// Replay a specific entry by id.
    pub id: Option<String>,
    /// Replay every pending entry.
    #[serde(default)]
    pub replay_all: bool,
}

#[derive(Debug, Serialize)]
pub struct ReplayResult {
    pub id: String,
    pub success: bool,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct ReplayResponse {
    pub attempted: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub results: Vec<ReplayResult>,
}

// ── In-memory store ─────────────────────────────────────────────────────────

pub type DlqStore = Arc<Mutex<HashMap<String, DlqEntry>>>;

pub fn create_dlq_store() -> DlqStore {
    Arc::new(Mutex::new(HashMap::new()))
}

#[derive(Clone)]
pub struct DlqState {
    pub store: DlqStore,
    pub http_client: Client,
}

impl DlqState {
    pub fn new() -> Self {
        Self {
            store: create_dlq_store(),
            http_client: Client::new(),
        }
    }
}

impl Default for DlqState {
    fn default() -> Self {
        Self::new()
    }
}

// ── Automatic routing on failure ────────────────────────────────────────────

/// Route a permanently failed delivery into the dead-letter queue.
///
/// Callers invoke this once their own retry budget is exhausted (see
/// `webhook::attempt_delivery`), so the payload isn't silently discarded.
pub fn route_to_dlq(
    state: &DlqState,
    source: impl Into<String>,
    target: Option<String>,
    payload: serde_json::Value,
    error: impl Into<String>,
    attempts: u32,
) -> DlqEntry {
    let now = Utc::now();
    let entry = DlqEntry {
        id: Uuid::new_v4().to_string(),
        source: source.into(),
        target,
        payload,
        error: error.into(),
        attempts,
        status: DlqStatus::Pending,
        created_at: now,
        last_attempt_at: now,
    };

    let mut store = state.store.lock().unwrap();
    store.insert(entry.id.clone(), entry.clone());
    tracing::warn!(dlq_id = %entry.id, source = %entry.source, "delivery routed to dead-letter queue");

    entry
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// `GET /admin/dlq` — inspect dead-lettered entries, optionally filtered.
pub async fn list_dlq_entries(
    State(state): State<Arc<DlqState>>,
    Query(query): Query<ListDlqQuery>,
) -> Json<Vec<DlqEntry>> {
    let store = state.store.lock().unwrap();
    let mut entries: Vec<DlqEntry> = store
        .values()
        .filter(|e| {
            if let Some(ref status) = query.status {
                let matches = match status.as_str() {
                    "pending" => e.status == DlqStatus::Pending,
                    "replayed" => e.status == DlqStatus::Replayed,
                    "replay_failed" => e.status == DlqStatus::ReplayFailed,
                    _ => true,
                };
                if !matches {
                    return false;
                }
            }
            if let Some(ref source) = query.source {
                if !e.source.contains(source.as_str()) {
                    return false;
                }
            }
            true
        })
        .cloned()
        .collect();

    entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if let Some(limit) = query.limit {
        entries.truncate(limit);
    }

    Json(entries)
}

/// `POST /admin/dlq/replay` — replay a specific entry, or every pending entry.
pub async fn replay_dlq_entries(
    State(state): State<Arc<DlqState>>,
    Json(body): Json<ReplayDlqRequest>,
) -> Result<Json<ReplayResponse>, (StatusCode, Json<serde_json::Value>)> {
    let targets: Vec<DlqEntry> = {
        let store = state.store.lock().unwrap();
        if body.replay_all {
            store
                .values()
                .filter(|e| e.status == DlqStatus::Pending)
                .cloned()
                .collect()
        } else if let Some(ref id) = body.id {
            match store.get(id) {
                Some(entry) => vec![entry.clone()],
                None => {
                    return Err((
                        StatusCode::NOT_FOUND,
                        Json(serde_json::json!({ "error": "dlq entry not found" })),
                    ))
                }
            }
        } else {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({ "error": "must specify id or replay_all" })),
            ));
        }
    };

    let mut results = Vec::with_capacity(targets.len());
    let mut succeeded = 0usize;
    let mut failed = 0usize;

    for entry in &targets {
        let outcome = replay_one(&state, entry).await;
        if outcome.success {
            succeeded += 1;
        } else {
            failed += 1;
        }
        results.push(outcome);
    }

    Ok(Json(ReplayResponse {
        attempted: targets.len(),
        succeeded,
        failed,
        results,
    }))
}

async fn replay_one(state: &DlqState, entry: &DlqEntry) -> ReplayResult {
    let Some(ref target) = entry.target else {
        mark_status(state, &entry.id, DlqStatus::ReplayFailed);
        return ReplayResult {
            id: entry.id.clone(),
            success: false,
            detail: "no replay target recorded for this entry".to_string(),
        };
    };

    let send_result = state
        .http_client
        .post(target)
        .header("Content-Type", "application/json")
        .header("X-Heirloom-Dlq-Replay", &entry.id)
        .json(&entry.payload)
        .send()
        .await;

    match send_result {
        Ok(resp) if resp.status().is_success() => {
            mark_status(state, &entry.id, DlqStatus::Replayed);
            ReplayResult {
                id: entry.id.clone(),
                success: true,
                detail: format!("replay accepted with status {}", resp.status()),
            }
        }
        Ok(resp) => {
            mark_status(state, &entry.id, DlqStatus::ReplayFailed);
            ReplayResult {
                id: entry.id.clone(),
                success: false,
                detail: format!("replay rejected with status {}", resp.status()),
            }
        }
        Err(e) => {
            mark_status(state, &entry.id, DlqStatus::ReplayFailed);
            ReplayResult {
                id: entry.id.clone(),
                success: false,
                detail: format!("replay request error: {e}"),
            }
        }
    }
}

fn mark_status(state: &DlqState, id: &str, status: DlqStatus) {
    let mut store = state.store.lock().unwrap();
    if let Some(entry) = store.get_mut(id) {
        entry.status = status;
        entry.last_attempt_at = Utc::now();
        entry.attempts += 1;
    }
}

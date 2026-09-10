// #73 — API Request Replay Capability
// Implements: request/response logging, POST /replay, conditional replay, replay validation

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// ── Log entry ─────────────────────────────────────────────────────────────────

/// A recorded HTTP request/response pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLog {
    /// Unique ID for this log entry.
    pub id: String,
    /// HTTP method (GET, POST, …).
    pub method: String,
    /// Request path including query string.
    pub path: String,
    /// Captured request headers (excluding Authorization).
    pub headers: HashMap<String, String>,
    /// Captured request body (JSON, or null for GET/DELETE).
    pub request_body: Option<serde_json::Value>,
    /// HTTP status code returned.
    pub response_status: u16,
    /// Captured response body.
    pub response_body: serde_json::Value,
    /// Milliseconds taken to process the original request.
    pub duration_ms: u64,
    /// Timestamp of the original request.
    pub recorded_at: DateTime<Utc>,
    /// Optional tag/label (e.g., vault ID or operation name).
    pub tag: Option<String>,
}

impl RequestLog {
    pub fn new(
        method: impl Into<String>,
        path: impl Into<String>,
        headers: HashMap<String, String>,
        request_body: Option<serde_json::Value>,
        response_status: u16,
        response_body: serde_json::Value,
        duration_ms: u64,
        tag: Option<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            method: method.into(),
            path: path.into(),
            headers,
            request_body,
            response_status,
            response_body,
            duration_ms,
            recorded_at: Utc::now(),
            tag,
        }
    }
}

// ── Replay result ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplayOutcome {
    /// The replayed response matched the original.
    Identical,
    /// The replayed response differed from the original.
    Diverged,
    /// The replayed request was skipped due to a failed validation.
    Skipped,
    /// The replay completed but validation was not requested.
    Unvalidated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayResult {
    pub log_id: String,
    pub method: String,
    pub path: String,
    /// The original response status.
    pub original_status: u16,
    /// The replayed response status.
    pub replayed_status: u16,
    /// The replayed response body.
    pub replayed_body: serde_json::Value,
    /// Outcome of the replay validation.
    pub outcome: ReplayOutcome,
    /// Human-readable diff summary when outcome is `Diverged`.
    pub diff_notes: Vec<String>,
    pub replayed_at: DateTime<Utc>,
}

// ── In-memory log store ───────────────────────────────────────────────────────

pub type RequestLogStore = Arc<Mutex<Vec<RequestLog>>>;

pub fn create_request_log_store() -> RequestLogStore {
    Arc::new(Mutex::new(Vec::new()))
}

/// Append a new log entry and return its ID.
pub fn record_request(store: &RequestLogStore, log: RequestLog) -> String {
    let id = log.id.clone();
    store.lock().unwrap().push(log);
    id
}

/// Retrieve a log entry by ID.
pub fn get_log(store: &RequestLogStore, id: &str) -> Option<RequestLog> {
    store.lock().unwrap().iter().find(|l| l.id == id).cloned()
}

/// List all logs, newest first, with optional path prefix filter.
pub fn list_logs(
    store: &RequestLogStore,
    path_prefix: Option<&str>,
    limit: usize,
) -> Vec<RequestLog> {
    let store = store.lock().unwrap();
    let mut logs: Vec<RequestLog> = store
        .iter()
        .filter(|l| path_prefix.map_or(true, |p| l.path.starts_with(p)))
        .cloned()
        .collect();
    logs.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
    logs.truncate(limit);
    logs
}

// ── Replay engine ─────────────────────────────────────────────────────────────

/// Validation rule for conditional replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayCondition {
    /// Only replay if the original response had this status code.
    OriginalStatusEquals(u16),
    /// Only replay if the path contains this substring.
    PathContains(String),
    /// Only replay if the request body key matches value.
    BodyKeyEquals {
        key: String,
        value: serde_json::Value,
    },
    /// Always replay (default).
    Always,
}

impl ReplayCondition {
    pub fn check(&self, log: &RequestLog) -> bool {
        match self {
            ReplayCondition::OriginalStatusEquals(code) => log.response_status == *code,
            ReplayCondition::PathContains(substring) => log.path.contains(substring.as_str()),
            ReplayCondition::BodyKeyEquals { key, value } => log
                .request_body
                .as_ref()
                .and_then(|b| b.get(key))
                .map_or(false, |v| v == value),
            ReplayCondition::Always => true,
        }
    }
}

/// Simulate re-executing a logged request and compare to the original response.
///
/// In a full production implementation this would re-issue the HTTP call.
/// Here we simulate the replay by re-using the stored response body and
/// marking the outcome based on a deterministic comparison.
pub fn replay_log_entry(
    log: &RequestLog,
    conditions: &[ReplayCondition],
    validate: bool,
) -> ReplayResult {
    // Check all conditions before replaying.
    for condition in conditions {
        if !condition.check(log) {
            return ReplayResult {
                log_id: log.id.clone(),
                method: log.method.clone(),
                path: log.path.clone(),
                original_status: log.response_status,
                replayed_status: 0,
                replayed_body: serde_json::Value::Null,
                outcome: ReplayOutcome::Skipped,
                diff_notes: vec![format!("Condition {:?} not met", condition)],
                replayed_at: Utc::now(),
            };
        }
    }

    // Simulate replay: in a real system this would re-issue the request.
    // We echo back the stored response to demonstrate the plumbing.
    let replayed_status = log.response_status;
    let replayed_body = log.response_body.clone();

    let (outcome, diff_notes) = if validate {
        compare_responses(
            log.response_status,
            &log.response_body,
            replayed_status,
            &replayed_body,
        )
    } else {
        (ReplayOutcome::Unvalidated, vec![])
    };

    ReplayResult {
        log_id: log.id.clone(),
        method: log.method.clone(),
        path: log.path.clone(),
        original_status: log.response_status,
        replayed_status,
        replayed_body,
        outcome,
        diff_notes,
        replayed_at: Utc::now(),
    }
}

/// Compare original and replayed responses and return diff notes.
fn compare_responses(
    orig_status: u16,
    orig_body: &serde_json::Value,
    replay_status: u16,
    replay_body: &serde_json::Value,
) -> (ReplayOutcome, Vec<String>) {
    let mut notes = Vec::new();

    if orig_status != replay_status {
        notes.push(format!(
            "Status mismatch: original={}, replayed={}",
            orig_status, replay_status
        ));
    }

    if orig_body != replay_body {
        notes.push("Response body differs from original".into());
        // Surface top-level key differences for objects.
        if let (Some(orig_obj), Some(replay_obj)) = (orig_body.as_object(), replay_body.as_object())
        {
            for key in orig_obj.keys() {
                if orig_obj.get(key) != replay_obj.get(key) {
                    notes.push(format!("  Key '{}' changed", key));
                }
            }
        }
    }

    if notes.is_empty() {
        (
            ReplayOutcome::Identical,
            vec!["Responses are identical".into()],
        )
    } else {
        (ReplayOutcome::Diverged, notes)
    }
}

// ── HTTP request / response types ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ReplayRequest {
    /// ID of the log entry to replay. Required.
    pub log_id: String,
    /// Optional conditions that must be satisfied before replaying.
    pub conditions: Option<Vec<ReplayCondition>>,
    /// If true, compare replayed response to original and report diff.
    pub validate: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ListLogsQuery {
    /// Filter by path prefix.
    pub path: Option<String>,
    /// Maximum number of results (default 50, max 200).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct BatchReplayRequest {
    pub log_ids: Vec<String>,
    pub conditions: Option<Vec<ReplayCondition>>,
    pub validate: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct BatchReplayResponse {
    pub results: Vec<ReplayResult>,
    pub total: usize,
    pub identical: usize,
    pub diverged: usize,
    pub skipped: usize,
}

// ── Route handlers ────────────────────────────────────────────────────────────

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};

use crate::error::AppError;

/// POST /replay — replay a single logged request (admin endpoint).
pub async fn replay_handler(
    State(log_store): State<RequestLogStore>,
    Json(body): Json<ReplayRequest>,
) -> Result<(StatusCode, Json<ReplayResult>), AppError> {
    let log = get_log(&log_store, &body.log_id).ok_or(AppError::NotFound)?;

    let conditions = body
        .conditions
        .unwrap_or_else(|| vec![ReplayCondition::Always]);
    let validate = body.validate.unwrap_or(true);

    let result = replay_log_entry(&log, &conditions, validate);
    Ok((StatusCode::OK, Json(result)))
}

/// POST /replay/batch — replay multiple logged requests at once.
pub async fn batch_replay_handler(
    State(log_store): State<RequestLogStore>,
    Json(body): Json<BatchReplayRequest>,
) -> Result<(StatusCode, Json<BatchReplayResponse>), AppError> {
    if body.log_ids.is_empty() {
        return Err(AppError::InvalidInput("log_ids must not be empty".into()));
    }
    if body.log_ids.len() > 50 {
        return Err(AppError::InvalidInput(
            "Cannot replay more than 50 logs at once".into(),
        ));
    }

    let conditions = body
        .conditions
        .unwrap_or_else(|| vec![ReplayCondition::Always]);
    let validate = body.validate.unwrap_or(true);

    let mut results = Vec::new();
    for id in &body.log_ids {
        match get_log(&log_store, id) {
            Some(log) => results.push(replay_log_entry(&log, &conditions, validate)),
            None => {
                // Record a skipped entry for missing log IDs.
                results.push(ReplayResult {
                    log_id: id.clone(),
                    method: String::new(),
                    path: String::new(),
                    original_status: 0,
                    replayed_status: 0,
                    replayed_body: serde_json::Value::Null,
                    outcome: ReplayOutcome::Skipped,
                    diff_notes: vec![format!("Log entry '{}' not found", id)],
                    replayed_at: Utc::now(),
                });
            }
        }
    }

    let identical = results
        .iter()
        .filter(|r| r.outcome == ReplayOutcome::Identical)
        .count();
    let diverged = results
        .iter()
        .filter(|r| r.outcome == ReplayOutcome::Diverged)
        .count();
    let skipped = results
        .iter()
        .filter(|r| r.outcome == ReplayOutcome::Skipped)
        .count();
    let total = results.len();

    Ok((
        StatusCode::OK,
        Json(BatchReplayResponse {
            results,
            total,
            identical,
            diverged,
            skipped,
        }),
    ))
}

/// GET /replay/logs — list stored request logs (admin endpoint).
pub async fn list_logs_handler(
    State(log_store): State<RequestLogStore>,
    Query(query): Query<ListLogsQuery>,
) -> Result<Json<Vec<RequestLog>>, AppError> {
    let limit = query.limit.unwrap_or(50).min(200);
    let logs = list_logs(&log_store, query.path.as_deref(), limit);
    Ok(Json(logs))
}

/// GET /replay/logs/:log_id — retrieve a single log entry.
pub async fn get_log_handler(
    State(log_store): State<RequestLogStore>,
    Path(log_id): Path<String>,
) -> Result<Json<RequestLog>, AppError> {
    let log = get_log(&log_store, &log_id).ok_or(AppError::NotFound)?;
    Ok(Json(log))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_log(method: &str, path: &str, status: u16) -> RequestLog {
        RequestLog::new(
            method,
            path,
            HashMap::new(),
            Some(serde_json::json!({"vault_id": "v1"})),
            status,
            serde_json::json!({"status": "ok"}),
            42,
            None,
        )
    }

    #[test]
    fn test_record_and_retrieve() {
        let store = create_request_log_store();
        let log = make_log("POST", "/api/vaults/1/check-in", 200);
        let id = record_request(&store, log);
        let retrieved = get_log(&store, &id).unwrap();
        assert_eq!(retrieved.method, "POST");
        assert_eq!(retrieved.response_status, 200);
    }

    #[test]
    fn test_list_logs_with_prefix() {
        let store = create_request_log_store();
        record_request(&store, make_log("POST", "/api/vaults/1/check-in", 200));
        record_request(&store, make_log("POST", "/api/vaults/2/check-in", 200));
        record_request(&store, make_log("GET", "/health", 200));

        let api_logs = list_logs(&store, Some("/api/vaults"), 10);
        assert_eq!(api_logs.len(), 2);

        let all_logs = list_logs(&store, None, 10);
        assert_eq!(all_logs.len(), 3);
    }

    #[test]
    fn test_replay_always_condition() {
        let log = make_log("POST", "/api/vaults/1/check-in", 200);
        let result = replay_log_entry(&log, &[ReplayCondition::Always], true);
        assert_eq!(result.outcome, ReplayOutcome::Identical);
        assert_eq!(result.replayed_status, 200);
    }

    #[test]
    fn test_replay_condition_skips_on_mismatch() {
        let log = make_log("POST", "/api/vaults/1/check-in", 404);
        let result = replay_log_entry(&log, &[ReplayCondition::OriginalStatusEquals(200)], true);
        assert_eq!(result.outcome, ReplayOutcome::Skipped);
    }

    #[test]
    fn test_replay_path_contains_condition() {
        let log = make_log("GET", "/api/vaults/1/ttl", 200);
        // Path contains "ttl" → should replay.
        let result = replay_log_entry(&log, &[ReplayCondition::PathContains("ttl".into())], false);
        assert_eq!(result.outcome, ReplayOutcome::Unvalidated);

        // Path contains "deposit" → should skip.
        let result2 = replay_log_entry(
            &log,
            &[ReplayCondition::PathContains("deposit".into())],
            false,
        );
        assert_eq!(result2.outcome, ReplayOutcome::Skipped);
    }

    #[test]
    fn test_replay_body_key_condition() {
        let log = make_log("POST", "/api/vaults/1/check-in", 200);
        // Body has {"vault_id": "v1"} → matches.
        let result = replay_log_entry(
            &log,
            &[ReplayCondition::BodyKeyEquals {
                key: "vault_id".into(),
                value: serde_json::json!("v1"),
            }],
            false,
        );
        assert_eq!(result.outcome, ReplayOutcome::Unvalidated);

        // Non-matching value → skip.
        let result2 = replay_log_entry(
            &log,
            &[ReplayCondition::BodyKeyEquals {
                key: "vault_id".into(),
                value: serde_json::json!("v999"),
            }],
            false,
        );
        assert_eq!(result2.outcome, ReplayOutcome::Skipped);
    }

    #[test]
    fn test_replay_unvalidated() {
        let log = make_log("GET", "/health", 200);
        let result = replay_log_entry(&log, &[ReplayCondition::Always], false);
        assert_eq!(result.outcome, ReplayOutcome::Unvalidated);
    }

    #[test]
    fn test_list_logs_limit() {
        let store = create_request_log_store();
        for i in 0..10 {
            record_request(&store, make_log("GET", &format!("/path/{}", i), 200));
        }
        let limited = list_logs(&store, None, 5);
        assert_eq!(limited.len(), 5);
    }

    #[test]
    fn test_batch_replay() {
        let store = create_request_log_store();
        let id1 = record_request(&store, make_log("POST", "/api/vaults/1/check-in", 200));
        let id2 = record_request(&store, make_log("GET", "/api/vaults/2", 200));

        let results: Vec<ReplayResult> = vec![id1, id2]
            .iter()
            .map(|id| {
                let log = get_log(&store, id).unwrap();
                replay_log_entry(&log, &[ReplayCondition::Always], true)
            })
            .collect();

        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|r| r.outcome == ReplayOutcome::Identical));
    }
}

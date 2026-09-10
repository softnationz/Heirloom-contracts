// #74 — Bulk Operation Queuing
// Implements: async job queue, POST /jobs, GET /jobs/{job_id}, progress tracking, result retrieval

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// ── Job status ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

// ── Supported bulk operation types ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BulkOperationType {
    /// Batch-update TTL for multiple vaults.
    UpdateTtl,
    /// Send reminder notifications to a list of vault owners.
    SendReminders,
    /// Export multiple vaults to JSON.
    ExportVaults,
    /// Apply a retention policy sweep across all time-series.
    RetentionSweep,
    /// Generic batch of arbitrary operations defined in `payload`.
    Custom,
}

// ── Core job struct ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulkJob {
    pub id: String,
    pub operation: BulkOperationType,
    /// Caller-supplied input data (e.g., list of vault IDs).
    pub payload: serde_json::Value,
    pub status: JobStatus,
    /// 0–100 progress percentage.
    pub progress: u8,
    /// Total number of items in this job.
    pub total_items: usize,
    /// Items processed so far.
    pub processed_items: usize,
    /// Items that resulted in an error.
    pub failed_items: usize,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Structured result once the job is done.
    pub result: Option<serde_json::Value>,
    /// Human-readable error message on failure.
    pub error: Option<String>,
    /// Optional label supplied by the caller.
    pub label: Option<String>,
}

impl BulkJob {
    pub fn new(
        operation: BulkOperationType,
        payload: serde_json::Value,
        total_items: usize,
        label: Option<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            operation,
            payload,
            status: JobStatus::Queued,
            progress: 0,
            total_items,
            processed_items: 0,
            failed_items: 0,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            result: None,
            error: None,
            label,
        }
    }

    /// Advance progress; clamps to 100.
    pub fn advance(&mut self, processed: usize, failed: usize) {
        self.processed_items += processed;
        self.failed_items += failed;
        if self.total_items > 0 {
            self.progress =
                ((self.processed_items * 100) / self.total_items).min(100) as u8;
        }
    }

    pub fn mark_running(&mut self) {
        self.status = JobStatus::Running;
        self.started_at = Some(Utc::now());
    }

    pub fn mark_completed(&mut self, result: serde_json::Value) {
        self.status = JobStatus::Completed;
        self.progress = 100;
        self.completed_at = Some(Utc::now());
        self.result = Some(result);
    }

    pub fn mark_failed(&mut self, error: impl Into<String>) {
        self.status = JobStatus::Failed;
        self.completed_at = Some(Utc::now());
        self.error = Some(error.into());
    }

    pub fn mark_cancelled(&mut self) {
        self.status = JobStatus::Cancelled;
        self.completed_at = Some(Utc::now());
    }

    /// Estimated seconds remaining (simple linear extrapolation).
    pub fn estimated_seconds_remaining(&self) -> Option<u64> {
        if self.status != JobStatus::Running {
            return None;
        }
        let started = self.started_at?;
        let elapsed = (Utc::now() - started).num_seconds().max(0) as u64;
        if self.processed_items == 0 {
            return None;
        }
        let rate = self.processed_items as f64 / elapsed.max(1) as f64;
        let remaining = (self.total_items - self.processed_items) as f64;
        Some((remaining / rate).ceil() as u64)
    }
}

// ── In-memory job store ───────────────────────────────────────────────────────

pub type JobStore = Arc<Mutex<HashMap<String, BulkJob>>>;

pub fn create_job_store() -> JobStore {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Submit a new job and return its ID.
pub fn enqueue_job(store: &JobStore, job: BulkJob) -> String {
    let id = job.id.clone();
    store.lock().unwrap().insert(id.clone(), job);
    id
}

/// Retrieve a job by ID.
pub fn get_job(store: &JobStore, id: &str) -> Option<BulkJob> {
    store.lock().unwrap().get(id).cloned()
}

/// List all jobs, optionally filtered by status.
pub fn list_jobs(store: &JobStore, status_filter: Option<JobStatus>) -> Vec<BulkJob> {
    let store = store.lock().unwrap();
    let mut jobs: Vec<BulkJob> = store
        .values()
        .filter(|j| status_filter.as_ref().map_or(true, |s| &j.status == s))
        .cloned()
        .collect();
    jobs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    jobs
}

/// Cancel a queued job. Returns false if the job is not found or not cancellable.
pub fn cancel_job(store: &JobStore, id: &str) -> bool {
    let mut store = store.lock().unwrap();
    if let Some(job) = store.get_mut(id) {
        if matches!(job.status, JobStatus::Queued | JobStatus::Running) {
            job.mark_cancelled();
            return true;
        }
    }
    false
}

// ── Job executor (synchronous simulation for in-process queue) ────────────────
//
// In production this would be a background Tokio task. Here we expose a
// `process_job` function that can be called from a spawned task or inline.

pub fn process_job(store: &JobStore, id: &str) {
    // Mark running
    {
        let mut s = store.lock().unwrap();
        if let Some(job) = s.get_mut(id) {
            if job.status != JobStatus::Queued {
                return; // already picked up or cancelled
            }
            job.mark_running();
        } else {
            return;
        }
    }

    // Simulate processing — real implementations would call domain logic here.
    let (operation, total_items) = {
        let s = store.lock().unwrap();
        let job = s.get(id).unwrap();
        (job.operation.clone(), job.total_items)
    };

    // Step through items in batches of up to 10.
    let batch_size = 10.min(total_items.max(1));
    let mut processed = 0usize;

    while processed < total_items {
        let chunk = batch_size.min(total_items - processed);
        processed += chunk;

        let mut s = store.lock().unwrap();
        if let Some(job) = s.get_mut(id) {
            if job.status == JobStatus::Cancelled {
                return;
            }
            job.advance(chunk, 0);
        }
    }

    // Build a summary result.
    let result = serde_json::json!({
        "operation": format!("{:?}", operation),
        "total_processed": total_items,
        "total_failed": 0,
        "message": "Bulk operation completed successfully"
    });

    let mut s = store.lock().unwrap();
    if let Some(job) = s.get_mut(id) {
        job.mark_completed(result);
    }
}

// ── HTTP request / response types ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateJobRequest {
    pub operation: BulkOperationType,
    /// The items to process. For `update_ttl`, list of vault IDs; for
    /// `send_reminders`, list of owner addresses; etc.
    pub items: serde_json::Value,
    pub label: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateJobResponse {
    pub job_id: String,
    pub status: JobStatus,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct JobStatusResponse {
    pub job: BulkJob,
    pub estimated_seconds_remaining: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct ListJobsQuery {
    pub status: Option<JobStatus>,
}

// ── Route handlers ────────────────────────────────────────────────────────────

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};

use crate::error::AppError;

/// POST /jobs — submit a bulk operation job.
pub async fn create_job_handler(
    State(job_store): State<JobStore>,
    Json(body): Json<CreateJobRequest>,
) -> Result<(StatusCode, Json<CreateJobResponse>), AppError> {
    let items_count = match &body.items {
        serde_json::Value::Array(arr) => arr.len(),
        _ => 1,
    };

    if items_count == 0 {
        return Err(AppError::InvalidInput("items must not be empty".into()));
    }

    let job = BulkJob::new(body.operation, body.items, items_count, body.label);
    let job_id = job.id.clone();

    enqueue_job(&job_store, job);

    // Spawn synchronous processing in a blocking task so the handler returns
    // immediately (non-blocking to the caller).
    let store_clone = Arc::clone(&job_store);
    let id_clone = job_id.clone();
    tokio::task::spawn_blocking(move || {
        process_job(&store_clone, &id_clone);
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(CreateJobResponse {
            job_id,
            status: JobStatus::Queued,
            message: "Job queued for processing".into(),
        }),
    ))
}

/// GET /jobs/:job_id — retrieve job status and progress.
pub async fn get_job_handler(
    State(job_store): State<JobStore>,
    Path(job_id): Path<String>,
) -> Result<Json<JobStatusResponse>, AppError> {
    let job = get_job(&job_store, &job_id).ok_or(AppError::NotFound)?;
    let eta = job.estimated_seconds_remaining();
    Ok(Json(JobStatusResponse {
        job,
        estimated_seconds_remaining: eta,
    }))
}

/// GET /jobs — list all jobs with optional status filter.
pub async fn list_jobs_handler(
    State(job_store): State<JobStore>,
    Query(query): Query<ListJobsQuery>,
) -> Result<Json<Vec<BulkJob>>, AppError> {
    let jobs = list_jobs(&job_store, query.status);
    Ok(Json(jobs))
}

/// DELETE /jobs/:job_id — cancel a queued or running job.
pub async fn cancel_job_handler(
    State(job_store): State<JobStore>,
    Path(job_id): Path<String>,
) -> Result<StatusCode, AppError> {
    if cancel_job(&job_store, &job_id) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enqueue_and_retrieve() {
        let store = create_job_store();
        let job = BulkJob::new(
            BulkOperationType::UpdateTtl,
            serde_json::json!(["v1", "v2"]),
            2,
            Some("test-label".into()),
        );
        let id = enqueue_job(&store, job);
        let retrieved = get_job(&store, &id).unwrap();
        assert_eq!(retrieved.status, JobStatus::Queued);
        assert_eq!(retrieved.total_items, 2);
    }

    #[test]
    fn test_job_progress_advance() {
        let mut job = BulkJob::new(
            BulkOperationType::SendReminders,
            serde_json::json!([]),
            10,
            None,
        );
        job.mark_running();
        job.advance(5, 0);
        assert_eq!(job.progress, 50);
        job.advance(5, 0);
        assert_eq!(job.progress, 100);
    }

    #[test]
    fn test_job_completion() {
        let mut job = BulkJob::new(
            BulkOperationType::ExportVaults,
            serde_json::json!([]),
            1,
            None,
        );
        job.mark_running();
        job.mark_completed(serde_json::json!({"done": true}));
        assert_eq!(job.status, JobStatus::Completed);
        assert!(job.result.is_some());
    }

    #[test]
    fn test_cancel_queued_job() {
        let store = create_job_store();
        let job = BulkJob::new(
            BulkOperationType::RetentionSweep,
            serde_json::json!([]),
            5,
            None,
        );
        let id = enqueue_job(&store, job);
        assert!(cancel_job(&store, &id));
        assert_eq!(get_job(&store, &id).unwrap().status, JobStatus::Cancelled);
    }

    #[test]
    fn test_list_jobs_filtered() {
        let store = create_job_store();
        for _ in 0..3 {
            let job = BulkJob::new(BulkOperationType::Custom, serde_json::json!([]), 1, None);
            enqueue_job(&store, job);
        }
        let all = list_jobs(&store, None);
        assert_eq!(all.len(), 3);
        let queued = list_jobs(&store, Some(JobStatus::Queued));
        assert_eq!(queued.len(), 3);
        let running = list_jobs(&store, Some(JobStatus::Running));
        assert_eq!(running.len(), 0);
    }

    #[test]
    fn test_process_job_completes() {
        let store = create_job_store();
        let job = BulkJob::new(
            BulkOperationType::UpdateTtl,
            serde_json::json!(["v1", "v2", "v3"]),
            3,
            None,
        );
        let id = enqueue_job(&store, job);
        process_job(&store, &id);
        let done = get_job(&store, &id).unwrap();
        assert_eq!(done.status, JobStatus::Completed);
        assert_eq!(done.progress, 100);
    }
}

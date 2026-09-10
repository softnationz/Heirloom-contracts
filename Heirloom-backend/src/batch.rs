//! Partial-failure handling for batch operations (reliability roadmap #3).
//!
//! Batch endpoints in this backend historically fail all-or-nothing: a
//! validation error on item 47 of 50 would either abort the whole request or
//! silently swallow which items succeeded. This module provides a generic
//! per-item outcome tracker plus a concrete batch endpoint
//! (`POST /api/vaults/batch/reminder-preferences`) that applies the pattern
//! to the existing `reminder-preferences` write path in `routes.rs`.
//!
//! # Architecture
//!
//! ```text
//! POST /api/vaults/batch/reminder-preferences → batch_set_preferences
//! ```

use std::sync::Arc;

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};

use crate::db::AppState;
use crate::models::{Channel, Frequency, ReminderPreferences};

// ── Generic partial-success tracking ────────────────────────────────────────

/// Outcome of a single item within a batch request.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum BatchItemOutcome<T: Serialize> {
    Success { item: T },
    Failure { error: String, retryable: bool },
}

/// Result of processing one input item, keyed by an identifier the caller
/// supplies (e.g. vault_id) so failures can be matched back to inputs.
#[derive(Debug, Clone, Serialize)]
pub struct BatchItemResult<T: Serialize> {
    pub key: String,
    #[serde(flatten)]
    pub outcome: BatchItemOutcome<T>,
}

/// Aggregate response for a batch operation with per-item error reporting.
#[derive(Debug, Clone, Serialize)]
pub struct BatchResponse<T: Serialize> {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    /// Fraction of items that succeeded, in `[0.0, 1.0]`.
    pub success_rate: f64,
    pub items: Vec<BatchItemResult<T>>,
    /// Guidance for the caller on whether/how to retry, derived from which
    /// failures (if any) were marked retryable.
    pub retry_guidance: String,
}

/// Accumulates per-item outcomes and produces a `BatchResponse` with
/// aggregate success/failure statistics and retry guidance.
pub struct BatchTracker<T: Serialize> {
    items: Vec<BatchItemResult<T>>,
}

impl<T: Serialize> BatchTracker<T> {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn record_success(&mut self, key: impl Into<String>, item: T) {
        self.items.push(BatchItemResult {
            key: key.into(),
            outcome: BatchItemOutcome::Success { item },
        });
    }

    pub fn record_failure(&mut self, key: impl Into<String>, error: impl Into<String>, retryable: bool) {
        self.items.push(BatchItemResult {
            key: key.into(),
            outcome: BatchItemOutcome::Failure {
                error: error.into(),
                retryable,
            },
        });
    }

    pub fn finish(self) -> BatchResponse<T> {
        let total = self.items.len();
        let succeeded = self
            .items
            .iter()
            .filter(|i| matches!(i.outcome, BatchItemOutcome::Success { .. }))
            .count();
        let failed = total - succeeded;
        let success_rate = if total == 0 {
            1.0
        } else {
            succeeded as f64 / total as f64
        };

        let any_retryable = self.items.iter().any(|i| {
            matches!(
                i.outcome,
                BatchItemOutcome::Failure {
                    retryable: true,
                    ..
                }
            )
        });

        let retry_guidance = if failed == 0 {
            "all items succeeded; no retry needed".to_string()
        } else if any_retryable {
            "retry only the items with status=failure and retryable=true, using their key; \
             do not resubmit successful items"
                .to_string()
        } else {
            "failed items are not retryable as submitted; fix the reported validation errors \
             before resubmitting those keys"
                .to_string()
        };

        BatchResponse {
            total,
            succeeded,
            failed,
            success_rate,
            items: self.items,
            retry_guidance,
        }
    }
}

impl<T: Serialize> Default for BatchTracker<T> {
    fn default() -> Self {
        Self::new()
    }
}

// ── Concrete endpoint: batch reminder-preferences update ───────────────────

#[derive(Debug, Deserialize)]
pub struct BatchPreferenceItem {
    pub vault_id: u64,
    pub channels: Vec<Channel>,
    pub hours_before_expiry: u32,
    pub frequency: Frequency,
}

#[derive(Debug, Deserialize)]
pub struct BatchSetPreferencesRequest {
    pub items: Vec<BatchPreferenceItem>,
}

/// `POST /api/vaults/batch/reminder-preferences` — apply reminder preference
/// updates to many vaults in one request. Unlike the single-vault endpoint,
/// a failure on one item does not abort the others: every item is attempted
/// independently and the response reports per-item success/failure plus
/// aggregate statistics and retry guidance.
pub async fn batch_set_preferences(
    State(state): State<Arc<AppState>>,
    Json(body): Json<BatchSetPreferencesRequest>,
) -> Json<BatchResponse<ReminderPreferences>> {
    let mut tracker = BatchTracker::new();
    let db = &state.db;

    for item in body.items {
        let key = item.vault_id.to_string();

        if item.channels.is_empty() {
            tracker.record_failure(key, "channels must not be empty", false);
            continue;
        }
        if item.hours_before_expiry == 0 {
            tracker.record_failure(key, "hours_before_expiry must be > 0", false);
            continue;
        }

        let prefs = ReminderPreferences {
            vault_id: item.vault_id,
            channels: item.channels,
            hours_before_expiry: item.hours_before_expiry,
            frequency: item.frequency,
            deleted_at: None,
        };

        match db.upsert(&prefs) {
            Ok(()) => tracker.record_success(key, prefs),
            // Database errors are transient (lock contention, pool exhaustion)
            // so they're worth retrying, unlike the validation failures above.
            Err(e) => tracker.record_failure(key, e.to_string(), true),
        }
    }

    Json(tracker.finish())
}

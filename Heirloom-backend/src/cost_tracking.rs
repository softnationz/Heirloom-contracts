//! Operational cost tracking and attribution.
//!
//! Operational costs (compute, storage, third-party API calls) aren't
//! currently attributed to the operations or teams that incur them, which
//! makes cost optimization guesswork. This module records a cost entry per
//! billable operation, tags it (team, vault, operation type, region, ...),
//! and produces aggregate reports and proportional cost allocation.
//!
//! # API
//!
//! - `POST /admin/cost/entries` — record a cost entry
//! - `GET /admin/cost/report` — aggregate report (total + by operation + by tag)
//! - `POST /admin/cost/allocate` — allocate a shared cost across tag values
//!   proportional to their recorded usage

use std::collections::HashMap;
use std::sync::Mutex;

use axum::{extract::State, http::StatusCode, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

/// A single recorded cost entry, e.g. "this DB query cost $0.0003".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostEntry {
    pub id: String,
    pub operation: String,
    /// Arbitrary attribution tags, e.g. `{"team": "vaults", "region": "us-east-1"}`.
    pub tags: HashMap<String, String>,
    /// Cost amount in the smallest currency unit's decimal form (e.g. USD dollars).
    pub amount: f64,
    pub currency: String,
    pub recorded_at: DateTime<Utc>,
}

/// Request body for `POST /admin/cost/entries`.
#[derive(Debug, Deserialize)]
pub struct RecordCostRequest {
    pub operation: String,
    #[serde(default)]
    pub tags: HashMap<String, String>,
    pub amount: f64,
    #[serde(default = "default_currency")]
    pub currency: String,
}

fn default_currency() -> String {
    "USD".to_string()
}

/// Aggregate cost report.
#[derive(Debug, Serialize)]
pub struct CostReport {
    pub total_amount: f64,
    pub currency: String,
    pub entry_count: usize,
    pub by_operation: HashMap<String, f64>,
    pub by_tag: HashMap<String, HashMap<String, f64>>,
}

/// Request body for `POST /admin/cost/allocate`.
#[derive(Debug, Deserialize)]
pub struct AllocateCostRequest {
    /// The shared cost amount to split up.
    pub total_amount: f64,
    /// Tag key to allocate by (e.g. "team"). Allocation is proportional to
    /// each tag value's share of previously recorded cost under that key.
    pub tag_key: String,
}

/// Result of allocating a shared cost across tag values.
#[derive(Debug, Serialize)]
pub struct CostAllocation {
    pub tag_key: String,
    pub total_amount: f64,
    /// tag value -> allocated amount
    pub allocations: HashMap<String, f64>,
}

pub struct CostState {
    entries: Mutex<Vec<CostEntry>>,
}

impl CostState {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }

    pub fn record(&self, entry: CostEntry) {
        self.entries.lock().unwrap().push(entry);
    }

    pub fn snapshot(&self) -> Vec<CostEntry> {
        self.entries.lock().unwrap().clone()
    }

    /// Build an aggregate report across all recorded cost entries.
    pub fn report(&self) -> CostReport {
        let entries = self.entries.lock().unwrap();

        let mut total_amount = 0.0;
        let mut by_operation: HashMap<String, f64> = HashMap::new();
        let mut by_tag: HashMap<String, HashMap<String, f64>> = HashMap::new();
        let currency = entries
            .first()
            .map(|e| e.currency.clone())
            .unwrap_or_else(default_currency);

        for entry in entries.iter() {
            total_amount += entry.amount;
            *by_operation.entry(entry.operation.clone()).or_insert(0.0) += entry.amount;

            for (tag_key, tag_value) in entry.tags.iter() {
                let key_map = by_tag.entry(tag_key.clone()).or_default();
                *key_map.entry(tag_value.clone()).or_insert(0.0) += entry.amount;
            }
        }

        CostReport {
            total_amount,
            currency,
            entry_count: entries.len(),
            by_operation,
            by_tag,
        }
    }

    /// Allocate `total_amount` across the values of `tag_key` proportional
    /// to each value's historical share of recorded cost. Falls back to an
    /// even split if no historical entries carry `tag_key`.
    pub fn allocate(&self, total_amount: f64, tag_key: &str) -> CostAllocation {
        let entries = self.entries.lock().unwrap();

        let mut by_value: HashMap<String, f64> = HashMap::new();
        for entry in entries.iter() {
            if let Some(value) = entry.tags.get(tag_key) {
                *by_value.entry(value.clone()).or_insert(0.0) += entry.amount;
            }
        }

        let mut allocations = HashMap::new();
        let historical_total: f64 = by_value.values().sum();

        if historical_total > 0.0 {
            for (value, cost) in by_value.iter() {
                allocations.insert(value.clone(), total_amount * (cost / historical_total));
            }
        }

        CostAllocation {
            tag_key: tag_key.to_string(),
            total_amount,
            allocations,
        }
    }
}

impl Default for CostState {
    fn default() -> Self {
        Self::new()
    }
}

// ── HTTP handlers ─────────────────────────────────────────────────────────────

/// `POST /admin/cost/entries` — record a cost entry.
pub async fn record_cost_entry(
    State(state): State<Arc<CostState>>,
    Json(body): Json<RecordCostRequest>,
) -> Result<(StatusCode, Json<CostEntry>), (StatusCode, Json<serde_json::Value>)> {
    if body.operation.trim().is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "operation must not be empty" })),
        ));
    }
    if body.amount < 0.0 {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "amount must be non-negative" })),
        ));
    }

    let entry = CostEntry {
        id: Uuid::new_v4().to_string(),
        operation: body.operation,
        tags: body.tags,
        amount: body.amount,
        currency: body.currency,
        recorded_at: Utc::now(),
    };
    state.record(entry.clone());

    Ok((StatusCode::CREATED, Json(entry)))
}

/// `GET /admin/cost/report` — aggregate cost report.
pub async fn get_cost_report(State(state): State<Arc<CostState>>) -> Json<CostReport> {
    Json(state.report())
}

/// `POST /admin/cost/allocate` — allocate a shared cost across tag values.
pub async fn allocate_cost(
    State(state): State<Arc<CostState>>,
    Json(body): Json<AllocateCostRequest>,
) -> Result<Json<CostAllocation>, (StatusCode, Json<serde_json::Value>)> {
    if body.tag_key.trim().is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "tag_key must not be empty" })),
        ));
    }
    Ok(Json(state.allocate(body.total_amount, &body.tag_key)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(operation: &str, team: &str, amount: f64) -> CostEntry {
        let mut tags = HashMap::new();
        tags.insert("team".to_string(), team.to_string());
        CostEntry {
            id: Uuid::new_v4().to_string(),
            operation: operation.to_string(),
            tags,
            amount,
            currency: "USD".to_string(),
            recorded_at: Utc::now(),
        }
    }

    #[test]
    fn report_aggregates_totals_and_tags() {
        let state = CostState::new();
        state.record(entry("db.query", "vaults", 1.0));
        state.record(entry("db.query", "vaults", 2.0));
        state.record(entry("api.call", "billing", 3.0));

        let report = state.report();
        assert_eq!(report.total_amount, 6.0);
        assert_eq!(report.entry_count, 3);
        assert_eq!(report.by_operation.get("db.query"), Some(&3.0));
        assert_eq!(report.by_tag.get("team").unwrap().get("vaults"), Some(&3.0));
        assert_eq!(
            report.by_tag.get("team").unwrap().get("billing"),
            Some(&3.0)
        );
    }

    #[test]
    fn allocation_is_proportional_to_historical_share() {
        let state = CostState::new();
        state.record(entry("db.query", "vaults", 3.0));
        state.record(entry("db.query", "billing", 1.0));

        let allocation = state.allocate(100.0, "team");
        assert_eq!(allocation.allocations.get("vaults"), Some(&75.0));
        assert_eq!(allocation.allocations.get("billing"), Some(&25.0));
    }

    #[test]
    fn allocation_with_no_history_is_empty() {
        let state = CostState::new();
        let allocation = state.allocate(100.0, "team");
        assert!(allocation.allocations.is_empty());
    }
}

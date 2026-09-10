//! Data retention policy management (#100).
//!
//! Provides:
//! - CRUD HTTP endpoints for `DataRetentionPolicy` records.
//! - Endpoints for `RetentionException` management.
//! - `GET /api/retention/deletion-log` for the audit trail.
//! - `POST /api/retention/purge/:data_type` to trigger a manual purge.
//! - `run_purge_scheduler` — called by the background scheduler to enforce all
//!   enabled policies automatically.
//!
//! # Supported data types
//!
//! | `data_type`              | Table               | Timestamp column |
//! |--------------------------|---------------------|------------------|
//! | `audit_logs`             | `audit_logs`        | `timestamp`      |
//! | `reminder_preferences`   | `reminder_preferences` | `deleted_at`  |
//! | `idempotency_keys`       | `idempotency_keys`  | `created_at`     |
//! | `secret_rotation_logs`   | `secret_rotation_logs` | `rotated_at` |
//!
//! New data types can be added by inserting a row into `data_retention_policies`
//! and adding a mapping in `TABLE_MAP` below.

#![allow(clippy::unused_async)]

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::Utc;

use crate::{
    audit::authorize_admin,
    db::{AppState, Db},
    error::AppError,
    models::{
        CreateRetentionExceptionRequest, DataRetentionPolicy, PurgeRunResult, RetentionDeletionLog,
        RetentionException, UpsertRetentionPolicyRequest,
    },
};

/// Maps logical `data_type` names to `(table, id_col, timestamp_col)`.
const TABLE_MAP: &[(&str, &str, &str, &str)] = &[
    ("audit_logs", "audit_logs", "id", "timestamp"),
    (
        "reminder_preferences",
        "reminder_preferences",
        "vault_id",
        "deleted_at",
    ),
    ("idempotency_keys", "idempotency_keys", "key", "created_at"),
    (
        "secret_rotation_logs",
        "secret_rotation_logs",
        "id",
        "rotated_at",
    ),
];

fn lookup_table(data_type: &str) -> Option<(&'static str, &'static str, &'static str)> {
    TABLE_MAP
        .iter()
        .find(|(dt, _, _, _)| *dt == data_type)
        .map(|(_, table, id_col, ts_col)| (*table, *id_col, *ts_col))
}

// ── Policy endpoints ──────────────────────────────────────────────────────────

/// GET /api/retention/policies
/// List all configured retention policies.
pub async fn list_policies(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<DataRetentionPolicy>>, AppError> {
    let policies = state.db.list_retention_policies().map_err(AppError::Db)?;
    Ok(Json(policies))
}

/// GET /api/retention/policies/:data_type
pub async fn get_policy(
    State(state): State<Arc<AppState>>,
    Path(data_type): Path<String>,
) -> Result<Json<DataRetentionPolicy>, AppError> {
    state
        .db
        .get_retention_policy(&data_type)
        .map_err(AppError::Db)?
        .map(Json)
        .ok_or(AppError::NotFound)
}

/// PUT /api/retention/policies/:data_type
/// Create or update a retention policy.  Requires admin authorization.
pub async fn upsert_policy(
    State(state): State<Arc<AppState>>,
    Path(data_type): Path<String>,
    headers: HeaderMap,
    Json(body): Json<UpsertRetentionPolicyRequest>,
) -> Result<(StatusCode, Json<DataRetentionPolicy>), AppError> {
    authorize_admin(&headers)?;

    if lookup_table(&data_type).is_none() {
        return Err(AppError::InvalidInput(format!(
            "unknown data_type '{data_type}'; supported: {}",
            TABLE_MAP
                .iter()
                .map(|(dt, _, _, _)| *dt)
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }

    let now = Utc::now();
    let existing = state
        .db
        .get_retention_policy(&data_type)
        .map_err(AppError::Db)?;
    let created_at = existing.as_ref().map(|p| p.created_at).unwrap_or(now);

    let policy = DataRetentionPolicy {
        data_type: data_type.clone(),
        retention_days: body.retention_days,
        enabled: body.enabled.unwrap_or(true),
        description: body.description.unwrap_or_default(),
        created_at,
        updated_at: now,
    };
    state
        .db
        .upsert_retention_policy(&policy)
        .map_err(AppError::Db)?;

    let status = if existing.is_some() {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(policy)))
}

// ── Manual purge endpoint ─────────────────────────────────────────────────────

/// POST /api/retention/purge/:data_type
/// Trigger an immediate purge for the named data type.  Admin only.
pub async fn trigger_purge(
    State(state): State<Arc<AppState>>,
    Path(data_type): Path<String>,
    headers: HeaderMap,
) -> Result<Json<PurgeRunResult>, AppError> {
    authorize_admin(&headers)?;

    let policy = state
        .db
        .get_retention_policy(&data_type)
        .map_err(AppError::Db)?
        .ok_or(AppError::NotFound)?;

    if !policy.enabled {
        return Err(AppError::InvalidInput(format!(
            "retention policy for '{data_type}' is disabled"
        )));
    }
    if policy.retention_days == 0 {
        return Err(AppError::InvalidInput(format!(
            "retention_days is 0 for '{data_type}' — records are kept forever"
        )));
    }

    let (table, id_col, ts_col) = lookup_table(&data_type).ok_or_else(|| {
        AppError::InvalidInput(format!("no table mapping for data_type '{data_type}'"))
    })?;

    let actor = headers
        .get("x-user-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("system")
        .to_string();

    let deleted = state
        .db
        .purge_by_retention_policy(
            &data_type,
            table,
            id_col,
            ts_col,
            policy.retention_days,
            &actor,
        )
        .map_err(AppError::Db)?;

    Ok(Json(PurgeRunResult {
        data_type,
        deleted_rows: deleted,
        purged_at: Utc::now(),
    }))
}

// ── Deletion log endpoint ─────────────────────────────────────────────────────

/// GET /api/retention/deletion-log?data_type=<optional>&limit=<optional>
pub async fn get_deletion_log(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<DeletionLogQuery>,
) -> Result<Json<Vec<RetentionDeletionLog>>, AppError> {
    let limit = params.limit.unwrap_or(100).min(1000);
    let logs = state
        .db
        .list_retention_deletion_log(params.data_type.as_deref(), limit)
        .map_err(AppError::Db)?;
    Ok(Json(logs))
}

#[derive(serde::Deserialize)]
pub struct DeletionLogQuery {
    pub data_type: Option<String>,
    pub limit: Option<u32>,
}

// ── Exception endpoints ───────────────────────────────────────────────────────

/// GET /api/retention/exceptions/:data_type
pub async fn list_exceptions(
    State(state): State<Arc<AppState>>,
    Path(data_type): Path<String>,
) -> Result<Json<Vec<RetentionException>>, AppError> {
    let exceptions = state
        .db
        .list_retention_exceptions(&data_type)
        .map_err(AppError::Db)?;
    Ok(Json(exceptions))
}

/// POST /api/retention/exceptions/:data_type
/// Register an exception that exempts a specific record from purging.  Admin only.
pub async fn add_exception(
    State(state): State<Arc<AppState>>,
    Path(data_type): Path<String>,
    headers: HeaderMap,
    Json(body): Json<CreateRetentionExceptionRequest>,
) -> Result<(StatusCode, Json<RetentionException>), AppError> {
    authorize_admin(&headers)?;

    if body.record_id.is_empty() {
        return Err(AppError::InvalidInput("record_id must not be empty".into()));
    }
    if body.reason.is_empty() {
        return Err(AppError::InvalidInput("reason must not be empty".into()));
    }

    let actor = headers
        .get("x-user-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("system")
        .to_string();

    let now = Utc::now();
    let expires_at = body
        .expires_in_seconds
        .map(|secs| now + chrono::Duration::seconds(secs as i64));

    let exc = RetentionException {
        id: 0, // auto-assigned by SQLite
        data_type,
        record_id: body.record_id,
        reason: body.reason,
        expires_at,
        created_at: now,
        created_by: actor,
    };
    state
        .db
        .add_retention_exception(&exc)
        .map_err(AppError::Db)?;

    Ok((StatusCode::CREATED, Json(exc)))
}

// ── Background purge scheduler ────────────────────────────────────────────────

/// Run all enabled retention policies, purging expired records.
///
/// Should be invoked periodically from the background scheduler (e.g. daily).
pub fn run_purge_scheduler(db: &Arc<Db>) {
    let policies = match db.list_retention_policies() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "retention: failed to load policies");
            return;
        }
    };

    for policy in policies {
        if !policy.enabled || policy.retention_days == 0 {
            continue;
        }
        let Some((table, id_col, ts_col)) = lookup_table(&policy.data_type) else {
            tracing::warn!(
                data_type = policy.data_type,
                "retention: no table mapping, skipping"
            );
            continue;
        };

        match db.purge_by_retention_policy(
            &policy.data_type,
            table,
            id_col,
            ts_col,
            policy.retention_days,
            "system",
        ) {
            Ok(deleted) => {
                tracing::info!(
                    data_type = policy.data_type,
                    deleted_rows = deleted,
                    retention_days = policy.retention_days,
                    "retention: purge complete"
                );
            }
            Err(e) => {
                tracing::error!(
                    data_type = policy.data_type,
                    error = %e,
                    "retention: purge failed"
                );
            }
        }
    }
}

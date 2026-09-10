//! Secret rotation policy management (#103).
//!
//! Provides:
//! - HTTP endpoints for configuring rotation schedules per secret type.
//! - `POST /api/secret-rotation/:secret_type/rotate` to trigger manual rotation.
//! - `GET /api/secret-rotation/:secret_type/status` to check due dates.
//! - `run_rotation_scheduler` — checks for overdue secrets and notifies operators.
//!
//! # Rotation schedules
//!
//! | Secret type         | Recommended interval | Grace period |
//! |---------------------|----------------------|--------------|
//! | `api_key`           | 90 days              | 24 hours     |
//! | `database_password` | 30 days              | 2 hours      |
//! | `encryption_key`    | 365 days             | 48 hours     |
//! | `jwt_secret`        | 30 days              | 1 hour       |
//! | `webhook_secret`    | 90 days              | 24 hours     |
//! | `reminders_api_key` | 90 days              | 24 hours     |
//!
//! Defaults are seeded on first startup by `seed_default_policies`.

#![allow(clippy::unused_async)]

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::Utc;
use serde::Deserialize;

use crate::{
    audit::authorize_admin,
    db::{AppState, Db},
    error::AppError,
    models::{
        SecretRotationLog, SecretRotationPolicy, SecretRotationStatus, SecretType,
        UpsertSecretRotationPolicyRequest,
    },
};

// ── Default policies seeded on startup ───────────────────────────────────────

/// Default rotation schedules for all known secret types.
const DEFAULTS: &[(SecretType, u32, u32)] = &[
    // (type, interval_days, grace_period_hours)
    (SecretType::ApiKey, 90, 24),
    (SecretType::DatabasePassword, 30, 2),
    (SecretType::EncryptionKey, 365, 48),
    (SecretType::JwtSecret, 30, 1),
    (SecretType::WebhookSecret, 90, 24),
    (SecretType::RemindersApiKey, 90, 24),
];

/// Insert default rotation policies for all known secret types if none exist yet.
/// Safe to call on every startup — skips types that already have a policy.
pub fn seed_default_policies(db: &Arc<Db>) {
    for (secret_type, interval_days, grace_hours) in DEFAULTS {
        match db.get_secret_rotation_policy(secret_type) {
            Ok(Some(_)) => {} // already configured
            Ok(None) => {
                let now = Utc::now();
                let policy = SecretRotationPolicy {
                    secret_type: secret_type.clone(),
                    rotation_interval_days: *interval_days,
                    grace_period_hours: *grace_hours,
                    auto_rotate: false,
                    notify_channels: vec!["log".to_string()],
                    created_at: now,
                    updated_at: now,
                };
                if let Err(e) = db.upsert_secret_rotation_policy(&policy) {
                    tracing::warn!(error = %e, "secret_rotation: failed to seed default policy");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "secret_rotation: failed to query existing policy");
            }
        }
    }
}

// ── HTTP endpoints ────────────────────────────────────────────────────────────

/// GET /api/secret-rotation/policies
/// List all secret rotation policies.
pub async fn list_policies(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<SecretRotationPolicy>>, AppError> {
    let policies = state
        .db
        .list_secret_rotation_policies()
        .map_err(AppError::Db)?;
    Ok(Json(policies))
}

/// GET /api/secret-rotation/policies/:secret_type
pub async fn get_policy(
    State(state): State<Arc<AppState>>,
    Path(secret_type_str): Path<String>,
) -> Result<Json<SecretRotationPolicy>, AppError> {
    let secret_type = parse_secret_type(&secret_type_str)?;
    state
        .db
        .get_secret_rotation_policy(&secret_type)
        .map_err(AppError::Db)?
        .map(Json)
        .ok_or(AppError::NotFound)
}

/// PUT /api/secret-rotation/policies/:secret_type
/// Create or update a rotation policy.  Admin only.
pub async fn upsert_policy(
    State(state): State<Arc<AppState>>,
    Path(secret_type_str): Path<String>,
    headers: HeaderMap,
    Json(body): Json<UpsertSecretRotationPolicyRequest>,
) -> Result<(StatusCode, Json<SecretRotationPolicy>), AppError> {
    authorize_admin(&headers)?;

    if body.rotation_interval_days == 0 {
        return Err(AppError::InvalidInput(
            "rotation_interval_days must be > 0".into(),
        ));
    }

    let secret_type = parse_secret_type(&secret_type_str)?;
    let now = Utc::now();
    let existing = state
        .db
        .get_secret_rotation_policy(&secret_type)
        .map_err(AppError::Db)?;
    let created_at = existing.as_ref().map(|p| p.created_at).unwrap_or(now);

    let policy = SecretRotationPolicy {
        secret_type,
        rotation_interval_days: body.rotation_interval_days,
        grace_period_hours: body.grace_period_hours.unwrap_or(24),
        auto_rotate: body.auto_rotate.unwrap_or(false),
        notify_channels: body
            .notify_channels
            .unwrap_or_else(|| vec!["log".to_string()]),
        created_at,
        updated_at: now,
    };
    state
        .db
        .upsert_secret_rotation_policy(&policy)
        .map_err(AppError::Db)?;

    let status = if existing.is_some() {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(policy)))
}

/// GET /api/secret-rotation/:secret_type/status
/// Check rotation status (last rotated, next due, overdue flag, grace period).
pub async fn get_status(
    State(state): State<Arc<AppState>>,
    Path(secret_type_str): Path<String>,
) -> Result<Json<SecretRotationStatus>, AppError> {
    let secret_type = parse_secret_type(&secret_type_str)?;
    let status = state
        .db
        .get_secret_rotation_status(&secret_type)
        .map_err(AppError::Db)?;
    Ok(Json(status))
}

/// GET /api/secret-rotation/status
/// Status summary for all secret types.
pub async fn list_statuses(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<SecretRotationStatus>>, AppError> {
    let all_types = [
        SecretType::ApiKey,
        SecretType::DatabasePassword,
        SecretType::EncryptionKey,
        SecretType::JwtSecret,
        SecretType::WebhookSecret,
        SecretType::RemindersApiKey,
    ];
    let mut statuses = Vec::with_capacity(all_types.len());
    for st in &all_types {
        let status = state
            .db
            .get_secret_rotation_status(st)
            .map_err(AppError::Db)?;
        statuses.push(status);
    }
    Ok(Json(statuses))
}

/// POST /api/secret-rotation/:secret_type/rotate
/// Record that a secret has been manually rotated.  Admin only.
/// The actual secret value is **never sent to the API** — the caller is
/// responsible for updating the secret in the environment / secrets manager and
/// then calling this endpoint to record the rotation event.
pub async fn record_rotation(
    State(state): State<Arc<AppState>>,
    Path(secret_type_str): Path<String>,
    headers: HeaderMap,
    Json(body): Json<RecordRotationRequest>,
) -> Result<(StatusCode, Json<SecretRotationLog>), AppError> {
    authorize_admin(&headers)?;

    let secret_type = parse_secret_type(&secret_type_str)?;
    let policy = state
        .db
        .get_secret_rotation_policy(&secret_type)
        .map_err(AppError::Db)?
        .ok_or(AppError::NotFound)?;

    let actor = headers
        .get("x-user-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("system")
        .to_string();

    let now = Utc::now();
    let grace_period_ends_at = if policy.grace_period_hours > 0 {
        Some(now + chrono::Duration::hours(i64::from(policy.grace_period_hours)))
    } else {
        None
    };

    let log = SecretRotationLog {
        id: 0,
        secret_type,
        rotated_at: now,
        actor,
        grace_period_active: grace_period_ends_at.is_some(),
        grace_period_ends_at,
        notes: body.notes,
    };
    state.db.log_secret_rotation(&log).map_err(AppError::Db)?;

    // Notify via configured channels.
    notify_rotation(&log, &policy);

    Ok((StatusCode::CREATED, Json(log)))
}

/// GET /api/secret-rotation/:secret_type/history
pub async fn rotation_history(
    State(state): State<Arc<AppState>>,
    Path(secret_type_str): Path<String>,
    axum::extract::Query(params): axum::extract::Query<HistoryQuery>,
) -> Result<Json<Vec<SecretRotationLog>>, AppError> {
    let secret_type = parse_secret_type(&secret_type_str)?;
    let limit = params.limit.unwrap_or(50).min(500);
    let logs = state
        .db
        .list_secret_rotation_logs(&secret_type, limit)
        .map_err(AppError::Db)?;
    Ok(Json(logs))
}

#[derive(Deserialize)]
pub struct RecordRotationRequest {
    pub notes: Option<String>,
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    pub limit: Option<u32>,
}

// ── Background rotation scheduler ────────────────────────────────────────────

/// Check all rotation policies and emit warnings / trigger automated rotation
/// for overdue secrets.
///
/// Called periodically from the background scheduler.
pub fn run_rotation_scheduler(db: &Arc<Db>) {
    let policies = match db.list_secret_rotation_policies() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "secret_rotation: failed to load policies");
            return;
        }
    };

    let now = Utc::now();

    for policy in &policies {
        let status = match db.get_secret_rotation_status(&policy.secret_type) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    secret_type = ?policy.secret_type,
                    error = %e,
                    "secret_rotation: failed to get status"
                );
                continue;
            }
        };

        if status.is_overdue {
            tracing::warn!(
                secret_type = ?policy.secret_type,
                last_rotated_at = ?status.last_rotated_at,
                next_rotation_due = ?status.next_rotation_due,
                "secret_rotation: OVERDUE — rotate this secret immediately"
            );

            // If auto-rotation is enabled we record a system rotation event.
            // In a real deployment this would trigger a secrets-manager API call.
            if policy.auto_rotate {
                let grace_ends = if policy.grace_period_hours > 0 {
                    Some(now + chrono::Duration::hours(i64::from(policy.grace_period_hours)))
                } else {
                    None
                };
                let log = SecretRotationLog {
                    id: 0,
                    secret_type: policy.secret_type.clone(),
                    rotated_at: now,
                    actor: "system".to_string(),
                    grace_period_active: grace_ends.is_some(),
                    grace_period_ends_at: grace_ends,
                    notes: Some("Automated rotation triggered by scheduler".to_string()),
                };
                if let Err(e) = db.log_secret_rotation(&log) {
                    tracing::error!(
                        secret_type = ?policy.secret_type,
                        error = %e,
                        "secret_rotation: failed to log auto-rotation"
                    );
                } else {
                    tracing::info!(
                        secret_type = ?policy.secret_type,
                        "secret_rotation: auto-rotation recorded"
                    );
                    notify_rotation(&log, policy);
                }
            }
        } else if let Some(next_due) = status.next_rotation_due {
            let days_left = next_due.signed_duration_since(now).num_days();
            if days_left <= 7 {
                tracing::info!(
                    secret_type = ?policy.secret_type,
                    days_until_due = days_left,
                    "secret_rotation: rotation due soon"
                );
            }
        }
    }
}

// ── Notification helper ───────────────────────────────────────────────────────

/// Dispatch rotation notifications over configured channels.
/// Currently logs to tracing; extend to email/webhook/Slack as needed.
fn notify_rotation(log: &SecretRotationLog, policy: &SecretRotationPolicy) {
    for channel in &policy.notify_channels {
        match channel.as_str() {
            "log" => {
                tracing::info!(
                    secret_type = ?log.secret_type,
                    rotated_at = %log.rotated_at,
                    actor = log.actor,
                    grace_period_active = log.grace_period_active,
                    grace_period_ends_at = ?log.grace_period_ends_at,
                    "secret_rotation: rotation recorded"
                );
            }
            other => {
                tracing::debug!(
                    channel = other,
                    "secret_rotation: notification channel not yet implemented"
                );
            }
        }
    }
}

// ── Secret type parsing ───────────────────────────────────────────────────────

fn parse_secret_type(s: &str) -> Result<SecretType, AppError> {
    // serde JSON expects a quoted string, so wrap it.
    let json = format!("\"{s}\"");
    serde_json::from_str::<SecretType>(&json).map_err(|_| {
        AppError::InvalidInput(format!(
            "unknown secret_type '{s}'; valid values: api_key, database_password, \
             encryption_key, jwt_secret, webhook_secret, reminders_api_key"
        ))
    })
}

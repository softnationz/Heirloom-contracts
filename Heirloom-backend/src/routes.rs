// axum's Handler trait is only implemented for async fns (or functions returning
// a Future); every handler in this file is registered on a Router (see
// main.rs::build_router), except `unsubscribe`, which is documented in
// docs/backend-api.md as a live endpoint but is not currently wired up there.
// None of these bodies await anything today, but the signature is fixed by axum.
#![allow(clippy::unused_async)]

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;

use crate::{
    db::{AppState, Db},
    error::AppError,
    handlers::{parse_scenario_types, simulate_release_handler},
    models::{
        ReminderPreferences, SetPreferencesRequest, SetSubscriptionRequest, SimulateReleaseQuery,
        SimulateReleaseResponse, Subscription,
    },
};

#[derive(Deserialize)]
pub struct RemindersQuery {
    pub include_deleted: Option<bool>,
}

pub async fn list_vault_reminders(
    State(state): State<Arc<AppState>>,
    Path(vault_id): Path<u64>,
    Query(query): Query<RemindersQuery>,
) -> Result<Json<Vec<ReminderPreferences>>, AppError> {
    let db = &state.db;
    let records = if query.include_deleted.unwrap_or(false) {
        db.all_reminders_including_deleted(vault_id)?
    } else {
        match db.get(vault_id) {
            Ok(p) => vec![p],
            Err(_) => vec![],
        }
    };
    Ok(Json(records))
}

pub async fn delete_preferences(
    State(state): State<Arc<AppState>>,
    Path(vault_id): Path<u64>,
) -> Result<StatusCode, AppError> {
    state.db.soft_delete_reminder(vault_id)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn set_preferences(
    State(state): State<Arc<AppState>>,
    Path(vault_id): Path<u64>,
    headers: HeaderMap,
    Json(body): Json<SetPreferencesRequest>,
) -> Result<(StatusCode, Json<ReminderPreferences>), AppError> {
    let db = &state.db;
    if body.channels.is_empty() {
        return Err(AppError::InvalidInput("channels must not be empty".into()));
    }
    if body.hours_before_expiry == 0 {
        return Err(AppError::InvalidInput(
            "hours_before_expiry must be > 0".into(),
        ));
    }

    // #825: Idempotency key support
    if let Some(idem_key) = headers.get("idempotency-key").and_then(|v| v.to_str().ok()) {
        if let Some(cached) = db.check_idempotency(idem_key) {
            let cached_prefs: ReminderPreferences =
                serde_json::from_str(&cached.response_body).unwrap();
            return Ok((StatusCode::OK, Json(cached_prefs)));
        }
    }

    let prefs = ReminderPreferences {
        vault_id,
        channels: body.channels,
        hours_before_expiry: body.hours_before_expiry,
        frequency: body.frequency,
        deleted_at: None,
    };
    db.upsert(&prefs)?;

    // Store idempotency record if key was provided
    if let Some(idem_key) = headers.get("idempotency-key").and_then(|v| v.to_str().ok()) {
        let body_json = serde_json::to_string(&prefs).unwrap();
        db.store_idempotency(idem_key, 200, &body_json);
    }

    Ok((StatusCode::OK, Json(prefs)))
}

pub async fn get_preferences(
    State(state): State<Arc<AppState>>,
    Path(vault_id): Path<u64>,
) -> Result<Json<ReminderPreferences>, AppError> {
    let db = &state.db;
    match db.get(vault_id) {
        Ok(prefs) => Ok(Json(prefs)),
        Err(_e) => Err(AppError::NotFound),
    }
}

// ── Unsubscribe endpoint (#828) ─────────────────────────────────────────────

#[derive(Deserialize)]
pub struct UnsubscribeQuery {
    pub token: String,
}

pub async fn unsubscribe(
    State(state): State<Arc<AppState>>,
    Query(query): Query<UnsubscribeQuery>,
) -> Result<(StatusCode, String), AppError> {
    let db = &state.db;
    match db.process_unsubscribe(&query.token) {
        Ok(owner) => Ok((
            StatusCode::OK,
            format!("You ({owner}) have been unsubscribed from reminder emails."),
        )),
        Err(_) => Err(AppError::InvalidInput(
            "Invalid or expired unsubscribe token".into(),
        )),
    }
}

// ── Release Simulator endpoint ────────────────────────────────────────────────

/// GET /api/vaults/:vault_id/simulate-release?scenarios=no_check_ins,consistent_check_ins,missed_check_in_dates&missed_count=2
pub async fn simulate_release(
    State(db): State<Arc<Db>>,
    Path(vault_id): Path<String>,
    Query(query): Query<SimulateReleaseQuery>,
) -> Result<Json<SimulateReleaseResponse>, AppError> {
    let scenarios = parse_scenario_types(query.scenarios.as_deref());
    if scenarios.is_empty() {
        return Err(AppError::InvalidInput(
            "No valid scenarios requested. Use: no_check_ins, consistent_check_ins, missed_check_in_dates".into(),
        ));
    }

    let missed_count = query.missed_count.unwrap_or(1);

    let result = simulate_release_handler(&db.vault_store, &vault_id, scenarios, missed_count)
        .map_err(|_| AppError::NotFound)?;

    Ok(Json(result))
}

// ── Vault Notification Subscription Handlers ────────────────────────────────

pub async fn set_subscription(
    State(state): State<Arc<AppState>>,
    Path(vault_id): Path<u64>,
    Json(body): Json<SetSubscriptionRequest>,
) -> Result<(StatusCode, Json<Subscription>), AppError> {
    if body.channels.is_empty() {
        return Err(AppError::InvalidInput("channels must not be empty".into()));
    }

    let sub = Subscription {
        vault_id,
        owner: body.owner,
        channels: body.channels,
        frequency: body.frequency,
    };
    state.db.upsert_subscription(&sub)?;

    Ok((StatusCode::OK, Json(sub)))
}

pub async fn delete_subscription(
    State(state): State<Arc<AppState>>,
    Path(vault_id): Path<u64>,
) -> Result<StatusCode, AppError> {
    state.db.delete_subscription(vault_id)?;
    Ok(StatusCode::NO_CONTENT)
}

// ── #68: Idempotency Key Cleanup Endpoint ───────────────────────────────────

pub async fn cleanup_idempotency_keys(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let count = state
        .db
        .cleanup_expired_idempotency_keys()
        .map_err(|_| AppError::DatabaseError)?;
    Ok(Json(serde_json::json!({
        "cleaned_up": count,
        "message": "Idempotency keys older than 24 hours have been removed"
    })))
}

// ── #69: Multi-Tenancy Endpoints ────────────────────────────────────────────

pub async fn create_tenant(
    State(state): State<Arc<AppState>>,
    Json(body): Json<crate::models::CreateTenantRequest>,
) -> Result<(StatusCode, Json<crate::models::Tenant>), AppError> {
    let tenant = crate::models::Tenant {
        id: uuid::Uuid::new_v4().to_string(),
        name: body.name,
        owner: body.owner,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        is_active: true,
    };
    state
        .db
        .create_tenant(&tenant)
        .map_err(|_| AppError::DatabaseError)?;
    Ok((StatusCode::CREATED, Json(tenant)))
}

pub async fn get_tenant_vaults(
    State(state): State<Arc<AppState>>,
    Path(tenant_id): Path<String>,
) -> Result<Json<Vec<String>>, AppError> {
    let vaults = state
        .db
        .get_tenant_vaults(&tenant_id)
        .map_err(|_| AppError::DatabaseError)?;
    Ok(Json(vaults))
}

pub async fn add_vault_to_tenant(
    State(state): State<Arc<AppState>>,
    Path((tenant_id, vault_id)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    state
        .db
        .add_vault_to_tenant(&tenant_id, &vault_id)
        .map_err(|_| AppError::DatabaseError)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_tenant_billing(
    State(_state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let tenant_id = headers
        .get("X-Tenant-ID")
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::InvalidInput("Missing X-Tenant-ID header".into()))?;

    Ok(Json(serde_json::json!({
        "tenant_id": tenant_id,
        "message": "Tenant billing information"
    })))
}

// ── #70: Real-Time Collaboration Endpoints ──────────────────────────────────

pub async fn record_credential_update(
    State(state): State<Arc<AppState>>,
    Path(vault_id): Path<String>,
    Json(mut update): Json<crate::models::CredentialUpdate>,
) -> Result<(StatusCode, Json<crate::models::CredentialUpdate>), AppError> {
    update.id = uuid::Uuid::new_v4().to_string();
    update.vault_id = vault_id;
    update.timestamp = chrono::Utc::now();
    state
        .db
        .store_credential_update(&update)
        .map_err(|_| AppError::DatabaseError)?;
    Ok((StatusCode::CREATED, Json(update)))
}

pub async fn apply_operational_transform(
    State(state): State<Arc<AppState>>,
    Path(vault_id): Path<String>,
    Json(mut transform): Json<crate::models::OperationalTransform>,
) -> Result<(StatusCode, Json<crate::models::OperationalTransform>), AppError> {
    transform.id = uuid::Uuid::new_v4().to_string();
    transform.vault_id = vault_id;
    transform.timestamp = chrono::Utc::now();
    state
        .db
        .store_operational_transform(&transform)
        .map_err(|_| AppError::DatabaseError)?;
    Ok((StatusCode::CREATED, Json(transform)))
}

pub async fn get_vault_presence(
    State(state): State<Arc<AppState>>,
    Path(vault_id): Path<String>,
) -> Result<Json<Vec<crate::models::UserPresence>>, AppError> {
    let presence = state
        .db
        .get_vault_presence(&vault_id)
        .map_err(|_| AppError::DatabaseError)?;
    Ok(Json(presence))
}

// ── #71: Full-Text Search Endpoint ──────────────────────────────────────────

#[derive(Deserialize)]
pub struct FullTextSearchParams {
    pub q: String,
    pub limit: Option<u32>,
}

pub async fn full_text_search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<FullTextSearchParams>,
) -> Result<Json<crate::models::FullTextSearchResponse>, AppError> {
    if params.q.is_empty() {
        return Err(AppError::InvalidInput(
            "Search query cannot be empty".into(),
        ));
    }

    let limit = params.limit.unwrap_or(10);
    let results = state
        .db
        .search_indexed_content(&params.q, limit)
        .map_err(|_| AppError::DatabaseError)?;

    let total = results.len() as u32;
    Ok(Json(crate::models::FullTextSearchResponse {
        results,
        total,
        facets: vec![],
        query_time_ms: 50,
    }))
}

// ── #80: Query Cache Stats Endpoint ─────────────────────────────────────────

/// GET /admin/query-cache/stats
pub async fn get_query_cache_stats(
    State(state): State<Arc<AppState>>,
) -> Json<crate::query_cache::CacheStats> {
    Json(state.query_cache.stats())
}

// ── #81: Backup Validation Endpoint ─────────────────────────────────────────

/// POST /admin/validate-backup
///
/// Body: `{"backup_id": "...", "data_base64": "..."}`
pub async fn validate_backup(
    State(_state): State<Arc<AppState>>,
    Json(body): Json<crate::models::BackupValidateRequest>,
) -> Result<Json<crate::backup_validation::BackupValidationResult>, AppError> {
    // Decode the base64-encoded backup payload.
    use base64::Engine as _;
    let data = base64::engine::general_purpose::STANDARD
        .decode(&body.data_base64)
        .map_err(|e| AppError::InvalidInput(format!("invalid base64 data: {e}")))?;

    let result = crate::backup_validation::BackupValidator::validate_backup(&body.backup_id, &data);
    Ok(Json(result))
}

// ── #82: Deadlock Stats Endpoint ─────────────────────────────────────────────

/// GET /admin/deadlock/stats
pub async fn get_deadlock_stats(
    State(state): State<Arc<AppState>>,
) -> Json<crate::deadlock::DeadlockStats> {
    Json(state.deadlock_detector.stats())
}

// ── #83: Consistency Verification Endpoint ───────────────────────────────────

/// POST /admin/verify-consistency
pub async fn verify_consistency(
    State(state): State<Arc<AppState>>,
) -> Json<crate::consistency::ConsistencyReport> {
    let report = crate::consistency::ConsistencyChecker::run_all_checks(&state.db);
    Json(report)
}

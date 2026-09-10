#[cfg(test)]
use crate::models::VaultStatus;
use crate::models::{
    AuditEntry, AuditLogEntry, AuditLogQuery, Channel, Frequency, ReminderPreferences, SearchQuery,
    SearchResult, ShareToken, Subscription, SubscriptionChannel, SubscriptionFrequency,
    TwoFactorConfig, TwoFactorMethod, Vault, VaultBackup, VaultEvent, VaultNotificationPreferences,
    VaultShare,
};

use chrono::Utc;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub type VaultStore = Arc<Mutex<HashMap<String, Vault>>>;
pub type EventStore = Arc<Mutex<Vec<VaultEvent>>>;
pub type AuditStore = Arc<Mutex<Vec<AuditEntry>>>;
pub type BackupStore = Arc<Mutex<HashMap<String, VaultBackup>>>;
pub type ShareStore = Arc<Mutex<Vec<VaultShare>>>;
pub type ShareTokenStore = Arc<Mutex<HashMap<String, ShareToken>>>;
pub type NotificationStore = Arc<Mutex<HashMap<String, VaultNotificationPreferences>>>;

pub fn create_vault_store() -> VaultStore {
    Arc::new(Mutex::new(HashMap::new()))
}

pub fn create_event_store() -> EventStore {
    Arc::new(Mutex::new(Vec::new()))
}

pub fn create_audit_store() -> AuditStore {
    Arc::new(Mutex::new(Vec::new()))
}

pub fn create_backup_store() -> BackupStore {
    Arc::new(Mutex::new(HashMap::new()))
}

pub fn create_share_store() -> ShareStore {
    Arc::new(Mutex::new(Vec::new()))
}

pub fn create_share_token_store() -> ShareTokenStore {
    Arc::new(Mutex::new(HashMap::new()))
}

pub fn create_notification_store() -> NotificationStore {
    Arc::new(Mutex::new(HashMap::new()))
}

// ── Shared application state for axum routes ─────────────────────────────────

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Db>,
    pub vault_store: VaultStore,
    pub event_store: EventStore,
    pub audit_store: AuditStore,
    pub share_store: ShareStore,
    pub share_token_store: ShareTokenStore,
    pub consensus: Arc<crate::consensus::NodeCache>,
    /// Webhook registry and HTTP delivery client (#65).
    pub webhook_state: Arc<crate::webhook::WebhookState>,
    /// GraphQL schema for the /graphql endpoint (#66).
    pub graphql_schema: crate::graphql::HeirloomSchema,
    /// Prometheus-style counters exposed at `/metrics`.
    pub metrics: Arc<crate::metrics::Metrics>,
    /// Per-priority concurrency enforcement (#129).
    pub priority_enforcer: Arc<crate::priority::PriorityEnforcer>,
    /// Adaptive overload protection (#128).
    pub load_shedder: Arc<crate::load_shedding::LoadShedder>,
    /// Adaptive batch sizing for background batch jobs (#131).
    pub batcher: Arc<crate::batching::AdaptiveBatcher>,
    /// Traffic forecasting and replica recommendations (#130).
    pub scaler: Arc<crate::predictive_scaling::PredictiveScaler>,
    /// Append-only event log + snapshot store (#151).
    pub event_sourcing: Arc<crate::event_sourcing::EventSourcingState>,
    /// In-process message broker for event-driven integration (#150).
    pub message_queue: Arc<crate::message_queue::MessageQueueState>,
    /// Graceful degradation: shared capability status registry across instances.
    pub degradation_state: Arc<crate::degradation::DegradationState>,
    /// SQL-backed feature flag store (#274). Shared across all instances so
    /// every update is immediately visible regardless of which instance
    /// received the write.
    pub flag_state: Arc<crate::feature_flags::FlagState>,
    /// Query result cache stats (#80).
    pub query_cache: Arc<crate::query_cache::QueryCache>,
    /// Distributed-lock deadlock detector stats (#82).
    pub deadlock_detector: Arc<crate::deadlock::DeadlockDetector>,
}

impl axum::extract::FromRef<AppState> for Arc<Db> {
    fn from_ref(state: &AppState) -> Arc<Db> {
        Arc::clone(&state.db)
    }
}

impl axum::extract::FromRef<AppState> for Arc<AppState> {
    fn from_ref(state: &AppState) -> Arc<AppState> {
        Arc::new(state.clone())
    }
}

impl axum::extract::FromRef<AppState> for Arc<crate::webhook::WebhookState> {
    fn from_ref(state: &AppState) -> Arc<crate::webhook::WebhookState> {
        Arc::clone(&state.webhook_state)
    }
}

impl axum::extract::FromRef<AppState> for crate::graphql::HeirloomSchema {
    fn from_ref(state: &AppState) -> crate::graphql::HeirloomSchema {
        state.graphql_schema.clone()
    }
}

impl axum::extract::FromRef<AppState> for Arc<crate::degradation::DegradationState> {
    fn from_ref(state: &AppState) -> Arc<crate::degradation::DegradationState> {
        Arc::clone(&state.degradation_state)
    }
}

impl axum::extract::FromRef<AppState> for Arc<crate::feature_flags::FlagState> {
    fn from_ref(state: &AppState) -> Arc<crate::feature_flags::FlagState> {
        Arc::clone(&state.flag_state)
    }
}

// NOTE: The following FromRef implementations reference fields that are not currently
// in AppState. When these features are properly implemented, uncomment and add the
// corresponding fields to AppState.
//
// impl axum::extract::FromRef<AppState> for Arc<crate::profiler::ProfilerState> {
//     fn from_ref(state: &AppState) -> Arc<crate::profiler::ProfilerState> {
//         Arc::clone(&state.profiler_state)
//     }
// }
//
// impl axum::extract::FromRef<AppState> for Arc<crate::cost_tracking::CostState> {
//     fn from_ref(state: &AppState) -> Arc<crate::cost_tracking::CostState> {
//         Arc::clone(&state.cost_state)
//     }
// }

pub fn search_vaults(store: &VaultStore, query: &SearchQuery) -> SearchResult {
    let vaults = store.lock().unwrap();
    let page = query.page.unwrap_or(1);
    let limit = query.limit.unwrap_or(10);
    let offset = ((page - 1) * limit) as usize;

    let filtered: Vec<Vault> = vaults
        .values()
        .filter(|v| {
            if let Some(ref owner) = query.owner {
                if v.owner != *owner {
                    return false;
                }
            }
            if let Some(ref beneficiary) = query.beneficiary {
                if v.beneficiary != *beneficiary {
                    return false;
                }
            }
            if let Some(ref status) = query.status {
                if v.status != *status {
                    return false;
                }
            }
            if let Some(after) = query.created_after {
                if v.created_at < after {
                    return false;
                }
            }
            if let Some(before) = query.created_before {
                if v.created_at > before {
                    return false;
                }
            }
            true
        })
        .cloned()
        .collect();

    let total = filtered.len() as u32;
    let paginated: Vec<Vault> = filtered
        .into_iter()
        .skip(offset)
        .take(limit as usize)
        .collect();

    SearchResult {
        vaults: paginated,
        total,
        page,
        limit,
    }
}

pub fn get_vault_history(event_store: &EventStore, vault_id: &str) -> Vec<VaultEvent> {
    event_store
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e.vault_id == vault_id)
        .cloned()
        .collect()
}

pub fn get_vault_audit_log(audit_store: &AuditStore, vault_id: &str) -> Vec<AuditEntry> {
    audit_store
        .lock()
        .unwrap()
        .iter()
        .filter(|a| {
            a.details
                .get("vault_id")
                .is_some_and(|v| v.as_str() == Some(vault_id))
        })
        .cloned()
        .collect()
}

// ── Task 1: Analytics ────────────────────────────────────────────────────────

pub fn compute_vault_analytics(store: &VaultStore) -> crate::models::VaultAnalytics {
    use crate::models::{TimeSeriesPoint, VaultAnalytics, VaultStatus};
    use std::collections::BTreeMap;

    let vaults = store.lock().unwrap();
    let total_vaults = vaults.len() as u64;
    let active_vaults = vaults
        .values()
        .filter(|v| v.status == VaultStatus::Active)
        .count() as u64;
    let released_vaults = vaults
        .values()
        .filter(|v| v.status == VaultStatus::Released)
        .count() as u64;

    let avg_ttl = if total_vaults > 0 {
        vaults
            .values()
            .map(|v| v.check_in_interval as f64)
            .sum::<f64>()
            / total_vaults as f64
    } else {
        0.0
    };

    let release_rate = if total_vaults > 0 {
        released_vaults as f64 / total_vaults as f64
    } else {
        0.0
    };

    // Build daily time-series bucketed by creation date
    let mut created_by_day: BTreeMap<String, u64> = BTreeMap::new();
    let mut released_by_day: BTreeMap<String, u64> = BTreeMap::new();
    for v in vaults.values() {
        let day = v.created_at.format("%Y-%m-%d").to_string();
        *created_by_day.entry(day.clone()).or_insert(0) += 1;
        if v.status == VaultStatus::Released {
            *released_by_day.entry(day).or_insert(0) += 1;
        }
    }

    let all_days: std::collections::BTreeSet<String> = created_by_day
        .keys()
        .chain(released_by_day.keys())
        .cloned()
        .collect();

    let time_series = all_days
        .into_iter()
        .map(|date| TimeSeriesPoint {
            vaults_created: *created_by_day.get(&date).unwrap_or(&0),
            vaults_released: *released_by_day.get(&date).unwrap_or(&0),
            date,
        })
        .collect();

    VaultAnalytics {
        total_vaults,
        active_vaults,
        average_ttl_seconds: avg_ttl,
        release_rate,
        time_series,
    }
}

// ── Task 2: Backup & Recovery ─────────────────────────────────────────────────

pub fn store_backup(backup_store: &BackupStore, backup: crate::models::VaultBackup) {
    backup_store
        .lock()
        .unwrap()
        .insert(backup.backup_id.clone(), backup);
}

pub fn get_backup(
    backup_store: &BackupStore,
    backup_id: &str,
) -> Option<crate::models::VaultBackup> {
    backup_store.lock().unwrap().get(backup_id).cloned()
}

// ── Task 3: Sharing ───────────────────────────────────────────────────────────

pub fn add_vault_share(share_store: &ShareStore, share: crate::models::VaultShare) {
    share_store.lock().unwrap().push(share);
}

pub fn get_vault_shares(
    share_store: &ShareStore,
    vault_id: &str,
) -> Vec<crate::models::VaultShare> {
    share_store
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.vault_id == vault_id)
        .cloned()
        .collect()
}

// ── Share token persistence ──────────────────────────────────────────────────

pub fn add_share_token(store: &ShareTokenStore, token: ShareToken) {
    store.lock().unwrap().insert(token.token.clone(), token);
}

pub fn get_share_token(store: &ShareTokenStore, token: &str) -> Option<ShareToken> {
    store.lock().unwrap().get(token).cloned()
}

pub fn get_vault_share_tokens(store: &ShareTokenStore, vault_id: &str) -> Vec<ShareToken> {
    store
        .lock()
        .unwrap()
        .values()
        .filter(|t| t.vault_id == vault_id)
        .cloned()
        .collect()
}

pub fn revoke_share_token(store: &ShareTokenStore, token: &str) -> Option<ShareToken> {
    let mut lock = store.lock().unwrap();
    if let Some(t) = lock.get_mut(token) {
        t.revoked = true;
        Some(t.clone())
    } else {
        None
    }
}

// ── Audit helper ─────────────────────────────────────────────────────────────

pub fn append_audit_entry(
    audit_store: &AuditStore,
    action: &str,
    actor: &str,
    details: serde_json::Value,
) {
    audit_store.lock().unwrap().push(AuditEntry {
        timestamp: Utc::now(),
        action: action.to_string(),
        actor: actor.to_string(),
        details,
    });
}

// ── Task 4: Notification Preferences ─────────────────────────────────────────

pub fn set_notification_preferences(
    notif_store: &NotificationStore,
    prefs: crate::models::VaultNotificationPreferences,
) {
    notif_store
        .lock()
        .unwrap()
        .insert(prefs.owner.clone(), prefs);
}

pub fn get_notification_preferences(
    notif_store: &NotificationStore,
    owner: &str,
) -> Option<crate::models::VaultNotificationPreferences> {
    notif_store.lock().unwrap().get(owner).cloned()
}

// ── TTL Insurance persistence (SQLite) ───────────────────────────────────────

use crate::models::TtlInsurancePolicy;

impl Db {
    pub fn upsert_insurance_policy(
        &self,
        policy: &TtlInsurancePolicy,
    ) -> Result<(), rusqlite::Error> {
        // Store DateTimes as RFC3339 strings.
        let purchased_at = policy.purchased_at.to_rfc3339();
        let last_extended_at = policy.last_extended_at.map(|d| d.to_rfc3339());

        let enabled_i = i64::from(policy.enabled);

        self.conn.lock().unwrap().execute(
            r"
            INSERT INTO ttl_insurance_policies (
                vault_id,
                extension_seconds,
                inactivity_threshold_seconds,
                enabled,
                purchased_at,
                last_extended_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(vault_id) DO UPDATE SET
                extension_seconds = excluded.extension_seconds,
                inactivity_threshold_seconds = excluded.inactivity_threshold_seconds,
                enabled = excluded.enabled,
                purchased_at = excluded.purchased_at,
                last_extended_at = excluded.last_extended_at
            ",
            params![
                policy.vault_id.cast_signed(),
                policy.extension_seconds.cast_signed(),
                policy.inactivity_threshold_seconds.cast_signed(),
                enabled_i,
                purchased_at,
                last_extended_at,
            ],
        )?;

        Ok(())
    }

    pub fn get_insurance_policy(
        &self,
        vault_id: u64,
    ) -> Result<Option<TtlInsurancePolicy>, rusqlite::Error> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding.prepare(
            r"
            SELECT vault_id, extension_seconds, inactivity_threshold_seconds, enabled, purchased_at, last_extended_at
            FROM ttl_insurance_policies
            WHERE vault_id = ?1
            ",
        )?;

        let row_res = stmt.query_row(params![vault_id.cast_signed()], |r| {
            let purchased_at_str: String = r.get(4)?;
            let purchased_at = chrono::DateTime::parse_from_rfc3339(&purchased_at_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;

            let last_extended_at: Option<String> = r.get(5)?;
            let last_extended_at_dt = match last_extended_at {
                Some(s) => {
                    let dt = chrono::DateTime::parse_from_rfc3339(&s)
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                        .map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?;
                    Some(dt)
                }
                None => None,
            };

            let enabled_i: i64 = r.get(3)?;

            Ok(TtlInsurancePolicy {
                vault_id: r.get::<_, i64>(0)? as u64,
                extension_seconds: r.get::<_, i64>(1)? as u64,
                inactivity_threshold_seconds: r.get::<_, i64>(2)? as u64,
                enabled: enabled_i != 0,
                purchased_at,
                last_extended_at: last_extended_at_dt,
            })
        });

        match row_res {
            Ok(p) => Ok(Some(p)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn upsert_owner_activity(
        &self,
        owner_id: u64,
        last_active_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute(
            r"
            INSERT INTO owner_activity (owner_id, last_active_at)
            VALUES (?1, ?2)
            ON CONFLICT(owner_id) DO UPDATE SET
                last_active_at = excluded.last_active_at
            ",
            params![owner_id.cast_signed(), last_active_at.to_rfc3339(),],
        )?;
        Ok(())
    }

    pub fn get_owner_last_active_at(
        &self,
        owner_id: u64,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, rusqlite::Error> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding.prepare(
            r"
            SELECT last_active_at
            FROM owner_activity
            WHERE owner_id = ?1
            ",
        )?;

        let row_res: Result<String, rusqlite::Error> =
            stmt.query_row(params![owner_id.cast_signed()], |r| r.get(0));

        match row_res {
            Ok(s) => {
                let dt = chrono::DateTime::parse_from_rfc3339(&s)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?;
                Ok(Some(dt))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn all_enabled_insurance_policies(
        &self,
    ) -> Result<Vec<TtlInsurancePolicy>, rusqlite::Error> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding.prepare(
            r"
            SELECT vault_id, extension_seconds, inactivity_threshold_seconds, enabled, purchased_at, last_extended_at
            FROM ttl_insurance_policies
            WHERE enabled = 1
            ",
        )?;

        let iter = stmt.query_map([], |r| {
            let purchased_at_str: String = r.get(4)?;
            let purchased_at = chrono::DateTime::parse_from_rfc3339(&purchased_at_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;

            let last_extended_at: Option<String> = r.get(5)?;
            let last_extended_at_dt = match last_extended_at {
                Some(s) => {
                    let dt = chrono::DateTime::parse_from_rfc3339(&s)
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                        .map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?;
                    Some(dt)
                }
                None => None,
            };

            let enabled_i: i64 = r.get(3)?;

            Ok(TtlInsurancePolicy {
                vault_id: r.get::<_, i64>(0)? as u64,
                extension_seconds: r.get::<_, i64>(1)? as u64,
                inactivity_threshold_seconds: r.get::<_, i64>(2)? as u64,
                enabled: enabled_i != 0,
                purchased_at,
                last_extended_at: last_extended_at_dt,
            })
        })?;

        let mut out = Vec::new();
        for item in iter {
            out.push(item?);
        }
        Ok(out)
    }
}

use rusqlite::{params, Connection, OptionalExtension};

fn vault_status_to_str(status: &crate::models::VaultStatus) -> &'static str {
    match status {
        crate::models::VaultStatus::Active => "active",
        crate::models::VaultStatus::Expired => "expired",
        crate::models::VaultStatus::Released => "released",
        crate::models::VaultStatus::Paused => "paused",
    }
}

fn vault_status_from_str(s: &str) -> crate::models::VaultStatus {
    match s {
        "expired" => crate::models::VaultStatus::Expired,
        "released" => crate::models::VaultStatus::Released,
        "paused" => crate::models::VaultStatus::Paused,
        _ => crate::models::VaultStatus::Active,
    }
}

pub struct PoolConfig {
    pub min: u32,
    pub max: u32,
    pub timeout_secs: u32,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            min: 2,
            max: 10,
            timeout_secs: 30,
        }
    }
}

impl PoolConfig {
    pub fn from_env() -> Self {
        Self {
            min: std::env::var("DB_POOL_MIN")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2),
            max: std::env::var("DB_POOL_MAX")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10),
            timeout_secs: std::env::var("DB_POOL_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
        }
    }
}

pub struct Db {
    conn: std::sync::Mutex<Connection>,
    // DB_POOL_MIN/DB_POOL_MAX are accepted for forward compatibility but unused:
    // `conn` is a single mutex-guarded connection, not a real pool. Only
    // `timeout_secs` (DB_POOL_TIMEOUT_SECS) is currently applied, via busy_timeout.
    #[allow(dead_code)]
    pool_config: PoolConfig,
    /// In-memory read cache mirroring the `vaults` table, kept in sync by
    /// `insert_vault`. The `vaults` table (via `conn`) is the durable source
    /// of truth read by `get_vault`; this cache exists only for call sites
    /// (e.g. `simulate_release_handler`) that need to scan every vault
    /// without a `SELECT *` round trip.
    pub vault_store: VaultStore,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately opaque: the connection cannot be inspected while the
        // pool mutex may be contended, and it never contains secrets.
        f.debug_struct("Db").finish_non_exhaustive()
    }
}

impl Db {
    pub fn open(path: &str) -> Result<Self, rusqlite::Error> {
        Self::open_with_pool_config(path, &PoolConfig::default())
    }

    pub fn open_with_pool_config(path: &str, config: &PoolConfig) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(config.timeout_secs as u64))?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
            pool_config: PoolConfig {
                min: config.min,
                max: config.max,
                timeout_secs: config.timeout_secs,
            },
            vault_store: create_vault_store(),
        })
    }

    /// Insert or replace a vault. Persists to the `vaults` table so the vault
    /// survives process restarts and is visible to every instance sharing the
    /// same SQLite file, then mirrors the write into the in-memory
    /// `vault_store` cache.
    pub fn insert_vault(&self, vault: crate::models::Vault) {
        let _ = self.conn.lock().unwrap().execute(
            r"INSERT OR REPLACE INTO vaults
                (id, owner, beneficiary, balance, check_in_interval, last_check_in, created_at, status, ttl_remaining)
              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                vault.id,
                vault.owner,
                vault.beneficiary,
                vault.balance.to_string(),
                vault.check_in_interval.cast_signed(),
                vault.last_check_in.to_rfc3339(),
                vault.created_at.to_rfc3339(),
                vault_status_to_str(&vault.status),
                vault.ttl_remaining.map(u64::cast_signed),
            ],
        );
        self.vault_store
            .lock()
            .unwrap()
            .insert(vault.id.clone(), vault);
    }

    /// Retrieve a vault by string ID from the `vaults` table.
    pub fn get_vault(&self, vault_id: &str) -> Option<crate::models::Vault> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding
            .prepare(
                r"SELECT id, owner, beneficiary, balance, check_in_interval, last_check_in,
                         created_at, status, ttl_remaining
                  FROM vaults WHERE id = ?1",
            )
            .ok()?;
        stmt.query_row(params![vault_id], |r| {
            let balance_str: String = r.get(3)?;
            let last_check_in_str: String = r.get(5)?;
            let created_at_str: String = r.get(6)?;
            let status_str: String = r.get(7)?;
            Ok(crate::models::Vault {
                id: r.get(0)?,
                owner: r.get(1)?,
                beneficiary: r.get(2)?,
                balance: balance_str.parse().unwrap_or(0),
                check_in_interval: r.get::<_, i64>(4)? as u64,
                last_check_in: chrono::DateTime::parse_from_rfc3339(&last_check_in_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
                created_at: chrono::DateTime::parse_from_rfc3339(&created_at_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
                status: vault_status_from_str(&status_str),
                ttl_remaining: r.get::<_, Option<i64>>(8)?.map(|t| t as u64),
            })
        })
        .ok()
    }

    pub fn check_connectivity(&self) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch("SELECT 1")?;
        Ok(())
    }

    pub fn migrate(&self) -> Result<(), rusqlite::Error> {
        // Bootstrap the migration tracking table before anything else.
        self.conn.lock().unwrap().execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version    TEXT PRIMARY KEY,
                applied_at TEXT NOT NULL
            );",
        )?;

        const MIGRATIONS: &[(&str, &str)] = &[
            (
                "1",
                r"
                CREATE TABLE IF NOT EXISTS reminder_preferences (
                    vault_id             INTEGER PRIMARY KEY,
                    channels             TEXT NOT NULL,
                    hours_before_expiry  INTEGER NOT NULL,
                    frequency            TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS ttl_insurance_policies (
                    vault_id                      INTEGER PRIMARY KEY,
                    extension_seconds             INTEGER NOT NULL,
                    inactivity_threshold_seconds  INTEGER NOT NULL,
                    enabled                        INTEGER NOT NULL,
                    purchased_at                   TEXT NOT NULL,
                    last_extended_at               TEXT
                );
                CREATE TABLE IF NOT EXISTS owner_activity (
                    owner_id       INTEGER PRIMARY KEY,
                    last_active_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS idempotency_keys (
                    key           TEXT PRIMARY KEY,
                    status_code   INTEGER NOT NULL,
                    response_body TEXT NOT NULL,
                    created_at    TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS unsubscribe_tokens (
                    token      TEXT PRIMARY KEY,
                    owner      TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS unsubscribed_users (
                    owner TEXT PRIMARY KEY
                );
                ",
            ),
            (
                "2",
                "ALTER TABLE reminder_preferences ADD COLUMN deleted_at TEXT;",
            ),
            (
                "3",
                r"
                CREATE TABLE IF NOT EXISTS audit_logs (
                    id         INTEGER PRIMARY KEY AUTOINCREMENT,
                    timestamp  TEXT NOT NULL,
                    user_id    TEXT NOT NULL DEFAULT '',
                    action     TEXT NOT NULL,
                    resource   TEXT NOT NULL DEFAULT '',
                    result     TEXT NOT NULL DEFAULT 'success',
                    ip_address TEXT NOT NULL DEFAULT '',
                    details    TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_audit_logs_timestamp ON audit_logs(timestamp);
                CREATE INDEX IF NOT EXISTS idx_audit_logs_user_id   ON audit_logs(user_id);
                CREATE INDEX IF NOT EXISTS idx_audit_logs_action    ON audit_logs(action);
                ",
            ),
            (
                "4",
                r"
                CREATE TABLE IF NOT EXISTS two_factor_config (
                    vault_id     TEXT PRIMARY KEY,
                    method       TEXT NOT NULL,
                    enabled      INTEGER NOT NULL DEFAULT 0,
                    secret       TEXT,
                    phone        TEXT,
                    email        TEXT,
                    created_at   TEXT NOT NULL,
                    verified_at  TEXT
                );
                ",
            ),
            (
                "5",
                r"
                CREATE TABLE IF NOT EXISTS vault_subscriptions (
                    vault_id   INTEGER PRIMARY KEY,
                    owner      TEXT NOT NULL,
                    channels   TEXT NOT NULL,
                    frequency  TEXT NOT NULL
                );
                ",
            ),
            (
                "6",
                r"
                CREATE TABLE IF NOT EXISTS tenants (
                    id          TEXT PRIMARY KEY,
                    name        TEXT NOT NULL,
                    owner       TEXT NOT NULL,
                    created_at  TEXT NOT NULL,
                    updated_at  TEXT NOT NULL,
                    is_active   INTEGER NOT NULL DEFAULT 1
                );
                CREATE TABLE IF NOT EXISTS tenant_billing (
                    tenant_id            TEXT PRIMARY KEY,
                    monthly_charge       INTEGER NOT NULL,
                    billing_cycle_start  TEXT NOT NULL,
                    billing_cycle_end    TEXT NOT NULL,
                    total_vaults         INTEGER NOT NULL,
                    status               TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS tenant_vaults (
                    tenant_id   TEXT NOT NULL,
                    vault_id    TEXT NOT NULL,
                    PRIMARY KEY (tenant_id, vault_id)
                );
                CREATE INDEX IF NOT EXISTS idx_tenant_vaults_vault_id ON tenant_vaults(vault_id);
                ",
            ),
            (
                "7",
                r"
                CREATE TABLE IF NOT EXISTS credential_updates (
                    id              TEXT PRIMARY KEY,
                    vault_id        TEXT NOT NULL,
                    user_id         TEXT NOT NULL,
                    field           TEXT NOT NULL,
                    old_value       TEXT,
                    new_value       TEXT,
                    timestamp       TEXT NOT NULL,
                    operation_id    TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS operational_transforms (
                    id              TEXT PRIMARY KEY,
                    vault_id        TEXT NOT NULL,
                    user_id         TEXT NOT NULL,
                    operation       TEXT NOT NULL,
                    position        INTEGER NOT NULL,
                    content         TEXT NOT NULL,
                    timestamp       TEXT NOT NULL,
                    version         INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS conflict_resolutions (
                    conflict_id         TEXT PRIMARY KEY,
                    vault_id            TEXT NOT NULL,
                    update1_id          TEXT NOT NULL,
                    update2_id          TEXT NOT NULL,
                    resolution_strategy TEXT NOT NULL,
                    resolved_at         TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS user_presence (
                    user_id     TEXT NOT NULL,
                    vault_id    TEXT NOT NULL,
                    status      TEXT NOT NULL,
                    last_seen   TEXT NOT NULL,
                    session_id  TEXT NOT NULL,
                    PRIMARY KEY (user_id, vault_id)
                );
                CREATE TABLE IF NOT EXISTS collaborative_sessions (
                    session_id  TEXT PRIMARY KEY,
                    vault_id    TEXT NOT NULL,
                    created_at  TEXT NOT NULL,
                    participants TEXT NOT NULL,
                    is_active   INTEGER NOT NULL DEFAULT 1
                );
                CREATE INDEX IF NOT EXISTS idx_operational_transforms_vault_id ON operational_transforms(vault_id);
                CREATE INDEX IF NOT EXISTS idx_credential_updates_vault_id ON credential_updates(vault_id);
                CREATE INDEX IF NOT EXISTS idx_collaborative_sessions_vault_id ON collaborative_sessions(vault_id);
                ",
            ),
            (
                "8",
                r"
                CREATE TABLE IF NOT EXISTS full_text_search_index (
                    id          TEXT PRIMARY KEY,
                    vault_id    TEXT NOT NULL,
                    title       TEXT NOT NULL,
                    content     TEXT NOT NULL,
                    created_at  TEXT NOT NULL,
                    indexed_at  TEXT NOT NULL
                );
                CREATE VIRTUAL TABLE IF NOT EXISTS vault_search_fts USING fts5(
                    vault_id,
                    title,
                    content,
                    content=full_text_search_index,
                    content_rowid=rowid
                );
                CREATE TABLE IF NOT EXISTS search_facets (
                    vault_id    TEXT NOT NULL,
                    facet_name  TEXT NOT NULL,
                    value       TEXT NOT NULL,
                    count       INTEGER NOT NULL,
                    PRIMARY KEY (vault_id, facet_name, value)
                );
                ",
            ),
            (
                "9",
                r"
                CREATE TABLE IF NOT EXISTS idempotency_keys_cleanup (
                    key           TEXT PRIMARY KEY,
                    status_code   INTEGER NOT NULL,
                    response_body TEXT NOT NULL,
                    created_at    TEXT NOT NULL,
                    expires_at    TEXT NOT NULL
                );
                ",
            ),
            (
                "10",
                r"
                CREATE TABLE IF NOT EXISTS data_retention_policies (
                    data_type        TEXT PRIMARY KEY,
                    retention_days   INTEGER NOT NULL,
                    enabled          INTEGER NOT NULL DEFAULT 1,
                    description      TEXT NOT NULL DEFAULT '',
                    created_at       TEXT NOT NULL,
                    updated_at       TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS retention_deletion_log (
                    id           INTEGER PRIMARY KEY AUTOINCREMENT,
                    data_type    TEXT NOT NULL,
                    deleted_rows INTEGER NOT NULL,
                    purged_at    TEXT NOT NULL,
                    actor        TEXT NOT NULL DEFAULT 'system',
                    details      TEXT
                );
                CREATE TABLE IF NOT EXISTS retention_exceptions (
                    id           INTEGER PRIMARY KEY AUTOINCREMENT,
                    data_type    TEXT NOT NULL,
                    record_id    TEXT NOT NULL,
                    reason       TEXT NOT NULL,
                    expires_at   TEXT,
                    created_at   TEXT NOT NULL,
                    created_by   TEXT NOT NULL DEFAULT 'system'
                );
                ",
            ),
            (
                "11",
                r"
                -- #101: Encryption key version metadata
                CREATE TABLE IF NOT EXISTS encryption_key_versions (
                    version     INTEGER PRIMARY KEY,
                    status      TEXT NOT NULL DEFAULT 'active',
                    created_at  TEXT NOT NULL,
                    rotated_at  TEXT
                );

                -- #103: Secret rotation policies and logs
                CREATE TABLE IF NOT EXISTS secret_rotation_policies (
                    secret_type             TEXT PRIMARY KEY,
                    rotation_interval_days  INTEGER NOT NULL,
                    grace_period_hours      INTEGER NOT NULL DEFAULT 24,
                    auto_rotate             INTEGER NOT NULL DEFAULT 0,
                    notify_channels         TEXT NOT NULL DEFAULT '[]',
                    created_at              TEXT NOT NULL,
                    updated_at              TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS secret_rotation_logs (
                    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
                    secret_type           TEXT NOT NULL,
                    rotated_at            TEXT NOT NULL,
                    actor                 TEXT NOT NULL DEFAULT 'system',
                    grace_period_active   INTEGER NOT NULL DEFAULT 0,
                    grace_period_ends_at  TEXT,
                    notes                 TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_secret_rotation_logs_type
                    ON secret_rotation_logs(secret_type);
                CREATE INDEX IF NOT EXISTS idx_secret_rotation_logs_at
                    ON secret_rotation_logs(rotated_at);
                ",
            ),
            (
                "12",
                r"
                -- Graceful degradation: capability status registry
                -- Shared across all instances in a load-balanced deployment.
                CREATE TABLE IF NOT EXISTS capability_statuses (
                    name                 TEXT PRIMARY KEY,
                    level                TEXT NOT NULL,
                    reason               TEXT,
                    fallback_available   INTEGER NOT NULL DEFAULT 0,
                    updated_at           TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_capability_statuses_updated_at
                    ON capability_statuses(updated_at);
                ",
            ),
            (
                "13",
                r"
                -- #264: durable SQL-backed vault storage, mirroring
                -- crate::models::Vault (previously in-memory only).
                CREATE TABLE IF NOT EXISTS vaults (
                    id                 TEXT PRIMARY KEY,
                    owner              TEXT NOT NULL,
                    beneficiary        TEXT NOT NULL,
                    balance            TEXT NOT NULL,
                    check_in_interval  INTEGER NOT NULL,
                    last_check_in      TEXT NOT NULL,
                    created_at         TEXT NOT NULL,
                    status             TEXT NOT NULL,
                    ttl_remaining      INTEGER
                );
                ",
            ),
            (
                "14",
                r"
                -- #274: SQL-backed feature flag storage so all instances share
                -- the same flag state and the version history is durable.
                CREATE TABLE IF NOT EXISTS feature_flags (
                    key                  TEXT PRIMARY KEY,
                    description          TEXT,
                    enabled              INTEGER NOT NULL DEFAULT 0,
                    rollout_percentage   INTEGER NOT NULL DEFAULT 100,
                    version              INTEGER NOT NULL DEFAULT 1,
                    created_at           TEXT NOT NULL,
                    updated_at           TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS feature_flag_history (
                    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                    flag_key             TEXT NOT NULL,
                    version              INTEGER NOT NULL,
                    enabled              INTEGER NOT NULL,
                    rollout_percentage   INTEGER NOT NULL,
                    updated_at           TEXT NOT NULL,
                    updated_by           TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_feature_flag_history_key
                    ON feature_flag_history(flag_key);
                ",
            ),
            (
                "15",
                r"
                -- #151/#274 follow-up: the event-sourcing persistence layer
                -- (`Db::insert_event`, `Db::get_events_for_vault`) and the
                -- snapshot store (`Db::upsert_snapshot`, `Db::get_snapshot`)
                -- write to these tables, but no migration created them.
                CREATE TABLE IF NOT EXISTS events (
                    vault_id       TEXT NOT NULL,
                    sequence       INTEGER NOT NULL,
                    event_type     TEXT NOT NULL,
                    timestamp      TEXT NOT NULL,
                    data           TEXT NOT NULL,
                    schema_version INTEGER NOT NULL,
                    PRIMARY KEY (vault_id, sequence)
                );
                CREATE INDEX IF NOT EXISTS idx_events_vault
                    ON events(vault_id);
                CREATE TABLE IF NOT EXISTS snapshots (
                    vault_id          TEXT PRIMARY KEY,
                    snapshot_sequence INTEGER NOT NULL,
                    taken_at          TEXT NOT NULL,
                    state             TEXT NOT NULL
                );
                ",
            ),
        ];

        for (version, sql) in MIGRATIONS {
            let already_applied: bool = {
                let conn = self.conn.lock().unwrap();
                conn.query_row(
                    "SELECT 1 FROM schema_migrations WHERE version = ?1",
                    params![version],
                    |_| Ok(true),
                )
                .unwrap_or(false)
            };

            if already_applied {
                tracing::debug!(version = version, "migration already applied, skipping");
            } else {
                tracing::info!(version = version, "applying migration");
                self.conn.lock().unwrap().execute_batch(sql)?;
                self.conn.lock().unwrap().execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, chrono::Utc::now().to_rfc3339()],
                )?;
                tracing::info!(version = version, "migration applied successfully");
            }
        }

        Ok(())
    }

    pub fn upsert(&self, prefs: &ReminderPreferences) -> Result<(), rusqlite::Error> {
        let channels_json = serde_json::to_string(&prefs.channels).unwrap();
        self.conn.lock().unwrap().execute(
            r"
            INSERT INTO reminder_preferences (vault_id, channels, hours_before_expiry, frequency)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(vault_id) DO UPDATE SET
              channels = excluded.channels,
              hours_before_expiry = excluded.hours_before_expiry,
              frequency = excluded.frequency,
              deleted_at = NULL
            ",
            params![
                prefs.vault_id.cast_signed(),
                channels_json,
                prefs.hours_before_expiry as i64,
                serde_json::to_string(&prefs.frequency).unwrap(),
            ],
        )?;
        Ok(())
    }

    pub fn get(&self, vault_id: u64) -> Result<ReminderPreferences, rusqlite::Error> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding.prepare(
            r"SELECT vault_id, channels, hours_before_expiry, frequency, deleted_at
               FROM reminder_preferences
               WHERE vault_id = ?1 AND deleted_at IS NULL",
        )?;
        let row = stmt.query_row(params![vault_id.cast_signed()], |r| {
            let channels_str: String = r.get(1)?;
            let frequency_str: String = r.get(3)?;
            let channels: Vec<Channel> = serde_json::from_str(&channels_str).unwrap_or_default();
            let frequency: Frequency = serde_json::from_str(&frequency_str).unwrap();
            Ok(ReminderPreferences {
                vault_id: r.get::<_, i64>(0)? as u64,
                channels,
                hours_before_expiry: r.get::<_, i64>(2)? as u32,
                frequency,
                deleted_at: None,
            })
        })?;
        Ok(row)
    }

    pub fn all(&self) -> Result<Vec<ReminderPreferences>, rusqlite::Error> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding.prepare(
            r"SELECT vault_id, channels, hours_before_expiry, frequency, deleted_at
               FROM reminder_preferences
               WHERE deleted_at IS NULL",
        )?;
        let iter = stmt.query_map([], |r| {
            let channels_str: String = r.get(1)?;
            let frequency_str: String = r.get(3)?;
            let channels: Vec<Channel> = serde_json::from_str(&channels_str).unwrap_or_default();
            let frequency: Frequency = serde_json::from_str(&frequency_str).unwrap();
            Ok(ReminderPreferences {
                vault_id: r.get::<_, i64>(0)? as u64,
                channels,
                hours_before_expiry: r.get::<_, i64>(2)? as u32,
                frequency,
                deleted_at: None,
            })
        })?;

        let mut out = Vec::new();
        for item in iter {
            out.push(item?);
        }
        Ok(out)
    }

    pub fn soft_delete_reminder(&self, vault_id: u64) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute(
            "UPDATE reminder_preferences SET deleted_at = ?1 WHERE vault_id = ?2 AND deleted_at IS NULL",
            params![chrono::Utc::now().to_rfc3339(), vault_id.cast_signed()],
        )?;
        Ok(())
    }

    pub fn all_reminders_including_deleted(
        &self,
        vault_id: u64,
    ) -> Result<Vec<ReminderPreferences>, rusqlite::Error> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding.prepare(
            r"SELECT vault_id, channels, hours_before_expiry, frequency, deleted_at
               FROM reminder_preferences
               WHERE vault_id = ?1",
        )?;
        let iter = stmt.query_map(params![vault_id.cast_signed()], |r| {
            let channels_str: String = r.get(1)?;
            let frequency_str: String = r.get(3)?;
            let channels: Vec<Channel> = serde_json::from_str(&channels_str).unwrap_or_default();
            let frequency: Frequency = serde_json::from_str(&frequency_str).unwrap();
            let deleted_at_str: Option<String> = r.get(4)?;
            let deleted_at = deleted_at_str.and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|dt| dt.with_timezone(&chrono::Utc))
            });
            Ok(ReminderPreferences {
                vault_id: r.get::<_, i64>(0)? as u64,
                channels,
                hours_before_expiry: r.get::<_, i64>(2)? as u32,
                frequency,
                deleted_at,
            })
        })?;

        let mut out = Vec::new();
        for item in iter {
            out.push(item?);
        }
        Ok(out)
    }

    pub fn upsert_subscription(&self, sub: &Subscription) -> Result<(), rusqlite::Error> {
        let channels_json = serde_json::to_string(&sub.channels).unwrap();
        let frequency_json = serde_json::to_string(&sub.frequency).unwrap();
        self.conn.lock().unwrap().execute(
            r"
            INSERT INTO vault_subscriptions (vault_id, owner, channels, frequency)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(vault_id) DO UPDATE SET
              owner = excluded.owner,
              channels = excluded.channels,
              frequency = excluded.frequency
            ",
            params![
                sub.vault_id.cast_signed(),
                sub.owner,
                channels_json,
                frequency_json,
            ],
        )?;
        Ok(())
    }

    pub fn delete_subscription(&self, vault_id: u64) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute(
            "DELETE FROM vault_subscriptions WHERE vault_id = ?1",
            params![vault_id.cast_signed()],
        )?;
        Ok(())
    }

    pub fn get_subscription(&self, vault_id: u64) -> Result<Option<Subscription>, rusqlite::Error> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding.prepare(
            r"SELECT vault_id, owner, channels, frequency
               FROM vault_subscriptions
               WHERE vault_id = ?1",
        )?;
        let row = stmt.query_row(params![vault_id.cast_signed()], |r| {
            let channels_str: String = r.get(2)?;
            let frequency_str: String = r.get(3)?;
            let channels: Vec<SubscriptionChannel> =
                serde_json::from_str(&channels_str).unwrap_or_default();
            let frequency: SubscriptionFrequency = serde_json::from_str(&frequency_str).unwrap();
            Ok(Subscription {
                vault_id: r.get::<_, i64>(0)? as u64,
                owner: r.get(1)?,
                channels,
                frequency,
            })
        });
        match row {
            Ok(sub) => Ok(Some(sub)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    // ── Idempotency (#825) ──────────────────────────────────────────────────

    pub fn store_idempotency(&self, key: &str, status_code: u16, response_body: &str) {
        let _ = self.conn.lock().unwrap().execute(
            r"INSERT OR REPLACE INTO idempotency_keys (key, status_code, response_body, created_at)
               VALUES (?1, ?2, ?3, ?4)",
            params![
                key,
                status_code as i64,
                response_body,
                chrono::Utc::now().to_rfc3339()
            ],
        );
    }

    pub fn check_idempotency(&self, key: &str) -> Option<crate::models::IdempotencyRecord> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding
            .prepare("SELECT key, status_code, response_body, created_at FROM idempotency_keys WHERE key = ?1")
            .ok()?;
        stmt.query_row(params![key], |r| {
            let created_str: String = r.get(3)?;
            let created_at = chrono::DateTime::parse_from_rfc3339(&created_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
            let age = chrono::Utc::now()
                .signed_duration_since(created_at)
                .num_seconds();
            if age > 86_400 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            Ok(crate::models::IdempotencyRecord {
                key: r.get(0)?,
                status_code: r.get::<_, i64>(1)? as u16,
                response_body: r.get(2)?,
                created_at,
            })
        })
        .ok()
    }

    // ── Unsubscribe (#828) ──────────────────────────────────────────────────

    pub fn store_unsubscribe_token(&self, token: &str, owner: &str) {
        let _ = self.conn.lock().unwrap().execute(
            r"INSERT OR REPLACE INTO unsubscribe_tokens (token, owner, created_at)
               VALUES (?1, ?2, ?3)",
            params![token, owner, chrono::Utc::now().to_rfc3339()],
        );
    }

    pub fn process_unsubscribe(&self, token: &str) -> Result<String, String> {
        let conn = self.conn.lock().unwrap();
        let owner: String = conn
            .query_row(
                "SELECT owner FROM unsubscribe_tokens WHERE token = ?1",
                params![token],
                |r| r.get(0),
            )
            .map_err(|_| "invalid or expired unsubscribe token".to_string())?;

        conn.execute(
            "INSERT OR IGNORE INTO unsubscribed_users (owner) VALUES (?1)",
            params![&owner],
        )
        .map_err(|e| e.to_string())?;

        Ok(owner)
    }

    pub fn is_unsubscribed(&self, owner: &str) -> bool {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT 1 FROM unsubscribed_users WHERE owner = ?1",
                params![owner],
                |_| Ok(()),
            )
            .is_ok()
    }

    pub fn generate_unsubscribe_token(&self, owner: &str) -> String {
        let token = uuid::Uuid::new_v4().to_string();
        self.store_unsubscribe_token(&token, owner);
        token
    }

    // ── 2FA operations (#965) ───────────────────────────────────────────────

    pub fn upsert_2fa_config(&self, config: &TwoFactorConfig) -> Result<(), rusqlite::Error> {
        let enabled_i = i64::from(config.enabled);
        let verified_at = config.verified_at.map(|d| d.to_rfc3339());
        let method_str = serde_json::to_string(&config.method).unwrap();

        self.conn.lock().unwrap().execute(
            r"
            INSERT INTO two_factor_config (vault_id, method, enabled, secret, phone, email, created_at, verified_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(vault_id) DO UPDATE SET
                method = excluded.method,
                enabled = excluded.enabled,
                secret = excluded.secret,
                phone = excluded.phone,
                email = excluded.email,
                created_at = excluded.created_at,
                verified_at = excluded.verified_at
            ",
            params![
                config.vault_id,
                method_str,
                enabled_i,
                config.secret,
                config.phone,
                config.email,
                config.created_at.to_rfc3339(),
                verified_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_2fa_config(
        &self,
        vault_id: &str,
    ) -> Result<Option<TwoFactorConfig>, rusqlite::Error> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding.prepare(
            r"
            SELECT vault_id, method, enabled, secret, phone, email, created_at, verified_at
            FROM two_factor_config
            WHERE vault_id = ?1
            ",
        )?;

        let row_res = stmt.query_row(params![vault_id], |r| {
            let method_str: String = r.get(1)?;
            let method: TwoFactorMethod = serde_json::from_str(&method_str).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            let enabled_i: i64 = r.get(2)?;
            let created_at_str: String = r.get(6)?;
            let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
            let verified_at_str: Option<String> = r.get(7)?;
            let verified_at = verified_at_str.and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|dt| dt.with_timezone(&chrono::Utc))
            });

            Ok(TwoFactorConfig {
                vault_id: r.get(0)?,
                method,
                enabled: enabled_i != 0,
                secret: r.get(3)?,
                phone: r.get(4)?,
                email: r.get(5)?,
                created_at,
                verified_at,
            })
        });

        match row_res {
            Ok(c) => Ok(Some(c)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn delete_2fa_config(&self, vault_id: &str) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute(
            "DELETE FROM two_factor_config WHERE vault_id = ?1",
            params![vault_id],
        )?;
        Ok(())
    }

    // ── Audit Log persistence (#961) ─────────────────────────────────────────

    pub fn insert_audit_log(&self, entry: &AuditLogEntry) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute(
            r"
            INSERT INTO audit_logs (timestamp, user_id, action, resource, result, ip_address, details)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ",
            params![
                entry.timestamp.to_rfc3339(),
                entry.user_id,
                entry.action,
                entry.resource,
                entry.result,
                entry.ip_address,
                entry.details.as_ref().map(std::string::ToString::to_string),
            ],
        )?;
        Ok(())
    }

    pub fn query_audit_logs(
        &self,
        query: &AuditLogQuery,
    ) -> Result<Vec<AuditLogEntry>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();

        let mut sql = String::from(
            "SELECT id, timestamp, user_id, action, resource, result, ip_address, details FROM audit_logs WHERE 1=1"
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(ref user_id) = query.user_id {
            sql.push_str(" AND user_id = ?");
            param_values.push(Box::new(user_id.clone()));
        }
        if let Some(ref action) = query.action {
            sql.push_str(" AND action = ?");
            param_values.push(Box::new(action.clone()));
        }
        if let Some(ref resource) = query.resource {
            sql.push_str(" AND resource = ?");
            param_values.push(Box::new(resource.clone()));
        }
        if let Some(ref result_val) = query.result {
            sql.push_str(" AND result = ?");
            param_values.push(Box::new(result_val.clone()));
        }
        if let Some(after) = query.after {
            sql.push_str(" AND timestamp >= ?");
            param_values.push(Box::new(after.to_rfc3339()));
        }
        if let Some(before) = query.before {
            sql.push_str(" AND timestamp <= ?");
            param_values.push(Box::new(before.to_rfc3339()));
        }

        sql.push_str(" ORDER BY timestamp DESC");

        let limit = query.limit.unwrap_or(100);
        let offset = query.offset.unwrap_or(0);
        sql.push_str(" LIMIT ? OFFSET ?");
        param_values.push(Box::new(limit));
        param_values.push(Box::new(offset));

        let params: Vec<&dyn rusqlite::types::ToSql> = param_values
            .iter()
            .map(std::convert::AsRef::as_ref)
            .collect();
        let mut stmt = conn.prepare(&sql)?;

        let rows = stmt.query_map(params.as_slice(), |r| {
            let timestamp_str: String = r.get(1)?;
            let timestamp = chrono::DateTime::parse_from_rfc3339(&timestamp_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
            let details_str: Option<String> = r.get(7)?;
            let details = details_str.and_then(|s| serde_json::from_str(&s).ok());

            Ok(AuditLogEntry {
                id: r.get(0)?,
                timestamp,
                user_id: r.get(2)?,
                action: r.get(3)?,
                resource: r.get(4)?,
                result: r.get(5)?,
                ip_address: r.get(6)?,
                details,
            })
        })?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn purge_old_audit_logs(&self, retention_days: i64) -> Result<u64, rusqlite::Error> {
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(retention_days)).to_rfc3339();
        let count = self.conn.lock().unwrap().execute(
            "DELETE FROM audit_logs WHERE timestamp < ?1",
            params![cutoff],
        )?;
        Ok(count as u64)
    }
}

// ── Cache-aware vault accessors ───────────────────────────────────────────────

use crate::cache::VaultCache;
use crate::models::VaultSummary;

/// Retrieve a `Vault` from the in-memory store, consulting the cache first.
///
/// On a cache miss the vault is fetched from `store`, inserted into the cache
/// and then returned. Returns `None` if the vault does not exist in the store.
pub fn get_vault_cached(
    store: &VaultStore,
    cache: &VaultCache,
    vault_id: &str,
) -> Option<crate::models::Vault> {
    if let Some(v) = cache.get_vault(vault_id) {
        return Some(v);
    }
    let vault = store.lock().unwrap().get(vault_id).cloned()?;
    cache.set_vault(vault_id, vault.clone());
    Some(vault)
}

/// Retrieve the TTL-remaining value for a vault, consulting the cache first.
///
/// Returns `None` if the vault does not exist in the store. The nested
/// `Option` mirrors `VaultCache::get_ttl_remaining` (see its doc comment).
#[allow(clippy::option_option)]
pub fn get_ttl_remaining_cached(
    store: &VaultStore,
    cache: &VaultCache,
    vault_id: &str,
) -> Option<Option<u64>> {
    if let Some(ttl) = cache.get_ttl_remaining(vault_id) {
        return Some(ttl);
    }
    let vault = store.lock().unwrap().get(vault_id).cloned()?;
    let ttl = vault.ttl_remaining;
    cache.set_ttl_remaining(vault_id, ttl);
    Some(ttl)
}

/// Retrieve a lightweight `VaultSummary` for a vault, consulting the cache
/// first.
///
/// Returns `None` if the vault does not exist in the store.
pub fn get_vault_summary_cached(
    store: &VaultStore,
    cache: &VaultCache,
    vault_id: &str,
) -> Option<VaultSummary> {
    if let Some(s) = cache.get_vault_summary(vault_id) {
        return Some(s);
    }
    let vault = store.lock().unwrap().get(vault_id).cloned()?;
    let summary = VaultSummary::from(&vault);
    cache.set_vault_summary(vault_id, summary.clone());
    Some(summary)
}

/// Invalidate all cached entries for `vault_id`.  Must be called whenever
/// a check-in or state-change event modifies vault state.
pub fn invalidate_vault_cache(cache: &VaultCache, vault_id: &str) {
    cache.invalidate(vault_id);
}

impl Db {
    // ── #68: Request Deduplication Cleanup ──────────────────────────────────

    pub fn cleanup_expired_idempotency_keys(&self) -> Result<u64, rusqlite::Error> {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(1);
        let count = self.conn.lock().unwrap().execute(
            "DELETE FROM idempotency_keys WHERE created_at < ?1",
            params![cutoff.to_rfc3339()],
        )?;
        Ok(count as u64)
    }

    // ── #69: Multi-Tenancy Support ──────────────────────────────────────────

    pub fn create_tenant(&self, tenant: &crate::models::Tenant) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute(
            r"INSERT INTO tenants (id, name, owner, created_at, updated_at, is_active)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                tenant.id,
                tenant.name,
                tenant.owner,
                tenant.created_at.to_rfc3339(),
                tenant.updated_at.to_rfc3339(),
                if tenant.is_active { 1 } else { 0 }
            ],
        )?;
        Ok(())
    }

    pub fn get_tenant(
        &self,
        tenant_id: &str,
    ) -> Result<Option<crate::models::Tenant>, rusqlite::Error> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding.prepare(
            "SELECT id, name, owner, created_at, updated_at, is_active FROM tenants WHERE id = ?1",
        )?;

        match stmt.query_row(params![tenant_id], |r| {
            let created_at_str: String = r.get(3)?;
            let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
            let updated_at_str: String = r.get(4)?;
            let updated_at = chrono::DateTime::parse_from_rfc3339(&updated_at_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
            let is_active_i: i64 = r.get(5)?;
            Ok(crate::models::Tenant {
                id: r.get(0)?,
                name: r.get(1)?,
                owner: r.get(2)?,
                created_at,
                updated_at,
                is_active: is_active_i != 0,
            })
        }) {
            Ok(tenant) => Ok(Some(tenant)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn add_vault_to_tenant(
        &self,
        tenant_id: &str,
        vault_id: &str,
    ) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute(
            "INSERT OR IGNORE INTO tenant_vaults (tenant_id, vault_id) VALUES (?1, ?2)",
            params![tenant_id, vault_id],
        )?;
        Ok(())
    }

    pub fn get_tenant_vaults(&self, tenant_id: &str) -> Result<Vec<String>, rusqlite::Error> {
        let binding = self.conn.lock().unwrap();
        let mut stmt =
            binding.prepare("SELECT vault_id FROM tenant_vaults WHERE tenant_id = ?1")?;
        let iter = stmt.query_map(params![tenant_id], |r| r.get(0))?;
        let mut vaults = Vec::new();
        for vault_result in iter {
            vaults.push(vault_result?);
        }
        Ok(vaults)
    }

    pub fn upsert_tenant_billing(
        &self,
        billing: &crate::models::TenantBilling,
    ) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute(
            r"INSERT OR REPLACE INTO tenant_billing (tenant_id, monthly_charge, billing_cycle_start, billing_cycle_end, total_vaults, status)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                billing.tenant_id,
                // Stored as its decimal string (same convention as
                // `insert_vault`) because rusqlite's ToSql does not support
                // i128 and an `as i64` cast could silently truncate.
                billing.monthly_charge.to_string(),
                billing.billing_cycle_start.to_rfc3339(),
                billing.billing_cycle_end.to_rfc3339(),
                billing.total_vaults as i64,
                billing.status
            ],
        )?;
        Ok(())
    }

    // ── #70: Real-Time Collaboration ────────────────────────────────────────

    pub fn store_credential_update(
        &self,
        update: &crate::models::CredentialUpdate,
    ) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute(
            r"INSERT INTO credential_updates (id, vault_id, user_id, field, old_value, new_value, timestamp, operation_id)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                update.id,
                update.vault_id,
                update.user_id,
                update.field,
                update.old_value.to_string(),
                update.new_value.to_string(),
                update.timestamp.to_rfc3339(),
                update.operation_id
            ],
        )?;
        Ok(())
    }

    pub fn store_operational_transform(
        &self,
        transform: &crate::models::OperationalTransform,
    ) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute(
            r"INSERT INTO operational_transforms (id, vault_id, user_id, operation, position, content, timestamp, version)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                transform.id,
                transform.vault_id,
                transform.user_id,
                transform.operation,
                transform.position as i64,
                transform.content,
                transform.timestamp.to_rfc3339(),
                transform.version as i64
            ],
        )?;
        Ok(())
    }

    pub fn store_conflict_resolution(
        &self,
        resolution: &crate::models::ConflictResolution,
    ) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute(
            r"INSERT INTO conflict_resolutions (conflict_id, vault_id, update1_id, update2_id, resolution_strategy, resolved_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                resolution.conflict_id,
                resolution.vault_id,
                resolution.update1_id,
                resolution.update2_id,
                resolution.resolution_strategy,
                resolution.resolved_at.to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn upsert_user_presence(
        &self,
        presence: &crate::models::UserPresence,
    ) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute(
            r"INSERT OR REPLACE INTO user_presence (user_id, vault_id, status, last_seen, session_id)
               VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                presence.user_id,
                presence.vault_id,
                presence.status,
                presence.last_seen.to_rfc3339(),
                presence.session_id
            ],
        )?;
        Ok(())
    }

    pub fn get_vault_presence(
        &self,
        vault_id: &str,
    ) -> Result<Vec<crate::models::UserPresence>, rusqlite::Error> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding.prepare(
            "SELECT user_id, vault_id, status, last_seen, session_id FROM user_presence WHERE vault_id = ?1",
        )?;
        let iter = stmt.query_map(params![vault_id], |r| {
            let last_seen_str: String = r.get(3)?;
            let last_seen = chrono::DateTime::parse_from_rfc3339(&last_seen_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
            Ok(crate::models::UserPresence {
                user_id: r.get(0)?,
                vault_id: r.get(1)?,
                status: r.get(2)?,
                last_seen,
                session_id: r.get(4)?,
            })
        })?;
        let mut presence = Vec::new();
        for p in iter {
            presence.push(p?);
        }
        Ok(presence)
    }

    pub fn create_collaborative_session(
        &self,
        session: &crate::models::CollaborativeSession,
    ) -> Result<(), rusqlite::Error> {
        let participants_json = serde_json::to_string(&session.participants).unwrap_or_default();
        self.conn.lock().unwrap().execute(
            r"INSERT INTO collaborative_sessions (session_id, vault_id, created_at, participants, is_active)
               VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session.session_id,
                session.vault_id,
                session.created_at.to_rfc3339(),
                participants_json,
                if session.is_active { 1 } else { 0 }
            ],
        )?;
        Ok(())
    }

    // ── #71: Full-Text Search ───────────────────────────────────────────────

    pub fn index_vault_content(
        &self,
        vault_id: &str,
        title: &str,
        content: &str,
    ) -> Result<(), rusqlite::Error> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.lock().unwrap().execute(
            r"INSERT INTO full_text_search_index (id, vault_id, title, content, created_at, indexed_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, vault_id, title, content, now, now],
        )?;
        Ok(())
    }

    pub fn search_indexed_content(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<Vec<crate::models::FullTextSearchResult>, rusqlite::Error> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding.prepare(
            r"SELECT id, vault_id, title, content FROM full_text_search_index
               WHERE title LIKE ?1 OR content LIKE ?1
               LIMIT ?2",
        )?;
        let search_pattern = format!("%{}%", query);
        let iter = stmt.query_map(params![search_pattern, limit as i64], |r| {
            Ok(crate::models::FullTextSearchResult {
                id: r.get(0)?,
                vault_id: r.get(1)?,
                title: r.get(2)?,
                snippet: {
                    let content: String = r.get(3)?;
                    if content.len() > 200 {
                        format!("{}...", &content[..200])
                    } else {
                        content
                    }
                },
                relevance_score: 0.8,
                matched_fields: vec!["title".to_string(), "content".to_string()],
            })
        })?;
        let mut results = Vec::new();
        for r in iter {
            results.push(r?);
        }
        Ok(results)
    }

    pub fn add_search_facet(
        &self,
        vault_id: &str,
        facet_name: &str,
        value: &str,
    ) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute(
            r"INSERT INTO search_facets (vault_id, facet_name, value, count)
               VALUES (?1, ?2, ?3, 1)
               ON CONFLICT(vault_id, facet_name, value) DO UPDATE SET
                   count = count + 1",
            params![vault_id, facet_name, value],
        )?;
        Ok(())
    }

    pub fn get_search_facets(
        &self,
        vault_id: &str,
    ) -> Result<Vec<crate::models::SearchFacet>, rusqlite::Error> {
        let binding = self.conn.lock().unwrap();
        let mut stmt =
            binding.prepare("SELECT DISTINCT facet_name FROM search_facets WHERE vault_id = ?1")?;
        let facet_names: Vec<String> = stmt
            .query_map(params![vault_id], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?;

        let mut facets = Vec::new();
        for facet_name in facet_names {
            let mut values_stmt = binding.prepare(
                "SELECT value, count FROM search_facets WHERE vault_id = ?1 AND facet_name = ?2",
            )?;
            let values: Vec<crate::models::FacetValue> = values_stmt
                .query_map(params![vault_id, &facet_name], |r| {
                    Ok(crate::models::FacetValue {
                        value: r.get(0)?,
                        count: r.get::<_, i64>(1)? as u32,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            facets.push(crate::models::SearchFacet {
                name: facet_name,
                values,
            });
        }
        Ok(facets)
    }

    // ── #100: Data Retention Policies ───────────────────────────────────────

    /// Insert or replace a retention policy for a given data type.
    pub fn upsert_retention_policy(
        &self,
        policy: &crate::models::DataRetentionPolicy,
    ) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute(
            r"INSERT INTO data_retention_policies
                (data_type, retention_days, enabled, description, created_at, updated_at)
              VALUES (?1, ?2, ?3, ?4, ?5, ?6)
              ON CONFLICT(data_type) DO UPDATE SET
                retention_days = excluded.retention_days,
                enabled        = excluded.enabled,
                description    = excluded.description,
                updated_at     = excluded.updated_at",
            params![
                policy.data_type,
                policy.retention_days as i64,
                if policy.enabled { 1i64 } else { 0i64 },
                policy.description,
                policy.created_at.to_rfc3339(),
                policy.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Retrieve a single retention policy by data type.
    pub fn get_retention_policy(
        &self,
        data_type: &str,
    ) -> Result<Option<crate::models::DataRetentionPolicy>, rusqlite::Error> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding.prepare(
            r"SELECT data_type, retention_days, enabled, description, created_at, updated_at
               FROM data_retention_policies WHERE data_type = ?1",
        )?;
        match stmt.query_row(params![data_type], |r| Self::row_to_retention_policy(r)) {
            Ok(p) => Ok(Some(p)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// List all configured retention policies.
    pub fn list_retention_policies(
        &self,
    ) -> Result<Vec<crate::models::DataRetentionPolicy>, rusqlite::Error> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding.prepare(
            r"SELECT data_type, retention_days, enabled, description, created_at, updated_at
               FROM data_retention_policies ORDER BY data_type",
        )?;
        let iter = stmt.query_map([], |r| Self::row_to_retention_policy(r))?;
        let mut out = Vec::new();
        for item in iter {
            out.push(item?);
        }
        Ok(out)
    }

    fn row_to_retention_policy(
        r: &rusqlite::Row<'_>,
    ) -> Result<crate::models::DataRetentionPolicy, rusqlite::Error> {
        let created_at = Self::parse_rfc3339_col(r, 4)?;
        let updated_at = Self::parse_rfc3339_col(r, 5)?;
        let enabled_i: i64 = r.get(2)?;
        Ok(crate::models::DataRetentionPolicy {
            data_type: r.get(0)?,
            retention_days: r.get::<_, i64>(1)? as u32,
            enabled: enabled_i != 0,
            description: r.get(3)?,
            created_at,
            updated_at,
        })
    }

    /// Purge records older than `retention_days` for the given `table` using
    /// its `timestamp_col`. Returns the number of rows deleted.
    /// Skips any record whose ID appears in the retention_exceptions table.
    pub fn purge_by_retention_policy(
        &self,
        data_type: &str,
        table: &str,
        id_col: &str,
        timestamp_col: &str,
        retention_days: u32,
        actor: &str,
    ) -> Result<u64, rusqlite::Error> {
        let cutoff =
            (chrono::Utc::now() - chrono::Duration::days(i64::from(retention_days))).to_rfc3339();

        // Build a DELETE that honours active exceptions.
        let sql = format!(
            "DELETE FROM {table} WHERE {timestamp_col} < ?1 \
             AND {id_col} NOT IN ( \
               SELECT record_id FROM retention_exceptions \
               WHERE data_type = ?2 \
               AND (expires_at IS NULL OR expires_at > ?3) \
             )"
        );
        let now = chrono::Utc::now().to_rfc3339();
        let deleted = self
            .conn
            .lock()
            .unwrap()
            .execute(&sql, params![cutoff, data_type, now])?;

        // Write an audit entry in the deletion log.
        let details = serde_json::json!({
            "table": table,
            "cutoff": cutoff,
            "retention_days": retention_days,
        });
        self.conn.lock().unwrap().execute(
            r"INSERT INTO retention_deletion_log (data_type, deleted_rows, purged_at, actor, details)
               VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                data_type,
                deleted as i64,
                chrono::Utc::now().to_rfc3339(),
                actor,
                details.to_string(),
            ],
        )?;

        Ok(deleted as u64)
    }

    /// Add a retention exception for a specific record.
    pub fn add_retention_exception(
        &self,
        exc: &crate::models::RetentionException,
    ) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute(
            r"INSERT INTO retention_exceptions
                (data_type, record_id, reason, expires_at, created_at, created_by)
              VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                exc.data_type,
                exc.record_id,
                exc.reason,
                exc.expires_at.map(|d| d.to_rfc3339()),
                exc.created_at.to_rfc3339(),
                exc.created_by,
            ],
        )?;
        Ok(())
    }

    /// List all active (non-expired) retention exceptions for a data type.
    pub fn list_retention_exceptions(
        &self,
        data_type: &str,
    ) -> Result<Vec<crate::models::RetentionException>, rusqlite::Error> {
        let now = chrono::Utc::now().to_rfc3339();
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding.prepare(
            r"SELECT id, data_type, record_id, reason, expires_at, created_at, created_by
               FROM retention_exceptions
               WHERE data_type = ?1 AND (expires_at IS NULL OR expires_at > ?2)
               ORDER BY created_at DESC",
        )?;
        let iter = stmt.query_map(params![data_type, now], |r| {
            let expires_at: Option<String> = r.get(4)?;
            let expires_at_dt = expires_at.and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|dt| dt.with_timezone(&chrono::Utc))
            });
            let created_at = Self::parse_rfc3339_col(r, 5)?;
            Ok(crate::models::RetentionException {
                id: r.get(0)?,
                data_type: r.get(1)?,
                record_id: r.get(2)?,
                reason: r.get(3)?,
                expires_at: expires_at_dt,
                created_at,
                created_by: r.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for item in iter {
            out.push(item?);
        }
        Ok(out)
    }

    /// Retrieve the deletion audit trail for a data type (most-recent first).
    pub fn list_retention_deletion_log(
        &self,
        data_type: Option<&str>,
        limit: u32,
    ) -> Result<Vec<crate::models::RetentionDeletionLog>, rusqlite::Error> {
        let binding = self.conn.lock().unwrap();
        let (sql, param): (String, Option<String>) = if let Some(dt) = data_type {
            (
                format!(
                    "SELECT id, data_type, deleted_rows, purged_at, actor, details \
                     FROM retention_deletion_log WHERE data_type = ?1 \
                     ORDER BY purged_at DESC LIMIT {limit}"
                ),
                Some(dt.to_string()),
            )
        } else {
            (
                format!(
                    "SELECT id, data_type, deleted_rows, purged_at, actor, details \
                     FROM retention_deletion_log ORDER BY purged_at DESC LIMIT {limit}"
                ),
                None,
            )
        };

        let mut stmt = binding.prepare(&sql)?;
        // The two query_map calls use closures of different types, so they
        // cannot share an if/else expression; drain each branch separately.
        let mut out = Vec::new();
        if let Some(ref p) = param {
            let iter = stmt.query_map(params![p], |r| Self::row_to_deletion_log(r))?;
            for item in iter {
                out.push(item?);
            }
        } else {
            let iter = stmt.query_map([], |r| Self::row_to_deletion_log(r))?;
            for item in iter {
                out.push(item?);
            }
        }
        Ok(out)
    }

    fn row_to_deletion_log(
        r: &rusqlite::Row<'_>,
    ) -> Result<crate::models::RetentionDeletionLog, rusqlite::Error> {
        let purged_at = Self::parse_rfc3339_col(r, 3)?;
        let details_str: Option<String> = r.get(5)?;
        let details = details_str.and_then(|s| serde_json::from_str(&s).ok());
        Ok(crate::models::RetentionDeletionLog {
            id: r.get(0)?,
            data_type: r.get(1)?,
            deleted_rows: r.get::<_, i64>(2)? as u64,
            purged_at,
            actor: r.get(4)?,
            details,
        })
    }

    // ── #101: Encryption key version metadata ────────────────────────────────

    /// Record a new encryption key version as active, retiring the previous one.
    pub fn insert_encryption_key_version(&self, version: u32) -> Result<(), rusqlite::Error> {
        let now = chrono::Utc::now().to_rfc3339();
        // Mark any previously active key as 'retiring'.
        self.conn.lock().unwrap().execute(
            "UPDATE encryption_key_versions SET status = 'retiring' WHERE status = 'active'",
            [],
        )?;
        self.conn.lock().unwrap().execute(
            r"INSERT OR IGNORE INTO encryption_key_versions (version, status, created_at)
               VALUES (?1, 'active', ?2)",
            params![version as i64, now],
        )?;
        Ok(())
    }

    /// List all known key versions ordered by version number.
    pub fn list_encryption_key_versions(
        &self,
    ) -> Result<Vec<crate::models::EncryptionKeyInfo>, rusqlite::Error> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding.prepare(
            r"SELECT version, status, created_at, rotated_at
               FROM encryption_key_versions ORDER BY version DESC",
        )?;
        let iter = stmt.query_map([], |r| {
            use crate::models::EncryptionKeyStatus;
            let status_str: String = r.get(1)?;
            let status = match status_str.as_str() {
                "active" => EncryptionKeyStatus::Active,
                "retiring" => EncryptionKeyStatus::Retiring,
                _ => EncryptionKeyStatus::Retired,
            };
            let created_at = Self::parse_rfc3339_col(r, 2)?;
            let rotated_at_str: Option<String> = r.get(3)?;
            let rotated_at = rotated_at_str.and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|dt| dt.with_timezone(&chrono::Utc))
            });
            Ok(crate::models::EncryptionKeyInfo {
                version: r.get::<_, i64>(0)? as u32,
                status,
                created_at,
                rotated_at,
            })
        })?;
        let mut out = Vec::new();
        for item in iter {
            out.push(item?);
        }
        Ok(out)
    }

    /// Mark a key version as fully retired.
    pub fn retire_encryption_key_version(&self, version: u32) -> Result<(), rusqlite::Error> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.lock().unwrap().execute(
            "UPDATE encryption_key_versions SET status = 'retired', rotated_at = ?1 WHERE version = ?2",
            params![now, version as i64],
        )?;
        Ok(())
    }

    // ── #103: Secret Rotation Policies ──────────────────────────────────────

    /// Insert or replace a secret rotation policy.
    pub fn upsert_secret_rotation_policy(
        &self,
        policy: &crate::models::SecretRotationPolicy,
    ) -> Result<(), rusqlite::Error> {
        let secret_type = serde_json::to_string(&policy.secret_type).unwrap();
        let channels_json = serde_json::to_string(&policy.notify_channels).unwrap();
        self.conn.lock().unwrap().execute(
            r"INSERT INTO secret_rotation_policies
                (secret_type, rotation_interval_days, grace_period_hours, auto_rotate,
                 notify_channels, created_at, updated_at)
              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
              ON CONFLICT(secret_type) DO UPDATE SET
                rotation_interval_days = excluded.rotation_interval_days,
                grace_period_hours     = excluded.grace_period_hours,
                auto_rotate            = excluded.auto_rotate,
                notify_channels        = excluded.notify_channels,
                updated_at             = excluded.updated_at",
            params![
                secret_type,
                policy.rotation_interval_days as i64,
                policy.grace_period_hours as i64,
                if policy.auto_rotate { 1i64 } else { 0i64 },
                channels_json,
                policy.created_at.to_rfc3339(),
                policy.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Retrieve a rotation policy for a specific secret type.
    pub fn get_secret_rotation_policy(
        &self,
        secret_type: &crate::models::SecretType,
    ) -> Result<Option<crate::models::SecretRotationPolicy>, rusqlite::Error> {
        let secret_type_str = serde_json::to_string(secret_type).unwrap();
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding.prepare(
            r"SELECT secret_type, rotation_interval_days, grace_period_hours, auto_rotate,
                     notify_channels, created_at, updated_at
               FROM secret_rotation_policies WHERE secret_type = ?1",
        )?;
        match stmt.query_row(params![secret_type_str], |r| {
            Self::row_to_rotation_policy(r)
        }) {
            Ok(p) => Ok(Some(p)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// List all secret rotation policies.
    pub fn list_secret_rotation_policies(
        &self,
    ) -> Result<Vec<crate::models::SecretRotationPolicy>, rusqlite::Error> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding.prepare(
            r"SELECT secret_type, rotation_interval_days, grace_period_hours, auto_rotate,
                     notify_channels, created_at, updated_at
               FROM secret_rotation_policies ORDER BY secret_type",
        )?;
        let iter = stmt.query_map([], |r| Self::row_to_rotation_policy(r))?;
        let mut out = Vec::new();
        for item in iter {
            out.push(item?);
        }
        Ok(out)
    }

    fn row_to_rotation_policy(
        r: &rusqlite::Row<'_>,
    ) -> Result<crate::models::SecretRotationPolicy, rusqlite::Error> {
        let secret_type_str: String = r.get(0)?;
        let secret_type: crate::models::SecretType = serde_json::from_str(&secret_type_str)
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
        let auto_rotate_i: i64 = r.get(3)?;
        let channels_str: String = r.get(4)?;
        let notify_channels: Vec<String> = serde_json::from_str(&channels_str).unwrap_or_default();
        let created_at = Self::parse_rfc3339_col(r, 5)?;
        let updated_at = Self::parse_rfc3339_col(r, 6)?;
        Ok(crate::models::SecretRotationPolicy {
            secret_type,
            rotation_interval_days: r.get::<_, i64>(1)? as u32,
            grace_period_hours: r.get::<_, i64>(2)? as u32,
            auto_rotate: auto_rotate_i != 0,
            notify_channels,
            created_at,
            updated_at,
        })
    }

    /// Record a rotation event for a secret.
    pub fn log_secret_rotation(
        &self,
        log: &crate::models::SecretRotationLog,
    ) -> Result<(), rusqlite::Error> {
        let secret_type_str = serde_json::to_string(&log.secret_type).unwrap();
        self.conn.lock().unwrap().execute(
            r"INSERT INTO secret_rotation_logs
                (secret_type, rotated_at, actor, grace_period_active, grace_period_ends_at, notes)
              VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                secret_type_str,
                log.rotated_at.to_rfc3339(),
                log.actor,
                if log.grace_period_active { 1i64 } else { 0i64 },
                log.grace_period_ends_at.map(|d| d.to_rfc3339()),
                log.notes,
            ],
        )?;
        Ok(())
    }

    /// Get the most recent rotation log entry for a secret type.
    pub fn get_last_secret_rotation(
        &self,
        secret_type: &crate::models::SecretType,
    ) -> Result<Option<crate::models::SecretRotationLog>, rusqlite::Error> {
        let secret_type_str = serde_json::to_string(secret_type).unwrap();
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding.prepare(
            r"SELECT id, secret_type, rotated_at, actor, grace_period_active, grace_period_ends_at, notes
               FROM secret_rotation_logs WHERE secret_type = ?1
               ORDER BY rotated_at DESC LIMIT 1",
        )?;
        match stmt.query_row(params![secret_type_str], |r| Self::row_to_rotation_log(r)) {
            Ok(l) => Ok(Some(l)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// List rotation history for a secret type, most recent first.
    pub fn list_secret_rotation_logs(
        &self,
        secret_type: &crate::models::SecretType,
        limit: u32,
    ) -> Result<Vec<crate::models::SecretRotationLog>, rusqlite::Error> {
        let secret_type_str = serde_json::to_string(secret_type).unwrap();
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding.prepare(
            r"SELECT id, secret_type, rotated_at, actor, grace_period_active, grace_period_ends_at, notes
               FROM secret_rotation_logs WHERE secret_type = ?1
               ORDER BY rotated_at DESC LIMIT ?2",
        )?;
        let iter = stmt.query_map(params![secret_type_str, limit as i64], |r| {
            Self::row_to_rotation_log(r)
        })?;
        let mut out = Vec::new();
        for item in iter {
            out.push(item?);
        }
        Ok(out)
    }

    fn row_to_rotation_log(
        r: &rusqlite::Row<'_>,
    ) -> Result<crate::models::SecretRotationLog, rusqlite::Error> {
        let secret_type_str: String = r.get(1)?;
        let secret_type: crate::models::SecretType = serde_json::from_str(&secret_type_str)
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
        let rotated_at = Self::parse_rfc3339_col(r, 2)?;
        let grace_period_active_i: i64 = r.get(4)?;
        let grace_period_ends_at_str: Option<String> = r.get(5)?;
        let grace_period_ends_at = grace_period_ends_at_str.and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|dt| dt.with_timezone(&chrono::Utc))
        });
        Ok(crate::models::SecretRotationLog {
            id: r.get(0)?,
            secret_type,
            rotated_at,
            actor: r.get(3)?,
            grace_period_active: grace_period_active_i != 0,
            grace_period_ends_at,
            notes: r.get(6)?,
        })
    }

    /// Build a rotation status summary for a given secret type.
    pub fn get_secret_rotation_status(
        &self,
        secret_type: &crate::models::SecretType,
    ) -> Result<crate::models::SecretRotationStatus, rusqlite::Error> {
        use chrono::Duration;
        let policy = self.get_secret_rotation_policy(secret_type)?;
        let last = self.get_last_secret_rotation(secret_type)?;
        let now = chrono::Utc::now();

        let (next_due, is_overdue) = if let (Some(ref p), Some(ref l)) = (&policy, &last) {
            let next = l.rotated_at + Duration::days(i64::from(p.rotation_interval_days));
            (Some(next), next < now)
        } else if let Some(ref p) = policy {
            // Never rotated — overdue immediately if interval > 0.
            let overdue = p.rotation_interval_days > 0;
            (None, overdue)
        } else {
            (None, false)
        };

        let (grace_active, grace_ends) = if let Some(ref l) = last {
            (
                l.grace_period_active && l.grace_period_ends_at.is_some_and(|d| d > now),
                l.grace_period_ends_at,
            )
        } else {
            (false, None)
        };

        Ok(crate::models::SecretRotationStatus {
            secret_type: secret_type.clone(),
            last_rotated_at: last.as_ref().map(|l| l.rotated_at),
            next_rotation_due: next_due,
            is_overdue,
            grace_period_active: grace_active,
            grace_period_ends_at: grace_ends,
        })
    }

    // ── Shared RFC-3339 parse helper ─────────────────────────────────────────

    fn parse_rfc3339_col(
        r: &rusqlite::Row<'_>,
        col: usize,
    ) -> Result<chrono::DateTime<chrono::Utc>, rusqlite::Error> {
        let s: String = r.get(col)?;
        chrono::DateTime::parse_from_rfc3339(&s)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    col,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
    }
}

// ── Graceful degradation: capability status management ───────────────────────

impl Db {
    /// Store or update a capability's degradation status in the database.
    /// All instances in a load-balanced deployment read from this shared store.
    pub fn set_capability_status(
        &self,
        status: &crate::degradation::CapabilityStatus,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r"
            INSERT INTO capability_statuses (name, level, reason, fallback_available, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(name) DO UPDATE SET
              level = excluded.level,
              reason = excluded.reason,
              fallback_available = excluded.fallback_available,
              updated_at = excluded.updated_at
            ",
            rusqlite::params![
                &status.name,
                serde_json::to_string(&status.level).unwrap(),
                &status.reason,
                status.fallback_available as i32,
                status.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Remove a capability's registered status (used when a capability is
    /// restored to `Full`, so `check` falls back to the default).
    pub fn delete_capability_status(&self, name: &str) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM capability_statuses WHERE name = ?1",
            params![name],
        )?;
        Ok(())
    }

    /// Look up a capability's status, returning `Full` (default) if not found.
    pub fn get_capability_status(
        &self,
        name: &str,
    ) -> Result<crate::degradation::CapabilityStatus, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT name, level, reason, fallback_available, updated_at 
             FROM capability_statuses 
             WHERE name = ?1",
        )?;

        let status = stmt.query_row(rusqlite::params![name], |r| {
            let level_str: String = r.get(1)?;
            let level: crate::degradation::DegradationLevel = serde_json::from_str(&level_str)
                .unwrap_or(crate::degradation::DegradationLevel::Full);
            Ok(crate::degradation::CapabilityStatus {
                name: r.get(0)?,
                level,
                reason: r.get(2)?,
                fallback_available: r.get::<_, i32>(3)? != 0,
                updated_at: {
                    let updated_str: String = r.get(4)?;
                    chrono::DateTime::parse_from_rfc3339(&updated_str)
                        .ok()
                        .and_then(|dt| Some(dt.with_timezone(&chrono::Utc)))
                        .unwrap_or_else(|| chrono::Utc::now())
                },
            })
        });

        match status {
            Ok(s) => Ok(s),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                // Default to Full for unregistered capabilities
                Ok(crate::degradation::CapabilityStatus {
                    name: name.to_string(),
                    level: crate::degradation::DegradationLevel::Full,
                    reason: None,
                    fallback_available: false,
                    updated_at: chrono::Utc::now(),
                })
            }
            Err(e) => Err(e),
        }
    }

    /// List all registered (non-default) capability statuses.
    pub fn list_capability_statuses(
        &self,
    ) -> Result<Vec<crate::degradation::CapabilityStatus>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT name, level, reason, fallback_available, updated_at 
             FROM capability_statuses
             ORDER BY updated_at DESC",
        )?;

        let statuses = stmt.query_map([], |r| {
            let level_str: String = r.get(1)?;
            let level: crate::degradation::DegradationLevel = serde_json::from_str(&level_str)
                .unwrap_or(crate::degradation::DegradationLevel::Full);
            Ok(crate::degradation::CapabilityStatus {
                name: r.get(0)?,
                level,
                reason: r.get(2)?,
                fallback_available: r.get::<_, i32>(3)? != 0,
                updated_at: {
                    let updated_str: String = r.get(4)?;
                    chrono::DateTime::parse_from_rfc3339(&updated_str)
                        .ok()
                        .and_then(|dt| Some(dt.with_timezone(&chrono::Utc)))
                        .unwrap_or_else(|| chrono::Utc::now())
                },
            })
        })?;

        let mut result = Vec::new();
        for row in statuses {
            result.push(row?);
        }
        Ok(result)
    }
}

// ── Consistency pragma helpers (#83) ─────────────────────────────────────────

impl Db {
    /// Expose an `MutexGuard<Connection>` so that callers outside this module
    /// (e.g. `consistency.rs`) can execute one-off queries without going through
    /// individual `Db` methods.
    pub fn conn_lock(&self) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
        self.conn.lock().unwrap()
    }

    /// Run SQLite's `PRAGMA foreign_key_check` and return one descriptive
    /// string per violation.  Returns an empty `Vec` if the database is
    /// clean.
    pub fn run_consistency_pragma(&self) -> Result<Vec<String>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
        let rows = stmt.query_map([], |r| {
            // Columns: table, rowid, parent, fkid
            let table: String = r.get(0)?;
            let rowid: i64 = r.get(1)?;
            let parent: String = r.get(2)?;
            let fkid: i64 = r.get(3)?;
            Ok(format!(
                "table={table} rowid={rowid} parent={parent} fkid={fkid}"
            ))
        })?;

        let mut violations = Vec::new();
        for row in rows {
            violations.push(row?);
        }
        Ok(violations)
    }
}

// ── Event sourcing persistence (#267) ────────────────────────────────────────

impl Db {
    /// Persist an event to the database.
    pub fn append_event(
        &self,
        vault_id: &str,
        sequence: u64,
        event_type: &str,
        timestamp: &chrono::DateTime<chrono::Utc>,
        data: &str,
        schema_version: u32,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r"
            INSERT INTO events (vault_id, sequence, event_type, timestamp, data, schema_version)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ",
            rusqlite::params![
                vault_id,
                sequence as i64,
                event_type,
                timestamp.to_rfc3339(),
                data,
                schema_version as i64
            ],
        )?;
        Ok(())
    }

    /// Retrieve all events for a vault, ordered by sequence.
    pub fn get_events_for_vault(
        &self,
        vault_id: &str,
    ) -> Result<Vec<(u64, String, String, String, u32)>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            r"
            SELECT sequence, event_type, timestamp, data, schema_version
            FROM events
            WHERE vault_id = ?1
            ORDER BY sequence ASC
            ",
        )?;

        let events = stmt.query_map(rusqlite::params![vault_id], |row| {
            Ok((
                row.get::<_, i64>(0)? as u64,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get::<_, i64>(4)? as u32,
            ))
        })?;

        let mut result = Vec::new();
        for event in events {
            result.push(event?);
        }
        Ok(result)
    }

    /// Save a snapshot for a vault.
    pub fn save_snapshot(
        &self,
        vault_id: &str,
        snapshot_sequence: u64,
        taken_at: &chrono::DateTime<chrono::Utc>,
        state: &str,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r"
            INSERT INTO snapshots (vault_id, snapshot_sequence, taken_at, state)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(vault_id) DO UPDATE SET
              snapshot_sequence = excluded.snapshot_sequence,
              taken_at = excluded.taken_at,
              state = excluded.state
            ",
            rusqlite::params![
                vault_id,
                snapshot_sequence as i64,
                taken_at.to_rfc3339(),
                state
            ],
        )?;
        Ok(())
    }

    /// Retrieve the snapshot for a vault, if any.
    pub fn get_snapshot(
        &self,
        vault_id: &str,
    ) -> Result<Option<(u64, String, String)>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            r"
            SELECT snapshot_sequence, taken_at, state
            FROM snapshots
            WHERE vault_id = ?1
            ",
        )?;

        stmt.query_row(rusqlite::params![vault_id], |row| {
            Ok((row.get::<_, i64>(0)? as u64, row.get(1)?, row.get(2)?))
        })
        .optional()
    }

    /// Delete snapshots older than a given date (for retention/archival).
    pub fn delete_old_snapshots(
        &self,
        cutoff_date: &chrono::DateTime<chrono::Utc>,
    ) -> Result<usize, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM snapshots WHERE taken_at < ?1",
            rusqlite::params![cutoff_date.to_rfc3339()],
        )
    }

    /// Delete events older than a given date (for retention/archival).
    pub fn delete_old_events(
        &self,
        cutoff_date: &chrono::DateTime<chrono::Utc>,
    ) -> Result<usize, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM events WHERE timestamp < ?1",
            rusqlite::params![cutoff_date.to_rfc3339()],
        )
    }
}

// ── Feature flag persistence (#274) ──────────────────────────────────────────

impl Db {
    /// Insert a brand-new flag row.  Callers must ensure the key does not
    /// already exist; use `upsert_feature_flag` for create-or-update.
    fn insert_feature_flag(
        &self,
        flag: &crate::feature_flags::FeatureFlag,
    ) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute(
            r"INSERT INTO feature_flags
                (key, description, enabled, rollout_percentage, version, created_at, updated_at)
              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                flag.key,
                flag.description,
                flag.enabled as i64,
                flag.rollout_percentage as i64,
                flag.version as i64,
                flag.created_at.to_rfc3339(),
                flag.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Create or update a feature flag.  If the flag already exists the old
    /// state is written to `feature_flag_history` before the row is updated,
    /// preserving a complete audit trail across restarts and instances.
    ///
    /// Returns the resulting flag (with incremented version on update).
    pub fn upsert_feature_flag(
        &self,
        req: &crate::feature_flags::UpsertFlagRequest,
    ) -> Result<crate::feature_flags::FeatureFlag, rusqlite::Error> {
        let now = chrono::Utc::now();

        match self.get_feature_flag(&req.key)? {
            Some(existing) => {
                // Snapshot the old state into history.
                self.conn.lock().unwrap().execute(
                    r"INSERT INTO feature_flag_history
                        (flag_key, version, enabled, rollout_percentage, updated_at, updated_by)
                      VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![
                        existing.key,
                        existing.version as i64,
                        existing.enabled as i64,
                        existing.rollout_percentage as i64,
                        existing.updated_at.to_rfc3339(),
                        req.updated_by,
                    ],
                )?;

                let new_version = existing.version + 1;
                let new_description = req
                    .description
                    .clone()
                    .or_else(|| existing.description.clone());

                self.conn.lock().unwrap().execute(
                    r"UPDATE feature_flags
                         SET description = ?1,
                             enabled = ?2,
                             rollout_percentage = ?3,
                             version = ?4,
                             updated_at = ?5
                       WHERE key = ?6",
                    rusqlite::params![
                        new_description,
                        req.enabled as i64,
                        req.rollout_percentage as i64,
                        new_version as i64,
                        now.to_rfc3339(),
                        req.key,
                    ],
                )?;

                Ok(crate::feature_flags::FeatureFlag {
                    key: req.key.clone(),
                    description: new_description,
                    enabled: req.enabled,
                    rollout_percentage: req.rollout_percentage,
                    version: new_version,
                    created_at: existing.created_at,
                    updated_at: now,
                    history: self.get_feature_flag_history(&req.key)?,
                })
            }
            None => {
                let flag = crate::feature_flags::FeatureFlag {
                    key: req.key.clone(),
                    description: req.description.clone(),
                    enabled: req.enabled,
                    rollout_percentage: req.rollout_percentage,
                    version: 1,
                    created_at: now,
                    updated_at: now,
                    history: Vec::new(),
                };
                self.insert_feature_flag(&flag)?;
                Ok(flag)
            }
        }
    }

    /// Fetch a single feature flag by key, including its history.
    /// Returns `None` if the key does not exist.
    pub fn get_feature_flag(
        &self,
        key: &str,
    ) -> Result<Option<crate::feature_flags::FeatureFlag>, rusqlite::Error> {
        // Collect the flag row into owned values inside a scoped block so the
        // mutex guard is released before we call get_feature_flag_history,
        // which also needs to acquire the lock.
        let maybe_flag = {
            let binding = self.conn.lock().unwrap();
            let mut stmt = binding.prepare(
                r"SELECT key, description, enabled, rollout_percentage, version, created_at, updated_at
                   FROM feature_flags WHERE key = ?1",
            )?;
            let row = stmt.query_row(rusqlite::params![key], |r| {
                let created_at = {
                    let s: String = r.get(5)?;
                    chrono::DateTime::parse_from_rfc3339(&s)
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                        .map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                5,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?
                };
                let updated_at = {
                    let s: String = r.get(6)?;
                    chrono::DateTime::parse_from_rfc3339(&s)
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                        .map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                6,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?
                };
                let enabled_i: i64 = r.get(2)?;
                Ok(crate::feature_flags::FeatureFlag {
                    key: r.get(0)?,
                    description: r.get(1)?,
                    enabled: enabled_i != 0,
                    rollout_percentage: r.get::<_, i64>(3)? as u8,
                    version: r.get::<_, i64>(4)? as u32,
                    created_at,
                    updated_at,
                    history: Vec::new(), // populated after the lock is released
                })
            });
            match row {
                Ok(flag) => Some(flag),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => return Err(e),
            }
        }; // mutex guard dropped here

        match maybe_flag {
            Some(mut flag) => {
                flag.history = self.get_feature_flag_history(&flag.key)?;
                Ok(Some(flag))
            }
            None => Ok(None),
        }
    }

    /// List all feature flags, each with its full history.
    pub fn list_feature_flags(
        &self,
    ) -> Result<Vec<crate::feature_flags::FeatureFlag>, rusqlite::Error> {
        let keys: Vec<String> = {
            let binding = self.conn.lock().unwrap();
            let mut stmt = binding.prepare("SELECT key FROM feature_flags ORDER BY key")?;
            let iter = stmt.query_map([], |r| r.get(0))?;
            let mut keys = Vec::new();
            for k in iter {
                keys.push(k?);
            }
            keys
        };

        let mut out = Vec::new();
        for key in &keys {
            if let Some(flag) = self.get_feature_flag(key)? {
                out.push(flag);
            }
        }
        Ok(out)
    }

    /// Retrieve the ordered version history for a flag (oldest first).
    pub fn get_feature_flag_history(
        &self,
        key: &str,
    ) -> Result<Vec<crate::feature_flags::FlagVersionSnapshot>, rusqlite::Error> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding.prepare(
            r"SELECT version, enabled, rollout_percentage, updated_at, updated_by
               FROM feature_flag_history
               WHERE flag_key = ?1
               ORDER BY id ASC",
        )?;
        let iter = stmt.query_map(rusqlite::params![key], |r| {
            let updated_at = {
                let s: String = r.get(3)?;
                chrono::DateTime::parse_from_rfc3339(&s)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?
            };
            let enabled_i: i64 = r.get(1)?;
            Ok(crate::feature_flags::FlagVersionSnapshot {
                version: r.get::<_, i64>(0)? as u32,
                enabled: enabled_i != 0,
                rollout_percentage: r.get::<_, i64>(2)? as u8,
                updated_at,
                updated_by: r.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for item in iter {
            out.push(item?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn test_search_vaults_by_owner() {
        let store = create_vault_store();
        let vault = Vault {
            id: "v1".to_string(),
            owner: "owner1".to_string(),
            beneficiary: "ben1".to_string(),
            balance: 1000,
            check_in_interval: 86400,
            last_check_in: Utc::now(),
            created_at: Utc::now(),
            status: VaultStatus::Active,
            ttl_remaining: Some(100_000),
        };
        store.lock().unwrap().insert("v1".to_string(), vault);

        let query = SearchQuery {
            owner: Some("owner1".to_string()),
            beneficiary: None,
            status: None,
            created_after: None,
            created_before: None,
            page: None,
            limit: None,
        };

        let result = search_vaults(&store, &query);
        assert_eq!(result.vaults.len(), 1);
        assert_eq!(result.total, 1);
    }

    #[test]
    fn test_search_vaults_pagination() {
        let store = create_vault_store();
        for i in 0..25 {
            let vault = Vault {
                id: format!("v{i}"),
                owner: "owner1".to_string(),
                beneficiary: "ben1".to_string(),
                balance: 1000,
                check_in_interval: 86400,
                last_check_in: Utc::now(),
                created_at: Utc::now(),
                status: VaultStatus::Active,
                ttl_remaining: Some(100_000),
            };
            store.lock().unwrap().insert(format!("v{i}"), vault);
        }

        let query = SearchQuery {
            owner: Some("owner1".to_string()),
            beneficiary: None,
            status: None,
            created_after: None,
            created_before: None,
            page: Some(2),
            limit: Some(10),
        };

        let result = search_vaults(&store, &query);
        assert_eq!(result.vaults.len(), 10);
        assert_eq!(result.total, 25);
        assert_eq!(result.page, 2);
    }

    fn make_test_vault(id: &str) -> Vault {
        Vault {
            id: id.to_string(),
            owner: "owner1".to_string(),
            beneficiary: "ben1".to_string(),
            balance: 1_000_000,
            check_in_interval: 86_400,
            last_check_in: Utc::now(),
            created_at: Utc::now(),
            status: VaultStatus::Active,
            ttl_remaining: Some(100_000),
        }
    }

    #[test]
    fn test_get_vault_survives_db_restart() {
        let path = std::env::temp_dir().join(format!(
            "heirloom_vault_restart_{}.sqlite3",
            uuid::Uuid::new_v4()
        ));
        let path_str = path.to_str().unwrap();
        let vault = make_test_vault("restart-vault");

        {
            let db = Db::open(path_str).unwrap();
            db.migrate().unwrap();
            db.insert_vault(vault.clone());
        }

        // Re-open against the same file, simulating a process restart.
        let db = Db::open(path_str).unwrap();
        db.migrate().unwrap();
        let reloaded = db
            .get_vault(&vault.id)
            .expect("vault should survive db restart");
        assert_eq!(reloaded.id, vault.id);
        assert_eq!(reloaded.owner, vault.owner);
        assert_eq!(reloaded.balance, vault.balance);
        assert_eq!(reloaded.status, vault.status);
        assert_eq!(reloaded.ttl_remaining, vault.ttl_remaining);

        let _ = std::fs::remove_file(path_str);
    }

    #[test]
    fn test_get_vault_visible_across_db_handles() {
        let path = std::env::temp_dir().join(format!(
            "heirloom_vault_multi_{}.sqlite3",
            uuid::Uuid::new_v4()
        ));
        let path_str = path.to_str().unwrap();

        // Two independent `Db` handles on the same file, simulating two
        // instances sharing a database behind a load balancer.
        let db_a = Db::open(path_str).unwrap();
        db_a.migrate().unwrap();
        let db_b = Db::open(path_str).unwrap();
        db_b.migrate().unwrap();

        let vault = make_test_vault("shared-vault");
        db_a.insert_vault(vault.clone());
        let seen_by_b = db_b
            .get_vault(&vault.id)
            .expect("handle B should see a vault inserted through handle A");
        assert_eq!(seen_by_b.owner, vault.owner);

        let mut vault2 = vault.clone();
        vault2.id = "shared-vault-2".to_string();
        db_b.insert_vault(vault2.clone());
        let seen_by_a = db_a
            .get_vault(&vault2.id)
            .expect("handle A should see a vault inserted through handle B");
        assert_eq!(seen_by_a.id, vault2.id);

        let _ = std::fs::remove_file(path_str);
    }
}

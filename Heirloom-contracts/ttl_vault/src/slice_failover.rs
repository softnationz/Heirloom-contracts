/// Issue #35 — Slice Failover Mechanism
///
/// Implements automatic failover for slices when primary becomes invalid.
/// Each slice can have one or more backup slices that are activated when
/// the primary fails to respond or becomes invalid.
///
/// # Failover strategy
/// 1. Register a backup slice for a primary slice
/// 2. Monitor primary for failures
/// 3. On failure threshold, automatically activate backup
/// 4. Track failover events for audit trail
use soroban_sdk::{contracttype, symbol_short, Env, Vec};

// ── Event topics ─────────────────────────────────────────────────────────────

pub const BACKUP_SLICE_REGISTERED_TOPIC: soroban_sdk::Symbol = symbol_short!("bkup_reg");
pub const FAILOVER_ACTIVATED_TOPIC: soroban_sdk::Symbol = symbol_short!("fail_act");
pub const FAILOVER_REVERTED_TOPIC: soroban_sdk::Symbol = symbol_short!("fail_rev");
pub const FAILOVER_EVENT_TOPIC: soroban_sdk::Symbol = symbol_short!("fail_evt");

// ── Storage keys ─────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub enum SliceFailoverKey {
    /// slice_id -> Vec<u64> of backup slice IDs (ordered by priority)
    BackupSlices(u64),
    /// slice_id -> u64 (current active slice ID, or the slice_id itself if primary)
    ActiveSlice(u64),
    /// (slice_id, backup_id) -> FailoverConfig
    FailoverConfig(u64, u64),
    /// slice_id -> u64 (failure count, reset on successful response)
    FailureCount(u64),
    /// slice_id -> u64 (timestamp of last failure)
    LastFailureTime(u64),
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// Configuration for failover behavior between primary and backup slices.
#[contracttype]
#[derive(Clone, Debug)]
pub struct FailoverConfig {
    /// Primary slice ID
    pub primary_slice_id: u64,
    /// Backup slice ID
    pub backup_slice_id: u64,
    /// Failure threshold before automatic failover triggers
    pub failure_threshold: u32,
    /// Whether failover is currently active
    pub is_active: bool,
    /// Timestamp when this failover config was created
    pub created_at: u64,
}

/// Event emitted when a failover is activated.
#[contracttype]
#[derive(Clone, Debug)]
pub struct FailoverActivatedEvent {
    pub primary_slice_id: u64,
    pub backup_slice_id: u64,
    pub reason: FailoverReason,
    pub timestamp: u64,
}

/// Reason for failover activation.
#[contracttype]
#[derive(Clone, Copy, Debug)]
#[repr(u32)]
pub enum FailoverReason {
    /// Primary exceeded failure threshold
    ThresholdExceeded = 0,
    /// Primary explicitly marked as failed
    ExplicitFailure = 1,
    /// Primary unresponsive
    Timeout = 2,
}

/// Event emitted when failover is reverted to primary.
#[contracttype]
#[derive(Clone, Debug)]
pub struct FailoverRevertedEvent {
    pub primary_slice_id: u64,
    pub backup_slice_id: u64,
    pub timestamp: u64,
}

/// Tracked failover event for audit trail.
#[contracttype]
#[derive(Clone, Debug)]
pub struct FailoverEvent {
    pub primary_slice_id: u64,
    pub backup_slice_id: u64,
    pub event_type: FailoverEventType,
    pub timestamp: u64,
}

#[contracttype]
#[derive(Clone, Copy, Debug)]
#[repr(u32)]
pub enum FailoverEventType {
    /// Backup registered
    Registered = 0,
    /// Failover activated
    Activated = 1,
    /// Failover reverted
    Reverted = 2,
    /// Failure recorded
    FailureRecorded = 3,
}

// ── Core functions ────────────────────────────────────────────────────────────

/// Register a backup slice for a primary slice.
pub fn register_backup_slice(
    env: &Env,
    primary_slice_id: u64,
    backup_slice_id: u64,
    failure_threshold: u32,
) -> u64 {
    if primary_slice_id == backup_slice_id {
        soroban_sdk::panic_with_error!(env, crate::ContractError::InvalidSlice);
    }

    let mut backups = get_backup_slices(env, primary_slice_id);
    backups.push_back(backup_slice_id);

    env.storage()
        .persistent()
        .set(&SliceFailoverKey::BackupSlices(primary_slice_id), &backups);
    env.storage().persistent().extend_ttl(
        &SliceFailoverKey::BackupSlices(primary_slice_id),
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    let config = FailoverConfig {
        primary_slice_id,
        backup_slice_id,
        failure_threshold,
        is_active: false,
        created_at: env.ledger().timestamp(),
    };

    env.storage().persistent().set(
        &SliceFailoverKey::FailoverConfig(primary_slice_id, backup_slice_id),
        &config,
    );
    env.storage().persistent().extend_ttl(
        &SliceFailoverKey::FailoverConfig(primary_slice_id, backup_slice_id),
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.events().publish(
        (BACKUP_SLICE_REGISTERED_TOPIC, primary_slice_id),
        FailoverEvent {
            primary_slice_id,
            backup_slice_id,
            event_type: FailoverEventType::Registered,
            timestamp: env.ledger().timestamp(),
        },
    );

    backup_slice_id
}

/// Get list of backup slices for a primary slice.
pub fn get_backup_slices(env: &Env, primary_slice_id: u64) -> Vec<u64> {
    let key = SliceFailoverKey::BackupSlices(primary_slice_id);
    env.storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| Vec::new(env))
}

/// Get the currently active slice (may be primary or backup).
pub fn get_active_slice(env: &Env, slice_id: u64) -> u64 {
    let key = SliceFailoverKey::ActiveSlice(slice_id);
    env.storage().persistent().get(&key).unwrap_or(slice_id)
}

/// Record a failure for a slice and check if failover should activate.
pub fn record_slice_failure(env: &Env, primary_slice_id: u64, reason: FailoverReason) -> bool {
    let mut failure_count: u32 = env
        .storage()
        .persistent()
        .get(&SliceFailoverKey::FailureCount(primary_slice_id))
        .unwrap_or(0);

    failure_count = failure_count.saturating_add(1);

    env.storage().persistent().set(
        &SliceFailoverKey::FailureCount(primary_slice_id),
        &failure_count,
    );
    env.storage().persistent().extend_ttl(
        &SliceFailoverKey::FailureCount(primary_slice_id),
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.storage().persistent().set(
        &SliceFailoverKey::LastFailureTime(primary_slice_id),
        &env.ledger().timestamp(),
    );
    env.storage().persistent().extend_ttl(
        &SliceFailoverKey::LastFailureTime(primary_slice_id),
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    // Check if we should activate failover
    let backups = get_backup_slices(env, primary_slice_id);
    if backups.is_empty() {
        return false;
    }

    let primary_backup = backups.get(0).unwrap();
    let config_key = SliceFailoverKey::FailoverConfig(primary_slice_id, primary_backup);

    let Some(config) = env
        .storage()
        .persistent()
        .get::<_, FailoverConfig>(&config_key)
    else {
        return false;
    };
    if failure_count >= config.failure_threshold && !config.is_active {
        return activate_failover(env, primary_slice_id, primary_backup, reason);
    }

    false
}

/// Activate failover from primary to backup slice.
pub fn activate_failover(
    env: &Env,
    primary_slice_id: u64,
    backup_slice_id: u64,
    reason: FailoverReason,
) -> bool {
    let config_key = SliceFailoverKey::FailoverConfig(primary_slice_id, backup_slice_id);

    // Get and update config
    let Some(mut config) = env
        .storage()
        .persistent()
        .get::<_, FailoverConfig>(&config_key)
    else {
        return false;
    };

    if config.is_active {
        return false; // Already active
    }

    config.is_active = true;

    env.storage().persistent().set(&config_key, &config);
    env.storage().persistent().extend_ttl(
        &config_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    // Update active slice
    env.storage().persistent().set(
        &SliceFailoverKey::ActiveSlice(primary_slice_id),
        &backup_slice_id,
    );
    env.storage().persistent().extend_ttl(
        &SliceFailoverKey::ActiveSlice(primary_slice_id),
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.events().publish(
        (FAILOVER_ACTIVATED_TOPIC, primary_slice_id),
        FailoverActivatedEvent {
            primary_slice_id,
            backup_slice_id,
            reason,
            timestamp: env.ledger().timestamp(),
        },
    );

    env.events().publish(
        (FAILOVER_EVENT_TOPIC, primary_slice_id),
        FailoverEvent {
            primary_slice_id,
            backup_slice_id,
            event_type: FailoverEventType::Activated,
            timestamp: env.ledger().timestamp(),
        },
    );

    true
}

/// Revert failover back to primary slice.
pub fn revert_failover(env: &Env, primary_slice_id: u64, backup_slice_id: u64) -> bool {
    let config_key = SliceFailoverKey::FailoverConfig(primary_slice_id, backup_slice_id);

    let Some(mut config) = env
        .storage()
        .persistent()
        .get::<_, FailoverConfig>(&config_key)
    else {
        return false;
    };

    if !config.is_active {
        return false; // Not currently in failover
    }

    config.is_active = false;

    env.storage().persistent().set(&config_key, &config);
    env.storage().persistent().extend_ttl(
        &config_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    // Reset active slice to primary
    env.storage().persistent().set(
        &SliceFailoverKey::ActiveSlice(primary_slice_id),
        &primary_slice_id,
    );
    env.storage().persistent().extend_ttl(
        &SliceFailoverKey::ActiveSlice(primary_slice_id),
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    // Reset failure count
    env.storage()
        .persistent()
        .set(&SliceFailoverKey::FailureCount(primary_slice_id), &0u32);
    env.storage().persistent().extend_ttl(
        &SliceFailoverKey::FailureCount(primary_slice_id),
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.events().publish(
        (FAILOVER_REVERTED_TOPIC, primary_slice_id),
        FailoverRevertedEvent {
            primary_slice_id,
            backup_slice_id,
            timestamp: env.ledger().timestamp(),
        },
    );

    env.events().publish(
        (FAILOVER_EVENT_TOPIC, primary_slice_id),
        FailoverEvent {
            primary_slice_id,
            backup_slice_id,
            event_type: FailoverEventType::Reverted,
            timestamp: env.ledger().timestamp(),
        },
    );

    true
}

/// Get failure count for a slice.
pub fn get_failure_count(env: &Env, slice_id: u64) -> u32 {
    env.storage()
        .persistent()
        .get(&SliceFailoverKey::FailureCount(slice_id))
        .unwrap_or(0)
}

/// Reset failure count for a slice.
pub fn reset_failure_count(env: &Env, slice_id: u64) {
    env.storage()
        .persistent()
        .set(&SliceFailoverKey::FailureCount(slice_id), &0u32);
    env.storage().persistent().extend_ttl(
        &SliceFailoverKey::FailureCount(slice_id),
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_failover_reason_enum() {
        // Just ensure the enum is constructible
        let _ = FailoverReason::ThresholdExceeded;
        let _ = FailoverReason::ExplicitFailure;
        let _ = FailoverReason::Timeout;
    }
}

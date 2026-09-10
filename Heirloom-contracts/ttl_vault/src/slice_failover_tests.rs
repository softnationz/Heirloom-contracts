//! Tests for the slice failover mechanism (Issue #35).
//!
//! Covers:
//! - Authorized failover: owner can register backup slices, record failures,
//!   trigger automatic and explicit failover, and revert.
//! - Unauthorized caller rejection: non-owner is rejected with `NotOwner` on
//!   every mutating entry point.
//! - Threshold-based automatic failover: once failure count reaches the
//!   configured threshold the backup is promoted automatically.
//! - Read-only queries: `get_backup_slices`, `get_active_slice`, and
//!   `get_failure_count` are accessible without auth.

#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{testutils::Address as _, token::StellarAssetClient, Address, Env};

// ── Test helpers ──────────────────────────────────────────────────────────────

/// Bootstrap a minimal environment: contract, XLM token, admin, vault owner,
/// and one vault.  Returns the pieces needed by all test cases.
fn setup() -> (
    Env,
    Address, // owner
    Address, // admin
    TtlVaultContractClient<'static>,
    u64, // vault_id
) {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let admin = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token_address = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    StellarAssetClient::new(&env, &token_address).mint(&owner, &10_000_000);

    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    let vault_id = client.create_vault(&owner, &beneficiary, &3_600u64, &None);

    // Safety: the client borrow must outlive the env in tests.
    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, owner, admin, client, vault_id)
}

// ── Authorized operations ─────────────────────────────────────────────────────

/// The vault owner can register a backup slice and read it back.
#[test]
fn test_register_backup_slice_by_owner() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let primary_id = 1u64;
    let backup_id = 2u64;
    let threshold = 3u32;

    let returned =
        client.register_backup_slice(&vault_id, &owner, &primary_id, &backup_id, &threshold);
    assert_eq!(returned, backup_id);

    let backups = client.get_backup_slices(&primary_id);
    assert_eq!(backups.len(), 1);
    assert_eq!(backups.get(0).unwrap(), backup_id);
}

/// Recording failures below the threshold does NOT activate failover.
#[test]
fn test_record_failure_below_threshold_does_not_failover() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let primary_id = 10u64;
    let backup_id = 20u64;
    let threshold = 5u32;

    client.register_backup_slice(&vault_id, &owner, &primary_id, &backup_id, &threshold);

    // Record threshold-1 failures — should not trigger failover.
    for _ in 0..(threshold - 1) {
        let activated = client.record_slice_failure(
            &vault_id,
            &owner,
            &primary_id,
            &slice_failover::FailoverReason::ThresholdExceeded,
        );
        assert!(!activated, "failover should not activate below threshold");
    }

    // Active slice is still the primary.
    assert_eq!(client.get_active_slice(&primary_id), primary_id);
    // Failure count reflects the recorded events.
    assert_eq!(client.get_failure_count(&primary_id), threshold - 1);
}

/// When the failure count reaches the threshold, the backup slice is promoted
/// automatically through `record_slice_failure`.
#[test]
fn test_record_failure_at_threshold_activates_failover() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let primary_id = 11u64;
    let backup_id = 21u64;
    let threshold = 3u32;

    client.register_backup_slice(&vault_id, &owner, &primary_id, &backup_id, &threshold);

    // Record threshold-1 failures silently.
    for _ in 0..(threshold - 1) {
        client.record_slice_failure(
            &vault_id,
            &owner,
            &primary_id,
            &slice_failover::FailoverReason::Timeout,
        );
    }

    // The threshold-th failure must activate failover and return true.
    let activated = client.record_slice_failure(
        &vault_id,
        &owner,
        &primary_id,
        &slice_failover::FailoverReason::Timeout,
    );
    assert!(activated, "failover should activate at threshold");

    // Active slice must now point to the backup.
    assert_eq!(client.get_active_slice(&primary_id), backup_id);
}

/// The owner can explicitly activate failover without going through the
/// failure-recording path.
#[test]
fn test_explicit_activate_failover_by_owner() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let primary_id = 12u64;
    let backup_id = 22u64;

    client.register_backup_slice(&vault_id, &owner, &primary_id, &backup_id, &10u32);

    let activated = client.activate_failover(
        &vault_id,
        &owner,
        &primary_id,
        &backup_id,
        &slice_failover::FailoverReason::ExplicitFailure,
    );
    assert!(activated);
    assert_eq!(client.get_active_slice(&primary_id), backup_id);
}

/// The owner can revert an active failover, restoring the primary and
/// resetting the failure counter.
#[test]
fn test_revert_failover_by_owner() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let primary_id = 13u64;
    let backup_id = 23u64;

    client.register_backup_slice(&vault_id, &owner, &primary_id, &backup_id, &10u32);
    client.activate_failover(
        &vault_id,
        &owner,
        &primary_id,
        &backup_id,
        &slice_failover::FailoverReason::ExplicitFailure,
    );

    let reverted = client.revert_failover(&vault_id, &owner, &primary_id, &backup_id);
    assert!(reverted);

    // Active slice returns to primary.
    assert_eq!(client.get_active_slice(&primary_id), primary_id);
    // Failure count was reset.
    assert_eq!(client.get_failure_count(&primary_id), 0u32);
}

/// Activating failover a second time (while already active) is a no-op that
/// returns false.
#[test]
fn test_double_activate_is_noop() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let primary_id = 14u64;
    let backup_id = 24u64;

    client.register_backup_slice(&vault_id, &owner, &primary_id, &backup_id, &10u32);
    client.activate_failover(
        &vault_id,
        &owner,
        &primary_id,
        &backup_id,
        &slice_failover::FailoverReason::ExplicitFailure,
    );

    let second = client.activate_failover(
        &vault_id,
        &owner,
        &primary_id,
        &backup_id,
        &slice_failover::FailoverReason::ExplicitFailure,
    );
    assert!(
        !second,
        "second activate while already active should return false"
    );
}

/// Reverting when no failover is active returns false.
#[test]
fn test_revert_when_not_active_is_noop() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let primary_id = 15u64;
    let backup_id = 25u64;

    client.register_backup_slice(&vault_id, &owner, &primary_id, &backup_id, &10u32);

    let reverted = client.revert_failover(&vault_id, &owner, &primary_id, &backup_id);
    assert!(!reverted, "revert when not active should return false");
}

/// get_active_slice returns the slice_id itself when no failover is configured.
#[test]
fn test_get_active_slice_default_returns_primary() {
    let (_env, _owner, _admin, client, _vault_id) = setup();

    let slice_id = 99u64;
    // No backup registered — active slice defaults to itself.
    assert_eq!(client.get_active_slice(&slice_id), slice_id);
}

// ── Unauthorized caller rejection ─────────────────────────────────────────────

/// A stranger (non-owner) cannot register a backup slice.
#[test]
fn test_register_backup_slice_rejects_non_owner() {
    let (env, _owner, _admin, client, vault_id) = setup();
    let stranger = Address::generate(&env);

    let err = client
        .try_register_backup_slice(&vault_id, &stranger, &1u64, &2u64, &3u32)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::NotOwner);
}

/// A stranger cannot record a slice failure.
#[test]
fn test_record_slice_failure_rejects_non_owner() {
    let (env, owner, _admin, client, vault_id) = setup();
    let stranger = Address::generate(&env);

    // Register config so the check doesn't fail for a different reason.
    client.register_backup_slice(&vault_id, &owner, &1u64, &2u64, &3u32);

    let err = client
        .try_record_slice_failure(
            &vault_id,
            &stranger,
            &1u64,
            &slice_failover::FailoverReason::Timeout,
        )
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::NotOwner);
}

/// A stranger cannot explicitly activate failover.
#[test]
fn test_activate_failover_rejects_non_owner() {
    let (env, owner, _admin, client, vault_id) = setup();
    let stranger = Address::generate(&env);

    client.register_backup_slice(&vault_id, &owner, &1u64, &2u64, &10u32);

    let err = client
        .try_activate_failover(
            &vault_id,
            &stranger,
            &1u64,
            &2u64,
            &slice_failover::FailoverReason::ExplicitFailure,
        )
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::NotOwner);
}

/// A stranger cannot revert failover.
#[test]
fn test_revert_failover_rejects_non_owner() {
    let (env, owner, _admin, client, vault_id) = setup();
    let stranger = Address::generate(&env);

    client.register_backup_slice(&vault_id, &owner, &1u64, &2u64, &10u32);
    client.activate_failover(
        &vault_id,
        &owner,
        &1u64,
        &2u64,
        &slice_failover::FailoverReason::ExplicitFailure,
    );

    let err = client
        .try_revert_failover(&vault_id, &stranger, &1u64, &2u64)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::NotOwner);
}

/// Registering a backup slice with primary == backup is rejected with InvalidSlice.
#[test]
fn test_register_backup_same_as_primary_fails() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let err = client
        .try_register_backup_slice(&vault_id, &owner, &5u64, &5u64, &2u32)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::InvalidSlice);
}

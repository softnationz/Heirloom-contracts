#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{testutils::Address as _, token::StellarAssetClient, vec, Address, Env};

// ── Helpers ───────────────────────────────────────────────────────────────────

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

    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, owner, admin, client, vault_id)
}

// ── record_attestor_performance ───────────────────────────────────────────────

#[test]
fn test_record_performance_creates_entry() {
    let (env, owner, _admin, client, vault_id) = setup();
    let attestor = Address::generate(&env);
    let slice_id = 1u64;

    // No data initially.
    assert!(client
        .get_attestor_performance(&slice_id, &attestor)
        .is_none());

    // Record a successful response (Soroban client panics on Err, returns () on Ok).
    client.record_attestor_performance(&vault_id, &owner, &slice_id, &attestor, &true, &50u64);

    let m = client
        .get_attestor_performance(&slice_id, &attestor)
        .unwrap();
    assert_eq!(m.total_responses, 1);
    assert_eq!(m.successful_responses, 1);
    assert_eq!(m.total_response_time_ms, 50);
}

#[test]
fn test_record_performance_accumulates() {
    let (env, owner, _admin, client, vault_id) = setup();
    let attestor = Address::generate(&env);
    let slice_id = 2u64;

    client.record_attestor_performance(&vault_id, &owner, &slice_id, &attestor, &true, &100u64);
    client.record_attestor_performance(&vault_id, &owner, &slice_id, &attestor, &false, &200u64);
    client.record_attestor_performance(&vault_id, &owner, &slice_id, &attestor, &true, &150u64);

    let m = client
        .get_attestor_performance(&slice_id, &attestor)
        .unwrap();
    assert_eq!(m.total_responses, 3);
    assert_eq!(m.successful_responses, 2);
    assert_eq!(m.total_response_time_ms, 450);
}

#[test]
fn test_record_performance_rejects_non_owner() {
    let (env, _owner, _admin, client, vault_id) = setup();
    let intruder = Address::generate(&env);
    let attestor = Address::generate(&env);

    let result = client
        .try_record_attestor_performance(&vault_id, &intruder, &1u64, &attestor, &true, &10u64);
    assert!(result.is_err());
}

// ── calculate_optimal_weights ─────────────────────────────────────────────────

#[test]
fn test_weights_equal_when_no_data() {
    let (env, _owner, _admin, client, _vault_id) = setup();
    let slice_id = 10u64;
    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);
    let attestors = vec![&env, a1.clone(), a2.clone(), a3.clone()];

    let weights = client.calculate_optimal_weights(&slice_id, &attestors);
    assert_eq!(weights.len(), 3);
    // Without data the total BPS must sum to 10 000.
    let total: u32 = weights.iter().map(|w| w.weight_bps).sum();
    assert_eq!(total, 10_000);
}

#[test]
fn test_weights_reflect_performance() {
    let (env, owner, _admin, client, vault_id) = setup();
    let slice_id = 20u64;
    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);

    // a1: 10/10 successes, 10 ms avg → high score
    for _ in 0..10 {
        client.record_attestor_performance(&vault_id, &owner, &slice_id, &a1, &true, &10u64);
    }
    // a2: 1/10 successes, 100 ms avg → low score
    client.record_attestor_performance(&vault_id, &owner, &slice_id, &a2, &true, &100u64);
    for _ in 0..9 {
        client.record_attestor_performance(&vault_id, &owner, &slice_id, &a2, &false, &100u64);
    }

    let attestors = vec![&env, a1.clone(), a2.clone()];
    let weights = client.calculate_optimal_weights(&slice_id, &attestors);
    assert_eq!(weights.len(), 2);

    let total: u32 = weights.iter().map(|w| w.weight_bps).sum();
    assert_eq!(total, 10_000);

    // a1 should have a significantly higher weight than a2.
    let w1 = weights.get(0).unwrap().weight_bps;
    let w2 = weights.get(1).unwrap().weight_bps;
    assert!(w1 > w2, "a1 weight ({w1}) should exceed a2 weight ({w2})");
}

#[test]
fn test_weights_single_attestor_gets_full_bps() {
    let (env, owner, _admin, client, vault_id) = setup();
    let slice_id = 30u64;
    let a1 = Address::generate(&env);

    client.record_attestor_performance(&vault_id, &owner, &slice_id, &a1, &true, &20u64);

    let attestors = vec![&env, a1.clone()];
    let weights = client.calculate_optimal_weights(&slice_id, &attestors);
    assert_eq!(weights.len(), 1);
    assert_eq!(weights.get(0).unwrap().weight_bps, 10_000);
}

// ── reweight_slice ────────────────────────────────────────────────────────────

#[test]
fn test_reweight_slice_persists_and_retrieves() {
    let (env, owner, _admin, client, vault_id) = setup();
    let slice_id = 40u64;
    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);

    client.record_attestor_performance(&vault_id, &owner, &slice_id, &a1, &true, &10u64);
    client.record_attestor_performance(&vault_id, &owner, &slice_id, &a2, &true, &100u64);

    let attestors = vec![&env, a1.clone(), a2.clone()];
    // reweight_slice returns Vec<AttestorWeight> directly (Soroban strips Result wrapper).
    let computed = client.reweight_slice(&vault_id, &owner, &slice_id, &attestors);

    let persisted = client.get_slice_weights(&slice_id).unwrap();
    assert_eq!(computed.len(), persisted.len());
    let total: u32 = persisted.iter().map(|w| w.weight_bps).sum();
    assert_eq!(total, 10_000);
}

#[test]
fn test_reweight_slice_rejects_non_owner() {
    let (env, _owner, _admin, client, vault_id) = setup();
    let intruder = Address::generate(&env);
    let a1 = Address::generate(&env);
    let attestors = vec![&env, a1];

    let result = client.try_reweight_slice(&vault_id, &intruder, &1u64, &attestors);
    assert!(result.is_err());
}

// ── Reputation Decay Tests ─────────────────────────────────────────────────

#[test]
fn test_apply_reputation_decay_reduces_reputation() {
    let (env, owner, _admin, client, vault_id) = setup();
    let attestor = Address::generate(&env);
    let slice_id = 50u64;

    // Initial reputation is 10000 (full).
    let initial_rep = client.get_reputation_factor(&slice_id, &attestor);
    assert_eq!(initial_rep, 10_000);

    // Apply 50% decay (decay_rate = 5000 means keep 50%).
    let reason = String::from_slice(&env, "low_success_rate");
    let new_rep =
        client.apply_reputation_decay(&vault_id, &owner, &slice_id, &attestor, &5_000u32, &reason);

    // New reputation should be 50% of 10000 = 5000.
    assert_eq!(new_rep, 5_000);

    // Verify persistence.
    let persisted_rep = client.get_reputation_factor(&slice_id, &attestor);
    assert_eq!(persisted_rep, 5_000);
}

#[test]
fn test_apply_reputation_decay_cascades() {
    let (env, owner, _admin, client, vault_id) = setup();
    let attestor = Address::generate(&env);
    let slice_id = 51u64;

    let reason1 = String::from_slice(&env, "decay_1");
    let reason2 = String::from_slice(&env, "decay_2");

    // First decay: 50% (10000 → 5000).
    let rep1 =
        client.apply_reputation_decay(&vault_id, &owner, &slice_id, &attestor, &5_000u32, &reason1);
    assert_eq!(rep1, 5_000);

    // Second decay: 50% (5000 → 2500).
    let rep2 =
        client.apply_reputation_decay(&vault_id, &owner, &slice_id, &attestor, &5_000u32, &reason2);
    assert_eq!(rep2, 2_500);
}

#[test]
fn test_apply_reputation_decay_full_decay() {
    let (env, owner, _admin, client, vault_id) = setup();
    let attestor = Address::generate(&env);
    let slice_id = 52u64;

    let reason = String::from_slice(&env, "complete_failure");

    // Full decay (decay_rate = 0 means complete decay).
    let new_rep =
        client.apply_reputation_decay(&vault_id, &owner, &slice_id, &attestor, &0u32, &reason);

    assert_eq!(new_rep, 0);
}

#[test]
fn test_apply_reputation_decay_no_decay() {
    let (env, owner, _admin, client, vault_id) = setup();
    let attestor = Address::generate(&env);
    let slice_id = 53u64;

    let reason = String::from_slice(&env, "preserve");

    // No decay (decay_rate = 10000 means preserve).
    let new_rep =
        client.apply_reputation_decay(&vault_id, &owner, &slice_id, &attestor, &10_000u32, &reason);

    assert_eq!(new_rep, 10_000);
}

#[test]
fn test_apply_reputation_decay_rejects_invalid_bps() {
    let (env, owner, _admin, client, vault_id) = setup();
    let attestor = Address::generate(&env);
    let slice_id = 54u64;

    let reason = String::from_slice(&env, "invalid");

    // Invalid BPS > 10000.
    let result = client
        .try_apply_reputation_decay(&vault_id, &owner, &slice_id, &attestor, &10_001u32, &reason);
    assert!(result.is_err());
}

#[test]
fn test_apply_reputation_decay_rejects_non_owner() {
    let (env, _owner, _admin, client, vault_id) = setup();
    let intruder = Address::generate(&env);
    let attestor = Address::generate(&env);
    let slice_id = 55u64;

    let reason = String::from_slice(&env, "unauthorized");

    let result = client.try_apply_reputation_decay(
        &vault_id, &intruder, &slice_id, &attestor, &5_000u32, &reason,
    );
    assert!(result.is_err());
}

#[test]
fn test_apply_reputation_recovery_restores_reputation() {
    let (env, owner, _admin, client, vault_id) = setup();
    let attestor = Address::generate(&env);
    let slice_id = 60u64;

    let decay_reason = String::from_slice(&env, "decay");

    // Reduce reputation to 5000.
    client.apply_reputation_decay(
        &vault_id,
        &owner,
        &slice_id,
        &attestor,
        &5_000u32,
        &decay_reason,
    );

    // Recover 50% (recover_rate = 5000).
    // new_rep = 5000 + (10000 - 5000) * 5000 / 10000 = 5000 + 2500 = 7500
    let new_rep =
        client.apply_reputation_recovery(&vault_id, &owner, &slice_id, &attestor, &5_000u32);

    assert_eq!(new_rep, 7_500);
}

#[test]
fn test_apply_reputation_recovery_full_recovery() {
    let (env, owner, _admin, client, vault_id) = setup();
    let attestor = Address::generate(&env);
    let slice_id = 61u64;

    let decay_reason = String::from_slice(&env, "decay");

    // Reduce reputation to 2500.
    client.apply_reputation_decay(
        &vault_id,
        &owner,
        &slice_id,
        &attestor,
        &2_500u32,
        &decay_reason,
    );

    // Full recovery (recovery_rate = 10000).
    let new_rep =
        client.apply_reputation_recovery(&vault_id, &owner, &slice_id, &attestor, &10_000u32);

    assert_eq!(new_rep, 10_000);
}

#[test]
fn test_apply_reputation_recovery_no_recovery() {
    let (env, owner, _admin, client, vault_id) = setup();
    let attestor = Address::generate(&env);
    let slice_id = 62u64;

    let decay_reason = String::from_slice(&env, "decay");

    // Reduce reputation to 5000.
    client.apply_reputation_decay(
        &vault_id,
        &owner,
        &slice_id,
        &attestor,
        &5_000u32,
        &decay_reason,
    );

    // No recovery (recovery_rate = 0).
    let new_rep = client.apply_reputation_recovery(&vault_id, &owner, &slice_id, &attestor, &0u32);

    assert_eq!(new_rep, 5_000);
}

#[test]
fn test_apply_reputation_recovery_already_at_max() {
    let (env, owner, _admin, client, vault_id) = setup();
    let attestor = Address::generate(&env);
    let slice_id = 63u64;

    // Reputation already at 10000, recovery should be no-op.
    let new_rep =
        client.apply_reputation_recovery(&vault_id, &owner, &slice_id, &attestor, &5_000u32);

    assert_eq!(new_rep, 10_000);
}

#[test]
fn test_apply_reputation_recovery_rejects_invalid_bps() {
    let (env, owner, _admin, client, vault_id) = setup();
    let attestor = Address::generate(&env);
    let slice_id = 64u64;

    // Invalid BPS > 10000.
    let result =
        client.try_apply_reputation_recovery(&vault_id, &owner, &slice_id, &attestor, &10_001u32);
    assert!(result.is_err());
}

#[test]
fn test_apply_reputation_recovery_rejects_non_owner() {
    let (env, _owner, _admin, client, vault_id) = setup();
    let intruder = Address::generate(&env);
    let attestor = Address::generate(&env);
    let slice_id = 65u64;

    let result =
        client.try_apply_reputation_recovery(&vault_id, &intruder, &slice_id, &attestor, &5_000u32);
    assert!(result.is_err());
}

#[test]
fn test_get_decay_history_tracks_changes() {
    let (env, owner, _admin, client, vault_id) = setup();
    let attestor = Address::generate(&env);
    let slice_id = 70u64;

    let decay_reason1 = String::from_slice(&env, "failure_1");
    let decay_reason2 = String::from_slice(&env, "failure_2");

    // Apply two decays.
    client.apply_reputation_decay(
        &vault_id,
        &owner,
        &slice_id,
        &attestor,
        &5_000u32,
        &decay_reason1,
    );
    client.apply_reputation_decay(
        &vault_id,
        &owner,
        &slice_id,
        &attestor,
        &5_000u32,
        &decay_reason2,
    );

    // Retrieve history (limit=10).
    let history = client.get_decay_history(&slice_id, &attestor, &10u64);

    // Should have 2 entries (most recent first).
    assert_eq!(history.len(), 2);

    // First entry (most recent) should be the second decay.
    let entry1 = history.get(0).unwrap();
    assert_eq!(entry1.decay_rate_bps, 5_000);
    assert_eq!(entry1.reputation_before, 5_000);
    assert_eq!(entry1.reputation_after, 2_500);

    // Second entry should be the first decay.
    let entry2 = history.get(1).unwrap();
    assert_eq!(entry2.decay_rate_bps, 5_000);
    assert_eq!(entry2.reputation_before, 10_000);
    assert_eq!(entry2.reputation_after, 5_000);
}

#[test]
fn test_get_decay_history_with_recovery() {
    let (env, owner, _admin, client, vault_id) = setup();
    let attestor = Address::generate(&env);
    let slice_id = 71u64;

    let decay_reason = String::from_slice(&env, "failure");

    // Apply decay then recovery.
    client.apply_reputation_decay(
        &vault_id,
        &owner,
        &slice_id,
        &attestor,
        &5_000u32,
        &decay_reason,
    );
    client.apply_reputation_recovery(&vault_id, &owner, &slice_id, &attestor, &5_000u32);

    let history = client.get_decay_history(&slice_id, &attestor, &10u64);

    // Should have 2 entries.
    assert_eq!(history.len(), 2);

    // First entry (most recent) should be recovery.
    let entry1 = history.get(0).unwrap();
    assert_eq!(entry1.reputation_before, 5_000);
    assert_eq!(entry1.reputation_after, 7_500);

    // Second entry should be decay.
    let entry2 = history.get(1).unwrap();
    assert_eq!(entry2.reputation_before, 10_000);
    assert_eq!(entry2.reputation_after, 5_000);
}

#[test]
fn test_get_decay_history_respects_limit() {
    let (env, owner, _admin, client, vault_id) = setup();
    let attestor = Address::generate(&env);
    let slice_id = 72u64;

    let reason = String::from_slice(&env, "decay");

    // Apply 5 decays.
    for _ in 0..5 {
        client.apply_reputation_decay(&vault_id, &owner, &slice_id, &attestor, &9_000u32, &reason);
    }

    // Request only 2 entries.
    let history = client.get_decay_history(&slice_id, &attestor, &2u64);

    assert_eq!(history.len(), 2);
}

#[test]
fn test_get_decay_history_empty_when_no_changes() {
    let (env, _owner, _admin, client, _vault_id) = setup();
    let attestor = Address::generate(&env);
    let slice_id = 73u64;

    let history = client.get_decay_history(&slice_id, &attestor, &10u64);
    assert_eq!(history.len(), 0);
}

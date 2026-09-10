#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{testutils::Address as _, token::StellarAssetClient, vec, Address, Bytes, Env};

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

// ── register_composition_rule ─────────────────────────────────────────────────

#[test]
fn test_register_rule_returns_sequential_ids() {
    let (env, _owner, admin, client, _vault_id) = setup();
    let payload = Bytes::from_slice(&env, b"rule_a");

    // Soroban client strips the Result<u64, _> wrapper and returns u64 directly.
    let id0 = client.register_composition_rule(&admin, &payload, &0u32, &1u32);
    let id1 = client.register_composition_rule(&admin, &payload, &1u32, &2u32);
    let id2 = client.register_composition_rule(&admin, &payload, &2u32, &3u32);

    assert_eq!(id0, 0);
    assert_eq!(id1, 1);
    assert_eq!(id2, 2);
}

#[test]
fn test_register_rule_rejects_non_admin() {
    let (env, owner, _admin, client, _vault_id) = setup();
    let payload = Bytes::from_slice(&env, b"x");

    let result = client.try_register_composition_rule(&owner, &payload, &0u32, &0u32);
    assert!(result.is_err());
}

#[test]
fn test_get_composition_rule_returns_correct_fields() {
    let (env, _owner, admin, client, _vault_id) = setup();
    let payload = Bytes::from_slice(&env, b"prefix_check");

    let rule_id = client.register_composition_rule(&admin, &payload, &5u32, &42u32);

    let rule = client.get_composition_rule(&rule_id).unwrap();
    assert_eq!(rule.rule_id, rule_id);
    assert_eq!(rule.priority, 5);
    assert_eq!(rule.tag, 42);
    assert!(rule.enabled);
}

// ── set_rule_enabled ──────────────────────────────────────────────────────────

#[test]
fn test_disable_and_reenable_rule() {
    let (env, _owner, admin, client, _vault_id) = setup();
    let payload = Bytes::from_slice(&env, b"");
    let rule_id = client.register_composition_rule(&admin, &payload, &0u32, &0u32);

    // Disable (client returns () directly).
    client.set_rule_enabled(&admin, &rule_id, &false);
    assert!(!client.get_composition_rule(&rule_id).unwrap().enabled);

    // Re-enable.
    client.set_rule_enabled(&admin, &rule_id, &true);
    assert!(client.get_composition_rule(&rule_id).unwrap().enabled);
}

#[test]
fn test_set_rule_enabled_rejects_unknown_rule() {
    let (_env, _owner, admin, client, _vault_id) = setup();
    let result = client.try_set_rule_enabled(&admin, &9999u64, &false);
    assert!(result.is_err());
}

// ── set_slice_rules / get_slice_rule_ids ──────────────────────────────────────

#[test]
fn test_set_and_get_slice_rules() {
    let (env, owner, admin, client, vault_id) = setup();
    let payload = Bytes::from_slice(&env, b"");
    let r0 = client.register_composition_rule(&admin, &payload, &0u32, &0u32);
    let r1 = client.register_composition_rule(&admin, &payload, &1u32, &0u32);

    let slice_id = 100u64;
    let rule_ids = vec![&env, r0, r1];
    client.set_slice_rules(&vault_id, &owner, &slice_id, &rule_ids);

    let stored = client.get_slice_rule_ids(&slice_id);
    assert_eq!(stored.len(), 2);
    assert_eq!(stored.get(0).unwrap(), r0);
    assert_eq!(stored.get(1).unwrap(), r1);
}

#[test]
fn test_set_slice_rules_rejects_non_owner() {
    let (env, _owner, admin, client, vault_id) = setup();
    let intruder = Address::generate(&env);
    let payload = Bytes::from_slice(&env, b"");
    let r0 = client.register_composition_rule(&admin, &payload, &0u32, &0u32);

    let result = client.try_set_slice_rules(&vault_id, &intruder, &1u64, &vec![&env, r0]);
    assert!(result.is_err());
}

// ── validate_slice_with_rules ─────────────────────────────────────────────────

#[test]
fn test_validate_empty_rules_is_valid() {
    let (env, _owner, _admin, client, _vault_id) = setup();
    let slice_data = Bytes::from_slice(&env, b"some_slice_data");
    let result = client.validate_slice_with_rules(&999u64, &slice_data);
    assert!(result.overall_valid);
    assert_eq!(result.outcomes.len(), 0);
    assert_eq!(result.conflicts.len(), 0);
}

#[test]
fn test_validate_passes_when_slice_matches_prefix() {
    let (env, owner, admin, client, vault_id) = setup();
    let slice_id = 200u64;

    // Rule: slice must start with b"ok"
    let rule_bytes = Bytes::from_slice(&env, b"ok");
    let r0 = client.register_composition_rule(&admin, &rule_bytes, &0u32, &0u32);

    let rule_ids = vec![&env, r0];
    client.set_slice_rules(&vault_id, &owner, &slice_id, &rule_ids);

    let slice_data = Bytes::from_slice(&env, b"ok_this_is_valid_data");
    let result = client.validate_slice_with_rules(&slice_id, &slice_data);
    assert!(result.overall_valid);
    assert_eq!(result.outcomes.len(), 1);
    assert!(result.outcomes.get(0).unwrap().passed);
}

#[test]
fn test_validate_fails_when_prefix_mismatch() {
    let (env, owner, admin, client, vault_id) = setup();
    let slice_id = 201u64;

    let rule_bytes = Bytes::from_slice(&env, b"expected");
    let r0 = client.register_composition_rule(&admin, &rule_bytes, &0u32, &0u32);
    client.set_slice_rules(&vault_id, &owner, &slice_id, &vec![&env, r0]);

    let slice_data = Bytes::from_slice(&env, b"wrong_prefix");
    let result = client.validate_slice_with_rules(&slice_id, &slice_data);
    assert!(!result.overall_valid);
    assert!(!result.outcomes.get(0).unwrap().passed);
}

#[test]
fn test_validate_disabled_rule_is_skipped() {
    let (env, owner, admin, client, vault_id) = setup();
    let slice_id = 202u64;

    // Rule that would fail (prefix "X" not present in slice_data).
    let rule_bytes = Bytes::from_slice(&env, b"X");
    let r0 = client.register_composition_rule(&admin, &rule_bytes, &0u32, &0u32);
    // Disable it.
    client.set_rule_enabled(&admin, &r0, &false);

    client.set_slice_rules(&vault_id, &owner, &slice_id, &vec![&env, r0]);

    // Even though the rule would fail, it's disabled → overall valid.
    let slice_data = Bytes::from_slice(&env, b"no_X_here");
    let result = client.validate_slice_with_rules(&slice_id, &slice_data);
    assert!(result.overall_valid);
    assert_eq!(result.outcomes.len(), 0);
}

#[test]
fn test_validate_empty_rule_bytes_unconditional_pass() {
    let (env, owner, admin, client, vault_id) = setup();
    let slice_id = 203u64;

    // Empty rule_bytes → unconditional pass.
    let rule_bytes = Bytes::from_slice(&env, b"");
    let r0 = client.register_composition_rule(&admin, &rule_bytes, &0u32, &0u32);
    client.set_slice_rules(&vault_id, &owner, &slice_id, &vec![&env, r0]);

    let slice_data = Bytes::from_slice(&env, b"anything");
    let result = client.validate_slice_with_rules(&slice_id, &slice_data);
    assert!(result.overall_valid);
}

#[test]
fn test_validate_conflict_detection() {
    let (env, owner, admin, client, vault_id) = setup();
    let slice_id = 204u64;

    // Two rules with the same priority: one will pass, one will fail.
    let pass_rule = Bytes::from_slice(&env, b"ok"); // passes for "ok..."
    let fail_rule = Bytes::from_slice(&env, b"bad"); // fails for "ok..."

    let r_pass = client.register_composition_rule(&admin, &pass_rule, &5u32, &0u32);
    let r_fail = client.register_composition_rule(&admin, &fail_rule, &5u32, &0u32);

    client.set_slice_rules(&vault_id, &owner, &slice_id, &vec![&env, r_pass, r_fail]);

    let slice_data = Bytes::from_slice(&env, b"ok_data");
    let result = client.validate_slice_with_rules(&slice_id, &slice_data);

    // Conflict → overall_valid == false.
    assert!(!result.overall_valid);
    // conflicts vec contains 2 entries (one conflict pair).
    assert_eq!(result.conflicts.len(), 2);
}

#[test]
fn test_validate_multiple_rules_priority_order() {
    let (env, owner, admin, client, vault_id) = setup();
    let slice_id = 205u64;

    // Three rules at different priorities — all should pass for "abc_xyz".
    let r0 =
        client.register_composition_rule(&admin, &Bytes::from_slice(&env, b"a"), &10u32, &0u32);
    let r1 = client.register_composition_rule(&admin, &Bytes::from_slice(&env, b""), &1u32, &0u32);
    let r2 =
        client.register_composition_rule(&admin, &Bytes::from_slice(&env, b"ab"), &5u32, &0u32);

    // Register in non-priority order.
    client.set_slice_rules(&vault_id, &owner, &slice_id, &vec![&env, r0, r1, r2]);

    let slice_data = Bytes::from_slice(&env, b"abc_xyz");
    let result = client.validate_slice_with_rules(&slice_id, &slice_data);

    assert!(result.overall_valid);
    assert_eq!(result.outcomes.len(), 3);
    assert_eq!(result.conflicts.len(), 0);

    // Outcomes must be in ascending priority order: r1(1) < r2(5) < r0(10).
    assert_eq!(result.outcomes.get(0).unwrap().rule_id, r1);
    assert_eq!(result.outcomes.get(1).unwrap().rule_id, r2);
    assert_eq!(result.outcomes.get(2).unwrap().rule_id, r0);
}

//! Tests for bounded on-chain storage of PasskeyUsage and PasskeyAuditLog.
//!
//! Issue: log_passkey_usage and log_passkey_audit_entry previously had no cap,
//! allowing the on-chain Vec to grow without bound on every check-in.  These
//! tests verify that:
//!
//!   1. The PasskeyUsage log never exceeds MAX_PASSKEY_USAGE_ENTRIES entries.
//!   2. The PasskeyAuditLog never exceeds MAX_PASSKEY_AUDIT_ENTRIES entries.
//!   3. When the cap is hit, the *oldest* entry is pruned (ring-buffer semantics).
//!   4. After a large number of check-ins well past the cap, both log sizes
//!      remain bounded — i.e. the growth is O(1), not O(n).
//!   5. Existing event emission (PASSKEY_USAGE_TOPIC, PASSKEY_AUDIT_TOPIC) is
//!      unchanged — every operation still emits its event regardless of pruning.
//!   6. Readers (get_passkey_usage, get_passkey_audit_log) return the bounded,
//!      most-recent slice without error after pruning.

#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger},
    token::StellarAssetClient,
    Address, BytesN, Env, IntoVal, TryIntoVal,
};

// ── helpers ──────────────────────────────────────────────────────────────────

fn setup() -> (Env, Address, Address, TtlVaultContractClient<'static>) {
    let env = Env::default();

    // These cap-boundary tests drive enough lifecycle/check-in operations to
    // push the capped Vecs (MAX_PASSKEY_*_ENTRIES = 1000) to and past their
    // limits. Each `Vec::remove(0)` is O(n), so the loop is O(n²) and blows the
    // small default soroban test budget well before the production cap logic is
    // reached. Lift the budget when building the test environment so the tests
    // actually exercise cap enforcement instead of dying in the harness.
    env.budget().reset_unlimited();

    env.mock_all_auths();

    let owner = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let admin = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token_address = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    StellarAssetClient::new(&env, &token_address).mint(&owner, &1_000_000);

    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, owner, beneficiary, client)
}

/// Perform `n` check-ins against `vault_id` using `passkey_hash`, advancing
/// the ledger timestamp by 61 seconds between each call so the
/// `DEFAULT_MIN_CHECKIN_COOLDOWN` (60 s) is always satisfied.
fn do_check_ins(
    env: &Env,
    client: &TtlVaultContractClient<'_>,
    vault_id: u64,
    owner: &Address,
    passkey_hash: &BytesN<32>,
    n: u32,
) {
    for _ in 0..n {
        client.check_in(&vault_id, owner, passkey_hash, &0u64);
        // Advance by 61 s — just over the 60-second default cooldown.
        env.ledger().with_mut(|li| {
            li.timestamp = li.timestamp.saturating_add(61);
        });
    }
}

// ── PasskeyUsage cap tests ────────────────────────────────────────────────────

/// After exactly MAX_PASSKEY_USAGE_ENTRIES check-ins the log is at the cap.
#[test]
fn test_passkey_usage_at_cap_boundary() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);
    let passkey_hash = BytesN::<32>::from_array(&env, &[1u8; 32]);

    do_check_ins(
        &env,
        &client,
        vault_id,
        &owner,
        &passkey_hash,
        MAX_PASSKEY_USAGE_ENTRIES,
    );

    let usage = client.get_passkey_usage(&vault_id);
    assert_eq!(
        usage.len(),
        MAX_PASSKEY_USAGE_ENTRIES,
        "usage log should be exactly at the cap after {} check-ins",
        MAX_PASSKEY_USAGE_ENTRIES
    );
}

/// After MAX_PASSKEY_USAGE_ENTRIES + 1 check-ins the log does not exceed the cap.
#[test]
fn test_passkey_usage_does_not_exceed_cap() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);
    let passkey_hash = BytesN::<32>::from_array(&env, &[2u8; 32]);

    do_check_ins(
        &env,
        &client,
        vault_id,
        &owner,
        &passkey_hash,
        MAX_PASSKEY_USAGE_ENTRIES + 1,
    );

    let usage = client.get_passkey_usage(&vault_id);
    assert_eq!(
        usage.len(),
        MAX_PASSKEY_USAGE_ENTRIES,
        "usage log must never exceed the cap of {} entries",
        MAX_PASSKEY_USAGE_ENTRIES
    );
}

/// After exceeding the cap, the oldest entry is dropped (ring-buffer semantics):
/// the entry at index 0 after cap+1 insertions should be the *second* entry
/// ever written, not the first.
#[test]
fn test_passkey_usage_oldest_entry_pruned() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    // Use two distinct passkey hashes so we can tell which entry survived.
    let hash_a = BytesN::<32>::from_array(&env, &[0xAAu8; 32]);
    let hash_b = BytesN::<32>::from_array(&env, &[0xBBu8; 32]);

    // First check-in with hash_a — this will be the entry that gets pruned.
    client.check_in(&vault_id, &owner, &hash_a, &0u64);
    env.ledger().with_mut(|li| li.timestamp += 61);

    // Fill the rest of the cap with hash_b entries.
    do_check_ins(
        &env,
        &client,
        vault_id,
        &owner,
        &hash_b,
        MAX_PASSKEY_USAGE_ENTRIES,
    );

    let usage = client.get_passkey_usage(&vault_id);

    // Total entries in vec should be capped at MAX_PASSKEY_USAGE_ENTRIES.
    assert_eq!(usage.len(), MAX_PASSKEY_USAGE_ENTRIES);

    // The very first entry (hash_a) must have been pruned.
    let oldest = usage.get(0).unwrap();
    assert_eq!(
        oldest.passkey_hash, hash_b,
        "oldest retained entry should be hash_b; hash_a (the very first) must have been pruned"
    );

    // The most-recent entry at the tail should also be hash_b.
    let newest = usage.get(MAX_PASSKEY_USAGE_ENTRIES - 1).unwrap();
    assert_eq!(newest.passkey_hash, hash_b);
}

/// After a large number of check-ins (cap × 2) the log size stays bounded.
#[test]
fn test_passkey_usage_bounded_after_many_check_ins() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);
    let passkey_hash = BytesN::<32>::from_array(&env, &[3u8; 32]);

    let large_n = MAX_PASSKEY_USAGE_ENTRIES * 2;
    do_check_ins(&env, &client, vault_id, &owner, &passkey_hash, large_n);

    let usage = client.get_passkey_usage(&vault_id);
    assert_eq!(
        usage.len(),
        MAX_PASSKEY_USAGE_ENTRIES,
        "after {} check-ins log size must still be capped at {}",
        large_n,
        MAX_PASSKEY_USAGE_ENTRIES
    );
}

/// Every check-in still emits PASSKEY_USAGE_TOPIC — pruning must not suppress
/// events, since off-chain indexers rely on them for full history.
#[test]
fn test_passkey_usage_events_emitted_past_cap() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);
    let passkey_hash = BytesN::<32>::from_array(&env, &[4u8; 32]);

    // Perform a handful of check-ins past the cap to verify event emission.
    // We use a small number here to keep the test fast; the invariant is the
    // same regardless of how many extra check-ins are performed.
    let extra = 5u32;
    do_check_ins(
        &env,
        &client,
        vault_id,
        &owner,
        &passkey_hash,
        MAX_PASSKEY_USAGE_ENTRIES + extra,
    );

    let total_usage_events = env
        .events()
        .all()
        .iter()
        .filter(|e| {
            let topics: soroban_sdk::Vec<soroban_sdk::Val> = e.1.clone().into_val(&env);
            topics
                .get(0)
                .and_then(|v| v.try_into_val(&env).ok())
                .is_some_and(|s: soroban_sdk::Symbol| s == PASSKEY_USAGE_TOPIC)
        })
        .count();

    assert_eq!(
        total_usage_events as u32,
        MAX_PASSKEY_USAGE_ENTRIES + extra,
        "every check-in must emit a usage event regardless of on-chain pruning"
    );
}

// ── PasskeyAuditLog cap tests ─────────────────────────────────────────────────

/// Helper: add+remove a passkey `n` times to produce 2n audit entries.
fn do_audit_cycles(
    env: &Env,
    client: &TtlVaultContractClient<'_>,
    vault_id: u64,
    owner: &Address,
    n: u32,
) {
    for i in 0..n {
        // Use a distinct hash per cycle to avoid duplicate-registration errors.
        let mut raw = [0u8; 32];
        raw[0] = (i & 0xFF) as u8;
        raw[1] = ((i >> 8) & 0xFF) as u8;
        let hash = BytesN::<32>::from_array(env, &raw);
        client.add_passkey(&vault_id, owner, &hash);
        client.remove_passkey(&vault_id, owner, &hash);
        env.ledger().with_mut(|li| {
            li.timestamp = li.timestamp.saturating_add(1);
        });
    }
}

/// After MAX_PASSKEY_AUDIT_ENTRIES operations the log is at the cap.
#[test]
fn test_passkey_audit_at_cap_boundary() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    // Each cycle produces 2 entries (add + remove).
    let cycles = MAX_PASSKEY_AUDIT_ENTRIES / 2;
    do_audit_cycles(&env, &client, vault_id, &owner, cycles);

    let log = client.get_passkey_audit_log(&vault_id);
    assert_eq!(
        log.len(),
        MAX_PASSKEY_AUDIT_ENTRIES,
        "audit log should be exactly at the cap after {} operations",
        MAX_PASSKEY_AUDIT_ENTRIES
    );
}

/// After exceeding MAX_PASSKEY_AUDIT_ENTRIES operations the log does not grow.
#[test]
fn test_passkey_audit_does_not_exceed_cap() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    // Produce MAX_PASSKEY_AUDIT_ENTRIES + 2 entries (cap/2 + 1 full cycles).
    let cycles = MAX_PASSKEY_AUDIT_ENTRIES / 2 + 1;
    do_audit_cycles(&env, &client, vault_id, &owner, cycles);

    let log = client.get_passkey_audit_log(&vault_id);
    assert_eq!(
        log.len(),
        MAX_PASSKEY_AUDIT_ENTRIES,
        "audit log must never exceed the cap of {} entries",
        MAX_PASSKEY_AUDIT_ENTRIES
    );
}

/// After exceeding the cap, the oldest audit entry is dropped.
#[test]
fn test_passkey_audit_oldest_entry_pruned() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    // Insert a single "add" entry with a known hash as the very first operation.
    let sentinel_hash = BytesN::<32>::from_array(&env, &[0xFFu8; 32]);
    client.add_passkey(&vault_id, &owner, &sentinel_hash);
    env.ledger().with_mut(|li| li.timestamp += 61);

    // Now fill the log with enough entries to push the sentinel off the front.
    // We already have 1 entry; add MAX_PASSKEY_AUDIT_ENTRIES more to guarantee
    // the sentinel (at index 0) has been pruned.
    let cycles = MAX_PASSKEY_AUDIT_ENTRIES / 2;
    do_audit_cycles(&env, &client, vault_id, &owner, cycles);

    let log = client.get_passkey_audit_log(&vault_id);

    // Cap should be maintained.
    assert_eq!(log.len(), MAX_PASSKEY_AUDIT_ENTRIES);

    // The sentinel entry (operation "add", hash 0xFF…) must no longer be present.
    let sentinel_still_present = log.iter().any(|e| e.passkey_hash == sentinel_hash);
    assert!(
        !sentinel_still_present,
        "the oldest audit entry (sentinel_hash) should have been pruned"
    );
}

/// After a large number of lifecycle operations the audit log size stays bounded.
#[test]
fn test_passkey_audit_bounded_after_many_operations() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    // 2 entries per cycle; run enough cycles to produce 4× the cap.
    let cycles = MAX_PASSKEY_AUDIT_ENTRIES * 2;
    do_audit_cycles(&env, &client, vault_id, &owner, cycles);

    let log = client.get_passkey_audit_log(&vault_id);
    assert_eq!(
        log.len(),
        MAX_PASSKEY_AUDIT_ENTRIES,
        "after {} operations audit log size must still be capped at {}",
        cycles * 2,
        MAX_PASSKEY_AUDIT_ENTRIES
    );
}

/// Every lifecycle operation still emits PASSKEY_AUDIT_TOPIC — pruning must
/// not suppress events.
#[test]
fn test_passkey_audit_events_emitted_past_cap() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    // Two extra cycles past the cap boundary (4 extra events).
    let cycles = MAX_PASSKEY_AUDIT_ENTRIES / 2 + 2;
    do_audit_cycles(&env, &client, vault_id, &owner, cycles);

    let total_audit_events = env
        .events()
        .all()
        .iter()
        .filter(|e| {
            let topics: soroban_sdk::Vec<soroban_sdk::Val> = e.1.clone().into_val(&env);
            topics
                .get(0)
                .and_then(|v| v.try_into_val(&env).ok())
                .is_some_and(|s: soroban_sdk::Symbol| s == PASSKEY_AUDIT_TOPIC)
        })
        .count();

    // Each cycle = add + remove = 2 audit events.
    let expected_events = cycles * 2;
    assert_eq!(
        total_audit_events as u32, expected_events,
        "every lifecycle operation must emit a pk_audit event regardless of on-chain pruning"
    );
}

/// get_passkey_audit_log reader returns the bounded, most-recent slice without
/// error after pruning has occurred.
#[test]
fn test_get_passkey_audit_log_works_after_pruning() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let cycles = MAX_PASSKEY_AUDIT_ENTRIES / 2 + 5;
    do_audit_cycles(&env, &client, vault_id, &owner, cycles);

    // Reader should succeed and return exactly the cap worth of entries.
    let log = client.get_passkey_audit_log(&vault_id);
    assert_eq!(
        log.len(),
        MAX_PASSKEY_AUDIT_ENTRIES,
        "get_passkey_audit_log must work correctly after pruning"
    );
}

/// get_passkey_usage reader returns the bounded slice without error after pruning.
#[test]
fn test_get_passkey_usage_works_after_pruning() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);
    let passkey_hash = BytesN::<32>::from_array(&env, &[5u8; 32]);

    do_check_ins(
        &env,
        &client,
        vault_id,
        &owner,
        &passkey_hash,
        MAX_PASSKEY_USAGE_ENTRIES + 10,
    );

    let usage = client.get_passkey_usage(&vault_id);
    assert_eq!(
        usage.len(),
        MAX_PASSKEY_USAGE_ENTRIES,
        "get_passkey_usage must work correctly after pruning"
    );
}

#![cfg(test)]

//! Tests for scheduled consistency re-checks for long-lived attestations
//! (`next_check_due` / `is_consistency_check_due` /
//! `reschedule_consistency_check`).
//!
//! Lives as a `tests/` integration target — rather than in `src/test.rs` —
//! following the precedent set by `tests/lattice_and_masking.rs`, because
//! `src/test.rs` has pre-existing compile errors unrelated to this feature.

use soroban_sdk::{
    bytes,
    testutils::{Address as _, Events as _, Ledger},
    Address, Env, IntoVal, TryIntoVal,
};
use zk_verifier::{ZkVerifierContract, ZkVerifierContractClient, CONSISTENCY_CHECK_INTERVAL};

fn setup() -> (Env, ZkVerifierContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let id = env.register_contract(None, ZkVerifierContract);
    let client = ZkVerifierContractClient::new(&env, &id);
    client.initialize(&admin);
    let client: ZkVerifierContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, client)
}

/// Attests a fixed (proof, claim) pair under a fresh oracle and returns its
/// credential_id.
fn attested_credential(env: &Env, client: &ZkVerifierContractClient<'static>) -> u64 {
    let oracle = Address::generate(env);
    client.register_oracle(&oracle);
    let proof = bytes!(env, 0xdeadbeef);
    let claim = bytes!(env, 0xcafebabe);
    client.attest(&oracle, &proof, &claim)
}

/// Returns true if a `cons_due` event was emitted for `credential_id`.
fn consistency_due_event(env: &Env, credential_id: u64) -> bool {
    env.events().all().iter().any(|e| {
        let topics: soroban_sdk::Vec<soroban_sdk::Val> = e.1.clone().into_val(env);
        if !topics
            .get(0)
            .and_then(|t| t.try_into_val(env).ok())
            .map(|s: soroban_sdk::Symbol| s == soroban_sdk::symbol_short!("cons_due"))
            .unwrap_or(false)
        {
            return false;
        }
        let data: (u64, u64) = e.2.clone().into_val(env);
        data.0 == credential_id
    })
}

// ---- scheduling window logic ----

/// A fresh attestation schedules its first check exactly one interval out;
/// the window is inclusive of the due timestamp itself.
#[test]
fn test_check_not_due_until_scheduled_timestamp() {
    let (env, client) = setup();
    env.ledger().set_timestamp(1_000);
    let credential_id = attested_credential(&env, &client);

    let due_at = 1_000 + CONSISTENCY_CHECK_INTERVAL;

    assert!(!client.is_consistency_check_due(&credential_id));
    env.ledger().set_timestamp(due_at - 1);
    assert!(
        !client.is_consistency_check_due(&credential_id),
        "one second before due_at must not be due"
    );
    env.ledger().set_timestamp(due_at);
    assert!(
        client.is_consistency_check_due(&credential_id),
        "exactly at due_at must be due"
    );
}

/// A due check stays due across repeated polls until rescheduled — a worker
/// that misses the first poll can still catch it on the next one.
#[test]
fn test_due_check_remains_due_until_rescheduled() {
    let (env, client) = setup();
    env.ledger().set_timestamp(1_000);
    let credential_id = attested_credential(&env, &client);

    env.ledger().set_timestamp(1_000 + CONSISTENCY_CHECK_INTERVAL);
    assert!(client.is_consistency_check_due(&credential_id));
    env.ledger().set_timestamp(1_000 + CONSISTENCY_CHECK_INTERVAL + 5_000);
    assert!(
        client.is_consistency_check_due(&credential_id),
        "still due until rescheduled"
    );
}

/// A credential id that was never attested has no schedule: never due, and
/// no event.
#[test]
fn test_unknown_credential_never_due() {
    let (env, client) = setup();
    assert!(!client.is_consistency_check_due(&999u64));
    assert!(!consistency_due_event(&env, 999u64));
    assert_eq!(env.events().all().len(), 0);
}

/// Re-attesting an existing (proof, claim) pair refreshes the schedule — an
/// overdue credential that gets re-attested is no longer due.
#[test]
fn test_reattestation_reschedules_window() {
    let (env, client) = setup();
    env.ledger().set_timestamp(1_000);
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle, &proof, &claim);

    env.ledger().set_timestamp(1_000 + CONSISTENCY_CHECK_INTERVAL + 10);
    assert!(client.is_consistency_check_due(&credential_id));

    env.ledger().set_timestamp(5_000);
    client.attest(&oracle, &proof, &claim);
    assert!(!client.is_consistency_check_due(&credential_id));
    env.ledger().set_timestamp(5_000 + CONSISTENCY_CHECK_INTERVAL);
    assert!(client.is_consistency_check_due(&credential_id));
}

/// create_derived_credential schedules a consistency check for the derived
/// credential just like attest does.
#[test]
fn test_derived_credential_schedules_check() {
    let (env, client) = setup();
    env.ledger().set_timestamp(1_000);
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let parent = client.attest(&oracle, &bytes!(&env, 0x01), &bytes!(&env, 0x02));
    let child = client.create_derived_credential(
        &oracle,
        &parent,
        &bytes!(&env, 0x03),
        &bytes!(&env, 0x04),
    );

    assert!(!client.is_consistency_check_due(&child));
    env.ledger().set_timestamp(1_000 + CONSISTENCY_CHECK_INTERVAL);
    assert!(client.is_consistency_check_due(&child));
}

// ---- cons_due event ----

/// The `cons_due` event carries (credential_id, next_check_due) so workers
/// can act on the exact credential, and only fires once the check is due.
#[test]
fn test_due_check_emits_event_with_id_and_due_at() {
    let (env, client) = setup();
    env.ledger().set_timestamp(1_000);
    let credential_id = attested_credential(&env, &client);
    let due_at = 1_000 + CONSISTENCY_CHECK_INTERVAL;

    assert!(!client.is_consistency_check_due(&credential_id));
    assert!(
        !consistency_due_event(&env, credential_id),
        "no cons_due event before the check is due"
    );

    env.ledger().set_timestamp(due_at);
    assert!(client.is_consistency_check_due(&credential_id));

    let event = env.events().all().iter().find(|e| {
        let topics: soroban_sdk::Vec<soroban_sdk::Val> = e.1.clone().into_val(&env);
        topics
            .get(0)
            .and_then(|t| t.try_into_val(&env).ok())
            .map(|s: soroban_sdk::Symbol| s == soroban_sdk::symbol_short!("cons_due"))
            .unwrap_or(false)
    });
    assert!(event.is_some(), "cons_due event not emitted when check is due");
    let data: (u64, u64) = event.unwrap().2.clone().into_val(&env);
    assert_eq!(data, (credential_id, due_at));
}

// ---- reschedule ----

/// After a worker performs the re-check, reschedule_consistency_check pushes
/// the next window out by a full interval from the reschedule time.
#[test]
fn test_reschedule_advances_window() {
    let (env, client) = setup();
    env.ledger().set_timestamp(1_000);
    let credential_id = attested_credential(&env, &client);

    env.ledger().set_timestamp(1_000 + CONSISTENCY_CHECK_INTERVAL);
    assert!(client.is_consistency_check_due(&credential_id));

    // Worker completes the check and reschedules from a later timestamp.
    env.ledger().set_timestamp(2_000);
    client.reschedule_consistency_check(&credential_id);
    assert!(
        !client.is_consistency_check_due(&credential_id),
        "rescheduled check must not be immediately due"
    );

    env.ledger().set_timestamp(2_000 + CONSISTENCY_CHECK_INTERVAL - 1);
    assert!(!client.is_consistency_check_due(&credential_id));
    env.ledger().set_timestamp(2_000 + CONSISTENCY_CHECK_INTERVAL);
    assert!(client.is_consistency_check_due(&credential_id));
}

/// Rescheduling a credential id that was never attested panics with
/// CredentialNotFound.
#[test]
#[should_panic(expected = "Error(Contract, #12)")]
fn test_reschedule_unknown_credential_panics() {
    let (_, client) = setup();
    client.reschedule_consistency_check(&999u64);
}

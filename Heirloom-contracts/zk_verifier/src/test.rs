#![cfg(test)]

use super::*;
use soroban_sdk::{
    bytes,
    testutils::{Address as _, Events as _, Ledger},
    vec, Bytes, Env,
};

fn setup() -> (Env, Address, ZkVerifierContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let id = env.register_contract(None, ZkVerifierContract);
    let client = ZkVerifierContractClient::new(&env, &id);
    client.initialize(&admin);
    let client: ZkVerifierContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, admin, client)
}

// ---- existing interface: empty inputs still panic ----

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_malformed_empty_proof_panics() {
    let (env, _, client) = setup();
    let proof = bytes!(&env,);
    let claim = bytes!(&env, 0xcafebabe);
    client.verify_claim(&proof, &claim);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_malformed_empty_claim_panics() {
    let (env, _, client) = setup();
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env,);
    client.verify_claim(&proof, &claim);
}

// ---- oracle model tests ----

/// An unattested (proof, claim) pair — never attested by any oracle — must
/// return `false`, not panic.
#[test]
fn test_unattested_proof_returns_false() {
    let (env, _, client) = setup();
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    assert!(!client.verify_claim(&proof, &claim));
}

// ── Existing correctness tests ────────────────────────────────────────────────

/// A (proof, claim) pair attested by a currently-registered oracle — must
/// return true.
#[test]
fn test_attested_proof_returns_true() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);

    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    client.attest(&oracle, &proof, &claim);

    assert!(client.verify_claim(&proof, &claim));
}

/// Attesting one proof does not validate a different, unattested proof for
/// the same claim.
#[test]
fn test_different_proof_not_validated_after_attestation() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);

    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    client.attest(&oracle, &proof, &claim);

    let other_proof = bytes!(&env, 0x1234);
    assert!(!client.verify_claim(&other_proof, &claim));
}

/// Once the attesting oracle is revoked, its previously-stored attestation
/// must no longer be honored — `verify_claim` returns `false`, not panic.
#[test]
fn test_revoked_oracle_attestation_no_longer_accepted() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);

    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    client.attest(&oracle, &proof, &claim);

    // Attestation is honored while the oracle remains registered.
    assert!(client.verify_claim(&proof, &claim));

    // Revoking the oracle does not delete the stored attestation record, but
    // verify_claim must stop honoring it since the attesting oracle is no
    // longer currently registered.
    client.revoke_oracle(&oracle);
    assert!(!client.is_oracle(&oracle));
    assert!(!client.verify_claim(&proof, &claim));
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_unregistered_oracle_cannot_attest() {
    let (env, _, client) = setup();
    let rogue = Address::generate(&env);

    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    client.attest(&rogue, &proof, &claim);
}

#[test]
fn test_register_and_is_oracle() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);

    assert!(!client.is_oracle(&oracle));
    client.register_oracle(&oracle);
    assert!(client.is_oracle(&oracle));
    client.revoke_oracle(&oracle);
    assert!(!client.is_oracle(&oracle));
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_double_initialize_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let id = env.register_contract(None, ZkVerifierContract);
    let client = ZkVerifierContractClient::new(&env, &id);
    client.initialize(&admin);
    client.initialize(&admin);
}

// ── #817: Input size limit tests ──────────────────────────────────────────────

/// Proof at exactly MAX_PROOF_SIZE — must pass validation (no panic) and,
/// once attested by a registered oracle, must verify as true.
#[test]
fn test_proof_at_max_size_succeeds() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);

    let data = [0xffu8; MAX_PROOF_SIZE as usize];
    let proof = Bytes::from_slice(&env, &data);
    let claim = bytes!(&env, 0xcafebabe);

    // Unattested: passes validation, but returns false.
    assert!(!client.verify_claim(&proof, &claim));

    client.attest(&oracle, &proof, &claim);
    assert!(client.verify_claim(&proof, &claim));
}

/// Proof one byte over MAX_PROOF_SIZE — must panic with ProofTooLarge (#3).
#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_proof_exceeds_max_size_panics() {
    let (env, _, client) = setup();
    let data = [0xffu8; MAX_PROOF_SIZE as usize + 1];
    let proof = Bytes::from_slice(&env, &data);
    let claim = bytes!(&env, 0xcafebabe);
    client.verify_claim(&proof, &claim);
}

/// Claim at exactly MAX_CLAIM_SIZE — must pass validation (no panic) and,
/// once attested by a registered oracle, must verify as true.
#[test]
fn test_claim_at_max_size_succeeds() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);

    let proof = bytes!(&env, 0xdeadbeef);
    let data = [0xaau8; MAX_CLAIM_SIZE as usize];
    let claim = Bytes::from_slice(&env, &data);

    // Unattested: passes validation, but returns false.
    assert!(!client.verify_claim(&proof, &claim));

    client.attest(&oracle, &proof, &claim);
    assert!(client.verify_claim(&proof, &claim));
}

/// Claim one byte over MAX_CLAIM_SIZE — must panic with ClaimTooLarge (#4).
#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_claim_exceeds_max_size_panics() {
    let (env, _, client) = setup();
    let proof = bytes!(&env, 0xdeadbeef);
    let data = [0xaau8; MAX_CLAIM_SIZE as usize + 1];
    let claim = Bytes::from_slice(&env, &data);
    client.verify_claim(&proof, &claim);
}

// ── #818: Event emission tests ────────────────────────────────────────────────

/// verify_claim with an attested proof must emit exactly one vfy_claim event.
#[test]
fn test_verify_claim_emits_event_on_true_result() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);

    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    client.attest(&oracle, &proof, &claim);

    // attest() does not itself publish any events, so verify_claim() is the
    // only event source here.
    let result = client.verify_claim(&proof, &claim);
    assert!(result);
    assert_eq!(env.events().all().len(), 1);
}

/// verify_claim with an unattested proof must emit exactly one vfy_claim
/// event even when the result is false.
#[test]
fn test_verify_claim_emits_event_on_false_result() {
    let (env, _, client) = setup();
    let proof = bytes!(&env, 0xdeadbeef); // never attested → result = false
    let claim = bytes!(&env, 0xcafebabe);
    let result = client.verify_claim(&proof, &claim);
    assert!(!result);
    assert_eq!(env.events().all().len(), 1);
}

// ── Credential dispute tests ──────────────────────────────────────────────────

/// Registers `n` fresh oracles and returns them.
fn register_oracles(env: &Env, client: &ZkVerifierContractClient<'static>, n: u32) -> Vec<Address> {
    let mut oracles = Vec::new(env);
    for _ in 0..n {
        let oracle = Address::generate(env);
        client.register_oracle(&oracle);
        oracles.push_back(oracle);
    }
    oracles
}

/// attest() returns a stable credential_id, starting at 1.
#[test]
fn test_attest_returns_credential_id() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);

    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle, &proof, &claim);
    assert_eq!(credential_id, 1);

    // Re-attesting the same (proof, claim) reuses the same credential id.
    let credential_id_again = client.attest(&oracle, &proof, &claim);
    assert_eq!(credential_id_again, 1);

    // A distinct (proof, claim) pair gets a new id.
    let other_claim = bytes!(&env, 0x1234);
    let other_id = client.attest(&oracle, &proof, &other_claim);
    assert_eq!(other_id, 2);
}

/// Disputing a credential id that was never attested must panic.
#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_dispute_unknown_credential_panics() {
    let (env, _, client) = setup();
    let initiator = Address::generate(&env);
    let reason = bytes!(&env, 0xaa);
    client.initiate_credential_dispute(&999u64, &initiator, &reason);
}

/// An empty dispute reason must panic.
#[test]
#[should_panic(expected = "Error(Contract, #13)")]
fn test_dispute_empty_reason_panics() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle, &proof, &claim);

    let initiator = Address::generate(&env);
    let reason = bytes!(&env,);
    client.initiate_credential_dispute(&credential_id, &initiator, &reason);
}

/// Filing a second dispute while one is still open must panic.
#[test]
#[should_panic(expected = "Error(Contract, #12)")]
fn test_duplicate_open_dispute_panics() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle, &proof, &claim);

    let initiator = Address::generate(&env);
    let reason = bytes!(&env, 0xaa);
    client.initiate_credential_dispute(&credential_id, &initiator, &reason);
    client.initiate_credential_dispute(&credential_id, &initiator, &reason);
}

/// A non-oracle address must not be able to vote.
#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_non_oracle_cannot_vote() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle, &proof, &claim);

    let initiator = Address::generate(&env);
    let reason = bytes!(&env, 0xaa);
    let dispute_id = client.initiate_credential_dispute(&credential_id, &initiator, &reason);

    let rogue = Address::generate(&env);
    client.vote_on_dispute(&dispute_id, &rogue, &true);
}

/// The same oracle voting twice on one dispute must panic.
#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn test_double_vote_panics() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle, &proof, &claim);

    let initiator = Address::generate(&env);
    let reason = bytes!(&env, 0xaa);
    let dispute_id = client.initiate_credential_dispute(&credential_id, &initiator, &reason);

    client.vote_on_dispute(&dispute_id, &oracle, &true);
    client.vote_on_dispute(&dispute_id, &oracle, &false);
}

/// Voting on an already-resolved dispute must panic.
#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn test_vote_after_resolution_panics() {
    let (env, _, client) = setup();
    let oracles = register_oracles(&env, &client, 3);
    let attester = oracles.get(0).unwrap();
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&attester, &proof, &claim);

    let initiator = Address::generate(&env);
    let reason = bytes!(&env, 0xaa);
    let dispute_id = client.initiate_credential_dispute(&credential_id, &initiator, &reason);

    for oracle in oracles.iter() {
        client.vote_on_dispute(&dispute_id, &oracle, &true);
    }
    assert_eq!(
        client.get_dispute(&dispute_id).status,
        DisputeStatus::Upheld
    );

    // A fourth registered oracle tries to vote after resolution.
    let late_oracle = Address::generate(&env);
    client.register_oracle(&late_oracle);
    client.vote_on_dispute(&dispute_id, &late_oracle, &true);
}

/// Reaching the "invalid" vote threshold upholds the dispute, invalidates
/// the credential, and verify_claim must flip to false even though the
/// original attesting oracle is still registered.
#[test]
fn test_dispute_upheld_invalidates_credential() {
    let (env, _, client) = setup();
    let oracles = register_oracles(&env, &client, 3);
    let attester = oracles.get(0).unwrap();
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&attester, &proof, &claim);
    assert!(client.verify_claim(&proof, &claim));

    let initiator = Address::generate(&env);
    let reason = bytes!(&env, 0xaa);
    let dispute_id = client.initiate_credential_dispute(&credential_id, &initiator, &reason);

    for oracle in oracles.iter() {
        client.vote_on_dispute(&dispute_id, &oracle, &true);
    }

    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.status, DisputeStatus::Upheld);
    assert_eq!(dispute.votes_for, 3);
    assert!(client.is_credential_invalidated(&credential_id));
    // The attester was never revoked, but the dispute still invalidates it.
    assert!(client.is_oracle(&attester));
    assert!(!client.verify_claim(&proof, &claim));
}

/// Reaching the "valid" vote threshold rejects the dispute and leaves the
/// credential honored by verify_claim.
#[test]
fn test_dispute_rejected_credential_stays_valid() {
    let (env, _, client) = setup();
    let oracles = register_oracles(&env, &client, 3);
    let attester = oracles.get(0).unwrap();
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&attester, &proof, &claim);

    let initiator = Address::generate(&env);
    let reason = bytes!(&env, 0xaa);
    let dispute_id = client.initiate_credential_dispute(&credential_id, &initiator, &reason);

    for oracle in oracles.iter() {
        client.vote_on_dispute(&dispute_id, &oracle, &false);
    }

    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.status, DisputeStatus::Rejected);
    assert!(!client.is_credential_invalidated(&credential_id));
    assert!(client.verify_claim(&proof, &claim));
}

/// A resolved dispute clears the "open dispute" lock, and dispute history
/// for the credential accumulates every filed dispute.
#[test]
fn test_credential_dispute_history_tracks_all_disputes() {
    let (env, _, client) = setup();
    let oracles = register_oracles(&env, &client, 3);
    let attester = oracles.get(0).unwrap();
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&attester, &proof, &claim);

    let initiator = Address::generate(&env);
    let reason = bytes!(&env, 0xaa);

    let first = client.initiate_credential_dispute(&credential_id, &initiator, &reason);
    for oracle in oracles.iter() {
        client.vote_on_dispute(&first, &oracle, &false);
    }

    let second = client.initiate_credential_dispute(&credential_id, &initiator, &reason);

    let history = client.get_credential_disputes(&credential_id);
    assert_eq!(history.len(), 2);
    assert_eq!(history.get(0).unwrap(), first);
    assert_eq!(history.get(1).unwrap(), second);
}

/// Admin can lower the dispute threshold so fewer votes are needed to
/// resolve; a non-default threshold of 1 resolves on the first vote.
#[test]
fn test_admin_configurable_dispute_threshold() {
    let (env, _admin, client) = setup();
    client.set_dispute_threshold(&1u32);
    assert_eq!(client.dispute_threshold(), 1u32);

    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle, &proof, &claim);

    let initiator = Address::generate(&env);
    let reason = bytes!(&env, 0xaa);
    let dispute_id = client.initiate_credential_dispute(&credential_id, &initiator, &reason);

    client.vote_on_dispute(&dispute_id, &oracle, &true);
    assert_eq!(
        client.get_dispute(&dispute_id).status,
        DisputeStatus::Upheld
    );
}

/// A zero threshold is rejected.
#[test]
#[should_panic(expected = "Error(Contract, #15)")]
fn test_zero_threshold_rejected() {
    let (_, _, client) = setup();
    client.set_dispute_threshold(&0u32);
}

// ── Credential temporal query tests ───────────────────────────────────────────

/// An unknown credential id has no snapshot history at all.
#[test]
fn test_get_credential_at_time_unknown_credential_returns_none() {
    let (env, _, client) = setup();
    let viewer = Address::generate(&env);
    assert!(client
        .get_credential_at_time(&viewer, &999u64, &0u64)
        .is_none());
}

/// Querying a timestamp before the credential's first snapshot returns None.
#[test]
fn test_get_credential_at_time_before_first_snapshot_returns_none() {
    let (env, _, client) = setup();
    env.ledger().set_timestamp(100);
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle, &proof, &claim);

    let viewer = Address::generate(&env);
    assert!(client
        .get_credential_at_time(&viewer, &credential_id, &50u64)
        .is_none());
}

/// attest() captures a snapshot at the ledger timestamp of attestation.
/// Querying at that timestamp, or any later one, returns it.
#[test]
fn test_get_credential_at_time_returns_snapshot_at_and_after_attest() {
    let (env, _, client) = setup();
    env.ledger().set_timestamp(100);
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle, &proof, &claim);

    let viewer = Address::generate(&env);
    let snapshot = client
        .get_credential_at_time(&viewer, &credential_id, &100u64)
        .unwrap();
    assert_eq!(snapshot.credential_id, credential_id);
    assert_eq!(snapshot.oracle, oracle);
    assert!(!snapshot.invalidated);
    assert_eq!(snapshot.timestamp, 100);

    env.ledger().set_timestamp(500);
    let later = client
        .get_credential_at_time(&viewer, &credential_id, &500u64)
        .unwrap();
    assert_eq!(later.timestamp, 100);
}

/// The historical query must reflect state as of the queried timestamp, not
/// the credential's current state: valid before a dispute resolves, invalid
/// from the moment it does.
#[test]
fn test_get_credential_at_time_reflects_state_before_and_after_dispute() {
    let (env, _, client) = setup();
    env.ledger().set_timestamp(100);
    let oracles = register_oracles(&env, &client, 3);
    let attester = oracles.get(0).unwrap();
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&attester, &proof, &claim);

    env.ledger().set_timestamp(200);
    let initiator = Address::generate(&env);
    let reason = bytes!(&env, 0xaa);
    let dispute_id = client.initiate_credential_dispute(&credential_id, &initiator, &reason);

    env.ledger().set_timestamp(300);
    for oracle in oracles.iter() {
        client.vote_on_dispute(&dispute_id, &oracle, &true);
    }
    assert!(client.is_credential_invalidated(&credential_id));

    let viewer = Address::generate(&env);
    // As of a moment before the dispute was even filed, the credential was
    // valid — even though it is invalid *now*.
    let before = client
        .get_credential_at_time(&viewer, &credential_id, &150u64)
        .unwrap();
    assert!(!before.invalidated);

    // As of the moment the dispute resolved, it was already invalid.
    let after = client
        .get_credential_at_time(&viewer, &credential_id, &300u64)
        .unwrap();
    assert!(after.invalidated);
    assert_eq!(after.timestamp, 300);
}

/// Re-attesting a credential under a different oracle records a new
/// snapshot without erasing the earlier one — a query at the old timestamp
/// still reports the original oracle.
#[test]
fn test_get_credential_at_time_tracks_oracle_change_on_reattestation() {
    let (env, _, client) = setup();
    env.ledger().set_timestamp(100);
    let oracle_a = Address::generate(&env);
    let oracle_b = Address::generate(&env);
    client.register_oracle(&oracle_a);
    client.register_oracle(&oracle_b);

    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle_a, &proof, &claim);

    env.ledger().set_timestamp(200);
    let reattested_id = client.attest(&oracle_b, &proof, &claim);
    assert_eq!(reattested_id, credential_id);

    let viewer = Address::generate(&env);
    let early = client
        .get_credential_at_time(&viewer, &credential_id, &150u64)
        .unwrap();
    assert_eq!(early.oracle, oracle_a);

    let late = client
        .get_credential_at_time(&viewer, &credential_id, &200u64)
        .unwrap();
    assert_eq!(late.oracle, oracle_b);
}

/// Two state changes landing at the same ledger timestamp overwrite the
/// snapshot in place instead of duplicating the timestamp index — a dispute
/// filed and resolved without any ledger-time advance still yields exactly
/// one snapshot for that timestamp, holding the latest state.
#[test]
fn test_get_credential_at_time_same_timestamp_overwrites_snapshot() {
    let (env, _, client) = setup();
    env.ledger().set_timestamp(1);
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    client.set_dispute_threshold(&1u32);
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle, &proof, &claim);

    let initiator = Address::generate(&env);
    let reason = bytes!(&env, 0xaa);
    let dispute_id = client.initiate_credential_dispute(&credential_id, &initiator, &reason);
    client.vote_on_dispute(&dispute_id, &oracle, &true);
    assert!(client.is_credential_invalidated(&credential_id));

    // All three state changes (attest, dispute filed, dispute resolved)
    // happened at timestamp 1; the query must return the latest one.
    let viewer = Address::generate(&env);
    let snapshot = client
        .get_credential_at_time(&viewer, &credential_id, &1u64)
        .unwrap();
    assert!(snapshot.invalidated);
    assert_eq!(snapshot.timestamp, 1);
}

/// The snapshot history is capped at MAX_CREDENTIAL_SNAPSHOTS per
/// credential: once exceeded, the oldest snapshot is pruned and becomes
/// unqueryable, while the next-oldest remains.
#[test]
fn test_credential_snapshot_retention_prunes_oldest() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);

    env.ledger().set_timestamp(1);
    let credential_id = client.attest(&oracle, &proof, &claim);

    // Re-attest at a fresh timestamp each time so every call records a
    // distinct snapshot, until retention is forced to prune the oldest.
    for ts in 2..=(MAX_CREDENTIAL_SNAPSHOTS as u64 + 1) {
        env.ledger().set_timestamp(ts);
        client.attest(&oracle, &proof, &claim);
    }

    // The very first snapshot (timestamp 1) has been pruned...
    let viewer = Address::generate(&env);
    assert!(client
        .get_credential_at_time(&viewer, &credential_id, &1u64)
        .is_none());
    // ...but the second (timestamp 2) is still the oldest retained, and is
    // what a query for anything before it now falls back to finding absent.
    let oldest_retained = client
        .get_credential_at_time(&viewer, &credential_id, &2u64)
        .unwrap();
    assert_eq!(oldest_retained.timestamp, 2);
}

// ── Credential privacy tests ──────────────────────────────────────────────────

/// A freshly attested credential defaults to `Public` — anyone may read its
/// state via `get_credential_at_time` without it being set explicitly.
#[test]
fn test_credential_privacy_defaults_to_public() {
    let (env, _, client) = setup();
    env.ledger().set_timestamp(1);
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle, &proof, &claim);

    assert_eq!(
        client.credential_privacy(&credential_id),
        PrivacyLevel::Public
    );

    let stranger = Address::generate(&env);
    assert!(client
        .get_credential_at_time(&stranger, &credential_id, &1u64)
        .is_some());
}

/// Setting privacy on a credential id that was never attested must panic.
#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_set_credential_privacy_unknown_credential_panics() {
    let (_, _, client) = setup();
    client.set_credential_privacy(&999u64, &PrivacyLevel::Internal);
}

/// The admin can change a credential's privacy level, and `credential_privacy`
/// reflects the update.
#[test]
fn test_set_credential_privacy_updates_getter() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle, &proof, &claim);

    client.set_credential_privacy(&credential_id, &PrivacyLevel::Confidential);
    assert_eq!(
        client.credential_privacy(&credential_id),
        PrivacyLevel::Confidential
    );
}

/// An `Internal` credential is readable by the admin and by any registered
/// oracle (not just the one that attested it).
#[test]
fn test_internal_privacy_allows_admin_and_any_oracle() {
    let (env, admin, client) = setup();
    env.ledger().set_timestamp(1);
    let attester = Address::generate(&env);
    client.register_oracle(&attester);
    let other_oracle = Address::generate(&env);
    client.register_oracle(&other_oracle);

    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&attester, &proof, &claim);
    client.set_credential_privacy(&credential_id, &PrivacyLevel::Internal);

    assert!(client
        .get_credential_at_time(&admin, &credential_id, &1u64)
        .is_some());
    assert!(client
        .get_credential_at_time(&other_oracle, &credential_id, &1u64)
        .is_some());
}

/// An `Internal` credential is not readable by an address that is neither
/// the admin nor a registered oracle.
#[test]
#[should_panic(expected = "Error(Contract, #16)")]
fn test_internal_privacy_denies_stranger() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle, &proof, &claim);
    client.set_credential_privacy(&credential_id, &PrivacyLevel::Internal);

    let stranger = Address::generate(&env);
    client.get_credential_at_time(&stranger, &credential_id, &0u64);
}

/// A `Confidential` credential is readable by the admin...
#[test]
fn test_confidential_privacy_allows_admin() {
    let (env, admin, client) = setup();
    env.ledger().set_timestamp(1);
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle, &proof, &claim);
    client.set_credential_privacy(&credential_id, &PrivacyLevel::Confidential);

    assert!(client
        .get_credential_at_time(&admin, &credential_id, &1u64)
        .is_some());
}

/// ...but not by a registered oracle — `Confidential` is stricter than
/// `Internal` and excludes everyone but the admin, including the oracle that
/// attested the credential in the first place.
#[test]
#[should_panic(expected = "Error(Contract, #16)")]
fn test_confidential_privacy_denies_attesting_oracle() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle, &proof, &claim);
    client.set_credential_privacy(&credential_id, &PrivacyLevel::Confidential);

    client.get_credential_at_time(&oracle, &credential_id, &0u64);
}

// ── Credential version history tests ──────────────────────────────────────────

/// A credential that has never been attested has no versions at all.
#[test]
fn test_credential_version_count_unknown_credential_is_zero() {
    let (env, _, client) = setup();
    assert_eq!(client.credential_version_count(&999u64), 0);
    let viewer = Address::generate(&env);
    assert!(client
        .get_credential_version(&viewer, &999u64, &1u32)
        .is_none());
}

/// attest() creates version 1; re-attesting at a later timestamp advances to
/// version 2, and both remain independently queryable by version number.
#[test]
fn test_get_credential_version_returns_each_recorded_version() {
    let (env, _, client) = setup();
    env.ledger().set_timestamp(100);
    let oracle_a = Address::generate(&env);
    let oracle_b = Address::generate(&env);
    client.register_oracle(&oracle_a);
    client.register_oracle(&oracle_b);

    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle_a, &proof, &claim);
    assert_eq!(client.credential_version_count(&credential_id), 1);

    env.ledger().set_timestamp(200);
    client.attest(&oracle_b, &proof, &claim);
    assert_eq!(client.credential_version_count(&credential_id), 2);

    let viewer = Address::generate(&env);
    let v1 = client
        .get_credential_version(&viewer, &credential_id, &1u32)
        .unwrap();
    assert_eq!(v1.oracle, oracle_a);
    assert_eq!(v1.timestamp, 100);
    assert_eq!(v1.version, 1);

    let v2 = client
        .get_credential_version(&viewer, &credential_id, &2u32)
        .unwrap();
    assert_eq!(v2.oracle, oracle_b);
    assert_eq!(v2.timestamp, 200);
    assert_eq!(v2.version, 2);

    assert!(client
        .get_credential_version(&viewer, &credential_id, &3u32)
        .is_none());
}

/// Multiple state changes landing at the same ledger timestamp (dispute
/// filed and resolved without any ledger-time advance) share a single
/// version number rather than each minting a new one.
#[test]
fn test_get_credential_version_same_timestamp_reuses_version() {
    let (env, _, client) = setup();
    env.ledger().set_timestamp(1);
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    client.set_dispute_threshold(&1u32);
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle, &proof, &claim);
    assert_eq!(client.credential_version_count(&credential_id), 1);

    let initiator = Address::generate(&env);
    let reason = bytes!(&env, 0xaa);
    let dispute_id = client.initiate_credential_dispute(&credential_id, &initiator, &reason);
    client.vote_on_dispute(&dispute_id, &oracle, &true);
    assert!(client.is_credential_invalidated(&credential_id));

    // The dispute resolving at the same timestamp as attest() must not
    // advance the version number, and version 1 now reflects the latest
    // (invalidated) state.
    assert_eq!(client.credential_version_count(&credential_id), 1);
    let viewer = Address::generate(&env);
    let v1 = client
        .get_credential_version(&viewer, &credential_id, &1u32)
        .unwrap();
    assert!(v1.invalidated);
}

/// Version numbers are never reused: once retention prunes the oldest
/// snapshot, its version becomes permanently unqueryable rather than being
/// reassigned to a newer state.
#[test]
fn test_credential_version_retention_prunes_oldest_without_renumbering() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);

    env.ledger().set_timestamp(1);
    let credential_id = client.attest(&oracle, &proof, &claim);

    for ts in 2..=(MAX_CREDENTIAL_SNAPSHOTS as u64 + 1) {
        env.ledger().set_timestamp(ts);
        client.attest(&oracle, &proof, &claim);
    }

    // Version 1 (the very first attestation) has been pruned...
    let viewer = Address::generate(&env);
    assert!(client
        .get_credential_version(&viewer, &credential_id, &1u32)
        .is_none());
    // ...but version 2 is still retained and still identifies itself as
    // version 2, not renumbered to 1.
    let oldest_retained = client
        .get_credential_version(&viewer, &credential_id, &2u32)
        .unwrap();
    assert_eq!(oldest_retained.version, 2);
    assert_eq!(oldest_retained.timestamp, 2);
    // The latest version number keeps counting up regardless of pruning.
    assert_eq!(
        client.credential_version_count(&credential_id),
        MAX_CREDENTIAL_SNAPSHOTS + 1
    );
}

/// diff_credential_versions reports an oracle change and no invalidation
/// change across a re-attestation under a different oracle.
#[test]
fn test_diff_credential_versions_reports_oracle_change() {
    let (env, _, client) = setup();
    env.ledger().set_timestamp(100);
    let oracle_a = Address::generate(&env);
    let oracle_b = Address::generate(&env);
    client.register_oracle(&oracle_a);
    client.register_oracle(&oracle_b);

    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle_a, &proof, &claim);

    env.ledger().set_timestamp(200);
    client.attest(&oracle_b, &proof, &claim);

    let viewer = Address::generate(&env);
    let diff = client.diff_credential_versions(&viewer, &credential_id, &1u32, &2u32);
    assert_eq!(diff.credential_id, credential_id);
    assert_eq!(diff.from_version, 1);
    assert_eq!(diff.to_version, 2);
    assert_eq!(diff.from_timestamp, 100);
    assert_eq!(diff.to_timestamp, 200);
    assert!(diff.oracle_changed);
    assert_eq!(diff.previous_oracle, oracle_a);
    assert_eq!(diff.current_oracle, oracle_b);
    assert!(!diff.invalidated_changed);
    assert!(!diff.previous_invalidated);
    assert!(!diff.current_invalidated);
}

/// diff_credential_versions reports an invalidation change (and no oracle
/// change) across a dispute being upheld.
#[test]
fn test_diff_credential_versions_reports_invalidation_change() {
    let (env, _, client) = setup();
    env.ledger().set_timestamp(100);
    let oracles = register_oracles(&env, &client, 3);
    let attester = oracles.get(0).unwrap();
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&attester, &proof, &claim);

    env.ledger().set_timestamp(200);
    let initiator = Address::generate(&env);
    let reason = bytes!(&env, 0xaa);
    let dispute_id = client.initiate_credential_dispute(&credential_id, &initiator, &reason);
    for oracle in oracles.iter() {
        client.vote_on_dispute(&dispute_id, &oracle, &true);
    }
    assert!(client.is_credential_invalidated(&credential_id));

    let viewer = Address::generate(&env);
    let diff = client.diff_credential_versions(&viewer, &credential_id, &1u32, &2u32);
    assert!(!diff.oracle_changed);
    assert!(diff.invalidated_changed);
    assert!(!diff.previous_invalidated);
    assert!(diff.current_invalidated);
}

/// Diffing a version that was never recorded panics with VersionNotFound.
#[test]
#[should_panic(expected = "Error(Contract, #17)")]
fn test_diff_credential_versions_unknown_version_panics() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle, &proof, &claim);

    let viewer = Address::generate(&env);
    client.diff_credential_versions(&viewer, &credential_id, &1u32, &99u32);
}

/// get_credential_version respects the same privacy gating as
/// get_credential_at_time: a stranger is denied on an Internal credential.
#[test]
#[should_panic(expected = "Error(Contract, #16)")]
fn test_get_credential_version_denies_stranger_on_internal_privacy() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle, &proof, &claim);
    client.set_credential_privacy(&credential_id, &PrivacyLevel::Internal);

    let stranger = Address::generate(&env);
    client.get_credential_version(&stranger, &credential_id, &1u32);
}

// ── Credential hierarchy tests ────────────────────────────────────────────────

/// A root credential (created via plain `attest`) has no recorded parent.
#[test]
fn test_root_credential_has_no_parent() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let proof = bytes!(&env, 0xdeadbeef);
    let claim = bytes!(&env, 0xcafebabe);
    let credential_id = client.attest(&oracle, &proof, &claim);

    assert!(client.get_credential_parent(&credential_id).is_none());
    assert_eq!(
        client.get_credential_chain(&credential_id),
        vec![&env, credential_id]
    );
    assert!(client.is_credential_chain_valid(&credential_id));
}

/// create_derived_credential mints a distinct credential_id, records the
/// given parent, and get_credential_chain reports the two-element ancestry.
#[test]
fn test_create_derived_credential_records_parent() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);

    let degree_proof = bytes!(&env, 0x01);
    let degree_claim = bytes!(&env, 0x02);
    let degree_id = client.attest(&oracle, &degree_proof, &degree_claim);

    let cert_proof = bytes!(&env, 0x03);
    let cert_claim = bytes!(&env, 0x04);
    let cert_id = client.create_derived_credential(&oracle, &degree_id, &cert_proof, &cert_claim);

    assert_ne!(cert_id, degree_id);
    assert_eq!(client.get_credential_parent(&cert_id), Some(degree_id));
    assert_eq!(
        client.get_credential_chain(&cert_id),
        vec![&env, cert_id, degree_id]
    );
}

/// A three-level chain (transcript -> degree -> certificate) reports its
/// full ancestry, oldest-descendant first.
#[test]
fn test_credential_chain_multi_level() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);

    let transcript_id = client.attest(&oracle, &bytes!(&env, 0x01), &bytes!(&env, 0x02));
    let degree_id = client.create_derived_credential(
        &oracle,
        &transcript_id,
        &bytes!(&env, 0x03),
        &bytes!(&env, 0x04),
    );
    let cert_id = client.create_derived_credential(
        &oracle,
        &degree_id,
        &bytes!(&env, 0x05),
        &bytes!(&env, 0x06),
    );

    assert_eq!(
        client.get_credential_chain(&cert_id),
        vec![&env, cert_id, degree_id, transcript_id]
    );
    assert!(client.is_credential_chain_valid(&cert_id));
}

/// Deriving from a parent_id that was never attested must panic.
#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_create_derived_credential_unknown_parent_panics() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    client.create_derived_credential(&oracle, &999u64, &bytes!(&env, 0x01), &bytes!(&env, 0x02));
}

/// A non-oracle address cannot create a derived credential.
#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_create_derived_credential_requires_registered_oracle() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let parent_id = client.attest(&oracle, &bytes!(&env, 0x01), &bytes!(&env, 0x02));

    let rogue = Address::generate(&env);
    client.create_derived_credential(&rogue, &parent_id, &bytes!(&env, 0x03), &bytes!(&env, 0x04));
}

/// Once the immediate parent is invalidated by an upheld dispute, no new
/// credential may be derived from it.
#[test]
#[should_panic(expected = "Error(Contract, #18)")]
fn test_create_derived_credential_invalidated_parent_panics() {
    let (env, _, client) = setup();
    let oracles = register_oracles(&env, &client, 3);
    let attester = oracles.get(0).unwrap();
    let parent_id = client.attest(&attester, &bytes!(&env, 0x01), &bytes!(&env, 0x02));

    let initiator = Address::generate(&env);
    let dispute_id =
        client.initiate_credential_dispute(&parent_id, &initiator, &bytes!(&env, 0xaa));
    for oracle in oracles.iter() {
        client.vote_on_dispute(&dispute_id, &oracle, &true);
    }
    assert!(client.is_credential_invalidated(&parent_id));

    client.create_derived_credential(
        &attester,
        &parent_id,
        &bytes!(&env, 0x03),
        &bytes!(&env, 0x04),
    );
}

/// Recursive validation: the immediate parent is itself valid, but an
/// ancestor further up the chain (the grandparent) has been invalidated.
/// create_derived_credential must still panic — a certificate built on a
/// degree is only as good as the transcript the degree itself rests on.
#[test]
#[should_panic(expected = "Error(Contract, #18)")]
fn test_create_derived_credential_recursive_validation_catches_grandparent() {
    let (env, _, client) = setup();
    let oracles = register_oracles(&env, &client, 3);
    let attester = oracles.get(0).unwrap();

    let transcript_id = client.attest(&attester, &bytes!(&env, 0x01), &bytes!(&env, 0x02));
    let degree_id = client.create_derived_credential(
        &attester,
        &transcript_id,
        &bytes!(&env, 0x03),
        &bytes!(&env, 0x04),
    );

    // Dispute and invalidate the transcript (the grandparent-to-be), not
    // the degree itself.
    let initiator = Address::generate(&env);
    let dispute_id =
        client.initiate_credential_dispute(&transcript_id, &initiator, &bytes!(&env, 0xaa));
    for oracle in oracles.iter() {
        client.vote_on_dispute(&dispute_id, &oracle, &true);
    }
    assert!(client.is_credential_invalidated(&transcript_id));
    // The degree itself was never directly disputed.
    assert!(!client.is_credential_invalidated(&degree_id));
    assert!(!client.is_credential_chain_valid(&degree_id));

    // Issuing a certificate on top of the (transitively invalid) degree
    // must be rejected.
    client.create_derived_credential(
        &attester,
        &degree_id,
        &bytes!(&env, 0x05),
        &bytes!(&env, 0x06),
    );
}

/// A credential cannot be derived from itself: passing the parent's own
/// (proof, claim) bytes back in as the derived credential's bytes dedups to
/// the same credential_id as the requested parent.
#[test]
#[should_panic(expected = "Error(Contract, #20)")]
fn test_create_derived_credential_self_referential_parent_panics() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let proof = bytes!(&env, 0x01);
    let claim = bytes!(&env, 0x02);
    let parent_id = client.attest(&oracle, &proof, &claim);

    client.create_derived_credential(&oracle, &parent_id, &proof, &claim);
}

/// A credential's parent is fixed at its first creation: re-deriving the
/// same (proof, claim) pair under a different parent must panic rather than
/// silently rewriting the hierarchy.
#[test]
#[should_panic(expected = "Error(Contract, #21)")]
fn test_create_derived_credential_parent_already_set_panics() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);

    let parent_a = client.attest(&oracle, &bytes!(&env, 0x01), &bytes!(&env, 0x02));
    let parent_b = client.attest(&oracle, &bytes!(&env, 0x03), &bytes!(&env, 0x04));

    let child_proof = bytes!(&env, 0x05);
    let child_claim = bytes!(&env, 0x06);
    client.create_derived_credential(&oracle, &parent_a, &child_proof, &child_claim);

    // Same (proof, claim) pair -> same credential_id, but a different
    // requested parent.
    client.create_derived_credential(&oracle, &parent_b, &child_proof, &child_claim);
}

/// Re-deriving the exact same (proof, claim, parent) triple is idempotent —
/// it must not panic, and must keep returning the same credential_id.
#[test]
fn test_create_derived_credential_idempotent_on_same_parent() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);
    let parent_id = client.attest(&oracle, &bytes!(&env, 0x01), &bytes!(&env, 0x02));

    let child_proof = bytes!(&env, 0x03);
    let child_claim = bytes!(&env, 0x04);
    let first = client.create_derived_credential(&oracle, &parent_id, &child_proof, &child_claim);
    let second = client.create_derived_credential(&oracle, &parent_id, &child_proof, &child_claim);

    assert_eq!(first, second);
    assert_eq!(client.get_credential_parent(&first), Some(parent_id));
}

/// A parent chain exceeding MAX_CREDENTIAL_CHAIN_DEPTH hops is rejected
/// rather than allowing unbounded recursive validation.
#[test]
#[should_panic(expected = "Error(Contract, #19)")]
fn test_create_derived_credential_chain_too_deep_panics() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);

    let mut current = client.attest(
        &oracle,
        &Bytes::from_array(&env, &[0xaa, 0x00]),
        &Bytes::from_array(&env, &[0xbb, 0x00]),
    );
    // MAX_CREDENTIAL_CHAIN_DEPTH is 32; comfortably exceed it so the chain
    // walk is forced to hit the depth cap before this loop ends.
    for i in 1..=(MAX_CREDENTIAL_CHAIN_DEPTH + 10) {
        let proof = Bytes::from_array(&env, &[0xaa, i as u8]);
        let claim = Bytes::from_array(&env, &[0xbb, i as u8]);
        current = client.create_derived_credential(&oracle, &current, &proof, &claim);
    }
}

/// is_credential_chain_valid reflects invalidation anywhere in the
/// ancestry, not just the queried credential itself.
#[test]
fn test_is_credential_chain_valid_reflects_ancestor_invalidation() {
    let (env, _, client) = setup();
    let oracles = register_oracles(&env, &client, 3);
    let attester = oracles.get(0).unwrap();

    let root_id = client.attest(&attester, &bytes!(&env, 0x01), &bytes!(&env, 0x02));
    let child_id = client.create_derived_credential(
        &attester,
        &root_id,
        &bytes!(&env, 0x03),
        &bytes!(&env, 0x04),
    );
    assert!(client.is_credential_chain_valid(&child_id));

    let initiator = Address::generate(&env);
    let dispute_id = client.initiate_credential_dispute(&root_id, &initiator, &bytes!(&env, 0xaa));
    for oracle in oracles.iter() {
        client.vote_on_dispute(&dispute_id, &oracle, &true);
    }

    assert!(!client.is_credential_chain_valid(&root_id));
    assert!(!client.is_credential_chain_valid(&child_id));
}

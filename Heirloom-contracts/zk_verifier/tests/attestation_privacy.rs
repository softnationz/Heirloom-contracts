#![cfg(test)]

//! Tests for the privacy-gated attestation query path (`get_attestation`).
//!
//! `get_attestation` is the attestation query path: it returns the
//! current `AttestationRecord` for a credential only after checking the
//! caller's authorization against the credential's [`PrivacyLevel`] —
//! anyone for `Public`, the admin or a registered oracle for `Internal`,
//! and only the admin for `Confidential`. Unauthorized callers at
//! `Confidential` (the most restrictive level) receive a *redacted*
//! record whose `oracle` field is masked out, rather than the full one.
//!
//! Lives as a `tests/` integration target for the same reason as
//! `lattice_and_masking.rs`: `src/test.rs` has pre-existing compile
//! errors unrelated to this feature (it references a dispute/temporal
//! query API that has not yet been restored to `src/lib.rs`).

use soroban_sdk::{bytes, testutils::Address as _, Address, Env, String};
use zk_verifier::{PrivacyLevel, ZkVerifierContract, ZkVerifierContractClient};

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

/// Attests a (proof, claim) pair with a fresh registered oracle and
/// returns `(credential_id, oracle)`.
fn attest_credential(env: &Env, client: &ZkVerifierContractClient<'static>) -> (u64, Address) {
    let oracle = Address::generate(env);
    client.register_oracle(&oracle);
    let proof = bytes!(env, 0xdeadbeef);
    let claim = bytes!(env, 0xcafebabe);
    let credential_id = client.attest(&oracle, &proof, &claim);
    (credential_id, oracle)
}

/// The well-defined "masked" oracle address used in redacted records:
/// the Stellar strkey for the all-zero Ed25519 account.
fn masked_oracle(env: &Env) -> Address {
    Address::from_string(&String::from_str(
        env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    ))
}

// ---- Public access path ----

/// A `Public` credential (the default) is readable by anyone: a stranger
/// receives the full record, including the attesting oracle.
#[test]
fn test_get_attestation_public_path_returns_full_record_to_anyone() {
    let (env, _admin, client) = setup();
    let (credential_id, oracle) = attest_credential(&env, &client);

    // No set_credential_privacy call: PrivacyLevel defaults to Public.
    let stranger = Address::generate(&env);
    let record = client.get_attestation(&stranger, &credential_id).unwrap();
    assert_eq!(record.credential_id, credential_id);
    assert_eq!(record.oracle, oracle);
}

/// Querying a credential id that was never attested returns `None` at any
/// privacy level — no record exists to disclose.
#[test]
fn test_get_attestation_unknown_credential_returns_none() {
    let (env, _admin, client) = setup();
    let stranger = Address::generate(&env);
    assert!(client.get_attestation(&stranger, &999u64).is_none());
}

// ---- Internal (Restricted) access path ----

/// An `Internal` credential is readable in full by the admin and by any
/// currently-registered oracle — not just the oracle that attested it.
#[test]
fn test_get_attestation_internal_path_allows_admin_and_any_registered_oracle() {
    let (env, admin, client) = setup();
    let (credential_id, attester) = attest_credential(&env, &client);
    let other_oracle = Address::generate(&env);
    client.register_oracle(&other_oracle);
    client.set_credential_privacy(&credential_id, &PrivacyLevel::Internal);

    let as_admin = client.get_attestation(&admin, &credential_id).unwrap();
    assert_eq!(as_admin.oracle, attester);

    let as_other_oracle = client
        .get_attestation(&other_oracle, &credential_id)
        .unwrap();
    assert_eq!(as_other_oracle.oracle, attester);
}

/// An `Internal` credential is not readable by an address that is neither
/// the admin nor a registered oracle — the call panics with AccessDenied
/// rather than revealing the record.
#[test]
#[should_panic(expected = "Error(Contract, #21)")]
fn test_get_attestation_internal_path_denies_stranger() {
    let (env, _admin, client) = setup();
    let (credential_id, _oracle) = attest_credential(&env, &client);
    client.set_credential_privacy(&credential_id, &PrivacyLevel::Internal);

    let stranger = Address::generate(&env);
    client.get_attestation(&stranger, &credential_id);
}

// ---- Confidential (Private) access path ----

/// A `Confidential` credential is readable in full by the admin.
#[test]
fn test_get_attestation_confidential_path_allows_admin() {
    let (env, admin, client) = setup();
    let (credential_id, attester) = attest_credential(&env, &client);
    client.set_credential_privacy(&credential_id, &PrivacyLevel::Confidential);

    let record = client.get_attestation(&admin, &credential_id).unwrap();
    assert_eq!(record.credential_id, credential_id);
    assert_eq!(record.oracle, attester);
}

/// An unauthorized caller at `Confidential` receives a redacted record:
/// the credential still identifies itself, but the `oracle` field is
/// masked out rather than disclosing the real attesting oracle.
#[test]
fn test_get_attestation_confidential_path_redacts_for_stranger() {
    let (env, _admin, client) = setup();
    let (credential_id, attester) = attest_credential(&env, &client);
    client.set_credential_privacy(&credential_id, &PrivacyLevel::Confidential);

    let stranger = Address::generate(&env);
    let record = client.get_attestation(&stranger, &credential_id).unwrap();

    assert_eq!(record.credential_id, credential_id);
    assert_eq!(record.oracle, masked_oracle(&env));
    assert_ne!(record.oracle, attester);
}

/// `Confidential` is stricter than `Internal`: even a registered oracle —
/// who could read the full record at `Internal` — is redacted here.
#[test]
fn test_get_attestation_confidential_path_redacts_for_registered_oracle() {
    let (env, _admin, client) = setup();
    let (credential_id, attester) = attest_credential(&env, &client);
    let other_oracle = Address::generate(&env);
    client.register_oracle(&other_oracle);
    client.set_credential_privacy(&credential_id, &PrivacyLevel::Confidential);

    let record = client
        .get_attestation(&other_oracle, &credential_id)
        .unwrap();
    assert_eq!(record.oracle, masked_oracle(&env));
    assert_ne!(record.oracle, attester);
}

#![cfg(test)]

//! Regression tests for the `verify_lattice_proof` / `is_valid_lattice_proof`
//! and `mask_proof_fields` fixes (issue #263).
//!
//! Lives as a `tests/` integration target — rather than alongside the rest
//! of the suite in `src/test.rs` — because `src/test.rs` has a large number
//! of pre-existing compile errors unrelated to this fix: it references a
//! credential-dispute / credential-hierarchy-query public API
//! (`initiate_credential_dispute`, `vote_on_dispute`, `get_credential_parent`,
//! `is_credential_chain_valid`, and friends) that does not exist in
//! `src/lib.rs`. That API predates this issue and is out of scope for it, so
//! rather than block these new tests on restoring it, they're kept
//! independent here, exercising only the contract's public interface.

use soroban_sdk::{bytes, testutils::Address as _, vec, Address, Bytes, Env};
use zk_verifier::{ZkVerifierContract, ZkVerifierContractClient, LATTICE_PROOF_HEADER};

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

/// Builds a `proof` satisfying `is_valid_lattice_proof`'s format: the
/// `LATTICE_V1` header, an arbitrary payload, then a 4-byte checksum (the
/// first 4 bytes of `sha256(header || payload)`).
fn well_formed_lattice_proof(env: &Env, payload: &[u8]) -> Bytes {
    let mut body = Bytes::from_slice(env, LATTICE_PROOF_HEADER);
    for &b in payload {
        body.push_back(b);
    }
    let digest: Bytes = env.crypto().sha256(&body).into();

    let mut proof = body;
    for i in 0..4u32 {
        proof.push_back(digest.get(i).unwrap());
    }
    proof
}

// ---- verify_lattice_proof / is_valid_lattice_proof ----

/// A proof with a valid `LATTICE_V1` header followed by garbage bytes (no
/// matching checksum) must now be rejected. Before the fix,
/// `is_valid_lattice_proof` only checked the header prefix, so this exact
/// input would have passed the format check.
#[test]
#[should_panic(expected = "Error(Contract, #17)")]
fn test_lattice_proof_garbage_after_valid_header_is_rejected() {
    let (env, client) = setup();

    let mut proof = Bytes::from_slice(&env, LATTICE_PROOF_HEADER);
    proof.extend_from_array(&[0xAAu8; 16]); // garbage payload + garbage "checksum"
    let claim = bytes!(&env, 0xcafebabe);

    client.verify_lattice_proof(&proof, &claim);
}

/// A proof that is *just* the header, with no room for a checksum, must
/// also be rejected (too short to be well-formed, not merely "unattested").
#[test]
#[should_panic(expected = "Error(Contract, #17)")]
fn test_lattice_proof_header_only_is_rejected() {
    let (env, client) = setup();
    let proof = Bytes::from_slice(&env, LATTICE_PROOF_HEADER);
    let claim = bytes!(&env, 0xcafebabe);
    client.verify_lattice_proof(&proof, &claim);
}

/// A well-formed lattice proof (valid header + matching checksum) that was
/// never attested by any oracle must still be rejected — proving rejection
/// isn't happening only at the header-check layer, but that the real trust
/// decision (oracle attestation, same model as `verify_claim`) is enforced
/// end-to-end.
#[test]
fn test_wellformed_lattice_proof_without_attestation_returns_false() {
    let (env, client) = setup();
    let proof = well_formed_lattice_proof(&env, &[0x01, 0x02, 0x03]);
    let claim = bytes!(&env, 0xcafebabe);

    assert!(!client.verify_lattice_proof(&proof, &claim));
}

/// A well-formed, oracle-attested lattice proof verifies successfully —
/// confirming the fix didn't break the legitimate path.
#[test]
fn test_wellformed_attested_lattice_proof_verifies() {
    let (env, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);

    let proof = well_formed_lattice_proof(&env, &[0xAB, 0xCD]);
    let claim = bytes!(&env, 0xcafebabe);
    client.attest(&oracle, &proof, &claim);

    assert!(client.verify_lattice_proof(&proof, &claim));
}

// ---- mask_proof_fields ----

/// `mask_proof_fields` must not leak the original bytes at masked field
/// offsets — a byte-level check, not just a length comparison (a
/// length-only check would have passed even when the old code appended the
/// full, unredacted proof after the header/bitmask).
#[test]
fn test_mask_proof_fields_redacts_masked_offsets() {
    let (env, client) = setup();
    let proof = bytes!(&env, 0x1122334455667788);
    let fields_to_mask = vec![&env, 2u32, 5u32];

    let masked = client.mask_proof_fields(&proof, &fields_to_mask);

    // Layout: [b"MASKED_V1" (9 bytes)][field_mask u32 LE (4 bytes)][proof.len() bytes]
    let payload_start: u32 = 13;
    assert_eq!(masked.len(), payload_start + proof.len());

    for i in 0..proof.len() {
        let original_byte = proof.get(i).unwrap();
        let masked_byte = masked.get(payload_start + i).unwrap();
        if i == 2 || i == 5 {
            assert_eq!(
                masked_byte, 0,
                "masked field {i} must not carry the original byte"
            );
            assert_ne!(
                masked_byte, original_byte,
                "masked field {i} must differ from the original (original byte was non-zero)"
            );
        } else {
            assert_eq!(
                masked_byte, original_byte,
                "unmasked field {i} must be preserved"
            );
        }
    }
}

/// An out-of-range field offset (>= proof length, or >= 32) must panic
/// rather than silently wrap the bitmask shift.
#[test]
#[should_panic(expected = "Error(Contract, #19)")]
fn test_mask_proof_fields_rejects_out_of_range_offset() {
    let (env, client) = setup();
    let proof = bytes!(&env, 0xdeadbeef);
    let fields_to_mask = vec![&env, 40u32];
    client.mask_proof_fields(&proof, &fields_to_mask);
}

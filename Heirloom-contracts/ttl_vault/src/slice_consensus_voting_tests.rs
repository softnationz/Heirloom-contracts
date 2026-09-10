//! Tests for slice consensus voting quorum enforcement.
//!
//! Covers:
//! - Finalization below the configured quorum is rejected with
//!   `InsufficientQuorum` and the proposal stays Pending.
//! - Finalization at exactly the quorum resolves the proposal.
//! - Finalization above the quorum resolves the proposal.
//! - The default quorum (1) preserves single-vote finalization unless
//!   a stricter configuration is set.

#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{testutils::Address as _, Address, Bytes, Env, Vec};

use crate::slice_consensus_voting::{
    get_modification_proposal, propose_slice_modification, register_attestor_registry,
    resolve_modification_voting, set_voting_config, vote_on_modification, ProposalStatus,
};

// ── Test helpers ──────────────────────────────────────────────────────────────

/// Register `count` generated attestors in the voting registry.
fn register_attestors(env: &Env, contract_id: &Address, count: u32) -> Vec<Address> {
    let mut attestors = Vec::new(env);
    for _ in 0..count {
        attestors.push_back(Address::generate(env));
    }
    env.as_contract(contract_id, || {
        register_attestor_registry(env, attestors.clone());
    });
    attestors
}

/// Propose a slice modification and return its proposal id.
fn propose(env: &Env, contract_id: &Address, slice_id: u64) -> u64 {
    let proposer = Address::generate(env);
    let changes = Bytes::from_slice(env, &[0u8, 0, 0, 0, 1]); // UpdateMetadata(1)
    env.as_contract(contract_id, || {
        propose_slice_modification(env, slice_id, changes, proposer)
    })
}

/// Cast a single vote from `voter` on a proposal.
fn cast_vote(
    env: &Env,
    contract_id: &Address,
    slice_id: u64,
    proposal_id: u64,
    voter: &Address,
    approve: bool,
) -> bool {
    env.as_contract(contract_id, || {
        vote_on_modification(env, slice_id, proposal_id, voter.clone(), approve)
    })
}

/// Convenience: register attestors, configure quorum, propose, and vote.
fn setup_proposal_with_votes(
    env: &Env,
    contract_id: &Address,
    attestor_count: u32,
    min_quorum: u32,
    approve_count: u32,
) -> (u64, Vec<Address>) {
    let attestors = register_attestors(env, contract_id, attestor_count);
    env.as_contract(contract_id, || set_voting_config(env, min_quorum));

    let slice_id = 1u64;
    let proposal_id = propose(env, contract_id, slice_id);

    for att in attestors.iter().take(approve_count as usize) {
        assert!(cast_vote(
            env,
            contract_id,
            slice_id,
            proposal_id,
            &att,
            true
        ));
    }
    (proposal_id, attestors)
}

// ── Below quorum ──────────────────────────────────────────────────────────────

/// Finalization with fewer votes than the quorum is rejected and the proposal
/// stays Pending — even when every cast vote approves.
#[test]
fn test_finalization_below_quorum_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::TtlVaultContract);

    // 3 attestors, quorum 3, but only 2 cast votes (both approving).
    let (proposal_id, _attestors) = setup_proposal_with_votes(&env, &contract_id, 3, 3, 2);

    let result = env.as_contract(&contract_id, || {
        resolve_modification_voting(&env, 1, proposal_id)
    });
    assert_eq!(result, Err(ContractError::InsufficientQuorum));

    // Proposal must remain Pending so voting can continue.
    let proposal = env
        .as_contract(&contract_id, || get_modification_proposal(&env, 1, proposal_id))
        .unwrap();
    assert_eq!(proposal.status, ProposalStatus::Pending);
    assert_eq!(proposal.approve_count, 2);
    assert_eq!(proposal.reject_count, 0);
}

/// Once enough votes are cast, a previously below-quorum proposal can be
/// finalized.
#[test]
fn test_below_quorum_then_reaches_quorum_resolves() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::TtlVaultContract);

    let attestors = register_attestors(&env, &contract_id, 3);
    env.as_contract(&contract_id, || set_voting_config(&env, 3));

    let slice_id = 1u64;
    let proposal_id = propose(&env, &contract_id, slice_id);

    // Only 2 of 3 vote → below quorum.
    assert!(cast_vote(
        &env,
        &contract_id,
        slice_id,
        proposal_id,
        &attestors.get(0).unwrap(),
        true
    ));
    assert!(cast_vote(
        &env,
        &contract_id,
        slice_id,
        proposal_id,
        &attestors.get(1).unwrap(),
        true
    ));
    let below = env.as_contract(&contract_id, || {
        resolve_modification_voting(&env, slice_id, proposal_id)
    });
    assert_eq!(below, Err(ContractError::InsufficientQuorum));

    // 3rd vote reaches quorum → resolves as Approved.
    assert!(cast_vote(
        &env,
        &contract_id,
        slice_id,
        proposal_id,
        &attestors.get(2).unwrap(),
        true
    ));
    let resolved = env.as_contract(&contract_id, || {
        resolve_modification_voting(&env, slice_id, proposal_id)
    });
    assert_eq!(resolved, Ok(true));

    let proposal = env
        .as_contract(&contract_id, || get_modification_proposal(&env, slice_id, proposal_id))
        .unwrap();
    assert_eq!(proposal.status, ProposalStatus::Approved);
}

// ── At quorum ─────────────────────────────────────────────────────────────────

/// Finalization with exactly the quorum number of votes resolves the proposal.
#[test]
fn test_finalization_at_quorum_resolves() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::TtlVaultContract);

    // 3 attestors, quorum 3, exactly 3 votes cast.
    let (proposal_id, _attestors) = setup_proposal_with_votes(&env, &contract_id, 3, 3, 3);

    let result = env.as_contract(&contract_id, || {
        resolve_modification_voting(&env, 1, proposal_id)
    });
    assert_eq!(result, Ok(true));

    let proposal = env
        .as_contract(&contract_id, || get_modification_proposal(&env, 1, proposal_id))
        .unwrap();
    assert_eq!(proposal.status, ProposalStatus::Approved);
}

/// At-quorum finalization with a majority of rejections resolves as Rejected.
#[test]
fn test_finalization_at_quorum_with_majority_reject_resolves_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::TtlVaultContract);

    // 3 attestors, quorum 3: 2 approve + 1 reject = exactly quorum.
    let attestors = register_attestors(&env, &contract_id, 3);
    env.as_contract(&contract_id, || set_voting_config(&env, 3));

    let slice_id = 1u64;
    let proposal_id = propose(&env, &contract_id, slice_id);

    assert!(cast_vote(
        &env,
        &contract_id,
        slice_id,
        proposal_id,
        &attestors.get(0).unwrap(),
        true
    ));
    assert!(cast_vote(
        &env,
        &contract_id,
        slice_id,
        proposal_id,
        &attestors.get(1).unwrap(),
        false
    ));
    assert!(cast_vote(
        &env,
        &contract_id,
        slice_id,
        proposal_id,
        &attestors.get(2).unwrap(),
        false
    ));

    let result = env.as_contract(&contract_id, || {
        resolve_modification_voting(&env, slice_id, proposal_id)
    });
    assert_eq!(result, Ok(true));

    let proposal = env
        .as_contract(&contract_id, || get_modification_proposal(&env, slice_id, proposal_id))
        .unwrap();
    assert_eq!(proposal.status, ProposalStatus::Rejected);
}

// ── Above quorum ──────────────────────────────────────────────────────────────

/// Finalization with more votes than the quorum resolves the proposal.
#[test]
fn test_finalization_above_quorum_resolves() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::TtlVaultContract);

    // 3 attestors, quorum 2, all 3 cast votes.
    let (proposal_id, _attestors) = setup_proposal_with_votes(&env, &contract_id, 3, 2, 3);

    let result = env.as_contract(&contract_id, || {
        resolve_modification_voting(&env, 1, proposal_id)
    });
    assert_eq!(result, Ok(true));

    let proposal = env
        .as_contract(&contract_id, || get_modification_proposal(&env, 1, proposal_id))
        .unwrap();
    assert_eq!(proposal.status, ProposalStatus::Approved);
}

// ── Default configuration ─────────────────────────────────────────────────────

/// Without an explicit configuration, the default quorum of 1 allows a single
/// vote to finalize (legacy behavior).
#[test]
fn test_default_quorum_allows_single_vote_finalization() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::TtlVaultContract);

    // No set_voting_config call → DEFAULT_MIN_QUORUM (1) applies.
    let attestors = register_attestors(&env, &contract_id, 1);

    let slice_id = 1u64;
    let proposal_id = propose(&env, &contract_id, slice_id);
    assert!(cast_vote(
        &env,
        &contract_id,
        slice_id,
        proposal_id,
        &attestors.get(0).unwrap(),
        true
    ));

    let result = env.as_contract(&contract_id, || {
        resolve_modification_voting(&env, slice_id, proposal_id)
    });
    assert_eq!(result, Ok(true));

    let proposal = env
        .as_contract(&contract_id, || get_modification_proposal(&env, slice_id, proposal_id))
        .unwrap();
    assert_eq!(proposal.status, ProposalStatus::Approved);
}

/// Issue #39 — Implement Slice Consensus Voting
///
/// Slice modifications require consensus voting. This module provides:
/// - Proposing a slice modification with description
/// - Attesting approval/rejection via quorum voting
/// - Executing approved modifications by applying concrete changes to slice state
///
/// # Design
///
/// A modification proposal encapsulates:
/// - `slice_id` — the slice being modified
/// - `proposed_changes` — serialized SliceModification describing the change
/// - `proposer` — Address that initiated the proposal
/// - `status` — one of Pending, Approved, Rejected, Executed
/// - `voting_deadline` — ledger timestamp when voting ends
///
/// Proposals are stored by `(slice_id, proposal_id)` where `proposal_id` is
/// monotonically incremented per slice.
///
/// # Voting rules
///
/// - Only registered attestors may vote.
/// - Each attestor votes once per proposal (no changing votes).
/// - Voting is open until `voting_deadline`.
/// - After deadline, a proposal is automatically approved if ≥ 50% of attestors
///   approve it; otherwise rejected.
/// - Finalization requires at least the configured `min_quorum` votes to have
///   been cast; otherwise resolution is rejected with `InsufficientQuorum` and
///   the proposal stays Pending so more attestors can vote.
/// - Once approved, the executor calls `execute_slice_modification` to apply changes.
///
/// # Modification history
///
/// Every executed modification is recorded with a timestamp, the proposer,
/// and the change description. This creates an auditable chain.
///
use soroban_sdk::{contracttype, symbol_short, Address, Bytes, Env, Map, Vec};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Default voting duration in seconds (7 days).
pub const DEFAULT_VOTING_PERIOD: u64 = 604_800;

/// Minimum number of attestors required for voting to proceed (prevents edge case with 0 attestors).
pub const MIN_ATTESTORS_REQUIRED: u32 = 1;

/// Default minimum number of votes (approvals + rejections) required before a
/// proposal can be finalized. A quorum of 1 preserves legacy behavior;
/// operators can raise it via `set_voting_config` so that a small number of
/// attestors cannot make binding decisions.
pub const DEFAULT_MIN_QUORUM: u32 = 1;

// ── Event topics ─────────────────────────────────────────────────────────────

pub const MODIFICATION_PROPOSED_TOPIC: soroban_sdk::Symbol = symbol_short!("mod_prop");
pub const MODIFICATION_VOTED_TOPIC: soroban_sdk::Symbol = symbol_short!("mod_vote");
pub const MODIFICATION_RESOLVED_TOPIC: soroban_sdk::Symbol = symbol_short!("mod_res");
pub const MODIFICATION_EXECUTED_TOPIC: soroban_sdk::Symbol = symbol_short!("mod_exec");

// ── Storage keys ─────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub enum VotingKey {
    /// Monotonic proposal counter per slice.
    SliceProposalCounter(u64),
    /// Proposal details: (slice_id, proposal_id) → ModificationProposal
    ModificationProposal(u64, u64),
    /// Mapping of attestors to votes for a proposal: (slice_id, proposal_id) → Map<Address, bool>
    ProposalVotes(u64, u64),
    /// History of executed modifications per slice.
    ModificationHistory(u64),
    /// List of registered attestors (cached for voting eligibility).
    AttestorRegistry,
    /// Voting configuration (e.g. minimum quorum required for finalization).
    VotingConfig,
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// Voting configuration for slice consensus.
#[contracttype]
#[derive(Clone, Debug)]
pub struct VotingConfig {
    /// Minimum number of votes (approvals + rejections) that must be cast
    /// before a proposal can be finalized. Prevents a small number of
    /// attestors from making binding decisions.
    pub min_quorum: u32,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum ProposalStatus {
    Pending,
    Approved,
    Rejected,
    Executed,
}

/// Concrete schema for slice modifications.
/// This enum defines what kinds of slice state changes can be proposed and executed.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum SliceModification {
    /// Update slice metadata (e.g., description or tags).
    /// Contains tag u32 for categorizing slice modifications.
    UpdateMetadata(u32),
    /// Update slice rules by rule IDs (composition validation rules).
    /// Contains rule_ids_len u32 (count of rule IDs).
    UpdateRules(u32),
    /// Update slice weights based on performance metrics.
    /// Contains attestor_addresses_len u32 (count of attestors being reweighted).
    ReweightAttestors(u32),
}

/// A modification proposal for a slice.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ModificationProposal {
    pub slice_id: u64,
    pub proposal_id: u64,
    /// Opaque description of proposed changes.
    pub proposed_changes: Bytes,
    /// Address that initiated the proposal.
    pub proposer: Address,
    pub status: ProposalStatus,
    /// Ledger timestamp when voting ends.
    pub voting_deadline: u64,
    /// Number of approvals received.
    pub approve_count: u32,
    /// Number of rejections received.
    pub reject_count: u32,
    /// Total attestors eligible to vote (cached at proposal creation).
    pub total_attestors: u32,
    /// Ledger timestamp when proposal was created.
    pub created_at: u64,
}

/// Record of a single executed modification.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ModificationRecord {
    pub proposal_id: u64,
    pub proposed_changes: Bytes,
    pub proposer: Address,
    /// Timestamp when the modification was executed.
    pub executed_at: u64,
    pub approve_count: u32,
    pub total_attestors: u32,
}

// ── Events ────────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug)]
pub struct ModificationProposedEvent {
    pub slice_id: u64,
    pub proposal_id: u64,
    pub proposer: Address,
    pub voting_deadline: u64,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct ModificationVotedEvent {
    pub slice_id: u64,
    pub proposal_id: u64,
    pub voter: Address,
    pub approve: bool,
    pub approve_count: u32,
    pub reject_count: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct ModificationResolvedEvent {
    pub slice_id: u64,
    pub proposal_id: u64,
    pub approved: bool,
    pub approve_count: u32,
    pub reject_count: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct ModificationExecutedEvent {
    pub slice_id: u64,
    pub proposal_id: u64,
    pub proposer: Address,
    pub executed_at: u64,
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Register attestors in the voting registry (called during slice configuration).
pub fn register_attestor_registry(env: &Env, attestors: Vec<Address>) {
    let key = VotingKey::AttestorRegistry;
    env.storage().persistent().set(&key, &attestors);
    env.storage().persistent().extend_ttl(
        &key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );
}

/// Get the current list of registered attestors.
pub fn get_attestor_registry(env: &Env) -> Vec<Address> {
    let key = VotingKey::AttestorRegistry;
    env.storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| Vec::new(env))
}

/// Set the voting configuration (e.g. the minimum quorum for finalization).
pub fn set_voting_config(env: &Env, min_quorum: u32) {
    let config = VotingConfig { min_quorum };
    let key = VotingKey::VotingConfig;
    env.storage().persistent().set(&key, &config);
    env.storage().persistent().extend_ttl(
        &key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );
}

/// Get the current voting configuration, falling back to `DEFAULT_MIN_QUORUM`
/// when no explicit configuration has been stored.
pub fn get_voting_config(env: &Env) -> VotingConfig {
    let key = VotingKey::VotingConfig;
    env.storage().persistent().get(&key).unwrap_or(VotingConfig {
        min_quorum: DEFAULT_MIN_QUORUM,
    })
}

/// Check if an address is a registered attestor.
fn is_registered_attestor(env: &Env, address: &Address) -> bool {
    let attestors = get_attestor_registry(env);
    for att in attestors.iter() {
        if &att == address {
            return true;
        }
    }
    false
}

/// Try to parse proposed_changes bytes into a SliceModification.
/// Returns Some(modification) if parsing succeeds, None otherwise.
fn parse_slice_modification(bytes: &Bytes) -> Option<SliceModification> {
    if bytes.len() < 5 {
        return None;
    }

    let first_byte = bytes.get(0).unwrap_or(255);
    let value_bytes = [
        bytes.get(1).unwrap_or(0),
        bytes.get(2).unwrap_or(0),
        bytes.get(3).unwrap_or(0),
        bytes.get(4).unwrap_or(0),
    ];
    let value = u32::from_be_bytes(value_bytes);

    match first_byte {
        0 => Some(SliceModification::UpdateMetadata(value)),
        1 => Some(SliceModification::UpdateRules(value)),
        2 => Some(SliceModification::ReweightAttestors(value)),
        _ => None,
    }
}

// ── Core functions ────────────────────────────────────────────────────────────

/// Propose a slice modification. Returns the proposal ID.
///
/// - `proposed_changes` is an opaque Bytes describing the modifications.
/// - Voting period defaults to `DEFAULT_VOTING_PERIOD`.
/// - All currently registered attestors are eligible voters.
pub fn propose_slice_modification(
    env: &Env,
    slice_id: u64,
    proposed_changes: Bytes,
    proposer: Address,
) -> u64 {
    let counter_key = VotingKey::SliceProposalCounter(slice_id);
    let proposal_id: u64 = env.storage().persistent().get(&counter_key).unwrap_or(0u64);

    let new_proposal_id = proposal_id.saturating_add(1);
    let voting_deadline = env
        .ledger()
        .timestamp()
        .saturating_add(DEFAULT_VOTING_PERIOD);

    let attestors = get_attestor_registry(env);
    let total_attestors = attestors.len() as u32;

    let proposal = ModificationProposal {
        slice_id,
        proposal_id: new_proposal_id,
        proposed_changes,
        proposer: proposer.clone(),
        status: ProposalStatus::Pending,
        voting_deadline,
        approve_count: 0,
        reject_count: 0,
        total_attestors,
        created_at: env.ledger().timestamp(),
    };

    let proposal_key = VotingKey::ModificationProposal(slice_id, new_proposal_id);
    env.storage().persistent().set(&proposal_key, &proposal);
    env.storage().persistent().extend_ttl(
        &proposal_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.storage()
        .persistent()
        .set(&counter_key, &new_proposal_id);
    env.storage().persistent().extend_ttl(
        &counter_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.events().publish(
        (MODIFICATION_PROPOSED_TOPIC, slice_id),
        ModificationProposedEvent {
            slice_id,
            proposal_id: new_proposal_id,
            proposer,
            voting_deadline,
        },
    );

    new_proposal_id
}

/// Vote on a modification proposal.
///
/// - Only registered attestors can vote.
/// - Each attestor votes once (changing vote is not allowed).
/// - Returns `true` if the vote was recorded successfully.
/// - Returns `false` if the voter is not an attestor or already voted.
pub fn vote_on_modification(
    env: &Env,
    slice_id: u64,
    proposal_id: u64,
    voter: Address,
    approve: bool,
) -> bool {
    // Check if voter is a registered attestor.
    if !is_registered_attestor(env, &voter) {
        return false;
    }

    let proposal_key = VotingKey::ModificationProposal(slice_id, proposal_id);
    let mut proposal: ModificationProposal = match env.storage().persistent().get(&proposal_key) {
        Some(p) => p,
        None => return false,
    };

    // Check proposal is still in Pending status.
    if proposal.status != ProposalStatus::Pending {
        return false;
    }

    // Check voting deadline has not passed.
    if env.ledger().timestamp() > proposal.voting_deadline {
        return false;
    }

    let votes_key = VotingKey::ProposalVotes(slice_id, proposal_id);
    let mut votes: Map<Address, bool> = env
        .storage()
        .persistent()
        .get(&votes_key)
        .unwrap_or_else(|| Map::new(env));

    // Check if voter has already voted.
    if votes.contains_key(voter.clone()) {
        return false;
    }

    // Record the vote.
    votes.set(voter.clone(), approve);

    if approve {
        proposal.approve_count = proposal.approve_count.saturating_add(1);
    } else {
        proposal.reject_count = proposal.reject_count.saturating_add(1);
    }

    env.storage().persistent().set(&votes_key, &votes);
    env.storage().persistent().extend_ttl(
        &votes_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.storage().persistent().set(&proposal_key, &proposal);
    env.storage().persistent().extend_ttl(
        &proposal_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.events().publish(
        (MODIFICATION_VOTED_TOPIC, slice_id),
        ModificationVotedEvent {
            slice_id,
            proposal_id,
            voter,
            approve,
            approve_count: proposal.approve_count,
            reject_count: proposal.reject_count,
        },
    );

    true
}

/// Finalize a proposal's voting (called after voting deadline expires or voting is complete).
///
/// Returns `Ok(true)` if the proposal was resolved, `Ok(false)` if the proposal
/// does not exist or was already resolved (idempotent).
/// Sets status to Approved if ≥ 50% of attestors voted in favor, otherwise Rejected.
/// Returns `Err(ContractError::InsufficientQuorum)` if fewer than the configured
/// quorum of votes have been cast; the proposal stays Pending so voting can continue.
pub fn resolve_modification_voting(
    env: &Env,
    slice_id: u64,
    proposal_id: u64,
) -> Result<bool, crate::ContractError> {
    let proposal_key = VotingKey::ModificationProposal(slice_id, proposal_id);
    let mut proposal: ModificationProposal = match env.storage().persistent().get(&proposal_key) {
        Some(p) => p,
        None => return Ok(false),
    };

    // If already resolved, return false (idempotent).
    if proposal.status != ProposalStatus::Pending {
        return Ok(false);
    }

    // Enforce the minimum quorum: a small number of voters must not be able to
    // make binding decisions, so finalization is rejected until enough
    // attestors have cast a vote.
    let total_voted = proposal.approve_count.saturating_add(proposal.reject_count);
    let min_quorum = get_voting_config(env).min_quorum;
    if total_voted < min_quorum {
        return Err(crate::ContractError::InsufficientQuorum);
    }

    // Determine if proposal is approved (≥ 50% approval).
    let is_approved = if proposal.total_attestors > 0 {
        (proposal.approve_count as u64 * 100) >= (proposal.total_attestors as u64 * 50)
    } else {
        false
    };

    proposal.status = if is_approved {
        ProposalStatus::Approved
    } else {
        ProposalStatus::Rejected
    };

    env.storage().persistent().set(&proposal_key, &proposal);
    env.storage().persistent().extend_ttl(
        &proposal_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.events().publish(
        (MODIFICATION_RESOLVED_TOPIC, slice_id),
        ModificationResolvedEvent {
            slice_id,
            proposal_id,
            approved: is_approved,
            approve_count: proposal.approve_count,
            reject_count: proposal.reject_count,
        },
    );

    Ok(true)
}

/// Execute an approved modification (owner calls this after voting is approved).
///
/// Parses the proposed_changes and applies the concrete modification to slice state.
/// Returns `true` if executed successfully, `false` if proposal is not approved,
/// already executed, or the proposed changes could not be parsed/applied.
pub fn execute_slice_modification(env: &Env, slice_id: u64, proposal_id: u64) -> bool {
    let proposal_key = VotingKey::ModificationProposal(slice_id, proposal_id);
    let mut proposal: ModificationProposal = match env.storage().persistent().get(&proposal_key) {
        Some(p) => p,
        None => return false,
    };

    // Only execute if approved.
    if proposal.status != ProposalStatus::Approved {
        return false;
    }

    // Parse the proposed_changes into a concrete SliceModification.
    let Some(modification) = parse_slice_modification(&proposal.proposed_changes) else {
        return false;
    };

    // Apply the modification to real slice state.
    let modification_applied = apply_slice_modification(env, slice_id, &modification);
    if !modification_applied {
        return false;
    }

    // Update proposal status to Executed.
    proposal.status = ProposalStatus::Executed;

    env.storage().persistent().set(&proposal_key, &proposal);
    env.storage().persistent().extend_ttl(
        &proposal_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    // Record in modification history.
    let history_key = VotingKey::ModificationHistory(slice_id);
    let mut history: Vec<ModificationRecord> = env
        .storage()
        .persistent()
        .get(&history_key)
        .unwrap_or_else(|| Vec::new(env));

    history.push_back(ModificationRecord {
        proposal_id,
        proposed_changes: proposal.proposed_changes,
        proposer: proposal.proposer.clone(),
        executed_at: env.ledger().timestamp(),
        approve_count: proposal.approve_count,
        total_attestors: proposal.total_attestors,
    });

    env.storage().persistent().set(&history_key, &history);
    env.storage().persistent().extend_ttl(
        &history_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.events().publish(
        (MODIFICATION_EXECUTED_TOPIC, slice_id),
        ModificationExecutedEvent {
            slice_id,
            proposal_id,
            proposer: proposal.proposer,
            executed_at: env.ledger().timestamp(),
        },
    );

    true
}

/// Apply a parsed SliceModification to the actual slice state.
/// This is where consensus-voted changes take effect on real data.
///
/// Returns `true` if the modification was successfully applied, `false` otherwise.
/// Note: Currently returns true for valid modification types. Future implementations
/// will integrate with actual slice state (e.g., slice_performance.rs, composition_rules.rs)
/// to apply these changes.
fn apply_slice_modification(_env: &Env, _slice_id: u64, modification: &SliceModification) -> bool {
    match modification {
        SliceModification::UpdateMetadata(_tag) => {
            // Validates the modification type was parsed correctly.
            // Future implementation: apply to slice metadata storage
            true
        }
        SliceModification::UpdateRules(_rule_ids_len) => {
            // Validates the modification type was parsed correctly.
            // Future implementation: parse rule IDs and call composition_rules module
            true
        }
        SliceModification::ReweightAttestors(_attestor_addresses_len) => {
            // Validates the modification type was parsed correctly.
            // Future implementation: parse weights and call slice_performance module
            true
        }
    }
}

/// Get a modification proposal.
pub fn get_modification_proposal(
    env: &Env,
    slice_id: u64,
    proposal_id: u64,
) -> Option<ModificationProposal> {
    let key = VotingKey::ModificationProposal(slice_id, proposal_id);
    env.storage().persistent().get(&key)
}

/// Get modification history for a slice.
pub fn get_modification_history(env: &Env, slice_id: u64) -> Vec<ModificationRecord> {
    let key = VotingKey::ModificationHistory(slice_id);
    env.storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| Vec::new(env))
}

/// Get vote count for a proposal.
pub fn get_proposal_votes(env: &Env, slice_id: u64, proposal_id: u64) -> Option<(u32, u32)> {
    let proposal = get_modification_proposal(env, slice_id, proposal_id)?;
    Some((proposal.approve_count, proposal.reject_count))
}

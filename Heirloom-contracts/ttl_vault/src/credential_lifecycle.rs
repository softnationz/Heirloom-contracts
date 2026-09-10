/// Issue #34 — Credential Lifecycle State Machine
///
/// Implements a state machine for credential lifecycle management.
/// Credentials transition through well-defined states with validation
/// to prevent invalid transitions.
///
/// # States
/// - Draft: Initial state, credential created but not yet attested
/// - Active: Credential is valid and can be used
/// - Suspended: Temporarily inactive, can be resumed
/// - Revoked: Permanently invalid, no further transitions allowed
/// - Expired: TTL has expired, awaiting cleanup
/// - Archived: Historical record, read-only
///
/// # Transitions
/// Valid transitions are enforced by the state machine validator.
///
use soroban_sdk::{contracttype, symbol_short, Env};

// ── Event topics ─────────────────────────────────────────────────────────────

pub const CREDENTIAL_STATE_CHANGED_TOPIC: soroban_sdk::Symbol = symbol_short!("cred_st");
pub const CREDENTIAL_STATE_INVALID_TOPIC: soroban_sdk::Symbol = symbol_short!("cred_inv");

// ── Storage keys ─────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum CredentialState {
    /// Credential created but not yet attested.
    Draft = 0,
    /// Credential is valid and active.
    Active = 1,
    /// Temporarily inactive; can transition back to Active.
    Suspended = 2,
    /// Permanently invalid; no transitions allowed.
    Revoked = 3,
    /// TTL expired; awaiting cleanup or archive.
    Expired = 4,
    /// Historical/read-only state; no further transitions.
    Archived = 5,
}

#[contracttype]
#[derive(Clone)]
pub enum CredentialLifecycleKey {
    /// credential_id -> CredentialState
    CredentialState(u64),
    /// credential_id -> u64 (ledger timestamp of state transition)
    StateTransitionTime(u64),
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// Record of a state transition with timestamp.
#[contracttype]
#[derive(Clone, Debug)]
pub struct StateTransition {
    pub credential_id: u64,
    pub from_state: CredentialState,
    pub to_state: CredentialState,
    pub timestamp: u64,
}

/// Event emitted when a credential changes state.
#[contracttype]
#[derive(Clone, Debug)]
pub struct CredentialStateChangedEvent {
    pub credential_id: u64,
    pub from_state: CredentialState,
    pub to_state: CredentialState,
    pub timestamp: u64,
}

/// Event emitted when an invalid state transition is attempted.
#[contracttype]
#[derive(Clone, Debug)]
pub struct InvalidStateTransitionEvent {
    pub credential_id: u64,
    pub current_state: CredentialState,
    pub attempted_state: CredentialState,
    pub timestamp: u64,
}

// ── Transition validation ────────────────────────────────────────────────────

/// Check if a transition from `from_state` to `to_state` is valid.
#[allow(clippy::match_same_arms)]
fn is_valid_transition(from: CredentialState, to: CredentialState) -> bool {
    match (from, to) {
        // Draft can transition to Active, Suspended, or Revoked
        (
            CredentialState::Draft,
            CredentialState::Active | CredentialState::Suspended | CredentialState::Revoked,
        ) => true,
        // Active can transition to Suspended, Revoked, Expired, or Archived
        (
            CredentialState::Active,
            CredentialState::Suspended
            | CredentialState::Revoked
            | CredentialState::Expired
            | CredentialState::Archived,
        ) => true,
        // Suspended can transition back to Active, or to Revoked/Expired/Archived
        (
            CredentialState::Suspended,
            CredentialState::Active
            | CredentialState::Revoked
            | CredentialState::Expired
            | CredentialState::Archived,
        ) => true,
        // Expired can transition to Archived or Revoked
        (CredentialState::Expired, CredentialState::Archived | CredentialState::Revoked) => true,
        // Revoked and Archived are terminal states — no outbound transitions
        (CredentialState::Revoked | CredentialState::Archived, _) => false,
        // No-op transitions (same state)
        (a, b) if a == b => false,
        // All other transitions are invalid
        _ => false,
    }
}

// ── Core functions ────────────────────────────────────────────────────────────

/// Initialize a credential with Draft state.
///
/// No-op if the credential already has a state — re-initializing would reset a
/// Revoked/Archived (terminal) credential back to Draft and "revive" it.
pub fn init_credential_state(env: &Env, credential_id: u64) {
    let key = CredentialLifecycleKey::CredentialState(credential_id);
    // Never overwrite an existing state: this is what keeps terminal states
    // (Revoked/Archived) permanent, even via the init entry point.
    if env
        .storage()
        .persistent()
        .get::<CredentialLifecycleKey, CredentialState>(&key)
        .is_some()
    {
        return;
    }
    env.storage()
        .persistent()
        .set(&key, &CredentialState::Draft);
    env.storage().persistent().extend_ttl(
        &key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    let time_key = CredentialLifecycleKey::StateTransitionTime(credential_id);
    env.storage()
        .persistent()
        .set(&time_key, &env.ledger().timestamp());
    env.storage().persistent().extend_ttl(
        &time_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );
}

/// Get the current state of a credential. Defaults to Draft if not set.
pub fn get_credential_state(env: &Env, credential_id: u64) -> CredentialState {
    let key = CredentialLifecycleKey::CredentialState(credential_id);
    env.storage()
        .persistent()
        .get(&key)
        .unwrap_or(CredentialState::Draft)
}

/// Attempt to transition a credential to a new state.
/// Returns `true` if transition succeeded, `false` if invalid.
pub fn transition_credential_state(
    env: &Env,
    credential_id: u64,
    new_state: CredentialState,
) -> bool {
    let current = get_credential_state(env, credential_id);

    // Terminal states (Revoked/Archived) are permanent — hard-block every
    // outbound transition here so the invariant holds even if the transition
    // table changes. This prevents a credential from being "revived" after
    // revocation or archival.
    let valid = !is_terminal_state(current) && is_valid_transition(current, new_state);

    // Check if transition is valid
    if !valid {
        env.events().publish(
            (CREDENTIAL_STATE_INVALID_TOPIC, credential_id),
            InvalidStateTransitionEvent {
                credential_id,
                current_state: current,
                attempted_state: new_state,
                timestamp: env.ledger().timestamp(),
            },
        );
        return false;
    }

    // Apply the transition
    let key = CredentialLifecycleKey::CredentialState(credential_id);
    env.storage().persistent().set(&key, &new_state);
    env.storage().persistent().extend_ttl(
        &key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    let timestamp = env.ledger().timestamp();
    let time_key = CredentialLifecycleKey::StateTransitionTime(credential_id);
    env.storage().persistent().set(&time_key, &timestamp);
    env.storage().persistent().extend_ttl(
        &time_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.events().publish(
        (CREDENTIAL_STATE_CHANGED_TOPIC, credential_id),
        CredentialStateChangedEvent {
            credential_id,
            from_state: current,
            to_state: new_state,
            timestamp,
        },
    );

    true
}

/// Get the timestamp of the last state transition for a credential.
pub fn get_state_transition_time(env: &Env, credential_id: u64) -> u64 {
    let key = CredentialLifecycleKey::StateTransitionTime(credential_id);
    env.storage().persistent().get(&key).unwrap_or(0)
}

/// Check if a credential is in a terminal state (Revoked or Archived).
pub fn is_terminal_state(state: CredentialState) -> bool {
    matches!(state, CredentialState::Revoked | CredentialState::Archived)
}

/// Check if a credential is in an active state (can be used).
pub fn is_active_state(state: CredentialState) -> bool {
    matches!(state, CredentialState::Active)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_transitions() {
        // Draft transitions
        assert!(is_valid_transition(
            CredentialState::Draft,
            CredentialState::Active
        ));
        assert!(is_valid_transition(
            CredentialState::Draft,
            CredentialState::Suspended
        ));
        assert!(is_valid_transition(
            CredentialState::Draft,
            CredentialState::Revoked
        ));

        // Active transitions
        assert!(is_valid_transition(
            CredentialState::Active,
            CredentialState::Suspended
        ));
        assert!(is_valid_transition(
            CredentialState::Active,
            CredentialState::Revoked
        ));
        assert!(is_valid_transition(
            CredentialState::Active,
            CredentialState::Expired
        ));
        assert!(is_valid_transition(
            CredentialState::Active,
            CredentialState::Archived
        ));

        // Suspended transitions
        assert!(is_valid_transition(
            CredentialState::Suspended,
            CredentialState::Active
        ));
        assert!(is_valid_transition(
            CredentialState::Suspended,
            CredentialState::Revoked
        ));

        // Expired transitions
        assert!(is_valid_transition(
            CredentialState::Expired,
            CredentialState::Archived
        ));
        assert!(is_valid_transition(
            CredentialState::Expired,
            CredentialState::Revoked
        ));
    }

    #[test]
    fn test_invalid_transitions() {
        // No-op transitions
        assert!(!is_valid_transition(
            CredentialState::Active,
            CredentialState::Active
        ));

        // Terminal states
        assert!(!is_valid_transition(
            CredentialState::Revoked,
            CredentialState::Active
        ));
        assert!(!is_valid_transition(
            CredentialState::Archived,
            CredentialState::Active
        ));

        // Invalid backward transitions
        assert!(!is_valid_transition(
            CredentialState::Active,
            CredentialState::Draft
        ));
        assert!(!is_valid_transition(
            CredentialState::Expired,
            CredentialState::Active
        ));
    }

    #[test]
    fn test_terminal_states() {
        assert!(is_terminal_state(CredentialState::Revoked));
        assert!(is_terminal_state(CredentialState::Archived));
        assert!(!is_terminal_state(CredentialState::Active));
        assert!(!is_terminal_state(CredentialState::Draft));
    }

    #[test]
    fn test_terminal_states_block_all_outbound_transitions() {
        // Exhaustive: every target state must be rejected from Revoked/Archived,
        // including same-state and Draft (the "revival" vector).
        let all_states = [
            CredentialState::Draft,
            CredentialState::Active,
            CredentialState::Suspended,
            CredentialState::Revoked,
            CredentialState::Expired,
            CredentialState::Archived,
        ];
        for to in all_states {
            assert!(
                !is_valid_transition(CredentialState::Revoked, to),
                "Revoked -> {to:?} must be rejected"
            );
            assert!(
                !is_valid_transition(CredentialState::Archived, to),
                "Archived -> {to:?} must be rejected"
            );
        }
    }

    #[test]
    fn test_active_states() {
        assert!(is_active_state(CredentialState::Active));
        assert!(!is_active_state(CredentialState::Draft));
        assert!(!is_active_state(CredentialState::Revoked));
    }
}

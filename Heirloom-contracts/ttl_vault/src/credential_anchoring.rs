/// Issue #32 — Credential Anchoring to External Systems
///
/// Credentials are isolated on-chain. This module provides a registry that
/// anchors on-chain credential IDs to identifiers in external systems (e.g.,
/// a traditional database, a government registry, or an enterprise identity
/// provider).
///
/// # Design
///
/// Each anchor record maps:
/// - `credential_id` (u64) — the on-chain SBT/credential identifier
/// - `external_id` (Bytes) — opaque identifier in the external system
///   (e.g., a UUID, a government ID number hash, or a database primary key)
/// - `system` (Bytes) — a short label identifying the external system
///   (e.g., b"kyc-v1", b"dmv-us", b"hr-db")
///
/// Anchor keys are indexed by `(external_id, system)` so callers can look up
/// whether a given external identity has already been anchored on-chain.
///
/// # Anchor format conventions (off-chain)
///
/// | `system` value  | `external_id` format                        |
/// |-----------------|---------------------------------------------|
/// | `kyc-v1`        | SHA-256 hash of the KYC provider's user ID  |
/// | `gov-id`        | SHA-256 hash of the national ID number      |
/// | `hr-db`         | UTF-8 company employee UUID (≤128 bytes)    |
/// | `email`         | SHA-256 hash of the email address           |
///
/// Raw PII is never stored on-chain — only opaque hashes or pseudonymous IDs.
///
/// # Validation rules
///
/// - `external_id` must not be empty.
/// - `system` must not be empty and must be ≤ 64 bytes.
/// - An anchor for `(external_id, system)` cannot be overwritten once set; the
///   caller must explicitly delete the old anchor first with
///   `remove_credential_anchor`.
/// - Only the credential owner (or the vault admin in a governance context)
///   should call `create_credential_anchor`; auth is enforced by the outer
///   wrapper in `lib.rs`.
use soroban_sdk::{contracttype, symbol_short, Bytes, Env, Vec};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum length (bytes) of the `system` label.
pub const MAX_SYSTEM_LEN: u32 = 64;

/// Maximum length (bytes) of the `external_id` value.
pub const MAX_EXTERNAL_ID_LEN: u32 = 256;

// ── Event topics ─────────────────────────────────────────────────────────────

pub const ANCHOR_CREATED_TOPIC: soroban_sdk::Symbol = symbol_short!("anc_crt");
pub const ANCHOR_REMOVED_TOPIC: soroban_sdk::Symbol = symbol_short!("anc_rem");

// ── Storage keys ─────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub enum AnchorKey {
    /// Forward lookup: credential_id → list of (external_id, system) tuples
    /// stored as a flat Vec<Bytes> in groups of two.
    CredentialAnchors(u64),
    /// Reverse lookup: (external_id_bytes ++ system_bytes) → credential_id.
    /// The composite key is the concatenation stored as Bytes.
    ReverseAnchor(Bytes),
    /// Total number of anchors ever created (monotonic counter).
    AnchorCount,
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// A single anchor record linking a credential to one external system entry.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CredentialAnchor {
    /// The on-chain credential / SBT identifier.
    pub credential_id: u64,
    /// Opaque identifier within the external system.
    pub external_id: Bytes,
    /// Short label identifying the external system.
    pub system: Bytes,
    /// Ledger timestamp when this anchor was created.
    pub created_at: u64,
}

// ── Events ────────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug)]
pub struct AnchorCreatedEvent {
    pub credential_id: u64,
    /// Truncated to 32 bytes in the event for gas efficiency.
    pub external_id: Bytes,
    pub system: Bytes,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct AnchorRemovedEvent {
    pub credential_id: u64,
    pub external_id: Bytes,
    pub system: Bytes,
}

// ── Internal helper ───────────────────────────────────────────────────────────

/// Build the composite reverse-lookup key: external_id ++ system.
fn composite_key(env: &Env, external_id: &Bytes, system: &Bytes) -> Bytes {
    let mut key = Bytes::new(env);
    key.append(external_id);
    key.append(system);
    key
}

// ── Core functions ────────────────────────────────────────────────────────────

/// Anchor `credential_id` to `external_id` in `system`.
///
/// Returns `Err(AnchorAlreadyExists)` represented as `true` from the caller
/// (the lib.rs wrapper converts it to a `ContractError`).  The boolean return
/// lets the inner function stay free of the outer error type.
///
/// Returns `true` on success.
/// Returns `false` if an anchor for `(external_id, system)` already exists.
pub fn create_credential_anchor(
    env: &Env,
    credential_id: u64,
    external_id: Bytes,
    system: Bytes,
) -> bool {
    // --- validation ---
    if external_id.is_empty() || system.is_empty() {
        return false;
    }
    if system.len() > MAX_SYSTEM_LEN || external_id.len() > MAX_EXTERNAL_ID_LEN {
        return false;
    }

    let rev_key = AnchorKey::ReverseAnchor(composite_key(env, &external_id, &system));

    // Reject duplicate anchors.
    if env.storage().persistent().has::<AnchorKey>(&rev_key) {
        return false;
    }

    let now = env.ledger().timestamp();
    let anchor = CredentialAnchor {
        credential_id,
        external_id: external_id.clone(),
        system: system.clone(),
        created_at: now,
    };

    // Store the reverse lookup.
    env.storage().persistent().set(&rev_key, &credential_id);
    env.storage().persistent().extend_ttl(
        &rev_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    // Append to the credential's forward list.
    let fwd_key = AnchorKey::CredentialAnchors(credential_id);
    let mut anchors: Vec<CredentialAnchor> = env
        .storage()
        .persistent()
        .get(&fwd_key)
        .unwrap_or_else(|| Vec::new(env));
    anchors.push_back(anchor);
    env.storage().persistent().set(&fwd_key, &anchors);
    env.storage().persistent().extend_ttl(
        &fwd_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    // Increment global counter.
    let count_key = AnchorKey::AnchorCount;
    let count: u64 = env.storage().persistent().get(&count_key).unwrap_or(0u64);
    env.storage()
        .persistent()
        .set(&count_key, &count.saturating_add(1));
    env.storage().persistent().extend_ttl(
        &count_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.events().publish(
        (ANCHOR_CREATED_TOPIC, credential_id),
        AnchorCreatedEvent {
            credential_id,
            external_id,
            system,
        },
    );

    true
}

/// Remove the anchor for `(external_id, system)` from `credential_id`.
///
/// Returns `true` if the anchor existed and was removed; `false` if it did not
/// exist (no-op, caller may treat this as success or handle as an error).
pub fn remove_credential_anchor(
    env: &Env,
    credential_id: u64,
    external_id: &Bytes,
    system: &Bytes,
) -> bool {
    let rev_key = AnchorKey::ReverseAnchor(composite_key(env, external_id, system));

    if !env.storage().persistent().has::<AnchorKey>(&rev_key) {
        return false;
    }

    // Remove reverse lookup.
    env.storage().persistent().remove(&rev_key);

    // Remove from forward list.
    let fwd_key = AnchorKey::CredentialAnchors(credential_id);
    if let Some(anchors) = env
        .storage()
        .persistent()
        .get::<AnchorKey, Vec<CredentialAnchor>>(&fwd_key)
    {
        let mut updated: Vec<CredentialAnchor> = Vec::new(env);
        for a in anchors.iter() {
            if &a.external_id != external_id || &a.system != system {
                updated.push_back(a);
            }
        }
        env.storage().persistent().set(&fwd_key, &updated);
        env.storage().persistent().extend_ttl(
            &fwd_key,
            crate::VAULT_TTL_THRESHOLD,
            crate::VAULT_TTL_LEDGERS,
        );
    }

    env.events().publish(
        (ANCHOR_REMOVED_TOPIC, credential_id),
        AnchorRemovedEvent {
            credential_id,
            external_id: external_id.clone(),
            system: system.clone(),
        },
    );

    true
}

/// Look up the credential ID anchored to `(external_id, system)`.
///
/// Returns `Some(credential_id)` if a match is found AND the credential is in
/// an active state. Returns `None` if no match is found OR the credential is
/// in a non-active state (Revoked, Archived, Suspended, etc.).
pub fn verify_external_anchor(env: &Env, external_id: &Bytes, system: &Bytes) -> Option<u64> {
    let rev_key = AnchorKey::ReverseAnchor(composite_key(env, external_id, system));
    let credential_id = env.storage().persistent().get::<AnchorKey, u64>(&rev_key)?;

    // Check if credential is in an active state
    let state = crate::credential_lifecycle::get_credential_state(env, credential_id);
    if crate::credential_lifecycle::is_active_state(state) {
        Some(credential_id)
    } else {
        None
    }
}

/// Returns `true` if `(external_id, system)` maps to some on-chain credential.
pub fn anchor_exists(env: &Env, external_id: &Bytes, system: &Bytes) -> bool {
    verify_external_anchor(env, external_id, system).is_some()
}

/// Retrieve all anchors registered for `credential_id`.
pub fn get_credential_anchors(env: &Env, credential_id: u64) -> Vec<CredentialAnchor> {
    let fwd_key = AnchorKey::CredentialAnchors(credential_id);
    env.storage()
        .persistent()
        .get(&fwd_key)
        .unwrap_or_else(|| Vec::new(env))
}

/// Total number of anchors ever created across all credentials.
pub fn get_anchor_count(env: &Env) -> u64 {
    env.storage()
        .persistent()
        .get::<AnchorKey, u64>(&AnchorKey::AnchorCount)
        .unwrap_or(0)
}

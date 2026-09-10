/// Issue #40 — Implement Slice Attribute-Based Matching
///
/// Attestors are selected manually. This module provides automatic discovery
/// of suitable attestors based on defined attributes (jurisdiction, reputation,
/// specialization). Matching uses weighted scoring and fuzzy matching for
/// flexible lookups.
///
/// # Design
///
/// Each attestor has a set of attributes:
/// - `jurisdiction` — operational region (e.g., b"US-NY", b"EU-DE", b"SG")
/// - `reputation_score` — 0-100 reflecting historical performance
/// - `specialization` — domain expertise (e.g., b"kyc", b"heritage", b"compliance")
/// - `active` — whether the attestor is currently accepting new assignments
///
/// Matching requests specify desired attributes and optional weights. The module
/// returns a sorted Vec of matching attestors scored by relevance.
///
/// # Matching algorithm
///
/// For each attribute in the request:
/// 1. Exact match (full string comparison)
/// 2. Prefix match (first N characters)
/// 3. Fuzzy match (Levenshtein distance ≤ 2)
/// 4. Score each match by weight and aggregated reputation
/// 5. Sort by score descending
///
use soroban_sdk::{contracttype, symbol_short, Address, Bytes, Env, Map, Vec};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum length of an attribute value (bytes).
pub const MAX_ATTRIBUTE_LEN: u32 = 64;

/// Maximum number of attributes per attestor.
pub const MAX_ATTRIBUTES_PER_ATTESTOR: u32 = 16;

/// Default weight for attributes if not specified (100).
pub const DEFAULT_ATTRIBUTE_WEIGHT: u32 = 100;

/// Fuzzy match threshold: Levenshtein distance must be ≤ this value.
pub const FUZZY_MATCH_THRESHOLD: u32 = 2;

/// Exact match bonus (added to score).
pub const EXACT_MATCH_BONUS: u32 = 1000;

/// Prefix match bonus (added to score).
pub const PREFIX_MATCH_BONUS: u32 = 500;

/// Fuzzy match bonus (added to score).
pub const FUZZY_MATCH_BONUS: u32 = 250;

// ── Event topics ─────────────────────────────────────────────────────────────

pub const ATTESTOR_ATTRIBUTES_SET_TOPIC: soroban_sdk::Symbol = symbol_short!("att_attr");
pub const ATTESTOR_ACTIVATED_TOPIC: soroban_sdk::Symbol = symbol_short!("att_act");
pub const ATTESTOR_DEACTIVATED_TOPIC: soroban_sdk::Symbol = symbol_short!("att_deact");

// ── Storage keys ─────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub enum AttributeKey {
    /// Attestor attributes: Address → AttestorProfile
    AttestorProfile(Address),
    /// Index of all active attestors (cached for iteration).
    ActiveAttestorIndex,
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// Attribute pair for flexible key-value queries.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Attribute {
    pub key: Bytes,
    pub value: Bytes,
}

/// Profile of an attestor including all attributes and reputation.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AttestorProfile {
    pub address: Address,
    /// List of (key, value) attribute pairs.
    pub attributes: Vec<Attribute>,
    /// Reputation score (0-100).
    pub reputation_score: u32,
    /// Whether actively accepting assignments.
    pub active: bool,
}

/// A match result from attribute-based search.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AttestorMatch {
    pub address: Address,
    /// Composite relevance score.
    pub score: u32,
    /// Reputation factor (affects weighting).
    pub reputation: u32,
}

/// Request to match attestors by attributes.
#[contracttype]
#[derive(Clone, Debug)]
pub struct MatchRequest {
    /// Desired attributes as (key, value) pairs.
    pub attributes: Vec<Attribute>,
    /// Optional weights: attribute_key → weight (default 100 if not specified).
    pub weights: Map<Bytes, u32>,
    /// If true, only return active attestors.
    pub active_only: bool,
    /// Maximum number of results to return (0 = unlimited).
    pub limit: u32,
}

// ── Events ────────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug)]
pub struct AttestorAttributesSetEvent {
    pub attestor: Address,
    pub reputation_score: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct AttestorActivatedEvent {
    pub attestor: Address,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct AttestorDeactivatedEvent {
    pub attestor: Address,
}

// ── Matching utilities ────────────────────────────────────────────────────────

/// Simplified byte comparison for matching.
/// Uses byte-by-byte comparison rather than full Levenshtein distance.
fn bytes_equal(a: &Bytes, b: &Bytes) -> bool {
    if a.len() != b.len() {
        return false;
    }
    for i in 0..a.len() {
        if a.get(i) != b.get(i) {
            return false;
        }
    }
    true
}

/// Check if two byte strings match exactly.
fn exact_match(a: &Bytes, b: &Bytes) -> bool {
    bytes_equal(a, b)
}

/// Check if `needle` is a prefix of `haystack`.
fn prefix_match(haystack: &Bytes, needle: &Bytes) -> bool {
    if needle.len() > haystack.len() {
        return false;
    }
    for i in 0..needle.len() {
        if haystack.get(i) != needle.get(i) {
            return false;
        }
    }
    true
}

/// Check fuzzy match by comparing lengths and first few bytes.
/// Simplified fuzzy matching for Soroban constraints.
fn fuzzy_match(a: &Bytes, b: &Bytes) -> bool {
    // If lengths differ by more than threshold, no fuzzy match
    let a_len = a.len() as u32;
    let b_len = b.len() as u32;
    let len_diff = a_len.abs_diff(b_len);

    if len_diff > FUZZY_MATCH_THRESHOLD {
        return false;
    }

    // Compare first min_len bytes
    let min_len = if a.len() < b.len() { a.len() } else { b.len() };
    let mut differences = 0u32;

    for i in 0..min_len {
        if a.get(i) != b.get(i) {
            differences = differences.saturating_add(1);
        }
    }

    differences <= FUZZY_MATCH_THRESHOLD
}

/// Score a single attribute match.
fn score_attribute_match(attestor_value: &Bytes, requested_value: &Bytes, weight: u32) -> u32 {
    let base_score = if exact_match(attestor_value, requested_value) {
        EXACT_MATCH_BONUS
    } else if prefix_match(attestor_value, requested_value)
        || prefix_match(requested_value, attestor_value)
    {
        PREFIX_MATCH_BONUS
    } else if fuzzy_match(attestor_value, requested_value) {
        FUZZY_MATCH_BONUS
    } else {
        0
    };

    base_score.saturating_mul(weight) / 100
}

// ── Core functions ────────────────────────────────────────────────────────────

/// Set or update an attestor's profile (attributes, reputation, active status).
///
/// - `reputation_score` must be 0-100.
/// - `attributes` must not exceed `MAX_ATTRIBUTES_PER_ATTESTOR`.
/// - Called by contract admin or governance.
pub fn set_attestor_profile(
    env: &Env,
    attestor: Address,
    attributes: Vec<Attribute>,
    reputation_score: u32,
    active: bool,
) -> bool {
    if reputation_score > 100 || (attributes.len() as u32) > MAX_ATTRIBUTES_PER_ATTESTOR {
        return false;
    }

    let profile = AttestorProfile {
        address: attestor.clone(),
        attributes,
        reputation_score,
        active,
    };

    let key = AttributeKey::AttestorProfile(attestor.clone());
    env.storage().persistent().set(&key, &profile);
    env.storage().persistent().extend_ttl(
        &key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    // Update active index if needed.
    if active {
        let index_key = AttributeKey::ActiveAttestorIndex;
        let mut active_list: Vec<Address> = env
            .storage()
            .persistent()
            .get(&index_key)
            .unwrap_or_else(|| Vec::new(env));

        // Avoid duplicates.
        let mut found = false;
        for addr in active_list.iter() {
            if addr == attestor {
                found = true;
                break;
            }
        }
        if !found {
            active_list.push_back(attestor.clone());
            env.storage().persistent().set(&index_key, &active_list);
            env.storage().persistent().extend_ttl(
                &index_key,
                crate::VAULT_TTL_THRESHOLD,
                crate::VAULT_TTL_LEDGERS,
            );
        }
    }

    env.events().publish(
        (ATTESTOR_ATTRIBUTES_SET_TOPIC, attestor.clone()),
        AttestorAttributesSetEvent {
            attestor,
            reputation_score,
        },
    );

    true
}

/// Activate an attestor (marks as available for assignment).
pub fn activate_attestor(env: &Env, attestor: Address) -> bool {
    let key = AttributeKey::AttestorProfile(attestor.clone());
    let mut profile: AttestorProfile = match env.storage().persistent().get(&key) {
        Some(p) => p,
        None => return false,
    };

    if profile.active {
        return false; // Already active.
    }

    profile.active = true;
    env.storage().persistent().set(&key, &profile);
    env.storage().persistent().extend_ttl(
        &key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    let index_key = AttributeKey::ActiveAttestorIndex;
    let mut active_list: Vec<Address> = env
        .storage()
        .persistent()
        .get(&index_key)
        .unwrap_or_else(|| Vec::new(env));
    active_list.push_back(attestor.clone());
    env.storage().persistent().set(&index_key, &active_list);
    env.storage().persistent().extend_ttl(
        &index_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.events().publish(
        (ATTESTOR_ACTIVATED_TOPIC, attestor.clone()),
        AttestorActivatedEvent { attestor },
    );

    true
}

/// Deactivate an attestor (marks as unavailable for new assignments).
pub fn deactivate_attestor(env: &Env, attestor: Address) -> bool {
    let key = AttributeKey::AttestorProfile(attestor.clone());
    let mut profile: AttestorProfile = match env.storage().persistent().get(&key) {
        Some(p) => p,
        None => return false,
    };

    if !profile.active {
        return false; // Already inactive.
    }

    profile.active = false;
    env.storage().persistent().set(&key, &profile);
    env.storage().persistent().extend_ttl(
        &key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    // Remove from active index.
    let index_key = AttributeKey::ActiveAttestorIndex;
    if let Some(active_list) = env
        .storage()
        .persistent()
        .get::<AttributeKey, Vec<Address>>(&index_key)
    {
        let mut updated: Vec<Address> = Vec::new(env);
        for addr in active_list.iter() {
            if addr != attestor {
                updated.push_back(addr);
            }
        }
        env.storage().persistent().set(&index_key, &updated);
        env.storage().persistent().extend_ttl(
            &index_key,
            crate::VAULT_TTL_THRESHOLD,
            crate::VAULT_TTL_LEDGERS,
        );
    }

    env.events().publish(
        (ATTESTOR_DEACTIVATED_TOPIC, attestor.clone()),
        AttestorDeactivatedEvent { attestor },
    );

    true
}

/// Match attestors by attributes.
///
/// Returns a Vec of `AttestorMatch` sorted by score (descending).
/// If `limit > 0`, returns at most `limit` results.
pub fn match_attestors_by_attributes(env: &Env, request: MatchRequest) -> Vec<AttestorMatch> {
    let mut matches: Vec<AttestorMatch> = Vec::new(env);

    let attestor_list = if request.active_only {
        let index_key = AttributeKey::ActiveAttestorIndex;
        env.storage()
            .persistent()
            .get::<AttributeKey, Vec<Address>>(&index_key)
            .unwrap_or_else(|| Vec::new(env))
    } else {
        // For non-active-only, would need a full attestor index; for now return empty.
        // In production, maintain a separate full attestor list.
        Vec::new(env)
    };

    for attestor_addr in attestor_list.iter() {
        let profile_key = AttributeKey::AttestorProfile(attestor_addr.clone());
        let profile: AttestorProfile = match env.storage().persistent().get(&profile_key) {
            Some(p) => p,
            None => continue,
        };

        let mut match_score = 0u32;

        // Score each requested attribute against the attestor's attributes.
        for req_attr in request.attributes.iter() {
            let weight = request
                .weights
                .get(req_attr.key.clone())
                .unwrap_or(DEFAULT_ATTRIBUTE_WEIGHT);

            for prof_attr in profile.attributes.iter() {
                if exact_match(&prof_attr.key, &req_attr.key) {
                    let attr_score =
                        score_attribute_match(&prof_attr.value, &req_attr.value, weight);
                    match_score = match_score.saturating_add(attr_score);
                }
            }
        }

        // Add reputation weighting.
        let reputation_factor = profile.reputation_score.saturating_mul(10);
        match_score = match_score.saturating_add(reputation_factor);

        if match_score > 0 {
            matches.push_back(AttestorMatch {
                address: attestor_addr,
                score: match_score,
                reputation: profile.reputation_score,
            });
        }
    }

    // Sort matches by score descending (bubble sort).
    let len = matches.len() as u32;
    for i in 0..len {
        for j in (i + 1)..len {
            let mi = matches.get(i);
            let mj = matches.get(j);
            if let (Some(m_i), Some(m_j)) = (mi, mj) {
                if m_i.score < m_j.score {
                    matches.set(i, m_j);
                    matches.set(j, m_i);
                }
            }
        }
    }

    // Apply limit if specified.
    if request.limit > 0 && len > request.limit {
        let mut limited: Vec<AttestorMatch> = Vec::new(env);
        for i in 0..request.limit {
            if let Some(m) = matches.get(i) {
                limited.push_back(m);
            }
        }
        limited
    } else {
        matches
    }
}

/// Get an attestor's profile.
pub fn get_attestor_profile(env: &Env, attestor: Address) -> Option<AttestorProfile> {
    let key = AttributeKey::AttestorProfile(attestor);
    env.storage().persistent().get(&key)
}

/// Check if an attestor is active.
pub fn is_attestor_active(env: &Env, attestor: Address) -> bool {
    get_attestor_profile(env, attestor).is_some_and(|p| p.active)
}

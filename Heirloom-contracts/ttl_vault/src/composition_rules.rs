/// Issue #44 — Slice Composition Validation Rules Engine
///
/// Provides a configurable rules engine for validating slice composition.
/// Rules are stored on-chain as opaque `Bytes` payloads (JSON policy fragments,
/// ABI-encoded predicates, or any off-chain-interpretable format) alongside
/// on-chain metadata: a priority, an optional description tag, and an
/// enabled/disabled flag.
///
/// # Rule lifecycle
///
/// 1. Admin (or vault owner, depending on the caller context) registers a rule
///    with `register_composition_rule` — returns a monotonically increasing
///    `rule_id`.
/// 2. Rules can be enabled/disabled individually without deletion.
/// 3. `validate_slice_with_rules` runs all enabled rules in priority order and
///    collects per-rule pass/fail results into a `ValidationResult`.
/// 4. Conflicts between rules of the same priority are detected and surfaced in
///    `ValidationResult::conflicts`.
///
/// # Conflict detection
///
/// Two rules *conflict* when they share the same priority **and** one passes
/// while the other fails for the same slice.  In that situation both rule IDs
/// are recorded in `ValidationResult::conflicts` and `overall_valid` is set to
/// `false`.
use soroban_sdk::{contracttype, symbol_short, Bytes, Env, Vec};

// ── Event topics ─────────────────────────────────────────────────────────────

pub const RULE_REGISTERED_TOPIC: soroban_sdk::Symbol = symbol_short!("rl_reg");
pub const RULE_UPDATED_TOPIC: soroban_sdk::Symbol = symbol_short!("rl_upd");
pub const SLICE_VALIDATED_TOPIC: soroban_sdk::Symbol = symbol_short!("sl_val");

// ── Storage keys ─────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub enum RulesEngineKey {
    /// Individual rule record, keyed by rule_id.
    Rule(u64),
    /// Monotonic counter — next rule_id to assign.
    RuleCount,
    /// Ordered list of rule IDs for a slice (vec of u64).
    SliceRules(u64),
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// A validation rule stored on-chain.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CompositionRule {
    /// Unique identifier, assigned at registration.
    pub rule_id: u64,
    /// Opaque rule payload (policy bytes, ABI-encoded predicate, etc.).
    pub rule_bytes: Bytes,
    /// Lower value == higher priority (0 is highest).
    pub priority: u32,
    /// Human-readable tag (max 9 chars for `symbol_short!` compatibility;
    /// stored as a raw u32 tag so no alloc is needed in `no_std`).
    pub tag: u32,
    /// Whether this rule is currently active.
    pub enabled: bool,
    /// Ledger timestamp when the rule was last modified.
    pub updated_at: u64,
}

/// Per-rule outcome produced by `validate_slice_with_rules`.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RuleOutcome {
    pub rule_id: u64,
    pub priority: u32,
    /// `true` if the rule's predicate was satisfied.
    pub passed: bool,
}

/// Aggregate result returned by `validate_slice_with_rules`.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ValidationResult {
    pub slice_id: u64,
    /// `true` only when every enabled rule passed and no conflicts were found.
    pub overall_valid: bool,
    /// Individual outcomes in priority order.
    pub outcomes: Vec<RuleOutcome>,
    /// Pairs of rule IDs that conflict (same priority, different outcomes).
    /// Each conflict is stored as `(rule_id_a, rule_id_b)` flattened into a
    /// `Vec<u64>` in groups of two.
    pub conflicts: Vec<u64>,
    /// Ledger timestamp at which validation was run.
    pub validated_at: u64,
}

// ── Events ────────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug)]
pub struct RuleRegisteredEvent {
    pub rule_id: u64,
    pub priority: u32,
    pub tag: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct RuleUpdatedEvent {
    pub rule_id: u64,
    pub enabled: bool,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct SliceValidatedEvent {
    pub slice_id: u64,
    pub overall_valid: bool,
    pub rules_evaluated: u32,
    pub conflicts_found: u32,
}

// ── Core functions ────────────────────────────────────────────────────────────

/// Register a new composition rule and return its `rule_id`.
///
/// - `rule_bytes` — opaque policy payload (caller-defined encoding).
/// - `priority` — lower == higher priority; rules with the same priority are
///   checked for conflicts.
/// - `tag` — numeric tag for categorization (e.g. a hash of a string label).
pub fn register_composition_rule(env: &Env, rule_bytes: Bytes, priority: u32, tag: u32) -> u64 {
    let rule_id: u64 = env
        .storage()
        .persistent()
        .get::<RulesEngineKey, u64>(&RulesEngineKey::RuleCount)
        .unwrap_or(0);

    let rule = CompositionRule {
        rule_id,
        rule_bytes,
        priority,
        tag,
        enabled: true,
        updated_at: env.ledger().timestamp(),
    };

    env.storage()
        .persistent()
        .set(&RulesEngineKey::Rule(rule_id), &rule);
    env.storage().persistent().extend_ttl(
        &RulesEngineKey::Rule(rule_id),
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    let next_id = rule_id.saturating_add(1);
    env.storage()
        .persistent()
        .set(&RulesEngineKey::RuleCount, &next_id);
    env.storage().persistent().extend_ttl(
        &RulesEngineKey::RuleCount,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.events().publish(
        (RULE_REGISTERED_TOPIC,),
        RuleRegisteredEvent {
            rule_id,
            priority,
            tag,
        },
    );

    rule_id
}

/// Enable or disable an existing rule.
pub fn set_rule_enabled(env: &Env, rule_id: u64, enabled: bool) {
    let key = RulesEngineKey::Rule(rule_id);
    let mut rule: CompositionRule = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| panic_with_rule_not_found(env));

    rule.enabled = enabled;
    rule.updated_at = env.ledger().timestamp();

    env.storage().persistent().set(&key, &rule);
    env.storage().persistent().extend_ttl(
        &key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.events().publish(
        (RULE_UPDATED_TOPIC, rule_id),
        RuleUpdatedEvent { rule_id, enabled },
    );
}

/// Associate an ordered list of `rule_ids` with `slice_id`.
///
/// This replaces any previous association.  Pass an empty `Vec` to clear.
pub fn set_slice_rules(env: &Env, slice_id: u64, rule_ids: Vec<u64>) {
    let key = RulesEngineKey::SliceRules(slice_id);
    env.storage().persistent().set(&key, &rule_ids);
    env.storage().persistent().extend_ttl(
        &key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );
}

/// Retrieve the rule IDs associated with `slice_id`.
pub fn get_slice_rules(env: &Env, slice_id: u64) -> Vec<u64> {
    let key = RulesEngineKey::SliceRules(slice_id);
    env.storage()
        .persistent()
        .get::<RulesEngineKey, Vec<u64>>(&key)
        .unwrap_or_else(|| Vec::new(env))
}

/// Retrieve a single rule by ID.  Returns `None` if not found.
pub fn get_rule(env: &Env, rule_id: u64) -> Option<CompositionRule> {
    let key = RulesEngineKey::Rule(rule_id);
    env.storage().persistent().get(&key)
}

/// Validate `slice_id` against all its associated enabled rules.
///
/// # Validation logic
///
/// Each rule's `rule_bytes` payload is evaluated against `slice_data` — the
/// raw bytes representation of the slice being validated.  The evaluation uses
/// the following on-chain predicate:
///
/// > A rule **passes** when `slice_data` starts with the rule's `rule_bytes`
/// > prefix *or* when `rule_bytes` is empty (unconditional pass).
///
/// This prefix-match semantics is deliberately simple so the contract can
/// enforce it deterministically without an interpreter.  Richer validation
/// (regex, JSON schema, etc.) is expected to be performed off-chain by
/// indexers that subscribe to the `SliceValidated` event, using the full
/// `rule_bytes` payload stored on-chain as the policy specification.
///
/// Rules are evaluated in ascending priority order (lower number first).
/// Conflicts are detected as described in the module doc.
pub fn validate_slice_with_rules(env: &Env, slice_id: u64, slice_data: &Bytes) -> ValidationResult {
    let rule_ids = get_slice_rules(env, slice_id);

    let mut outcomes: Vec<RuleOutcome> = Vec::new(env);
    let mut conflicts: Vec<u64> = Vec::new(env);
    let mut overall_valid = true;

    // ── Sort rule IDs by priority (insertion sort — small N) ─────────────────
    // We need to build a locally sorted copy.  Soroban Vec doesn't provide
    // sort, so we collect into a buffer and bubble-sort it.
    let mut sorted_ids: Vec<u64> = Vec::new(env);
    for id in rule_ids.iter() {
        sorted_ids.push_back(id);
    }
    // Bubble sort by priority (ascending).
    let n = sorted_ids.len();
    for i in 0..n {
        for j in 0..n.saturating_sub(i + 1) {
            let id_j = sorted_ids.get(j).unwrap();
            let id_j1 = sorted_ids.get(j + 1).unwrap();
            let prio_j = get_rule(env, id_j).map_or(u32::MAX, |r| r.priority);
            let prio_j1 = get_rule(env, id_j1).map_or(u32::MAX, |r| r.priority);
            if prio_j > prio_j1 {
                sorted_ids.set(j, id_j1);
                sorted_ids.set(j + 1, id_j);
            }
        }
    }

    // ── Evaluate rules ────────────────────────────────────────────────────────
    for rule_id in sorted_ids.iter() {
        let Some(rule) = get_rule(env, rule_id) else {
            continue;
        };
        if !rule.enabled {
            continue;
        }

        let passed = evaluate_rule(slice_data, &rule.rule_bytes);

        if !passed {
            overall_valid = false;
        }

        outcomes.push_back(RuleOutcome {
            rule_id,
            priority: rule.priority,
            passed,
        });
    }

    // ── Detect conflicts ──────────────────────────────────────────────────────
    // Two outcomes conflict when same priority + different pass/fail result.
    let olen = outcomes.len();
    for i in 0..olen {
        for j in (i + 1)..olen {
            let oi = outcomes.get(i).unwrap();
            let oj = outcomes.get(j).unwrap();
            if oi.priority == oj.priority && oi.passed != oj.passed {
                conflicts.push_back(oi.rule_id);
                conflicts.push_back(oj.rule_id);
                overall_valid = false;
            }
        }
    }

    let conflicts_found = (conflicts.len() / 2) as u32;
    let rules_evaluated = outcomes.len();

    let result = ValidationResult {
        slice_id,
        overall_valid,
        outcomes,
        conflicts,
        validated_at: env.ledger().timestamp(),
    };

    env.events().publish(
        (SLICE_VALIDATED_TOPIC, slice_id),
        SliceValidatedEvent {
            slice_id,
            overall_valid,
            rules_evaluated,
            conflicts_found,
        },
    );

    result
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Evaluate a single rule against `slice_data`.
///
/// Passes when `rule_bytes` is empty (unconditional pass) or when `slice_data`
/// starts with the `rule_bytes` prefix.
fn evaluate_rule(slice_data: &Bytes, rule_bytes: &Bytes) -> bool {
    let rlen = rule_bytes.len();
    if rlen == 0 {
        return true;
    }
    let slen = slice_data.len();
    if slen < rlen {
        return false;
    }
    // Compare byte-by-byte over the prefix.
    for i in 0..rlen {
        if slice_data.get(i).unwrap() != rule_bytes.get(i).unwrap() {
            return false;
        }
    }
    true
}

#[cold]
#[inline(never)]
fn panic_with_rule_not_found(env: &Env) -> ! {
    soroban_sdk::panic_with_error!(env, crate::ContractError::RuleNotFound)
}

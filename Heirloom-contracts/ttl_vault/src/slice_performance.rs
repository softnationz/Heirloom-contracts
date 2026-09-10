/// Issue #36 — Slice Performance-Based Weighting
///
/// Tracks per-attestor performance metrics (response time, success rate) for
/// vault slices and uses them to compute optimal BPS weights.  Weights are
/// stored persistently and can be reapplied via `reweight_slice`.
///
/// # Algorithm
/// Each attestor accumulates:
/// - `total_responses` — number of recorded observations
/// - `successful_responses` — how many returned success
/// - `total_response_time_ms` — cumulative response latency in milliseconds
///
/// The optimal weight for attestor *i* is calculated as:
///
/// ```text
/// score_i  = success_rate_i × (1 / avg_latency_i)
/// weight_i = (score_i / sum_of_all_scores) × 10_000   [BPS]
/// ```
///
/// If an attestor has zero responses the score defaults to zero and the BPS
/// weight is set to 0.  If **all** attestors have zero score (e.g. no data
/// yet) each attestor is assigned an equal share so that BPS always sums to
/// 10 000.  Any rounding remainder is absorbed by the first attestor.
use soroban_sdk::{contracttype, symbol_short, Address, Env, String, Vec};

// ── Event topics ─────────────────────────────────────────────────────────────

pub const ATTESTOR_PERF_RECORDED_TOPIC: soroban_sdk::Symbol = symbol_short!("atst_rec");
pub const SLICE_REWEIGHTED_TOPIC: soroban_sdk::Symbol = symbol_short!("sl_rewt");
pub const REPUTATION_DECAY_APPLIED_TOPIC: soroban_sdk::Symbol = symbol_short!("rep_dec");
pub const REPUTATION_RECOVERED_TOPIC: soroban_sdk::Symbol = symbol_short!("rep_rec");

// ── Storage key ──────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub enum SlicePerfKey {
    /// Performance record for a single attestor on a given slice.
    AttestorPerf(u64, Address),
    /// Latest computed BPS weights for a slice.
    SliceWeights(u64),
    /// Reputation decay factor for an attestor on a slice (scaled 0-10000, where 10000 = no decay).
    ReputationDecay(u64, Address),
    /// Decay history entries tracking when decay was applied.
    DecayHistory(u64, Address, u64), // (slice_id, attestor, entry_index)
    /// Count of decay history entries for an attestor on a slice.
    DecayHistoryCount(u64, Address),
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// Accumulated performance data for one attestor on one slice.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PerformanceMetrics {
    /// Total number of observed responses (success + failure).
    pub total_responses: u64,
    /// Number of responses counted as successful.
    pub successful_responses: u64,
    /// Cumulative response latency in milliseconds.
    pub total_response_time_ms: u64,
    /// Ledger timestamp of the last recorded observation.
    pub last_recorded_at: u64,
}

/// BPS weight assigned to one attestor for a slice.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct AttestorWeight {
    pub attestor: Address,
    /// Basis-points allocation (sum across all attestors for a slice == 10 000).
    pub weight_bps: u32,
}

// ── Events ────────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug)]
pub struct AttestorPerfRecordedEvent {
    pub slice_id: u64,
    pub attestor: Address,
    pub success: bool,
    pub response_time_ms: u64,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct SliceReweightedEvent {
    pub slice_id: u64,
    /// Number of attestors that received updated weights.
    pub attestor_count: u32,
}

/// Reputation decay event published when decay is applied to an attestor.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ReputationDecayAppliedEvent {
    pub slice_id: u64,
    pub attestor: Address,
    /// Decay rate applied (in BPS, 0-10000 where 10000 = no decay, 0 = full decay).
    pub decay_rate_bps: u32,
    /// New reputation factor after decay (scaled 0-10000).
    pub new_reputation_factor: u32,
    /// Reason for decay (e.g., "low_success_rate", "high_latency", "manual_decay").
    pub reason: String,
}

/// Reputation recovery event published when reputation improves.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ReputationRecoveredEvent {
    pub slice_id: u64,
    pub attestor: Address,
    /// Performance improvement metric that triggered recovery.
    pub improvement_factor: u32,
    /// New reputation factor after recovery (scaled 0-10000).
    pub new_reputation_factor: u32,
}

/// Decay history entry tracking reputation changes over time.
#[contracttype]
#[derive(Clone, Debug)]
pub struct DecayHistoryEntry {
    /// Ledger timestamp when the decay was applied.
    pub applied_at: u64,
    /// Decay rate applied (in BPS, 0-10000).
    pub decay_rate_bps: u32,
    /// Reputation factor before decay.
    pub reputation_before: u32,
    /// Reputation factor after decay.
    pub reputation_after: u32,
    /// Optional reason/description.
    pub reason: String,
}

// ── Core functions ────────────────────────────────────────────────────────────

/// Record one performance observation for `attestor` on `slice_id`.
///
/// - `caller` must be the vault owner (auth enforced by the outer
///   `TtlVaultContract` wrapper before calling this helper).
/// - `success` — whether the attestor responded correctly.
/// - `response_time_ms` — round-trip latency in milliseconds.
pub fn record_attestor_performance(
    env: &Env,
    slice_id: u64,
    attestor: &Address,
    success: bool,
    response_time_ms: u64,
) {
    let key = SlicePerfKey::AttestorPerf(slice_id, attestor.clone());

    let mut metrics: PerformanceMetrics =
        env.storage()
            .persistent()
            .get(&key)
            .unwrap_or(PerformanceMetrics {
                total_responses: 0,
                successful_responses: 0,
                total_response_time_ms: 0,
                last_recorded_at: 0,
            });

    metrics.total_responses = metrics.total_responses.saturating_add(1);
    if success {
        metrics.successful_responses = metrics.successful_responses.saturating_add(1);
    }
    metrics.total_response_time_ms = metrics
        .total_response_time_ms
        .saturating_add(response_time_ms);
    metrics.last_recorded_at = env.ledger().timestamp();

    env.storage().persistent().set(&key, &metrics);
    env.storage().persistent().extend_ttl(
        &key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.events().publish(
        (ATTESTOR_PERF_RECORDED_TOPIC, slice_id),
        AttestorPerfRecordedEvent {
            slice_id,
            attestor: attestor.clone(),
            success,
            response_time_ms,
        },
    );
}

/// Retrieve the current performance metrics for a single attestor on a slice.
/// Returns `None` if no data has been recorded yet.
pub fn get_attestor_performance(
    env: &Env,
    slice_id: u64,
    attestor: &Address,
) -> Option<PerformanceMetrics> {
    let key = SlicePerfKey::AttestorPerf(slice_id, attestor.clone());
    env.storage().persistent().get(&key)
}

/// Compute optimal BPS weights for all `attestors` on `slice_id` based on
/// their stored performance data.
///
/// Returns a `Vec<AttestorWeight>` in the same order as `attestors`.
pub fn calculate_optimal_weights(
    env: &Env,
    slice_id: u64,
    attestors: &Vec<Address>,
) -> Vec<AttestorWeight> {
    // ── 1. Gather raw scores (u64 fixed-point: score × 1 000 000) ────────────
    // score = success_rate × (1 000 000 / avg_latency_ms)
    // Both numerator components are scaled to avoid floating point.

    let mut scores: Vec<u64> = Vec::new(env);
    let mut score_sum: u64 = 0u64;

    for attestor in attestors.iter() {
        let key = SlicePerfKey::AttestorPerf(slice_id, attestor.clone());
        let score = if let Some(m) = env
            .storage()
            .persistent()
            .get::<SlicePerfKey, PerformanceMetrics>(&key)
        {
            // success_rate_scaled = (successful_responses * 1_000_000) / total_responses
            // avg_latency_ms — floor at 1 to avoid zero denominator
            // score = success_rate_scaled / avg_latency
            let success_rate_scaled = m
                .successful_responses
                .saturating_mul(1_000_000)
                .checked_div(m.total_responses)
                .unwrap_or(0);
            let avg_latency = m
                .total_response_time_ms
                .checked_div(m.total_responses)
                .unwrap_or(0)
                .max(1);
            success_rate_scaled.checked_div(avg_latency).unwrap_or(0)
        } else {
            0u64
        };

        scores.push_back(score);
        score_sum = score_sum.saturating_add(score);
    }

    // ── 2. Convert scores → BPS weights ──────────────────────────────────────
    let mut weights: Vec<AttestorWeight> = Vec::new(env);
    let count = attestors.len();

    if count == 0 {
        return weights;
    }

    if score_sum == 0 {
        // No performance data yet — distribute equally.
        let equal_bps = 10_000u32 / count;
        let remainder = 10_000u32 - equal_bps * count;
        for (idx, attestor) in attestors.iter().enumerate() {
            let w = if idx == 0 {
                equal_bps + remainder
            } else {
                equal_bps
            };
            weights.push_back(AttestorWeight {
                attestor,
                weight_bps: w,
            });
        }
    } else {
        let mut bps_assigned: u32 = 0u32;
        let last_idx = count - 1;
        for (idx, (attestor, score)) in attestors.iter().zip(scores.iter()).enumerate() {
            let bps = if idx as u32 == last_idx {
                // absorb rounding remainder in the last attestor
                10_000u32.saturating_sub(bps_assigned)
            } else {
                let raw = score
                    .saturating_mul(10_000)
                    .checked_div(score_sum)
                    .unwrap_or(0);
                raw.min(10_000u64) as u32
            };
            bps_assigned = bps_assigned.saturating_add(bps);
            weights.push_back(AttestorWeight {
                attestor,
                weight_bps: bps,
            });
        }
    }

    weights
}

/// Persist the optimal weights for `slice_id` and emit a `SliceReweightedEvent`.
///
/// Call this after `calculate_optimal_weights` to make the new weights durable.
pub fn reweight_slice(env: &Env, slice_id: u64, weights: Vec<AttestorWeight>) {
    let count = weights.len();
    let key = SlicePerfKey::SliceWeights(slice_id);
    env.storage().persistent().set(&key, &weights);
    env.storage().persistent().extend_ttl(
        &key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.events().publish(
        (SLICE_REWEIGHTED_TOPIC, slice_id),
        SliceReweightedEvent {
            slice_id,
            attestor_count: count,
        },
    );
}

/// Retrieve the latest persisted BPS weights for `slice_id`.
pub fn get_slice_weights(env: &Env, slice_id: u64) -> Option<Vec<AttestorWeight>> {
    let key = SlicePerfKey::SliceWeights(slice_id);
    env.storage().persistent().get(&key)
}

// ── Reputation Decay Functions ─────────────────────────────────────────────────

/// Apply reputation decay to an attestor on a slice.
///
/// The decay mechanism reduces attestor reputation scores when performance degrades.
/// - `decay_rate_bps` — decay percentage in basis points (0-10000)
///   - 10000 = no decay (preserve reputation)
///   - 5000 = 50% decay
///   - 0 = complete decay (reputation becomes 0)
/// - `reason` — descriptive reason for decay (e.g., "low_success_rate")
///
/// Returns the new reputation factor (0-10000).
pub fn apply_reputation_decay(
    env: &Env,
    slice_id: u64,
    attestor: &Address,
    decay_rate_bps: u32,
    reason: String,
) -> u32 {
    // Load current reputation factor (default to full reputation if none exists).
    let decay_key = SlicePerfKey::ReputationDecay(slice_id, attestor.clone());
    let current_reputation: u32 = env
        .storage()
        .persistent()
        .get(&decay_key)
        .unwrap_or(10_000u32);

    // Apply decay: new_reputation = current * decay_rate / 10000
    let new_reputation = current_reputation
        .saturating_mul(decay_rate_bps)
        .saturating_div(10_000u32);

    // Persist the new reputation.
    env.storage().persistent().set(&decay_key, &new_reputation);
    env.storage().persistent().extend_ttl(
        &decay_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    // Record decay in history.
    record_decay_history(
        env,
        slice_id,
        attestor,
        decay_rate_bps,
        current_reputation,
        new_reputation,
        reason.clone(),
    );

    // Publish decay event.
    env.events().publish(
        (REPUTATION_DECAY_APPLIED_TOPIC, slice_id),
        ReputationDecayAppliedEvent {
            slice_id,
            attestor: attestor.clone(),
            decay_rate_bps,
            new_reputation_factor: new_reputation,
            reason,
        },
    );

    new_reputation
}

/// Recover reputation when performance improves.
///
/// Gradually restores reputation toward the original 10000 baseline, applying
/// diminishing returns to prevent rapid reputation swings.
/// - `improvement_rate_bps` — recovery percentage in basis points (0-10000)
///   - 10000 = recover at maximum rate toward baseline
///   - 5000 = recover at 50% rate
///   - 0 = no recovery
///
/// Returns the new reputation factor (0-10000).
pub fn apply_reputation_recovery(
    env: &Env,
    slice_id: u64,
    attestor: &Address,
    improvement_rate_bps: u32,
) -> u32 {
    // Load current reputation factor.
    let decay_key = SlicePerfKey::ReputationDecay(slice_id, attestor.clone());
    let current_reputation: u32 = env
        .storage()
        .persistent()
        .get(&decay_key)
        .unwrap_or(10_000u32);

    // If already at max reputation, no recovery needed.
    if current_reputation >= 10_000u32 {
        return current_reputation;
    }

    // Recovery: move toward baseline at rate proportional to improvement_rate_bps
    // new_reputation = current + (10000 - current) * improvement_rate / 10000
    let max_recovery = 10_000u32.saturating_sub(current_reputation);
    let recovery_amount = max_recovery
        .saturating_mul(improvement_rate_bps)
        .saturating_div(10_000u32);
    let new_reputation = current_reputation
        .saturating_add(recovery_amount)
        .min(10_000u32);

    // Persist the recovered reputation.
    env.storage().persistent().set(&decay_key, &new_reputation);
    env.storage().persistent().extend_ttl(
        &decay_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    // Record in decay history.
    record_decay_history(
        env,
        slice_id,
        attestor,
        improvement_rate_bps,
        current_reputation,
        new_reputation,
        String::from_str(env, "reputation_recovery"),
    );

    // Publish recovery event.
    env.events().publish(
        (REPUTATION_RECOVERED_TOPIC, slice_id),
        ReputationRecoveredEvent {
            slice_id,
            attestor: attestor.clone(),
            improvement_factor: improvement_rate_bps,
            new_reputation_factor: new_reputation,
        },
    );

    new_reputation
}

/// Retrieve the current reputation factor for an attestor on a slice.
/// Returns 10000 (full reputation) if no decay history exists.
pub fn get_reputation_factor(env: &Env, slice_id: u64, attestor: &Address) -> u32 {
    let decay_key = SlicePerfKey::ReputationDecay(slice_id, attestor.clone());
    env.storage()
        .persistent()
        .get(&decay_key)
        .unwrap_or(10_000u32)
}

/// Record a decay history entry for an attestor on a slice.
fn record_decay_history(
    env: &Env,
    slice_id: u64,
    attestor: &Address,
    rate_bps: u32,
    reputation_before: u32,
    reputation_after: u32,
    reason: String,
) {
    let count_key = SlicePerfKey::DecayHistoryCount(slice_id, attestor.clone());
    let current_count: u64 = env.storage().persistent().get(&count_key).unwrap_or(0u64);

    let entry = DecayHistoryEntry {
        applied_at: env.ledger().timestamp(),
        decay_rate_bps: rate_bps,
        reputation_before,
        reputation_after,
        reason,
    };

    let history_key = SlicePerfKey::DecayHistory(slice_id, attestor.clone(), current_count);
    env.storage().persistent().set(&history_key, &entry);
    env.storage().persistent().extend_ttl(
        &history_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    // Increment count.
    let new_count = current_count.saturating_add(1);
    env.storage().persistent().set(&count_key, &new_count);
    env.storage().persistent().extend_ttl(
        &count_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );
}

/// Retrieve decay history entries for an attestor on a slice.
/// Returns up to `limit` entries starting from the most recent.
pub fn get_decay_history(
    env: &Env,
    slice_id: u64,
    attestor: &Address,
    limit: u64,
) -> Vec<DecayHistoryEntry> {
    let count_key = SlicePerfKey::DecayHistoryCount(slice_id, attestor.clone());
    let total_count: u64 = env.storage().persistent().get(&count_key).unwrap_or(0u64);

    let mut entries: Vec<DecayHistoryEntry> = Vec::new(env);

    if total_count == 0 {
        return entries;
    }

    // Iterate from most recent to oldest, up to limit.
    let mut retrieved = 0u64;

    // Start from the most recent and work backwards.
    for i in (0..total_count).rev() {
        if retrieved >= limit {
            break;
        }
        let history_key = SlicePerfKey::DecayHistory(slice_id, attestor.clone(), i);
        if let Some(entry) = env
            .storage()
            .persistent()
            .get::<SlicePerfKey, DecayHistoryEntry>(&history_key)
        {
            entries.push_back(entry);
            retrieved = retrieved.saturating_add(1);
        }
    }

    entries
}

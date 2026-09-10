/// Issue #38 — Slice Composition Cost Tracking
///
/// There is no visibility into the resource cost of slice operations.  This
/// module provides per-slice cost accounting so operators can identify
/// expensive slices and make informed optimization decisions.
///
/// # Model
///
/// Each slice accumulates a `CostLedger` with four categories of cost:
/// - `compute_units` — Soroban instruction units consumed by operations on
///   this slice (caller-reported; real instrumentation is off-chain).
/// - `storage_bytes` — net bytes of persistent storage consumed.
/// - `event_count` — number of events emitted by slice operations.
/// - `cross_contract_calls` — cross-contract invocations triggered.
///
/// When a slice operation completes, the caller reports the observed deltas via
/// `record_slice_operation_cost`.  These deltas are accumulated into the
/// `CostLedger`.
///
/// # Cost projections
///
/// `project_slice_cost` extrapolates a `CostProjection` from the historical
/// average cost per operation and a caller-supplied number of future operations.
///
/// # Optimization hints
///
/// `get_cost_optimization_hints` inspects the `CostLedger` and returns a
/// `Vec<Bytes>` of short hint strings (ASCII, ≤64 bytes each) drawn from a
/// rule table:
///
/// | Condition                              | Hint                        |
/// |----------------------------------------|-----------------------------|
/// | avg_compute_units > 10_000             | b"reduce-compute"           |
/// | storage_bytes > 50_000                 | b"optimize-storage"         |
/// | avg_cross_calls > 2 per operation      | b"batch-cross-calls"        |
/// | event_count / ops > 5                  | b"reduce-events"            |
use soroban_sdk::{contracttype, symbol_short, Bytes, Env, Vec};

// ── Constants ─────────────────────────────────────────────────────────────────

/// If average compute units per operation exceeds this threshold, emit a
/// "reduce-compute" hint.
pub const COMPUTE_WARN_THRESHOLD: u64 = 10_000;

/// If total storage bytes exceeds this threshold, emit an "optimize-storage" hint.
pub const STORAGE_WARN_THRESHOLD: u64 = 50_000;

/// If average cross-contract calls per operation exceeds this value, emit a
/// "batch-cross-calls" hint.
pub const CROSS_CALL_WARN_THRESHOLD: u64 = 2;

/// If average events per operation exceeds this value, emit a "reduce-events" hint.
pub const EVENT_WARN_THRESHOLD: u64 = 5;

// ── Event topics ─────────────────────────────────────────────────────────────

pub const COST_RECORDED_TOPIC: soroban_sdk::Symbol = symbol_short!("cost_rec");
pub const COST_RESET_TOPIC: soroban_sdk::Symbol = symbol_short!("cost_rst");

// ── Storage keys ─────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub enum CostKey {
    /// Accumulated cost ledger for a slice.
    SliceCost(u64),
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// The running cost totals for a single slice.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CostLedger {
    /// Total Soroban compute units consumed (caller-reported).
    pub compute_units: u64,
    /// Total persistent storage bytes consumed (net).
    pub storage_bytes: u64,
    /// Total number of on-chain events emitted.
    pub event_count: u64,
    /// Total number of cross-contract calls made.
    pub cross_contract_calls: u64,
    /// Number of operations recorded (used for computing averages).
    pub operation_count: u64,
    /// Ledger timestamp of the first recorded operation.
    pub first_recorded_at: u64,
    /// Ledger timestamp of the most recent recorded operation.
    pub last_recorded_at: u64,
}

/// A cost breakdown for a single slice, including derived metrics.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CostBreakdown {
    pub slice_id: u64,
    /// Raw accumulated totals.
    pub ledger: CostLedger,
    /// Average compute units per operation (0 if no operations recorded).
    pub avg_compute_units: u64,
    /// Average storage bytes per operation.
    pub avg_storage_bytes: u64,
    /// Average events per operation.
    pub avg_events: u64,
    /// Average cross-contract calls per operation.
    pub avg_cross_calls: u64,
}

/// A forward cost projection for a given number of future operations.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CostProjection {
    pub slice_id: u64,
    /// Number of future operations being projected.
    pub projected_operations: u64,
    /// Projected total compute units.
    pub projected_compute_units: u64,
    /// Projected total storage bytes.
    pub projected_storage_bytes: u64,
    /// Projected total events.
    pub projected_events: u64,
    /// Projected total cross-contract calls.
    pub projected_cross_calls: u64,
}

/// Cost deltas reported for a single slice operation.
#[contracttype]
#[derive(Clone, Debug)]
pub struct OperationCostDelta {
    pub slice_id: u64,
    pub compute_units: u64,
    pub storage_bytes: u64,
    pub event_count: u64,
    pub cross_contract_calls: u64,
}

// ── Events ────────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug)]
pub struct CostRecordedEvent {
    pub slice_id: u64,
    pub compute_units: u64,
    pub storage_bytes: u64,
    pub operation_count: u64,
}

// ── Core functions ────────────────────────────────────────────────────────────

/// Record the resource cost of one slice operation.
///
/// - `compute_units` — Soroban instruction units consumed.
/// - `storage_bytes` — net bytes of persistent storage consumed.
/// - `event_count` — events emitted during this operation.
/// - `cross_contract_calls` — cross-contract calls made.
pub fn record_slice_operation_cost(
    env: &Env,
    slice_id: u64,
    compute_units: u64,
    storage_bytes: u64,
    event_count: u64,
    cross_contract_calls: u64,
) {
    let key = CostKey::SliceCost(slice_id);
    let now = env.ledger().timestamp();

    let mut ledger: CostLedger = env.storage().persistent().get(&key).unwrap_or(CostLedger {
        compute_units: 0,
        storage_bytes: 0,
        event_count: 0,
        cross_contract_calls: 0,
        operation_count: 0,
        first_recorded_at: now,
        last_recorded_at: now,
    });

    ledger.compute_units = ledger.compute_units.saturating_add(compute_units);
    ledger.storage_bytes = ledger.storage_bytes.saturating_add(storage_bytes);
    ledger.event_count = ledger.event_count.saturating_add(event_count);
    ledger.cross_contract_calls = ledger
        .cross_contract_calls
        .saturating_add(cross_contract_calls);
    ledger.operation_count = ledger.operation_count.saturating_add(1);
    ledger.last_recorded_at = now;

    env.storage().persistent().set(&key, &ledger);
    env.storage().persistent().extend_ttl(
        &key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.events().publish(
        (COST_RECORDED_TOPIC, slice_id),
        CostRecordedEvent {
            slice_id,
            compute_units,
            storage_bytes,
            operation_count: ledger.operation_count,
        },
    );
}

/// Return a full `CostBreakdown` for `slice_id`, including derived averages.
///
/// Returns `None` if no operations have been recorded for this slice.
#[allow(clippy::manual_checked_ops)]
pub fn get_slice_cost_breakdown(env: &Env, slice_id: u64) -> Option<CostBreakdown> {
    let key = CostKey::SliceCost(slice_id);
    let ledger: CostLedger = env.storage().persistent().get(&key)?;

    let ops = ledger.operation_count;
    let (avg_compute, avg_storage, avg_events, avg_cross) = if ops > 0 {
        (
            ledger.compute_units / ops,
            ledger.storage_bytes / ops,
            ledger.event_count / ops,
            ledger.cross_contract_calls / ops,
        )
    } else {
        (0, 0, 0, 0)
    };

    Some(CostBreakdown {
        slice_id,
        ledger,
        avg_compute_units: avg_compute,
        avg_storage_bytes: avg_storage,
        avg_events,
        avg_cross_calls: avg_cross,
    })
}

/// Project costs for `projected_operations` future operations based on the
/// historical average for `slice_id`.
///
/// Returns `None` if no baseline data exists yet.
pub fn project_slice_cost(
    env: &Env,
    slice_id: u64,
    projected_operations: u64,
) -> Option<CostProjection> {
    let breakdown = get_slice_cost_breakdown(env, slice_id)?;

    Some(CostProjection {
        slice_id,
        projected_operations,
        projected_compute_units: breakdown
            .avg_compute_units
            .saturating_mul(projected_operations),
        projected_storage_bytes: breakdown
            .avg_storage_bytes
            .saturating_mul(projected_operations),
        projected_events: breakdown.avg_events.saturating_mul(projected_operations),
        projected_cross_calls: breakdown
            .avg_cross_calls
            .saturating_mul(projected_operations),
    })
}

/// Return a list of short optimization hint strings for `slice_id`.
///
/// Each hint is an ASCII `Bytes` value of ≤64 bytes.  An empty `Vec` means the
/// slice is operating within normal cost parameters.
pub fn get_cost_optimization_hints(env: &Env, slice_id: u64) -> Vec<Bytes> {
    let mut hints: Vec<Bytes> = Vec::new(env);
    let Some(bd) = get_slice_cost_breakdown(env, slice_id) else {
        return hints;
    };

    if bd.avg_compute_units > COMPUTE_WARN_THRESHOLD {
        hints.push_back(Bytes::from_slice(env, b"reduce-compute"));
    }
    if bd.ledger.storage_bytes > STORAGE_WARN_THRESHOLD {
        hints.push_back(Bytes::from_slice(env, b"optimize-storage"));
    }
    if bd.avg_cross_calls > CROSS_CALL_WARN_THRESHOLD {
        hints.push_back(Bytes::from_slice(env, b"batch-cross-calls"));
    }
    if bd.avg_events > EVENT_WARN_THRESHOLD {
        hints.push_back(Bytes::from_slice(env, b"reduce-events"));
    }

    hints
}

/// Reset the cost ledger for `slice_id` (admin/owner operation).
///
/// Returns `true` if a ledger existed and was cleared; `false` if there was
/// nothing to reset.
pub fn reset_slice_cost(env: &Env, slice_id: u64) -> bool {
    let key = CostKey::SliceCost(slice_id);
    if !env.storage().persistent().has::<CostKey>(&key) {
        return false;
    }
    env.storage().persistent().remove(&key);
    env.events().publish((COST_RESET_TOPIC, slice_id), slice_id);
    true
}

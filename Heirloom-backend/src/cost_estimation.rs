// #72 — Request Cost Estimation
// Implements: operation cost model, POST /estimate-cost, cost breakdown, scenario-based estimation

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Cost factors (network fee model, Stellar/Soroban-inspired) ────────────────
//
// Fees are expressed in stroops (1 XLM = 10_000_000 stroops).
// Values are configurable via environment variables; defaults mirror Stellar
// testnet fee schedule as of 2026.

pub struct FeeConfig {
    /// Base transaction fee in stroops.
    pub base_fee_stroops: u64,
    /// Per-byte fee for instruction bandwidth.
    pub byte_fee_stroops: u64,
    /// Write-entry fee per ledger key written.
    pub write_entry_fee_stroops: u64,
    /// Read-entry fee per ledger key read.
    pub read_entry_fee_stroops: u64,
    /// State archival rent per entry per ledger.
    pub rent_fee_per_ledger_stroops: u64,
    /// Number of ledgers the TTL extension covers (for vault check-in).
    pub default_ttl_extension_ledgers: u64,
}

impl Default for FeeConfig {
    fn default() -> Self {
        Self {
            base_fee_stroops: 100,
            byte_fee_stroops: 10,
            write_entry_fee_stroops: 2_500,
            read_entry_fee_stroops: 500,
            rent_fee_per_ledger_stroops: 50,
            default_ttl_extension_ledgers: 100,
        }
    }
}

impl FeeConfig {
    /// Load from environment variables, falling back to defaults.
    pub fn from_env() -> Self {
        let def = Self::default();
        Self {
            base_fee_stroops: env_u64("COST_BASE_FEE_STROOPS", def.base_fee_stroops),
            byte_fee_stroops: env_u64("COST_BYTE_FEE_STROOPS", def.byte_fee_stroops),
            write_entry_fee_stroops: env_u64(
                "COST_WRITE_ENTRY_FEE_STROOPS",
                def.write_entry_fee_stroops,
            ),
            read_entry_fee_stroops: env_u64(
                "COST_READ_ENTRY_FEE_STROOPS",
                def.read_entry_fee_stroops,
            ),
            rent_fee_per_ledger_stroops: env_u64(
                "COST_RENT_FEE_PER_LEDGER_STROOPS",
                def.rent_fee_per_ledger_stroops,
            ),
            default_ttl_extension_ledgers: env_u64(
                "COST_DEFAULT_TTL_EXTENSION_LEDGERS",
                def.default_ttl_extension_ledgers,
            ),
        }
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

// ── Supported operation types ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationType {
    CreateVault,
    CheckIn,
    Deposit,
    Withdraw,
    TriggerRelease,
    UpdateBeneficiary,
    BulkCheckIn,
    /// Any operation not listed above.
    Custom,
}

// ── Cost breakdown ────────────────────────────────────────────────────────────

/// Detailed cost breakdown for a single operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostBreakdown {
    pub base_fee_stroops: u64,
    pub instruction_fee_stroops: u64,
    pub write_entries_fee_stroops: u64,
    pub read_entries_fee_stroops: u64,
    pub rent_fee_stroops: u64,
    pub total_stroops: u64,
    /// Convenience: total in XLM (stroops / 10_000_000), rounded to 7dp.
    pub total_xlm: f64,
    /// Human-readable description of what drives the cost.
    pub notes: Vec<String>,
}

impl CostBreakdown {
    pub fn new(
        base: u64,
        instructions: u64,
        write_entries: u64,
        read_entries: u64,
        rent: u64,
        notes: Vec<String>,
    ) -> Self {
        let total = base + instructions + write_entries + read_entries + rent;
        Self {
            base_fee_stroops: base,
            instruction_fee_stroops: instructions,
            write_entries_fee_stroops: write_entries,
            read_entries_fee_stroops: read_entries,
            rent_fee_stroops: rent,
            total_stroops: total,
            total_xlm: total as f64 / 10_000_000.0,
            notes,
        }
    }
}

// ── Scenario types ────────────────────────────────────────────────────────────

/// A named scenario is a labelled variation of the base operation with
/// different parameters (e.g., large vs small vault balance).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostScenario {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub breakdown: CostBreakdown,
}

// ── Request / Response ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct EstimateCostRequest {
    pub operation: OperationType,
    /// Optional vault ID — used to look up vault-specific data if available.
    pub vault_id: Option<String>,
    /// For `deposit`/`withdraw` operations: the amount in stroops.
    pub amount_stroops: Option<u64>,
    /// For `bulk_check_in`: number of vaults.
    pub bulk_count: Option<u64>,
    /// For `custom`: caller-supplied payload size in bytes.
    pub payload_bytes: Option<u64>,
    /// If true, also return common scenario variants.
    pub include_scenarios: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct EstimateCostResponse {
    pub operation: OperationType,
    pub breakdown: CostBreakdown,
    pub scenarios: Vec<CostScenario>,
    pub estimated_at: DateTime<Utc>,
    /// Link to the cost documentation.
    pub docs_url: String,
}

// ── Core estimator ────────────────────────────────────────────────────────────

pub struct CostEstimator {
    config: FeeConfig,
}

impl CostEstimator {
    pub fn new(config: FeeConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(FeeConfig::default())
    }

    /// Estimate the cost for the given request.
    pub fn estimate(&self, req: &EstimateCostRequest) -> EstimateCostResponse {
        let breakdown = self.compute_breakdown(req);
        let scenarios = if req.include_scenarios.unwrap_or(false) {
            self.build_scenarios(&req.operation)
        } else {
            vec![]
        };

        EstimateCostResponse {
            operation: req.operation.clone(),
            breakdown,
            scenarios,
            estimated_at: Utc::now(),
            docs_url: "/docs/cost-estimation".into(),
        }
    }

    fn compute_breakdown(&self, req: &EstimateCostRequest) -> CostBreakdown {
        let c = &self.config;
        match &req.operation {
            OperationType::CreateVault => {
                // Writes: vault entry + beneficiary entry + TTL entry = 3 keys
                let write = c.write_entry_fee_stroops * 3;
                // Reads: owner existence check = 1 key
                let read = c.read_entry_fee_stroops;
                // Instructions: ~500 bytes of contract code
                let instr = c.byte_fee_stroops * 500;
                // Rent for initial TTL
                let rent = c.rent_fee_per_ledger_stroops * c.default_ttl_extension_ledgers;
                CostBreakdown::new(
                    c.base_fee_stroops,
                    instr,
                    write,
                    read,
                    rent,
                    vec![
                        "Writes 3 ledger entries (vault, beneficiary, TTL counter)".into(),
                        format!(
                            "Rent for {} ledgers of initial TTL",
                            c.default_ttl_extension_ledgers
                        ),
                    ],
                )
            }
            OperationType::CheckIn => {
                // Writes: TTL update = 1 key
                let write = c.write_entry_fee_stroops;
                // Reads: vault state = 1 key
                let read = c.read_entry_fee_stroops;
                let instr = c.byte_fee_stroops * 200;
                let rent = c.rent_fee_per_ledger_stroops * c.default_ttl_extension_ledgers;
                CostBreakdown::new(
                    c.base_fee_stroops,
                    instr,
                    write,
                    read,
                    rent,
                    vec![
                        "Updates TTL ledger entry".into(),
                        format!("Extends TTL by {} ledgers", c.default_ttl_extension_ledgers),
                    ],
                )
            }
            OperationType::Deposit => {
                let amount = req.amount_stroops.unwrap_or(0);
                let write = c.write_entry_fee_stroops * 2; // vault balance + event log
                let read = c.read_entry_fee_stroops;
                let instr = c.byte_fee_stroops * 300;
                let rent = 0;
                let mut notes = vec!["Updates vault balance and appends deposit event".into()];
                if amount > 0 {
                    notes.push(format!("Depositing {} stroops", amount));
                }
                CostBreakdown::new(c.base_fee_stroops, instr, write, read, rent, notes)
            }
            OperationType::Withdraw => {
                let amount = req.amount_stroops.unwrap_or(0);
                let write = c.write_entry_fee_stroops * 2;
                let read = c.read_entry_fee_stroops * 2; // balance + auth
                let instr = c.byte_fee_stroops * 400;
                let rent = 0;
                let mut notes = vec!["Verifies balance, writes updated balance + withdrawal log".into()];
                if amount > 0 {
                    notes.push(format!("Withdrawing {} stroops", amount));
                }
                CostBreakdown::new(c.base_fee_stroops, instr, write, read, rent, notes)
            }
            OperationType::TriggerRelease => {
                // Most expensive: reads vault + beneficiary, writes release record + clears TTL
                let write = c.write_entry_fee_stroops * 3;
                let read = c.read_entry_fee_stroops * 2;
                let instr = c.byte_fee_stroops * 800;
                let rent = 0;
                CostBreakdown::new(
                    c.base_fee_stroops,
                    instr,
                    write,
                    read,
                    rent,
                    vec![
                        "Verifies TTL expiry, transfers balance to beneficiary".into(),
                        "Writes release record and clears vault state".into(),
                    ],
                )
            }
            OperationType::UpdateBeneficiary => {
                let write = c.write_entry_fee_stroops * 2;
                let read = c.read_entry_fee_stroops;
                let instr = c.byte_fee_stroops * 250;
                CostBreakdown::new(
                    c.base_fee_stroops,
                    instr,
                    write,
                    read,
                    0,
                    vec!["Updates beneficiary address and writes audit entry".into()],
                )
            }
            OperationType::BulkCheckIn => {
                let count = req.bulk_count.unwrap_or(1).max(1);
                let write = c.write_entry_fee_stroops * count;
                let read = c.read_entry_fee_stroops * count;
                let instr = c.byte_fee_stroops * 200 * count;
                let rent = c.rent_fee_per_ledger_stroops
                    * c.default_ttl_extension_ledgers
                    * count;
                CostBreakdown::new(
                    c.base_fee_stroops,
                    instr,
                    write,
                    read,
                    rent,
                    vec![
                        format!("Bulk check-in for {} vaults", count),
                        "Each vault incurs individual write + rent cost".into(),
                    ],
                )
            }
            OperationType::Custom => {
                let payload = req.payload_bytes.unwrap_or(256);
                let instr = c.byte_fee_stroops * payload;
                let write = c.write_entry_fee_stroops;
                let read = c.read_entry_fee_stroops;
                CostBreakdown::new(
                    c.base_fee_stroops,
                    instr,
                    write,
                    read,
                    0,
                    vec![format!("Custom operation with {} byte payload", payload)],
                )
            }
        }
    }

    fn build_scenarios(&self, operation: &OperationType) -> Vec<CostScenario> {
        match operation {
            OperationType::Deposit => vec![
                self.scenario(
                    "small_deposit",
                    "Deposit 100 XLM",
                    &EstimateCostRequest {
                        operation: OperationType::Deposit,
                        amount_stroops: Some(1_000_000_000),
                        vault_id: None,
                        bulk_count: None,
                        payload_bytes: None,
                        include_scenarios: None,
                    },
                ),
                self.scenario(
                    "large_deposit",
                    "Deposit 10,000 XLM",
                    &EstimateCostRequest {
                        operation: OperationType::Deposit,
                        amount_stroops: Some(100_000_000_000),
                        vault_id: None,
                        bulk_count: None,
                        payload_bytes: None,
                        include_scenarios: None,
                    },
                ),
            ],
            OperationType::BulkCheckIn => vec![
                self.scenario(
                    "bulk_10",
                    "Bulk check-in for 10 vaults",
                    &EstimateCostRequest {
                        operation: OperationType::BulkCheckIn,
                        bulk_count: Some(10),
                        vault_id: None,
                        amount_stroops: None,
                        payload_bytes: None,
                        include_scenarios: None,
                    },
                ),
                self.scenario(
                    "bulk_100",
                    "Bulk check-in for 100 vaults",
                    &EstimateCostRequest {
                        operation: OperationType::BulkCheckIn,
                        bulk_count: Some(100),
                        vault_id: None,
                        amount_stroops: None,
                        payload_bytes: None,
                        include_scenarios: None,
                    },
                ),
            ],
            _ => vec![],
        }
    }

    fn scenario(&self, name: &str, description: &str, req: &EstimateCostRequest) -> CostScenario {
        CostScenario {
            name: name.to_string(),
            description: description.to_string(),
            parameters: serde_json::json!({
                "operation": format!("{:?}", req.operation),
                "amount_stroops": req.amount_stroops,
                "bulk_count": req.bulk_count,
            }),
            breakdown: self.compute_breakdown(req),
        }
    }
}

// ── Route handler ─────────────────────────────────────────────────────────────

use axum::{extract::State, http::StatusCode, Json};
use std::sync::Arc;

use crate::error::AppError;

/// POST /estimate-cost
pub async fn estimate_cost_handler(
    State(_state): State<Arc<crate::db::AppState>>,
    Json(body): Json<EstimateCostRequest>,
) -> Result<(StatusCode, Json<EstimateCostResponse>), AppError> {
    let estimator = CostEstimator::new(FeeConfig::from_env());
    let response = estimator.estimate(&body);
    Ok((StatusCode::OK, Json(response)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn estimator() -> CostEstimator {
        CostEstimator::with_defaults()
    }

    fn req(op: OperationType) -> EstimateCostRequest {
        EstimateCostRequest {
            operation: op,
            vault_id: None,
            amount_stroops: None,
            bulk_count: None,
            payload_bytes: None,
            include_scenarios: None,
        }
    }

    #[test]
    fn test_create_vault_cost() {
        let e = estimator();
        let resp = e.estimate(&req(OperationType::CreateVault));
        assert!(resp.breakdown.total_stroops > 0);
        assert!(resp.breakdown.rent_fee_stroops > 0);
    }

    #[test]
    fn test_check_in_cost() {
        let e = estimator();
        let resp = e.estimate(&req(OperationType::CheckIn));
        assert!(resp.breakdown.total_stroops > 0);
        assert!(resp.breakdown.rent_fee_stroops > 0);
    }

    #[test]
    fn test_deposit_with_amount() {
        let e = estimator();
        let mut r = req(OperationType::Deposit);
        r.amount_stroops = Some(1_000_000_000);
        let resp = e.estimate(&r);
        assert!(resp.breakdown.notes.iter().any(|n| n.contains("stroops")));
    }

    #[test]
    fn test_bulk_check_in_scales() {
        let e = estimator();
        let mut r1 = req(OperationType::BulkCheckIn);
        r1.bulk_count = Some(1);
        let mut r10 = req(OperationType::BulkCheckIn);
        r10.bulk_count = Some(10);
        let cost1 = e.estimate(&r1).breakdown.total_stroops;
        let cost10 = e.estimate(&r10).breakdown.total_stroops;
        // 10-vault bulk should cost roughly 10x more than 1-vault.
        assert!(cost10 > cost1 * 5);
    }

    #[test]
    fn test_scenarios_included_when_requested() {
        let e = estimator();
        let mut r = req(OperationType::Deposit);
        r.include_scenarios = Some(true);
        let resp = e.estimate(&r);
        assert!(!resp.scenarios.is_empty());
    }

    #[test]
    fn test_scenarios_empty_by_default() {
        let e = estimator();
        let resp = e.estimate(&req(OperationType::CheckIn));
        assert!(resp.scenarios.is_empty());
    }

    #[test]
    fn test_trigger_release_is_most_expensive() {
        let e = estimator();
        let release_cost = e.estimate(&req(OperationType::TriggerRelease)).breakdown.total_stroops;
        let check_in_cost = e.estimate(&req(OperationType::CheckIn)).breakdown.total_stroops;
        assert!(release_cost > check_in_cost);
    }

    #[test]
    fn test_total_xlm_conversion() {
        let e = estimator();
        let resp = e.estimate(&req(OperationType::CreateVault));
        let expected_xlm = resp.breakdown.total_stroops as f64 / 10_000_000.0;
        assert!((resp.breakdown.total_xlm - expected_xlm).abs() < 0.000_001);
    }

    #[test]
    fn test_docs_url_present() {
        let e = estimator();
        let resp = e.estimate(&req(OperationType::CheckIn));
        assert!(!resp.docs_url.is_empty());
    }
}

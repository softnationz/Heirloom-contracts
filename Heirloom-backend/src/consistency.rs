/// Data consistency verification for the SQLite database.
///
/// `ConsistencyChecker` runs a battery of checks against the live database and
/// produces a `ConsistencyReport` containing every issue found together with a
/// severity classification.
///
/// The checks currently implemented are:
/// 1. **Foreign-key check** – delegates to SQLite's built-in
///    `PRAGMA foreign_key_check`.
/// 2. **Reminder consistency** – verifies that every `reminder_preferences`
///    row references a vault that has a matching subscription, and that
///    `hours_before_expiry` is greater than zero.
/// 3. **Derived-field check** – verifies that every `tenant_vaults` entry
///    references a tenant that exists in the `tenants` table.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::db::Db;

// ── IssueSeverity ─────────────────────────────────────────────────────────────

/// How severe a detected consistency issue is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueSeverity {
    /// A minor anomaly that does not affect correctness but should be
    /// investigated.
    Warning,
    /// A data integrity problem that may cause incorrect behaviour.
    Error,
    /// A serious problem that could lead to data loss or corruption.
    Critical,
}

// ── ConsistencyIssue ──────────────────────────────────────────────────────────

/// A single consistency problem discovered during a check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsistencyIssue {
    /// Short name identifying the check that produced this issue.
    pub check_name: String,
    /// Severity classification.
    pub severity: IssueSeverity,
    /// Human-readable description of the problem.
    pub description: String,
    /// Number of database rows affected by this issue.
    pub affected_rows: u64,
}

// ── ConsistencyReport ─────────────────────────────────────────────────────────

/// Aggregated result of running all consistency checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsistencyReport {
    /// When the checks were performed.
    pub checked_at: DateTime<Utc>,
    /// All issues found across every check (empty if all checks passed).
    pub issues: Vec<ConsistencyIssue>,
    /// Total number of distinct checks that were executed.
    pub total_checks: u32,
    /// Number of checks that found no issues.
    pub passed_checks: u32,
    /// Number of checks that produced at least one issue.
    pub failed_checks: u32,
}

// ── ConsistencyChecker ────────────────────────────────────────────────────────

/// Runs consistency checks against a `Db` instance.
pub struct ConsistencyChecker;

impl ConsistencyChecker {
    /// Create a new `ConsistencyChecker`.
    pub fn new() -> Self {
        Self
    }

    // ── Individual checks ─────────────────────────────────────────────────────

    /// Run SQLite's built-in `PRAGMA foreign_key_check` and return one
    /// `ConsistencyIssue` per violating row.
    ///
    /// Each row returned by the PRAGMA represents a foreign-key constraint
    /// violation.
    pub fn check_foreign_keys(db: &Db) -> Vec<ConsistencyIssue> {
        match db.run_consistency_pragma() {
            Ok(violations) if violations.is_empty() => vec![],
            Ok(violations) => {
                vec![ConsistencyIssue {
                    check_name: "foreign_key_check".to_string(),
                    severity: IssueSeverity::Critical,
                    description: format!(
                        "PRAGMA foreign_key_check returned {} violation(s): {}",
                        violations.len(),
                        violations.join("; ")
                    ),
                    affected_rows: violations.len() as u64,
                }]
            }
            Err(e) => {
                vec![ConsistencyIssue {
                    check_name: "foreign_key_check".to_string(),
                    severity: IssueSeverity::Error,
                    description: format!("Failed to execute PRAGMA foreign_key_check: {e}"),
                    affected_rows: 0,
                }]
            }
        }
    }

    /// Check that `reminder_preferences` rows are internally consistent:
    ///
    /// - `hours_before_expiry` must be greater than zero (invalid reminders
    ///   would fire at the exact moment of expiry, which is useless).
    /// - Every `vault_id` in `reminder_preferences` should have a matching row
    ///   in `vault_subscriptions`.
    pub fn check_reminder_consistency(db: &Db) -> Vec<ConsistencyIssue> {
        let mut issues = Vec::new();

        // ── hours_before_expiry > 0 ──────────────────────────────────────────
        let zero_hours_count = {
            let conn = db.conn_lock();
            conn.query_row(
                "SELECT COUNT(*) FROM reminder_preferences WHERE hours_before_expiry = 0 AND deleted_at IS NULL",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0) as u64
        };

        if zero_hours_count > 0 {
            issues.push(ConsistencyIssue {
                check_name: "reminder_hours_before_expiry".to_string(),
                severity: IssueSeverity::Warning,
                description: format!(
                    "{zero_hours_count} reminder_preferences row(s) have hours_before_expiry = 0"
                ),
                affected_rows: zero_hours_count,
            });
        }

        // ── reminder vault_ids exist in vault_subscriptions ──────────────────
        let orphan_count = {
            let conn = db.conn_lock();
            conn.query_row(
                r"SELECT COUNT(*) FROM reminder_preferences rp
                  WHERE rp.deleted_at IS NULL
                    AND NOT EXISTS (
                        SELECT 1 FROM vault_subscriptions vs
                        WHERE vs.vault_id = rp.vault_id
                    )",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0) as u64
        };

        if orphan_count > 0 {
            issues.push(ConsistencyIssue {
                check_name: "reminder_orphaned_vault_ids".to_string(),
                severity: IssueSeverity::Warning,
                description: format!(
                    "{orphan_count} reminder_preferences row(s) reference a vault_id with no subscription"
                ),
                affected_rows: orphan_count,
            });
        }

        issues
    }

    /// Verify that `tenant_vaults` rows reference tenants that exist in the
    /// `tenants` table.
    ///
    /// Because `tenant_vaults` uses a plain foreign key to `tenants(id)` that
    /// SQLite may not enforce at the row level (depending on pragma settings),
    /// this explicit check catches orphaned references.
    pub fn check_derived_fields(db: &Db) -> Vec<ConsistencyIssue> {
        let mut issues = Vec::new();

        let orphan_count = {
            let conn = db.conn_lock();
            conn.query_row(
                r"SELECT COUNT(*) FROM tenant_vaults tv
                  WHERE NOT EXISTS (
                      SELECT 1 FROM tenants t WHERE t.id = tv.tenant_id
                  )",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0) as u64
        };

        if orphan_count > 0 {
            issues.push(ConsistencyIssue {
                check_name: "tenant_vaults_orphaned_tenant_id".to_string(),
                severity: IssueSeverity::Error,
                description: format!(
                    "{orphan_count} tenant_vaults row(s) reference a tenant_id that does not exist in the tenants table"
                ),
                affected_rows: orphan_count,
            });
        }

        issues
    }

    // ── Consolidated runner ───────────────────────────────────────────────────

    /// Run every available check and return a `ConsistencyReport`.
    pub fn run_all_checks(db: &Db) -> ConsistencyReport {
        // Define each check as a boxed closure so we can track pass/fail
        // counts independently.
        let check_fns: &[(&str, fn(&Db) -> Vec<ConsistencyIssue>)] = &[
            ("foreign_keys", Self::check_foreign_keys),
            ("reminder_consistency", Self::check_reminder_consistency),
            ("derived_fields", Self::check_derived_fields),
        ];

        let total_checks = check_fns.len() as u32;
        let mut all_issues: Vec<ConsistencyIssue> = Vec::new();
        let mut failed_checks = 0u32;

        for (_name, check_fn) in check_fns {
            let issues = check_fn(db);
            if issues.is_empty() {
                // check passed – nothing to record
            } else {
                failed_checks += 1;
                all_issues.extend(issues);
            }
        }

        let passed_checks = total_checks - failed_checks;

        ConsistencyReport {
            checked_at: Utc::now(),
            issues: all_issues,
            total_checks,
            passed_checks,
            failed_checks,
        }
    }
}

impl Default for ConsistencyChecker {
    fn default() -> Self {
        Self::new()
    }
}

//! Compliance Audit Report — Task #99
//!
//! This module provides:
//!
//! - [`run_compliance_checks`]: evaluates GDPR, SOC2, and ISO 27001 controls
//!   against the current runtime environment and returns a list of
//!   [`ComplianceCheck`] results.
//! - [`generate_pdf_stub`]: returns a placeholder [`serde_json::Value`]
//!   representing where a real PDF export would appear.
//! - [`compliance_report_handler`]: axum handler for
//!   `GET /admin/compliance-report`.

use axum::{extract::State, http::HeaderMap, Json};
use chrono::Utc;
use std::sync::Arc;

use crate::{
    audit::authorize_admin,
    db::AppState,
    error::AppError,
    models::{
        ComplianceAuditReport, ComplianceCheck, ComplianceReportResponse, ComplianceStatus,
    },
};

// ── Compliance check helpers ─────────────────────────────────────────────────

/// Build a single [`ComplianceCheck`] from its constituent parts.
fn check(
    id: &str,
    name: &str,
    framework: &str,
    status: ComplianceStatus,
    description: &str,
    remediation: &str,
) -> ComplianceCheck {
    ComplianceCheck {
        id: id.to_string(),
        name: name.to_string(),
        framework: framework.to_string(),
        status,
        description: description.to_string(),
        remediation: remediation.to_string(),
    }
}

// ── GDPR checks ──────────────────────────────────────────────────────────────

fn gdpr_checks() -> Vec<ComplianceCheck> {
    let admin_key_set = !std::env::var("ADMIN_API_KEY")
        .unwrap_or_default()
        .is_empty();

    let reminder_email_key_set = !std::env::var("REMINDER_EMAIL_API_KEY")
        .unwrap_or_default()
        .is_empty();

    let reminder_sms_key_set = !std::env::var("REMINDER_SMS_API_KEY")
        .unwrap_or_default()
        .is_empty();

    vec![
        // Article 5 — Data minimisation
        check(
            "gdpr.data_minimisation",
            "Data Minimisation",
            "GDPR",
            ComplianceStatus::Pass,
            "Vault data stored by the backend is limited to owner address, beneficiary address, \
             balance, check-in interval, and event timestamps. No unnecessary personal data is \
             persisted.",
            "",
        ),
        // Article 5 — Purpose limitation
        check(
            "gdpr.purpose_limitation",
            "Purpose Limitation",
            "GDPR",
            ComplianceStatus::Pass,
            "Collected data (vault state, audit logs, notification preferences) is used \
             exclusively to operate the vault inheritance service. No secondary uses have been \
             identified.",
            "",
        ),
        // Article 5 — Storage limitation
        check(
            "gdpr.storage_limitation",
            "Storage Limitation",
            "GDPR",
            ComplianceStatus::Warning,
            "Audit log entries are currently retained indefinitely. GDPR Article 5(1)(e) \
             requires data to be kept for no longer than necessary.",
            "Implement a configurable audit-log retention policy (e.g. rolling 12-month window) \
             and schedule a periodic purge job.",
        ),
        // Article 13 / 14 — Transparency & notice
        check(
            "gdpr.transparency",
            "Transparency & Privacy Notice",
            "GDPR",
            ComplianceStatus::Warning,
            "There is no machine-readable or linked privacy notice exposed via the API. \
             Users should be informed of data processing purposes at the point of vault creation.",
            "Add a /privacy endpoint or embed a privacy-notice URL in onboarding responses. \
             Include the data controller identity, legal basis, and data subject rights.",
        ),
        // Article 17 — Right to erasure
        check(
            "gdpr.right_to_erasure",
            "Right to Erasure (Right to be Forgotten)",
            "GDPR",
            ComplianceStatus::Fail,
            "No API endpoint exists to honour a data-erasure request. The current schema \
             stores owner addresses in vault records and audit logs without a deletion path.",
            "Implement DELETE /api/users/{owner}/data that pseudonymises or removes vault and \
             audit-log records associated with an owner address, subject to legal hold \
             obligations.",
        ),
        // Article 20 — Data portability
        check(
            "gdpr.data_portability",
            "Data Portability",
            "GDPR",
            ComplianceStatus::Warning,
            "A JSON export endpoint (GET /api/vaults/{id}/export) exists but does not cover \
             notification preferences, audit logs, or unsubscribe tokens.",
            "Extend the export endpoint to include all data associated with the owner: \
             notification preferences, delivery logs, and audit entries.",
        ),
        // Article 25 — Privacy by design
        check(
            "gdpr.privacy_by_design",
            "Privacy by Design & Default",
            "GDPR",
            ComplianceStatus::Pass,
            "Passkey/WebAuthn authentication avoids storing seed phrases or passwords. \
             Vault balances are stored as i128 integers with no linkage to off-chain identity \
             beyond the owner's Stellar address.",
            "",
        ),
        // Article 32 — Security of processing
        {
            let (status, remediation) = if admin_key_set && reminder_email_key_set && reminder_sms_key_set {
                (
                    ComplianceStatus::Pass,
                    "".to_string(),
                )
            } else {
                let missing: Vec<&str> = [
                    (!admin_key_set).then_some("ADMIN_API_KEY"),
                    (!reminder_email_key_set).then_some("REMINDER_EMAIL_API_KEY"),
                    (!reminder_sms_key_set).then_some("REMINDER_SMS_API_KEY"),
                ]
                .into_iter()
                .flatten()
                .collect();
                (
                    ComplianceStatus::Fail,
                    format!(
                        "Set the following required environment variables before deploying to \
                         production: {}. Unset keys leave admin and notification endpoints \
                         unauthenticated.",
                        missing.join(", ")
                    ),
                )
            };
            check(
                "gdpr.security_of_processing",
                "Security of Processing",
                "GDPR",
                status,
                "Checks that secrets (ADMIN_API_KEY, REMINDER_EMAIL_API_KEY, \
                 REMINDER_SMS_API_KEY) are present, indicating that sensitive endpoints are \
                 protected in this deployment.",
                &remediation,
            )
        },
    ]
}

// ── SOC 2 checks ─────────────────────────────────────────────────────────────

fn soc2_checks() -> Vec<ComplianceCheck> {
    let cors_origins_set = !std::env::var("ALLOWED_ORIGINS")
        .unwrap_or_default()
        .is_empty();

    vec![
        // CC6.1 — Logical and physical access controls
        check(
            "soc2.cc6_1_access_controls",
            "Logical Access Controls (CC6.1)",
            "SOC2",
            if std::env::var("ADMIN_API_KEY").map(|v| !v.is_empty()).unwrap_or(false) {
                ComplianceStatus::Pass
            } else {
                ComplianceStatus::Fail
            },
            "The ADMIN_API_KEY environment variable gates all /admin/* endpoints. \
             When set, bearer-token authentication is enforced by authorize_admin().",
            if std::env::var("ADMIN_API_KEY").map(|v| !v.is_empty()).unwrap_or(false) {
                ""
            } else {
                "Set ADMIN_API_KEY in the deployment environment to enforce admin access controls."
            },
        ),
        // CC6.2 — Prior to issuing credentials
        check(
            "soc2.cc6_2_credential_issuance",
            "Credential Issuance Controls (CC6.2)",
            "SOC2",
            ComplianceStatus::Pass,
            "Vault access uses Passkey/WebAuthn credentials. No username/password pairs are \
             issued, eliminating credential-stuffing and phishing attack surfaces.",
            "",
        ),
        // CC6.3 — Access revocation
        check(
            "soc2.cc6_3_access_revocation",
            "Access Revocation (CC6.3)",
            "SOC2",
            ComplianceStatus::Warning,
            "Share tokens can be revoked via DELETE /api/vaults/{id}/shares/tokens/{token}. \
             However, there is no mechanism to revoke a compromised Passkey credential from \
             the backend — revocation must occur at the authenticator level.",
            "Implement a passkey revocation endpoint that marks a passkey hash as revoked \
             and rejects future authentication attempts from that credential.",
        ),
        // CC7.1 — System monitoring
        check(
            "soc2.cc7_1_monitoring",
            "System Monitoring & Anomaly Detection (CC7.1)",
            "SOC2",
            ComplianceStatus::Pass,
            "Structured tracing (tracing_subscriber) and a /metrics endpoint are present. \
             Audit-log middleware records every API request with method, path, status code, \
             IP address, and user identity.",
            "",
        ),
        // CC7.2 — Incident response
        check(
            "soc2.cc7_2_incident_response",
            "Incident Response Procedures (CC7.2)",
            "SOC2",
            ComplianceStatus::Warning,
            "No documented incident-response runbook is present in the repository.",
            "Create an INCIDENT_RESPONSE.md document covering detection, containment, \
             eradication, recovery, and post-incident review steps. Link it from SECURITY.md.",
        ),
        // CC8.1 — Change management
        check(
            "soc2.cc8_1_change_management",
            "Change Management (CC8.1)",
            "SOC2",
            ComplianceStatus::Pass,
            "CI/CD pipeline (GitHub Actions) runs the full test suite on every pull request. \
             Deployment scripts include environment-specific confirmation prompts for mainnet.",
            "",
        ),
        // CC9.1 — Risk assessment
        check(
            "soc2.cc9_1_risk_assessment",
            "Risk Assessment (CC9.1)",
            "SOC2",
            ComplianceStatus::Pass,
            "A threat model and security policy document (docs/security.md and SECURITY.md) \
             are present, covering known attack vectors and disclosure procedures.",
            "",
        ),
        // A1.1 — Availability (CORS)
        check(
            "soc2.a1_1_availability_cors",
            "Availability — CORS Policy (A1.1)",
            "SOC2",
            if cors_origins_set {
                ComplianceStatus::Pass
            } else {
                ComplianceStatus::Warning
            },
            "ALLOWED_ORIGINS controls the CORS allow-list. A restrictive origin policy reduces \
             the risk of cross-origin data exfiltration.",
            if cors_origins_set {
                ""
            } else {
                "Set ALLOWED_ORIGINS to a comma-separated list of trusted frontend origins. \
                 Leaving it empty permits all origins in the current CorsLayer configuration."
            },
        ),
    ]
}

// ── ISO 27001 checks ──────────────────────────────────────────────────────────

fn iso27001_checks() -> Vec<ComplianceCheck> {
    vec![
        // A.9.1 — Access control policy
        check(
            "iso27001.a9_1_access_control_policy",
            "Access Control Policy (A.9.1)",
            "ISO27001",
            ComplianceStatus::Pass,
            "Admin endpoints are protected by bearer-token authentication enforced in \
             authorize_admin(). Vault-level actions are scoped to the owner's Stellar address.",
            "",
        ),
        // A.9.4 — System and application access control
        check(
            "iso27001.a9_4_app_access_control",
            "Application Access Control (A.9.4)",
            "ISO27001",
            ComplianceStatus::Pass,
            "Two-factor authentication (TOTP, SMS, Email) is available per vault via the \
             /api/vaults/{id}/2fa/* endpoints. Share tokens implement time-limited, \
             permission-scoped access delegation.",
            "",
        ),
        // A.10.1 — Cryptographic controls
        check(
            "iso27001.a10_1_cryptographic_controls",
            "Cryptographic Controls (A.10.1)",
            "ISO27001",
            ComplianceStatus::Pass,
            "Vault backups are AES-GCM encrypted before storage. Passkey authentication \
             relies on WebAuthn public-key cryptography. No plaintext secrets are persisted \
             in the database.",
            "",
        ),
        // A.12.1 — Operational procedures and responsibilities
        check(
            "iso27001.a12_1_operational_procedures",
            "Operational Procedures (A.12.1)",
            "ISO27001",
            ComplianceStatus::Warning,
            "Deployment scripts exist for testnet and mainnet, but no formal operating \
             procedure document covers routine tasks such as key rotation, database backups, \
             or on-call escalation.",
            "Create a docs/operations.md runbook covering key-rotation procedures, \
             backup schedules, restore drills, and on-call contacts.",
        ),
        // A.12.4 — Logging and monitoring
        check(
            "iso27001.a12_4_logging",
            "Logging & Monitoring (A.12.4)",
            "ISO27001",
            ComplianceStatus::Pass,
            "All API requests are logged to the audit store via audit_middleware. \
             Structured logs are emitted via the tracing crate. The /api/audit-logs endpoint \
             allows administrators to query the audit trail.",
            "",
        ),
        // A.12.6 — Management of technical vulnerabilities
        check(
            "iso27001.a12_6_vulnerability_management",
            "Technical Vulnerability Management (A.12.6)",
            "ISO27001",
            ComplianceStatus::Warning,
            "No automated dependency-vulnerability scanning (e.g. cargo-audit) is configured \
             in the CI pipeline.",
            "Add a `cargo audit` step to the CI workflow and configure Dependabot or \
             RenovateBot to open PRs for dependency updates.",
        ),
        // A.14.2 — Security in development and support processes
        check(
            "iso27001.a14_2_secure_development",
            "Security in Development (A.14.2)",
            "ISO27001",
            ComplianceStatus::Pass,
            "The CI pipeline runs `cargo test` and enforces code review via pull requests. \
             A SECURITY.md vulnerability-disclosure policy is present in the repository.",
            "",
        ),
        // A.16.1 — Management of information security incidents
        check(
            "iso27001.a16_1_incident_management",
            "Incident Management (A.16.1)",
            "ISO27001",
            ComplianceStatus::Warning,
            "SECURITY.md describes responsible disclosure but does not detail internal \
             incident classification, escalation paths, or SLA commitments.",
            "Expand SECURITY.md or create INCIDENT_RESPONSE.md with severity levels, \
             response-time SLAs, internal escalation matrix, and post-incident review \
             requirements.",
        ),
        // A.18.1 — Compliance with legal and contractual requirements
        check(
            "iso27001.a18_1_legal_compliance",
            "Legal & Contractual Compliance (A.18.1)",
            "ISO27001",
            ComplianceStatus::NotApplicable,
            "Specific legal or contractual obligations (HIPAA, PCI-DSS, etc.) have not been \
             identified for this deployment. Assessment should be revisited for each \
             production deployment region.",
            "",
        ),
    ]
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Execute all GDPR, SOC 2, and ISO 27001 compliance checks and return the
/// individual [`ComplianceCheck`] results.
///
/// Checks are evaluated against the current process environment and known
/// application configuration. No database queries are performed.
pub fn run_compliance_checks() -> Vec<ComplianceCheck> {
    let mut checks = Vec::new();
    checks.extend(gdpr_checks());
    checks.extend(soc2_checks());
    checks.extend(iso27001_checks());
    checks
}

/// Return a [`serde_json::Value`] placeholder where a real PDF report would be
/// embedded.
///
/// Generating a true PDF requires a rendering engine (e.g. `printpdf` or
/// `wkhtmltopdf`) that is deliberately excluded to avoid adding heavyweight
/// optional dependencies. Consumers may use the JSON report to render their own
/// PDF via any suitable client-side or server-side tool.
pub fn generate_pdf_stub() -> serde_json::Value {
    serde_json::json!({
        "format": "pdf",
        "version": "1.0",
        "note": "PDF generation requires an external renderer. \
                 Use the JSON report fields to produce a PDF via your preferred tooling.",
        "placeholder_base64": "",
    })
}

// ── Handler ───────────────────────────────────────────────────────────────────

/// `GET /admin/compliance-report`
///
/// Runs all compliance checks for the current deployment and returns a
/// structured [`ComplianceReportResponse`].
///
/// Requires a valid admin API key in the `Authorization: Bearer <key>` header
/// when `ADMIN_API_KEY` is set in the environment.
///
/// # Errors
///
/// - `401 Unauthorized` — missing or invalid admin key.
pub async fn compliance_report_handler(
    headers: HeaderMap,
    State(_state): State<Arc<AppState>>,
) -> Result<Json<ComplianceReportResponse>, AppError> {
    // Admin-only: validate the bearer token before serving the report.
    authorize_admin(&headers).map_err(|_api_err| {
        AppError::InvalidInput("valid admin API key required".to_string())
    })?;

    let checks = run_compliance_checks();

    let passed = checks
        .iter()
        .filter(|c| c.status == ComplianceStatus::Pass)
        .count();
    let failed = checks
        .iter()
        .filter(|c| c.status == ComplianceStatus::Fail)
        .count();
    let warnings = checks
        .iter()
        .filter(|c| c.status == ComplianceStatus::Warning)
        .count();
    let not_applicable = checks
        .iter()
        .filter(|c| c.status == ComplianceStatus::NotApplicable)
        .count();
    let total_checks = checks.len();

    let report = ComplianceAuditReport {
        generated_at: Utc::now(),
        total_checks,
        passed,
        failed,
        warnings,
        not_applicable,
        checks,
        pdf_stub: generate_pdf_stub(),
    };

    Ok(Json(ComplianceReportResponse { report }))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_compliance_checks_returns_nonempty_list() {
        let checks = run_compliance_checks();
        assert!(!checks.is_empty(), "expected at least one compliance check");
    }

    #[test]
    fn all_checks_have_non_empty_id_name_framework() {
        for c in run_compliance_checks() {
            assert!(!c.id.is_empty(), "check id must not be empty");
            assert!(!c.name.is_empty(), "check name must not be empty");
            assert!(!c.framework.is_empty(), "check framework must not be empty");
            assert!(!c.description.is_empty(), "check description must not be empty");
        }
    }

    #[test]
    fn pass_checks_have_empty_remediation() {
        for c in run_compliance_checks() {
            if c.status == ComplianceStatus::Pass {
                assert!(
                    c.remediation.is_empty(),
                    "passing check '{}' should have empty remediation",
                    c.id
                );
            }
        }
    }

    #[test]
    fn fail_and_warning_checks_have_remediation() {
        for c in run_compliance_checks() {
            if matches!(c.status, ComplianceStatus::Fail | ComplianceStatus::Warning) {
                assert!(
                    !c.remediation.is_empty(),
                    "non-passing check '{}' must provide remediation guidance",
                    c.id
                );
            }
        }
    }

    #[test]
    fn pdf_stub_has_expected_shape() {
        let stub = generate_pdf_stub();
        assert_eq!(stub["format"], "pdf");
        assert!(stub["note"].as_str().is_some());
    }

    #[test]
    fn compliance_status_display() {
        assert_eq!(ComplianceStatus::Pass.to_string(), "pass");
        assert_eq!(ComplianceStatus::Fail.to_string(), "fail");
        assert_eq!(ComplianceStatus::Warning.to_string(), "warning");
        assert_eq!(ComplianceStatus::NotApplicable.to_string(), "not_applicable");
    }

    #[test]
    fn frameworks_cover_gdpr_soc2_iso27001() {
        let checks = run_compliance_checks();
        let frameworks: std::collections::HashSet<&str> =
            checks.iter().map(|c| c.framework.as_str()).collect();
        assert!(frameworks.contains("GDPR"));
        assert!(frameworks.contains("SOC2"));
        assert!(frameworks.contains("ISO27001"));
    }
}

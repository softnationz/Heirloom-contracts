# Compliance Audit Reports

Task #99 — Compliance status tracking and audit report generation for Heirloom-Protocol.

## Overview

The compliance module (`backend/src/compliance.rs`) evaluates the current deployment against three regulatory and security frameworks:

- **GDPR** (EU General Data Protection Regulation)
- **SOC 2** (Service Organization Control 2 — Trust Service Criteria)
- **ISO 27001** (Information Security Management)

A single admin-gated endpoint returns a structured JSON report of all evaluated controls.

## Endpoint

```
GET /admin/compliance-report
Authorization: Bearer <ADMIN_API_KEY>
```

### Response

```json
{
  "report": {
    "generated_at": "2024-01-15T10:30:00Z",
    "total_checks": 24,
    "passed": 14,
    "failed": 2,
    "warnings": 7,
    "not_applicable": 1,
    "checks": [
      {
        "id": "gdpr.data_minimisation",
        "name": "Data Minimisation",
        "framework": "GDPR",
        "status": "pass",
        "description": "Vault data stored by the backend is limited to...",
        "remediation": ""
      }
    ],
    "pdf_stub": {
      "format": "pdf",
      "version": "1.0",
      "note": "PDF generation requires an external renderer...",
      "placeholder_base64": ""
    }
  }
}
```

### Status Codes

| Status | Meaning |
|--------|---------|
| `200 OK` | Report generated successfully |
| `401 Unauthorized` | Missing or invalid `ADMIN_API_KEY` |

## Check Status Values

| Status | Meaning |
|--------|---------|
| `pass` | Control is implemented and functioning correctly |
| `fail` | Control is missing or broken — remediation required |
| `warning` | Control is partially implemented or has a known gap |
| `not_applicable` | Control is not relevant to this deployment |

## GDPR Requirements

| Check ID | Control | Current Status |
|----------|---------|----------------|
| `gdpr.data_minimisation` | Data Minimisation (Art. 5) | Pass |
| `gdpr.purpose_limitation` | Purpose Limitation (Art. 5) | Pass |
| `gdpr.storage_limitation` | Storage Limitation (Art. 5) | Warning |
| `gdpr.transparency` | Transparency & Privacy Notice (Art. 13/14) | Warning |
| `gdpr.right_to_erasure` | Right to Erasure (Art. 17) | **Fail** |
| `gdpr.data_portability` | Data Portability (Art. 20) | Warning |
| `gdpr.privacy_by_design` | Privacy by Design (Art. 25) | Pass |
| `gdpr.security_of_processing` | Security of Processing (Art. 32) | Pass/Fail* |

*`gdpr.security_of_processing` passes only when `ADMIN_API_KEY`, `REMINDER_EMAIL_API_KEY`, and `REMINDER_SMS_API_KEY` are all set.

### Key GDPR Remediation Items

**Right to Erasure (Fail)**  
Implement `DELETE /api/users/{owner}/data` that pseudonymises or removes vault and audit-log records associated with an owner's Stellar address, subject to legal hold obligations.

**Storage Limitation (Warning)**  
Implement a configurable audit-log retention policy (e.g. rolling 12-month window) and schedule a periodic purge job.

**Transparency (Warning)**  
Add a `/privacy` endpoint or embed a privacy-notice URL in onboarding responses.

## SOC 2 Requirements

| Check ID | Control | Current Status |
|----------|---------|----------------|
| `soc2.cc6_1_access_controls` | Logical Access Controls (CC6.1) | Pass/Fail* |
| `soc2.cc6_2_credential_issuance` | Credential Issuance Controls (CC6.2) | Pass |
| `soc2.cc6_3_access_revocation` | Access Revocation (CC6.3) | Warning |
| `soc2.cc7_1_monitoring` | System Monitoring (CC7.1) | Pass |
| `soc2.cc7_2_incident_response` | Incident Response (CC7.2) | Warning |
| `soc2.cc8_1_change_management` | Change Management (CC8.1) | Pass |
| `soc2.cc9_1_risk_assessment` | Risk Assessment (CC9.1) | Pass |
| `soc2.a1_1_availability_cors` | Availability — CORS Policy (A1.1) | Pass/Warn* |

*`soc2.cc6_1_access_controls` requires `ADMIN_API_KEY` to be set.  
*`soc2.a1_1_availability_cors` requires `ALLOWED_ORIGINS` to be set.

### Key SOC 2 Remediation Items

**Access Revocation (Warning)**  
Implement a passkey revocation endpoint that marks a passkey hash as revoked and rejects future authentication attempts from that credential.

**Incident Response (Warning)**  
Create an `INCIDENT_RESPONSE.md` document covering detection, containment, eradication, recovery, and post-incident review steps.

## ISO 27001 Requirements

| Check ID | Control | Current Status |
|----------|---------|----------------|
| `iso27001.a9_1_access_control_policy` | Access Control Policy (A.9.1) | Pass |
| `iso27001.a9_4_app_access_control` | Application Access Control (A.9.4) | Pass |
| `iso27001.a10_1_cryptographic_controls` | Cryptographic Controls (A.10.1) | Pass |
| `iso27001.a12_1_operational_procedures` | Operational Procedures (A.12.1) | Warning |
| `iso27001.a12_4_logging` | Logging & Monitoring (A.12.4) | Pass |
| `iso27001.a12_6_vulnerability_management` | Vulnerability Management (A.12.6) | Warning |
| `iso27001.a14_2_secure_development` | Security in Development (A.14.2) | Pass |
| `iso27001.a16_1_incident_management` | Incident Management (A.16.1) | Warning |
| `iso27001.a18_1_legal_compliance` | Legal & Contractual Compliance (A.18.1) | Not Applicable |

### Key ISO 27001 Remediation Items

**Operational Procedures (Warning)**  
Create `docs/operations.md` covering key-rotation procedures, backup schedules, restore drills, and on-call contacts.

**Vulnerability Management (Warning)**  
Add a `cargo audit` step to the CI workflow and configure Dependabot or RenovateBot to open PRs for dependency updates.

**Incident Management (Warning)**  
Expand `SECURITY.md` or create `INCIDENT_RESPONSE.md` with severity levels, response-time SLAs, and post-incident review requirements.

## PDF Reports

The endpoint includes a `pdf_stub` field in every report response. True PDF generation requires an external rendering engine (e.g. `printpdf`, `wkhtmltopdf`, or a headless browser). The JSON report fields contain all the data needed to render a PDF via any suitable client-side or server-side tooling.

## Environment Variables

The compliance check for `gdpr.security_of_processing` and `soc2.cc6_1_access_controls` is evaluated dynamically at request time against the following environment variables:

| Variable | Purpose |
|----------|---------|
| `ADMIN_API_KEY` | Gates all `/admin/*` endpoints |
| `REMINDER_EMAIL_API_KEY` | Authenticates the email notification provider |
| `REMINDER_SMS_API_KEY` | Authenticates the SMS notification provider |
| `ALLOWED_ORIGINS` | CORS allow-list for the API |

Set all four variables in production deployments to achieve maximum compliance coverage.

## Running Compliance Checks Programmatically

The core check functions are exposed as public Rust APIs:

```rust
use heirloom_protocol_backend::compliance::{run_compliance_checks, generate_pdf_stub};

// Run all GDPR, SOC 2, and ISO 27001 checks
let checks = run_compliance_checks();

// Filter to failing checks only
let failures: Vec<_> = checks.iter()
    .filter(|c| c.status == ComplianceStatus::Fail)
    .collect();

// Get PDF stub metadata
let pdf = generate_pdf_stub();
```

## Related Documentation

- [Threat Model & Security](security.md)
- [Security Policy & Vulnerability Disclosure](../SECURITY.md)
- [Secret Scanning](secret-scanning.md)
- [API Reference](api-reference.md)

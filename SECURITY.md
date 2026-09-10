# Security Policy

## Overview

The Heirloom-Protocol smart contract handles real XLM (Stellar Lumens) and implements time-locked vaults with beneficiary distributions. Security is critical to protect user funds and ensure the integrity of the contract.

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| Latest  | :white_check_mark: |
| < Latest| :x:                |

Only the latest version of the contract is supported. Users should always deploy the most recent audited version.

## Reporting a Vulnerability

We take security vulnerabilities seriously. If you discover a security vulnerability, please follow responsible disclosure practices.

### How to Report

**DO NOT** open a public GitHub issue for security vulnerabilities.

Instead, please report vulnerabilities via one of the following methods:

1. **Email**: Send details to [security@heirloom-protocol.example.com](mailto:security@heirloom-protocol.example.com)
2. **Encrypted Communication**: Use our PGP key available at [https://heirloom-protocol.example.com/security-key.asc](https://heirloom-protocol.example.com/security-key.asc)

### What to Include

When reporting a vulnerability, please include:

- Description of the vulnerability
- Steps to reproduce the issue
- Potential impact on user funds
- Suggested fix (if available)
- Your contact information for follow-up

### Response Timeline

- **Initial Response**: Within 48 hours of receipt
- **Status Update**: Within 7 days
- **Fix Timeline**: Depends on severity, typically 14-30 days for critical issues

### Severity Levels

- **Critical**: Immediate fund loss or unauthorized access to user funds
- **High**: Potential fund loss or contract manipulation
- **Medium**: Contract functionality issues without direct fund risk
- **Low**: Minor issues or improvements

## Security Measures

### Pre-Mainnet Audit Requirement

**IMPORTANT**: Before deploying to mainnet, the contract MUST undergo a comprehensive security audit by a reputable third-party auditor. No mainnet deployment should occur without:

1. Complete code review
2. Formal security audit report
3. Resolution of all critical and high-severity findings
4. Community review period

### Current Security Features

- **Authorization Checks**: All sensitive operations require proper authentication
- **Error Handling**: Structured error codes for reliable client-side error handling
- **Time-Lock Mechanism**: Funds are locked until specified conditions are met
- **Beneficiary Protection**: Multiple beneficiaries with BPS-based distribution
- **Pause Functionality**: Admin can pause contract in emergency situations

### Best Practices for Users

1. **Verify Contract Address**: Always confirm you're interacting with the official contract
2. **Check Parameters**: Double-check all transaction parameters before signing
3. **Monitor Expiry**: Use `ping_expiry` to monitor vault TTL status
4. **Use View Functions**: Check vault status before executing state-changing operations

## Bug Bounty Program

We are considering implementing a bug bounty program for security researchers. Details will be announced once the program is established.

## Security Updates

Security updates and announcements will be published through:

- GitHub Security Advisories
- Official project communication channels
- Contract upgrade notifications (when applicable)

## Contact

For general security questions or concerns: [security@heirloom-protocol.example.com](mailto:security@heirloom-protocol.example.com)

## Acknowledgments

We thank the security research community for their contributions to keeping this project secure.

---

**Last Updated**: 2026-03-27

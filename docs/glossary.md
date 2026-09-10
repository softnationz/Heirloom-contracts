# Glossary and Terminology Guide

This glossary defines terms used throughout Heirloom-Protocol documentation, code, and communications. Entries include definitions, context, cross-references, and pronunciation guidance where applicable.

## How to Use This Glossary

- Terms are listed alphabetically within their category.
- **Bold** text inside a definition indicates another glossary term.
- Cross-references link to the most relevant documentation section.
- Pronunciation is given for technical terms that are frequently mispronounced.

## Table of Contents

- [Core Protocol Terms](#core-protocol-terms)
- [Stellar / Soroban Terms](#stellar--soroban-terms)
- [Smart Contract Terms](#smart-contract-terms)
- [Authentication Terms](#authentication-terms)
- [Backend & Infrastructure Terms](#backend--infrastructure-terms)
- [Financial & Vault Terms](#financial--vault-terms)
- [Operational Terms](#operational-terms)
- [Acronyms & Abbreviations](#acronyms--abbreviations)

---

## Core Protocol Terms

### Check-In

**Definition**: An on-chain action performed by the **Vault Owner** to confirm they are alive and active. A check-in resets the **TTL** countdown, preventing the vault from expiring.

**Usage**: "The owner must check in every 90 days or the vault will release funds to the beneficiary."

**Context**: Check-ins are authenticated via **Passkey** (WebAuthn). The contract records `last_check_in` timestamp and extends the TTL on each successful check-in.

**Related**: [TTL & State Archival Logic](ttl-logic.md), [UC-01: Personal Inheritance Vault](use-cases.md#uc-01-personal-inheritance-vault)

---

### Dead Man's Switch

**Definition**: A mechanism that activates automatically when a person fails to perform a periodic action (proving they are alive). In Heirloom-Protocol, the switch fires when a **Vault Owner** stops **checking in**, releasing vault funds to the **Beneficiary**.

**Pronunciation**: *ded man's switch*

**Usage**: "The vault acts as a dead man's switch — if the owner doesn't check in, funds are released automatically."

**Context**: The term originates from railway safety systems where a train's brakes would engage if the operator released a foot pedal. Heirloom-Protocol adapts this concept for digital asset inheritance.

---

### Beneficiary

**Definition**: The Stellar address designated to receive vault funds when the **TTL** expires and a **Trigger Release** is executed.

**Usage**: "The owner named their spouse as the vault beneficiary."

**Context**: A beneficiary may be required to **accept** the role (see **Conditional Acceptance**) and can delegate to another address via **Beneficiary Delegation**. Conflicts between multiple claimants are resolved by the **Conflict Resolution** mechanism.

**Related**: [Beneficiary Conditional Acceptance](beneficiary-conditional-acceptance.md), [Beneficiary Conflict Resolution](beneficiary-conflict-resolution.md), [Beneficiary Advanced Features](beneficiary-advanced-features.md)

---

### Vault

**Definition**: A smart contract instance that holds **XLM** (or tokenized assets) on behalf of a **Vault Owner**, with a designated **Beneficiary** and a **TTL**-based release mechanism.

**Usage**: "The owner created a vault with a 90-day check-in interval."

**Context**: Each vault has a unique `vault_id` (a `u64` counter). Vault state is stored in **Persistent Storage** and subject to **State Archival** if the TTL expires.

**Related**: [Architecture Overview](architecture.md)

---

### Vault Owner

**Definition**: The Stellar address that created and controls a **Vault**. The owner can deposit funds, check in, withdraw, update the beneficiary, and enter/exit **Hibernation**.

**Usage**: "Only the vault owner can call `check_in()` or `withdraw()`."

---

### Trigger Release

**Definition**: The on-chain action that transfers vault funds to the **Beneficiary** after the **TTL** has expired. Can be called by anyone once the vault is expired.

**Usage**: "After the owner failed to check in for 90 days, anyone was able to trigger release."

**Context**: `trigger_release` first attempts to restore an **Archived Vault** before executing the transfer. It checks `is_expired()` before acting.

**Related**: [TTL & State Archival Logic](ttl-logic.md)

---

## Stellar / Soroban Terms

### Ledger

**Definition**: The global state of the Stellar network at a given point in time. Each **Ledger Close** advances the ledger sequence and timestamps.

**Pronunciation**: *lej-er*

**Usage**: "The contract reads `env.ledger().timestamp()` to determine the current time."

---

### Ledger Entry

**Definition**: A unit of data stored on the Stellar network. Soroban contracts store state as ledger entries, which are subject to **TTL** and **State Archival**.

---

### Soroban

**Definition**: The smart contract platform built on the Stellar network. Heirloom-Protocol's core vault logic is implemented as Soroban smart contracts written in Rust.

**Pronunciation**: *so-ROH-ban*

**Usage**: "The ttl_vault contract is deployed to the Soroban environment."

**Related**: [Stellar Developer Documentation](https://developers.stellar.org/docs/smart-contracts)

---

### State Archival

**Definition**: A Stellar/Soroban mechanism where **Ledger Entries** that haven't had their **TTL** extended are moved to an archived (dormant) state to reduce active ledger size. Archived entries can be restored.

**Usage**: "If the owner stops all activity, the vault's persistent storage entry will eventually be archived."

**Context**: State archival does not delete data — archived entries remain recoverable. Heirloom-Protocol extends TTL on vault creation, check-ins, deposits, and withdrawals to prevent archival.

**Related**: [TTL & State Archival Logic](ttl-logic.md)

---

### Strkey

**Definition**: The standard encoding format for Stellar addresses, contract IDs, and other identifiers. Strkeys are Base32-encoded with a checksum prefix.

**Pronunciation**: *str-key*

**Usage**: "Contract IDs are 56-character Strkeys starting with `C`."

---

### Stroops

**Definition**: The smallest unit of XLM. 1 XLM = 10,000,000 stroops (10^7 stroops).

**Pronunciation**: *stroops*

**Usage**: "A deposit of 100 XLM equals `1_000_000_000` stroops in contract parameters."

**Context**: All balance values in Heirloom-Protocol smart contracts use stroops (`i128`).

---

### TTL (Time to Live)

**Definition**: In the Soroban context, TTL refers to the number of **ledger** seconds a stored entry remains in the active (non-archived) state before being subject to **State Archival**. In Heirloom-Protocol, TTL also informally refers to the countdown before a **Vault** expires and funds are released.

**Pronunciation**: *T-T-L* (spelled out)

**Context**: The two meanings are related but distinct:
- **Soroban TTL**: Storage-level entry lifetime managed by the network.
- **Vault TTL**: Application-level countdown (`last_check_in + check_in_interval - current_time`).

**Related**: [TTL & State Archival Logic](ttl-logic.md)

---

### XLM

**Definition**: The native cryptocurrency of the Stellar network, used for transaction fees and as the primary asset type in Heirloom-Protocol vaults.

**Pronunciation**: *X-L-M* (spelled out) or *Lumens*

**Full Name**: Stellar Lumens

---

## Smart Contract Terms

### Accelerated TTL Decay

**Definition**: A function that allows a **Vault Owner** to voluntarily shorten their vault's remaining TTL, moving the expiry deadline forward.

**Usage**: "The owner used accelerated TTL decay to trigger inheritance sooner than the scheduled interval."

**Related**: [TTL & State Archival Logic](ttl-logic.md)

---

### Archived Vault

**Definition**: A vault whose **Persistent Storage** entry has been moved to the **State Archival** tier due to inactivity. The vault can be restored by extending its TTL.

**Related**: [TTL & State Archival Logic](ttl-logic.md)

---

### Check-In Cooldown

**Definition**: A minimum enforced delay between consecutive **Check-Ins** on the same vault, preventing storage abuse from excessive rapid check-ins.

**Usage**: "With a 60-second cooldown, the owner cannot check in more than once per minute."

**Related**: [TTL & State Archival Logic](ttl-logic.md), error code `CheckInTooFrequent` (54)

---

### ContractError

**Definition**: The enumerated error type returned by Heirloom-Protocol smart contract functions when an operation fails. Each error has a unique numeric code.

**Usage**: "The function returned `ContractError::NotExpired` (2) because the TTL had not elapsed."

**Context**: See `contracts/ttl_vault/src/` for the full error enum definition.

---

### DataKey

**Definition**: An enum used as the key for **Persistent Storage** lookups within the Soroban contract. Examples: `DataKey::Vault(vault_id)`, `DataKey::ArchivedVault(vault_id)`.

**Usage**: "Vault data is stored under `DataKey::Vault(vault_id)`."

---

### Hibernation

**Definition**: A vault state that pauses the **TTL** countdown. An owner enters hibernation before a planned absence (travel, medical procedure) to prevent accidental expiry.

**Usage**: "The owner hibernated the vault for 30 days while traveling."

**Related**: [Vault Hibernation](hibernation.md)

---

### Persistent Storage

**Definition**: Soroban's long-lived on-chain storage tier. Entries in persistent storage survive across ledgers but are subject to **State Archival** if their **TTL** is not extended.

**Contrast with**: Temporary storage (discarded at ledger close) and instance storage (tied to contract instance TTL).

---

### TTL Borrowing

**Definition**: A mechanism allowing a **Vault Owner** to temporarily transfer **TTL** from one of their vaults (the lender) to another (the borrower), extending the borrower's expiry at the cost of shortening the lender's.

**Usage**: "The owner borrowed 7 days of TTL from their savings vault to prevent their inheritance vault from expiring."

**Related**: [TTL & State Archival Logic](ttl-logic.md)

---

### WASM

**Definition**: WebAssembly. The binary format in which Soroban smart contracts are compiled and deployed to the Stellar network.

**Pronunciation**: *waz-um*

**Context**: The compiled WASM binary size affects deployment costs. See [WASM Size Budget](wasm-size-budget.md).

---

## Authentication Terms

### Passkey

**Definition**: A cryptographic credential based on the **WebAuthn** standard, stored on a user's device (phone, laptop, security key). Used in Heirloom-Protocol to authenticate vault owner actions without exposing a seed phrase.

**Usage**: "The owner authenticated the check-in using a Passkey stored on their phone."

**Related**: [Passkey Integration](passkeys.md)

---

### Passkey Hash

**Definition**: A hash of the **Passkey** public key or credential ID, stored on-chain to associate a Passkey with a vault for future authentication challenges.

---

### Relying Party (RP)

**Definition**: In the **WebAuthn** specification, the Relying Party is the server-side application that verifies Passkey credentials. In Heirloom-Protocol, the backend acts as the RP.

**Abbreviation**: RP

**Config**: `PASSKEY_RP_ID`, `PASSKEY_RP_ORIGIN`

**Related**: [Passkey Integration](passkeys.md), [Configuration Reference](configuration-reference.md)

---

### WebAuthn

**Definition**: A W3C web standard for Passkey-based authentication. Heirloom-Protocol uses WebAuthn as the sole authentication mechanism for vault operations, eliminating the need for seed phrases.

**Pronunciation**: *web-AW-then*

**Full Name**: Web Authentication API

**Related**: [Passkey Integration](passkeys.md)

---

## Backend & Infrastructure Terms

### Scheduler

**Definition**: A background process in the Heirloom-Protocol backend that periodically checks vault TTL status and dispatches **Reminder Notifications** to owners approaching their check-in deadline.

**Related**: [Monitoring Guide](monitoring-guide.md)

---

### Reminder Notification

**Definition**: An automated email or SMS alert sent to a **Vault Owner** when their check-in deadline is approaching, giving them time to check in before funds are released.

**Related**: [Configuration Reference — Notification Services](configuration-reference.md#notification-services)

---

### Webhook

**Definition**: An HTTP callback triggered by the backend when a specific vault event occurs (e.g., TTL expiry, trigger release). Used by integrators to react to vault state changes.

**Usage**: "Register a webhook to be notified when a vault's trigger_release is executed."

---

### SBT (Soulbound Token)

**Definition**: A non-transferable token on Stellar that represents a persistent, identity-linked credential. Heirloom-Protocol uses SBTs for beneficiary proof-of-life and verification.

**Pronunciation**: *S-B-T* (spelled out)

**Related**: [SBT Documentation](sbt.md), [Beneficiary Proof of Life and Voting](beneficiary-proof-of-life-and-voting.md)

---

## Financial & Vault Terms

### Beneficiary Conflict Resolution

**Definition**: The automated on-chain mechanism for adjudicating disputes when multiple parties claim the role of **Beneficiary** for the same vault.

**Related**: [Beneficiary Conflict Resolution](beneficiary-conflict-resolution.md)

---

### Beneficiary Delegation

**Definition**: The ability for a **Beneficiary** to designate another Stellar address to act on their behalf, creating a chain of custody for vault release.

**Related**: [TTL & State Archival Logic](ttl-logic.md)

---

### Conditional Acceptance

**Definition**: A feature where a **Beneficiary** only accepts the beneficiary role if the vault balance meets a configured minimum threshold. Protects beneficiaries from inheriting vaults with negligible funds.

**Related**: [Beneficiary Conditional Acceptance](beneficiary-conditional-acceptance.md)

---

### Vesting Schedule

**Definition**: A time-based release plan that distributes vault funds to a **Beneficiary** gradually over a period (e.g., monthly over 2 years) rather than in a single lump sum.

**Related**: [Vesting Schedules](vesting-schedules.md)

---

## Operational Terms

### Deployment Identity

**Definition**: The Stellar CLI key pair used to sign and broadcast smart contract deployment transactions. Should be a dedicated, securely stored key separate from runtime keys.

**Usage**: "Generate a dedicated deployer identity before deploying to mainnet."

**Related**: [Deployment Guide](deployment-guide.md)

---

### Disaster Recovery

**Definition**: A set of procedures for restoring normal operations after a major failure (data loss, contract bug, infrastructure outage).

**Related**: [Disaster Recovery Runbook](disaster-recovery-runbook.md)

---

### Futurenet

**Definition**: A pre-release Stellar test network used to evaluate upcoming protocol changes before they reach **Testnet** or **Mainnet**.

**Usage**: "Deploy to futurenet to test compatibility with upcoming Soroban changes."

---

### Mainnet

**Definition**: The production Stellar network where real funds are at stake. All mainnet deployments require careful verification and explicit confirmation.

**Contrast with**: **Testnet**, **Futurenet**, **Standalone**

---

### Standalone

**Definition**: A local, isolated Stellar network instance used for rapid development and testing without network fees or external dependencies. Started via Docker Compose in this project.

**Related**: [Configuration Reference — Network Configurations](configuration-reference.md#network-configurations-environmentstoml)

---

### Testnet

**Definition**: Stellar's public test network. Uses test XLM (no real value) and mirrors mainnet behavior. All contracts should be validated on testnet before mainnet deployment.

---

## Acronyms & Abbreviations

| Acronym | Full Form | Description |
|---|---|---|
| **API** | Application Programming Interface | Interface for programmatic interaction with a service |
| **CI** | Continuous Integration | Automated build and test pipeline (GitHub Actions) |
| **CORS** | Cross-Origin Resource Sharing | HTTP security policy for cross-domain requests |
| **HMAC** | Hash-based Message Authentication Code | Cryptographic signature for message integrity |
| **JWT** | JSON Web Token | Compact token format for authenticated sessions |
| **RPC** | Remote Procedure Call | Protocol for invoking smart contract functions |
| **RP** | Relying Party | WebAuthn server-side authenticator |
| **SBT** | Soulbound Token | Non-transferable on-chain credential |
| **SDK** | Software Development Kit | Library for interacting with a platform (e.g., Stellar SDK) |
| **TOTP** | Time-based One-Time Password | 2FA method using time-based codes |
| **TTL** | Time to Live | Duration before an entry expires or is archived |
| **WASM** | WebAssembly | Binary instruction format for smart contracts |
| **WebAuthn** | Web Authentication API | W3C standard for Passkey authentication |
| **XLM** | Stellar Lumens | Native Stellar cryptocurrency (also: Lumens) |
| **ZK** | Zero-Knowledge | Cryptographic proof system (used in `zk_verifier`) |

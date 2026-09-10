# Interactive Playground

The Heirloom-Protocol Interactive Playground is a web-based environment that lets you experiment with vault operations against a live testnet (or a local Stellar Quickstart node) without any local toolchain setup. It lowers the barrier to learning by providing pre-configured scenarios, a built-in editor, and one-click execution in your browser.

## Table of Contents

- [Overview](#overview)
- [Accessing the Playground](#accessing-the-playground)
- [Quick Start](#quick-start)
- [Pre-configured Scenarios](#pre-configured-scenarios)
- [Scenario Reference](#scenario-reference)
- [Playground Interface](#playground-interface)
- [Execution Modes](#execution-modes)
- [Tutorial Integration](#tutorial-integration)
- [Advanced Usage](#advanced-usage)
- [Limitations](#limitations)
- [Local Playground Setup](#local-playground-setup)
- [Troubleshooting](#troubleshooting)

---

## Overview

The Playground provides:

- **In-browser execution** — invoke contract functions directly against testnet or a local Stellar Quickstart node; no local Rust or Stellar CLI installation required.
- **Pre-configured scenarios** — ready-to-run examples covering every key workflow (vault creation, check-in, release, beneficiary management, passkeys).
- **Live state inspection** — query vault state and see real-time results after each operation.
- **Tutorial integration** — each tutorial in [docs/video-tutorials.md](video-tutorials.md) links to the corresponding playground scenario.
- **Editable parameters** — modify any parameter inline and re-run without leaving the page.

The Playground runs exclusively against **testnet** by default. No real funds are at risk.

---

## Accessing the Playground

### Hosted Version

The hosted Playground is served from the backend server at:

```
http://localhost:3000/playground
```

when running the local Docker stack (`docker-compose up -d`). For a deployed instance, access it at the equivalent path on your deployment URL.

The self-contained HTML file is located at:

```
backend/simulator.html
```

You can open it directly in any modern browser for a fully offline experience with a local Stellar Quickstart node.

### Requirements

- A modern browser (Chrome 90+, Firefox 88+, Safari 15+, Edge 90+)
- For testnet scenarios: an internet connection
- For local scenarios: the Docker stack running (`docker-compose up -d`)

No wallet extension, CLI, or local SDK installation is required.

---

## Quick Start

1. **Open the Playground** — navigate to `http://localhost:3000/playground` or open `backend/simulator.html` directly.
2. **Select a scenario** — choose "Scenario 1: Create and Fund a Vault" from the scenario list.
3. **Review parameters** — the scenario pre-fills sensible defaults (testnet, 30-day TTL, sample beneficiary address).
4. **Click Run** — the Playground submits the transaction and shows the result inline.
5. **Inspect state** — click "Query Vault State" to see the vault's current TTL and balance.
6. **Advance the scenario** — click "Next Step" to proceed to the check-in step.

Each step shows the equivalent CLI command so you can reproduce it locally once you are ready.

---

## Pre-configured Scenarios

| # | Scenario Name | Key Operations Covered |
|---|---|---|
| 1 | Create and Fund a Vault | `create_vault`, `deposit` |
| 2 | Check-In and TTL Extension | `check_in`, `get_ttl_remaining` |
| 3 | Vault Release (Expired TTL) | `trigger_release`, `get_release_status` |
| 4 | Beneficiary Conditional Acceptance | `update_beneficiary`, `trigger_release` with threshold |
| 5 | Withdrawal Lifecycle | `withdraw`, audit trail, dispute |
| 6 | Passkey Registration and Biometric Check-In | `bind_passkey_biometric`, `biometric_check_in` |
| 7 | Vault Hibernation | `enter_hibernation`, `exit_hibernation`, `get_hibernation` |
| 8 | TTL Borrowing Between Vaults | `borrow_ttl`, `repay_ttl_borrow` |
| 9 | Beneficiary Conflict Resolution | Multi-claim scenario, ranking resolution |
| 10 | Disaster Recovery: Restoring an Archived Vault | `restore_vault`, archived state inspection |

---

## Scenario Reference

### Scenario 1: Create and Fund a Vault

**Tutorial link**: [T-201 · Creating Your First Vault](video-tutorials.md#t-201--creating-your-first-vault-issuance)

**Steps**:

1. Fill in `beneficiary` — a testnet Stellar address (a pre-funded test address is provided by default).
2. Set `check_in_interval` — default is `2592000` (30 days in seconds).
3. Run `create_vault` — the response shows the new `vault_id`.
4. Fill in `amount` in stroops (default: `100000000` = 10 XLM).
5. Run `deposit(vault_id, amount)`.
6. Run `get_vault(vault_id)` to confirm the vault is active.

**What you will observe**:
- A `vault_id` returned from `create_vault`.
- `get_vault` shows `balance`, `last_check_in`, and `check_in_interval`.

**Equivalent CLI**:
```bash
stellar contract invoke \
  --id $CONTRACT_TTL_VAULT \
  --network testnet \
  --source deployer \
  -- create_vault \
  --beneficiary GBENEFI...CIARY \
  --check_in_interval 2592000
```

---

### Scenario 2: Check-In and TTL Extension

**Tutorial link**: [T-202 · Performing a Check-In](video-tutorials.md#t-202--performing-a-check-in-attestation)

**Steps**:

1. Use the `vault_id` from Scenario 1 (or enter a known vault ID).
2. Run `get_ttl_remaining(vault_id)` — note the current remaining seconds.
3. Run `check_in(vault_id)`.
4. Run `get_ttl_remaining(vault_id)` again — observe the TTL reset.

**What you will observe**:
- TTL resets to the full `check_in_interval` after check-in.

**Common error**: `CheckInTooFrequent` (error 54) — wait 60 seconds between check-ins.

---

### Scenario 3: Vault Release (Expired TTL)

**Tutorial link**: [T-203 · Triggering Vault Release](video-tutorials.md#t-203--triggering-vault-release-verification)

> This scenario uses a vault with a very short TTL (5 seconds) pre-configured for demo purposes.

**Steps**:

1. The Playground creates a demo vault with `check_in_interval = 5`.
2. Wait 6 seconds.
3. Run `is_expired(vault_id)` — should return `true`.
4. Run `trigger_release(vault_id)`.
5. Run `get_release_status(vault_id)` — should return `Released`.

**What you will observe**:
- Fund transfer to the beneficiary address visible in the Stellar Testnet Explorer link provided.

---

### Scenario 4: Beneficiary Conditional Acceptance

**Tutorial link**: [T-301 · Conditional Acceptance](video-tutorials.md#t-301--conditional-acceptance-and-minimum-thresholds)

**Steps**:

1. Create a vault with a minimum acceptance threshold set.
2. Attempt `trigger_release` with vault balance below threshold — observe rejection.
3. Deposit enough to exceed the threshold.
4. Trigger release — observe acceptance.

---

### Scenario 5: Withdrawal Lifecycle

**Tutorial link**: [T-204 · Withdrawing Funds](video-tutorials.md#t-204--withdrawing-funds)

**Steps**:

1. Create and fund a vault.
2. Run `withdraw(vault_id, amount)`.
3. Inspect the withdrawal audit trail.
4. Simulate an unauthorized withdrawal and open a dispute within the 24-hour window.

---

### Scenario 6: Passkey Registration and Biometric Check-In

**Tutorial link**: [T-401 · Passkey Setup and Biometric Check-In](video-tutorials.md#t-401--passkey-setup-and-biometric-check-in)

**Steps**:

1. Create a vault.
2. Register a biometric credential using `bind_passkey_biometric` with a sample `credential_hash`.
3. Perform `biometric_check_in` with the credential hash.
4. Inspect `get_vault_biometrics` to see the registered entries.

---

### Scenario 7: Vault Hibernation

**Written reference**: [docs/hibernation.md](hibernation.md)

**Steps**:

1. Create a vault.
2. Call `enter_hibernation(vault_id, caller, duration_seconds)`.
3. Call `get_hibernation(vault_id)` — observe the hibernation entry.
4. Call `exit_hibernation(vault_id, caller)`.
5. Run `get_ttl_remaining` — confirm TTL is normal after exit.

---

### Scenario 8: TTL Borrowing Between Vaults

**Written reference**: [docs/ttl-logic.md](ttl-logic.md#ttl-borrowing-emergency)

**Steps**:

1. Create two vaults: a "lender" and a "borrower".
2. Call `borrow_ttl(borrower_vault_id, lender_vault_id, caller, 86400)` (borrow 1 day).
3. Inspect TTL on both vaults using `get_ttl_remaining`.
4. Call `repay_ttl_borrow(borrower_vault_id, caller)`.
5. Confirm lender TTL is restored.

---

### Scenario 9: Beneficiary Conflict Resolution

**Written reference**: [docs/beneficiary-conflict-resolution.md](beneficiary-conflict-resolution.md)

**Steps**:

1. Create a vault with multiple candidate beneficiary addresses.
2. Expire the TTL (short TTL demo vault).
3. Submit conflicting claims from multiple addresses.
4. Observe the ranking algorithm select the winning beneficiary.
5. Call `trigger_release` — observe funds going to the resolved beneficiary.

---

### Scenario 10: Disaster Recovery — Restoring an Archived Vault

**Written reference**: [docs/ttl-logic.md](ttl-logic.md#vault-archival-and-restoration), [docs/disaster-recovery-runbook.md](disaster-recovery-runbook.md)

**Steps**:

1. The Playground simulates an archived vault state.
2. Call `get_archived_vault_info(vault_id)` — observe the archived snapshot.
3. Call `restore_vault(vault_id)` — TTL is re-extended.
4. Call `get_vault(vault_id)` — confirm the vault is accessible again.

---

## Playground Interface

### Layout

```
┌─────────────────────────────────────────────────────────────┐
│ Scenario Selector        │  Network: [ Testnet ▼ ]          │
├──────────────────────────┴──────────────────────────────────┤
│ PARAMETERS                      │ OUTPUT                     │
│  vault_id:  [         ]         │  {                         │
│  amount:    [         ]         │    "vault_id": 1,          │
│  beneficiary: [       ]         │    "balance": 100000000,   │
│                                 │    "last_check_in": ...    │
│  [▶ Run]  [↺ Reset]             │  }                         │
├─────────────────────────────────┴──────────────────────────-┤
│ EQUIVALENT CLI COMMAND                                       │
│  stellar contract invoke --id $CONTRACT ...                  │
└─────────────────────────────────────────────────────────────┘
```

### Controls

| Control | Description |
|---|---|
| **Scenario Selector** | Dropdown to switch between the 10 pre-configured scenarios |
| **Network Selector** | Switch between Testnet and Local (Quickstart) |
| **Parameters Panel** | Editable fields for all function arguments |
| **Run** | Execute the current function with the given parameters |
| **Reset** | Restore default parameter values for the current scenario |
| **Next Step** | Advance to the next step in a multi-step scenario |
| **Output Panel** | Displays the JSON response or error from the last operation |
| **CLI Command** | Shows the equivalent `stellar contract invoke` command |
| **History** | Collapsible list of all previous operations in the session |

---

## Execution Modes

### Testnet Mode (Default)

All operations execute against `soroban-testnet.stellar.org`. Transactions are real but use testnet XLM with no monetary value. Results are visible on the [Stellar Testnet Explorer](https://testnet.stellar.expert).

### Local Mode

Targets a local Stellar Quickstart node at `http://localhost:8000`. Requires:

```bash
docker-compose up -d
```

Local mode is completely isolated — ideal for rapid iteration and scenarios that require repeatedly resetting state.

### Read-Only Mode

Available via the "Inspect" tab. Lets you query vault state (`get_vault`, `get_ttl_remaining`, `is_expired`, `get_release_status`) without submitting any transactions. No signing required.

---

## Tutorial Integration

Each entry in [docs/video-tutorials.md](video-tutorials.md) includes a link to the corresponding playground scenario. The tutorial video demonstrates the scenario step-by-step, and the Playground lets you follow along interactively at your own pace.

Cross-reference table:

| Tutorial | Playground Scenario |
|---|---|
| T-201: Creating a Vault | Scenario 1 |
| T-202: Check-In | Scenario 2 |
| T-203: Triggering Release | Scenario 3 |
| T-204: Withdrawals | Scenario 5 |
| T-301: Conditional Acceptance | Scenario 4 |
| T-302: Conflict Resolution | Scenario 9 |
| T-401: Passkeys and Biometrics | Scenario 6 |
| T-603: Disaster Recovery | Scenario 10 |

---

## Advanced Usage

### Chaining Operations Across Scenarios

You can copy the `vault_id` from one scenario's output and paste it into another scenario's parameter field to chain operations (e.g., create in Scenario 1, then test hibernation from Scenario 7 on the same vault).

### Editing Parameters Inline

All parameter fields are fully editable. You can override any default to explore edge cases:
- Set `check_in_interval = 5` to test expiry quickly.
- Set `amount = 0` to verify the contract rejects zero-amount deposits.
- Use a known passkey hash to test expiry and compromise scenarios.

### Viewing On-Chain Events

The output panel includes an "Events" tab that displays all contract events emitted by the transaction: `check_in`, `pk_expd`, `pk_comp`, `del_ben`, `ttl_bor`, etc.

### Exporting Session History

Use the "Export" button (top-right) to download a JSON file of your session's operations and outputs. Useful for sharing reproduction steps when filing an issue.

---

## Limitations

| Limitation | Detail |
|---|---|
| Testnet only for hosted version | No mainnet operations to protect real funds |
| No wallet signing | Uses a pre-funded demo account; production vaults require your own identity |
| No persistent state between sessions | Session history clears on page reload |
| Passkey simulation only | Full WebAuthn signing requires a native app (planned for v2.0) |
| Rate limits on testnet RPC | Excessive Playground use may hit public RPC rate limits; use local mode for load testing |

---

## Local Playground Setup

The Playground is included in the repository and requires no additional installation beyond the standard Docker stack.

### Starting the Stack

```bash
cp .env.example .env
# Edit .env: set CONTRACT_TTL_VAULT to your deployed contract address
docker-compose up -d
```

### Opening the Playground

Navigate to:
```
http://localhost:3000/playground
```

Or open the file directly:
```
backend/simulator.html
```

### Connecting to Your Own Contract

Set `CONTRACT_TTL_VAULT` in your `.env` to point to your deployed contract:

```env
CONTRACT_TTL_VAULT=CXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX
```

The Playground reads this value via the backend API and uses it for all scenario invocations.

### Using a Custom RPC Endpoint

For private testnet clusters or a custom Stellar Quickstart configuration, update:

```env
STELLAR_RPC_URL=http://localhost:8000/soroban/rpc
```

The Playground's network selector will reflect this endpoint in local mode.

---

## Troubleshooting

### "Failed to fetch" errors

The Playground cannot reach the Stellar RPC. Check:
1. Docker stack is running: `docker-compose ps`
2. Your internet connection (for testnet mode)
3. The `STELLAR_RPC_URL` in `.env` is correct

### "ContractError: NotExpired" in Scenario 3

The demo vault TTL may not have elapsed yet. Wait the remaining seconds shown in the output, then re-run `trigger_release`.

### Parameters reset unexpectedly

Parameters reset when you switch scenarios. Copy any custom values before switching.

### Blank output panel

Check the browser console for JavaScript errors. Ensure you are using a supported browser version. Hard-refresh with `Ctrl+Shift+R` (Windows/Linux) or `Cmd+Shift+R` (macOS).

### Local mode shows "Contract not found"

Ensure `CONTRACT_TTL_VAULT` is set in `.env` and the backend has been restarted after the update:

```bash
docker-compose restart backend
```

---

## Related Documentation

- [docs/video-tutorials.md](video-tutorials.md) — video walkthroughs for each scenario
- [docs/faq.md](faq.md) — answers to common questions encountered in the playground
- [docs/ttl-logic.md](ttl-logic.md) — deep dive into TTL mechanics
- [docs/passkeys.md](passkeys.md) — passkey authentication details
- [docs/deployment-guide.md](deployment-guide.md) — deploying your own contract to connect to the playground
- [docs/disaster-recovery-runbook.md](disaster-recovery-runbook.md) — Scenario 10 background

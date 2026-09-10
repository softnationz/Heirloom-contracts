# FAQ and Common Issues Database

> **Tip**: Use your browser's search (`Ctrl+F` / `Cmd+F`) or the section links below to jump directly to your topic.

## Table of Contents

- [General](#general)
- [Vault Setup and Configuration](#vault-setup-and-configuration)
- [Check-In and TTL](#check-in-and-ttl)
- [Beneficiary Management](#beneficiary-management)
- [Deposits and Withdrawals](#deposits-and-withdrawals)
- [Passkey and Authentication](#passkey-and-authentication)
- [Deployment and Network](#deployment-and-network)
- [Smart Contract Errors](#smart-contract-errors)
- [Backend and API](#backend-and-api)
- [Security and Audit](#security-and-audit)
- [Troubleshooting Common Issues](#troubleshooting-common-issues)

---

## General

### What is Heirloom-Protocol?

Heirloom-Protocol is a decentralized "Dead Man's Switch" built on Stellar/Soroban smart contracts. It lets you deposit funds into a vault that automatically releases to a designated beneficiary if you fail to periodically check in. It is designed for digital inheritance without lawyers, seed phrases, or trusted intermediaries.

See the [README](../README.md) for a complete overview.

### What problem does Heirloom-Protocol solve?

Over $140 billion in crypto assets are estimated to be permanently lost because wallet owners become unable to pass on access. Heirloom-Protocol solves this by using Stellar's native TTL (Time-to-Live) mechanics as an automated inheritance trigger — no executor required.

### Is Heirloom-Protocol audited?

Not yet. The codebase has not undergone a formal third-party security audit. Community review is welcome. See [docs/security.md](security.md) and [docs/security-audit-checklist.md](security-audit-checklist.md) for the current threat model and checklist.

### What networks does Heirloom-Protocol support?

| Network | Purpose |
|---|---|
| `testnet` | Development and testing (default for CI) |
| `mainnet` | Production deployments |
| `futurenet` | Bleeding-edge Stellar features |
| `standalone` | Local Docker-based development |

Configured in `environments.toml`.

### What tokens can I store in a vault?

Currently XLM (Stellar Lumens) is natively supported. Custom Stellar tokens (e.g., USDC, EURC) are on the roadmap for v1.1. See [docs/roadmap.md](roadmap.md).

---

## Vault Setup and Configuration

### How do I create a vault?

```rust
create_vault(beneficiary: Address, check_in_interval: u64) -> u64
```

- `beneficiary`: The Stellar address that receives the funds when TTL expires.
- `check_in_interval`: Seconds between required check-ins. When this window passes without a check-in, the vault is considered expired.
- Returns the `vault_id` you use for all subsequent calls.

### Can I set myself as the beneficiary?

No. The contract explicitly rejects `owner == beneficiary` to prevent misuse of the release logic. You will get `ContractError::InvalidBeneficiary`.

### Can I change the beneficiary after vault creation?

Yes:

```rust
update_beneficiary(vault_id: u64, new_beneficiary: Address)
```

Only the vault owner can call this. The new beneficiary cannot equal the owner.

### What is the minimum check-in interval?

There is no enforced minimum interval at the contract level, but very short intervals are impractical. The check-in rate limiter (default cooldown: 60 seconds) prevents rapid consecutive check-ins. See [docs/ttl-logic.md](ttl-logic.md) for details.

### Can I have multiple vaults?

Yes. Each `create_vault` call returns a unique `vault_id`. You can create as many vaults as you need, each with its own beneficiary and TTL configuration.

### How do I prevent duplicate vaults for the same beneficiary?

The protocol has duplicate vault prevention logic. See [docs/duplicate-vault-prevention.md](duplicate-vault-prevention.md).

---

## Check-In and TTL

### How does TTL work?

Each vault tracks:
- `last_check_in`: Timestamp of the last owner check-in.
- `check_in_interval`: Seconds before expiry.

A vault is expired when:
```
current_time >= last_check_in + check_in_interval
```

Calling `check_in` resets `last_check_in` to the current timestamp. See [docs/ttl-logic.md](ttl-logic.md) for full details.

### How do I check in?

```rust
check_in(vault_id: u64)
```

Requires the vault owner's authentication. On success it extends the TTL countdown.

### Can I check in with location metadata?

Yes. Use the geo check-in function:

```rust
check_in_with_geo(
    vault_id, caller, passkey_hash,
    latitude_micro, longitude_micro, country_code
)
```

Coordinates are in microdegrees (e.g. `37_422_000` = 37.422°). Country code is ISO 3166-1 alpha-2 (e.g. `"US"`). History is stored on-chain under `CheckInGeoLog(vault_id)`.

### What is TTL borrowing?

If you have two vaults, you can temporarily "borrow" TTL from one to extend the other during an emergency:

```rust
borrow_ttl(borrower_vault_id, lender_vault_id, caller, borrow_seconds)
repay_ttl_borrow(borrower_vault_id, caller)
```

The lender's TTL shortens by `borrow_seconds`; the borrower's TTL extends by the same amount. A `TtlBorrowRecord` is stored on-chain for auditability.

### What is accelerated TTL decay?

You can voluntarily shorten your vault's remaining TTL to trigger release sooner:

```rust
accelerate_ttl_decay(vault_id, caller, accelerate_by_seconds)
```

Capped at 30 days per call. Cannot push expiry to the current time.

### What happens if Soroban archives my vault state?

Vault data is not deleted — it is archived. You or anyone else can restore it:

```rust
restore_vault(vault_id)
```

`trigger_release` will also automatically attempt restoration before transferring funds. See [docs/ttl-logic.md](ttl-logic.md#vault-archival-and-restoration).

### I am getting `CheckInTooFrequent` (error 54). What does this mean?

You are trying to check in before the cooldown window has passed. The default cooldown is 60 seconds. Wait and try again, or contact an admin to adjust the cooldown:

```rust
set_min_checkin_cooldown(cooldown_seconds) // admin only
```

---

## Beneficiary Management

### What conditions does a beneficiary need to accept the role?

Beneficiaries can be required to accept their role only when vault funds exceed a threshold. See [docs/beneficiary-conditional-acceptance.md](beneficiary-conditional-acceptance.md).

### What happens if multiple beneficiaries claim the same vault?

The contract has automated conflict resolution logic. See [docs/beneficiary-conflict-resolution.md](beneficiary-conflict-resolution.md) for the full resolution workflow.

### Can a beneficiary delegate their role to someone else?

Yes. The beneficiary (or the current delegate) can call:

```rust
delegate_beneficiary_role(vault_id, delegate_address)
```

This updates the delegation chain and emits a `del_ben` event.

### What are beneficiary caps, floors, and ranking?

- **Caps**: Maximum payout per beneficiary — [docs/beneficiary-caps.md](beneficiary-caps.md)
- **Floors**: Minimum payout threshold — [docs/beneficiary-floors.md](beneficiary-floors.md)
- **Ranking**: Priority ordering when multiple beneficiaries compete — [docs/beneficiary-ranking.md](beneficiary-ranking.md)
- **Advanced features overview**: [docs/beneficiary-advanced-features.md](beneficiary-advanced-features.md)

### What is the beneficiary proof-of-life and voting mechanism?

Beneficiaries can be required to prove they are active before receiving funds. See [docs/beneficiary-proof-of-life-and-voting.md](beneficiary-proof-of-life-and-voting.md).

---

## Deposits and Withdrawals

### How do I deposit funds into a vault?

```rust
deposit(vault_id: u64, amount: i128)
```

Only the vault owner can deposit. Amounts are in stroops (1 XLM = 10,000,000 stroops).

### How do I withdraw funds?

```rust
withdraw(vault_id: u64, amount: i128)
```

Only the vault owner can withdraw while the vault is active and not expired. For all withdrawal features including batching, audit trail, notifications, and dispute — see [docs/withdrawal-features.md](withdrawal-features.md).

### What is withdrawal batching?

Multiple small withdrawals can be batched into a single transaction for efficiency. See [docs/withdrawal-features.md](withdrawal-features.md#batching).

### Can I dispute a withdrawal I did not authorize?

Yes. There is a 24-hour grace period during which unauthorized withdrawals can be disputed. See [docs/withdrawal-features.md](withdrawal-features.md#dispute).

---

## Passkey and Authentication

### What are Passkeys and why does Heirloom use them?

Passkeys (WebAuthn) replace seed phrases with biometric authentication — fingerprint, Face ID, or hardware security key. They are phishing-resistant and hardware-backed. No seed phrase exposure means no single point of failure.

Full details: [docs/passkeys.md](passkeys.md).

### Is Passkey integration available now?

Full WebAuthn on-chain verification is planned for v2.0. The current implementation uses standard Stellar address authentication. See [docs/passkeys.md#current-status](passkeys.md#current-status).

### What happens when a passkey expires?

If a passkey's expiry has been set via `extend_passkey_expiry` and that time has passed, any check-in attempt with that passkey returns `PasskeyExpired` (error 59). A `pk_expd` event is also emitted. Rotate the passkey using `register_passkey` and `revoke_passkey`.

### What happens if my passkey is compromised?

You can manually flag a passkey:

```rust
report_passkey_compromise(vault_id, caller, passkey_hash)
```

Subsequent check-ins with that hash return `PasskeyCompromised` (error 62). To clear the flag:

```rust
clear_passkey_compromise(vault_id, caller, passkey_hash)
```

The contract also performs automatic compromise detection: if 3 or more consecutive check-ins use different passkey hashes, a `pk_comp` event is emitted as an advisory alert.

### What is biometric check-in?

Biometric credentials (hash commitments of fingerprint/face data) can be registered and used for check-ins. Raw biometric data never leaves the device — only the SHA-256 hash is stored on-chain.

```rust
bind_passkey_biometric(vault_id, caller, passkey_hash, credential_hash)
biometric_check_in(vault_id, caller, passkey_hash, credential_hash)
```

See [docs/passkeys.md](passkeys.md#biometric-verification).

---

## Deployment and Network

### How do I deploy to testnet?

```bash
stellar keys generate deployer --network testnet
./scripts/deploy_testnet.sh
```

Full guide: [docs/deployment-guide.md](deployment-guide.md).

### How do I deploy to mainnet?

```bash
export STELLAR_MAINNET_RPC_URL=https://mainnet.sorobanrpc.com
stellar keys generate deployer-mainnet --network mainnet
./scripts/deploy_mainnet.sh
```

The script will prompt you to type `mainnet` before proceeding.

### How do I set up local development?

```bash
cp .env.example .env
docker-compose up -d
```

This starts PostgreSQL (port 5432), the backend (port 3000), and a local Stellar Quickstart node (port 8000). See [docs/deployment-guide.md](deployment-guide.md).

### Where are network configurations defined?

In `environments.toml` at the project root. Contains RPC URLs and network passphrases for all supported networks.

---

## Smart Contract Errors

Below is a reference table of common contract errors. Full enum definitions are in `contracts/ttl_vault/src/lib.rs`.

| Code | Name | Meaning | Fix |
|---|---|---|---|
| — | `AlreadyInitialized` | Contract was initialized twice | Do not call `initialize` again |
| — | `NotExpired` | `trigger_release` called before TTL expired | Wait for TTL to lapse |
| — | `AlreadyReleased` | Vault already released | No action needed |
| — | `InvalidBeneficiary` | Owner == beneficiary, or invalid address | Use a different beneficiary address |
| 26 | `InvalidPasskey` | Passkey not registered for this vault | Register the passkey first |
| 54 | `CheckInTooFrequent` | Check-in within cooldown window | Wait for cooldown period to expire |
| 55 | `InsufficientTtlToAccelerate` | Remaining TTL too small to accelerate | Reduce acceleration amount |
| 59 | `PasskeyExpired` | Passkey registration has expired | Rotate the passkey |
| 62 | `PasskeyCompromised` | Passkey flagged as compromised | Rotate and clear the compromise flag |

---

## Backend and API

### What does the backend service do?

The Rust/Axum backend handles:
- Reminder emails and SMS for upcoming check-in deadlines
- WebSocket real-time notifications
- REST API for frontend integrations
- GraphQL endpoint
- Webhook delivery for external integrations

See [docs/backend-api.md](backend-api.md) and [docs/api-reference.md](api-reference.md).

### Where is the OpenAPI specification?

[docs/openapi.yaml](openapi.yaml) — importable into Postman, Insomnia, Swagger UI, or any OpenAPI-compatible tool.

### How do I configure reminder notifications?

Set these in your `.env` file:

```env
REMINDER_EMAIL_API_KEY=<your-key>
REMINDER_SMS_API_KEY=<your-key>
```

See [docs/push-notifications.md](push-notifications.md) for advanced notification configuration.

### What monitoring is available?

Prometheus-compatible metrics are exposed for service health and contract event tracking. See [docs/monitoring-guide.md](monitoring-guide.md).

---

## Security and Audit

### Can an admin steal vault funds?

No. Admin capabilities are intentionally limited:
- Admin cannot access vault funds.
- Admin cannot change vault owners or beneficiaries.
- Admin transitions require a two-step `propose_admin` / `accept_admin` flow.
- All admin actions are transparent and on-chain.

### What prevents a beneficiary from triggering early release?

`trigger_release` calls `is_expired()` first. If the TTL has not lapsed, it returns `ContractError::NotExpired` and reverts. Nothing is transferred.

### Can the contract be re-initialized by an attacker?

No. `initialize()` checks for an existing admin/token and returns `ContractError::AlreadyInitialized` if already set.

### Where is the full threat model?

[docs/security.md](security.md) and [docs/security-audit-checklist.md](security-audit-checklist.md).

### Is there a vulnerability disclosure process?

Yes. See [SECURITY.md](../SECURITY.md) for the responsible disclosure policy.

---

## Troubleshooting Common Issues

### Build fails with `wasm-opt not found`

Install `wasm-opt` via `binaryen`:

```bash
# macOS
brew install binaryen

# Debian/Ubuntu
apt-get install binaryen
```

Or use the provided Docker setup which includes all tooling.

### `stellar keys generate` fails with network error

Check your RPC URL in `.env` or `environments.toml`. For testnet, the default is:

```
https://soroban-testnet.stellar.org
```

Ensure you have an internet connection and the testnet is not under maintenance (check [Stellar Status](https://status.stellar.org)).

### Contract invocation returns `HostError: General`

This typically means a precondition was not met. Common causes:
1. Calling `check_in` with an unregistered passkey → register the passkey first.
2. Calling `trigger_release` before TTL expires → check `get_ttl_remaining`.
3. Calling `deposit` or `withdraw` with zero amount.

### Docker containers fail to start

```bash
docker-compose down -v
docker-compose up -d --build
```

Ensure ports 5432, 3000, and 8000 are not already in use.

### PostgreSQL connection refused

Check that the `DATABASE_URL` in your `.env` matches the Docker Compose configuration. The health check in `docker-compose.yml` ensures the database is ready before the backend starts.

### How do I reset local state for a fresh start?

```bash
docker-compose down -v   # removes volumes including the DB
docker-compose up -d
```

For Stellar local state, clear `.soroban/` in your home directory.

---

## Contributing and Support

- Found a bug? [Open an issue](https://github.com/ethos-protocol/ethos-contracts-backend/issues)
- Contributing code? See [CONTRIBUTING.md](../CONTRIBUTING.md)
- Security issue? See [SECURITY.md](../SECURITY.md)
- All contributors must follow the [Code of Conduct](../CODE_OF_CONDUCT.md)

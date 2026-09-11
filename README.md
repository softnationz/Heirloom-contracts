# Heirloom-Protocol — Micro-Endowment Check-In Vault on Stellar

[![CI](https://github.com/ethos-protocol/ethos-contracts-backend/actions/workflows/ci.yml/badge.svg)](https://github.com/ethos-protocol/ethos-contracts-backend/actions/workflows/ci.yml)

A decentralized "Dead Man's Switch" built on Stellar/Soroban smart contracts.

> **Repository layout:** this repo is two independent Cargo workspaces —
> `Heirloom-contracts/` (on-chain Soroban/WASM) and `Heirloom-backend/`
> (off-chain HTTP/GraphQL service). There is no root `Cargo.toml`; run
> `cargo` from inside either directory. See [REPO_LAYOUT.md](REPO_LAYOUT.md).

Heirloom-Protocol is a time-capsule vault where funds (XLM or tokenized assets) are released to a beneficiary only if the owner fails to "check in" via a Passkey-powered interface. It leverages Soroban's State Archival and TTL (Time to Live) features to automate asset inheritance — no seed phrase complexity required.

## 🎯 What is Heirloom-Protocol?

Heirloom-Protocol turns Stellar's native state archival mechanics into a programmable inheritance trigger. Vault owners:

- Deposit funds into a personal vault contract
- Periodically "check in" to extend the contract's TTL and prove liveness
- Designate a beneficiary address for automatic release
- Authenticate exclusively via Passkeys (WebAuthn) — no seed phrases

If the owner stops checking in, the contract's TTL expires and the vault automatically releases funds to the beneficiary.

This Soroban implementation makes Heirloom-Protocol:

✅ Trustless (no executor, lawyer, or coordinator needed)  
✅ Transparent (all vault state and transfers are on-chain)  
✅ Secure (Passkey/WebAuthn authentication, no exposed seed phrases)  
✅ Automated (TTL expiry triggers transfer without manual intervention)

## 🚀 Features

- **Create a Vault**: Set a beneficiary address and check-in interval
- **Check-In**: Extend the contract TTL to reset the countdown
- **Automatic Release**: Funds transfer to beneficiary when TTL lapses
- **Beneficiary Conditional Acceptance**: Beneficiary can accept role only if funds exceed threshold
- **Beneficiary Conflict Resolution**: Automated conflict resolution when multiple beneficiaries claim same vault
- **Passkey Auth**: WebAuthn-based authentication for all owner actions
- **Reminder System**: Backend sends encrypted email/SMS check-in reminders
- **Legacy Dashboard**: Minimalist frontend to manage vault state and history
- **Native XLM Support**: Built-in support for Stellar Lumens
- **Token Ready**: Architecture supports custom Stellar tokens (roadmap item)
- **Withdrawal Audit Trail**: Track all withdrawal attempts (successful and failed) with comprehensive details
- **Withdrawal Batching**: Batch multiple small withdrawals into single transaction for efficiency
- **Withdrawal Notifications**: Real-time alerts to vault owners for all withdrawal attempts
- **Withdrawal Dispute**: Allow disputing unauthorized withdrawals within 24-hour grace period

## 🛠️ Quick Start

### Prerequisites

- Rust (1.70+)
- Soroban CLI
- Stellar CLI

### Build

```bash
./scripts/build.sh
```

### Test

```bash
./scripts/test.sh
```

### Local Development Setup (Docker)

For fastest local setup, use Docker Compose:

```bash
docker-compose up -d
```

This will start:
- **PostgreSQL** on `localhost:5432`
- **Backend** on `localhost:3000`
- **Stellar Quickstart** on `localhost:8000`

The `docker-compose.override.yml` configures development-specific settings (standalone Stellar, loose auth, mounted volumes).

Database health check ensures the service is ready before the backend starts.

### Setup Environment

Copy the example environment file:

```bash
cp .env.example .env
```

Configure your environment variables in `.env`:

```env
# Network configuration
STELLAR_NETWORK=testnet
STELLAR_RPC_URL=https://soroban-testnet.stellar.org

# Contract addresses (after deployment)
CONTRACT_TTL_VAULT=<your-contract-id>

# Frontend configuration
VITE_STELLAR_NETWORK=testnet
VITE_STELLAR_RPC_URL=https://soroban-testnet.stellar.org

# Backend (reminder service)
REMINDER_EMAIL_API_KEY=<your-key>
REMINDER_SMS_API_KEY=<your-key>
```

Network configurations are defined in `environments.toml`:

- `testnet` — Stellar testnet
- `mainnet` — Stellar mainnet
- `futurenet` — Stellar futurenet
- `standalone` — Local development

### Deploy to Testnet

```bash
# Configure your testnet identity first
stellar keys generate deployer --network testnet

# Deploy
./scripts/deploy_testnet.sh
```

### Deploy to Mainnet

Required environment variables before running:

| Variable | Description |
|---|---|
| `STELLAR_MAINNET_RPC_URL` | Mainnet RPC endpoint (e.g. `https://mainnet.sorobanrpc.com`) |
| `DEPLOYER_IDENTITY` | Stellar CLI key name to sign the deployment (default: `deployer-mainnet`) |

```bash
# Configure your mainnet identity first
stellar keys generate deployer-mainnet --network mainnet

# Set required env var
export STELLAR_MAINNET_RPC_URL=https://mainnet.sorobanrpc.com

# Deploy (will prompt for confirmation)
./scripts/deploy_mainnet.sh
```

The script will display the target network and identity, then require you to type `mainnet` before proceeding.

## 📖 Documentation

### Learning Resources
- [FAQ and Common Issues](docs/faq.md)
- [Video Tutorials](docs/video-tutorials.md)
- [Interactive Playground](docs/playground.md)
- [Architecture Diagrams and Visual Guides](docs/architecture-diagrams.md)

### Core Concepts
- [Architecture Overview](docs/architecture.md)
- [TTL & State Archival Logic](docs/ttl-logic.md)
- [Vault Hibernation](docs/hibernation.md)
- [Passkey Integration](docs/passkeys.md)

### Beneficiary Features
- [Beneficiary Conditional Acceptance](docs/beneficiary-conditional-acceptance.md)
- [Beneficiary Conflict Resolution](docs/beneficiary-conflict-resolution.md)
- [Beneficiary Advanced Features](docs/beneficiary-advanced-features.md)
- [Vesting Schedules](docs/vesting-schedules.md)

### Operations
- [Withdrawal Features](docs/withdrawal-features.md)
- [Deployment Guide](docs/deployment-guide.md)
- [Monitoring Guide](docs/monitoring-guide.md)
- [Disaster Recovery Runbook](docs/disaster-recovery-runbook.md)

### Security
- [Threat Model & Security](docs/security.md)
- [Security Audit Checklist](docs/security-audit-checklist.md)
- [Security Policy & Vulnerability Disclosure](SECURITY.md)

### Reference
- [API Reference](docs/api-reference.md)
- [OpenAPI Specification](docs/openapi.yaml)
- [Roadmap](docs/roadmap.md)

## 🎓 Smart Contract API

### Vault Management

```rust
create_vault(beneficiary: Address, check_in_interval: u64) -> u64
get_vault(vault_id: u64) -> Vault
get_ttl_remaining(vault_id: u64) -> Option<u64>
```

### Owner Actions

```rust
check_in(vault_id: u64)
deposit(vault_id: u64, amount: i128)
withdraw(vault_id: u64, amount: i128)
update_beneficiary(vault_id: u64, new_beneficiary: Address)
```

### Release

```rust
trigger_release(vault_id: u64)
is_expired(vault_id: u64) -> bool
get_release_status(vault_id: u64) -> ReleaseStatus
```

### Hibernation

```rust
enter_hibernation(vault_id: u64, caller: Address, duration_seconds: u64)
exit_hibernation(vault_id: u64, caller: Address)
get_hibernation(vault_id: u64) -> Option<HibernationEntry>
```

### Slice Failover (Issue #35)

Protects slices from becoming single points of failure. The vault owner
pre-registers backup slices; the contract auto-promotes a backup once the
primary's failure count reaches a configured threshold.

```rust
// Owner-only: register backup and configure threshold
register_backup_slice(vault_id, caller, primary_slice_id, backup_slice_id, failure_threshold) -> u64

// Owner-only: record a failure and auto-promote backup when threshold is reached
record_slice_failure(vault_id, caller, primary_slice_id, reason: FailoverReason) -> bool

// Owner-only: force-promote backup without threshold
activate_failover(vault_id, caller, primary_slice_id, backup_slice_id, reason) -> bool

// Owner-only: restore primary and reset failure counter
revert_failover(vault_id, caller, primary_slice_id, backup_slice_id) -> bool

// Read-only queries (no auth required)
get_backup_slices(primary_slice_id) -> Vec<u64>
get_active_slice(slice_id) -> u64      // returns primary_id when healthy, backup_id when failed over
get_failure_count(slice_id) -> u32
```

See [docs/slice-performance-and-rules-engine.md](docs/slice-performance-and-rules-engine.md) for full details.

## 🧪 Testing

Comprehensive test suite covering:

✅ Vault creation and configuration  
✅ Check-in flow and TTL extension  
✅ TTL expiry and automatic release  
✅ Passkey authentication validation  
✅ Beneficiary payout execution  
✅ Slice failover — authorized promotion and unauthorized rejection  
✅ Error handling and edge cases  

Run tests:

```bash
cd Heirloom-contracts && cargo test        # on-chain contracts
cd Heirloom-backend   && cargo test        # off-chain service
```

## 🌍 Why This Matters

**The Problem**: Over $140 billion in crypto assets are estimated to be permanently lost due to inaccessible wallets. Traditional inheritance mechanisms don't map to self-custodied digital assets.

**Blockchain Benefits**:

- No trusted executor or legal intermediary required
- Transparent vault state and release history on-chain
- Programmable rules enforced by smart contracts
- Passkey auth removes the seed phrase single point of failure

**Target Users**:

- Long-term crypto holders planning for asset continuity
- Individuals without access to traditional estate planning
- Families and communities building generational wealth on-chain
- Anyone who wants a secure, automated digital legacy plan

## 🗺️ Roadmap

- **v1.0 (Current)**: XLM vaults, TTL-based release, Passkey auth
- **v1.1**: Custom token support (USDC, EURC, etc.)
- **v2.0**: Multi-beneficiary splits, conditional release logic
- **v3.0**: Mobile-friendly frontend, push notification reminders
- **v4.0**: Fiat on/off-ramps, legal document anchoring via Stellar

See [docs/roadmap.md](docs/roadmap.md) for details.

## 🤝 Contributing

We welcome contributions! Please:

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

See our [Code of Conduct](CODE_OF_CONDUCT.md) and [Contributing Guidelines](CONTRIBUTING.md).

## 📄 License

This project is licensed under the MIT License — see the [LICENSE](LICENSE) file for details.

## 🙏 Acknowledgments

- [Stellar Development Foundation](https://stellar.org) for Soroban
- The WebAuthn/Passkey standards community
- Everyone building toward self-sovereign financial tools




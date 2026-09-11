# Best Practices Guide

This guide documents recommended practices for developing, deploying, and operating Heirloom-Protocol. Following these practices improves security, reliability, and maintainability.

## Table of Contents

- [Development Best Practices](#development-best-practices)
- [Smart Contract Best Practices](#smart-contract-best-practices)
- [Deployment Best Practices](#deployment-best-practices)
- [Operational Best Practices](#operational-best-practices)
- [Security Best Practices](#security-best-practices)
- [Configuration Best Practices](#configuration-best-practices)
- [Testing Best Practices](#testing-best-practices)

---

## Development Best Practices

### Code Organization

**Keep contract logic pure and deterministic**

Soroban contracts must produce identical results for the same inputs. Avoid relying on external state or non-deterministic inputs:

```rust
// ✅ Good: deterministic, uses on-chain time
let current_time = env.ledger().timestamp();

// ❌ Bad: system time is not available in contracts
let current_time = std::time::SystemTime::now(); // will not compile in Soroban
```

**Separate concerns between contract and backend**

- Smart contract: on-chain state, fund transfers, TTL logic, access control
- Backend: notifications, user sessions, off-chain indexing, webhook delivery

Do not push off-chain logic (email, SMS, UI formatting) into the contract.

**Use explicit error types**

Define all contract errors as an enum and return `Result<T, ContractError>` from fallible functions:

```rust
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ContractError {
    NotFound = 1,
    NotExpired = 2,
    Unauthorized = 3,
    InsufficientBalance = 4,
    // ...
}
```

This makes error handling predictable and testable.

**Avoid storage reads in loops**

Each storage read incurs a fee. Batch reads where possible:

```rust
// ❌ Bad: reads storage inside loop
for id in vault_ids {
    let vault = env.storage().persistent().get::<_, Vault>(&DataKey::Vault(id));
    // ...
}

// ✅ Better: collect and process outside hot paths, or redesign data model
```

**Use meaningful event names**

Emit events for every state-changing action. Keep event names short (Soroban fee efficiency) but descriptive:

```rust
env.events().publish(("check_in", "v1"), (vault_id, caller));
```

---

### Rust/Soroban Conventions

**Pin dependency versions**

Use exact versions in `Cargo.toml` to avoid unexpected breakage:

```toml
[dependencies]
soroban-sdk = "=21.0.0"  # ✅ pinned
soroban-sdk = "^21"      # ❌ allows minor updates that may break
```

**Run Clippy before committing**

The project includes a `.clippy.toml` configuration. Always run:

```bash
cargo clippy -- -D warnings
```

**Format code with rustfmt**

```bash
cargo fmt --all
```

**Document all public functions**

```rust
/// Creates a new vault with the specified beneficiary and check-in interval.
///
/// # Arguments
/// * `beneficiary` - The Stellar address that receives funds if the owner stops checking in
/// * `check_in_interval` - Seconds between required owner check-ins
///
/// # Returns
/// The unique `vault_id` for the created vault
pub fn create_vault(env: Env, beneficiary: Address, check_in_interval: u64) -> u64 {
    // ...
}
```

---

## Smart Contract Best Practices

### Access Control

**Always require caller authentication**

Every state-modifying function must authenticate the caller:

```rust
pub fn withdraw(env: Env, vault_id: u64, amount: i128) {
    let vault = Self::load_vault(&env, vault_id);
    vault.owner.require_auth(); // ✅ always authenticate
    // ...
}
```

**Separate owner and admin roles**

Owner actions (check-in, deposit, withdraw) must only be callable by the vault owner. Admin actions (set cooldown, upgrade) must require a separate admin key. Never conflate the two.

**Validate all inputs**

Check inputs before modifying state:

```rust
if amount <= 0 {
    return Err(ContractError::InvalidAmount);
}
if check_in_interval == 0 {
    return Err(ContractError::InvalidInterval);
}
```

### State Management

**Use persistent storage for vault data**

Vault state must survive ledger TTL expiry of temporary entries:

```rust
env.storage().persistent().set(&DataKey::Vault(vault_id), &vault);
```

**Extend TTL on every write**

After writing to persistent storage, extend the entry TTL to prevent archival:

```rust
env.storage().persistent().extend_ttl(
    &DataKey::Vault(vault_id),
    MIN_TTL,
    MAX_TTL,
);
```

**Avoid unbounded storage growth**

Limit log sizes (e.g., geo check-in logs, audit trails) with a max-entry cap:

```rust
const MAX_GEO_LOG_ENTRIES: usize = 100;
if log.len() >= MAX_GEO_LOG_ENTRIES {
    log.remove(0); // drop oldest entry
}
```

### Upgrades

**Test upgrade paths before deploying to mainnet**

Deploy the new WASM to testnet and run the full test suite against the upgraded contract before touching mainnet.

**Emit an event on upgrade**

```rust
env.events().publish(("contract_upgrade", "v1"), new_wasm_hash);
```

---

## Deployment Best Practices

### Pre-Deployment Checklist

Before deploying to any network:

- [ ] All tests pass (`cargo test`)
- [ ] Clippy reports no warnings (`cargo clippy -- -D warnings`)
- [ ] WASM size is within budget — see [WASM Size Budget](wasm-size-budget.md)
- [ ] Contract has been audited or reviewed for the target network
- [ ] Environment variables are configured and validated
- [ ] Database schema is up to date
- [ ] Backup of current mainnet contract state (if upgrading)

### Testnet First

Always deploy to testnet before mainnet:

```bash
./scripts/deploy_testnet.sh
# Verify contract behavior on testnet
./scripts/deploy_mainnet.sh  # only after testnet validation
```

### Mainnet Deployment

**Require explicit confirmation**

The `deploy_mainnet.sh` script prompts for manual confirmation. Never skip this:

```bash
# The script will display target network and identity,
# then require you to type "mainnet" to proceed.
./scripts/deploy_mainnet.sh
```

**Use a dedicated deployer identity**

Never use a personal wallet or hot wallet for deployment:

```bash
stellar keys generate deployer-mainnet --network mainnet
```

Store the deployer key securely and restrict access.

**Record deployed contract IDs**

After deployment, update `CONTRACT_TTL_VAULT` and other contract address variables in your production environment and configuration management system.

### Rollback Plan

Before any mainnet upgrade:

1. Document the current contract ID
2. Snapshot all vault states
3. Test the rollback procedure on testnet
4. Have a recovery plan if the upgrade introduces bugs (e.g., re-deploying the previous WASM)

---

## Operational Best Practices

### Monitoring

**Monitor vault expiry events**

Set up alerts for vaults approaching TTL expiry that have not checked in recently. See [Monitoring Guide](monitoring-guide.md).

**Track backend health**

The backend exposes a `/health` endpoint and Prometheus metrics. Monitor:

- Request error rate
- Database connection pool utilization
- Scheduler job success rate
- Notification delivery rate

**Set up log aggregation**

```env
RUST_LOG=warn  # production: warn or error only
```

Ship logs to a centralized system (e.g., CloudWatch, Datadog) for alerting and auditing.

### Scheduler & Reminders

**Validate reminder delivery**

After deployment, send a test reminder to confirm notification pipelines are functional.

**Handle notification failures gracefully**

A failed email or SMS must not prevent vault operations. Log the failure and retry independently.

### Database

**Run regular backups**

Back up PostgreSQL daily. Test restore procedures quarterly.

**Use connection pooling**

Always use the pool (`DB_MAX_CONNECTIONS`) rather than direct connections to avoid exhausting PostgreSQL's connection limit.

**Index frequently queried columns**

Ensure indexes exist on `vault_id`, `owner_address`, and `beneficiary_address` in the backend database.

---

## Security Best Practices

### Key Management

**Never expose seed phrases**

Heirloom-Protocol is designed to avoid seed phrases entirely. Use Passkey/WebAuthn for all owner authentication. Do not add seed-phrase-based fallbacks.

**Rotate JWT secrets periodically**

```bash
openssl rand -hex 32  # generate new secret
# Update JWT_SECRET in production, restart backend
```

Active sessions will be invalidated on rotation — plan for a brief re-authentication window.

**Restrict deployer key usage**

The deployer Stellar key should only be used for contract deployment. Do not use it for any runtime operations.

### Passkey / WebAuthn

**Set `PASSKEY_RP_ID` to your exact domain**

A mismatch between `PASSKEY_RP_ID` and the actual origin will silently break authentication:

```env
# ✅ Matches your production domain
PASSKEY_RP_ID=app.yourdomain.com
PASSKEY_RP_ORIGIN=https://app.yourdomain.com

# ❌ Overly broad RP ID
PASSKEY_RP_ID=yourdomain.com  # only valid if app runs at root domain
```

**Verify user presence, not just user verification**

WebAuthn supports both UP (user presence) and UV (user verification, e.g. PIN or biometric). Require UV for all vault-modifying operations.

### Input Validation

**Validate all amounts are positive**

```rust
if amount <= 0 {
    return Err(ContractError::InvalidAmount);
}
```

**Cap check-in intervals to reasonable limits**

Extremely large intervals could prevent beneficiaries from ever claiming funds:

```rust
const MAX_CHECK_IN_INTERVAL: u64 = 10 * 365 * 24 * 60 * 60; // 10 years
```

### Network Security

**Use HTTPS for all external endpoints**

All API keys, webhook URLs, and Passkey origins must use HTTPS in production.

**Restrict CORS origins**

Do not use `*` as an allowed CORS origin in production. Set the exact frontend origin.

**Validate webhook signatures**

Incoming webhooks must be verified with HMAC signatures before processing:

```rust
// Verify X-Webhook-Signature header before processing payload
```

For the full threat model, see [Threat Model & Security](security.md) and [Security Policy](../SECURITY.md).

---

## Configuration Best Practices

**Never commit secrets to source control**

`.env`, `.env.local`, and any file containing API keys or JWT secrets must be in `.gitignore` (already set up in this project).

**Use separate configs per environment**

Maintain distinct environment files or secrets management entries for:
- `local` / `standalone`
- `testnet`
- `mainnet`

**Validate configuration at startup**

The backend checks for required variables at boot. Fix all warnings before going live.

**Document every custom environment variable**

Add any new variables to `.env.example` with a description, so teammates know what is needed.

---

## Testing Best Practices

**Write tests for every contract function**

Cover the happy path, error cases, and edge cases:

```rust
#[test]
fn test_create_vault_sets_beneficiary() { /* ... */ }

#[test]
fn test_trigger_release_fails_before_expiry() { /* ... */ }

#[test]
fn test_trigger_release_succeeds_after_expiry() { /* ... */ }
```

**Use the Soroban test environment**

```rust
let env = Env::default();
env.mock_all_auths();
```

**Test TTL and time-dependent logic with mocked ledger time**

```rust
env.ledger().with_mut(|l| {
    l.timestamp = initial_time + check_in_interval + 1; // simulate expiry
});
```

**Do not commit test snapshots to main branches**

Test snapshot files (`.snap`, `.snap.new`) are in `.gitignore`. They should not be included in feature branch commits.

**Run integration tests against testnet before releasing**

See [Integration Testing Guide](integration-testing-guide.md) for the full procedure.

**Use fuzz testing for critical paths**

The project includes a fuzz test harness under `Heirloom-contracts/ttl_vault/fuzz/`. Run periodically:

```bash
cargo fuzz run fuzz_target_1
```

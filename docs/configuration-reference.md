# Configuration Reference Guide

This guide documents all configuration options for Heirloom-Protocol, including environment variables, contract parameters, network settings, and backend configuration. Each option includes its impact, recommended values, validation rules, and precedence information.

## Table of Contents

- [Configuration Precedence](#configuration-precedence)
- [Environment Variables](#environment-variables)
  - [Stellar Network](#stellar-network)
  - [Contract Addresses](#contract-addresses)
  - [Backend Service](#backend-service)
  - [Notification Services](#notification-services)
  - [Database](#database)
  - [Authentication](#authentication)
  - [Monitoring](#monitoring)
- [Contract Parameters](#contract-parameters)
  - [Vault Parameters](#vault-parameters)
  - [TTL & Check-in Parameters](#ttl--check-in-parameters)
  - [Beneficiary Parameters](#beneficiary-parameters)
  - [Vesting Parameters](#vesting-parameters)
- [Network Configurations (environments.toml)](#network-configurations-environmentstoml)
- [Docker Configuration](#docker-configuration)
- [Configuration Validation](#configuration-validation)
- [Configuration Examples](#configuration-examples)

---

## Configuration Precedence

Configuration values are resolved in the following order (highest to lowest priority):

1. **Runtime environment variables** — set in the shell or Docker environment
2. **`.env` file** — loaded at startup from the project root
3. **`environments.toml`** — network-specific defaults
4. **Built-in contract defaults** — compiled into the smart contract
5. **Soroban network defaults** — provided by the Stellar network

When the same option is set in multiple places, the higher-priority source wins.

> **Note**: `.env.local` overrides `.env` for local development. Never commit `.env` or `.env.local` to source control.

---

## Environment Variables

Copy `.env.example` to `.env` to get started:

```bash
cp .env.example .env
```

### Stellar Network

| Variable | Type | Default | Description |
|---|---|---|---|
| `STELLAR_NETWORK` | `string` | `testnet` | Target Stellar network (`testnet`, `mainnet`, `futurenet`, `standalone`) |
| `STELLAR_RPC_URL` | `string` | *(see environments.toml)* | Soroban RPC endpoint URL |
| `STELLAR_MAINNET_RPC_URL` | `string` | — | Mainnet-specific RPC URL (required for mainnet deploys) |
| `STELLAR_HORIZON_URL` | `string` | *(network default)* | Horizon API URL for balance/account queries |
| `NETWORK_PASSPHRASE` | `string` | *(network default)* | Stellar network passphrase |

**Impact**: Incorrect network settings will cause all contract calls to fail or target the wrong chain.

**Recommended values**:

```env
# Testnet
STELLAR_NETWORK=testnet
STELLAR_RPC_URL=https://soroban-testnet.stellar.org

# Mainnet
STELLAR_NETWORK=mainnet
STELLAR_RPC_URL=https://mainnet.sorobanrpc.com
```

---

### Contract Addresses

| Variable | Type | Default | Description |
|---|---|---|---|
| `CONTRACT_TTL_VAULT` | `string` | — | Deployed `ttl_vault` contract ID (Strkey format) |
| `CONTRACT_SBT` | `string` | — | Deployed `sbt` contract ID |
| `CONTRACT_ZK_VERIFIER` | `string` | — | Deployed `zk_verifier` contract ID |

**Impact**: If these are missing or incorrect, all vault operations will fail.

**Validation**: Must be valid Stellar contract IDs (56-character Strkey starting with `C`).

```env
CONTRACT_TTL_VAULT=CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4
```

---

### Backend Service

| Variable | Type | Default | Description |
|---|---|---|---|
| `BACKEND_HOST` | `string` | `0.0.0.0` | Address the backend HTTP server binds to |
| `BACKEND_PORT` | `integer` | `3000` | Port the backend listens on |
| `RUST_LOG` | `string` | `info` | Log level (`error`, `warn`, `info`, `debug`, `trace`) |
| `RUST_BACKTRACE` | `string` | `0` | Enable backtraces (`0`, `1`, `full`) |
| `JWT_SECRET` | `string` | — | Secret key for signing JWT tokens (min 32 bytes) |
| `SESSION_TTL_SECONDS` | `integer` | `3600` | Session expiry in seconds |

**Impact**: `JWT_SECRET` must be set for authenticated endpoints to work. Use a cryptographically random value.

**Recommended**:

```bash
# Generate a secure JWT secret
openssl rand -hex 32
```

```env
BACKEND_PORT=3000
RUST_LOG=info
JWT_SECRET=<output-from-openssl>
```

---

### Notification Services

| Variable | Type | Default | Description |
|---|---|---|---|
| `REMINDER_EMAIL_API_KEY` | `string` | — | API key for email reminder service |
| `REMINDER_SMS_API_KEY` | `string` | — | API key for SMS reminder service |
| `REMINDER_EMAIL_FROM` | `string` | — | Sender email address for reminders |
| `REMINDER_LEAD_TIME_HOURS` | `integer` | `72` | Hours before TTL expiry to send first reminder |
| `REMINDER_SECOND_LEAD_TIME_HOURS` | `integer` | `24` | Hours before expiry for second reminder |
| `PUSH_NOTIFICATION_KEY` | `string` | — | API key for push notification provider |

**Impact**: Without these keys, owners will not receive expiry reminders and may miss check-in deadlines.

**Recommended**:

```env
REMINDER_LEAD_TIME_HOURS=72
REMINDER_SECOND_LEAD_TIME_HOURS=24
```

---

### Database

| Variable | Type | Default | Description |
|---|---|---|---|
| `DATABASE_URL` | `string` | — | PostgreSQL connection string |
| `DB_MAX_CONNECTIONS` | `integer` | `10` | Maximum connection pool size |
| `DB_MIN_CONNECTIONS` | `integer` | `1` | Minimum idle connections |
| `DB_CONNECT_TIMEOUT_SECONDS` | `integer` | `30` | Connection timeout |
| `DB_IDLE_TIMEOUT_SECONDS` | `integer` | `600` | Idle connection timeout |

**Recommended**:

```env
DATABASE_URL=postgres://user:password@localhost:5432/heirloom
DB_MAX_CONNECTIONS=10
```

**Validation**: `DATABASE_URL` must be a valid PostgreSQL URI. The schema must be initialized before starting the backend.

---

### Authentication

| Variable | Type | Default | Description |
|---|---|---|---|
| `PASSKEY_RP_ID` | `string` | — | Relying Party ID for WebAuthn (typically your domain) |
| `PASSKEY_RP_ORIGIN` | `string` | — | Origin URL where Passkey authentication occurs |
| `PASSKEY_TIMEOUT_MS` | `integer` | `60000` | WebAuthn ceremony timeout in milliseconds |
| `WEBAUTHN_ALLOWED_ORIGINS` | `string` | — | Comma-separated list of allowed WebAuthn origins |
| `TWO_FACTOR_ISSUER` | `string` | `Heirloom-Protocol` | TOTP issuer name shown in authenticator apps |

**Impact**: Incorrect `PASSKEY_RP_ID` or `PASSKEY_RP_ORIGIN` will cause all Passkey authentication to fail.

**Recommended** (local development):

```env
PASSKEY_RP_ID=localhost
PASSKEY_RP_ORIGIN=http://localhost:3000
```

**Recommended** (production):

```env
PASSKEY_RP_ID=yourdomain.com
PASSKEY_RP_ORIGIN=https://yourdomain.com
```

---

### Monitoring

| Variable | Type | Default | Description |
|---|---|---|---|
| `METRICS_ENABLED` | `bool` | `true` | Enable Prometheus metrics endpoint |
| `METRICS_PORT` | `integer` | `9090` | Port for metrics scraping |
| `METRICS_PATH` | `string` | `/metrics` | Path for metrics endpoint |
| `SENTRY_DSN` | `string` | — | Sentry error reporting DSN (optional) |
| `TRACING_ENABLED` | `bool` | `false` | Enable distributed tracing |

**Recommended** (production):

```env
METRICS_ENABLED=true
METRICS_PORT=9090
SENTRY_DSN=https://your-dsn@sentry.io/project
```

---

## Contract Parameters

These parameters are set via smart contract function calls, not environment variables.

### Vault Parameters

| Parameter | Type | Default | Validation | Description |
|---|---|---|---|---|
| `check_in_interval` | `u64` | — | > 0, ≤ `MAX_CHECK_IN_INTERVAL` | Seconds between required check-ins |
| `initial_balance` | `i128` | `0` | ≥ 0 | Initial XLM deposit on vault creation |
| `beneficiary` | `Address` | — | Valid Stellar address | Address to receive funds on release |

**Recommended check-in intervals**:

| Use Case | Interval |
|---|---|
| Dead man's switch (urgent) | 7 days (`604800`) |
| Personal emergency fund | 90 days (`7776000`) |
| Long-term inheritance | 180–365 days |

---

### TTL & Check-in Parameters

| Parameter | Function | Default | Description |
|---|---|---|---|
| `min_checkin_cooldown` | `set_min_checkin_cooldown()` | `60` | Minimum seconds between check-ins (anti-spam) |
| `max_accelerate_seconds` | Built-in constant | `2592000` (30 days) | Max TTL reduction per `accelerate_ttl_decay()` call |
| `borrow_seconds` | `borrow_ttl()` | — | Seconds of TTL to borrow from another vault |

```rust
// Set minimum check-in cooldown (admin only)
contract.set_min_checkin_cooldown(300); // 5 minutes minimum between check-ins
```

---

### Beneficiary Parameters

| Parameter | Function | Default | Description |
|---|---|---|---|
| `minimum_threshold` | `set_beneficiary_minimum_threshold()` | `0` | Minimum vault balance for beneficiary to accept role |
| `acceptance_window` | — | — | Time window for beneficiary to accept role |
| `conflict_resolution_period` | — | — | Duration of the conflict resolution window |

```rust
// Require at least 100 XLM before beneficiary can accept
contract.set_beneficiary_minimum_threshold(vault_id, 100_0000000_i128);
```

---

### Vesting Parameters

| Parameter | Function | Validation | Description |
|---|---|---|---|
| `start_timestamp` | `set_vesting_schedule()` | ≥ current timestamp | When vesting begins |
| `duration_seconds` | `set_vesting_schedule()` | > 0 | Total vesting duration |
| `cliff_seconds` | `set_vesting_schedule()` | < `duration_seconds` | Time before any tokens vest |
| `total_amount` | `set_vesting_schedule()` | ≤ vault balance | Total amount to vest |

For full details, see [Vesting Schedules](vesting-schedules.md).

---

## Network Configurations (environments.toml)

The `environments.toml` file in the project root defines per-network RPC and passphrase defaults:

```toml
[testnet]
rpc_url = "https://soroban-testnet.stellar.org"
network_passphrase = "Test SDF Network ; September 2015"

[mainnet]
rpc_url = "https://mainnet.sorobanrpc.com"
network_passphrase = "Public Global Stellar Network ; September 2015"

[futurenet]
rpc_url = "https://rpc-futurenet.stellar.org"
network_passphrase = "Test SDF Future Network ; October 2022"

[standalone]
rpc_url = "http://localhost:8000/soroban/rpc"
network_passphrase = "Standalone Network ; February 2017"
```

**Precedence**: Values from `environments.toml` are overridden by environment variables (`STELLAR_RPC_URL`, `NETWORK_PASSPHRASE`).

---

## Docker Configuration

### docker-compose.yml

The base compose file defines production-like services:

| Service | Default Port | Description |
|---|---|---|
| `postgres` | `5432` | PostgreSQL database |
| `backend` | `3000` | Heirloom backend API |
| `stellar-quickstart` | `8000` | Local Stellar node |

### docker-compose.override.yml

Override file applies development-specific settings automatically when running `docker-compose up`:

- Uses `standalone` Stellar network
- Mounts source code as a volume for live-reload
- Relaxes authentication requirements
- Enables debug logging

**To run production mode without overrides**:

```bash
docker-compose -f docker-compose.yml up -d
```

---

## Configuration Validation

The backend validates required configuration at startup. Missing or invalid values produce a clear error:

```
ERROR heirloom_backend: Missing required config: CONTRACT_TTL_VAULT
ERROR heirloom_backend: Invalid DATABASE_URL: connection refused
```

### Required Variables Checklist

Before deploying, verify these are set:

- [ ] `STELLAR_NETWORK`
- [ ] `STELLAR_RPC_URL`
- [ ] `CONTRACT_TTL_VAULT`
- [ ] `DATABASE_URL`
- [ ] `JWT_SECRET` (min 32 characters)
- [ ] `PASSKEY_RP_ID`
- [ ] `PASSKEY_RP_ORIGIN`

### Optional but Recommended

- [ ] `REMINDER_EMAIL_API_KEY`
- [ ] `REMINDER_SMS_API_KEY`
- [ ] `SENTRY_DSN`
- [ ] `METRICS_ENABLED=true`

---

## Configuration Examples

### Testnet Development

```env
STELLAR_NETWORK=testnet
STELLAR_RPC_URL=https://soroban-testnet.stellar.org
CONTRACT_TTL_VAULT=<testnet-contract-id>
DATABASE_URL=postgres://heirloom:heirloom@localhost:5432/heirloom_dev
JWT_SECRET=dev-secret-not-for-production-use-only
PASSKEY_RP_ID=localhost
PASSKEY_RP_ORIGIN=http://localhost:3000
RUST_LOG=debug
METRICS_ENABLED=false
```

### Production Mainnet

```env
STELLAR_NETWORK=mainnet
STELLAR_RPC_URL=https://mainnet.sorobanrpc.com
CONTRACT_TTL_VAULT=<mainnet-contract-id>
DATABASE_URL=postgres://heirloom:strongpassword@db.internal:5432/heirloom_prod
JWT_SECRET=<32-byte-random-hex>
PASSKEY_RP_ID=yourdomain.com
PASSKEY_RP_ORIGIN=https://yourdomain.com
REMINDER_EMAIL_API_KEY=<key>
REMINDER_SMS_API_KEY=<key>
RUST_LOG=warn
METRICS_ENABLED=true
SENTRY_DSN=https://your-dsn@sentry.io/project
```

### Docker Compose Local

```env
STELLAR_NETWORK=standalone
STELLAR_RPC_URL=http://stellar-quickstart:8000/soroban/rpc
DATABASE_URL=postgres://heirloom:heirloom@postgres:5432/heirloom
PASSKEY_RP_ID=localhost
PASSKEY_RP_ORIGIN=http://localhost:3000
RUST_LOG=debug
```

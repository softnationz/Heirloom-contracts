# Passkey Integration

## Overview

Heirloom-Protocol uses Passkeys (WebAuthn) for authentication, eliminating seed phrase management.

## Why Passkeys?

- No seed phrases to lose or expose
- Biometric authentication (fingerprint, Face ID)
- Hardware-backed security
- Phishing-resistant

## Architecture (Planned)

1. **Frontend**: WebAuthn API for passkey creation and signing
2. **Smart Contract**: Verifies signatures via zk_verifier contract
3. **User Flow**:
   - Register passkey during vault creation
   - Sign check-ins with passkey
   - No private key exposure

## Current Status

Passkey integration is planned for v2.0. Current implementation uses standard Stellar address authentication.

## Future Implementation

- Store passkey public key in vault metadata
- Verify WebAuthn signatures on-chain
- Support multiple passkeys per vault

## Biometric Verification

Heirloom-Protocol supports biometric verification (fingerprint, face) as an enhanced check-in mechanism. Biometric credentials are stored as SHA-256 hash commitments — the raw biometric data never leaves the device.

### How It Works

1. Owner registers a biometric credential hash via `register_biometric`
2. On check-in, the owner presents the credential hash via `biometric_check_in`
3. The contract verifies the hash matches a registered credential
4. On success, `last_check_in` is reset and a `bio_ci` event is emitted

### Biometric API

```rust
bind_passkey_biometric(vault_id: u64, caller: Address, passkey_hash: BytesN<32>, credential_hash: BytesN<32>) -> Result<(), ContractError>
unbind_passkey_biometric(vault_id: u64, caller: Address, passkey_hash: BytesN<32>) -> Result<(), ContractError>
biometric_check_in(vault_id: u64, caller: Address, passkey_hash: BytesN<32>, credential_hash: BytesN<32>) -> Result<(), ContractError>
get_vault_biometrics(vault_id: u64) -> Vec<BiometricEntry>
is_valid_biometric(vault_id: u64, credential_hash: BytesN<32>) -> bool
```

### Events

| Topic | Data | Description |
|---|---|---|
| `bio_reg` | `credential_hash` | Biometric credential registered |
| `bio_rm` | `credential_hash` | Biometric credential removed |
| `bio_ci` | `(caller, timestamp)` | Biometric check-in performed |

### Security Properties

- Multiple credentials per vault (e.g., fingerprint + face ID)
- Duplicate registration is rejected with `InvalidPasskey`
- Only the vault owner can register or remove credentials
- Biometric check-in respects contract and vault pause state
- Raw biometric data is never stored on-chain — only the hash commitment
- Check-in on a Released vault is rejected with `AlreadyReleased`

## Passkey Expiry Enforcement (Issue #549)

Every call to `check_in`, `check_in_with_pow`, and `batch_check_in_v2` now
enforces two sequential validations before updating `last_check_in`:

### 1. Registration Check

| Passkey state | Behaviour |
|---|---|
| `VaultPasskeys` list non-empty | Hash **must** appear in the list; otherwise `InvalidPasskey` |
| List empty, `vault.passkey_hash` is `Some` | Hash **must** match the primary passkey; otherwise `InvalidPasskey` |
| Both empty/None | Any hash accepted (no passkey configured — backwards compatible) |

### 2. Expiry Check

If `extend_passkey_expiry` was previously called for this hash, the stored expiry
timestamp is compared with the current ledger time.  If `now > expiry`:

- A `pk_expd` event is emitted with the passkey hash.
- The call returns `PasskeyExpired` (error code 59) — distinct from `InvalidPasskey`.

### Errors

| Code | Name | Meaning |
|------|------|---------|
| 26 | `InvalidPasskey` | Passkey not registered for this vault |
| 59 | `PasskeyExpired`  | Passkey registration has expired |

## Passkey Compromise Detection (Issue #550)

See the dedicated compromise detection section below.

### `report_passkey_compromise`

```rust
fn report_passkey_compromise(
    env: Env,
    vault_id: u64,
    caller: Address,    // must be vault owner
    passkey_hash: BytesN<32>,
) -> Result<(), ContractError>
```

Manually flags a passkey as compromised. Subsequent `check_in` calls using that
hash return `PasskeyCompromised` (error 62).

### `clear_passkey_compromise`

```rust
fn clear_passkey_compromise(
    env: Env,
    vault_id: u64,
    caller: Address,    // must be vault owner
    passkey_hash: BytesN<32>,
) -> Result<(), ContractError>
```

Removes the compromise flag, allowing the passkey to be used again.

### `is_passkey_compromised`

```rust
fn is_passkey_compromised(env: Env, vault_id: u64, passkey_hash: BytesN<32>) -> bool
```

### Automatic Detection

During every `check_in`, the contract inspects the last 5 passkey usage entries.
If 3 or more **consecutive** entries used **different** passkey hashes, a
`pk_comp` event is emitted as an advisory alert (the check-in is not blocked).
Owners should monitor for this event and rotate or revoke suspected passkeys.

### Events

| Topic | Data | Emitted when |
|---|---|---|
| `pk_expd` | `passkey_hash` | Expired passkey used in check-in |
| `pk_comp` | `(vault_id, passkey_hash)` | Compromise detected or reported |

## On-chain Log Bounds and Full History (Issue fix)

### Bounded on-chain storage

Both the passkey usage log (`PasskeyUsage`) and the passkey audit log
(`PasskeyAuditLog`) are **capped at 1 000 entries** per vault on-chain:

| Log | Data key | Cap | Pruning strategy |
|-----|----------|-----|-----------------|
| Passkey usage | `PasskeyUsage(vault_id)` | 1 000 entries | Drop-oldest (index 0) |
| Passkey audit | `PasskeyAuditLog(vault_id)` | 1 000 entries | Drop-oldest (index 0) |

This matches the existing `timestamps.len() > 1000` snapshot cap applied
elsewhere in the contract.  The cap is enforced *before* the write on every
check-in / lifecycle operation, so storage size is strictly bounded regardless
of how many years a vault is actively used.

### Why the cap exists

Every check-in appends to both Vecs and writes the entire serialized entry back
to Soroban's persistent storage.  Without a cap, a long-lived vault that
check-ins daily for several years would accumulate thousands of entries, risk
exceeding Soroban's practical per-entry serialisation limit, and ultimately
brick check-ins — triggering premature vault release.

### Full history via events

The cap is a **storage concern only**.  Every check-in and lifecycle operation
continues to emit its event regardless of whether an on-chain entry was pruned:

| Event topic | What it covers |
|-------------|----------------|
| `pk_usage`  | Every passkey check-in, forever |
| `pk_audit`  | Every passkey add / remove / use lifecycle event, forever |

Off-chain indexers (e.g. Horizon event streaming) receive the full unbounded
event history.  The on-chain `get_passkey_usage` and `get_passkey_audit_log`
queries return only the **most recent ≤ 1 000 entries**.  If your application
needs the complete historical record, consume the event stream rather than
querying on-chain storage directly.

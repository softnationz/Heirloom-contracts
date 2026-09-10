# SBT Advanced Features: Compression, Fractional Ownership, and Escrow

This document covers the implementation of three advanced features for Soroban SBT (Soul-Bound Token) contracts:

1. **Metadata Compression** (Issue #26, superseding the legacy #47 format)
2. **Fractional Ownership** (Issue #45)
3. **SBT Escrow for Conditional Transfer** (Issue #46)

---

## 1. Metadata Compression

### Overview

SBT metadata can grow large when storing structured data (JSON, image URIs,
attributes, etc.). Metadata compression reduces on-chain storage costs with a
MessagePack extension containing a compact PackBits-style block stream.

### How It Works

#### Compression Strategy

The compressor creates direct-byte and delta-byte candidates. Each candidate
uses literal and repeated-byte blocks, and the smaller candidate is wrapped in
a MessagePack extension. If the complete framed value is not smaller than the
original metadata, the original bytes are returned unchanged.

#### Compression Format

```text
MessagePack extension:
  [0xC7][payload length: u8][type: 0x45][payload...]
  or
  [0xC8][payload length: u16 big-endian][type: 0x45][payload...]

Payload:
  [version: 0x01][mode: 0x00 direct | 0x01 delta][blocks...]

Block:
  Literal: [0LLLLLLL][1-128 literal bytes]
  Repeat:  [1RRRRRRR][one repeated byte]

Literal length = L + 1
Repeat length  = R + 3
```

The extension type `0x45` identifies Heirloom credential metadata. Ext8 is used
for payloads up to 255 bytes; larger payloads use Ext16.

### API

#### Compress and Decompress Bytes

```rust
pub fn compress_metadata(env: Env, metadata: Bytes) -> Bytes
pub fn decompress_metadata(env: Env, metadata: Bytes) -> Bytes
```

`decompress_metadata` accepts the current MessagePack extension, the legacy
`0xC1` delta/RLE representation, and ordinary uncompressed bytes.

#### Create (at mint time)

New SBTs are created with uncompressed metadata:

```rust
pub fn mint(env: Env, to: Address, metadata: String) -> u64 {
    // metadata is stored as-is (uncompressed)
}
```

#### Compress On-Demand

Compress an existing SBT's metadata:

```rust
pub fn compress_sbt_metadata(env: Env, sbt_id: u64) -> u64 {
    // Returns: size reduction (original_size - compressed_size)
    // Only owner can compress
    // Idempotent: already compressed SBTs return 0
    // Metadata remains unmodified when compression would increase its size
}
```

#### Read Metadata

```rust
pub fn get_metadata(env: Env, sbt_id: u64) -> String {
    // Preserves the existing String API and automatically decompresses
}

pub fn decompress_sbt_metadata(env: Env, sbt_id: u64) -> Bytes {
    // Returns: raw bytes (decompressed if compressed, as-is otherwise)
}
```

#### Check Compression Status

```rust
pub fn is_sbt_metadata_compressed(env: Env, sbt_id: u64) -> bool {
    // Returns: true if metadata is compressed
}
```

### Backward Compatibility

- Existing uncompressed metadata is not retroactively compressed
- New SBTs default to uncompressed format
- Compression is opt-in via `compress_sbt_metadata()`
- Decompression is automatic on read operations
- Metadata written with the legacy `0xC1` representation remains readable
- Unknown or ordinary bytes pass through decompression unchanged

### Storage Savings

The focused storage benchmark uses 64 repeated metadata bytes:

| Representation | Bytes |
| --- | ---: |
| Original | 64 |
| MessagePack extension | 7 |
| Saved | 57 |
| Savings | 89.06% |

Savings depend on the input. Short or non-repeating metadata remains
uncompressed when the MessagePack envelope would be larger.

---

## 2. Fractional Ownership

### Overview

SBTs are traditionally all-or-nothing owned. Fractional ownership enables multiple holders to co-own a single SBT, each with a proportional stake.

Use cases:
- **Collective credentials**: Teams, DAOs, organizations
- **Shared attributes**: Multi-signature approvals required
- **Inheritance planning**: Estate distributed among heirs

### How It Works

#### Fractions Representation

Ownership is expressed in basis points (0-10000, where 10000 = 100%):

```rust
pub struct FractionalOwnership {
    pub sbt_id: u64,
    pub holders: Vec<Address>,
    pub fractions: Vec<u64>,  // e.g., [5000, 3000, 2000] = 50%, 30%, 20%
    pub created_at: u64,
}
```

#### Unanimous Approval Requirement

Any operation affecting the SBT (e.g., delegation, escrow) requires authentication from all fractional owners:

```rust
// All holders must authorize operations
for holder in fractional_ownership.holders.iter() {
    holder.require_auth();
}
```

#### Ownership History

Every fractional ownership change is recorded for audit:

```rust
pub struct OwnershipHistoryEntry {
    pub sbt_id: u64,
    pub holder: Address,
    pub fraction: u64,
    pub action: OwnershipAction,  // Created, Updated, Removed
    pub at: u64,
}
```

### API

#### Create Fractional SBT

```rust
pub fn create_fractional_sbt(
    env: Env,
    sbt_id: u64,
    holders: Vec<Address>,
    fractions: Vec<u64>,
) -> u64 {
    // Current owner must authorize (before becoming fractional)
    // Fractions must sum to 10000
    // Returns: sbt_id
    
    // Example: 3-way split (50%, 30%, 20%)
    // holders = [alice, bob, charlie]
    // fractions = [5000, 3000, 2000]
}
```

#### Query Fractional Ownership

```rust
pub fn get_fractional_ownership(env: Env, sbt_id: u64) -> Option<FractionalOwnership> {
    // Returns: fractional record or None if not fractional
}

pub fn is_fractional(env: Env, sbt_id: u64) -> bool {
    // Returns: true if SBT is fractionally owned
}
```

### Constraints

1. **Sum validation**: All fractions must total exactly 10000
2. **Unanimous approval**: All holders must approve operations
3. **Array matching**: holders and fractions must be same length
4. **Non-empty**: At least one holder required

### Example Workflow

```
1. Alice owns SBT #42 (verified credential)
2. Alice calls create_fractional_sbt(42, [bob, charlie], [6000, 4000])
3. Now SBT #42 is owned 60% by Bob, 40% by Charlie
4. Any operation on #42 requires both Bob and Charlie to sign
5. Ownership history is immutable audit trail
```

---

## 3. SBT Escrow for Conditional Transfer

### Overview

Escrow enables conditional transfer of SBTs. An SBT is held by an escrow agent pending satisfaction of conditions (e.g., payment received, conditions met, time elapsed).

Use cases:
- **Conditional asset transfer**: "Release credential once payment confirmed"
- **Contingent delegation**: "Grant access pending approval"
- **Time-locked release**: "SBT released after X days"
- **Multi-party workflows**: Complex transfer negotiations

### How It Works

#### Escrow Record

```rust
pub struct EscrowRecord {
    pub escrow_id: u64,
    pub sbt_id: u64,
    pub escrow_agent: Address,
    pub conditions: Bytes,        // Opaque condition encoding
    pub created_at: u64,
    pub released: bool,
}
```

#### Release Mechanism

1. **Condition encoding**: Conditions are stored as opaque bytes, allowing flexibility in validation
2. **Proof submission**: Escrow agent provides proof that conditions are met
3. **Atomic release**: On validation, SBT is released atomically

#### Atomic Multi-Credential Release

```rust
pub fn atomic_release_credentials(
    env: Env,
    credential_ids: Vec<u64>,
) -> Vec<bool>
```

`atomic_release_credentials` releases multiple escrowed SBT credentials in a
single contract invocation. Each distinct escrow agent must authorize the
batch, serving as that agent's attestation that the corresponding escrow
conditions have been satisfied.

The contract validates the complete input before changing storage. Empty
batches, duplicate credential IDs, missing escrow records, credentials that
were already released, or missing agent authorization abort the invocation.
Soroban then rolls back the invocation, so no credential in a failed batch is
released and no release event from that invocation is committed.

On success, the function releases every credential, emits the existing escrow
release event for each one, and returns a vector containing `true` for every
input credential in the same order.

### API

#### Enter Escrow

```rust
pub fn escrow_sbt(
    env: Env,
    sbt_id: u64,
    escrow_agent: Address,
    conditions: Bytes,
) -> u64 {
    // Only current owner can place SBT in escrow
    // SBT cannot already be in escrow
    // Returns: escrow_id
    
    // Example: Release SBT if payment hash matches
    // conditions = encoded_condition_payload
}
```

#### Release from Escrow

```rust
pub fn release_sbt_from_escrow(
    env: Env,
    sbt_id: u64,
    proof: Bytes,
) {
    // Only escrow_agent can release
    // Proof must satisfy conditions (validated against conditions field)
    // Atomically marks escrow as released
    
    // Note: In production, proof validation would be more rigorous,
    // checking against stored conditions, ZK proofs, etc.
}
```

#### Query Escrow Status

```rust
pub fn get_escrow_status(env: Env, sbt_id: u64) -> Option<EscrowRecord> {
    // Returns: escrow record or None if not in escrow
}
```

### Constraints

1. **Single escrow per SBT**: An SBT can only be in one escrow at a time
2. **Agent-only release**: Only the designated escrow agent can release
3. **Non-empty proof**: Proof must be provided (placeholder for actual validation)
4. **Immutable conditions**: Conditions are set at escrow creation time

### Example Workflow

```
1. Alice owns credential SBT #50 (professional license)
2. Alice wants to transfer to Bob, but Bob hasn't paid yet
3. Alice calls escrow_sbt(50, bob_escrow_agent, payment_conditions)
4. SBT #50 is now in escrow, held by bob_escrow_agent
5. Bob pays the escrow agent
6. Escrow agent calls release_sbt_from_escrow(50, payment_proof)
7. SBT #50 is released and can be transferred to Bob
8. Event ESCROW_RELEASED is emitted for audit trail
```

---

## Batch Credential Verification with Consistency Checks

### Overview (ZK Verifier Contract)

The ZK Verifier contract now supports batch credential verification with conflict detection. When multiple credentials are verified simultaneously, the contract checks for consistency conflicts (e.g., mutually exclusive claims).

### Conflict Rules

Conflicts are defined per credential type. Examples:

#### Age Range Conflicts

Two age credentials conflict if their ranges don't overlap:

```
[18, 65] and [21, 100] → Compatible (overlap)
[18, 20] and [21, 100] → Conflict (disjoint)
```

#### KYC Status Conflicts

KYC credentials conflict if statuses are mutually exclusive:

```
Pending + Approved → Compatible
Pending + Rejected → Conflict (contradictory)
Approved + Rejected → Conflict (contradictory)
```

### API

```rust
pub fn verify_credentials_consistent(
    env: Env,
    proofs: Vec<Bytes>,
    claims: Vec<Bytes>,
) -> bool {
    // Verifies all credentials are valid (via verify_claim)
    // Checks pairwise consistency between all claims
    // Returns: true if all valid and consistent, false otherwise
    
    // Panics if proofs and claims lengths don't match
}
```

### Implementation

1. **Individual verification**: Each credential is verified via `verify_claim`
2. **Pairwise checking**: All credential pairs are checked for compatibility
3. **Type detection**: Credential type is inferred from claim structure
4. **Conflict reporting**: Detailed conflict reason is logged for audit

### Extensibility

New conflict rules can be added by implementing the `ConflictRule` trait:

```rust
pub trait ConflictRule {
    fn are_compatible(env: &Env, claim_a: &Bytes, claim_b: &Bytes) -> bool;
    fn conflict_reason(env: &Env) -> Bytes;
}
```

Example: Geographic credentials

```rust
pub struct GeographicConflictRule;

impl ConflictRule for GeographicConflictRule {
    fn are_compatible(env: &Env, claim_a: &Bytes, claim_b: &Bytes) -> bool {
        // Parse jurisdiction from claims
        // Return true if jurisdictions are compatible
    }
    
    fn conflict_reason(_env: &Env) -> Bytes {
        Bytes::from_slice(_env, b"Jurisdiction mismatch")
    }
}
```

---

## Integration Points

### SBT Contract

- **Metadata Compression**: Reduces storage on every SBT
- **Fractional Ownership**: Enables multi-party credentials
- **Escrow**: Supports conditional credential transfer

### ZK Verifier Contract

- **Batch Verification**: Efficiently verify multiple credentials
- **Consistency Checking**: Detect conflicting claims before operations

### Backend

- **Compression Analytics**: Track compression ratios per SBT
- **Fractional Approval Workflow**: Coordinate multi-party signing
- **Escrow Automation**: Monitor escrow completion and settlement

---

## Testing Strategy

### Unit Tests

- Compression round-trip (compress → decompress → verify)
- Fractional fraction validation (sums to 10000)
- Escrow state transitions (active → released)
- Conflict detection (matching rules)

### Integration Tests

- Multi-holder voting on fractional SBT operations
- Escrow with cascading conditions
- Batch verification with mixed credential types
- Compression with various metadata sizes

### Performance Benchmarks

- Compression ratio on real metadata samples
- Decompression cost (gas)
- Fractional ownership scaling (N holders)
- Consistency checking O(N²) complexity

---

## Security Considerations

### Compression

- **Decompression bomb protection**: Maximum output size enforced
- **Magic prefix validation**: Ensures correct format
- **Backward compatibility**: Uncompressed data remains readable

### Fractional Ownership

- **Unanimous approval**: All holders must authorize operations
- **No partial ownership changes**: Can only create or remove fractions (not modify in-place)
- **Immutable history**: Ownership changes are append-only

### Escrow

- **Single agent control**: Only escrow_agent can release
- **Atomic transitions**: State changes are all-or-nothing
- **Non-expiring escrows**: No built-in timeout (can be added if needed)

### Consistency Checking

- **Type inference**: May misclassify credentials; defaults to compatible on mismatch
- **Extensible rules**: New rule implementations must be carefully audited
- **O(N²) complexity**: Batch size should be reasonable (e.g., < 100 credentials)

---

## Roadmap

### Short-term (v1.1)

- [ ] Escrow expiration and automatic release
- [ ] Fractional ownership modification (rebalancing)
- [ ] More sophisticated condition validation for escrow

### Medium-term (v2.0)

- [ ] Hierarchical escrow (escrow of escrowed SBTs)
- [ ] Batch fractional transfers (multiple SBTs to multiple holders)
- [ ] Native governance for fractional SBTs (voting on operations)

### Long-term (v3.0)

- [ ] Auction-based escrow resolution
- [ ] Fractional SBT market (trading fractions)
- [ ] ZK proof of commitment to fractional operations

---

## Related Issues

- **#33**: Implement Batch Credential Verification with Consistency Checks
- **#45**: Implement SBT Fractional Ownership
- **#46**: Add SBT Escrow for Conditional Transfer
- **#47**: Implement SBT Metadata Compression

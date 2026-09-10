# ZK Verifier Contract: Stub Implementation & Migration Path

## Overview

The `zk_verifier` contract in Heirloom-Protocol is currently a **stub implementation** designed to validate the architectural integration and provide a path toward full zero-knowledge proof verification in future versions.

This document explains:
1. Why it is a stub
2. Current security properties and limitations
3. What a real ZK implementation requires
4. Roadmap for full ZK support

---

## Current Stub Implementation

### What the Stub Does

The stub `zk_verifier` contract provides:

- **Oracle attestation registration**: Trusted oracles can register and publish attestations
- **Proof/claim validation**: Performs basic input validation and sentinel checks
- **Event emission**: Publishes verification results to the network
- **SHA-256 hashing**: Stores digest-based proofs to minimize on-chain storage

### Signature of Key Functions

```rust
/// Initialize the contract with an admin address.
pub fn initialize(env: Env, admin: Address)

/// Register a trusted oracle. Admin only.
pub fn register_oracle(env: Env, oracle: Address)

/// Revoke a trusted oracle. Admin only.
pub fn revoke_oracle(env: Env, oracle: Address)

/// Returns whether the given address is a registered oracle.
pub fn is_oracle(env: Env, oracle: Address) -> bool

/// An oracle publishes an attestation that `proof` is valid for `claim`.
/// Returns the attestation's stable credential_id.
pub fn attest(env: Env, oracle: Address, proof: Bytes, claim: Bytes) -> u64

/// Verifies a zero-knowledge proof against a claim using oracle attestation.
pub fn verify_claim(env: Env, proof: Bytes, claim: Bytes) -> bool

/// Files a dispute against a credential (an attestation, addressed by the
/// id returned from `attest`). Returns the new dispute_id.
pub fn initiate_credential_dispute(env: Env, credential_id: u64, initiator: Address, reason: Bytes) -> u64

/// A registered oracle votes on an open dispute: `true` asserts the
/// credential is invalid, `false` asserts it remains valid.
pub fn vote_on_dispute(env: Env, dispute_id: u64, voter: Address, vote: bool)

/// Returns the full record for a dispute.
pub fn get_dispute(env: Env, dispute_id: u64) -> Dispute

/// Returns every dispute id ever filed against a credential, oldest first.
pub fn get_credential_disputes(env: Env, credential_id: u64) -> Vec<u64>

/// Returns whether a credential has been invalidated by an upheld dispute.
pub fn is_credential_invalidated(env: Env, credential_id: u64) -> bool

/// Returns the number of concurring oracle votes needed to resolve a dispute.
pub fn dispute_threshold(env: Env) -> u32

/// Sets the number of concurring oracle votes needed to resolve a dispute. Admin only.
pub fn set_dispute_threshold(env: Env, threshold: u32)

/// Returns the credential's attestation state as of `timestamp` (the most
/// recent snapshot at or before it), or `None` if no such snapshot exists.
/// `requester` must authorize the call and be permitted to view the
/// credential under its current PrivacyLevel.
pub fn get_credential_at_time(env: Env, requester: Address, credential_id: u64, timestamp: u64) -> Option<CredentialSnapshot>

/// Sets the PrivacyLevel governing who may read a credential's state via
/// `get_credential_at_time`. Admin only.
pub fn set_credential_privacy(env: Env, credential_id: u64, level: PrivacyLevel)

/// Returns a credential's current PrivacyLevel (defaults to `Public`).
pub fn credential_privacy(env: Env, credential_id: u64) -> PrivacyLevel

/// Returns the credential's current AttestationRecord (its attesting
/// oracle and stable credential_id), gated on the caller's authorization
/// under its PrivacyLevel. Unauthorized callers at Confidential receive a
/// redacted record whose oracle field is masked out (recorded via
/// MaskingConfig); unauthorized callers at Internal are denied with
/// AccessDenied.
pub fn get_attestation(env: Env, requester: Address, credential_id: u64) -> Option<AttestationRecord>

/// Returns the credential's recorded state as of a specific version number
/// (1-based, monotonically increasing, never reused), or `None` if that
/// version was never recorded or has since been pruned. Same PrivacyLevel
/// gating as `get_credential_at_time`.
pub fn get_credential_version(env: Env, requester: Address, credential_id: u64, version: u32) -> Option<CredentialSnapshot>

/// Returns the latest version number recorded for a credential (0 if it has
/// never been attested), regardless of whether earlier versions have since
/// been pruned.
pub fn credential_version_count(env: Env, credential_id: u64) -> u32

/// Compares two recorded versions of a credential and reports what changed
/// between them. Panics with VersionNotFound if either version is unknown.
/// Same PrivacyLevel gating as `get_credential_at_time`.
pub fn diff_credential_versions(env: Env, requester: Address, credential_id: u64, from_version: u32, to_version: u32) -> CredentialVersionDiff

/// Attests (proof, claim) as a credential derived from `parent_id`.
/// Recursively validates parent_id's entire ancestor chain before issuing;
/// panics if any ancestor is invalidated, the chain is too deep, the
/// derived credential would be its own parent, or this (proof, claim) pair
/// was already derived from a different parent. Returns the credential_id.
pub fn create_derived_credential(env: Env, oracle: Address, parent_id: u64, proof: Bytes, claim: Bytes) -> u64

/// Returns the immediate parent of a credential, or None if it is a root.
pub fn get_credential_parent(env: Env, credential_id: u64) -> Option<u64>

/// Returns a credential's full ancestry, starting with itself and walking
/// up to its root: [credential_id, parent, grandparent, ...].
pub fn get_credential_chain(env: Env, credential_id: u64) -> Vec<u64>

/// Returns whether a credential and every one of its ancestors is
/// currently valid (none invalidated by an upheld dispute).
pub fn is_credential_chain_valid(env: Env, credential_id: u64) -> bool

/// Returns whether the credential's scheduled consistency re-check is due
/// (now >= next_check_due), publishing a `cons_due` event with
/// (credential_id, next_check_due) when it is. Never-attested ids are never due.
pub fn is_consistency_check_due(env: Env, credential_id: u64) -> bool

/// Advances a credential's consistency-check window by one full
/// CONSISTENCY_CHECK_INTERVAL from the current timestamp, after a completed
/// re-check. Panics with CredentialNotFound if never attested. Anyone may call it.
pub fn reschedule_consistency_check(env: Env, credential_id: u64)
```

### Current Verification Logic

The `verify_claim` function:

1. **Validates input bounds**:
   - Rejects empty or oversized proofs (max 4 KB)
   - Rejects empty or oversized claims (max 1 KB)

2. **Applies sentinel check**:
   - Returns `false` if proof is exactly `0x00` (known-invalid sentinel)
   - Returns `true` for all other non-empty proofs

3. **Computes claim hash**:
   - SHA-256 digest of the claim bytes
   - Published in a `vfy_claim` event for off-chain indexing

4. **Emits event**:
   ```
   vfy_claim: (result: bool, claim_hash: BytesN<32>)
   ```

### Security Limitations of the Stub

⚠️ **The stub is NOT suitable for production use where actual proof verification matters.**

| Aspect | Stub Behavior | Security Impact |
|--------|---------------|-----------------|
| **Proof Verification** | Accepts all non-0x00 proofs as valid | Anyone can forge valid "proofs" |
| **Cryptographic Soundness** | None (sentinel-based only) | Claims can be falsely validated |
| **Oracle Trust Model** | Attestations stored but not used | Malicious oracles can bypass checks |
| **Claim Authentication** | Stored as SHA-256 digest only | No binding between claim and proof |
| **Replay Protection** | None | Same proof/claim can be reused indefinitely |

---

## Lattice-Tagged Proofs & Field Masking

`verify_lattice_proof` and `mask_proof_fields` are separate from the stub
verification described above; this section documents what they actually do,
since earlier revisions shipped them undocumented and, in `verify_lattice_proof`'s
case, under a rustdoc comment calling it a "quantum-resistant" proof check.

### `verify_lattice_proof`

Despite the name, **this crate implements no lattice-based (e.g.
Dilithium/Falcon-style, post-quantum) cryptographic scheme**, and
`verify_lattice_proof` performs no such verification. What it actually does:

1. Requires `proof` to carry the `LATTICE_V1` format header
   (`LATTICE_PROOF_HEADER`) followed by a 4-byte checksum (the first 4 bytes
   of `sha256` over everything before it) — see `is_valid_lattice_proof`.
   This rejects accidental or naively-forged input; it is a structural tag,
   not a proof of any mathematical statement.
2. Requires the exact `proof` bytes to have been attested by a
   currently-registered oracle for `claim` — the same oracle-attestation
   trust model `verify_claim` uses. This is the actual trust boundary: a
   `true` result means "a registered oracle vouched for these bytes,"
   nothing more.

If genuine post-quantum proof verification is ever needed, it requires a
real lattice signature scheme (see "What a Real ZK Implementation
Requires" below) — `verify_lattice_proof` is not a step toward that on its
own, only a format tag on top of the existing oracle-attestation model.

### `mask_proof_fields`

Given `fields_to_mask` (byte offsets into `proof`, each `< 32` and `<
proof.len()`), returns `b"MASKED_V1" || field_mask (u32 LE) || proof` with
every masked offset's byte zeroed out. The output does not contain the
original byte at any masked offset — inspecting it does not recover masked
data. `verify_masked_proof` then checks the *masked* output against oracle
attestation, exactly like `verify_claim`.

### Additional error codes

| Code | Name | Description |
|------|------|-------------|
| 17 | InvalidLatticeProof | `proof` does not carry a valid LATTICE_V1 header + checksum |
| 18 | ExternalFormatTooLarge | Exported/masked proof format exceeds MAX_EXTERNAL_FORMAT_SIZE (8 KB) |
| 19 | InvalidMaskSpec | `fields_to_mask` was empty, referenced an out-of-range offset, or `masked_proof` was too short |
| 20 | MaskedVerificationFailed | The masked proof was not attested by a currently-registered oracle |

> **Note on this document's other error codes:** the table below (and the
> Credential Dispute Resolution / Temporal Queries / Version History /
> Hierarchies / Privacy Levels sections that follow) describe a larger
> public API — `initiate_credential_dispute`, `vote_on_dispute`,
> `get_credential_at_time`, `get_credential_parent`,
> `is_credential_chain_valid`, and related functions — that is **not
> present in `src/lib.rs`** as of this revision. That gap predates and is
> unrelated to the lattice/masking fix described above (`src/test.rs` has
> pre-existing compile errors referencing the same missing API); reconciling
> it is a separate, larger effort than this document's scope here covers.

---

## Credential Dispute Resolution

Oracle attestations are, in effect, credentials asserting that a `(proof,
claim)` pair is valid. Trust in a single attesting oracle is not always
enough — `attest` now assigns every attestation a stable `credential_id`, and
that credential can be formally disputed:

1. **Initiate**: Anyone may call `initiate_credential_dispute(credential_id,
   initiator, reason)` to challenge a credential. Only one dispute may be
   open per credential at a time. Returns a `dispute_id`.
2. **Vote**: Registered oracles call `vote_on_dispute(dispute_id, voter,
   vote)` — `vote = true` asserts the credential is invalid, `vote = false`
   asserts it remains valid. Each oracle gets one vote per dispute.
3. **Resolve**: Once either side reaches `dispute_threshold()` (default `3`,
   configurable by the admin via `set_dispute_threshold`), the dispute
   resolves automatically:
   - **Upheld** (invalid votes win): the credential is marked invalidated.
     `verify_claim` now returns `false` for it, even if the attesting oracle
     is still registered.
   - **Rejected** (valid votes win): the credential is unaffected; a new
     dispute may be filed later if new evidence emerges.
4. **History**: `get_credential_disputes(credential_id)` returns every
   dispute ever filed against a credential, in order, so past challenges
   remain auditable even after resolution.

This is deliberately a lighter-weight trust mechanism than oracle
registration itself — voting rights are tied to the existing oracle
allowlist rather than a separate credential-dispute-specific role, so no new
admin surface is introduced beyond the threshold setting.

---

## Credential Temporal Queries & Retention Policy

Every other credential query (`is_credential_invalidated`,
`get_credential_disputes`, ...) answers "what is true *right now*?" —
there was previously no way to ask "was this credential valid on Jan 1?"
after the fact. `get_credential_at_time(credential_id, timestamp)` answers
exactly that.

### How it works

1. **Snapshot on every state change**: whenever a credential's attestation
   state changes — a fresh `attest` call (including re-attestation, which
   can change the attesting oracle) or a dispute against it resolving
   (`Upheld` or `Rejected`) — the contract records a `CredentialSnapshot`
   (`credential_id`, `oracle`, `invalidated`, `timestamp`) at the current
   ledger timestamp. This happens automatically; there is no separate
   "take a snapshot" call to remember to make.
2. **Index per credential**: each credential keeps an ascending list of the
   ledger timestamps it has a snapshot at
   (`DataKey::CredentialSnapshotTimestamps`). Because Soroban ledger close
   time is monotonically non-decreasing and snapshots are only ever
   appended, this list is always sorted — no explicit sort step is needed.
3. **Lookup**: `get_credential_at_time` binary-searches that index for the
   rightmost timestamp `<= timestamp` (O(log n) in the number of retained
   snapshots for that credential) and returns the snapshot stored there, or
   `None` if every snapshot postdates the query (including when the
   credential has no snapshots at all). If two state changes land at the
   same ledger timestamp — e.g. a dispute is filed and resolved without any
   ledger-time advance — the later one overwrites the snapshot at that
   timestamp rather than creating a duplicate index entry.

### Retention policy

Snapshots are stored in **persistent** storage (unlike the rest of this
contract's state, which lives in instance storage) since they are
write-once, append-only history rather than live state — this mirrors the
`VaultSnapshot` pattern in `ttl_vault`. To bound persistent-storage growth,
each credential retains at most `MAX_CREDENTIAL_SNAPSHOTS` (**1000**)
snapshots: once a credential's snapshot count would exceed that, the
oldest snapshot is pruned before the newest one is recorded.

Practical implications:

- A credential that changes state at most once per ledger close needs
  1000 state changes (attestations/re-attestations plus dispute
  resolutions) before its earliest history is pruned — in practice, far
  more ledger closes than that, since state changes are comparatively
  infrequent per credential.
- Once pruned, a query for a timestamp older than the oldest retained
  snapshot returns `None` — the same result as if the credential never
  existed at that time. Callers that need unbounded history should index
  the `disp_res`/`attest` activity off-chain (e.g. via an indexer watching
  contract events) rather than relying on on-chain retention.
- This cap is per-credential, not global — a busy credential does not
  starve the retention budget of any other credential.

---

## Credential Version History

`get_credential_at_time` answers "what was true at timestamp T?"; it does
not give an audit trail a stable identity — an off-chain caller cannot say
"show me version 3" and get the same answer regardless of what has been
pruned around it, because timestamps are not renumbered but *are*
context-dependent (you need to already know roughly when a change
happened). Credential versions give every recorded state change a stable,
gap-free, 1-based number instead.

### Version semantics

- **Numbering**: the first recorded state for a credential (its initial
  `attest`) is version 1. Each subsequent state change — a re-attestation
  that changes the attesting oracle, or a dispute resolving — increments
  the version by exactly one. Versions are per-credential; credential 1's
  version 3 and credential 2's version 3 are unrelated.
- **Stability**: a version number is assigned once and never reassigned,
  even after retention prunes the snapshot it identifies. Once pruned,
  `get_credential_version` for that version returns `None` — it never
  starts pointing at a different state, unlike, hypothetically, a
  position-based index would.
- **Same-timestamp changes do not create new versions**: if two state
  changes land at the same ledger timestamp (e.g. a dispute filed and
  resolved without any ledger-time advance), they share one version number,
  matching the snapshot-overwrite behavior described above under
  "Credential Temporal Queries". A version therefore corresponds 1:1 with a
  *distinct retained timestamp*, not with "every call that touched credential
  state."
- **Storage**: versions are not a parallel history store. They are a second,
  parallel index (`DataKey::CredentialSnapshotVersions`) over the same
  `CredentialSnapshot` records `get_credential_at_time` already uses, kept
  in lockstep with `DataKey::CredentialSnapshotTimestamps` — same length,
  same retention bound (`MAX_CREDENTIAL_SNAPSHOTS`), same pruning behavior.
  No credential state is duplicated on-chain to support version lookups.

### Usage

```rust
// version 1 is always the credential's original attested state (until/unless
// pruned).
let v1 = client.get_credential_version(&requester, &credential_id, &1u32);

// How many versions has this credential gone through?
let latest = client.credential_version_count(&credential_id);

// What changed between the original attestation and its current state?
let diff = client.diff_credential_versions(&requester, &credential_id, &1u32, &latest);
if diff.oracle_changed {
    // diff.previous_oracle -> diff.current_oracle
}
if diff.invalidated_changed {
    // diff.previous_invalidated -> diff.current_invalidated
}
```

`diff_credential_versions` panics with `VersionNotFound` (#17) if either
`from_version` or `to_version` was never recorded or has since been pruned.
`from_version` and `to_version` need not be adjacent or chronologically
ordered — diffing version 3 against version 1 is valid and simply reports
the same fields with signs effectively reversed.

Both `get_credential_version` and `diff_credential_versions` require
`requester` to authorize the call and enforce the same [`PrivacyLevel`]
access check as `get_credential_at_time` (see "Credential Privacy Levels"
below) — version history is exactly as sensitive as the state it records.
`credential_version_count` is not privacy-gated, since a bare version count
does not expose any oracle or invalidation data, mirroring
`get_credential_disputes`'s ungated dispute count.

---

## Credential Hierarchies

Some credentials are only meaningful in light of another credential they
were built on — a certificate issued on the basis of a degree, which was
itself issued on the basis of a transcript. Before this feature, `attest`
had no way to express that relationship: every credential was a standalone
root with no notion of provenance.

`create_derived_credential(oracle, parent_id, proof, claim)` attests a new
credential exactly like `attest` (same dedup rule: identical `(proof,
claim)` bytes always map to the same stable `credential_id`), but also
records `parent_id` as its parent.

### How it works

1. **Parent reference**: each credential's parent (if any) is stored as
   `DataKey::CredentialParent(child_id) -> parent_id`, set the first time a
   credential_id is derived and never reassigned afterward — a credential's
   place in the hierarchy is fixed at creation.
2. **Recursive validation before issuance**: before minting or re-attesting
   the derived credential, `create_derived_credential` walks `parent_id`'s
   *entire* ancestor chain — not just its immediate state — via
   `validate_credential_chain`, checking every credential in that chain
   (`parent_id` itself, its parent, its grandparent, and so on) for
   invalidation. A single directly-invalidated ancestor anywhere in the
   chain blocks new issuance, even if the immediate parent was never
   itself disputed. This is what makes the validation *recursive* rather
   than a single-level check.
3. **Depth bound**: the ancestor walk is capped at
   `MAX_CREDENTIAL_CHAIN_DEPTH` (**32**) hops. A parent chain that would
   require more hops than that to validate is rejected with
   `CredentialChainTooDeep`, keeping chain-walk cost — and the cost of
   `get_credential_chain` / `is_credential_chain_valid` queries over it —
   bounded regardless of how deep a malicious or accidental chain grows.

### Usage

```rust
// A transcript is a root credential — no parent.
let transcript_id = client.attest(&oracle, &transcript_proof, &transcript_claim);

// A degree is derived from the transcript. This panics if the transcript
// (or anything above it) has been invalidated by an upheld dispute.
let degree_id = client.create_derived_credential(
    &oracle, &transcript_id, &degree_proof, &degree_claim,
);

// A certificate derived from the degree — three levels deep.
let cert_id = client.create_derived_credential(
    &oracle, &degree_id, &cert_proof, &cert_claim,
);

// Full ancestry, most-derived first: [cert_id, degree_id, transcript_id].
let chain = client.get_credential_chain(&cert_id);

// True only if cert_id and every one of its ancestors is currently valid.
let still_valid = client.is_credential_chain_valid(&cert_id);
```

If the transcript is later disputed and invalidated, `is_credential_chain_valid`
for the degree or certificate returns `false` even though neither was
directly disputed — and any further `create_derived_credential` call built
on the degree panics with `ParentCredentialInvalid`, since the compromised
ancestor makes the whole chain untrustworthy.

### Error conditions

`create_derived_credential` panics with:

- `CredentialNotFound` (#8) — `parent_id` was never attested.
- `ParentCredentialInvalid` (#18) — `parent_id` or any of its ancestors has
  been invalidated by an upheld dispute.
- `CredentialChainTooDeep` (#19) — `parent_id`'s ancestor chain already
  exceeds `MAX_CREDENTIAL_CHAIN_DEPTH` hops.
- `SelfReferentialParent` (#20) — `proof`/`claim` hash to the same
  credential_id as `parent_id` (a credential cannot be its own parent).
- `ParentAlreadySet` (#21) — this exact `(proof, claim)` pair was already
  derived from a *different* parent; re-deriving it under a new parent is
  rejected rather than silently rewriting the hierarchy. Re-deriving it
  under the *same* parent is a no-op, matching `attest`'s re-attestation
  idempotency.

`create_derived_credential` does not itself gate on privacy or add any new
gating to `verify_claim` — it only governs *issuance*. `is_credential_chain_valid`
and `get_credential_chain` are not privacy-gated (mirroring
`credential_version_count` and `get_credential_disputes`), since they only
expose id relationships and a boolean, not oracle or invalidation detail.

---

## Credential Privacy Levels

By default every credential is equally visible to anyone who knows its
`credential_id` — `get_credential_at_time` never checked who was asking.
`PrivacyLevel` lets the admin restrict that on a per-credential basis:

- **`Public`** (default): anyone may read the credential's state.
- **`Internal`**: only the admin or a currently-registered oracle may read
  it — not necessarily the oracle that attested it, any registered oracle.
- **`Confidential`**: only the admin may read it.

### Usage

```rust
// Admin-only: restrict credential 1 to internal readers.
client.set_credential_privacy(&1u64, &PrivacyLevel::Internal);

// Anyone can check the configured level.
let level = client.credential_privacy(&1u64);

// Reads are now access-controlled: `requester` must authorize the call
// (so it cannot be spoofed) and must satisfy the credential's privacy
// level, or the call panics with AccessDenied (#16).
let snapshot = client.get_credential_at_time(&requester, &1u64, &timestamp);
```

`set_credential_privacy` panics with `CredentialNotFound` (#8) if
`credential_id` was never attested, mirroring `initiate_credential_dispute`.

### Attestation query path (`get_attestation`)

The credential-level gating above also applies to the attestation query
path itself: `get_attestation(requester, credential_id)` returns the
credential's current `AttestationRecord` — the attesting oracle and its
stable `credential_id`. `requester` must authorize the call (so the
caller cannot be spoofed) and must satisfy the credential's
`PrivacyLevel`:

| PrivacyLevel | Authorized callers | Unauthorized callers |
|--------------|--------------------|----------------------|
| `Public` | anyone | — (everyone is authorized) |
| `Internal` | admin, any registered oracle | panics with `AccessDenied` (#21) |
| `Confidential` | admin | receive a **redacted** record |

At `Confidential`, an unauthorized caller is *not* denied outright —
denying would itself confirm the credential exists. Instead the caller
receives a redacted `AttestationRecord` whose `oracle` field is masked
out (replaced with the all-zero Ed25519 account
`GAAAA...WHF`, a strkey that is not a usable Stellar account and so can
never be mistaken for a genuine attesting oracle), while `credential_id`
is kept so the record still identifies itself. The redaction is recorded
on-chain in a `MaskingConfig` — the same type `mask_proof_fields` uses
for proof-field masking — stored under `DataKey::AttestationMasking`
keyed by the credential, so exactly what was masked remains auditable.

> **Terminology note:** this enforcement model is the "Public /
> Restricted / Private" model by another name — the codebase calls the
> middle tier `Internal` and the most restrictive tier `Confidential`.
> "Restricted" readers are the admin and registered oracles; "Private"
> records are readable in full only by the admin and are redacted for
> everyone else.

### What this does and does not cover

Privacy filtering applies to `get_attestation` (the attestation query
path) and, once the temporal-query API described above is restored, to
`get_credential_at_time`, `get_credential_version`, and
`diff_credential_versions` — every query that exposes a credential's full
attestation state (oracle, invalidated flag, timestamp). It intentionally
does **not** gate `verify_claim`: that call
already requires the caller to possess the exact `proof` and `claim` bytes,
so it authenticates via knowledge of the secret rather than identity, and
restricting it further would not add confidentiality — only availability
loss for legitimate holders of a confidential credential's proof.

---

## Scheduled Consistency Re-Checks

`verify_credentials_consistent` (and `CredentialRegistry` in
`src/consistency.rs`) provides on-demand consistency verification, but until
this feature there was no way to schedule periodic re-checks for long-lived
attestations. Every attestation now tracks a `next_check_due` timestamp:

- **Scheduling**: `attest` and `create_derived_credential` set
  `next_check_due` to the attestation time plus `CONSISTENCY_CHECK_INTERVAL`
  (30 days). Re-attesting an existing `(proof, claim)` pair refreshes the
  schedule.
- **Detection**: `is_consistency_check_due(credential_id)` returns `true`
  once the ledger timestamp is at or past `next_check_due`. When due it also
  publishes a `cons_due` event with `(credential_id, next_check_due)` so
  off-chain workers can pick the credential up for re-verification (e.g. via
  `verify_credentials_consistent`). Unknown credential ids are never due
  (absence means no schedule), mirroring `is_credential_invalidated`.
- **Periodicity**: a due check stays due until an off-chain worker completes
  the re-check and calls `reschedule_consistency_check(credential_id)`, which
  pushes the next window out by one full `CONSISTENCY_CHECK_INTERVAL` from
  the current timestamp. Anyone may call it — it only moves a timer forward.
  Without this step the check would be due-once-and-forever instead of
  periodic.

Note: because publishing an event requires a real transaction, off-chain
workers must *invoke* `is_consistency_check_due` (not view-call it) for the
`cons_due` event to be recorded; the boolean result is available either way.

Adding `next_check_due` to the stored `AttestationRecord` changes its XDR
layout, so attestation records persisted by an older contract version would
fail to decode — in this stub's lifecycle that only matters for contracts
that were already deployed before the field existed, and is resolved by
re-attestation (which rewrites the record with a fresh schedule).

---

## Why It Is a Stub

### Technical Reasons

1. **Soroban Host Functions Unavailable**: Full ZK verification (e.g., Groth16) requires:
   - Pairing operations (elliptic curve)
   - Field arithmetic (BN128, BLS12-381)
   - Custom host functions not yet exposed in Soroban v20.x

2. **Storage Constraints**: On-chain ZK proofs are large:
   - Groth16 proof: ~288 bytes (manageable)
   - Verification key: ~3-5 KB (significant)
   - Vkey storage per proof set becomes prohibitive

3. **Performance**: Cryptographic operations on-chain are expensive:
   - BN128 pairing: ~50-100ms per proof
   - Contract execution cost scales with complexity
   - Threshold pricing makes frequent verification costly

4. **Trusted Setup Requirement**: Groth16 needs a trusted setup ceremony:
   - Parameters must be securely generated
   - Cannot be deployed without external coordination
   - Increases complexity and trust assumptions

### Design Philosophy

The stub allows:

- ✅ Architecture validation (prove the contract model works)
- ✅ Oracle integration testing (validate the attestation flow)
- ✅ Event emission patterns (ensure off-chain indexing works)
- ✅ Future migration (clear upgrade path to real ZK)

---

## What a Real ZK Implementation Requires

### 1. Cryptographic Backend

**Current Limitation**: Soroban lacks pairing-friendly elliptic curve host functions.

**Required**: At least one of:

- **Groth16** (BN128 or BLS12-381):
  - Most compact proofs (~288 bytes)
  - Fastest verification (single pairing check)
  - Widely supported in ZK frameworks

- **PLONK** (Generic elliptic curves):
  - Longer proofs (~5-10 KB)
  - Flexible setup ceremony
  - Better for multiple proof systems

- **Bulletproofs**:
  - Smaller proofs than PLONK
  - Transparent setup (no trusted ceremony)
  - Slower verification

### 2. Host Functions Needed

Soroban must expose (or Heirloom-Protocol must implement as a precompile):

```rust
/// Verify a Groth16 proof
fn groth16_verify(
    vkey: BytesN<4096>,        // Verification key
    proof: BytesN<288>,         // Groth16 proof
    pub_inputs: Vec<u256>       // Public inputs
) -> bool

/// Verify a PLONK proof  
fn plonk_verify(
    vkey: Bytes,                // Verification key (variable size)
    proof: Bytes,               // PLONK proof
    pub_inputs: Vec<Scalar>     // Public inputs
) -> bool
```

### 3. Trusted Setup (if using Groth16)

**Setup Ceremony Output**:

```
Common Reference String (CRS)
├── Proving Key (prover side, not stored on-chain)
├── Verification Key (stored on-chain, immutable)
└── Toxic Waste (destroyed, never stored)
```

**Ceremony Participants**: Minimum 3-5 independent parties running the MPC protocol.

**Heirloom-Protocol Integration**:

- Verification keys stored in contract
- New key registration requires multi-sig approval
- Audit trail for all key changes

### 4. Circuit Implementation

**What the circuit must prove**:

```rust
// Example: Prove vault ownership without revealing private key
circuit prove_vault_access(
    owner_secret: Field,           // Hidden input
    vault_salt: Field,             // Public input
    commitment: Field              // Public commitment
) {
    // Verify: hash(owner_secret, vault_salt) == commitment
    let derived = poseidon_hash([owner_secret, vault_salt]);
    assert(derived == commitment);
}
```

### 5. Performance Envelope

**Target metrics for production**:

| Metric | Groth16 | PLONK | Bulletproofs |
|--------|---------|-------|--------------|
| **Proof Size** | 288 B | 5-10 KB | 2-4 KB |
| **On-Chain Verification** | ~5 ms | ~50 ms | ~500 ms |
| **Setup Time** | Hours (ceremony) | Minutes | Transparent |
| **Prover Time** | ~100 ms | ~500 ms | ~5 s |
| **Memory** | ~1 GB | ~2 GB | ~500 MB |

---

## Roadmap for Full ZK Implementation

### Phase 1: Foundation (Soroban v21.x - Q4 2026)

**Deliverables**:

- [ ] Soroban exposes scalar multiplication for BN128
- [ ] Heirloom-Protocol implements Miller-Rabin for fast BN128 pairing
- [ ] Verification key registration framework
- [ ] Test circuit with Groth16 (via circom + snarkjs)

**Effort**: 3-4 weeks

### Phase 2: Groth16 Verifier (Soroban v22.x - Q1 2027)

**Deliverables**:

- [ ] Full Groth16 verifier in Rust (without host functions, naive)
- [ ] Integration with Heirloom-Protocol contract
- [ ] Trusted setup ceremony documentation
- [ ] Example: vault access proof

**Effort**: 4-6 weeks

**Status**: Groth16 without optimized host functions is slow (~50-100 ms per proof).

### Phase 3: Optimized Verification (Soroban v23.x - Q2 2027)

**Deliverables**:

- [ ] Native BN128 pairing in Soroban
- [ ] Fast Groth16 verifier (~5-10 ms per proof)
- [ ] PLONK verifier as alternative
- [ ] Proof batching support (verify N proofs in one transaction)

**Effort**: 6-8 weeks

### Phase 4: Productionization (Q3 2027)

**Deliverables**:

- [ ] Security audit of verifier code
- [ ] Trusted setup ceremony execution
- [ ] Circuit library for common proving tasks
- [ ] CLI tools for proof generation
- [ ] Off-chain prover integration

**Effort**: 8-10 weeks

**Timeline**: ~6 months from Phase 1 start.

---

## Using the Stub Today

### For Testing

The stub is suitable for:

1. **Architecture validation**: Confirm the contract model integrates correctly
2. **Oracle testing**: Verify the attestation workflow
3. **Event indexing**: Test off-chain listening
4. **Integration tests**: Mock ZK verification for workflow testing

### Example Test Flow

```rust
#[test]
fn test_oracle_attestation_flow() {
    let env = Env::default();
    let contract = ZkVerifierContract::new(&env);
    
    // 1. Initialize with admin
    let admin = Address::generate(&env);
    contract.initialize(admin.clone());
    
    // 2. Register oracle
    let oracle = Address::generate(&env);
    contract.register_oracle(oracle.clone());
    
    // 3. Oracle attests
    let proof = Bytes::from_slice(&env, &[1, 2, 3]);
    let claim = Bytes::from_slice(&env, &[4, 5, 6]);
    contract.attest(oracle.clone(), proof.clone(), claim.clone());
    
    // 4. Verify claim
    let result = contract.verify_claim(proof, claim);
    assert!(result);  // Stub always returns true for non-0x00 proofs
}
```

### For Production

**Do NOT use the stub for**:

- ❌ Actual proof verification security
- ❌ Authorization decisions based on proofs
- ❌ Financial transactions gated by ZK
- ❌ Any security-critical logic

---

## Error Codes

> **Numbering note:** this table documents the full API described in this
> document, some of which is not yet present in `src/lib.rs` (see the
> note above the "Additional error codes" section). In the current
> `src/lib.rs`, `AccessDenied` is error **21** — not 16 as listed below —
> because the current error numbering differs from the documented API:
> the hierarchy errors are 12-16 (`CredentialNotFound` 12,
> `SelfReferentialParent` 13, `ParentAlreadySet` 14,
> `CredentialChainTooDeep` 15, `ParentCredentialInvalid` 16), the
> lattice/masking errors are 17-20 (see "Additional error codes" above),
> and `AccessDenied` is 21. Codes 8-11 and `VersionNotFound` (17 below)
> belong to the dispute / version-history API that is not yet present in
> `src/lib.rs`.

| Code | Name | Description |
|------|------|-------------|
| 1 | EmptyProof | Proof bytes are empty |
| 2 | EmptyClaim | Claim bytes are empty |
| 3 | ProofTooLarge | Proof exceeds 4 KB |
| 4 | ClaimTooLarge | Claim exceeds 1 KB |
| 5 | AlreadyInitialized | Contract already initialized |
| 6 | NotInitialized | Contract has not been initialized |
| 7 | OracleNotFound | Oracle address not registered (also used when a non-oracle attempts to vote) |
| 8 | CredentialNotFound | No credential exists with the given id |
| 9 | DisputeNotFound | No dispute exists with the given id |
| 10 | DisputeNotOpen | The dispute has already been resolved |
| 11 | AlreadyVoted | This oracle already voted on this dispute |
| 12 | DisputeAlreadyOpen | A dispute is already open for this credential |
| 13 | EmptyReason | Dispute reason bytes were empty |
| 14 | ReasonTooLarge | Dispute reason exceeds MAX_REASON_SIZE (1 KB) |
| 15 | InvalidThreshold | Dispute threshold must be greater than zero |
| 16 | AccessDenied | Caller is not permitted to view this credential at its current privacy level |
| 17 | VersionNotFound | No version exists with the given number for this credential (never recorded, or since pruned) |
| 18 | ParentCredentialInvalid | An ancestor in the parent chain (the parent itself, or one of its own ancestors) has been invalidated by an upheld dispute |
| 19 | CredentialChainTooDeep | The parent chain exceeds MAX_CREDENTIAL_CHAIN_DEPTH (32) hops |
| 20 | SelfReferentialParent | A credential cannot be derived from itself |
| 21 | ParentAlreadySet | This (proof, claim) pair was already derived from a different parent |

---

## References

- [Groth16: Succinct Non-Interactive Zero Knowledge for a von Neumann Architecture](https://eprint.iacr.org/2016/260)
- [PLONK: Permutations over Lagrange-bases for Oecumenical Noninteractive arguments of Knowledge](https://eprint.iacr.org/2019/953)
- [Bulletproofs: Short Proofs for Confidential Transactions and More](https://eprint.iacr.org/2017/1066)
- [Stellar Soroban Documentation](https://developers.stellar.org/docs/learn/soroban)
- [circom: Circuit Compiler](https://github.com/iden3/circom)
- [snarkjs: ZK Proof Generation](https://github.com/iden3/snarkjs)

---

## Migration Notes

When Soroban adds native ZK support or Heirloom-Protocol implements a verifier:

1. **Update** `verify_claim` implementation (drop sentinel check)
2. **Add** verification key management endpoints
3. **Emit** circuit-specific events
4. **Maintain** backward compatibility where possible
5. **Deprecate** oracle attestation flow (if not needed)

The stub contract serves as a forward-compatible skeleton for this transition.

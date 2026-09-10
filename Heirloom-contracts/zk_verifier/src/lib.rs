#![no_std]

pub mod compression;
pub mod consistency;
use consistency::CredentialRegistry;

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short,
    xdr::FromXdr, Address, Bytes, BytesN, Env, String, Vec,
};

pub const MAX_PROOF_SIZE: u32 = 4096;
pub const MAX_CLAIM_SIZE: u32 = 1024;
/// Maximum size of a dispute's `reason` bytes, mirroring MAX_CLAIM_SIZE so a
/// dispute cannot be used to smuggle unbounded data onto the ledger.
pub const MAX_REASON_SIZE: u32 = 1024;
/// Maximum size of an exported/masked proof format (`export_proof_for_verification`,
/// `mask_proof_fields`), bounding the extra header/checksum overhead added on
/// top of `MAX_PROOF_SIZE`.
pub const MAX_EXTERNAL_FORMAT_SIZE: u32 = 8192;
/// Format/version tag every `verify_lattice_proof` input must be prefixed
/// with. This is a structural tag only — see [`ZkVerifierContract::is_valid_lattice_proof`]
/// for what it does and does not prove.
pub const LATTICE_PROOF_HEADER: &[u8] = b"LATTICE_V1";
/// Number of concurring oracle votes required to resolve a dispute (either
/// upholding or rejecting it) when no explicit threshold has been configured
/// by the admin via [`ZkVerifierContract::set_dispute_threshold`].
pub const DEFAULT_DISPUTE_THRESHOLD: u32 = 3;
/// Maximum number of historical snapshots retained per credential. Once a
/// credential's snapshot count exceeds this, the oldest snapshot is pruned
/// to bound persistent-storage growth. See docs/zk-verifier.md, "Credential
/// Retention Policy".
pub const MAX_CREDENTIAL_SNAPSHOTS: u32 = 1000;
/// Maximum number of hops a credential's parent chain may have. Enforced by
/// [`ZkVerifierContract::create_derived_credential`] when recursively
/// validating a parent's ancestry, so that chain walks (and the gas they
/// cost) stay bounded. See docs/zk-verifier.md, "Credential Hierarchies".
pub const MAX_CREDENTIAL_CHAIN_DEPTH: u32 = 32;
/// Number of ledger seconds between scheduled consistency re-checks for a
/// long-lived attestation. `attest` and `create_derived_credential` schedule
/// the first check this far after attestation; each completed re-check (via
/// [`ZkVerifierContract::reschedule_consistency_check`]) pushes the next one
/// out by the same window. See docs/zk-verifier.md, "Scheduled Consistency
/// Re-Checks".
pub const CONSISTENCY_CHECK_INTERVAL: u64 = 30 * 24 * 60 * 60;

const VERIFY_CLAIM_TOPIC: soroban_sdk::Symbol = symbol_short!("vfy_claim");
const VERIFY_CONDITIONAL_TOPIC: soroban_sdk::Symbol = symbol_short!("vfy_cond");
const VERIFY_LATTICE_TOPIC: soroban_sdk::Symbol = symbol_short!("vfy_latt");
const AUDIT_LOG_TOPIC: soroban_sdk::Symbol = symbol_short!("audit_log");
const PROOF_MASKED_TOPIC: soroban_sdk::Symbol = symbol_short!("proof_msk");
const CONSISTENCY_DUE_TOPIC: soroban_sdk::Symbol = symbol_short!("cons_due");

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum VerifierError {
    /// Proof bytes were empty.
    EmptyProof = 1,
    /// Claim bytes were empty.
    EmptyClaim = 2,
    /// Proof bytes exceed MAX_PROOF_SIZE.
    ProofTooLarge = 3,
    /// Claim bytes exceed MAX_CLAIM_SIZE.
    ClaimTooLarge = 4,
    /// Contract has already been initialized.
    AlreadyInitialized = 5,
    /// Contract has not been initialized.
    NotInitialized = 6,
    /// The oracle address is not registered.
    OracleNotFound = 7,
    /// `proof` could not be decoded as a `ConditionalProof`.
    MalformedConditionalProof = 8,
    /// Batch consistency check failed; credentials conflict.
    BatchConsistencyError = 9,
    /// Batch credential IDs list was empty.
    EmptyBatchIds = 10,
    /// Proof and claim lists have different lengths.
    MismatchedBatchLengths = 11,
    /// A credential referenced as a parent (directly, or via
    /// `create_derived_credential`) was never attested.
    CredentialNotFound = 12,
    /// A derived credential's `(proof, claim)` pair hashes to the same
    /// credential_id as its claimed parent.
    SelfReferentialParent = 13,
    /// This `(proof, claim)` pair was already derived from a different
    /// parent; a credential's place in the hierarchy is fixed at creation.
    ParentAlreadySet = 14,
    /// A credential's ancestor chain exceeds MAX_CREDENTIAL_CHAIN_DEPTH hops.
    CredentialChainTooDeep = 15,
    /// An ancestor in a derived credential's chain has been invalidated by
    /// an upheld dispute.
    ParentCredentialInvalid = 16,
    /// `proof` does not carry a valid LATTICE_V1 format header and checksum.
    InvalidLatticeProof = 17,
    /// Exported or masked proof format exceeds MAX_EXTERNAL_FORMAT_SIZE.
    ExternalFormatTooLarge = 18,
    /// `fields_to_mask` was empty, referenced an out-of-range field, or
    /// `masked_proof` was too short to carry a mask header.
    InvalidMaskSpec = 19,
    /// A masked proof was not attested by a currently-registered oracle for
    /// the given claim.
    MaskedVerificationFailed = 20,
    /// The caller is not permitted to view this credential's attestation
    /// record at its current privacy level.
    AccessDenied = 21,
}

/// The on-chain format for a conditional ("prove X if Y, else prove Z")
/// proof, encoded into the opaque `proof: Bytes` argument of
/// [`ZkVerifierContract::verify_conditional_proof`] via XDR.
///
/// The condition `Y` is the caller-supplied `condition` claim; this bundle
/// carries the proof for `Y` plus both branches' claim/proof pairs so the
/// contract itself — not the caller — decides and checks which branch
/// applies, exactly once `Y`'s truth is established from oracle attestation.
#[contracttype]
#[derive(Clone)]
pub struct ConditionalProof {
    /// Proof that the `condition` claim (`Y`) holds.
    pub condition_proof: Bytes,
    /// Claim `X`, checked when the condition is true.
    pub then_claim: Bytes,
    /// Proof that claim `X` holds.
    pub then_proof: Bytes,
    /// Claim `Z`, checked when the condition is false.
    pub else_claim: Bytes,
    /// Proof that claim `Z` holds.
    pub else_proof: Bytes,
}

/// Storage key discriminants.
mod keys {
    use soroban_sdk::{contracttype, Address, BytesN};

    #[contracttype]
    pub enum DataKey {
        Admin,
        Oracle(Address),
        /// Attestation: (proof_sha256, claim_sha256) -> AttestationRecord
        Attestation(BytesN<32>, BytesN<32>),
        /// Incrementing generation counter for credential ids.
        CredentialCount,
        /// credential_id -> (proof_sha256, claim_sha256), the reverse of
        /// `Attestation`, so a credential can be looked up by id.
        CredentialHashes(u64),
        /// Present (and true) once a dispute against this credential has
        /// been upheld; absence means the credential is not disputed-invalid.
        CredentialInvalidated(u64),
        /// credential_id -> dispute_id of the currently open dispute against
        /// it, if any. Cleared once that dispute resolves.
        CredentialOpenDispute(u64),
        /// credential_id -> Vec<dispute_id>, full dispute history.
        CredentialDisputeHistory(u64),
        /// Incrementing generation counter for dispute ids.
        DisputeCount,
        DisputeRecord(u64),
        /// (dispute_id, voter) -> vote cast, used to prevent double-voting.
        DisputeVote(u64, Address),
        /// Number of concurring votes needed to resolve a dispute. Falls
        /// back to DEFAULT_DISPUTE_THRESHOLD when unset.
        DisputeThreshold,
        /// (credential_id, timestamp) -> CredentialSnapshot, captured every
        /// time a credential's attestation or invalidation status changes.
        CredentialSnapshot(u64, u64),
        /// credential_id -> Vec<timestamp>, ascending, one entry per
        /// retained snapshot for that credential (bounded by
        /// MAX_CREDENTIAL_SNAPSHOTS).
        CredentialSnapshotTimestamps(u64),
        /// credential_id -> Vec<version>, ascending, a parallel index to
        /// `CredentialSnapshotTimestamps` (same length, same order, same
        /// retention bound) mapping each retained snapshot to its version
        /// number.
        CredentialSnapshotVersions(u64),
        /// credential_id -> PrivacyLevel. Absence means `Public`, so
        /// pre-existing credentials are unaffected until an admin opts them
        /// into a stricter level via `set_credential_privacy`.
        CredentialPrivacy(u64),
        /// child credential_id -> parent credential_id. Absence means the
        /// credential is a root — either created via `attest`, or a
        /// derived credential that has no recorded parent. Set once, at the
        /// first time a credential_id is associated with a parent via
        /// `create_derived_credential`, and never reassigned thereafter.
        CredentialParent(u64),
        /// proof_sha256 -> Vec<VerificationRecord>, the append-only
        /// verification audit log recorded by
        /// `ZkVerifierContract::record_verification`.
        VerificationHistory(BytesN<32>),
        /// proof_sha256 (of the original, unmasked proof) -> MaskingConfig,
        /// recorded by `ZkVerifierContract::mask_proof_fields`.
        MaskingConfig(BytesN<32>),
        /// credential_id -> MaskingConfig, recorded the first time
        /// `ZkVerifierContract::get_attestation` serves a redacted
        /// `AttestationRecord` for that credential (i.e. an unauthorized
        /// caller at `PrivacyLevel::Confidential`). Mirrors the proof-field
        /// masking audit trail in `DataKey::MaskingConfig`.
        AttestationMasking(u64),
        /// proof_sha256 -> ledger timestamp of the most recent
        /// `ZkVerifierContract::verify_lattice_proof` call for that proof.
        LastVerificationTime(BytesN<32>),
    }

    /// A single entry in a proof's verification audit trail. See
    /// [`DataKey::VerificationHistory`].
    #[contracttype]
    #[derive(Clone)]
    pub struct VerificationRecord {
        pub timestamp: u64,
        pub verified: bool,
        pub oracle: Address,
    }

    /// Records which fields of a proof were redacted by
    /// `ZkVerifierContract::mask_proof_fields`. See
    /// [`DataKey::MaskingConfig`].
    #[contracttype]
    #[derive(Clone)]
    pub struct MaskingConfig {
        /// SHA-256 of the little-endian field bitmask that was applied.
        pub masked_fields: BytesN<32>,
        pub version: u32,
    }
}

use keys::{DataKey, MaskingConfig, VerificationRecord};

/// A stored oracle attestation, now addressable by a stable `credential_id`
/// in addition to the `(proof_hash, claim_hash)` pair used by `verify_claim`.
#[contracttype]
#[derive(Clone)]
pub struct AttestationRecord {
    pub credential_id: u64,
    pub oracle: Address,
    /// Ledger timestamp at which the attestation's next scheduled
    /// consistency re-check becomes due. Set to `attestation time +
    /// CONSISTENCY_CHECK_INTERVAL` at attestation (and re-attestation), and
    /// advanced by `reschedule_consistency_check` after each completed
    /// re-check. See `is_consistency_check_due` and
    /// docs/zk-verifier.md, "Scheduled Consistency Re-Checks".
    pub next_check_due: u64,
}

/// A point-in-time snapshot of a credential's attestation state, captured
/// whenever that state changes (re-attestation, or a dispute resolving).
/// Used to answer historical questions like "was this credential valid at
/// time T?" via [`ZkVerifierContract::get_credential_at_time`], or "what did
/// version N look like?" via [`ZkVerifierContract::get_credential_version`].
#[contracttype]
#[derive(Clone)]
pub struct CredentialSnapshot {
    pub credential_id: u64,
    pub oracle: Address,
    pub invalidated: bool,
    /// Ledger timestamp at which this snapshot was captured.
    pub timestamp: u64,
    /// Monotonically increasing version number for this credential, starting
    /// at 1. Unlike the snapshot itself, a version number is never reused or
    /// renumbered once assigned — even after the retention policy prunes the
    /// snapshot it identifies, so audit references to "version N" remain
    /// meaningful (as "not found") rather than silently pointing at a
    /// different state. See docs/zk-verifier.md, "Credential Version
    /// History".
    pub version: u32,
}

/// The result of comparing two recorded versions of a credential's
/// attestation state, returned by
/// [`ZkVerifierContract::diff_credential_versions`].
#[contracttype]
#[derive(Clone)]
pub struct CredentialVersionDiff {
    pub credential_id: u64,
    pub from_version: u32,
    pub to_version: u32,
    pub from_timestamp: u64,
    pub to_timestamp: u64,
    pub oracle_changed: bool,
    pub previous_oracle: Address,
    pub current_oracle: Address,
    pub invalidated_changed: bool,
    pub previous_invalidated: bool,
    pub current_invalidated: bool,
}

/// Controls who may read a credential's attestation record/state via
/// [`ZkVerifierContract::get_attestation`] (and, once restored,
/// `get_credential_at_time`). Set per-credential by the admin via
/// [`ZkVerifierContract::set_credential_privacy`]; defaults to `Public` for
/// every credential until explicitly changed.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivacyLevel {
    /// Readable by anyone.
    Public,
    /// Readable only by the admin and registered oracles.
    Internal,
    /// Readable only by the admin.
    Confidential,
}

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisputeStatus {
    /// Open for voting.
    Open,
    /// Threshold of "invalid" votes reached; the credential is now treated
    /// as invalid by `verify_claim`.
    Upheld,
    /// Threshold of "valid" votes reached; the credential remains valid.
    Rejected,
}

#[contracttype]
#[derive(Clone)]
pub struct Dispute {
    pub id: u64,
    pub credential_id: u64,
    pub initiator: Address,
    pub reason: Bytes,
    pub status: DisputeStatus,
    /// Votes asserting the credential is invalid.
    pub votes_for: u32,
    /// Votes asserting the credential remains valid.
    pub votes_against: u32,
    pub created_at: u64,
    /// Ledger timestamp of resolution, or 0 while still Open.
    pub resolved_at: u64,
}

#[contract]
pub struct ZkVerifierContract;

#[contractimpl]
impl ZkVerifierContract {
    /// Initialize the contract with an admin address.
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(&env, VerifierError::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
    }

    /// Register a trusted oracle. Admin only.
    pub fn register_oracle(env: Env, oracle: Address) {
        Self::require_admin(&env);
        env.storage()
            .instance()
            .set(&DataKey::Oracle(oracle), &true);
    }

    /// Revoke a trusted oracle. Admin only.
    pub fn revoke_oracle(env: Env, oracle: Address) {
        Self::require_admin(&env);
        env.storage().instance().remove(&DataKey::Oracle(oracle));
    }

    /// Returns whether the given address is a registered oracle.
    pub fn is_oracle(env: Env, oracle: Address) -> bool {
        env.storage()
            .instance()
            .get::<DataKey, bool>(&DataKey::Oracle(oracle))
            .unwrap_or(false)
    }

    /// An oracle publishes an attestation that `proof` is valid for `claim`.
    ///
    /// The contract stores the SHA-256 digests of both byte strings so that
    /// the full proof bytes are not stored on-chain. Returns the stable
    /// `credential_id` for this `(proof, claim)` pair — a fresh id the first
    /// time it is attested, or the existing id if it was attested before
    /// (e.g. by a different oracle, or re-attested after a dispute).
    pub fn attest(env: Env, oracle: Address, proof: Bytes, claim: Bytes) -> u64 {
        if proof.is_empty() {
            panic_with_error!(&env, VerifierError::EmptyProof);
        }
        if claim.is_empty() {
            panic_with_error!(&env, VerifierError::EmptyClaim);
        }
        Self::require_registered_oracle(&env, &oracle);
        oracle.require_auth();
        let proof_hash: BytesN<32> = env.crypto().sha256(&proof).into();
        let claim_hash: BytesN<32> = env.crypto().sha256(&claim).into();

        let credential_id = Self::mint_or_reuse_credential_id(&env, &proof_hash, &claim_hash);

        env.storage().instance().set(
            &DataKey::Attestation(proof_hash, claim_hash),
            &AttestationRecord {
                credential_id,
                oracle: oracle.clone(),
                next_check_due: env
                    .ledger()
                    .timestamp()
                    .saturating_add(CONSISTENCY_CHECK_INTERVAL),
            },
        );

        let invalidated = Self::is_credential_invalidated(env.clone(), credential_id);
        Self::record_credential_snapshot(&env, credential_id, oracle, invalidated);

        credential_id
    }

    /// Attests `(proof, claim)` as a credential *derived from* `parent_id`
    /// — e.g. a certificate issued on the basis of a degree, which was
    /// itself issued on the basis of a transcript. Otherwise behaves
    /// exactly like [`Self::attest`]: it dedups on `(proof, claim)`, reuses
    /// the existing credential_id if this exact pair was attested before,
    /// and requires a currently-registered `oracle` to authorize the call.
    ///
    /// Before minting or re-attesting the derived credential, this walks
    /// `parent_id`'s *entire* ancestor chain — not just its immediate state
    /// — and panics with `ParentCredentialInvalid` if any credential in
    /// that chain (`parent_id` itself, its parent, its grandparent, and so
    /// on) has been invalidated by an upheld dispute. A derived credential
    /// is only as trustworthy as everything it was built on, so a
    /// compromised ancestor anywhere in the chain blocks new issuance, not
    /// just a directly invalidated parent.
    ///
    /// Panics with `CredentialNotFound` if `parent_id` was never attested,
    /// with `CredentialChainTooDeep` if `parent_id`'s chain already spans
    /// more than `MAX_CREDENTIAL_CHAIN_DEPTH` hops, with
    /// `SelfReferentialParent` if the derived credential would be its own
    /// parent (i.e. `proof`/`claim` hash to the same credential_id as
    /// `parent_id`), and with `ParentAlreadySet` if this exact `(proof,
    /// claim)` pair was already derived from a *different* parent — a
    /// credential's place in the hierarchy is fixed at its first creation
    /// and cannot be changed by re-deriving it.
    pub fn create_derived_credential(
        env: Env,
        oracle: Address,
        parent_id: u64,
        proof: Bytes,
        claim: Bytes,
    ) -> u64 {
        if proof.is_empty() {
            panic_with_error!(&env, VerifierError::EmptyProof);
        }
        if claim.is_empty() {
            panic_with_error!(&env, VerifierError::EmptyClaim);
        }
        if !env
            .storage()
            .instance()
            .has(&DataKey::CredentialHashes(parent_id))
        {
            panic_with_error!(&env, VerifierError::CredentialNotFound);
        }
        Self::validate_credential_chain(&env, parent_id);

        Self::require_registered_oracle(&env, &oracle);
        oracle.require_auth();

        let proof_hash: BytesN<32> = env.crypto().sha256(&proof).into();
        let claim_hash: BytesN<32> = env.crypto().sha256(&claim).into();
        let credential_id = Self::mint_or_reuse_credential_id(&env, &proof_hash, &claim_hash);

        if credential_id == parent_id {
            panic_with_error!(&env, VerifierError::SelfReferentialParent);
        }

        match Self::load_parent(&env, credential_id) {
            Some(existing_parent) if existing_parent != parent_id => {
                panic_with_error!(&env, VerifierError::ParentAlreadySet);
            }
            _ => {
                env.storage()
                    .instance()
                    .set(&DataKey::CredentialParent(credential_id), &parent_id);
            }
        }

        env.storage().instance().set(
            &DataKey::Attestation(proof_hash, claim_hash),
            &AttestationRecord {
                credential_id,
                oracle: oracle.clone(),
                next_check_due: env
                    .ledger()
                    .timestamp()
                    .saturating_add(CONSISTENCY_CHECK_INTERVAL),
            },
        );

        let invalidated = Self::is_credential_invalidated(env.clone(), credential_id);
        Self::record_credential_snapshot(&env, credential_id, oracle, invalidated);

        credential_id
    }

    /// Verifies a zero-knowledge proof against a claim using oracle attestation.
    ///
    /// This hashes `proof` and `claim` with SHA-256 (the same digests used by
    /// [`Self::attest`]) and looks up `DataKey::Attestation(proof_hash,
    /// claim_hash)` in instance storage. Returns `true` only if:
    ///   1. an attestation exists for this exact `(proof, claim)` pair, AND
    ///   2. the oracle that made that attestation is *currently* a registered
    ///      oracle (i.e. has not since been revoked via `revoke_oracle`).
    ///
    /// Revocation semantics: attestations are not deleted on revocation, but
    /// they are only honored while the attesting oracle remains registered.
    /// This is the safer choice for a contract that gates release of real
    /// funds — a revoked oracle (e.g. one that was compromised or found to be
    /// misbehaving) should immediately lose the ability to have its past
    /// attestations relied upon, without requiring a separate sweep to purge
    /// its attestation records.
    ///
    /// Returns `false` (does not panic) when no matching attestation exists,
    /// when the attesting oracle is no longer registered, or when the
    /// credential has been invalidated by an upheld dispute (see
    /// [`Self::vote_on_dispute`]) — all are normal "not verified" outcomes.
    ///
    /// Emits a `vfy_claim` event with `(result, claim_hash)` on every call
    /// that passes input validation.
    pub fn verify_claim(env: Env, proof: Bytes, claim: Bytes) -> bool {
        let proof_hash: BytesN<32> = env.crypto().sha256(&proof).into();
        let claim_hash: BytesN<32> = env.crypto().sha256(&claim).into();
        let result = Self::verify_internal(&env, &proof, &claim);

        env.events()
            .publish((VERIFY_CLAIM_TOPIC,), (result, claim_hash));

        if result {
            Self::record_verification(&env, &proof_hash, true);
        }

        result
    }

    /// Verifies a proof tagged with the `LATTICE_V1` format header.
    ///
    /// Despite the name, this crate implements no lattice-based (e.g.
    /// Dilithium/Falcon-style, post-quantum) cryptographic scheme — see
    /// [`Self::is_valid_lattice_proof`] for exactly what the format check
    /// does and does not prove. The actual trust decision is identical to
    /// [`Self::verify_claim`]'s oracle-attestation model: this returns
    /// `true` only when the exact `proof` bytes were attested by a
    /// currently-registered oracle for `claim`.
    pub fn verify_lattice_proof(env: Env, proof: Bytes, claim: Bytes) -> bool {
        if proof.is_empty() {
            panic_with_error!(&env, VerifierError::EmptyProof);
        }
        if proof.len() > MAX_PROOF_SIZE {
            panic_with_error!(&env, VerifierError::ProofTooLarge);
        }
        if claim.is_empty() {
            panic_with_error!(&env, VerifierError::EmptyClaim);
        }
        if claim.len() > MAX_CLAIM_SIZE {
            panic_with_error!(&env, VerifierError::ClaimTooLarge);
        }

        if !Self::is_valid_lattice_proof(&env, &proof) {
            panic_with_error!(&env, VerifierError::InvalidLatticeProof);
        }

        let proof_hash: BytesN<32> = env.crypto().sha256(&proof).into();
        let claim_hash: BytesN<32> = env.crypto().sha256(&claim).into();

        let result = env
            .storage()
            .instance()
            .get::<DataKey, AttestationRecord>(&DataKey::Attestation(
                proof_hash.clone(),
                claim_hash.clone(),
            ))
            .is_some_and(|record| {
                env.storage()
                    .instance()
                    .get::<DataKey, bool>(&DataKey::Oracle(record.oracle))
                    .unwrap_or(false)
            });

        let current_time: u64 = env.ledger().timestamp();
        env.storage().instance().set(
            &DataKey::LastVerificationTime(proof_hash.clone()),
            &current_time,
        );

        env.events()
            .publish((VERIFY_LATTICE_TOPIC,), (result, proof_hash.clone()));

        if result {
            Self::record_verification(&env, &proof_hash, true);
        }

        result
    }

    /// Exports a proof in standard external format for cross-system verification.
    /// Supports interoperability with external verifiers.
    ///
    /// Named `export_proof_for_verification` (rather than
    /// `..._external_verification`) because Soroban caps contract function
    /// names at 32 characters; the longer name never compiled.
    pub fn export_proof_for_verification(env: Env, proof: Bytes, format_type: u32) -> Bytes {
        if proof.is_empty() {
            panic_with_error!(&env, VerifierError::EmptyProof);
        }
        if proof.len() > MAX_PROOF_SIZE {
            panic_with_error!(&env, VerifierError::ProofTooLarge);
        }

        let mut exported = Bytes::new(&env);
        exported.append(&Bytes::from_array(&env, &[format_type as u8; 1]));
        exported.append(&proof);

        let proof_hash: BytesN<32> = env.crypto().sha256(&proof).into();
        exported.append(&Bytes::from(proof_hash));

        if exported.len() > MAX_EXTERNAL_FORMAT_SIZE {
            panic_with_error!(&env, VerifierError::ExternalFormatTooLarge);
        }

        exported
    }

    /// Gets the verification history for a proof identified by its hash.
    /// Returns all verification attempts with timestamps.
    pub fn get_proof_verification_history(
        env: Env,
        proof_hash: BytesN<32>,
    ) -> Vec<VerificationRecord> {
        env.storage()
            .instance()
            .get::<DataKey, Vec<VerificationRecord>>(&DataKey::VerificationHistory(proof_hash))
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Masks sensitive fields in a proof before verification.
    ///
    /// `fields_to_mask` are byte offsets into `proof` (each must be `< 32`
    /// and `< proof.len()`, since they are packed into a `u32` bitmask for
    /// the audit trail). The returned proof has the header, the bitmask,
    /// then `proof`'s bytes with every masked offset zeroed out — unlike a
    /// length-preserving "masked" copy that still carries the original
    /// bytes, a caller inspecting the output cannot recover a masked byte.
    pub fn mask_proof_fields(env: Env, proof: Bytes, fields_to_mask: Vec<u32>) -> Bytes {
        if proof.is_empty() {
            panic_with_error!(&env, VerifierError::EmptyProof);
        }
        if proof.len() > MAX_PROOF_SIZE {
            panic_with_error!(&env, VerifierError::ProofTooLarge);
        }
        if fields_to_mask.is_empty() {
            panic_with_error!(&env, VerifierError::InvalidMaskSpec);
        }

        let mut field_mask: u32 = 0;
        for i in 0..fields_to_mask.len() {
            if let Some(field_idx) = fields_to_mask.get(i) {
                if field_idx >= 32 || field_idx >= proof.len() {
                    panic_with_error!(&env, VerifierError::InvalidMaskSpec);
                }
                field_mask |= 1 << field_idx;
            }
        }

        let mut masked = Bytes::new(&env);
        masked.append(&Bytes::from_array(&env, b"MASKED_V1"));
        masked.append(&Bytes::from_array(&env, &field_mask.to_le_bytes()));

        for i in 0..proof.len() {
            let is_masked = field_mask & (1 << i) != 0;
            masked.push_back(if is_masked {
                0u8
            } else {
                proof.get(i).unwrap_or(0)
            });
        }

        if masked.len() > MAX_EXTERNAL_FORMAT_SIZE {
            panic_with_error!(&env, VerifierError::ExternalFormatTooLarge);
        }

        let proof_hash: BytesN<32> = env.crypto().sha256(&proof).into();
        let masking_spec: BytesN<32> = env
            .crypto()
            .sha256(&Bytes::from_array(&env, &field_mask.to_le_bytes()))
            .into();

        env.storage().instance().set(
            &DataKey::MaskingConfig(proof_hash.clone()),
            &MaskingConfig {
                masked_fields: masking_spec,
                version: 1,
            },
        );

        env.events().publish((PROOF_MASKED_TOPIC,), (proof_hash,));

        masked
    }

    /// Verifies a masked proof, comparing only unmasked fields.
    pub fn verify_masked_proof(env: Env, masked_proof: Bytes, claim: Bytes) -> bool {
        if masked_proof.is_empty() {
            panic_with_error!(&env, VerifierError::EmptyProof);
        }
        if claim.is_empty() {
            panic_with_error!(&env, VerifierError::EmptyClaim);
        }

        if masked_proof.len() < 13 {
            panic_with_error!(&env, VerifierError::InvalidMaskSpec);
        }

        let masked_hash: BytesN<32> = env.crypto().sha256(&masked_proof).into();
        let claim_hash: BytesN<32> = env.crypto().sha256(&claim).into();

        let result = env
            .storage()
            .instance()
            .get::<DataKey, AttestationRecord>(&DataKey::Attestation(masked_hash, claim_hash))
            .is_some_and(|record| {
                env.storage()
                    .instance()
                    .get::<DataKey, bool>(&DataKey::Oracle(record.oracle))
                    .unwrap_or(false)
            });

        if !result {
            panic_with_error!(&env, VerifierError::MaskedVerificationFailed);
        }

        result
    }

    /// Verifies a conditional ("prove X if Y, else prove Z") proof.
    ///
    /// `condition` is claim `Y`. `proof` is the XDR encoding of a
    /// [`ConditionalProof`] bundle carrying the proof for `Y` plus both
    /// branches. The condition is checked first via the same oracle
    /// attestation model as [`Self::verify_claim`]; the contract then
    /// verifies the `then` branch if the condition holds, or the `else`
    /// branch otherwise — the caller cannot pick a branch that skips the
    /// condition check.
    ///
    /// Returns `false` (does not panic) when the condition or the selected
    /// branch is unattested; panics with `MalformedConditionalProof` if
    /// `proof` is not a valid `ConditionalProof` encoding.
    ///
    /// Emits a `vfy_cond` event with `(result, condition_result, claim_hash)`
    /// of the branch that was checked.
    pub fn verify_conditional_proof(env: Env, proof: Bytes, condition: Bytes) -> bool {
        let bundle = ConditionalProof::from_xdr(&env, &proof)
            .unwrap_or_else(|_| panic_with_error!(&env, VerifierError::MalformedConditionalProof));

        let condition_result = Self::verify_internal(&env, &bundle.condition_proof, &condition);

        let (branch_proof, branch_claim) = if condition_result {
            (bundle.then_proof, bundle.then_claim)
        } else {
            (bundle.else_proof, bundle.else_claim)
        };

        let claim_hash: BytesN<32> = env.crypto().sha256(&branch_claim).into();
        let result = Self::verify_internal(&env, &branch_proof, &branch_claim);

        env.events().publish(
            (VERIFY_CONDITIONAL_TOPIC,),
            (result, condition_result, claim_hash),
        );

        result
    }

    /// Sets the [`PrivacyLevel`] governing who may call
    /// `get_credential_at_time` for `credential_id`. Admin only.
    pub fn set_credential_privacy(env: Env, credential_id: u64, level: PrivacyLevel) {
        Self::require_admin(&env);
        if !env
            .storage()
            .instance()
            .has(&DataKey::CredentialHashes(credential_id))
        {
            panic_with_error!(&env, VerifierError::CredentialNotFound);
        }
        env.storage()
            .instance()
            .set(&DataKey::CredentialPrivacy(credential_id), &level);
    }

    /// Returns `credential_id`'s current [`PrivacyLevel`], defaulting to
    /// `Public` if it has never been explicitly set.
    pub fn credential_privacy(env: Env, credential_id: u64) -> PrivacyLevel {
        env.storage()
            .instance()
            .get(&DataKey::CredentialPrivacy(credential_id))
            .unwrap_or(PrivacyLevel::Public)
    }

    /// Returns the current [`AttestationRecord`] for `credential_id` (the
    /// oracle currently standing behind it, and its stable credential id),
    /// or `None` if that id was never attested.
    ///
    /// `requester` must authorize the call (so the caller cannot be
    /// spoofed) and is checked against the credential's current
    /// [`PrivacyLevel`]:
    ///
    /// - **`Public`** — anyone may read the full record.
    /// - **`Internal`** — the admin or a currently-registered oracle may
    ///   read the full record; anyone else is denied with `AccessDenied`.
    /// - **`Confidential`** — the admin may read the full record; anyone
    ///   else receives a *redacted* copy whose sensitive fields (`oracle`)
    ///   are masked out (see [`Self::redact_attestation_record`]), and the
    ///   redaction is recorded on-chain in a [`MaskingConfig`] under
    ///   `DataKey::AttestationMasking`. The record's existence and
    ///   `credential_id` remain visible, but the attesting oracle is not
    ///   disclosed.
    pub fn get_attestation(
        env: Env,
        requester: Address,
        credential_id: u64,
    ) -> Option<AttestationRecord> {
        requester.require_auth();

        let record = Self::load_attestation_record(&env, credential_id)?;

        match Self::credential_privacy(env.clone(), credential_id) {
            PrivacyLevel::Public => Some(record),
            PrivacyLevel::Internal => {
                if Self::is_admin(&env, &requester) || Self::is_registered_oracle(&env, &requester)
                {
                    Some(record)
                } else {
                    panic_with_error!(&env, VerifierError::AccessDenied);
                }
            }
            PrivacyLevel::Confidential => {
                if Self::is_admin(&env, &requester) {
                    Some(record)
                } else {
                    Some(Self::redact_attestation_record(&env, &record))
                }
            }
        }
    }

    // ---- helpers ----

    /// Structural format check for `verify_lattice_proof`'s input: `proof`
    /// must start with [`LATTICE_PROOF_HEADER`] and end with a 4-byte
    /// checksum (the first 4 bytes of `sha256` over everything before it).
    ///
    /// This is **not** a cryptographic verification of any lattice-based
    /// proof statement — this crate implements no such scheme, so there is
    /// nothing here that establishes the mathematical claim a real
    /// post-quantum proof system would. It exists only to reject
    /// accidental or naively-forged input (e.g. the header alone, or the
    /// header followed by arbitrary bytes) before the real trust decision
    /// in `verify_lattice_proof`, which — exactly like `verify_claim` — is
    /// made entirely by oracle attestation of the exact proof bytes.
    fn is_valid_lattice_proof(env: &Env, proof: &Bytes) -> bool {
        const CHECKSUM_LEN: u32 = 4;
        let header_len = LATTICE_PROOF_HEADER.len() as u32;

        if proof.len() < header_len + CHECKSUM_LEN {
            return false;
        }

        for (i, &byte) in LATTICE_PROOF_HEADER.iter().enumerate() {
            if proof.get(i as u32).unwrap_or(0) != byte {
                return false;
            }
        }

        let body_len = proof.len() - CHECKSUM_LEN;
        let body = proof.slice(0..body_len);
        let checksum = proof.slice(body_len..proof.len());
        let digest: Bytes = env.crypto().sha256(&body).into();

        digest.slice(0..CHECKSUM_LEN) == checksum
    }

    fn record_verification(env: &Env, proof_hash: &BytesN<32>, verified: bool) {
        let mut history = env
            .storage()
            .instance()
            .get::<DataKey, Vec<VerificationRecord>>(&DataKey::VerificationHistory(
                proof_hash.clone(),
            ))
            .unwrap_or_else(|| Vec::new(env));

        let current_time = env.ledger().timestamp();

        history.push_back(VerificationRecord {
            timestamp: current_time,
            verified,
            // `record_verification` isn't handed the attesting oracle (its
            // callers only have a bool result), so this logs the contract's
            // own address as a placeholder rather than a real oracle.
            oracle: env.current_contract_address(),
        });

        env.storage()
            .instance()
            .set(&DataKey::VerificationHistory(proof_hash.clone()), &history);

        env.events().publish(
            (AUDIT_LOG_TOPIC,),
            (proof_hash.clone(), current_time, verified),
        );
    }

    /// Panics with `OracleNotFound` unless `oracle` is currently registered.
    fn require_registered_oracle(env: &Env, oracle: &Address) {
        if !Self::is_registered_oracle(env, oracle) {
            panic_with_error!(env, VerifierError::OracleNotFound);
        }
    }

    /// Returns whether `oracle` is a currently-registered oracle.
    fn is_registered_oracle(env: &Env, oracle: &Address) -> bool {
        env.storage()
            .instance()
            .get::<DataKey, bool>(&DataKey::Oracle(oracle.clone()))
            .unwrap_or(false)
    }

    /// Returns whether `address` is the contract's admin.
    fn is_admin(env: &Env, address: &Address) -> bool {
        env.storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::Admin)
            .is_some_and(|admin| &admin == address)
    }

    /// Looks up the current `AttestationRecord` for `credential_id` by
    /// following `CredentialHashes(credential_id)` to the underlying
    /// `Attestation(proof_hash, claim_hash)` entry. Returns `None` if the
    /// credential id is unknown.
    fn load_attestation_record(env: &Env, credential_id: u64) -> Option<AttestationRecord> {
        let (proof_hash, claim_hash): (BytesN<32>, BytesN<32>) = env
            .storage()
            .instance()
            .get(&DataKey::CredentialHashes(credential_id))?;
        env.storage()
            .instance()
            .get(&DataKey::Attestation(proof_hash, claim_hash))
    }

    /// Returns a copy of `record` with its sensitive fields masked out, for
    /// callers that may learn a credential exists but may not see who stands
    /// behind it (unauthorized callers at `PrivacyLevel::Confidential`).
    ///
    /// The redaction is recorded on-chain so it is auditable: a
    /// [`MaskingConfig`] — the same type `mask_proof_fields` uses for
    /// proof-field masking — is stored under `DataKey::AttestationMasking`
    /// keyed by the credential, with the masked-field bitmask set for
    /// [`ATTESTATION_RECORD_FIELD_ORACLE`]. `credential_id` is never
    /// masked, so the redacted record still identifies itself.
    fn redact_attestation_record(env: &Env, record: &AttestationRecord) -> AttestationRecord {
        let field_mask = 1u32 << ATTESTATION_RECORD_FIELD_ORACLE;
        env.storage().instance().set(
            &DataKey::AttestationMasking(record.credential_id),
            &MaskingConfig {
                masked_fields: env
                    .crypto()
                    .sha256(&Bytes::from_array(env, &field_mask.to_le_bytes()))
                    .into(),
                version: 1,
            },
        );
        AttestationRecord {
            credential_id: record.credential_id,
            oracle: Self::masked_oracle(env),
        }
    }

    /// Returns the well-defined "masked" `Address` used in place of a
    /// redacted attestation record's `oracle`: the all-zero Ed25519 account
    /// ([`MASKED_ORACLE_STRKEY`]). An all-zero key is not a usable Stellar
    /// account, so a masked oracle can never be mistaken for a genuine
    /// attesting oracle, but it is a valid `Address` value that round-trips
    /// through storage and events.
    fn masked_oracle(env: &Env) -> Address {
        Address::from_string(&String::from_str(env, MASKED_ORACLE_STRKEY))
    }

    /// Returns the existing credential_id for `(proof_hash, claim_hash)` if
    /// this exact pair was attested before, otherwise mints a fresh one.
    fn mint_or_reuse_credential_id(
        env: &Env,
        proof_hash: &BytesN<32>,
        claim_hash: &BytesN<32>,
    ) -> u64 {
        if let Some(existing) =
            env.storage()
                .instance()
                .get::<DataKey, AttestationRecord>(&DataKey::Attestation(
                    proof_hash.clone(),
                    claim_hash.clone(),
                ))
        {
            return existing.credential_id;
        }

        let credential_id = env
            .storage()
            .instance()
            .get::<DataKey, u64>(&DataKey::CredentialCount)
            .unwrap_or(0)
            + 1;
        env.storage()
            .instance()
            .set(&DataKey::CredentialCount, &credential_id);
        env.storage().instance().set(
            &DataKey::CredentialHashes(credential_id),
            &(proof_hash.clone(), claim_hash.clone()),
        );

        credential_id
    }

    /// Returns whether `credential_id` has been invalidated by an upheld
    /// dispute. Always `false` for now: nothing currently writes
    /// `DataKey::CredentialInvalidated` since dispute voting has no public
    /// entry point yet, but `attest`, `create_derived_credential`, and
    /// `validate_credential_chain` already gate on it so that future dispute
    /// resolution work only has to set the flag.
    pub fn is_credential_invalidated(env: Env, credential_id: u64) -> bool {
        env.storage()
            .instance()
            .get::<DataKey, bool>(&DataKey::CredentialInvalidated(credential_id))
            .unwrap_or(false)
    }

    /// Records a point-in-time snapshot of a credential's attestation state,
    /// pruning the oldest snapshot once more than MAX_CREDENTIAL_SNAPSHOTS
    /// are retained for it. See `DataKey::CredentialSnapshot` and friends.
    fn record_credential_snapshot(
        env: &Env,
        credential_id: u64,
        oracle: Address,
        invalidated: bool,
    ) {
        let timestamp = env.ledger().timestamp();
        let mut timestamps = env
            .storage()
            .instance()
            .get::<DataKey, Vec<u64>>(&DataKey::CredentialSnapshotTimestamps(credential_id))
            .unwrap_or_else(|| Vec::new(env));
        let mut versions = env
            .storage()
            .instance()
            .get::<DataKey, Vec<u32>>(&DataKey::CredentialSnapshotVersions(credential_id))
            .unwrap_or_else(|| Vec::new(env));

        let version = versions.last().unwrap_or(0) + 1;

        env.storage().instance().set(
            &DataKey::CredentialSnapshot(credential_id, timestamp),
            &CredentialSnapshot {
                credential_id,
                oracle,
                invalidated,
                timestamp,
                version,
            },
        );

        timestamps.push_back(timestamp);
        versions.push_back(version);

        if timestamps.len() > MAX_CREDENTIAL_SNAPSHOTS {
            let pruned_timestamp = timestamps.pop_front_unchecked();
            versions.pop_front_unchecked();
            env.storage()
                .instance()
                .remove(&DataKey::CredentialSnapshot(
                    credential_id,
                    pruned_timestamp,
                ));
        }

        env.storage().instance().set(
            &DataKey::CredentialSnapshotTimestamps(credential_id),
            &timestamps,
        );
        env.storage().instance().set(
            &DataKey::CredentialSnapshotVersions(credential_id),
            &versions,
        );
    }

    /// Returns `credential_id`'s immediate parent, or `None` if it is a root.
    fn load_parent(env: &Env, credential_id: u64) -> Option<u64> {
        env.storage()
            .instance()
            .get::<DataKey, u64>(&DataKey::CredentialParent(credential_id))
    }

    /// Walks `credential_id`'s ancestor chain (itself, its parent,
    /// grandparent, ...), panicking with `CredentialChainTooDeep` past
    /// MAX_CREDENTIAL_CHAIN_DEPTH hops or `ParentCredentialInvalid` if any
    /// ancestor has been invalidated by an upheld dispute. Every ancestor
    /// visited here is assumed to already exist, since `CredentialParent` is
    /// only ever set (by `create_derived_credential`) to a credential_id
    /// that was itself validated at the time it became a parent.
    fn validate_credential_chain(env: &Env, credential_id: u64) {
        let mut current = credential_id;
        let mut depth: u32 = 0;

        loop {
            if Self::is_credential_invalidated(env.clone(), current) {
                panic_with_error!(env, VerifierError::ParentCredentialInvalid);
            }

            match Self::load_parent(env, current) {
                Some(parent) => {
                    depth += 1;
                    if depth > MAX_CREDENTIAL_CHAIN_DEPTH {
                        panic_with_error!(env, VerifierError::CredentialChainTooDeep);
                    }
                    current = parent;
                }
                None => break,
            }
        }
    }

    fn require_admin(env: &Env) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, VerifierError::NotInitialized));
        admin.require_auth();
    }

    /// Validates size/emptiness constraints on `proof`/`claim`, then looks
    /// up whether a currently-registered oracle has attested this exact
    /// pair. Shared by [`Self::verify_claim`] and
    /// [`Self::verify_conditional_proof`] so both branch checks and the
    /// top-level claim check apply identical validation and revocation
    /// semantics.
    fn verify_internal(env: &Env, proof: &Bytes, claim: &Bytes) -> bool {
        if proof.is_empty() {
            panic_with_error!(env, VerifierError::EmptyProof);
        }
        if proof.len() > MAX_PROOF_SIZE {
            panic_with_error!(env, VerifierError::ProofTooLarge);
        }
        if claim.is_empty() {
            panic_with_error!(env, VerifierError::EmptyClaim);
        }
        if claim.len() > MAX_CLAIM_SIZE {
            panic_with_error!(env, VerifierError::ClaimTooLarge);
        }

        let proof_hash: BytesN<32> = env.crypto().sha256(proof).into();
        let claim_hash: BytesN<32> = env.crypto().sha256(claim).into();

        env.storage()
            .instance()
            .get::<DataKey, AttestationRecord>(&DataKey::Attestation(proof_hash, claim_hash))
            .is_some_and(|record| {
                env.storage()
                    .instance()
                    .get::<DataKey, bool>(&DataKey::Oracle(record.oracle))
                    .unwrap_or(false)
            })
    }

    /// Verify a batch of credentials with consistency checking.
    ///
    /// Verifies that multiple credentials are all valid (via `verify_claim`)
    /// and that they are mutually consistent (no conflicting claims).
    ///
    /// # Arguments
    ///
    /// * `credential_ids` - List of credential IDs to verify
    /// * `proofs` - Corresponding proof bytes for each credential
    /// * `claims` - Corresponding claim bytes for each credential
    ///
    /// # Returns
    ///
    /// `true` if all credentials are valid and consistent, `false` otherwise.
    ///
    /// # Panics
    ///
    /// Panics if the input lists are mismatched lengths or empty.
    pub fn verify_credentials_consistent(env: Env, proofs: Vec<Bytes>, claims: Vec<Bytes>) -> bool {
        if proofs.len() != claims.len() {
            panic_with_error!(&env, VerifierError::MismatchedBatchLengths);
        }
        if proofs.is_empty() {
            panic_with_error!(&env, VerifierError::EmptyBatchIds);
        }

        // Step 1: Verify each credential individually
        for (proof, claim) in proofs.iter().zip(claims.iter()) {
            if !Self::verify_claim(env.clone(), proof, claim) {
                return false;
            }
        }

        // Step 2: Check consistency between all pairs
        let mut consistency_pairs: Vec<(u64, Bytes, u64, Bytes)> = Vec::new(&env);

        for i in 0..claims.len() {
            for j in (i + 1)..claims.len() {
                if let (Some(claim_i), Some(claim_j)) = (claims.get(i), claims.get(j)) {
                    consistency_pairs.push_back((i as u64, claim_i, j as u64, claim_j));
                }
            }
        }

        // Verify batch consistency
        match CredentialRegistry::verify_batch_consistency(&env, consistency_pairs) {
            Ok(()) => true,
            Err(_) => false,
        }
    }

    /// Returns whether the credential's scheduled consistency re-check is
    /// due: the current ledger timestamp is at or past the attestation's
    /// `next_check_due` (set at attestation, advanced by
    /// [`Self::reschedule_consistency_check`]). Mirrors
    /// [`Self::is_credential_invalidated`]'s absence semantics — a
    /// credential id that was never attested has no schedule and is never
    /// due, so this returns `false` rather than panicking.
    ///
    /// When the check is due, this also publishes a `cons_due` event
    /// carrying `(credential_id, next_check_due)` so off-chain workers can
    /// pick the credential up for re-verification (e.g. via
    /// [`Self::verify_credentials_consistent`]) — the same "return a bool
    /// and emit an event" shape as `verify_claim`'s `vfy_claim` event.
    /// Publishing an event requires an actual transaction, so workers must
    /// *invoke* this (not view-call it) for the event to be recorded; the
    /// boolean result is available either way.
    ///
    /// A due check stays due until [`Self::reschedule_consistency_check`]
    /// advances the window, so a worker that misses one poll can still act
    /// on the next.
    pub fn is_consistency_check_due(env: Env, credential_id: u64) -> bool {
        let Some((proof_hash, claim_hash)) = env
            .storage()
            .instance()
            .get::<DataKey, (BytesN<32>, BytesN<32>)>(&DataKey::CredentialHashes(credential_id))
        else {
            return false;
        };
        let Some(record) = env.storage().instance().get::<DataKey, AttestationRecord>(
            &DataKey::Attestation(proof_hash, claim_hash),
        ) else {
            return false;
        };

        let due = env.ledger().timestamp() >= record.next_check_due;
        if due {
            env.events().publish(
                (CONSISTENCY_DUE_TOPIC,),
                (credential_id, record.next_check_due),
            );
        }
        due
    }

    /// Advances a credential's consistency-check schedule by one full
    /// [`CONSISTENCY_CHECK_INTERVAL`] from the current ledger timestamp.
    ///
    /// Off-chain workers call this after performing the re-verification the
    /// `cons_due` event signalled, which is what makes the check *periodic*
    /// rather than due-once-and-forever. It has no privileged inputs — it
    /// only moves a timer forward — so anyone may call it. Panics with
    /// `CredentialNotFound` if `credential_id` was never attested.
    pub fn reschedule_consistency_check(env: Env, credential_id: u64) {
        let Some((proof_hash, claim_hash)) = env
            .storage()
            .instance()
            .get::<DataKey, (BytesN<32>, BytesN<32>)>(&DataKey::CredentialHashes(credential_id))
        else {
            panic_with_error!(&env, VerifierError::CredentialNotFound);
        };

        let mut record = env
            .storage()
            .instance()
            .get::<DataKey, AttestationRecord>(&DataKey::Attestation(
                proof_hash.clone(),
                claim_hash.clone(),
            ))
            .unwrap_or_else(|| panic_with_error!(&env, VerifierError::CredentialNotFound));
        record.next_check_due = env
            .ledger()
            .timestamp()
            .saturating_add(CONSISTENCY_CHECK_INTERVAL);
        env.storage()
            .instance()
            .set(&DataKey::Attestation(proof_hash, claim_hash), &record);
    }
}

#[cfg(test)]
mod test;

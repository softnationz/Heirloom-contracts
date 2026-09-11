#![no_std]

#[cfg(test)]
mod atomic_release_tests;
#[cfg(test)]
mod recovery_tests;
mod compression;

use crate::compression::{
    compress_metadata as compress_metadata_bytes, decompress_metadata as decompress_metadata_bytes,
    is_compressed, MAX_METADATA_SIZE,
};
use soroban_sdk::{
    contract, contractclient, contracterror, contractimpl, contracttype, panic_with_error,
    symbol_short, Address, Bytes, BytesN, Env, Map, String, Vec,
};

const MINT_TOPIC: soroban_sdk::Symbol = symbol_short!("sbt_mint");
const COMPOSE_TOPIC: soroban_sdk::Symbol = symbol_short!("sbt_cmpse");
const DECOMPOSE_TOPIC: soroban_sdk::Symbol = symbol_short!("sbt_dcmp");
const SHARED_METADATA_TOPIC: soroban_sdk::Symbol = symbol_short!("sbt_smeta");
const BATCH_TRANSFER_TOPIC: soroban_sdk::Symbol = symbol_short!("sbt_btxfr");
const DELEGATE_TOPIC: soroban_sdk::Symbol = symbol_short!("sbt_dlg");
const REVOKE_DELEGATE_TOPIC: soroban_sdk::Symbol = symbol_short!("sbt_rdlg");
const METADATA_COMPRESSED_TOPIC: soroban_sdk::Symbol = symbol_short!("sbt_mcmp");
const FRACTIONAL_CREATED_TOPIC: soroban_sdk::Symbol = symbol_short!("frac_crt");
const ESCROW_CREATED_TOPIC: soroban_sdk::Symbol = symbol_short!("esc_crt");
const ESCROW_RELEASED_TOPIC: soroban_sdk::Symbol = symbol_short!("esc_rel");
const RECOVERY_CODES_GENERATED_TOPIC: soroban_sdk::Symbol = symbol_short!("rc_gen");
const RECOVERY_SUCCEEDED_TOPIC: soroban_sdk::Symbol = symbol_short!("rc_ok");
const RECOVERY_RATE_LIMITED_TOPIC: soroban_sdk::Symbol = symbol_short!("rc_rl");

/// Number of one-time recovery codes issued per `generate_sbt_recovery_codes` call.
pub const RECOVERY_CODE_COUNT: u32 = 5;
/// Recovery attempts allowed per `RECOVERY_ATTEMPT_WINDOW_SECONDS` window, per SBT.
pub const RECOVERY_MAX_ATTEMPTS: u32 = 5;
pub const RECOVERY_ATTEMPT_WINDOW_SECONDS: u64 = 3600;
/// Maximum hops a composition graph walk (cycle check or resolution) may
/// take before being treated as invalid. Matches ttl_vault's
/// MAX_INHERITANCE_DEPTH, the equivalent bound for template-inheritance
/// chain walks.
pub const MAX_COMPOSITION_DEPTH: u32 = 16;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum SbtError {
    /// Contract has already been initialized.
    AlreadyInitialized = 1,
    /// Contract has not been initialized.
    NotInitialized = 2,
    /// No SBT exists with the given id.
    TokenNotFound = 3,
    /// Metadata bytes were empty.
    EmptyMetadata = 4,
    /// The SBT is already composed with an NFT.
    AlreadyComposed = 5,
    /// The SBT is not currently composed with any NFT.
    NotComposed = 6,
    /// The caller does not own the referenced NFT on the target contract.
    NftOwnershipMismatch = 7,
    /// Delegation duration must be greater than zero.
    InvalidDuration = 8,
    /// The SBT has no active (non-expired) delegation.
    NoActiveDelegation = 9,
    /// `sbt_ids` and `transfers` must be the same length.
    MismatchedBatchLengths = 10,
    /// A conditional transfer's condition was not satisfied.
    TransferConditionNotMet = 11,
    /// Batch operations require at least one entry.
    EmptyBatch = 12,
    /// An SBT cannot be delegated to its own owner.
    SelfDelegation = 13,
    /// Metadata compression failed.
    MetadataCompressionFailed = 14,
    /// SBT id maps to compressed metadata; decompression must be used.
    MetadataIsCompressed = 15,
    /// SBT is fractionally owned; cannot perform operation on single-owner SBTs only.
    FractionalOwnershipExists = 16,
    /// Unanimous approval is required for fractional operations.
    ApprovalNotUnanimous = 17,
    /// Fraction total does not equal basis points (10000).
    InvalidFractionSum = 18,
    /// Holder not found in fractional ownership.
    HolderNotFound = 19,
    /// SBT is not in escrow.
    NotInEscrow = 20,
    /// SBT is already in escrow with another agent.
    AlreadyInEscrow = 21,
    /// Escrow conditions not met.
    EscrowConditionsNotMet = 22,
    /// Only escrow agent can perform this action.
    NotEscrowAgent = 23,
    /// Holder count and fraction count must match.
    MismatchedOwnershipArrays = 24,
    /// An escrowed credential has already been released.
    CredentialAlreadyReleased = 25,
    /// A credential id appears more than once in an atomic release batch.
    DuplicateCredentialId = 26,
    /// No unused recovery codes exist for this SBT.
    NoRecoveryCodes = 27,
    /// Recovery attempts have exceeded the allowed rate for the current window.
    RecoveryRateLimited = 28,
    /// A proposed composition would create a cycle, or the graph walk
    /// needed to check that exceeded MAX_COMPOSITION_DEPTH hops.
    InvalidCompositionGraph = 29,
}

/// Storage key discriminants. All SBT state is keyed by `sbt_id`.
#[contracttype]
pub enum DataKey {
    Admin,
    NextTokenId,
    Owner(u64),
    Metadata(u64),
    MintedAt(u64),
    Composition(u64),
    CompositionComponents(u64),
    SharedMetadata(u64),
    Delegation(u64),
    DelegationHistory(u64),
    /// Mapping of which SBTs have compressed metadata (schema_version >= 3).
    MetadataCompressed(u64),
    /// Fractional ownership for an SBT (issue #45).
    FractionalOwnership(u64),
    /// Ownership history for fractional SBTs (issue #45).
    OwnershipHistory(u64),
    /// SBT escrow records (issue #46).
    Escrow(u64),
    /// Escrow counter for generating unique escrow IDs.
    NextEscrowId,
    /// Escrow history for auditing.
    EscrowHistory(u64),
    /// sbt_id -> unused, hashed recovery codes (issue #51).
    RecoveryCodes(u64),
    /// sbt_id -> recovery attempt rate-limit state (issue #51).
    RecoveryAttempts(u64),
}

/// A bridge record linking an SBT to a token on a standard (transferable) NFT
/// contract. While composed, the SBT and the referenced NFT are treated as a
/// linked pair for the purposes of shared metadata; composing does not move
/// or lock the NFT itself, it only records the association on-chain.
#[contracttype]
#[derive(Clone)]
pub struct CompositionRecord {
    pub nft_address: Address,
    pub nft_id: u64,
    pub composed_at: u64,
}

/// Bounded local component references used when resolving composed ownership.
#[contracttype]
#[derive(Clone)]
pub struct CompositionComponents {
    pub component_ids: Vec<u64>,
}

#[contracttype]
#[derive(Clone)]
pub struct DelegationRecord {
    pub delegate: Address,
    pub delegated_at: u64,
    pub expires_at: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DelegationAction {
    Delegated,
    Revoked,
}

/// An append-only audit entry recording a single delegation lifecycle event.
#[contracttype]
#[derive(Clone)]
pub struct DelegationHistoryEntry {
    pub delegate: Address,
    pub action: DelegationAction,
    pub at: u64,
    pub expires_at: u64,
}

/// Represents fractional ownership of an SBT. Multiple holders can own portions of a single SBT.
#[contracttype]
#[derive(Clone)]
pub struct FractionalOwnership {
    pub sbt_id: u64,
    pub holders: Vec<Address>,
    pub fractions: Vec<u64>, // Each fraction is in basis points (0-10000), sum = 10000
    pub created_at: u64,
}

/// Ownership history entry for tracking fraction changes.
#[contracttype]
#[derive(Clone)]
pub struct OwnershipHistoryEntry {
    pub sbt_id: u64,
    pub holder: Address,
    pub fraction: u64,
    pub action: OwnershipAction,
    pub at: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OwnershipAction {
    Created,
    Updated,
    Removed,
}

/// SBT held in escrow pending condition satisfaction.
#[contracttype]
#[derive(Clone)]
pub struct EscrowRecord {
    pub escrow_id: u64,
    pub sbt_id: u64,
    pub escrow_agent: Address,
    pub conditions: Bytes,
    pub created_at: u64,
    pub released: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EscrowStatus {
    Active,
    Released,
    Disputed,
}

/// Per-SBT recovery attempt rate-limit state (issue #51). Both successful
/// and failed recovery attempts count toward `attempt_count` within a
/// `RECOVERY_ATTEMPT_WINDOW_SECONDS` window starting at `window_start`, so
/// brute-forcing a recovery code is bounded to `RECOVERY_MAX_ATTEMPTS`
/// guesses per window.
#[contracttype]
#[derive(Clone)]
pub struct RecoveryAttemptState {
    pub attempt_count: u32,
    pub window_start: u64,
}

/// A condition gating one leg of a conditional batch transfer.
#[contracttype]
#[derive(Clone)]
pub enum TransferCondition {
    /// No restriction beyond ownership.
    Always,
    /// The SBT must have been held by its current owner for at least this
    /// many seconds (measured from mint time).
    MinHoldSeconds(u64),
    /// The SBT must not currently have an active (non-expired) delegation.
    NoActiveDelegation,
}

/// One leg of a `batch_transfer_sbt_conditional` call: the destination owner
/// and the condition that must hold for the transfer to be applied.
#[contracttype]
#[derive(Clone)]
pub struct TransferInstruction {
    pub to: Address,
    pub condition: TransferCondition,
}

/// Minimal interface bridged to standard (transferable) NFT contracts so an
/// SBT can be composed with an NFT the same owner holds elsewhere.
#[contractclient(name = "NftClient")]
pub trait NftInterface {
    fn owner_of(env: Env, token_id: u64) -> Address;
}

#[contract]
pub struct SbtContract;

#[contractimpl]
impl SbtContract {
    // ---- admin/lifecycle ----

    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(&env, SbtError::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::NextTokenId, &0u64);
    }

    /// Mints a new soulbound token to `to`. Admin only.
    pub fn mint(env: Env, to: Address, metadata: String) -> u64 {
        Self::require_admin(&env);
        if metadata.is_empty() {
            panic_with_error!(&env, SbtError::EmptyMetadata);
        }

        let sbt_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextTokenId)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::NextTokenId, &(sbt_id + 1));

        env.storage().instance().set(&DataKey::Owner(sbt_id), &to);
        env.storage()
            .instance()
            .set(&DataKey::Metadata(sbt_id), &metadata);
        env.storage()
            .instance()
            .set(&DataKey::MintedAt(sbt_id), &env.ledger().timestamp());

        env.events().publish((MINT_TOPIC,), (sbt_id, to));
        sbt_id
    }

    pub fn owner_of(env: Env, sbt_id: u64) -> Address {
        Self::load_owner(&env, sbt_id)
    }

    pub fn get_metadata(env: Env, sbt_id: u64) -> String {
        if !Self::is_sbt_metadata_compressed(env.clone(), sbt_id) {
            return env
                .storage()
                .instance()
                .get(&DataKey::Metadata(sbt_id))
                .unwrap_or_else(|| panic_with_error!(&env, SbtError::TokenNotFound));
        }

        let compressed: Bytes = env
            .storage()
            .instance()
            .get(&DataKey::Metadata(sbt_id))
            .unwrap_or_else(|| panic_with_error!(&env, SbtError::TokenNotFound));
        let metadata = decompress_metadata_bytes(&env, &compressed)
            .unwrap_or_else(|_| panic_with_error!(&env, SbtError::MetadataCompressionFailed));
        Self::bytes_to_string(&env, &metadata)
    }

    /// Compress arbitrary credential metadata using the Heirloom MessagePack format.
    ///
    /// Metadata that would not become smaller is returned unchanged.
    pub fn compress_metadata(env: Env, metadata: Bytes) -> Bytes {
        compress_metadata_bytes(&env, &metadata)
    }

    /// Decompress current, legacy, or ordinary uncompressed credential metadata.
    pub fn decompress_metadata(env: Env, metadata: Bytes) -> Bytes {
        decompress_metadata_bytes(&env, &metadata)
            .unwrap_or_else(|_| panic_with_error!(&env, SbtError::MetadataCompressionFailed))
    }

    /// Compress an SBT's metadata in-place. Owner only.
    pub fn compress_sbt_metadata(env: Env, sbt_id: u64) -> u64 {
        Self::require_owner(&env, sbt_id);

        if Self::is_sbt_metadata_compressed(env.clone(), sbt_id) {
            return 0;
        }

        let metadata: String = env
            .storage()
            .instance()
            .get(&DataKey::Metadata(sbt_id))
            .unwrap_or_else(|| panic_with_error!(&env, SbtError::TokenNotFound));
        let metadata = Self::string_to_bytes(&env, &metadata);
        let compressed = compress_metadata_bytes(&env, &metadata);

        if !is_compressed(&compressed) {
            return 0;
        }

        let original_size = metadata.len() as u64;
        let compressed_size = compressed.len() as u64;

        env.storage()
            .instance()
            .set(&DataKey::Metadata(sbt_id), &compressed);
        env.storage()
            .instance()
            .set(&DataKey::MetadataCompressed(sbt_id), &true);

        env.events().publish(
            (METADATA_COMPRESSED_TOPIC,),
            (sbt_id, original_size, compressed_size),
        );

        original_size.saturating_sub(compressed_size)
    }

    /// Decompress an SBT's metadata if it was compressed, returning raw bytes.
    pub fn decompress_sbt_metadata(env: Env, sbt_id: u64) -> Bytes {
        if !Self::is_sbt_metadata_compressed(env.clone(), sbt_id) {
            let metadata: String = env
                .storage()
                .instance()
                .get(&DataKey::Metadata(sbt_id))
                .unwrap_or_else(|| panic_with_error!(&env, SbtError::TokenNotFound));
            return Self::string_to_bytes(&env, &metadata);
        }

        let metadata: Bytes = env
            .storage()
            .instance()
            .get(&DataKey::Metadata(sbt_id))
            .unwrap_or_else(|| panic_with_error!(&env, SbtError::TokenNotFound));

        decompress_metadata_bytes(&env, &metadata)
            .unwrap_or_else(|_| panic_with_error!(&env, SbtError::MetadataCompressionFailed))
    }

    /// Check if an SBT's metadata is compressed.
    pub fn is_sbt_metadata_compressed(env: Env, sbt_id: u64) -> bool {
        env.storage()
            .instance()
            .get::<DataKey, bool>(&DataKey::MetadataCompressed(sbt_id))
            .unwrap_or(false)
    }

    // ---- #45: Fractional Ownership ----

    /// Create a fractionally-owned SBT. All holders must approve.
    pub fn create_fractional_sbt(
        env: Env,
        sbt_id: u64,
        holders: Vec<Address>,
        fractions: Vec<u64>,
    ) -> u64 {
        Self::require_owner(&env, sbt_id);

        if holders.len() != fractions.len() {
            panic_with_error!(&env, SbtError::MismatchedOwnershipArrays);
        }
        if holders.is_empty() {
            panic_with_error!(&env, SbtError::EmptyBatch);
        }

        // Validate fractions sum to 10000 basis points
        let mut total: u64 = 0;
        for fraction in fractions.iter() {
            total = total.saturating_add(fraction);
        }
        if total != 10000 {
            panic_with_error!(&env, SbtError::InvalidFractionSum);
        }

        let fractional = FractionalOwnership {
            sbt_id,
            holders: holders.clone(),
            fractions: fractions.clone(),
            created_at: env.ledger().timestamp(),
        };

        env.storage()
            .instance()
            .set(&DataKey::FractionalOwnership(sbt_id), &fractional);

        // Record ownership history
        for (holder, fraction) in holders.iter().zip(fractions.iter()) {
            let history_entry = OwnershipHistoryEntry {
                sbt_id,
                holder: holder.clone(),
                fraction,
                action: OwnershipAction::Created,
                at: env.ledger().timestamp(),
            };
            Self::push_ownership_history(&env, sbt_id, history_entry);
        }

        env.events().publish((FRACTIONAL_CREATED_TOPIC,), sbt_id);
        sbt_id
    }

    /// Get fractional ownership details for an SBT.
    pub fn get_fractional_ownership(env: Env, sbt_id: u64) -> Option<FractionalOwnership> {
        env.storage()
            .instance()
            .get(&DataKey::FractionalOwnership(sbt_id))
    }

    /// Check if an SBT is fractionally owned.
    pub fn is_fractional(env: Env, sbt_id: u64) -> bool {
        env.storage()
            .instance()
            .has(&DataKey::FractionalOwnership(sbt_id))
    }

    // ---- #46: SBT Escrow for Conditional Transfer ----

    /// Place an SBT in escrow with conditions for release.
    pub fn escrow_sbt(env: Env, sbt_id: u64, escrow_agent: Address, conditions: Bytes) -> u64 {
        Self::require_owner(&env, sbt_id);

        if env.storage().instance().has(&DataKey::Escrow(sbt_id)) {
            panic_with_error!(&env, SbtError::AlreadyInEscrow);
        }

        let escrow_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextEscrowId)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::NextEscrowId, &(escrow_id + 1));

        let escrow = EscrowRecord {
            escrow_id,
            sbt_id,
            escrow_agent: escrow_agent.clone(),
            conditions: conditions.clone(),
            created_at: env.ledger().timestamp(),
            released: false,
        };

        env.storage()
            .instance()
            .set(&DataKey::Escrow(sbt_id), &escrow);

        env.events()
            .publish((ESCROW_CREATED_TOPIC,), (escrow_id, sbt_id, escrow_agent));

        escrow_id
    }

    /// Release an SBT from escrow after conditions are satisfied.
    pub fn release_sbt_from_escrow(env: Env, sbt_id: u64, proof: Bytes) {
        let escrow: EscrowRecord = env
            .storage()
            .instance()
            .get(&DataKey::Escrow(sbt_id))
            .unwrap_or_else(|| panic_with_error!(&env, SbtError::NotInEscrow));

        escrow.escrow_agent.require_auth();

        if proof.is_empty() {
            panic_with_error!(&env, SbtError::EscrowConditionsNotMet);
        }

        let mut updated_escrow = escrow.clone();
        updated_escrow.released = true;

        env.storage()
            .instance()
            .set(&DataKey::Escrow(sbt_id), &updated_escrow);

        env.events()
            .publish((ESCROW_RELEASED_TOPIC,), (updated_escrow.escrow_id, sbt_id));
    }

    /// Releases multiple escrowed credentials in one atomic invocation.
    ///
    /// Every credential is validated before any escrow record is updated. Each
    /// distinct escrow agent must authorize the invocation, which attests that
    /// the corresponding escrow condition has been satisfied. If any id is
    /// duplicated, missing from escrow, already released, or lacks agent
    /// authorization, Soroban aborts the invocation and commits no changes.
    ///
    /// The returned vector preserves the input order and contains `true` for
    /// every credential when the complete batch succeeds.
    pub fn atomic_release_credentials(env: Env, credential_ids: Vec<u64>) -> Vec<bool> {
        if credential_ids.is_empty() {
            panic_with_error!(&env, SbtError::EmptyBatch);
        }

        let mut seen_ids: Map<u64, bool> = Map::new(&env);
        let mut authorized_agents: Map<Address, bool> = Map::new(&env);
        let mut escrows: Vec<EscrowRecord> = Vec::new(&env);

        for credential_id in credential_ids.iter() {
            if seen_ids.contains_key(credential_id) {
                panic_with_error!(&env, SbtError::DuplicateCredentialId);
            }
            seen_ids.set(credential_id, true);

            let escrow: EscrowRecord = env
                .storage()
                .instance()
                .get(&DataKey::Escrow(credential_id))
                .unwrap_or_else(|| panic_with_error!(&env, SbtError::NotInEscrow));

            if escrow.released {
                panic_with_error!(&env, SbtError::CredentialAlreadyReleased);
            }

            if !authorized_agents.contains_key(escrow.escrow_agent.clone()) {
                escrow.escrow_agent.require_auth();
                authorized_agents.set(escrow.escrow_agent.clone(), true);
            }

            escrows.push_back(escrow);
        }

        let mut results = Vec::new(&env);
        for escrow in escrows.iter() {
            let mut released = escrow.clone();
            released.released = true;

            env.storage()
                .instance()
                .set(&DataKey::Escrow(released.sbt_id), &released);
            env.events().publish(
                (ESCROW_RELEASED_TOPIC,),
                (released.escrow_id, released.sbt_id),
            );
            results.push_back(true);
        }

        results
    }

    /// Get escrow details for an SBT if it is in escrow.
    pub fn get_escrow_status(env: Env, sbt_id: u64) -> Option<EscrowRecord> {
        env.storage().instance().get(&DataKey::Escrow(sbt_id))
    }

    // ---- #54: SBT composability with other NFTs ----

    /// Bridges this SBT to a token on a standard NFT contract. Requires the
    /// SBT owner to also currently own `nft_id` on `nft_address` (verified
    /// via a cross-contract call to the NFT contract's `owner_of`).
    pub fn compose_sbt_with_nft(env: Env, sbt_id: u64, nft_address: Address, nft_id: u64) {
        let owner = Self::require_owner(&env, sbt_id);

        if env.storage().instance().has(&DataKey::Composition(sbt_id)) {
            panic_with_error!(&env, SbtError::AlreadyComposed);
        }

        let nft_owner = NftClient::new(&env, &nft_address).owner_of(&nft_id);
        if nft_owner != owner {
            panic_with_error!(&env, SbtError::NftOwnershipMismatch);
        }

        let record = CompositionRecord {
            nft_address: nft_address.clone(),
            nft_id,
            composed_at: env.ledger().timestamp(),
        };
        env.storage()
            .instance()
            .set(&DataKey::Composition(sbt_id), &record);

        env.events()
            .publish((COMPOSE_TOPIC,), (sbt_id, nft_address, nft_id));
    }

    /// Removes the SBT's association with any composed NFT. The NFT itself
    /// is untouched — this only clears the on-chain link.
    pub fn decompose_sbt(env: Env, sbt_id: u64) {
        Self::require_owner(&env, sbt_id);

        if !env.storage().instance().has(&DataKey::Composition(sbt_id)) {
            panic_with_error!(&env, SbtError::NotComposed);
        }
        env.storage()
            .instance()
            .remove(&DataKey::Composition(sbt_id));

        env.events().publish((DECOMPOSE_TOPIC,), sbt_id);
    }

    pub fn get_composition(env: Env, sbt_id: u64) -> Option<CompositionRecord> {
        env.storage().instance().get(&DataKey::Composition(sbt_id))
    }

    /// Sets bounded local SBT components for an existing composition.
    /// Components are validated as a graph before storage is updated.
    pub fn set_composition_components(env: Env, sbt_id: u64, component_ids: Vec<u64>) {
        Self::require_owner(&env, sbt_id);
        if component_ids.is_empty() || Self::composition_reaches_any(&env, &component_ids, sbt_id) {
            panic_with_error!(&env, SbtError::InvalidCompositionGraph);
        }
        for component_id in component_ids.iter() {
            Self::load_owner(&env, component_id);
        }
        env.storage().instance().set(
            &DataKey::CompositionComponents(sbt_id),
            &CompositionComponents { component_ids },
        );
    }

    pub fn get_composition_components(env: Env, sbt_id: u64) -> Option<Vec<u64>> {
        env.storage()
            .instance()
            .get::<DataKey, CompositionComponents>(&DataKey::CompositionComponents(sbt_id))
            .map(|components| components.component_ids)
    }

    /// Resolves all reachable local components with a hard depth and cycle bound.
    pub fn resolve_composition(env: Env, sbt_id: u64) -> Vec<u64> {
        let mut visited = Map::new(&env);
        let mut resolved = Vec::new(&env);
        Self::resolve_composition_inner(&env, sbt_id, 0, &mut visited, &mut resolved);
        resolved
    }

    /// Sets metadata shared across the SBT and any NFT it is composed with.
    /// Owner only.
    pub fn set_shared_metadata(env: Env, sbt_id: u64, metadata: String) {
        Self::require_owner(&env, sbt_id);
        env.storage()
            .instance()
            .set(&DataKey::SharedMetadata(sbt_id), &metadata);
        env.events().publish((SHARED_METADATA_TOPIC,), sbt_id);
    }

    pub fn get_shared_metadata(env: Env, sbt_id: u64) -> Option<String> {
        env.storage()
            .instance()
            .get(&DataKey::SharedMetadata(sbt_id))
    }

    // ---- #53: batch transfer with conditions ----

    /// Transfers multiple SBTs in a single call, each gated by its own
    /// [`TransferCondition`]. `sbt_ids[i]` is transferred per
    /// `transfers[i]`. Every leg's ownership auth and condition are
    /// validated before any storage is mutated, so the whole batch either
    /// fully applies or (on any panic) fully reverts — Soroban only commits
    /// state changes from a top-level invocation that returns successfully.
    pub fn batch_transfer_sbt_conditional(
        env: Env,
        sbt_ids: Vec<u64>,
        transfers: Vec<TransferInstruction>,
    ) {
        if sbt_ids.len() != transfers.len() {
            panic_with_error!(&env, SbtError::MismatchedBatchLengths);
        }
        if sbt_ids.is_empty() {
            panic_with_error!(&env, SbtError::EmptyBatch);
        }

        for (sbt_id, instruction) in sbt_ids.iter().zip(transfers.iter()) {
            let owner = Self::require_owner(&env, sbt_id);
            if !Self::evaluate_condition(&env, sbt_id, &owner, &instruction.condition) {
                panic_with_error!(&env, SbtError::TransferConditionNotMet);
            }
        }

        for (sbt_id, instruction) in sbt_ids.iter().zip(transfers.iter()) {
            env.storage()
                .instance()
                .set(&DataKey::Owner(sbt_id), &instruction.to);
            // A delegation granted by the previous owner should not carry
            // over to the new owner.
            env.storage()
                .instance()
                .remove(&DataKey::Delegation(sbt_id));

            env.events()
                .publish((BATCH_TRANSFER_TOPIC,), (sbt_id, instruction.to.clone()));
        }
    }

    // ---- #52: delegation with time limits ----

    /// Temporarily delegates the SBT to `delegate` for `duration_seconds`.
    /// Does not change ownership. Owner only.
    pub fn delegate_sbt_temporarily(
        env: Env,
        sbt_id: u64,
        delegate: Address,
        duration_seconds: u64,
    ) {
        let owner = Self::require_owner(&env, sbt_id);
        if duration_seconds == 0 {
            panic_with_error!(&env, SbtError::InvalidDuration);
        }
        if delegate == owner {
            panic_with_error!(&env, SbtError::SelfDelegation);
        }

        let now = env.ledger().timestamp();
        let expires_at = now.saturating_add(duration_seconds);

        let record = DelegationRecord {
            delegate: delegate.clone(),
            delegated_at: now,
            expires_at,
        };
        env.storage()
            .instance()
            .set(&DataKey::Delegation(sbt_id), &record);

        Self::push_delegation_history(
            &env,
            sbt_id,
            DelegationHistoryEntry {
                delegate: delegate.clone(),
                action: DelegationAction::Delegated,
                at: now,
                expires_at,
            },
        );

        env.events()
            .publish((DELEGATE_TOPIC,), (sbt_id, delegate, expires_at));
    }

    /// Revokes the SBT's active delegation before it expires. Owner only.
    pub fn revoke_sbt_delegation(env: Env, sbt_id: u64) {
        Self::require_owner(&env, sbt_id);

        let record: DelegationRecord = env
            .storage()
            .instance()
            .get(&DataKey::Delegation(sbt_id))
            .unwrap_or_else(|| panic_with_error!(&env, SbtError::NoActiveDelegation));

        env.storage()
            .instance()
            .remove(&DataKey::Delegation(sbt_id));

        let now = env.ledger().timestamp();
        Self::push_delegation_history(
            &env,
            sbt_id,
            DelegationHistoryEntry {
                delegate: record.delegate.clone(),
                action: DelegationAction::Revoked,
                at: now,
                expires_at: record.expires_at,
            },
        );

        env.events()
            .publish((REVOKE_DELEGATE_TOPIC,), (sbt_id, record.delegate));
    }

    /// Returns the current delegate, or `None` if there is no delegation or
    /// it has expired. Expiry is enforced here at read time rather than by
    /// eagerly clearing storage, mirroring how attestations are honored
    /// elsewhere in this workspace only while still currently valid.
    pub fn get_active_delegate(env: Env, sbt_id: u64) -> Option<Address> {
        let record: DelegationRecord =
            env.storage().instance().get(&DataKey::Delegation(sbt_id))?;
        if record.expires_at > env.ledger().timestamp() {
            Some(record.delegate)
        } else {
            None
        }
    }

    pub fn get_delegation_history(env: Env, sbt_id: u64) -> Vec<DelegationHistoryEntry> {
        Self::load_delegation_history(&env, sbt_id)
    }

    // ---- #51: recovery code system ----
    //
    // Recovery codes let a lost SBT be reclaimed without the original
    // owner's signature (the whole point of a recovery flow is that the
    // owner can no longer sign). Only `sha256(code)` is ever stored; the
    // plaintext codes are returned once, at generation time, and the caller
    // is responsible for storing them off-chain. Regenerating replaces (and
    // so invalidates) any previously issued, unused codes.
    //
    // `recover_sbt_with_recovery_code` takes an explicit `new_holder`
    // address rather than relying on an implicit caller identity: Soroban
    // has no `msg.sender` equivalent, so the address regaining control must
    // be passed and authorized explicitly.
    //
    // Security note: recovery codes are generated with `env.prng()`, which
    // the Soroban SDK documents as unsuitable for secrets in applications
    // with low risk tolerance. This is acceptable for the scope of this
    // feature but should be revisited (e.g. moving to an off-chain-generated,
    // on-chain-committed scheme) before using this contract to guard
    // high-value identities.

    /// Generates `RECOVERY_CODE_COUNT` fresh one-time recovery codes for
    /// `sbt_id`, returning the plaintext codes. Only their SHA-256 hashes
    /// are persisted. Owner only.
    pub fn generate_sbt_recovery_codes(env: Env, sbt_id: u64) -> Vec<BytesN<32>> {
        Self::require_owner(&env, sbt_id);

        let prng = env.prng();
        let mut plaintext_codes: Vec<BytesN<32>> = Vec::new(&env);
        let mut hashed_codes: Vec<BytesN<32>> = Vec::new(&env);

        for _ in 0..RECOVERY_CODE_COUNT {
            let raw: [u8; 32] = prng.gen();
            let code = BytesN::from_array(&env, &raw);
            let code_bytes: Bytes = code.clone().into();
            let hash: BytesN<32> = env.crypto().sha256(&code_bytes).into();
            plaintext_codes.push_back(code);
            hashed_codes.push_back(hash);
        }

        env.storage()
            .instance()
            .set(&DataKey::RecoveryCodes(sbt_id), &hashed_codes);

        env.events().publish(
            (RECOVERY_CODES_GENERATED_TOPIC, sbt_id),
            hashed_codes.len(),
        );
        plaintext_codes
    }

    /// Reclaims `sbt_id` for `new_holder` by redeeming a still-unused
    /// recovery code. `new_holder` must authorize the call. Rate limited to
    /// `RECOVERY_MAX_ATTEMPTS` attempts per `RECOVERY_ATTEMPT_WINDOW_SECONDS`
    /// per SBT, counting both successful and failed attempts. Returns
    /// `true` and reassigns the holder on success; `false` if the code does
    /// not match any unused, stored hash.
    pub fn recover_sbt_with_recovery_code(
        env: Env,
        sbt_id: u64,
        recovery_code: Bytes,
        new_holder: Address,
    ) -> bool {
        new_holder.require_auth();
        // Panics with TokenNotFound if the SBT does not exist, before any
        // attempt budget is consumed.
        Self::load_owner(&env, sbt_id);

        // A fractionally-owned SBT has no single holder to reassign.
        if Self::is_fractional(env.clone(), sbt_id) {
            panic_with_error!(&env, SbtError::FractionalOwnershipExists);
        }

        Self::enforce_recovery_rate_limit(&env, sbt_id);

        let codes_key = DataKey::RecoveryCodes(sbt_id);
        let mut hashed_codes: Vec<BytesN<32>> = env
            .storage()
            .instance()
            .get(&codes_key)
            .unwrap_or_else(|| panic_with_error!(&env, SbtError::NoRecoveryCodes));

        let submitted_hash: BytesN<32> = env.crypto().sha256(&recovery_code).into();
        let Some(index) = hashed_codes.first_index_of(submitted_hash) else {
            return false;
        };

        hashed_codes.remove(index);
        env.storage().instance().set(&codes_key, &hashed_codes);

        env.storage()
            .instance()
            .set(&DataKey::Owner(sbt_id), &new_holder);
        // A delegation granted by the previous holder must not carry over to
        // the new holder, mirroring `batch_transfer_sbt_conditional`.
        env.storage().instance().remove(&DataKey::Delegation(sbt_id));

        env.events()
            .publish((RECOVERY_SUCCEEDED_TOPIC, sbt_id), new_holder);
        true
    }

    /// Enforces the per-SBT recovery attempt rate limit. Every call to
    /// `recover_sbt_with_recovery_code` — successful or failed — counts
    /// against `RECOVERY_MAX_ATTEMPTS` attempts per
    /// `RECOVERY_ATTEMPT_WINDOW_SECONDS`, so brute-forcing a recovery code
    /// is bounded. Panics with `RecoveryRateLimited` once the budget is
    /// exhausted; the window restarts on the first attempt after it elapses.
    fn enforce_recovery_rate_limit(env: &Env, sbt_id: u64) {
        let now = env.ledger().timestamp();
        let key = DataKey::RecoveryAttempts(sbt_id);
        let mut state = env
            .storage()
            .instance()
            .get::<DataKey, RecoveryAttemptState>(&key)
            .unwrap_or(RecoveryAttemptState {
                attempt_count: 0,
                window_start: now,
            });

        if now.saturating_sub(state.window_start) >= RECOVERY_ATTEMPT_WINDOW_SECONDS {
            state.window_start = now;
            state.attempt_count = 0;
        }

        if state.attempt_count >= RECOVERY_MAX_ATTEMPTS {
            env.events()
                .publish((RECOVERY_RATE_LIMITED_TOPIC, sbt_id), state.attempt_count);
            panic_with_error!(env, SbtError::RecoveryRateLimited);
        }

        state.attempt_count += 1;
        env.storage().instance().set(&key, &state);
    }

    // ---- helpers ----

    fn require_admin(env: &Env) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, SbtError::NotInitialized));
        admin.require_auth();
    }

    fn string_to_bytes(env: &Env, metadata: &String) -> Bytes {
        if metadata.len() > MAX_METADATA_SIZE {
            panic_with_error!(env, SbtError::MetadataCompressionFailed);
        }

        let length = metadata.len() as usize;
        let mut buffer = [0u8; MAX_METADATA_SIZE as usize];
        metadata.copy_into_slice(&mut buffer[..length]);
        Bytes::from_slice(env, &buffer[..length])
    }

    fn bytes_to_string(env: &Env, metadata: &Bytes) -> String {
        if metadata.len() > MAX_METADATA_SIZE {
            panic_with_error!(env, SbtError::MetadataCompressionFailed);
        }

        let length = metadata.len() as usize;
        let mut buffer = [0u8; MAX_METADATA_SIZE as usize];
        metadata.copy_into_slice(&mut buffer[..length]);
        String::from_bytes(env, &buffer[..length])
    }

    fn composition_reaches_any(env: &Env, roots: &Vec<u64>, target: u64) -> bool {
        let mut visited = Map::new(env);
        let mut pending = Vec::new(env);
        for root in roots.iter() {
            pending.push_back(root);
        }
        let mut steps = 0u32;
        while let Some(current) = pending.pop_back() {
            if current == target {
                return true;
            }
            if visited.contains_key(current) {
                continue;
            }
            visited.set(current, true);
            steps += 1;
            if steps > MAX_COMPOSITION_DEPTH {
                return true;
            }
            if let Some(components) = env.storage().instance().get::<DataKey, CompositionComponents>(
                &DataKey::CompositionComponents(current),
            ) {
                for component in components.component_ids.iter() {
                    pending.push_back(component);
                }
            }
        }
        false
    }

    fn resolve_composition_inner(
        env: &Env,
        current: u64,
        depth: u32,
        visited: &mut Map<u64, bool>,
        resolved: &mut Vec<u64>,
    ) {
        if depth >= MAX_COMPOSITION_DEPTH || visited.contains_key(current) {
            panic_with_error!(env, SbtError::InvalidCompositionGraph);
        }
        visited.set(current, true);
        if let Some(components) = env.storage().instance().get::<DataKey, CompositionComponents>(
            &DataKey::CompositionComponents(current),
        ) {
            for component in components.component_ids.iter() {
                Self::load_owner(env, component);
                resolved.push_back(component);
                Self::resolve_composition_inner(env, component, depth + 1, visited, resolved);
            }
        }
    }

    fn load_owner(env: &Env, sbt_id: u64) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Owner(sbt_id))
            .unwrap_or_else(|| panic_with_error!(env, SbtError::TokenNotFound))
    }

    fn require_owner(env: &Env, sbt_id: u64) -> Address {
        let owner = Self::load_owner(env, sbt_id);
        owner.require_auth();
        owner
    }

    fn has_active_delegation(env: &Env, sbt_id: u64) -> bool {
        env.storage()
            .instance()
            .get::<DataKey, DelegationRecord>(&DataKey::Delegation(sbt_id))
            .map(|record| record.expires_at > env.ledger().timestamp())
            .unwrap_or(false)
    }

    fn evaluate_condition(
        env: &Env,
        sbt_id: u64,
        _owner: &Address,
        condition: &TransferCondition,
    ) -> bool {
        match condition {
            TransferCondition::Always => true,
            TransferCondition::MinHoldSeconds(min_seconds) => {
                let minted_at: u64 = env
                    .storage()
                    .instance()
                    .get(&DataKey::MintedAt(sbt_id))
                    .unwrap_or(0);
                env.ledger().timestamp().saturating_sub(minted_at) >= *min_seconds
            }
            TransferCondition::NoActiveDelegation => !Self::has_active_delegation(env, sbt_id),
        }
    }

    fn load_delegation_history(env: &Env, sbt_id: u64) -> Vec<DelegationHistoryEntry> {
        env.storage()
            .instance()
            .get(&DataKey::DelegationHistory(sbt_id))
            .unwrap_or_else(|| Vec::new(env))
    }

    fn push_delegation_history(env: &Env, sbt_id: u64, entry: DelegationHistoryEntry) {
        let mut history = Self::load_delegation_history(env, sbt_id);
        history.push_back(entry);
        env.storage()
            .instance()
            .set(&DataKey::DelegationHistory(sbt_id), &history);
    }

    // ---- Helpers for fractional ownership ----

    fn load_ownership_history(env: &Env, sbt_id: u64) -> Vec<OwnershipHistoryEntry> {
        env.storage()
            .instance()
            .get(&DataKey::OwnershipHistory(sbt_id))
            .unwrap_or_else(|| Vec::new(env))
    }

    fn push_ownership_history(env: &Env, sbt_id: u64, entry: OwnershipHistoryEntry) {
        let mut history = Self::load_ownership_history(env, sbt_id);
        history.push_back(entry);
        env.storage()
            .instance()
            .set(&DataKey::OwnershipHistory(sbt_id), &history);
    }
}

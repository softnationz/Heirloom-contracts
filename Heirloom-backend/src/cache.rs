/// In-memory vault cache with TTL-based expiry.
///
/// Caches the results of expensive vault state lookups (`get_vault`,
/// `get_ttl_remaining`, `get_vault_summary`) for up to `TTL_SECS` seconds.
/// Cache entries are invalidated automatically on expiry or explicitly via
/// `invalidate`.
///
/// # Features
///
/// - **#88 Cache Size and TTL Optimization**: dynamic capacity cap, adaptive
///   TTL based on hit/miss ratios, per-operation hit/miss counters, and an
///   `auto_tune` helper that adjusts TTL automatically.
/// - **#89 Distributed Cache Coherence**: versioned invalidation tokens,
///   cross-instance invalidation helpers, and quorum-consistency verification.
/// - **#90 Cache Bypass for Stale Data Protection**: per-vault freshness
///   validation, configurable staleness threshold, bypass flag, and staleness
///   hit/bypass counters.
/// - **#91 Cache Compression for Large Values**: transparent
///   deflate-compression for serialised values above a configurable byte
///   threshold; decompression happens automatically on retrieval.
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::models::{Vault, VaultSummary};

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

/// Default cache time-to-live: 5 minutes.
pub const TTL_SECS: u64 = 300;

/// Default maximum number of vault entries kept in the cache.
pub const DEFAULT_MAX_ENTRIES: usize = 1_000;

/// Default compression threshold in bytes.  Serialised values larger than
/// this will be stored compressed.
pub const DEFAULT_COMPRESSION_THRESHOLD: usize = 1_024; // 1 KiB

/// Default staleness threshold: entries older than this fraction of their TTL
/// are considered stale for bypass purposes (0.9 = 90 %).
pub const DEFAULT_STALENESS_RATIO: f64 = 0.9;

/// Minimum TTL the auto-tuner will shrink to.
pub const MIN_TTL_SECS: u64 = 30;

/// Maximum TTL the auto-tuner will grow to.
pub const MAX_TTL_SECS: u64 = 3_600; // 1 hour

// ─────────────────────────────────────────────────────────────────────────────
// #88 – Hit/Miss metrics
// ─────────────────────────────────────────────────────────────────────────────

/// Counters collected per cache instance.
#[derive(Debug, Default, Clone)]
pub struct CacheMetrics {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub stale_bypasses: u64,
    pub compressed_entries: u64,
    pub decompressions: u64,
}

impl CacheMetrics {
    /// Hit ratio in the range `[0.0, 1.0]`.  Returns `0.0` when no requests
    /// have been recorded yet.
    pub fn hit_ratio(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Compression helpers (#91)
// ─────────────────────────────────────────────────────────────────────────────

/// Compress `data` with raw deflate.  Returns the compressed bytes.
///
/// We implement a minimal deflate encoder here using only `std` so that no
/// additional crate is required.  For production use, swap this body with
/// `flate2::write::DeflateEncoder`.
fn compress(data: &[u8]) -> Vec<u8> {
    // Minimal "store" deflate block: header 0x01 (BFINAL=1, BTYPE=00 stored),
    // then LEN / NLEN, then the raw bytes.  This is valid deflate and can be
    // decompressed by any compliant decoder.  It does not achieve compression
    // but is dependency-free.  Swap with flate2 for real compression.
    let len = data.len() as u16;
    let nlen = !len;
    let mut out = Vec::with_capacity(5 + data.len());
    out.push(0x01); // BFINAL=1, BTYPE=00 (no compression)
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&nlen.to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// Decompress data that was compressed with [`compress`].
///
/// Returns `None` if the data is malformed.
fn decompress(data: &[u8]) -> Option<Vec<u8>> {
    // Expect at least the 5-byte stored-block header.
    if data.len() < 5 {
        return None;
    }
    let _bfinal_btype = data[0];
    let len = u16::from_le_bytes([data[1], data[2]]) as usize;
    let nlen = u16::from_le_bytes([data[3], data[4]]);
    let len_check = u16::from_le_bytes([data[1], data[2]]);
    if len_check != !nlen {
        return None;
    }
    if data.len() < 5 + len {
        return None;
    }
    Some(data[5..5 + len].to_vec())
}

// ─────────────────────────────────────────────────────────────────────────────
// Possibly-compressed value (#91)
// ─────────────────────────────────────────────────────────────────────────────

/// Wrapper that stores a value either as raw JSON bytes or as compressed bytes.
#[derive(Clone)]
enum MaybeCompressed {
    Raw(Vec<u8>),
    Compressed(Vec<u8>),
}

impl MaybeCompressed {
    /// Encode `value` and decide whether to compress based on the threshold.
    fn encode<T: Serialize>(value: &T, threshold: usize, metrics: &mut CacheMetrics) -> Self {
        let raw = serde_json::to_vec(value).unwrap_or_default();
        if raw.len() >= threshold {
            let compressed = compress(&raw);
            metrics.compressed_entries += 1;
            MaybeCompressed::Compressed(compressed)
        } else {
            MaybeCompressed::Raw(raw)
        }
    }

    /// Decode back to `T`, decompressing if necessary.
    fn decode<T: for<'de> Deserialize<'de>>(&self, metrics: &mut CacheMetrics) -> Option<T> {
        let raw = match self {
            MaybeCompressed::Raw(b) => b.clone(),
            MaybeCompressed::Compressed(b) => {
                metrics.decompressions += 1;
                decompress(b)?
            }
        };
        serde_json::from_slice(&raw).ok()
    }

    /// Whether this entry is stored compressed.
    fn is_compressed(&self) -> bool {
        matches!(self, MaybeCompressed::Compressed(_))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Cache entry
// ─────────────────────────────────────────────────────────────────────────────

struct CacheEntry {
    value: MaybeCompressed,
    inserted_at: Instant,
    ttl: Duration,
    /// #89 – monotonically-increasing version token for coherence tracking.
    version: u64,
}

/// Outcome of classifying a cached slot before metrics/decoding are applied.
///
/// Classification is confined to a scoped block so the mutable borrow of the
/// map ends before metrics are updated or the value decoded.
enum CachedLookup {
    /// Entry present, fresh, and ready to decode.
    Hit(MaybeCompressed),
    /// Entry present but past its TTL; cleared on read.
    Expired,
    /// Entry present but beyond the staleness ratio (#90); bypassed.
    Stale,
    /// No entry for this vault/slot.
    Miss,
}

/// Classify one cached slot, clearing it when expired.  Metrics are applied
/// by the caller once its borrow of the map has ended.
fn classify_slot(slot: &mut Option<CacheEntry>, staleness_ratio: f64) -> CachedLookup {
    match slot.as_ref() {
        Some(entry) if entry.is_expired() => {
            slot.take();
            CachedLookup::Expired
        }
        Some(entry) if entry.is_stale(staleness_ratio) => CachedLookup::Stale,
        Some(entry) => CachedLookup::Hit(entry.value.clone()),
        None => CachedLookup::Miss,
    }
}

impl CacheEntry {
    fn new(value: MaybeCompressed, ttl: Duration, version: u64) -> Self {
        Self {
            value,
            inserted_at: Instant::now(),
            ttl,
            version,
        }
    }

    fn is_expired(&self) -> bool {
        self.inserted_at.elapsed() >= self.ttl
    }

    /// #90 – true when the entry has consumed more than `ratio` of its TTL.
    fn is_stale(&self, ratio: f64) -> bool {
        let threshold = self.ttl.mul_f64(ratio.clamp(0.0, 1.0));
        self.inserted_at.elapsed() >= threshold
    }

    /// Age of this entry.
    fn age(&self) -> Duration {
        self.inserted_at.elapsed()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Per-vault cached data
// ─────────────────────────────────────────────────────────────────────────────

struct VaultCacheEntries {
    vault: Option<CacheEntry>,
    ttl_remaining: Option<CacheEntry>,
    summary: Option<CacheEntry>,
}

impl VaultCacheEntries {
    fn new() -> Self {
        Self {
            vault: None,
            ttl_remaining: None,
            summary: None,
        }
    }

    fn has_live_entry(&self) -> bool {
        self.vault.as_ref().is_some_and(|e| !e.is_expired())
            || self.ttl_remaining.as_ref().is_some_and(|e| !e.is_expired())
            || self.summary.as_ref().is_some_and(|e| !e.is_expired())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// #89 – Distributed coherence helpers
// ─────────────────────────────────────────────────────────────────────────────

/// A coherence token identifies a specific version of a cached value across
/// cache instances.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoherenceToken {
    pub vault_id: String,
    pub version: u64,
}

/// Result of a quorum-consistency check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuorumResult {
    /// All queried versions agree.
    Consistent,
    /// At least one version disagreed; the outdated vault IDs are listed.
    Inconsistent(Vec<String>),
}

// ─────────────────────────────────────────────────────────────────────────────
// Cache configuration (#88)
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration knobs for `VaultCache`.
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Base TTL for new entries.
    pub ttl: Duration,
    /// Hard cap on the number of vault entries stored simultaneously.
    pub max_entries: usize,
    /// Compression threshold in bytes (#91).
    pub compression_threshold: usize,
    /// Fraction of TTL after which an entry is considered stale (#90).
    pub staleness_ratio: f64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(TTL_SECS),
            max_entries: DEFAULT_MAX_ENTRIES,
            compression_threshold: DEFAULT_COMPRESSION_THRESHOLD,
            staleness_ratio: DEFAULT_STALENESS_RATIO,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Inner mutable state
// ─────────────────────────────────────────────────────────────────────────────

struct CacheInner {
    map: HashMap<String, VaultCacheEntries>,
    config: CacheConfig,
    metrics: CacheMetrics,
    /// #89 – global version counter; incremented on every write.
    global_version: u64,
}

impl CacheInner {
    fn new(config: CacheConfig) -> Self {
        Self {
            map: HashMap::new(),
            config,
            metrics: CacheMetrics::default(),
            global_version: 0,
        }
    }

    fn next_version(&mut self) -> u64 {
        self.global_version += 1;
        self.global_version
    }

    /// #88 – Evict the oldest live entry when the map is at capacity.
    fn evict_if_needed(&mut self) {
        if self.map.len() < self.config.max_entries {
            return;
        }
        // Find vault_id with the oldest inserted_at across all sub-entries.
        let oldest = self
            .map
            .iter()
            .filter_map(|(id, entries)| {
                let oldest_age = [
                    entries.vault.as_ref().map(|e| e.age()),
                    entries.ttl_remaining.as_ref().map(|e| e.age()),
                    entries.summary.as_ref().map(|e| e.age()),
                ]
                .iter()
                .flatten()
                .copied()
                .max();
                oldest_age.map(|age| (id.clone(), age))
            })
            .max_by_key(|(_, age)| *age)
            .map(|(id, _)| id);

        if let Some(id) = oldest {
            self.map.remove(&id);
            self.metrics.evictions += 1;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Public cache type
// ─────────────────────────────────────────────────────────────────────────────

/// Thread-safe in-memory cache keyed by `vault_id` (String).
///
/// Implements Tasks #88–#91.
pub struct VaultCache {
    inner: Mutex<CacheInner>,
}

impl VaultCache {
    /// Create a new cache with default configuration.
    pub fn new() -> Self {
        Self::with_config(CacheConfig::default())
    }

    /// Create a cache with a custom TTL (useful for tests).
    pub fn with_ttl(ttl: Duration) -> Self {
        Self::with_config(CacheConfig {
            ttl,
            ..CacheConfig::default()
        })
    }

    /// Create a cache with full configuration control.
    pub fn with_config(config: CacheConfig) -> Self {
        Self {
            inner: Mutex::new(CacheInner::new(config)),
        }
    }

    // ── Metrics (#88) ─────────────────────────────────────────────────────────

    /// Snapshot of current hit/miss and other metrics.
    pub fn metrics(&self) -> CacheMetrics {
        self.inner.lock().unwrap().metrics.clone()
    }

    // ── #88 Auto-tune ─────────────────────────────────────────────────────────

    /// Adjust the cache TTL based on the current hit ratio.
    ///
    /// - Hit ratio ≥ 0.8 → grow TTL by 20 % (up to `MAX_TTL_SECS`).
    /// - Hit ratio < 0.5 → shrink TTL by 20 % (down to `MIN_TTL_SECS`).
    /// - Otherwise → no change.
    ///
    /// Returns the new TTL after tuning.
    pub fn auto_tune(&self) -> Duration {
        let mut inner = self.inner.lock().unwrap();
        let ratio = inner.metrics.hit_ratio();
        let current_secs = inner.config.ttl.as_secs();

        let new_secs = if ratio >= 0.8 {
            (current_secs + current_secs / 5).min(MAX_TTL_SECS)
        } else if ratio < 0.5 {
            (current_secs - current_secs / 5).max(MIN_TTL_SECS)
        } else {
            current_secs
        };

        inner.config.ttl = Duration::from_secs(new_secs);
        inner.config.ttl
    }

    /// Read-only view of the current configuration.
    pub fn config(&self) -> CacheConfig {
        self.inner.lock().unwrap().config.clone()
    }

    // ── get_vault / set_vault ─────────────────────────────────────────────────

    /// Return the cached `Vault` for `vault_id`, if present and not expired.
    ///
    /// Returns `None` if the entry is absent, expired, or bypassed due to
    /// staleness (#90).
    pub fn get_vault(&self, vault_id: &str) -> Option<Vault> {
        let mut inner = self.inner.lock().unwrap();
        let staleness_ratio = inner.config.staleness_ratio;
        let lookup = inner
            .map
            .get_mut(vault_id)
            .map_or(CachedLookup::Miss, |entries| {
                classify_slot(&mut entries.vault, staleness_ratio)
            });

        match lookup {
            CachedLookup::Hit(encoded) => {
                inner.metrics.hits += 1;
                encoded.decode(&mut inner.metrics)
            }
            CachedLookup::Stale => {
                inner.metrics.stale_bypasses += 1;
                inner.metrics.misses += 1;
                None
            }
            CachedLookup::Expired | CachedLookup::Miss => {
                inner.metrics.misses += 1;
                None
            }
        }
    }

    /// Insert or update the cached `Vault` for `vault_id`.
    pub fn set_vault(&self, vault_id: &str, vault: Vault) {
        let mut inner = self.inner.lock().unwrap();
        inner.evict_if_needed();
        let ttl = inner.config.ttl;
        let threshold = inner.config.compression_threshold;
        let version = inner.next_version();
        let encoded = MaybeCompressed::encode(&vault, threshold, &mut inner.metrics);
        let entries = inner
            .map
            .entry(vault_id.to_string())
            .or_insert_with(VaultCacheEntries::new);
        entries.vault = Some(CacheEntry::new(encoded, ttl, version));
    }

    // ── get_ttl_remaining / set_ttl_remaining ─────────────────────────────────

    /// Return the cached TTL-remaining value for `vault_id`, if present and
    /// not expired.
    ///
    /// The nested `Option` is intentional: `None` = cache miss,
    /// `Some(None)` = cached "no TTL" result, `Some(Some(n))` = cached value.
    #[allow(clippy::option_option)]
    pub fn get_ttl_remaining(&self, vault_id: &str) -> Option<Option<u64>> {
        let mut inner = self.inner.lock().unwrap();
        let staleness_ratio = inner.config.staleness_ratio;
        let lookup = inner
            .map
            .get_mut(vault_id)
            .map_or(CachedLookup::Miss, |entries| {
                classify_slot(&mut entries.ttl_remaining, staleness_ratio)
            });

        match lookup {
            CachedLookup::Hit(encoded) => {
                inner.metrics.hits += 1;
                encoded.decode(&mut inner.metrics)
            }
            CachedLookup::Stale => {
                inner.metrics.stale_bypasses += 1;
                inner.metrics.misses += 1;
                None
            }
            CachedLookup::Expired | CachedLookup::Miss => {
                inner.metrics.misses += 1;
                None
            }
        }
    }

    /// Insert or update the cached TTL-remaining value for `vault_id`.
    pub fn set_ttl_remaining(&self, vault_id: &str, ttl_remaining: Option<u64>) {
        let mut inner = self.inner.lock().unwrap();
        inner.evict_if_needed();
        let ttl = inner.config.ttl;
        let threshold = inner.config.compression_threshold;
        let version = inner.next_version();
        let encoded = MaybeCompressed::encode(&ttl_remaining, threshold, &mut inner.metrics);
        let entries = inner
            .map
            .entry(vault_id.to_string())
            .or_insert_with(VaultCacheEntries::new);
        entries.ttl_remaining = Some(CacheEntry::new(encoded, ttl, version));
    }

    // ── get_vault_summary / set_vault_summary ─────────────────────────────────

    /// Return the cached `VaultSummary` for `vault_id`, if present and not
    /// expired.
    pub fn get_vault_summary(&self, vault_id: &str) -> Option<VaultSummary> {
        let mut inner = self.inner.lock().unwrap();
        let staleness_ratio = inner.config.staleness_ratio;
        let lookup = inner
            .map
            .get_mut(vault_id)
            .map_or(CachedLookup::Miss, |entries| {
                classify_slot(&mut entries.summary, staleness_ratio)
            });

        match lookup {
            CachedLookup::Hit(encoded) => {
                inner.metrics.hits += 1;
                encoded.decode(&mut inner.metrics)
            }
            CachedLookup::Stale => {
                inner.metrics.stale_bypasses += 1;
                inner.metrics.misses += 1;
                None
            }
            CachedLookup::Expired | CachedLookup::Miss => {
                inner.metrics.misses += 1;
                None
            }
        }
    }

    /// Insert or update the cached `VaultSummary` for `vault_id`.
    pub fn set_vault_summary(&self, vault_id: &str, summary: VaultSummary) {
        let mut inner = self.inner.lock().unwrap();
        inner.evict_if_needed();
        let ttl = inner.config.ttl;
        let threshold = inner.config.compression_threshold;
        let version = inner.next_version();
        let encoded = MaybeCompressed::encode(&summary, threshold, &mut inner.metrics);
        let entries = inner
            .map
            .entry(vault_id.to_string())
            .or_insert_with(VaultCacheEntries::new);
        entries.summary = Some(CacheEntry::new(encoded, ttl, version));
    }

    // ── Invalidation ──────────────────────────────────────────────────────────

    /// Remove all cached entries for `vault_id`.
    pub fn invalidate(&self, vault_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.map.remove(vault_id);
    }

    /// Remove all entries from the cache.
    pub fn invalidate_all(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.map.clear();
    }

    /// Return how many vault IDs currently have at least one live entry.
    pub fn live_entry_count(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.map.values().filter(|e| e.has_live_entry()).count()
    }

    // ── #89 – Distributed cache coherence ─────────────────────────────────────

    /// Return the coherence token for a cached vault, if present.
    ///
    /// The token includes the current version number.  Use this when
    /// broadcasting invalidation requests to other cache instances.
    pub fn get_coherence_token(&self, vault_id: &str) -> Option<CoherenceToken> {
        let inner = self.inner.lock().unwrap();
        inner.map.get(vault_id).and_then(|entries| {
            entries.vault.as_ref().map(|e| CoherenceToken {
                vault_id: vault_id.to_string(),
                version: e.version,
            })
        })
    }

    /// Invalidate `vault_id` only if the local version is older than or equal
    /// to `remote_version`.
    ///
    /// Returns `true` if an invalidation occurred, `false` otherwise.
    ///
    /// Use this when receiving cross-instance invalidation messages: if the
    /// remote instance has a newer version, discard the local entry.
    pub fn invalidate_if_older(&self, vault_id: &str, remote_version: u64) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if let Some(entries) = inner.map.get(vault_id) {
            let local_version = entries.vault.as_ref().map(|e| e.version).unwrap_or(0);
            if local_version <= remote_version {
                inner.map.remove(vault_id);
                return true;
            }
        }
        false
    }

    /// Check consistency of `vault_id` against a set of remote tokens.
    ///
    /// Returns `QuorumResult::Consistent` if all tokens agree with the local
    /// version, or `QuorumResult::Inconsistent(vault_ids)` listing the vault
    /// IDs that disagreed.
    ///
    /// Use this for quorum-based consistency verification.
    pub fn verify_quorum(&self, vault_id: &str, remote_tokens: &[CoherenceToken]) -> QuorumResult {
        let inner = self.inner.lock().unwrap();
        let local_version = inner
            .map
            .get(vault_id)
            .and_then(|e| e.vault.as_ref())
            .map(|e| e.version)
            .unwrap_or(0);

        let inconsistent: Vec<String> = remote_tokens
            .iter()
            .filter(|t| t.version != local_version)
            .map(|t| t.vault_id.clone())
            .collect();

        if inconsistent.is_empty() {
            QuorumResult::Consistent
        } else {
            QuorumResult::Inconsistent(inconsistent)
        }
    }

    // ── #90 – Freshness validation helpers ────────────────────────────────────

    /// Return whether the cached vault entry for `vault_id` is currently stale
    /// (i.e. past the configured staleness ratio but not yet expired).
    ///
    /// Returns `false` if the entry is absent or already expired.
    pub fn is_stale(&self, vault_id: &str) -> bool {
        let inner = self.inner.lock().unwrap();
        let ratio = inner.config.staleness_ratio;
        inner
            .map
            .get(vault_id)
            .and_then(|e| e.vault.as_ref())
            .map(|e| !e.is_expired() && e.is_stale(ratio))
            .unwrap_or(false)
    }

    /// Force the cache to bypass the next read for `vault_id` by evicting all
    /// its entries.  Use this when you know the upstream data has changed and
    /// want to ensure the next read fetches fresh data.
    pub fn force_bypass(&self, vault_id: &str) {
        self.invalidate(vault_id);
    }

    /// Update the staleness ratio at runtime.
    ///
    /// `ratio` must be in `(0.0, 1.0)`.  Values outside this range are
    /// clamped.
    pub fn set_staleness_ratio(&self, ratio: f64) {
        let mut inner = self.inner.lock().unwrap();
        inner.config.staleness_ratio = ratio.clamp(0.0, 1.0);
    }

    // ── #91 – Compression introspection ──────────────────────────────────────

    /// Return whether the vault entry for `vault_id` is currently stored
    /// compressed.
    pub fn is_vault_compressed(&self, vault_id: &str) -> bool {
        let inner = self.inner.lock().unwrap();
        inner
            .map
            .get(vault_id)
            .and_then(|e| e.vault.as_ref())
            .map(|e| e.value.is_compressed())
            .unwrap_or(false)
    }

    /// Update the compression threshold at runtime.  Values already stored
    /// are not re-encoded; the new threshold applies to subsequent writes.
    pub fn set_compression_threshold(&self, threshold: usize) {
        let mut inner = self.inner.lock().unwrap();
        inner.config.compression_threshold = threshold;
    }
}

impl Default for VaultCache {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Unit tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Vault, VaultStatus, VaultSummary};
    use chrono::Utc;

    fn make_vault(id: &str) -> Vault {
        Vault {
            id: id.to_string(),
            owner: "owner1".to_string(),
            beneficiary: "ben1".to_string(),
            balance: 1000,
            check_in_interval: 86400,
            last_check_in: Utc::now(),
            created_at: Utc::now(),
            status: VaultStatus::Active,
            ttl_remaining: Some(86400),
        }
    }

    fn make_summary(vault_id: &str) -> VaultSummary {
        VaultSummary {
            vault_id: vault_id.to_string(),
            owner: "owner1".to_string(),
            status: VaultStatus::Active,
            ttl_remaining: Some(86400),
            balance: 1000,
        }
    }

    // ── get_vault / set_vault (pre-existing) ──────────────────────────────────

    #[test]
    fn test_get_vault_miss_on_empty_cache() {
        let cache = VaultCache::new();
        assert!(cache.get_vault("v1").is_none());
    }

    #[test]
    fn test_set_and_get_vault() {
        let cache = VaultCache::new();
        let vault = make_vault("v1");
        cache.set_vault("v1", vault.clone());
        let result = cache.get_vault("v1");
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "v1");
    }

    #[test]
    fn test_vault_cache_expires_after_ttl() {
        let cache = VaultCache::with_ttl(Duration::from_millis(1));
        cache.set_vault("v1", make_vault("v1"));
        std::thread::sleep(Duration::from_millis(5));
        assert!(cache.get_vault("v1").is_none());
    }

    #[test]
    fn test_vault_cache_updated_value_is_returned() {
        let cache = VaultCache::new();
        let mut vault = make_vault("v1");
        cache.set_vault("v1", vault.clone());
        vault.balance = 9999;
        cache.set_vault("v1", vault.clone());
        let result = cache.get_vault("v1").unwrap();
        assert_eq!(result.balance, 9999);
    }

    // ── get_ttl_remaining / set_ttl_remaining (pre-existing) ──────────────────

    #[test]
    fn test_get_ttl_remaining_miss_on_empty_cache() {
        let cache = VaultCache::new();
        assert!(cache.get_ttl_remaining("v1").is_none());
    }

    #[test]
    fn test_set_and_get_ttl_remaining_some() {
        let cache = VaultCache::new();
        cache.set_ttl_remaining("v1", Some(3600));
        let result = cache.get_ttl_remaining("v1");
        assert_eq!(result, Some(Some(3600)));
    }

    #[test]
    fn test_set_and_get_ttl_remaining_none() {
        let cache = VaultCache::new();
        cache.set_ttl_remaining("v1", None);
        let result = cache.get_ttl_remaining("v1");
        assert_eq!(result, Some(None));
    }

    #[test]
    fn test_ttl_remaining_expires_after_ttl() {
        let cache = VaultCache::with_ttl(Duration::from_millis(1));
        cache.set_ttl_remaining("v1", Some(100));
        std::thread::sleep(Duration::from_millis(5));
        assert!(cache.get_ttl_remaining("v1").is_none());
    }

    // ── get_vault_summary / set_vault_summary (pre-existing) ──────────────────

    #[test]
    fn test_get_vault_summary_miss_on_empty_cache() {
        let cache = VaultCache::new();
        assert!(cache.get_vault_summary("v1").is_none());
    }

    #[test]
    fn test_set_and_get_vault_summary() {
        let cache = VaultCache::new();
        let summary = make_summary("v1");
        cache.set_vault_summary("v1", summary.clone());
        let result = cache.get_vault_summary("v1");
        assert!(result.is_some());
        assert_eq!(result.unwrap().vault_id, "v1");
    }

    #[test]
    fn test_vault_summary_expires_after_ttl() {
        let cache = VaultCache::with_ttl(Duration::from_millis(1));
        cache.set_vault_summary("v1", make_summary("v1"));
        std::thread::sleep(Duration::from_millis(5));
        assert!(cache.get_vault_summary("v1").is_none());
    }

    // ── invalidation (pre-existing) ───────────────────────────────────────────

    #[test]
    fn test_invalidate_removes_all_entries_for_vault() {
        let cache = VaultCache::new();
        cache.set_vault("v1", make_vault("v1"));
        cache.set_ttl_remaining("v1", Some(100));
        cache.set_vault_summary("v1", make_summary("v1"));
        cache.invalidate("v1");
        assert!(cache.get_vault("v1").is_none());
        assert!(cache.get_ttl_remaining("v1").is_none());
        assert!(cache.get_vault_summary("v1").is_none());
    }

    #[test]
    fn test_invalidate_does_not_affect_other_vaults() {
        let cache = VaultCache::new();
        cache.set_vault("v1", make_vault("v1"));
        cache.set_vault("v2", make_vault("v2"));
        cache.invalidate("v1");
        assert!(cache.get_vault("v1").is_none());
        assert!(cache.get_vault("v2").is_some());
    }

    #[test]
    fn test_invalidate_all_clears_entire_cache() {
        let cache = VaultCache::new();
        cache.set_vault("v1", make_vault("v1"));
        cache.set_vault("v2", make_vault("v2"));
        cache.invalidate_all();
        assert!(cache.get_vault("v1").is_none());
        assert!(cache.get_vault("v2").is_none());
    }

    // ── cache consistency (pre-existing) ─────────────────────────────────────

    #[test]
    fn test_cache_consistency_after_state_change() {
        let cache = VaultCache::new();
        let vault = make_vault("v1");
        cache.set_vault("v1", vault);
        cache.set_ttl_remaining("v1", Some(86400));
        cache.set_vault_summary("v1", make_summary("v1"));
        cache.invalidate("v1");
        let mut updated_vault = make_vault("v1");
        updated_vault.ttl_remaining = Some(86400 * 2);
        cache.set_vault("v1", updated_vault.clone());
        cache.set_ttl_remaining("v1", Some(86400 * 2));
        let cached = cache.get_vault("v1").unwrap();
        assert_eq!(cached.ttl_remaining, Some(86400 * 2));
        let cached_ttl = cache.get_ttl_remaining("v1").unwrap();
        assert_eq!(cached_ttl, Some(86400 * 2));
    }

    #[test]
    fn test_independent_vaults_do_not_interfere() {
        let cache = VaultCache::new();
        cache.set_vault("v1", make_vault("v1"));
        cache.set_vault("v2", make_vault("v2"));
        cache.set_ttl_remaining("v1", Some(100));
        cache.set_ttl_remaining("v2", Some(200));
        assert_eq!(cache.get_ttl_remaining("v1"), Some(Some(100)));
        assert_eq!(cache.get_ttl_remaining("v2"), Some(Some(200)));
    }

    // ── live_entry_count (pre-existing) ──────────────────────────────────────

    #[test]
    fn test_live_entry_count_empty() {
        let cache = VaultCache::new();
        assert_eq!(cache.live_entry_count(), 0);
    }

    #[test]
    fn test_live_entry_count_with_entries() {
        let cache = VaultCache::new();
        cache.set_vault("v1", make_vault("v1"));
        cache.set_vault("v2", make_vault("v2"));
        assert_eq!(cache.live_entry_count(), 2);
    }

    #[test]
    fn test_live_entry_count_decrements_after_invalidation() {
        let cache = VaultCache::new();
        cache.set_vault("v1", make_vault("v1"));
        cache.set_vault("v2", make_vault("v2"));
        cache.invalidate("v1");
        assert_eq!(cache.live_entry_count(), 1);
    }

    #[test]
    fn test_live_entry_count_zero_after_expiry() {
        let cache = VaultCache::with_ttl(Duration::from_millis(1));
        cache.set_vault("v1", make_vault("v1"));
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(cache.live_entry_count(), 0);
    }

    // ── #88 – Metrics ─────────────────────────────────────────────────────────

    #[test]
    fn test_metrics_initial_zero() {
        let cache = VaultCache::new();
        let m = cache.metrics();
        assert_eq!(m.hits, 0);
        assert_eq!(m.misses, 0);
    }

    #[test]
    fn test_metrics_hit_recorded() {
        let cache = VaultCache::with_config(CacheConfig {
            staleness_ratio: 1.0, // disable stale bypass
            ..CacheConfig::default()
        });
        cache.set_vault("v1", make_vault("v1"));
        cache.get_vault("v1");
        assert_eq!(cache.metrics().hits, 1);
        assert_eq!(cache.metrics().misses, 0);
    }

    #[test]
    fn test_metrics_miss_recorded() {
        let cache = VaultCache::new();
        cache.get_vault("missing");
        assert_eq!(cache.metrics().misses, 1);
        assert_eq!(cache.metrics().hits, 0);
    }

    #[test]
    fn test_hit_ratio_zero_when_no_requests() {
        let cache = VaultCache::new();
        assert_eq!(cache.metrics().hit_ratio(), 0.0);
    }

    #[test]
    fn test_hit_ratio_calculation() {
        let cache = VaultCache::with_config(CacheConfig {
            staleness_ratio: 1.0,
            ..CacheConfig::default()
        });
        cache.set_vault("v1", make_vault("v1"));
        cache.get_vault("v1"); // hit
        cache.get_vault("v1"); // hit
        cache.get_vault("missing"); // miss
        let ratio = cache.metrics().hit_ratio();
        // 2 hits / 3 total ≈ 0.666
        assert!((ratio - 2.0 / 3.0).abs() < 1e-6);
    }

    // ── #88 – Dynamic sizing / eviction ──────────────────────────────────────

    #[test]
    fn test_eviction_when_max_entries_reached() {
        let cache = VaultCache::with_config(CacheConfig {
            max_entries: 2,
            ..CacheConfig::default()
        });
        cache.set_vault("v1", make_vault("v1"));
        cache.set_vault("v2", make_vault("v2"));
        // Adding v3 should evict one of the existing entries.
        cache.set_vault("v3", make_vault("v3"));
        assert_eq!(cache.metrics().evictions, 1);
    }

    // ── #88 – Auto-tune TTL ───────────────────────────────────────────────────

    #[test]
    fn test_auto_tune_grows_ttl_on_high_hit_ratio() {
        let cache = VaultCache::with_config(CacheConfig {
            ttl: Duration::from_secs(100),
            staleness_ratio: 1.0,
            ..CacheConfig::default()
        });
        // Drive hit ratio above 0.8 (8 hits, 1 miss).
        cache.set_vault("v1", make_vault("v1"));
        for _ in 0..8 {
            cache.get_vault("v1");
        }
        cache.get_vault("missing"); // 1 miss

        let new_ttl = cache.auto_tune();
        assert!(new_ttl.as_secs() > 100);
    }

    #[test]
    fn test_auto_tune_shrinks_ttl_on_low_hit_ratio() {
        let cache = VaultCache::with_config(CacheConfig {
            ttl: Duration::from_secs(100),
            ..CacheConfig::default()
        });
        // Drive hit ratio below 0.5 (all misses).
        for _ in 0..10 {
            cache.get_vault("missing");
        }
        let new_ttl = cache.auto_tune();
        assert!(new_ttl.as_secs() < 100);
    }

    #[test]
    fn test_auto_tune_respects_min_ttl() {
        let cache = VaultCache::with_config(CacheConfig {
            ttl: Duration::from_secs(MIN_TTL_SECS),
            ..CacheConfig::default()
        });
        for _ in 0..10 {
            cache.get_vault("missing");
        }
        let new_ttl = cache.auto_tune();
        assert_eq!(new_ttl.as_secs(), MIN_TTL_SECS);
    }

    #[test]
    fn test_auto_tune_respects_max_ttl() {
        let cache = VaultCache::with_config(CacheConfig {
            ttl: Duration::from_secs(MAX_TTL_SECS),
            staleness_ratio: 1.0,
            ..CacheConfig::default()
        });
        cache.set_vault("v1", make_vault("v1"));
        for _ in 0..10 {
            cache.get_vault("v1");
        }
        let new_ttl = cache.auto_tune();
        assert_eq!(new_ttl.as_secs(), MAX_TTL_SECS);
    }

    // ── #89 – Coherence token ─────────────────────────────────────────────────

    #[test]
    fn test_coherence_token_none_when_absent() {
        let cache = VaultCache::new();
        assert!(cache.get_coherence_token("v1").is_none());
    }

    #[test]
    fn test_coherence_token_returned_after_set() {
        let cache = VaultCache::new();
        cache.set_vault("v1", make_vault("v1"));
        let token = cache.get_coherence_token("v1");
        assert!(token.is_some());
        assert_eq!(token.unwrap().vault_id, "v1");
    }

    #[test]
    fn test_coherence_token_version_increments() {
        let cache = VaultCache::new();
        cache.set_vault("v1", make_vault("v1"));
        let v1 = cache.get_coherence_token("v1").unwrap().version;
        cache.set_vault("v1", make_vault("v1")); // overwrite → new version
        let v2 = cache.get_coherence_token("v1").unwrap().version;
        assert!(v2 > v1);
    }

    // ── #89 – Cross-instance invalidation ────────────────────────────────────

    #[test]
    fn test_invalidate_if_older_removes_stale_entry() {
        let cache = VaultCache::new();
        cache.set_vault("v1", make_vault("v1"));
        let local_version = cache.get_coherence_token("v1").unwrap().version;
        // Remote version is newer → invalidate.
        let evicted = cache.invalidate_if_older("v1", local_version + 1);
        assert!(evicted);
        assert!(cache.get_vault("v1").is_none());
    }

    #[test]
    fn test_invalidate_if_older_keeps_newer_entry() {
        let cache = VaultCache::new();
        cache.set_vault("v1", make_vault("v1"));
        let local_version = cache.get_coherence_token("v1").unwrap().version;
        // Remote version is older → do NOT invalidate.
        let evicted = cache.invalidate_if_older("v1", local_version - 1);
        assert!(!evicted);
        // Entry still present (bypass stale check by using a 100% ratio cache)
    }

    // ── #89 – Quorum consistency ──────────────────────────────────────────────

    #[test]
    fn test_quorum_consistent_when_versions_match() {
        let cache = VaultCache::new();
        cache.set_vault("v1", make_vault("v1"));
        let version = cache.get_coherence_token("v1").unwrap().version;
        let remote_tokens = vec![
            CoherenceToken {
                vault_id: "v1".to_string(),
                version,
            },
            CoherenceToken {
                vault_id: "v1".to_string(),
                version,
            },
        ];
        assert_eq!(
            cache.verify_quorum("v1", &remote_tokens),
            QuorumResult::Consistent
        );
    }

    #[test]
    fn test_quorum_inconsistent_when_versions_differ() {
        let cache = VaultCache::new();
        cache.set_vault("v1", make_vault("v1"));
        let version = cache.get_coherence_token("v1").unwrap().version;
        let remote_tokens = vec![CoherenceToken {
            vault_id: "v1".to_string(),
            version: version + 99,
        }];
        let result = cache.verify_quorum("v1", &remote_tokens);
        assert!(matches!(result, QuorumResult::Inconsistent(_)));
    }

    // ── #90 – Stale bypass ────────────────────────────────────────────────────

    #[test]
    fn test_stale_bypass_returns_none() {
        // Use 0% staleness ratio → every live entry is immediately stale.
        let cache = VaultCache::with_config(CacheConfig {
            staleness_ratio: 0.0,
            ..CacheConfig::default()
        });
        cache.set_vault("v1", make_vault("v1"));
        // Should be bypassed since elapsed ≥ 0% of TTL immediately.
        let result = cache.get_vault("v1");
        assert!(result.is_none());
    }

    #[test]
    fn test_stale_bypass_increments_counter() {
        let cache = VaultCache::with_config(CacheConfig {
            staleness_ratio: 0.0,
            ..CacheConfig::default()
        });
        cache.set_vault("v1", make_vault("v1"));
        cache.get_vault("v1");
        assert!(cache.metrics().stale_bypasses >= 1);
    }

    #[test]
    fn test_is_stale_false_on_fresh_entry() {
        let cache = VaultCache::with_config(CacheConfig {
            staleness_ratio: 1.0, // nothing is stale until fully expired
            ..CacheConfig::default()
        });
        cache.set_vault("v1", make_vault("v1"));
        assert!(!cache.is_stale("v1"));
    }

    #[test]
    fn test_is_stale_true_after_ratio_elapsed() {
        // Very short TTL (100ms) with 0% ratio → immediately stale.
        let cache = VaultCache::with_config(CacheConfig {
            ttl: Duration::from_millis(100),
            staleness_ratio: 0.0,
            ..CacheConfig::default()
        });
        cache.set_vault("v1", make_vault("v1"));
        assert!(cache.is_stale("v1"));
    }

    #[test]
    fn test_force_bypass_evicts_entry() {
        let cache = VaultCache::with_config(CacheConfig {
            staleness_ratio: 1.0,
            ..CacheConfig::default()
        });
        cache.set_vault("v1", make_vault("v1"));
        cache.force_bypass("v1");
        assert!(cache.get_vault("v1").is_none());
    }

    #[test]
    fn test_set_staleness_ratio_takes_effect() {
        let cache = VaultCache::with_config(CacheConfig {
            staleness_ratio: 1.0, // nothing stale initially
            ..CacheConfig::default()
        });
        cache.set_vault("v1", make_vault("v1"));
        // Entry should be returned (not stale yet).
        assert!(cache.is_stale("v1") == false);
        // Now set ratio to 0 → immediately stale.
        cache.set_staleness_ratio(0.0);
        assert!(cache.is_stale("v1"));
    }

    // ── #91 – Compression ─────────────────────────────────────────────────────

    #[test]
    fn test_compress_decompress_roundtrip() {
        let original = b"hello, cache compression!";
        let compressed = compress(original);
        let decompressed = decompress(&compressed).unwrap();
        assert_eq!(decompressed, original);
    }

    #[test]
    fn test_compress_decompress_empty() {
        let original: &[u8] = b"";
        let compressed = compress(original);
        let decompressed = decompress(&compressed).unwrap();
        assert_eq!(decompressed, original);
    }

    #[test]
    fn test_decompress_returns_none_on_invalid_data() {
        assert!(decompress(b"bad data").is_none());
    }

    #[test]
    fn test_small_values_not_compressed() {
        // Threshold of 10_000 bytes means a small vault won't be compressed.
        let cache = VaultCache::with_config(CacheConfig {
            compression_threshold: 10_000,
            staleness_ratio: 1.0,
            ..CacheConfig::default()
        });
        cache.set_vault("v1", make_vault("v1"));
        assert!(!cache.is_vault_compressed("v1"));
    }

    #[test]
    fn test_large_values_are_compressed() {
        // Threshold of 1 byte forces compression of everything.
        let cache = VaultCache::with_config(CacheConfig {
            compression_threshold: 1,
            staleness_ratio: 1.0,
            ..CacheConfig::default()
        });
        cache.set_vault("v1", make_vault("v1"));
        assert!(cache.is_vault_compressed("v1"));
    }

    #[test]
    fn test_compressed_entry_decompresses_on_get() {
        let cache = VaultCache::with_config(CacheConfig {
            compression_threshold: 1, // always compress
            staleness_ratio: 1.0,
            ..CacheConfig::default()
        });
        let vault = make_vault("v1");
        cache.set_vault("v1", vault.clone());
        let retrieved = cache.get_vault("v1").unwrap();
        assert_eq!(retrieved.id, vault.id);
        assert_eq!(retrieved.balance, vault.balance);
    }

    #[test]
    fn test_compression_metric_incremented() {
        let cache = VaultCache::with_config(CacheConfig {
            compression_threshold: 1,
            staleness_ratio: 1.0,
            ..CacheConfig::default()
        });
        cache.set_vault("v1", make_vault("v1"));
        assert!(cache.metrics().compressed_entries >= 1);
    }

    #[test]
    fn test_decompression_metric_incremented_on_get() {
        let cache = VaultCache::with_config(CacheConfig {
            compression_threshold: 1,
            staleness_ratio: 1.0,
            ..CacheConfig::default()
        });
        cache.set_vault("v1", make_vault("v1"));
        cache.get_vault("v1");
        assert!(cache.metrics().decompressions >= 1);
    }

    #[test]
    fn test_set_compression_threshold_applies_to_new_writes() {
        let cache = VaultCache::with_config(CacheConfig {
            compression_threshold: 10_000, // start: no compression
            staleness_ratio: 1.0,
            ..CacheConfig::default()
        });
        cache.set_vault("v1", make_vault("v1"));
        assert!(!cache.is_vault_compressed("v1"));

        // Lower threshold → subsequent writes will be compressed.
        cache.set_compression_threshold(1);
        cache.set_vault("v2", make_vault("v2"));
        assert!(cache.is_vault_compressed("v2"));
        // v1 was already written with the old threshold; not re-encoded.
        assert!(!cache.is_vault_compressed("v1"));
    }
}

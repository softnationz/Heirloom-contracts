/// Multi-level caching strategy (L1: in-memory, L2: simulated persistent store).
///
/// Implements a two-level cache hierarchy:
/// - L1 (in-memory): Fast, small capacity, short TTL.
/// - L2 (persistent/Redis-compatible interface): Slower, large capacity, longer TTL.
///
/// Cache coherence between levels is maintained on write (write-through)
/// and on miss (read-through with promotion).
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::models::{Vault, VaultSummary};

// ── Configuration ─────────────────────────────────────────────────────────────

/// L1 cache TTL: 1 minute (fast, in-process cache).
pub const L1_TTL_SECS: u64 = 60;

/// L2 cache TTL: 30 minutes (slower, larger capacity).
pub const L2_TTL_SECS: u64 = 1800;

/// L1 maximum entry count — evict LRU when exceeded.
pub const L1_MAX_ENTRIES: usize = 500;

// ── Generic Cache Entry ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Entry<T> {
    value: T,
    inserted_at: Instant,
    ttl: Duration,
    access_count: u64,
    last_accessed: Instant,
}

impl<T: Clone> Entry<T> {
    fn new(value: T, ttl: Duration) -> Self {
        let now = Instant::now();
        Self {
            value,
            inserted_at: now,
            ttl,
            access_count: 0,
            last_accessed: now,
        }
    }

    fn is_expired(&self) -> bool {
        self.inserted_at.elapsed() >= self.ttl
    }

    fn access(&mut self) -> T {
        self.access_count += 1;
        self.last_accessed = Instant::now();
        self.value.clone()
    }
}

// ── L1 In-Memory Cache ────────────────────────────────────────────────────────

struct L1Cache {
    vaults: HashMap<String, Entry<Vault>>,
    ttl_remaining: HashMap<String, Entry<Option<u64>>>,
    summaries: HashMap<String, Entry<VaultSummary>>,
    max_entries: usize,
    stats: L1Stats,
}

#[derive(Debug, Default, Clone)]
pub struct L1Stats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub insertions: u64,
}

impl L1Cache {
    fn new(max_entries: usize) -> Self {
        Self {
            vaults: HashMap::new(),
            ttl_remaining: HashMap::new(),
            summaries: HashMap::new(),
            max_entries,
            stats: L1Stats::default(),
        }
    }

    fn get_vault(&mut self, vault_id: &str) -> Option<Vault> {
        if let Some(entry) = self.vaults.get_mut(vault_id) {
            if !entry.is_expired() {
                self.stats.hits += 1;
                return Some(entry.access());
            }
            self.vaults.remove(vault_id);
        }
        self.stats.misses += 1;
        None
    }

    fn set_vault(&mut self, vault_id: &str, vault: Vault, ttl: Duration) {
        self.maybe_evict(&mut self.vaults.len().clone());
        self.vaults
            .insert(vault_id.to_string(), Entry::new(vault, ttl));
        self.stats.insertions += 1;
    }

    fn get_ttl_remaining(&mut self, vault_id: &str) -> Option<Option<u64>> {
        if let Some(entry) = self.ttl_remaining.get_mut(vault_id) {
            if !entry.is_expired() {
                self.stats.hits += 1;
                return Some(entry.access());
            }
            self.ttl_remaining.remove(vault_id);
        }
        self.stats.misses += 1;
        None
    }

    fn set_ttl_remaining(&mut self, vault_id: &str, value: Option<u64>, ttl: Duration) {
        self.ttl_remaining
            .insert(vault_id.to_string(), Entry::new(value, ttl));
        self.stats.insertions += 1;
    }

    fn get_summary(&mut self, vault_id: &str) -> Option<VaultSummary> {
        if let Some(entry) = self.summaries.get_mut(vault_id) {
            if !entry.is_expired() {
                self.stats.hits += 1;
                return Some(entry.access());
            }
            self.summaries.remove(vault_id);
        }
        self.stats.misses += 1;
        None
    }

    fn set_summary(&mut self, vault_id: &str, summary: VaultSummary, ttl: Duration) {
        self.summaries
            .insert(vault_id.to_string(), Entry::new(summary, ttl));
        self.stats.insertions += 1;
    }

    fn invalidate(&mut self, vault_id: &str) {
        self.vaults.remove(vault_id);
        self.ttl_remaining.remove(vault_id);
        self.summaries.remove(vault_id);
    }

    fn invalidate_all(&mut self) {
        self.vaults.clear();
        self.ttl_remaining.clear();
        self.summaries.clear();
    }

    fn live_entry_count(&self) -> usize {
        let vault_count = self.vaults.values().filter(|e| !e.is_expired()).count();
        let ttl_count = self
            .ttl_remaining
            .values()
            .filter(|e| !e.is_expired())
            .count();
        let summary_count = self.summaries.values().filter(|e| !e.is_expired()).count();
        vault_count.max(ttl_count).max(summary_count)
    }

    /// LRU eviction: remove the least recently accessed entry when at capacity.
    fn maybe_evict(&mut self, current_len: &usize) {
        if *current_len >= self.max_entries {
            if let Some(oldest_key) = self
                .vaults
                .iter()
                .min_by_key(|(_, e)| e.last_accessed)
                .map(|(k, _)| k.clone())
            {
                self.vaults.remove(&oldest_key);
                self.stats.evictions += 1;
            }
        }
    }
}

// ── L2 Cache (Redis-compatible interface) ─────────────────────────────────────
//
// In production this would be backed by a Redis client. Here we implement
// the same interface with an in-memory store so the logic is testable without
// a running Redis instance. Swap `L2Store` for a real Redis client in deployment.

struct L2Cache {
    vaults: HashMap<String, Entry<Vault>>,
    ttl_remaining: HashMap<String, Entry<Option<u64>>>,
    summaries: HashMap<String, Entry<VaultSummary>>,
    stats: L2Stats,
}

#[derive(Debug, Default, Clone)]
pub struct L2Stats {
    pub hits: u64,
    pub misses: u64,
    pub promotions: u64, // Entries promoted from L2 → L1.
    pub insertions: u64,
}

impl L2Cache {
    fn new() -> Self {
        Self {
            vaults: HashMap::new(),
            ttl_remaining: HashMap::new(),
            summaries: HashMap::new(),
            stats: L2Stats::default(),
        }
    }

    fn get_vault(&mut self, vault_id: &str) -> Option<Vault> {
        if let Some(entry) = self.vaults.get_mut(vault_id) {
            if !entry.is_expired() {
                self.stats.hits += 1;
                return Some(entry.access());
            }
            self.vaults.remove(vault_id);
        }
        self.stats.misses += 1;
        None
    }

    fn set_vault(&mut self, vault_id: &str, vault: Vault, ttl: Duration) {
        self.vaults
            .insert(vault_id.to_string(), Entry::new(vault, ttl));
        self.stats.insertions += 1;
    }

    fn get_ttl_remaining(&mut self, vault_id: &str) -> Option<Option<u64>> {
        if let Some(entry) = self.ttl_remaining.get_mut(vault_id) {
            if !entry.is_expired() {
                self.stats.hits += 1;
                return Some(entry.access());
            }
            self.ttl_remaining.remove(vault_id);
        }
        self.stats.misses += 1;
        None
    }

    fn set_ttl_remaining(&mut self, vault_id: &str, value: Option<u64>, ttl: Duration) {
        self.ttl_remaining
            .insert(vault_id.to_string(), Entry::new(value, ttl));
        self.stats.insertions += 1;
    }

    fn get_summary(&mut self, vault_id: &str) -> Option<VaultSummary> {
        if let Some(entry) = self.summaries.get_mut(vault_id) {
            if !entry.is_expired() {
                self.stats.hits += 1;
                return Some(entry.access());
            }
            self.summaries.remove(vault_id);
        }
        self.stats.misses += 1;
        None
    }

    fn set_summary(&mut self, vault_id: &str, summary: VaultSummary, ttl: Duration) {
        self.summaries
            .insert(vault_id.to_string(), Entry::new(summary, ttl));
        self.stats.insertions += 1;
    }

    fn invalidate(&mut self, vault_id: &str) {
        self.vaults.remove(vault_id);
        self.ttl_remaining.remove(vault_id);
        self.summaries.remove(vault_id);
    }

    fn invalidate_all(&mut self) {
        self.vaults.clear();
        self.ttl_remaining.clear();
        self.summaries.clear();
    }

    fn live_entry_count(&self) -> usize {
        let vault_count = self.vaults.values().filter(|e| !e.is_expired()).count();
        let ttl_count = self
            .ttl_remaining
            .values()
            .filter(|e| !e.is_expired())
            .count();
        let summary_count = self.summaries.values().filter(|e| !e.is_expired()).count();
        vault_count.max(ttl_count).max(summary_count)
    }
}

// ── Multi-Level Cache ─────────────────────────────────────────────────────────

/// Two-level cache with automatic read-through and write-through coherence.
///
/// Read order: L1 → L2 → miss.
/// Write order: L1 (short TTL) + L2 (long TTL) simultaneously.
/// Invalidation: cascades to both levels.
pub struct MultiLevelCache {
    l1: Mutex<L1Cache>,
    l2: Mutex<L2Cache>,
    l1_ttl: Duration,
    l2_ttl: Duration,
}

impl MultiLevelCache {
    pub fn new() -> Self {
        Self {
            l1: Mutex::new(L1Cache::new(L1_MAX_ENTRIES)),
            l2: Mutex::new(L2Cache::new()),
            l1_ttl: Duration::from_secs(L1_TTL_SECS),
            l2_ttl: Duration::from_secs(L2_TTL_SECS),
        }
    }

    /// Create with custom TTLs (useful for tests).
    pub fn with_ttls(l1_ttl: Duration, l2_ttl: Duration) -> Self {
        Self {
            l1: Mutex::new(L1Cache::new(L1_MAX_ENTRIES)),
            l2: Mutex::new(L2Cache::new()),
            l1_ttl,
            l2_ttl,
        }
    }

    // ── get_vault ─────────────────────────────────────────────────────────────

    /// Retrieve a vault: L1 first, then L2 with promotion, then miss.
    pub fn get_vault(&self, vault_id: &str) -> Option<Vault> {
        // Try L1.
        {
            let mut l1 = self.l1.lock().unwrap();
            if let Some(v) = l1.get_vault(vault_id) {
                return Some(v);
            }
        }

        // Try L2 with promotion to L1.
        let mut l2 = self.l2.lock().unwrap();
        if let Some(v) = l2.get_vault(vault_id) {
            l2.stats.promotions += 1;
            drop(l2);

            // Promote to L1.
            let mut l1 = self.l1.lock().unwrap();
            l1.set_vault(vault_id, v.clone(), self.l1_ttl);
            return Some(v);
        }

        None
    }

    /// Write vault to both L1 and L2 (write-through).
    pub fn set_vault(&self, vault_id: &str, vault: Vault) {
        {
            let mut l1 = self.l1.lock().unwrap();
            l1.set_vault(vault_id, vault.clone(), self.l1_ttl);
        }
        {
            let mut l2 = self.l2.lock().unwrap();
            l2.set_vault(vault_id, vault, self.l2_ttl);
        }
    }

    // ── get_ttl_remaining ─────────────────────────────────────────────────────

    #[allow(clippy::option_option)]
    pub fn get_ttl_remaining(&self, vault_id: &str) -> Option<Option<u64>> {
        {
            let mut l1 = self.l1.lock().unwrap();
            if let Some(v) = l1.get_ttl_remaining(vault_id) {
                return Some(v);
            }
        }

        let mut l2 = self.l2.lock().unwrap();
        if let Some(v) = l2.get_ttl_remaining(vault_id) {
            drop(l2);
            let mut l1 = self.l1.lock().unwrap();
            l1.set_ttl_remaining(vault_id, v, self.l1_ttl);
            return Some(v);
        }

        None
    }

    pub fn set_ttl_remaining(&self, vault_id: &str, value: Option<u64>) {
        {
            let mut l1 = self.l1.lock().unwrap();
            l1.set_ttl_remaining(vault_id, value, self.l1_ttl);
        }
        {
            let mut l2 = self.l2.lock().unwrap();
            l2.set_ttl_remaining(vault_id, value, self.l2_ttl);
        }
    }

    // ── get_summary ───────────────────────────────────────────────────────────

    pub fn get_summary(&self, vault_id: &str) -> Option<VaultSummary> {
        {
            let mut l1 = self.l1.lock().unwrap();
            if let Some(v) = l1.get_summary(vault_id) {
                return Some(v);
            }
        }

        let mut l2 = self.l2.lock().unwrap();
        if let Some(v) = l2.get_summary(vault_id) {
            drop(l2);
            let mut l1 = self.l1.lock().unwrap();
            l1.set_summary(vault_id, v.clone(), self.l1_ttl);
            return Some(v);
        }

        None
    }

    pub fn set_summary(&self, vault_id: &str, summary: VaultSummary) {
        {
            let mut l1 = self.l1.lock().unwrap();
            l1.set_summary(vault_id, summary.clone(), self.l1_ttl);
        }
        {
            let mut l2 = self.l2.lock().unwrap();
            l2.set_summary(vault_id, summary, self.l2_ttl);
        }
    }

    // ── Invalidation ──────────────────────────────────────────────────────────

    /// Invalidate all cached data for a vault in both levels.
    pub fn invalidate(&self, vault_id: &str) {
        self.l1.lock().unwrap().invalidate(vault_id);
        self.l2.lock().unwrap().invalidate(vault_id);
    }

    /// Flush both cache levels entirely.
    pub fn invalidate_all(&self) {
        self.l1.lock().unwrap().invalidate_all();
        self.l2.lock().unwrap().invalidate_all();
    }

    // ── Statistics ────────────────────────────────────────────────────────────

    /// Get per-level cache statistics.
    pub fn get_stats(&self) -> CacheStats {
        let l1_stats = self.l1.lock().unwrap().stats.clone();
        let l2_stats = self.l2.lock().unwrap().stats.clone();
        let l1_entries = self.l1.lock().unwrap().live_entry_count();
        let l2_entries = self.l2.lock().unwrap().live_entry_count();

        CacheStats {
            l1: l1_stats,
            l2: l2_stats,
            l1_live_entries: l1_entries,
            l2_live_entries: l2_entries,
        }
    }
}

impl Default for MultiLevelCache {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub l1: L1Stats,
    pub l2: L2Stats,
    pub l1_live_entries: usize,
    pub l2_live_entries: usize,
}

// ── Unit Tests ────────────────────────────────────────────────────────────────

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

    #[test]
    fn test_set_and_get_vault_l1_hit() {
        let cache = MultiLevelCache::new();
        cache.set_vault("v1", make_vault("v1"));

        let result = cache.get_vault("v1");
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "v1");

        let stats = cache.get_stats();
        assert_eq!(stats.l1.hits, 1);
    }

    #[test]
    fn test_l2_fallback_on_l1_miss() {
        // Use a very short L1 TTL and longer L2 TTL.
        let cache = MultiLevelCache::with_ttls(Duration::from_millis(1), Duration::from_secs(60));
        cache.set_vault("v1", make_vault("v1"));

        // Wait for L1 to expire.
        std::thread::sleep(Duration::from_millis(5));

        // L1 miss → should fall back to L2.
        let result = cache.get_vault("v1");
        assert!(result.is_some());

        let stats = cache.get_stats();
        assert_eq!(stats.l1.misses, 1);
        assert_eq!(stats.l2.hits, 1);
    }

    #[test]
    fn test_l2_promotion_to_l1() {
        let cache = MultiLevelCache::with_ttls(Duration::from_millis(1), Duration::from_secs(60));
        cache.set_vault("v1", make_vault("v1"));
        std::thread::sleep(Duration::from_millis(5));

        // First access: L1 miss → L2 hit → promote to L1.
        cache.get_vault("v1");

        let stats = cache.get_stats();
        assert_eq!(stats.l2.promotions, 1);
    }

    #[test]
    fn test_invalidate_clears_both_levels() {
        let cache = MultiLevelCache::new();
        cache.set_vault("v1", make_vault("v1"));

        cache.invalidate("v1");

        assert!(cache.get_vault("v1").is_none());

        let stats = cache.get_stats();
        assert_eq!(stats.l1.misses, 1);
        assert_eq!(stats.l2.misses, 1);
    }

    #[test]
    fn test_invalidate_all_clears_both_levels() {
        let cache = MultiLevelCache::new();
        cache.set_vault("v1", make_vault("v1"));
        cache.set_vault("v2", make_vault("v2"));

        cache.invalidate_all();

        assert!(cache.get_vault("v1").is_none());
        assert!(cache.get_vault("v2").is_none());
    }

    #[test]
    fn test_cache_miss_returns_none() {
        let cache = MultiLevelCache::new();
        assert!(cache.get_vault("nonexistent").is_none());
    }

    #[test]
    fn test_set_and_get_ttl_remaining() {
        let cache = MultiLevelCache::new();
        cache.set_ttl_remaining("v1", Some(3600));

        let result = cache.get_ttl_remaining("v1");
        assert_eq!(result, Some(Some(3600)));
    }

    #[test]
    fn test_set_and_get_summary() {
        let cache = MultiLevelCache::new();
        cache.set_summary("v1", make_summary("v1"));

        let result = cache.get_summary("v1");
        assert!(result.is_some());
        assert_eq!(result.unwrap().vault_id, "v1");
    }

    #[test]
    fn test_stats_track_hits_and_misses() {
        let cache = MultiLevelCache::new();
        cache.set_vault("v1", make_vault("v1"));

        cache.get_vault("v1"); // Hit in L1.
        cache.get_vault("missing"); // Miss in both.

        let stats = cache.get_stats();
        assert_eq!(stats.l1.hits, 1);
        assert_eq!(stats.l1.misses, 1);
    }

    #[test]
    fn test_stats_track_live_entries() {
        let cache = MultiLevelCache::new();
        cache.set_vault("v1", make_vault("v1"));
        cache.set_vault("v2", make_vault("v2"));

        let stats = cache.get_stats();
        assert_eq!(stats.l1_live_entries, 2);
        assert_eq!(stats.l2_live_entries, 2);
    }
}

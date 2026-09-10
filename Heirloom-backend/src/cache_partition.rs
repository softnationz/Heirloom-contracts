/// Cache partitioning for multi-tenant isolation (#92).
///
/// Each tenant operates within its own logical partition of the cache,
/// ensuring that keys from one tenant cannot collide with or leak into
/// another tenant's data.  Partition statistics are tracked per-tenant
/// so operators can observe per-tenant cache activity.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ── Key namespacing ───────────────────────────────────────────────────────────

/// Separator used between tenant ID and cache key.
const KEY_SEP: char = ':';

/// Return a namespaced cache key for the given tenant and logical key.
///
/// ```
/// # use heirloom_protocol_backend::cache_partition::namespaced_key;
/// let k = namespaced_key("tenant_abc", "vault:42");
/// assert_eq!(k, "tenant_abc:vault:42");
/// ```
pub fn namespaced_key(tenant_id: &str, key: &str) -> String {
    format!("{tenant_id}{KEY_SEP}{key}")
}

/// Extract the tenant ID and original key from a namespaced key.
/// Returns `None` if the key does not contain the separator.
pub fn parse_namespaced_key(namespaced: &str) -> Option<(&str, &str)> {
    namespaced
        .find(KEY_SEP)
        .map(|pos| (&namespaced[..pos], &namespaced[pos + 1..]))
}

// ── Partition entry ───────────────────────────────────────────────────────────

struct PartitionEntry {
    value: String,
    inserted_at: Instant,
    ttl: Duration,
}

impl PartitionEntry {
    fn new(value: String, ttl: Duration) -> Self {
        Self {
            value,
            inserted_at: Instant::now(),
            ttl,
        }
    }

    fn is_expired(&self) -> bool {
        self.inserted_at.elapsed() >= self.ttl
    }
}

// ── Per-tenant partition statistics ──────────────────────────────────────────

/// Statistics for a single tenant partition.
#[derive(Debug, Clone, Default)]
pub struct PartitionStats {
    /// Number of keys currently alive (non-expired) in this partition.
    pub live_keys: usize,
    /// Cumulative cache hits for this partition since creation.
    pub hits: u64,
    /// Cumulative cache misses for this partition since creation.
    pub misses: u64,
    /// Cumulative evictions (expired entries removed on read) for this partition.
    pub evictions: u64,
}

impl PartitionStats {
    /// Cache-hit ratio in the range `[0.0, 1.0]`.  Returns `None` when no
    /// accesses have been recorded yet.
    pub fn hit_ratio(&self) -> Option<f64> {
        let total = self.hits + self.misses;
        if total == 0 {
            None
        } else {
            Some(self.hits as f64 / total as f64)
        }
    }
}

// ── Tenant partition (internal) ───────────────────────────────────────────────

struct TenantPartition {
    data: HashMap<String, PartitionEntry>,
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl TenantPartition {
    fn new() -> Self {
        Self {
            data: HashMap::new(),
            hits: 0,
            misses: 0,
            evictions: 0,
        }
    }

    /// Insert a value under the given (non-namespaced) key.
    fn insert(&mut self, key: &str, value: String, ttl: Duration) {
        self.data
            .insert(key.to_string(), PartitionEntry::new(value, ttl));
    }

    /// Get a value. Returns `None` on miss or expiry (and bumps the
    /// eviction counter if the entry was expired).
    fn get(&mut self, key: &str) -> Option<String> {
        match self.data.get(key) {
            Some(entry) if !entry.is_expired() => {
                self.hits += 1;
                Some(entry.value.clone())
            }
            Some(_expired) => {
                self.evictions += 1;
                self.misses += 1;
                self.data.remove(key);
                None
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }

    /// Remove a single key from the partition.
    fn remove(&mut self, key: &str) {
        self.data.remove(key);
    }

    /// Remove all entries from the partition.
    fn clear(&mut self) {
        self.data.clear();
    }

    /// Count live (non-expired) keys.
    fn live_key_count(&self) -> usize {
        self.data.values().filter(|e| !e.is_expired()).count()
    }

    fn stats(&self) -> PartitionStats {
        PartitionStats {
            live_keys: self.live_key_count(),
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
        }
    }
}

// ── Public PartitionedCache ───────────────────────────────────────────────────

/// Thread-safe, tenant-partitioned cache.
///
/// All entries are stored under tenant-namespaced keys (`tenant:key`),
/// providing strict isolation between tenants.  Each tenant's partition
/// is created lazily on first access.
///
/// # Example
/// ```
/// # use std::time::Duration;
/// # use heirloom_protocol_backend::cache_partition::PartitionedCache;
/// let cache = PartitionedCache::new(Duration::from_secs(300));
///
/// cache.set("tenant_a", "vault:1", "data".to_string());
/// assert_eq!(cache.get("tenant_a", "vault:1"), Some("data".to_string()));
///
/// // tenant_b cannot see tenant_a's data
/// assert!(cache.get("tenant_b", "vault:1").is_none());
/// ```
pub struct PartitionedCache {
    partitions: Arc<Mutex<HashMap<String, TenantPartition>>>,
    default_ttl: Duration,
}

impl PartitionedCache {
    /// Create a new cache with the given default TTL.
    pub fn new(default_ttl: Duration) -> Self {
        Self {
            partitions: Arc::new(Mutex::new(HashMap::new())),
            default_ttl,
        }
    }

    /// Create with the default 5-minute TTL.
    pub fn with_default_ttl() -> Self {
        Self::new(Duration::from_secs(300))
    }

    // ── Core get / set / remove ───────────────────────────────────────────────

    /// Insert a value for the given tenant and key using the default TTL.
    pub fn set(&self, tenant_id: &str, key: &str, value: String) {
        self.set_with_ttl(tenant_id, key, value, self.default_ttl);
    }

    /// Insert a value with a custom TTL.
    pub fn set_with_ttl(&self, tenant_id: &str, key: &str, value: String, ttl: Duration) {
        let mut partitions = self.partitions.lock().unwrap();
        partitions
            .entry(tenant_id.to_string())
            .or_insert_with(TenantPartition::new)
            .insert(key, value, ttl);
    }

    /// Retrieve a value for the given tenant and key.
    ///
    /// Returns `None` on cache miss or if the entry has expired.
    pub fn get(&self, tenant_id: &str, key: &str) -> Option<String> {
        let mut partitions = self.partitions.lock().unwrap();
        partitions
            .get_mut(tenant_id)
            .and_then(|p| p.get(key))
    }

    /// Remove a single key from the given tenant's partition.
    pub fn remove(&self, tenant_id: &str, key: &str) {
        let mut partitions = self.partitions.lock().unwrap();
        if let Some(p) = partitions.get_mut(tenant_id) {
            p.remove(key);
        }
    }

    /// Remove all entries for the given tenant.
    pub fn clear_tenant(&self, tenant_id: &str) {
        let mut partitions = self.partitions.lock().unwrap();
        if let Some(p) = partitions.get_mut(tenant_id) {
            p.clear();
        }
    }

    /// Remove all entries across all tenants.
    pub fn clear_all(&self) {
        let mut partitions = self.partitions.lock().unwrap();
        for p in partitions.values_mut() {
            p.clear();
        }
    }

    // ── Isolation enforcement ─────────────────────────────────────────────────

    /// Verify that `tenant_id` does not contain the key-separator character,
    /// which could be used to craft a key that crosses tenant boundaries.
    ///
    /// Returns `Err` if the tenant ID is invalid.
    pub fn validate_tenant_id(tenant_id: &str) -> Result<(), String> {
        if tenant_id.is_empty() {
            return Err("tenant_id must not be empty".into());
        }
        if tenant_id.contains(KEY_SEP) {
            return Err(format!(
                "tenant_id must not contain the separator character '{KEY_SEP}'"
            ));
        }
        Ok(())
    }

    /// Like `set`, but returns `Err` if the tenant ID is invalid.
    pub fn set_safe(
        &self,
        tenant_id: &str,
        key: &str,
        value: String,
    ) -> Result<(), String> {
        Self::validate_tenant_id(tenant_id)?;
        self.set(tenant_id, key, value);
        Ok(())
    }

    /// Like `get`, but returns `Err` if the tenant ID is invalid.
    pub fn get_safe(
        &self,
        tenant_id: &str,
        key: &str,
    ) -> Result<Option<String>, String> {
        Self::validate_tenant_id(tenant_id)?;
        Ok(self.get(tenant_id, key))
    }

    // ── Partition statistics ──────────────────────────────────────────────────

    /// Return statistics for a single tenant's partition.
    ///
    /// Returns `None` if the tenant has never accessed the cache.
    pub fn partition_stats(&self, tenant_id: &str) -> Option<PartitionStats> {
        let partitions = self.partitions.lock().unwrap();
        partitions.get(tenant_id).map(|p| p.stats())
    }

    /// Return statistics for all known tenant partitions.
    pub fn all_partition_stats(&self) -> HashMap<String, PartitionStats> {
        let partitions = self.partitions.lock().unwrap();
        partitions
            .iter()
            .map(|(id, p)| (id.clone(), p.stats()))
            .collect()
    }

    /// Return the number of known tenant partitions (including empty ones).
    pub fn tenant_count(&self) -> usize {
        self.partitions.lock().unwrap().len()
    }
}

impl Default for PartitionedCache {
    fn default() -> Self {
        Self::with_default_ttl()
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // ── namespaced_key ────────────────────────────────────────────────────────

    #[test]
    fn test_namespaced_key_format() {
        assert_eq!(namespaced_key("tenant_a", "vault:1"), "tenant_a:vault:1");
    }

    #[test]
    fn test_parse_namespaced_key_valid() {
        let (tenant, key) = parse_namespaced_key("tenant_a:vault:1").unwrap();
        assert_eq!(tenant, "tenant_a");
        assert_eq!(key, "vault:1");
    }

    #[test]
    fn test_parse_namespaced_key_no_separator() {
        assert!(parse_namespaced_key("no_separator").is_none());
    }

    // ── set / get ─────────────────────────────────────────────────────────────

    #[test]
    fn test_set_and_get_same_tenant() {
        let cache = PartitionedCache::new(Duration::from_secs(60));
        cache.set("tenant_a", "k1", "value1".to_string());
        assert_eq!(cache.get("tenant_a", "k1"), Some("value1".to_string()));
    }

    #[test]
    fn test_get_miss_on_empty_cache() {
        let cache = PartitionedCache::new(Duration::from_secs(60));
        assert!(cache.get("tenant_a", "k1").is_none());
    }

    // ── Tenant isolation ──────────────────────────────────────────────────────

    #[test]
    fn test_tenant_isolation_different_tenants_same_key() {
        let cache = PartitionedCache::new(Duration::from_secs(60));
        cache.set("tenant_a", "vault:1", "data_a".to_string());
        cache.set("tenant_b", "vault:1", "data_b".to_string());

        assert_eq!(
            cache.get("tenant_a", "vault:1"),
            Some("data_a".to_string())
        );
        assert_eq!(
            cache.get("tenant_b", "vault:1"),
            Some("data_b".to_string())
        );
    }

    #[test]
    fn test_tenant_a_cannot_read_tenant_b_data() {
        let cache = PartitionedCache::new(Duration::from_secs(60));
        cache.set("tenant_b", "secret", "sensitive".to_string());
        assert!(cache.get("tenant_a", "secret").is_none());
    }

    #[test]
    fn test_clear_tenant_only_removes_that_tenant() {
        let cache = PartitionedCache::new(Duration::from_secs(60));
        cache.set("tenant_a", "k1", "a".to_string());
        cache.set("tenant_b", "k1", "b".to_string());

        cache.clear_tenant("tenant_a");

        assert!(cache.get("tenant_a", "k1").is_none());
        assert_eq!(cache.get("tenant_b", "k1"), Some("b".to_string()));
    }

    #[test]
    fn test_clear_all_removes_all_tenants() {
        let cache = PartitionedCache::new(Duration::from_secs(60));
        cache.set("tenant_a", "k1", "a".to_string());
        cache.set("tenant_b", "k1", "b".to_string());

        cache.clear_all();

        assert!(cache.get("tenant_a", "k1").is_none());
        assert!(cache.get("tenant_b", "k1").is_none());
    }

    // ── TTL expiry ────────────────────────────────────────────────────────────

    #[test]
    fn test_entry_expires_after_ttl() {
        let cache = PartitionedCache::new(Duration::from_millis(1));
        cache.set("tenant_a", "k1", "value".to_string());
        std::thread::sleep(Duration::from_millis(5));
        assert!(cache.get("tenant_a", "k1").is_none());
    }

    #[test]
    fn test_set_with_custom_ttl() {
        let cache = PartitionedCache::new(Duration::from_secs(60));
        // Override with a very short TTL.
        cache.set_with_ttl(
            "tenant_a",
            "k1",
            "value".to_string(),
            Duration::from_millis(1),
        );
        std::thread::sleep(Duration::from_millis(5));
        assert!(cache.get("tenant_a", "k1").is_none());
    }

    // ── remove ────────────────────────────────────────────────────────────────

    #[test]
    fn test_remove_specific_key() {
        let cache = PartitionedCache::new(Duration::from_secs(60));
        cache.set("tenant_a", "k1", "v1".to_string());
        cache.set("tenant_a", "k2", "v2".to_string());

        cache.remove("tenant_a", "k1");

        assert!(cache.get("tenant_a", "k1").is_none());
        assert_eq!(cache.get("tenant_a", "k2"), Some("v2".to_string()));
    }

    // ── validate_tenant_id ────────────────────────────────────────────────────

    #[test]
    fn test_validate_empty_tenant_id_rejected() {
        assert!(PartitionedCache::validate_tenant_id("").is_err());
    }

    #[test]
    fn test_validate_tenant_id_with_separator_rejected() {
        assert!(PartitionedCache::validate_tenant_id("bad:id").is_err());
    }

    #[test]
    fn test_validate_valid_tenant_id_accepted() {
        assert!(PartitionedCache::validate_tenant_id("tenant_abc_123").is_ok());
    }

    #[test]
    fn test_set_safe_rejects_invalid_tenant_id() {
        let cache = PartitionedCache::new(Duration::from_secs(60));
        let result = cache.set_safe("bad:tenant", "k", "v".to_string());
        assert!(result.is_err());
    }

    #[test]
    fn test_get_safe_rejects_invalid_tenant_id() {
        let cache = PartitionedCache::new(Duration::from_secs(60));
        let result = cache.get_safe("bad:tenant", "k");
        assert!(result.is_err());
    }

    // ── Partition statistics ──────────────────────────────────────────────────

    #[test]
    fn test_partition_stats_none_for_unknown_tenant() {
        let cache = PartitionedCache::new(Duration::from_secs(60));
        assert!(cache.partition_stats("unknown").is_none());
    }

    #[test]
    fn test_partition_stats_tracks_hits_and_misses() {
        let cache = PartitionedCache::new(Duration::from_secs(60));
        cache.set("tenant_a", "k1", "v".to_string());

        // One hit
        let _ = cache.get("tenant_a", "k1");
        // One miss
        let _ = cache.get("tenant_a", "missing");

        let stats = cache.partition_stats("tenant_a").unwrap();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
    }

    #[test]
    fn test_partition_stats_hit_ratio() {
        let cache = PartitionedCache::new(Duration::from_secs(60));
        cache.set("tenant_a", "k1", "v".to_string());

        // 2 hits, 0 misses
        let _ = cache.get("tenant_a", "k1");
        let _ = cache.get("tenant_a", "k1");

        let stats = cache.partition_stats("tenant_a").unwrap();
        assert_eq!(stats.hit_ratio(), Some(1.0));
    }

    #[test]
    fn test_partition_stats_evictions_on_expiry() {
        let cache = PartitionedCache::new(Duration::from_millis(1));
        cache.set("tenant_a", "k1", "v".to_string());
        std::thread::sleep(Duration::from_millis(5));
        // Accessing an expired entry should increment evictions.
        let _ = cache.get("tenant_a", "k1");

        let stats = cache.partition_stats("tenant_a").unwrap();
        assert_eq!(stats.evictions, 1);
    }

    #[test]
    fn test_partition_stats_live_keys() {
        let cache = PartitionedCache::new(Duration::from_secs(60));
        cache.set("tenant_a", "k1", "v1".to_string());
        cache.set("tenant_a", "k2", "v2".to_string());

        let stats = cache.partition_stats("tenant_a").unwrap();
        assert_eq!(stats.live_keys, 2);
    }

    #[test]
    fn test_all_partition_stats_returns_all_tenants() {
        let cache = PartitionedCache::new(Duration::from_secs(60));
        cache.set("tenant_a", "k1", "v".to_string());
        cache.set("tenant_b", "k1", "v".to_string());

        let all = cache.all_partition_stats();
        assert!(all.contains_key("tenant_a"));
        assert!(all.contains_key("tenant_b"));
    }

    #[test]
    fn test_tenant_count() {
        let cache = PartitionedCache::new(Duration::from_secs(60));
        assert_eq!(cache.tenant_count(), 0);
        cache.set("tenant_a", "k", "v".to_string());
        cache.set("tenant_b", "k", "v".to_string());
        assert_eq!(cache.tenant_count(), 2);
    }

    #[test]
    fn test_stats_no_hit_ratio_when_no_accesses() {
        let cache = PartitionedCache::new(Duration::from_secs(60));
        cache.set("tenant_a", "k1", "v".to_string());
        // No gets yet
        let stats = cache.partition_stats("tenant_a").unwrap();
        assert_eq!(stats.hit_ratio(), None);
    }
}

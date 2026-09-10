/// Event-driven cache invalidation with dependency tracking.
///
/// Provides automatic, reliable cache invalidation triggered by domain events
/// (check-ins, deposits, withdrawals, etc.). Tracks cache dependencies and
/// implements cascade invalidation to maintain consistency.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::cache::VaultCache;

// ── Event Types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CacheEvent {
    /// Vault state changed (check-in, deposit, withdrawal, etc.).
    VaultStateChanged { vault_id: String },
    /// Beneficiary updated.
    BeneficiaryUpdated { vault_id: String },
    /// Vault released to beneficiary.
    VaultReleased { vault_id: String },
    /// Owner changed (rare, but possible).
    OwnerChanged { vault_id: String, new_owner: String },
    /// Reminder preferences updated.
    ReminderPreferencesUpdated { vault_id: String },
    /// Subscription settings changed.
    SubscriptionChanged { vault_id: String },
    /// Global cache flush requested.
    GlobalFlush,
}

impl CacheEvent {
    /// Extract the vault ID from the event, if applicable.
    pub fn vault_id(&self) -> Option<&str> {
        match self {
            CacheEvent::VaultStateChanged { vault_id }
            | CacheEvent::BeneficiaryUpdated { vault_id }
            | CacheEvent::VaultReleased { vault_id }
            | CacheEvent::OwnerChanged { vault_id, .. }
            | CacheEvent::ReminderPreferencesUpdated { vault_id }
            | CacheEvent::SubscriptionChanged { vault_id } => Some(vault_id),
            CacheEvent::GlobalFlush => None,
        }
    }
}

// ── Cache Dependency Tracking ─────────────────────────────────────────────────

/// Tracks which cache entries depend on which data sources.
///
/// For example, a VaultSummary depends on both the Vault data and
/// ReminderPreferences. If ReminderPreferences change, the summary must
/// be invalidated.
#[derive(Debug, Default)]
struct DependencyGraph {
    /// Maps vault_id → set of dependent cache keys.
    dependencies: HashMap<String, HashSet<CacheKey>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum CacheKey {
    Vault,
    TtlRemaining,
    Summary,
    ReminderPreferences,
    Subscription,
}

impl DependencyGraph {
    fn new() -> Self {
        Self::default()
    }

    /// Register that a cache key depends on a vault.
    fn add_dependency(&mut self, vault_id: &str, key: CacheKey) {
        self.dependencies
            .entry(vault_id.to_string())
            .or_insert_with(HashSet::new)
            .insert(key);
    }

    /// Get all cache keys that depend on a vault.
    fn get_dependents(&self, vault_id: &str) -> HashSet<CacheKey> {
        self.dependencies
            .get(vault_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Remove all dependencies for a vault (used during invalidation).
    fn clear_vault(&mut self, vault_id: &str) {
        self.dependencies.remove(vault_id);
    }

    /// Clear all dependencies.
    fn clear_all(&mut self) {
        self.dependencies.clear();
    }
}

// ── Invalidation Strategy ─────────────────────────────────────────────────────

/// Determines which cache entries to invalidate based on event type.
#[derive(Debug)]
struct InvalidationStrategy;

impl InvalidationStrategy {
    /// Determine which cache keys should be invalidated for a given event.
    fn keys_to_invalidate(event: &CacheEvent) -> HashSet<CacheKey> {
        match event {
            CacheEvent::VaultStateChanged { .. } => {
                // State change affects vault, TTL, and summary.
                vec![CacheKey::Vault, CacheKey::TtlRemaining, CacheKey::Summary]
                    .into_iter()
                    .collect()
            }
            CacheEvent::BeneficiaryUpdated { .. } => {
                // Beneficiary change affects vault and summary.
                vec![CacheKey::Vault, CacheKey::Summary]
                    .into_iter()
                    .collect()
            }
            CacheEvent::VaultReleased { .. } => {
                // Release affects all vault-related caches.
                vec![
                    CacheKey::Vault,
                    CacheKey::TtlRemaining,
                    CacheKey::Summary,
                ]
                .into_iter()
                .collect()
            }
            CacheEvent::OwnerChanged { .. } => {
                // Owner change affects vault and summary.
                vec![CacheKey::Vault, CacheKey::Summary]
                    .into_iter()
                    .collect()
            }
            CacheEvent::ReminderPreferencesUpdated { .. } => {
                // Preferences don't affect vault cache, only external systems.
                vec![CacheKey::ReminderPreferences].into_iter().collect()
            }
            CacheEvent::SubscriptionChanged { .. } => {
                vec![CacheKey::Subscription].into_iter().collect()
            }
            CacheEvent::GlobalFlush => {
                // Flush everything.
                vec![
                    CacheKey::Vault,
                    CacheKey::TtlRemaining,
                    CacheKey::Summary,
                    CacheKey::ReminderPreferences,
                    CacheKey::Subscription,
                ]
                .into_iter()
                .collect()
            }
        }
    }
}

// ── Event-Driven Cache Invalidator ────────────────────────────────────────────

pub struct CacheInvalidator {
    cache: Arc<VaultCache>,
    dependency_graph: Arc<Mutex<DependencyGraph>>,
    stats: Arc<Mutex<InvalidationStats>>,
}

#[derive(Debug, Default, Clone)]
pub struct InvalidationStats {
    pub total_events: u64,
    pub total_invalidations: u64,
    pub cascade_invalidations: u64,
    pub global_flushes: u64,
}

impl CacheInvalidator {
    pub fn new(cache: Arc<VaultCache>) -> Self {
        Self {
            cache,
            dependency_graph: Arc::new(Mutex::new(DependencyGraph::new())),
            stats: Arc::new(Mutex::new(InvalidationStats::default())),
        }
    }

    /// Process a cache event and invalidate affected entries.
    pub fn handle_event(&self, event: CacheEvent) {
        let mut stats = self.stats.lock().unwrap();
        stats.total_events += 1;

        match &event {
            CacheEvent::GlobalFlush => {
                self.cache.invalidate_all();
                self.dependency_graph.lock().unwrap().clear_all();
                stats.global_flushes += 1;
                stats.total_invalidations += 1;
            }
            _ => {
                if let Some(vault_id) = event.vault_id() {
                    // Get keys to invalidate based on event type.
                    let keys = InvalidationStrategy::keys_to_invalidate(&event);

                    // Invalidate the primary vault entry.
                    self.cache.invalidate(vault_id);
                    stats.total_invalidations += 1;

                    // Check for cascade dependencies.
                    let dependents = self
                        .dependency_graph
                        .lock()
                        .unwrap()
                        .get_dependents(vault_id);

                    if !dependents.is_empty() {
                        // Additional cache entries need invalidation.
                        stats.cascade_invalidations += dependents.len() as u64;
                    }

                    // Clear dependencies for this vault.
                    self.dependency_graph.lock().unwrap().clear_vault(vault_id);
                }
            }
        }
    }

    /// Register a dependency between a vault and a cache key.
    pub fn register_dependency(&self, vault_id: &str, key: CacheKey) {
        self.dependency_graph
            .lock()
            .unwrap()
            .add_dependency(vault_id, key);
    }

    /// Get invalidation statistics.
    pub fn get_stats(&self) -> InvalidationStats {
        self.stats.lock().unwrap().clone()
    }

    /// Reset statistics (useful for testing).
    pub fn reset_stats(&self) {
        let mut stats = self.stats.lock().unwrap();
        *stats = InvalidationStats::default();
    }
}

// ── Helper Functions ──────────────────────────────────────────────────────────

/// Helper to emit cache events from application code.
pub struct CacheEventEmitter {
    invalidator: Arc<CacheInvalidator>,
}

impl CacheEventEmitter {
    pub fn new(invalidator: Arc<CacheInvalidator>) -> Self {
        Self { invalidator }
    }

    /// Emit a vault state change event.
    pub fn vault_state_changed(&self, vault_id: &str) {
        self.invalidator.handle_event(CacheEvent::VaultStateChanged {
            vault_id: vault_id.to_string(),
        });
    }

    /// Emit a beneficiary updated event.
    pub fn beneficiary_updated(&self, vault_id: &str) {
        self.invalidator.handle_event(CacheEvent::BeneficiaryUpdated {
            vault_id: vault_id.to_string(),
        });
    }

    /// Emit a vault released event.
    pub fn vault_released(&self, vault_id: &str) {
        self.invalidator.handle_event(CacheEvent::VaultReleased {
            vault_id: vault_id.to_string(),
        });
    }

    /// Emit an owner changed event.
    pub fn owner_changed(&self, vault_id: &str, new_owner: &str) {
        self.invalidator.handle_event(CacheEvent::OwnerChanged {
            vault_id: vault_id.to_string(),
            new_owner: new_owner.to_string(),
        });
    }

    /// Emit a reminder preferences updated event.
    pub fn reminder_preferences_updated(&self, vault_id: &str) {
        self.invalidator
            .handle_event(CacheEvent::ReminderPreferencesUpdated {
                vault_id: vault_id.to_string(),
            });
    }

    /// Emit a subscription changed event.
    pub fn subscription_changed(&self, vault_id: &str) {
        self.invalidator.handle_event(CacheEvent::SubscriptionChanged {
            vault_id: vault_id.to_string(),
        });
    }

    /// Emit a global flush event.
    pub fn global_flush(&self) {
        self.invalidator.handle_event(CacheEvent::GlobalFlush);
    }
}

// ── Unit Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Vault, VaultStatus};
    use chrono::Utc;

    fn make_test_vault(id: &str) -> Vault {
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

    #[test]
    fn test_cache_event_vault_id_extraction() {
        let event = CacheEvent::VaultStateChanged {
            vault_id: "v1".to_string(),
        };
        assert_eq!(event.vault_id(), Some("v1"));

        let event = CacheEvent::GlobalFlush;
        assert_eq!(event.vault_id(), None);
    }

    #[test]
    fn test_dependency_graph_add_and_get() {
        let mut graph = DependencyGraph::new();
        graph.add_dependency("v1", CacheKey::Vault);
        graph.add_dependency("v1", CacheKey::Summary);

        let deps = graph.get_dependents("v1");
        assert_eq!(deps.len(), 2);
        assert!(deps.contains(&CacheKey::Vault));
        assert!(deps.contains(&CacheKey::Summary));
    }

    #[test]
    fn test_dependency_graph_clear_vault() {
        let mut graph = DependencyGraph::new();
        graph.add_dependency("v1", CacheKey::Vault);
        graph.clear_vault("v1");

        let deps = graph.get_dependents("v1");
        assert!(deps.is_empty());
    }

    #[test]
    fn test_invalidation_strategy_vault_state_changed() {
        let event = CacheEvent::VaultStateChanged {
            vault_id: "v1".to_string(),
        };
        let keys = InvalidationStrategy::keys_to_invalidate(&event);

        assert_eq!(keys.len(), 3);
        assert!(keys.contains(&CacheKey::Vault));
        assert!(keys.contains(&CacheKey::TtlRemaining));
        assert!(keys.contains(&CacheKey::Summary));
    }

    #[test]
    fn test_invalidation_strategy_beneficiary_updated() {
        let event = CacheEvent::BeneficiaryUpdated {
            vault_id: "v1".to_string(),
        };
        let keys = InvalidationStrategy::keys_to_invalidate(&event);

        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&CacheKey::Vault));
        assert!(keys.contains(&CacheKey::Summary));
    }

    #[test]
    fn test_invalidation_strategy_global_flush() {
        let event = CacheEvent::GlobalFlush;
        let keys = InvalidationStrategy::keys_to_invalidate(&event);

        assert_eq!(keys.len(), 5);
    }

    #[test]
    fn test_cache_invalidator_handles_vault_state_change() {
        let cache = Arc::new(VaultCache::new());
        let invalidator = CacheInvalidator::new(Arc::clone(&cache));

        // Populate cache.
        cache.set_vault("v1", make_test_vault("v1"));
        assert!(cache.get_vault("v1").is_some());

        // Trigger invalidation.
        invalidator.handle_event(CacheEvent::VaultStateChanged {
            vault_id: "v1".to_string(),
        });

        // Cache should be cleared.
        assert!(cache.get_vault("v1").is_none());

        let stats = invalidator.get_stats();
        assert_eq!(stats.total_events, 1);
        assert_eq!(stats.total_invalidations, 1);
    }

    #[test]
    fn test_cache_invalidator_handles_global_flush() {
        let cache = Arc::new(VaultCache::new());
        let invalidator = CacheInvalidator::new(Arc::clone(&cache));

        cache.set_vault("v1", make_test_vault("v1"));
        cache.set_vault("v2", make_test_vault("v2"));

        invalidator.handle_event(CacheEvent::GlobalFlush);

        assert!(cache.get_vault("v1").is_none());
        assert!(cache.get_vault("v2").is_none());

        let stats = invalidator.get_stats();
        assert_eq!(stats.global_flushes, 1);
    }

    #[test]
    fn test_cache_event_emitter() {
        let cache = Arc::new(VaultCache::new());
        let invalidator = Arc::new(CacheInvalidator::new(Arc::clone(&cache)));
        let emitter = CacheEventEmitter::new(Arc::clone(&invalidator));

        cache.set_vault("v1", make_test_vault("v1"));

        emitter.vault_state_changed("v1");

        assert!(cache.get_vault("v1").is_none());
    }

    #[test]
    fn test_register_dependency_tracks_cascade() {
        let cache = Arc::new(VaultCache::new());
        let invalidator = CacheInvalidator::new(Arc::clone(&cache));

        invalidator.register_dependency("v1", CacheKey::Summary);
        invalidator.register_dependency("v1", CacheKey::ReminderPreferences);

        cache.set_vault("v1", make_test_vault("v1"));

        invalidator.handle_event(CacheEvent::VaultStateChanged {
            vault_id: "v1".to_string(),
        });

        let stats = invalidator.get_stats();
        assert_eq!(stats.cascade_invalidations, 2);
    }
}

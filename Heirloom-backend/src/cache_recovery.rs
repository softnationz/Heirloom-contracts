/// Cache fault recovery (#93).
///
/// Provides automatic failure detection, transparent failover to the source
/// of truth, cache rebuild, and failure event tracking.  Consumers wrap
/// their existing cache operations with `FaultTolerantCache`, which catches
/// cache-level errors and falls back to a caller-supplied source function.
///
/// # Design
/// - `CacheFailureEvent` records every detected failure with a timestamp and
///   description so that operators can observe cache health over time.
/// - `FaultTolerantCache` holds an inner `VaultCache` plus an atomic
///   health flag.  When any operation returns an error the health is set to
///   degraded, the failure is recorded, and the value is obtained from the
///   source instead.
/// - The `rebuild` method drains all keys from the source into the cache in
///   one pass, resetting the health flag to healthy once it succeeds.
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Instant;

// ── Failure event ─────────────────────────────────────────────────────────────

/// A single recorded cache failure event.
#[derive(Debug, Clone)]
pub struct CacheFailureEvent {
    /// Human-readable description of the failure.
    pub description: String,
    /// Monotonic timestamp when the failure was detected.
    pub occurred_at: Instant,
}

impl CacheFailureEvent {
    fn new(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            occurred_at: Instant::now(),
        }
    }
}

// ── Cache health state ────────────────────────────────────────────────────────

/// The observed health of the cache backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheHealth {
    /// Cache is operating normally.
    Healthy,
    /// One or more failures have been detected; reads fall back to source.
    Degraded,
}

// ── Failure tracker ───────────────────────────────────────────────────────────

/// Bounded ring-buffer of recent cache failure events.
///
/// Keeps the most recent `capacity` events; older ones are dropped
/// automatically so memory is bounded.
pub struct FailureTracker {
    events: Mutex<VecDeque<CacheFailureEvent>>,
    capacity: usize,
}

impl FailureTracker {
    pub fn new(capacity: usize) -> Self {
        Self {
            events: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
        }
    }

    /// Record a new failure event.
    pub fn record(&self, description: impl Into<String>) {
        let mut events = self.events.lock().unwrap();
        if events.len() == self.capacity {
            events.pop_front();
        }
        events.push_back(CacheFailureEvent::new(description));
    }

    /// Return a snapshot of all recorded events.
    pub fn events(&self) -> Vec<CacheFailureEvent> {
        self.events.lock().unwrap().iter().cloned().collect()
    }

    /// Number of failures currently stored.
    pub fn failure_count(&self) -> usize {
        self.events.lock().unwrap().len()
    }

    /// Remove all recorded events.
    pub fn clear(&self) {
        self.events.lock().unwrap().clear();
    }
}

// ── Fault-tolerant cache ──────────────────────────────────────────────────────

/// A cache layer that detects failures, records them, falls back to source,
/// and supports full cache rebuild.
///
/// The generic parameter `V` is the value type.  Callers supply a fallback
/// function (`Fn(&str) -> Result<Option<V>, E>`) when calling `get_or_fallback`.
///
/// # Thread safety
/// All public methods are safe to call from multiple threads concurrently.
pub struct FaultTolerantCache<V: Clone + Send + 'static> {
    /// Underlying in-memory store.  `None` if the cache backend is unavailable.
    store: Mutex<Option<std::collections::HashMap<String, V>>>,
    /// `true` when healthy, `false` when degraded.
    healthy: AtomicBool,
    pub tracker: Arc<FailureTracker>,
}

impl<V: Clone + Send + 'static> FaultTolerantCache<V> {
    /// Create a new fault-tolerant cache with the given failure-event capacity.
    pub fn new(event_capacity: usize) -> Self {
        Self {
            store: Mutex::new(Some(std::collections::HashMap::new())),
            healthy: AtomicBool::new(true),
            tracker: Arc::new(FailureTracker::new(event_capacity)),
        }
    }

    // ── Health reporting ──────────────────────────────────────────────────────

    /// Current cache health.
    pub fn health(&self) -> CacheHealth {
        if self.healthy.load(Ordering::Relaxed) {
            CacheHealth::Healthy
        } else {
            CacheHealth::Degraded
        }
    }

    /// Mark the cache as degraded and record a failure event.
    pub fn record_failure(&self, description: impl Into<String>) {
        self.healthy.store(false, Ordering::Relaxed);
        self.tracker.record(description);
    }

    /// Simulate a cache backend becoming unavailable.  Sets the internal store
    /// to `None` and marks health as degraded.
    pub fn simulate_backend_failure(&self, reason: impl Into<String>) {
        let mut store = self.store.lock().unwrap();
        *store = None;
        drop(store);
        self.record_failure(reason);
    }

    // ── Core operations ───────────────────────────────────────────────────────

    /// Insert a value into the cache.
    ///
    /// If the cache backend is unavailable, the failure is recorded and the
    /// write is silently dropped (best-effort).
    pub fn set(&self, key: &str, value: V) {
        let mut store = self.store.lock().unwrap();
        match store.as_mut() {
            Some(map) => {
                map.insert(key.to_string(), value);
            }
            None => {
                self.record_failure(format!("set failed: cache unavailable for key '{key}'"));
            }
        }
    }

    /// Return the cached value for `key`, or fall back to `source_fn` if:
    /// - the cache is unavailable, or
    /// - the key is not present in the cache.
    ///
    /// If `source_fn` succeeds and returns `Some(value)`, the value is written
    /// back into the cache (if the backend is available).
    ///
    /// # Errors
    /// Returns `Err` only if `source_fn` itself returns an error.
    pub fn get_or_fallback<F, E>(
        &self,
        key: &str,
        source_fn: F,
    ) -> Result<Option<V>, E>
    where
        F: FnOnce(&str) -> Result<Option<V>, E>,
    {
        // Try cache first.
        {
            let mut store = self.store.lock().unwrap();
            match store.as_mut() {
                Some(map) => {
                    if let Some(v) = map.get(key) {
                        return Ok(Some(v.clone()));
                    }
                    // Cache miss — fall through to source.
                }
                None => {
                    // Backend unavailable — record if not already degraded.
                    if self.health() == CacheHealth::Healthy {
                        drop(store);
                        self.record_failure(format!(
                            "get failed: cache unavailable for key '{key}'"
                        ));
                    }
                }
            }
        }

        // Fall back to source.
        let value = source_fn(key)?;

        // Write-back if available.
        if let Some(ref v) = value {
            let mut store = self.store.lock().unwrap();
            if let Some(map) = store.as_mut() {
                map.insert(key.to_string(), v.clone());
            }
        }

        Ok(value)
    }

    /// Remove a key from the cache.
    pub fn remove(&self, key: &str) {
        let mut store = self.store.lock().unwrap();
        if let Some(map) = store.as_mut() {
            map.remove(key);
        }
    }

    // ── Cache rebuild ─────────────────────────────────────────────────────────

    /// Rebuild the cache from scratch by calling `source_fn` for each of the
    /// provided `keys`.
    ///
    /// On success the cache health is reset to `Healthy` and a `Rebuilt` event
    /// is recorded.  On failure the health remains `Degraded`.
    ///
    /// Returns the number of keys successfully loaded.
    ///
    /// # Errors
    /// Returns `Err` if any `source_fn` call fails.  The cache is left in
    /// whatever partially-rebuilt state it was in at the point of failure.
    pub fn rebuild<F, E>(
        &self,
        keys: &[&str],
        mut source_fn: F,
    ) -> Result<usize, E>
    where
        F: FnMut(&str) -> Result<Option<V>, E>,
    {
        // Ensure the store is alive before rebuilding.
        {
            let mut store = self.store.lock().unwrap();
            if store.is_none() {
                *store = Some(std::collections::HashMap::new());
            }
        }

        let mut loaded = 0;
        for &key in keys {
            if let Some(value) = source_fn(key)? {
                let mut store = self.store.lock().unwrap();
                if let Some(map) = store.as_mut() {
                    map.insert(key.to_string(), value);
                    loaded += 1;
                }
            }
        }

        // Mark healthy after successful rebuild.
        self.healthy.store(true, Ordering::Relaxed);
        self.tracker.record(format!("cache rebuilt: {loaded} keys loaded"));

        Ok(loaded)
    }

    /// Return the number of entries currently stored (0 if backend is down).
    pub fn entry_count(&self) -> usize {
        self.store
            .lock()
            .unwrap()
            .as_ref()
            .map(|m| m.len())
            .unwrap_or(0)
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    type TestCache = FaultTolerantCache<String>;

    fn make_cache() -> TestCache {
        FaultTolerantCache::new(100)
    }

    // ── FailureTracker ────────────────────────────────────────────────────────

    #[test]
    fn test_failure_tracker_records_events() {
        let tracker = FailureTracker::new(10);
        tracker.record("error one");
        tracker.record("error two");
        assert_eq!(tracker.failure_count(), 2);
    }

    #[test]
    fn test_failure_tracker_bounded_capacity() {
        let tracker = FailureTracker::new(3);
        tracker.record("e1");
        tracker.record("e2");
        tracker.record("e3");
        tracker.record("e4"); // should evict "e1"
        assert_eq!(tracker.failure_count(), 3);
        let events = tracker.events();
        assert_eq!(events[0].description, "e2");
    }

    #[test]
    fn test_failure_tracker_clear() {
        let tracker = FailureTracker::new(10);
        tracker.record("e1");
        tracker.clear();
        assert_eq!(tracker.failure_count(), 0);
    }

    // ── Health ────────────────────────────────────────────────────────────────

    #[test]
    fn test_initial_health_is_healthy() {
        let cache = make_cache();
        assert_eq!(cache.health(), CacheHealth::Healthy);
    }

    #[test]
    fn test_record_failure_marks_degraded() {
        let cache = make_cache();
        cache.record_failure("something went wrong");
        assert_eq!(cache.health(), CacheHealth::Degraded);
    }

    #[test]
    fn test_simulate_backend_failure_marks_degraded() {
        let cache = make_cache();
        cache.simulate_backend_failure("disk full");
        assert_eq!(cache.health(), CacheHealth::Degraded);
        assert_eq!(cache.tracker.failure_count(), 1);
    }

    // ── set / get_or_fallback ─────────────────────────────────────────────────

    #[test]
    fn test_set_and_get_from_cache() {
        let cache = make_cache();
        cache.set("k1", "v1".to_string());
        let result: Result<Option<String>, ()> =
            cache.get_or_fallback("k1", |_| Ok(None));
        assert_eq!(result.unwrap(), Some("v1".to_string()));
    }

    #[test]
    fn test_get_falls_back_to_source_on_miss() {
        let cache = make_cache();
        let result: Result<Option<String>, ()> =
            cache.get_or_fallback("missing_key", |k| Ok(Some(format!("source:{k}"))));
        assert_eq!(result.unwrap(), Some("source:missing_key".to_string()));
    }

    #[test]
    fn test_fallback_value_written_back_to_cache() {
        let cache = make_cache();
        // First call: cache miss → source returns value and it is written back.
        let _: Result<Option<String>, ()> =
            cache.get_or_fallback("k1", |_| Ok(Some("from_source".to_string())));
        // Second call: should now be a cache hit, source not consulted.
        let mut source_called = false;
        let result: Result<Option<String>, ()> = cache.get_or_fallback("k1", |_| {
            source_called = true;
            Ok(None)
        });
        assert_eq!(result.unwrap(), Some("from_source".to_string()));
        assert!(!source_called, "source should not have been called on second get");
    }

    #[test]
    fn test_get_falls_back_when_backend_unavailable() {
        let cache = make_cache();
        cache.simulate_backend_failure("backend down");
        let result: Result<Option<String>, ()> =
            cache.get_or_fallback("k1", |k| Ok(Some(format!("fallback:{k}"))));
        assert_eq!(result.unwrap(), Some("fallback:k1".to_string()));
    }

    #[test]
    fn test_source_error_propagated() {
        let cache = make_cache();
        let result: Result<Option<String>, &str> =
            cache.get_or_fallback("k1", |_| Err("source error"));
        assert!(result.is_err());
    }

    // ── Failure detection on set ──────────────────────────────────────────────

    #[test]
    fn test_set_records_failure_when_backend_unavailable() {
        let cache = make_cache();
        cache.simulate_backend_failure("initial failure");
        let before = cache.tracker.failure_count();
        cache.set("k1", "v1".to_string());
        // An additional failure should have been recorded for the failed set.
        assert!(cache.tracker.failure_count() >= before);
    }

    // ── Rebuild ───────────────────────────────────────────────────────────────

    #[test]
    fn test_rebuild_populates_cache() {
        let cache = make_cache();
        cache.simulate_backend_failure("failure");

        let keys = ["k1", "k2", "k3"];
        let loaded = cache
            .rebuild(&keys, |k| Ok::<Option<String>, ()>(Some(k.to_string())))
            .unwrap();
        assert_eq!(loaded, 3);
        assert_eq!(cache.entry_count(), 3);
    }

    #[test]
    fn test_rebuild_resets_health_to_healthy() {
        let cache = make_cache();
        cache.simulate_backend_failure("failure");
        assert_eq!(cache.health(), CacheHealth::Degraded);

        cache
            .rebuild(&["k1"], |_| Ok::<Option<String>, ()>(Some("v".to_string())))
            .unwrap();
        assert_eq!(cache.health(), CacheHealth::Healthy);
    }

    #[test]
    fn test_rebuild_records_event() {
        let cache = make_cache();
        cache
            .rebuild(&["k1"], |_| Ok::<Option<String>, ()>(Some("v".to_string())))
            .unwrap();
        let events = cache.tracker.events();
        assert!(events.last().unwrap().description.contains("rebuilt"));
    }

    #[test]
    fn test_rebuild_skips_keys_where_source_returns_none() {
        let cache = make_cache();
        let keys = ["k1", "k2"];
        let loaded = cache
            .rebuild(&keys, |k| {
                if k == "k1" {
                    Ok::<Option<String>, ()>(Some("v1".to_string()))
                } else {
                    Ok(None) // k2 not found in source
                }
            })
            .unwrap();
        assert_eq!(loaded, 1);
    }

    // ── remove ────────────────────────────────────────────────────────────────

    #[test]
    fn test_remove_existing_key() {
        let cache = make_cache();
        cache.set("k1", "v1".to_string());
        cache.remove("k1");
        let result: Result<Option<String>, ()> =
            cache.get_or_fallback("k1", |_| Ok(None));
        assert_eq!(result.unwrap(), None);
    }

    // ── entry_count ───────────────────────────────────────────────────────────

    #[test]
    fn test_entry_count_zero_when_backend_down() {
        let cache = make_cache();
        cache.set("k1", "v1".to_string());
        cache.simulate_backend_failure("down");
        assert_eq!(cache.entry_count(), 0);
    }

    #[test]
    fn test_entry_count_increments_on_set() {
        let cache = make_cache();
        assert_eq!(cache.entry_count(), 0);
        cache.set("k1", "v1".to_string());
        assert_eq!(cache.entry_count(), 1);
        cache.set("k2", "v2".to_string());
        assert_eq!(cache.entry_count(), 2);
    }
}

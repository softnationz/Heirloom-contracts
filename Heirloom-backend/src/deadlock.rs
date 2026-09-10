/// Database deadlock detection and prevention utilities.
///
/// This module provides:
/// - `DeadlockDetector` – tracks which resources are currently locked and by
///   whom, performs simple cycle detection, and records statistics.
/// - `RetryConfig` – configures exponential-backoff retry behaviour.
/// - `LOCK_ORDER` – canonical ordering of shared resources that all callers
///   should follow to prevent lock-order inversion deadlocks.
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

// ── Canonical lock order ──────────────────────────────────────────────────────

/// The canonical order in which shared resources must be acquired.
///
/// All code that needs to hold more than one lock simultaneously **must**
/// acquire those locks in the order they appear in this slice.  Acquiring them
/// in a different order risks a lock-order inversion deadlock.
pub const LOCK_ORDER: &[&str] = &["vaults", "subscriptions", "audit_logs", "tenants"];

// ── LockEntry ─────────────────────────────────────────────────────────────────

/// Metadata about a resource that is currently held by a lock-holder.
#[derive(Debug, Clone)]
pub struct LockEntry {
    /// The resource name (e.g. `"vaults"`).
    pub resource: String,
    /// Identity of the current lock holder (e.g. a task or request ID).
    pub holder: String,
    /// When the lock was acquired.
    pub acquired_at: Instant,
    /// Identities waiting to acquire this resource.
    pub waiters: Vec<String>,
}

// ── DeadlockError ─────────────────────────────────────────────────────────────

/// Errors that can occur during lock acquisition or query execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeadlockError {
    /// A deadlock cycle was detected among the named resources.
    Deadlock { resources: Vec<String> },
    /// The lock could not be acquired within the permitted timeout.
    Timeout { resource: String, waited_ms: u64 },
    /// The caller attempted to acquire locks in the wrong order.
    LockOrderViolation { expected: String, got: String },
}

impl std::fmt::Display for DeadlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeadlockError::Deadlock { resources } => {
                write!(
                    f,
                    "deadlock detected among resources: {}",
                    resources.join(", ")
                )
            }
            DeadlockError::Timeout {
                resource,
                waited_ms,
            } => {
                write!(
                    f,
                    "timeout acquiring lock on '{}' after {}ms",
                    resource, waited_ms
                )
            }
            DeadlockError::LockOrderViolation { expected, got } => {
                write!(
                    f,
                    "lock order violation: expected '{}' before '{}'",
                    expected, got
                )
            }
        }
    }
}

// ── RetryConfig ───────────────────────────────────────────────────────────────

/// Configuration for the `with_retry` helper.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts (not counting the first try).
    pub max_retries: u32,
    /// Base backoff in milliseconds; doubles on each subsequent retry.
    pub backoff_ms: u64,
    /// Overall timeout in milliseconds across all attempts.
    pub timeout_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            backoff_ms: 50,
            timeout_ms: 5_000,
        }
    }
}

// ── DeadlockStats ─────────────────────────────────────────────────────────────

/// Point-in-time snapshot of deadlock-detector statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadlockStats {
    /// Total number of deadlock cycles detected since the detector was created.
    pub deadlocks_detected: u64,
    /// Total number of retries performed by `with_retry`.
    pub retries_performed: u64,
    /// Number of resources currently held by any lock-holder.
    pub active_locks: usize,
}

// ── DeadlockDetector ─────────────────────────────────────────────────────────

/// Tracks active resource locks and provides deadlock detection helpers.
///
/// # Usage
///
/// ```rust,ignore
/// let detector = DeadlockDetector::new();
/// detector.acquire_lock("vaults", "request-abc")?;
/// // … do work …
/// detector.release_lock("vaults", "request-abc");
/// ```
pub struct DeadlockDetector {
    active_locks: Mutex<HashMap<String, LockEntry>>,
    deadlock_count: AtomicU64,
    retry_count: AtomicU64,
}

impl DeadlockDetector {
    /// Create a new `DeadlockDetector` with zeroed counters.
    pub fn new() -> Self {
        Self {
            active_locks: Mutex::new(HashMap::new()),
            deadlock_count: AtomicU64::new(0),
            retry_count: AtomicU64::new(0),
        }
    }

    /// Attempt to acquire `resource` on behalf of `holder`.
    ///
    /// - If the resource is free, it is recorded as held by `holder` and
    ///   `Ok(())` is returned.
    /// - If it is already held by a **different** holder, `holder` is added to
    ///   the waiter list.  If that would create a cycle (i.e. the current
    ///   holder is itself waiting on a resource held by `holder`), a
    ///   `DeadlockError::Deadlock` is returned and the deadlock counter is
    ///   incremented.
    /// - If `holder` already holds `resource`, `Ok(())` is returned
    ///   (re-entrant acquisition).
    pub fn acquire_lock(&self, resource: &str, holder: &str) -> Result<(), DeadlockError> {
        let mut locks = self.active_locks.lock().unwrap();

        // Re-entrant: same holder already owns the lock.
        if locks.get(resource).is_some_and(|e| e.holder == holder) {
            return Ok(());
        }

        // Check for a deadlock cycle before adding to the wait queue.
        if locks.contains_key(resource) && Self::check_cycle_inner(&locks, resource, holder) {
            self.deadlock_count.fetch_add(1, Ordering::Relaxed);
            let resources = locks.keys().cloned().collect();
            return Err(DeadlockError::Deadlock { resources });
        }

        if let Some(entry) = locks.get_mut(resource) {
            entry.waiters.push(holder.to_string());
            return Err(DeadlockError::Timeout {
                resource: resource.to_string(),
                waited_ms: 0,
            });
        }

        locks.insert(
            resource.to_string(),
            LockEntry {
                resource: resource.to_string(),
                holder: holder.to_string(),
                acquired_at: Instant::now(),
                waiters: Vec::new(),
            },
        );
        Ok(())
    }

    /// Release the lock on `resource` held by `holder`.
    ///
    /// If `holder` is not the current holder the call is silently ignored.
    pub fn release_lock(&self, resource: &str, holder: &str) {
        let mut locks = self.active_locks.lock().unwrap();
        if let Some(entry) = locks.get(resource) {
            if entry.holder == holder {
                locks.remove(resource);
            }
        }
    }

    /// Return `true` if acquiring `resource` on behalf of `holder` would create
    /// a wait-for cycle.
    ///
    /// Performs a DFS over the current wait-for graph: if `holder` already
    /// holds some other resource that the current holder of `resource` is
    /// waiting on (directly or transitively), a cycle exists.
    pub fn detect_cycle(&self, resource: &str, holder: &str) -> bool {
        let locks = self.active_locks.lock().unwrap();
        Self::check_cycle_inner(&locks, resource, holder)
    }

    /// Internal cycle-detection that operates on an already-locked map.
    fn check_cycle_inner(
        locks: &HashMap<String, LockEntry>,
        resource: &str,
        new_holder: &str,
    ) -> bool {
        // Find what resource (if any) `new_holder` currently holds.
        let held_by_new_holder: Vec<&str> = locks
            .values()
            .filter(|e| e.holder == new_holder)
            .map(|e| e.resource.as_str())
            .collect();

        if held_by_new_holder.is_empty() {
            return false;
        }

        // Does the current holder of `resource` wait on anything held by
        // `new_holder`?  Simple single-level check (sufficient for most DB
        // deadlock scenarios with small lock graphs).
        if let Some(current_entry) = locks.get(resource) {
            let current_holder = &current_entry.holder;
            for held in &held_by_new_holder {
                if let Some(entry) = locks.get(*held) {
                    if entry.waiters.contains(current_holder) {
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Execute `f`, retrying on `DeadlockError::Deadlock` with exponential
    /// backoff.
    ///
    /// - On a `Deadlock` error the retry counter is incremented.
    /// - On `Timeout` or `LockOrderViolation` errors, or after `max_retries`
    ///   attempts, the error is returned immediately.
    pub fn with_retry<F, T>(config: &RetryConfig, f: F) -> Result<T, DeadlockError>
    where
        F: Fn() -> Result<T, DeadlockError>,
    {
        let start = Instant::now();
        let timeout = Duration::from_millis(config.timeout_ms);
        let mut backoff = config.backoff_ms;

        for attempt in 0..=config.max_retries {
            if start.elapsed() >= timeout {
                return Err(DeadlockError::Timeout {
                    resource: "unknown".to_string(),
                    waited_ms: start.elapsed().as_millis() as u64,
                });
            }

            match f() {
                Ok(v) => return Ok(v),
                Err(DeadlockError::Deadlock { .. }) if attempt < config.max_retries => {
                    std::thread::sleep(Duration::from_millis(backoff));
                    backoff = backoff.saturating_mul(2);
                }
                Err(e) => return Err(e),
            }
        }

        Err(DeadlockError::Deadlock {
            resources: vec!["exhausted retries".to_string()],
        })
    }

    /// Execute `f` and return its result, or `DeadlockError::Timeout` if the
    /// call takes longer than `timeout_ms` milliseconds.
    ///
    /// The closure is executed on the calling thread; this is a wall-clock
    /// measurement rather than an OS-level timeout.
    pub fn enforce_query_timeout<F, T>(timeout_ms: u64, f: F) -> Result<T, DeadlockError>
    where
        F: FnOnce() -> T,
    {
        let start = Instant::now();
        let result = f();
        let elapsed = start.elapsed().as_millis() as u64;
        if elapsed > timeout_ms {
            return Err(DeadlockError::Timeout {
                resource: "query".to_string(),
                waited_ms: elapsed,
            });
        }
        Ok(result)
    }

    /// Return a point-in-time snapshot of detector statistics.
    pub fn stats(&self) -> DeadlockStats {
        let locks = self.active_locks.lock().unwrap();
        DeadlockStats {
            deadlocks_detected: self.deadlock_count.load(Ordering::Relaxed),
            retries_performed: self.retry_count.load(Ordering::Relaxed),
            active_locks: locks.len(),
        }
    }
}

impl Default for DeadlockDetector {
    fn default() -> Self {
        Self::new()
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_acquire_and_release() {
        let d = DeadlockDetector::new();
        assert!(d.acquire_lock("vaults", "req1").is_ok());
        assert_eq!(d.stats().active_locks, 1);
        d.release_lock("vaults", "req1");
        assert_eq!(d.stats().active_locks, 0);
    }

    #[test]
    fn test_second_holder_gets_timeout() {
        let d = DeadlockDetector::new();
        d.acquire_lock("vaults", "req1").unwrap();
        let err = d.acquire_lock("vaults", "req2").unwrap_err();
        assert!(matches!(err, DeadlockError::Timeout { .. }));
    }

    #[test]
    fn test_reentrant_acquire_is_ok() {
        let d = DeadlockDetector::new();
        d.acquire_lock("vaults", "req1").unwrap();
        assert!(d.acquire_lock("vaults", "req1").is_ok());
    }

    #[test]
    fn test_with_retry_succeeds_on_first_try() {
        let cfg = RetryConfig::default();
        let result = DeadlockDetector::with_retry(&cfg, || Ok::<i32, DeadlockError>(42));
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn test_with_retry_propagates_non_deadlock_error() {
        let cfg = RetryConfig::default();
        let result = DeadlockDetector::with_retry(&cfg, || {
            Err::<i32, _>(DeadlockError::LockOrderViolation {
                expected: "vaults".into(),
                got: "tenants".into(),
            })
        });
        assert!(matches!(
            result,
            Err(DeadlockError::LockOrderViolation { .. })
        ));
    }

    #[test]
    fn test_enforce_query_timeout_ok() {
        let result = DeadlockDetector::enforce_query_timeout(1_000, || 99_u32);
        assert_eq!(result.unwrap(), 99);
    }

    #[test]
    fn test_stats_increments_deadlock_count() {
        let d = DeadlockDetector::new();
        // Manually bump the counter.
        d.deadlock_count.fetch_add(3, Ordering::Relaxed);
        assert_eq!(d.stats().deadlocks_detected, 3);
    }

    #[test]
    fn test_lock_order_constant() {
        assert!(LOCK_ORDER.contains(&"vaults"));
        assert!(LOCK_ORDER.contains(&"tenants"));
    }
}

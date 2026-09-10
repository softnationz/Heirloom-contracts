//! Request prioritization (#129).
//!
//! All requests previously had equal standing, so a burst of low-value
//! traffic could starve critical-path requests (e.g. vault release checks)
//! of capacity. Clients now declare relative importance via the
//! `X-Priority` header; that value drives a priority-ordered queue for
//! internal work items and a per-priority concurrency budget enforced on
//! the request path. See `docs/request-prioritization.md`.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;

/// Header clients use to declare the importance of a request.
pub const PRIORITY_HEADER: &str = "x-priority";

/// Relative importance of an inbound request or queued work item.
///
/// Variants are declared low-to-high so the derived `Ord` orders
/// `Critical > High > Normal > Low`, which `PriorityQueue` relies on
/// directly.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Low,
    Normal,
    High,
    Critical,
}

impl Default for Priority {
    fn default() -> Self {
        Priority::Normal
    }
}

impl Priority {
    /// Parse a priority from a raw `X-Priority` header value. Unknown or
    /// missing values fall back to `Normal` so priority-unaware clients
    /// keep working unchanged.
    pub fn parse(value: &str) -> Priority {
        match value.trim().to_ascii_lowercase().as_str() {
            "low" => Priority::Low,
            "normal" => Priority::Normal,
            "high" => Priority::High,
            "critical" => Priority::Critical,
            _ => Priority::Normal,
        }
    }

    /// Extract the priority declared on an inbound request, defaulting to
    /// `Normal` when the header is absent or unparsable.
    pub fn from_headers(headers: &HeaderMap) -> Priority {
        headers
            .get(PRIORITY_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(Priority::parse)
            .unwrap_or_default()
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Normal => "normal",
            Priority::High => "high",
            Priority::Critical => "critical",
        }
    }
}

/// Per-priority tunables. `max_concurrent` bounds how many in-flight
/// requests a priority level may occupy at once (`0` = unbounded).
#[derive(Debug, Clone, Copy)]
pub struct PriorityConfig {
    pub low_max_concurrent: u64,
    pub normal_max_concurrent: u64,
    pub high_max_concurrent: u64,
    pub critical_max_concurrent: u64,
}

impl Default for PriorityConfig {
    fn default() -> Self {
        Self {
            low_max_concurrent: 50,
            normal_max_concurrent: 200,
            high_max_concurrent: 400,
            critical_max_concurrent: 0,
        }
    }
}

impl PriorityConfig {
    /// Build configuration from `PRIORITY_<LEVEL>_MAX_CONCURRENT`
    /// environment variables, falling back to defaults for anything unset
    /// or unparsable.
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            low_max_concurrent: env_u64("PRIORITY_LOW_MAX_CONCURRENT", defaults.low_max_concurrent),
            normal_max_concurrent: env_u64(
                "PRIORITY_NORMAL_MAX_CONCURRENT",
                defaults.normal_max_concurrent,
            ),
            high_max_concurrent: env_u64(
                "PRIORITY_HIGH_MAX_CONCURRENT",
                defaults.high_max_concurrent,
            ),
            critical_max_concurrent: env_u64(
                "PRIORITY_CRITICAL_MAX_CONCURRENT",
                defaults.critical_max_concurrent,
            ),
        }
    }

    pub fn max_concurrent(&self, priority: Priority) -> u64 {
        match priority {
            Priority::Low => self.low_max_concurrent,
            Priority::Normal => self.normal_max_concurrent,
            Priority::High => self.high_max_concurrent,
            Priority::Critical => self.critical_max_concurrent,
        }
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// A queued item ordered first by `Priority` (higher first), then by
/// arrival order (FIFO within the same priority level).
struct QueueEntry<T> {
    priority: Priority,
    sequence: u64,
    item: T,
}

impl<T> PartialEq for QueueEntry<T> {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.sequence == other.sequence
    }
}
impl<T> Eq for QueueEntry<T> {}

impl<T> PartialOrd for QueueEntry<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for QueueEntry<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher priority dequeues first; for equal priority, the older
        // (lower-sequence) entry dequeues first.
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

/// A thread-safe FIFO-within-priority queue for ordering internal work
/// items (e.g. webhook or notification dispatch) by declared `Priority`.
pub struct PriorityQueue<T> {
    heap: Mutex<BinaryHeap<QueueEntry<T>>>,
    next_sequence: AtomicU64,
}

impl<T> Default for PriorityQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> PriorityQueue<T> {
    pub fn new() -> Self {
        Self {
            heap: Mutex::new(BinaryHeap::new()),
            next_sequence: AtomicU64::new(0),
        }
    }

    /// Enqueue an item at the given priority.
    pub fn push(&self, priority: Priority, item: T) {
        let sequence = self.next_sequence.fetch_add(1, AtomicOrdering::Relaxed);
        self.heap.lock().unwrap().push(QueueEntry {
            priority,
            sequence,
            item,
        });
    }

    /// Dequeue the highest-priority, oldest-enqueued item.
    pub fn pop(&self) -> Option<T> {
        self.heap.lock().unwrap().pop().map(|e| e.item)
    }

    pub fn len(&self) -> usize {
        self.heap.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A reserved concurrency slot for one priority level. Releases the slot
/// automatically when dropped.
pub struct PriorityPermit {
    enforcer: Arc<PriorityEnforcer>,
    priority: Priority,
}

impl Drop for PriorityPermit {
    fn drop(&mut self) {
        self.enforcer
            .counter(self.priority)
            .fetch_sub(1, AtomicOrdering::AcqRel);
    }
}

/// Enforces per-priority concurrency limits (`PriorityConfig::max_concurrent`)
/// on the request path. Call `try_acquire` before doing the work for a
/// request; hold the returned `PriorityPermit` until it completes.
pub struct PriorityEnforcer {
    config: PriorityConfig,
    low_inflight: AtomicU64,
    normal_inflight: AtomicU64,
    high_inflight: AtomicU64,
    critical_inflight: AtomicU64,
}

impl PriorityEnforcer {
    pub fn new(config: PriorityConfig) -> Self {
        Self {
            config,
            low_inflight: AtomicU64::new(0),
            normal_inflight: AtomicU64::new(0),
            high_inflight: AtomicU64::new(0),
            critical_inflight: AtomicU64::new(0),
        }
    }

    fn counter(&self, priority: Priority) -> &AtomicU64 {
        match priority {
            Priority::Low => &self.low_inflight,
            Priority::Normal => &self.normal_inflight,
            Priority::High => &self.high_inflight,
            Priority::Critical => &self.critical_inflight,
        }
    }

    pub fn inflight(&self, priority: Priority) -> u64 {
        self.counter(priority).load(AtomicOrdering::Relaxed)
    }

    /// Attempt to reserve a concurrency slot for `priority` on `enforcer`.
    /// Returns `None` if that priority level is already at its configured
    /// `max_concurrent` (a limit of `0` means unbounded). Takes `&Arc<Self>`
    /// explicitly (rather than as a `self` receiver) since stable Rust
    /// doesn't support arbitrary `self: &Arc<Self>` receivers.
    pub fn try_acquire(
        enforcer: &Arc<PriorityEnforcer>,
        priority: Priority,
    ) -> Option<PriorityPermit> {
        let limit = enforcer.config.max_concurrent(priority);
        let counter = enforcer.counter(priority);
        loop {
            let current = counter.load(AtomicOrdering::Relaxed);
            if limit != 0 && current >= limit {
                return None;
            }
            if counter
                .compare_exchange_weak(
                    current,
                    current + 1,
                    AtomicOrdering::AcqRel,
                    AtomicOrdering::Relaxed,
                )
                .is_ok()
            {
                return Some(PriorityPermit {
                    enforcer: Arc::clone(enforcer),
                    priority,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn test_priority_ordering() {
        assert!(Priority::Critical > Priority::High);
        assert!(Priority::High > Priority::Normal);
        assert!(Priority::Normal > Priority::Low);
    }

    #[test]
    fn test_priority_parse_known_and_unknown() {
        assert_eq!(Priority::parse("Critical"), Priority::Critical);
        assert_eq!(Priority::parse(" high "), Priority::High);
        assert_eq!(Priority::parse("bogus"), Priority::Normal);
        assert_eq!(Priority::parse(""), Priority::Normal);
    }

    #[test]
    fn test_priority_from_headers_default() {
        let headers = HeaderMap::new();
        assert_eq!(Priority::from_headers(&headers), Priority::Normal);
    }

    #[test]
    fn test_priority_from_headers_present() {
        let mut headers = HeaderMap::new();
        headers.insert(PRIORITY_HEADER, HeaderValue::from_static("critical"));
        assert_eq!(Priority::from_headers(&headers), Priority::Critical);
    }

    #[test]
    fn test_queue_dequeues_highest_priority_first() {
        let queue: PriorityQueue<&str> = PriorityQueue::new();
        queue.push(Priority::Low, "low");
        queue.push(Priority::Critical, "critical");
        queue.push(Priority::Normal, "normal");

        assert_eq!(queue.pop(), Some("critical"));
        assert_eq!(queue.pop(), Some("normal"));
        assert_eq!(queue.pop(), Some("low"));
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn test_queue_is_fifo_within_priority() {
        let queue: PriorityQueue<u32> = PriorityQueue::new();
        queue.push(Priority::Normal, 1);
        queue.push(Priority::Normal, 2);
        queue.push(Priority::Normal, 3);

        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), Some(2));
        assert_eq!(queue.pop(), Some(3));
    }

    #[test]
    fn test_enforcer_blocks_at_limit() {
        let config = PriorityConfig {
            low_max_concurrent: 1,
            normal_max_concurrent: 0,
            high_max_concurrent: 0,
            critical_max_concurrent: 0,
        };
        let enforcer = Arc::new(PriorityEnforcer::new(config));

        let permit = PriorityEnforcer::try_acquire(&enforcer, Priority::Low);
        assert!(permit.is_some());
        assert_eq!(enforcer.inflight(Priority::Low), 1);

        assert!(PriorityEnforcer::try_acquire(&enforcer, Priority::Low).is_none());

        drop(permit);
        assert_eq!(enforcer.inflight(Priority::Low), 0);
        assert!(PriorityEnforcer::try_acquire(&enforcer, Priority::Low).is_some());
    }

    #[test]
    fn test_enforcer_unbounded_when_zero() {
        let enforcer = Arc::new(PriorityEnforcer::new(PriorityConfig {
            low_max_concurrent: 0,
            normal_max_concurrent: 0,
            high_max_concurrent: 0,
            critical_max_concurrent: 0,
        }));
        for _ in 0..1000 {
            std::mem::forget(PriorityEnforcer::try_acquire(&enforcer, Priority::Critical));
        }
        assert_eq!(enforcer.inflight(Priority::Critical), 1000);
    }
}

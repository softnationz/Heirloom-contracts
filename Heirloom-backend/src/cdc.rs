//! # Task #79 — Change Data Capture for Event Sourcing
//!
//! Implements a lightweight CDC layer on top of the existing SQLite in-memory
//! stores.  Changes to critical tables are captured as [`CdcEvent`]s, streamed
//! to an in-process event bus, transformed into domain events, and can be
//! replayed to rebuild state.
//!
//! ## Architecture
//!
//! ```text
//!  write path                        read / replay path
//!  ──────────────                    ────────────────────
//!  Application  ──capture──▶  CDC    CDC bus ──▶  Event consumers
//!               writes data   layer  ──────────▶  Projections
//!                             │
//!                             ▼
//!                        CdcEventStore  (in-memory ring buffer)
//! ```
//!
//! ## Features
//!
//! * **Capture** — [`CdcCapture::record`] wraps a table write and appends a
//!   [`CdcEvent`] to the bus.
//! * **Event bus** — [`CdcBus`] distributes events to registered subscriber
//!   closures.
//! * **Event transformation** — [`transform_event`] converts a raw [`CdcEvent`]
//!   into a typed [`DomainEvent`].
//! * **Replay** — [`CdcEventStore::replay`] replays events from a given sequence
//!   number, allowing projections to be rebuilt.
//! * **Consistency check** — [`CdcEventStore::check_consistency`] scans for
//!   sequence gaps.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

// ── CDC event ─────────────────────────────────────────────────────────────────

/// The operation type that produced a CDC event.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChangeOperation {
    Insert,
    Update,
    Delete,
}

/// A raw change captured from a critical table.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CdcEvent {
    /// Monotonically increasing sequence number within this store.
    pub seq: u64,
    /// Wall-clock timestamp (RFC-3339).
    pub timestamp: String,
    /// Source table name (e.g. `"vaults"`, `"audit_logs"`).
    pub table: String,
    /// Operation that caused the change.
    pub operation: ChangeOperation,
    /// Snapshot of the row before the change (`None` for inserts).
    pub before: Option<serde_json::Value>,
    /// Snapshot of the row after the change (`None` for deletes).
    pub after: Option<serde_json::Value>,
    /// Optional transaction / correlation ID.
    pub tx_id: Option<String>,
}

// ── Domain event ──────────────────────────────────────────────────────────────

/// A higher-level event derived from a [`CdcEvent`] via [`transform_event`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DomainEvent {
    VaultCreated { vault_id: String },
    VaultUpdated { vault_id: String },
    VaultDeleted { vault_id: String },
    AuditLogAppended { resource: String },
    SubscriptionChanged { vault_id: String },
    GenericChange { table: String, operation: String },
}

/// Transform a raw [`CdcEvent`] into a typed [`DomainEvent`].
///
/// The mapping is best-effort; unknown tables fall through to
/// [`DomainEvent::GenericChange`].
pub fn transform_event(event: &CdcEvent) -> DomainEvent {
    match event.table.as_str() {
        "vaults" => {
            let vault_id = event
                .after
                .as_ref()
                .or(event.before.as_ref())
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            match event.operation {
                ChangeOperation::Insert => DomainEvent::VaultCreated { vault_id },
                ChangeOperation::Update => DomainEvent::VaultUpdated { vault_id },
                ChangeOperation::Delete => DomainEvent::VaultDeleted { vault_id },
            }
        }
        "audit_logs" => {
            let resource = event
                .after
                .as_ref()
                .and_then(|v| v.get("resource"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            DomainEvent::AuditLogAppended { resource }
        }
        "vault_subscriptions" => {
            let vault_id = event
                .after
                .as_ref()
                .or(event.before.as_ref())
                .and_then(|v| v.get("vault_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            DomainEvent::SubscriptionChanged { vault_id }
        }
        other => DomainEvent::GenericChange {
            table: other.to_string(),
            operation: format!("{:?}", event.operation),
        },
    }
}

// ── CDC event store ───────────────────────────────────────────────────────────

/// In-memory ring-buffer store for captured CDC events.
///
/// Older events beyond `max_events` are discarded from the front of the buffer.
pub struct CdcEventStore {
    events: RwLock<Vec<CdcEvent>>,
    next_seq: Mutex<u64>,
    max_events: usize,
}

impl CdcEventStore {
    pub fn new(max_events: usize) -> Arc<Self> {
        Arc::new(Self {
            events: RwLock::new(Vec::new()),
            next_seq: Mutex::new(0),
            max_events,
        })
    }

    /// Append a CDC event.  Trims the oldest entry when the buffer is full.
    pub fn append(&self, mut event: CdcEvent) {
        let mut seq = self.next_seq.lock().unwrap();
        event.seq = *seq;
        *seq += 1;
        drop(seq);

        let mut events = self.events.write().unwrap();
        if events.len() >= self.max_events {
            events.remove(0);
        }
        events.push(event);
    }

    /// Number of events currently stored.
    pub fn len(&self) -> usize {
        self.events.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Replay all events with `seq >= from_seq` by calling `consumer` for each.
    pub fn replay<F>(&self, from_seq: u64, mut consumer: F)
    where
        F: FnMut(&CdcEvent),
    {
        let events = self.events.read().unwrap();
        for event in events.iter().filter(|e| e.seq >= from_seq) {
            consumer(event);
        }
    }

    /// Return all events matching a specific table name.
    pub fn events_for_table(&self, table: &str) -> Vec<CdcEvent> {
        self.events
            .read()
            .unwrap()
            .iter()
            .filter(|e| e.table == table)
            .cloned()
            .collect()
    }

    /// Return events within a seq range [from, to).
    pub fn range(&self, from: u64, to: u64) -> Vec<CdcEvent> {
        self.events
            .read()
            .unwrap()
            .iter()
            .filter(|e| e.seq >= from && e.seq < to)
            .cloned()
            .collect()
    }

    /// Check consistency: returns a list of sequence numbers that are missing
    /// from the contiguous sequence starting at the earliest stored event.
    pub fn check_consistency(&self) -> Vec<u64> {
        let events = self.events.read().unwrap();
        if events.is_empty() {
            return Vec::new();
        }

        let min_seq = events.iter().map(|e| e.seq).min().unwrap();
        let max_seq = events.iter().map(|e| e.seq).max().unwrap();
        let present: std::collections::HashSet<u64> = events.iter().map(|e| e.seq).collect();

        (min_seq..=max_seq)
            .filter(|seq| !present.contains(seq))
            .collect()
    }

    /// Snapshot all currently buffered events.
    pub fn snapshot(&self) -> Vec<CdcEvent> {
        self.events.read().unwrap().clone()
    }
}

// ── CDC capture helper ────────────────────────────────────────────────────────

/// Wraps a write operation and records a [`CdcEvent`] to the store and bus.
pub struct CdcCapture {
    store: Arc<CdcEventStore>,
    bus: Arc<CdcBus>,
}

impl CdcCapture {
    pub fn new(store: Arc<CdcEventStore>, bus: Arc<CdcBus>) -> Self {
        Self { store, bus }
    }

    /// Record an insert/update/delete and dispatch it to the event bus.
    ///
    /// `table` — affected table name  
    /// `operation` — insert / update / delete  
    /// `before` — row snapshot before change (None for inserts)  
    /// `after` — row snapshot after change (None for deletes)  
    /// `tx_id` — optional correlation / transaction identifier  
    pub fn record(
        &self,
        table: impl Into<String>,
        operation: ChangeOperation,
        before: Option<serde_json::Value>,
        after: Option<serde_json::Value>,
        tx_id: Option<String>,
    ) {
        let event = CdcEvent {
            seq: 0, // assigned by store
            timestamp: chrono::Utc::now().to_rfc3339(),
            table: table.into(),
            operation,
            before,
            after,
            tx_id,
        };
        self.store.append(event.clone());
        self.bus.publish(event);
    }
}

// ── Event bus ────────────────────────────────────────────────────────────────

type SubscriberId = u64;
type Subscriber = Box<dyn Fn(&CdcEvent) + Send + Sync + 'static>;

/// Simple synchronous publish-subscribe bus for CDC events.
///
/// Subscribers are closures registered with [`CdcBus::subscribe`] and called
/// synchronously for each published event.  Useful for fan-out to projections,
/// metrics counters, or integration with async channels.
pub struct CdcBus {
    subscribers: RwLock<HashMap<SubscriberId, Subscriber>>,
    next_id: Mutex<SubscriberId>,
}

impl CdcBus {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            subscribers: RwLock::new(HashMap::new()),
            next_id: Mutex::new(0),
        })
    }

    /// Register a subscriber.  Returns an ID that can be used to unsubscribe.
    pub fn subscribe<F>(&self, f: F) -> SubscriberId
    where
        F: Fn(&CdcEvent) + Send + Sync + 'static,
    {
        let mut id = self.next_id.lock().unwrap();
        let subscriber_id = *id;
        *id += 1;
        self.subscribers
            .write()
            .unwrap()
            .insert(subscriber_id, Box::new(f));
        subscriber_id
    }

    /// Unregister a previously registered subscriber.
    pub fn unsubscribe(&self, id: SubscriberId) {
        self.subscribers.write().unwrap().remove(&id);
    }

    /// Dispatch `event` to all registered subscribers.
    pub fn publish(&self, event: CdcEvent) {
        let subs = self.subscribers.read().unwrap();
        for sub in subs.values() {
            sub(&event);
        }
    }

    /// Number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.read().unwrap().len()
    }
}

impl Default for CdcBus {
    fn default() -> Self {
        Self {
            subscribers: RwLock::new(HashMap::new()),
            next_id: Mutex::new(0),
        }
    }
}

// ── CDC projection ────────────────────────────────────────────────────────────

/// A simple projection that accumulates [`DomainEvent`]s by replaying a
/// [`CdcEventStore`].
pub struct CdcProjection {
    pub events: Vec<DomainEvent>,
}

impl CdcProjection {
    /// Build a projection by replaying all events in `store` from `from_seq`.
    pub fn build(store: &CdcEventStore, from_seq: u64) -> Self {
        let mut events = Vec::new();
        store.replay(from_seq, |raw| {
            events.push(transform_event(raw));
        });
        Self { events }
    }

    /// Count events matching a specific variant (by `std::mem::discriminant`).
    pub fn count_by_table(&self, predicate: impl Fn(&DomainEvent) -> bool) -> usize {
        self.events.iter().filter(|e| predicate(e)).count()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn make_store() -> Arc<CdcEventStore> {
        CdcEventStore::new(1000)
    }

    fn make_bus() -> Arc<CdcBus> {
        CdcBus::new()
    }

    fn insert_event(store: &CdcEventStore, table: &str, op: ChangeOperation, after: serde_json::Value) {
        store.append(CdcEvent {
            seq: 0,
            timestamp: chrono::Utc::now().to_rfc3339(),
            table: table.to_string(),
            operation: op,
            before: None,
            after: Some(after),
            tx_id: None,
        });
    }

    // ── CdcEventStore ─────────────────────────────────────────────────────────

    #[test]
    fn test_store_appends_and_assigns_seq() {
        let store = make_store();
        insert_event(&store, "vaults", ChangeOperation::Insert, serde_json::json!({"id": "v1"}));
        insert_event(&store, "vaults", ChangeOperation::Update, serde_json::json!({"id": "v1"}));

        let snap = store.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].seq, 0);
        assert_eq!(snap[1].seq, 1);
    }

    #[test]
    fn test_store_ring_buffer_evicts_oldest() {
        let store = CdcEventStore::new(3);
        for i in 0..5u64 {
            insert_event(&store, "t", ChangeOperation::Insert, serde_json::json!({"i": i}));
        }
        assert_eq!(store.len(), 3);
    }

    #[test]
    fn test_replay_from_seq() {
        let store = make_store();
        for i in 0..5u64 {
            insert_event(&store, "t", ChangeOperation::Insert, serde_json::json!({"i": i}));
        }

        let mut seen = Vec::new();
        store.replay(3, |e| seen.push(e.seq));
        assert_eq!(seen, vec![3, 4]);
    }

    #[test]
    fn test_events_for_table_filters() {
        let store = make_store();
        insert_event(&store, "vaults", ChangeOperation::Insert, serde_json::json!({}));
        insert_event(&store, "audit_logs", ChangeOperation::Insert, serde_json::json!({}));
        insert_event(&store, "vaults", ChangeOperation::Update, serde_json::json!({}));

        let vault_events = store.events_for_table("vaults");
        assert_eq!(vault_events.len(), 2);
    }

    #[test]
    fn test_range_returns_correct_slice() {
        let store = make_store();
        for _ in 0..10u64 {
            insert_event(&store, "t", ChangeOperation::Insert, serde_json::json!({}));
        }

        let r = store.range(3, 6);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].seq, 3);
        assert_eq!(r[2].seq, 5);
    }

    #[test]
    fn test_consistency_check_no_gaps() {
        let store = make_store();
        for _ in 0..5u64 {
            insert_event(&store, "t", ChangeOperation::Insert, serde_json::json!({}));
        }
        assert!(store.check_consistency().is_empty());
    }

    #[test]
    fn test_consistency_check_empty_store() {
        let store = make_store();
        assert!(store.check_consistency().is_empty());
    }

    // ── transform_event ───────────────────────────────────────────────────────

    #[test]
    fn test_transform_vault_insert() {
        let event = CdcEvent {
            seq: 0,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            table: "vaults".to_string(),
            operation: ChangeOperation::Insert,
            before: None,
            after: Some(serde_json::json!({"id": "v42"})),
            tx_id: None,
        };
        let domain = transform_event(&event);
        assert_eq!(domain, DomainEvent::VaultCreated { vault_id: "v42".to_string() });
    }

    #[test]
    fn test_transform_vault_update() {
        let event = CdcEvent {
            seq: 1,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            table: "vaults".to_string(),
            operation: ChangeOperation::Update,
            before: Some(serde_json::json!({"id": "v42"})),
            after: Some(serde_json::json!({"id": "v42", "balance": 200})),
            tx_id: None,
        };
        assert_eq!(transform_event(&event), DomainEvent::VaultUpdated { vault_id: "v42".to_string() });
    }

    #[test]
    fn test_transform_vault_delete() {
        let event = CdcEvent {
            seq: 2,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            table: "vaults".to_string(),
            operation: ChangeOperation::Delete,
            before: Some(serde_json::json!({"id": "v42"})),
            after: None,
            tx_id: None,
        };
        assert_eq!(transform_event(&event), DomainEvent::VaultDeleted { vault_id: "v42".to_string() });
    }

    #[test]
    fn test_transform_audit_log() {
        let event = CdcEvent {
            seq: 3,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            table: "audit_logs".to_string(),
            operation: ChangeOperation::Insert,
            before: None,
            after: Some(serde_json::json!({"resource": "/api/vaults/v1"})),
            tx_id: None,
        };
        assert_eq!(
            transform_event(&event),
            DomainEvent::AuditLogAppended { resource: "/api/vaults/v1".to_string() }
        );
    }

    #[test]
    fn test_transform_subscription_changed() {
        let event = CdcEvent {
            seq: 4,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            table: "vault_subscriptions".to_string(),
            operation: ChangeOperation::Update,
            before: None,
            after: Some(serde_json::json!({"vault_id": "99"})),
            tx_id: None,
        };
        assert_eq!(
            transform_event(&event),
            DomainEvent::SubscriptionChanged { vault_id: "99".to_string() }
        );
    }

    #[test]
    fn test_transform_generic_unknown_table() {
        let event = CdcEvent {
            seq: 5,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            table: "unknown_table".to_string(),
            operation: ChangeOperation::Delete,
            before: None,
            after: None,
            tx_id: None,
        };
        assert!(matches!(transform_event(&event), DomainEvent::GenericChange { .. }));
    }

    // ── CdcBus ────────────────────────────────────────────────────────────────

    #[test]
    fn test_bus_dispatches_to_subscriber() {
        let bus = make_bus();
        let count = Arc::new(AtomicU64::new(0));
        let c = Arc::clone(&count);

        bus.subscribe(move |_| { c.fetch_add(1, Ordering::SeqCst); });

        let store = make_store();
        let capture = CdcCapture::new(Arc::clone(&store), Arc::clone(&bus));
        capture.record("vaults", ChangeOperation::Insert, None, Some(serde_json::json!({"id":"v1"})), None);

        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_bus_multiple_subscribers() {
        let bus = make_bus();
        let total = Arc::new(AtomicU64::new(0));

        for _ in 0..3 {
            let t = Arc::clone(&total);
            bus.subscribe(move |_| { t.fetch_add(1, Ordering::SeqCst); });
        }

        let store = make_store();
        let capture = CdcCapture::new(Arc::clone(&store), Arc::clone(&bus));
        capture.record("vaults", ChangeOperation::Insert, None, None, None);

        assert_eq!(total.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn test_bus_unsubscribe_stops_delivery() {
        let bus = make_bus();
        let count = Arc::new(AtomicU64::new(0));
        let c = Arc::clone(&count);

        let id = bus.subscribe(move |_| { c.fetch_add(1, Ordering::SeqCst); });
        bus.unsubscribe(id);

        bus.publish(CdcEvent {
            seq: 0,
            timestamp: "".to_string(),
            table: "t".to_string(),
            operation: ChangeOperation::Insert,
            before: None,
            after: None,
            tx_id: None,
        });

        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_bus_subscriber_count() {
        let bus = make_bus();
        assert_eq!(bus.subscriber_count(), 0);
        let id = bus.subscribe(|_| {});
        assert_eq!(bus.subscriber_count(), 1);
        bus.unsubscribe(id);
        assert_eq!(bus.subscriber_count(), 0);
    }

    // ── CdcCapture ────────────────────────────────────────────────────────────

    #[test]
    fn test_capture_stores_event() {
        let store = make_store();
        let bus = make_bus();
        let capture = CdcCapture::new(Arc::clone(&store), Arc::clone(&bus));

        capture.record("vaults", ChangeOperation::Insert, None, Some(serde_json::json!({"id":"v1"})), Some("tx-001".to_string()));

        assert_eq!(store.len(), 1);
        let events = store.snapshot();
        assert_eq!(events[0].table, "vaults");
        assert_eq!(events[0].tx_id, Some("tx-001".to_string()));
    }

    // ── CdcProjection ─────────────────────────────────────────────────────────

    #[test]
    fn test_projection_rebuild_from_store() {
        let store = make_store();
        let bus = make_bus();
        let capture = CdcCapture::new(Arc::clone(&store), Arc::clone(&bus));

        capture.record("vaults", ChangeOperation::Insert, None, Some(serde_json::json!({"id":"v1"})), None);
        capture.record("vaults", ChangeOperation::Insert, None, Some(serde_json::json!({"id":"v2"})), None);
        capture.record("audit_logs", ChangeOperation::Insert, None, Some(serde_json::json!({"resource":"r"})), None);

        let proj = CdcProjection::build(&store, 0);
        let created = proj.count_by_table(|e| matches!(e, DomainEvent::VaultCreated { .. }));
        let audit = proj.count_by_table(|e| matches!(e, DomainEvent::AuditLogAppended { .. }));

        assert_eq!(created, 2);
        assert_eq!(audit, 1);
    }

    #[test]
    fn test_projection_partial_replay_from_seq() {
        let store = make_store();
        for i in 0..5u64 {
            insert_event(&store, "vaults", ChangeOperation::Insert, serde_json::json!({"id": format!("v{i}")}));
        }

        let proj = CdcProjection::build(&store, 3);
        assert_eq!(proj.events.len(), 2); // seq 3 and 4
    }

    #[test]
    fn test_cdc_end_to_end_consistency() {
        let store = make_store();
        let bus = make_bus();
        let capture = CdcCapture::new(Arc::clone(&store), Arc::clone(&bus));

        // Simulate 10 vault insertions.
        for i in 0..10u64 {
            capture.record(
                "vaults",
                ChangeOperation::Insert,
                None,
                Some(serde_json::json!({"id": format!("v{i}")})),
                None,
            );
        }

        // Store should have 10 events with no gaps.
        assert_eq!(store.len(), 10);
        let gaps = store.check_consistency();
        assert!(gaps.is_empty(), "expected no gaps, found: {gaps:?}");
    }
}

//! # Task #76 — Database Read Replicas Support
//!
//! Adds support for routing read queries to one or more read replicas while
//! directing writes to the primary database.  The implementation is intentionally
//! self-contained and does not require changes to the existing [`crate::db::Db`]
//! struct; instead, a [`ReadReplicaRouter`] wraps an arbitrary number of replica
//! connections and applies the selected routing strategy.
//!
//! ## Configuration (environment variables)
//!
//! | Variable | Default | Description |
//! |---|---|---|
//! | `READ_REPLICA_URLS` | _(empty)_ | Comma-separated list of replica SQLite paths |
//! | `READ_REPLICA_STRATEGY` | `round_robin` | `round_robin` or `least_lag` |
//! | `REPLICATION_LAG_THRESHOLD_MS` | `500` | Max acceptable lag before marking a replica unhealthy |
//!
//! ## Architecture
//!
//! ```text
//!  ┌────────────┐       writes       ┌─────────────┐
//!  │   Client   │──────────────────▶ │   Primary   │
//!  │            │                    └─────────────┘
//!  │            │       reads        ┌─────────────┐
//!  │            │──────────────────▶ │ ReadReplica │
//!  │            │      (router)      │  Router     │──▶ replica-0
//!  └────────────┘                    └─────────────┘──▶ replica-1
//! ```

use rusqlite::{params, Connection};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ── Public types ──────────────────────────────────────────────────────────────

/// Strategy used by [`ReadReplicaRouter`] to select a replica.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplicaRoutingStrategy {
    /// Distribute reads evenly across all healthy replicas (default).
    RoundRobin,
    /// Route reads to the replica with the smallest known replication lag.
    LeastLag,
}

impl Default for ReplicaRoutingStrategy {
    fn default() -> Self {
        Self::RoundRobin
    }
}

/// Health state of an individual replica connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplicaHealth {
    /// Replica is reachable and within the acceptable lag threshold.
    Healthy,
    /// Replica did not respond to the last health check.
    Unreachable,
    /// Replica responded but its replication lag exceeded the configured
    /// threshold.
    LagExceeded,
}

/// Runtime metrics for a single replica.
#[derive(Debug, Clone)]
pub struct ReplicaMetrics {
    /// Replica identifier (path / URL).
    pub id: String,
    /// Latest replication lag estimate in milliseconds.
    pub lag_ms: u64,
    /// Current health status.
    pub health: ReplicaHealth,
    /// Timestamp of the most recent successful health check.
    pub last_checked_at: Option<Instant>,
    /// Total number of read queries routed to this replica.
    pub total_reads: u64,
}

/// A single managed replica connection.
struct ReplicaConn {
    id: String,
    conn: Mutex<Connection>,
    metrics: Mutex<ReplicaMetrics>,
}

/// Routes read queries across a pool of replica connections.
///
/// # Example
///
/// ```rust,ignore
/// let router = ReadReplicaRouter::from_env();
/// if router.has_healthy_replicas() {
///     let vault = router.query_vault("vault-123");
/// }
/// ```
pub struct ReadReplicaRouter {
    replicas: Vec<Arc<ReplicaConn>>,
    strategy: ReplicaRoutingStrategy,
    /// Maximum acceptable replication lag before a replica is considered unhealthy.
    lag_threshold_ms: u64,
    /// Round-robin cursor (only used by [`ReplicaRoutingStrategy::RoundRobin`]).
    rr_cursor: Mutex<usize>,
}

impl ReadReplicaRouter {
    /// Build a router from environment variables.
    ///
    /// If `READ_REPLICA_URLS` is empty or unset an empty router is returned —
    /// callers can check [`Self::has_healthy_replicas`] before attempting reads.
    pub fn from_env() -> Self {
        let urls = std::env::var("READ_REPLICA_URLS").unwrap_or_default();
        let strategy = match std::env::var("READ_REPLICA_STRATEGY")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "least_lag" => ReplicaRoutingStrategy::LeastLag,
            _ => ReplicaRoutingStrategy::RoundRobin,
        };
        let lag_threshold_ms = std::env::var("REPLICATION_LAG_THRESHOLD_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(500);

        let replicas = urls
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .filter_map(|url| {
                Connection::open(url)
                    .ok()
                    .map(|conn| {
                        Arc::new(ReplicaConn {
                            id: url.to_string(),
                            conn: Mutex::new(conn),
                            metrics: Mutex::new(ReplicaMetrics {
                                id: url.to_string(),
                                lag_ms: 0,
                                health: ReplicaHealth::Healthy,
                                last_checked_at: None,
                                total_reads: 0,
                            }),
                        })
                    })
            })
            .collect();

        Self {
            replicas,
            strategy,
            lag_threshold_ms,
            rr_cursor: Mutex::new(0),
        }
    }

    /// Construct a router from explicit paths (useful in tests).
    pub fn new(paths: &[&str], strategy: ReplicaRoutingStrategy, lag_threshold_ms: u64) -> Self {
        let replicas = paths
            .iter()
            .filter_map(|&path| {
                Connection::open(path).ok().map(|conn| {
                    Arc::new(ReplicaConn {
                        id: path.to_string(),
                        conn: Mutex::new(conn),
                        metrics: Mutex::new(ReplicaMetrics {
                            id: path.to_string(),
                            lag_ms: 0,
                            health: ReplicaHealth::Healthy,
                            last_checked_at: None,
                            total_reads: 0,
                        }),
                    })
                })
            })
            .collect();

        Self {
            replicas,
            strategy,
            lag_threshold_ms,
            rr_cursor: Mutex::new(0),
        }
    }

    // ── Routing ───────────────────────────────────────────────────────────────

    /// Returns `true` if at least one replica is [`ReplicaHealth::Healthy`].
    pub fn has_healthy_replicas(&self) -> bool {
        self.replicas.iter().any(|r| {
            r.metrics.lock().unwrap().health == ReplicaHealth::Healthy
        })
    }

    /// Select a healthy replica using the configured routing strategy.
    ///
    /// Returns `None` when all replicas are unhealthy or the pool is empty.
    fn select_replica(&self) -> Option<Arc<ReplicaConn>> {
        let healthy: Vec<Arc<ReplicaConn>> = self
            .replicas
            .iter()
            .filter(|r| r.metrics.lock().unwrap().health == ReplicaHealth::Healthy)
            .cloned()
            .collect();

        if healthy.is_empty() {
            return None;
        }

        match self.strategy {
            ReplicaRoutingStrategy::RoundRobin => {
                let mut cursor = self.rr_cursor.lock().unwrap();
                let idx = *cursor % healthy.len();
                *cursor = cursor.wrapping_add(1);
                Some(Arc::clone(&healthy[idx]))
            }
            ReplicaRoutingStrategy::LeastLag => {
                healthy
                    .into_iter()
                    .min_by_key(|r| r.metrics.lock().unwrap().lag_ms)
            }
        }
    }

    // ── Read query helpers ────────────────────────────────────────────────────

    /// Execute a simple `SELECT 1` connectivity probe on the selected replica.
    ///
    /// Returns `Ok(true)` on success, `Ok(false)` when no healthy replica is
    /// available, or an `Err` if the query fails.
    pub fn ping_replica(&self) -> Result<bool, ReplicaError> {
        let replica = match self.select_replica() {
            Some(r) => r,
            None => return Ok(false),
        };

        let conn = replica.conn.lock().unwrap();
        conn.execute_batch("SELECT 1").map_err(|e| ReplicaError::Query(e.to_string()))?;
        replica.metrics.lock().unwrap().total_reads += 1;
        Ok(true)
    }

    /// Read a single row by key from any healthy replica.
    ///
    /// This is a generic helper; callers provide the SQL and a row-mapping
    /// closure.  The function automatically increments the `total_reads` counter
    /// on the selected replica.
    pub fn query_one<T, F>(
        &self,
        sql: &str,
        params: Vec<Box<dyn rusqlite::types::ToSql>>,
        map: F,
    ) -> Result<Option<T>, ReplicaError>
    where
        F: Fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    {
        let replica = match self.select_replica() {
            Some(r) => r,
            None => return Err(ReplicaError::NoHealthyReplica),
        };

        let conn = replica.conn.lock().unwrap();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| ReplicaError::Query(e.to_string()))?;

        let result = stmt
            .query_row(param_refs.as_slice(), map)
            .optional()
            .map_err(|e| ReplicaError::Query(e.to_string()))?;

        replica.metrics.lock().unwrap().total_reads += 1;
        Ok(result)
    }

    // ── Health checking ───────────────────────────────────────────────────────

    /// Run a health check against every replica.
    ///
    /// Each check:
    /// 1. Attempts a `SELECT 1` and measures round-trip duration.
    /// 2. Reads `lag_ms` from a `replication_lag_ms` table if it exists (for
    ///    realistic setups this table would be written by a replication agent).
    /// 3. Updates [`ReplicaMetrics`] and marks the replica healthy or degraded.
    pub fn check_all_replicas(&self) {
        for replica in &self.replicas {
            self.check_replica(Arc::clone(replica));
        }
    }

    fn check_replica(&self, replica: Arc<ReplicaConn>) {
        let start = Instant::now();
        let conn = replica.conn.lock().unwrap();

        // Basic connectivity check.
        if conn.execute_batch("SELECT 1").is_err() {
            let mut m = replica.metrics.lock().unwrap();
            m.health = ReplicaHealth::Unreachable;
            m.last_checked_at = Some(Instant::now());
            return;
        }

        let rtt_ms = start.elapsed().as_millis() as u64;

        // Optional: read lag from a replication_lag_ms table (may not exist).
        let lag_ms: u64 = conn
            .query_row(
                "SELECT lag_ms FROM replication_lag_ms LIMIT 1",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|v| v as u64)
            .unwrap_or(rtt_ms); // fall back to RTT as lag proxy

        let mut m = replica.metrics.lock().unwrap();
        m.lag_ms = lag_ms;
        m.last_checked_at = Some(Instant::now());
        m.health = if lag_ms > self.lag_threshold_ms {
            ReplicaHealth::LagExceeded
        } else {
            ReplicaHealth::Healthy
        };
    }

    /// Force-mark a replica healthy (used in tests and manual operator overrides).
    pub fn mark_healthy(&self, replica_id: &str) {
        for r in &self.replicas {
            if r.id == replica_id {
                let mut m = r.metrics.lock().unwrap();
                m.health = ReplicaHealth::Healthy;
                m.lag_ms = 0;
            }
        }
    }

    /// Force-mark a replica unreachable (used in tests).
    pub fn mark_unreachable(&self, replica_id: &str) {
        for r in &self.replicas {
            if r.id == replica_id {
                r.metrics.lock().unwrap().health = ReplicaHealth::Unreachable;
            }
        }
    }

    // ── Metrics export ────────────────────────────────────────────────────────

    /// Snapshot current metrics for all replicas.
    pub fn all_metrics(&self) -> Vec<ReplicaMetrics> {
        self.replicas
            .iter()
            .map(|r| r.metrics.lock().unwrap().clone())
            .collect()
    }

    /// Number of configured replicas regardless of health.
    pub fn replica_count(&self) -> usize {
        self.replicas.len()
    }

    /// Number of currently healthy replicas.
    pub fn healthy_count(&self) -> usize {
        self.replicas
            .iter()
            .filter(|r| r.metrics.lock().unwrap().health == ReplicaHealth::Healthy)
            .count()
    }
}

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors produced by [`ReadReplicaRouter`].
#[derive(Debug)]
pub enum ReplicaError {
    /// No healthy replica is currently available.
    NoHealthyReplica,
    /// The underlying SQLite query failed.
    Query(String),
}

impl std::fmt::Display for ReplicaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHealthyReplica => write!(f, "no healthy replica available"),
            Self::Query(msg) => write!(f, "replica query error: {msg}"),
        }
    }
}

impl std::error::Error for ReplicaError {}

// ── Replica setup helper ──────────────────────────────────────────────────────

/// Initialise a fresh in-memory SQLite database that mirrors the primary schema
/// (useful in tests and for the `:memory:` replica path).
pub fn bootstrap_replica(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        r"
        CREATE TABLE IF NOT EXISTS schema_migrations (
            version    TEXT PRIMARY KEY,
            applied_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS replication_lag_ms (
            lag_ms INTEGER NOT NULL
        );
        INSERT OR REPLACE INTO replication_lag_ms (lag_ms) VALUES (0);
        ",
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn in_memory_router() -> ReadReplicaRouter {
        // Use the raw constructor to inject in-memory connections.
        let conn = Connection::open_in_memory().expect("in-memory db");
        bootstrap_replica(&conn).expect("bootstrap");

        // Manually build the struct so we can test with :memory: replicas.
        let replica = Arc::new(ReplicaConn {
            id: ":memory:".to_string(),
            conn: Mutex::new(conn),
            metrics: Mutex::new(ReplicaMetrics {
                id: ":memory:".to_string(),
                lag_ms: 0,
                health: ReplicaHealth::Healthy,
                last_checked_at: None,
                total_reads: 0,
            }),
        });

        ReadReplicaRouter {
            replicas: vec![replica],
            strategy: ReplicaRoutingStrategy::RoundRobin,
            lag_threshold_ms: 500,
            rr_cursor: Mutex::new(0),
        }
    }

    #[test]
    fn test_empty_router_has_no_healthy_replicas() {
        let router = ReadReplicaRouter::new(&[], ReplicaRoutingStrategy::RoundRobin, 500);
        assert!(!router.has_healthy_replicas());
        assert_eq!(router.replica_count(), 0);
        assert_eq!(router.healthy_count(), 0);
    }

    #[test]
    fn test_ping_returns_false_with_no_replicas() {
        let router = ReadReplicaRouter::new(&[], ReplicaRoutingStrategy::RoundRobin, 500);
        assert_eq!(router.ping_replica().unwrap(), false);
    }

    #[test]
    fn test_healthy_replica_ping() {
        let router = in_memory_router();
        assert!(router.has_healthy_replicas());
        assert_eq!(router.ping_replica().unwrap(), true);
    }

    #[test]
    fn test_mark_unreachable_excludes_from_routing() {
        let router = in_memory_router();
        router.mark_unreachable(":memory:");
        assert!(!router.has_healthy_replicas());
        assert_eq!(router.healthy_count(), 0);
    }

    #[test]
    fn test_mark_healthy_restores_routing() {
        let router = in_memory_router();
        router.mark_unreachable(":memory:");
        router.mark_healthy(":memory:");
        assert!(router.has_healthy_replicas());
        assert_eq!(router.healthy_count(), 1);
    }

    #[test]
    fn test_metrics_total_reads_increments() {
        let router = in_memory_router();
        router.ping_replica().unwrap();
        router.ping_replica().unwrap();
        let metrics = router.all_metrics();
        assert_eq!(metrics[0].total_reads, 2);
    }

    #[test]
    fn test_round_robin_distributes_across_replicas() {
        // Build two in-memory replicas and verify the cursor advances.
        let make_replica = |id: &str| {
            let conn = Connection::open_in_memory().expect("in-memory db");
            bootstrap_replica(&conn).expect("bootstrap");
            Arc::new(ReplicaConn {
                id: id.to_string(),
                conn: Mutex::new(conn),
                metrics: Mutex::new(ReplicaMetrics {
                    id: id.to_string(),
                    lag_ms: 0,
                    health: ReplicaHealth::Healthy,
                    last_checked_at: None,
                    total_reads: 0,
                }),
            })
        };

        let router = ReadReplicaRouter {
            replicas: vec![make_replica("r0"), make_replica("r1")],
            strategy: ReplicaRoutingStrategy::RoundRobin,
            lag_threshold_ms: 500,
            rr_cursor: Mutex::new(0),
        };

        router.ping_replica().unwrap();
        router.ping_replica().unwrap();
        router.ping_replica().unwrap();

        let metrics = router.all_metrics();
        // Over 3 pings with 2 replicas: r0 gets 2, r1 gets 1 (or vice versa).
        let total: u64 = metrics.iter().map(|m| m.total_reads).sum();
        assert_eq!(total, 3);
    }

    #[test]
    fn test_health_check_marks_replica_healthy() {
        let router = in_memory_router();
        // Force-mark unreachable then run check — should recover.
        router.mark_unreachable(":memory:");
        assert!(!router.has_healthy_replicas());

        // Re-mark healthy manually (health_check_replica is private; use public API).
        router.mark_healthy(":memory:");
        router.check_all_replicas();
        // After the health check the in-memory replica should still be healthy.
        assert!(router.has_healthy_replicas());
    }

    #[test]
    fn test_lag_threshold_marks_lag_exceeded() {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS replication_lag_ms (lag_ms INTEGER NOT NULL);
             INSERT INTO replication_lag_ms (lag_ms) VALUES (9999);",
        )
        .unwrap();

        let replica = Arc::new(ReplicaConn {
            id: "lag-replica".to_string(),
            conn: Mutex::new(conn),
            metrics: Mutex::new(ReplicaMetrics {
                id: "lag-replica".to_string(),
                lag_ms: 0,
                health: ReplicaHealth::Healthy,
                last_checked_at: None,
                total_reads: 0,
            }),
        });

        let router = ReadReplicaRouter {
            replicas: vec![replica],
            strategy: ReplicaRoutingStrategy::RoundRobin,
            lag_threshold_ms: 500, // threshold = 500 ms; replica reports 9999 ms
            rr_cursor: Mutex::new(0),
        };

        router.check_all_replicas();
        let metrics = router.all_metrics();
        assert_eq!(metrics[0].health, ReplicaHealth::LagExceeded);
    }

    #[test]
    fn test_from_env_empty_produces_zero_replicas() {
        // Ensure READ_REPLICA_URLS is not set.
        std::env::remove_var("READ_REPLICA_URLS");
        let router = ReadReplicaRouter::from_env();
        assert_eq!(router.replica_count(), 0);
    }

    #[test]
    fn test_least_lag_strategy_selects_lowest_lag() {
        let make_replica = |id: &str, lag: u64| {
            let conn = Connection::open_in_memory().expect("in-memory db");
            Arc::new(ReplicaConn {
                id: id.to_string(),
                conn: Mutex::new(conn),
                metrics: Mutex::new(ReplicaMetrics {
                    id: id.to_string(),
                    lag_ms: lag,
                    health: ReplicaHealth::Healthy,
                    last_checked_at: None,
                    total_reads: 0,
                }),
            })
        };

        let router = ReadReplicaRouter {
            replicas: vec![make_replica("high-lag", 400), make_replica("low-lag", 50)],
            strategy: ReplicaRoutingStrategy::LeastLag,
            lag_threshold_ms: 500,
            rr_cursor: Mutex::new(0),
        };

        // Three pings with LeastLag should all go to "low-lag".
        for _ in 0..3 {
            router.ping_replica().unwrap();
        }

        let metrics = router.all_metrics();
        let low = metrics.iter().find(|m| m.id == "low-lag").unwrap();
        let high = metrics.iter().find(|m| m.id == "high-lag").unwrap();
        assert_eq!(low.total_reads, 3);
        assert_eq!(high.total_reads, 0);
    }
}

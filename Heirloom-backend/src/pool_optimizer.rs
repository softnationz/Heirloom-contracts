//! # Task #78 — Database Connection Pool Optimization
//!
//! Provides an adaptive connection pool that wraps `rusqlite` connections.  The
//! pool grows toward [`PoolConfig::max`] under load and shrinks back toward
//! [`PoolConfig::min`] during idle periods, while tracking per-connection metrics
//! for observability.
//!
//! ## Features
//!
//! * **Adaptive pool sizing** — automatically acquires new connections when the
//!   pool is saturated and releases idle connections after a configurable idle
//!   timeout.
//! * **Connection lifecycle management** — each connection tracks creation time,
//!   last-used time, and total query count; connections exceeding `max_lifetime`
//!   are recycled.
//! * **Query queuing** — callers that cannot acquire an immediately available
//!   connection wait up to `queue_timeout` for one to become free.
//! * **Pool metrics** — a [`PoolMetrics`] snapshot is available at any time for
//!   Prometheus export or debug dashboards.
//! * **Benchmark helpers** — [`BenchmarkReport`] records throughput under
//!   configurable concurrency.
//!
//! ## Configuration (environment variables)
//!
//! | Variable | Default | Description |
//! |---|---|---|
//! | `DB_POOL_MIN` | `2` | Minimum live connections |
//! | `DB_POOL_MAX` | `10` | Maximum live connections |
//! | `DB_POOL_TIMEOUT_SECS` | `30` | SQLite busy-timeout per connection |
//! | `DB_POOL_IDLE_TIMEOUT_SECS` | `300` | Idle threshold before a connection is culled |
//! | `DB_POOL_MAX_LIFETIME_SECS` | `3600` | Maximum connection age before recycling |
//! | `DB_POOL_QUEUE_TIMEOUT_MS` | `5000` | How long a caller waits for a free connection |

use rusqlite::Connection;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

// ── Configuration ─────────────────────────────────────────────────────────────

/// Tunable parameters for the optimized pool.
#[derive(Debug, Clone)]
pub struct OptimizedPoolConfig {
    /// Minimum number of connections to keep alive.
    pub min: u32,
    /// Maximum number of connections the pool may create.
    pub max: u32,
    /// SQLite busy-timeout applied to each connection.
    pub timeout_secs: u32,
    /// A connection idle longer than this is eligible for culling.
    pub idle_timeout_secs: u64,
    /// A connection older than this is forcibly recycled.
    pub max_lifetime_secs: u64,
    /// How long a caller will block waiting for a free connection (ms).
    pub queue_timeout_ms: u64,
    /// Path to the SQLite database (`:memory:` for tests).
    pub db_path: String,
}

impl Default for OptimizedPoolConfig {
    fn default() -> Self {
        Self {
            min: 2,
            max: 10,
            timeout_secs: 30,
            idle_timeout_secs: 300,
            max_lifetime_secs: 3600,
            queue_timeout_ms: 5000,
            db_path: ":memory:".to_string(),
        }
    }
}

impl OptimizedPoolConfig {
    pub fn from_env() -> Self {
        Self {
            min: std::env::var("DB_POOL_MIN")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2),
            max: std::env::var("DB_POOL_MAX")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10),
            timeout_secs: std::env::var("DB_POOL_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
            idle_timeout_secs: std::env::var("DB_POOL_IDLE_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
            max_lifetime_secs: std::env::var("DB_POOL_MAX_LIFETIME_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3600),
            queue_timeout_ms: std::env::var("DB_POOL_QUEUE_TIMEOUT_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5000),
            db_path: std::env::var("DB_PATH").unwrap_or_else(|_| ":memory:".to_string()),
        }
    }
}

// ── Pooled connection ─────────────────────────────────────────────────────────

/// Lifecycle metadata for a single pooled connection.
#[derive(Debug, Clone)]
pub struct ConnMetadata {
    /// Sequential connection ID within this pool instance.
    pub id: u64,
    /// When this connection was first opened.
    pub created_at: Instant,
    /// When this connection was last returned to the pool (checked in).
    pub last_used_at: Instant,
    /// Total number of times this connection has been leased out.
    pub total_uses: u64,
    /// Whether this connection is currently checked out by a caller.
    pub in_use: bool,
}

struct PooledConn {
    conn: Connection,
    meta: ConnMetadata,
}

// ── Pool metrics ──────────────────────────────────────────────────────────────

/// Snapshot of current pool state for observability.
#[derive(Debug, Clone)]
pub struct PoolMetrics {
    /// Total connections currently in the pool (idle + in-use).
    pub total_connections: u32,
    /// Connections currently leased to callers.
    pub active_connections: u32,
    /// Connections sitting idle in the pool.
    pub idle_connections: u32,
    /// Total successful acquire operations since pool creation.
    pub total_acquires: u64,
    /// Total acquire attempts that had to wait in the queue.
    pub queued_acquires: u64,
    /// Total acquire attempts that timed out.
    pub acquire_timeouts: u64,
    /// Total connections that were recycled due to lifetime expiry.
    pub recycled_connections: u64,
    /// Total connections culled due to idle-timeout.
    pub idle_culled: u64,
    /// Current configured minimum.
    pub min: u32,
    /// Current configured maximum.
    pub max: u32,
}

// ── Pool inner state ──────────────────────────────────────────────────────────

struct Inner {
    config: OptimizedPoolConfig,
    /// Idle connections available for checkout.
    idle: Vec<PooledConn>,
    /// Total live connections (idle + in-use).
    live_count: u32,
    /// Monotonically increasing connection ID.
    next_id: u64,
    // Stats.
    total_acquires: u64,
    queued_acquires: u64,
    acquire_timeouts: u64,
    recycled_connections: u64,
    idle_culled: u64,
    /// Simulated in-use count (updated on acquire/release).
    in_use_count: u32,
}

impl Inner {
    fn new(config: OptimizedPoolConfig) -> Self {
        Self {
            config,
            idle: Vec::new(),
            live_count: 0,
            next_id: 0,
            total_acquires: 0,
            queued_acquires: 0,
            acquire_timeouts: 0,
            recycled_connections: 0,
            idle_culled: 0,
            in_use_count: 0,
        }
    }

    /// Open a fresh connection to the configured database.
    fn open_conn(&mut self) -> Result<PooledConn, PoolError> {
        let conn = Connection::open(&self.config.db_path)
            .map_err(|e| PoolError::ConnectionFailed(e.to_string()))?;
        conn.busy_timeout(Duration::from_secs(self.config.timeout_secs as u64))
            .ok();

        let id = self.next_id;
        self.next_id += 1;
        self.live_count += 1;

        Ok(PooledConn {
            conn,
            meta: ConnMetadata {
                id,
                created_at: Instant::now(),
                last_used_at: Instant::now(),
                total_uses: 0,
                in_use: false,
            },
        })
    }

    /// Remove connections that have exceeded `max_lifetime_secs`.
    fn evict_stale(&mut self) {
        let max = Duration::from_secs(self.config.max_lifetime_secs);
        let before = self.idle.len();
        self.idle.retain(|c| {
            let age = c.meta.created_at.elapsed();
            age < max
        });
        let evicted = (before - self.idle.len()) as u32;
        self.live_count = self.live_count.saturating_sub(evicted);
        self.recycled_connections += evicted as u64;
    }

    /// Remove idle connections beyond `min` that have been idle longer than
    /// `idle_timeout_secs`.
    fn cull_idle(&mut self) {
        if self.idle.len() as u32 <= self.config.min {
            return;
        }
        let threshold = Duration::from_secs(self.config.idle_timeout_secs);
        let mut culled = 0_u32;
        self.idle.retain(|c| {
            let idle_for = c.meta.last_used_at.elapsed();
            let keep = idle_for < threshold || (self.idle.len() as u32 - culled) <= self.config.min;
            if !keep {
                culled += 1;
            }
            keep
        });
        self.live_count = self.live_count.saturating_sub(culled);
        self.idle_culled += culled as u64;
    }

    fn metrics(&self) -> PoolMetrics {
        PoolMetrics {
            total_connections: self.live_count,
            active_connections: self.in_use_count,
            idle_connections: self.idle.len() as u32,
            total_acquires: self.total_acquires,
            queued_acquires: self.queued_acquires,
            acquire_timeouts: self.acquire_timeouts,
            recycled_connections: self.recycled_connections,
            idle_culled: self.idle_culled,
            min: self.config.min,
            max: self.config.max,
        }
    }
}

// ── Pool ──────────────────────────────────────────────────────────────────────

/// An adaptive, observable connection pool for `rusqlite`.
///
/// # Example
///
/// ```rust,ignore
/// let pool = OptimizedConnectionPool::new(OptimizedPoolConfig::default())?;
/// pool.prefill()?;
///
/// // Acquire a connection (blocks if all are in use, up to queue_timeout_ms).
/// let guard = pool.acquire()?;
/// guard.conn.execute_batch("SELECT 1").unwrap();
/// // Connection is returned to the pool when `guard` is dropped.
/// ```
pub struct OptimizedConnectionPool {
    inner: Arc<(Mutex<Inner>, Condvar)>,
}

impl OptimizedConnectionPool {
    /// Create a new pool with the given configuration.
    pub fn new(config: OptimizedPoolConfig) -> Result<Self, PoolError> {
        Ok(Self {
            inner: Arc::new((Mutex::new(Inner::new(config)), Condvar::new())),
        })
    }

    /// Create a pool from environment variables.
    pub fn from_env() -> Result<Self, PoolError> {
        Self::new(OptimizedPoolConfig::from_env())
    }

    /// Pre-fill the pool up to `min` connections.
    pub fn prefill(&self) -> Result<(), PoolError> {
        let (lock, _) = &*self.inner;
        let mut inner = lock.lock().unwrap();
        while (inner.idle.len() as u32) < inner.config.min {
            let pc = inner.open_conn()?;
            inner.idle.push(pc);
        }
        Ok(())
    }

    /// Acquire a connection from the pool.
    ///
    /// If no idle connection is available and the pool has room to grow, a new
    /// connection is created.  Otherwise the caller blocks for up to
    /// `queue_timeout_ms`.  Returns a [`PoolGuard`] that releases the connection
    /// when dropped.
    pub fn acquire(&self) -> Result<PoolGuard<'_>, PoolError> {
        let (lock, cvar) = &*self.inner;
        let timeout = {
            let inner = lock.lock().unwrap();
            Duration::from_millis(inner.config.queue_timeout_ms)
        };

        let deadline = Instant::now() + timeout;

        loop {
            let mut inner = lock.lock().unwrap();
            inner.evict_stale();

            // Try to pop an idle connection.
            if let Some(mut pc) = inner.idle.pop() {
                pc.meta.in_use = true;
                pc.meta.total_uses += 1;
                pc.meta.last_used_at = Instant::now();
                inner.total_acquires += 1;
                inner.in_use_count += 1;
                return Ok(PoolGuard {
                    conn: Some(pc),
                    pool: Arc::clone(&self.inner),
                });
            }

            // Can we grow?
            if inner.live_count < inner.config.max {
                let mut pc = inner.open_conn()?;
                pc.meta.in_use = true;
                pc.meta.total_uses += 1;
                inner.total_acquires += 1;
                inner.in_use_count += 1;
                return Ok(PoolGuard {
                    conn: Some(pc),
                    pool: Arc::clone(&self.inner),
                });
            }

            // Queue: wait for a release signal.
            inner.queued_acquires += 1;
            let remaining = match deadline.checked_duration_since(Instant::now()) {
                Some(d) => d,
                None => {
                    inner.acquire_timeouts += 1;
                    return Err(PoolError::Timeout);
                }
            };

            let (mut inner, timeout_result) = cvar.wait_timeout(inner, remaining).unwrap();
            if timeout_result.timed_out() {
                inner.acquire_timeouts += 1;
                return Err(PoolError::Timeout);
            }
        }
    }

    /// Run maintenance: evict stale connections and cull excess idle ones.
    ///
    /// Should be called periodically (e.g., by a background task).
    pub fn maintain(&self) {
        let (lock, _) = &*self.inner;
        let mut inner = lock.lock().unwrap();
        inner.evict_stale();
        inner.cull_idle();
    }

    /// Snapshot current pool metrics.
    pub fn metrics(&self) -> PoolMetrics {
        let (lock, _) = &*self.inner;
        lock.lock().unwrap().metrics()
    }

    /// Current number of idle connections.
    pub fn idle_count(&self) -> usize {
        let (lock, _) = &*self.inner;
        lock.lock().unwrap().idle.len()
    }

    /// Current number of live connections.
    pub fn live_count(&self) -> u32 {
        let (lock, _) = &*self.inner;
        lock.lock().unwrap().live_count
    }
}

// ── Pool guard ────────────────────────────────────────────────────────────────

/// RAII handle to a leased connection.  Returning the connection to the pool
/// happens automatically when this value is dropped.
pub struct PoolGuard<'a> {
    conn: Option<PooledConn>,
    pool: Arc<(Mutex<Inner>, Condvar)>,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> PoolGuard<'a> {
    /// Access the underlying [`Connection`].
    pub fn conn(&self) -> &Connection {
        &self.conn.as_ref().unwrap().conn
    }
}

impl Drop for PoolGuard<'_> {
    fn drop(&mut self) {
        if let Some(mut pc) = self.conn.take() {
            pc.meta.in_use = false;
            pc.meta.last_used_at = Instant::now();

            let (lock, cvar) = &*self.pool;
            let mut inner = lock.lock().unwrap();
            inner.in_use_count = inner.in_use_count.saturating_sub(1);
            inner.idle.push(pc);
            cvar.notify_one();
        }
    }
}

// ── Benchmark ────────────────────────────────────────────────────────────────

/// Result of a pool performance benchmark.
#[derive(Debug, Clone)]
pub struct BenchmarkReport {
    /// Number of iterations run.
    pub iterations: u64,
    /// Total elapsed time.
    pub elapsed: Duration,
    /// Throughput in operations per second.
    pub ops_per_sec: f64,
    /// Average time per operation in microseconds.
    pub avg_us: f64,
}

/// Run `iterations` sequential acquire→execute→release cycles and return a
/// performance report.
pub fn benchmark_pool(pool: &OptimizedConnectionPool, iterations: u64) -> BenchmarkReport {
    let start = Instant::now();
    for _ in 0..iterations {
        if let Ok(guard) = pool.acquire() {
            let _ = guard.conn().execute_batch("SELECT 1");
        }
    }
    let elapsed = start.elapsed();
    let secs = elapsed.as_secs_f64();
    let ops_per_sec = if secs > 0.0 { iterations as f64 / secs } else { f64::INFINITY };
    let avg_us = if iterations > 0 {
        elapsed.as_micros() as f64 / iterations as f64
    } else {
        0.0
    };
    BenchmarkReport { iterations, elapsed, ops_per_sec, avg_us }
}

// ── Error type ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum PoolError {
    /// A new connection could not be opened.
    ConnectionFailed(String),
    /// No connection became available within `queue_timeout_ms`.
    Timeout,
}

impl std::fmt::Display for PoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConnectionFailed(msg) => write!(f, "connection failed: {msg}"),
            Self::Timeout => write!(f, "acquire timed out waiting for a free connection"),
        }
    }
}

impl std::error::Error for PoolError {}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pool(min: u32, max: u32) -> OptimizedConnectionPool {
        let config = OptimizedPoolConfig {
            min,
            max,
            timeout_secs: 5,
            idle_timeout_secs: 300,
            max_lifetime_secs: 3600,
            queue_timeout_ms: 200,
            db_path: ":memory:".to_string(),
        };
        let pool = OptimizedConnectionPool::new(config).expect("pool");
        pool.prefill().expect("prefill");
        pool
    }

    #[test]
    fn test_pool_config_defaults() {
        let cfg = OptimizedPoolConfig::default();
        assert_eq!(cfg.min, 2);
        assert_eq!(cfg.max, 10);
    }

    #[test]
    fn test_prefill_creates_min_connections() {
        let pool = make_pool(3, 10);
        assert_eq!(pool.idle_count(), 3);
    }

    #[test]
    fn test_acquire_returns_working_connection() {
        let pool = make_pool(1, 5);
        let guard = pool.acquire().expect("acquire");
        guard.conn().execute_batch("SELECT 1").expect("query");
    }

    #[test]
    fn test_acquire_decrements_idle_count() {
        let pool = make_pool(2, 5);
        let _guard = pool.acquire().expect("acquire");
        assert_eq!(pool.idle_count(), 1);
    }

    #[test]
    fn test_release_increments_idle_count() {
        let pool = make_pool(2, 5);
        {
            let _guard = pool.acquire().expect("acquire");
            assert_eq!(pool.idle_count(), 1);
        }
        // Guard dropped → connection returned.
        assert_eq!(pool.idle_count(), 2);
    }

    #[test]
    fn test_pool_grows_beyond_min() {
        let pool = make_pool(1, 5);
        let g1 = pool.acquire().expect("g1");
        let g2 = pool.acquire().expect("g2");
        assert_eq!(pool.live_count(), 2);
        drop(g1);
        drop(g2);
    }

    #[test]
    fn test_acquire_timeout_when_pool_exhausted() {
        let pool = make_pool(1, 1);
        let _g = pool.acquire().expect("first acquire");
        // Pool is now full (1 live, 0 idle); next acquire should time out.
        let result = pool.acquire();
        assert!(matches!(result, Err(PoolError::Timeout)));
    }

    #[test]
    fn test_metrics_tracks_acquire_count() {
        let pool = make_pool(1, 5);
        {
            let _g = pool.acquire().expect("acquire");
        }
        let m = pool.metrics();
        assert_eq!(m.total_acquires, 1);
    }

    #[test]
    fn test_metrics_timeout_counter() {
        let pool = make_pool(1, 1);
        let _g = pool.acquire().expect("first");
        let _ = pool.acquire(); // will timeout
        let m = pool.metrics();
        assert_eq!(m.acquire_timeouts, 1);
    }

    #[test]
    fn test_maintain_culls_idle_over_min() {
        let config = OptimizedPoolConfig {
            min: 1,
            max: 5,
            timeout_secs: 5,
            idle_timeout_secs: 0, // instant idle timeout
            max_lifetime_secs: 3600,
            queue_timeout_ms: 200,
            db_path: ":memory:".to_string(),
        };
        let pool = OptimizedConnectionPool::new(config).expect("pool");
        pool.prefill().expect("prefill");

        // Add extra connections.
        {
            let _g1 = pool.acquire().expect("g1");
            let _g2 = pool.acquire().expect("g2");
        }

        // After maintain, idle beyond min (=1) with idle_timeout=0 should be culled.
        pool.maintain();
        let m = pool.metrics();
        assert!(m.idle_connections <= m.min);
    }

    #[test]
    fn test_benchmark_returns_report() {
        let pool = make_pool(2, 4);
        let report = benchmark_pool(&pool, 10);
        assert_eq!(report.iterations, 10);
        assert!(report.ops_per_sec > 0.0);
    }

    #[test]
    fn test_from_env_uses_defaults_when_env_unset() {
        std::env::remove_var("DB_POOL_MIN");
        std::env::remove_var("DB_POOL_MAX");
        let cfg = OptimizedPoolConfig::from_env();
        assert_eq!(cfg.min, 2);
        assert_eq!(cfg.max, 10);
    }

    #[test]
    fn test_pool_metrics_min_max() {
        let pool = make_pool(2, 8);
        let m = pool.metrics();
        assert_eq!(m.min, 2);
        assert_eq!(m.max, 8);
    }

    #[test]
    fn test_multiple_sequential_acquires() {
        let pool = make_pool(2, 10);
        for _ in 0..20 {
            let guard = pool.acquire().expect("acquire");
            guard.conn().execute_batch("SELECT 1").unwrap();
        }
        // All connections returned, idle >= min.
        assert!(pool.idle_count() >= 2);
    }
}

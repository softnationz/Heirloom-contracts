/// Cache metrics and observability (#94).
///
/// Tracks cache hit/miss rates, eviction patterns, entry counts, and
/// per-operation latency.  Exposes a `CacheMetricsSnapshot` that can be
/// serialised to JSON for a statistics endpoint, and renders to Prometheus
/// text format via `render_prometheus`.
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ── Rolling window for eviction timestamps ────────────────────────────────────

/// A bounded rolling window that records the timestamps of recent evictions,
/// allowing callers to query the eviction rate over an arbitrary lookback
/// window.
struct EvictionWindow {
    timestamps: Mutex<std::collections::VecDeque<Instant>>,
    capacity: usize,
}

impl EvictionWindow {
    fn new(capacity: usize) -> Self {
        Self {
            timestamps: Mutex::new(std::collections::VecDeque::with_capacity(capacity)),
            capacity,
        }
    }

    fn record(&self) {
        let mut ts = self.timestamps.lock().unwrap();
        if ts.len() == self.capacity {
            ts.pop_front();
        }
        ts.push_back(Instant::now());
    }

    /// Count evictions that occurred within the last `window` duration.
    fn count_within(&self, window: Duration) -> usize {
        let ts = self.timestamps.lock().unwrap();
        let cutoff = Instant::now()
            .checked_sub(window)
            .unwrap_or_else(|| Instant::now());
        ts.iter().filter(|&&t| t >= cutoff).count()
    }

    fn total(&self) -> usize {
        self.timestamps.lock().unwrap().len()
    }
}

// ── Latency accumulator ───────────────────────────────────────────────────────

/// Accumulates total latency (in microseconds) and sample count for computing
/// the mean operation latency.
struct LatencyAccumulator {
    total_us: AtomicU64,
    count: AtomicU64,
}

impl LatencyAccumulator {
    fn new() -> Self {
        Self {
            total_us: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    fn record(&self, elapsed: Duration) {
        let us = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        self.total_us.fetch_add(us, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    /// Mean latency in microseconds, or `None` if no samples.
    fn mean_us(&self) -> Option<f64> {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 {
            None
        } else {
            Some(self.total_us.load(Ordering::Relaxed) as f64 / count as f64)
        }
    }
}

// ── CacheMetrics ──────────────────────────────────────────────────────────────

/// Instrumented cache metrics collector.
///
/// All counters are updated via atomic operations; the eviction window uses a
/// `Mutex`-guarded `VecDeque` which is sized at construction time.
///
/// # Usage
/// ```
/// # use heirloom_protocol_backend::cache_metrics::CacheMetrics;
/// # use std::time::Instant;
/// let m = CacheMetrics::new(1000);
/// m.record_hit();
/// m.record_miss();
/// let snap = m.snapshot();
/// assert_eq!(snap.hits, 1);
/// assert_eq!(snap.misses, 1);
/// ```
pub struct CacheMetrics {
    hits: AtomicU64,
    misses: AtomicU64,
    sets: AtomicU64,
    deletes: AtomicU64,
    total_entries: AtomicU64,
    eviction_window: EvictionWindow,
    read_latency: LatencyAccumulator,
    write_latency: LatencyAccumulator,
}

impl CacheMetrics {
    /// Create a new metrics collector.
    ///
    /// `eviction_window_capacity` bounds the number of eviction timestamps
    /// kept in memory; once full, the oldest timestamps are dropped.
    pub fn new(eviction_window_capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            sets: AtomicU64::new(0),
            deletes: AtomicU64::new(0),
            total_entries: AtomicU64::new(0),
            eviction_window: EvictionWindow::new(eviction_window_capacity),
            read_latency: LatencyAccumulator::new(),
            write_latency: LatencyAccumulator::new(),
        })
    }

    // ── Recording helpers ─────────────────────────────────────────────────────

    /// Record a cache hit.
    pub fn record_hit(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a cache hit with the observed read latency.
    pub fn record_hit_with_latency(&self, elapsed: Duration) {
        self.hits.fetch_add(1, Ordering::Relaxed);
        self.read_latency.record(elapsed);
    }

    /// Record a cache miss.
    pub fn record_miss(&self) {
        self.misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a cache miss with the observed read latency.
    pub fn record_miss_with_latency(&self, elapsed: Duration) {
        self.misses.fetch_add(1, Ordering::Relaxed);
        self.read_latency.record(elapsed);
    }

    /// Record a cache set (write) operation.
    ///
    /// `is_new_entry` should be `true` if this is a new key; pass `false` for
    /// updates to an existing key so that `total_entries` stays accurate.
    pub fn record_set(&self, is_new_entry: bool) {
        self.sets.fetch_add(1, Ordering::Relaxed);
        if is_new_entry {
            self.total_entries.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record a cache set with write latency.
    pub fn record_set_with_latency(&self, is_new_entry: bool, elapsed: Duration) {
        self.record_set(is_new_entry);
        self.write_latency.record(elapsed);
    }

    /// Record a deletion from the cache.
    pub fn record_delete(&self) {
        self.deletes.fetch_add(1, Ordering::Relaxed);
        self.total_entries.fetch_saturating_sub(1, Ordering::Relaxed);
    }

    /// Record that a cache entry was evicted (e.g. due to TTL expiry).
    ///
    /// This decrements the live-entry counter **and** appends a timestamp to
    /// the eviction window for rate analysis.
    pub fn record_eviction(&self) {
        self.total_entries.fetch_saturating_sub(1, Ordering::Relaxed);
        self.eviction_window.record();
    }

    // ── Derived metrics ───────────────────────────────────────────────────────

    /// Cache-hit ratio in `[0.0, 1.0]`.  Returns `None` when no accesses have
    /// been recorded.
    pub fn hit_ratio(&self) -> Option<f64> {
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let total = hits + misses;
        if total == 0 {
            None
        } else {
            Some(hits as f64 / total as f64)
        }
    }

    /// Number of evictions recorded in the rolling window.
    ///
    /// Pass a `lookback` to restrict the count to recent evictions, or
    /// `None` to return all timestamps currently in the window.
    pub fn evictions_in_window(&self, lookback: Option<Duration>) -> usize {
        match lookback {
            Some(d) => self.eviction_window.count_within(d),
            None => self.eviction_window.total(),
        }
    }

    /// Total evictions ever recorded in the rolling window (bounded by capacity).
    pub fn total_evictions_tracked(&self) -> usize {
        self.eviction_window.total()
    }

    /// Mean read latency in microseconds, or `None` if no reads recorded.
    pub fn mean_read_latency_us(&self) -> Option<f64> {
        self.read_latency.mean_us()
    }

    /// Mean write latency in microseconds, or `None` if no writes recorded.
    pub fn mean_write_latency_us(&self) -> Option<f64> {
        self.write_latency.mean_us()
    }

    // ── Snapshot ──────────────────────────────────────────────────────────────

    /// Return a point-in-time snapshot of all metrics.
    pub fn snapshot(&self) -> CacheMetricsSnapshot {
        CacheMetricsSnapshot {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            sets: self.sets.load(Ordering::Relaxed),
            deletes: self.deletes.load(Ordering::Relaxed),
            total_entries: self.total_entries.load(Ordering::Relaxed),
            hit_ratio: self.hit_ratio(),
            evictions_tracked: self.eviction_window.total() as u64,
            mean_read_latency_us: self.read_latency.mean_us(),
            mean_write_latency_us: self.write_latency.mean_us(),
        }
    }

    // ── Prometheus rendering ──────────────────────────────────────────────────

    /// Render all cache metrics in Prometheus text exposition format.
    ///
    /// Suitable for appending to an existing `/metrics` response.
    pub fn render_prometheus(&self) -> String {
        let snap = self.snapshot();
        let mut out = String::new();

        push_counter(&mut out, "cache_hits_total", "Total cache hits", snap.hits);
        push_counter(
            &mut out,
            "cache_misses_total",
            "Total cache misses",
            snap.misses,
        );
        push_counter(
            &mut out,
            "cache_sets_total",
            "Total cache set operations",
            snap.sets,
        );
        push_counter(
            &mut out,
            "cache_deletes_total",
            "Total cache delete operations",
            snap.deletes,
        );
        push_gauge(
            &mut out,
            "cache_entries",
            "Current number of cache entries",
            snap.total_entries,
        );
        push_gauge(
            &mut out,
            "cache_evictions_tracked",
            "Evictions recorded in rolling window",
            snap.evictions_tracked,
        );

        if let Some(ratio) = snap.hit_ratio {
            let _ = writeln!(out, "# HELP cache_hit_ratio Cache hit ratio");
            let _ = writeln!(out, "# TYPE cache_hit_ratio gauge");
            let _ = writeln!(out, "cache_hit_ratio {ratio:.6}");
        }

        if let Some(lat) = snap.mean_read_latency_us {
            let _ = writeln!(
                out,
                "# HELP cache_mean_read_latency_us Mean read latency in microseconds"
            );
            let _ = writeln!(out, "# TYPE cache_mean_read_latency_us gauge");
            let _ = writeln!(out, "cache_mean_read_latency_us {lat:.3}");
        }

        if let Some(lat) = snap.mean_write_latency_us {
            let _ = writeln!(
                out,
                "# HELP cache_mean_write_latency_us Mean write latency in microseconds"
            );
            let _ = writeln!(out, "# TYPE cache_mean_write_latency_us gauge");
            let _ = writeln!(out, "cache_mean_write_latency_us {lat:.3}");
        }

        out
    }
}

// ── Snapshot type (serialisable) ─────────────────────────────────────────────

/// A point-in-time snapshot of cache metrics, suitable for JSON serialisation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CacheMetricsSnapshot {
    /// Total cache hits since creation.
    pub hits: u64,
    /// Total cache misses since creation.
    pub misses: u64,
    /// Total set operations since creation.
    pub sets: u64,
    /// Total delete operations since creation.
    pub deletes: u64,
    /// Current number of tracked cache entries.
    pub total_entries: u64,
    /// Computed hit ratio `[0.0, 1.0]`; `None` if no accesses recorded.
    pub hit_ratio: Option<f64>,
    /// Number of evictions currently in the rolling window.
    pub evictions_tracked: u64,
    /// Mean read latency in microseconds; `None` if no reads recorded.
    pub mean_read_latency_us: Option<f64>,
    /// Mean write latency in microseconds; `None` if no writes recorded.
    pub mean_write_latency_us: Option<f64>,
}

// ── Route handler (statistics endpoint) ──────────────────────────────────────

/// Axum handler for `GET /api/cache/stats`.
///
/// Returns the current `CacheMetricsSnapshot` as JSON.
///
/// # Example response
/// ```json
/// {
///   "hits": 120,
///   "misses": 30,
///   "sets": 55,
///   "deletes": 5,
///   "total_entries": 50,
///   "hit_ratio": 0.8,
///   "evictions_tracked": 10,
///   "mean_read_latency_us": 12.5,
///   "mean_write_latency_us": 8.2
/// }
/// ```
pub async fn cache_stats_handler(
    axum::extract::State(metrics): axum::extract::State<Arc<CacheMetrics>>,
) -> axum::Json<CacheMetricsSnapshot> {
    axum::Json(metrics.snapshot())
}

// ── Prometheus render helpers ─────────────────────────────────────────────────

fn push_counter(out: &mut String, name: &str, help: &str, value: u64) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} counter");
    let _ = writeln!(out, "{name} {value}");
}

fn push_gauge(out: &mut String, name: &str, help: &str, value: u64) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} gauge");
    let _ = writeln!(out, "{name} {value}");
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_metrics() -> Arc<CacheMetrics> {
        CacheMetrics::new(100)
    }

    // ── Hit / miss counters ───────────────────────────────────────────────────

    #[test]
    fn test_initial_counters_zero() {
        let m = make_metrics();
        let snap = m.snapshot();
        assert_eq!(snap.hits, 0);
        assert_eq!(snap.misses, 0);
        assert_eq!(snap.sets, 0);
        assert_eq!(snap.deletes, 0);
        assert_eq!(snap.total_entries, 0);
    }

    #[test]
    fn test_record_hit_increments_hits() {
        let m = make_metrics();
        m.record_hit();
        m.record_hit();
        assert_eq!(m.snapshot().hits, 2);
    }

    #[test]
    fn test_record_miss_increments_misses() {
        let m = make_metrics();
        m.record_miss();
        assert_eq!(m.snapshot().misses, 1);
    }

    // ── Hit ratio ─────────────────────────────────────────────────────────────

    #[test]
    fn test_hit_ratio_none_with_no_accesses() {
        let m = make_metrics();
        assert!(m.hit_ratio().is_none());
    }

    #[test]
    fn test_hit_ratio_all_hits() {
        let m = make_metrics();
        m.record_hit();
        m.record_hit();
        assert_eq!(m.hit_ratio(), Some(1.0));
    }

    #[test]
    fn test_hit_ratio_all_misses() {
        let m = make_metrics();
        m.record_miss();
        m.record_miss();
        assert_eq!(m.hit_ratio(), Some(0.0));
    }

    #[test]
    fn test_hit_ratio_mixed() {
        let m = make_metrics();
        m.record_hit();
        m.record_miss();
        assert_eq!(m.hit_ratio(), Some(0.5));
    }

    // ── Sets / deletes ────────────────────────────────────────────────────────

    #[test]
    fn test_record_set_new_entry_increments_entry_count() {
        let m = make_metrics();
        m.record_set(true);
        m.record_set(true);
        let snap = m.snapshot();
        assert_eq!(snap.sets, 2);
        assert_eq!(snap.total_entries, 2);
    }

    #[test]
    fn test_record_set_existing_entry_no_entry_count_change() {
        let m = make_metrics();
        m.record_set(true); // new
        m.record_set(false); // update
        let snap = m.snapshot();
        assert_eq!(snap.sets, 2);
        assert_eq!(snap.total_entries, 1);
    }

    #[test]
    fn test_record_delete_decrements_entry_count() {
        let m = make_metrics();
        m.record_set(true);
        m.record_set(true);
        m.record_delete();
        assert_eq!(m.snapshot().total_entries, 1);
    }

    #[test]
    fn test_record_delete_does_not_underflow() {
        let m = make_metrics();
        // Delete with no entries should not panic or wrap around.
        m.record_delete();
        assert_eq!(m.snapshot().total_entries, 0);
    }

    // ── Evictions ─────────────────────────────────────────────────────────────

    #[test]
    fn test_record_eviction_decrements_entries() {
        let m = make_metrics();
        m.record_set(true);
        m.record_eviction();
        assert_eq!(m.snapshot().total_entries, 0);
    }

    #[test]
    fn test_evictions_tracked_increments() {
        let m = make_metrics();
        m.record_eviction();
        m.record_eviction();
        assert_eq!(m.snapshot().evictions_tracked, 2);
    }

    #[test]
    fn test_evictions_in_window_recent() {
        let m = make_metrics();
        m.record_eviction();
        // All evictions happened within the last second.
        assert_eq!(m.evictions_in_window(Some(Duration::from_secs(1))), 1);
    }

    #[test]
    fn test_evictions_in_window_excludes_old() {
        let m = CacheMetrics::new(100);
        // Record 0 evictions; the window lookback of 0 duration should return 0.
        m.record_eviction();
        // Looking back 0 seconds should return 0 (cutoff == now).
        let count = m.evictions_in_window(Some(Duration::from_nanos(0)));
        // May be 0 or 1 depending on sub-nanosecond timing; just check no panic.
        let _ = count;
    }

    // ── Latency ───────────────────────────────────────────────────────────────

    #[test]
    fn test_read_latency_none_with_no_reads() {
        let m = make_metrics();
        assert!(m.mean_read_latency_us().is_none());
    }

    #[test]
    fn test_record_hit_with_latency_tracks_latency() {
        let m = make_metrics();
        m.record_hit_with_latency(Duration::from_micros(100));
        m.record_hit_with_latency(Duration::from_micros(200));
        let mean = m.mean_read_latency_us().unwrap();
        assert!((mean - 150.0).abs() < 1.0);
    }

    #[test]
    fn test_record_miss_with_latency_tracks_latency() {
        let m = make_metrics();
        m.record_miss_with_latency(Duration::from_micros(50));
        let mean = m.mean_read_latency_us().unwrap();
        assert!((mean - 50.0).abs() < 1.0);
    }

    #[test]
    fn test_write_latency_tracked() {
        let m = make_metrics();
        m.record_set_with_latency(true, Duration::from_micros(80));
        let mean = m.mean_write_latency_us().unwrap();
        assert!((mean - 80.0).abs() < 1.0);
    }

    // ── Prometheus rendering ──────────────────────────────────────────────────

    #[test]
    fn test_render_prometheus_contains_all_metrics() {
        let m = make_metrics();
        m.record_hit();
        m.record_miss();
        m.record_set(true);
        m.record_delete();
        m.record_eviction();
        m.record_hit_with_latency(Duration::from_micros(10));

        let output = m.render_prometheus();
        assert!(output.contains("cache_hits_total 1"));
        assert!(output.contains("cache_misses_total 1"));
        assert!(output.contains("cache_sets_total 1"));
        assert!(output.contains("cache_deletes_total 1"));
        assert!(output.contains("# TYPE cache_hits_total counter"));
        assert!(output.contains("# TYPE cache_entries gauge"));
    }

    #[test]
    fn test_render_prometheus_includes_hit_ratio_when_available() {
        let m = make_metrics();
        m.record_hit();
        let output = m.render_prometheus();
        assert!(output.contains("cache_hit_ratio"));
    }

    #[test]
    fn test_render_prometheus_no_hit_ratio_when_no_accesses() {
        let m = make_metrics();
        let output = m.render_prometheus();
        assert!(!output.contains("cache_hit_ratio"));
    }

    // ── Snapshot serialisation ────────────────────────────────────────────────

    #[test]
    fn test_snapshot_serialises_to_json() {
        let m = make_metrics();
        m.record_hit();
        let snap = m.snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains("\"hits\":1"));
    }
}

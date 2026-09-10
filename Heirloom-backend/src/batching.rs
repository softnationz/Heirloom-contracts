//! Adaptive batching (#131).
//!
//! Static batch sizes leave throughput on the table: too small and
//! per-batch overhead dominates, too large and tail latency blows up.
//! `AdaptiveBatcher` tracks a rolling window of recent batch processing
//! latency and grows or shrinks the next batch size to track a target
//! latency, subject to configured `min`/`max` bounds. See
//! `docs/adaptive-batching.md`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Tunable limits and target latency for adaptive batch sizing.
#[derive(Debug, Clone, Copy)]
pub struct BatchConfig {
    pub min_batch_size: usize,
    pub max_batch_size: usize,
    pub initial_batch_size: usize,
    /// Desired time to process one batch. The batcher grows batch size
    /// while observed latency stays comfortably under this and shrinks
    /// once it's exceeded.
    pub target_latency: Duration,
    /// How many recent batch latencies to average over.
    pub window: usize,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            min_batch_size: 1,
            max_batch_size: 500,
            initial_batch_size: 25,
            target_latency: Duration::from_millis(200),
            window: 20,
        }
    }
}

impl BatchConfig {
    /// Build from `BATCH_MIN_SIZE`, `BATCH_MAX_SIZE`, `BATCH_INITIAL_SIZE`,
    /// `BATCH_TARGET_LATENCY_MS` and `BATCH_LATENCY_WINDOW` environment
    /// variables, falling back to defaults when unset or unparsable.
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            min_batch_size: env_usize("BATCH_MIN_SIZE", defaults.min_batch_size),
            max_batch_size: env_usize("BATCH_MAX_SIZE", defaults.max_batch_size),
            initial_batch_size: env_usize("BATCH_INITIAL_SIZE", defaults.initial_batch_size),
            target_latency: Duration::from_millis(env_u64(
                "BATCH_TARGET_LATENCY_MS",
                defaults.target_latency.as_millis() as u64,
            )),
            window: env_usize("BATCH_LATENCY_WINDOW", defaults.window),
        }
    }
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

struct LatencyWindow {
    samples: VecDeque<Duration>,
    capacity: usize,
}

impl LatencyWindow {
    fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
        }
    }

    fn push(&mut self, sample: Duration) {
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    fn average(&self) -> Option<Duration> {
        if self.samples.is_empty() {
            return None;
        }
        let total: Duration = self.samples.iter().sum();
        Some(total / self.samples.len() as u32)
    }
}

/// Point-in-time adaptive batching metrics.
pub struct BatchingMetrics {
    pub current_batch_size: usize,
    pub average_latency_ms: Option<u128>,
    pub batches_processed_total: u64,
    pub items_processed_total: u64,
    pub resizes_total: u64,
}

/// Dynamically sizes batches to track `BatchConfig::target_latency`,
/// bounded by `min_batch_size`/`max_batch_size`.
pub struct AdaptiveBatcher {
    config: BatchConfig,
    current_size: AtomicUsize,
    latencies: Mutex<LatencyWindow>,
    batches_processed_total: AtomicU64,
    items_processed_total: AtomicU64,
    resizes_total: AtomicU64,
}

impl AdaptiveBatcher {
    pub fn new(config: BatchConfig) -> Self {
        let current_size = config.initial_batch_size.clamp(
            config.min_batch_size,
            config.max_batch_size.max(config.min_batch_size),
        );
        Self {
            latencies: Mutex::new(LatencyWindow::new(config.window)),
            current_size: AtomicUsize::new(current_size),
            config,
            batches_processed_total: AtomicU64::new(0),
            items_processed_total: AtomicU64::new(0),
            resizes_total: AtomicU64::new(0),
        }
    }

    /// The batch size callers should use for the next batch.
    pub fn current_batch_size(&self) -> usize {
        self.current_size.load(Ordering::Relaxed)
    }

    /// Record that a batch of `items` took `elapsed` to process, and
    /// recompute the next batch size from the rolling average latency.
    pub fn record_batch(&self, items: usize, elapsed: Duration) {
        self.batches_processed_total.fetch_add(1, Ordering::Relaxed);
        self.items_processed_total
            .fetch_add(items as u64, Ordering::Relaxed);

        let average = {
            let mut window = self.latencies.lock().unwrap();
            window.push(elapsed);
            window.average().unwrap_or(elapsed)
        };

        self.resize(average);
    }

    fn resize(&self, average_latency: Duration) {
        let target = self.config.target_latency;
        let current = self.current_batch_size();

        // A slack band around the target avoids oscillating size by +/-1
        // on every single batch.
        let low_water = target.mul_f64(0.8);
        let high_water = target;

        let next = if average_latency > high_water {
            // Over budget: shrink proportionally to how far over we are.
            let ratio = high_water.as_secs_f64() / average_latency.as_secs_f64().max(0.000_001);
            ((current as f64) * ratio).floor() as usize
        } else if average_latency < low_water {
            // Under budget: grow, capped so a single step can't overshoot.
            current + (current / 4).max(1)
        } else {
            current
        };

        let clamped = next.clamp(self.config.min_batch_size, self.config.max_batch_size);
        if clamped != current {
            self.current_size.store(clamped, Ordering::Relaxed);
            self.resizes_total.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(
                previous = current,
                next = clamped,
                average_latency_ms = average_latency.as_millis(),
                "adaptive batch size adjusted"
            );
        }
    }

    pub fn metrics(&self) -> BatchingMetrics {
        let average_latency_ms = self
            .latencies
            .lock()
            .unwrap()
            .average()
            .map(|d| d.as_millis());
        BatchingMetrics {
            current_batch_size: self.current_batch_size(),
            average_latency_ms,
            batches_processed_total: self.batches_processed_total.load(Ordering::Relaxed),
            items_processed_total: self.items_processed_total.load(Ordering::Relaxed),
            resizes_total: self.resizes_total.load(Ordering::Relaxed),
        }
    }

    pub fn render_prometheus(&self) -> String {
        let m = self.metrics();
        let mut out = String::new();
        crate::metrics::push_gauge(
            &mut out,
            "heirloom_protocol_batch_current_size",
            "Current adaptive batch size",
            m.current_batch_size as u64,
        );
        crate::metrics::push_counter(
            &mut out,
            "heirloom_protocol_batches_processed_total",
            "Total batches processed",
            m.batches_processed_total,
        );
        crate::metrics::push_counter(
            &mut out,
            "heirloom_protocol_batch_items_processed_total",
            "Total items processed across all batches",
            m.items_processed_total,
        );
        crate::metrics::push_counter(
            &mut out,
            "heirloom_protocol_batch_resizes_total",
            "Total adaptive batch size adjustments",
            m.resizes_total,
        );
        if let Some(avg) = m.average_latency_ms {
            crate::metrics::push_gauge(
                &mut out,
                "heirloom_protocol_batch_average_latency_ms",
                "Rolling average batch processing latency in milliseconds",
                avg as u64,
            );
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> BatchConfig {
        BatchConfig {
            min_batch_size: 5,
            max_batch_size: 100,
            initial_batch_size: 20,
            target_latency: Duration::from_millis(100),
            window: 5,
        }
    }

    #[test]
    fn test_initial_batch_size() {
        let batcher = AdaptiveBatcher::new(test_config());
        assert_eq!(batcher.current_batch_size(), 20);
    }

    #[test]
    fn test_grows_when_under_target_latency() {
        let batcher = AdaptiveBatcher::new(test_config());
        batcher.record_batch(20, Duration::from_millis(10));
        assert!(batcher.current_batch_size() > 20);
    }

    #[test]
    fn test_shrinks_when_over_target_latency() {
        let batcher = AdaptiveBatcher::new(test_config());
        batcher.record_batch(20, Duration::from_millis(500));
        assert!(batcher.current_batch_size() < 20);
    }

    #[test]
    fn test_respects_max_limit() {
        let batcher = AdaptiveBatcher::new(test_config());
        for _ in 0..50 {
            batcher.record_batch(batcher.current_batch_size(), Duration::from_millis(1));
        }
        assert!(batcher.current_batch_size() <= 100);
    }

    #[test]
    fn test_respects_min_limit() {
        let batcher = AdaptiveBatcher::new(test_config());
        for _ in 0..50 {
            batcher.record_batch(batcher.current_batch_size(), Duration::from_secs(10));
        }
        assert!(batcher.current_batch_size() >= 5);
    }

    #[test]
    fn test_metrics_tracked() {
        let batcher = AdaptiveBatcher::new(test_config());
        batcher.record_batch(20, Duration::from_millis(10));
        batcher.record_batch(25, Duration::from_millis(10));

        let metrics = batcher.metrics();
        assert_eq!(metrics.batches_processed_total, 2);
        assert_eq!(metrics.items_processed_total, 45);
        assert!(metrics.average_latency_ms.is_some());
    }
}

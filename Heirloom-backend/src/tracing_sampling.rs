/// Request tracing with sampling for Heirloom-Protocol backend.
///
/// # Overview
///
/// Full distributed tracing of every request is expensive at high throughput.
/// This module provides a configurable sampling layer that gates which requests
/// receive trace spans, with three complementary strategies:
///
/// - **Head-based sampling**: The sampling decision is made at the very start of
///   the request, before any work is done.  A random number is compared against
///   a configured rate (`0.0`–`1.0`).
/// - **Adaptive sampling**: The sample rate automatically decreases when the
///   server is under load (estimated via request-per-second tracking) so
///   tracing overhead stays bounded.
/// - **Always-on for errors**: Requests that result in 4xx/5xx responses are
///   always traced regardless of the sampling rate, ensuring errors are never
///   silently dropped from traces.
///
/// # Configuration
///
/// | Environment variable | Default | Description |
/// |---|---|---|
/// | `TRACE_SAMPLE_RATE` | `0.1` | Baseline fraction of requests to trace (0.0–1.0) |
/// | `TRACE_ADAPTIVE` | `true` | Enable adaptive rate reduction under load |
/// | `TRACE_ADAPTIVE_HIGH_RPS` | `500` | RPS threshold above which rate is halved |
/// | `TRACE_ALWAYS_ERRORS` | `true` | Always trace requests that produce errors |
/// | `TRACE_ENABLED` | `true` | Master toggle for tracing |
///
/// # Usage
///
/// ```rust,ignore
/// use heirloom_protocol_backend::tracing_sampling::{SamplingConfig, TraceSampler};
///
/// let sampler = TraceSampler::from_env();
///
/// // In a Tower middleware or axum extractor:
/// if sampler.should_sample() {
///     // attach tracing span …
/// }
/// ```
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ── Configuration ─────────────────────────────────────────────────────────────

/// Sampling strategy configuration.
#[derive(Debug, Clone)]
pub struct SamplingConfig {
    /// Baseline probability of sampling a request (0.0 = never, 1.0 = always).
    pub sample_rate: f64,
    /// Whether adaptive rate adjustment is enabled.
    pub adaptive: bool,
    /// Requests-per-second above which the effective rate is halved.
    pub adaptive_high_rps: f64,
    /// Always emit a trace for requests that produce an HTTP error response.
    pub always_trace_errors: bool,
    /// Master toggle — when `false` no requests are sampled.
    pub enabled: bool,
}

impl Default for SamplingConfig {
    fn default() -> Self {
        Self {
            sample_rate: 0.1,
            adaptive: true,
            adaptive_high_rps: 500.0,
            always_trace_errors: true,
            enabled: true,
        }
    }
}

impl SamplingConfig {
    /// Build from environment variables, falling back to defaults.
    pub fn from_env() -> Self {
        Self {
            sample_rate: std::env::var("TRACE_SAMPLE_RATE")
                .ok()
                .and_then(|v| v.parse::<f64>().ok())
                .map(|r| r.clamp(0.0, 1.0))
                .unwrap_or(0.1),
            adaptive: std::env::var("TRACE_ADAPTIVE")
                .ok()
                .map(|v| v.to_lowercase() != "false" && v != "0")
                .unwrap_or(true),
            adaptive_high_rps: std::env::var("TRACE_ADAPTIVE_HIGH_RPS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(500.0),
            always_trace_errors: std::env::var("TRACE_ALWAYS_ERRORS")
                .ok()
                .map(|v| v.to_lowercase() != "false" && v != "0")
                .unwrap_or(true),
            enabled: std::env::var("TRACE_ENABLED")
                .ok()
                .map(|v| v.to_lowercase() != "false" && v != "0")
                .unwrap_or(true),
        }
    }
}

// ── RPS tracker (for adaptive sampling) ───────────────────────────────────────

/// Sliding-window request-rate estimator using a simple 1-second bucket.
#[derive(Debug)]
struct RpsTracker {
    /// Number of requests counted in the current bucket.
    count: AtomicU64,
    /// Start of the current 1-second window.
    window_start: std::sync::Mutex<Instant>,
    /// Smoothed RPS estimate.
    smoothed_rps: std::sync::Mutex<f64>,
}

impl RpsTracker {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            count: AtomicU64::new(0),
            window_start: std::sync::Mutex::new(Instant::now()),
            smoothed_rps: std::sync::Mutex::new(0.0),
        })
    }

    /// Record one request and return the current smoothed RPS estimate.
    fn tick(&self) -> f64 {
        let now = Instant::now();
        let count = self.count.fetch_add(1, Ordering::Relaxed) + 1;

        let elapsed = {
            let start = self.window_start.lock().unwrap();
            now.duration_since(*start)
        };

        if elapsed >= Duration::from_secs(1) {
            // Bucket expired — compute rate for this window.
            let rate = count as f64 / elapsed.as_secs_f64();

            // Exponential moving average (alpha = 0.3).
            let mut smoothed = self.smoothed_rps.lock().unwrap();
            *smoothed = 0.3 * rate + 0.7 * (*smoothed);

            // Reset bucket.
            self.count.store(0, Ordering::Relaxed);
            let mut start = self.window_start.lock().unwrap();
            *start = now;

            *smoothed
        } else {
            *self.smoothed_rps.lock().unwrap()
        }
    }
}

// ── TraceSampler ──────────────────────────────────────────────────────────────

/// The main sampling engine.  Cheap to clone (inner state behind `Arc`).
#[derive(Clone, Debug)]
pub struct TraceSampler {
    config: SamplingConfig,
    rps: Arc<RpsTracker>,
    /// Total requests evaluated.
    pub total_evaluated: Arc<AtomicU64>,
    /// Total requests that were sampled (trace was started).
    pub total_sampled: Arc<AtomicU64>,
}

impl TraceSampler {
    /// Create a sampler from an explicit [`SamplingConfig`].
    pub fn new(config: SamplingConfig) -> Self {
        Self {
            config,
            rps: RpsTracker::new(),
            total_evaluated: Arc::new(AtomicU64::new(0)),
            total_sampled: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Create a sampler from environment variables.
    pub fn from_env() -> Self {
        Self::new(SamplingConfig::from_env())
    }

    /// Compute the effective sample rate, applying adaptive reduction when
    /// the server is under high load.
    pub fn effective_rate(&self) -> f64 {
        if !self.config.enabled {
            return 0.0;
        }

        let mut rate = self.config.sample_rate;

        if self.config.adaptive {
            let current_rps = *self.rps.smoothed_rps.lock().unwrap();
            if current_rps >= self.config.adaptive_high_rps {
                // Halve the rate under high load to reduce overhead.
                rate *= 0.5;
            }
        }

        rate.clamp(0.0, 1.0)
    }

    /// Head-based sampling decision for a new request.
    ///
    /// Records an RPS tick, increments counters, and returns `true` if the
    /// request should be traced.
    pub fn should_sample(&self) -> bool {
        self.rps.tick();
        self.total_evaluated.fetch_add(1, Ordering::Relaxed);

        if !self.config.enabled {
            return false;
        }

        let rate = self.effective_rate();
        let sample = pseudo_random_f64() < rate;

        if sample {
            self.total_sampled.fetch_add(1, Ordering::Relaxed);
        }

        sample
    }

    /// Force a trace for an error response, regardless of sample rate.
    ///
    /// Returns `true` if `always_trace_errors` is on (and tracing is enabled).
    pub fn should_sample_error(&self) -> bool {
        if !self.config.enabled {
            return false;
        }
        if self.config.always_trace_errors {
            self.total_sampled.fetch_add(1, Ordering::Relaxed);
            true
        } else {
            self.should_sample()
        }
    }

    /// Render sampling metrics in Prometheus text format.
    pub fn render_metrics(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();

        let evaluated = self.total_evaluated.load(Ordering::Relaxed);
        let sampled = self.total_sampled.load(Ordering::Relaxed);
        let rate = self.effective_rate();

        let _ = writeln!(
            out,
            "# HELP heirloom_trace_requests_evaluated_total Requests evaluated for sampling"
        );
        let _ = writeln!(out, "# TYPE heirloom_trace_requests_evaluated_total counter");
        let _ = writeln!(out, "heirloom_trace_requests_evaluated_total {evaluated}");

        let _ = writeln!(
            out,
            "# HELP heirloom_trace_requests_sampled_total Requests that were traced"
        );
        let _ = writeln!(out, "# TYPE heirloom_trace_requests_sampled_total counter");
        let _ = writeln!(out, "heirloom_trace_requests_sampled_total {sampled}");

        let _ = writeln!(
            out,
            "# HELP heirloom_trace_effective_sample_rate Current effective sample rate"
        );
        let _ = writeln!(out, "# TYPE heirloom_trace_effective_sample_rate gauge");
        let _ = writeln!(out, "heirloom_trace_effective_sample_rate {rate:.4}");

        out
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// A fast, thread-safe pseudo-random float in [0, 1).
///
/// Uses the current thread's stack address mixed with a nanosecond timestamp
/// as entropy.  This is intentionally *not* cryptographically secure — it just
/// needs to distribute sampling decisions pseudo-randomly.
fn pseudo_random_f64() -> f64 {
    use std::time::SystemTime;

    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);

    // Mix with thread-local stack address for additional entropy per thread.
    let addr = &nanos as *const _ as u64;
    let mixed = nanos as u64 ^ addr.wrapping_mul(0x9e37_79b9_7f4a_7c15);

    // Map to [0, 1).
    (mixed & 0x000F_FFFF_FFFF_FFFF) as f64 / (0x0010_0000_0000_0000u64 as f64)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let cfg = SamplingConfig::default();
        assert!((cfg.sample_rate - 0.1).abs() < f64::EPSILON);
        assert!(cfg.adaptive);
        assert!(cfg.always_trace_errors);
        assert!(cfg.enabled);
    }

    #[test]
    fn test_config_from_env() {
        std::env::set_var("TRACE_SAMPLE_RATE", "0.5");
        std::env::set_var("TRACE_ADAPTIVE", "false");
        std::env::set_var("TRACE_ADAPTIVE_HIGH_RPS", "1000");
        std::env::set_var("TRACE_ALWAYS_ERRORS", "false");

        let cfg = SamplingConfig::from_env();
        assert!((cfg.sample_rate - 0.5).abs() < f64::EPSILON);
        assert!(!cfg.adaptive);
        assert!((cfg.adaptive_high_rps - 1000.0).abs() < f64::EPSILON);
        assert!(!cfg.always_trace_errors);

        std::env::remove_var("TRACE_SAMPLE_RATE");
        std::env::remove_var("TRACE_ADAPTIVE");
        std::env::remove_var("TRACE_ADAPTIVE_HIGH_RPS");
        std::env::remove_var("TRACE_ALWAYS_ERRORS");
    }

    #[test]
    fn test_sample_rate_clamp() {
        std::env::set_var("TRACE_SAMPLE_RATE", "2.5");
        let cfg = SamplingConfig::from_env();
        assert!((cfg.sample_rate - 1.0).abs() < f64::EPSILON);
        std::env::remove_var("TRACE_SAMPLE_RATE");
    }

    #[test]
    fn test_disabled_never_samples() {
        let cfg = SamplingConfig {
            enabled: false,
            sample_rate: 1.0,
            ..Default::default()
        };
        let sampler = TraceSampler::new(cfg);
        for _ in 0..100 {
            assert!(!sampler.should_sample());
        }
        assert_eq!(sampler.total_sampled.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_always_sample_rate_one() {
        let cfg = SamplingConfig {
            sample_rate: 1.0,
            adaptive: false,
            enabled: true,
            ..Default::default()
        };
        let sampler = TraceSampler::new(cfg);
        for _ in 0..20 {
            assert!(sampler.should_sample());
        }
    }

    #[test]
    fn test_never_sample_rate_zero() {
        let cfg = SamplingConfig {
            sample_rate: 0.0,
            adaptive: false,
            enabled: true,
            ..Default::default()
        };
        let sampler = TraceSampler::new(cfg);
        for _ in 0..100 {
            assert!(!sampler.should_sample());
        }
    }

    #[test]
    fn test_error_always_sampled() {
        let cfg = SamplingConfig {
            sample_rate: 0.0,
            adaptive: false,
            always_trace_errors: true,
            enabled: true,
            ..Default::default()
        };
        let sampler = TraceSampler::new(cfg);
        // Even with 0% rate, errors should be sampled.
        assert!(sampler.should_sample_error());
    }

    #[test]
    fn test_evaluated_counter_increments() {
        let sampler = TraceSampler::new(SamplingConfig::default());
        for _ in 0..10 {
            sampler.should_sample();
        }
        assert_eq!(sampler.total_evaluated.load(Ordering::Relaxed), 10);
    }

    #[test]
    fn test_render_metrics_contains_keys() {
        let sampler = TraceSampler::new(SamplingConfig {
            sample_rate: 1.0,
            adaptive: false,
            ..Default::default()
        });
        sampler.should_sample();
        let out = sampler.render_metrics();
        assert!(out.contains("heirloom_trace_requests_evaluated_total"));
        assert!(out.contains("heirloom_trace_requests_sampled_total"));
        assert!(out.contains("heirloom_trace_effective_sample_rate"));
    }

    #[test]
    fn test_effective_rate_adaptive_high_rps() {
        let cfg = SamplingConfig {
            sample_rate: 0.8,
            adaptive: true,
            adaptive_high_rps: 0.0, // threshold set to 0 so it always triggers
            enabled: true,
            ..Default::default()
        };
        let sampler = TraceSampler::new(cfg);

        // Manually set smoothed RPS above threshold.
        {
            let mut rps = sampler.rps.smoothed_rps.lock().unwrap();
            *rps = 1000.0;
        }

        // With 0 threshold and 0.8 rate, effective should be halved to 0.4.
        let rate = sampler.effective_rate();
        assert!((rate - 0.4).abs() < 1e-9, "expected ~0.4, got {rate}");
    }

    #[test]
    fn test_rps_tracker_tick() {
        let tracker = RpsTracker::new();
        // Ticking increments the count.
        for _ in 0..5 {
            tracker.tick();
        }
        // The count should be at most 5 (may have rolled over if > 1 second elapsed).
        let count = tracker.count.load(Ordering::Relaxed);
        assert!(count <= 5);
    }
}

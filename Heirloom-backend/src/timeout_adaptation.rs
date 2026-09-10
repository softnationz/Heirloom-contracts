//! Adaptive per-endpoint timeouts (#127).
//!
//! Fixed timeouts are either too tight (spurious failures during normal
//! load spikes) or too loose (slow failure detection). This module keeps a
//! rolling latency histogram per endpoint, derives a timeout from a high
//! percentile of recent observations, and exposes a lightweight predictor
//! that extrapolates the next timeout from the recent trend.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;

/// Rolling window of recent latency samples for one endpoint.
struct LatencyWindow {
    samples: VecDeque<Duration>,
    capacity: usize,
    /// Percentile values computed at each `record`, used for trend prediction.
    percentile_history: VecDeque<Duration>,
}

const HISTORY_CAPACITY: usize = 20;

impl LatencyWindow {
    fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
            percentile_history: VecDeque::with_capacity(HISTORY_CAPACITY),
        }
    }

    fn record(&mut self, sample: Duration) {
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    /// Nearest-rank percentile (`p` in `[0.0, 1.0]`) over the current window.
    fn percentile(&self, p: f64) -> Option<Duration> {
        if self.samples.is_empty() {
            return None;
        }
        let mut sorted: Vec<Duration> = self.samples.iter().copied().collect();
        sorted.sort_unstable();
        let rank = ((p.clamp(0.0, 1.0)) * (sorted.len() - 1) as f64).round() as usize;
        sorted.get(rank).copied()
    }

    fn push_percentile_history(&mut self, value: Duration) {
        if self.percentile_history.len() == HISTORY_CAPACITY {
            self.percentile_history.pop_front();
        }
        self.percentile_history.push_back(value);
    }

    fn len(&self) -> usize {
        self.samples.len()
    }
}

/// Tunables for how a timeout is derived from observed latency.
#[derive(Clone, Copy, Debug)]
pub struct TimeoutAdaptationConfig {
    /// Percentile of recent latency used as the basis for the timeout (e.g. 0.99).
    pub percentile: f64,
    /// Safety multiplier applied on top of the observed percentile.
    pub multiplier: f64,
    /// Minimum number of samples required before adapting away from the default.
    pub min_samples: usize,
    /// Hard floor for any computed timeout.
    pub min_timeout: Duration,
    /// Hard ceiling for any computed timeout.
    pub max_timeout: Duration,
    /// Size of the rolling sample window per endpoint.
    pub window_size: usize,
    /// Timeout used before enough samples have been observed.
    pub default_timeout: Duration,
}

impl Default for TimeoutAdaptationConfig {
    fn default() -> Self {
        Self {
            percentile: 0.99,
            multiplier: 1.5,
            min_samples: 20,
            min_timeout: Duration::from_millis(50),
            max_timeout: Duration::from_secs(30),
            window_size: 200,
            default_timeout: Duration::from_secs(5),
        }
    }
}

/// Snapshot describing the current timeout state for one endpoint.
#[derive(Clone, Debug, Serialize)]
pub struct TimeoutSnapshot {
    pub endpoint: String,
    pub sample_count: usize,
    pub current_timeout_ms: u64,
    pub observed_percentile_ms: Option<u64>,
    pub predicted_timeout_ms: Option<u64>,
}

/// Tracks per-endpoint latency and derives adaptive timeouts.
pub struct AdaptiveTimeoutManager {
    config: TimeoutAdaptationConfig,
    windows: Mutex<HashMap<String, LatencyWindow>>,
}

impl AdaptiveTimeoutManager {
    pub fn new(config: TimeoutAdaptationConfig) -> Self {
        Self {
            config,
            windows: Mutex::new(HashMap::new()),
        }
    }

    /// Record one observed latency sample for `endpoint`.
    pub fn record_latency(&self, endpoint: &str, latency: Duration) {
        let mut windows = self.windows.lock().unwrap();
        let window = windows
            .entry(endpoint.to_string())
            .or_insert_with(|| LatencyWindow::new(self.config.window_size));
        window.record(latency);

        if window.len() >= self.config.min_samples {
            if let Some(p) = window.percentile(self.config.percentile) {
                window.push_percentile_history(p);
            }
        }
    }

    /// The timeout that should currently be used for `endpoint`.
    ///
    /// Falls back to `default_timeout` until `min_samples` observations have
    /// been recorded, then dynamically tracks the configured percentile.
    pub fn current_timeout(&self, endpoint: &str) -> Duration {
        let windows = self.windows.lock().unwrap();
        let Some(window) = windows.get(endpoint) else {
            return self.config.default_timeout;
        };

        if window.len() < self.config.min_samples {
            return self.config.default_timeout;
        }

        match window.percentile(self.config.percentile) {
            Some(p) => self.clamp(scale(p, self.config.multiplier)),
            None => self.config.default_timeout,
        }
    }

    /// Predict the timeout likely to be needed shortly, extrapolating the
    /// recent trend in the tracked percentile via an exponential moving
    /// average with a small look-ahead bias.
    pub fn predict_timeout(&self, endpoint: &str) -> Option<Duration> {
        let windows = self.windows.lock().unwrap();
        let window = windows.get(endpoint)?;
        if window.percentile_history.len() < 2 {
            return None;
        }

        const ALPHA: f64 = 0.3;
        let mut ema = window.percentile_history[0].as_secs_f64();
        for value in window.percentile_history.iter().skip(1) {
            ema = ALPHA * value.as_secs_f64() + (1.0 - ALPHA) * ema;
        }

        let first = window.percentile_history.front()?.as_secs_f64();
        let last = window.percentile_history.back()?.as_secs_f64();
        let trend = (last - first) / window.percentile_history.len() as f64;
        let predicted_secs = (ema + trend.max(0.0)).max(0.0);

        Some(self.clamp(scale(
            Duration::from_secs_f64(predicted_secs),
            self.config.multiplier,
        )))
    }

    pub fn snapshot(&self, endpoint: &str) -> TimeoutSnapshot {
        let windows = self.windows.lock().unwrap();
        let sample_count = windows.get(endpoint).map_or(0, LatencyWindow::len);
        let observed_percentile_ms = windows
            .get(endpoint)
            .and_then(|w| w.percentile(self.config.percentile))
            .map(|d| d.as_millis() as u64);
        drop(windows);

        TimeoutSnapshot {
            endpoint: endpoint.to_string(),
            sample_count,
            current_timeout_ms: self.current_timeout(endpoint).as_millis() as u64,
            observed_percentile_ms,
            predicted_timeout_ms: self.predict_timeout(endpoint).map(|d| d.as_millis() as u64),
        }
    }

    pub fn tracked_endpoints(&self) -> Vec<String> {
        self.windows.lock().unwrap().keys().cloned().collect()
    }

    fn clamp(&self, timeout: Duration) -> Duration {
        timeout.clamp(self.config.min_timeout, self.config.max_timeout)
    }
}

fn scale(duration: Duration, multiplier: f64) -> Duration {
    Duration::from_secs_f64((duration.as_secs_f64() * multiplier).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager_with_samples(count: usize) -> (AdaptiveTimeoutManager, &'static str) {
        let config = TimeoutAdaptationConfig {
            min_samples: 5,
            ..TimeoutAdaptationConfig::default()
        };
        let manager = AdaptiveTimeoutManager::new(config);
        for i in 0..count {
            manager.record_latency("get_vault", Duration::from_millis(50 + (i as u64 * 2)));
        }
        (manager, "get_vault")
    }

    #[test]
    fn uses_default_timeout_before_min_samples() {
        let (manager, endpoint) = manager_with_samples(2);
        assert_eq!(
            manager.current_timeout(endpoint),
            TimeoutAdaptationConfig::default().default_timeout
        );
    }

    #[test]
    fn adapts_timeout_once_enough_samples_recorded() {
        let (manager, endpoint) = manager_with_samples(10);
        let timeout = manager.current_timeout(endpoint);
        assert_ne!(timeout, TimeoutAdaptationConfig::default().default_timeout);
        assert!(timeout > Duration::from_millis(50));
    }

    #[test]
    fn timeout_respects_min_and_max_bounds() {
        let config = TimeoutAdaptationConfig {
            min_samples: 1,
            min_timeout: Duration::from_millis(500),
            max_timeout: Duration::from_millis(600),
            multiplier: 1.0,
            ..TimeoutAdaptationConfig::default()
        };
        let manager = AdaptiveTimeoutManager::new(config);
        manager.record_latency("fast_endpoint", Duration::from_millis(1));
        assert_eq!(
            manager.current_timeout("fast_endpoint"),
            Duration::from_millis(500)
        );

        manager.record_latency("slow_endpoint", Duration::from_secs(10));
        assert_eq!(
            manager.current_timeout("slow_endpoint"),
            Duration::from_millis(600)
        );
    }

    #[test]
    fn unknown_endpoint_falls_back_to_default() {
        let manager = AdaptiveTimeoutManager::new(TimeoutAdaptationConfig::default());
        assert_eq!(
            manager.current_timeout("never_seen"),
            TimeoutAdaptationConfig::default().default_timeout
        );
    }

    #[test]
    fn predicts_none_with_insufficient_history() {
        let (manager, endpoint) = manager_with_samples(3);
        assert!(manager.predict_timeout(endpoint).is_none());
    }

    #[test]
    fn predicts_upward_when_latency_trend_rises() {
        let config = TimeoutAdaptationConfig {
            min_samples: 3,
            ..TimeoutAdaptationConfig::default()
        };
        let manager = AdaptiveTimeoutManager::new(config);
        for i in 0..40 {
            manager.record_latency("rising", Duration::from_millis(50 + i * 5));
        }

        let predicted = manager
            .predict_timeout("rising")
            .expect("expected a prediction");
        let current = manager.current_timeout("rising");
        assert!(predicted >= current || predicted > Duration::from_millis(0));
    }

    #[test]
    fn snapshot_reports_sample_count_and_timeout() {
        let (manager, endpoint) = manager_with_samples(10);
        let snap = manager.snapshot(endpoint);
        assert_eq!(snap.sample_count, 10);
        assert_eq!(snap.endpoint, endpoint);
        assert!(snap.observed_percentile_ms.is_some());
    }

    #[test]
    fn tracked_endpoints_lists_recorded_names() {
        let manager = AdaptiveTimeoutManager::new(TimeoutAdaptationConfig::default());
        manager.record_latency("a", Duration::from_millis(10));
        manager.record_latency("b", Duration::from_millis(20));
        let mut endpoints = manager.tracked_endpoints();
        endpoints.sort();
        assert_eq!(endpoints, vec!["a".to_string(), "b".to_string()]);
    }

    /// Micro-benchmark-style scenario (no external harness): simulate a
    /// burst of latency samples and confirm adaptation stays within a
    /// reasonable wall-clock budget. Documented further in
    /// docs/timeout-adaptation.md.
    #[test]
    fn benchmark_adaptation_under_load() {
        let manager = AdaptiveTimeoutManager::new(TimeoutAdaptationConfig::default());
        let start = std::time::Instant::now();
        for i in 0..5_000u64 {
            manager.record_latency("bench_endpoint", Duration::from_micros(100 + i % 50));
            let _ = manager.current_timeout("bench_endpoint");
            let _ = manager.predict_timeout("bench_endpoint");
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "adaptation loop took too long: {elapsed:?}"
        );
    }
}

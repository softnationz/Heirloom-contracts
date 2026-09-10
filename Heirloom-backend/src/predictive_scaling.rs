//! Predictive scaling (#130).
//!
//! Reactive autoscaling only adds capacity after load has already spiked,
//! so users feel the lag while new capacity comes online. `PredictiveScaler`
//! keeps a rolling history of traffic samples, forecasts near-term demand
//! with Holt's double exponential smoothing (level + trend), and
//! recommends a replica count ahead of time via an `AutoscalerClient` so
//! capacity can be pre-provisioned. See `docs/predictive-scaling.md`.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use std::collections::VecDeque;
use std::sync::Arc;

/// One traffic observation: requests observed during a fixed sampling
/// interval, tagged with when the sample was taken.
#[derive(Debug, Clone, Copy)]
pub struct TrafficSample {
    pub timestamp_secs: u64,
    pub requests: u64,
}

/// Rolling window of historical traffic samples used for forecasting.
pub struct TrafficHistory {
    samples: Mutex<VecDeque<TrafficSample>>,
    capacity: usize,
}

impl TrafficHistory {
    pub fn new(capacity: usize) -> Self {
        Self {
            samples: Mutex::new(VecDeque::with_capacity(capacity.max(1))),
            capacity: capacity.max(1),
        }
    }

    pub fn record(&self, sample: TrafficSample) {
        let mut samples = self.samples.lock().unwrap();
        if samples.len() == self.capacity {
            samples.pop_front();
        }
        samples.push_back(sample);
    }

    pub fn samples(&self) -> Vec<TrafficSample> {
        self.samples.lock().unwrap().iter().copied().collect()
    }

    pub fn len(&self) -> usize {
        self.samples.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Holt's double exponential smoothing: tracks a smoothed level and trend
/// so short-term forecasts account for traffic that is actively rising or
/// falling, not just its most recent value.
#[derive(Debug, Clone, Copy)]
pub struct ForecastModel {
    /// Smoothing factor for the level component, in `[0.0, 1.0]`.
    pub alpha: f64,
    /// Smoothing factor for the trend component, in `[0.0, 1.0]`.
    pub beta: f64,
}

impl Default for ForecastModel {
    fn default() -> Self {
        Self {
            alpha: 0.4,
            beta: 0.3,
        }
    }
}

impl ForecastModel {
    /// Fit level/trend over `samples` (oldest first) and forecast demand
    /// `periods_ahead` sampling intervals into the future. `None` if there
    /// is no history yet.
    pub fn forecast(&self, samples: &[TrafficSample], periods_ahead: u32) -> Option<f64> {
        if samples.is_empty() {
            return None;
        }
        if samples.len() == 1 {
            return Some(samples[0].requests as f64);
        }

        let mut level = samples[0].requests as f64;
        let mut trend = (samples[1].requests as f64) - (samples[0].requests as f64);

        for sample in &samples[1..] {
            let value = sample.requests as f64;
            let last_level = level;
            level = self.alpha * value + (1.0 - self.alpha) * (level + trend);
            trend = self.beta * (level - last_level) + (1.0 - self.beta) * trend;
        }

        Some((level + trend * periods_ahead as f64).max(0.0))
    }
}

/// Bounds and target throughput-per-replica used to translate a forecast
/// into a recommended replica count.
#[derive(Debug, Clone, Copy)]
pub struct ScalingConfig {
    pub min_replicas: u32,
    pub max_replicas: u32,
    /// Requests per sampling interval a single replica can sustain.
    pub requests_per_replica: f64,
    pub forecast_periods_ahead: u32,
}

impl Default for ScalingConfig {
    fn default() -> Self {
        Self {
            min_replicas: 2,
            max_replicas: 50,
            requests_per_replica: 100.0,
            forecast_periods_ahead: 3,
        }
    }
}

impl ScalingConfig {
    /// Build from `SCALING_MIN_REPLICAS`, `SCALING_MAX_REPLICAS`,
    /// `SCALING_REQUESTS_PER_REPLICA` and
    /// `SCALING_FORECAST_PERIODS_AHEAD` environment variables, falling
    /// back to defaults when unset or unparsable.
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            min_replicas: env_u32("SCALING_MIN_REPLICAS", defaults.min_replicas),
            max_replicas: env_u32("SCALING_MAX_REPLICAS", defaults.max_replicas),
            requests_per_replica: env_f64(
                "SCALING_REQUESTS_PER_REPLICA",
                defaults.requests_per_replica,
            ),
            forecast_periods_ahead: env_u32(
                "SCALING_FORECAST_PERIODS_AHEAD",
                defaults.forecast_periods_ahead,
            ),
        }
    }
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Implemented by whatever actually talks to the autoscaling platform
/// (Kubernetes HPA, ECS service scaling, etc). `PredictiveScaler` calls
/// this with its recommendation; production wiring supplies a real
/// client, tests/dev use a stub.
pub trait AutoscalerClient: Send + Sync {
    fn set_desired_replicas(&self, replicas: u32);
}

/// Default autoscaler client that logs the recommendation. Swap for a real
/// Kubernetes/ECS client when integrating with a live autoscaling
/// platform.
pub struct LoggingAutoscalerClient;

impl AutoscalerClient for LoggingAutoscalerClient {
    fn set_desired_replicas(&self, replicas: u32) {
        tracing::info!(replicas, "predictive scaling recommendation");
    }
}

/// Point-in-time predictive scaling metrics.
pub struct ScalingMetrics {
    pub recommended_replicas: u32,
    pub forecast_requests: f64,
    pub history_samples: usize,
    pub scaling_decisions_total: u64,
}

/// Combines `TrafficHistory`, `ForecastModel` and `ScalingConfig` to
/// periodically recommend, and apply via `AutoscalerClient`, a replica
/// count ahead of anticipated demand.
pub struct PredictiveScaler {
    pub history: TrafficHistory,
    model: ForecastModel,
    config: ScalingConfig,
    autoscaler: Box<dyn AutoscalerClient>,
    last_recommended_replicas: AtomicU32,
    last_forecast_requests: Mutex<f64>,
    scaling_decisions_total: AtomicU64,
}

impl PredictiveScaler {
    pub fn new(
        history_capacity: usize,
        model: ForecastModel,
        config: ScalingConfig,
        autoscaler: Box<dyn AutoscalerClient>,
    ) -> Self {
        Self {
            history: TrafficHistory::new(history_capacity),
            model,
            last_recommended_replicas: AtomicU32::new(config.min_replicas),
            config,
            autoscaler,
            last_forecast_requests: Mutex::new(0.0),
            scaling_decisions_total: AtomicU64::new(0),
        }
    }

    pub fn record_traffic(&self, sample: TrafficSample) {
        self.history.record(sample);
    }

    /// Recompute the forecast from historical samples and, if it changes
    /// the recommended replica count, apply it via the `AutoscalerClient`.
    /// Returns the (possibly unchanged) recommendation, or `None` if there
    /// is no traffic history yet.
    pub fn evaluate(&self) -> Option<u32> {
        let samples = self.history.samples();
        let forecast = self
            .model
            .forecast(&samples, self.config.forecast_periods_ahead)?;

        *self.last_forecast_requests.lock().unwrap() = forecast;

        let raw_replicas = (forecast / self.config.requests_per_replica).ceil() as u32;
        let recommended = raw_replicas.clamp(self.config.min_replicas, self.config.max_replicas);

        let previous = self
            .last_recommended_replicas
            .swap(recommended, Ordering::Relaxed);
        if previous != recommended {
            self.scaling_decisions_total.fetch_add(1, Ordering::Relaxed);
            self.autoscaler.set_desired_replicas(recommended);
        }

        Some(recommended)
    }

    pub fn metrics(&self) -> ScalingMetrics {
        ScalingMetrics {
            recommended_replicas: self.last_recommended_replicas.load(Ordering::Relaxed),
            forecast_requests: *self.last_forecast_requests.lock().unwrap(),
            history_samples: self.history.len(),
            scaling_decisions_total: self.scaling_decisions_total.load(Ordering::Relaxed),
        }
    }

    pub fn render_prometheus(&self) -> String {
        let m = self.metrics();
        let mut out = String::new();
        crate::metrics::push_gauge(
            &mut out,
            "heirloom_protocol_scaling_recommended_replicas",
            "Predictively recommended replica count",
            m.recommended_replicas as u64,
        );
        crate::metrics::push_gauge(
            &mut out,
            "heirloom_protocol_scaling_forecast_requests",
            "Forecasted request volume for the next sampling period",
            m.forecast_requests.max(0.0) as u64,
        );
        crate::metrics::push_counter(
            &mut out,
            "heirloom_protocol_scaling_decisions_total",
            "Total replica count changes recommended",
            m.scaling_decisions_total,
        );
        out
    }
}

/// Periodically samples request volume from `metrics` (using the delta of
/// `http_requests_total` over each interval as one `TrafficSample`),
/// records it into `scaler`'s history, and re-evaluates the scaling
/// recommendation. Integrates the forecasting model with the autoscaling
/// platform via `PredictiveScaler::evaluate`.
pub async fn run(
    scaler: Arc<PredictiveScaler>,
    metrics: Arc<crate::metrics::Metrics>,
    sample_interval: Duration,
) {
    let mut interval = tokio::time::interval(sample_interval);
    let mut last_total = metrics.http_requests_total.load(Ordering::Relaxed);
    let mut elapsed_secs: u64 = 0;

    loop {
        interval.tick().await;
        elapsed_secs = elapsed_secs.saturating_add(sample_interval.as_secs().max(1));

        let current_total = metrics.http_requests_total.load(Ordering::Relaxed);
        let requests_this_period = current_total.saturating_sub(last_total);
        last_total = current_total;

        scaler.record_traffic(TrafficSample {
            timestamp_secs: elapsed_secs,
            requests: requests_this_period,
        });

        if let Some(replicas) = scaler.evaluate() {
            tracing::debug!(
                replicas,
                requests_this_period,
                "predictive scaling evaluated"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32 as StdAtomicU32;

    fn sample(t: u64, requests: u64) -> TrafficSample {
        TrafficSample {
            timestamp_secs: t,
            requests,
        }
    }

    #[test]
    fn test_forecast_empty_history() {
        let model = ForecastModel::default();
        assert_eq!(model.forecast(&[], 3), None);
    }

    #[test]
    fn test_forecast_single_sample() {
        let model = ForecastModel::default();
        assert_eq!(model.forecast(&[sample(0, 100)], 3), Some(100.0));
    }

    #[test]
    fn test_forecast_rising_trend_extrapolates_up() {
        let model = ForecastModel::default();
        let samples: Vec<_> = (0..10).map(|i| sample(i, 100 + i * 20)).collect();
        let forecast = model.forecast(&samples, 3).unwrap();
        // Traffic is climbing ~20/period; a forecast 3 periods out should
        // clear the last observed value.
        assert!(forecast > 280.0);
    }

    #[test]
    fn test_forecast_flat_traffic_stays_flat() {
        let model = ForecastModel::default();
        let samples: Vec<_> = (0..10).map(|i| sample(i, 100)).collect();
        let forecast = model.forecast(&samples, 5).unwrap();
        assert!((forecast - 100.0).abs() < 1.0);
    }

    struct RecordingAutoscaler {
        last: StdAtomicU32,
    }

    impl AutoscalerClient for RecordingAutoscaler {
        fn set_desired_replicas(&self, replicas: u32) {
            self.last.store(replicas, Ordering::Relaxed);
        }
    }

    #[test]
    fn test_scaler_recommends_within_bounds() {
        let config = ScalingConfig {
            min_replicas: 2,
            max_replicas: 10,
            requests_per_replica: 50.0,
            forecast_periods_ahead: 1,
        };
        let scaler = PredictiveScaler::new(
            20,
            ForecastModel::default(),
            config,
            Box::new(LoggingAutoscalerClient),
        );

        for i in 0..5 {
            scaler.record_traffic(sample(i, 1000));
        }

        let recommended = scaler.evaluate().unwrap();
        assert!(recommended >= 2 && recommended <= 10);
    }

    #[test]
    fn test_scaler_applies_via_autoscaler_client_on_change() {
        let config = ScalingConfig {
            min_replicas: 1,
            max_replicas: 100,
            requests_per_replica: 10.0,
            forecast_periods_ahead: 1,
        };
        let recorder = Arc::new(RecordingAutoscaler {
            last: StdAtomicU32::new(0),
        });

        struct Forwarding(Arc<RecordingAutoscaler>);
        impl AutoscalerClient for Forwarding {
            fn set_desired_replicas(&self, replicas: u32) {
                self.0.set_desired_replicas(replicas);
            }
        }

        let scaler = PredictiveScaler::new(
            20,
            ForecastModel::default(),
            config,
            Box::new(Forwarding(Arc::clone(&recorder))),
        );

        scaler.record_traffic(sample(0, 500));
        scaler.record_traffic(sample(1, 500));
        scaler.evaluate();

        assert!(recorder.last.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn test_no_evaluation_without_history() {
        let scaler = PredictiveScaler::new(
            10,
            ForecastModel::default(),
            ScalingConfig::default(),
            Box::new(LoggingAutoscalerClient),
        );
        assert_eq!(scaler.evaluate(), None);
    }
}

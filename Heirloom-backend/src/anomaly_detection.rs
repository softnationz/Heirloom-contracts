//! Automatic anomaly detection and alerting (issue: "Anomalies aren't
//! detected automatically. Detection would enable proactive alerting.").
//!
//! Uses an online (streaming) z-score detector: a running mean/variance
//! ("baseline") is learned per metric via Welford's algorithm, and any new
//! observation more than `z_threshold` standard deviations from that
//! baseline is flagged. Baselines still update on anomalous points so the
//! detector adapts to genuine regime shifts rather than getting stuck
//! alerting forever. False positives are filtered by (a) requiring a
//! minimum number of samples before a baseline is trusted and (b) a
//! per-metric cooldown so a single spike doesn't fire dozens of duplicate
//! alerts.
//!
//! See `docs/anomaly-detection.md` for tuning guidance.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Minimum number of observations before a baseline is trusted enough to
/// generate alerts. Below this, the detector is still "learning".
const MIN_SAMPLES_FOR_DETECTION: u64 = 5;

/// Number of standard deviations from the mean that counts as anomalous.
const DEFAULT_Z_THRESHOLD: f64 = 3.0;

/// Minimum time between two alerts for the same metric, to suppress
/// duplicate/false-positive alert storms from a single sustained anomaly.
const ALERT_COOLDOWN_SECONDS: i64 = 60;

/// Running (Welford) baseline statistics for one metric.
#[derive(Debug, Clone, Serialize)]
pub struct Baseline {
    pub count: u64,
    pub mean: f64,
    #[serde(skip)]
    m2: f64,
    pub last_updated: DateTime<Utc>,
}

impl Default for Baseline {
    fn default() -> Self {
        Self {
            count: 0,
            mean: 0.0,
            m2: 0.0,
            last_updated: Utc::now(),
        }
    }
}

impl Baseline {
    fn std_dev(&self) -> f64 {
        if self.count < 2 {
            return 0.0;
        }
        (self.m2 / (self.count - 1) as f64).sqrt()
    }

    /// Welford's online update. Returns the z-score of `value` against the
    /// baseline as it was *before* this update, so the current point is
    /// judged against history, not against itself.
    fn observe(&mut self, value: f64) -> f64 {
        let std_dev_before = self.std_dev();
        let mean_before = self.mean;
        let z = if self.count >= MIN_SAMPLES_FOR_DETECTION && std_dev_before > f64::EPSILON {
            (value - mean_before) / std_dev_before
        } else {
            0.0
        };

        self.count += 1;
        let delta = value - self.mean;
        self.mean += delta / self.count as f64;
        let delta2 = value - self.mean;
        self.m2 += delta * delta2;
        self.last_updated = Utc::now();

        z
    }
}

/// Severity bucket derived from how far outside the threshold a point was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warning,
    Critical,
}

/// A generated anomaly alert.
#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub id: String,
    pub metric: String,
    pub value: f64,
    pub baseline_mean: f64,
    pub baseline_std_dev: f64,
    pub z_score: f64,
    pub severity: Severity,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct ObserveRequest {
    pub metric: String,
    pub value: f64,
}

#[derive(Default)]
struct Inner {
    baselines: HashMap<String, Baseline>,
    alerts: Vec<Alert>,
    last_alert_at: HashMap<String, DateTime<Utc>>,
}

/// Shared anomaly-detection state: one baseline per metric plus the
/// generated alert history.
#[derive(Default)]
pub struct AnomalyStore {
    inner: RwLock<Inner>,
}

impl AnomalyStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Feed a new observation for `metric`. Updates the learned baseline and
    /// returns `Some(Alert)` if this point was anomalous and not suppressed
    /// by the cooldown filter.
    pub fn observe(&self, metric: &str, value: f64) -> Option<Alert> {
        let mut inner = self.inner.write().expect("anomaly lock poisoned");

        let baseline = inner.baselines.entry(metric.to_string()).or_default();
        let mean_before = baseline.mean;
        let std_dev_before = baseline.std_dev();
        let count_before = baseline.count;
        let z = baseline.observe(value);

        if count_before < MIN_SAMPLES_FOR_DETECTION || z.abs() < DEFAULT_Z_THRESHOLD {
            return None;
        }

        let now = Utc::now();
        if let Some(last) = inner.last_alert_at.get(metric) {
            if now - *last < Duration::seconds(ALERT_COOLDOWN_SECONDS) {
                return None; // false-positive filtering: still in cooldown
            }
        }

        let severity = if z.abs() >= DEFAULT_Z_THRESHOLD * 2.0 {
            Severity::Critical
        } else {
            Severity::Warning
        };

        let alert = Alert {
            id: Uuid::new_v4().to_string(),
            metric: metric.to_string(),
            value,
            baseline_mean: mean_before,
            baseline_std_dev: std_dev_before,
            z_score: z,
            severity,
            timestamp: now,
        };

        inner.last_alert_at.insert(metric.to_string(), now);
        inner.alerts.push(alert.clone());
        Some(alert)
    }

    pub fn alerts(&self) -> Vec<Alert> {
        self.inner
            .read()
            .expect("anomaly lock poisoned")
            .alerts
            .clone()
    }

    pub fn baseline(&self, metric: &str) -> Option<Baseline> {
        self.inner
            .read()
            .expect("anomaly lock poisoned")
            .baselines
            .get(metric)
            .cloned()
    }
}

/// `POST /anomaly/observe` - feed a metric observation into the detector.
/// Returns the generated alert, if any, as `{"alert": ...}` or
/// `{"alert": null}` when the point was normal or suppressed.
pub async fn observe_metric(
    State(store): State<Arc<AnomalyStore>>,
    Json(req): Json<ObserveRequest>,
) -> impl IntoResponse {
    let alert = store.observe(&req.metric, req.value);
    Json(serde_json::json!({ "alert": alert }))
}

/// `GET /anomaly/alerts` - list all alerts generated so far.
pub async fn list_alerts(State(store): State<Arc<AnomalyStore>>) -> impl IntoResponse {
    Json(store.alerts())
}

/// `GET /anomaly/baseline/:metric` - inspect the learned baseline for a metric.
pub async fn get_baseline(
    State(store): State<Arc<AnomalyStore>>,
    Path(metric): Path<String>,
) -> impl IntoResponse {
    match store.baseline(&metric) {
        Some(baseline) => (StatusCode::OK, Json(baseline)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_alert_while_learning_baseline() {
        let store = AnomalyStore::default();
        for v in [10.0, 11.0, 9.0, 10.5] {
            assert!(store.observe("cpu_pct", v).is_none());
        }
    }

    #[test]
    fn spike_after_stable_baseline_triggers_alert() {
        let store = AnomalyStore::default();
        for v in [10.0, 10.1, 9.9, 10.0, 10.05, 9.95, 10.0] {
            store.observe("cpu_pct", v);
        }
        let alert = store.observe("cpu_pct", 500.0);
        assert!(alert.is_some(), "large spike should be flagged");
        assert_eq!(alert.unwrap().metric, "cpu_pct");
    }

    #[test]
    fn cooldown_suppresses_duplicate_alerts() {
        let store = AnomalyStore::default();
        for v in [10.0, 10.1, 9.9, 10.0, 10.05, 9.95, 10.0] {
            store.observe("cpu_pct", v);
        }
        let first = store.observe("cpu_pct", 500.0);
        let second = store.observe("cpu_pct", 501.0);
        assert!(first.is_some());
        assert!(
            second.is_none(),
            "second spike within cooldown should be suppressed"
        );
    }

    #[test]
    fn stable_values_never_alert() {
        let store = AnomalyStore::default();
        for _ in 0..50 {
            assert!(store.observe("steady_metric", 42.0).is_none());
        }
    }
}

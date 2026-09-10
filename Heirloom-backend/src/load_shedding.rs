//! Load shedding (#128).
//!
//! Under overload, continuing to accept every request causes cascading
//! failures: queues grow, latency climbs, clients retry, and retries add
//! even more load. `LoadShedder` watches a live load signal (in-flight
//! request count) and adaptively rejects incoming traffic once configured
//! thresholds are crossed, shedding lower `Priority` (#129) requests first
//! and always admitting `Priority::Critical` traffic. See
//! `docs/load-shedding.md`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};

use crate::db::AppState;
use crate::priority::Priority;

/// Tracks live load signals used to decide whether to shed requests.
#[derive(Default)]
pub struct LoadMonitor {
    inflight: AtomicU64,
    accepted_total: AtomicU64,
    rejected_total: AtomicU64,
}

impl LoadMonitor {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn inflight(&self) -> u64 {
        self.inflight.load(Ordering::Relaxed)
    }

    /// Mark a request as started; the returned guard decrements the
    /// in-flight count when dropped.
    pub fn begin_request(&self) -> InflightGuard<'_> {
        self.inflight.fetch_add(1, Ordering::AcqRel);
        InflightGuard { monitor: self }
    }

    pub fn record_accepted(&self) {
        self.accepted_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_rejected(&self) {
        self.rejected_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn accepted_total(&self) -> u64 {
        self.accepted_total.load(Ordering::Relaxed)
    }

    pub fn rejected_total(&self) -> u64 {
        self.rejected_total.load(Ordering::Relaxed)
    }

    /// Fraction of lifetime-evaluated requests that were rejected, a
    /// coarse `[0.0, 1.0]` load indicator.
    pub fn rejection_rate(&self) -> f64 {
        let accepted = self.accepted_total() as f64;
        let rejected = self.rejected_total() as f64;
        let total = accepted + rejected;
        if total == 0.0 {
            0.0
        } else {
            rejected / total
        }
    }
}

pub struct InflightGuard<'a> {
    monitor: &'a LoadMonitor,
}

impl Drop for InflightGuard<'_> {
    fn drop(&mut self) {
        self.monitor.inflight.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A load level: once in-flight requests reach `inflight_threshold`, every
/// request at `shed_at_or_below` priority or lower is rejected.
#[derive(Debug, Clone, Copy)]
pub struct SheddingThreshold {
    pub inflight_threshold: u64,
    pub shed_at_or_below: Priority,
}

/// Adaptive shedding thresholds, ordered from least to most severe.
#[derive(Debug, Clone)]
pub struct SheddingConfig {
    pub thresholds: Vec<SheddingThreshold>,
}

impl Default for SheddingConfig {
    fn default() -> Self {
        Self {
            thresholds: vec![
                SheddingThreshold {
                    inflight_threshold: 300,
                    shed_at_or_below: Priority::Low,
                },
                SheddingThreshold {
                    inflight_threshold: 600,
                    shed_at_or_below: Priority::Normal,
                },
                SheddingThreshold {
                    inflight_threshold: 900,
                    shed_at_or_below: Priority::High,
                },
            ],
        }
    }
}

impl SheddingConfig {
    /// Build from `LOAD_SHED_THRESHOLD_<LOW|NORMAL|HIGH>` environment
    /// variables (the in-flight count at which that priority tier and
    /// below starts getting shed), falling back to defaults when unset or
    /// unparsable.
    pub fn from_env() -> Self {
        let defaults = Self::default();
        let low = env_u64(
            "LOAD_SHED_THRESHOLD_LOW",
            defaults.thresholds[0].inflight_threshold,
        );
        let normal = env_u64(
            "LOAD_SHED_THRESHOLD_NORMAL",
            defaults.thresholds[1].inflight_threshold,
        );
        let high = env_u64(
            "LOAD_SHED_THRESHOLD_HIGH",
            defaults.thresholds[2].inflight_threshold,
        );
        Self {
            thresholds: vec![
                SheddingThreshold {
                    inflight_threshold: low,
                    shed_at_or_below: Priority::Low,
                },
                SheddingThreshold {
                    inflight_threshold: normal,
                    shed_at_or_below: Priority::Normal,
                },
                SheddingThreshold {
                    inflight_threshold: high,
                    shed_at_or_below: Priority::High,
                },
            ],
        }
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Point-in-time load shedding metrics.
pub struct SheddingMetrics {
    pub inflight: u64,
    pub accepted_total: u64,
    pub rejected_total: u64,
    pub shed_total: u64,
    pub rejection_rate: f64,
}

/// Adaptively decides whether an incoming request should be shed, based on
/// current load (`LoadMonitor`) and configured thresholds
/// (`SheddingConfig`).
pub struct LoadShedder {
    pub monitor: Arc<LoadMonitor>,
    config: SheddingConfig,
    shed_total: AtomicU64,
}

impl LoadShedder {
    pub fn new(monitor: Arc<LoadMonitor>, config: SheddingConfig) -> Self {
        Self {
            monitor,
            config,
            shed_total: AtomicU64::new(0),
        }
    }

    /// The most severe active shed ceiling: everything at or below this
    /// priority is currently being shed. `None` if load is under every
    /// threshold.
    fn active_shed_ceiling(&self) -> Option<Priority> {
        let inflight = self.monitor.inflight();
        self.config
            .thresholds
            .iter()
            .filter(|t| inflight >= t.inflight_threshold)
            .map(|t| t.shed_at_or_below)
            .max()
    }

    /// Decide whether a request at `priority` should be rejected right
    /// now. `Priority::Critical` is never shed. Recording (shed/rejected
    /// counters) happens as a side effect of a `true` result.
    pub fn should_shed(&self, priority: Priority) -> bool {
        if priority == Priority::Critical {
            return false;
        }
        let Some(ceiling) = self.active_shed_ceiling() else {
            return false;
        };
        let shed = priority <= ceiling;
        if shed {
            self.shed_total.fetch_add(1, Ordering::Relaxed);
            self.monitor.record_rejected();
        }
        shed
    }

    pub fn metrics(&self) -> SheddingMetrics {
        SheddingMetrics {
            inflight: self.monitor.inflight(),
            accepted_total: self.monitor.accepted_total(),
            rejected_total: self.monitor.rejected_total(),
            shed_total: self.shed_total.load(Ordering::Relaxed),
            rejection_rate: self.monitor.rejection_rate(),
        }
    }

    pub fn render_prometheus(&self) -> String {
        let m = self.metrics();
        let mut out = String::new();
        crate::metrics::push_gauge(
            &mut out,
            "heirloom_protocol_load_shedding_inflight",
            "Current in-flight request count",
            m.inflight,
        );
        crate::metrics::push_counter(
            &mut out,
            "heirloom_protocol_load_shedding_accepted_total",
            "Total requests admitted past load shedding",
            m.accepted_total,
        );
        crate::metrics::push_counter(
            &mut out,
            "heirloom_protocol_load_shedding_rejected_total",
            "Total requests rejected by load shedding",
            m.rejected_total,
        );
        crate::metrics::push_counter(
            &mut out,
            "heirloom_protocol_load_shedding_shed_total",
            "Total requests shed due to priority-based shedding",
            m.shed_total,
        );
        out
    }
}

/// Axum middleware layered over the whole router: tracks in-flight load,
/// applies adaptive priority-based load shedding (#128), then enforces
/// per-priority concurrency limits (#129) before letting the request
/// through to its handler.
pub async fn admission_middleware(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    state
        .metrics
        .http_requests_total
        .fetch_add(1, Ordering::Relaxed);

    let priority = Priority::from_headers(request.headers());
    let _inflight_guard = state.load_shedder.monitor.begin_request();

    if state.load_shedder.should_shed(priority) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "code": "load_shed",
                "message": "request shed due to high load",
                "priority": priority.as_str(),
            })),
        )
            .into_response();
    }
    state.load_shedder.monitor.record_accepted();

    let Some(_permit) =
        crate::priority::PriorityEnforcer::try_acquire(&state.priority_enforcer, priority)
    else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "code": "priority_limit_exceeded",
                "message": "priority concurrency limit exceeded",
                "priority": priority.as_str(),
            })),
        )
            .into_response();
    };

    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shedder_with_thresholds() -> LoadShedder {
        LoadShedder::new(LoadMonitor::new(), SheddingConfig::default())
    }

    #[test]
    fn test_no_shedding_under_threshold() {
        let shedder = shedder_with_thresholds();
        assert!(!shedder.should_shed(Priority::Low));
    }

    #[test]
    fn test_sheds_low_priority_over_threshold() {
        let shedder = shedder_with_thresholds();
        let mut guards = Vec::new();
        for _ in 0..300 {
            guards.push(shedder.monitor.begin_request());
        }
        assert!(shedder.should_shed(Priority::Low));
        assert!(!shedder.should_shed(Priority::High));
    }

    #[test]
    fn test_critical_never_shed() {
        let shedder = shedder_with_thresholds();
        let mut guards = Vec::new();
        for _ in 0..1000 {
            guards.push(shedder.monitor.begin_request());
        }
        assert!(!shedder.should_shed(Priority::Critical));
        assert!(shedder.should_shed(Priority::High));
        assert!(shedder.should_shed(Priority::Normal));
        assert!(shedder.should_shed(Priority::Low));
    }

    #[test]
    fn test_shed_total_and_metrics_increment() {
        let shedder = shedder_with_thresholds();
        let mut guards = Vec::new();
        for _ in 0..300 {
            guards.push(shedder.monitor.begin_request());
        }
        assert!(shedder.should_shed(Priority::Low));
        assert!(shedder.should_shed(Priority::Low));

        let metrics = shedder.metrics();
        assert_eq!(metrics.shed_total, 2);
        assert_eq!(metrics.rejected_total, 2);
        assert_eq!(metrics.inflight, 300);
    }

    #[test]
    fn test_inflight_guard_decrements_on_drop() {
        let monitor = LoadMonitor::new();
        {
            let _guard = monitor.begin_request();
            assert_eq!(monitor.inflight(), 1);
        }
        assert_eq!(monitor.inflight(), 0);
    }

    #[test]
    fn test_rejection_rate() {
        let monitor = LoadMonitor::new();
        assert_eq!(monitor.rejection_rate(), 0.0);
        monitor.record_accepted();
        monitor.record_accepted();
        monitor.record_accepted();
        monitor.record_rejected();
        assert!((monitor.rejection_rate() - 0.25).abs() < f64::EPSILON);
    }
}

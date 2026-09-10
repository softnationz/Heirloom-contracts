//! Continuous performance profiling.
//!
//! Manual investigation of performance bottlenecks doesn't scale. This
//! module provides always-on, low-overhead profiling so regressions are
//! caught automatically instead of relying on someone noticing slowness.
//!
//! # Components
//!
//! - [`ProfilerState`] — in-memory ring buffer of recorded samples
//! - [`profile_operation`] — wraps an async operation, recording its stack
//!   label and duration (continuous profiling hook)
//! - [`generate_flamegraph`] — aggregates samples into the standard
//!   "folded stack" format consumed by flamegraph renderers
//!   (e.g. `inferno-flamegraph`, Brendan Gregg's `flamegraph.pl`)
//! - [`detect_regressions`] — compares recent sample averages per operation
//!   against a recorded baseline and flags operations that got slower by
//!   more than a configurable threshold
//!
//! # API
//!
//! - `GET /admin/profiler/samples` — recent raw samples
//! - `GET /admin/profiler/flamegraph` — folded-stack flame graph data
//! - `POST /admin/profiler/baseline` — record current averages as baseline
//! - `GET /admin/profiler/regressions` — operations that regressed vs baseline

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::{extract::State, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Maximum number of samples retained before the oldest are evicted.
const MAX_SAMPLES: usize = 5_000;

/// Default threshold (%) above baseline before an operation is flagged as
/// a performance regression.
const DEFAULT_REGRESSION_THRESHOLD_PCT: f64 = 20.0;

/// A single recorded profiling sample.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSample {
    pub operation: String,
    /// Call-stack labels, root-first (e.g. `["handler", "db", "query"]`).
    pub stack: Vec<String>,
    pub duration_ms: f64,
    pub recorded_at: DateTime<Utc>,
}

/// Aggregated performance regression finding.
#[derive(Debug, Clone, Serialize)]
pub struct RegressionFinding {
    pub operation: String,
    pub baseline_avg_ms: f64,
    pub current_avg_ms: f64,
    pub percent_change: f64,
    pub sample_count: usize,
}

/// Continuous profiling state shared across the application.
pub struct ProfilerState {
    samples: Mutex<Vec<ProfileSample>>,
    /// Recorded baseline average duration (ms) per operation.
    baseline: Mutex<HashMap<String, f64>>,
}

impl ProfilerState {
    pub fn new() -> Self {
        Self {
            samples: Mutex::new(Vec::new()),
            baseline: Mutex::new(HashMap::new()),
        }
    }

    /// Record a completed profiling sample, evicting the oldest sample if
    /// the ring buffer is full.
    pub fn record(&self, sample: ProfileSample) {
        let mut samples = self.samples.lock().unwrap();
        if samples.len() >= MAX_SAMPLES {
            samples.remove(0);
        }
        samples.push(sample);
    }

    pub fn snapshot(&self) -> Vec<ProfileSample> {
        self.samples.lock().unwrap().clone()
    }

    /// Compute the mean duration per operation across all retained samples.
    pub fn averages(&self) -> HashMap<String, f64> {
        let samples = self.samples.lock().unwrap();
        let mut sums: HashMap<String, (f64, usize)> = HashMap::new();
        for s in samples.iter() {
            let entry = sums.entry(s.operation.clone()).or_insert((0.0, 0));
            entry.0 += s.duration_ms;
            entry.1 += 1;
        }
        sums.into_iter()
            .map(|(op, (sum, count))| (op, sum / count as f64))
            .collect()
    }

    /// Snapshot current per-operation averages as the new baseline.
    pub fn set_baseline_from_current(&self) -> HashMap<String, f64> {
        let averages = self.averages();
        let mut baseline = self.baseline.lock().unwrap();
        *baseline = averages.clone();
        averages
    }

    pub fn baseline_snapshot(&self) -> HashMap<String, f64> {
        self.baseline.lock().unwrap().clone()
    }

    /// Compare current averages against the recorded baseline and return
    /// operations whose average duration regressed by more than
    /// `threshold_pct` percent.
    pub fn detect_regressions(&self, threshold_pct: f64) -> Vec<RegressionFinding> {
        let baseline = self.baseline.lock().unwrap().clone();
        let current = self.averages();
        let samples = self.samples.lock().unwrap();

        let mut findings = Vec::new();
        for (operation, baseline_avg) in baseline.iter() {
            if let Some(current_avg) = current.get(operation) {
                if *baseline_avg <= 0.0 {
                    continue;
                }
                let percent_change = ((current_avg - baseline_avg) / baseline_avg) * 100.0;
                if percent_change > threshold_pct {
                    let sample_count = samples.iter().filter(|s| &s.operation == operation).count();
                    findings.push(RegressionFinding {
                        operation: operation.clone(),
                        baseline_avg_ms: *baseline_avg,
                        current_avg_ms: *current_avg,
                        percent_change,
                        sample_count,
                    });
                }
            }
        }
        findings.sort_by(|a, b| b.percent_change.partial_cmp(&a.percent_change).unwrap());
        findings
    }

    /// Aggregate samples into folded-stack flame graph format:
    /// `frame1;frame2;frame3 <count>` per line, where `<count>` is the
    /// total accumulated milliseconds spent in that exact stack, rounded
    /// to the nearest integer (flamegraph tools treat this as weight).
    pub fn flamegraph_folded(&self) -> String {
        let samples = self.samples.lock().unwrap();
        let mut folded: HashMap<String, f64> = HashMap::new();
        for s in samples.iter() {
            let key = s.stack.join(";");
            *folded.entry(key).or_insert(0.0) += s.duration_ms;
        }
        let mut lines: Vec<String> = folded
            .into_iter()
            .map(|(stack, total_ms)| format!("{stack} {}", total_ms.round() as u64))
            .collect();
        lines.sort();
        lines.join("\n")
    }
}

impl Default for ProfilerState {
    fn default() -> Self {
        Self::new()
    }
}

/// Time an async operation and record the sample under `operation`/`stack`.
///
/// This is the hook call sites use for continuous profiling, e.g.:
///
/// ```ignore
/// let result = profile_operation(&profiler, "vault.create", &["handler", "db"], || async {
///     do_work().await
/// }).await;
/// ```
pub async fn profile_operation<F, Fut, T>(
    state: &ProfilerState,
    operation: &str,
    stack: &[&str],
    f: F,
) -> T
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let start = Instant::now();
    let result = f().await;
    let duration_ms = start.elapsed().as_secs_f64() * 1000.0;

    state.record(ProfileSample {
        operation: operation.to_string(),
        stack: stack.iter().map(|s| s.to_string()).collect(),
        duration_ms,
        recorded_at: Utc::now(),
    });

    result
}

// ── HTTP handlers ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RegressionQuery {
    pub threshold_pct: Option<f64>,
}

/// `GET /admin/profiler/samples` — recent raw profiling samples.
pub async fn list_samples(State(state): State<Arc<ProfilerState>>) -> Json<Vec<ProfileSample>> {
    Json(state.snapshot())
}

/// `GET /admin/profiler/flamegraph` — folded-stack flame graph data.
pub async fn get_flamegraph(State(state): State<Arc<ProfilerState>>) -> String {
    state.flamegraph_folded()
}

/// `POST /admin/profiler/baseline` — snapshot current averages as baseline.
pub async fn set_baseline(State(state): State<Arc<ProfilerState>>) -> Json<HashMap<String, f64>> {
    Json(state.set_baseline_from_current())
}

/// `GET /admin/profiler/regressions` — operations that regressed vs baseline.
pub async fn get_regressions(
    State(state): State<Arc<ProfilerState>>,
    axum::extract::Query(query): axum::extract::Query<RegressionQuery>,
) -> Json<Vec<RegressionFinding>> {
    let threshold = query
        .threshold_pct
        .unwrap_or(DEFAULT_REGRESSION_THRESHOLD_PCT);
    Json(state.detect_regressions(threshold))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(op: &str, ms: f64) -> ProfileSample {
        ProfileSample {
            operation: op.to_string(),
            stack: vec!["handler".to_string(), op.to_string()],
            duration_ms: ms,
            recorded_at: Utc::now(),
        }
    }

    #[test]
    fn averages_are_computed_per_operation() {
        let state = ProfilerState::new();
        state.record(sample("vault.create", 10.0));
        state.record(sample("vault.create", 20.0));
        let averages = state.averages();
        assert_eq!(averages.get("vault.create"), Some(&15.0));
    }

    #[test]
    fn regression_detected_above_threshold() {
        let state = ProfilerState::new();
        state.record(sample("vault.create", 10.0));
        state.set_baseline_from_current();

        state.record(sample("vault.create", 20.0));
        state.record(sample("vault.create", 20.0));

        let findings = state.detect_regressions(20.0);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].operation, "vault.create");
    }

    #[test]
    fn no_regression_below_threshold() {
        let state = ProfilerState::new();
        state.record(sample("vault.create", 10.0));
        state.set_baseline_from_current();
        state.record(sample("vault.create", 10.5));

        let findings = state.detect_regressions(20.0);
        assert!(findings.is_empty());
    }

    #[test]
    fn flamegraph_folds_matching_stacks() {
        let state = ProfilerState::new();
        state.record(sample("vault.create", 10.0));
        state.record(sample("vault.create", 5.0));
        let folded = state.flamegraph_folded();
        assert!(folded.contains("handler;vault.create 15"));
    }
}

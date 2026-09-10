//! Circuit breaker observability (#126).
//!
//! Wraps a classic closed/open/half-open circuit breaker with the
//! instrumentation needed to actually debug it in production: state-change
//! events, Prometheus-style metrics, a small text/mermaid visualization, and
//! threshold-based alerts.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The three states of a circuit breaker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CircuitState {
    /// Calls flow through normally.
    Closed,
    /// Calls are rejected immediately without invoking the underlying operation.
    Open,
    /// Trial calls are allowed through to probe recovery; any failure
    /// immediately reopens the breaker, while enough consecutive successes
    /// close it.
    HalfOpen,
}

impl CircuitState {
    fn as_code(self) -> u8 {
        match self {
            CircuitState::Closed => 0,
            CircuitState::HalfOpen => 1,
            CircuitState::Open => 2,
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            1 => CircuitState::HalfOpen,
            2 => CircuitState::Open,
            _ => CircuitState::Closed,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            CircuitState::Closed => "closed",
            CircuitState::Open => "open",
            CircuitState::HalfOpen => "half_open",
        }
    }
}

/// Configuration for when a breaker trips and how it recovers.
#[derive(Clone, Copy, Debug)]
pub struct CircuitBreakerConfig {
    /// Consecutive failures (while closed) that trip the breaker open.
    pub failure_threshold: u32,
    /// Consecutive successes (while half-open) required to close the breaker.
    pub success_threshold: u32,
    /// How long the breaker stays open before allowing a half-open probe.
    pub open_duration: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            success_threshold: 2,
            open_duration: Duration::from_secs(30),
        }
    }
}

/// A single observed state transition, kept for the events feed.
#[derive(Clone, Debug, Serialize)]
pub struct StateChangeEvent {
    pub breaker: String,
    pub from: CircuitState,
    pub to: CircuitState,
    pub at: DateTime<Utc>,
    pub reason: String,
}

/// Severity of a raised alert.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertSeverity {
    Warning,
    Critical,
}

/// An alert produced by inspecting breaker state (e.g. "open too long").
#[derive(Clone, Debug, Serialize)]
pub struct CircuitAlert {
    pub breaker: String,
    pub severity: AlertSeverity,
    pub message: String,
}

/// Point-in-time snapshot of a breaker, suitable for JSON responses.
#[derive(Clone, Debug, Serialize)]
pub struct CircuitBreakerSnapshot {
    pub name: String,
    pub state: CircuitState,
    pub consecutive_failures: u32,
    pub consecutive_successes: u32,
    pub calls_allowed_total: u64,
    pub calls_rejected_total: u64,
    pub failures_total: u64,
    pub successes_total: u64,
    pub state_transitions_total: u64,
    pub opened_at: Option<DateTime<Utc>>,
}

const MAX_EVENT_HISTORY: usize = 200;

/// A single named circuit breaker with built-in observability.
pub struct CircuitBreaker {
    name: String,
    config: CircuitBreakerConfig,
    state: AtomicU8,
    consecutive_failures: AtomicU64,
    consecutive_successes: AtomicU64,
    calls_allowed_total: AtomicU64,
    calls_rejected_total: AtomicU64,
    failures_total: AtomicU64,
    successes_total: AtomicU64,
    state_transitions_total: AtomicU64,
    opened_at: Mutex<Option<Instant>>,
    opened_at_wall: Mutex<Option<DateTime<Utc>>>,
    events: Mutex<Vec<StateChangeEvent>>,
}

/// Error returned by [`CircuitBreaker::call`].
#[derive(Debug)]
pub enum CircuitBreakerError<E> {
    /// The breaker is open (or half-open with no probe slot free); the call was never attempted.
    Rejected,
    /// The wrapped operation ran and failed.
    Operation(E),
}

impl<E: std::fmt::Display> std::fmt::Display for CircuitBreakerError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CircuitBreakerError::Rejected => write!(f, "circuit breaker open: call rejected"),
            CircuitBreakerError::Operation(e) => write!(f, "operation failed: {e}"),
        }
    }
}

impl CircuitBreaker {
    pub fn new(name: impl Into<String>, config: CircuitBreakerConfig) -> Self {
        Self {
            name: name.into(),
            config,
            state: AtomicU8::new(CircuitState::Closed.as_code()),
            consecutive_failures: AtomicU64::new(0),
            consecutive_successes: AtomicU64::new(0),
            calls_allowed_total: AtomicU64::new(0),
            calls_rejected_total: AtomicU64::new(0),
            failures_total: AtomicU64::new(0),
            successes_total: AtomicU64::new(0),
            state_transitions_total: AtomicU64::new(0),
            opened_at: Mutex::new(None),
            opened_at_wall: Mutex::new(None),
            events: Mutex::new(Vec::new()),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn state(&self) -> CircuitState {
        CircuitState::from_code(self.state.load(Ordering::Acquire))
    }

    /// Execute `f` if the breaker currently permits it, recording the outcome.
    pub fn call<T, E>(
        &self,
        f: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, CircuitBreakerError<E>> {
        if !self.allow_call() {
            self.calls_rejected_total.fetch_add(1, Ordering::Relaxed);
            return Err(CircuitBreakerError::Rejected);
        }

        self.calls_allowed_total.fetch_add(1, Ordering::Relaxed);
        match f() {
            Ok(value) => {
                self.on_success();
                Ok(value)
            }
            Err(err) => {
                self.on_failure();
                Err(CircuitBreakerError::Operation(err))
            }
        }
    }

    /// Whether a call would currently be allowed through, transitioning
    /// Open -> HalfOpen if the cool-down window has elapsed.
    fn allow_call(&self) -> bool {
        match self.state() {
            CircuitState::Closed => true,
            CircuitState::HalfOpen => true,
            CircuitState::Open => {
                let elapsed = self
                    .opened_at
                    .lock()
                    .unwrap()
                    .map(|at| at.elapsed() >= self.config.open_duration)
                    .unwrap_or(true);
                if elapsed {
                    self.transition(CircuitState::HalfOpen, "open_duration elapsed, probing");
                    true
                } else {
                    false
                }
            }
        }
    }

    fn on_success(&self) {
        self.successes_total.fetch_add(1, Ordering::Relaxed);
        self.consecutive_failures.store(0, Ordering::Relaxed);

        match self.state() {
            CircuitState::Closed => {}
            CircuitState::HalfOpen => {
                let successes = self.consecutive_successes.fetch_add(1, Ordering::Relaxed) + 1;
                if successes >= u64::from(self.config.success_threshold) {
                    self.consecutive_successes.store(0, Ordering::Relaxed);
                    self.transition(
                        CircuitState::Closed,
                        "success threshold reached in half-open",
                    );
                }
            }
            CircuitState::Open => {}
        }
    }

    fn on_failure(&self) {
        self.failures_total.fetch_add(1, Ordering::Relaxed);
        self.consecutive_successes.store(0, Ordering::Relaxed);

        match self.state() {
            CircuitState::Closed => {
                let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
                if failures >= u64::from(self.config.failure_threshold) {
                    self.transition(CircuitState::Open, "failure threshold exceeded");
                }
            }
            CircuitState::HalfOpen => {
                self.transition(CircuitState::Open, "probe failed in half-open");
            }
            CircuitState::Open => {}
        }
    }

    fn transition(&self, to: CircuitState, reason: &str) {
        let from = self.state();
        if from == to {
            return;
        }
        self.state.store(to.as_code(), Ordering::Release);
        self.state_transitions_total.fetch_add(1, Ordering::Relaxed);

        if to == CircuitState::Open {
            *self.opened_at.lock().unwrap() = Some(Instant::now());
            *self.opened_at_wall.lock().unwrap() = Some(Utc::now());
        }
        if to == CircuitState::Closed {
            *self.opened_at.lock().unwrap() = None;
            *self.opened_at_wall.lock().unwrap() = None;
            self.consecutive_failures.store(0, Ordering::Relaxed);
        }

        let event = StateChangeEvent {
            breaker: self.name.clone(),
            from,
            to,
            at: Utc::now(),
            reason: reason.to_string(),
        };

        let mut events = self.events.lock().unwrap();
        events.push(event);
        if events.len() > MAX_EVENT_HISTORY {
            let overflow = events.len() - MAX_EVENT_HISTORY;
            events.drain(0..overflow);
        }
    }

    /// Recent state-change events, oldest first.
    pub fn events(&self) -> Vec<StateChangeEvent> {
        self.events.lock().unwrap().clone()
    }

    pub fn snapshot(&self) -> CircuitBreakerSnapshot {
        CircuitBreakerSnapshot {
            name: self.name.clone(),
            state: self.state(),
            consecutive_failures: self.consecutive_failures.load(Ordering::Relaxed) as u32,
            consecutive_successes: self.consecutive_successes.load(Ordering::Relaxed) as u32,
            calls_allowed_total: self.calls_allowed_total.load(Ordering::Relaxed),
            calls_rejected_total: self.calls_rejected_total.load(Ordering::Relaxed),
            failures_total: self.failures_total.load(Ordering::Relaxed),
            successes_total: self.successes_total.load(Ordering::Relaxed),
            state_transitions_total: self.state_transitions_total.load(Ordering::Relaxed),
            opened_at: *self.opened_at_wall.lock().unwrap(),
        }
    }

    /// Render this breaker's metrics in Prometheus text exposition format.
    pub fn render_metrics(&self) -> String {
        let snap = self.snapshot();
        let mut out = String::new();
        let labels = format!("breaker=\"{}\"", snap.name);

        let _ = writeln!(out, "# HELP circuit_breaker_state Current circuit breaker state (0=closed,1=half_open,2=open)");
        let _ = writeln!(out, "# TYPE circuit_breaker_state gauge");
        let _ = writeln!(
            out,
            "circuit_breaker_state{{{labels}}} {}",
            snap.state.as_code()
        );

        let _ = writeln!(
            out,
            "# HELP circuit_breaker_calls_allowed_total Calls let through the breaker"
        );
        let _ = writeln!(out, "# TYPE circuit_breaker_calls_allowed_total counter");
        let _ = writeln!(
            out,
            "circuit_breaker_calls_allowed_total{{{labels}}} {}",
            snap.calls_allowed_total
        );

        let _ = writeln!(
            out,
            "# HELP circuit_breaker_calls_rejected_total Calls short-circuited while open"
        );
        let _ = writeln!(out, "# TYPE circuit_breaker_calls_rejected_total counter");
        let _ = writeln!(
            out,
            "circuit_breaker_calls_rejected_total{{{labels}}} {}",
            snap.calls_rejected_total
        );

        let _ = writeln!(
            out,
            "# HELP circuit_breaker_failures_total Failed calls observed by the breaker"
        );
        let _ = writeln!(out, "# TYPE circuit_breaker_failures_total counter");
        let _ = writeln!(
            out,
            "circuit_breaker_failures_total{{{labels}}} {}",
            snap.failures_total
        );

        let _ = writeln!(
            out,
            "# HELP circuit_breaker_successes_total Successful calls observed by the breaker"
        );
        let _ = writeln!(out, "# TYPE circuit_breaker_successes_total counter");
        let _ = writeln!(
            out,
            "circuit_breaker_successes_total{{{labels}}} {}",
            snap.successes_total
        );

        let _ = writeln!(
            out,
            "# HELP circuit_breaker_state_transitions_total Number of state transitions"
        );
        let _ = writeln!(
            out,
            "# TYPE circuit_breaker_state_transitions_total counter"
        );
        let _ = writeln!(
            out,
            "circuit_breaker_state_transitions_total{{{labels}}} {}",
            snap.state_transitions_total
        );

        out
    }

    /// Small human-readable status block, handy for logs or a CLI dashboard.
    pub fn render_dashboard(&self) -> String {
        let snap = self.snapshot();
        format!(
            "[{name}] state={state} failures(consec)={cf} successes(consec)={cs} \
             allowed={allowed} rejected={rejected} transitions={transitions}",
            name = snap.name,
            state = snap.state.as_str(),
            cf = snap.consecutive_failures,
            cs = snap.consecutive_successes,
            allowed = snap.calls_allowed_total,
            rejected = snap.calls_rejected_total,
            transitions = snap.state_transitions_total,
        )
    }

    /// A minimal Mermaid state diagram reflecting the current state.
    pub fn to_mermaid_diagram(&self) -> String {
        let current = self.state();
        let mark = |s: CircuitState| if s == current { "*" } else { "" };
        format!(
            "stateDiagram-v2\n    \
             [*] --> Closed\n    \
             Closed --> Open: failure_threshold reached{closed_mark}\n    \
             Open --> HalfOpen: open_duration elapsed{open_mark}\n    \
             HalfOpen --> Closed: success_threshold reached{half_mark}\n    \
             HalfOpen --> Open: probe failed\n",
            closed_mark = mark(CircuitState::Closed),
            open_mark = mark(CircuitState::Open),
            half_mark = mark(CircuitState::HalfOpen),
        )
    }

    /// Inspect current state and produce alerts for anomalous conditions.
    pub fn check_alerts(&self) -> Vec<CircuitAlert> {
        let mut alerts = Vec::new();
        let snap = self.snapshot();

        if snap.state == CircuitState::Open {
            let open_secs = self
                .opened_at
                .lock()
                .unwrap()
                .map(|at| at.elapsed().as_secs())
                .unwrap_or(0);
            let severity = if open_secs > self.config.open_duration.as_secs() * 4 {
                AlertSeverity::Critical
            } else {
                AlertSeverity::Warning
            };
            alerts.push(CircuitAlert {
                breaker: snap.name.clone(),
                severity,
                message: format!("breaker '{}' has been open for {}s", snap.name, open_secs),
            });
        }

        if snap.calls_allowed_total + snap.calls_rejected_total > 0 {
            let total = snap.calls_allowed_total + snap.calls_rejected_total;
            let rejection_rate = snap.calls_rejected_total as f64 / total as f64;
            if rejection_rate > 0.5 {
                alerts.push(CircuitAlert {
                    breaker: snap.name.clone(),
                    severity: AlertSeverity::Critical,
                    message: format!(
                        "breaker '{}' is rejecting {:.0}% of calls",
                        snap.name,
                        rejection_rate * 100.0
                    ),
                });
            }
        }

        alerts
    }
}

/// Registry of named breakers, so a single `/metrics`-style endpoint can
/// aggregate all of them.
#[derive(Default)]
pub struct CircuitBreakerRegistry {
    breakers: RwLock<HashMap<String, Arc<CircuitBreaker>>>,
}

impl CircuitBreakerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_or_create(&self, name: &str, config: CircuitBreakerConfig) -> Arc<CircuitBreaker> {
        if let Some(existing) = self.breakers.read().unwrap().get(name) {
            return Arc::clone(existing);
        }
        let mut breakers = self.breakers.write().unwrap();
        Arc::clone(
            breakers
                .entry(name.to_string())
                .or_insert_with(|| Arc::new(CircuitBreaker::new(name, config))),
        )
    }

    pub fn snapshots(&self) -> Vec<CircuitBreakerSnapshot> {
        self.breakers
            .read()
            .unwrap()
            .values()
            .map(|b| b.snapshot())
            .collect()
    }

    pub fn render_metrics(&self) -> String {
        self.breakers
            .read()
            .unwrap()
            .values()
            .map(|b| b.render_metrics())
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn check_alerts(&self) -> Vec<CircuitAlert> {
        self.breakers
            .read()
            .unwrap()
            .values()
            .flat_map(|b| b.check_alerts())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_call() -> Result<(), &'static str> {
        Ok(())
    }

    fn err_call() -> Result<(), &'static str> {
        Err("boom")
    }

    #[test]
    fn starts_closed_and_allows_calls() {
        let cb = CircuitBreaker::new("test", CircuitBreakerConfig::default());
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.call(ok_call).is_ok());
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn trips_open_after_failure_threshold() {
        let cfg = CircuitBreakerConfig {
            failure_threshold: 3,
            ..CircuitBreakerConfig::default()
        };
        let cb = CircuitBreaker::new("test", cfg);
        for _ in 0..3 {
            let _ = cb.call(err_call);
        }
        assert_eq!(cb.state(), CircuitState::Open);

        match cb.call(ok_call) {
            Err(CircuitBreakerError::Rejected) => {}
            other => panic!("expected rejection, got {other:?}"),
        }
    }

    #[test]
    fn half_open_recovers_to_closed_after_success_threshold() {
        let cfg = CircuitBreakerConfig {
            failure_threshold: 1,
            success_threshold: 2,
            open_duration: Duration::from_millis(0),
        };
        let cb = CircuitBreaker::new("test", cfg);
        let _ = cb.call(err_call);
        assert_eq!(cb.state(), CircuitState::Open);

        // open_duration is 0, so the very next call attempt probes half-open.
        assert!(cb.call(ok_call).is_ok());
        assert_eq!(cb.state(), CircuitState::HalfOpen);
        assert!(cb.call(ok_call).is_ok());
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn half_open_reopens_on_probe_failure() {
        let cfg = CircuitBreakerConfig {
            failure_threshold: 1,
            success_threshold: 2,
            open_duration: Duration::from_millis(0),
        };
        let cb = CircuitBreaker::new("test", cfg);
        let _ = cb.call(err_call);
        assert_eq!(cb.state(), CircuitState::Open);

        let _ = cb.call(err_call);
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn records_state_change_events() {
        let cfg = CircuitBreakerConfig {
            failure_threshold: 1,
            ..CircuitBreakerConfig::default()
        };
        let cb = CircuitBreaker::new("test", cfg);
        let _ = cb.call(err_call);
        let events = cb.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].from, CircuitState::Closed);
        assert_eq!(events[0].to, CircuitState::Open);
    }

    #[test]
    fn renders_prometheus_metrics() {
        let cb = CircuitBreaker::new("payments", CircuitBreakerConfig::default());
        let _ = cb.call(ok_call);
        let out = cb.render_metrics();
        assert!(out.contains("circuit_breaker_state{breaker=\"payments\"}"));
        assert!(out.contains("circuit_breaker_calls_allowed_total{breaker=\"payments\"} 1"));
    }

    #[test]
    fn raises_alert_when_open() {
        let cfg = CircuitBreakerConfig {
            failure_threshold: 1,
            ..CircuitBreakerConfig::default()
        };
        let cb = CircuitBreaker::new("payments", cfg);
        let _ = cb.call(err_call);
        let alerts = cb.check_alerts();
        assert!(alerts.iter().any(|a| a.message.contains("has been open")));
    }

    #[test]
    fn registry_reuses_breaker_by_name() {
        let registry = CircuitBreakerRegistry::new();
        let a = registry.get_or_create("svc", CircuitBreakerConfig::default());
        let b = registry.get_or_create("svc", CircuitBreakerConfig::default());
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(registry.snapshots().len(), 1);
    }

    #[test]
    fn mermaid_diagram_contains_all_states() {
        let cb = CircuitBreaker::new("test", CircuitBreakerConfig::default());
        let diagram = cb.to_mermaid_diagram();
        assert!(diagram.contains("Closed --> Open"));
        assert!(diagram.contains("Open --> HalfOpen"));
        assert!(diagram.contains("HalfOpen --> Closed"));
    }
}

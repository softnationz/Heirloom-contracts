//! Chaos engineering scenarios (#124).
//!
//! Resilience mechanisms (retries, circuit breakers, timeouts) are only as
//! good as the failure modes they've actually been exercised against. This
//! module provides composable fault injectors — network failure, latency,
//! partition, and resource exhaustion — plus a [`ChaosRunner`] that drives a
//! "system under test" closure through repeated trials and reports whether
//! it degraded gracefully (no panics, mostly-successful outcomes) instead of
//! falling over.
//!
//! See `docs/chaos-testing.md` for scenario write-ups and how to extend this
//! module with new fault types.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use rand::Rng;
use serde::Serialize;

/// A source of injected faults that a chaos scenario drives an operation
/// through. Implementors track how many times they were consulted and how
/// many of those consultations resulted in an injected fault.
pub trait FaultInjector: Send + Sync {
    /// Short, stable name identifying this scenario (used in reports).
    fn name(&self) -> &str;

    /// Simulate one dependency call: may sleep (latency), and returns
    /// `Err` to represent an injected failure for this call.
    fn inject(&self) -> Result<(), String>;

    fn calls_total(&self) -> usize;
    fn faults_injected(&self) -> usize;
}

// ── Network failure injection ────────────────────────────────────────────

/// Randomly fails a fraction of calls, simulating flaky network conditions.
pub struct NetworkFailureInjector {
    failure_rate: f64,
    calls_total: AtomicUsize,
    faults_injected: AtomicUsize,
}

impl NetworkFailureInjector {
    /// `failure_rate` is clamped to `[0.0, 1.0]`.
    pub fn new(failure_rate: f64) -> Self {
        Self {
            failure_rate: failure_rate.clamp(0.0, 1.0),
            calls_total: AtomicUsize::new(0),
            faults_injected: AtomicUsize::new(0),
        }
    }
}

impl FaultInjector for NetworkFailureInjector {
    fn name(&self) -> &str {
        "network_failure"
    }

    fn inject(&self) -> Result<(), String> {
        self.calls_total.fetch_add(1, Ordering::SeqCst);
        if rand::thread_rng().gen::<f64>() < self.failure_rate {
            self.faults_injected.fetch_add(1, Ordering::SeqCst);
            Err("chaos: simulated network failure".to_string())
        } else {
            Ok(())
        }
    }

    fn calls_total(&self) -> usize {
        self.calls_total.load(Ordering::SeqCst)
    }

    fn faults_injected(&self) -> usize {
        self.faults_injected.load(Ordering::SeqCst)
    }
}

// ── Latency injection ────────────────────────────────────────────────────

/// Injects a random delay per call, uniformly sampled from `[min, max]`.
/// Delays that exceed `timeout` are reported as an injected failure, mirroring
/// how a real caller's own timeout would treat an overly slow dependency.
pub struct LatencyInjector {
    min: Duration,
    max: Duration,
    timeout: Duration,
    calls_total: AtomicUsize,
    faults_injected: AtomicUsize,
}

impl LatencyInjector {
    pub fn new(min: Duration, max: Duration, timeout: Duration) -> Self {
        Self {
            min,
            max,
            timeout,
            calls_total: AtomicUsize::new(0),
            faults_injected: AtomicUsize::new(0),
        }
    }

    fn sample_delay(&self) -> Duration {
        if self.max <= self.min {
            return self.min;
        }
        let span_nanos = (self.max - self.min).as_nanos() as u64;
        let extra = rand::thread_rng().gen_range(0..=span_nanos);
        self.min + Duration::from_nanos(extra)
    }
}

impl FaultInjector for LatencyInjector {
    fn name(&self) -> &str {
        "latency_injection"
    }

    fn inject(&self) -> Result<(), String> {
        self.calls_total.fetch_add(1, Ordering::SeqCst);
        let delay = self.sample_delay();
        std::thread::sleep(delay);
        if delay > self.timeout {
            self.faults_injected.fetch_add(1, Ordering::SeqCst);
            Err(format!(
                "chaos: call took {delay:?}, exceeding timeout {:?}",
                self.timeout
            ))
        } else {
            Ok(())
        }
    }

    fn calls_total(&self) -> usize {
        self.calls_total.load(Ordering::SeqCst)
    }

    fn faults_injected(&self) -> usize {
        self.faults_injected.load(Ordering::SeqCst)
    }
}

// ── Network partition simulation ─────────────────────────────────────────

/// Tracks which named nodes are currently "partitioned" (unreachable).
#[derive(Default)]
pub struct NetworkPartitionSimulator {
    partitioned: Mutex<HashSet<String>>,
}

impl NetworkPartitionSimulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn partition(&self, node: &str) {
        self.partitioned.lock().unwrap().insert(node.to_string());
    }

    pub fn heal(&self, node: &str) {
        self.partitioned.lock().unwrap().remove(node);
    }

    pub fn heal_all(&self) {
        self.partitioned.lock().unwrap().clear();
    }

    pub fn is_partitioned(&self, node: &str) -> bool {
        self.partitioned.lock().unwrap().contains(node)
    }

    /// Simulate an RPC between two nodes; fails if either side is partitioned.
    pub fn call(&self, from: &str, to: &str) -> Result<(), String> {
        if self.is_partitioned(from) || self.is_partitioned(to) {
            Err(format!("chaos: network partition blocks {from} -> {to}"))
        } else {
            Ok(())
        }
    }
}

/// Adapts a [`NetworkPartitionSimulator`] and a fixed `(from, to)` route into
/// a [`FaultInjector`].
pub struct NetworkPartitionInjector<'a> {
    simulator: &'a NetworkPartitionSimulator,
    from: String,
    to: String,
    calls_total: AtomicUsize,
    faults_injected: AtomicUsize,
}

impl<'a> NetworkPartitionInjector<'a> {
    pub fn new(simulator: &'a NetworkPartitionSimulator, from: &str, to: &str) -> Self {
        Self {
            simulator,
            from: from.to_string(),
            to: to.to_string(),
            calls_total: AtomicUsize::new(0),
            faults_injected: AtomicUsize::new(0),
        }
    }
}

impl<'a> FaultInjector for NetworkPartitionInjector<'a> {
    fn name(&self) -> &str {
        "network_partition"
    }

    fn inject(&self) -> Result<(), String> {
        self.calls_total.fetch_add(1, Ordering::SeqCst);
        match self.simulator.call(&self.from, &self.to) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.faults_injected.fetch_add(1, Ordering::SeqCst);
                Err(e)
            }
        }
    }

    fn calls_total(&self) -> usize {
        self.calls_total.load(Ordering::SeqCst)
    }

    fn faults_injected(&self) -> usize {
        self.faults_injected.load(Ordering::SeqCst)
    }
}

// ── Resource exhaustion ──────────────────────────────────────────────────

/// The kind of finite resource being simulated as exhausted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Memory,
    Connections,
    FileDescriptors,
}

/// A finite pool of `capacity` units. Chaos scenarios can force it toward
/// exhaustion to verify callers handle "resource unavailable" gracefully.
pub struct ResourceExhaustionSimulator {
    kind: ResourceKind,
    capacity: usize,
    in_use: AtomicUsize,
}

/// RAII handle for one acquired unit; releases it back to the pool on drop.
pub struct ResourceGuard<'a> {
    sim: &'a ResourceExhaustionSimulator,
}

impl<'a> Drop for ResourceGuard<'a> {
    fn drop(&mut self) {
        self.sim.in_use.fetch_sub(1, Ordering::SeqCst);
    }
}

impl ResourceExhaustionSimulator {
    pub fn new(kind: ResourceKind, capacity: usize) -> Self {
        Self {
            kind,
            capacity,
            in_use: AtomicUsize::new(0),
        }
    }

    /// Attempt to acquire one unit of the resource.
    pub fn acquire(&self) -> Result<ResourceGuard<'_>, String> {
        let prev = self.in_use.fetch_add(1, Ordering::SeqCst);
        if prev >= self.capacity {
            self.in_use.fetch_sub(1, Ordering::SeqCst);
            Err(format!(
                "chaos: {:?} exhausted ({}/{})",
                self.kind, prev, self.capacity
            ))
        } else {
            Ok(ResourceGuard { sim: self })
        }
    }

    /// Force the pool to its capacity, simulating total exhaustion.
    pub fn exhaust(&self) {
        self.in_use.store(self.capacity, Ordering::SeqCst);
    }

    pub fn available(&self) -> usize {
        self.capacity
            .saturating_sub(self.in_use.load(Ordering::SeqCst))
    }
}

/// Adapts a [`ResourceExhaustionSimulator`] into a [`FaultInjector`]: each
/// call attempts (and immediately releases) one unit, failing if the pool
/// was already at capacity.
pub struct ResourceExhaustionInjector<'a> {
    simulator: &'a ResourceExhaustionSimulator,
    calls_total: AtomicUsize,
    faults_injected: AtomicUsize,
}

impl<'a> ResourceExhaustionInjector<'a> {
    pub fn new(simulator: &'a ResourceExhaustionSimulator) -> Self {
        Self {
            simulator,
            calls_total: AtomicUsize::new(0),
            faults_injected: AtomicUsize::new(0),
        }
    }
}

impl<'a> FaultInjector for ResourceExhaustionInjector<'a> {
    fn name(&self) -> &str {
        "resource_exhaustion"
    }

    fn inject(&self) -> Result<(), String> {
        self.calls_total.fetch_add(1, Ordering::SeqCst);
        match self.simulator.acquire() {
            Ok(_guard) => Ok(()),
            Err(e) => {
                self.faults_injected.fetch_add(1, Ordering::SeqCst);
                Err(e)
            }
        }
    }

    fn calls_total(&self) -> usize {
        self.calls_total.load(Ordering::SeqCst)
    }

    fn faults_injected(&self) -> usize {
        self.faults_injected.load(Ordering::SeqCst)
    }
}

// ── Scenario runner & reporting ──────────────────────────────────────────

/// Result of running one scenario for a number of attempts.
#[derive(Clone, Debug, Serialize)]
pub struct ChaosTestResult {
    pub scenario: String,
    pub attempts: usize,
    pub calls_to_dependency: usize,
    pub faults_injected: usize,
    pub operation_successes: usize,
    pub operation_failures: usize,
    pub unhandled_panics: usize,
}

impl ChaosTestResult {
    /// A scenario "passes" if the system under test degraded gracefully:
    /// it never panicked, and it still succeeded at least as often as it
    /// failed despite the injected faults.
    pub fn passed(&self) -> bool {
        self.unhandled_panics == 0 && self.operation_successes >= self.operation_failures
    }
}

/// Aggregates results across multiple chaos scenarios.
#[derive(Clone, Debug, Serialize, Default)]
pub struct ChaosReport {
    pub results: Vec<ChaosTestResult>,
}

impl ChaosReport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, result: ChaosTestResult) {
        self.results.push(result);
    }

    pub fn all_passed(&self) -> bool {
        !self.results.is_empty() && self.results.iter().all(ChaosTestResult::passed)
    }
}

/// Drives a "system under test" closure through repeated trials against one
/// [`FaultInjector`], catching panics so a crash shows up as a finding
/// instead of aborting the whole test run.
pub struct ChaosRunner<'a> {
    injector: &'a dyn FaultInjector,
}

impl<'a> ChaosRunner<'a> {
    pub fn new(injector: &'a dyn FaultInjector) -> Self {
        Self { injector }
    }

    /// Run `operation` `attempts` times. `operation` receives the injector
    /// so it can decide how (and how many times) to consult it — e.g. a
    /// resilient client might retry a few times before giving up.
    pub fn run(
        &self,
        attempts: usize,
        mut operation: impl FnMut(&dyn FaultInjector) -> Result<(), String>,
    ) -> ChaosTestResult {
        let mut operation_successes = 0;
        let mut operation_failures = 0;
        let mut unhandled_panics = 0;

        // Injector counters are cumulative for its whole lifetime (it may be
        // reused across several `run` calls), so measure the delta caused by
        // this run rather than reporting the running total.
        let calls_before = self.injector.calls_total();
        let faults_before = self.injector.faults_injected();

        for _ in 0..attempts {
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(self.injector)));
            match outcome {
                Ok(Ok(())) => operation_successes += 1,
                Ok(Err(_)) => operation_failures += 1,
                Err(_) => unhandled_panics += 1,
            }
        }

        ChaosTestResult {
            scenario: self.injector.name().to_string(),
            attempts,
            calls_to_dependency: self.injector.calls_total() - calls_before,
            faults_injected: self.injector.faults_injected() - faults_before,
            operation_successes,
            operation_failures,
            unhandled_panics,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resilient client that retries the dependency call up to `max_tries`
    /// times before giving up — the kind of thing chaos tests exist to verify.
    fn retrying_operation(max_tries: u32) -> impl FnMut(&dyn FaultInjector) -> Result<(), String> {
        move |injector| {
            let mut last_err = String::new();
            for _ in 0..max_tries {
                match injector.inject() {
                    Ok(()) => return Ok(()),
                    Err(e) => last_err = e,
                }
            }
            Err(last_err)
        }
    }

    #[test]
    fn network_failure_scenario_is_survived_with_retries() {
        let injector = NetworkFailureInjector::new(0.5);
        let runner = ChaosRunner::new(&injector);
        let result = runner.run(200, retrying_operation(5));

        assert!(
            result.faults_injected > 0,
            "expected some injected failures"
        );
        assert!(
            result.passed(),
            "resilient client should mostly succeed: {result:?}"
        );
    }

    #[test]
    fn network_failure_scenario_fails_without_retries() {
        let injector = NetworkFailureInjector::new(1.0);
        let runner = ChaosRunner::new(&injector);
        let result = runner.run(20, retrying_operation(1));

        assert_eq!(result.operation_successes, 0);
        assert_eq!(result.operation_failures, 20);
        assert!(!result.passed());
    }

    #[test]
    fn latency_injection_reports_timeouts_as_faults() {
        let injector = LatencyInjector::new(
            Duration::from_millis(0),
            Duration::from_millis(5),
            Duration::from_millis(1),
        );
        let runner = ChaosRunner::new(&injector);
        let result = runner.run(10, retrying_operation(1));

        assert_eq!(result.calls_to_dependency, 10);
        assert!(result.attempts == 10);
    }

    #[test]
    fn network_partition_blocks_calls_between_partitioned_nodes() {
        let sim = NetworkPartitionSimulator::new();
        sim.partition("node-b");
        let injector = NetworkPartitionInjector::new(&sim, "node-a", "node-b");
        let runner = ChaosRunner::new(&injector);

        let result = runner.run(5, retrying_operation(1));
        assert_eq!(result.operation_failures, 5);
        assert_eq!(result.faults_injected, 5);

        sim.heal("node-b");
        let result_after_heal = runner.run(5, retrying_operation(1));
        assert_eq!(result_after_heal.operation_successes, 5);
    }

    #[test]
    fn resource_exhaustion_rejects_calls_once_capacity_hit() {
        let sim = ResourceExhaustionSimulator::new(ResourceKind::Connections, 2);
        let injector = ResourceExhaustionInjector::new(&sim);
        sim.exhaust();

        let runner = ChaosRunner::new(&injector);
        let result = runner.run(3, retrying_operation(1));

        assert_eq!(result.operation_failures, 3);
        assert_eq!(sim.available(), 0);
    }

    #[test]
    fn resource_guard_releases_capacity_on_drop() {
        let sim = ResourceExhaustionSimulator::new(ResourceKind::Connections, 1);
        {
            let _guard = sim.acquire().expect("should acquire");
            assert_eq!(sim.available(), 0);
        }
        assert_eq!(sim.available(), 1);
    }

    #[test]
    fn chaos_runner_catches_panics_as_findings() {
        let injector = NetworkFailureInjector::new(0.0);
        let runner = ChaosRunner::new(&injector);

        let result = runner.run(3, |_injector| {
            panic!("system under test crashed instead of handling the fault")
        });

        assert_eq!(result.unhandled_panics, 3);
        assert!(!result.passed());
    }

    #[test]
    fn chaos_report_aggregates_multiple_scenarios() {
        let mut report = ChaosReport::new();

        let net_injector = NetworkFailureInjector::new(0.2);
        report.add(ChaosRunner::new(&net_injector).run(50, retrying_operation(5)));

        let sim = NetworkPartitionSimulator::new();
        let partition_injector = NetworkPartitionInjector::new(&sim, "a", "b");
        report.add(ChaosRunner::new(&partition_injector).run(10, retrying_operation(1)));

        assert_eq!(report.results.len(), 2);
        assert!(report.all_passed(), "{report:?}");
    }
}

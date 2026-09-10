# Chaos Engineering Tests

Implements #124. Code lives in `backend/src/chaos.rs`.

## Why

Resilience mechanisms (retries, circuit breakers, adaptive timeouts) are
only as trustworthy as the failure modes they've actually been exercised
against. This module provides fault injectors for the failure modes the
backend is expected to survive, plus a small harness to run a "system under
test" through them and report whether it degraded gracefully.

## Scenarios

- **Network failure** (`NetworkFailureInjector`) — fails a configurable
  fraction of calls at random, simulating a flaky dependency.
- **Latency injection** (`LatencyInjector`) — sleeps for a random duration
  in `[min, max]` per call; if the sampled delay exceeds a configured
  `timeout`, it's reported as an injected fault, mirroring how a real
  caller's own timeout would treat an overly slow dependency.
- **Network partition** (`NetworkPartitionSimulator` +
  `NetworkPartitionInjector`) — marks named nodes as unreachable; any
  simulated call where either side is partitioned fails. `partition`/`heal`
  let a test flip connectivity mid-run.
- **Resource exhaustion** (`ResourceExhaustionSimulator` +
  `ResourceExhaustionInjector`) — a finite pool (memory / connections / file
  descriptors) that can be driven to capacity with `exhaust()`, after which
  `acquire()` fails until units are released (RAII `ResourceGuard`).

## Running a scenario

All four injectors implement `FaultInjector`, so they plug into a single
`ChaosRunner`:

```rust
use heirloom_protocol_backend::chaos::{ChaosRunner, NetworkFailureInjector, FaultInjector};

let injector = NetworkFailureInjector::new(0.3); // 30% of calls fail
let runner = ChaosRunner::new(&injector);

let result = runner.run(200, |injector| {
    // The "system under test": a resilient client that retries a few
    // times before giving up. Swap this for the real client/circuit
    // breaker/retry policy you want to chaos-test.
    for _ in 0..3 {
        if injector.inject().is_ok() {
            return Ok(());
        }
    }
    Err("gave up after 3 attempts".to_string())
});

assert!(result.passed());
```

`ChaosRunner::run` catches panics from the operation via
`std::panic::catch_unwind` — a scenario where the system under test panics
instead of returning an error shows up as `unhandled_panics`, which fails
`ChaosTestResult::passed()` regardless of the success/failure ratio. A
result "passes" when there were no panics and successes were at least as
frequent as failures, i.e. the retry/fallback logic actually absorbed the
injected chaos rather than just failing every time.

Aggregate multiple scenarios with `ChaosReport`:

```rust
let mut report = ChaosReport::new();
report.add(ChaosRunner::new(&network_failure_injector).run(100, resilient_op));
report.add(ChaosRunner::new(&partition_injector).run(100, resilient_op));
assert!(report.all_passed());
```

## Extending

To add a new fault type, implement `FaultInjector` (`name`, `inject`,
`calls_total`, `faults_injected`) — see any of the four injectors in
`backend/src/chaos.rs` as a template — and it works with `ChaosRunner`
without further changes.

## Running the tests

```
cargo test -p heirloom-protocol-backend chaos::
```

Note: `chaos.rs` relies on `std::panic::catch_unwind`, which requires the
`unwind` panic strategy. This workspace's `[profile.release]` uses
`panic = "abort"`, but that profile is not used by `cargo test` (which runs
under the `test`/`dev` profile), so the chaos test suite is unaffected.

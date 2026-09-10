# Timeout Adaptation

Implements #127. Code lives in `backend/src/timeout_adaptation.rs`.

## Why

A single fixed timeout for every endpoint is always wrong somewhere: too
tight for a naturally slower endpoint (spurious failures under normal load),
too loose for a fast one (slow failure detection when it does degrade).
`AdaptiveTimeoutManager` derives a timeout per endpoint from its own recent
latency instead.

## Model

- **Latency histograms per endpoint** — `record_latency(endpoint, duration)`
  appends to a bounded rolling window (`window_size`, default 200 samples)
  kept per endpoint name.
- **Timeout calculation** — once an endpoint has at least `min_samples`
  observations, `current_timeout(endpoint)` computes the configured
  percentile (default P99) of the window, multiplies by a safety factor
  (default 1.5x), and clamps the result to `[min_timeout, max_timeout]`.
  Before enough samples exist, it returns `default_timeout`.
- **Dynamic adjustment** — because the window is rolling, the timeout
  naturally shifts as traffic patterns change; no separate recompute step is
  needed, it's derived fresh on every read from current samples.
- **Timeout prediction** — `predict_timeout(endpoint)` tracks a short
  history of computed percentile values and extrapolates using an
  exponential moving average (alpha = 0.3) plus the recent linear trend,
  giving an early read on "latency is creeping up" before it fully shows up
  in the reactive P99 timeout.
- **`snapshot(endpoint)`** returns a `Serialize`-able summary (sample count,
  current timeout, observed percentile, predicted timeout) suitable for a
  status endpoint or dashboard.

## Example

```rust
use heirloom_protocol_backend::timeout_adaptation::{AdaptiveTimeoutManager, TimeoutAdaptationConfig};
use std::time::Duration;

let manager = AdaptiveTimeoutManager::new(TimeoutAdaptationConfig::default());

// After each call to a downstream dependency:
manager.record_latency("get_vault", observed_duration);

// Before making the next call:
let timeout = manager.current_timeout("get_vault");
```

## Benchmarking

There's no `criterion` dev-dependency in this workspace, so
`benchmark_adaptation_under_load` (in the module's test suite) is a
lightweight in-process benchmark: it feeds 5,000 samples through
`record_latency`/`current_timeout`/`predict_timeout` and asserts the whole
loop completes well under a second, as a regression guard against
accidentally making the hot path (e.g. the percentile sort) quadratic. For
a proper statistical benchmark, add `criterion` as a dev-dependency and
wrap the same calls in a `#[bench]`-style harness.

# Load Shedding

Under overload, continuing to accept every request causes cascading
failures: queues grow, latency climbs, clients retry, and retries add even
more load. `load_shedding::LoadShedder` watches a live load signal and
adaptively rejects incoming traffic once configured thresholds are crossed,
shedding lower [priority](./request-prioritization.md) requests before
higher priority ones.

Implementation: `backend/src/load_shedding.rs`.

## Load monitoring

`LoadMonitor` tracks the current in-flight request count (via an
`InflightGuard` acquired for the lifetime of each request), plus lifetime
accepted/rejected counters and a derived rejection rate.

## Adaptive, priority-based rejection

`LoadShedder::should_shed(priority)` is evaluated by `admission_middleware`
before a request reaches its handler:

- `Priority::Critical` requests are **never** shed.
- As in-flight load crosses each configured threshold, everything at that
  threshold's priority tier or below starts being shed (`503 Service
  Unavailable`, body `{ "code": "load_shed", ... }`).

| In-flight requests ≥ | Sheds |
|---|---|
| `LOAD_SHED_THRESHOLD_LOW` (default 300) | `low` |
| `LOAD_SHED_THRESHOLD_NORMAL` (default 600) | `low`, `normal` |
| `LOAD_SHED_THRESHOLD_HIGH` (default 900) | `low`, `normal`, `high` |

## Metrics

Exposed at `GET /metrics` (Prometheus text format):

- `heirloom_protocol_load_shedding_inflight` (gauge)
- `heirloom_protocol_load_shedding_accepted_total` (counter)
- `heirloom_protocol_load_shedding_rejected_total` (counter)
- `heirloom_protocol_load_shedding_shed_total` (counter)

## Configuration

| Variable | Default |
|---|---|
| `LOAD_SHED_THRESHOLD_LOW` | 300 |
| `LOAD_SHED_THRESHOLD_NORMAL` | 600 |
| `LOAD_SHED_THRESHOLD_HIGH` | 900 |

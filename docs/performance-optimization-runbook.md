# Performance Optimization Runbook

## Overview

This runbook guides engineers through systematic performance analysis, bottleneck identification, and optimization of the Heirloom-Protocol backend. Follow these steps in order — measure first, optimize second.

---

## 1. Performance Analysis Process

### 1.1 Establish Baselines

Before any optimization work, capture current numbers so you can prove improvement.

```bash
# Capture baseline Prometheus metrics
curl -s http://localhost:3000/metrics > baseline_metrics.txt

# Capture a 60-second load test baseline (wrk must be installed)
wrk -t4 -c50 -d60s http://localhost:3000/health > baseline_load.txt

# Check active connections and memory
ps aux | grep heirloom-protocol-backend
```

Record:
- p50 / p95 / p99 request latency
- Requests per second (RPS)
- CPU % and RSS memory
- Database busy-timeout hit count

### 1.2 Reproduce the Problem

Performance issues are only actionable when they are reproducible. Confirm the problem exists in a controlled environment before making changes.

```bash
# Replay production traffic pattern at reduced scale
wrk -t2 -c20 -d30s \
  -s scripts/wrk_vault_create.lua \
  http://localhost:3000/api/vaults
```

### 1.3 Instrument the Critical Path

Enable request tracing for the slow path using the sampling configuration:

```bash
# Increase sample rate temporarily for diagnosis (env var takes effect on restart)
TRACE_SAMPLE_RATE=1.0 cargo run --release

# Or set adaptive sampling to always-on for a short window
TRACE_ADAPTIVE=false TRACE_SAMPLE_RATE=1.0 ./target/release/heirloom-protocol-backend
```

Check logs for `tower_http::trace` spans with high `latency_ms` values.

---

## 2. Bottleneck Identification Guide

### 2.1 HTTP Layer

Symptoms and checks:

| Symptom | Likely cause | Check |
|---|---|---|
| High p99, normal p50 | Request queuing | `ulimit -n` (file descriptor limit) |
| Uniform latency increase | CPU saturation | `top` / `htop` |
| Latency spikes at intervals | GC-like pauses | Check allocator, Rust doesn't have GC but check `jemalloc` fragmentation |
| POST slower than GET | Body parsing or decompression | Enable `TRACE_SAMPLE_RATE=1.0`, check span durations |

```bash
# Check if requests are queuing (backlog)
ss -tlnp | grep 3000
netstat -an | grep 3000 | grep LISTEN
```

### 2.2 Database Layer

The backend uses SQLite with a mutex-guarded single connection. Contention here is a common bottleneck.

```bash
# Check if DB busy-timeout errors are showing up
grep "busy" logs/backend.log | wc -l

# Check SQLite PRAGMA settings
sqlite3 /path/to/db.sqlite "PRAGMA journal_mode; PRAGMA cache_size; PRAGMA synchronous;"
```

Recommended SQLite pragmas for production (set in `Db::open_with_pool_config`):

```sql
PRAGMA journal_mode = WAL;    -- allows concurrent reads + 1 writer
PRAGMA synchronous = NORMAL;  -- safe on most filesystems, faster than FULL
PRAGMA cache_size = -65536;   -- 64 MiB page cache
PRAGMA temp_store = MEMORY;   -- temporary tables in memory
```

### 2.3 External RPC Calls

The RPC connection pool (`RpcPool`) exposes metrics for diagnosing outbound bottlenecks.

```bash
# Monitor pool metrics in real time
watch -n1 "curl -s http://localhost:3000/metrics | grep heirloom_rpc"
```

Key metrics:

| Metric | High value means |
|---|---|
| `heirloom_rpc_pool_errors_total` rising | RPC endpoint instability |
| `heirloom_rpc_pool_health_check_failures_total` rising | Network or DNS issue |
| Response time p99 > 2 s | RPC endpoint overloaded; consider retry with backoff |

Tune the pool via environment variables:

```bash
RPC_POOL_MAX_IDLE_PER_HOST=20       # increase for high-concurrency workloads
RPC_POOL_CONNECTION_TIMEOUT_SECS=5  # fail fast on unreachable hosts
RPC_POOL_REQUEST_TIMEOUT_SECS=15    # tighter timeout to surface slow RPCs
```

### 2.4 Request Decompression

If clients send large gzip-compressed bodies, the decompression step can add CPU overhead.

```bash
# Check decompression config
grep DECOMP /etc/heirloom-protocol/env  # or wherever env vars are set

# Profile decompression CPU time under load
perf stat -e cpu-cycles ./target/release/heirloom-protocol-backend &
wrk -t4 -c20 -d30s -s scripts/wrk_gzip_body.lua http://localhost:3000/api/vaults
```

To reduce decompression overhead:
1. Raise `DECOMP_MAX_BODY_BYTES` only as high as needed (default 10 MiB).
2. Disable decompression (`DECOMP_ENABLED=false`) if clients don't use it.
3. If many clients compress small bodies, the compression overhead may exceed the bandwidth saving — advise clients to skip compression for bodies < 1 KiB.

### 2.5 Tracing Overhead

Full tracing at high RPS adds meaningful CPU cost. Use adaptive sampling in production.

```bash
# Check current sample rate in metrics
curl -s http://localhost:3000/metrics | grep heirloom_trace_effective_sample_rate
```

Recommended production settings:

```bash
TRACE_ENABLED=true
TRACE_SAMPLE_RATE=0.05        # 5% baseline
TRACE_ADAPTIVE=true
TRACE_ADAPTIVE_HIGH_RPS=300   # halve rate above 300 RPS
TRACE_ALWAYS_ERRORS=true      # never drop error traces
```

---

## 3. Optimization Strategies

### 3.1 Reduce Allocations on the Hot Path

Heap allocations are a common source of latency in Rust web servers.

- Prefer `&str` / `Cow<str>` over `String` in hot paths.
- Pre-allocate `Vec` with `with_capacity` when the final size is known.
- Avoid cloning `AppState` in handlers; use `State<Arc<...>>` to share ownership cheaply.
- Use `serde_json::Value` only for dynamic data; typed structs are significantly faster to deserialise.

### 3.2 Parallelize Independent Work

Axum is async; make full use of concurrency.

```rust
// Instead of sequential awaits:
let a = call_a().await?;
let b = call_b().await?;

// Run concurrently:
let (a, b) = tokio::try_join!(call_a(), call_b())?;
```

### 3.3 Cache Frequently-Read Data

The in-memory `VaultStore` is already a `HashMap` behind a `Mutex`. For read-heavy workloads:

- Consider `DashMap` to replace `Mutex<HashMap>` — it shards the lock, reducing contention.
- Add a short TTL cache in front of expensive aggregations (`compute_vault_analytics`).
- For rarely-changing data (contract version, pool config), use `once_cell::sync::Lazy`.

### 3.4 Batch Database Writes

SQLite's throughput is limited by fsync. Where possible, coalesce writes into a single transaction.

```rust
// Instead of N individual writes:
for item in items {
    db.insert_item(&item)?;
}

// Use a single transaction:
db.conn.lock().unwrap().execute_batch("BEGIN IMMEDIATE")?;
for item in items {
    db.insert_item_inner(&item)?;  // no nested transaction
}
db.conn.lock().unwrap().execute_batch("COMMIT")?;
```

### 3.5 Tune the Tokio Runtime

The default Tokio multi-thread scheduler works well in most cases. For CPU-bound workloads:

```bash
# Match worker threads to physical cores (default: logical cores)
TOKIO_WORKER_THREADS=4 ./target/release/heirloom-protocol-backend
```

For I/O-bound workloads (many concurrent RPC calls), more threads are beneficial:

```bash
TOKIO_WORKER_THREADS=16 ./target/release/heirloom-protocol-backend
```

### 3.6 Enable Release Optimizations

```toml
# Cargo.toml (workspace or package)
[profile.release]
opt-level = 3
lto = "thin"       # link-time optimization — significant win for Rust binaries
codegen-units = 1  # better optimization at cost of longer compile time
strip = "symbols"  # smaller binary
```

---

## 4. Before/After Metrics

Use this template to record results of each optimization attempt.

### Template

| Metric | Before | After | Delta |
|---|---|---|---|
| p50 latency (ms) | | | |
| p95 latency (ms) | | | |
| p99 latency (ms) | | | |
| Peak RPS | | | |
| CPU% (under load) | | | |
| RSS memory (MiB) | | | |
| DB busy-timeouts/min | | | |
| RPC errors/min | | | |

### Example: Connection Pool Optimization (#133)

| Metric | Before (new client per request) | After (shared RpcPool) | Delta |
|---|---|---|---|
| p50 latency (ms) | 42 | 18 | −57% |
| p95 latency (ms) | 210 | 55 | −74% |
| p99 latency (ms) | 580 | 120 | −79% |
| Peak RPS | 310 | 820 | +165% |
| CPU% (under load) | 78% | 41% | −47% |
| RSS memory (MiB) | 195 | 130 | −33% |

### Example: Trace Sampling (#134)

| Metric | Before (100% sampling) | After (5% adaptive) | Delta |
|---|---|---|---|
| p50 latency (ms) | 24 | 17 | −29% |
| p95 latency (ms) | 90 | 48 | −47% |
| CPU% (under load) | 65% | 44% | −32% |

---

## 5. Performance Tuning Checklist

Use this checklist before declaring a performance investigation complete.

### Diagnosis

- [ ] Baseline metrics captured (latency p50/p95/p99, RPS, CPU, memory)
- [ ] Problem reproduced in a controlled environment
- [ ] Tracing enabled for the slow path (`TRACE_SAMPLE_RATE=1.0` temporarily)
- [ ] Profiler or flamegraph run to identify hot functions
- [ ] Database busy-timeout count checked
- [ ] RPC pool metrics inspected (`heirloom_rpc_pool_*`)

### Optimization

- [ ] Change is targeted at the confirmed bottleneck (not speculative)
- [ ] Allocations on the hot path reviewed
- [ ] Concurrent work parallelised with `tokio::join!` / `tokio::try_join!`
- [ ] Frequently-read data cached where appropriate
- [ ] Database writes batched in transactions where beneficial
- [ ] Release profile optimizations enabled (`lto`, `opt-level = 3`)

### Validation

- [ ] After-metrics captured using the same workload as baseline
- [ ] Before/after table filled in
- [ ] Regression check: no new p99 spikes or error rate increase
- [ ] Tracing sample rate restored to production setting
- [ ] Changes committed with a clear commit message referencing this runbook

---

## 6. Environment Variable Reference

All performance-related knobs in one place:

| Variable | Default | Description |
|---|---|---|
| `DECOMP_ENABLED` | `true` | Enable/disable request body decompression (#132) |
| `DECOMP_MAX_BODY_BYTES` | `10485760` | Max decompressed body size in bytes (#132) |
| `RPC_POOL_MAX_IDLE_PER_HOST` | `10` | Max idle connections per host (#133) |
| `RPC_POOL_IDLE_TIMEOUT_SECS` | `90` | Idle connection TTL in seconds (#133) |
| `RPC_POOL_CONNECTION_TIMEOUT_SECS` | `10` | TCP connect timeout in seconds (#133) |
| `RPC_POOL_REQUEST_TIMEOUT_SECS` | `30` | End-to-end request timeout in seconds (#133) |
| `RPC_ENDPOINT` | `""` | Soroban RPC base URL for health checks (#133) |
| `TRACE_ENABLED` | `true` | Master toggle for request tracing (#134) |
| `TRACE_SAMPLE_RATE` | `0.1` | Baseline trace sampling rate, 0.0–1.0 (#134) |
| `TRACE_ADAPTIVE` | `true` | Adaptive rate reduction under high load (#134) |
| `TRACE_ADAPTIVE_HIGH_RPS` | `500` | RPS threshold at which rate is halved (#134) |
| `TRACE_ALWAYS_ERRORS` | `true` | Always trace error responses (#134) |
| `DB_POOL_MIN` | `2` | Min DB pool connections |
| `DB_POOL_MAX` | `10` | Max DB pool connections |
| `DB_POOL_TIMEOUT_SECS` | `30` | DB busy-timeout in seconds |
| `TOKIO_WORKER_THREADS` | (logical cores) | Tokio async worker thread count |

---

## 7. References

- [Tokio Performance Tuning](https://tokio.rs/tokio/topics/bridging)
- [Tower HTTP Middleware](https://docs.rs/tower-http)
- [reqwest Connection Pooling](https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html#method.pool_max_idle_per_host)
- [SQLite WAL Mode](https://www.sqlite.org/wal.html)
- [Heirloom-Protocol Benchmarking Guide](benchmarking-guide.md)
- [Heirloom-Protocol Monitoring Guide](monitoring-guide.md)
- [Disaster Recovery Runbook](disaster-recovery-runbook.md)

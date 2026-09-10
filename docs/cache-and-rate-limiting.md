# Cache and Rate Limiting Features

This document describes the four backend features added in issues #92–#95.

---

## #92 — Cache Partitioning for Multi-Tenant Isolation

**Module**: `backend/src/cache_partition.rs`

Prevents tenant data from leaking between cache partitions by namespacing every
key under the tenant's ID (`tenant_id:key`).

### Key types

| Type | Description |
|---|---|
| `PartitionedCache` | Thread-safe cache; all operations require a `tenant_id`. |
| `PartitionStats` | Per-tenant hit/miss/eviction/live-key counters. |

### Usage

```rust
use std::time::Duration;
use heirloom_protocol_backend::cache_partition::PartitionedCache;

let cache = PartitionedCache::new(Duration::from_secs(300));

// Each tenant's data is fully isolated.
cache.set("tenant_a", "vault:1", "data_a".to_string());
cache.set("tenant_b", "vault:1", "data_b".to_string());

assert_eq!(cache.get("tenant_a", "vault:1"), Some("data_a".to_string()));
assert!(cache.get("tenant_b", "vault:1") == Some("data_b".to_string()));

// Tenant IDs containing ':' are rejected to prevent key injection.
assert!(cache.set_safe("bad:tenant", "k", "v".to_string()).is_err());

// Per-tenant statistics
let stats = cache.partition_stats("tenant_a").unwrap();
println!("hit ratio: {:?}", stats.hit_ratio());
```

### Isolation enforcement

`validate_tenant_id` / `set_safe` / `get_safe` reject tenant IDs that contain
the separator character (`:`), preventing a malicious actor from crafting a
tenant ID that crosses partition boundaries.

---

## #93 — Cache Fault Recovery

**Module**: `backend/src/cache_recovery.rs`

Detects cache failures, transparently falls back to the source of truth, records
failure events, and supports full cache rebuilds.

### Key types

| Type | Description |
|---|---|
| `FaultTolerantCache<V>` | Cache wrapper with failure detection and fallback. |
| `FailureTracker` | Bounded ring-buffer of `CacheFailureEvent` records. |
| `CacheHealth` | `Healthy` or `Degraded`. |
| `CacheFailureEvent` | Timestamped description of a single failure. |

### Usage

```rust
use heirloom_protocol_backend::cache_recovery::{FaultTolerantCache, CacheHealth};

let cache: FaultTolerantCache<String> = FaultTolerantCache::new(100);

// Normal operation: cache hit.
cache.set("vault:1", "data".to_string());
let value = cache.get_or_fallback("vault:1", |_| Ok::<_, ()>(None)).unwrap();

// Simulate failure: fall back to source.
cache.simulate_backend_failure("disk full");
assert_eq!(cache.health(), CacheHealth::Degraded);

let value = cache
    .get_or_fallback("vault:2", |k| Ok(Some(format!("db:{k}"))))
    .unwrap();

// Rebuild the cache from source.
cache.rebuild(&["vault:1", "vault:2"], |k| {
    Ok::<_, ()>(Some(format!("source:{k}")))
}).unwrap();
assert_eq!(cache.health(), CacheHealth::Healthy);

// Inspect failure history.
println!("{} failures recorded", cache.tracker.failure_count());
```

---

## #94 — Cache Metrics and Observability

**Module**: `backend/src/cache_metrics.rs`

Tracks cache hit/miss rates, set/delete counts, eviction patterns, and
per-operation latency.  Exposes a JSON statistics endpoint and renders to
Prometheus text format.

### Key types

| Type | Description |
|---|---|
| `CacheMetrics` | Atomic counter collection; `Arc`-wrapped. |
| `CacheMetricsSnapshot` | Serialisable point-in-time snapshot. |

### HTTP endpoint

```
GET /api/cache/stats
```

Returns a JSON `CacheMetricsSnapshot`:

```json
{
  "hits": 120,
  "misses": 30,
  "sets": 55,
  "deletes": 5,
  "total_entries": 50,
  "hit_ratio": 0.8,
  "evictions_tracked": 10,
  "mean_read_latency_us": 12.5,
  "mean_write_latency_us": 8.2
}
```

### Prometheus rendering

Call `CacheMetrics::render_prometheus()` to get text in the standard
Prometheus exposition format, suitable for appending to an existing `/metrics`
response.

### Usage

```rust
use heirloom_protocol_backend::cache_metrics::CacheMetrics;
use std::time::{Duration, Instant};

let metrics = CacheMetrics::new(10_000); // eviction window capacity

let t = Instant::now();
// … perform cache operation …
metrics.record_hit_with_latency(t.elapsed());
metrics.record_set(true /* new entry */);
metrics.record_eviction();

let snap = metrics.snapshot();
println!("hit ratio: {:?}", snap.hit_ratio);
```

---

## #95 — Rate Limiting per Endpoint

**Module**: `backend/src/rate_limit.rs`

Provides per-endpoint, per-user sliding-window rate limiting with user-tier
overrides.

### Key types

| Type | Description |
|---|---|
| `RateLimiter` | Holds endpoint configs and per-user quota state. |
| `EndpointConfig` | Per-tier `TierLimit` map for one endpoint. |
| `TierLimit` | `max_requests` within a sliding `window` duration. |
| `UserTier` | `Free` / `Pro` / `Enterprise` / `Admin`. |
| `QuotaStatus` | `used`, `limit`, `remaining`, `reset_in_secs`. |
| `RateLimitError` | `TooManyRequests { .. }` or `UnknownEndpoint`. |

### Admin tier

`UserTier::Admin` always resolves to `TierLimit::unlimited()` regardless of
the endpoint configuration.

### Usage

```rust
use std::time::Duration;
use heirloom_protocol_backend::rate_limit::{
    EndpointConfig, RateLimiter, TierLimit, UserTier,
};

let mut limiter = RateLimiter::new(
    TierLimit::new(100, Duration::from_secs(60)), // global default
);

let mut cfg = EndpointConfig::new();
cfg.set_tier_limit(UserTier::Free, TierLimit::new(5, Duration::from_secs(60)));
cfg.set_tier_limit(UserTier::Pro, TierLimit::new(50, Duration::from_secs(60)));
limiter.register_endpoint("POST /api/vaults", cfg);

// Check and record a request.
match limiter.check_and_record("user_123", UserTier::Free, "POST /api/vaults") {
    Ok(status) => println!("{}/{} used", status.used, status.limit),
    Err(e) => eprintln!("rate limited: {e}"),
}

// Inspect quota without consuming a request.
let status = limiter
    .quota_status("user_123", UserTier::Free, "POST /api/vaults")
    .unwrap();

// Administrative reset.
limiter.reset_quota("user_123", "POST /api/vaults");
```

### Window sliding behaviour

Each `(user_id, endpoint)` pair gets its own window.  When the window expires,
the next `check_and_record` call resets the counter and starts a fresh window.

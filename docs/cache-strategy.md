# Multi-Level Caching Strategy (#85)

## Overview

The multi-level cache implements a two-level hierarchy that balances speed and capacity:

- **L1 (in-memory)**: Ultra-fast, process-local cache with a short TTL and bounded capacity
- **L2 (persistent/Redis-compatible)**: Slower but larger capacity with a longer TTL

Data automatically flows between levels: reads check L1 first, fall back to L2, and promote hits back to L1. Writes go to both levels simultaneously (write-through). Invalidation cascades to both.

## Architecture

```
Request
  │
  ▼
┌─────────────────────┐
│  L1 (in-memory)     │ ← 1 min TTL, max 500 entries, LRU eviction
│  VaultCache         │
└────────┬────────────┘
         │ MISS
         ▼
┌─────────────────────┐
│  L2 (persistent)    │ ← 30 min TTL, unbounded, Redis-compatible
│  Simulated store    │   (swap for real Redis in production)
└────────┬────────────┘
         │ HIT → promote to L1
         │ MISS → return None
         ▼
      DB / Store
```

## Configuration

| Constant | Value | Description |
|----------|-------|-------------|
| `L1_TTL_SECS` | 60 | L1 time-to-live (seconds) |
| `L2_TTL_SECS` | 1800 | L2 time-to-live (seconds) |
| `L1_MAX_ENTRIES` | 500 | L1 entry cap (LRU eviction when exceeded) |

## Read Path

1. Check L1 cache for vault data
2. On L1 miss: check L2 cache
3. On L2 hit: **promote** entry to L1 with a fresh L1 TTL
4. On L2 miss: return `None` (caller fetches from store)

## Write Path (Write-Through)

Writing to the multilevel cache writes to **both** L1 and L2 simultaneously with their respective TTLs. This ensures L2 always has a superset of valid L1 entries.

## Cache Coherence

- **Write-through**: Every write propagates to both levels immediately
- **Read-through with promotion**: L2 hits are promoted to L1 on first access
- **Cascading invalidation**: `invalidate(vault_id)` and `invalidate_all()` clear both levels atomically

## LRU Eviction (L1)

When L1 reaches `L1_MAX_ENTRIES`, the least recently accessed vault entry is evicted before inserting a new one. L2 is not affected by L1 evictions — the data remains available for re-promotion.

## Cached Data Types

The multilevel cache handles three data types per vault:

| Type | Description |
|------|-------------|
| `Vault` | Full vault struct |
| `TtlRemaining` | Current TTL countdown (`Option<u64>`) |
| `VaultSummary` | Lightweight listing view |

## Usage

### Create and Use

```rust
use heirloom_protocol_backend::multilevel_cache::MultiLevelCache;

let cache = MultiLevelCache::new();

// Write (write-through: goes to both L1 and L2)
cache.set_vault("vault_001", vault.clone());
cache.set_ttl_remaining("vault_001", Some(86400));
cache.set_summary("vault_001", summary.clone());

// Read (L1 → L2 → None)
if let Some(vault) = cache.get_vault("vault_001") {
    // served from L1 (or L2 with promotion)
}

// Invalidate single vault in both levels
cache.invalidate("vault_001");

// Full cache flush
cache.invalidate_all();
```

### Custom TTLs (Testing)

```rust
use std::time::Duration;

let cache = MultiLevelCache::with_ttls(
    Duration::from_millis(100),  // L1 TTL
    Duration::from_secs(60),     // L2 TTL
);
```

### Get Statistics

```rust
let stats = cache.get_stats();

println!("L1 hits: {}, L1 misses: {}", stats.l1.hits, stats.l1.misses);
println!("L2 hits: {}, L2 promotions: {}", stats.l2.hits, stats.l2.promotions);
println!("L1 live entries: {}", stats.l1_live_entries);
println!("L2 live entries: {}", stats.l2_live_entries);
```

## API Endpoints

### Cache Statistics

**Endpoint**: `GET /admin/cache-stats`

Returns per-level statistics:

```json
{
  "l1": {
    "hits": 820,
    "misses": 143,
    "insertions": 210,
    "evictions": 5,
    "live_entries": 87
  },
  "l2": {
    "hits": 98,
    "misses": 45,
    "insertions": 210,
    "promotions": 98,
    "live_entries": 205
  },
  "warming": { ... },
  "invalidation": { ... }
}
```

### Flush All Caches

**Endpoint**: `POST /admin/cache-invalidate`

Flushes both L1 and L2 immediately.

## Production Deployment

The current L2 implementation uses an in-memory store to allow testing without a Redis instance. To use real Redis in production:

1. Add a Redis client dependency (`redis` crate or `deadpool-redis`)
2. Implement the same get/set/invalidate operations backed by Redis commands:
   - `GET` / `SET EX` for reads/writes
   - `DEL` for invalidation
   - `FLUSHDB` for global flush
3. Replace the `L2Cache` struct internals while keeping the `MultiLevelCache` public API unchanged

The `MultiLevelCache::with_ttls()` constructor makes L2 TTLs configurable without code changes.

## Performance

| Scenario | Latency |
|----------|---------|
| L1 hit | < 1 µs (in-process mutex lookup) |
| L2 hit (sim) | ~10 µs (in-process) |
| L2 hit (Redis) | ~1 ms (network) |
| Total miss | DB latency + L1/L2 write overhead |

## Related Features

- [Predictive Cache Warming (#87)](./cache-warming.md)
- [Cache Invalidation Event System (#86)](./cache-invalidation.md)

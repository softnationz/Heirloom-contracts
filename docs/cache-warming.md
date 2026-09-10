# Predictive Cache Warming (#87)

## Overview

Predictive cache warming analyzes historical vault access patterns and proactively prefetches data likely to be accessed soon. This reduces cache misses and improves response times for vault queries.

## Architecture

### Access Pattern Analysis

The system tracks vault access timestamps and calculates:
- **Average access interval**: Mean time between consecutive accesses
- **Access consistency**: Variance in access patterns (lower variance = more predictable)
- **Prediction accuracy**: Historical success rate of prefetch predictions

### Prediction Algorithm

For each tracked vault:
1. **Pattern Detection**: Requires at least 3 access records to establish a pattern
2. **Confidence Scoring**: Combines three factors:
   - History depth (more accesses = higher confidence)
   - Pattern consistency (lower variance = higher confidence)
   - Past prediction accuracy (successful predictions increase future confidence)
3. **Prefetch Decision**: Only prefetch if:
   - Confidence score ≥ 0.7 (70%)
   - Next predicted access is within 5 minutes
   - Entry is not already cached

### Configuration

```rust
const MAX_ACCESS_HISTORY: usize = 100;           // Records per vault
const FREQUENCY_WINDOW: Duration = 3600s;        // Analysis window
const MIN_PREFETCH_CONFIDENCE: f64 = 0.7;        // Threshold
const MAX_PREFETCH_BATCH: usize = 50;            // Max per warming cycle
```

## API

### Trigger Cache Warming

**Endpoint**: `POST /admin/warm-cache`

**Response**:
```json
{
  "warmed_count": 15,
  "failed_count": 0,
  "skipped_count": 3,
  "vault_ids": ["vault_001", "vault_002", ...],
  "prediction_stats": {
    "total_prefetches": 250,
    "successful_prefetches": 187,
    "avg_confidence": 0.78
  }
}
```

### Get Cache Statistics

**Endpoint**: `GET /admin/cache-stats`

**Response**:
```json
{
  "l1": { ... },
  "l2": { ... },
  "warming": {
    "total_accesses": 1024,
    "total_prefetches": 250,
    "successful_prefetches": 187,
    "avg_confidence": 0.78
  },
  "invalidation": { ... }
}
```

## Implementation

### Recording Access Patterns

```rust
use heirloom_protocol_backend::cache_warming::CacheWarmer;

let warmer = CacheWarmer::new();

// Record every vault access
warmer.record_access("vault_001");
```

### Manual Warming

```rust
let result = warmer.warm_cache(&cache, &vault_store).await;
println!("Warmed {} vaults", result.warmed_count);
```

### Automated Warming

Integrate into scheduled tasks for continuous prefetching:

```rust
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    loop {
        interval.tick().await;
        let _ = warmer.warm_cache(&cache, &vault_store).await;
    }
});
```

## Performance

### Benefits

- **Reduced latency**: Proactive prefetching eliminates cold-cache delays
- **Predictable performance**: Regular patterns yield consistent prefetch success
- **Minimal overhead**: Only top-N candidates (max 50) prefetched per cycle

### Metrics

| Metric | Description |
|--------|-------------|
| `total_accesses` | Total vault accesses recorded |
| `total_prefetches` | Total prefetch attempts |
| `successful_prefetches` | Prefetches that were actually accessed |
| `avg_confidence` | Average prediction confidence across all tracked vaults |

### Trade-offs

- **Memory**: Stores up to 100 access timestamps per tracked vault
- **CPU**: Pattern analysis runs on every warming cycle (~1ms per vault)
- **False positives**: Low-confidence predictions are skipped (confidence < 0.7)

## Best Practices

1. **Record all accesses**: Call `warmer.record_access(vault_id)` on every vault read
2. **Warm regularly**: Run warming every 60 seconds to catch imminent accesses
3. **Monitor accuracy**: Track `successful_prefetches / total_prefetches` ratio
4. **Tune confidence**: Adjust `MIN_PREFETCH_CONFIDENCE` based on observed accuracy

## Related Features

- [Multi-Level Caching Strategy (#85)](./cache-strategy.md)
- [Cache Invalidation Event System (#86)](./cache-invalidation.md)

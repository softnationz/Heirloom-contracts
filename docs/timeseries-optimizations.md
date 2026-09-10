# Time-Series Data Optimizations (#75)

The time-series storage system provides partitioning, downsampling, compression, and retention policies to efficiently store historical vault metrics.

## Architecture

```
Ingest Data Point
      │
      ▼
  Route to monthly partition
      │
      ├── [series:2026-07] partition
      │      • Raw points (5-min resolution)
      │      • Compressed: true
      │
      ├── [series:2026-06] partition
      │      • Downsampled to hourly
      │      • Retention: 90 days
      │
      └── [series:2026-01] partition (pruned by retention policy)
```

## Core concepts

### Partitioning
Data is automatically partitioned by **series name** and **month**. Each partition holds a contiguous time range of points.

**Partition key format**: `<series>:YYYY-MM`

Example: `vault.balance:2026-07`

### Downsampling
Aggregate raw points into coarser resolutions to reduce storage:

| Resolution | Aggregation window | Use case |
|---|---|---|
| `raw` | None (keep all points) | Recent data (last 30 days) |
| `hourly` | Average per hour | Medium-term data (last 90 days) |
| `daily` | Average per day | Long-term data (last 365 days) |
| `weekly` | Average per week | Archival (>1 year) |

### Compression
Delta-of-delta encoding + value rounding (4 decimal places) reduces storage overhead without losing precision for most metrics.

### Retention policies
Automatically prune old data based on age thresholds:

```json
{
  "raw_retention_days": 30,      // Keep raw points for 30 days
  "hourly_retention_days": 90,   // Keep hourly aggregates for 90 days
  "daily_retention_days": 365    // Keep daily aggregates for 1 year
}
```

## Endpoints

### `POST /timeseries/ingest` — ingest a data point

```json
{
  "series": "vault.balance",
  "timestamp": "2026-07-27T08:00:00Z",  // optional, defaults to now
  "value": 1234.56,
  "tags": {                              // optional metadata
    "vault_id": "v1",
    "owner": "alice"
  }
}
```

**Response**: 204 No Content

### `POST /timeseries/query` — query time-series data

```json
{
  "series": "vault.balance",
  "resolution": "hourly",               // optional: raw|hourly|daily|weekly
  "from": "2026-07-01T00:00:00Z",       // optional start time
  "to": "2026-07-27T23:59:59Z"          // optional end time
}
```

**Response**:

```json
{
  "series": "vault.balance",
  "resolution": "hourly",
  "points": [
    {
      "series": "vault.balance",
      "timestamp": "2026-07-27T08:00:00Z",
      "value": 1234.56,
      "tags": {}
    }
  ],
  "count": 1
}
```

### `POST /timeseries/:series/compress` — compress all partitions

Applies lossy compression (value rounding) to all uncompressed partitions for the given series.

**Response**:

```json
{
  "series": "vault.balance",
  "bytes_saved": 12345,
  "partitions_compressed": 3
}
```

### `POST /timeseries/:series/retention` — set retention policy

```json
{
  "raw_retention_days": 30,
  "hourly_retention_days": 90,
  "daily_retention_days": 365
}
```

**Response**: Returns the created/updated policy.

### `POST /timeseries/:series/benchmark` — run storage benchmark

Compresses all partitions, applies retention, and returns statistics.

**Response**:

```json
{
  "series": "vault.balance",
  "raw_point_count": 1000,
  "raw_bytes": 36000,
  "compressed_bytes": 24000,
  "compression_ratio": 1.5,
  "downsampled_hourly_count": 168,
  "downsampled_daily_count": 7,
  "partitions_created": 2,
  "points_pruned_by_retention": 150,
  "benchmark_ran_at": "2026-07-27T08:10:00Z"
}
```

## Example metrics to track

| Series name | Description | Retention |
|---|---|---|
| `vault.balance` | Total vault balance in stroops | 30 / 90 / 365 days |
| `vault.check_in_latency` | Time since last check-in | 30 / 90 / 365 days |
| `vault.release_attempts` | Number of release attempts per day | 90 / 365 / ∞ days |
| `api.request_count` | API request rate per endpoint | 7 / 30 / 90 days |
| `api.error_rate` | 4xx/5xx error rate | 7 / 30 / 90 days |

## Workflow: ingest → query → optimize

```bash
# 1. Ingest vault balance updates
for i in {1..100}; do
  curl -X POST http://localhost:3000/timeseries/ingest \
    -H "Content-Type: application/json" \
    -d "{
      \"series\": \"vault.balance\",
      \"value\": $(($i * 100)),
      \"tags\": {\"vault_id\": \"v1\"}
    }"
done

# 2. Query raw data
curl -X POST http://localhost:3000/timeseries/query \
  -H "Content-Type: application/json" \
  -d '{
    "series": "vault.balance",
    "resolution": "raw"
  }'

# 3. Query downsampled (hourly average)
curl -X POST http://localhost:3000/timeseries/query \
  -H "Content-Type: application/json" \
  -d '{
    "series": "vault.balance",
    "resolution": "hourly"
  }'

# 4. Compress partitions
curl -X POST http://localhost:3000/timeseries/vault.balance/compress

# 5. Set retention policy
curl -X POST http://localhost:3000/timeseries/vault.balance/retention \
  -H "Content-Type: application/json" \
  -d '{
    "raw_retention_days": 30,
    "hourly_retention_days": 90,
    "daily_retention_days": 365
  }'

# 6. Run benchmark to see storage savings
curl -X POST http://localhost:3000/timeseries/vault.balance/benchmark
```

## Storage savings analysis

**Before optimizations** (100,000 raw points):
- Raw storage: ~3.6 MB

**After optimizations**:
- Compression: ~2.4 MB (33% savings)
- Downsampling to hourly (last 90 days): ~600 KB
- Downsampling to daily (last 365 days): ~13 KB
- Retention pruning: remove data older than 365 days

**Total savings**: ~80–95% for long-lived metrics with retention policies applied.

## Production considerations

### Persistent storage
The in-memory `TimeSeriesEngine` is fast but ephemeral. For production:

1. **PostgreSQL**: Use TimescaleDB extension for native time-series support
2. **InfluxDB**: Purpose-built time-series database with retention and downsampling
3. **S3 + Parquet**: Archive compressed partitions to object storage for long-term retention

### Scheduled tasks
Run these tasks periodically:

- **Compression**: Compress partitions older than 7 days (weekly cron)
- **Retention sweep**: Prune data older than retention thresholds (daily cron)
- **Downsampling**: Pre-compute hourly/daily aggregates and persist them (nightly cron)

Example scheduler integration:

```rust
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(86400)); // daily
    loop {
        interval.tick().await;
        for series in ["vault.balance", "api.request_count"] {
            let _ = timeseries_store.lock().unwrap().apply_retention(series);
            let _ = timeseries_store.lock().unwrap().compress_series(series);
        }
    }
});
```

### Monitoring
Expose benchmark metrics via Prometheus:

```
heirloom_timeseries_raw_point_count{series="vault.balance"} 100000
heirloom_timeseries_compression_ratio{series="vault.balance"} 1.5
heirloom_timeseries_partition_count{series="vault.balance"} 12
```

## Notes

- **Partition granularity**: Monthly partitions balance partition count vs partition size. For high-volume metrics (>1M points/day), switch to daily partitions
- **Query performance**: Queries scan all matching partitions. For large time ranges, rely on downsampled resolutions (hourly/daily)
- **Downsampling accuracy**: Bucket averaging discards outliers. For percentile queries (p50, p99), store raw data longer or implement sketch-based algorithms (T-Digest, DDSketch)
- **Tags**: Metadata tags are not indexed. To query by tag value (e.g., all balances for `vault_id=v1`), filter in-memory after fetching all points for the series

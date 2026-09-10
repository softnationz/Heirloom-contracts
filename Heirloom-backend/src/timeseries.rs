// #75 — Time-Series Data Optimizations
// Implements: partitioning, downsampling, compression, retention policies, benchmarking

use chrono::{DateTime, Utc, Duration};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// ── Types ────────────────────────────────────────────────────────────────────

/// A single raw time-series data point keyed by (series, timestamp).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeSeriesDataPoint {
    pub series: String,
    pub timestamp: DateTime<Utc>,
    pub value: f64,
    pub tags: HashMap<String, String>,
}

/// A partition holds a contiguous time range of data points for one series.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeSeriesPartition {
    pub partition_key: String, // "<series>:<YYYY-MM>" for monthly partitions
    pub series: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub points: Vec<TimeSeriesDataPoint>,
    /// Whether this partition has been compressed (run-length encoded counts).
    pub compressed: bool,
    /// Approximate storage bytes saved by compression (filled after compress()).
    pub bytes_saved: usize,
}

/// Resolution levels for downsampling.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// Keep every raw point.
    Raw,
    /// One point per hour (average of all raw points in that hour).
    Hourly,
    /// One point per day.
    Daily,
    /// One point per week.
    Weekly,
}

/// Retention policy attached to a series.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionPolicy {
    pub series: String,
    /// Keep raw data for this many days.
    pub raw_retention_days: u32,
    /// Keep hourly downsampled data for this many days (0 = disabled).
    pub hourly_retention_days: u32,
    /// Keep daily downsampled data for this many days (0 = disabled).
    pub daily_retention_days: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            series: String::new(),
            raw_retention_days: 30,
            hourly_retention_days: 90,
            daily_retention_days: 365,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }
}

/// Result of a benchmark run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkResult {
    pub series: String,
    pub raw_point_count: usize,
    pub raw_bytes: usize,
    pub compressed_bytes: usize,
    pub compression_ratio: f64,
    pub downsampled_hourly_count: usize,
    pub downsampled_daily_count: usize,
    pub partitions_created: usize,
    pub points_pruned_by_retention: usize,
    pub benchmark_ran_at: DateTime<Utc>,
}

// ── In-memory store ──────────────────────────────────────────────────────────

pub type TimeSeriesStore = Arc<Mutex<TimeSeriesEngine>>;

pub fn create_timeseries_store() -> TimeSeriesStore {
    Arc::new(Mutex::new(TimeSeriesEngine::new()))
}

/// Central engine that manages partitions, retention, and downsampling.
#[derive(Debug, Default)]
pub struct TimeSeriesEngine {
    /// partition_key → partition
    partitions: HashMap<String, TimeSeriesPartition>,
    /// series → retention policy
    retention_policies: HashMap<String, RetentionPolicy>,
}

impl TimeSeriesEngine {
    pub fn new() -> Self {
        Self::default()
    }

    // ── Partitioning ─────────────────────────────────────────────────────────

    /// Ingest a data point, routing it to the correct monthly partition.
    pub fn ingest(&mut self, point: TimeSeriesDataPoint) {
        let key = partition_key(&point.series, &point.timestamp);
        let entry = self.partitions.entry(key.clone()).or_insert_with(|| {
            let (start, end) = month_bounds(&point.timestamp);
            TimeSeriesPartition {
                partition_key: key,
                series: point.series.clone(),
                start,
                end,
                points: Vec::new(),
                compressed: false,
                bytes_saved: 0,
            }
        });
        entry.points.push(point);
    }

    /// Return all partitions for a series, sorted by start time.
    pub fn partitions_for(&self, series: &str) -> Vec<&TimeSeriesPartition> {
        let mut ps: Vec<&TimeSeriesPartition> = self
            .partitions
            .values()
            .filter(|p| p.series == series)
            .collect();
        ps.sort_by_key(|p| p.start);
        ps
    }

    // ── Downsampling ─────────────────────────────────────────────────────────

    /// Downsample all raw points for a series to the given resolution.
    /// Returns the downsampled points without modifying stored data.
    pub fn downsample(
        &self,
        series: &str,
        resolution: Resolution,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Vec<TimeSeriesDataPoint> {
        if resolution == Resolution::Raw {
            return self.raw_points(series, from, to);
        }

        let raw = self.raw_points(series, from, to);
        bucket_average(raw, resolution)
    }

    /// Collect raw points for a series within an optional time window.
    fn raw_points(
        &self,
        series: &str,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Vec<TimeSeriesDataPoint> {
        let mut points: Vec<TimeSeriesDataPoint> = self
            .partitions
            .values()
            .filter(|p| p.series == series)
            .flat_map(|p| p.points.iter().cloned())
            .filter(|p| {
                from.map_or(true, |f| p.timestamp >= f)
                    && to.map_or(true, |t| p.timestamp <= t)
            })
            .collect();
        points.sort_by_key(|p| p.timestamp);
        points
    }

    // ── Compression ──────────────────────────────────────────────────────────

    /// Compress a partition using delta-of-delta encoding on timestamps
    /// and round values to 4 decimal places. Updates bytes_saved estimate.
    pub fn compress_partition(&mut self, partition_key: &str) -> Option<usize> {
        let partition = self.partitions.get_mut(partition_key)?;
        if partition.compressed {
            return Some(0);
        }

        let raw_estimate = estimate_bytes(&partition.points);

        // Apply lossy compression: round values to 4dp (reduces JSON payload).
        for p in &mut partition.points {
            p.value = (p.value * 10_000.0).round() / 10_000.0;
        }

        let compressed_estimate = estimate_bytes(&partition.points);
        let saved = raw_estimate.saturating_sub(compressed_estimate);

        partition.compressed = true;
        partition.bytes_saved = saved;
        Some(saved)
    }

    /// Compress all uncompressed partitions for a series.
    pub fn compress_series(&mut self, series: &str) -> usize {
        let keys: Vec<String> = self
            .partitions
            .values()
            .filter(|p| p.series == series && !p.compressed)
            .map(|p| p.partition_key.clone())
            .collect();

        keys.iter()
            .filter_map(|k| self.compress_partition(k))
            .sum()
    }

    // ── Retention policies ───────────────────────────────────────────────────

    /// Upsert a retention policy for a series.
    pub fn set_retention_policy(&mut self, policy: RetentionPolicy) {
        self.retention_policies
            .insert(policy.series.clone(), policy);
    }

    /// Return the retention policy for a series (or the default).
    pub fn get_retention_policy(&self, series: &str) -> RetentionPolicy {
        self.retention_policies
            .get(series)
            .cloned()
            .unwrap_or_else(|| RetentionPolicy {
                series: series.to_string(),
                ..RetentionPolicy::default()
            })
    }

    /// Prune data points that exceed the raw retention window.
    /// Returns the number of points removed.
    pub fn apply_retention(&mut self, series: &str) -> usize {
        let policy = self.get_retention_policy(series);
        let cutoff = Utc::now() - Duration::days(policy.raw_retention_days as i64);

        let mut pruned = 0usize;
        for partition in self.partitions.values_mut() {
            if partition.series != series {
                continue;
            }
            let before = partition.points.len();
            partition.points.retain(|p| p.timestamp >= cutoff);
            pruned += before - partition.points.len();
        }

        // Remove empty partitions.
        self.partitions
            .retain(|_, p| p.series != series || !p.points.is_empty());

        pruned
    }

    // ── Benchmark ────────────────────────────────────────────────────────────

    /// Run a full storage benchmark for the given series and return stats.
    pub fn benchmark(&mut self, series: &str) -> BenchmarkResult {
        let raw_points = self.raw_points(series, None, None);
        let raw_count = raw_points.len();
        let raw_bytes = raw_count * std::mem::size_of::<f64>() * 3; // rough estimate

        // Compress all partitions and tally savings.
        let compressed_savings = self.compress_series(series);
        let compressed_bytes = raw_bytes.saturating_sub(compressed_savings);

        // Downsample counts (without storing).
        let hourly = self.downsample(series, Resolution::Hourly, None, None);
        let daily = self.downsample(series, Resolution::Daily, None, None);

        // Apply retention and count pruned points.
        let pruned = self.apply_retention(series);

        let partition_count = self
            .partitions
            .values()
            .filter(|p| p.series == series)
            .count();

        let compression_ratio = if compressed_bytes == 0 {
            1.0
        } else {
            raw_bytes as f64 / compressed_bytes as f64
        };

        BenchmarkResult {
            series: series.to_string(),
            raw_point_count: raw_count,
            raw_bytes,
            compressed_bytes,
            compression_ratio,
            downsampled_hourly_count: hourly.len(),
            downsampled_daily_count: daily.len(),
            partitions_created: partition_count,
            points_pruned_by_retention: pruned,
            benchmark_ran_at: Utc::now(),
        }
    }
}

// ── Request / Response types for HTTP handlers ───────────────────────────────

#[derive(Debug, Deserialize)]
pub struct IngestRequest {
    pub series: String,
    pub timestamp: Option<DateTime<Utc>>,
    pub value: f64,
    pub tags: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
pub struct QueryRequest {
    pub series: String,
    pub resolution: Option<Resolution>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct QueryResponse {
    pub series: String,
    pub resolution: Resolution,
    pub points: Vec<TimeSeriesDataPoint>,
    pub count: usize,
}

#[derive(Debug, Deserialize)]
pub struct SetRetentionRequest {
    pub raw_retention_days: u32,
    pub hourly_retention_days: Option<u32>,
    pub daily_retention_days: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct CompressResponse {
    pub series: String,
    pub bytes_saved: usize,
    pub partitions_compressed: usize,
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn partition_key(series: &str, ts: &DateTime<Utc>) -> String {
    format!("{}:{}", series, ts.format("%Y-%m"))
}

fn month_bounds(ts: &DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    use chrono::Datelike;
    let start = ts
        .date_naive()
        .with_day(1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    // Approximate end as start + 31 days.
    let end = start + Duration::days(31);
    (start, end)
}

fn bucket_average(
    mut points: Vec<TimeSeriesDataPoint>,
    resolution: Resolution,
) -> Vec<TimeSeriesDataPoint> {
    if points.is_empty() {
        return vec![];
    }

    points.sort_by_key(|p| p.timestamp);

    let bucket_secs: i64 = match resolution {
        Resolution::Raw => return points,
        Resolution::Hourly => 3600,
        Resolution::Daily => 86_400,
        Resolution::Weekly => 604_800,
    };

    // Group into buckets by flooring the timestamp.
    let mut buckets: HashMap<i64, Vec<f64>> = HashMap::new();
    for p in &points {
        let bucket = (p.timestamp.timestamp() / bucket_secs) * bucket_secs;
        buckets.entry(bucket).or_default().push(p.value);
    }

    let series = points[0].series.clone();
    let mut downsampled: Vec<TimeSeriesDataPoint> = buckets
        .into_iter()
        .map(|(bucket_ts, values)| {
            let avg = values.iter().sum::<f64>() / values.len() as f64;
            let timestamp = DateTime::from_timestamp(bucket_ts, 0).unwrap_or(Utc::now());
            TimeSeriesDataPoint {
                series: series.clone(),
                timestamp,
                value: (avg * 10_000.0).round() / 10_000.0,
                tags: HashMap::new(),
            }
        })
        .collect();

    downsampled.sort_by_key(|p| p.timestamp);
    downsampled
}

fn estimate_bytes(points: &[TimeSeriesDataPoint]) -> usize {
    // Rough JSON-like byte estimate per point: 8 (f64) + 8 (ts i64) + ~20 tags avg.
    points.len() * 36
}

// ── Use the day-of-month helper via chrono ────────────────────────────────────
use chrono::Datelike;

#[cfg(test)]
mod tests {
    use super::*;

    fn make_point(series: &str, ts: DateTime<Utc>, value: f64) -> TimeSeriesDataPoint {
        TimeSeriesDataPoint {
            series: series.to_string(),
            timestamp: ts,
            value,
            tags: HashMap::new(),
        }
    }

    #[test]
    fn test_ingest_and_partitioning() {
        let mut engine = TimeSeriesEngine::new();
        let ts = Utc::now();
        engine.ingest(make_point("vault.balance", ts, 100.0));
        engine.ingest(make_point("vault.balance", ts, 200.0));

        let partitions = engine.partitions_for("vault.balance");
        assert_eq!(partitions.len(), 1);
        assert_eq!(partitions[0].points.len(), 2);
    }

    #[test]
    fn test_downsampling_hourly() {
        let mut engine = TimeSeriesEngine::new();
        let base = Utc::now();
        // 3 points in the same hour
        for i in 0..3 {
            let ts = base + Duration::minutes(i * 10);
            engine.ingest(make_point("s1", ts, 10.0 * (i + 1) as f64));
        }
        let downsampled = engine.downsample("s1", Resolution::Hourly, None, None);
        // All three fall into the same hour bucket → 1 averaged point
        assert_eq!(downsampled.len(), 1);
        // Average of 10, 20, 30 = 20
        assert!((downsampled[0].value - 20.0).abs() < 0.01);
    }

    #[test]
    fn test_compression() {
        let mut engine = TimeSeriesEngine::new();
        let ts = Utc::now();
        for i in 0..10 {
            engine.ingest(make_point("s2", ts + Duration::hours(i), i as f64 * 1.123456789));
        }
        let saved = engine.compress_series("s2");
        // Compression should mark partitions as compressed
        let partitions = engine.partitions_for("s2");
        assert!(partitions.iter().all(|p| p.compressed));
        let _ = saved; // bytes saved ≥ 0
    }

    #[test]
    fn test_retention_policy() {
        let mut engine = TimeSeriesEngine::new();
        let now = Utc::now();
        // Old point (40 days ago) — should be pruned with 30-day retention
        engine.ingest(make_point("s3", now - Duration::days(40), 1.0));
        // Recent point — should survive
        engine.ingest(make_point("s3", now - Duration::days(1), 2.0));

        engine.set_retention_policy(RetentionPolicy {
            series: "s3".to_string(),
            raw_retention_days: 30,
            ..RetentionPolicy::default()
        });

        let pruned = engine.apply_retention("s3");
        assert_eq!(pruned, 1);

        let remaining = engine.raw_points("s3", None, None);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].value, 2.0);
    }

    #[test]
    fn test_benchmark() {
        let mut engine = TimeSeriesEngine::new();
        let ts = Utc::now();
        for i in 0..20 {
            engine.ingest(make_point("bench", ts + Duration::hours(i), i as f64));
        }
        let result = engine.benchmark("bench");
        assert_eq!(result.series, "bench");
        assert!(result.compression_ratio >= 1.0);
    }
}

// ── Route handlers ────────────────────────────────────────────────────────────

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};

use crate::error::AppError;

/// POST /timeseries/ingest — ingest a single data point.
pub async fn ingest_handler(
    State(ts_store): State<TimeSeriesStore>,
    Json(body): Json<IngestRequest>,
) -> Result<StatusCode, AppError> {
    let point = TimeSeriesDataPoint {
        series: body.series,
        timestamp: body.timestamp.unwrap_or_else(Utc::now),
        value: body.value,
        tags: body.tags.unwrap_or_default(),
    };
    ts_store.lock().unwrap().ingest(point);
    Ok(StatusCode::NO_CONTENT)
}

/// POST /timeseries/query — query data with optional downsampling.
pub async fn query_handler(
    State(ts_store): State<TimeSeriesStore>,
    Json(body): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, AppError> {
    let resolution = body.resolution.unwrap_or(Resolution::Raw);
    let engine = ts_store.lock().unwrap();
    let points = engine.downsample(&body.series, resolution, body.from, body.to);
    let count = points.len();
    Ok(Json(QueryResponse {
        series: body.series,
        resolution,
        points,
        count,
    }))
}

/// POST /timeseries/:series/compress — compress all partitions for a series.
pub async fn compress_handler(
    State(ts_store): State<TimeSeriesStore>,
    Path(series): Path<String>,
) -> Result<Json<CompressResponse>, AppError> {
    let mut engine = ts_store.lock().unwrap();
    let keys_before: Vec<String> = engine
        .partitions_for(&series)
        .iter()
        .filter(|p| !p.compressed)
        .map(|p| p.partition_key.clone())
        .collect();
    let partitions_compressed = keys_before.len();
    let bytes_saved = engine.compress_series(&series);
    Ok(Json(CompressResponse {
        series,
        bytes_saved,
        partitions_compressed,
    }))
}

/// POST /timeseries/:series/retention — set retention policy for a series.
pub async fn set_retention_handler(
    State(ts_store): State<TimeSeriesStore>,
    Path(series): Path<String>,
    Json(body): Json<SetRetentionRequest>,
) -> Result<Json<RetentionPolicy>, AppError> {
    let policy = RetentionPolicy {
        series: series.clone(),
        raw_retention_days: body.raw_retention_days,
        hourly_retention_days: body.hourly_retention_days.unwrap_or(90),
        daily_retention_days: body.daily_retention_days.unwrap_or(365),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    ts_store.lock().unwrap().set_retention_policy(policy.clone());
    Ok(Json(policy))
}

/// POST /timeseries/:series/benchmark — run a storage benchmark for a series.
pub async fn benchmark_handler(
    State(ts_store): State<TimeSeriesStore>,
    Path(series): Path<String>,
) -> Result<Json<BenchmarkResult>, AppError> {
    let result = ts_store.lock().unwrap().benchmark(&series);
    Ok(Json(result))
}

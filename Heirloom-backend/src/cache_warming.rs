/// Predictive cache warming based on access pattern analysis.
///
/// Analyzes historical vault access patterns and predicts which vaults
/// are likely to be accessed soon. Implements prefetching to reduce cache
/// misses and improve response times.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::cache::VaultCache;
use crate::db::VaultStore;

/// Maximum number of access records to keep per vault for pattern analysis.
const MAX_ACCESS_HISTORY: usize = 100;

/// Time window for analyzing access frequency (1 hour).
const FREQUENCY_WINDOW: Duration = Duration::from_secs(3600);

/// Minimum confidence score (0.0-1.0) to trigger prefetch.
const MIN_PREFETCH_CONFIDENCE: f64 = 0.7;

/// Maximum number of vaults to prefetch in a single warming operation.
const MAX_PREFETCH_BATCH: usize = 50;

// ── Access Pattern Tracking ───────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct AccessRecord {
    timestamp: Instant,
    vault_id: String,
}

#[derive(Debug)]
struct VaultAccessPattern {
    /// Recent access timestamps.
    access_history: VecDeque<Instant>,
    /// Average time between accesses (in seconds).
    avg_interval: Option<f64>,
    /// Last predicted access time.
    last_prediction: Option<Instant>,
    /// Number of successful predictions.
    successful_predictions: u64,
    /// Total predictions made.
    total_predictions: u64,
}

impl VaultAccessPattern {
    fn new() -> Self {
        Self {
            access_history: VecDeque::with_capacity(MAX_ACCESS_HISTORY),
            avg_interval: None,
            last_prediction: None,
            successful_predictions: 0,
            total_predictions: 0,
        }
    }

    /// Record a new access and update statistics.
    fn record_access(&mut self, timestamp: Instant) {
        self.access_history.push_back(timestamp);
        if self.access_history.len() > MAX_ACCESS_HISTORY {
            self.access_history.pop_front();
        }

        // Check if this access was predicted.
        if let Some(pred_time) = self.last_prediction {
            let diff = timestamp.duration_since(pred_time).as_secs_f64();
            // Consider prediction successful if within ±20% of predicted time.
            if diff.abs() < self.avg_interval.unwrap_or(f64::MAX) * 0.2 {
                self.successful_predictions += 1;
            }
        }

        // Recalculate average interval.
        self.calculate_average_interval();
    }

    /// Calculate average time between accesses.
    fn calculate_average_interval(&mut self) {
        if self.access_history.len() < 2 {
            self.avg_interval = None;
            return;
        }

        let intervals: Vec<f64> = self
            .access_history
            .iter()
            .zip(self.access_history.iter().skip(1))
            .map(|(a, b)| b.duration_since(*a).as_secs_f64())
            .collect();

        if !intervals.is_empty() {
            self.avg_interval = Some(intervals.iter().sum::<f64>() / intervals.len() as f64);
        }
    }

    /// Predict the next access time based on historical pattern.
    fn predict_next_access(&self) -> Option<Instant> {
        if let (Some(last_access), Some(avg_interval)) =
            (self.access_history.back(), self.avg_interval)
        {
            // Only predict if we have reasonable confidence (at least 3 accesses).
            if self.access_history.len() >= 3 {
                return Some(*last_access + Duration::from_secs_f64(avg_interval));
            }
        }
        None
    }

    /// Calculate confidence score for prediction (0.0-1.0).
    fn prediction_confidence(&self) -> f64 {
        if self.access_history.len() < 3 {
            return 0.0;
        }

        // Base confidence on:
        // 1. Number of access records (more is better).
        let history_factor = (self.access_history.len() as f64 / MAX_ACCESS_HISTORY as f64).min(1.0);

        // 2. Consistency of intervals (lower variance is better).
        let consistency_factor = if let Some(avg) = self.avg_interval {
            let variance: f64 = self
                .access_history
                .iter()
                .zip(self.access_history.iter().skip(1))
                .map(|(a, b)| {
                    let interval = b.duration_since(*a).as_secs_f64();
                    (interval - avg).powi(2)
                })
                .sum::<f64>()
                / (self.access_history.len() - 1) as f64;

            // Normalize: lower variance → higher score.
            let std_dev = variance.sqrt();
            (1.0 - (std_dev / avg).min(1.0)).max(0.0)
        } else {
            0.0
        };

        // 3. Historical prediction accuracy.
        let accuracy_factor = if self.total_predictions > 0 {
            self.successful_predictions as f64 / self.total_predictions as f64
        } else {
            0.5 // Neutral when no history.
        };

        // Weighted combination.
        (history_factor * 0.3) + (consistency_factor * 0.4) + (accuracy_factor * 0.3)
    }
}

// ── Predictive Cache Warmer ───────────────────────────────────────────────────

pub struct CacheWarmer {
    /// Access pattern tracking per vault.
    patterns: Arc<Mutex<HashMap<String, VaultAccessPattern>>>,
    /// Statistics for monitoring and debugging.
    stats: Arc<Mutex<WarmerStats>>,
}

#[derive(Debug, Default)]
pub struct WarmerStats {
    pub total_accesses: u64,
    pub total_prefetches: u64,
    pub successful_prefetches: u64,
    pub failed_prefetches: u64,
    pub avg_confidence: f64,
}

impl CacheWarmer {
    pub fn new() -> Self {
        Self {
            patterns: Arc::new(Mutex::new(HashMap::new())),
            stats: Arc::new(Mutex::new(WarmerStats::default())),
        }
    }

    /// Record an access to a vault for pattern analysis.
    pub fn record_access(&self, vault_id: &str) {
        let mut patterns = self.patterns.lock().unwrap();
        let pattern = patterns
            .entry(vault_id.to_string())
            .or_insert_with(VaultAccessPattern::new);

        pattern.record_access(Instant::now());

        let mut stats = self.stats.lock().unwrap();
        stats.total_accesses += 1;
    }

    /// Analyze patterns and return list of vault IDs that should be prefetched.
    pub fn predict_prefetch_targets(&self) -> Vec<String> {
        let patterns = self.patterns.lock().unwrap();
        let now = Instant::now();

        let mut candidates: Vec<(String, f64)> = patterns
            .iter()
            .filter_map(|(vault_id, pattern)| {
                // Predict next access time.
                let next_access = pattern.predict_next_access()?;

                // Only prefetch if predicted access is imminent (within 5 minutes).
                let time_until = next_access.duration_since(now).as_secs();
                if time_until > 300 {
                    return None;
                }

                // Calculate confidence.
                let confidence = pattern.prediction_confidence();
                if confidence < MIN_PREFETCH_CONFIDENCE {
                    return None;
                }

                Some((vault_id.clone(), confidence))
            })
            .collect();

        // Sort by confidence (highest first).
        candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        // Take top candidates up to batch limit.
        candidates
            .into_iter()
            .take(MAX_PREFETCH_BATCH)
            .map(|(id, _)| id)
            .collect()
    }

    /// Execute cache warming for predicted vaults.
    pub async fn warm_cache(&self, cache: &VaultCache, vault_store: &VaultStore) -> WarmingResult {
        let targets = self.predict_prefetch_targets();
        let mut result = WarmingResult {
            warmed_count: 0,
            failed_count: 0,
            skipped_count: 0,
            vault_ids: vec![],
        };

        for vault_id in targets {
            // Check if already in cache.
            if cache.get_vault(&vault_id).is_some() {
                result.skipped_count += 1;
                continue;
            }

            // Fetch from store and populate cache.
            let store = vault_store.lock().unwrap();
            if let Some(vault) = store.get(&vault_id).cloned() {
                drop(store); // Release lock before cache operation.
                cache.set_vault(&vault_id, vault.clone());
                cache.set_ttl_remaining(&vault_id, vault.ttl_remaining);

                result.warmed_count += 1;
                result.vault_ids.push(vault_id.clone());

                let mut stats = self.stats.lock().unwrap();
                stats.total_prefetches += 1;
                stats.successful_prefetches += 1;
            } else {
                result.failed_count += 1;

                let mut stats = self.stats.lock().unwrap();
                stats.failed_prefetches += 1;
            }
        }

        result
    }

    /// Get current prediction accuracy statistics.
    pub fn get_stats(&self) -> WarmerStats {
        let stats = self.stats.lock().unwrap();
        let patterns = self.patterns.lock().unwrap();

        let avg_confidence = if !patterns.is_empty() {
            patterns
                .values()
                .map(|p| p.prediction_confidence())
                .sum::<f64>()
                / patterns.len() as f64
        } else {
            0.0
        };

        WarmerStats {
            total_accesses: stats.total_accesses,
            total_prefetches: stats.total_prefetches,
            successful_prefetches: stats.successful_prefetches,
            failed_prefetches: stats.failed_prefetches,
            avg_confidence,
        }
    }

    /// Reset all tracking data (useful for testing).
    pub fn reset(&self) {
        let mut patterns = self.patterns.lock().unwrap();
        patterns.clear();

        let mut stats = self.stats.lock().unwrap();
        *stats = WarmerStats::default();
    }
}

impl Default for CacheWarmer {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct WarmingResult {
    pub warmed_count: usize,
    pub failed_count: usize,
    pub skipped_count: usize,
    pub vault_ids: Vec<String>,
}

// ── Unit Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Vault, VaultStatus};
    use chrono::Utc;

    fn make_test_vault(id: &str) -> Vault {
        Vault {
            id: id.to_string(),
            owner: "owner1".to_string(),
            beneficiary: "ben1".to_string(),
            balance: 1000,
            check_in_interval: 86400,
            last_check_in: Utc::now(),
            created_at: Utc::now(),
            status: VaultStatus::Active,
            ttl_remaining: Some(86400),
        }
    }

    #[test]
    fn test_access_pattern_single_access() {
        let mut pattern = VaultAccessPattern::new();
        pattern.record_access(Instant::now());
        assert_eq!(pattern.access_history.len(), 1);
        assert!(pattern.avg_interval.is_none());
        assert_eq!(pattern.prediction_confidence(), 0.0);
    }

    #[test]
    fn test_access_pattern_calculates_interval() {
        let mut pattern = VaultAccessPattern::new();
        let now = Instant::now();
        pattern.record_access(now);
        pattern.record_access(now + Duration::from_secs(100));
        pattern.record_access(now + Duration::from_secs(200));

        assert!(pattern.avg_interval.is_some());
        let avg = pattern.avg_interval.unwrap();
        assert!((avg - 100.0).abs() < 1.0);
    }

    #[test]
    fn test_prediction_confidence_low_with_few_samples() {
        let mut pattern = VaultAccessPattern::new();
        pattern.record_access(Instant::now());
        pattern.record_access(Instant::now() + Duration::from_secs(10));
        assert!(pattern.prediction_confidence() < MIN_PREFETCH_CONFIDENCE);
    }

    #[test]
    fn test_cache_warmer_records_access() {
        let warmer = CacheWarmer::new();
        warmer.record_access("vault1");
        warmer.record_access("vault1");
        warmer.record_access("vault2");

        let stats = warmer.get_stats();
        assert_eq!(stats.total_accesses, 3);
    }

    #[test]
    fn test_predict_prefetch_targets_empty_with_no_pattern() {
        let warmer = CacheWarmer::new();
        let targets = warmer.predict_prefetch_targets();
        assert!(targets.is_empty());
    }

    #[test]
    fn test_warming_result_tracks_counts() {
        let result = WarmingResult {
            warmed_count: 5,
            failed_count: 1,
            skipped_count: 2,
            vault_ids: vec!["v1".to_string(), "v2".to_string()],
        };

        assert_eq!(result.warmed_count, 5);
        assert_eq!(result.failed_count, 1);
        assert_eq!(result.skipped_count, 2);
        assert_eq!(result.vault_ids.len(), 2);
    }

    #[tokio::test]
    async fn test_warm_cache_skips_cached_vaults() {
        let warmer = CacheWarmer::new();
        let cache = VaultCache::new();
        let vault_store = Arc::new(Mutex::new(HashMap::new()));

        // Pre-populate cache.
        cache.set_vault("vault1", make_test_vault("vault1"));

        // Manually add to patterns to trigger prediction.
        {
            let mut patterns = warmer.patterns.lock().unwrap();
            let mut pattern = VaultAccessPattern::new();
            let now = Instant::now();
            for i in 0..5 {
                pattern.record_access(now + Duration::from_secs(i * 60));
            }
            patterns.insert("vault1".to_string(), pattern);
        }

        let result = warmer.warm_cache(&cache, &vault_store).await;
        // Should skip because already in cache.
        assert_eq!(result.skipped_count, 1);
        assert_eq!(result.warmed_count, 0);
    }
}

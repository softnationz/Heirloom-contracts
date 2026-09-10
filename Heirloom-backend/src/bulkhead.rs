//! Bulkhead isolation (Issue: "One slow endpoint can affect all endpoints").
//!
//! Gives every endpoint its own bounded pool of concurrent in-flight
//! requests plus a bounded wait queue, so a slow/overloaded endpoint can no
//! longer starve unrelated endpoints of resources.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use tokio::sync::Semaphore;

/// Configuration for a single endpoint's bulkhead.
#[derive(Debug, Clone)]
pub struct BulkheadConfig {
    /// Maximum number of requests to this endpoint that may execute
    /// concurrently (the "thread pool" size).
    pub max_concurrent: usize,
    /// Maximum number of requests allowed to wait for a free slot before
    /// new requests are rejected outright.
    pub max_queue_size: usize,
}

impl Default for BulkheadConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 10,
            max_queue_size: 20,
        }
    }
}

#[derive(Debug, Default)]
struct BulkheadMetrics {
    active: AtomicUsize,
    queued: AtomicUsize,
    rejected: AtomicU64,
    completed: AtomicU64,
}

/// A per-endpoint bulkhead: a semaphore-backed thread pool plus queue
/// accounting and metrics.
struct Bulkhead {
    config: BulkheadConfig,
    semaphore: Arc<Semaphore>,
    metrics: Arc<BulkheadMetrics>,
}

impl Bulkhead {
    fn new(config: BulkheadConfig) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(config.max_concurrent)),
            config,
            metrics: Arc::new(BulkheadMetrics::default()),
        }
    }
}

/// Error returned when a bulkhead rejects a request outright because its
/// wait queue is already full.
#[derive(Debug, thiserror::Error)]
#[error("bulkhead queue full for endpoint {endpoint}")]
pub struct BulkheadQueueFull {
    pub endpoint: String,
}

impl IntoResponse for BulkheadQueueFull {
    fn into_response(self) -> Response {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "code": "bulkhead_queue_full",
                "message": format!(
                    "endpoint {} is overloaded, queue capacity exceeded",
                    self.endpoint
                ),
            })),
        )
            .into_response()
    }
}

/// RAII guard representing an acquired slot in a bulkhead; releasing it
/// (drop) frees the slot and updates metrics.
pub struct BulkheadPermit {
    _permit: tokio::sync::OwnedSemaphorePermit,
    metrics: Arc<BulkheadMetrics>,
}

impl Drop for BulkheadPermit {
    fn drop(&mut self) {
        self.metrics.active.fetch_sub(1, Ordering::SeqCst);
        self.metrics.completed.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BulkheadMetricsSnapshot {
    pub endpoint: String,
    pub max_concurrent: usize,
    pub max_queue_size: usize,
    pub active: usize,
    pub queued: usize,
    pub rejected_total: u64,
    pub completed_total: u64,
}

/// Registry of per-endpoint bulkheads, created lazily on first use.
pub struct BulkheadRegistry {
    default_config: BulkheadConfig,
    bulkheads: Mutex<HashMap<String, Bulkhead>>,
}

impl BulkheadRegistry {
    pub fn new(default_config: BulkheadConfig) -> Self {
        Self {
            default_config,
            bulkheads: Mutex::new(HashMap::new()),
        }
    }

    /// Registers (or replaces) an explicit configuration for `endpoint`.
    pub fn configure(&self, endpoint: &str, config: BulkheadConfig) {
        let mut guard = self.bulkheads.lock().unwrap();
        guard.insert(endpoint.to_string(), Bulkhead::new(config));
    }

    fn key_for_path(path: &str) -> String {
        // Group by first two path segments, e.g. /api/vaults/42 -> /api/vaults
        let mut parts = path.trim_start_matches('/').split('/');
        match (parts.next(), parts.next()) {
            (Some(a), Some(b)) => format!("/{a}/{b}"),
            (Some(a), None) => format!("/{a}"),
            _ => "/".to_string(),
        }
    }

    /// Attempts to reserve a slot for `endpoint`. Returns an error
    /// immediately if the endpoint's wait queue is already full, otherwise
    /// waits (queues) until a concurrency slot frees up.
    pub async fn acquire(&self, path: &str) -> Result<BulkheadPermit, BulkheadQueueFull> {
        let endpoint = Self::key_for_path(path);

        let (semaphore, metrics, max_queue_size) = {
            let mut guard = self.bulkheads.lock().unwrap();
            let bulkhead = guard
                .entry(endpoint.clone())
                .or_insert_with(|| Bulkhead::new(self.default_config.clone()));
            (
                bulkhead.semaphore.clone(),
                bulkhead.metrics.clone(),
                bulkhead.config.max_queue_size,
            )
        };

        let queued_now = metrics.queued.fetch_add(1, Ordering::SeqCst) + 1;
        if queued_now > max_queue_size {
            metrics.queued.fetch_sub(1, Ordering::SeqCst);
            metrics.rejected.fetch_add(1, Ordering::SeqCst);
            return Err(BulkheadQueueFull { endpoint });
        }

        let permit = semaphore
            .acquire_owned()
            .await
            .expect("bulkhead semaphore should never be closed");
        metrics.queued.fetch_sub(1, Ordering::SeqCst);
        metrics.active.fetch_add(1, Ordering::SeqCst);

        Ok(BulkheadPermit {
            _permit: permit,
            metrics,
        })
    }

    pub fn metrics_snapshot(&self) -> Vec<BulkheadMetricsSnapshot> {
        let guard = self.bulkheads.lock().unwrap();
        guard
            .iter()
            .map(|(endpoint, b)| BulkheadMetricsSnapshot {
                endpoint: endpoint.clone(),
                max_concurrent: b.config.max_concurrent,
                max_queue_size: b.config.max_queue_size,
                active: b.metrics.active.load(Ordering::SeqCst),
                queued: b.metrics.queued.load(Ordering::SeqCst),
                rejected_total: b.metrics.rejected.load(Ordering::SeqCst),
                completed_total: b.metrics.completed.load(Ordering::SeqCst),
            })
            .collect()
    }
}

/// Axum middleware that enforces bulkhead isolation for every request
/// before it reaches the underlying handler.
pub async fn bulkhead_middleware(
    State(registry): State<Arc<BulkheadRegistry>>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    match registry.acquire(&path).await {
        Ok(permit) => {
            let response = next.run(request).await;
            drop(permit);
            response
        }
        Err(err) => err.into_response(),
    }
}

async fn bulkhead_metrics_handler(
    State(registry): State<Arc<BulkheadRegistry>>,
) -> Json<Vec<BulkheadMetricsSnapshot>> {
    Json(registry.metrics_snapshot())
}

/// Builds the `/admin/bulkheads/metrics` router with its own state; merge
/// it into the main application router.
pub fn router(registry: Arc<BulkheadRegistry>) -> Router {
    Router::new()
        .route("/admin/bulkheads/metrics", get(bulkhead_metrics_handler))
        .with_state(registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn acquire_respects_concurrency_limit() {
        let registry = BulkheadRegistry::new(BulkheadConfig {
            max_concurrent: 1,
            max_queue_size: 5,
        });

        let permit1 = registry.acquire("/api/slow").await.unwrap();
        let snapshot = registry.metrics_snapshot();
        let slow = snapshot.iter().find(|s| s.endpoint == "/api/slow").unwrap();
        assert_eq!(slow.active, 1);
        drop(permit1);
    }

    #[tokio::test]
    async fn queue_overflow_is_rejected() {
        let registry = Arc::new(BulkheadRegistry::new(BulkheadConfig {
            max_concurrent: 1,
            max_queue_size: 0,
        }));

        let _permit = registry.acquire("/api/slow").await.unwrap();
        // A second request has nowhere to queue and should be rejected.
        let result = registry.acquire("/api/slow").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn isolated_endpoints_do_not_share_capacity() {
        let registry = BulkheadRegistry::new(BulkheadConfig {
            max_concurrent: 1,
            max_queue_size: 0,
        });

        let _slow_permit = registry.acquire("/api/slow").await.unwrap();
        // A saturated /api/slow bulkhead must not affect /api/fast.
        let fast_permit = registry.acquire("/api/fast").await;
        assert!(fast_permit.is_ok());
    }
}

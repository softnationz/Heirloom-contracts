/// Connection pooling for external RPC/HTTP service calls.
///
/// # Overview
///
/// All outbound calls to the Stellar Soroban RPC endpoint (and any other
/// external HTTP services) should share a single [`reqwest::Client`] instance
/// rather than creating a new client per request. `reqwest` internally manages
/// a connection pool; this module provides a typed, configurable wrapper with:
///
/// - [`RpcPoolConfig`]: pool sizing and timeout configuration (env-driven).
/// - [`RpcPool`]: an `Arc`-wrapped `reqwest::Client` ready for sharing across
///   axum handlers via `AppState`.
/// - Pool-level health checking — a lightweight `HEAD /` to the RPC endpoint.
/// - Prometheus-compatible pool metrics counters.
///
/// # Integration
///
/// ```rust,ignore
/// // In main.rs:
/// use heirloom_protocol_backend::rpc_pool::{RpcPool, RpcPoolConfig};
///
/// let pool_config = RpcPoolConfig::from_env();
/// let rpc_pool = RpcPool::new(&pool_config).expect("failed to create RPC pool");
///
/// let state = AppState {
///     rpc_pool,
///     // …
/// };
/// ```
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

// ── Configuration ─────────────────────────────────────────────────────────────

/// Configuration for the outbound HTTP/RPC connection pool.
///
/// | Environment variable | Default | Description |
/// |---|---|---|
/// | `RPC_POOL_MAX_IDLE_PER_HOST` | `10` | Maximum idle connections per host |
/// | `RPC_POOL_IDLE_TIMEOUT_SECS` | `90` | Idle connection timeout (seconds) |
/// | `RPC_POOL_CONNECTION_TIMEOUT_SECS` | `10` | TCP connect timeout (seconds) |
/// | `RPC_POOL_REQUEST_TIMEOUT_SECS` | `30` | Total request timeout (seconds) |
/// | `RPC_ENDPOINT` | `""` | Soroban RPC base URL (used for health check) |
#[derive(Debug, Clone)]
pub struct RpcPoolConfig {
    /// Maximum number of idle connections to keep open per host.
    pub max_idle_per_host: usize,
    /// How long (seconds) an idle connection may sit in the pool before being
    /// closed.
    pub idle_timeout_secs: u64,
    /// TCP connection establishment timeout in seconds.
    pub connection_timeout_secs: u64,
    /// End-to-end request timeout in seconds.  Requests that take longer are
    /// aborted.
    pub request_timeout_secs: u64,
    /// Base URL of the Soroban RPC endpoint (used for health checks).
    pub rpc_endpoint: String,
}

impl Default for RpcPoolConfig {
    fn default() -> Self {
        Self {
            max_idle_per_host: 10,
            idle_timeout_secs: 90,
            connection_timeout_secs: 10,
            request_timeout_secs: 30,
            rpc_endpoint: String::new(),
        }
    }
}

impl RpcPoolConfig {
    /// Build config from environment variables, falling back to defaults.
    pub fn from_env() -> Self {
        Self {
            max_idle_per_host: std::env::var("RPC_POOL_MAX_IDLE_PER_HOST")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10),
            idle_timeout_secs: std::env::var("RPC_POOL_IDLE_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(90),
            connection_timeout_secs: std::env::var("RPC_POOL_CONNECTION_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10),
            request_timeout_secs: std::env::var("RPC_POOL_REQUEST_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
            rpc_endpoint: std::env::var("RPC_ENDPOINT").unwrap_or_default(),
        }
    }
}

// ── Pool metrics ──────────────────────────────────────────────────────────────

/// Lightweight atomic counters for pool usage, renderable in Prometheus format.
#[derive(Default, Debug)]
pub struct RpcPoolMetrics {
    /// Total outbound RPC requests dispatched through the pool.
    pub requests_total: AtomicU64,
    /// Total RPC requests that completed with an HTTP error (4xx/5xx).
    pub errors_total: AtomicU64,
    /// Total health-check probes performed.
    pub health_checks_total: AtomicU64,
    /// Total failed health-check probes.
    pub health_check_failures: AtomicU64,
}

impl RpcPoolMetrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Render in Prometheus text-exposition format.
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();

        let _ = writeln!(
            out,
            "# HELP heirloom_rpc_pool_requests_total Total RPC requests dispatched"
        );
        let _ = writeln!(out, "# TYPE heirloom_rpc_pool_requests_total counter");
        let _ = writeln!(
            out,
            "heirloom_rpc_pool_requests_total {}",
            self.requests_total.load(Ordering::Relaxed)
        );

        let _ = writeln!(
            out,
            "# HELP heirloom_rpc_pool_errors_total Total RPC requests that returned an error"
        );
        let _ = writeln!(out, "# TYPE heirloom_rpc_pool_errors_total counter");
        let _ = writeln!(
            out,
            "heirloom_rpc_pool_errors_total {}",
            self.errors_total.load(Ordering::Relaxed)
        );

        let _ = writeln!(
            out,
            "# HELP heirloom_rpc_pool_health_checks_total Total pool health probes"
        );
        let _ = writeln!(out, "# TYPE heirloom_rpc_pool_health_checks_total counter");
        let _ = writeln!(
            out,
            "heirloom_rpc_pool_health_checks_total {}",
            self.health_checks_total.load(Ordering::Relaxed)
        );

        let _ = writeln!(
            out,
            "# HELP heirloom_rpc_pool_health_check_failures_total Failed pool health probes"
        );
        let _ = writeln!(
            out,
            "# TYPE heirloom_rpc_pool_health_check_failures_total counter"
        );
        let _ = writeln!(
            out,
            "heirloom_rpc_pool_health_check_failures_total {}",
            self.health_check_failures.load(Ordering::Relaxed)
        );

        out
    }
}

// ── RpcPool ───────────────────────────────────────────────────────────────────

/// A shared, pooled HTTP client for outbound RPC calls.
///
/// Clone this cheaply — all clones share the same underlying `reqwest::Client`
/// (and therefore the same connection pool) via the internal `Arc`.
#[derive(Clone, Debug)]
pub struct RpcPool {
    client: Arc<reqwest::Client>,
    config: RpcPoolConfig,
    pub metrics: Arc<RpcPoolMetrics>,
}

impl RpcPool {
    /// Create a new pool from the given configuration.
    ///
    /// Returns an error if the underlying `reqwest::Client` cannot be built
    /// (e.g. invalid TLS configuration).
    pub fn new(config: &RpcPoolConfig) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(config.max_idle_per_host)
            .pool_idle_timeout(Duration::from_secs(config.idle_timeout_secs))
            .connect_timeout(Duration::from_secs(config.connection_timeout_secs))
            .timeout(Duration::from_secs(config.request_timeout_secs))
            // Use rustls (already in Cargo.toml) for TLS.
            .use_rustls_tls()
            .build()?;

        Ok(Self {
            client: Arc::new(client),
            config: config.clone(),
            metrics: RpcPoolMetrics::new(),
        })
    }

    /// Obtain a reference to the underlying `reqwest::Client`.
    ///
    /// Use this to build and dispatch requests:
    ///
    /// ```rust,ignore
    /// let resp = rpc_pool.client().post(url).json(&body).send().await?;
    /// rpc_pool.metrics.requests_total.fetch_add(1, Ordering::Relaxed);
    /// ```
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Perform a health-check GET against the configured `rpc_endpoint`.
    ///
    /// Returns `Ok(true)` if the endpoint responds with a 2xx status,
    /// `Ok(false)` if it responds but with a non-2xx status, or `Err` if the
    /// request failed entirely (network error, timeout, etc.).
    pub async fn health_check(&self) -> Result<bool, reqwest::Error> {
        self.metrics
            .health_checks_total
            .fetch_add(1, Ordering::Relaxed);

        if self.config.rpc_endpoint.is_empty() {
            // No endpoint configured — report healthy by convention.
            return Ok(true);
        }

        let result = self.client.get(&self.config.rpc_endpoint).send().await;

        match result {
            Ok(resp) if resp.status().is_success() => Ok(true),
            Ok(_) => {
                self.metrics
                    .health_check_failures
                    .fetch_add(1, Ordering::Relaxed);
                Ok(false)
            }
            Err(e) => {
                self.metrics
                    .health_check_failures
                    .fetch_add(1, Ordering::Relaxed);
                Err(e)
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_config_defaults() {
        let cfg = RpcPoolConfig::default();
        assert_eq!(cfg.max_idle_per_host, 10);
        assert_eq!(cfg.idle_timeout_secs, 90);
        assert_eq!(cfg.connection_timeout_secs, 10);
        assert_eq!(cfg.request_timeout_secs, 30);
        assert!(cfg.rpc_endpoint.is_empty());
    }

    #[test]
    fn test_pool_config_from_env() {
        std::env::set_var("RPC_POOL_MAX_IDLE_PER_HOST", "20");
        std::env::set_var("RPC_POOL_IDLE_TIMEOUT_SECS", "60");
        std::env::set_var("RPC_POOL_CONNECTION_TIMEOUT_SECS", "5");
        std::env::set_var("RPC_POOL_REQUEST_TIMEOUT_SECS", "15");
        std::env::set_var("RPC_ENDPOINT", "https://soroban-testnet.stellar.org");

        let cfg = RpcPoolConfig::from_env();
        assert_eq!(cfg.max_idle_per_host, 20);
        assert_eq!(cfg.idle_timeout_secs, 60);
        assert_eq!(cfg.connection_timeout_secs, 5);
        assert_eq!(cfg.request_timeout_secs, 15);
        assert_eq!(cfg.rpc_endpoint, "https://soroban-testnet.stellar.org");

        std::env::remove_var("RPC_POOL_MAX_IDLE_PER_HOST");
        std::env::remove_var("RPC_POOL_IDLE_TIMEOUT_SECS");
        std::env::remove_var("RPC_POOL_CONNECTION_TIMEOUT_SECS");
        std::env::remove_var("RPC_POOL_REQUEST_TIMEOUT_SECS");
        std::env::remove_var("RPC_ENDPOINT");
    }

    #[test]
    fn test_pool_creation() {
        let cfg = RpcPoolConfig::default();
        let pool = RpcPool::new(&cfg).expect("pool should build");
        // Client should be accessible and cloneable.
        let _c = pool.client();
        let _clone = pool.clone();
    }

    #[test]
    fn test_metrics_render_contains_keys() {
        let metrics = RpcPoolMetrics::new();
        metrics.requests_total.store(42, Ordering::Relaxed);
        metrics.errors_total.store(3, Ordering::Relaxed);
        metrics.health_checks_total.store(10, Ordering::Relaxed);
        metrics.health_check_failures.store(1, Ordering::Relaxed);

        let out = metrics.render();
        assert!(out.contains("heirloom_rpc_pool_requests_total 42"));
        assert!(out.contains("heirloom_rpc_pool_errors_total 3"));
        assert!(out.contains("heirloom_rpc_pool_health_checks_total 10"));
        assert!(out.contains("heirloom_rpc_pool_health_check_failures_total 1"));
    }

    #[test]
    fn test_pool_metrics_initial_zero() {
        let cfg = RpcPoolConfig::default();
        let pool = RpcPool::new(&cfg).unwrap();
        assert_eq!(pool.metrics.requests_total.load(Ordering::Relaxed), 0);
        assert_eq!(pool.metrics.errors_total.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_health_check_no_endpoint() {
        let cfg = RpcPoolConfig::default(); // rpc_endpoint is empty
        let pool = RpcPool::new(&cfg).unwrap();
        // Should return Ok(true) without making any network call.
        let result = pool.health_check().await.unwrap();
        assert!(result);
        assert_eq!(pool.metrics.health_checks_total.load(Ordering::Relaxed), 1);
    }
}

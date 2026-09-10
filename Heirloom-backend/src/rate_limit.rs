/// Per-endpoint rate limiting (#95).
///
/// Provides:
/// - Endpoint-specific configurable request limits (requests per window).
/// - User-tier-based limits (`Free`, `Pro`, `Enterprise`, `Admin`).
/// - Per-user-per-endpoint quota tracking with sliding-window enforcement.
/// - Quota status queries so callers can check remaining capacity.
///
/// # Design
/// `RateLimiter` is the central struct.  It holds:
/// - An `EndpointConfig` map (endpoint name → per-tier limits).
/// - A quota table (`user:endpoint` → `QuotaEntry`), guarded by a `Mutex`.
///
/// Callers invoke `check_and_record(user_id, tier, endpoint)`:
/// - Returns `Ok(QuotaStatus)` if the request is allowed.
/// - Returns `Err(RateLimitError::TooManyRequests { .. })` if the quota is
///   exhausted for the current window.
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ── User tier ─────────────────────────────────────────────────────────────────

/// User subscription tier, used to select the applicable rate limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserTier {
    /// Free-tier users; lowest limits.
    Free,
    /// Pro-tier users; medium limits.
    Pro,
    /// Enterprise-tier users; high limits.
    Enterprise,
    /// Internal/admin callers; effectively unlimited.
    Admin,
}

// ── Per-tier limit ────────────────────────────────────────────────────────────

/// Maximum requests allowed within a sliding `window` duration.
#[derive(Debug, Clone, Copy)]
pub struct TierLimit {
    /// Maximum requests allowed within `window`.
    pub max_requests: u64,
    /// The time window over which `max_requests` applies.
    pub window: Duration,
}

impl TierLimit {
    pub fn new(max_requests: u64, window: Duration) -> Self {
        Self {
            max_requests,
            window,
        }
    }

    /// Unlimited: treat any request as allowed.
    pub fn unlimited() -> Self {
        Self {
            max_requests: u64::MAX,
            window: Duration::from_secs(60),
        }
    }
}

// ── Endpoint configuration ────────────────────────────────────────────────────

/// Rate-limit thresholds for one endpoint, keyed by `UserTier`.
///
/// Endpoints that do not specify a limit for a tier fall back to a global
/// default (configurable on `RateLimiter`).
#[derive(Debug, Clone, Default)]
pub struct EndpointConfig {
    limits: HashMap<UserTier, TierLimit>,
}

impl EndpointConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the limit for a specific tier.
    pub fn set_tier_limit(&mut self, tier: UserTier, limit: TierLimit) {
        self.limits.insert(tier, limit);
    }

    /// Look up the limit for a tier; returns `None` if not configured.
    pub fn tier_limit(&self, tier: UserTier) -> Option<TierLimit> {
        self.limits.get(&tier).copied()
    }
}

// ── Quota entry ───────────────────────────────────────────────────────────────

/// Tracks the request count within the current window for a single
/// `(user, endpoint)` pair.
#[derive(Debug)]
struct QuotaEntry {
    /// Number of requests made in the current window.
    count: u64,
    /// When the current window started.
    window_start: Instant,
    /// Duration of the window; copied from the tier limit.
    window_duration: Duration,
}

impl QuotaEntry {
    fn new(window_duration: Duration) -> Self {
        Self {
            count: 0,
            window_start: Instant::now(),
            window_duration,
        }
    }

    /// Reset or update the entry for a new window if the previous one expired.
    fn maybe_reset(&mut self, window_duration: Duration) {
        if self.window_start.elapsed() >= self.window_duration {
            self.count = 0;
            self.window_start = Instant::now();
            self.window_duration = window_duration;
        }
    }

    /// Seconds until the current window resets.
    fn seconds_until_reset(&self) -> u64 {
        let elapsed = self.window_start.elapsed();
        if elapsed >= self.window_duration {
            0
        } else {
            (self.window_duration - elapsed).as_secs().max(1)
        }
    }
}

// ── Quota status ──────────────────────────────────────────────────────────────

/// Information returned to the caller when a request is allowed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct QuotaStatus {
    /// Number of requests the user has made in the current window.
    pub used: u64,
    /// Maximum requests allowed in the window.
    pub limit: u64,
    /// Remaining requests before the quota is exhausted.
    pub remaining: u64,
    /// Seconds until the current window resets.
    pub reset_in_secs: u64,
}

// ── Rate-limit error ──────────────────────────────────────────────────────────

/// Errors returned by `RateLimiter::check_and_record`.
#[derive(Debug, Clone)]
pub enum RateLimitError {
    /// The caller has exceeded their quota.
    TooManyRequests {
        used: u64,
        limit: u64,
        reset_in_secs: u64,
    },
    /// The endpoint is not registered in the limiter.
    UnknownEndpoint(String),
}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyRequests {
                used,
                limit,
                reset_in_secs,
            } => write!(
                f,
                "rate limit exceeded: {used}/{limit} requests used; resets in {reset_in_secs}s"
            ),
            Self::UnknownEndpoint(ep) => write!(f, "unknown endpoint: {ep}"),
        }
    }
}

// ── RateLimiter ───────────────────────────────────────────────────────────────

/// Central rate-limiter: holds endpoint configs and per-user quota state.
///
/// # Example
/// ```
/// # use std::time::Duration;
/// # use heirloom_protocol_backend::rate_limit::{RateLimiter, EndpointConfig, TierLimit, UserTier};
/// let mut limiter = RateLimiter::new(TierLimit::new(100, Duration::from_secs(60)));
///
/// let mut cfg = EndpointConfig::new();
/// cfg.set_tier_limit(UserTier::Free, TierLimit::new(5, Duration::from_secs(60)));
/// cfg.set_tier_limit(UserTier::Pro, TierLimit::new(50, Duration::from_secs(60)));
/// limiter.register_endpoint("POST /api/vaults", cfg);
///
/// let status = limiter
///     .check_and_record("user_123", UserTier::Free, "POST /api/vaults")
///     .unwrap();
/// assert_eq!(status.used, 1);
/// assert_eq!(status.remaining, 4);
/// ```
pub struct RateLimiter {
    /// Per-endpoint configuration.
    endpoints: HashMap<String, EndpointConfig>,
    /// Global default limit used when an endpoint has no tier-specific entry.
    default_limit: TierLimit,
    /// Quota table: `"{user_id}:{endpoint}"` → `QuotaEntry`.
    quotas: Mutex<HashMap<String, QuotaEntry>>,
}

impl RateLimiter {
    /// Create a new limiter with the given global default tier limit.
    pub fn new(default_limit: TierLimit) -> Self {
        Self {
            endpoints: HashMap::new(),
            default_limit,
            quotas: Mutex::new(HashMap::new()),
        }
    }

    /// Register an endpoint with its per-tier configuration.
    pub fn register_endpoint(&mut self, endpoint: impl Into<String>, config: EndpointConfig) {
        self.endpoints.insert(endpoint.into(), config);
    }

    /// Look up the effective `TierLimit` for `(endpoint, tier)`.
    ///
    /// Falls back to the global default if the endpoint has no entry for the
    /// given tier.  Returns `Err(UnknownEndpoint)` if the endpoint is not
    /// registered at all.
    pub fn resolve_limit(
        &self,
        endpoint: &str,
        tier: UserTier,
    ) -> Result<TierLimit, RateLimitError> {
        // Admin always gets unlimited.
        if tier == UserTier::Admin {
            return Ok(TierLimit::unlimited());
        }

        match self.endpoints.get(endpoint) {
            Some(cfg) => Ok(cfg.tier_limit(tier).unwrap_or(self.default_limit)),
            None => Err(RateLimitError::UnknownEndpoint(endpoint.to_string())),
        }
    }

    /// Check the quota for `(user_id, tier, endpoint)` and, if allowed,
    /// record the request.
    ///
    /// # Returns
    /// - `Ok(QuotaStatus)` if the request is within quota.
    /// - `Err(RateLimitError::TooManyRequests { .. })` if the quota is exhausted.
    /// - `Err(RateLimitError::UnknownEndpoint(_))` if the endpoint is not registered.
    pub fn check_and_record(
        &self,
        user_id: &str,
        tier: UserTier,
        endpoint: &str,
    ) -> Result<QuotaStatus, RateLimitError> {
        let limit = self.resolve_limit(endpoint, tier)?;
        let quota_key = format!("{user_id}:{endpoint}");

        let mut quotas = self.quotas.lock().unwrap();
        let entry = quotas
            .entry(quota_key)
            .or_insert_with(|| QuotaEntry::new(limit.window));

        // Reset the window if it has expired.
        entry.maybe_reset(limit.window);

        if entry.count >= limit.max_requests {
            return Err(RateLimitError::TooManyRequests {
                used: entry.count,
                limit: limit.max_requests,
                reset_in_secs: entry.seconds_until_reset(),
            });
        }

        entry.count += 1;
        let used = entry.count;
        let reset_in_secs = entry.seconds_until_reset();

        Ok(QuotaStatus {
            used,
            limit: limit.max_requests,
            remaining: limit.max_requests.saturating_sub(used),
            reset_in_secs,
        })
    }

    /// Return the current quota status for `(user_id, endpoint)` without
    /// consuming a request.
    ///
    /// Returns `None` if no requests have been made yet for this pair.
    pub fn quota_status(
        &self,
        user_id: &str,
        tier: UserTier,
        endpoint: &str,
    ) -> Result<Option<QuotaStatus>, RateLimitError> {
        let limit = self.resolve_limit(endpoint, tier)?;
        let quota_key = format!("{user_id}:{endpoint}");

        let mut quotas = self.quotas.lock().unwrap();
        match quotas.get_mut(&quota_key) {
            Some(entry) => {
                entry.maybe_reset(limit.window);
                Ok(Some(QuotaStatus {
                    used: entry.count,
                    limit: limit.max_requests,
                    remaining: limit.max_requests.saturating_sub(entry.count),
                    reset_in_secs: entry.seconds_until_reset(),
                }))
            }
            None => Ok(None),
        }
    }

    /// Reset the quota for a specific `(user_id, endpoint)`.  Useful for
    /// testing or administrative override.
    pub fn reset_quota(&self, user_id: &str, endpoint: &str) {
        let quota_key = format!("{user_id}:{endpoint}");
        self.quotas.lock().unwrap().remove(&quota_key);
    }

    /// Reset all quota state (all users, all endpoints).
    pub fn reset_all(&self) {
        self.quotas.lock().unwrap().clear();
    }

    /// Return the number of active quota entries.
    pub fn active_quota_count(&self) -> usize {
        self.quotas.lock().unwrap().len()
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const ENDPOINT: &str = "POST /api/vaults";

    fn make_limiter(max: u64, window_secs: u64) -> RateLimiter {
        let default = TierLimit::new(max, Duration::from_secs(window_secs));
        let mut limiter = RateLimiter::new(default);

        let mut cfg = EndpointConfig::new();
        cfg.set_tier_limit(UserTier::Free, TierLimit::new(3, Duration::from_secs(60)));
        cfg.set_tier_limit(UserTier::Pro, TierLimit::new(20, Duration::from_secs(60)));
        cfg.set_tier_limit(
            UserTier::Enterprise,
            TierLimit::new(200, Duration::from_secs(60)),
        );
        limiter.register_endpoint(ENDPOINT, cfg);
        limiter
    }

    // ── Basic allow / deny ────────────────────────────────────────────────────

    #[test]
    fn test_first_request_is_allowed() {
        let limiter = make_limiter(100, 60);
        let status = limiter
            .check_and_record("user1", UserTier::Free, ENDPOINT)
            .unwrap();
        assert_eq!(status.used, 1);
        assert_eq!(status.remaining, 2);
        assert_eq!(status.limit, 3);
    }

    #[test]
    fn test_request_denied_after_limit_reached() {
        let limiter = make_limiter(100, 60);
        for _ in 0..3 {
            limiter
                .check_and_record("user1", UserTier::Free, ENDPOINT)
                .unwrap();
        }
        let err = limiter
            .check_and_record("user1", UserTier::Free, ENDPOINT)
            .unwrap_err();
        assert!(
            matches!(err, RateLimitError::TooManyRequests { used: 3, limit: 3, .. }),
            "expected TooManyRequests, got: {err}"
        );
    }

    #[test]
    fn test_different_users_have_independent_quotas() {
        let limiter = make_limiter(100, 60);
        for _ in 0..3 {
            limiter
                .check_and_record("user1", UserTier::Free, ENDPOINT)
                .unwrap();
        }
        // user2 has not hit the limit.
        let status = limiter
            .check_and_record("user2", UserTier::Free, ENDPOINT)
            .unwrap();
        assert_eq!(status.used, 1);
    }

    // ── User tiers ────────────────────────────────────────────────────────────

    #[test]
    fn test_pro_tier_higher_limit_than_free() {
        let limiter = make_limiter(100, 60);
        let limit = limiter.resolve_limit(ENDPOINT, UserTier::Pro).unwrap();
        let free_limit = limiter.resolve_limit(ENDPOINT, UserTier::Free).unwrap();
        assert!(limit.max_requests > free_limit.max_requests);
    }

    #[test]
    fn test_admin_tier_always_unlimited() {
        let limiter = make_limiter(1, 60); // Even with a very low global default.
        // Admin should never be rate-limited.
        for _ in 0..1000 {
            limiter
                .check_and_record("admin_user", UserTier::Admin, ENDPOINT)
                .unwrap();
        }
    }

    #[test]
    fn test_enterprise_tier_limit() {
        let limiter = make_limiter(100, 60);
        let limit = limiter
            .resolve_limit(ENDPOINT, UserTier::Enterprise)
            .unwrap();
        assert_eq!(limit.max_requests, 200);
    }

    // ── Unknown endpoint ──────────────────────────────────────────────────────

    #[test]
    fn test_unknown_endpoint_returns_error() {
        let limiter = make_limiter(100, 60);
        let err = limiter
            .check_and_record("user1", UserTier::Free, "UNKNOWN /does/not/exist")
            .unwrap_err();
        assert!(matches!(err, RateLimitError::UnknownEndpoint(_)));
    }

    #[test]
    fn test_resolve_limit_unknown_endpoint_is_error() {
        let limiter = make_limiter(100, 60);
        assert!(limiter
            .resolve_limit("UNKNOWN /endpoint", UserTier::Free)
            .is_err());
    }

    // ── Configurable thresholds ───────────────────────────────────────────────

    #[test]
    fn test_endpoint_specific_limit_overrides_default() {
        // Global default: 100; Free on ENDPOINT: 3.
        let limiter = make_limiter(100, 60);
        // Free tier should hit 3, not 100.
        for _ in 0..3 {
            limiter
                .check_and_record("user1", UserTier::Free, ENDPOINT)
                .unwrap();
        }
        assert!(limiter
            .check_and_record("user1", UserTier::Free, ENDPOINT)
            .is_err());
    }

    #[test]
    fn test_global_default_used_when_tier_not_configured() {
        // Register an endpoint with only a Free tier limit.
        let default = TierLimit::new(10, Duration::from_secs(60));
        let mut limiter = RateLimiter::new(default);
        let mut cfg = EndpointConfig::new();
        cfg.set_tier_limit(UserTier::Free, TierLimit::new(2, Duration::from_secs(60)));
        limiter.register_endpoint("GET /api/vaults", cfg);

        // Pro tier is not configured → falls back to default (10).
        let limit = limiter
            .resolve_limit("GET /api/vaults", UserTier::Pro)
            .unwrap();
        assert_eq!(limit.max_requests, 10);
    }

    // ── Quota tracking ────────────────────────────────────────────────────────

    #[test]
    fn test_quota_status_none_before_any_request() {
        let limiter = make_limiter(100, 60);
        let status = limiter
            .quota_status("user1", UserTier::Free, ENDPOINT)
            .unwrap();
        assert!(status.is_none());
    }

    #[test]
    fn test_quota_status_after_requests() {
        let limiter = make_limiter(100, 60);
        limiter
            .check_and_record("user1", UserTier::Free, ENDPOINT)
            .unwrap();
        limiter
            .check_and_record("user1", UserTier::Free, ENDPOINT)
            .unwrap();
        let status = limiter
            .quota_status("user1", UserTier::Free, ENDPOINT)
            .unwrap()
            .unwrap();
        assert_eq!(status.used, 2);
        assert_eq!(status.remaining, 1);
    }

    #[test]
    fn test_quota_status_does_not_consume_request() {
        let limiter = make_limiter(100, 60);
        limiter
            .check_and_record("user1", UserTier::Free, ENDPOINT)
            .unwrap();
        // Check status twice; count should not change.
        let s1 = limiter
            .quota_status("user1", UserTier::Free, ENDPOINT)
            .unwrap()
            .unwrap();
        let s2 = limiter
            .quota_status("user1", UserTier::Free, ENDPOINT)
            .unwrap()
            .unwrap();
        assert_eq!(s1.used, s2.used);
    }

    // ── Reset ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_reset_quota_for_user() {
        let limiter = make_limiter(100, 60);
        for _ in 0..3 {
            limiter
                .check_and_record("user1", UserTier::Free, ENDPOINT)
                .unwrap();
        }
        limiter.reset_quota("user1", ENDPOINT);
        // Should be allowed again.
        assert!(limiter
            .check_and_record("user1", UserTier::Free, ENDPOINT)
            .is_ok());
    }

    #[test]
    fn test_reset_all() {
        let limiter = make_limiter(100, 60);
        limiter
            .check_and_record("user1", UserTier::Free, ENDPOINT)
            .unwrap();
        limiter
            .check_and_record("user2", UserTier::Free, ENDPOINT)
            .unwrap();
        limiter.reset_all();
        assert_eq!(limiter.active_quota_count(), 0);
    }

    // ── Active quota count ────────────────────────────────────────────────────

    #[test]
    fn test_active_quota_count_increments_per_unique_pair() {
        let limiter = make_limiter(100, 60);
        limiter
            .check_and_record("user1", UserTier::Free, ENDPOINT)
            .unwrap();
        limiter
            .check_and_record("user2", UserTier::Free, ENDPOINT)
            .unwrap();
        assert_eq!(limiter.active_quota_count(), 2);
    }

    #[test]
    fn test_repeated_requests_same_user_same_endpoint_single_entry() {
        let limiter = make_limiter(100, 60);
        limiter
            .check_and_record("user1", UserTier::Free, ENDPOINT)
            .unwrap();
        limiter
            .check_and_record("user1", UserTier::Free, ENDPOINT)
            .unwrap();
        assert_eq!(limiter.active_quota_count(), 1);
    }

    // ── Window reset (time-based) ─────────────────────────────────────────────

    #[test]
    fn test_quota_resets_after_window_expires() {
        // Use a very short window so we can observe the reset.
        let default = TierLimit::new(100, Duration::from_secs(60));
        let mut limiter = RateLimiter::new(default);
        let mut cfg = EndpointConfig::new();
        cfg.set_tier_limit(
            UserTier::Free,
            TierLimit::new(2, Duration::from_millis(50)),
        );
        limiter.register_endpoint(ENDPOINT, cfg);

        // Exhaust the 2-request quota.
        limiter
            .check_and_record("user1", UserTier::Free, ENDPOINT)
            .unwrap();
        limiter
            .check_and_record("user1", UserTier::Free, ENDPOINT)
            .unwrap();
        assert!(limiter
            .check_and_record("user1", UserTier::Free, ENDPOINT)
            .is_err());

        // Wait for the window to expire.
        std::thread::sleep(Duration::from_millis(100));

        // Should be allowed again.
        assert!(limiter
            .check_and_record("user1", UserTier::Free, ENDPOINT)
            .is_ok());
    }

    // ── Error formatting ──────────────────────────────────────────────────────

    #[test]
    fn test_too_many_requests_error_display() {
        let err = RateLimitError::TooManyRequests {
            used: 5,
            limit: 5,
            reset_in_secs: 30,
        };
        let s = err.to_string();
        assert!(s.contains("rate limit exceeded"));
        assert!(s.contains("5/5"));
    }

    #[test]
    fn test_unknown_endpoint_error_display() {
        let err = RateLimitError::UnknownEndpoint("/bad".to_string());
        assert!(err.to_string().contains("unknown endpoint"));
    }

    // ── Multiple endpoints ────────────────────────────────────────────────────

    #[test]
    fn test_different_endpoints_have_independent_quotas() {
        let default = TierLimit::new(100, Duration::from_secs(60));
        let mut limiter = RateLimiter::new(default);

        let mut cfg1 = EndpointConfig::new();
        cfg1.set_tier_limit(UserTier::Free, TierLimit::new(2, Duration::from_secs(60)));
        limiter.register_endpoint("GET /api/vaults", cfg1);

        let mut cfg2 = EndpointConfig::new();
        cfg2.set_tier_limit(UserTier::Free, TierLimit::new(2, Duration::from_secs(60)));
        limiter.register_endpoint("POST /api/checkin", cfg2);

        // Exhaust /api/vaults.
        limiter
            .check_and_record("user1", UserTier::Free, "GET /api/vaults")
            .unwrap();
        limiter
            .check_and_record("user1", UserTier::Free, "GET /api/vaults")
            .unwrap();
        assert!(limiter
            .check_and_record("user1", UserTier::Free, "GET /api/vaults")
            .is_err());

        // /api/checkin is still fresh.
        assert!(limiter
            .check_and_record("user1", UserTier::Free, "POST /api/checkin")
            .is_ok());
    }
}

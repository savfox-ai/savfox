use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

/// Token-bucket rate limiter for gateway connections and requests.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<RateLimiterInner>>,
    config: RateLimitConfig,
}

#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Maximum requests per window.
    pub max_requests: u32,
    /// Window duration.
    pub window: Duration,
    /// Maximum concurrent WebSocket connections per IP.
    pub max_connections_per_ip: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_requests: 100,
            window: Duration::from_secs(60),
            max_connections_per_ip: 10,
        }
    }
}

#[derive(Debug)]
struct RateLimiterInner {
    /// Per-IP request buckets.
    ip_buckets: HashMap<IpAddr, Bucket>,
    /// Per-token request buckets.
    token_buckets: HashMap<String, Bucket>,
    /// Per-IP active connection count.
    ip_connections: HashMap<IpAddr, u32>,
}

#[derive(Debug)]
struct Bucket {
    tokens: u32,
    last_refill: Instant,
}

impl RateLimiter {
    #[must_use]
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RateLimiterInner {
                ip_buckets: HashMap::new(),
                token_buckets: HashMap::new(),
                ip_connections: HashMap::new(),
            })),
            config,
        }
    }

    /// Check if a request from the given IP is allowed.
    pub async fn check_ip(&self, ip: IpAddr) -> bool {
        let mut inner = self.inner.lock().await;
        let bucket = inner.ip_buckets.entry(ip).or_insert_with(|| Bucket {
            tokens: self.config.max_requests,
            last_refill: Instant::now(),
        });

        Self::refill(bucket, &self.config);

        if bucket.tokens > 0 {
            bucket.tokens -= 1;
            true
        } else {
            false
        }
    }

    /// Check if a request for the given token is allowed.
    pub async fn check_token(&self, token: &str) -> bool {
        let mut inner = self.inner.lock().await;
        let bucket = inner
            .token_buckets
            .entry(token.to_owned())
            .or_insert_with(|| Bucket {
                tokens: self.config.max_requests,
                last_refill: Instant::now(),
            });

        Self::refill(bucket, &self.config);

        if bucket.tokens > 0 {
            bucket.tokens -= 1;
            true
        } else {
            false
        }
    }

    /// Check if a new connection from the given IP is allowed without
    /// reserving a slot. Prefer [`Self::try_add_connection`] for atomic
    /// check-and-add at the WS upgrade site.
    pub async fn check_connection(&self, ip: IpAddr) -> bool {
        let inner = self.inner.lock().await;
        let count = inner.ip_connections.get(&ip).copied().unwrap_or(0);
        count < self.config.max_connections_per_ip
    }

    /// Atomically check-and-reserve a connection slot for `ip`.
    ///
    /// Returns `true` if the slot was reserved and the caller should
    /// proceed (and later call [`Self::remove_connection`] on shutdown).
    /// Returns `false` if the per-IP cap has been reached — the caller
    /// must reject the upgrade.
    ///
    /// Closes M19 in the security review: previously, two concurrent
    /// callers could both call `check_connection`, both observe
    /// `count < max`, and both call `add_connection` — exceeding the cap.
    pub async fn try_add_connection(&self, ip: IpAddr) -> bool {
        let mut inner = self.inner.lock().await;
        let entry = inner.ip_connections.entry(ip).or_insert(0);
        if *entry >= self.config.max_connections_per_ip {
            return false;
        }
        *entry += 1;
        true
    }

    /// Register a new connection from an IP. Prefer
    /// [`Self::try_add_connection`] which combines the check and the
    /// reservation under a single lock acquisition.
    pub async fn add_connection(&self, ip: IpAddr) {
        let mut inner = self.inner.lock().await;
        *inner.ip_connections.entry(ip).or_insert(0) += 1;
    }

    /// Unregister a connection from an IP.
    pub async fn remove_connection(&self, ip: IpAddr) {
        let mut inner = self.inner.lock().await;
        if let Some(count) = inner.ip_connections.get_mut(&ip) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                inner.ip_connections.remove(&ip);
            }
        }
    }

    /// Drop request-bucket entries that haven't been touched in
    /// `2 * window`. Without this the per-IP and per-token HashMaps grow
    /// unbounded under attack scenarios that rotate addresses or tokens.
    /// Should be called periodically from a maintenance task.
    pub async fn evict_stale_buckets(&self) -> EvictReport {
        let mut inner = self.inner.lock().await;
        let stale_after = self.config.window.saturating_mul(2);
        let now = Instant::now();
        let before_ip = inner.ip_buckets.len();
        let before_token = inner.token_buckets.len();
        inner
            .ip_buckets
            .retain(|_, bucket| now.duration_since(bucket.last_refill) < stale_after);
        inner
            .token_buckets
            .retain(|_, bucket| now.duration_since(bucket.last_refill) < stale_after);
        EvictReport {
            ip_buckets_pruned: before_ip - inner.ip_buckets.len(),
            token_buckets_pruned: before_token - inner.token_buckets.len(),
        }
    }

    fn refill(bucket: &mut Bucket, config: &RateLimitConfig) {
        let elapsed = bucket.last_refill.elapsed();
        if elapsed >= config.window {
            bucket.tokens = config.max_requests;
            bucket.last_refill = Instant::now();
        }
    }
}

/// Counts of bucket entries removed by [`RateLimiter::evict_stale_buckets`].
#[derive(Debug, Default, Clone, Copy)]
pub struct EvictReport {
    pub ip_buckets_pruned: usize,
    pub token_buckets_pruned: usize,
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    fn fast_config() -> RateLimitConfig {
        RateLimitConfig {
            max_requests: 3,
            window: Duration::from_millis(80),
            max_connections_per_ip: 2,
        }
    }

    fn ip(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, n))
    }

    #[tokio::test]
    async fn check_ip_consumes_tokens_then_blocks() {
        let limiter = RateLimiter::new(fast_config());
        let addr = ip(1);
        // 3 requests should fit the bucket; the 4th must be rejected.
        for i in 0..3 {
            assert!(limiter.check_ip(addr).await, "request {i} should pass");
        }
        assert!(!limiter.check_ip(addr).await, "4th request must be blocked");
    }

    #[tokio::test]
    async fn check_ip_refills_after_window() {
        let cfg = fast_config();
        let window = cfg.window;
        let limiter = RateLimiter::new(cfg);
        let addr = ip(2);
        for _ in 0..3 {
            assert!(limiter.check_ip(addr).await);
        }
        assert!(!limiter.check_ip(addr).await);
        // Sleep past the window so the bucket refills on next check.
        tokio::time::sleep(window + Duration::from_millis(20)).await;
        assert!(
            limiter.check_ip(addr).await,
            "request after window must be allowed again"
        );
    }

    #[tokio::test]
    async fn check_ip_isolates_per_address() {
        let limiter = RateLimiter::new(fast_config());
        let a = ip(3);
        let b = ip(4);
        for _ in 0..3 {
            assert!(limiter.check_ip(a).await);
        }
        assert!(!limiter.check_ip(a).await);
        // b must still have its full bucket.
        for _ in 0..3 {
            assert!(limiter.check_ip(b).await, "b's bucket must be untouched");
        }
    }

    #[tokio::test]
    async fn check_token_is_isolated_from_ip_buckets() {
        let limiter = RateLimiter::new(fast_config());
        let addr = ip(5);
        // Drain the IP bucket.
        for _ in 0..3 {
            assert!(limiter.check_ip(addr).await);
        }
        assert!(!limiter.check_ip(addr).await);
        // Token bucket should still be full.
        for _ in 0..3 {
            assert!(limiter.check_token("tok-A").await);
        }
        assert!(!limiter.check_token("tok-A").await);
    }

    #[tokio::test]
    async fn add_connection_enforces_per_ip_cap() {
        let limiter = RateLimiter::new(fast_config());
        let addr = ip(6);
        assert!(limiter.check_connection(addr).await);
        limiter.add_connection(addr).await;
        assert!(limiter.check_connection(addr).await);
        limiter.add_connection(addr).await;
        // Now at cap (2). A new check must fail until one is released.
        assert!(!limiter.check_connection(addr).await);
        limiter.remove_connection(addr).await;
        assert!(limiter.check_connection(addr).await);
    }

    #[tokio::test]
    async fn remove_connection_clamps_at_zero() {
        let limiter = RateLimiter::new(fast_config());
        let addr = ip(7);
        // Calling remove on an IP we never added must not panic and must not
        // wrap around to u32::MAX.
        limiter.remove_connection(addr).await;
        // Adding still works after the no-op remove.
        limiter.add_connection(addr).await;
        limiter.remove_connection(addr).await;
        // After full release the entry is removed; counter is back to 0.
        assert!(limiter.check_connection(addr).await);
    }

    /// `check_connection` followed by `add_connection` is intentionally
    /// non-atomic: two concurrent callers can both observe `count < max`
    /// and both add, exceeding the cap by one. Real call sites must use
    /// [`RateLimiter::try_add_connection`] (added by this PR) which
    /// performs both steps under a single lock.
    #[tokio::test]
    async fn check_then_add_is_not_atomic_today() {
        let limiter = RateLimiter::new(RateLimitConfig {
            max_connections_per_ip: 1,
            ..fast_config()
        });
        let addr = ip(8);
        assert!(limiter.check_connection(addr).await);
        // Race simulation: both callers pass the check, then both add.
        assert!(limiter.check_connection(addr).await);
        limiter.add_connection(addr).await;
        limiter.add_connection(addr).await;
        // Cap has been exceeded — count is 2 even though the limit is 1.
        assert!(!limiter.check_connection(addr).await);
    }

    #[tokio::test]
    async fn try_add_connection_is_atomic() {
        let limiter = RateLimiter::new(RateLimitConfig {
            max_connections_per_ip: 1,
            ..fast_config()
        });
        let addr = ip(9);
        assert!(limiter.try_add_connection(addr).await, "first must succeed");
        assert!(
            !limiter.try_add_connection(addr).await,
            "second must be rejected at the cap"
        );
        // After release a slot opens back up.
        limiter.remove_connection(addr).await;
        assert!(limiter.try_add_connection(addr).await);
    }

    #[tokio::test]
    async fn try_add_connection_under_concurrent_callers_respects_cap() {
        // Stress: 100 concurrent try_add calls, cap = 5. Exactly 5 should
        // succeed; the rest must fail.
        let limiter = std::sync::Arc::new(RateLimiter::new(RateLimitConfig {
            max_connections_per_ip: 5,
            ..fast_config()
        }));
        let addr = ip(10);
        let mut handles = Vec::new();
        for _ in 0..100 {
            let lim = limiter.clone();
            handles.push(tokio::spawn(
                async move { lim.try_add_connection(addr).await },
            ));
        }
        let mut accepted = 0usize;
        for h in handles {
            if h.await.unwrap() {
                accepted += 1;
            }
        }
        assert_eq!(accepted, 5, "exactly 5 should be accepted under the cap");
    }

    #[tokio::test]
    async fn evict_stale_buckets_drops_old_ip_and_token_entries() {
        let cfg = RateLimitConfig {
            window: Duration::from_millis(50),
            ..fast_config()
        };
        let stale_after = cfg.window * 2;
        let limiter = RateLimiter::new(cfg);
        // Touch some buckets, then sleep past 2*window so they go stale.
        let _ = limiter.check_ip(ip(11)).await;
        let _ = limiter.check_token("old-token").await;
        tokio::time::sleep(stale_after + Duration::from_millis(20)).await;
        // Add a fresh bucket so we can confirm only stale ones are pruned.
        let _ = limiter.check_ip(ip(12)).await;

        let report = limiter.evict_stale_buckets().await;
        assert_eq!(report.ip_buckets_pruned, 1);
        assert_eq!(report.token_buckets_pruned, 1);
        // The fresh ip(12) bucket is still there: another check must
        // consume from the existing bucket, not create a new one.
        let inner = limiter.inner.lock().await;
        assert!(inner.ip_buckets.contains_key(&ip(12)));
        assert!(!inner.ip_buckets.contains_key(&ip(11)));
    }
}

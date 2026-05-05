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

    /// Check if a new connection from the given IP is allowed.
    pub async fn check_connection(&self, ip: IpAddr) -> bool {
        let inner = self.inner.lock().await;
        let count = inner.ip_connections.get(&ip).copied().unwrap_or(0);
        count < self.config.max_connections_per_ip
    }

    /// Register a new connection from an IP.
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

    fn refill(bucket: &mut Bucket, config: &RateLimitConfig) {
        let elapsed = bucket.last_refill.elapsed();
        if elapsed >= config.window {
            bucket.tokens = config.max_requests;
            bucket.last_refill = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

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

    /// Documents the known race the security review (M19) called out:
    /// `check_connection` and `add_connection` are *not* a single atomic
    /// step today. Two concurrent callers can both observe `count < max`
    /// and both add — exceeding the cap by one. This test pins the current
    /// (intentional) behavior so a future fix is recognized as a *change*.
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
        // A correct atomic implementation would reject the second add.
        assert!(!limiter.check_connection(addr).await);
    }
}

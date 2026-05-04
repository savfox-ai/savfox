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

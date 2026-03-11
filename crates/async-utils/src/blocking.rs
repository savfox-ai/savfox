//! Helpers for offloading blocking work to the Tokio blocking thread pool.
//!
//! Both [`spawn_cpu_bound`] and [`spawn_io`] delegate to
//! [`tokio::task::spawn_blocking`].  The two entry-points exist so that
//! call-sites convey *intent* — readers of the code can immediately tell
//! whether a blocking section is compute-heavy or IO-heavy, which matters
//! when reasoning about thread-pool saturation and back-pressure.

use tokio::task::JoinHandle;

/// Spawn a CPU-bound task on the blocking thread pool.
///
/// Use this for computation-heavy operations that would block the async
/// runtime (e.g., hashing, serialization of large payloads, compression).
///
/// # Example
///
/// ```rust
/// # async fn example() -> Result<(), tokio::task::JoinError> {
/// let result = savfox_async_utils::spawn_cpu_bound(|| {
///     // expensive computation
///     42
/// })
/// .await?;
/// assert_eq!(result, 42);
/// # Ok(())
/// # }
/// ```
pub fn spawn_cpu_bound<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
}

/// Spawn an IO-bound blocking task on the blocking thread pool.
///
/// Use this for synchronous IO operations (file reads, blocking HTTP calls,
/// database drivers without async support) that would block the async runtime.
///
/// # Example
///
/// ```rust
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let content = savfox_async_utils::spawn_io(|| {
///     std::fs::read_to_string("Cargo.toml")
/// })
/// .await??;
/// # Ok(())
/// # }
/// ```
pub fn spawn_io<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_cpu_bound_returns_value() {
        let result = spawn_cpu_bound(|| 1 + 2).await.expect("task panicked");
        assert_eq!(result, 3);
    }

    #[tokio::test]
    async fn spawn_io_returns_value() {
        let result = spawn_io(|| String::from("hello"))
            .await
            .expect("task panicked");
        assert_eq!(result, "hello");
    }
}

//! Async utilities for the Savfox ecosystem.
//!
//! This crate provides common async patterns and utilities used across
//! Savfox crates, particularly around cancellation and error handling.
//!
//! # Key Features
//!
//! - **Cancellation Support**: [`OrCancelExt`] trait for cancelable futures
//! - **Error Types**: [`CancelErr`] for cancellation errors
//!
//! # Example
//!
//! ```rust
//! use savfox_async_utils::OrCancelExt;
//! use tokio_util::sync::CancellationToken;
//!
//! # async fn example() -> Result<(), savfox_async_utils::CancelErr> {
//! let token = CancellationToken::new();
//!
//! // A long-running operation that can be cancelled
//! let result = async {
//!     // Some work here
//!     42
//! }
//! .or_cancel(&token)
//! .await?;
//!
//! # Ok(())
//! # }
//! ```

use std::future::Future;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

/// Error returned when an operation is cancelled.
///
/// This is typically returned when a [`CancellationToken`] is triggered
/// while a future is still pending.
#[derive(Debug, PartialEq, Eq)]
pub enum CancelErr {
    /// The operation was cancelled via a cancellation token.
    Cancelled,
}

/// Extension trait for adding cancellation support to futures.
///
/// This trait allows any future to be cancelled via a [`CancellationToken`].
///
/// # Example
///
/// ```rust
/// use savfox_async_utils::OrCancelExt;
/// use tokio_util::sync::CancellationToken;
///
/// # async fn example() {
/// let token = CancellationToken::new();
///
/// let result = async { 42 }.or_cancel(&token).await;
///
/// assert_eq!(result, Ok(42));
/// # }
/// ```
#[async_trait]
pub trait OrCancelExt: Sized {
    /// The output type of the future.
    type Output;

    /// Execute this future or cancel if the token is triggered.
    ///
    /// Returns `Ok(output)` if the future completes before cancellation,
    /// or `Err(CancelErr::Cancelled)` if the token is cancelled first.
    ///
    /// # Example
    ///
    /// ```rust
    /// use savfox_async_utils::OrCancelExt;
    /// use tokio_util::sync::CancellationToken;
    ///
    /// # async fn example() {
    /// let token = CancellationToken::new();
    /// token.cancel();
    ///
    /// let result = async { 42 }.or_cancel(&token).await;
    ///
    /// assert_eq!(result, Err(savfox_async_utils::CancelErr::Cancelled));
    /// # }
    /// ```
    async fn or_cancel(self, token: &CancellationToken) -> Result<Self::Output, CancelErr>;
}

#[async_trait]
impl<F> OrCancelExt for F
where
    F: Future + Send,
    F::Output: Send,
{
    type Output = F::Output;

    async fn or_cancel(self, token: &CancellationToken) -> Result<Self::Output, CancelErr> {
        tokio::select! {
            _ = token.cancelled() => Err(CancelErr::Cancelled),
            res = self => Ok(res),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pretty_assertions::assert_eq;
    use tokio::task;
    use tokio::time::sleep;

    use super::*;

    #[tokio::test]
    async fn returns_ok_when_future_completes_first() {
        let token = CancellationToken::new();
        let value = async { 42 };

        let result = value.or_cancel(&token).await;

        assert_eq!(Ok(42), result);
    }

    #[tokio::test]
    async fn returns_err_when_token_cancelled_first() {
        let token = CancellationToken::new();
        let token_clone = token.clone();

        let cancel_handle = task::spawn(async move {
            sleep(Duration::from_millis(10)).await;
            token_clone.cancel();
        });

        let result = async {
            sleep(Duration::from_millis(100)).await;
            7
        }
        .or_cancel(&token)
        .await;

        cancel_handle.await.expect("cancel task panicked");
        assert_eq!(Err(CancelErr::Cancelled), result);
    }

    #[tokio::test]
    async fn returns_err_when_token_already_cancelled() {
        let token = CancellationToken::new();
        token.cancel();

        let result = async {
            sleep(Duration::from_millis(50)).await;
            5
        }
        .or_cancel(&token)
        .await;

        assert_eq!(Err(CancelErr::Cancelled), result);
    }
}

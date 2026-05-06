use std::fmt;
use std::future::Future;

use futures::future::{BoxFuture, FutureExt, Shared};

use crate::config_loader::ConfigRequirementsToml;

/// Lazy, shared loader for cloud-side config requirements.
///
/// # Fail-open behaviour ⚠️
///
/// The loader currently surfaces failures as `None` (see [`Self::get`]).
/// Callers that "if `Some`, enforce" effectively **fail open** when the
/// cloud is unreachable: a transient network blip or an outage on the
/// requirements service silently disables enforcement.
///
/// This is the historical behaviour the original TODO(gt) flagged as
/// needing a fail-closed `Result`-returning alternative (S17 in the
/// security review). Reworking the public type to `Result` cascades
/// through the entire `Config` builder API; the lower-impact
/// intermediate is to:
///
/// 1. Keep [`Self::get`] returning `Option` for back-compat.
/// 2. Document the fail-open behaviour loudly on the public type so downstream callers know to
///    treat `None` as "couldn't load" and decide for themselves whether that should fail closed.
/// 3. Track a follow-up to add a `try_get -> Result<...>` once one caller actually needs
///    fail-closed semantics, at which point we can plumb the error through the new path without
///    breaking every existing builder.
///
/// **Operators that need strict fail-closed behaviour today should
/// pre-load the requirements at startup and refuse to launch when the
/// cloud lookup fails — relying on `get()` returning `Some` is not
/// sufficient.**
#[derive(Clone)]
pub struct CloudRequirementsLoader {
    fut: Shared<BoxFuture<'static, Option<ConfigRequirementsToml>>>,
}

impl CloudRequirementsLoader {
    pub fn new<F>(fut: F) -> Self
    where
        F: Future<Output = Option<ConfigRequirementsToml>> + Send + 'static,
    {
        Self {
            fut: fut.boxed().shared(),
        }
    }

    /// Wait for the cloud requirements to load and return them.
    ///
    /// **Returns `None` for both "cloud explicitly returned no
    /// requirements" and "we failed to reach the cloud."** Callers that
    /// need to distinguish those cases must wrap their own loader future
    /// to capture the underlying error before passing it to [`Self::new`].
    /// See the type-level docs for the fail-open caveat.
    pub async fn get(&self) -> Option<ConfigRequirementsToml> {
        self.fut.clone().await
    }
}

impl fmt::Debug for CloudRequirementsLoader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CloudRequirementsLoader").finish()
    }
}

impl Default for CloudRequirementsLoader {
    fn default() -> Self {
        Self::new(async { None })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use pretty_assertions::assert_eq;

    use super::*;

    #[tokio::test]
    async fn shared_future_runs_once() {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);
        let loader = CloudRequirementsLoader::new(async move {
            counter_clone.fetch_add(1, Ordering::SeqCst);
            Some(ConfigRequirementsToml::default())
        });

        let (first, second) = tokio::join!(loader.get(), loader.get());
        assert_eq!(first, second);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}

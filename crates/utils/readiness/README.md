# savfox-utils-readiness

Implements a one-shot readiness flag (`ReadinessFlag`) with token-based authorization and async waiting, built on Tokio primitives. Components that need to gate startup or initialization can subscribe to the flag to receive a `Token`, and the flag transitions to ready only when a valid token holder calls `mark_ready`. If no subscriptions are active when `is_ready()` is checked, the flag is automatically marked ready.

The `Readiness` trait abstracts the interface (`is_ready`, `subscribe`, `mark_ready`, `wait_ready`) for testability and alternative implementations. Internally, the flag uses an `AtomicBool` for cheap reads, a `Mutex<HashSet<Token>>` for subscription management, and a `watch` channel to broadcast readiness to async waiters. Once ready, the state is irreversible -- further subscriptions return `FlagAlreadyReady`.

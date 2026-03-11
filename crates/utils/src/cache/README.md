# savfox-utils::cache

Provides `BlockingLruCache<K, V>`, a thread-safe LRU cache backed by the `lru` crate and protected by a Tokio mutex. All cache operations are synchronous from the caller's perspective: when a Tokio runtime is active the mutex is acquired via `block_in_place`, and when no runtime is present every operation gracefully degrades to a no-op (reads return `None`, writes are discarded). This makes the cache safe to use in both async and non-async contexts without panicking.

The cache exposes standard `get`, `insert`, `remove`, and `clear` operations, as well as `get_or_insert_with` and `get_or_try_insert_with` for compute-on-miss patterns. A `with_mut` escape hatch grants direct mutable access to the underlying `LruCache` within a lock guard.

The crate also exports a `sha1_digest` helper that computes a 20-byte SHA-1 hash of arbitrary byte slices, useful for building content-addressed cache keys (e.g., keying on file contents rather than file paths to avoid stale entries).


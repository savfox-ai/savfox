//! File-backed [`SeqStore`] for the cokret bridge runtime.
//!
//! The `cokret-bridge-runtime` crate drives outbound event minting through a
//! restart-safe monotonic sequence allocator. The runtime's own
//! `SeqAllocator` reserves blocks from a pluggable [`SeqStore`]; the default
//! upstream implementation is diesel-backed. savfox does not embed diesel in
//! the channels crate (the runtime is pulled with the `edge` feature, no
//! diesel), so we provide a small file-backed store instead.
//!
//! [`FileSeqStore`] persists per-key high-water marks as a flat JSON object
//! (`{"<key>": <i64>, ...}`) under the savfox home directory. Each
//! [`reserve_block`](FileSeqStore::reserve_block) call loads-or-initializes the
//! value for the key, advances it by `block`, persists atomically
//! (temp file + rename), and returns the new high-water mark. Because the new
//! value is flushed to disk before being returned, a fresh process over the
//! same file always resumes strictly above the prior high-water mark — exactly
//! the monotonicity invariant the allocator relies on.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use cokret_bridge_runtime::{BridgeError, Result as BridgeResult, SeqStore};
use parking_lot::Mutex;

/// On-disk JSON shape: `{ "<seq-key>": <high-water-mark>, ... }`.
type SeqMap = HashMap<String, i64>;

/// A file-backed, restart-safe [`SeqStore`].
///
/// Guards an in-memory `HashMap<String, i64>` plus the backing file with a
/// single [`parking_lot::Mutex`], so `reserve_block` is atomic across threads
/// and the on-disk state is always consistent with what callers have observed.
#[derive(Debug)]
pub struct FileSeqStore {
    path: PathBuf,
    state: Mutex<SeqMap>,
}

impl FileSeqStore {
    /// Open (or lazily create) a store backed by `path`.
    ///
    /// The file is read eagerly so that a restart resumes from the prior
    /// high-water marks. A missing file is treated as an empty map; it is
    /// created on the first [`reserve_block`](Self::reserve_block).
    pub fn new(path: PathBuf) -> BridgeResult<Self> {
        let state = load_map(&path)?;
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    /// Convenience constructor returning a trait object ready to hand to the
    /// runtime's `SeqAllocator::new`.
    pub fn shared(path: PathBuf) -> BridgeResult<Arc<dyn SeqStore>> {
        Ok(Arc::new(Self::new(path)?))
    }

    /// Persist the current in-memory map atomically: write a sibling temp file
    /// then rename it over the target so a crash mid-write never leaves a
    /// truncated/partial JSON file.
    fn persist(&self, map: &SeqMap) -> BridgeResult<()> {
        let bytes = serde_json::to_vec_pretty(map)
            .map_err(|e| BridgeError::App(format!("seq_store: serialize: {e}")))?;
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| {
                BridgeError::App(format!("seq_store: create dir {}: {e}", parent.display()))
            })?;
        }
        let tmp = self.tmp_path();
        std::fs::write(&tmp, &bytes)
            .map_err(|e| BridgeError::App(format!("seq_store: write {}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| {
            // Best-effort cleanup of the temp file on a failed rename.
            let _ = std::fs::remove_file(&tmp);
            BridgeError::App(format!(
                "seq_store: rename {} -> {}: {e}",
                tmp.display(),
                self.path.display()
            ))
        })?;
        Ok(())
    }

    fn tmp_path(&self) -> PathBuf {
        let mut name = self
            .path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| "seq_store.json".into());
        name.push(".tmp");
        self.path.with_file_name(name)
    }
}

impl SeqStore for FileSeqStore {
    fn reserve_block(&self, key: &str, block: i64) -> BridgeResult<i64> {
        let mut map = self.state.lock();
        let current = map.get(key).copied().unwrap_or(0);
        let reserved = current.saturating_add(block);
        map.insert(key.to_owned(), reserved);
        // Persist while holding the lock so the on-disk high-water mark is
        // never behind a value already handed back to a caller.
        self.persist(&map)?;
        Ok(reserved)
    }
}

/// Read and parse the backing file. A missing file yields an empty map; an
/// unreadable or malformed file is surfaced as a typed [`BridgeError`].
fn load_map(path: &PathBuf) -> BridgeResult<SeqMap> {
    match std::fs::read(path) {
        Ok(bytes) => {
            if bytes.is_empty() {
                return Ok(SeqMap::new());
            }
            serde_json::from_slice(&bytes)
                .map_err(|e| BridgeError::App(format!("seq_store: parse {}: {e}", path.display())))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SeqMap::new()),
        Err(e) => Err(BridgeError::App(format!(
            "seq_store: read {}: {e}",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "savfox-cokret-seqstore-{}-{}-{}.json",
            label,
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ))
    }

    #[test]
    fn reserve_block_round_trips_and_is_monotonic() {
        let path = temp_file("monotonic");
        let store = FileSeqStore::new(path.clone()).expect("open");

        let a = store.reserve_block("actor_seq", 256).expect("reserve 1");
        let b = store.reserve_block("actor_seq", 256).expect("reserve 2");
        let c = store.reserve_block("actor_seq", 256).expect("reserve 3");

        assert_eq!(a, 256);
        assert_eq!(b, 512);
        assert_eq!(c, 768);
        assert!(a < b && b < c, "high-water marks must strictly increase");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn distinct_keys_are_independent() {
        let path = temp_file("keys");
        let store = FileSeqStore::new(path.clone()).expect("open");

        assert_eq!(store.reserve_block("alpha", 10).expect("alpha"), 10);
        assert_eq!(store.reserve_block("beta", 5).expect("beta"), 5);
        assert_eq!(store.reserve_block("alpha", 10).expect("alpha2"), 20);
        assert_eq!(store.reserve_block("beta", 5).expect("beta2"), 10);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fresh_instance_resumes_above_prior_high_water_mark() {
        let path = temp_file("resume");

        {
            let store = FileSeqStore::new(path.clone()).expect("open 1");
            assert_eq!(store.reserve_block("actor_seq", 256).expect("r1"), 256);
            assert_eq!(store.reserve_block("actor_seq", 256).expect("r2"), 512);
        }

        // A brand-new instance over the same file must NOT regress below the
        // persisted high-water mark.
        let store = FileSeqStore::new(path.clone()).expect("open 2");
        let next = store.reserve_block("actor_seq", 256).expect("resume");
        assert_eq!(
            next, 768,
            "must resume strictly above prior high-water mark"
        );
        assert!(next > 512);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn shared_returns_usable_trait_object() {
        let path = temp_file("shared");
        let store: Arc<dyn SeqStore> = FileSeqStore::shared(path.clone()).expect("shared");
        assert_eq!(store.reserve_block("k", 1).expect("r"), 1);
        assert_eq!(store.reserve_block("k", 1).expect("r2"), 2);
        let _ = std::fs::remove_file(&path);
    }
}

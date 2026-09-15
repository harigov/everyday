//! A size-capped cache of segment files this session has already decrypted.
//!
//! See `directory.rs`'s module docs for why this exists at all: a tantivy
//! segment is immutable once tantivy finishes writing it, so decrypting the
//! same bytes twice in one session is pure waste, not a correctness
//! safeguard. This module is that memoisation, kept apart from
//! [`crate::directory::SealedDirectory`] so its eviction policy can be
//! reasoned about — and tested — without a `Directory` in the way.
//!
//! # Why a byte cap, not an entry cap
//!
//! Segment files vary hugely in size: a small mailbox's whole postings list
//! can be smaller than one large attachment's worth of body text in a
//! single segment. Capping by *count* would let a handful of huge segments
//! blow the memory budget the constructor promised, or let thousands of tiny
//! ones sit uselessly under a cap sized for a mailbox with big ones. Capping
//! by bytes is the only version of "behind a size-capped LRU" the plan's
//! wording actually means.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lru::LruCache;

/// Decrypted segment bytes this session has already paid to open, evicted
/// least-recently-used once their combined size would exceed `cap_bytes`.
///
/// A segment larger than the cap itself is never cached — it is decrypted
/// on every read instead of being allowed to evict everything else and then
/// immediately not fit. That trade only matters for a cache configured
/// smaller than a single segment can get, which is a misconfiguration, not
/// a case worth being clever about.
pub struct DecryptedCache {
    entries: LruCache<PathBuf, Arc<[u8]>>,
    total_bytes: usize,
    cap_bytes: usize,
}

impl DecryptedCache {
    pub fn new(cap_bytes: usize) -> Self {
        Self { entries: LruCache::unbounded(), total_bytes: 0, cap_bytes }
    }

    /// The decrypted bytes for `path`, if this cache is holding them, moved
    /// to the most-recently-used end.
    pub fn get(&mut self, path: &Path) -> Option<Arc<[u8]>> {
        self.entries.get(path).cloned()
    }

    /// Remember `bytes` as `path`'s decrypted content, evicting the least
    /// recently used entries until the cache fits `cap_bytes` again.
    pub fn insert(&mut self, path: PathBuf, bytes: Arc<[u8]>) {
        if bytes.len() > self.cap_bytes {
            // See the type's docs: too big to ever fit, so it is not worth
            // evicting everything else in a doomed attempt to make room.
            return;
        }
        if let Some(old) = self.entries.put(path, bytes.clone()) {
            self.total_bytes -= old.len();
        }
        self.total_bytes += bytes.len();
        while self.total_bytes > self.cap_bytes {
            let Some((_, evicted)) = self.entries.pop_lru() else { break };
            self.total_bytes -= evicted.len();
        }
    }

    /// Forget `path`, if this cache was holding it — called when the
    /// directory deletes the underlying file, so a later re-creation at the
    /// same path (tantivy reuses segment ids across compactions rarely, but
    /// never a name still live) cannot be served stale bytes.
    pub fn remove(&mut self, path: &Path) {
        if let Some(old) = self.entries.pop(path) {
            self.total_bytes -= old.len();
        }
    }

    #[cfg(test)]
    fn contains(&mut self, path: &Path) -> bool {
        self.entries.contains(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(n: usize) -> Arc<[u8]> {
        Arc::from(vec![0u8; n])
    }

    #[test]
    fn round_trips_an_entry_under_the_cap() {
        let mut cache = DecryptedCache::new(1024);
        let path = PathBuf::from("a.term");
        cache.insert(path.clone(), bytes(100));
        assert_eq!(cache.get(&path).map(|b| b.len()), Some(100));
    }

    #[test]
    fn a_miss_returns_none() {
        let mut cache = DecryptedCache::new(1024);
        assert!(cache.get(Path::new("missing")).is_none());
    }

    #[test]
    fn evicts_the_least_recently_used_entry_once_over_the_cap() {
        let mut cache = DecryptedCache::new(150);
        cache.insert(PathBuf::from("a"), bytes(100));
        cache.insert(PathBuf::from("b"), bytes(100));
        // Inserting b pushed total past 150; a is the least recently used
        // and must be the one evicted.
        assert!(!cache.contains(Path::new("a")));
        assert!(cache.contains(Path::new("b")));
    }

    #[test]
    fn getting_an_entry_protects_it_from_the_next_eviction() {
        let mut cache = DecryptedCache::new(170);
        cache.insert(PathBuf::from("a"), bytes(80));
        cache.insert(PathBuf::from("b"), bytes(80));
        // Touch `a` so `b` becomes the least recently used instead.
        cache.get(Path::new("a"));
        cache.insert(PathBuf::from("c"), bytes(80));
        assert!(cache.contains(Path::new("a")));
        assert!(!cache.contains(Path::new("b")));
    }

    #[test]
    fn an_entry_larger_than_the_cap_is_never_cached() {
        let mut cache = DecryptedCache::new(50);
        cache.insert(PathBuf::from("huge"), bytes(100));
        assert!(!cache.contains(Path::new("huge")));
        assert_eq!(cache.total_bytes, 0);
    }

    #[test]
    fn replacing_an_entry_updates_the_byte_total_rather_than_doubling_it() {
        let mut cache = DecryptedCache::new(1024);
        let path = PathBuf::from("a");
        cache.insert(path.clone(), bytes(100));
        cache.insert(path.clone(), bytes(50));
        assert_eq!(cache.total_bytes, 50);
        assert_eq!(cache.get(&path).map(|b| b.len()), Some(50));
    }

    #[test]
    fn removing_an_entry_frees_its_bytes() {
        let mut cache = DecryptedCache::new(1024);
        let path = PathBuf::from("a");
        cache.insert(path.clone(), bytes(100));
        cache.remove(&path);
        assert_eq!(cache.total_bytes, 0);
        assert!(cache.get(&path).is_none());
    }

    #[test]
    fn removing_a_path_never_cached_is_not_an_error() {
        let mut cache = DecryptedCache::new(1024);
        cache.remove(Path::new("never-there"));
    }
}

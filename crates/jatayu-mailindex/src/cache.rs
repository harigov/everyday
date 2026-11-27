//! Segment files this session has already decrypted.
//!
//! See `directory.rs`'s module docs for why this exists at all: a tantivy
//! segment is immutable once tantivy finishes writing it, so decrypting the
//! same bytes twice in one session is pure waste, not a correctness
//! safeguard. This module is that memoisation, kept apart from
//! [`crate::directory::SealedDirectory`] so its eviction policy can be
//! reasoned about — and tested — without a `Directory` in the way.
//!
//! # Two ways to remember
//!
//! - **What a reader still holds.** Every file handed out is remembered by a
//!   weak pointer for as long as anybody — an open searcher, above all —
//!   keeps its handle. Asking for the same file again hands back the very
//!   same bytes, whatever the cap below says. This is what keeps a reader
//!   reload cheap: tantivy opens every segment afresh on each reload, and
//!   without it a reload would decrypt the whole index again whenever the
//!   index is bigger than the cap. It costs nothing, because the bytes are
//!   alive anyway.
//! - **A size-capped LRU** of decrypted bytes, for a file nobody holds a
//!   handle to right now but somebody read recently (a deletes file read
//!   once into a bitset, say). Entries share their allocation with any live
//!   handle, so the cap bounds only what is kept *beyond* what readers
//!   already hold.
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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use lru::LruCache;
use tantivy::directory::OwnedBytes;

/// Decrypted segment bytes this session has already paid to open: every
/// file a reader still holds, plus a least-recently-used set capped at
/// `cap_bytes`. See the module docs.
///
/// A segment larger than the cap itself never enters the LRU — it is
/// decrypted again once no reader holds it, rather than being allowed to
/// evict everything else and then immediately not fit. That trade only
/// matters for a cache configured smaller than a single segment can get,
/// which is a misconfiguration, not a case worth being clever about.
pub struct DecryptedCache {
    entries: LruCache<PathBuf, OwnedBytes>,
    live: HashMap<PathBuf, Weak<OwnedBytes>>,
    total_bytes: usize,
    cap_bytes: usize,
}

impl DecryptedCache {
    pub fn new(cap_bytes: usize) -> Self {
        Self { entries: LruCache::unbounded(), live: HashMap::new(), total_bytes: 0, cap_bytes }
    }

    /// A handle on the decrypted bytes for `path`, if this cache is holding
    /// them: the one a reader already has, or else a fresh handle on the
    /// LRU's copy, which moves to the most-recently-used end.
    pub fn get(&mut self, path: &Path) -> Option<Arc<OwnedBytes>> {
        if let Some(handle) = self.live.get(path).and_then(Weak::upgrade) {
            self.entries.promote(path);
            return Some(handle);
        }
        let bytes = self.entries.get(path)?.clone();
        Some(self.track(path, bytes))
    }

    /// Remember `bytes` as `path`'s decrypted content, evicting the least
    /// recently used entries until the LRU fits `cap_bytes` again, and hand
    /// back the handle a reader should hold.
    pub fn insert(&mut self, path: PathBuf, bytes: OwnedBytes) -> Arc<OwnedBytes> {
        let handle = self.track(&path, bytes.clone());
        if bytes.len() > self.cap_bytes {
            // See the type's docs: too big to ever fit, so it is not worth
            // evicting everything else in a doomed attempt to make room.
            return handle;
        }
        let len = bytes.len();
        if let Some(old) = self.entries.put(path, bytes) {
            self.total_bytes -= old.len();
        }
        self.total_bytes += len;
        while self.total_bytes > self.cap_bytes {
            let Some((_, evicted)) = self.entries.pop_lru() else { break };
            self.total_bytes -= evicted.len();
        }
        handle
    }

    /// Forget `path`, if this cache was holding it — called when the
    /// directory deletes the underlying file, so a later re-creation at the
    /// same path (tantivy reuses segment ids across compactions rarely, but
    /// never a name still live) cannot be served stale bytes.
    pub fn remove(&mut self, path: &Path) {
        self.live.remove(path);
        if let Some(old) = self.entries.pop(path) {
            self.total_bytes -= old.len();
        }
    }

    /// A new handle on `bytes`, remembered weakly under `path`. Dead entries
    /// are dropped on the way: there are a few dozen files in an index, so
    /// the sweep costs nothing next to the decrypt that usually precedes it.
    fn track(&mut self, path: &Path, bytes: OwnedBytes) -> Arc<OwnedBytes> {
        let handle = Arc::new(bytes);
        self.live.retain(|_, held| held.strong_count() > 0);
        self.live.insert(path.to_path_buf(), Arc::downgrade(&handle));
        handle
    }

    #[cfg(test)]
    fn contains(&mut self, path: &Path) -> bool {
        self.entries.contains(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(n: usize) -> OwnedBytes {
        OwnedBytes::new(vec![0u8; n])
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
        assert!(cache.get(Path::new("a")).is_none(), "nobody holds a, so it is gone");
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
        assert!(cache.get(Path::new("huge")).is_none(), "and nobody holds it either");
    }

    /// The reload case: tantivy opens every segment again while the old
    /// searcher still holds them, and must get the same bytes back rather
    /// than a fresh decrypt, even when the LRU long since let them go.
    #[test]
    fn a_file_a_reader_still_holds_is_handed_back_whatever_the_cap() {
        let mut cache = DecryptedCache::new(50);
        let held = cache.insert(PathBuf::from("huge"), bytes(100));
        cache.insert(PathBuf::from("small"), bytes(40));
        cache.insert(PathBuf::from("other"), bytes(40));
        let again = cache.get(Path::new("huge")).expect("still held by a reader");
        assert!(Arc::ptr_eq(&held, &again));

        drop(held);
        drop(again);
        assert!(cache.get(Path::new("huge")).is_none(), "released once nobody holds it");
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
        let held = cache.insert(path.clone(), bytes(100));
        cache.remove(&path);
        assert_eq!(cache.total_bytes, 0);
        assert!(cache.get(&path).is_none(), "a deleted file is not served, even while held");
        drop(held);
    }

    #[test]
    fn removing_a_path_never_cached_is_not_an_error() {
        let mut cache = DecryptedCache::new(1024);
        cache.remove(Path::new("never-there"));
    }
}

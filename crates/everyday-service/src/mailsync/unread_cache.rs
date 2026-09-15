//! Caching [`everyday_core::store::mail::MailStore::unread_counts`], and
//! invalidating it by change.
//!
//! `unread_counts` is a clear-column `SUM` over `thread_mailboxes` -- see
//! that trait method's own docs -- which decrypts nothing, but still walks
//! every row of every mailbox an account has. At a hundred thousand
//! messages that is measured at around 60 ms, and the plan says it is read
//! "after every change": the app bar's badge and the mailbox list's counts
//! both want it redrawn the instant a thread is archived, starred, or a new
//! message lands, which is exactly the access pattern -- one read per
//! write, on an otherwise unread-by-nobody-in-between value -- a small cache
//! turns into "recomputed once per burst of writes" rather than "recomputed
//! on every redraw".
//!
//! # Why a cache rather than incremental counters
//!
//! The plan's schema keeps `thread_mailboxes.unread` as the one place a
//! thread's read state is counted, recomputed in the same write as every
//! flag change, label change, ingest and removal
//! (`everyday_core::store::mail`'s own module docs). Threading a second,
//! incremental `mailboxes.unread_count` column through every one of those
//! call sites -- in two SQL backends, kept consistent with the aggregate a
//! reader could still compute directly -- is real surface area for a
//! second source of truth to drift from the first. A cache invalidated by
//! the same write paths costs one `HashMap` entry removed rather than one
//! more column kept in step, and it can never disagree with
//! `unread_counts` itself: the worst a bug here can do is recompute more
//! often than strictly necessary, never answer wrongly.
//!
//! # Invalidation
//!
//! Two call sites, matching the two ways an account's unread counts can
//! actually change:
//!
//! - The sync engine, after [`crate::mailsync::passes::sync_headers`] has
//!   ingested new headers or applied flag changes for one mailbox -- new
//!   mail, and another client's flag changes, both land through here.
//! - [`crate::domains::mail`]'s batch actions, after they enqueue an
//!   outbox op and apply the optimistic local write -- a person's own
//!   mark-read, star, archive and the rest.
//!
//! Both invalidate the *whole* account rather than one mailbox: a single
//! flag change can move a thread's unread state in more than one mailbox at
//! once (a Gmail message under two labels), and the cache is cheap enough
//! to rebuild that narrowing the invalidation to "exactly the mailboxes
//! this write touched" would save a query at the cost of a much easier bug
//! to introduce.

use std::collections::HashMap;
use std::sync::Mutex;

use everyday_core::error::Result;
use everyday_core::id::{AccountId, MailboxId};

/// One process's cached answer to `unread_counts`, per account. Shared
/// between [`crate::service::Service`] (which owns one, alongside the pack
/// store and search index -- see `crate::mailsync::wiring::MailState`) and
/// the sync engine, which invalidates it as it writes.
#[derive(Default)]
pub struct UnreadCache(Mutex<HashMap<AccountId, Vec<(MailboxId, u64)>>>);

impl UnreadCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The cached counts for `account`, or `compute`'s answer if nothing is
    /// cached (or it was invalidated since) -- caching that answer for the
    /// next call. `compute` runs at most once per call, and only on a miss.
    pub fn get_or_compute(
        &self,
        account: AccountId,
        compute: impl FnOnce() -> Result<Vec<(MailboxId, u64)>>,
    ) -> Result<Vec<(MailboxId, u64)>> {
        if let Some(cached) = self.lock().get(&account) {
            return Ok(cached.clone());
        }
        let fresh = compute()?;
        self.lock().insert(account, fresh.clone());
        Ok(fresh)
    }

    /// Forget `account`'s cached counts, so the next
    /// [`UnreadCache::get_or_compute`] recomputes them. See the module docs
    /// for the two moments this is called.
    pub fn invalidate(&self, account: AccountId) {
        self.lock().remove(&account);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<AccountId, Vec<(MailboxId, u64)>>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn a_second_read_before_any_write_does_not_recompute() {
        let cache = UnreadCache::new();
        let account = AccountId::new();
        let calls = AtomicU32::new(0);
        let compute = || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![(MailboxId::new(), 3)])
        };

        let first = cache.get_or_compute(account, compute).unwrap();
        let second = cache.get_or_compute(account, compute).unwrap();
        assert_eq!(first, second);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "the second read must reuse the cached answer");
    }

    #[test]
    fn invalidating_forces_the_next_read_to_recompute() {
        let cache = UnreadCache::new();
        let account = AccountId::new();
        let calls = AtomicU32::new(0);
        let compute = || {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![(MailboxId::new(), u64::from(n))])
        };

        let first = cache.get_or_compute(account, compute).unwrap();
        cache.invalidate(account);
        let second = cache.get_or_compute(account, compute).unwrap();
        assert_ne!(first, second, "invalidation must force a fresh compute");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn two_accounts_do_not_share_a_cache_slot() {
        let cache = UnreadCache::new();
        let a = AccountId::new();
        let b = AccountId::new();
        cache.get_or_compute(a, || Ok(vec![(MailboxId::new(), 1)])).unwrap();
        cache.get_or_compute(b, || Ok(vec![(MailboxId::new(), 2)])).unwrap();
        cache.invalidate(a);
        // `b` must still answer from its own cached entry -- a compute that
        // panics proves it was never called again.
        let still_cached =
            cache.get_or_compute(b, || panic!("must not recompute an untouched account")).unwrap();
        assert_eq!(still_cached[0].1, 2);
    }
}
